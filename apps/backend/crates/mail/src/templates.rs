//! Email bodies, composed as plain Rust strings.
//!
//! There is no template engine here on purpose. An email is not a page: it is a
//! table-based document with every style that matters inlined, which is exactly
//! the kind of output a templating language makes harder to read rather than
//! easier. Four messages do not justify a rendering dependency, a template cache
//! and a class of runtime errors that only fire when a report goes out at 07:00.
//!
//! # The house rules these bodies follow
//!
//! * **One 520px column on a white page**, `<table>`-based, spacing done with
//!   cell padding. No card, no border: the brand is the mark, the blue and
//!   orange stripe under it, and the same two colours on every figure that
//!   belongs to people or to AI. Outlook for Windows renders mail with Word,
//!   which has no flex, ignores `max-width` and honours margins only some of
//!   the time.
//! * **Every style that matters is inline.** Gmail drops a whole `<style>`
//!   block for reasons it does not report, so the one block in [`head_style`]
//!   only adds what a client may ignore: the dark palette, phone padding and
//!   the data-detector resets.
//! * **Light is the base, dark is the enhancement.** Apple Mail honours
//!   `prefers-color-scheme` and gets the dark palette. Gmail and Outlook ignore
//!   the query and recolour on their own; a light base survives that, where a
//!   dark base gets inverted into a muddy grey.
//! * **Both parts, always.** Every builder returns the plain-text alternative
//!   next to the HTML, and the text part is written to be read, not to be a
//!   stripped-tag apology.
//! * **No images and no tracking pixel.** Micaforge sells the claim that it does
//!   not follow people around; an open-tracking pixel in its own mail would
//!   contradict the product in the one place a customer would notice. The brand
//!   mark, the stripe and the share bars are table cells painted in colour, so
//!   nothing in a message needs an image.
//!
//! The escape boundary is [`render`] and the fragment builders it calls: every
//! field of a [`Letter`] is plain text and is escaped there, except
//! [`Block::Paragraph`], which takes HTML built with [`escape`] and [`strong`].
//! No builder interpolates customer data raw.

use crate::{AlertPayload, ReportPayload};

// ---------------------------------------------------------------------------
// Palette. Every colour any message uses is named here and nowhere else, so a
// change of brand is a change to this block. The light set is inlined on every
// element; the dark set only reaches the clients that read the `<style>` block.
// The values are the site's own tokens (apps/web/src/styles/tokens.css).
// ---------------------------------------------------------------------------

/// People. The blue the site, the dashboard and every chart use for a person,
/// here as the first half of the brand stripe and the bars of people lists.
const BLUE: &str = "#2f6bff";
/// AI. The orange the site uses for AI traffic: the second half of the brand
/// stripe, the dot in the mark and the bars of crawler lists.
const ORANGE: &str = "#f25a1d";
/// The text-safe orange (5.2:1 on white), for the one figure a message exists
/// to deliver.
const ACCENT: &str = "#c2410c";
/// The orange of the mark's dot, the same value the site's logo uses.
const MARK_DOT: &str = ORANGE;
/// The button, and the mark's tile: the near-black the site's buttons use.
const BUTTON: &str = "#18181b";
/// Text set on [`BUTTON`].
const ON_BUTTON: &str = "#ffffff";

/// The page. White, with no card around the column: the message is the page.
const PAGE: &str = "#ffffff";
/// The ground of the code panel and the bar tracks.
const PANEL: &str = "#f4f4f5";
/// The empty part of a share bar.
const TRACK: &str = "#ececee";
/// Headings, figures and anything set in weight.
const INK: &str = "#09090b";
/// Running copy. Softer than [`INK`] so a paragraph does not glare.
const TEXT: &str = "#3f3f46";
/// Labels, notes and the footer. 4.8:1 on white.
const MUTE: &str = "#71717a";
/// Hairlines.
const LINE: &str = "#e4e4e7";
/// A rise, in a report. Never the only encoding: the sign is always printed.
const RISE: &str = "#067647";
/// A fall, in a report.
const FALL: &str = "#b42318";

/// [`PAGE`] in dark mode.
const PAGE_DARK: &str = "#0b0b0c";
/// [`PANEL`] in dark mode.
const PANEL_DARK: &str = "#18181b";
/// [`TRACK`] in dark mode.
const TRACK_DARK: &str = "#27272a";
/// [`INK`] in dark mode.
const INK_DARK: &str = "#fafafa";
/// [`TEXT`] in dark mode.
const TEXT_DARK: &str = "#d4d4d8";
/// [`MUTE`] in dark mode.
const MUTE_DARK: &str = "#a1a1aa";
/// [`LINE`] in dark mode.
const LINE_DARK: &str = "#27272a";
/// [`ACCENT`] in dark mode, lifted so it clears 4.5:1 on the dark page.
const ACCENT_DARK: &str = "#ff8a5c";
/// [`RISE`] in dark mode.
const RISE_DARK: &str = "#5fc79a";
/// [`FALL`] in dark mode.
const FALL_DARK: &str = "#f08a72";

/// UI text. The literal `system-ui` keyword is unreliable in mail clients, so the
/// stack names the faces it would have resolved to.
const SANS: &str =
    "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,'Helvetica Neue',Arial,sans-serif";
/// The code, and figures read down a column.
const MONO: &str =
    "ui-monospace,'SF Mono',SFMono-Regular,Menlo,Consolas,'Liberation Mono',monospace";

/// The column's width, in pixels: a comfortable measure for 15px copy, and wide
/// enough for a row of three figures.
const COLUMN_WIDTH: u32 = 520;

/// One rendered message: the subject line and both MIME alternatives.
///
/// The subject travels with the bodies because it is written at the same time
/// and from the same values; splitting it out is how a report ends up announcing
/// a period its body does not cover.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// The `Subject` header, as plain text.
    pub subject: String,
    /// The `text/html` alternative: a complete document.
    pub html: String,
    /// The `text/plain` alternative. Written to be read on its own.
    pub text: String,
}

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

