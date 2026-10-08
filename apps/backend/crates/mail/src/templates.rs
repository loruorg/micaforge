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
//! * **One 560px card**, `<table>`-based, spacing done with cell padding.
//!   Outlook for Windows renders mail with Word, which has no flex, ignores
//!   `max-width` and honours margins only some of the time.
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
//!   mark is a table cell painted in the accent colour beside the wordmark, so
//!   even the masthead needs no image.
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
// ---------------------------------------------------------------------------

/// The brand orange, text-safe on white (5.2:1): the dot in the mark and the
/// figures a report sets in the accent. It is the colour of AI traffic on the
/// site and in the dashboard, and means the same thing here.
const ACCENT: &str = "#c2410c";
/// The orange of the mark's dot, the same value the site's logo uses.
const MARK_DOT: &str = "#f25a1d";
/// The button, and the mark's tile: the near-black the site's buttons use.
const BUTTON: &str = "#18181b";
/// Text set on [`BUTTON`].
const ON_BUTTON: &str = "#ffffff";

/// The ground around the card.
const PAGE: &str = "#f4f4f5";
/// The card itself.
const CARD: &str = "#ffffff";
/// The card's edge.
const CARD_EDGE: &str = "#e4e4e7";
/// Headings, figures and anything set in weight.
const INK: &str = "#09090b";
/// Running copy. Softer than [`INK`] so a paragraph does not glare.
const TEXT: &str = "#3f3f46";
/// Labels, notes and the footer. Still clears 4.5:1 on [`PANEL`].
const MUTE: &str = "#71717a";
/// Hairlines between rows and above the note.
const LINE: &str = "#e4e4e7";
/// The ground of the code, details and list panels.
const PANEL: &str = "#fafafa";
/// A rise, in a report. Never the only encoding: the sign is always printed.
const RISE: &str = "#1d7a52";
/// A fall, in a report.
const FALL: &str = "#b5432c";

/// [`PAGE`] in dark mode.
const PAGE_DARK: &str = "#09090b";
/// [`CARD`] in dark mode.
const CARD_DARK: &str = "#18181b";
/// [`CARD_EDGE`] in dark mode.
const CARD_EDGE_DARK: &str = "#27272a";
/// [`INK`] in dark mode.
const INK_DARK: &str = "#fafafa";
/// [`TEXT`] in dark mode.
const TEXT_DARK: &str = "#d4d4d8";
/// [`MUTE`] in dark mode.
const MUTE_DARK: &str = "#a1a1aa";
/// [`LINE`] in dark mode.
const LINE_DARK: &str = "#27272a";
/// [`PANEL`] in dark mode.
const PANEL_DARK: &str = "#202023";
/// [`ACCENT`] as text on a dark card, lifted so accent figures clear 4.5:1.
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

