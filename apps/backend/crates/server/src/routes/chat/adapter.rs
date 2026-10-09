//! The one conversation all three chat apps hold.
//!
//! Slack, Discord and Telegram each deliver a message in their own envelope
//! and take an answer in their own way, and the platform handlers in
//! [`super`] deal with exactly that and nothing else. What a message *means*
//! is decided here, once: [`parse`] turns the text into a [`Command`], and
//! [`handle`] answers it. A command therefore behaves the same whichever app it
//! was typed into, which is the parity the product promises.
//!
//! ## Who may ask
//!
//! Anybody who can post in a linked channel may ask about its site. That is
//! the deliberate meaning of a link: a member of the site's organisation chose
//! to make this channel answer, with a one-time code only a member could
//! generate. An unlinked channel answers nothing about any site, and `unlink`
//! is open to the channel for the same reason a link is: the channel's own
//! people can always stop it talking.

use chrono::Utc;
use micaforge_ai::BaseQuery;
use micaforge_core::{
    new_id, Audience, Dimension, Error, Filter, FilterGroup, FilterNode, FilterOp, FilterValue,
    Result,
};
use micaforge_query::stats;
use micaforge_store::models::chat::{self, ChatLink};
use micaforge_store::models::sites::{self, Site};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::routes::feedback::{self, Reporter};
use crate::state::AppState;

/// Which platform a message came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Slack.
    Slack,
    /// Discord.
    Discord,
    /// Telegram.
    Telegram,
}

impl Platform {
    /// The `chat_links.platform` value.
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::Slack => "slack",
            Platform::Discord => "discord",
            Platform::Telegram => "telegram",
        }
    }

    /// How a command is typed on this platform, for the help text.
    fn syntax(&self, command: &str, argument: Option<&str>) -> String {
        match (self, argument) {
            (Platform::Slack, Some(arg)) => format!("/micaforge {command} <{arg}>"),
            (Platform::Slack, None) => format!("/micaforge {command}"),
            (Platform::Discord, Some(arg)) => format!("/micaforge {command} {arg}:<{arg}>"),
            (Platform::Discord, None) => format!("/micaforge {command}"),
            (Platform::Telegram, Some(arg)) => format!("/{command} <{arg}>"),
            (Platform::Telegram, None) => format!("/{command}"),
        }
    }
}

/// One message, reduced to what the conversation needs.
#[derive(Debug, Clone)]
pub struct Incoming {
    /// Where it came from.
    pub platform: Platform,
    /// Slack team or Discord guild; empty for Telegram.
    pub workspace_id: String,
    /// The channel or chat.
    pub channel_id: String,
    /// The channel's name, when the platform sent one.
    pub channel_label: String,
    /// Who sent it, as the platform names them.
    pub sender: String,
    /// What was asked.
    pub command: Command,
}

/// What a message asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// A question for the AI analyst.
    Ask(String),
    /// Today's headline numbers.
    Stats,
    /// Link this channel with a code from the dashboard.
    Link(String),
    /// Unlink this channel.
    Unlink,
    /// Send feedback to the people who run this install.
    Feedback(String),
    /// What this app can do.
    Help,
}

/// Read the text of a message as a command.
///
/// The same grammar on every platform: an optional leading slash, a command
/// word, and the rest. Anything that is not a command word is a question,
/// which is what lets a person simply type "how was last week?".
pub fn parse(text: &str) -> Command {
    let trimmed = text.trim();
    let without_slash = trimmed.strip_prefix('/').unwrap_or(trimmed);
    let (word, rest) = match without_slash.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (without_slash, ""),
    };
    // Telegram appends the bot's name to a command in a group: `/ask@MfBot`.
    let word = word.split('@').next().unwrap_or(word).to_lowercase();
    match word.as_str() {
        "" | "help" | "start" => Command::Help,
        "stats" | "today" => Command::Stats,
        "link" => Command::Link(rest.to_string()),
        "unlink" => Command::Unlink,
        "feedback" => Command::Feedback(rest.to_string()),
        "ask" => {
            if rest.is_empty() {
                Command::Help
            } else {
                Command::Ask(rest.to_string())
            }
        }
        _ => Command::Ask(trimmed.to_string()),
    }
}