/// The address an invitation is accepted at.
///
/// The invite flow falls back to "copy this link" whenever no mail transport is
/// configured, so this is called from outside the crate as often as in. The
/// path is the dashboard's invitation screen, which signs the reader in with a
/// code sent to the invited address and then accepts.
pub fn invitation_link(base_url: &str, token: &str) -> String {
    format!("{}/app/invitation?token={}", trim(base_url), encode(token))
}

// ---------------------------------------------------------------------------
// The four messages
// ---------------------------------------------------------------------------

/// A sign-in code.
///
/// The code is in the body, the preheader and the plain-text part, and never in
/// the subject: a subject is what a lock screen shows and what a mail log keeps.
pub fn sign_in_code(code: &str, ttl_minutes: u32) -> Rendered {
    let subject = "Your Micaforge sign-in code".to_string();
    let expiry = minutes(ttl_minutes);
    let sentence = "Enter this code on the Micaforge sign-in page.";
    let caption = format!("Expires in {expiry}. Works once.");
    let note = "If you did not ask for this code, you can ignore this email. Nobody can sign in \
                without it.";

    let html = render(&Letter {
        title: &subject,
        preheader: &format!("{code} is your code. It expires in {expiry}."),
        heading: "Your sign-in code",
        meta: "",
        blocks: vec![
            Block::Paragraph(escape(sentence)),
            Block::Code {
                code: code.to_string(),
                caption,
            },
        ],
        action: None,
        note,
    });

    let text = text_page(
        "Sign in",
        &format!(
            "YOUR SIGN-IN CODE\n\n    {code}\n\n{}\n\n{}",
            wrap(&format!(
                "{sentence} It works once and expires in {expiry}."
            )),
            wrap(note)
        ),
    );

    Rendered {
        subject,
        html,
        text,
    }
}

/// Invite somebody into an organisation.
pub fn invitation(base_url: &str, org: &str, token: &str) -> Rendered {
    let link = invitation_link(base_url, token);
    let subject = format!("You have been invited to {org} on Micaforge");
    let pitch = "measures its traffic with Micaforge: who visits its pages, and which AI \
                 crawlers read them.";
    let no_password = "There is no password to choose. Micaforge signs you in with a code it \
                       sends to this address.";
    let note = "The invitation expires in seven days. If you were not expecting it, you can \
                ignore this email.";

    let html = render(&Letter {
        title: &subject,
        preheader: &format!("Accept to see {org} in Micaforge."),
        heading: &format!("Join {org} on Micaforge"),
        meta: "",
        blocks: vec![
            Block::Paragraph(format!("{} {}", strong(org), escape(pitch))),
            Block::Paragraph(escape(no_password)),
        ],
        action: Some(Action {
            label: "Accept the invitation",
            href: &link,
            show_url: true,
        }),
        note,
    });

    let text = text_page(
        org,
        &format!(
            "JOIN {} ON MICAFORGE\n\n{}\n\n{}\n\nAccept the invitation:\n{link}\n\n{}",
            org.to_uppercase(),
            wrap(&format!("{org} {pitch}")),
            wrap(no_password),
            wrap(note),
        ),
    );

    Rendered {
        subject,
        html,
        text,
    }
}

/// A scheduled report for one site over one period.
pub fn report(base_url: &str, site: &str, period: &str, r: &ReportPayload) -> Rendered {
    let link = format!("{}/app", trim(base_url));
    let subject = format!("{site} · {period}");
    let (delta_text, delta_ink) = delta_bits(r.visitors_delta);
    let note = "This report is scheduled under Settings, Reports, on the site it covers.";

    // The lead figure is the heading. A report that opens with the word
    // "Overview" has wasted the only line anyone reads on their phone.
    let heading = format!("{} visitors", fmt_int(r.visitors));

    let html = render(&Letter {
        title: &subject,
        preheader: &format!(
            "{} visitors, {} from AI answers, {} AI crawler visits.",
            fmt_int(r.visitors),
            fmt_int(r.answer_arrivals),
            fmt_int(r.agent_fetches)
        ),
        heading: &heading,
        meta: &subject,
        blocks: vec![
            Block::Paragraph(format!(
                "{} {}",
                inked(&delta_text, delta_ink),
                escape("on the period before.")
            )),
            Block::Section {
                title: "People".to_string(),
                dot: BLUE,
            },
            Block::Stats(vec![
                Stat::plain("Pageviews", fmt_int(r.pageviews)),
                Stat::plain("Sessions", fmt_int(r.sessions)),
                Stat::plain("Bounce rate", fmt_pct(r.bounce_rate)),
            ]),
            Block::Section {
                title: "AI traffic".to_string(),
                dot: ORANGE,
            },
            Block::Stats(vec![
                Stat::plain("AI crawler visits", fmt_int(r.agent_fetches)),
                Stat::plain("From AI answers", fmt_int(r.answer_arrivals)),
                // The number the message exists to deliver: the Citation Gap.
                Stat::accent("Citation gap", fmt_ratio(r.gap_ratio)),
            ]),
            Block::Ranked(Ranked::new("Top pages", "visitors", BLUE, &r.top_pages)),
            Block::Ranked(Ranked::new("Top sources", "visitors", BLUE, &r.top_sources)),
            Block::Ranked(Ranked::new("AI crawlers", "visits", ORANGE, &r.top_agents)),
        ],
        action: Some(Action {
            label: "Open the dashboard",
            href: &link,
            show_url: false,
        }),
        note,
    });

    let mut t = String::new();
    t.push_str(&format!(
        "VISITORS  {}   {delta_text} on the period before\n\n",
        fmt_int(r.visitors)
    ));
    t.push_str(&text_row("Pageviews", &fmt_int(r.pageviews)));
    t.push_str(&text_row("Sessions", &fmt_int(r.sessions)));
    t.push_str(&text_row("Bounce rate", &fmt_pct(r.bounce_rate)));
    t.push_str("\nAI TRAFFIC\n\n");
    t.push_str(&text_row("AI crawler visits", &fmt_int(r.agent_fetches)));
    t.push_str(&text_row(
        "Visitors from AI answers",
        &fmt_int(r.answer_arrivals),
    ));
    t.push_str(&text_row("Citation gap", &fmt_ratio(r.gap_ratio)));
    t.push_str(&text_list("TOP PAGES (visitors)", &r.top_pages));
    t.push_str(&text_list("TOP SOURCES (visitors)", &r.top_sources));
    t.push_str(&text_list("AI CRAWLERS (visits)", &r.top_agents));
    t.push_str(&format!("\nOpen the dashboard: {link}\n"));
    t.push_str(&format!("\n{note}"));

    let text = text_page(&subject, &t);

    Rendered {
        subject,
        html,
        text,
    }
}

