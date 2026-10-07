//! Where a visit came from: channel, source, campaign.
//!
//! Attribution is the one derived field an operator will argue with, so the
//! rule is written down rather than inferred from the code. In order, first
//! match wins:
//!
//! 1. **`utm_medium`.** An explicit tag is the site owner telling us what this
//!    link is. Nothing outranks it.
//! 2. **A click id.** `gclid`, `fbclid`, `msclkid`, `ttclid` and their
//!    siblings are stamped by an ad platform and mean the click was paid for,
//!    whatever the referrer says.
//! 3. **An answer engine**, resolved through the callback described below.
//! 4. **A search engine**, from a compiled-in table.
//! 5. **A social network**, likewise.
//! 6. **A webmail client**, likewise.
//! 7. **The site itself** — same registrable domain as the page. Kept as
//!    `internal` rather than folded into `direct`, so a self-referral is
//!    visible instead of quietly inflating direct traffic.
//! 8. **Any other referrer** → `referral`.
//! 9. **Nothing at all** → `direct`.
//!
//! One documented refinement to step 9: a hit with no referrer but a
//! `utm_source` is reported as `referral` with that source, not as `direct`.
//! Somebody tagged that link on purpose, and burying a tagged campaign in the
//! direct bucket is the failure mode this whole module exists to avoid.
//!
//! ## Why the answer-engine lookup is a callback
//!
//! Answer engines are the product's reason to exist, and their list changes
//! whenever a new assistant ships — so it lives in Postgres, in the agent
//! catalogue, owned by `micaforge-agents`. If this crate looked it up itself it
//! would need that crate, which needs a store, which needs a database, which
//! would mean the ingest hot path and every test in this file dragged a
//! connection pool behind them. Instead the server passes a closure over the
//! catalogue it already holds in memory. `micaforge-enrich` stays a pure
//! function of its inputs, and the two crates never see each other.

use std::collections::HashMap;

use micaforge_core::Channel;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use url::Url;

/// The acquisition answer for one hit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attribution {
    /// The bucket this hit belongs to.
    pub channel: Channel,
    /// The resolved name within the channel: `google`, `chatgpt`, `reddit`,
    /// `news.ycombinator.com`. Empty for `direct`.
    ///
    /// A name resolved from a table beats a hand-typed `utm_source`, because
    /// the table spells it the same way every time and a link author does not.
    pub source: String,
    /// The referrer's host, lowercased with a leading `www.` removed. Empty
    /// when there was no referrer.
    pub referrer_host: String,
}

/// The campaign tags on a URL.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Utm {
    /// `utm_source`, or `ref` when `utm_source` is absent.
    pub source: String,
    /// `utm_medium`.
    pub medium: String,
    /// `utm_campaign`.
    pub campaign: String,
    /// `utm_term`.
    pub term: String,
    /// `utm_content`.
    pub content: String,
    /// The value of whichever ad-platform click id was present. Which platform
    /// it came from is recoverable from [`Attribution::source`]; the raw value
    /// is stored so an operator can match a row against their ad account.
    pub click_id: String,
}

/// Longest campaign value stored. These land in `LowCardinality` columns, and
/// an unbounded distinct set is the one thing that encoding is bad at.
const MAX_VALUE_LEN: usize = 255;

/// Most query parameters kept on one event.
const MAX_PARAMS: usize = 32;

/// Longest querystring stored.
const MAX_QUERYSTRING_LEN: usize = 2048;

/// Ad-platform click ids, and the network each one belongs to.
const CLICK_IDS: &[(&str, &str)] = &[
    ("gclid", "google"),
    ("gbraid", "google"),
    ("wbraid", "google"),
    ("dclid", "google"),
    ("msclkid", "bing"),
    ("fbclid", "facebook"),
    ("ttclid", "tiktok"),
    ("twclid", "twitter"),
    ("li_fat_id", "linkedin"),
    ("rdt_cid", "reddit"),
    ("sccid", "snapchat"),
    ("epik", "pinterest"),
    ("yclid", "yandex"),
];

/// Parameters stripped from the stored querystring.
///
/// They are already captured as their own columns, and leaving them in means
/// the same page appears as a hundred distinct querystrings in a breakdown.
const STRIPPED_PARAMS: &[&str] = &[
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "utm_id",
    "utm_source_platform",
    "utm_creative_format",
    "utm_marketing_tactic",
    "ref",
    "source",
    "mtm_source",
    "mtm_medium",
    "mtm_campaign",
    "_ga",
    "_gl",
    "mc_cid",
    "mc_eid",
    "igshid",
    "si",
    "fbadid",
    "vero_id",
    "vero_conv",
    "s_kwcid",
    "hsa_acc",
    "hsa_cam",
    "hsa_grp",
    "hsa_ad",
    "hsa_src",
    "hsa_tgt",
    "hsa_kw",
    "hsa_mt",
    "hsa_net",
    "hsa_ver",
];

