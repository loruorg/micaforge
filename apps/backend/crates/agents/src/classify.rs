//! Deciding what sent the request.
//!
//! Three answers, and the difference between them is the difference between the
//! three tables in `docs/CONTRACT.md` §1:
//!
//! - [`Classification::Agent`] — a client we can name. Goes to `agent_events`
//!   with an operator, a purpose and a verification result.
//! - [`Classification::SuspectedBot`] — not a person, not nameable. Goes to
//!   `bot_events` with the score and the reasons that produced it, so an
//!   operator can audit the judgement instead of trusting it.
//! - [`Classification::Human`] — everything else. Goes to `events`.
//!
//! The bar for `Human` is *not* proof of humanity, and the name of the third
//! variant is deliberately `SuspectedBot` rather than `Bot`. Heuristics on a
//! header set are evidence, not certainty, and a product whose first principle
//! is "never invent a number" does not get to dress a guess as a fact. What it
//! can do is show its working, which is what [`BotSignals`] is for.

use micaforge_core::Table;
use serde::Serialize;

use crate::catalog::{find_by_ua, AgentDef};

/// The score at or above which a hit is treated as non-human.
///
/// Set so that a single strong signal (a library user agent, a headless
/// browser token, an empty user agent plus one missing browser header) is
/// enough, while any one weak signal on its own is not. A real browser behind a
/// privacy extension strips headers, and misfiling those readers as bots is the
/// more expensive mistake: an under-counted human is a lie about the audience,
/// while an over-counted one is a lie about the traffic.
pub const BOT_THRESHOLD: u8 = 50;

/// What a request was, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// A person in a browser, as far as the request can show.
    Human,
    /// A named non-human client from the catalog.
    Agent(&'static AgentDef),
    /// Non-human, but not nameable.
    SuspectedBot(BotSignals),
}

impl Classification {
    /// The ClickHouse table this hit is written to.
    ///
    /// The one place the three-way decision turns into storage, so that the
    /// ingest path never re-derives it and the two can never disagree.
    pub fn table(&self) -> Table {
        match self {
            Classification::Human => Table::Events,
            Classification::Agent(_) => Table::AgentEvents,
            Classification::SuspectedBot(_) => Table::BotEvents,
        }
    }

    /// True when this hit counts towards the human-audience numbers.
    pub fn is_human(&self) -> bool {
        matches!(self, Classification::Human)
    }

    /// The catalog entry, when the client was nameable.
    pub fn agent(&self) -> Option<&'static AgentDef> {
        match self {
            Classification::Agent(def) => Some(def),
            _ => None,
        }
    }

    /// The evidence, when the client was judged rather than named.
    pub fn signals(&self) -> Option<&BotSignals> {
        match self {
            Classification::SuspectedBot(signals) => Some(signals),
            _ => None,
        }
    }
}

/// Why a request was judged non-human, and how strongly.
///
/// Both fields are written to `bot_events` (`score UInt8`, `reasons String`).
/// Keeping the reasons is what makes the classifier reviewable: an operator who
/// disagrees with a judgement can see the exact rule that made it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct BotSignals {
    /// 0–100. Saturates rather than wraps.
    pub score: u8,
    /// One short phrase per rule that fired, in the order they were tested.
    pub reasons: Vec<String>,
}

impl BotSignals {
    /// True when the score clears [`BOT_THRESHOLD`].
    pub fn is_bot(&self) -> bool {
        self.score >= BOT_THRESHOLD
    }

    /// The reasons as one comma-separated string, for the `reasons` column.
    pub fn reasons_joined(&self) -> String {
        self.reasons.join(",")
    }

    fn add(&mut self, weight: u8, reason: &str) {
        self.score = self.score.saturating_add(weight).min(100);
        self.reasons.push(reason.to_string());
    }
}