/// One fired alert.
pub fn alert(base_url: &str, site: &str, a: &AlertPayload) -> Rendered {
    // The job supplies the deep link because only the job knows the site id and
    // the window that tripped the rule. An empty or non-HTTP link degrades to the
    // site chooser rather than shipping a dead button.
    let link = if safe_href(&a.link).is_empty() {
        format!("{}/app", trim(base_url))
    } else {
        a.link.clone()
    };
    let subject = format!("{site} · {}", a.headline);
    let context = format!("{} · {site}", humanise(&a.kind));
    let note = "Alerts are configured under Settings, Alerts, on the site they watch.";

    let deviation = if a.expected.is_finite() && a.expected != 0.0 {
        delta_bits((a.observed - a.expected) / a.expected).0
    } else {
        "no baseline".to_string()
    };

    let html = render(&Letter {
        title: &subject,
        preheader: &a.detail,
        heading: &a.headline,
        meta: &context,
        blocks: vec![
            Block::Paragraph(escape(&a.detail)),
            // The deviation is set in the accent rather than in a rise or fall
            // colour. A spike is not automatically good and a drop is not
            // automatically bad, and colour that pretends otherwise reads as a
            // judgement the tool has not earned.
            Block::Stats(vec![
                Stat::plain(&humanise(&a.metric), fmt_num(a.observed)),
                Stat::plain("Expected", fmt_num(a.expected)),
                Stat::accent("Change", deviation.clone()),
            ]),
        ],
        action: Some(Action {
            label: "Open the dashboard",
            href: &link,
            show_url: false,
        }),
        note,
    });

    let mut t = String::new();
    t.push_str(&format!(
        "{}\n\n{}\n\n",
        a.headline.to_uppercase(),
        wrap(&a.detail)
    ));
    t.push_str(&text_row(&a.metric, &fmt_num(a.observed)));
    t.push_str(&text_row("Expected", &fmt_num(a.expected)));
    t.push_str(&text_row("Change", &deviation));
    t.push_str(&format!("\nOpen the dashboard: {link}\n"));
    t.push_str(&format!("\n{note}"));

    let text = text_page(&context, &t);

    Rendered {
        subject,
        html,
        text,
    }
}

// ---------------------------------------------------------------------------
// The shell
// ---------------------------------------------------------------------------

/// One message, as [`render`] lays it out, top to bottom.
///
/// Every field is plain text and is escaped by [`render`], except
/// [`Block::Paragraph`]. An empty `meta` or `note` is left out.
struct Letter<'a> {
    /// The document `<title>`, which some clients show in a tab or a print.
    title: &'a str,
    /// The inbox preview line shown after the subject.
    preheader: &'a str,
    /// The heading.
    heading: &'a str,
    /// A quiet line under the heading: which site and period a report covers,
    /// or what kind of alert fired. Never a label above the heading.
    meta: &'a str,
    /// The body, in order.
    blocks: Vec<Block>,
    /// The one button, if the message has somewhere to send the reader.
    action: Option<Action<'a>>,
    /// Fine print at the end of the message, above the footer.
    note: &'a str,
}

/// The one call to action.
struct Action<'a> {
    label: &'a str,
    /// Dropped for anything but an absolute `http`/`https` address.
    href: &'a str,
    /// Also print the address under the button, for the clients and the people
    /// that do not trust a button. An invitation that only exists inside an
    /// anchor is an invitation nobody can accept from a text-only client.
    show_url: bool,
}

/// One piece of a message body.
enum Block {
    /// Running copy. HTML, built from [`escape`] and [`strong`] only.
    Paragraph(String),
    /// A one-time code, set large and letter-spaced in a panel, with a caption.
    Code { code: String, caption: String },
    /// A row of figures, each with its label above it.
    Stats(Vec<Stat>),
    /// A heading that opens a section, led by a dot in the audience's colour.
    Section { title: String, dot: &'static str },
    /// A ranked list with a share bar under each row.
    Ranked(Ranked),
}

/// The colour a value or a phrase is set in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    /// The ordinary case.
    Plain,
    /// The number the message exists to deliver; at most one per row.
    Accent,
    /// A rise.
    Rise,
    /// A fall.
    Fall,
    /// No comparison to make.
    Mute,
}

impl Ink {
    /// The class the dark palette keys on, and the light colour inlined.
    fn class_and_colour(self) -> (&'static str, &'static str) {
        match self {
            Ink::Plain => ("mf-ink", INK),
            Ink::Accent => ("mf-accent", ACCENT),
            Ink::Rise => ("mf-rise", RISE),
            Ink::Fall => ("mf-fall", FALL),
            Ink::Mute => ("mf-mute", MUTE),
        }
    }
}

/// One figure in a [`Block::Stats`] row.
struct Stat {
    label: String,
    value: String,
    ink: Ink,
}

impl Stat {
    /// A value in ink.
    fn plain(label: &str, value: String) -> Stat {
        Stat {
            label: label.to_string(),
            value,
            ink: Ink::Plain,
        }
    }

    /// A value in the accent.
    fn accent(label: &str, value: String) -> Stat {
        Stat {
            label: label.to_string(),
            value,
            ink: Ink::Accent,
        }
    }
}

/// A ranked list: a title, the unit of its figures, the colour of its bars,
/// and the rows.
struct Ranked {
    title: String,
    unit: String,
    bar: &'static str,
    rows: Vec<(String, u64)>,
}

impl Ranked {
    fn new(title: &str, unit: &str, bar: &'static str, rows: &[(String, u64)]) -> Ranked {
        Ranked {
            title: title.to_string(),
            unit: unit.to_string(),
            bar,
            rows: rows.to_vec(),
        }
    }
}

