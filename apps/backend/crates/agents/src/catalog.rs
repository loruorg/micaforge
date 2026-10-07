//! The compiled-in register of named non-human clients.
//!
//! Every entry here is a client Micaforge can *name*. Naming is the whole point:
//! a hit from a named client goes to `agent_events` with an operator and a
//! purpose attached, and becomes a row in the agent report. A hit from an
//! unnamed non-human client goes to `bot_events` with a score and a list of
//! reasons. A tool that cannot tell those two apart is the tool this product
//! exists to replace.
//!
//! ## The accuracy rule
//!
//! **No token in this file is invented.** Every string in [`AgentDef::ua_patterns`]
//! is a product token the operator has published, and every URL in
//! `ip_ranges_url` is a document the operator actually serves. A fabricated
//! token would be worse than an absent one: it can never match, so it silently
//! becomes a permanently-zero row in a customer's dashboard, and it would put an
//! operator's name next to traffic they never sent.
//!
//! Agents deliberately left out, because no operator-published token could be
//! confirmed at the time of writing: Brave's Leo fetcher, Cognition's Devin,
//! xAI's crawler, and the various "computer use" agents that currently fetch
//! under an existing token (OpenAI's Operator fetches as `ChatGPT-User`). They
//! belong here the day their operator documents a token, and not before.
//!
//! ## Robots-token-only entries
//!
//! Two entries — [`Google-Extended`](CATALOG) and `Applebot-Extended` — carry an
//! empty `ua_patterns`. That is not an oversight. Both operators document them
//! as `robots.txt` *controls* rather than crawlers: they never appear in a
//! request, they only change what the operator may do with what another crawler
//! already fetched. They are in the catalog so the policy report can tell a site
//! owner whether their `robots.txt` opts out of training, which is a question
//! about a token that will never show up in a log line.

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use micaforge_core::{Error, Result};

/// Why an operator fetched the page.
///
/// This is the `purpose` column of `agent_events`, and it is the axis the agent
/// report groups by, because it is the axis a site owner actually cares about:
/// being crawled for a training corpus and being fetched to answer one person's
/// question right now are different events with different consequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// Bulk collection for a training corpus.
    Training,
    /// Retrieval-augmented generation: fetched to ground an answer, usually
    /// while a person waits.
    Rag,
    /// Building a search or answer index.
    Search,
    /// Rendering a link preview or unfurl.
    Preview,
    /// Benchmarking, evaluation, or one-off research crawls.
    Eval,
    /// Named, but the operator has not said what for.
    #[default]
    Unknown,
}

impl Purpose {
    /// The value as written to ClickHouse and to JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            Purpose::Training => "training",
            Purpose::Rag => "rag",
            Purpose::Search => "search",
            Purpose::Preview => "preview",
            Purpose::Eval => "eval",
            Purpose::Unknown => "unknown",
        }
    }

    /// Parse the wire value.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "training" => Ok(Purpose::Training),
            "rag" => Ok(Purpose::Rag),
            "search" => Ok(Purpose::Search),
            "preview" => Ok(Purpose::Preview),
            "eval" => Ok(Purpose::Eval),
            "unknown" => Ok(Purpose::Unknown),
            other => Err(Error::validation(
                "purpose",
                format!("unknown agent purpose `{other}`"),
            )),
        }
    }

    /// Every purpose, in the order the agent report lists them.
    pub fn all() -> &'static [Purpose] {
        &[
            Purpose::Training,
            Purpose::Rag,
            Purpose::Search,
            Purpose::Preview,
            Purpose::Eval,
            Purpose::Unknown,
        ]
    }
}

impl std::fmt::Display for Purpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One named non-human client.
///
/// The struct is `Copy` and every field is `'static`: the catalog is compiled in
/// rather than read from Postgres so that classification never touches a
/// database on the ingest path. `agent_catalog` in Postgres mirrors this table
/// and exists so an operator can *add* to it locally, not so the process has to
/// load it before it can classify a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AgentDef {
    /// Stable lowercase identifier. This is the `agent_id` column, the primary
    /// key of `agent_catalog`, and the value a filter binds — so it never
    /// changes once shipped, even if the operator renames the product.
    pub slug: &'static str,
    /// The operator's own spelling of the name, for display.
    pub display_name: &'static str,
    /// The company behind it, lowercased and hyphenated. Groups the agent report
    /// by the entity a site owner would actually negotiate with.
    pub operator: &'static str,
    /// What the fetch is for.
    pub purpose: Purpose,
    /// Product tokens as they appear inside the `User-Agent` header. Matched
    /// case-insensitively as substrings; empty for robots-token-only controls.
    pub ua_patterns: &'static [&'static str],
    /// Suffixes a reverse DNS lookup of a genuine source IP must end with.
    /// Empty when the operator documents no rDNS scheme.
    pub rdns_suffixes: &'static [&'static str],
    /// The operator's machine-readable IP range document, when they publish one.
    pub ip_ranges_url: Option<&'static str>,
    /// Where the operator documents this agent, shown next to it in the UI so a
    /// site owner can read the source rather than trust us.
    pub docs_url: Option<&'static str>,
    /// Whether the operator *states* the agent obeys `robots.txt`. This records
    /// the claim, never the behaviour: what it actually did is measured, and the
    /// two are shown side by side in the policy report.
    pub respects_robots: bool,
}

