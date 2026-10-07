//! Write every message to disk so a person can look at it.
//!
//! An email body cannot be reviewed from its source, and it must never be
//! reviewed by sending one to yourself and hoping. Run this, open the files, and
//! read both alternatives before changing anything in `templates`.
//!
//! ```text
//! cargo run -p micaforge-mail --example preview -- /tmp/mail
//! ```

use std::fs;
use std::path::PathBuf;

use micaforge_mail::{templates, AlertPayload, ReportPayload};

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

    let messages = [
        ("verification", templates::verification(base, "demo-token")),
        (
            "password-reset",
            templates::password_reset(base, "demo-token"),
        ),
        (
            "invitation",
            templates::invitation(base, "Reeds & Co", "demo-token"),
        ),
        (
            "report",
            templates::report(base, "reeds.dev", "12 to 18 August", &report),
        ),
        ("alert", templates::alert(base, "reeds.dev", &alert)),
    ];

    for (name, rendered) in messages {
        fs::write(out.join(format!("{name}.html")), &rendered.html)?;
        fs::write(out.join(format!("{name}.txt")), &rendered.text)?;
        println!("{name}: {}", rendered.subject);
    }
    println!("written to {}", out.display());
    Ok(())
}
