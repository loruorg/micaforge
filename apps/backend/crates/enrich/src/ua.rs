//! Browser, OS and device type, from the `User-Agent` header and Client Hints.
//!
//! Three sources, in increasing order of trust:
//!
//! 1. [`woothee`], which carries a long tail of historic desktop, feature-phone
//!    and crawler strings that is not worth re-deriving by hand.
//! 2. A small matcher for the modern Chromium-derived browsers woothee predates
//!    or reports as plain Chrome — Edge, Opera, Samsung Internet, Brave, Arc,
//!    Vivaldi, Yandex, DuckDuckGo, Firefox on Android and iOS — plus the two
//!    device cases the UA string alone gets wrong: an Android tablet (which
//!    omits the `Mobile` token) and an iPad in desktop mode (which claims to be
//!    a Mac and gives itself away only by keeping a `Mobile/` build token).
//! 3. Client Hints, which the browser states about itself rather than leaving
//!    us to infer, and which therefore win wherever they are present.
//!
//! The point of the layering is that every source only overwrites what it
//! genuinely knows. `Sec-CH-UA` carries a brand and a major version and nothing
//! else, so it renames the browser but keeps the fuller version the UA string
//! gave, and it never touches the OS version, which it does not carry.
//!
//! Everything is best-effort by design. There is no `Result` here: an
//! unrecognised client is reported as an unrecognised client, never dropped,
//! because "unknown" is a legitimate row in a browser breakdown and a discarded
//! hit is not.

use serde::{Deserialize, Serialize};
use woothee::parser::Parser;

/// A client, as far as the request headers could describe one.
///
/// Every field is written straight into the matching `LowCardinality(String)`
/// column in `events`, so an unknown value is the empty string rather than a
/// `None`: ClickHouse stores one shared empty entry per part, and a breakdown
/// that has to report "unknown" wants a row, not a gap.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Client {
    /// Product name, as a person would say it: `Chrome`, `Edge`, `Safari`,
    /// `Samsung Internet`. Empty when nothing matched.
    pub browser: String,
    /// The fullest version string available, dotted (`120.0.6099.109`). Client
    /// Hints alone give only the major version.
    pub browser_version: String,
    /// OS family without its version: `Windows`, `macOS`, `iOS`, `iPadOS`,
    /// `Android`, `Linux`, `Chrome OS`.
    pub os: String,
    /// OS version, dotted. Note that Windows 11 is indistinguishable from
    /// Windows 10 in a `User-Agent` string — both say `Windows NT 10.0` — so a
    /// Windows 11 machine reports `10` unless it sends
    /// `Sec-CH-UA-Platform-Version`.
    pub os_version: String,
    /// One of `desktop`, `mobile`, `tablet`, `tv`, `wearable`, `console`,
    /// `bot`. Never empty: an unrecognised but non-empty user agent is treated
    /// as a desktop, and an empty one as a bot, because no browser ships
    /// without a `User-Agent` header and plenty of scripts do.
    pub device_type: String,
}

/// Device type: a person at a computer.
pub const DESKTOP: &str = "desktop";
/// Device type: a phone.
pub const MOBILE: &str = "mobile";
/// Device type: a tablet.
pub const TABLET: &str = "tablet";
/// Device type: a television or a streaming stick.
pub const TV: &str = "tv";
/// Device type: a watch or another wrist-worn client.
pub const WEARABLE: &str = "wearable";
/// Device type: a games console.
pub const CONSOLE: &str = "console";
/// Device type: not a person. Named agents are classified properly by
/// `micaforge-agents`; this is only the coarse signal the UA string gives.
pub const BOT: &str = "bot";

/// woothee's stand-in for a field it could not fill.
const UNKNOWN: &str = "UNKNOWN";

/// Longest header value this module will look at.
///
/// Real user agents top out well under 512 bytes. A header an order of
/// magnitude past that is either a probe or a broken client, and truncating it
/// bounds the regex work woothee does on the ingest hot path.
const MAX_UA_LEN: usize = 512;