/// Hosts, and app package names, that are search engines.
const SEARCH_HOSTS: &[(&str, &str)] = &[
    ("duckduckgo.com", "duckduckgo"),
    ("search.brave.com", "brave"),
    ("search.marginalia.nu", "marginalia"),
    ("kagi.com", "kagi"),
    ("ecosia.org", "ecosia"),
    ("startpage.com", "startpage"),
    ("qwant.com", "qwant"),
    ("baidu.com", "baidu"),
    ("naver.com", "naver"),
    ("daum.net", "daum"),
    ("seznam.cz", "seznam"),
    ("sogou.com", "sogou"),
    ("so.com", "360-search"),
    ("ask.com", "ask"),
    ("aol.com", "aol"),
    ("mojeek.com", "mojeek"),
    ("lycos.com", "lycos"),
    ("yep.com", "yep"),
    ("swisscows.com", "swisscows"),
    ("searx.be", "searx"),
    ("rambler.ru", "rambler"),
    ("goo.ne.jp", "goo"),
    ("nate.com", "nate"),
    ("com.google.android.googlequicksearchbox", "google"),
];

/// Brands that operate on dozens of country domains, matched on the first
/// label of the registrable domain so that `google.co.uk` and `google.com.br`
/// need no entries of their own.
const SEARCH_BRANDS: &[(&str, &str)] = &[
    ("google", "google"),
    ("bing", "bing"),
    ("yahoo", "yahoo"),
    ("yandex", "yandex"),
    ("duckduckgo", "duckduckgo"),
    ("ecosia", "ecosia"),
    ("qwant", "qwant"),
    ("startpage", "startpage"),
];

/// Hosts, and app package names, that are social networks or communities.
const SOCIAL_HOSTS: &[(&str, &str)] = &[
    ("facebook.com", "facebook"),
    ("fb.com", "facebook"),
    ("fb.me", "facebook"),
    ("l.facebook.com", "facebook"),
    ("lm.facebook.com", "facebook"),
    ("com.facebook.katana", "facebook"),
    ("instagram.com", "instagram"),
    ("l.instagram.com", "instagram"),
    ("com.instagram.android", "instagram"),
    ("threads.net", "threads"),
    ("threads.com", "threads"),
    ("x.com", "x"),
    ("twitter.com", "x"),
    ("t.co", "x"),
    ("com.twitter.android", "x"),
    ("linkedin.com", "linkedin"),
    ("lnkd.in", "linkedin"),
    ("com.linkedin.android", "linkedin"),
    ("reddit.com", "reddit"),
    ("redd.it", "reddit"),
    ("out.reddit.com", "reddit"),
    ("com.reddit.frontpage", "reddit"),
    ("news.ycombinator.com", "hacker-news"),
    ("lobste.rs", "lobsters"),
    ("youtube.com", "youtube"),
    ("youtu.be", "youtube"),
    ("m.youtube.com", "youtube"),
    ("tiktok.com", "tiktok"),
    ("com.zhiliaoapp.musically", "tiktok"),
    ("pinterest.com", "pinterest"),
    ("bsky.app", "bluesky"),
    ("mastodon.social", "mastodon"),
    ("fosstodon.org", "mastodon"),
    ("hachyderm.io", "mastodon"),
    ("discord.com", "discord"),
    ("discordapp.com", "discord"),
    ("t.me", "telegram"),
    ("telegram.me", "telegram"),
    ("web.telegram.org", "telegram"),
    ("wa.me", "whatsapp"),
    ("whatsapp.com", "whatsapp"),
    ("com.whatsapp", "whatsapp"),
    ("slack.com", "slack"),
    ("vk.com", "vk"),
    ("weibo.com", "weibo"),
    ("quora.com", "quora"),
    ("tumblr.com", "tumblr"),
    ("twitch.tv", "twitch"),
    ("snapchat.com", "snapchat"),
    ("xing.com", "xing"),
    ("line.me", "line"),
    ("producthunt.com", "product-hunt"),
];

/// Hosts, and app package names, that are webmail clients.
const EMAIL_HOSTS: &[(&str, &str)] = &[
    ("mail.google.com", "gmail"),
    ("com.google.android.gm", "gmail"),
    ("mail.yahoo.com", "yahoo-mail"),
    ("outlook.live.com", "outlook"),
    ("outlook.office.com", "outlook"),
    ("outlook.office365.com", "outlook"),
    ("com.microsoft.office.outlook", "outlook"),
    ("mail.proton.me", "proton-mail"),
    ("protonmail.com", "proton-mail"),
    ("mail.zoho.com", "zoho-mail"),
    ("mail.aol.com", "aol-mail"),
    ("mail.yandex.ru", "yandex-mail"),
    ("e.mail.ru", "mail-ru"),
    ("mail.ru", "mail-ru"),
    ("app.fastmail.com", "fastmail"),
    ("fastmail.com", "fastmail"),
    ("app.hey.com", "hey"),
    ("hey.com", "hey"),
    ("mail.tutanota.com", "tutanota"),
    ("app.tuta.com", "tuta"),
    ("mail.superhuman.com", "superhuman"),
    ("roundcube.net", "roundcube"),
];