/// The request headers the classifier reads.
///
/// A borrowed view rather than a map: the ingest path already holds the header
/// map, and copying six strings per hit for the sake of a tidier signature is a
/// cost paid on every single request.
///
/// `signature_agent` is the `Signature-Agent` header from the Web Bot Auth
/// drafts, where a client cryptographically signs its own identity. Micaforge
/// does not verify the signature here — that needs the operator's key set — but
/// a client that *names itself* in a dedicated header is telling the truth about
/// what it is even when its user agent is a copied browser string, and that is
/// worth reading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeaderView<'a> {
    /// `Accept`.
    pub accept: Option<&'a str>,
    /// `Accept-Language`.
    pub accept_language: Option<&'a str>,
    /// `Sec-Fetch-Mode`.
    pub sec_fetch_mode: Option<&'a str>,
    /// `Sec-CH-UA`.
    pub sec_ch_ua: Option<&'a str>,
    /// `From`, the contact address a well-behaved crawler advertises.
    pub from: Option<&'a str>,
    /// `Signature-Agent`, the self-declared identity from Web Bot Auth.
    pub signature_agent: Option<&'a str>,
    /// The automation bitmask from the page (`a` in `docs/CONTRACT.md` §4,
    /// `automation` on the edge). Not a header, but evidence of the same kind:
    /// what the client showed about itself. 0 means nothing was seen.
    pub automation: u32,
}

impl<'a> HeaderView<'a> {
    /// A view with nothing set, for a caller that has only a user agent.
    pub fn empty() -> Self {
        HeaderView::default()
    }
}

/// Library and tool tokens. Anything here is a program, not a browser.
const CLIENT_LIBRARIES: &[&str] = &[
    "curl/",
    "wget",
    "python-requests",
    "python-urllib",
    "aiohttp",
    "httpx",
    "go-http-client",
    "java/",
    "okhttp",
    "axios/",
    "node-fetch",
    "undici",
    "libwww-perl",
    "lwp::simple",
    "guzzlehttp",
    "php/",
    "ruby",
    "httpclient",
    "apache-httpclient",
    "postmanruntime",
    "insomnia",
    "restsharp",
    "scrapy",
    "http_request2",
    "winhttp",
    "zgrab",
];

/// Automation tokens. A real reader may be behind one, but the session is
/// driven by a script.
const HEADLESS_TOKENS: &[&str] = &[
    "headlesschrome",
    "headless",
    "phantomjs",
    "puppeteer",
    "playwright",
    "selenium",
    "webdriver",
    "electron/",
    "chrome-lighthouse",
    "cypress",
];

/// The automation bits and their reason names, `docs/CONTRACT.md` §4.
const AUTOMATION_BITS: &[(u32, &str)] = &[
    (1, "webdriver"),
    (2, "cdp"),
    (4, "selenium"),
    (8, "headless_brand"),
    (16, "site"),
];

/// Generic self-descriptions. Weaker evidence than the two lists above, because
/// a handful of real products carry them, but strong enough to matter.
const GENERIC_BOT_TOKENS: &[&str] = &[
    "bot",
    "crawler",
    "spider",
    "scraper",
    "crawl",
    "fetcher",
    "indexer",
    "archiver",
    "monitor",
    "checker",
    "validator",
    "preview",
    "feed",
];

/// Classify one request.
///
/// Order matters and is not negotiable:
///
/// 1. A self-declared identity (`Signature-Agent`) that names a catalog entry.
/// 2. A catalog product token in the user agent.
/// 3. Heuristics.
///
/// Naming beats scoring every time. An agent that fetches with a copied browser
/// user agent but declares itself in a signed header is a named agent, and a
/// crawler whose token we know is never sent down the heuristic path where it
/// could be scored as merely suspicious.
///
/// `ip` is accepted and not yet read: proving a source address belongs to the
/// operator it claims needs the fetched range set in [`crate::verify`], which
/// this function does not own and must not block on. The parameter is in the
/// signature so that adding that step later is not an API break for every
/// caller.
pub fn classify(user_agent: &str, headers: &HeaderView<'_>, ip: Option<&str>) -> Classification {
    let _ = ip;

    if let Some(declared) = headers.signature_agent {
        if let Some(def) = find_by_ua(declared) {
            return Classification::Agent(def);
        }
    }

    if let Some(def) = find_by_ua(user_agent) {
        return Classification::Agent(def);
    }

    let signals = score(user_agent, headers);
    if signals.is_bot() {
        Classification::SuspectedBot(signals)
    } else {
        Classification::Human
    }
}