/// Product tokens that identify a browser unambiguously, most specific first.
///
/// Every one of these rides inside a UA that also claims to be Chrome or
/// Safari, which is exactly why they have to be checked before the generic
/// engine tokens: a Samsung Internet hit reported as Chrome is not a small
/// error, it is the whole Android breakdown being wrong.
const PRODUCT_TOKENS: &[(&str, &str)] = &[
    ("EdgiOS/", "Edge"),
    ("EdgA/", "Edge"),
    ("Edg/", "Edge"),
    ("Edge/", "Edge"),
    ("OPiOS/", "Opera"),
    ("OPT/", "Opera Touch"),
    ("OPR/", "Opera"),
    ("SamsungBrowser/", "Samsung Internet"),
    ("YaBrowser/", "Yandex Browser"),
    ("Vivaldi/", "Vivaldi"),
    ("Brave/", "Brave"),
    ("Arc/", "Arc"),
    ("DuckDuckGo/", "DuckDuckGo"),
    ("Ddg/", "DuckDuckGo"),
    ("UCBrowser/", "UC Browser"),
    ("HuaweiBrowser/", "Huawei Browser"),
    ("MiuiBrowser/", "MIUI Browser"),
    ("QQBrowser/", "QQ Browser"),
    ("Silk/", "Silk"),
    ("Focus/", "Firefox Focus"),
    ("FxiOS/", "Firefox"),
    ("Firefox/", "Firefox"),
    ("CriOS/", "Chrome"),
];

/// Uppercased tokens that mean "this is a television, not a browser window".
const TV_TOKENS: &[&str] = &[
    "SMART-TV",
    "SMARTTV",
    "SMART TV",
    "HBBTV",
    "APPLETV",
    "APPLE TV",
    "GOOGLETV",
    "GOOGLE TV",
    "NETCAST",
    "WEBOS.TV",
    "ROKU",
    "BRAVIA",
    "VIERA",
    "AQUOSBROWSER",
    "CRKEY",
    "DIGITALTV",
    "FIRE TV",
    "AFTS",
    "AFTB",
    "AFTT",
    "AFTM",
    "TIZEN TV",
    "DTV",
];

/// Uppercased tokens for games consoles.
const CONSOLE_TOKENS: &[&str] = &["PLAYSTATION", "XBOX", "NINTENDO"];

/// Uppercased tokens for wrist-worn clients.
const WEARABLE_TOKENS: &[&str] = &["WEAR OS", "WEAROS", "WATCH;", "WATCH/"];

/// Parse a client from the request headers.
///
/// `sec_ch_ua`, `sec_ch_ua_platform` and `sec_ch_ua_mobile` are the raw header
/// values (`"Chromium";v="120", "Google Chrome";v="120"`, `"macOS"`, `?1`).
/// Pass `None` for any header the request did not carry; Safari and Firefox
/// send none of them, so the UA path is not a fallback but the common case.
///
/// ```
/// # use micaforge_enrich::ua;
/// let c = ua::parse(
///     "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
///      (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
///     None,
///     None,
///     None,
/// );
/// assert_eq!(c.browser, "Chrome");
/// assert_eq!(c.os, "Windows");
/// assert_eq!(c.device_type, "desktop");
/// ```
pub fn parse(
    user_agent: &str,
    sec_ch_ua: Option<&str>,
    sec_ch_ua_platform: Option<&str>,
    sec_ch_ua_mobile: Option<&str>,
) -> Client {
    let ua = truncate(user_agent.trim(), MAX_UA_LEN);

    if ua.is_empty() {
        // No header at all. Every browser sends one; scripts routinely do not.
        return Client {
            device_type: BOT.to_string(),
            ..Client::default()
        };
    }

    let mut client = from_woothee(ua);
    let upper = ua.to_ascii_uppercase();

    correct_browser(ua, &mut client);
    correct_os(ua, &upper, &mut client);
    correct_device(ua, &upper, &mut client);

    apply_hints(sec_ch_ua, sec_ch_ua_platform, sec_ch_ua_mobile, &mut client);

    if client.device_type.is_empty() {
        client.device_type = DESKTOP.to_string();
    }
    client
}

// ---------------------------------------------------------------------------
// Layer 1: woothee
// ---------------------------------------------------------------------------

