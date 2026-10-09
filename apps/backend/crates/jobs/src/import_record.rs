//! One exported record, shaped for the staging table.
//!
//! Both readers in [`crate::run_import`], the CSV one and the JSON one, hand
//! every record they read to a [`Record`] one column at a time and stage the
//! flat map it gives back. One set of rules, so a PostHog event reads the same
//! whether it arrived as a CSV row, a JSON array element or a line of NDJSON.
//!
//! # A property bag is flattened, not given its own pipeline
//!
//! PostHog does not export one column per field. It exports `uuid`, `event`,
//! `timestamp` and `distinct_id`, and a `properties` column holding a JSON
//! object, and the page, the referrer, the browser and the session are all in
//! that object as `$current_url`, `$referrer`, `$browser` and `$session_id`. A
//! second pipeline for that shape would be the per-format parser the importer
//! exists to avoid. So the bag is flattened here into the same flat map a CSV
//! row becomes: `$pathname` lands under `pathname`, exactly where Umami's
//! `url_path` column lands, and the alias table in `run_import` reads both.
//!
//! Only the `$` properties that map to an `events` column are kept ([`KEPT`]).
//! The rest is PostHog's own bookkeeping (`$insert_id`, `$lib_version`,
//! `$active_feature_flags`, `$event_type`, …), and keeping it would be worse
//! than useless: an autocapture's `$event_type: "click"` would flatten into the
//! very key the importer reads an event name from. A property without the `$`
//! is the site's own, and it becomes `props` on the event, the place a live
//! custom event keeps its properties. Simple Analytics' `metadata.*` columns
//! land in `props` the same way.
//!
//! # What never reaches the staging table
//!
//! `imports_raw` keeps its records for as long as the events they became, so it
//! is held to the same rule as the events: no personal data the product would
//! not collect itself. Two things happen to every record as it is read.
//!
//! - **Person data is dropped** ([`DROPPED`]): PostHog's `person_properties` and
//!   `$set` payloads (names, email addresses, whatever a site identified people
//!   with), the autocapture element chain, the sender's IP address and the raw
//!   user agent. None of it maps to a column, and an analytics tool without
//!   cookies has no business keeping a copy of somebody's CRM.
//! - **Identifiers are pseudonymised.** A PostHog `distinct_id` is very often an
//!   email address or the site's own user id, and a GA4 `user_id` can be too.
//!   Every column the importer reads as "who" is replaced, before it is staged,
//!   by an HMAC-SHA-256 digest under a key derived from
//!   `MICAFORGE_SECRET_KEY_BASE`, with the import's id mixed in. It is stable
//!   within one import, which is all the normaliser needs to tell people apart,
//!   and it is worthless without the server's secret, so a dump of the staging
//!   table is not a list of the site's users. Nothing here ever reaches
//!   `identified_id`: an imported row is never an identified row.

use micaforge_core::crypto;
use serde_json::{Map, Value as Json};
use uuid::Uuid;

use crate::run_import::{fields, normalise_key};

/// The key a record's custom properties are staged under.
///
/// [`normalise_key`] keeps only letters and digits, so no column of any export
/// can ever normalise to a key that starts with `$`. The custom properties can
/// therefore sit in the same flat map as the columns without colliding with one.
pub const PROPS_KEY: &str = "$props";

/// PostHog `$` properties that become columns, as [`normalise_key`] spells
/// them once the `$` is gone.
///
/// The campaign tags and click ids appear here as well as in [`CAMPAIGN`]
/// because PostHog's own examples spell them both ways, `utm_source` and
/// `$utm_source`.
pub const KEPT: &[&str] = &[
    "currenturl",
    "pathname",
    "host",
    "referrer",
    "browser",
    "browserversion",
    "os",
    "osversion",
    "devicetype",
    "screenwidth",
    "screenheight",
    "viewportwidth",
    "viewportheight",
    "browserlanguage",
    "timezone",
    "geoipcountrycode",
    "geoipsubdivision1name",
    "geoipcityname",
    "sessionid",
    "prevpageviewduration",
    "prevpageviewmaxscrollpercentage",
    "webvitalslcpvalue",
    "webvitalsclsvalue",
    "webvitalsinpvalue",
    "webvitalsfcpvalue",
    "utmsource",
    "utmmedium",
    "utmcampaign",
    "utmterm",
    "utmcontent",
    "gclid",
    "fbclid",
    "msclkid",
    "ttclid",
];

