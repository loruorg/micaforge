//! Frustration: where people clicked and got nothing, clicked in anger, or
//! gave up on a form.
//!
//! The rows are `events` of kind `frustration`, written by the tracker's
//! frustration plugin: `event_name` is `rage_click`, `dead_click` or
//! `form_abandon`, and `props` holds the element's `selector` (and the burst's
//! `clicks`) or the abandoned `form` and the last `field` touched. Nothing else
//! is in them; no field value is ever recorded.
//!
//! Three readings of the same rows, one per question on the screen: which pages
//! (with their pageviews beside them, so a page with ten rage clicks in a
//! million views is not ranked above one with ten in a hundred), which elements
//! on them, and which forms with the field people last touched before leaving.

use std::collections::BTreeMap;

use clickhouse::Row;
use micaforge_core::{QueryEnvelope, Result, Table};
use micaforge_store::Ch;
use serde::{Deserialize, Serialize};

use crate::exec::fetch_all;
use crate::sql::scope_predicate;

/// Most pages, elements and forms listed.
const MAX_ROWS: u32 = 200;

/// Most last fields listed under one form.
const MAX_FIELDS: usize = 5;

/// The props document as text, for `JSONExtract*`. The column is `JSON`, and
/// reading a key through its text form is what the rest of the engine does.
const PROPS: &str = "toJSONString(props)";

/// One page with frustration on it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrustratedPage {
    /// The path.
    pub pathname: String,
    /// Rage-click bursts.
    pub rage_clicks: u64,
    /// Dead clicks.
    pub dead_clicks: u64,
    /// Forms left unsent.
    pub abandoned_forms: u64,
    /// Distinct visitors with any of the three.
    pub visitors: u64,
    /// Pageviews of the page in the window, for scale.
    pub pageviews: u64,
}

/// One element that drew rage or dead clicks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrustratedElement {
    /// The page it is on.
    pub pathname: String,
    /// `rage_click` | `dead_click`.
    pub signal: String,
    /// The tracker's short CSS selector for it.
    pub selector: String,
    /// How many times the signal fired on it.
    pub count: u64,
    /// Distinct visitors it fired for.
    pub visitors: u64,
    /// For rage clicks, the clicks in the bursts together; 0 for dead clicks.
    pub clicks: u64,
}

/// One last field, and how many abandonments stopped at it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LastField {
    /// The field's `name` or `id`.
    pub field: String,
    /// Abandonments whose last touched field it was.
    pub count: u64,
}

/// One form people left unsent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AbandonedForm {
    /// The page it is on.
    pub pathname: String,
    /// The form's `id`, `name` or selector.
    pub form: String,
    /// How many times it was left unsent.
    pub abandons: u64,
    /// Distinct visitors who left it.
    pub visitors: u64,
    /// The fields people stopped at, most common first.
    pub last_fields: Vec<LastField>,
}

/// The whole report.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Frustration {
    /// Totals over the window.
    pub rage_clicks: u64,
    /// Totals over the window.
    pub dead_clicks: u64,
    /// Totals over the window.
    pub abandoned_forms: u64,
    /// Distinct visitors with any signal.
    pub visitors: u64,
    /// Pages, most frustrated first.
    pub pages: Vec<FrustratedPage>,
    /// Elements, most frequent first.
    pub elements: Vec<FrustratedElement>,
    /// Forms, most abandoned first.
    pub forms: Vec<AbandonedForm>,
}

#[derive(Debug, Row, Deserialize)]
struct PageRaw {
    pathname: String,
    rage: u64,
    dead: u64,
    abandons: u64,
    visitors: u64,
    pageviews: u64,
}

#[derive(Debug, Row, Deserialize)]
struct ElementRaw {
    pathname: String,
    signal: String,
    selector: String,
    hits: u64,
    visitors: u64,
    clicks: u64,
}

#[derive(Debug, Row, Deserialize)]
struct FormRaw {
    pathname: String,
    form: String,
    field: String,
    hits: u64,
    visitors: u64,
}

#[derive(Debug, Row, Deserialize)]
struct TotalsRaw {
    rage: u64,
    dead: u64,
    abandons: u64,
    visitors: u64,
}