/// The operator's own spelling of its name.
///
/// `operator` is stored lowercase and hyphenated so it groups cleanly in a
/// column and never changes once shipped. That is the wrong string to show a
/// reader: title-casing the slug produces "Openai" and "Bytedance", which makes
/// the whole report look machine-generated. Companies capitalise themselves in
/// ways no algorithm derives, so the mapping is a table, and an operator missing
/// from it falls back to the slug rather than to a guess.
pub fn operator_display(slug: &str) -> &str {
    match slug {
        "ahrefs" => "Ahrefs",
        "allen-institute" => "Allen Institute for AI",
        "amazon" => "Amazon",
        "anthropic" => "Anthropic",
        "apple" => "Apple",
        "baidu" => "Baidu",
        "bytedance" => "ByteDance",
        "cohere" => "Cohere",
        "common-crawl" => "Common Crawl",
        "diffbot" => "Diffbot",
        "discord" => "Discord",
        "duckduckgo" => "DuckDuckGo",
        "firecrawl" => "Firecrawl",
        "google" => "Google",
        "huawei" => "Huawei",
        "imagesift" => "ImageSift",
        "kangaroo-llm" => "Kangaroo LLM",
        "linkedin" => "LinkedIn",
        "meta" => "Meta",
        "microsoft" => "Microsoft",
        "mistral" => "Mistral AI",
        "open-source" => "Open source",
        "openai" => "OpenAI",
        "perplexity" => "Perplexity",
        "semrush" => "Semrush",
        "slack" => "Slack",
        "telegram" => "Telegram",
        "timpi" => "Timpi",
        "webz-io" => "Webz.io",
        "x" => "X",
        "yandex" => "Yandex",
        "you" => "You.com",
        other => other,
    }
}

/// What a fetch is collected for, in a reader's words.
pub fn purpose_display(slug: &str) -> &str {
    match slug {
        "training" => "Training corpus",
        "rag" => "Answering a question now",
        "search" => "Search index",
        "preview" => "Link preview",
        "eval" => "Evaluation",
        "unknown" => "Undeclared",
        other => other,
    }
}

impl AgentDef {
    /// A minimal definition. Optional facts are added with the builders below,
    /// which keeps [`CATALOG`] readable as the data table it is.
    const fn new(
        slug: &'static str,
        display_name: &'static str,
        operator: &'static str,
        purpose: Purpose,
        ua_patterns: &'static [&'static str],
    ) -> Self {
        AgentDef {
            slug,
            display_name,
            operator,
            purpose,
            ua_patterns,
            rdns_suffixes: &[],
            ip_ranges_url: None,
            docs_url: None,
            respects_robots: true,
        }
    }

    /// Attach the operator's published IP range document.
    const fn ranges(mut self, url: &'static str) -> Self {
        self.ip_ranges_url = Some(url);
        self
    }

    /// Attach the reverse-DNS suffixes a genuine source IP resolves to.
    const fn rdns(mut self, suffixes: &'static [&'static str]) -> Self {
        self.rdns_suffixes = suffixes;
        self
    }

    /// Attach the operator's documentation page.
    const fn docs(mut self, url: &'static str) -> Self {
        self.docs_url = Some(url);
        self
    }

    /// Record that the operator states this agent does *not* consult
    /// `robots.txt`.
    const fn ignores_robots(mut self) -> Self {
        self.respects_robots = false;
        self
    }

    /// The token to look for in `robots.txt`.
    ///
    /// For nearly every agent this is the same string as the user-agent product
    /// token. For the robots-token-only controls it is the only token there is,
    /// which is why this falls back to the display name rather than to the slug:
    /// a site owner writes `User-agent: Google-Extended`, never
    /// `User-agent: google-extended`, and the match is case-insensitive anyway.
    pub fn robots_token(&self) -> &'static str {
        match self.ua_patterns.first() {
            Some(p) => p,
            None => self.display_name,
        }
    }

    /// True when this entry exists only to be written in `robots.txt` and will
    /// never appear in a request.
    pub fn is_robots_token_only(&self) -> bool {
        self.ua_patterns.is_empty()
    }

    /// True when the operator publishes a machine-readable range document, so a
    /// fetch can be verified rather than trusted.
    pub fn publishes_ranges(&self) -> bool {
        self.ip_ranges_url.is_some()
    }

    /// True when a fetch by this agent may end up inside a generated answer that
    /// a reader sees instead of the page. These are the agents the Citation Gap
    /// counts on the "taken" side.
    pub fn feeds_answers(&self) -> bool {
        matches!(
            self.purpose,
            Purpose::Training | Purpose::Rag | Purpose::Search
        )
    }
}