/// The woothee reading, with its `UNKNOWN` sentinels flattened to empty.
fn from_woothee(ua: &str) -> Client {
    let parsed = match Parser::new().parse(ua) {
        Some(r) => r,
        None => return Client::default(),
    };

    let category = clean(parsed.category);
    let raw_os = clean(parsed.os);
    let (os, os_version) = normalise_os(raw_os, clean(&parsed.os_version));

    // woothee reports the OS as the `name` for appliances and for user agents
    // with no browser in them at all, which would otherwise land a value like
    // "PlayStation 4" in the browser column.
    let raw_name = clean(parsed.name);
    let browser = if raw_name == raw_os { "" } else { raw_name };

    Client {
        browser: rename_browser(browser).to_string(),
        browser_version: clean(parsed.version).to_string(),
        os: os.to_string(),
        os_version,
        device_type: device_from_category(category).to_string(),
    }
}

/// Map woothee's own spelling onto the product name people use.
fn rename_browser(name: &str) -> &str {
    match name {
        "SamsungBrowser" => "Samsung Internet",
        "Webview" => "WebView",
        "Sleipnir" => "Sleipnir",
        "Firefox OS" => "Firefox",
        other => other,
    }
}

/// woothee's coarse category, as a device type.
///
/// `misc` collects HTTP libraries and feed readers, which are non-human even
/// when they are not crawling, so they land in `bot` alongside the named
/// crawlers rather than being counted as desktop visitors.
fn device_from_category(category: &str) -> &'static str {
    match category {
        "pc" => DESKTOP,
        "smartphone" | "mobilephone" => MOBILE,
        "crawler" | "misc" => BOT,
        // `appliance` mixes consoles, televisions and e-readers; the token
        // scan in `correct_device` splits them.
        _ => "",
    }
}

/// Reduce woothee's OS name to a family, and give it a usable version.
///
/// woothee folds the version into the name for Windows ("Windows 10") and
/// reports the raw registry-style version separately ("NT 10.0"). Neither is
/// what a report wants, so the family and the marketing version are split back
/// apart here.
fn normalise_os(os: &str, version: &str) -> (&'static str, String) {
    if let Some(rest) = os.strip_prefix("Windows Phone") {
        return ("Windows Phone", rest.trim().to_string());
    }
    if let Some(rest) = os.strip_prefix("Windows") {
        let marketing = rest.trim();
        return (
            "Windows",
            if marketing.is_empty() {
                version.to_string()
            } else {
                marketing.to_string()
            },
        );
    }
    match os {
        "Mac OSX" | "Mac OS Classic" => ("macOS", version.to_string()),
        "iPhone" | "iPod" | "iOS" => ("iOS", version.to_string()),
        "iPad" => ("iPadOS", version.to_string()),
        "Android" => ("Android", version.to_string()),
        "ChromeOS" => ("Chrome OS", version.to_string()),
        "Linux" => ("Linux", version.to_string()),
        "BSD" => ("BSD", version.to_string()),
        "SymbianOS" => ("Symbian", version.to_string()),
        "BlackBerry" | "BlackBerry 10" => ("BlackBerry", version.to_string()),
        "" => ("", String::new()),
        // Consoles and televisions keep their own name; there is no family to
        // reduce "PlayStation 4" to.
        _ => (leak_known(os), version.to_string()),
    }
}

/// Keep an OS name woothee produced that has no family to fold into.
///
/// The set is closed (it comes from woothee's compiled-in dataset), so the
/// match is exhaustive over the names that reach here and falls back to the
/// empty string rather than leaking an arbitrary header value into a
/// `LowCardinality` column, where unbounded distinct values are expensive.
fn leak_known(os: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "PlayStation 3",
        "PlayStation 4",
        "PlayStation Portable",
        "PlayStation Vita",
        "Nintendo 3DS",
        "Nintendo DSi",
        "Nintendo Wii",
        "Nintendo Wii U",
        "Xbox 360",
        "Xbox One",
        "DigitalTV",
        "Firefox OS",
        "docomo",
        "au",
        "SoftBank",
        "WILLCOM",
        "emobile",
    ];
    KNOWN.iter().copied().find(|k| *k == os).unwrap_or("")
}

// ---------------------------------------------------------------------------
// Layer 2: the corrections woothee needs
// ---------------------------------------------------------------------------