/// Two-and-three-label public suffixes, enough to get the registrable domain
/// right for the hosts that actually appear in referrer logs.
///
/// This is an approximation of the Public Suffix List, and it is one on
/// purpose. The real list is a quarter of a megabyte that has to be shipped and
/// kept current, and a self-hosted install may not fetch it at runtime. The
/// only decisions this function makes are "is this referrer the site itself"
/// and "which table entry does `m.facebook.com` fall back to". Getting an
/// exotic suffix wrong shows up as one extra referral row, never as lost data.
const MULTI_LABEL_SUFFIXES: &[&str] = &[
    "co.uk",
    "org.uk",
    "ac.uk",
    "gov.uk",
    "me.uk",
    "net.uk",
    "sch.uk",
    "com.au",
    "net.au",
    "org.au",
    "edu.au",
    "gov.au",
    "co.nz",
    "net.nz",
    "org.nz",
    "govt.nz",
    "co.za",
    "org.za",
    "com.br",
    "net.br",
    "org.br",
    "gov.br",
    "com.mx",
    "com.ar",
    "com.tr",
    "com.cn",
    "net.cn",
    "org.cn",
    "gov.cn",
    "co.jp",
    "or.jp",
    "ne.jp",
    "ac.jp",
    "go.jp",
    "co.kr",
    "or.kr",
    "co.in",
    "net.in",
    "org.in",
    "co.id",
    "or.id",
    "com.sg",
    "com.hk",
    "com.tw",
    "com.my",
    "com.ph",
    "com.vn",
    "com.pk",
    "com.eg",
    "com.sa",
    "com.ua",
    "com.pl",
    "com.ru",
    "co.il",
    "com.co",
    "com.pe",
    "com.ve",
    "com.ec",
    "com.uy",
    "co.th",
    "in.th",
    "com.ng",
    "com.gh",
    "co.ke",
    "com.es",
    "com.pt",
    "com.cy",
    "co.at",
    "or.at",
    "github.io",
    "gitlab.io",
    "pages.dev",
    "workers.dev",
    "vercel.app",
    "netlify.app",
    "herokuapp.com",
    "blogspot.com",
    "wordpress.com",
    "substack.com",
    "notion.site",
    "fly.dev",
    "onrender.com",
    "web.app",
    "firebaseapp.com",
];

static SEARCH: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| build(SEARCH_HOSTS));
static SOCIAL: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| build(SOCIAL_HOSTS));
static EMAIL: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| build(EMAIL_HOSTS));

fn build(table: &'static [(&'static str, &'static str)]) -> HashMap<&'static str, &'static str> {
    table.iter().copied().collect()
}