// ---------------------------------------------------------------------------
// The catalog
// ---------------------------------------------------------------------------

/// Every named non-human client Micaforge knows about.
///
/// Ordered by operator, then by how much traffic the agent typically accounts
/// for. Order is presentation only: every lookup goes through [`find_by_ua`],
/// which sorts by pattern length so that the most specific token wins.
pub static CATALOG: &[AgentDef] = &[
    // -- OpenAI ------------------------------------------------------------
    // Three separate tokens with three separate purposes, and separate range
    // documents for each. Conflating them is the mistake this catalog exists to
    // avoid: blocking GPTBot does not stop ChatGPT-User, and a site owner who
    // believes otherwise is measuring the wrong thing.
    AgentDef::new(
        "gptbot",
        "GPTBot",
        "openai",
        Purpose::Training,
        &["GPTBot"],
    )
    .ranges("https://openai.com/gptbot.json")
    .docs("https://platform.openai.com/docs/bots"),
    AgentDef::new(
        "oai-searchbot",
        "OAI-SearchBot",
        "openai",
        Purpose::Search,
        &["OAI-SearchBot"],
    )
    .ranges("https://openai.com/searchbot.json")
    .docs("https://platform.openai.com/docs/bots"),
    AgentDef::new(
        "chatgpt-user",
        "ChatGPT-User",
        "openai",
        Purpose::Rag,
        &["ChatGPT-User"],
    )
    .ranges("https://openai.com/chatgpt-user.json")
    .docs("https://platform.openai.com/docs/bots"),
    // -- Anthropic ---------------------------------------------------------
    AgentDef::new(
        "claudebot",
        "ClaudeBot",
        "anthropic",
        Purpose::Training,
        &["ClaudeBot"],
    )
    .docs("https://support.anthropic.com/en/articles/8896518-does-anthropic-crawl-data-from-the-web-and-how-can-site-owners-block-the-crawler"),
    AgentDef::new(
        "claude-user",
        "Claude-User",
        "anthropic",
        Purpose::Rag,
        &["Claude-User"],
    )
    .docs("https://support.anthropic.com/en/articles/8896518-does-anthropic-crawl-data-from-the-web-and-how-can-site-owners-block-the-crawler"),
    AgentDef::new(
        "claude-searchbot",
        "Claude-SearchBot",
        "anthropic",
        Purpose::Search,
        &["Claude-SearchBot"],
    )
    .docs("https://support.anthropic.com/en/articles/8896518-does-anthropic-crawl-data-from-the-web-and-how-can-site-owners-block-the-crawler"),
    // Retired tokens, kept because they are still written in a great many
    // robots.txt files and still turn up in old log archives an operator
    // imports. Reporting them under their own names is more honest than
    // folding them into ClaudeBot's row.
    AgentDef::new(
        "anthropic-ai",
        "anthropic-ai (retired)",
        "anthropic",
        Purpose::Training,
        &["anthropic-ai"],
    ),
    AgentDef::new(
        "claude-web",
        "Claude-Web (retired)",
        "anthropic",
        Purpose::Rag,
        &["Claude-Web"],
    ),
    // -- Perplexity --------------------------------------------------------
    AgentDef::new(
        "perplexitybot",
        "PerplexityBot",
        "perplexity",
        Purpose::Search,
        &["PerplexityBot"],
    )
    .ranges("https://www.perplexity.com/perplexitybot.json")
    .docs("https://docs.perplexity.ai/guides/bots"),
    // Perplexity documents that this one is triggered by a person asking a
    // question and therefore does not consult robots.txt. That is a published
    // position, not an accusation, and the policy report says so.
    AgentDef::new(
        "perplexity-user",
        "Perplexity-User",
        "perplexity",
        Purpose::Rag,
        &["Perplexity-User"],
    )
    .ranges("https://www.perplexity.com/perplexity-user.json")
    .docs("https://docs.perplexity.ai/guides/bots")
    .ignores_robots(),
    // -- Google ------------------------------------------------------------
    AgentDef::new(
        "googlebot",
        "Googlebot",
        "google",
        Purpose::Search,
        &["Googlebot"],
    )
    .ranges("https://developers.google.com/static/search/apis/ipranges/googlebot.json")
    .rdns(&["googlebot.com", "google.com"])
    .docs("https://developers.google.com/search/docs/crawling-indexing/overview-google-crawlers"),
    // Robots-token-only: Google states it does not crawl under this name; it
    // governs whether what Googlebot already fetched may be used for Gemini and
    // Vertex training.
    AgentDef::new(
        "google-extended",
        "Google-Extended",
        "google",
        Purpose::Training,
        &[],
    )
    .docs("https://developers.google.com/search/docs/crawling-indexing/overview-google-crawlers"),
    AgentDef::new(
        "googleother",
        "GoogleOther",
        "google",
        Purpose::Eval,
        &["GoogleOther"],
    )
    .ranges("https://developers.google.com/static/search/apis/ipranges/special-crawlers.json")
    .rdns(&["googlebot.com", "google.com"])
    .docs("https://developers.google.com/search/docs/crawling-indexing/google-special-case-crawlers"),
    AgentDef::new(
        "google-cloudvertexbot",
        "Google-CloudVertexBot",
        "google",
        Purpose::Rag,
        &["Google-CloudVertexBot"],
    )
    .ranges("https://developers.google.com/static/search/apis/ipranges/special-crawlers.json")
    .docs("https://developers.google.com/search/docs/crawling-indexing/google-special-case-crawlers"),
    AgentDef::new(
        "google-inspectiontool",
        "Google-InspectionTool",
        "google",
        Purpose::Eval,
        &["Google-InspectionTool"],
    )
    .ranges("https://developers.google.com/static/search/apis/ipranges/user-triggered-fetchers.json")
    .docs("https://developers.google.com/search/docs/crawling-indexing/google-user-triggered-fetchers"),
    // -- Microsoft ---------------------------------------------------------
    AgentDef::new(
        "bingbot",
        "bingbot",
        "microsoft",
        Purpose::Search,
        &["bingbot"],
    )
    .ranges("https://www.bing.com/toolbox/bingbot.json")
    .rdns(&["search.msn.com"])
    .docs("https://www.bing.com/webmasters/help/which-crawlers-does-bing-use-8c184ec0"),
    AgentDef::new(
        "bingpreview",
        "BingPreview",
        "microsoft",
        Purpose::Preview,
        &["BingPreview"],
    )
    .rdns(&["search.msn.com"])
    .docs("https://www.bing.com/webmasters/help/which-crawlers-does-bing-use-8c184ec0"),
    AgentDef::new("msnbot", "msnbot", "microsoft", Purpose::Search, &["msnbot"])
        .rdns(&["search.msn.com"])
        .docs("https://www.bing.com/webmasters/help/which-crawlers-does-bing-use-8c184ec0"),
    // -- Meta --------------------------------------------------------------
    AgentDef::new(
        "meta-externalagent",
        "meta-externalagent",
        "meta",
        Purpose::Training,
        &["meta-externalagent"],
    )
    .docs("https://developers.facebook.com/docs/sharing/webmasters/web-crawlers"),
    AgentDef::new(
        "meta-externalfetcher",
        "meta-externalfetcher",
        "meta",
        Purpose::Rag,
        &["meta-externalfetcher"],
    )
    .docs("https://developers.facebook.com/docs/sharing/webmasters/web-crawlers"),
    AgentDef::new(
        "facebookbot",
        "FacebookBot",
        "meta",
        Purpose::Training,
        &["FacebookBot"],
    )
    .docs("https://developers.facebook.com/docs/sharing/webmasters/web-crawlers"),
    AgentDef::new(
        "facebookexternalhit",
        "facebookexternalhit",
        "meta",
        Purpose::Preview,
        &["facebookexternalhit"],
    )
    .docs("https://developers.facebook.com/docs/sharing/webmasters/web-crawlers"),
    // -- Apple -------------------------------------------------------------
    AgentDef::new("applebot", "Applebot", "apple", Purpose::Search, &["Applebot"])
        .ranges("https://search.developer.apple.com/applebot.json")
        .rdns(&["applebot.apple.com"])
        .docs("https://support.apple.com/en-us/119829"),
    // Robots-token-only, exactly like Google-Extended: it governs use of what
    // Applebot already has, and never fetches anything itself.
    AgentDef::new(
        "applebot-extended",
        "Applebot-Extended",
        "apple",
        Purpose::Training,
        &[],
    )
    .docs("https://support.apple.com/en-us/119829"),
    // -- Amazon ------------------------------------------------------------
    // Amazon documents Amazonbot as improving its services, "such as enabling
    // Alexa to answer even more questions" — an index that feeds answers, so it
    // is filed under Search rather than Training.
    AgentDef::new(
        "amazonbot",
        "Amazonbot",
        "amazon",
        Purpose::Search,
        &["Amazonbot"],
    )
    .docs("https://developer.amazon.com/amazonbot"),
    // -- ByteDance ---------------------------------------------------------
    AgentDef::new(
        "bytespider",
        "Bytespider",
        "bytedance",
        Purpose::Training,
        &["Bytespider"],
    ),
    // -- Common Crawl ------------------------------------------------------
    // Not a model operator, but the single largest upstream of everyone else's
    // training data, which makes it the most consequential row in this table for
    // a site owner deciding what to allow.
    AgentDef::new(
        "ccbot",
        "CCBot",
        "common-crawl",
        Purpose::Training,
        &["CCBot"],
    )
    .docs("https://commoncrawl.org/ccbot"),
    // -- Cohere ------------------------------------------------------------
    AgentDef::new(
        "cohere-training-data-crawler",
        "cohere-training-data-crawler",
        "cohere",
        Purpose::Training,
        &["cohere-training-data-crawler"],
    ),
    AgentDef::new("cohere-ai", "cohere-ai", "cohere", Purpose::Rag, &["cohere-ai"]),
    // -- Mistral -----------------------------------------------------------
    AgentDef::new(
        "mistralai-user",
        "MistralAI-User",
        "mistral",
        Purpose::Rag,
        &["MistralAI-User"],
    )
    .docs("https://docs.mistral.ai/robots"),
    // -- You.com -----------------------------------------------------------
    AgentDef::new("youbot", "YouBot", "you", Purpose::Search, &["YouBot"])
        .docs("https://about.you.com/youbot/"),
    // -- DuckDuckGo --------------------------------------------------------
    AgentDef::new(
        "duckassistbot",
        "DuckAssistBot",
        "duckduckgo",
        Purpose::Rag,
        &["DuckAssistBot"],
    )
    .docs("https://duckduckgo.com/duckduckgo-help-pages/results/duckassistbot/"),
    AgentDef::new(
        "duckduckbot",
        "DuckDuckBot",
        "duckduckgo",
        Purpose::Search,
        &["DuckDuckBot"],
    )
    .docs("https://duckduckgo.com/duckduckgo-help-pages/results/duckduckbot/"),
    // -- Allen Institute for AI -------------------------------------------
    // The `AI2Bot` pattern also covers `AI2Bot-Dolma`, the corpus variant.
    AgentDef::new(
        "ai2bot",
        "AI2Bot",
        "allen-institute",
        Purpose::Training,
        &["AI2Bot"],
    )
    .docs("https://allenai.org/crawler"),
    // -- Diffbot -----------------------------------------------------------
    AgentDef::new(
        "diffbot",
        "Diffbot",
        "diffbot",
        Purpose::Training,
        &["Diffbot"],
    ),
    // -- Webz.io -----------------------------------------------------------
    // Two tokens, one shorter than the other. This pair is why `find_by_ua`
    // sorts longest-first: `omgili` is a substring of `omgilibot`, and matching
    // in catalog order would attribute every `omgilibot` fetch to the wrong row.
    AgentDef::new(
        "omgilibot",
        "omgilibot",
        "webz-io",
        Purpose::Training,
        &["omgilibot"],
    ),
    AgentDef::new("omgili", "omgili", "webz-io", Purpose::Training, &["omgili"]),
    // -- Timpi -------------------------------------------------------------
    AgentDef::new("timpibot", "Timpibot", "timpi", Purpose::Search, &["Timpibot"]),
    // -- Huawei ------------------------------------------------------------
    AgentDef::new("pangubot", "PanguBot", "huawei", Purpose::Training, &["PanguBot"]),
    AgentDef::new("petalbot", "PetalBot", "huawei", Purpose::Search, &["PetalBot"])
        .docs("https://webmaster.petalsearch.com/site/petalbot"),
    // -- Kangaroo LLM ------------------------------------------------------
    AgentDef::new(
        "kangaroo-bot",
        "Kangaroo Bot",
        "kangaroo-llm",
        Purpose::Training,
        &["Kangaroo Bot"],
    ),
    // -- Image corpora -----------------------------------------------------
    AgentDef::new(
        "imagesiftbot",
        "ImagesiftBot",
        "imagesift",
        Purpose::Training,
        &["ImagesiftBot"],
    )
    .docs("https://imagesift.com/about"),
    // A library rather than a service: it fetches image URLs listed in a dataset
    // and does not read robots.txt on its own, so a site owner cannot opt out of
    // it by writing a rule. Saying so is the point of the field.
    AgentDef::new(
        "img2dataset",
        "img2dataset",
        "open-source",
        Purpose::Training,
        &["img2dataset"],
    )
    .ignores_robots(),
    // -- Firecrawl ---------------------------------------------------------
    AgentDef::new(
        "firecrawl",
        "FirecrawlAgent",
        "firecrawl",
        Purpose::Rag,
        &["FirecrawlAgent"],
    )
    .docs("https://docs.firecrawl.dev/"),
    // -- Semrush -----------------------------------------------------------
    // The OCOB token is Semrush's AI-content crawler; the plain token is their
    // long-standing SEO crawler. Longest-match keeps them apart.
    AgentDef::new(
        "semrushbot-ocob",
        "SemrushBot-OCOB",
        "semrush",
        Purpose::Training,
        &["SemrushBot-OCOB"],
    )
    .docs("https://www.semrush.com/bot/"),
    AgentDef::new(
        "semrushbot",
        "SemrushBot",
        "semrush",
        Purpose::Unknown,
        &["SemrushBot"],
    )
    .docs("https://www.semrush.com/bot/"),
    AgentDef::new(
        "ahrefsbot",
        "AhrefsBot",
        "ahrefs",
        Purpose::Unknown,
        &["AhrefsBot"],
    )
    .docs("https://ahrefs.com/robot"),
    // -- Other search engines ---------------------------------------------
    AgentDef::new("yandexbot", "YandexBot", "yandex", Purpose::Search, &["YandexBot"])
        .rdns(&["yandex.ru", "yandex.net", "yandex.com"])
        .docs("https://yandex.com/support/webmaster/robot-workings/check-yandex-robots.html"),
    AgentDef::new(
        "baiduspider",
        "Baiduspider",
        "baidu",
        Purpose::Search,
        &["Baiduspider"],
    ),
    // -- Link unfurlers ----------------------------------------------------
    // Not AI, but decisively not human either. Without them a shared link looks
    // like a small burst of readers who bounced instantly, which is exactly the
    // kind of quiet lie the product is against.
    AgentDef::new(
        "twitterbot",
        "Twitterbot",
        "x",
        Purpose::Preview,
        &["Twitterbot"],
    ),
    AgentDef::new(
        "linkedinbot",
        "LinkedInBot",
        "linkedin",
        Purpose::Preview,
        &["LinkedInBot"],
    ),
    AgentDef::new("slackbot", "Slackbot", "slack", Purpose::Preview, &["Slackbot"])
        .docs("https://api.slack.com/robots"),
    AgentDef::new(
        "discordbot",
        "Discordbot",
        "discord",
        Purpose::Preview,
        &["Discordbot"],
    ),
    AgentDef::new(
        "telegrambot",
        "TelegramBot",
        "telegram",
        Purpose::Preview,
        &["TelegramBot"],
    ),
];

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

