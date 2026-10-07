//! `micaforge-agents`: everything Micaforge knows about non-human readers.
//!
//! This is the crate the product is named for. Every other backend crate does
//! something an analytics tool has always done; this one holds the part that is
//! new, and the whole differentiator reduces to being able to answer four
//! questions about a request that is not a person:
//!
//! 1. **Who is it?** [`catalog`] — a compiled-in register of named crawlers,
//!    fetchers and agents, with the operator and the purpose behind each.
//! 2. **Is it really them?** [`verify`] — published IP ranges and documented
//!    reverse-DNS suffixes, so `agent_events.verified` means something.
//! 3. **What did the site tell it?** [`robots`] and [`llms_txt`] — an RFC 9309
//!    matcher and an `llms.txt` reader, joined by [`policy`] into stated policy
//!    versus observed behaviour.
//! 4. **Did any of it come back?** [`answer_engines`] — the referrer hosts that
//!    mean a reader arrived from a generated answer. This is the "returned" side
//!    of the Citation Gap; the catalog is the "taken" side.
//!
//! [`classify`] is the front door: one call on the ingest path decides between a
//! human event, a named agent event and a suspected bot, which is the same
//! three-way split as the three ClickHouse tables in `docs/CONTRACT.md` §1.
//!
//! ## What this crate refuses to do
//!
//! It never guesses a name. An agent is named because its published product
//! token was found, not because a heuristic thought it looked like one, and a
//! fetch is verified only against an address the operator publishes. Everything
//! else is reported as unknown, which is a real answer and appears as one in the
//! UI. `PRODUCT.md` puts it as a principle: never invent a number. Half of a
//! Citation Gap made of guesses would be worse than no Citation Gap at all.
//!
//! ## No I/O on the hot path
//!
//! The catalog is compiled in and the classifier touches nothing outside its
//! arguments, so classifying a request costs a lowercase and a slice scan. The
//! one function in the crate that opens a socket is
//! [`verify::fetch_published_ranges`], which runs from a scheduled job an
//! operator enables — never from ingest — so a self-hosted install still makes
//! no outbound call at runtime.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod answer_engines;
pub mod catalog;
pub mod classify;
pub mod llms_txt;
pub mod policy;
pub mod robots;
pub mod verify;

pub use answer_engines::{classify_referrer_host, AnswerEngine, ANSWER_ENGINES};
pub use catalog::{find_by_ua, AgentDef, Purpose, SearchEngine, CATALOG, SEARCH_ENGINES};
pub use classify::{classify, BotSignals, Classification, HeaderView};
pub use llms_txt::LlmsTxt;
pub use policy::PolicyReport;
pub use robots::Robots;
pub use verify::{RangeStore, Verification};

/// What a referrer turned out to be.
///
/// The two variants this crate is willing to name are the two it owns a table
/// for. Everything else is [`ReferrerKind::Other`], because whether a referrer
/// is social, internal or an ordinary link is a question about the site's own
/// domain and the caller's social list, and answering it from here would be a
/// guess dressed as a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferrerKind {
    /// No referrer was sent.
    Absent,
    /// An AI answer surface.
    AnswerEngine(&'static AnswerEngine),
    /// A traditional search engine.
    Search(&'static SearchEngine),
    /// A host neither table names.
    Other,
}

impl ReferrerKind {
    /// The acquisition channel, when this crate can settle it.
    ///
    /// `None` means "not mine to decide": an absent referrer may still be a paid
    /// arrival carrying a click id, and an unrecognised host may be social, a
    /// self-referral, or an ordinary link. Only the two channels backed by a
    /// table here are claimed.
    pub fn channel(&self) -> Option<micaforge_core::Channel> {
        match self {
            ReferrerKind::AnswerEngine(_) => Some(micaforge_core::Channel::AnswerEngine),
            ReferrerKind::Search(_) => Some(micaforge_core::Channel::Search),
            ReferrerKind::Absent | ReferrerKind::Other => None,
        }
    }

    /// The resolved `source` value (`chatgpt`, `google`, …), when there is one.
    pub fn source(&self) -> Option<&'static str> {
        match self {
            ReferrerKind::AnswerEngine(engine) => Some(engine.slug),
            ReferrerKind::Search(engine) => Some(engine.slug),
            ReferrerKind::Absent | ReferrerKind::Other => None,
        }
    }
}

/// Classify a referrer, host or full URL.
///
/// **Answer engines are tested before search engines, and that order is the
/// product.** `gemini.google.com` is a Google host; filing it under Search would
/// erase the arrival the Citation Gap exists to count. Doing the two lookups in
/// one function is how that ordering stops being something a caller has to
/// remember.
///
/// A full URL is used whole when one is given, which is the only way the
/// path-qualified surfaces in [`answer_engines::PATH_QUALIFIED`] can ever
/// resolve. A bare host is matched on the host alone.
pub fn classify_referrer(referrer: &str) -> ReferrerKind {
    let referrer = referrer.trim();
    if referrer.is_empty() {
        return ReferrerKind::Absent;
    }

    // A full URL can identify a surface a bare host cannot, so it is tried
    // first; the host form below still runs for everything else.
    if referrer.contains("://") {
        if let Some(engine) = answer_engines::classify_referrer_url(referrer) {
            return ReferrerKind::AnswerEngine(engine);
        }
    }

    let host = normalize_host(referrer);
    if host.is_empty() {
        return ReferrerKind::Absent;
    }
    if let Some(engine) = answer_engines::classify_referrer_host(&host) {
        return ReferrerKind::AnswerEngine(engine);
    }
    if let Some(engine) = catalog::classify_search_host(&host) {
        return ReferrerKind::Search(engine);
    }
    ReferrerKind::Other
}