/// The only `<style>` block. See the module docs for why it holds nothing a
/// message depends on.
fn head_style() -> String {
    format!(
        "<style type=\"text/css\">
body {{ margin:0; padding:0; -webkit-text-size-adjust:100%; -ms-text-size-adjust:100%; }}
table {{ mso-table-lspace:0pt; mso-table-rspace:0pt; }}
td {{ mso-line-height-rule:exactly; }}
a[x-apple-data-detectors], .mf-nolink a {{ color:inherit !important; text-decoration:none !important; font:inherit !important; }}
@media only screen and (max-width:600px) {{
  .mf-outer {{ padding:32px 20px 40px !important; }}
  .mf-h {{ font-size:24px !important; line-height:30px !important; }}
  .mf-code {{ font-size:34px !important; letter-spacing:8px !important; padding-left:8px !important; }}
  .mf-stat {{ font-size:18px !important; line-height:24px !important; }}
}}
@media (prefers-color-scheme: dark) {{
  .mf-page {{ background:{PAGE_DARK} !important; }}
  .mf-ink {{ color:{INK_DARK} !important; }}
  .mf-text {{ color:{TEXT_DARK} !important; }}
  .mf-mute {{ color:{MUTE_DARK} !important; }}
  .mf-panel {{ background:{PANEL_DARK} !important; }}
  .mf-track {{ background:{TRACK_DARK} !important; }}
  .mf-rule {{ border-color:{LINE_DARK} !important; }}
  .mf-accent {{ color:{ACCENT_DARK} !important; }}
  .mf-link {{ color:{INK_DARK} !important; }}
  .mf-tile {{ background:{INK_DARK} !important; }}
  .mf-btn {{ background:{INK_DARK} !important; }}
  .mf-btn a {{ color:{BUTTON} !important; }}
  .mf-rise {{ color:{RISE_DARK} !important; }}
  .mf-fall {{ color:{FALL_DARK} !important; }}
}}
</style>"
    )
}

/// Lay a [`Letter`] out as a complete HTML document.
fn render(letter: &Letter<'_>) -> String {
    // (space above, html). The first block's space is dropped.
    let mut parts: Vec<(u32, String)> = Vec::new();

    parts.push((
        0,
        format!(
            "<h1 class=\"mf-ink mf-h\" style=\"margin:0;font-family:{SANS};font-size:28px;\
             line-height:34px;font-weight:700;letter-spacing:-0.025em;color:{INK};\">{}</h1>",
            escape(letter.heading)
        ),
    ));

    if !letter.meta.is_empty() {
        parts.push((
            6,
            format!(
                "<p class=\"mf-mute\" style=\"margin:0;font-family:{SANS};font-size:14px;\
                 line-height:20px;color:{MUTE};\">{}</p>",
                escape(letter.meta)
            ),
        ));
    }

    for block in &letter.blocks {
        parts.push(match block {
            Block::Paragraph(html) => (16, paragraph(html)),
            Block::Code { code, caption } => (24, code_panel(code, caption)),
            Block::Stats(stats) => (14, stats_row(stats)),
            Block::Section { title, dot } => (32, section(title, dot)),
            Block::Ranked(list) => (32, ranked(list)),
        });
    }

    if let Some(action) = &letter.action {
        let href = safe_href(action.href);
        parts.push((32, button(&href, action.label)));
        if action.show_url && !href.is_empty() {
            parts.push((16, link_fallback(&href)));
        }
    }

    if !letter.note.is_empty() {
        parts.push((
            32,
            format!(
                "<p class=\"mf-mute\" style=\"margin:0;font-family:{SANS};font-size:13px;\
                 line-height:20px;color:{MUTE};\">{}</p>",
                escape(letter.note)
            ),
        ));
    }

    let body: String = parts
        .iter()
        .enumerate()
        .map(|(i, (space, html))| {
            let space = if i == 0 { 0 } else { *space };
            format!("<tr><td style=\"padding-top:{space}px;\">{html}</td></tr>\n")
        })
        .collect();

    // The preheader is followed by invisible filler, so a client building the
    // preview line runs out of characters before it reaches the body copy.
    let preheader = format!(
        "<div style=\"display:none;max-height:0;max-width:0;overflow:hidden;opacity:0;mso-hide:all;\
         font-size:1px;line-height:1px;color:{PAGE};\">{}{}</div>",
        escape(letter.preheader),
        "&#847;&zwnj;&nbsp;".repeat(60)
    );

    let mut h = String::with_capacity(body.len() + 8192);
    h.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    h.push_str("<meta charset=\"utf-8\">\n");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n");
    h.push_str("<meta name=\"x-apple-disable-message-reformatting\">\n");
    h.push_str(
        "<meta name=\"format-detection\" content=\"telephone=no,date=no,address=no,email=no\">\n",
    );
    // Both spellings: the second is what Apple Mail and Outlook.com read
    // before they decide whether to recolour the message themselves.
    h.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n");
    h.push_str("<meta name=\"supported-color-schemes\" content=\"light dark\">\n");
    h.push_str(&format!("<title>{}</title>\n", escape(letter.title)));
    h.push_str(&head_style());
    h.push_str("\n</head>\n");
    h.push_str(&format!(
        "<body class=\"mf-page\" style=\"margin:0;padding:0;background:{PAGE};\">\n"
    ));
    h.push_str(&preheader);
    h.push('\n');
    h.push_str(&format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
         class=\"mf-page\" bgcolor=\"{PAGE}\" style=\"background:{PAGE};\">\n\
         <tr><td align=\"center\" class=\"mf-outer\" style=\"padding:48px 24px 56px;\">\n\
         <!--[if mso]><table role=\"presentation\" width=\"{COLUMN_WIDTH}\" cellpadding=\"0\" \
         cellspacing=\"0\" border=\"0\"><tr><td><![endif]-->\n\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
         style=\"max-width:{COLUMN_WIDTH}px;\">\n"
    ));
    h.push_str(&masthead());
    h.push_str(&format!(
        "<tr><td style=\"padding-top:40px;\">\n\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\n\
         {body}</table>\n</td></tr>\n"
    ));
    h.push_str(&footer());
    h.push_str("</table>\n<!--[if mso]></td></tr></table><![endif]-->\n");
    h.push_str("</td></tr>\n</table>\n</body>\n</html>\n");
    h
}