/// `GET /api/stats/frustration`.
pub async fn frustration(ch: &Ch, env: &QueryEnvelope) -> Result<Frustration> {
    let (scope, params) = scope_predicate(env, Table::Events)?;
    let signal = "kind = 'frustration'";

    let totals = fetch_all::<TotalsRaw>(
        ch,
        "frustration_totals",
        &format!(
            "SELECT
  toUInt64(countIf(event_name = 'rage_click')) AS rage,
  toUInt64(countIf(event_name = 'dead_click')) AS dead,
  toUInt64(countIf(event_name = 'form_abandon')) AS abandons,
  uniq(visitor_id) AS visitors
FROM events
WHERE {scope} AND {signal}"
        ),
        &params,
    )
    .await?
    .into_iter()
    .next();

    // Pageviews are read in the same scan, for the pages that have a signal.
    let pages = fetch_all::<PageRaw>(
        ch,
        "frustration_pages",
        &format!(
            "SELECT pathname,
  toUInt64(countIf({signal} AND event_name = 'rage_click')) AS rage,
  toUInt64(countIf({signal} AND event_name = 'dead_click')) AS dead,
  toUInt64(countIf({signal} AND event_name = 'form_abandon')) AS abandons,
  uniqIf(visitor_id, {signal}) AS f_visitors,
  toUInt64(countIf(kind = 'pageview')) AS pv
FROM events
WHERE {scope} AND kind IN ('frustration', 'pageview')
GROUP BY pathname
HAVING rage + dead + abandons > 0
ORDER BY rage + dead + abandons DESC, pathname ASC
LIMIT {MAX_ROWS}"
        ),
        &params,
    )
    .await?;

    let elements = fetch_all::<ElementRaw>(
        ch,
        "frustration_elements",
        &format!(
            "SELECT pathname, event_name AS sig,
  JSONExtractString({PROPS}, 'selector') AS sel,
  toUInt64(count()) AS hits,
  uniq(visitor_id) AS e_visitors,
  toUInt64(sum(JSONExtractUInt({PROPS}, 'clicks'))) AS e_clicks
FROM events
WHERE {scope} AND {signal} AND event_name IN ('rage_click', 'dead_click')
GROUP BY pathname, sig, sel
ORDER BY hits DESC, pathname ASC, sel ASC
LIMIT {MAX_ROWS}"
        ),
        &params,
    )
    .await?;

    let forms = fetch_all::<FormRaw>(
        ch,
        "frustration_forms",
        &format!(
            "SELECT pathname,
  JSONExtractString({PROPS}, 'form') AS f_form,
  JSONExtractString({PROPS}, 'field') AS f_field,
  toUInt64(count()) AS hits,
  uniq(visitor_id) AS f_visitors
FROM events
WHERE {scope} AND {signal} AND event_name = 'form_abandon'
GROUP BY pathname, f_form, f_field
ORDER BY hits DESC
LIMIT {limit}",
            limit = MAX_ROWS * MAX_FIELDS as u32,
        ),
        &params,
    )
    .await?;

    let totals = totals.unwrap_or(TotalsRaw {
        rage: 0,
        dead: 0,
        abandons: 0,
        visitors: 0,
    });
    Ok(Frustration {
        rage_clicks: totals.rage,
        dead_clicks: totals.dead,
        abandoned_forms: totals.abandons,
        visitors: totals.visitors,
        pages: pages
            .into_iter()
            .map(|p| FrustratedPage {
                pathname: p.pathname,
                rage_clicks: p.rage,
                dead_clicks: p.dead,
                abandoned_forms: p.abandons,
                visitors: p.visitors,
                pageviews: p.pageviews,
            })
            .collect(),
        elements: elements
            .into_iter()
            .map(|e| FrustratedElement {
                pathname: e.pathname,
                signal: e.signal,
                selector: e.selector,
                count: e.hits,
                visitors: e.visitors,
                clicks: e.clicks,
            })
            .collect(),
        forms: fold_forms(forms),
    })
}

/// Fold `(page, form, field)` rows into one row per form with its last fields.
///
/// The visitor figure of a form is the largest of its fields' figures rather
/// than their sum: one person who left the same form twice, stopping at two
/// different fields, is one visitor, and a sum would count them twice. The
/// largest is a floor that is never an overcount.
fn fold_forms(rows: Vec<FormRaw>) -> Vec<AbandonedForm> {
    let mut by_form: BTreeMap<(String, String), AbandonedForm> = BTreeMap::new();
    for row in rows {
        let entry = by_form
            .entry((row.pathname.clone(), row.form.clone()))
            .or_insert_with(|| AbandonedForm {
                pathname: row.pathname.clone(),
                form: row.form.clone(),
                abandons: 0,
                visitors: 0,
                last_fields: Vec::new(),
            });
        entry.abandons += row.hits;
        entry.visitors = entry.visitors.max(row.visitors);
        entry.last_fields.push(LastField {
            field: row.field,
            count: row.hits,
        });
    }
    let mut forms: Vec<AbandonedForm> = by_form
        .into_values()
        .map(|mut form| {
            form.last_fields
                .sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.field.cmp(&b.field)));
            form.last_fields.truncate(MAX_FIELDS);
            form
        })
        .collect();
    forms.sort_by(|a, b| {
        b.abandons
            .cmp(&a.abandons)
            .then_with(|| a.pathname.cmp(&b.pathname))
            .then_with(|| a.form.cmp(&b.form))
    });
    forms.truncate(MAX_ROWS as usize);
    forms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, form: &str, field: &str, hits: u64, visitors: u64) -> FormRaw {
        FormRaw {
            pathname: path.into(),
            form: form.into(),
            field: field.into(),
            hits,
            visitors,
        }
    }

    #[test]
    fn forms_fold_with_their_last_fields_most_common_first() {
        let forms = fold_forms(vec![
            row("/signup", "signup", "email", 7, 6),
            row("/signup", "signup", "password", 12, 10),
            row("/signup", "signup", "company", 1, 1),
            row("/contact", "contact", "message", 3, 3),
        ]);
        assert_eq!(forms.len(), 2);
        assert_eq!(forms[0].form, "signup");
        assert_eq!(forms[0].abandons, 20);
        assert_eq!(
            forms[0].visitors, 10,
            "the largest field figure, never a sum"
        );
        assert_eq!(
            forms[0].last_fields[0],
            LastField {
                field: "password".into(),
                count: 12
            }
        );
        assert_eq!(forms[0].last_fields.len(), 3);
        assert_eq!(forms[1].abandons, 3);
    }

    #[test]
    fn a_form_lists_at_most_five_last_fields() {
        let rows = (0..9)
            .map(|i| row("/f", "f", &format!("field{i}"), 10 - i, 1))
            .collect();
        let forms = fold_forms(rows);
        assert_eq!(forms[0].last_fields.len(), MAX_FIELDS);
        assert_eq!(forms[0].last_fields[0].field, "field0");
    }

    #[test]
    fn the_same_form_on_two_pages_is_two_rows() {
        let forms = fold_forms(vec![
            row("/a", "newsletter", "email", 2, 2),
            row("/b", "newsletter", "email", 1, 1),
        ]);
        assert_eq!(forms.len(), 2);
    }
}