/// Replace the browser when an unambiguous product token is present.
///
/// A product token always wins. Every entry in [`PRODUCT_TOKENS`] names one
/// specific browser and cannot appear in another one's UA, whereas the generic
/// `Chrome/` and `Safari/` tokens appear in all of them.
fn correct_browser(ua: &str, client: &mut Client) {
    if let Some((token, name)) = PRODUCT_TOKENS.iter().find(|(t, _)| ua.contains(t)) {
        client.browser = (*name).to_string();
        let version = token_version(ua, token);
        if !version.is_empty() {
            client.browser_version = version;
        }
        return;
    }

    if !client.browser.is_empty() {
        return;
    }

    // Nothing matched and woothee had no opinion: fall back to the engine
    // tokens, which at least separate the three rendering engines.
    if ua.contains("Chrome/") {
        client.browser = "Chrome".to_string();
        client.browser_version = token_version(ua, "Chrome/");
    } else if ua.contains("Safari/") && ua.contains("Version/") {
        client.browser = "Safari".to_string();
        client.browser_version = token_version(ua, "Version/");
    } else if ua.contains("Trident/") || ua.contains("MSIE ") {
        client.browser = "Internet Explorer".to_string();
    }
}

/// Fix the OS cases the UA string reports misleadingly.
fn correct_os(ua: &str, upper: &str, client: &mut Client) {
    // An iPad in desktop mode sends a Macintosh user agent. The one thing it
    // keeps from its own is the `Mobile/` build token, which desktop Safari
    // never sends, so that pair is the only server-side way to tell an iPad
    // from a Mac.
    if ua.contains("Macintosh") && ua.contains("Mobile/") {
        client.os = "iPadOS".to_string();
        return;
    }

    // Firefox on Android is the notable miss: its UA has no `Linux` token, so
    // the Linux branch that normally finds `Android` never runs.
    if client.os.is_empty() || client.os == "Linux" {
        if upper.contains("ANDROID") {
            client.os = "Android".to_string();
            let version = token_version(ua, "Android ");
            if !version.is_empty() {
                client.os_version = version;
            }
        } else if ua.contains("CrOS") {
            client.os = "Chrome OS".to_string();
        }
    }
}

/// Decide the device type from the tokens the UA carries.
///
/// Ordered from least to most ambiguous: a console or a television says so
/// outright, a tablet is recognised by what it *omits*, and only then does the
/// `Mobile` token get to mean a phone.
fn correct_device(ua: &str, upper: &str, client: &mut Client) {
    if client.device_type == BOT {
        return;
    }

    if CONSOLE_TOKENS.iter().any(|t| upper.contains(t)) {
        client.device_type = CONSOLE.to_string();
        return;
    }
    if TV_TOKENS.iter().any(|t| upper.contains(t)) {
        client.device_type = TV.to_string();
        return;
    }
    if WEARABLE_TOKENS.iter().any(|t| upper.contains(t)) {
        client.device_type = WEARABLE.to_string();
        return;
    }

    if client.os == "iPadOS" || ua.contains("iPad") || upper.contains("TABLET") {
        client.device_type = TABLET.to_string();
        return;
    }

    // Android's own convention: a phone browser adds `Mobile` to the UA and a
    // tablet browser leaves it out. It is the only signal there is, and it is
    // reliable enough that Google documents it.
    if client.os == "Android" {
        client.device_type = if ua.contains("Mobile") {
            MOBILE
        } else {
            TABLET
        }
        .to_string();
        return;
    }

    if ua.contains("iPhone") || ua.contains("iPod") || ua.contains("Mobile") {
        client.device_type = MOBILE.to_string();
    }
}

// ---------------------------------------------------------------------------
// Layer 3: Client Hints
// ---------------------------------------------------------------------------