/// The help text, in this platform's syntax.
pub fn help(platform: Platform) -> String {
    let p = platform;
    format!(
        "Micaforge answers questions about the site linked to this channel.\n\
         • {} — ask anything about the site's traffic, AI crawlers or AI answers\n\
         • {} — today's headline numbers\n\
         • {} — link this channel to a site (generate the code in the dashboard: Site setup, Chat apps)\n\
         • {} — stop this channel answering for its site\n\
         • {} — send feedback to the people who run this Micaforge\n\
         New and updated investigations about the linked site are posted here too.",
        p.syntax("ask", Some("question")),
        p.syntax("stats", None),
        p.syntax("link", Some("code")),
        p.syntax("unlink", None),
        p.syntax("feedback", Some("text")),
    )
}

/// What a link code stands for while it waits to be used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkCode {
    /// The site, by UUID.
    pub site: Uuid,
    /// Its organisation.
    pub org: Uuid,
    /// Who generated the code.
    pub user: Uuid,
}

/// The Valkey key a link code lives under.
pub fn code_key(code: &str) -> String {
    format!("mf:chat:link:{}", code.trim().to_uppercase())
}

/// Answer one message. Always returns text: a failure is said in words.
pub async fn handle(st: &AppState, incoming: &Incoming) -> String {
    match respond(st, incoming).await {
        Ok(text) => text,
        Err(e) if e.is_public() => format!("That did not work: {}", public_reason(&e)),
        Err(e) => {
            tracing::warn!(platform = incoming.platform.as_str(), error = %e, "a chat command failed");
            "Something failed on the Micaforge server; the failure has been logged. Try again \
             in a minute."
                .to_string()
        }
    }
}

/// The reason a public error gives, without its variant's prefix.
fn public_reason(e: &Error) -> String {
    match e {
        Error::Validation { message, .. } => message.clone(),
        Error::NotFound(what) => format!("{what} was not found"),
        Error::Conflict(message) | Error::Forbidden(message) | Error::BadRequest(message) => {
            message.clone()
        }
        Error::RateLimited { retry_after } => format!(
            "the limit for today is reached; it resets in {} minutes",
            retry_after.div_ceil(60)
        ),
        other => other.to_string(),
    }
}

async fn respond(st: &AppState, incoming: &Incoming) -> Result<String> {
    let platform = incoming.platform.as_str();
    let link = chat::find(
        &st.pg,
        platform,
        &incoming.workspace_id,
        &incoming.channel_id,
    )
    .await?;

    match &incoming.command {
        Command::Help => Ok(help(incoming.platform)),
        Command::Link(code) => link_channel(st, incoming, code).await,
        Command::Unlink => match link {
            Some(link) => {
                chat::delete(&st.pg, link.id).await?;
                Ok("This channel no longer answers for its site.".to_string())
            }
            None => Ok("This channel is not linked to a site.".to_string()),
        },
        Command::Feedback(text) => {
            if text.trim().is_empty() {
                return Ok(format!(
                    "Write the feedback after the command: {}",
                    incoming.platform.syntax("feedback", Some("text"))
                ));
            }
            let site = match link.as_ref() {
                Some(link) => Some(sites::get(&st.pg, link.site_id).await?),
                None => None,
            };
            let reporter = Reporter {
                source: "chat",
                chat: Some(format!(
                    "{platform}:{}/{} {}",
                    incoming.workspace_id, incoming.channel_id, incoming.sender
                )),
                org_id: link.as_ref().map(|l| l.org_id),
                ..Reporter::default()
            };
            let input = feedback::Input {
                kind: "other".to_string(),
                title: title_from(text),
                body: text.trim().to_string(),
                ..feedback::Input::default()
            };
            feedback::submit(st, &reporter, &input, site.as_ref(), None).await?;
            Ok(
                "Thank you. Your feedback was sent to the people who run this Micaforge."
                    .to_string(),
            )
        }
        Command::Stats => {
            let link = linked(link, incoming.platform)?;
            let site = sites::get(&st.pg, link.site_id).await?;
            stats_today(st, &site).await
        }
        Command::Ask(question) => {
            let link = linked(link, incoming.platform)?;
            let site = sites::get(&st.pg, link.site_id).await?;
            ask(st, &site, question).await
        }
    }
}