/// The card's width, in pixels. Narrow enough to read on a phone held
/// sideways, wide enough for a three-row figure panel.
const CARD_WIDTH: u32 = 560;

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
    let sentence = format!(
        "Enter this code on the Micaforge sign-in page. It works once and expires in {expiry}."
    );
    let note = "If you did not ask for this code, you can ignore this email. Nobody can sign in \
                without it.";

    let html = render(&Letter {
        title: &subject,
        preheader: &format!("{code} is your code. It expires in {expiry}."),
        eyebrow: "Sign in",
        heading: "Your sign-in code",
        blocks: vec![
            Block::Paragraph(escape(&sentence)),
            Block::Code(code.to_string()),
        ],
        action: None,
        note,
    });

    let text = text_page(
        "Sign in",
        &format!(
            "YOUR SIGN-IN CODE\n\n    {code}\n\n{}\n\n{}",
            wrap(&sentence),
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
                 crawlers read them. Accept to get access.";
    let no_password = "There is no password to choose. Micaforge signs you in with a code it \
                       sends to this address.";
    let note = "If you were not expecting this, ignore it.";

    let html = render(&Letter {
        title: &subject,
        preheader: &format!("Access to {org}."),
        eyebrow: "Invitation",
        heading: &format!("Join {org}"),
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
            "JOIN {}\n\n{}\n\n{}\n\n{link}\n\n{note}",
            org.to_uppercase(),
            wrap(&format!("{org} {pitch}")),
            wrap(no_password),
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
            "{} visitors, {} agent fetches.",
            fmt_int(r.visitors),
            fmt_int(r.agent_fetches)
        ),
        eyebrow: &subject,
        heading: &heading,
        blocks: vec![
            Block::Paragraph(format!(
                "{} {}",
                inked(&delta_text, delta_ink),
                escape("on the period before.")
            )),
            Block::Details(vec![
                Detail::plain("Pageviews", fmt_int(r.pageviews)),
                Detail::plain("Sessions", fmt_int(r.sessions)),
                Detail::plain("Bounce rate", fmt_pct(r.bounce_rate)),
            ]),
            Block::Subheading("AI traffic".to_string()),
            Block::Details(vec![
                Detail::plain("AI crawler visits", fmt_int(r.agent_fetches)),
                Detail::plain("Visitors from AI answers", fmt_int(r.answer_arrivals)),
                // The number the message exists to deliver: the Citation Gap.
                Detail::accent("Fetched per arrival", fmt_ratio(r.gap_ratio)),
            ]),
            Block::Ranked(Ranked::new("Pages", "visitors", &r.top_pages)),
            Block::Ranked(Ranked::new("Sources", "visitors", &r.top_sources)),
            Block::Ranked(Ranked::new("Agents", "fetches", &r.top_agents)),
        ],
        action: Some(Action {
            label: "Open the record",
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
    t.push_str(&text_row("Fetched per arrival", &fmt_ratio(r.gap_ratio)));
    t.push_str(&text_list("PAGES (visitors)", &r.top_pages));
    t.push_str(&text_list("SOURCES (visitors)", &r.top_sources));
    t.push_str(&text_list("AGENTS (fetches)", &r.top_agents));
    t.push_str(&format!("\nOpen the record: {link}\n"));
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
        eyebrow: &context,
        heading: &a.headline,
        blocks: vec![
            Block::Paragraph(escape(&a.detail)),
            // The deviation is set in the accent rather than in a rise or fall
            // colour. A spike is not automatically good and a drop is not
            // automatically bad, and colour that pretends otherwise reads as a
            // judgement the tool has not earned.
            Block::Details(vec![
                Detail::plain(&a.metric, fmt_num(a.observed)),
                Detail::plain("Expected", fmt_num(a.expected)),
                Detail::accent("Against expected", deviation.clone()),
            ]),
        ],
        action: Some(Action {
            label: "See it in the record",
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
    t.push_str(&text_row("Against expected", &deviation));
    t.push_str(&format!("\nSee it in the record: {link}\n"));
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
/// [`Block::Paragraph`]. An empty `eyebrow`, `heading` or `note` is left out.
struct Letter<'a> {
    /// The document `<title>`, which some clients show in a tab or a print.
    title: &'a str,
    /// The inbox preview line shown after the subject.
    preheader: &'a str,
    /// A short label above the heading: what kind of message this is, or
    /// which site and period it covers.
    eyebrow: &'a str,
    /// The heading.
    heading: &'a str,
    /// The body, in order.
    blocks: Vec<Block>,
    /// The one button, if the message has somewhere to send the reader.
    action: Option<Action<'a>>,
    /// Fine print under a hairline at the end of the card.
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
    /// A one-time code, set large and letter-spaced in a panel.
    Code(String),
    /// Label and value pairs in a panel.
    Details(Vec<Detail>),
    /// A heading that opens a second section of the card.
    Subheading(String),
    /// A ranked list in a panel, with its unit in the header.
    Ranked(Ranked),
}

/// The colour a value or a phrase is set in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    /// The ordinary case.
    Plain,
    /// The number the message exists to deliver; at most one per panel.
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

/// One row of a details panel.
struct Detail {
    label: String,
    value: String,
    ink: Ink,
}

impl Detail {
    /// A value in ink.
    fn plain(label: &str, value: String) -> Detail {
        Detail {
            label: label.to_string(),
            value,
            ink: Ink::Plain,
        }
    }

    /// A value in the accent.
    fn accent(label: &str, value: String) -> Detail {
        Detail {
            label: label.to_string(),
            value,
            ink: Ink::Accent,
        }
    }
}

/// A ranked list: a title, the unit of its right-hand column, and the rows.
struct Ranked {
    title: String,
    unit: String,
    rows: Vec<(String, u64)>,
}

impl Ranked {
    fn new(title: &str, unit: &str, rows: &[(String, u64)]) -> Ranked {
        Ranked {
            title: title.to_string(),
            unit: unit.to_string(),
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
  .mf-outer {{ padding:24px 12px 32px !important; }}
  .mf-card {{ padding:30px 22px !important; }}
  .mf-h {{ font-size:22px !important; line-height:28px !important; }}
  .mf-code {{ font-size:32px !important; letter-spacing:6px !important; padding-left:6px !important; }}
}}
@media (prefers-color-scheme: dark) {{
  .mf-page {{ background:{PAGE_DARK} !important; }}
  .mf-card {{ background:{CARD_DARK} !important; border-color:{CARD_EDGE_DARK} !important; }}
  .mf-ink {{ color:{INK_DARK} !important; }}
  .mf-text {{ color:{TEXT_DARK} !important; }}
  .mf-mute {{ color:{MUTE_DARK} !important; }}
  .mf-panel {{ background:{PANEL_DARK} !important; border-color:{LINE_DARK} !important; }}
  .mf-row, .mf-rule {{ border-color:{LINE_DARK} !important; }}
  .mf-accent {{ color:{ACCENT_DARK} !important; }}
  .mf-link {{ color:{INK_DARK} !important; }}
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

    if !letter.eyebrow.is_empty() {
        parts.push((
            0,
            format!(
                "<p class=\"mf-mute\" style=\"margin:0;font-family:{SANS};font-size:12px;\
                 line-height:16px;font-weight:600;letter-spacing:0.08em;text-transform:uppercase;\
                 color:{MUTE};\">{}</p>",
                escape(letter.eyebrow)
            ),
        ));
    }

    if !letter.heading.is_empty() {
        parts.push((
            10,
            format!(
                "<h1 class=\"mf-ink mf-h\" style=\"margin:0;font-family:{SANS};font-size:24px;\
                 line-height:31px;font-weight:600;letter-spacing:-0.02em;color:{INK};\">{}</h1>",
                escape(letter.heading)
            ),
        ));
    }

    for block in &letter.blocks {
        parts.push(match block {
            Block::Paragraph(html) => (14, paragraph(html)),
            Block::Code(code) => (24, code_panel(code)),
            Block::Details(rows) => (24, details_panel(rows)),
            Block::Subheading(text) => (32, subheading(text)),
            Block::Ranked(list) => (24, ranked_panel(list)),
        });
    }

    if let Some(action) = &letter.action {
        let href = safe_href(action.href);
        parts.push((28, button(&href, action.label)));
        if action.show_url && !href.is_empty() {
            parts.push((18, link_fallback(&href)));
        }
    }

    if !letter.note.is_empty() {
        parts.push((
            32,
            format!(
                "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" \
                 border=\"0\"><tr><td class=\"mf-mute mf-rule\" style=\"border-top:1px solid {LINE};\
                 padding-top:20px;font-family:{SANS};font-size:13px;line-height:20px;color:{MUTE};\">\
                 {}</td></tr></table>",
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
         <tr><td align=\"center\" class=\"mf-outer\" style=\"padding:40px 16px 48px;\">\n\
         <!--[if mso]><table role=\"presentation\" width=\"{CARD_WIDTH}\" cellpadding=\"0\" \
         cellspacing=\"0\" border=\"0\"><tr><td><![endif]-->\n\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
         style=\"max-width:{CARD_WIDTH}px;\">\n"
    ));
    h.push_str(&masthead());
    h.push_str(&format!(
        "<tr><td class=\"mf-card\" bgcolor=\"{CARD}\" style=\"background:{CARD};\
         border:1px solid {CARD_EDGE};border-radius:16px;padding:40px;\">\n\
         <table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\n\
         {body}</table>\n</td></tr>\n"
    ));
    h.push_str(&format!(
        "<tr><td class=\"mf-mute\" style=\"padding:24px 4px 0;font-family:{SANS};font-size:12px;\
         line-height:19px;color:{MUTE};\">No images and no tracking pixel. Micaforge cannot tell \
         whether this email was opened.</td></tr>\n"
    ));
    h.push_str("</table>\n<!--[if mso]></td></tr></table><![endif]-->\n");
    h.push_str("</td></tr>\n</table>\n</body>\n</html>\n");
    h
}

/// The brand mark and the wordmark, above the card.
///
/// The mark is the site logo reduced to what a table can draw: a near-black
/// tile with the orange dot in it. Drawn rather than loaded, so it is there with
/// images off and costs no request. It sits in a table of its own, because a
/// cell in the wordmark's row would stretch to the wordmark's line height and
/// stop being square.
fn masthead() -> String {
    format!(
        "<tr><td style=\"padding:0 4px 20px;\">\
         <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\
         <td valign=\"middle\" style=\"vertical-align:middle;\">\
         <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\
         <td width=\"22\" height=\"22\" bgcolor=\"{BUTTON}\" align=\"center\" valign=\"middle\" \
         style=\"width:22px;height:22px;background:{BUTTON};border-radius:6px;\">\
         <table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" align=\"center\"><tr>\
         <td width=\"8\" height=\"8\" bgcolor=\"{MARK_DOT}\" style=\"width:8px;height:8px;\
         background:{MARK_DOT};border-radius:4px;font-size:0;line-height:0;\">&nbsp;</td>\
         </tr></table></td>\
         </tr></table></td>\
         <td valign=\"middle\" class=\"mf-ink\" style=\"vertical-align:middle;padding-left:10px;\
         font-family:{SANS};font-size:16px;line-height:20px;font-weight:600;\
         letter-spacing:-0.01em;color:{INK};\">Micaforge</td>\
         </tr></table></td></tr>\n"
    )
}

/// A paragraph of running copy. `html` is already escaped.
fn paragraph(html: &str) -> String {
    format!(
        "<p class=\"mf-text\" style=\"margin:0;font-family:{SANS};font-size:15px;line-height:24px;\
         color:{TEXT};\">{html}</p>"
    )
}

/// A section heading inside the card.
fn subheading(text: &str) -> String {
    format!(
        "<h2 class=\"mf-ink\" style=\"margin:0;font-family:{SANS};font-size:17px;line-height:24px;\
         font-weight:600;letter-spacing:-0.01em;color:{INK};\">{}</h2>",
        escape(text)
    )
}

/// The code, large, monospaced and letter-spaced, alone in a panel.
///
/// The left padding equals the letter spacing, so the trailing space a client
/// adds after the last digit does not push the code off centre. `mf-nolink`
/// stops a client from turning six digits into a telephone link.
fn code_panel(code: &str) -> String {
    format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\
         <tr><td align=\"center\" class=\"mf-panel\" bgcolor=\"{PANEL}\" style=\"background:{PANEL};\
         border:1px solid {LINE};border-radius:12px;padding:22px 12px;\">\
         <span class=\"mf-ink mf-code mf-nolink\" style=\"font-family:{MONO};font-size:36px;\
         line-height:42px;font-weight:600;letter-spacing:8px;padding-left:8px;white-space:nowrap;\
         color:{INK};\">{}</span></td></tr></table>",
        escape(code)
    )
}

/// Label and value pairs in a panel, one hairline between rows.
fn details_panel(rows: &[Detail]) -> String {
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        let rule = if i + 1 < rows.len() {
            format!("border-bottom:1px solid {LINE};")
        } else {
            String::new()
        };
        let cell =
            format!("font-family:{SANS};font-size:14px;line-height:20px;padding:13px 18px;{rule}");
        let (class, colour) = row.ink.class_and_colour();
        out.push_str(&format!(
            "<tr><td class=\"mf-mute mf-row\" style=\"{cell}color:{MUTE};\">{}</td>\
             <td align=\"right\" class=\"{class} mf-row mf-nolink\" style=\"{cell}font-weight:600;\
             text-align:right;white-space:nowrap;color:{colour};\">{}</td></tr>",
            escape(&row.label),
            escape(&row.value)
        ));
    }
    panel(&out)
}

/// A ranked list in a panel: a header row naming the list and its unit, then
/// the rows, labels shortened and figures in the monospaced face so they read
/// down a column.
fn ranked_panel(list: &Ranked) -> String {
    let edge = format!("border-bottom:1px solid {LINE};");
    let mut out = format!(
        "<tr><td class=\"mf-ink mf-row\" style=\"font-family:{SANS};font-size:14px;line-height:20px;\
         font-weight:600;padding:13px 18px;{edge}color:{INK};\">{}</td>\
         <td align=\"right\" class=\"mf-mute mf-row\" style=\"font-family:{SANS};font-size:12px;\
         line-height:20px;padding:13px 18px;text-align:right;{edge}color:{MUTE};\">{}</td></tr>",
        escape(&list.title),
        escape(&list.unit)
    );

    if list.rows.is_empty() {
        // Principle 5: an empty state says empty. It never borrows last week's
        // number and it never shows a plausible-looking zero-filled row.
        out.push_str(&format!(
            "<tr><td colspan=\"2\" class=\"mf-mute\" style=\"font-family:{SANS};font-size:14px;\
             line-height:20px;padding:13px 18px;color:{MUTE};\">Nothing recorded in this period.</td></tr>"
        ));
    } else {
        for (i, (label, value)) in list.rows.iter().enumerate() {
            let rule = if i + 1 < list.rows.len() {
                edge.as_str()
            } else {
                ""
            };
            out.push_str(&format!(
                "<tr><td class=\"mf-text mf-row\" style=\"font-family:{SANS};font-size:14px;\
                 line-height:20px;padding:11px 18px;{rule}color:{TEXT};\">{}</td>\
                 <td align=\"right\" class=\"mf-ink mf-row mf-nolink\" style=\"font-family:{MONO};\
                 font-size:14px;line-height:20px;padding:11px 18px;text-align:right;white-space:nowrap;\
                 {rule}color:{INK};\">{}</td></tr>",
                escape(&truncate(label, 46)),
                fmt_int(*value)
            ));
        }
    }
    panel(&out)
}

/// The rounded panel the details and lists sit in.
fn panel(rows: &str) -> String {
    format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
         class=\"mf-panel\" bgcolor=\"{PANEL}\" style=\"background:{PANEL};border:1px solid {LINE};\
         border-radius:12px;border-collapse:separate;\">{rows}</table>"
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
         color:{MUTE};\">If the button does not open, use this address:<br>\
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
/// stable token. An eyebrow is not the place to show one.
fn humanise(slug: &str) -> String {
    slug.replace('_', " ")
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
            .contains(&format!("background:{CARD_DARK} !important")));
        // The base every client sees is the light one, inline.
        assert!(r.html.contains(&format!("background:{CARD};")));
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
    fn plain_text_lines_stay_readable() {
        let wrapped = wrap(&"word ".repeat(40));
        assert!(wrapped.lines().all(|line| line.chars().count() <= 72));
        assert_eq!(wrap("one two"), "one two");
    }
}