/// Every `(lowercased pattern, definition)` pair, longest pattern first.
///
/// Built once. Sorting by descending length is what makes the match
/// deterministic: `omgili` is a substring of `omgilibot` and `SemrushBot` is a
/// substring of `SemrushBot-OCOB`, so the first match in catalog order would be
/// the wrong one roughly half the time. Ties break alphabetically so that the
/// result never depends on the sort's stability.
static UA_INDEX: Lazy<Vec<(String, &'static AgentDef)>> = Lazy::new(|| {
    let mut index: Vec<(String, &'static AgentDef)> = CATALOG
        .iter()
        .flat_map(|def| {
            def.ua_patterns
                .iter()
                .map(move |pattern| (pattern.to_ascii_lowercase(), def))
        })
        .collect();
    index.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    index
});

/// Find the catalog entry whose product token appears in this user agent.
///
/// Case-insensitive substring match; the longest matching token wins. Substring
/// rather than exact match because a real user agent wraps its token in a
/// sentence — `Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible;
/// GPTBot/1.2; +https://openai.com/gptbot)` — and operators change the version
/// suffix without notice.
pub fn find_by_ua(ua: &str) -> Option<&'static AgentDef> {
    if ua.trim().is_empty() {
        return None;
    }
    let haystack = ua.to_ascii_lowercase();
    UA_INDEX
        .iter()
        .find(|(pattern, _)| haystack.contains(pattern.as_str()))
        .map(|(_, def)| *def)
}

