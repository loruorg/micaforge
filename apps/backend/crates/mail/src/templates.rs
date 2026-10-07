//! Email bodies, composed as plain Rust strings.
//!
//! There is no template engine here on purpose. An email is not a page: it is a
//! table-based document with every style inlined, which is exactly the kind of
//! output a templating language makes harder to read rather than easier. Five
//! messages do not justify a rendering dependency, a template cache and a class
//! of runtime errors that only fire when a report goes out at 07:00.
//!
//! # The house rules these bodies follow
//!
//! * **One 600px panel**, `<table>`-based, every rule inlined. Layout survives
//!   Outlook's Word renderer and Gmail's stylesheet stripping.
//! * **Dark by declaration.** The surface is the product's warm soot, and the
//!   document declares `color-scheme: dark` so the clients that force-invert a
//!   light email leave this one alone.
//! * **Both parts, always.** Every builder returns the plain-text alternative
//!   next to the HTML, and the text part is written to be read, not to be a
//!   stripped-tag apology.
//! * **No images and no tracking pixel.** Micaforge sells the claim that it does
//!   not follow people around; an open-tracking pixel in its own mail would
//!   contradict the product in the one place a customer would notice. That also
//!   means the layout can never depend on a spacer GIF.
//!
//! The visual language is the one in `docs/CONTRACT.md` §6: a smoked drum record.
//! The panel carries the instrument's pin-feed perforation down a left gutter,
//! and every divider is a *scratched trace* rather than a hairline: one bright
//! line with the displaced soot burred along one side, offset to the right so the
//! pair reads as a gouge and not as a border.

use crate::{AlertPayload, ReportPayload};

// ---------------------------------------------------------------------------
// Palette. `docs/CONTRACT.md` §6 is the source; these are the same values the
// dashboard's tokens carry, restated because an email cannot read a CSS file.
// ---------------------------------------------------------------------------

/// Warm soot. The ground, never a cool blue-charcoal.
const SOOT: &str = "#13110E";
/// The record panel, a hair lifted off the ground so it reads as a sheet.
const PANEL: &str = "#17140F";
/// The pin-feed gutter, sunk below the panel.
const GUTTER: &str = "#100E0B";
/// A punched sprocket hole.
const HOLE: &str = "#070605";
/// The panel's own edge, self-coloured rather than a contrasting outline.
const EDGE: &str = "#2A251C";
/// Full-strength ink, for figures and headings.
const INK: &str = "#E9E3D6";
/// Running copy. Softer than `INK` so a paragraph does not glare.
const INK_BODY: &str = "#CFC8BA";
/// Captions and secondary labels.
const INK_DIM: &str = "#A79E8D";
/// Colophon and footnotes. Still clears 4.5:1 on `PANEL`.
const INK_FAINT: &str = "#8C8474";
/// Shellac, the one accent: the fixative varnish on the drum.
const SHELLAC: &str = "#C98A46";
/// Shellac lifted for link text, which sits on a darker field than a button.
const SHELLAC_LIT: &str = "#D9A465";
/// Verdigris, the first annotation ink. Used for a rise.
const VERDIGRIS: &str = "#5F9C82";
/// Oxide red, the second annotation ink. Used for a fall.
const OXIDE: &str = "#C9694B";
/// The scratch itself: light dragged through soot.
const TRACE: &str = "#5C5346";
/// The burr of displaced soot along the scratch.
const BURR: &str = "#070604";

/// Display face. `Trench Slab` will not load in mail, so the stack falls through
/// old-style serifs with a similar colour on the page before reaching Georgia.
const DISPLAY: &str =
    "'Trench Slab','Iowan Old Style','Palatino Linotype',Palatino,Georgia,'Times New Roman',serif";
/// UI text. The literal `system-ui` keyword is unreliable in mail clients, so the
/// stack names the same faces it would have resolved to.
const SANS: &str =
    "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,'Helvetica Neue',Arial,sans-serif";