/// Overlay whatever the client stated about itself.
fn apply_hints(
    sec_ch_ua: Option<&str>,
    platform: Option<&str>,
    mobile: Option<&str>,
    client: &mut Client,
) {
    if let Some(raw) = sec_ch_ua {
        if let Some((brand, version)) = pick_brand(raw) {
            client.browser = brand;
            // The hint carries only a major version. Keep the UA's fuller one
            // when the two agree, so `120` does not overwrite `120.0.6099.109`.
            let keeps = !version.is_empty()
                && (client.browser_version == version
                    || client
                        .browser_version
                        .strip_prefix(&version)
                        .is_some_and(|rest| rest.starts_with('.')));
            if !keeps && !version.is_empty() {
                client.browser_version = version;
            }
        }
    }

    let hinted_platform = platform.and_then(normalise_platform);
    if let Some(os) = hinted_platform {
        if client.os != os {
            // A different family means the version we inferred belongs to the
            // wrong OS, and the hint set does not carry a replacement unless
            // `Sec-CH-UA-Platform-Version` was also requested.
            client.os_version.clear();
            client.os = os.to_string();
        }
    }

    // A television, a console, a watch or a crawler is not something the
    // mobile flag has anything to say about, and those are the four the UA
    // string identifies far better than the hint does.
    let hardware_is_settled = matches!(client.device_type.as_str(), TV | CONSOLE | WEARABLE | BOT);
    if hardware_is_settled {
        return;
    }

    match mobile.map(str::trim) {
        Some("?1") => {
            // Chrome sends `?1` from a phone and `?0` from a tablet, so this
            // corrects the tablet guess the UA string forces when an Android
            // browser omits its `Mobile` token.
            client.device_type = MOBILE.to_string();
        }
        Some("?0") => {
            if hinted_platform == Some("Android") {
                // Android plus "not mobile" is the platform's own definition of
                // a tablet.
                client.device_type = TABLET.to_string();
            } else if client.device_type.is_empty()
                || (client.device_type == MOBILE
                    && matches!(
                        hinted_platform,
                        Some("Windows") | Some("macOS") | Some("Linux") | Some("Chrome OS")
                    ))
            {
                client.device_type = DESKTOP.to_string();
            }
        }
        _ => {}
    }
}

/// Choose the meaningful brand out of a `Sec-CH-UA` list.
///
/// The list deliberately contains a randomised decoy brand (GREASE) so that
/// servers cannot hard-code a match, and it contains `Chromium` on every
/// Chromium browser. The brand worth keeping is the one that is neither.
fn pick_brand(raw: &str) -> Option<(String, String)> {
    let brands = parse_brand_list(raw);
    let chosen = brands
        .iter()
        .find(|(b, _)| !is_grease(b) && !b.eq_ignore_ascii_case("Chromium"))
        .or_else(|| brands.iter().find(|(b, _)| !is_grease(b)))?;
    Some((rename_brand(&chosen.0), chosen.1.clone()))
}

/// A GREASE brand exists only to be ignored; it always reads as some variation
/// of "Not A Brand".
fn is_grease(brand: &str) -> bool {
    let lower = brand.to_ascii_lowercase();
    lower.contains("not") && lower.contains("brand")
}

/// Map a Client Hints brand string onto the same product name the UA path uses.
///
/// The two paths have to agree, or one browser produces two rows in a
/// breakdown depending on which headers a given request happened to carry.
fn rename_brand(brand: &str) -> String {
    match brand.trim() {
        "Google Chrome" => "Chrome".to_string(),
        "Microsoft Edge" => "Edge".to_string(),
        "Opera GX" => "Opera".to_string(),
        "Yandex" | "YaBrowser" => "Yandex Browser".to_string(),
        "Android WebView" => "WebView".to_string(),
        other => other.to_string(),
    }
}

/// Split `"Brand";v="120", "Other";v="8"` into its pairs.
///
/// Written as a scan rather than a `split(',')` because the brand token is a
/// quoted string and a quoted string is allowed to contain the separator. `"`
/// cannot occur inside a UTF-8 continuation byte, so every index the scan
/// produces is a char boundary.
fn parse_brand_list(raw: &str) -> Vec<(String, String)> {
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        while i < bytes.len() && bytes[i] != b'"' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let brand = raw[start..i].to_string();
        i += 1;

        // The version sits in the parameter that follows, before the next
        // comma. A brand with no `v=` parameter is still a brand.
        let mut version = String::new();
        while i < bytes.len() && bytes[i] != b',' {
            if bytes[i] == b'"' {
                let vstart = i + 1;
                let mut k = vstart;
                while k < bytes.len() && bytes[k] != b'"' {
                    k += 1;
                }
                if k <= bytes.len() {
                    version = raw[vstart..k.min(bytes.len())].to_string();
                }
                i = k + 1;
                break;
            }
            i += 1;
        }

        out.push((brand, version));
        if out.len() >= 8 {
            break;
        }
    }

    out
}