/// The link, or the sentence that explains how to make one.
fn linked(link: Option<ChatLink>, platform: Platform) -> Result<ChatLink> {
    link.ok_or_else(|| {
        Error::Conflict(format!(
            "this channel is not linked to a site yet. In the Micaforge dashboard open Site \
             setup, Chat apps, generate a code, then type {} here",
            platform.syntax("link", Some("code"))
        ))
    })
}

/// A title for a chat feedback report: its first line, shortened.
fn title_from(text: &str) -> String {
    let first = text.trim().lines().next().unwrap_or("").trim();
    let mut title: String = first.chars().take(80).collect();
    if first.chars().count() > 80 {
        title.push('…');
    }
    if title.chars().count() < 3 {
        title = "Feedback from chat".to_string();
    }
    title
}

/// Consume a link code and link this channel to its site.
async fn link_channel(st: &AppState, incoming: &Incoming, code: &str) -> Result<String> {
    let code = code.trim();
    if code.is_empty() {
        return Ok(format!(
            "Type the code after the command: {}",
            incoming.platform.syntax("link", Some("code"))
        ));
    }
    let key = code_key(code);
    let Some(snapshot) = st.kv.get_snapshot::<LinkCode>(&key).await? else {
        return Ok(
            "That code is unknown or has expired. Generate a new one in the dashboard: Site \
             setup, Chat apps."
                .to_string(),
        );
    };
    // Consumed with a compare-and-swap: of two channels racing with the same
    // code, one is linked.
    if !st.kv.swap_json::<LinkCode>(&key, &snapshot, None).await? {
        return Ok("That code was just used. Generate a new one in the dashboard.".to_string());
    }
    let grant = snapshot.value;
    let site = sites::get(&st.pg, grant.site).await?;
    let label = if incoming.channel_label.is_empty() {
        incoming.channel_id.clone()
    } else {
        incoming.channel_label.clone()
    };
    chat::link(
        &st.pg,
        new_id(),
        grant.org,
        site.id,
        incoming.platform.as_str(),
        &incoming.workspace_id,
        &incoming.channel_id,
        &label,
        Some(grant.user),
    )
    .await?;
    Ok(format!(
        "This channel now answers for {}. Ask a question with {}, or see {}.",
        site.domain,
        incoming.platform.syntax("ask", Some("question")),
        incoming.platform.syntax("help", None)
    ))
}

/// Today's headline numbers, in the site's own calendar.
async fn stats_today(st: &AppState, site: &Site) -> Result<String> {
    let tz: chrono_tz::Tz = site.timezone.parse().unwrap_or(chrono_tz::UTC);
    let range = micaforge_core::time::resolve_range(Some("today"), None, None, tz)?;
    let people = micaforge_core::QueryEnvelope::new(site.site_id_u16(), range);
    let mut agents = people.clone();
    agents.audience = Audience::Agent;
    let mut answers = people.clone();
    answers.filters = Some(FilterGroup::and(vec![FilterNode::Leaf(Filter::new(
        Dimension::parse("channel")?,
        FilterOp::Is,
        FilterValue::One("answer_engine".to_string()),
    ))]));

    let human = stats::overview(&st.ch, &people).await?;
    let machine = micaforge_query::agents_q::overview(&st.ch, &agents).await?;
    let from_answers = stats::overview(&st.ch, &answers).await?;
    let link = format!(
        "{}/app/{}",
        st.cfg.dashboard_url.trim_end_matches('/'),
        site.site_id_u16()
    );
    Ok(format!(
        "Today on {} so far ({}): {} visitors, {} pageviews, {} visits from AI answers, {} AI \
         crawler fetches.\n{link}",
        site.domain,
        Utc::now().with_timezone(&tz).format("%H:%M %Z"),
        thousands(human.visitors),
        thousands(human.pageviews),
        thousands(from_answers.sessions),
        thousands(machine.fetches),
    ))
}