/// The brand at the top of the column: the mark, the wordmark, and under them
/// the brand stripe, one short bar in the people blue and one in the AI orange,
/// the pair every chart in the product is drawn in.
///
/// The mark is the site logo reduced to what a table can draw: a near-black
/// tile with the orange dot in it. Drawn rather than loaded, so it is there with
/// images off and costs no request. Each square sits in a table of its own,
/// because a cell in the wordmark's row would stretch to the wordmark's line
/// height and stop being square.
fn masthead() -> String {
    format!(
        "<tr><td>\
         <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\
         <td valign=\"middle\" style=\"vertical-align:middle;\">\
         <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\
         <td class=\"mf-tile\" width=\"26\" height=\"26\" bgcolor=\"{BUTTON}\" align=\"center\" \
         valign=\"middle\" style=\"width:26px;height:26px;background:{BUTTON};border-radius:7px;\">\
         <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" align=\"center\"><tr>\
         <td width=\"9\" height=\"9\" bgcolor=\"{MARK_DOT}\" style=\"width:9px;height:9px;\
         background:{MARK_DOT};border-radius:5px;font-size:0;line-height:0;\">&nbsp;</td>\
         </tr></table></td>\
         </tr></table></td>\
         <td valign=\"middle\" class=\"mf-ink\" style=\"vertical-align:middle;padding-left:10px;\
         font-family:{SANS};font-size:17px;line-height:22px;font-weight:700;\
         letter-spacing:-0.02em;color:{INK};\">Micaforge</td>\
         </tr></table>\
         <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
         style=\"margin-top:20px;\"><tr>\
         <td width=\"28\" height=\"3\" bgcolor=\"{BLUE}\" style=\"width:28px;height:3px;\
         background:{BLUE};border-radius:2px 0 0 2px;font-size:0;line-height:0;\">&nbsp;</td>\
         <td width=\"14\" height=\"3\" bgcolor=\"{ORANGE}\" style=\"width:14px;height:3px;\
         background:{ORANGE};border-radius:0 2px 2px 0;font-size:0;line-height:0;\">&nbsp;</td>\
         </tr></table>\
         </td></tr>\n"
    )
}

/// The footer: a hairline, what Micaforge is, and the privacy statement the
/// product makes about its own mail.
fn footer() -> String {
    format!(
        "<tr><td style=\"padding-top:48px;\">\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\
         <tr><td class=\"mf-mute mf-rule\" style=\"border-top:1px solid {LINE};padding-top:20px;\
         font-family:{SANS};font-size:12px;line-height:19px;color:{MUTE};\">\
         <strong class=\"mf-ink\" style=\"font-weight:600;color:{INK};\">Micaforge</strong> \
         &nbsp;·&nbsp; Open-source, cookie-free web analytics<br>\
         No images and no tracking pixel. Micaforge cannot tell whether this email was opened.\
         </td></tr></table></td></tr>\n"
    )
}

/// A paragraph of running copy. `html` is already escaped.
fn paragraph(html: &str) -> String {
    format!(
        "<p class=\"mf-text\" style=\"margin:0;font-family:{SANS};font-size:16px;line-height:26px;\
         color:{TEXT};\">{html}</p>"
    )
}

/// A section heading, led by a small dot in the colour of the audience it is
/// about: blue for people, orange for AI.
fn section(title: &str, dot: &str) -> String {
    format!(
        "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\
         <td valign=\"middle\" style=\"vertical-align:middle;\">\
         <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\
         <td width=\"8\" height=\"8\" bgcolor=\"{dot}\" style=\"width:8px;height:8px;\
         background:{dot};border-radius:2px;font-size:0;line-height:0;\">&nbsp;</td>\
         </tr></table></td>\
         <td valign=\"middle\" class=\"mf-ink\" style=\"vertical-align:middle;padding-left:8px;\
         font-family:{SANS};font-size:15px;line-height:22px;font-weight:600;color:{INK};\">{}</td>\
         </tr></table>",
        escape(title)
    )
}

/// The code, large, monospaced and letter-spaced, in a soft panel, with its
/// caption under it.
///
/// The left padding equals the letter spacing, so the trailing space a client
/// adds after the last digit does not push the code off centre. `mf-nolink`
/// stops a client from turning six digits into a telephone link.
fn code_panel(code: &str, caption: &str) -> String {
    format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\
         <tr><td align=\"center\" class=\"mf-panel\" bgcolor=\"{PANEL}\" style=\"background:{PANEL};\
         border-radius:14px;padding:28px 12px 22px;\">\
         <span class=\"mf-ink mf-code mf-nolink\" style=\"font-family:{MONO};font-size:40px;\
         line-height:46px;font-weight:700;letter-spacing:10px;padding-left:10px;white-space:nowrap;\
         color:{INK};\">{}</span>\
         <p class=\"mf-mute\" style=\"margin:12px 0 0;font-family:{SANS};font-size:13px;\
         line-height:18px;color:{MUTE};\">{}</p></td></tr></table>",
        escape(code),
        escape(caption)
    )
}

/// A row of figures: label above, value below, in up to three columns. No
/// boxes and no borders; the space between columns is the separation.
fn stats_row(stats: &[Stat]) -> String {
    let width = 100 / stats.len().max(1);
    let cells: String = stats
        .iter()
        .map(|s| {
            let (class, colour) = s.ink.class_and_colour();
            format!(
                "<td width=\"{width}%\" valign=\"top\" style=\"width:{width}%;vertical-align:top;\
                 padding-right:12px;\">\
                 <p class=\"mf-mute\" style=\"margin:0;font-family:{SANS};font-size:13px;\
                 line-height:18px;color:{MUTE};\">{}</p>\
                 <p class=\"{class} mf-stat mf-nolink\" style=\"margin:4px 0 0;font-family:{SANS};\
                 font-size:22px;line-height:28px;font-weight:700;letter-spacing:-0.02em;\
                 white-space:nowrap;color:{colour};\">{}</p></td>",
                escape(&s.label),
                escape(&s.value)
            )
        })
        .collect();
    format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\
         <tr>{cells}</tr></table>"
    )
}