/// Find a catalog entry by its stable slug.
pub fn find_by_slug(slug: &str) -> Option<&'static AgentDef> {
    let slug = slug.trim().to_ascii_lowercase();
    CATALOG.iter().find(|def| def.slug == slug)
}

/// Every agent an operator publishes ranges for, in catalog order.
///
/// This is the work list for the range-refresh job: the agents where a fetch can
/// be *proved* rather than believed.
pub fn verifiable() -> impl Iterator<Item = &'static AgentDef> {
    CATALOG.iter().filter(|def| def.publishes_ranges())
}

/// Every distinct operator name in the catalog, in first-seen order.
pub fn operators() -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for def in CATALOG {
        if !seen.contains(&def.operator) {
            seen.push(def.operator);
        }
    }
    seen
}

// ---------------------------------------------------------------------------
// Search engines
// ---------------------------------------------------------------------------

/// A traditional search engine, for referrer attribution.
///
/// Search sits beside `answer_engine` in [`micaforge_core::Channel`], never
/// above it. This table is what puts a referrer in the `search` bucket; the
/// table in [`crate::answer_engines`] is what puts it in the other one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SearchEngine {
    /// Stable lowercase identifier, written to the `source` column.
    pub slug: &'static str,
    /// The engine's own spelling of its name.
    pub display_name: &'static str,
    /// Registrable hosts. A referrer host matches on equality or as a subdomain,
    /// so `uk.search.yahoo.com` matches `yahoo.com`.
    pub hosts: &'static [&'static str],
    /// The query-string parameter that used to carry the search term. Almost
    /// every engine stopped sending it years ago; it is kept because imported
    /// historical data still has it and throwing it away would lose real facts.
    pub query_param: Option<&'static str>,
}