/// Score a request without deciding on it.
///
/// Exposed so the ingest path can record the evidence for a hit that was *not*
/// filed as a bot, and so the rules can be exercised directly from a test.
pub fn score(user_agent: &str, headers: &HeaderView<'_>) -> BotSignals {
    let mut signals = BotSignals::default();
    let ua = user_agent.trim();
    let lower = ua.to_ascii_lowercase();

    if ua.is_empty() {
        // Every browser sends one. Its absence is the loudest single signal
        // short of a self-declared library.
        signals.add(60, "empty_user_agent");
    } else {
        if let Some(token) = CLIENT_LIBRARIES.iter().find(|t| lower.contains(**t)) {
            signals.add(60, &format!("http_library:{}", token.trim_end_matches('/')));
        }
        if let Some(token) = HEADLESS_TOKENS.iter().find(|t| lower.contains(**t)) {
            signals.add(60, &format!("headless:{}", token.trim_end_matches('/')));
        }
        if let Some(token) = GENERIC_BOT_TOKENS.iter().find(|t| lower.contains(**t)) {
            signals.add(50, &format!("self_described:{token}"));
        }
        // Well-behaved crawlers advertise a contact URL inside the user agent,
        // usually as `+https://…`. Browsers never do.
        if lower.contains("+http") || lower.contains("(+") {
            signals.add(30, "contact_url_in_user_agent");
        }
        if ua.len() < 20 {
            signals.add(25, "user_agent_too_short");
        }
        if !lower.contains("mozilla") && !lower.contains('(') {
            // Not proof of anything on its own, but a browser string always
            // carries a platform in parentheses.
            signals.add(20, "no_platform_comment");
        }
    }

    // The page saw a driver. As strong as a headless token, and true however
    // real the headers look: a headed Chrome under automation sends them all.
    for (bit, name) in AUTOMATION_BITS {
        if headers.automation & bit != 0 {
            signals.add(60, &format!("automation:{name}"));
        }
    }

    if headers.accept_language.is_none() {
        // A browser sends this on a navigation. A fetcher rarely bothers.
        signals.add(20, "no_accept_language");
    }
    if headers.accept.is_none() {
        signals.add(15, "no_accept");
    } else if headers.accept == Some("*/*") {
        signals.add(15, "accept_any");
    }
    if headers.from.is_some() {
        // `From` is a crawler contact address. No browser sends it.
        signals.add(35, "from_header");
    }

    // The Sec-Fetch and Sec-CH-UA families are sent by every current browser and
    // are hard to forge convincingly, so their absence *from a string that
    // claims to be a modern browser* is worth more than their absence generally.
    let claims_chromium = lower.contains("chrome/") || lower.contains("edg/");
    if claims_chromium && headers.sec_ch_ua.is_none() {
        signals.add(25, "chromium_without_sec_ch_ua");
    }
    if lower.contains("mozilla") && headers.sec_fetch_mode.is_none() {
        signals.add(10, "no_sec_fetch_mode");
    }

    signals
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The headers a current browser actually sends on a navigation.
    fn browser_headers() -> HeaderView<'static> {
        HeaderView {
            accept: Some("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"),
            accept_language: Some("en-GB,en;q=0.9"),
            sec_fetch_mode: Some("navigate"),
            sec_ch_ua: Some("\"Chromium\";v=\"124\", \"Google Chrome\";v=\"124\""),
            from: None,
            signature_agent: None,
            automation: 0,
        }
    }

    const CHROME: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

    #[test]
    fn a_browser_is_a_human() {
        let c = classify(CHROME, &browser_headers(), None);
        assert_eq!(c, Classification::Human);
        assert_eq!(c.table(), Table::Events);
    }

    #[test]
    fn an_older_browser_without_client_hints_is_still_a_human() {
        // Safari and Firefox send no Sec-CH-UA at all. Weak signals must not
        // add up to a bot on their own.
        let safari = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
                      (KHTML, like Gecko) Version/17.4 Safari/605.1.15";
        let headers = HeaderView {
            sec_ch_ua: None,
            ..browser_headers()
        };
        assert_eq!(classify(safari, &headers, None), Classification::Human);
    }

    #[test]
    fn a_privacy_hardened_browser_is_still_a_human() {
        // No Accept-Language, no client hints: 20 + 25 is under the threshold on
        // purpose. Losing real readers to a strict extension is the worse error.
        let headers = HeaderView {
            accept_language: None,
            sec_ch_ua: None,
            ..browser_headers()
        };
        assert!(classify(CHROME, &headers, None).is_human());
    }

    #[test]
    fn a_named_agent_beats_every_heuristic() {
        let ua = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2; \
                  +https://openai.com/gptbot)";
        let c = classify(ua, &HeaderView::empty(), None);
        assert_eq!(c.agent().map(|d| d.slug), Some("gptbot"));
        assert_eq!(c.table(), Table::AgentEvents);
        assert!(!c.is_human());
    }

    #[test]
    fn a_self_declared_agent_is_named_even_behind_a_browser_string() {
        let headers = HeaderView {
            signature_agent: Some("https://openai.com ChatGPT-User"),
            ..browser_headers()
        };
        let c = classify(CHROME, &headers, None);
        assert_eq!(c.agent().map(|d| d.slug), Some("chatgpt-user"));
    }

    #[test]
    fn libraries_are_bots() {
        for ua in [
            "curl/8.4.0",
            "Wget/1.21.4",
            "python-requests/2.31.0",
            "Go-http-client/2.0",
            "okhttp/4.12.0",
            "axios/1.6.8",
            "node-fetch/1.0 (+https://github.com/bitinn/node-fetch)",
            "Java/17.0.10",
        ] {
            let c = classify(ua, &HeaderView::empty(), None);
            assert!(
                matches!(c, Classification::SuspectedBot(_)),
                "expected a bot for `{ua}`, got {c:?}"
            );
            assert_eq!(c.table(), Table::BotEvents);
        }
    }

    #[test]
    fn headless_browsers_are_bots() {
        let ua = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                  HeadlessChrome/124.0.0.0 Safari/537.36";
        let c = classify(ua, &browser_headers(), None);
        let signals = c.signals().expect("headless chrome must be flagged");
        assert!(signals.score >= BOT_THRESHOLD);
        assert!(signals.reasons.iter().any(|r| r.starts_with("headless:")));
    }

    #[test]
    fn an_unknown_crawler_is_suspected_not_named() {
        let ua = "Mozilla/5.0 (compatible; ExampleCorpCrawler/2.1; +https://example.com/crawler)";
        let c = classify(ua, &HeaderView::empty(), None);
        let signals = c
            .signals()
            .expect("a self-described crawler must be flagged");
        assert!(signals
            .reasons
            .iter()
            .any(|r| r == "self_described:crawler"));
        // Named beats scored: it must NOT have been attributed to a real agent.
        assert!(c.agent().is_none());
    }

    #[test]
    fn an_empty_user_agent_is_a_bot() {
        let c = classify("", &HeaderView::empty(), None);
        let signals = c.signals().unwrap();
        assert!(signals.reasons.iter().any(|r| r == "empty_user_agent"));
        assert!(!signals.reasons_joined().is_empty());
    }

    #[test]
    fn automated_chrome_with_real_headers_is_a_bot() {
        for (bits, reason) in [
            (1, "automation:webdriver"),
            (2, "automation:cdp"),
            (4, "automation:selenium"),
            (8, "automation:headless_brand"),
            (16, "automation:site"),
        ] {
            let headers = HeaderView {
                automation: bits,
                ..browser_headers()
            };
            let c = classify(CHROME, &headers, None);
            let signals = c.signals().expect("automation must be flagged");
            assert!(signals.is_bot());
            assert!(
                signals.reasons.iter().any(|r| r == reason),
                "{reasons:?}",
                reasons = signals.reasons
            );
            assert_eq!(c.table(), Table::BotEvents);
        }
        let all = score(
            CHROME,
            &HeaderView {
                automation: 31,
                ..browser_headers()
            },
        );
        assert_eq!(all.reasons.len(), 5);
        assert_eq!(all.score, 100);
    }

    #[test]
    fn automation_never_renames_an_agent() {
        let ua = "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2; \
                  +https://openai.com/gptbot)";
        let headers = HeaderView {
            automation: 1,
            ..browser_headers()
        };
        assert_eq!(
            classify(ua, &headers, None).agent().map(|d| d.slug),
            Some("gptbot")
        );
        assert!(classify(CHROME, &headers, None).agent().is_none());
    }

    #[test]
    fn the_score_saturates_rather_than_wrapping() {
        let s = score("", &HeaderView::empty());
        assert!(s.score <= 100);
        assert!(s.is_bot());
    }
}
