//! Answer surfaces: the referrer hosts that mean a reader arrived from a
//! generated answer rather than from a list of links.
//!
//! This table is the "returned" half of the Citation Gap. The other half is
//! [`crate::catalog`], which names the agents that took the page in the first
//! place. Holding both is the product.
//!
//! ## What this can and cannot see
//!
//! Two limits, stated here because pretending otherwise would inflate the
//! product's own signature metric:
//!
//! 1. **Referrers usually arrive as an origin, not a URL.** Browsers default to
//!    `strict-origin-when-cross-origin`, so a cross-origin referrer carries the
//!    scheme and host and nothing else. Any answer surface that shares a host
//!    with an ordinary search page — DuckDuckGo's chat at `?ia=chat`, Hugging
//!    Face's chat under `/chat`, Kagi's assistant under `/assistant` — therefore
//!    cannot be told apart from its own search results in the common case. Those
//!    three are marked [`PATH_QUALIFIED`] and are matched **only** by
//!    [`classify_referrer_url`], which needs a path that most browsers will not
//!    send. [`classify_referrer_host`] deliberately refuses them, because
//!    counting every DuckDuckGo search as an answer-engine arrival would be a
//!    fabricated number.
//!
//! 2. **In-page AI answers are invisible by construction.** A reader who clicks
//!    a citation inside a Google AI Overview arrives with `google.com` as the
//!    referrer, indistinguishable from an ordinary search click. Micaforge does
//!    not guess at those. The Citation Gap is therefore a floor on what was
//!    returned, never a ceiling, and the UI says so.

use serde::Serialize;

/// An AI answer surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AnswerEngine {
    /// Stable lowercase identifier, written to the `source` column of `events`
    /// when the channel is `answer_engine`.
    pub slug: &'static str,
    /// The product's own spelling of its name.
    pub display_name: &'static str,
    /// Hosts that belong to this surface. Matched on equality or as a subdomain,
    /// so `www.perplexity.ai` matches `perplexity.ai`.
    pub hosts: &'static [&'static str],
}

impl AnswerEngine {
    const fn new(
        slug: &'static str,
        display_name: &'static str,
        hosts: &'static [&'static str],
    ) -> Self {
        AnswerEngine {
            slug,
            display_name,
            hosts,
        }
    }

    /// True when this surface shares its host with a non-answer product, so a
    /// bare host is not enough to attribute an arrival to it. See the module
    /// documentation.
    pub fn needs_path(&self) -> bool {
        PATH_QUALIFIED.iter().any(|(slug, _)| *slug == self.slug)
    }
}

/// Every answer surface Micaforge attributes by name.
pub static ANSWER_ENGINES: &[AnswerEngine] = &[
    // OpenAI. `openai.com` itself is the company site, not an answer surface,
    // and is deliberately absent.
    AnswerEngine::new("chatgpt", "ChatGPT", &["chatgpt.com", "chat.openai.com"]),
    AnswerEngine::new(
        "perplexity",
        "Perplexity",
        &["perplexity.ai", "perplexity.com"],
    ),
    AnswerEngine::new("claude", "Claude", &["claude.ai", "claude.com"]),
    AnswerEngine::new(
        "gemini",
        "Gemini",
        &["gemini.google.com", "bard.google.com"],
    ),
    AnswerEngine::new("copilot", "Microsoft Copilot", &["copilot.microsoft.com"]),
    AnswerEngine::new("you", "You.com", &["you.com"]),
    AnswerEngine::new("phind", "Phind", &["phind.com"]),
    AnswerEngine::new("poe", "Poe", &["poe.com"]),
    AnswerEngine::new("grok", "Grok", &["grok.com", "x.ai"]),
    AnswerEngine::new(
        "deepseek",
        "DeepSeek",
        &["chat.deepseek.com", "deepseek.com"],
    ),
    AnswerEngine::new("meta-ai", "Meta AI", &["meta.ai"]),
    // Le Chat lives on its own host, so it needs no path qualification.
    AnswerEngine::new("le-chat", "Le Chat", &["chat.mistral.ai"]),
    // The three below share a host with a search page or a hub. They are in the
    // table so the UI can name them, and excluded from host-only matching.
    AnswerEngine::new("duckduckgo-ai", "DuckDuckGo AI Chat", &["duckduckgo.com"]),
    AnswerEngine::new("huggingface-chat", "HuggingChat", &["huggingface.co"]),
    AnswerEngine::new("kagi-assistant", "Kagi Assistant", &["kagi.com"]),
];

/// Surfaces that share a host with something that is not an answer, and the path
/// or query fragment that identifies them.
///
/// The fragment is matched against the referrer's path *and* query string, so
/// `ia=chat` catches `duckduckgo.com/?q=x&ia=chat`.
pub static PATH_QUALIFIED: &[(&str, &str)] = &[
    ("duckduckgo-ai", "ia=chat"),
    ("huggingface-chat", "/chat"),
    ("kagi-assistant", "/assistant"),
];