impl SearchEngine {
    /// A minimal entry.
    const fn new(
        slug: &'static str,
        display_name: &'static str,
        hosts: &'static [&'static str],
        query_param: Option<&'static str>,
    ) -> Self {
        SearchEngine {
            slug,
            display_name,
            hosts,
            query_param,
        }
    }
}

/// The search engines Micaforge attributes by name.
///
/// Google's country domains are not listed: there are close to two hundred of
/// them and they all follow one shape, so [`classify_search_host`] recognises
/// them by rule instead.
pub static SEARCH_ENGINES: &[SearchEngine] = &[
    SearchEngine::new("google", "Google", &["google.com"], Some("q")),
    SearchEngine::new("bing", "Bing", &["bing.com"], Some("q")),
    SearchEngine::new("duckduckgo", "DuckDuckGo", &["duckduckgo.com"], Some("q")),
    SearchEngine::new("yahoo", "Yahoo", &["yahoo.com", "yahoo.co.jp"], Some("p")),
    SearchEngine::new(
        "yandex",
        "Yandex",
        &["yandex.ru", "yandex.com"],
        Some("text"),
    ),
    SearchEngine::new("baidu", "Baidu", &["baidu.com"], Some("wd")),
    SearchEngine::new("ecosia", "Ecosia", &["ecosia.org"], Some("q")),
    SearchEngine::new("brave", "Brave Search", &["search.brave.com"], Some("q")),
    SearchEngine::new("startpage", "Startpage", &["startpage.com"], Some("query")),
    SearchEngine::new("qwant", "Qwant", &["qwant.com"], Some("q")),
    SearchEngine::new("naver", "Naver", &["naver.com"], Some("query")),
    SearchEngine::new("seznam", "Seznam", &["seznam.cz"], Some("q")),
    SearchEngine::new("mojeek", "Mojeek", &["mojeek.com"], Some("q")),
    SearchEngine::new("ask", "Ask", &["ask.com"], Some("q")),
    SearchEngine::new("aol", "AOL", &["aol.com"], Some("q")),
    SearchEngine::new("sogou", "Sogou", &["sogou.com"], Some("query")),
];

