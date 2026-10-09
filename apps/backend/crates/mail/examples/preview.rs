//! Write every message to disk so a person can look at it.
//!
//! An email body cannot be reviewed from its source, and it must never be
//! reviewed by sending one to yourself and hoping. Run this, open the files, and
//! read both alternatives, in a light and a dark system theme, before changing
//! anything in `templates`.
//!
//! ```text
//! cargo run -p micaforge-mail --example preview -- /tmp/mail
//! ```

use std::fs;
use std::path::PathBuf;

use micaforge_mail::{
    templates, AlertPayload, DigestItem, DigestPayload, FeedbackPayload, ReportPayload,
};

fn main() -> std::io::Result<()> {
    let out: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/mail-preview".to_string())
        .into();
    fs::create_dir_all(&out)?;

    // Demo numbers, and labelled as such wherever they are shown. `PRODUCT.md`
    // principle 5: nothing invented is ever presented as real.
    let base = "https://app.example.com";
    let report = ReportPayload {
        visitors: 12_481,
        visitors_delta: 0.124,
        pageviews: 38_904,
        sessions: 15_220,
        bounce_rate: 0.41,
        agent_fetches: 91_004,
        answer_arrivals: 320,
        gap_ratio: 284.4,
        top_pages: vec![
            ("/docs/quickstart".to_string(), 4_210),
            ("/docs/self-host".to_string(), 2_880),
            ("/blog/agent-traffic-is-a-channel".to_string(), 1_945),
            ("/".to_string(), 1_402),
            ("/docs/api/stats/breakdown".to_string(), 918),
        ],
        top_sources: vec![
            ("google".to_string(), 3_401),
            ("chatgpt.com".to_string(), 1_190),
            ("news.ycombinator.com".to_string(), 884),
            ("perplexity.ai".to_string(), 402),
        ],
        top_agents: vec![
            ("gptbot".to_string(), 40_112),
            ("claudebot".to_string(), 22_540),
            ("perplexitybot".to_string(), 12_004),
            ("google-extended".to_string(), 9_338),
        ],
    };
    let alert = AlertPayload {
        kind: "agent_surge".to_string(),
        headline: "GPTBot is fetching four times as often".to_string(),
        detail: "Over the last six hours GPTBot fetched 3,004 pages against a \
                 baseline of 740. Eleven of those paths are disallowed in robots.txt."
            .to_string(),
        metric: "fetches, 6h".to_string(),
        observed: 3_004.0,
        expected: 740.0,
        link: format!("{base}/app/1/agents"),
    };

    let digest = DigestPayload {
        period: "29 September to 5 October".to_string(),
        opened: 2,
        resolved: 1,
        items: vec![
            DigestItem {
                title: "Visitors from AI answers rose on /docs/quickstart".to_string(),
                status: "open".to_string(),
                change: "+64% against the previous 7 days".to_string(),
            },
            DigestItem {
                title: "Bounce rate rose".to_string(),
                status: "monitoring".to_string(),
                change: "+18% against the previous 7 days".to_string(),
            },
            DigestItem {
                title: "Pageviews fell".to_string(),
                status: "resolved".to_string(),
                change: "back within 5% of the previous 7 days".to_string(),
            },
        ],
        link: format!("{base}/app/1/insights"),
        ai_configured: true,
    };
    let feedback = FeedbackPayload {
        kind: "bug".to_string(),
        title: "The citation gap chart is empty for a new site".to_string(),
        body: "Demo report. After adding the tracker the chart says no data although the \
               AI crawler table has rows."
            .to_string(),
        source: "dashboard".to_string(),
        reporter: "ada@example.com".to_string(),
        site: Some("reeds.dev".to_string()),
        page_url: Some(format!("{base}/app/1/citation-gap")),
        link: format!("{base}/app/admin"),
    };

    let messages = [
        ("sign-in-code", templates::sign_in_code("048213", 10)),
        (
            "invitation",
            templates::invitation(base, "Reeds & Co", "demo-token"),
        ),
        (
            "report",
            templates::report(base, "reeds.dev", "12 to 18 August", &report),
        ),
        ("alert", templates::alert(base, "reeds.dev", &alert)),
        (
            "insight-digest",
            templates::insight_digest(base, "reeds.dev", &digest),
        ),
        ("feedback", templates::feedback_notice(base, &feedback)),
        (
            "blocked-traffic",
            templates::blocked_traffic(
                base,
                "reeds.dev",
                &templates::BlockedTraffic {
                    day: "8 October".to_string(),
                    denylist: 4_120,
                    not_allowlisted: 0,
                    hostname: 37,
                    threshold: 1_000,
                    link: format!("{base}/app/1/settings/guards"),
                },
            ),
        ),
        (
            "usage-warning",
            templates::usage(
                base,
                &templates::UsageNotice {
                    org: "Reeds & Co".to_string(),
                    plan: "Team".to_string(),
                    month: "October 2026".to_string(),
                    events: 812_400,
                    quota: 1_000_000,
                    reached: false,
                    overage_policy: "Micaforge keeps recording every event past the quota. \
                                     Nothing is dropped."
                        .to_string(),
                    link: format!("{base}/app/settings/billing"),
                },
            ),
        ),
        (
            "payment-failed",
            templates::payment_failed(
                base,
                &templates::PaymentFailed {
                    org: "Reeds & Co".to_string(),
                    plan: "Team".to_string(),
                    link: format!("{base}/app/settings/billing"),
                },
            ),
        ),
    ];

    for (name, rendered) in messages {
        fs::write(out.join(format!("{name}.html")), &rendered.html)?;
        fs::write(out.join(format!("{name}.txt")), &rendered.text)?;
        println!("{name}: {}", rendered.subject);
    }
    println!("written to {}", out.display());
    Ok(())
}