/// Figures in a column. Genuine data only, per the contract: never the house voice.
const TABULAR: &str =
    "'Tabular',ui-monospace,'SFMono-Regular',Menlo,Consolas,'Liberation Mono',monospace";

/// The only `<style>` block. Held as a constant because CSS braces and
/// `format!` braces do not coexist without noise.
const HEAD_STYLE: &str = r#"<style type="text/css">
:root { color-scheme: dark; supported-color-schemes: dark; }
body { margin:0 !important; padding:0 !important; width:100% !important; background-color:#13110E; }
table { border-collapse:collapse; mso-table-lspace:0pt; mso-table-rspace:0pt; }
td { mso-line-height-rule:exactly; }
a { color:#D9A465; }
@media only screen and (max-width:620px) {
  .mf-panel { width:100% !important; }
  .mf-pad { padding-left:20px !important; padding-right:20px !important; }
  .mf-gutter { width:18px !important; }
  .mf-lead { font-size:42px !important; }
  .mf-fig { font-size:21px !important; }
}
</style>"#;

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

/// The address a new account confirms itself at.
///
/// Public because the signup handler shows the same link on screen when SMTP is
/// not configured, and two spellings of one route is a support ticket.
pub fn verification_link(base_url: &str, token: &str) -> String {
    format!(
        "{}/app/verify-email?token={}",
        trim(base_url),
        encode(token)
    )
}

/// The address a password reset is completed at.
pub fn password_reset_link(base_url: &str, token: &str) -> String {
    format!(
        "{}/app/reset-password?token={}",
        trim(base_url),
        encode(token)
    )
}

/// The address an invitation is accepted at.
///
/// The invite flow falls back to "copy this link" whenever the mailer is in
/// logging-only mode, so this is called from outside the crate as often as in.
pub fn invitation_link(base_url: &str, token: &str) -> String {
    format!("{}/app/invite?token={}", trim(base_url), encode(token))
}

// ---------------------------------------------------------------------------
// The five messages
// ---------------------------------------------------------------------------

/// Confirm a newly registered address.
pub fn verification(base_url: &str, token: &str) -> Rendered {
    let link = verification_link(base_url, token);
    let subject = "Confirm your Micaforge address".to_string();

    let mut body = String::new();
    body.push_str(&heading("Confirm this address"));
    body.push_str(&spacer(12));
    body.push_str(&para(
        "Your Micaforge account is made. It stays idle until this address is confirmed.",
    ));
    body.push_str(&spacer(24));
    body.push_str(&button(&link, "Confirm address"));
    body.push_str(&spacer(20));
    body.push_str(&link_fallback(&link));

    let html = layout(
        &subject,
        "One link, and the account is live.",
        "",
        &body,
        "If you did not start this, ignore it. Nothing happens until the link is opened.",
    );

    let text = text_page(
        "",
        &format!(
            "CONFIRM THIS ADDRESS\n\n\
             Your Micaforge account is made. It stays idle until this address\n\
             is confirmed.\n\n\
             {link}\n\n\
             If you did not start this, ignore it. Nothing happens until the\n\
             link is opened."
        ),
    );

    Rendered {
        subject,
        html,
        text,
    }
}

/// Start a password reset.
pub fn password_reset(base_url: &str, token: &str) -> Rendered {
    let link = password_reset_link(base_url, token);
    let subject = "Reset your Micaforge password".to_string();

    let mut body = String::new();
    body.push_str(&heading("Set a new password"));
    body.push_str(&spacer(12));
    body.push_str(&para(
        "Choose a new password with the link below. The current one keeps working \
         until you do.",
    ));
    body.push_str(&spacer(24));
    body.push_str(&button(&link, "Choose a new password"));
    body.push_str(&spacer(20));
    body.push_str(&link_fallback(&link));

    let html = layout(
        &subject,
        "A link to set a new password.",
        "",
        &body,
        "If you did not ask for this, ignore it. Your password has not changed.",
    );

    let text = text_page(
        "",
        &format!(
            "SET A NEW PASSWORD\n\n\
             Choose a new password with the link below. The current one keeps\n\
             working until you do.\n\n\
             {link}\n\n\
             If you did not ask for this, ignore it. Your password has not\n\
             changed."
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

    let mut body = String::new();
    body.push_str(&heading(&format!("Join {org}")));
    body.push_str(&spacer(12));
    body.push_str(&para(&format!(
        "{org} measures its traffic with Micaforge: who reads its pages, and \
         which machines fetch them. Accept to get access.",
    )));
    body.push_str(&spacer(24));
    body.push_str(&button(&link, "Accept the invitation"));
    body.push_str(&spacer(20));
    body.push_str(&link_fallback(&link));

    let html = layout(
        &subject,
        &format!("Access to {org}."),
        org,
        &body,
        "If you were not expecting this, ignore it.",
    );

    let text = text_page(
        org,
        &format!(
            "JOIN {org}\n\n\
             {org} measures its traffic with Micaforge: who reads its pages,\n\
             and which machines fetch them. Accept to get access.\n\n\
             {link}\n\n\
             If you were not expecting this, ignore it."
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
    let (delta_text, delta_colour) = delta_bits(r.visitors_delta);

    let mut body = String::new();

    // The lead figure is the heading. A report that opens with the word
    // "Overview" has wasted the only line anyone reads on their phone.
    body.push_str(&format!(
        "<div class=\"mf-lead\" style=\"font-family:{DISPLAY};font-size:54px;line-height:1.04;\
         letter-spacing:0.01em;color:{INK};\">{}</div>",
        fmt_int(r.visitors)
    ));
    body.push_str(&spacer(10));
    body.push_str(&format!(
        "<div style=\"font-family:{SANS};font-size:13px;line-height:1.5;color:{INK_DIM};\">\
         visitors <span style=\"color:{delta_colour};\">{}</span> \
         <span style=\"color:{INK_FAINT};\">on the period before</span></div>",
        escape(&delta_text)
    ));

    body.push_str(&spacer(26));
    body.push_str(&trace_rule());
    body.push_str(&spacer(22));

    body.push_str(&figure_row(&[
        Figure::ink(fmt_int(r.pageviews), "pageviews"),
        Figure::ink(fmt_int(r.sessions), "sessions"),
        Figure::ink(fmt_pct(r.bounce_rate), "bounce rate"),
    ]));

    body.push_str(&spacer(30));
    body.push_str(&trace_rule());
    body.push_str(&spacer(22));

    body.push_str(&heading("What the machines did"));
    body.push_str(&spacer(16));
    body.push_str(&figure_row(&[
        Figure::ink(fmt_int(r.agent_fetches), "agent fetches"),
        Figure::ink(fmt_int(r.answer_arrivals), "answer arrivals"),
        Figure::accent(fmt_ratio(r.gap_ratio), "fetched per arrival"),
    ]));

    body.push_str(&spacer(30));
    body.push_str(&trace_rule());
    body.push_str(&spacer(22));

    body.push_str(&list_block("Pages", "visitors", &r.top_pages));
    body.push_str(&spacer(26));
    body.push_str(&list_block("Sources", "visitors", &r.top_sources));
    body.push_str(&spacer(26));
    body.push_str(&list_block("Agents", "fetches", &r.top_agents));

    body.push_str(&spacer(30));
    body.push_str(&button(&link, "Open the record"));

    let html = layout(
        &subject,
        &format!(
            "{} visitors, {} agent fetches.",
            fmt_int(r.visitors),
            fmt_int(r.agent_fetches)
        ),
        &format!("{site} · {period}"),
        &body,
        "This report is scheduled under Settings, Reports, on the site it covers.",
    );

    let mut t = String::new();
    t.push_str(&format!(
        "VISITORS  {}   {delta_text} on the period before\n\n",
        fmt_int(r.visitors)
    ));
    t.push_str(&text_row("Pageviews", &fmt_int(r.pageviews)));
    t.push_str(&text_row("Sessions", &fmt_int(r.sessions)));
    t.push_str(&text_row("Bounce rate", &fmt_pct(r.bounce_rate)));
    t.push_str("\nWHAT THE MACHINES DID\n\n");
    t.push_str(&text_row("Agent fetches", &fmt_int(r.agent_fetches)));
    t.push_str(&text_row("Answer arrivals", &fmt_int(r.answer_arrivals)));
    t.push_str(&text_row("Fetched per arrival", &fmt_ratio(r.gap_ratio)));
    t.push_str(&text_list("PAGES (visitors)", &r.top_pages));
    t.push_str(&text_list("SOURCES (visitors)", &r.top_sources));
    t.push_str(&text_list("AGENTS (fetches)", &r.top_agents));
    t.push_str(&format!("\nOpen the record: {link}\n"));
    t.push_str("\nThis report is scheduled under Settings, Reports, on the site it covers.");

    let text = text_page(&format!("{site} · {period}"), &t);

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

    let deviation = if a.expected.is_finite() && a.expected != 0.0 {
        delta_bits((a.observed - a.expected) / a.expected).0
    } else {
        "no baseline".to_string()
    };

    let mut body = String::new();
    body.push_str(&heading(&a.headline));
    body.push_str(&spacer(12));
    body.push_str(&para(&a.detail));
    body.push_str(&spacer(26));
    body.push_str(&trace_rule());
    body.push_str(&spacer(22));

    // The deviation is set in shellac rather than in an annotation ink. A spike
    // is not automatically good and a drop is not automatically bad, and colour
    // that pretends otherwise reads as a judgement the tool has not earned.
    body.push_str(&figure_row(&[
        Figure::ink(fmt_num(a.observed), &a.metric),
        Figure::ink(fmt_num(a.expected), "expected"),
        Figure::accent(deviation.clone(), "against expected"),
    ]));

    body.push_str(&spacer(30));
    body.push_str(&button(&link, "See it in the record"));

    let html = layout(
        &subject,
        &a.detail,
        &format!("{} · {site}", humanise(&a.kind)),
        &body,
        "Alerts are configured under Settings, Alerts, on the site they watch.",
    );

    let mut t = String::new();
    t.push_str(&format!(
        "{}\n\n{}\n\n",
        a.headline.to_uppercase(),
        a.detail
    ));
    t.push_str(&text_row(&a.metric, &fmt_num(a.observed)));
    t.push_str(&text_row("Expected", &fmt_num(a.expected)));
    t.push_str(&text_row("Against expected", &deviation));
    t.push_str(&format!("\nSee it in the record: {link}\n"));
    t.push_str("\nAlerts are configured under Settings, Alerts, on the site they watch.");

    let text = text_page(&format!("{} · {site}", humanise(&a.kind)), &t);

    Rendered {
        subject,
        html,
        text,
    }
}

// ---------------------------------------------------------------------------
// The shell
// ---------------------------------------------------------------------------

/// Wrap a body fragment in the record panel.
///
/// `body` and `footnote` are HTML and are trusted; everything else is escaped
/// here. Keeping the escape boundary at this one function is why no builder
/// above has to think about it.
fn layout(subject: &str, preheader: &str, context: &str, body: &str, footnote: &str) -> String {
    let mut h = String::with_capacity(body.len() + 6144);

    h.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    h.push_str("<meta charset=\"utf-8\">\n");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n");
    // Both spellings: the second is what Apple Mail and Outlook.com actually read
    // before they decide whether to force-invert the message.
    h.push_str("<meta name=\"color-scheme\" content=\"dark\">\n");
    h.push_str("<meta name=\"supported-color-schemes\" content=\"dark\">\n");
    h.push_str(&format!("<title>{}</title>\n", escape(subject)));
    h.push_str(HEAD_STYLE);
    h.push_str("\n</head>\n");
    h.push_str(&format!(
        "<body style=\"margin:0;padding:0;background-color:{SOOT};\">\n"
    ));

    h.push_str(&format!(
        "<div style=\"display:none;max-height:0;overflow:hidden;opacity:0;mso-hide:all;\">{}</div>\n",
        escape(preheader)
    ));

    h.push_str(&format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\" \
         bgcolor=\"{SOOT}\" style=\"background-color:{SOOT};\"><tr>\
         <td align=\"center\" style=\"padding:32px 12px 40px 12px;\">"
    ));

    h.push_str(&format!(
        "<table role=\"presentation\" class=\"mf-panel\" width=\"600\" cellpadding=\"0\" \
         cellspacing=\"0\" border=\"0\" bgcolor=\"{PANEL}\" \
         style=\"width:600px;max-width:600px;background-color:{PANEL};border:1px solid {EDGE};\"><tr>"
    ));

    // The pin-feed gutter. The perforation is a tiled radial gradient, not an
    // image, so it costs no request and cannot be blocked. Clients that drop
    // background images (Outlook's Word engine) keep the sunken column, which
    // still reads as the edge of a sheet.
    h.push_str(&format!(
        "<td class=\"mf-gutter\" width=\"30\" valign=\"top\" bgcolor=\"{GUTTER}\" \
         style=\"width:30px;background-color:{GUTTER};\
         background-image:radial-gradient(circle at 15px 13px,{HOLE} 0,{HOLE} 3px,transparent 3.5px);\
         background-size:30px 26px;font-size:0;line-height:0;\">&nbsp;</td>"
    ));

    h.push_str("<td class=\"mf-pad\" valign=\"top\" style=\"padding:26px 30px 28px 26px;\">");

    // Masthead: the wordmark, and whatever names this particular message.
    h.push_str(&format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\
         <td align=\"left\" valign=\"bottom\" style=\"font-family:{DISPLAY};font-size:15px;\
         letter-spacing:0.17em;text-transform:uppercase;color:{INK};\">Micaforge</td>\
         <td align=\"right\" valign=\"bottom\" style=\"font-family:{SANS};font-size:12px;\
         color:{INK_FAINT};\">{}</td></tr></table>",
        escape(context)
    ));
    h.push_str(&spacer(14));
    h.push_str(&trace_rule());
    h.push_str(&spacer(28));

    h.push_str(body);

    h.push_str(&spacer(30));
    h.push_str(&trace_rule());
    h.push_str(&spacer(16));
    h.push_str(&format!(
        "<div style=\"font-family:{SANS};font-size:12px;line-height:1.65;color:{INK_FAINT};\">{footnote}</div>"
    ));
    h.push_str(&spacer(8));
    h.push_str(&format!(
        "<div style=\"font-family:{SANS};font-size:12px;line-height:1.65;color:{INK_FAINT};\">\
         No images, no tracking pixel. Micaforge cannot tell whether this was opened.</div>"
    ));

    h.push_str("</td></tr></table>");
    h.push_str("</td></tr></table>\n</body>\n</html>\n");
    h
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
    t.push_str("No images, no tracking pixel. Micaforge cannot tell whether this\n");
    t.push_str("was opened.\n");
    t
}

// ---------------------------------------------------------------------------
// Fragments
// ---------------------------------------------------------------------------

/// A scratched trace: one bright line, with the displaced soot burred along its
/// underside and offset to the right. This replaces every hairline divider and
/// every glow in the product.
fn trace_rule() -> String {
    format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\
         <tr><td colspan=\"2\" height=\"1\" style=\"height:1px;line-height:1px;font-size:0;\
         background-color:{TRACE};\">&nbsp;</td></tr>\
         <tr><td width=\"24\" height=\"1\" style=\"width:24px;height:1px;line-height:1px;font-size:0;\">&nbsp;</td>\
         <td height=\"1\" style=\"height:1px;line-height:1px;font-size:0;background-color:{BURR};\">&nbsp;</td></tr>\
         </table>"
    )
}

/// Vertical space. A div with an explicit height beats a margin, which Outlook
/// discards, and beats a spacer image, which this crate refuses to send.
fn spacer(px: u32) -> String {
    format!("<div style=\"height:{px}px;line-height:{px}px;font-size:0;\">&nbsp;</div>")
}

/// A section heading in the display face. Sentence case, no kicker above it.
fn heading(text: &str) -> String {
    format!(
        "<div style=\"font-family:{DISPLAY};font-size:23px;line-height:1.25;color:{INK};\">{}</div>",
        escape(text)
    )
}

/// A paragraph of running copy.
fn para(text: &str) -> String {
    format!(
        "<div style=\"font-family:{SANS};font-size:15px;line-height:1.65;color:{INK_BODY};\">{}</div>",
        escape(text)
    )
}

/// The one call to action: a solid shellac block with soot type on it.
///
/// No gradient, no pill, no offset slab behind it pretending to be a shadow. A
/// mail client will not render a shadow honestly, so the button does not ask for
/// one.
fn button(href: &str, label: &str) -> String {
    let safe = safe_href(href);
    format!(
        "<table role=\"presentation\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\"><tr>\
         <td bgcolor=\"{SHELLAC}\" style=\"background-color:{SHELLAC};border-radius:2px;\">\
         <a href=\"{}\" style=\"display:inline-block;padding:14px 24px;font-family:{SANS};\
         font-size:15px;font-weight:600;line-height:1;color:{SOOT};text-decoration:none;\">{}</a>\
         </td></tr></table>",
        escape(&safe),
        escape(label)
    )
}

/// The address written out, for the clients and the people that do not trust a
/// button. An auth link that only exists inside an anchor is an account nobody
/// can recover.
fn link_fallback(href: &str) -> String {
    let safe = safe_href(href);
    format!(
        "<div style=\"font-family:{SANS};font-size:12px;line-height:1.6;color:{INK_FAINT};\">\
         If the button does not open, use this address:</div>\
         <div style=\"font-family:{TABULAR};font-size:12px;line-height:1.6;word-break:break-all;\">\
         <a href=\"{0}\" style=\"color:{SHELLAC_LIT};text-decoration:underline;\">{0}</a></div>",
        escape(&safe)
    )
}

/// One figure and its caption.
struct Figure {
    value: String,
    caption: String,
    colour: &'static str,
}

impl Figure {
    /// A figure in ink: the ordinary case.
    fn ink(value: String, caption: &str) -> Self {
        Figure {
            value,
            caption: caption.to_string(),
            colour: INK,
        }
    }

    /// A figure in shellac. Reserved for the number the message exists to
    /// deliver, which is at most one per block.
    fn accent(value: String, caption: &str) -> Self {
        Figure {
            value,
            caption: caption.to_string(),
            colour: SHELLAC,
        }
    }
}

/// A row of figures with their captions.
///
/// Values and captions live in two `<tr>`s rather than in per-column stacks, so
/// every figure sits on one baseline and every caption on another however long
/// the strings are. A ragged row of numbers is the fastest way to make a report
/// look broken.
fn figure_row(items: &[Figure]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let width = 100 / items.len() as u32;

    let mut values = String::new();
    let mut captions = String::new();
    for f in items {
        values.push_str(&format!(
            "<td width=\"{width}%\" align=\"left\" valign=\"bottom\" class=\"mf-fig\" \
             style=\"width:{width}%;padding:0 12px 0 0;font-family:{DISPLAY};font-size:27px;\
             line-height:1.15;letter-spacing:0.01em;color:{};\">{}</td>",
            f.colour,
            escape(&f.value)
        ));
        captions.push_str(&format!(
            "<td width=\"{width}%\" align=\"left\" valign=\"top\" \
             style=\"width:{width}%;padding:7px 12px 0 0;font-family:{SANS};font-size:12px;\
             line-height:1.4;color:{INK_DIM};\">{}</td>",
            escape(&f.caption)
        ));
    }

    format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\
         <tr>{values}</tr><tr>{captions}</tr></table>"
    )
}

/// A ranked list: a display heading, the unit of its right-hand column, and the
/// rows. Numbers are set in the tabular face because they are read down a column.
fn list_block(title: &str, unit: &str, rows: &[(String, u64)]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "<table role=\"presentation\" width=\"100%\" cellpadding=\"0\" cellspacing=\"0\" border=\"0\">\
         <tr><td align=\"left\" valign=\"bottom\" style=\"padding:0 0 12px 0;font-family:{DISPLAY};\
         font-size:19px;line-height:1.2;color:{INK};\">{}</td>\
         <td align=\"right\" valign=\"bottom\" width=\"110\" style=\"width:110px;padding:0 0 12px 0;\
         font-family:{SANS};font-size:11px;line-height:1.2;color:{INK_FAINT};\">{}</td></tr>",
        escape(title),
        escape(unit)
    ));

    if rows.is_empty() {
        // Principle 5: an empty state says empty. It never borrows last week's
        // number and it never shows a plausible-looking zero-filled row.
        out.push_str(&format!(
            "<tr><td colspan=\"2\" style=\"font-family:{SANS};font-size:14px;line-height:1.5;\
             color:{INK_FAINT};\">Nothing recorded in this period.</td></tr>"
        ));
    } else {
        for (label, value) in rows {
            out.push_str(&format!(
                "<tr><td align=\"left\" valign=\"top\" style=\"padding:0 12px 9px 0;font-family:{SANS};\
                 font-size:14px;line-height:1.45;color:{INK_BODY};\">{}</td>\
                 <td align=\"right\" valign=\"top\" width=\"110\" style=\"width:110px;padding:0 0 9px 0;\
                 font-family:{TABULAR};font-size:14px;line-height:1.45;color:{INK};\">{}</td></tr>",
                escape(&truncate(label, 46)),
                fmt_int(*value)
            ));
        }
    }

    out.push_str("</table>");
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
/// account-recovery email.
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
/// stable token. An email masthead is not the place to show one.
fn humanise(slug: &str) -> String {
    slug.replace('_', " ")
}

/// Drop a trailing slash so a joined path never doubles it.
fn trim(base_url: &str) -> &str {
    base_url.trim_end_matches('/')
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
/// separate verdigris from oxide. Colour is never the only encoding.
fn delta_bits(v: f64) -> (String, &'static str) {
    if !v.is_finite() {
        return ("no comparison".to_string(), INK_DIM);
    }
    let pct = v * 100.0;
    if pct.abs() < 0.05 {
        return ("no change".to_string(), INK_DIM);
    }
    let colour = if pct > 0.0 { VERDIGRIS } else { OXIDE };
    (format!("{pct:+.1}%"), colour)
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
    }

    #[test]
    fn deltas_carry_their_sign() {
        assert_eq!(delta_bits(0.124).0, "+12.4%");
        assert_eq!(delta_bits(-0.031).0, "-3.1%");
        assert_eq!(delta_bits(0.0).0, "no change");
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
    }

    #[test]
    fn a_hostile_alert_link_is_dropped_not_rendered() {
        assert_eq!(safe_href("javascript:alert(1)"), "");
        assert_eq!(safe_href("  https://ok.example/x "), "https://ok.example/x");
        let r = alert(
            "https://mf.example.com",
            "site",
            &AlertPayload {
                kind: "drop".to_string(),
                headline: "Traffic fell".to_string(),
                detail: "detail".to_string(),
                metric: "visitors".to_string(),
                observed: 10.0,
                expected: 100.0,
                link: "javascript:alert(1)".to_string(),
            },
        );
        assert!(!r.html.contains("javascript:"));
        assert!(r.html.contains("https://mf.example.com/app"));
    }

    #[test]
    fn every_message_ships_both_parts_and_no_image() {
        let base = "https://mf.example.com/";
        let all = vec![
            verification(base, "tok"),
            password_reset(base, "tok"),
            invitation(base, "Reeds", "tok"),
            report(base, "reeds.dev", "last 7 days", &sample_report()),
            alert(
                base,
                "reeds.dev",
                &AlertPayload {
                    kind: "agent_surge".to_string(),
                    headline: "GPTBot tripled".to_string(),
                    detail: "detail".to_string(),
                    metric: "fetches".to_string(),
                    observed: 3000.0,
                    expected: 1000.0,
                    link: String::new(),
                },
            ),
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
}