/// Properties without a `$` that are still columns rather than custom
/// properties: the campaign tags, the ad click ids, and the page title that
/// `posthog-js` sends on every `$pageview` as plain `title`.
pub const CAMPAIGN: &[&str] = &[
    "utmsource",
    "utmmedium",
    "utmcampaign",
    "utmterm",
    "utmcontent",
    "gclid",
    "fbclid",
    "msclkid",
    "ttclid",
    "title",
];

/// Properties without a `$` that are neither columns nor the site's own:
/// `posthog-js` copies the project token and the distinct id into the bag of
/// every event, and neither is a property anybody set.
const NOT_PROPS: &[&str] = &["token", "distinct_id"];

/// Columns that are never staged, as [`normalise_key`] spells them. See the
/// module documentation.
pub const DROPPED: &[&str] = &[
    "personproperties",
    "person",
    "set",
    "setonce",
    "elements",
    "elementschain",
    "ip",
    "useragent",
];

/// Turns exported records into staged ones for one import.
///
/// Holds the pseudonymisation key, so it is built once per import and every
/// record of that import is shaped through it.
pub struct Shaper {
    key: [u8; 32],
    import: String,
}

impl Shaper {
    /// A shaper for `import`, pseudonymising under `key`. The key comes from
    /// `Config::derive_key`, so it is the same on every replica and every run.
    pub fn new(key: [u8; 32], import: Uuid) -> Shaper {
        Shaper {
            key,
            import: import.to_string(),
        }
    }

    /// Start one record.
    pub fn record(&self) -> Record<'_> {
        Record {
            shaper: self,
            fields: Map::new(),
            props: Map::new(),
        }
    }

    /// The staged stand-in for an identifier. Thirty-two hex characters, 128
    /// bits, far more than the normaliser needs to keep a site's visitors apart.
    fn pseudonym(&self, value: &str) -> String {
        let mut digest = crypto::keyed_digest(&self.key, &[&self.import, value]);
        digest.truncate(32);
        digest
    }
}

/// One record being shaped. See the module documentation for the rules.
pub struct Record<'a> {
    shaper: &'a Shaper,
    fields: Map<String, Json>,
    props: Map<String, Json>,
}