/// Attribute one hit.
///
/// - `referrer` is the raw `document.referrer` (or the `Referer` header for a
///   server-side hit). A bare host, a full URL and an `android-app://` URI all
///   work; anything unparseable is treated as no referrer.
/// - `url` is the page URL, read only for its campaign parameters.
/// - `site_host` is the site's own domain, used for the self-referral check.
///   Pass `""` to skip it.
/// - `answer_engine_lookup` maps a normalised referrer host to a catalogue slug
///   (`"chatgpt.com"` → `Some("chatgpt")`). See the module docs for why this is
///   a callback.
///
/// ```
/// # use micaforge_enrich::referrer::attribute;
/// # use micaforge_core::Channel;
/// let url = url::Url::parse("https://example.com/docs").unwrap();
/// let lookup = |host: &str| (host == "chatgpt.com").then_some("chatgpt");
/// let a = attribute(Some("https://chatgpt.com/"), &url, "example.com", &lookup);
/// assert_eq!(a.channel, Channel::AnswerEngine);
/// assert_eq!(a.source, "chatgpt");
/// ```
pub fn attribute(
    referrer: Option<&str>,
    url: &Url,
    site_host: &str,
    answer_engine_lookup: &dyn Fn(&str) -> Option<&'static str>,
) -> Attribution {
    let utm = parse_utm(url);
    let referrer_host = referrer.and_then(referrer_host).unwrap_or_default();

    // 1. An explicit medium outranks everything, including a click id: an
    //    operator who tagged a link has told us what it is.
    if !utm.medium.is_empty() {
        if let Some(channel) = channel_from_medium(&utm.medium) {
            let source = first_non_empty(&[
                utm.source.as_str(),
                referrer_host.as_str(),
                utm.medium.as_str(),
            ]);
            return Attribution {
                channel,
                source,
                referrer_host,
            };
        }
    }

    // 2. A click id means an ad platform sent this click, whatever the
    //    referrer host happens to be after the redirect chain.
    if !utm.click_id.is_empty() {
        if let Some(network) = click_id_network(url) {
            return Attribution {
                channel: Channel::Paid,
                source: network.to_string(),
                referrer_host,
            };
        }
    }

    if !referrer_host.is_empty() {
        // A checkout or sign-in page handing the visitor back. That is the same
        // visit continuing after a payment or a login, not a site that sent a
        // new reader, so it is filed as internal rather than as acquisition.
        // First, because `accounts.google.com` would otherwise read as search.
        if is_return_host(&referrer_host) {
            return Attribution {
                channel: Channel::Internal,
                source: referrer_host.clone(),
                referrer_host,
            };
        }

        // 3. The wedge. Checked before search, because several answer engines
        //    also run a conventional search index and would otherwise be
        //    counted as one.
        if let Some(slug) = answer_engine_lookup(&referrer_host) {
            return Attribution {
                channel: Channel::AnswerEngine,
                source: slug.to_string(),
                referrer_host,
            };
        }

        if let Some(slug) = search_source(&referrer_host) {
            return Attribution {
                channel: Channel::Search,
                source: slug.to_string(),
                referrer_host,
            };
        }
        if let Some(slug) = table_source(&SOCIAL, &referrer_host) {
            return Attribution {
                channel: Channel::Social,
                source: slug.to_string(),
                referrer_host,
            };
        }
        if let Some(slug) = table_source(&EMAIL, &referrer_host) {
            return Attribution {
                channel: Channel::Email,
                source: slug.to_string(),
                referrer_host,
            };
        }

        // 7. Our own site. A self-referral is a real event — it is what an
        //    outbound-link click back to the page looks like — so it gets its
        //    own channel rather than being counted as a fresh direct visit.
        if same_site(&referrer_host, site_host) {
            return Attribution {
                channel: Channel::Internal,
                source: referrer_host.clone(),
                referrer_host,
            };
        }

        return Attribution {
            channel: Channel::Referral,
            source: referrer_host.clone(),
            referrer_host,
        };
    }

    // No referrer. A tagged link is still not direct traffic: see the module
    // docs for why this deviates from a strict reading of the order.
    if !utm.source.is_empty() {
        return Attribution {
            channel: Channel::Referral,
            source: utm.source,
            referrer_host,
        };
    }

    Attribution {
        channel: Channel::Direct,
        source: String::new(),
        referrer_host,
    }
}

/// Read the campaign tags off a URL.
///
/// `ref` is accepted as a `utm_source` alias because many sites tag their links
/// with `?ref=` and imported Plausible data carries it; an explicit `utm_source`
/// still wins.
pub fn parse_utm(url: &Url) -> Utm {
    let mut utm = Utm::default();
    let mut ref_alias = String::new();

    for (key, value) in url.query_pairs() {
        let key = key.to_ascii_lowercase();
        let value = clamp(value.trim(), MAX_VALUE_LEN);
        if value.is_empty() {
            continue;
        }

        match key.as_str() {
            "utm_source" => utm.source = value,
            "utm_medium" => utm.medium = value,
            "utm_campaign" => utm.campaign = value,
            "utm_term" => utm.term = value,
            "utm_content" => utm.content = value,
            "ref" | "source" if ref_alias.is_empty() => ref_alias = value,
            other => {
                if utm.click_id.is_empty() && CLICK_IDS.iter().any(|(name, _)| *name == other) {
                    utm.click_id = value;
                }
            }
        }
    }

    if utm.source.is_empty() {
        utm.source = ref_alias;
    }
    utm
}

/// Split a URL's query into the part worth storing and its parsed pairs.
///
/// Returns the querystring with the campaign and tracking parameters removed
/// (no leading `?`), and the same parameters as pairs for the `url_params`
/// map column. Both are bounded: at most [`MAX_PARAMS`] parameters, values
/// clamped to [`MAX_VALUE_LEN`], and the whole string to
/// [`MAX_QUERYSTRING_LEN`]. A page whose query is a session token dump should
/// cost one row, not a partition.
pub fn strip_and_collect_params(url: &Url) -> (String, Vec<(String, String)>) {
    let mut pairs: Vec<(String, String)> = Vec::new();

    for (key, value) in url.query_pairs() {
        if pairs.len() >= MAX_PARAMS {
            break;
        }
        let lower = key.to_ascii_lowercase();
        if lower.starts_with("utm_")
            || STRIPPED_PARAMS.contains(&lower.as_str())
            || CLICK_IDS.iter().any(|(name, _)| *name == lower)
        {
            continue;
        }
        let key = clamp(key.trim(), 64);
        if key.is_empty() {
            continue;
        }
        pairs.push((key, clamp(value.trim(), MAX_VALUE_LEN)));
    }

    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in &pairs {
        serializer.append_pair(key, value);
    }
    let querystring = clamp(&serializer.finish(), MAX_QUERYSTRING_LEN);

    (querystring, pairs)
}