/// A ranked list: its title and unit, then each row as a label and a figure
/// with a thin share bar under them, drawn in the audience's colour against a
/// grey track. The bars are table cells, so they render with images off and in
/// every client that renders a table.
fn ranked(list: &Ranked) -> String {
    let mut out = format!(
        "<tr><td class=\"mf-ink\" style=\"font-family:{SANS};font-size:15px;line-height:22px;\
         font-weight:600;padding-bottom:4px;color:{INK};\">{}</td>\
         <td align=\"right\" class=\"mf-mute\" style=\"font-family:{SANS};font-size:13px;\
         line-height:22px;text-align:right;padding-bottom:4px;color:{MUTE};\">{}</td></tr>",
        escape(&list.title),
        escape(&list.unit)
    );

    if list.rows.is_empty() {
        // Principle 5: an empty state says empty. It never borrows last week's
        // number and it never shows a plausible-looking zero-filled row.
        out.push_str(&format!(
            "<tr><td colspan=\"2\" class=\"mf-mute\" style=\"font-family:{SANS};font-size:14px;\
             line-height:20px;padding-top:10px;color:{MUTE};\">Nothing recorded in this period.</td></tr>"
        ));
    } else {
        let peak = list.rows.iter().map(|(_, v)| *v).max().unwrap_or(0).max(1);
        for (label, value) in &list.rows {
            // At least one percent, so a row that counted anything shows a mark.
            let share = ((*value as f64 / peak as f64) * 100.0)
                .round()
                .clamp(1.0, 100.0) as u32;
            let rest = 100 - share;
            let track = if rest > 0 {
                format!(
                    "<td class=\"mf-track\" width=\"{rest}%\" height=\"4\" bgcolor=\"{TRACK}\" \
                     style=\"width:{rest}%;height:4px;background:{TRACK};font-size:0;line-height:0;\">&nbsp;</td>"
                )
            } else {
                String::new()
            };
            out.push_str(&format!(
                "<tr><td class=\"mf-text\" style=\"font-family:{SANS};font-size:14px;line-height:20px;\
                 padding-top:12px;color:{TEXT};\">{}</td>\
                 <td align=\"right\" class=\"mf-ink mf-nolink\" style=\"font-family:{MONO};font-size:14px;\
                 line-height:20px;padding-top:12px;text-align:right;white-space:nowrap;color:{INK};\">{}</td></tr>\
                 <tr><td colspan=\"2\" style=\"padding-top:6px;\">\
                 <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
                 style=\"border-radius:2px;\"><tr>\
                 <td width=\"{share}%\" height=\"4\" bgcolor=\"{bar}\" style=\"width:{share}%;height:4px;\
                 background:{bar};font-size:0;line-height:0;\">&nbsp;</td>{track}</tr></table></td></tr>",
                escape(&truncate(label, 46)),
                fmt_int(*value),
                bar = list.bar,
            ));
        }
    }
    format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\
         {out}</table>"
    )
}

/// The one call to action: a near-black block with white type on it, the way
/// the site's own buttons look.
///
/// No gradient and no shadow. A mail client will not render a shadow honestly,
/// so the button does not ask for one. An unsafe `href` arrives here empty and
/// the button is left out rather than shipped dead.
fn button(href: &str, label: &str) -> String {
    if href.is_empty() {
        return String::new();
    }
    format!(
        "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\
         <td class=\"mf-btn\" bgcolor=\"{BUTTON}\" style=\"background:{BUTTON};border-radius:8px;\">\
         <a href=\"{}\" target=\"_blank\" style=\"display:inline-block;padding:13px 22px;\
         font-family:{SANS};font-size:15px;line-height:20px;font-weight:600;color:{ON_BUTTON};\
         text-decoration:none;border-radius:8px;\">{}</a></td></tr></table>",
        escape(href),
        escape(label)
    )
}

/// The address written out under the button.
fn link_fallback(href: &str) -> String {
    format!(
        "<p class=\"mf-mute\" style=\"margin:0;font-family:{SANS};font-size:13px;line-height:20px;\
         color:{MUTE};\">Or open this address:<br>\
         <a href=\"{0}\" target=\"_blank\" class=\"mf-link\" style=\"color:{INK};\
         text-decoration:underline;word-break:break-all;\">{0}</a></p>",
        escape(href)
    )
}

/// An escaped value set in weight and in ink, for use inside a paragraph.
fn strong(text: &str) -> String {
    format!(
        "<strong class=\"mf-ink\" style=\"font-weight:600;color:{INK};\">{}</strong>",
        escape(text)
    )
}

/// An escaped phrase in one of the inks, for use inside a paragraph.
fn inked(text: &str, ink: Ink) -> String {
    let (class, colour) = ink.class_and_colour();
    format!(
        "<span class=\"{class}\" style=\"font-weight:600;color:{colour};\">{}</span>",
        escape(text)
    )
}

/// The plain-text shell: the same masthead and the same closing statement, so
/// the two alternatives say the same thing to anyone comparing them.
fn text_page(context: &str, body: &str) -> String {
    let mut t = String::with_capacity(body.len() + 256);
    t.push_str("MICAFORGE");
    if !context.is_empty() {
        t.push_str("  ·  ");
        t.push_str(context);
    }
    t.push_str("\n----------------------------------------\n\n");
    t.push_str(body);
    t.push_str("\n\n----------------------------------------\n");
    t.push_str("No images and no tracking pixel. Micaforge cannot tell whether\n");
    t.push_str("this email was opened.\n");
    t
}

/// Break a sentence into lines of at most 72 characters, on spaces, for the
/// plain-text part. A word longer than a line is left whole.
fn wrap(text: &str) -> String {
    const WIDTH: usize = 72;
    let mut out = String::with_capacity(text.len() + 8);
    let mut line = 0usize;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        if line > 0 && line + 1 + len > WIDTH {
            out.push('\n');
            line = 0;
        } else if line > 0 {
            out.push(' ');
            line += 1;
        }
        out.push_str(word);
        line += len;
    }
    out
}