/// Attribute a referrer host to an answer surface.
///
/// Takes a bare host (`chatgpt.com`), tolerates a full URL, and returns `None`
/// for the path-qualified surfaces, which a host alone cannot identify. This is
/// the function the ingest path calls, because a host is all a referrer normally
/// carries.
pub fn classify_referrer_host(host: &str) -> Option<&'static AnswerEngine> {
    let host = crate::normalize_host(host);
    if host.is_empty() {
        return None;
    }
    ANSWER_ENGINES.iter().find(|engine| {
        !engine.needs_path() && engine.hosts.iter().any(|h| crate::host_matches(&host, h))
    })
}

/// Attribute a full referrer URL to an answer surface.
///
/// Same as [`classify_referrer_host`] but also resolves the path-qualified
/// surfaces, on the rare occasion a referrer arrives with its path intact (a
/// same-origin referrer, a referrer policy the other site loosened, or a
/// server-side log line where the whole `Referer` header was recorded).
pub fn classify_referrer_url(url: &str) -> Option<&'static AnswerEngine> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    let host = crate::normalize_host(parsed.host_str()?);
    if host.is_empty() {
        return None;
    }

    let mut target = String::from(parsed.path());
    if let Some(query) = parsed.query() {
        target.push('?');
        target.push_str(query);
    }
    let target = target.to_ascii_lowercase();

    ANSWER_ENGINES.iter().find(|engine| {
        if !engine.hosts.iter().any(|h| crate::host_matches(&host, h)) {
            return false;
        }
        match PATH_QUALIFIED.iter().find(|(slug, _)| *slug == engine.slug) {
            Some((_, fragment)) => target.contains(fragment),
            None => true,
        }
    })
}

/// Find an answer surface by its slug.
pub fn find_by_slug(slug: &str) -> Option<&'static AnswerEngine> {
    let slug = slug.trim().to_ascii_lowercase();
    ANSWER_ENGINES.iter().find(|engine| engine.slug == slug)
}

/// True when this host is an answer surface, by host alone.
pub fn is_answer_engine_host(host: &str) -> bool {
    classify_referrer_host(host).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_named_surfaces_resolve() {
        let cases = [
            ("chatgpt.com", "chatgpt"),
            ("chat.openai.com", "chatgpt"),
            ("www.perplexity.ai", "perplexity"),
            ("claude.ai", "claude"),
            ("gemini.google.com", "gemini"),
            ("copilot.microsoft.com", "copilot"),
            ("grok.com", "grok"),
            ("chat.mistral.ai", "le-chat"),
            ("meta.ai", "meta-ai"),
        ];
        for (host, slug) in cases {
            assert_eq!(
                classify_referrer_host(host).map(|e| e.slug),
                Some(slug),
                "host: {host}"
            );
        }
    }

    #[test]
    fn hosts_normalise() {
        assert_eq!(
            classify_referrer_host("ChatGPT.com.").map(|e| e.slug),
            Some("chatgpt")
        );
        assert_eq!(
            classify_referrer_host("https://chatgpt.com/c/abc").map(|e| e.slug),
            Some("chatgpt")
        );
        assert_eq!(
            classify_referrer_host("chatgpt.com:443").map(|e| e.slug),
            Some("chatgpt")
        );
    }

    #[test]
    fn a_search_engine_is_not_an_answer_engine() {
        assert!(classify_referrer_host("google.com").is_none());
        assert!(classify_referrer_host("www.google.co.uk").is_none());
        assert!(classify_referrer_host("bing.com").is_none());
        assert!(classify_referrer_host("news.ycombinator.com").is_none());
        assert!(classify_referrer_host("").is_none());
    }

    #[test]
    fn a_subdomain_does_not_leak_across_brands() {
        // `openai.com` is the company site; only the chat hosts count.
        assert!(classify_referrer_host("openai.com").is_none());
        assert!(classify_referrer_host("platform.openai.com").is_none());
        // A host that merely ends in the same letters must not match.
        assert!(classify_referrer_host("notchatgpt.com").is_none());
        assert!(classify_referrer_host("evil-claude.ai.example.com").is_none());
    }

    #[test]
    fn path_qualified_surfaces_are_refused_on_host_alone() {
        // Counting every DuckDuckGo search as an answer arrival would fabricate
        // the product's headline number.
        assert!(classify_referrer_host("duckduckgo.com").is_none());
        assert!(classify_referrer_host("huggingface.co").is_none());
        assert!(classify_referrer_host("kagi.com").is_none());
    }

    #[test]
    fn path_qualified_surfaces_resolve_from_a_full_url() {
        assert_eq!(
            classify_referrer_url("https://duckduckgo.com/?q=rust&ia=chat").map(|e| e.slug),
            Some("duckduckgo-ai")
        );
        assert!(classify_referrer_url("https://duckduckgo.com/?q=rust").is_none());
        assert_eq!(
            classify_referrer_url("https://huggingface.co/chat/conversation/1").map(|e| e.slug),
            Some("huggingface-chat")
        );
        assert!(classify_referrer_url("https://huggingface.co/models").is_none());
        assert_eq!(
            classify_referrer_url("https://kagi.com/assistant?t=1").map(|e| e.slug),
            Some("kagi-assistant")
        );
        // A plain host still resolves through the URL form.
        assert_eq!(
            classify_referrer_url("https://chatgpt.com/").map(|e| e.slug),
            Some("chatgpt")
        );
        assert!(classify_referrer_url("not a url").is_none());
    }
}