/// Ask the analyst about the linked site.
async fn ask(st: &AppState, site: &Site, question: &str) -> Result<String> {
    if st.cfg.llm.is_none() {
        return Ok(crate::routes::ai::not_configured());
    }
    let base = BaseQuery {
        range: Some("7d".to_string()),
        ..BaseQuery::default()
    };
    match crate::routes::ai::answer(st, site, base, question).await {
        Ok(answer) => {
            let reads = answer.traces.len();
            Ok(format!(
                "{}\n\n({} {} on {}. The queries behind this answer are in the dashboard's Ask AI screen when you ask there: {}/app/{}/ask)",
                answer.text,
                reads,
                if reads == 1 { "read" } else { "reads" },
                site.domain,
                st.cfg.dashboard_url.trim_end_matches('/'),
                site.site_id_u16()
            ))
        }
        Err(failed) if failed.error.is_public() => Err(failed.error),
        Err(failed) => Ok(format!(
            "The answer failed: {}",
            crate::routes::ai::failure_text(&failed.error)
        )),
    }
}

/// `12345` as `12,345`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_grammar_reads_every_platforms_text() {
        assert_eq!(parse("stats"), Command::Stats);
        assert_eq!(parse("/stats"), Command::Stats);
        assert_eq!(parse("/stats@MicaforgeBot"), Command::Stats);
        assert_eq!(parse("link K7Q2M9XP"), Command::Link("K7Q2M9XP".into()));
        assert_eq!(
            parse("/link@MicaforgeBot  K7Q2M9XP "),
            Command::Link("K7Q2M9XP".into())
        );
        assert_eq!(parse("unlink"), Command::Unlink);
        assert_eq!(
            parse("feedback the chart is empty"),
            Command::Feedback("the chart is empty".into())
        );
        assert_eq!(
            parse("ask which pages grew?"),
            Command::Ask("which pages grew?".into())
        );
        assert_eq!(parse("/ask"), Command::Help);
        assert_eq!(parse(""), Command::Help);
        assert_eq!(parse("/start"), Command::Help);
        // Anything else is a question, word for word.
        assert_eq!(
            parse("How was last week?"),
            Command::Ask("How was last week?".into())
        );
    }

    #[test]
    fn help_is_written_in_each_platforms_syntax() {
        assert!(help(Platform::Slack).contains("/micaforge link <code>"));
        assert!(help(Platform::Discord).contains("/micaforge link code:<code>"));
        assert!(help(Platform::Telegram).contains("/link <code>"));
        for p in [Platform::Slack, Platform::Discord, Platform::Telegram] {
            let text = help(p);
            for word in ["ask", "stats", "link", "unlink", "feedback"] {
                assert!(text.contains(word), "{p:?} help lacks {word}");
            }
        }
    }

    #[test]
    fn codes_are_looked_up_without_regard_to_case() {
        assert_eq!(code_key(" k7q2m9xp "), code_key("K7Q2M9XP"));
    }

    #[test]
    fn a_feedback_title_is_its_first_line() {
        assert_eq!(
            title_from("The chart is empty\nsince Monday"),
            "The chart is empty"
        );
        assert_eq!(title_from("ok"), "Feedback from chat");
        assert!(title_from(&"x".repeat(200)).ends_with('…'));
    }

    #[test]
    fn counts_are_grouped_by_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(12_345), "12,345");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