/// One label-and-value line in the plain-text part, padded so the values form a
/// column when the client uses a monospaced face and stay readable when it does
/// not.
fn text_row(label: &str, value: &str) -> String {
    format!("  {:<26}{}\n", truncate(label, 25), value)
}

/// A ranked list in the plain-text part.
fn text_list(title: &str, rows: &[(String, u64)]) -> String {
    let mut t = format!("\n{title}\n\n");
    if rows.is_empty() {
        t.push_str("  Nothing recorded in this period.\n");
        return t;
    }
    for (label, value) in rows {
        t.push_str(&format!(
            "  {:<44}{}\n",
            truncate(label, 43),
            fmt_int(*value)
        ));
    }
    t
}

// ---------------------------------------------------------------------------
// Formatting and safety
// ---------------------------------------------------------------------------

/// Escape text for use in HTML element content or in a quoted attribute.
///
/// Site names, page paths, agent names and alert copy all reach these templates
/// from customer data or from a crawled page title, so nothing is interpolated
/// without passing through here first.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Percent-encode one URL query component.
///
/// Tokens are URL-safe base64 today, so in practice this changes nothing. It
/// exists so that a future token alphabet cannot quietly break every link in an
/// invitation email.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Return the href if it is an absolute `http`/`https` address, otherwise an
/// empty string.
///
/// Alert links arrive from a job payload that an operator can edit. Anything
/// that is not plainly a web address is dropped rather than rendered, because a
/// `javascript:` or `data:` href in an email is a phishing primitive with our
/// name on the envelope.
fn safe_href(href: &str) -> String {
    let trimmed = href.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        trimmed.to_string()
    } else {
        String::new()
    }
}

/// Turn a database slug into something a person reads.
///
/// `alerts.kind` is stored as `agent_surge` because Postgres and the API want a
/// stable token. A heading or a label is not the place to show one, so this
/// spaces it out and starts it with a capital.
fn humanise(slug: &str) -> String {
    let spaced = slug.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
    }
}

/// Drop a trailing slash so a joined path never doubles it.
fn trim(base_url: &str) -> &str {
    base_url.trim_end_matches('/')
}

/// `10 minutes`, `1 minute`.
fn minutes(n: u32) -> String {
    if n == 1 {
        "1 minute".to_string()
    } else {
        format!("{n} minutes")
    }
}

/// Shorten a label to `max` characters, on a character boundary.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

/// Group an integer in threes.
fn fmt_int(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Render a rate held as a fraction. `0.41` becomes `41%`.
fn fmt_pct(v: f64) -> String {
    if !v.is_finite() {
        return "\u{2013}".to_string();
    }
    format!("{:.0}%", v * 100.0)
}

/// Render a free-floating number: whole values keep their grouping, fractional
/// ones keep enough precision to be worth printing.
fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return "\u{2013}".to_string();
    }
    let magnitude = v.abs();
    if magnitude.fract() == 0.0 && magnitude < 1e15 {
        let sign = if v < 0.0 { "-" } else { "" };
        format!("{sign}{}", fmt_int(magnitude as u64))
    } else if magnitude < 10.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.1}")
    }
}

/// Render the Citation Gap as what it is: fetches per arrival.
///
/// A site whose pages are crawled and never cited has no finite ratio, and
/// saying so plainly is more useful than printing an infinity glyph.
fn fmt_ratio(v: f64) -> String {
    if !v.is_finite() || v <= 0.0 {
        return "none returned".to_string();
    }
    if v >= 100.0 {
        format!("{} : 1", fmt_int(v.round() as u64))
    } else {
        format!("{v:.1} : 1")
    }
}