/// The referrer's host, normalised the way it is stored.
///
/// Accepts a full URL, an `android-app://` URI, or a bare host. Returns `None`
/// for anything with no host at all, which includes `about:blank` and the
/// empty string a same-page navigation produces.
pub fn referrer_host(referrer: &str) -> Option<String> {
    let raw = referrer.trim();
    if raw.is_empty() {
        return None;
    }

    let parsed = Url::parse(raw).or_else(|_| Url::parse(&format!("https://{raw}")));
    let host = parsed.ok()?.host_str()?.to_string();
    Some(normalise_host(&host))
}

/// Lowercase, drop a trailing dot, drop a leading `www.`.
///
/// `www.` is stripped because nobody wants `www.example.com` and
/// `example.com` as two rows in a referrer breakdown. No other subdomain is
/// touched: `blog.example.com` and `docs.example.com` are genuinely different
/// sources.
pub fn normalise_host(host: &str) -> String {
    let lower = host.trim().trim_end_matches('.').to_ascii_lowercase();
    lower.strip_prefix("www.").unwrap_or(&lower).to_string()
}

/// The registrable domain (`eTLD+1`) of a host, approximated.
///
/// See [`MULTI_LABEL_SUFFIXES`] for the shape of the approximation and why the
/// real Public Suffix List is not loaded.
pub fn registrable_domain(host: &str) -> &str {
    let host = host.trim_end_matches('.');
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() <= 2 {
        return host;
    }

    let last_two_start =
        host.len() - labels[labels.len() - 2].len() - 1 - labels[labels.len() - 1].len();
    let last_two = &host[last_two_start..];

    if MULTI_LABEL_SUFFIXES.contains(&last_two) && labels.len() >= 3 {
        let third = labels[labels.len() - 3];
        return &host[last_two_start - third.len() - 1..];
    }
    last_two
}

/// Do these two hosts belong to the same site?
pub fn same_site(referrer_host: &str, site_host: &str) -> bool {
    if site_host.is_empty() || referrer_host.is_empty() {
        return false;
    }
    let site = normalise_host(site_host);
    registrable_domain(referrer_host) == registrable_domain(&site)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// The channel an explicit `utm_medium` names.
///
/// Returning `None` for an unrecognised medium is deliberate: the hit falls
/// through to the referrer rules rather than being forced into `referral`,
/// because a medium of `chatgpt` alongside a `chatgpt.com` referrer should
/// still land in `answer_engine`.
fn channel_from_medium(medium: &str) -> Option<Channel> {
    match medium.to_ascii_lowercase().as_str() {
        "cpc" | "ppc" | "paid" | "paidsearch" | "paid_search" | "paid-search" | "cpm" | "cpv"
        | "cpa" | "cpp" | "display" | "banner" | "retargeting" | "remarketing" | "paidsocial"
        | "paid_social" | "paid-social" => Some(Channel::Paid),
        "email" | "e-mail" | "e_mail" | "newsletter" | "mail" => Some(Channel::Email),
        "social" | "social-network" | "social_network" | "social-media" | "social_media" | "sm" => {
            Some(Channel::Social)
        }
        "organic" | "search" | "organic_search" | "organic-search" => Some(Channel::Search),
        "answer_engine" | "answer-engine" | "ai" | "llm" | "genai" => Some(Channel::AnswerEngine),
        "referral" | "referrer" | "affiliate" | "link" => Some(Channel::Referral),
        "internal" | "self" => Some(Channel::Internal),
        _ => None,
    }
}

/// Which ad network stamped the click id on this URL.
fn click_id_network(url: &Url) -> Option<&'static str> {
    for (key, _) in url.query_pairs() {
        let lower = key.to_ascii_lowercase();
        if let Some((_, network)) = CLICK_IDS.iter().find(|(name, _)| *name == lower) {
            return Some(*network);
        }
    }
    None
}

/// Subdomains a search engine actually serves results from.
///
/// The guard on the brand fallback below. Without it, every host under
/// `google.com` becomes a search referral, and `mail.google.com` — an email
/// client — is the loudest example.
const SEARCH_SUBDOMAINS: &[&str] = &["search", "m", "news", "images", "cse", "encrypted", "lite"];

/// A search engine's slug, by exact host, then by registrable domain, then by
/// brand (which is what covers `google.co.uk` and its sixty siblings).
///
/// The brand fallback only fires on a host that is plausibly the search
/// product itself: the bare registrable domain (which is what `www.google.de`
/// normalises to) or one of the subdomains results are served from. Every
/// other `*.google.com` host is a different product and resolves further down
/// the chain — `mail.google.com` as email, `docs.google.com` as a referral.
fn search_source(host: &str) -> Option<&'static str> {
    if let Some(slug) = table_source(&SEARCH, host) {
        return Some(slug);
    }

    let domain = registrable_domain(host);
    let is_search_host = host == domain
        || host
            .strip_suffix(domain)
            .and_then(|prefix| prefix.strip_suffix('.'))
            .is_some_and(|sub| SEARCH_SUBDOMAINS.contains(&sub));
    if !is_search_host {
        return None;
    }

    let brand = domain.split('.').next()?;
    SEARCH_BRANDS
        .iter()
        .find(|(label, _)| *label == brand)
        .map(|(_, slug)| *slug)
}