/// Reduce anything host-shaped to a bare comparable host.
///
/// Accepts a bare host, a host with a port, a full URL, or a `Referer` header
/// value, because all four turn up: the tracker sends `document.referrer` as a
/// full URL, a reverse-proxy log line sends a host with a port, and an imported
/// dataset sends whatever the previous tool stored. Lower-cases, drops the
/// scheme, userinfo, port, path and the root label's trailing dot.
///
/// Returns an empty string for anything that leaves no host behind, which every
/// caller treats as "no referrer" rather than as an error.
pub fn normalize_host(input: &str) -> String {
    let mut host = input.trim();

    if let Some(idx) = host.find("://") {
        host = &host[idx + 3..];
    }
    // A protocol-relative reference (`//example.com/x`) is a URL too.
    host = host.strip_prefix("//").unwrap_or(host);
    // Cut the path, query and fragment before looking for userinfo or a port,
    // so a `@` or `:` further along the URL cannot be mistaken for either.
    if let Some(idx) = host.find(['/', '?', '#']) {
        host = &host[..idx];
    }
    if let Some(idx) = host.rfind('@') {
        host = &host[idx + 1..];
    }

    // An IPv6 literal keeps its brackets around the colons; anything else has
    // at most one colon, and it introduces the port.
    if let Some(rest) = host.strip_prefix('[') {
        host = rest.split(']').next().unwrap_or("");
    } else if let Some(idx) = host.rfind(':') {
        if host[idx + 1..].chars().all(|c| c.is_ascii_digit()) {
            host = &host[..idx];
        }
    }

    host.trim_end_matches('.').to_ascii_lowercase()
}

/// Does a host equal a domain, or sit underneath it?
///
/// `example.com` matches `example.com` and `www.example.com`, and does **not**
/// match `notexample.com`. The label boundary is the whole point: a suffix test
/// alone is the bug behind most home-grown referrer classifiers, and it is the
/// bug that would file traffic from `notchatgpt.com` as an answer-engine
/// arrival.
pub(crate) fn host_matches(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    match host.len().checked_sub(domain.len() + 1) {
        Some(boundary) => host.ends_with(domain) && host.as_bytes()[boundary] == b'.',
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_normalise_from_every_shape_they_arrive_in() {
        assert_eq!(normalize_host("Example.COM"), "example.com");
        assert_eq!(normalize_host("example.com."), "example.com");
        assert_eq!(normalize_host("example.com:8443"), "example.com");
        assert_eq!(
            normalize_host("https://example.com/a/b?c=d#e"),
            "example.com"
        );
        assert_eq!(
            normalize_host("http://user:pw@example.com/x"),
            "example.com"
        );
        assert_eq!(normalize_host("//example.com/x"), "example.com");
        assert_eq!(normalize_host("[2001:db8::1]:443"), "2001:db8::1");
        assert_eq!(normalize_host("  "), "");
        assert_eq!(normalize_host(""), "");
    }

    #[test]
    fn a_port_like_suffix_that_is_not_a_port_survives() {
        // Not a real host, but the rule must be about digits, not about colons.
        assert_eq!(
            normalize_host("example.com:notaport"),
            "example.com:notaport"
        );
    }

    #[test]
    fn a_referrer_resolves_to_a_channel_and_a_source() {
        use micaforge_core::Channel;

        let chatgpt = classify_referrer("https://chatgpt.com/");
        assert_eq!(chatgpt.channel(), Some(Channel::AnswerEngine));
        assert_eq!(chatgpt.source(), Some("chatgpt"));

        let google = classify_referrer("www.google.de");
        assert_eq!(google.channel(), Some(Channel::Search));
        assert_eq!(google.source(), Some("google"));

        // The ordering that the whole product rests on.
        let gemini = classify_referrer("https://gemini.google.com/app");
        assert_eq!(gemini.channel(), Some(Channel::AnswerEngine));
        assert_eq!(gemini.source(), Some("gemini"));

        // A path-qualified surface, which only a full URL can settle.
        assert_eq!(
            classify_referrer("https://duckduckgo.com/?q=x&ia=chat").source(),
            Some("duckduckgo-ai")
        );
        assert_eq!(
            classify_referrer("https://duckduckgo.com/?q=x").source(),
            Some("duckduckgo")
        );

        assert_eq!(classify_referrer(""), ReferrerKind::Absent);
        assert_eq!(
            classify_referrer("news.ycombinator.com"),
            ReferrerKind::Other
        );
        assert_eq!(classify_referrer("news.ycombinator.com").channel(), None);
    }

    #[test]
    fn host_matching_respects_the_label_boundary() {
        assert!(host_matches("example.com", "example.com"));
        assert!(host_matches("www.example.com", "example.com"));
        assert!(host_matches("a.b.example.com", "example.com"));
        assert!(!host_matches("notexample.com", "example.com"));
        assert!(!host_matches("example.com.evil.net", "example.com"));
        assert!(!host_matches("com", "example.com"));
        assert!(!host_matches("", "example.com"));
    }
}