/// `Sec-CH-UA-Platform` values, as the specification enumerates them.
fn normalise_platform(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim().trim_matches('"');
    match trimmed {
        "Windows" => Some("Windows"),
        "macOS" => Some("macOS"),
        "Android" => Some("Android"),
        "Linux" => Some("Linux"),
        "Chrome OS" | "Chromium OS" => Some("Chrome OS"),
        "iOS" => Some("iOS"),
        // "Unknown" is a real value the specification defines, and it means
        // exactly that: leave what the UA string said alone.
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// woothee fills unknown fields with a sentinel rather than an empty string.
fn clean(value: &str) -> &str {
    if value == UNKNOWN {
        ""
    } else {
        value
    }
}

/// The dotted number that follows `token`, e.g. `Chrome/` → `120.0.6099.109`.
fn token_version(ua: &str, token: &str) -> String {
    let Some(rest) = ua.split(token).nth(1) else {
        return String::new();
    };
    rest.chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>()
        .trim_end_matches('.')
        .to_string()
}

/// Cut a string to `max` bytes without splitting a character.
fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_on_windows() {
        let c = parse(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/120.0.6099.109 Safari/537.36",
            None,
            None,
            None,
        );
        assert_eq!(c.browser, "Chrome");
        assert_eq!(c.browser_version, "120.0.6099.109");
        assert_eq!(c.os, "Windows");
        assert_eq!(c.os_version, "10");
        assert_eq!(c.device_type, DESKTOP);
    }

    #[test]
    fn edge_is_not_reported_as_chrome() {
        let c = parse(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/120.0.0.0 Safari/537.36 Edg/120.0.2210.91",
            None,
            None,
            None,
        );
        assert_eq!(c.browser, "Edge");
        assert_eq!(c.browser_version, "120.0.2210.91");
        assert_eq!(c.device_type, DESKTOP);
    }

    #[test]
    fn opera_is_not_reported_as_chrome() {
        let c = parse(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like \
             Gecko) Chrome/119.0.0.0 Safari/537.36 OPR/105.0.4970.29",
            None,
            None,
            None,
        );
        assert_eq!(c.browser, "Opera");
        assert_eq!(c.browser_version, "105.0.4970.29");
        assert_eq!(c.os, "macOS");
    }

    #[test]
    fn samsung_internet_is_not_reported_as_chrome() {
        let c = parse(
            "Mozilla/5.0 (Linux; Android 13; SM-S911B) AppleWebKit/537.36 (KHTML, like Gecko) \
             SamsungBrowser/23.0 Chrome/115.0.0.0 Mobile Safari/537.36",
            None,
            None,
            None,
        );
        assert_eq!(c.browser, "Samsung Internet");
        assert_eq!(c.browser_version, "23.0");
        assert_eq!(c.os, "Android");
        assert_eq!(c.os_version, "13");
        assert_eq!(c.device_type, MOBILE);
    }

    #[test]
    fn firefox_on_android_is_mobile() {
        // The UA carries no `Linux` token, which is what makes this the case
        // woothee gets wrong.
        let c = parse(
            "Mozilla/5.0 (Android 13; Mobile; rv:120.0) Gecko/120.0 Firefox/120.0",
            None,
            None,
            None,
        );
        assert_eq!(c.browser, "Firefox");
        assert_eq!(c.browser_version, "120.0");
        assert_eq!(c.os, "Android");
        assert_eq!(c.device_type, MOBILE);
    }

    #[test]
    fn android_tablet_is_not_a_phone() {
        let c = parse(
            "Mozilla/5.0 (Linux; Android 13; SM-X710) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/120.0.0.0 Safari/537.36",
            None,
            None,
            None,
        );
        assert_eq!(c.os, "Android");
        assert_eq!(c.device_type, TABLET);
    }

    #[test]
    fn iphone_is_mobile() {
        let c = parse(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, \
             like Gecko) Version/17.1 Mobile/15E148 Safari/604.1",
            None,
            None,
            None,
        );
        assert_eq!(c.browser, "Safari");
        assert_eq!(c.os, "iOS");
        assert_eq!(c.os_version, "17.1");
        assert_eq!(c.device_type, MOBILE);
    }

    #[test]
    fn ipad_is_a_tablet() {
        let c = parse(
            "Mozilla/5.0 (iPad; CPU OS 17_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like \
             Gecko) Version/17.1 Mobile/15E148 Safari/604.1",
            None,
            None,
            None,
        );
        assert_eq!(c.os, "iPadOS");
        assert_eq!(c.device_type, TABLET);
    }

    #[test]
    fn ipad_in_desktop_mode_is_still_a_tablet() {
        // Claims to be a Mac; only the `Mobile/` build token gives it away.
        let c = parse(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like \
             Gecko) Version/17.1 Mobile/15E148 Safari/604.1",
            None,
            None,
            None,
        );
        assert_eq!(c.os, "iPadOS");
        assert_eq!(c.device_type, TABLET);
    }

    #[test]
    fn safari_on_a_real_mac_stays_a_desktop() {
        let c = parse(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like \
             Gecko) Version/17.1 Safari/605.1.15",
            None,
            None,
            None,
        );
        assert_eq!(c.browser, "Safari");
        assert_eq!(c.os, "macOS");
        assert_eq!(c.device_type, DESKTOP);
    }

    #[test]
    fn client_hints_name_a_browser_the_ua_hides() {
        // Brave ships a UA byte-identical to Chrome's; the hint is the only
        // place it identifies itself.
        let c = parse(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/120.0.0.0 Safari/537.36",
            Some(r#""Not_A Brand";v="8", "Chromium";v="120", "Brave";v="120""#),
            Some(r#""Windows""#),
            Some("?0"),
        );
        assert_eq!(c.browser, "Brave");
        // The hint's bare major version must not replace the fuller UA one.
        assert_eq!(c.browser_version, "120.0.0.0");
        assert_eq!(c.os, "Windows");
        assert_eq!(c.device_type, DESKTOP);
    }

    #[test]
    fn client_hints_grease_brand_is_ignored() {
        let brands = parse_brand_list(r#""Not/A)Brand";v="99", "Google Chrome";v="121""#);
        assert_eq!(brands.len(), 2);
        let (brand, version) =
            pick_brand(r#""Not/A)Brand";v="99", "Google Chrome";v="121""#).unwrap();
        assert_eq!(brand, "Chrome");
        assert_eq!(version, "121");
    }

    #[test]
    fn client_hints_mobile_flag_wins_over_a_desktop_guess() {
        let c = parse(
            "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/120.0.0.0 Safari/537.36",
            Some(r#""Chromium";v="120", "Google Chrome";v="120""#),
            Some(r#""Android""#),
            Some("?1"),
        );
        assert_eq!(c.browser, "Chrome");
        assert_eq!(c.os, "Android");
        assert_eq!(c.device_type, MOBILE);
    }

    #[test]
    fn android_platform_with_mobile_false_is_a_tablet() {
        let c = parse(
            "Mozilla/5.0 (Linux; Android 13; SM-X710) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/120.0.0.0 Safari/537.36",
            Some(r#""Chromium";v="120", "Google Chrome";v="120""#),
            Some(r#""Android""#),
            Some("?0"),
        );
        assert_eq!(c.device_type, TABLET);
    }

    #[test]
    fn a_crawler_is_a_bot() {
        let c = parse(
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
            None,
            None,
            None,
        );
        assert_eq!(c.device_type, BOT);
    }

    #[test]
    fn an_empty_user_agent_is_a_bot() {
        let c = parse("", None, None, None);
        assert_eq!(c.device_type, BOT);
        assert_eq!(c.browser, "");
    }

    #[test]
    fn a_console_is_a_console() {
        let c = parse(
            "Mozilla/5.0 (PlayStation; PlayStation 5/2.26) AppleWebKit/605.1.15 (KHTML, like \
             Gecko) Version/13.0 Safari/605.1.15",
            None,
            None,
            None,
        );
        assert_eq!(c.device_type, CONSOLE);
    }

    #[test]
    fn a_television_is_a_television() {
        let c = parse(
            "Mozilla/5.0 (SMART-TV; LINUX; Tizen 6.0) AppleWebKit/537.36 (KHTML, like Gecko) \
             SamsungBrowser/4.0 Chrome/76.0.3809.146 TV Safari/537.36",
            None,
            None,
            None,
        );
        assert_eq!(c.device_type, TV);
    }

    #[test]
    fn an_absurd_user_agent_is_bounded_and_still_answers() {
        let c = parse(&"A".repeat(40_000), None, None, None);
        assert_eq!(c.device_type, DESKTOP);
    }

    #[test]
    fn version_token_stops_at_the_first_non_number() {
        assert_eq!(
            token_version("x Chrome/120.0.1 Safari/537", "Chrome/"),
            "120.0.1"
        );
        assert_eq!(token_version("x Android 13; y", "Android "), "13");
        assert_eq!(token_version("no token here", "Chrome/"), "");
    }
}