impl Record<'_> {
    /// Add one column, under the name the export gave it.
    pub fn column(&mut self, name: &str, value: Json) {
        self.column_at(name, value, false);
    }

    /// The flat map to stage.
    pub fn finish(mut self) -> Map<String, Json> {
        if !self.props.is_empty() {
            self.fields
                .insert(PROPS_KEY.to_string(), Json::Object(self.props));
        }
        self.fields
    }

    /// [`Record::column`], knowing whether this column came out of a `*` cell,
    /// so a `*` inside a `*` cannot recurse.
    fn column_at(&mut self, name: &str, value: Json, nested: bool) {
        let name = name.trim();

        if name.eq_ignore_ascii_case("properties") {
            if let Some(bag) = object_of(value) {
                for (key, value) in bag {
                    self.property(&key, value);
                }
            }
            return;
        }
        // PostHog's event list exports the whole event, as JSON, in a column
        // whose header is `*`.
        if name == "*" {
            if nested {
                return;
            }
            if let Some(event) = object_of(value) {
                for (key, value) in event {
                    self.column_at(&key, value, true);
                }
            }
            return;
        }
        // A PostHog SQL export names a selected property by its path.
        if let Some(property) = name.strip_prefix("properties.") {
            self.property(property, value);
            return;
        }
        // Simple Analytics' metadata, typed by a suffix on the name.
        if let Some(field) = name.strip_prefix("metadata.") {
            let (name, value) = metadata(field, value);
            if !name.is_empty() && !value.is_null() {
                self.props.entry(name.to_string()).or_insert(value);
            }
            return;
        }

        let key = normalise_key(name);
        if key.is_empty() || DROPPED.contains(&key.as_str()) {
            return;
        }
        self.field(key, &value);
    }

    /// One entry of a PostHog property bag.
    fn property(&mut self, name: &str, value: Json) {
        if let Some(internal) = name.strip_prefix('$') {
            let key = normalise_key(internal);
            if KEPT.contains(&key.as_str()) {
                self.field(key, &value);
            }
            return;
        }
        let key = normalise_key(name);
        if CAMPAIGN.contains(&key.as_str()) {
            self.field(key, &value);
        } else if !NOT_PROPS.contains(&name) && !name.is_empty() && !value.is_null() {
            self.props.entry(name.to_string()).or_insert(value);
        }
    }

    /// Stage a scalar under `key`, unless something already took the key.
    ///
    /// First come, first kept: a top-level column precedes the bag in every
    /// PostHog export, and it is the more authoritative of the two.
    fn field(&mut self, key: String, value: &Json) {
        if self.fields.contains_key(&key) {
            return;
        }
        let Some(text) = scalar_text(value) else {
            return;
        };
        let text = if fields::VISITOR.contains(&key.as_str()) {
            self.shaper.pseudonym(&text)
        } else {
            text
        };
        self.fields.insert(key, Json::String(text));
    }
}

/// A JSON object, whether it arrived as one or as the text of one. PostHog's
/// CSV and its query API both send `properties` as text.
fn object_of(value: Json) -> Option<Map<String, Json>> {
    match value {
        Json::Object(map) => Some(map),
        Json::String(text) => match serde_json::from_str::<Json>(&text) {
            Ok(Json::Object(map)) => Some(map),
            _ => None,
        },
        _ => None,
    }
}