/// Look a host up in one table, falling back to its registrable domain so that
/// `m.facebook.com` and `l.facebook.com` resolve without their own entries.
fn table_source(table: &HashMap<&'static str, &'static str>, host: &str) -> Option<&'static str> {
    if let Some(slug) = table.get(host) {
        return Some(*slug);
    }
    table.get(registrable_domain(host)).copied()
}

/// Checkout and sign-in hosts a visitor comes back from in the middle of a
/// visit. Exact hosts, never registrable domains: `google.com` itself is a
/// search engine, and only its sign-in host is a return.
const RETURN_HOSTS: &[&str] = &[
    "checkout.stripe.com",
    "pay.stripe.com",
    "buy.stripe.com",
    "billing.stripe.com",
    "invoice.stripe.com",
    "paypal.com",
    "www.paypal.com",
    "checkout.paddle.com",
    "buy.paddle.com",
    "pay.paddle.io",
    "checkout.lemonsqueezy.com",
    "nowpayments.io",
    "account.nowpayments.io",
    "checkout.shopify.com",
    "accounts.google.com",
    "appleid.apple.com",
    "login.microsoftonline.com",
    "login.live.com",
];

/// Is this referrer a checkout or sign-in page returning the visitor?
fn is_return_host(host: &str) -> bool {
    RETURN_HOSTS.contains(&host) || host.ends_with(".lemonsqueezy.com")
}

/// The first of these that is not empty, owned.
fn first_non_empty(candidates: &[&str]) -> String {
    candidates
        .iter()
        .find(|c| !c.is_empty())
        .map(|c| (*c).to_string())
        .unwrap_or_default()
}