/// Render a signed change held as a fraction, with the ink it is set in.
///
/// The sign is always present, so the direction survives for a reader who cannot
/// separate the rise colour from the fall colour. Colour is never the only
/// encoding.
fn delta_bits(v: f64) -> (String, Ink) {
    if !v.is_finite() {
        return ("no comparison".to_string(), Ink::Mute);
    }
    let pct = v * 100.0;
    if pct.abs() < 0.05 {
        return ("no change".to_string(), Ink::Mute);
    }
    let ink = if pct > 0.0 { Ink::Rise } else { Ink::Fall };
    (format!("{pct:+.1}%"), ink)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_report() -> ReportPayload {
        ReportPayload {
            visitors: 12_481,
            visitors_delta: 0.124,
            pageviews: 38_904,
            sessions: 15_220,
            bounce_rate: 0.41,
            agent_fetches: 91_004,
            answer_arrivals: 320,
            gap_ratio: 284.4,
            top_pages: vec![("/docs/quickstart".to_string(), 4210)],
            top_sources: vec![("chatgpt.com".to_string(), 190)],
            top_agents: vec![("gptbot".to_string(), 40_112)],
        }
    }

    fn sample_alert(link: &str) -> AlertPayload {
        AlertPayload {
            kind: "agent_surge".to_string(),
            headline: "GPTBot tripled".to_string(),
            detail: "detail".to_string(),
            metric: "fetches".to_string(),
            observed: 3000.0,
            expected: 1000.0,
            link: link.to_string(),
        }
    }

    #[test]
    fn numbers_read_the_way_a_person_writes_them() {
        assert_eq!(fmt_int(0), "0");
        assert_eq!(fmt_int(999), "999");
        assert_eq!(fmt_int(1_000), "1,000");
        assert_eq!(fmt_int(12_481), "12,481");
        assert_eq!(fmt_int(1_000_000), "1,000,000");
        assert_eq!(fmt_pct(0.41), "41%");
        assert_eq!(fmt_num(12.0), "12");
        assert_eq!(fmt_num(-3.0), "-3");
        assert_eq!(fmt_num(4.567), "4.57");
        assert_eq!(fmt_ratio(284.4), "284 : 1");
        assert_eq!(fmt_ratio(2.5), "2.5 : 1");
        assert_eq!(fmt_ratio(f64::INFINITY), "none returned");
        assert_eq!(minutes(10), "10 minutes");
        assert_eq!(minutes(1), "1 minute");
    }

    #[test]
    fn deltas_carry_their_sign() {
        assert_eq!(delta_bits(0.124), ("+12.4%".to_string(), Ink::Rise));
        assert_eq!(delta_bits(-0.031), ("-3.1%".to_string(), Ink::Fall));
        assert_eq!(delta_bits(0.0).0, "no change");
    }

    #[test]
    fn the_sign_in_email_carries_the_code_everywhere_but_the_subject() {
        let r = sign_in_code("048213", 10);
        assert_eq!(r.subject, "Your Micaforge sign-in code");
        assert!(
            !r.subject.contains("048213"),
            "a subject is what a lock screen shows"
        );
        assert!(r
            .html
            .contains("048213 is your code. It expires in 10 minutes."));
        assert!(r.html.contains("Your sign-in code"));
        assert!(r.html.contains(">048213</span>"), "the code panel");
        assert!(r.html.contains(
            "If you did not ask for this code, you can ignore this email. Nobody can sign in \
             without it."
        ));
        // The leading zero survives, and the text part carries the code on a
        // line of its own where it is easy to copy.
        assert!(r.text.contains("\n    048213\n"));
        let words = r.text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("expires in 10 minutes"));
        // A code email has nowhere to send the reader, so it has no button.
        assert!(!r.html.contains("<a href"));
    }

    #[test]
    fn the_brand_mark_is_drawn_not_loaded() {
        let r = sign_in_code("123456", 10);
        assert!(r.html.contains(&format!("bgcolor=\"{BUTTON}\"")));
        assert!(r.html.contains(&format!("bgcolor=\"{MARK_DOT}\"")));
        assert!(r.html.contains(">Micaforge</td>"));
        assert!(!r.html.contains("<img"));
        assert!(!r.html.contains("url("), "no background image either");
    }

    #[test]
    fn the_dark_palette_is_an_enhancement_on_a_light_base() {
        let r = sign_in_code("123456", 10);
        assert!(r.html.contains("prefers-color-scheme: dark"));
        assert!(r
            .html
            .contains(&format!("background:{PAGE_DARK} !important")));
        // The base every client sees is the light one, inline.
        assert!(r.html.contains(&format!("background:{PAGE};")));
        assert!(r.html.contains("content=\"light dark\""));
    }

    #[test]
    fn customer_text_cannot_open_a_tag() {
        let r = alert(
            "https://mf.example.com",
            "<script>x</script>",
            &AlertPayload {
                kind: "spike".to_string(),
                headline: "\"quoted\" & <b>bold</b>".to_string(),
                detail: "detail".to_string(),
                metric: "visitors".to_string(),
                observed: 400.0,
                expected: 100.0,
                link: "https://mf.example.com/app/1".to_string(),
            },
        );
        assert!(!r.html.contains("<script>"));
        assert!(r.html.contains("&lt;script&gt;"));
        assert!(r.html.contains("&quot;quoted&quot;"));

        let invite = invitation("https://mf.example.com", "<i>Reeds</i> & Co", "tok");
        assert!(!invite.html.contains("<i>Reeds</i>"));
        assert!(invite.html.contains("&lt;i&gt;Reeds&lt;/i&gt; &amp; Co"));
    }

    #[test]
    fn a_hostile_alert_link_is_dropped_not_rendered() {
        assert_eq!(safe_href("javascript:alert(1)"), "");
        assert_eq!(safe_href("  https://ok.example/x "), "https://ok.example/x");
        let r = alert(
            "https://mf.example.com",
            "site",
            &sample_alert("javascript:alert(1)"),
        );
        assert!(!r.html.contains("javascript:"));
        assert!(r.html.contains("https://mf.example.com/app"));
    }

    #[test]
    fn an_invitation_links_to_the_screen_that_accepts_it() {
        let r = invitation("https://mf.example.com/", "Reeds", "a b/c");
        let link = "https://mf.example.com/app/invitation?token=a%20b%2Fc";
        assert_eq!(invitation_link("https://mf.example.com/", "a b/c"), link);
        assert!(
            r.html.contains(link),
            "the button and the written-out address"
        );
        assert!(r.text.contains(link));
        assert!(r.text.contains("no password"));
    }

    #[test]
    fn every_message_ships_both_parts_and_no_image() {
        let base = "https://mf.example.com/";
        let all = vec![
            sign_in_code("123456", 10),
            invitation(base, "Reeds", "tok"),
            report(base, "reeds.dev", "last 7 days", &sample_report()),
            alert(base, "reeds.dev", &sample_alert("")),
        ];
        for r in all {
            assert!(!r.subject.is_empty());
            assert!(r.html.starts_with("<!doctype html>"));
            assert!(r.text.contains("MICAFORGE"));
            assert!(!r.html.contains("<img"), "an email must carry no images");
            assert!(
                !r.html.contains("//mf.example.com//"),
                "doubled slash in a link"
            );
            assert!(r.html.contains("color-scheme"));
            assert!(r.html.contains("No images and no tracking pixel."));
            assert!(r.text.contains("No images and no tracking pixel."));
        }
    }

    #[test]
    fn an_empty_list_says_empty() {
        let mut payload = sample_report();
        payload.top_agents.clear();
        let r = report("https://mf.example.com", "reeds.dev", "yesterday", &payload);
        assert!(r.html.contains("Nothing recorded in this period."));
        assert!(r.text.contains("Nothing recorded in this period."));
    }

    #[test]
    fn a_long_path_is_shortened_on_a_character_boundary() {
        let long = "/docs/".to_string() + &"é".repeat(80);
        let out = truncate(&long, 20);
        assert_eq!(out.chars().count(), 20);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn a_slug_reads_as_a_sentence() {
        assert_eq!(humanise("agent_surge"), "Agent surge");
        assert_eq!(humanise("fetches, 6h"), "Fetches, 6h");
        assert_eq!(humanise(""), "");
    }

    #[test]
    fn plain_text_lines_stay_readable() {
        let wrapped = wrap(&"word ".repeat(40));
        assert!(wrapped.lines().all(|line| line.chars().count() <= 72));
        assert_eq!(wrap("one two"), "one two");
    }
}