/// The text of a scalar, or `None` for an empty string, a null, or anything
/// nested. Every staged column is text, the shape a CSV cell already has, so
/// the normaliser has one kind of value to read.
fn scalar_text(value: &Json) -> Option<String> {
    match value {
        Json::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Json::Number(number) => Some(number.to_string()),
        Json::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// A Simple Analytics metadata field, without its type suffix, and its value
/// in that type.
///
/// Simple Analytics names a metadata field `metadata.<name>_<type>`, and a CSV
/// export carries every value as text. The suffix is what says `42` was a
/// number and `true` a flag, so it is used to restore them before they become
/// `props`, where a filter on `props.count > 10` expects a number.
fn metadata(field: &str, value: Json) -> (&str, Json) {
    for (suffix, kind) in [
        ("_text", 't'),
        ("_int", 'i'),
        ("_bool", 'b'),
        ("_date", 'd'),
    ] {
        let Some(name) = field.strip_suffix(suffix).filter(|n| !n.is_empty()) else {
            continue;
        };
        let typed = match value.as_str().map(str::trim) {
            Some("") => Json::Null,
            Some(text) if kind == 'i' => text
                .parse::<i64>()
                .map_or_else(|_| Json::from(text), Json::from),
            Some("true") if kind == 'b' => Json::Bool(true),
            Some("false") if kind == 'b' => Json::Bool(false),
            _ => value,
        };
        return (name, typed);
    }
    (field, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn shaped(columns: &[(&str, Json)]) -> Map<String, Json> {
        let shaper = Shaper::new([7; 32], Uuid::nil());
        let mut record = shaper.record();
        for (name, value) in columns {
            record.column(name, value.clone());
        }
        record.finish()
    }

    #[test]
    fn a_property_bag_flattens_onto_the_columns_the_aliases_read() {
        let record = shaped(&[
            ("event", json!("$pageview")),
            (
                "properties",
                json!(
                    r#"{"$current_url":"https://example.com/a?x=1","$browser":"Chrome","$screen_width":1920,"title":"A"}"#
                ),
            ),
        ]);
        assert_eq!(record["event"], "$pageview");
        assert_eq!(record["currenturl"], "https://example.com/a?x=1");
        assert_eq!(record["browser"], "Chrome");
        // Numbers become text, the shape a CSV cell has.
        assert_eq!(record["screenwidth"], "1920");
        assert_eq!(record["title"], "A");
        assert!(!record.contains_key(PROPS_KEY));
    }

    #[test]
    fn posthog_bookkeeping_never_becomes_a_field() {
        let record = shaped(&[(
            "properties",
            json!({ "$event_type": "click", "$insert_id": "x1", "$lib_version": "1.2" }),
        )]);
        // `$event_type` would otherwise flatten onto an event-name alias.
        assert!(record.is_empty());
    }

    #[test]
    fn the_sites_own_properties_become_props() {
        let record = shaped(&[(
            "properties",
            json!({ "plan": "pro", "seats": 3, "token": "phc_x", "distinct_id": "a@b.c", "utm_source": "news" }),
        )]);
        assert_eq!(record[PROPS_KEY], json!({ "plan": "pro", "seats": 3 }));
        assert_eq!(record["utmsource"], "news");
    }

    #[test]
    fn person_data_is_dropped_and_identifiers_are_pseudonymised() {
        let record = shaped(&[
            ("distinct_id", json!("ada@example.com")),
            ("person_properties", json!(r#"{"email":"ada@example.com"}"#)),
            ("ip", json!("203.0.113.9")),
            ("elements_chain", json!("a:nth-child(1)")),
        ]);
        let staged = serde_json::to_string(&record).unwrap();
        assert!(!staged.contains("ada@example.com"));
        assert!(!staged.contains("203.0.113.9"));
        let pseudonym = record["distinctid"].as_str().unwrap();
        assert_eq!(pseudonym.len(), 32);
        // Stable within the import, which is what tells people apart.
        assert_eq!(
            shaped(&[("distinct_id", json!("ada@example.com"))])["distinctid"],
            pseudonym
        );
        // And different in another import.
        let other = Shaper::new([7; 32], Uuid::from_u128(1));
        let mut record = other.record();
        record.column("distinct_id", json!("ada@example.com"));
        assert_ne!(record.finish()["distinctid"], pseudonym);
    }

    #[test]
    fn a_star_column_is_the_whole_event_and_cannot_recurse() {
        let record = shaped(&[(
            "*",
            json!(
                r#"{"event":"signup","timestamp":"2026-08-21T09:00:00Z","properties":{"$pathname":"/join"},"*":{"event":"nested"}}"#
            ),
        )]);
        assert_eq!(record["event"], "signup");
        assert_eq!(record["pathname"], "/join");
    }

    #[test]
    fn a_sql_export_names_properties_by_their_path() {
        let record = shaped(&[
            ("properties.$current_url", json!("https://example.com/b")),
            ("properties.plan", json!("team")),
        ]);
        assert_eq!(record["currenturl"], "https://example.com/b");
        assert_eq!(record[PROPS_KEY], json!({ "plan": "team" }));
    }

    #[test]
    fn simple_analytics_metadata_keeps_its_type() {
        let record = shaped(&[
            ("metadata.plan_text", json!("pro")),
            ("metadata.seats_int", json!("12")),
            ("metadata.trial_bool", json!("false")),
            ("metadata.empty_text", json!("")),
        ]);
        assert_eq!(
            record[PROPS_KEY],
            json!({ "plan": "pro", "seats": 12, "trial": false })
        );
    }

    #[test]
    fn the_first_value_for_a_key_is_kept() {
        let record = shaped(&[
            ("pathname", json!("/top")),
            ("properties", json!({ "$pathname": "/bag" })),
        ]);
        assert_eq!(record["pathname"], "/top");
    }
}