/// Cut a string to `max` bytes without splitting a character.
fn clamp(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in for the catalogue `micaforge-agents` owns.
    fn answer_engines(host: &str) -> Option<&'static str> {
        match host {
            "chatgpt.com" | "chat.openai.com" => Some("chatgpt"),
            "perplexity.ai" => Some("perplexity"),
            "claude.ai" => Some("claude"),
            "gemini.google.com" => Some("gemini"),
            "copilot.microsoft.com" => Some("copilot"),
            _ => None,
        }
    }

    fn page(url: &str) -> Url {
        Url::parse(url).unwrap()
    }

    fn attribute_for(referrer: Option<&str>, url: &str) -> Attribution {
        attribute(referrer, &page(url), "example.com", &answer_engines)
    }

    // -- returns from checkout and sign-in -----------------------------------

    #[test]
    fn a_return_from_checkout_or_sign_in_is_the_same_visit_not_a_referral() {
        for referrer in [
            "https://checkout.stripe.com/c/pay/cs_live_x",
            "https://www.paypal.com/checkoutnow?token=x",
            "https://acme.lemonsqueezy.com/checkout/buy/x",
            "https://accounts.google.com/o/oauth2/v2/auth",
        ] {
            let a = attribute_for(Some(referrer), "https://example.com/thanks");
            assert_eq!(a.channel, Channel::Internal, "{referrer}");
        }
        // Google itself is still search; only its sign-in host is a return.
        let a = attribute_for(Some("https://www.google.com/"), "https://example.com/");
        assert_eq!(a.channel, Channel::Search);
    }

    // -- the wedge ----------------------------------------------------------

    #[test]
    fn chatgpt_is_an_answer_engine_not_a_referral() {
        let a = attribute_for(Some("https://chatgpt.com/"), "https://example.com/docs");
        assert_eq!(a.channel, Channel::AnswerEngine);
        assert_eq!(a.source, "chatgpt");
        assert_eq!(a.referrer_host, "chatgpt.com");
    }

    #[test]
    fn perplexity_is_an_answer_engine() {
        let a = attribute_for(
            Some("https://www.perplexity.ai/search?q=x"),
            "https://example.com/",
        );
        assert_eq!(a.channel, Channel::AnswerEngine);
        assert_eq!(a.source, "perplexity");
        assert_eq!(a.referrer_host, "perplexity.ai");
    }

    #[test]
    fn gemini_is_an_answer_engine_even_though_it_is_a_google_host() {
        // The whole point of checking the callback before the search table:
        // `gemini.google.com` would otherwise resolve to the search brand.
        let a = attribute_for(
            Some("https://gemini.google.com/app"),
            "https://example.com/",
        );
        assert_eq!(a.channel, Channel::AnswerEngine);
        assert_eq!(a.source, "gemini");
    }

    #[test]
    fn an_unknown_ai_host_is_an_ordinary_referral() {
        // The catalogue is the authority; this crate never guesses one.
        let a = attribute_for(
            Some("https://some-new-assistant.example.net/"),
            "https://example.com/",
        );
        assert_eq!(a.channel, Channel::Referral);
        assert_eq!(a.source, "some-new-assistant.example.net");
    }

    #[test]
    fn an_answer_engine_tagged_as_ai_medium_still_lands_in_the_channel() {
        let a = attribute_for(
            Some("https://chatgpt.com/"),
            "https://example.com/?utm_medium=ai&utm_source=chatgpt",
        );
        assert_eq!(a.channel, Channel::AnswerEngine);
        assert_eq!(a.source, "chatgpt");
    }

    // -- paid click ids -----------------------------------------------------

    #[test]
    fn gclid_is_paid_google() {
        let a = attribute_for(
            Some("https://www.google.com/"),
            "https://example.com/?gclid=EAIaIQobChMI",
        );
        assert_eq!(a.channel, Channel::Paid);
        assert_eq!(a.source, "google");
        assert_eq!(a.referrer_host, "google.com");
    }

    #[test]
    fn fbclid_is_paid_facebook() {
        let a = attribute_for(
            Some("https://l.facebook.com/"),
            "https://example.com/?fbclid=IwAR1",
        );
        assert_eq!(a.channel, Channel::Paid);
        assert_eq!(a.source, "facebook");
    }

    #[test]
    fn msclkid_is_paid_bing() {
        let a = attribute_for(None, "https://example.com/?msclkid=abc123");
        assert_eq!(a.channel, Channel::Paid);
        assert_eq!(a.source, "bing");
    }

    #[test]
    fn ttclid_is_paid_tiktok() {
        let a = attribute_for(
            Some("https://www.tiktok.com/"),
            "https://example.com/?ttclid=E.C.P",
        );
        assert_eq!(a.channel, Channel::Paid);
        assert_eq!(a.source, "tiktok");
    }

    #[test]
    fn a_click_id_beats_the_referrer_but_not_an_explicit_medium() {
        let a = attribute_for(
            Some("https://www.google.com/"),
            "https://example.com/?gclid=x&utm_medium=email&utm_source=weekly",
        );
        assert_eq!(a.channel, Channel::Email);
        assert_eq!(a.source, "weekly");
    }

    #[test]
    fn the_click_id_value_is_kept() {
        let utm = parse_utm(&page("https://example.com/?gclid=EAIaIQobChMI"));
        assert_eq!(utm.click_id, "EAIaIQobChMI");
    }

    // -- the rest of the order ---------------------------------------------

    #[test]
    fn google_on_any_country_domain_is_search() {
        for referrer in [
            "https://www.google.com/",
            "https://www.google.co.uk/",
            "https://www.google.com.br/",
            "https://google.de/",
        ] {
            let a = attribute_for(Some(referrer), "https://example.com/");
            assert_eq!(a.channel, Channel::Search, "{referrer}");
            assert_eq!(a.source, "google", "{referrer}");
        }
    }

    #[test]
    fn duckduckgo_is_search() {
        let a = attribute_for(Some("https://duckduckgo.com/"), "https://example.com/");
        assert_eq!(a.channel, Channel::Search);
        assert_eq!(a.source, "duckduckgo");
    }

    #[test]
    fn social_resolves_through_the_registrable_domain() {
        let a = attribute_for(Some("https://m.facebook.com/story"), "https://example.com/");
        assert_eq!(a.channel, Channel::Social);
        assert_eq!(a.source, "facebook");
    }

    #[test]
    fn hacker_news_is_social() {
        let a = attribute_for(
            Some("https://news.ycombinator.com/item?id=1"),
            "https://example.com/",
        );
        assert_eq!(a.channel, Channel::Social);
        assert_eq!(a.source, "hacker-news");
    }

    #[test]
    fn another_product_on_a_search_engines_domain_is_not_search() {
        // `mail.google.com` is an email client that happens to live under
        // google.com; a greedy brand match reported it as a search referral.
        let a = attribute_for(
            Some("https://docs.google.com/document/d/1"),
            "https://example.com/",
        );
        assert_eq!(a.channel, Channel::Referral);
        assert_eq!(a.source, "docs.google.com");
    }

    #[test]
    fn a_search_subdomain_still_resolves_to_the_brand() {
        let a = attribute_for(
            Some("https://search.yahoo.com/search?p=x"),
            "https://example.com/",
        );
        assert_eq!(a.channel, Channel::Search);
        assert_eq!(a.source, "yahoo");
    }

    #[test]
    fn gmail_is_email() {
        let a = attribute_for(Some("https://mail.google.com/"), "https://example.com/");
        assert_eq!(a.channel, Channel::Email);
        assert_eq!(a.source, "gmail");
    }

    #[test]
    fn the_gmail_android_app_is_email() {
        let a = attribute_for(
            Some("android-app://com.google.android.gm/"),
            "https://example.com/",
        );
        assert_eq!(a.channel, Channel::Email);
        assert_eq!(a.source, "gmail");
    }

    #[test]
    fn a_self_referral_is_internal() {
        let a = attribute_for(Some("https://docs.example.com/a"), "https://example.com/b");
        assert_eq!(a.channel, Channel::Internal);
        assert_eq!(a.referrer_host, "docs.example.com");
    }

    #[test]
    fn a_different_github_pages_site_is_not_internal() {
        let a = attribute(
            Some("https://someone-else.github.io/"),
            &page("https://ours.github.io/"),
            "ours.github.io",
            &answer_engines,
        );
        assert_eq!(a.channel, Channel::Referral);
    }

    #[test]
    fn no_referrer_is_direct() {
        let a = attribute_for(None, "https://example.com/");
        assert_eq!(a.channel, Channel::Direct);
        assert_eq!(a.source, "");
        assert_eq!(a.referrer_host, "");
    }

    #[test]
    fn an_empty_referrer_string_is_direct() {
        let a = attribute_for(Some("   "), "https://example.com/");
        assert_eq!(a.channel, Channel::Direct);
    }

    #[test]
    fn a_tagged_link_with_no_referrer_is_not_direct() {
        let a = attribute_for(None, "https://example.com/?utm_source=printed-sticker");
        assert_eq!(a.channel, Channel::Referral);
        assert_eq!(a.source, "printed-sticker");
    }

    #[test]
    fn an_unrecognised_medium_falls_through_to_the_referrer() {
        let a = attribute_for(
            Some("https://duckduckgo.com/"),
            "https://example.com/?utm_medium=qr-code",
        );
        assert_eq!(a.channel, Channel::Search);
        assert_eq!(a.source, "duckduckgo");
    }

    // -- campaign parameters ------------------------------------------------

    #[test]
    fn utm_parameters_are_read() {
        let utm = parse_utm(&page(
            "https://example.com/?utm_source=hn&utm_medium=social&utm_campaign=launch\
             &utm_term=rust&utm_content=top",
        ));
        assert_eq!(utm.source, "hn");
        assert_eq!(utm.medium, "social");
        assert_eq!(utm.campaign, "launch");
        assert_eq!(utm.term, "rust");
        assert_eq!(utm.content, "top");
    }

    #[test]
    fn ref_is_a_utm_source_alias_but_never_overrides_it() {
        assert_eq!(parse_utm(&page("https://example.com/?ref=hn")).source, "hn");
        assert_eq!(
            parse_utm(&page("https://example.com/?ref=hn&utm_source=twitter")).source,
            "twitter"
        );
    }

    #[test]
    fn tracking_parameters_are_stripped_from_the_stored_querystring() {
        let (query, params) = strip_and_collect_params(&page(
            "https://example.com/search?q=rust&page=2&utm_source=hn&gclid=x&_ga=1",
        ));
        assert_eq!(query, "q=rust&page=2");
        assert_eq!(
            params,
            vec![
                ("q".to_string(), "rust".to_string()),
                ("page".to_string(), "2".to_string())
            ]
        );
    }

    #[test]
    fn a_query_with_only_tracking_parameters_stores_nothing() {
        let (query, params) =
            strip_and_collect_params(&page("https://example.com/?utm_source=x&fbclid=y"));
        assert_eq!(query, "");
        assert!(params.is_empty());
    }

    #[test]
    fn the_parameter_map_is_bounded() {
        let many: String = (0..200).map(|i| format!("k{i}=v&")).collect();
        let (_, params) = strip_and_collect_params(&page(&format!("https://example.com/?{many}")));
        assert_eq!(params.len(), MAX_PARAMS);
    }

    // -- host helpers -------------------------------------------------------

    #[test]
    fn hosts_are_normalised() {
        assert_eq!(
            referrer_host("https://WWW.Example.COM/x").unwrap(),
            "example.com"
        );
        assert_eq!(referrer_host("example.com").unwrap(), "example.com");
        assert_eq!(referrer_host("about:blank"), None);
        assert_eq!(referrer_host(""), None);
    }

    #[test]
    fn registrable_domains_handle_multi_label_suffixes() {
        assert_eq!(registrable_domain("example.com"), "example.com");
        assert_eq!(registrable_domain("a.b.example.com"), "example.com");
        assert_eq!(registrable_domain("shop.example.co.uk"), "example.co.uk");
        assert_eq!(registrable_domain("example.co.uk"), "example.co.uk");
        assert_eq!(
            registrable_domain("blog.someone.github.io"),
            "someone.github.io"
        );
        assert_eq!(registrable_domain("localhost"), "localhost");
    }
}