/// Attribute a referrer host to a search engine.
///
/// Answer engines are checked first and win. Without that order every arrival
/// from `gemini.google.com` would be filed under Google Search, which would
/// quietly delete the metric this product is built to report.
pub fn classify_search_host(host: &str) -> Option<&'static SearchEngine> {
    let host = crate::normalize_host(host);
    if host.is_empty() {
        return None;
    }
    if crate::answer_engines::classify_referrer_host(&host).is_some() {
        return None;
    }
    if is_google_search_host(&host) {
        return SEARCH_ENGINES.iter().find(|e| e.slug == "google");
    }
    SEARCH_ENGINES
        .iter()
        .find(|engine| engine.hosts.iter().any(|h| crate::host_matches(&host, h)))
}

/// True for `google.com` and its country domains (`google.co.uk`,
/// `google.com.br`, …) including the `www.` and search-subdomain forms.
///
/// Deliberately narrow: only a host whose *first* label is `google`, or whose
/// labels before it are the handful of prefixes Google actually serves search
/// from. `gemini.google.com` and `bard.google.com` therefore do not match here
/// even before the answer-engine check runs.
fn is_google_search_host(host: &str) -> bool {
    let bare = host.strip_prefix("www.").unwrap_or(host);
    bare == "google" || bare.starts_with("google.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_is_internally_consistent() {
        for def in CATALOG {
            assert_eq!(
                def.slug,
                def.slug.to_ascii_lowercase(),
                "slug `{}` must be lowercase",
                def.slug
            );
            assert!(!def.display_name.is_empty(), "{} has no name", def.slug);
            assert!(!def.operator.is_empty(), "{} has no operator", def.slug);
            for pattern in def.ua_patterns {
                assert!(
                    pattern.len() >= 4,
                    "`{pattern}` is too short to match safely"
                );
            }
        }
    }

    #[test]
    fn slugs_are_unique() {
        let mut seen: Vec<&str> = Vec::new();
        for def in CATALOG {
            assert!(!seen.contains(&def.slug), "duplicate slug `{}`", def.slug);
            seen.push(def.slug);
        }
    }

    #[test]
    fn longest_pattern_wins_over_a_shorter_prefix() {
        // The whole reason the index is sorted by length.
        let omgilibot = find_by_ua("Mozilla/5.0 (compatible; omgilibot/0.6)").unwrap();
        assert_eq!(omgilibot.slug, "omgilibot");
        let omgili = find_by_ua("omgili/0.5 +http://omgili.com").unwrap();
        assert_eq!(omgili.slug, "omgili");

        let ocob = find_by_ua("Mozilla/5.0 (compatible; SemrushBot-OCOB/7~bl)").unwrap();
        assert_eq!(ocob.slug, "semrushbot-ocob");
    }

    #[test]
    fn real_user_agent_strings_resolve() {
        let cases = [
            (
                "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko; compatible; GPTBot/1.2; +https://openai.com/gptbot)",
                "gptbot",
            ),
            (
                "Mozilla/5.0 (compatible; ClaudeBot/1.0; +claudebot@anthropic.com)",
                "claudebot",
            ),
            (
                "Mozilla/5.0 (compatible; PerplexityBot/1.0; +https://perplexity.ai/perplexitybot)",
                "perplexitybot",
            ),
            (
                "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
                "googlebot",
            ),
            (
                "meta-externalagent/1.1 (+https://developers.facebook.com/docs/sharing/webmasters/crawler)",
                "meta-externalagent",
            ),
            ("CCBot/2.0 (https://commoncrawl.org/faq/)", "ccbot"),
        ];
        for (ua, slug) in cases {
            assert_eq!(find_by_ua(ua).map(|d| d.slug), Some(slug), "ua: {ua}");
        }
    }

    #[test]
    fn an_agent_that_names_another_agent_still_resolves_to_itself() {
        // Telegram's unfurler really does introduce itself as
        // `TelegramBot (like TwitterBot)`. Longest-match is what keeps that
        // fetch on Telegram's row instead of X's.
        let def = find_by_ua("TelegramBot (like TwitterBot)").unwrap();
        assert_eq!(def.slug, "telegrambot");
    }

    #[test]
    fn matching_is_case_insensitive_and_ignores_versions() {
        assert_eq!(find_by_ua("gptbot/9.9").map(|d| d.slug), Some("gptbot"));
        assert_eq!(find_by_ua("GPTBOT").map(|d| d.slug), Some("gptbot"));
    }

    #[test]
    fn a_browser_is_not_an_agent() {
        let chrome = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                      (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";
        assert!(find_by_ua(chrome).is_none());
        assert!(find_by_ua("").is_none());
        // `Applebot` must not fall out of `AppleWebKit`.
        let safari = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) \
                      AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";
        assert!(find_by_ua(safari).is_none());
    }

    #[test]
    fn robots_token_only_entries_never_match_a_request() {
        let extended = find_by_slug("google-extended").unwrap();
        assert!(extended.is_robots_token_only());
        assert_eq!(extended.robots_token(), "Google-Extended");
        // It has no pattern, so nothing can be attributed to it.
        assert!(find_by_ua("Google-Extended").is_none());
    }

    #[test]
    fn search_hosts_resolve_without_stealing_answer_engines() {
        assert_eq!(
            classify_search_host("www.google.co.uk").map(|e| e.slug),
            Some("google")
        );
        assert_eq!(
            classify_search_host("google.com").map(|e| e.slug),
            Some("google")
        );
        assert_eq!(
            classify_search_host("uk.search.yahoo.com").map(|e| e.slug),
            Some("yahoo")
        );
        // The line the product is built on.
        assert!(classify_search_host("gemini.google.com").is_none());
        assert!(classify_search_host("chatgpt.com").is_none());
        assert!(classify_search_host("example.com").is_none());
    }
}
