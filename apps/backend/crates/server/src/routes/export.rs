//! Taking the record off the machine: `GET /api/stats/export.csv` and
//! `GET /api/stats/export.pdf`. `docs/CONTRACT.md` §3.3.
//!
//! Both routes answer the *same question* as the screen the person is looking
//! at. They take the standard envelope, run the same engine function the
//! matching endpoint runs, and write the rows out. An export that quietly
//! widened the window or dropped a filter would be the worst kind of wrong: it
//! is the copy that leaves the building and gets pasted into a deck.
//!
//! ## About `export.pdf`
//!
//! **This route does not return a PDF, and it does not pretend to.** It returns
//! a self-contained, print-styled HTML document, with `content-type: text/html`
//! and an attachment filename ending in `.html`. Opening it and pressing print
//! gives the report as paper or as a PDF, via the renderer already on the
//! reader's machine.
//!
//! The path is `export.pdf` because that is the path the contract fixes. The
//! *file* is not, because typing a real PDF requires a layout engine
//! (headless Chromium, `typst`, `weasyprint`) that this build does not carry,
//! and handing somebody an HTML document named `report.pdf` would be a small
//! lie that only shows up when their PDF reader refuses to open it. A real
//! render is a follow-up; until it lands, the honest artifact is the printable
//! one.

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderValue};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use micaforge_core::{Audience, Dimension, Error, Metric, QueryEnvelope};
use micaforge_query::stats::{self, GoalDef};
use micaforge_query::to_csv;
use micaforge_store::models::goals as goal_repo;

use super::agents::same_site;
use crate::error::ApiError;
use crate::extract::{Envelope, SiteAccess};
use crate::state::AppState;

/// The two export routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/stats/export.csv", get(export_csv))
        .route("/api/stats/export.pdf", get(export_html))
}

/// Rows carried into the printable report per table.
///
/// A printed report is read, not scrolled. Twenty rows fills a page and says
/// what the page is for; two hundred is a data dump wearing a title.
const REPORT_ROWS: usize = 20;

/// Which report an export is of.
#[derive(Debug, Default, Deserialize)]
struct ExportParams {
    /// A report name, optionally with its dimension after a colon:
    /// `pages`, `pages:entry_path`, `breakdown:country`, `sessions`.
    report: Option<String>,
    /// A dimension, for the reports that take one. Overridden by the suffix on
    /// `report` when both are given.
    dimension: Option<String>,
}

/// The reports an export can be of.
///
/// A closed set on purpose. The name lands in a filename and in a
/// `content-disposition` header, and a closed set is what makes it safe to put
/// there without escaping games.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Report {
    /// The totals card, as one row.
    Overview,
    /// Pages, with entries, exits, scroll and time.
    Pages,
    /// Any dimension, ranked.
    Breakdown,
    /// The session list.
    Sessions,
    /// The visitor list.
    Visitors,
    /// Custom event names.
    Events,
    /// Configured goals and their conversions.
    Goals,
    /// Core Web Vitals by a dimension.
    Performance,
    /// Client-side exceptions, grouped.
    Errors,
}

impl Report {
    /// The name that goes in the filename. Never caller text.
    fn slug(self) -> &'static str {
        match self {
            Report::Overview => "overview",
            Report::Pages => "pages",
            Report::Breakdown => "breakdown",
            Report::Sessions => "sessions",
            Report::Visitors => "visitors",
            Report::Events => "events",
            Report::Goals => "goals",
            Report::Performance => "performance",
            Report::Errors => "errors",
        }
    }

    fn parse(name: &str) -> Result<Self, Error> {
        match name.trim() {
            "" | "overview" => Ok(Report::Overview),
            "pages" => Ok(Report::Pages),
            "breakdown" | "sources" | "locations" | "devices" => Ok(Report::Breakdown),
            "sessions" => Ok(Report::Sessions),
            "visitors" => Ok(Report::Visitors),
            "events" => Ok(Report::Events),
            "goals" => Ok(Report::Goals),
            "performance" => Ok(Report::Performance),
            "errors" => Ok(Report::Errors),
            other => Err(Error::validation(
                "report",
                format!(
                    "there is no `{other}` report to export; \
                     it can be overview, pages, breakdown, sessions, visitors, \
                     events, goals, performance or errors"
                ),
            )),
        }
    }
}

/// Split `report` into the report and the dimension it may carry.
///
/// `pages:entry_path` is how the audience screens name a tab, and it is the same
/// thing as `report=pages&dimension=entry_path`. Both spellings are read so an
/// export key can be built from whatever the screen already has in hand.
fn resolve(params: &ExportParams) -> Result<(Report, Dimension), Error> {
    let raw = params.report.as_deref().unwrap_or("overview");
    let (name, suffix) = match raw.split_once(':') {
        Some((name, dim)) => (name, Some(dim)),
        None => (raw, None),
    };

    let report = Report::parse(name)?;

    // The three screen names that are really one breakdown each keep the
    // dimension their screen opens on, so `report=locations` exports countries
    // rather than paths.
    let fallback = match name.trim() {
        "sources" => Dimension::Channel,
        "locations" => Dimension::Country,
        "devices" => Dimension::Browser,
        _ => Dimension::Pathname,
    };

    let dimension = match suffix.or(params.dimension.as_deref()) {
        Some(raw) if !raw.trim().is_empty() => Dimension::parse(raw)?,
        _ => fallback,
    };

    Ok((report, dimension))
}

// ---------------------------------------------------------------------------
// Flat rows
// ---------------------------------------------------------------------------

/// The totals card as one CSV row.
///
/// `micaforge_query::to_csv` refuses a nested row, and it is right to: a cell
/// holds one value. So the comparison window and the deltas are dropped here
/// rather than flattened into columns whose meaning would have to be explained
/// in a second document.
#[derive(Debug, Serialize)]
struct OverviewCsv {
    visitors: u64,
    sessions: u64,
    pageviews: u64,
    bounce_rate: Option<f64>,
    avg_duration_s: Option<f64>,
    views_per_session: Option<f64>,
}

/// One error group as one CSV row.
///
/// The sparkline is a `Vec` and has no honest CSV cell, so it is left out and
/// the counts that bound it are kept.
#[derive(Debug, Serialize)]
struct ErrorCsv {
    fingerprint: String,
    name: String,
    message: String,
    count: u64,
    visitors: u64,
    first_seen: String,
    last_seen: String,
}

// ---------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------

/// `GET /api/stats/export.csv` — one report, as a comma-separated document.
///
/// The whole document is built before any of it is sent. That is a deliberate
/// bound rather than an oversight: `micaforge_query::to_csv` writes a slice, and
/// the slice is exactly the page the envelope's `limit` asked for, so the
/// largest response this route can produce is one page of one report. A chunked
/// writer that streams straight off a ClickHouse cursor belongs in the engine
/// beside `to_csv`, not here — the route would only be passing it through.
async fn export_csv(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
    Query(params): Query<ExportParams>,
) -> Result<Response, ApiError> {
    same_site(&access, &env)?;
    let (report, dimension) = resolve(&params)?;

    let csv = match report {
        Report::Overview => {
            let totals = stats::overview(&st.ch, &env).await?;
            to_csv(&[OverviewCsv {
                visitors: totals.visitors,
                sessions: totals.sessions,
                pageviews: totals.pageviews,
                bounce_rate: totals.bounce_rate,
                avg_duration_s: totals.avg_duration_s,
                views_per_session: totals.views_per_session,
            }])?
        }
        Report::Pages if dimension == Dimension::Pathname && env.audience == Audience::Human => {
            to_csv(&stats::pages(&st.ch, &env).await?)?
        }
        Report::Pages | Report::Breakdown => {
            to_csv(&stats::breakdown(&st.ch, &env, &dimension, &[Metric::Visitors]).await?)?
        }
        Report::Sessions => to_csv(&stats::sessions(&st.ch, &env).await?.rows)?,
        Report::Visitors => to_csv(&stats::visitors(&st.ch, &env).await?.rows)?,
        Report::Events => to_csv(&stats::events(&st.ch, &env).await?)?,
        Report::Goals => {
            let defs = goal_defs(&st, access.site.id).await?;
            to_csv(&stats::goals(&st.ch, &env, &defs).await?)?
        }
        Report::Performance => to_csv(&stats::performance(&st.ch, &env, &dimension).await?)?,
        Report::Errors => {
            let rows: Vec<ErrorCsv> = stats::errors(&st.ch, &env)
                .await?
                .into_iter()
                .map(|g| ErrorCsv {
                    fingerprint: g.fingerprint,
                    name: g.name,
                    message: g.message,
                    count: g.count,
                    visitors: g.users,
                    first_seen: g.first_seen.to_rfc3339(),
                    last_seen: g.last_seen.to_rfc3339(),
                })
                .collect();
            to_csv(&rows)?
        }
    };

    let filename = format!(
        "micaforge-{}-{}-{}.csv",
        report.slug(),
        env.range.start.format("%Y%m%d"),
        env.range.end.format("%Y%m%d"),
    );

    Ok(attachment(csv, "text/csv; charset=utf-8", &filename))
}

/// Every configured goal on a site, in the engine's form.
async fn goal_defs(st: &AppState, site: uuid::Uuid) -> Result<Vec<GoalDef>, Error> {
    goal_repo::list(&st.pg, site)
        .await?
        .into_iter()
        .map(|g| {
            GoalDef::parse(
                g.id.to_string(),
                g.name,
                &g.kind,
                &g.conditions.to_string(),
                g.value_cents,
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The printable report
// ---------------------------------------------------------------------------

/// `GET /api/stats/export.pdf` — the report as a printable HTML document.
///
/// See the module documentation for why this is HTML and why it says so. The
/// document carries no external reference of any kind: no font, no stylesheet,
/// no image, no script. It has to open and print correctly on a machine with no
/// network, which is the same promise the self-hosted build makes everywhere
/// else.
async fn export_html(
    State(st): State<AppState>,
    access: SiteAccess,
    Envelope(env): Envelope,
) -> Result<Response, ApiError> {
    same_site(&access, &env)?;
    let totals = stats::overview(&st.ch, &env).await?;

    // Four ranked tables, all through the same function, so the printed report
    // and the screen it was printed from cannot disagree about how a row was
    // counted. Breakdown rather than the composite tree readings: it is the one
    // shape that answers for the machine record as well as the human one.
    let pages = stats::breakdown(&st.ch, &env, &Dimension::Pathname, &[Metric::Pageviews]).await?;
    let channels = stats::breakdown(&st.ch, &env, &Dimension::Channel, &[Metric::Visitors]).await?;
    let countries =
        stats::breakdown(&st.ch, &env, &Dimension::Country, &[Metric::Visitors]).await?;
    let browsers = stats::breakdown(&st.ch, &env, &Dimension::Browser, &[Metric::Visitors]).await?;

    let html = report_document(
        &access.site.name,
        &access.site.domain,
        &env,
        &totals,
        &[
            ("Pages", "Path", pages.as_slice()),
            ("Acquisition", "Channel", channels.as_slice()),
            ("Places", "Country", countries.as_slice()),
            ("Clients", "Browser", browsers.as_slice()),
        ],
    );

    let filename = format!(
        "micaforge-report-{}-{}.html",
        env.range.start.format("%Y%m%d"),
        env.range.end.format("%Y%m%d"),
    );

    Ok(attachment(html, "text/html; charset=utf-8", &filename))
}

/// Build the printable document.
///
/// Set for paper, not for a browser window: one column, a measured type scale,
/// and rules only where a table genuinely needs a row separated from the next.
/// Nothing here is decorative, because nothing decorative survives a laser
/// printer at 100% black.
fn report_document(
    name: &str,
    domain: &str,
    env: &QueryEnvelope,
    totals: &stats::Overview,
    tables: &[(&str, &str, &[stats::BreakdownRow])],
) -> String {
    let mut out = String::with_capacity(8 * 1024);

    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!(
        "<title>{} — Micaforge report</title>\n",
        esc(name)
    ));
    out.push_str(STYLE);
    out.push_str("</head>\n<body>\n<main>\n");

    out.push_str("<header class=\"head\">\n");
    out.push_str(&format!("<h1>{}</h1>\n", esc(name)));
    out.push_str(&format!(
        "<p class=\"sub\">{} · {} to {} · {} traffic</p>\n",
        esc(domain),
        env.range.start.format("%-d %B %Y"),
        env.range.end.format("%-d %B %Y"),
        env.audience.as_str(),
    ));
    out.push_str("</header>\n");

    out.push_str("<section class=\"totals\">\n<dl>\n");
    for (label, value) in [
        ("Visitors", count(totals.visitors)),
        ("Sessions", count(totals.sessions)),
        ("Pageviews", count(totals.pageviews)),
        ("Bounce rate", percent(totals.bounce_rate)),
        ("Time per session", seconds(totals.avg_duration_s)),
        ("Views per session", decimal(totals.views_per_session)),
    ] {
        out.push_str(&format!(
            "<div><dt>{}</dt><dd>{}</dd></div>\n",
            esc(label),
            value
        ));
    }
    out.push_str("</dl>\n</section>\n");

    for (title, column, rows) in tables {
        out.push_str(&format!("<section>\n<h2>{}</h2>\n", esc(title)));
        if rows.is_empty() {
            out.push_str("<p class=\"empty\">Nothing was recorded in this window.</p>\n");
        } else {
            out.push_str("<table>\n<thead><tr>");
            out.push_str(&format!("<th scope=\"col\">{}</th>", esc(column)));
            out.push_str(
                "<th scope=\"col\" class=\"n\">Visitors</th>\
                 <th scope=\"col\" class=\"n\">Pageviews</th>\
                 <th scope=\"col\" class=\"n\">Share</th>",
            );
            out.push_str("</tr></thead>\n<tbody>\n");
            for row in rows.iter().take(REPORT_ROWS) {
                out.push_str(&format!(
                    "<tr><td>{}</td><td class=\"n\">{}</td><td class=\"n\">{}</td>\
                     <td class=\"n\">{}</td></tr>\n",
                    esc(&row.label),
                    count(row.visitors),
                    count(row.pageviews),
                    percent(Some(row.share * 100.0)),
                ));
            }
            out.push_str("</tbody>\n</table>\n");
            if rows.len() > REPORT_ROWS {
                out.push_str(&format!(
                    "<p class=\"more\">{} further rows are in the CSV export.</p>\n",
                    rows.len() - REPORT_ROWS
                ));
            }
        }
        out.push_str("</section>\n");
    }

    out.push_str(&format!(
        "<footer><p>Recorded by Micaforge · site {} · taken {}</p>\
         <p>This is the printable report. Print it to paper, or to PDF.</p></footer>\n",
        env.site,
        Utc::now().format("%Y-%m-%d %H:%M UTC"),
    ));

    out.push_str("</main>\n</body>\n</html>\n");
    out
}

/// The print stylesheet.
///
/// Inline and complete: the document has to render the same on a machine that
/// has never heard of this server. `@page` sets the paper margin so the browser
/// does not add its own header on top of the report's.
const STYLE: &str = r#"<style>
:root { color-scheme: light; }
@page { margin: 18mm 16mm; }
* { box-sizing: border-box; }
body {
  margin: 0;
  background: #fff;
  color: #14120f;
  font: 11pt/1.5 ui-serif, Georgia, "Times New Roman", serif;
  -webkit-print-color-adjust: exact;
  print-color-adjust: exact;
}
main { max-width: 46rem; margin: 0 auto; padding: 2.5rem 1.5rem 3rem; }
.head { margin-bottom: 2rem; }
h1 { margin: 0; font-size: 1.75rem; font-weight: 600; letter-spacing: -0.01em; }
.sub { margin: 0.35rem 0 0; font-size: 0.9rem; color: #58524a; }
h2 {
  margin: 2.25rem 0 0.6rem;
  font-size: 0.78rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.09em;
  color: #58524a;
}
.totals dl {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 1.1rem 1.5rem;
  margin: 0;
  padding: 1.1rem 0;
  border-top: 1px solid #14120f;
  border-bottom: 1px solid #d8d2c8;
}
.totals dt { font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.08em; color: #58524a; }
.totals dd {
  margin: 0.2rem 0 0;
  font-size: 1.45rem;
  font-variant-numeric: tabular-nums;
  font-feature-settings: "tnum" 1;
}
table { width: 100%; border-collapse: collapse; font-size: 0.9rem; }
thead th {
  text-align: left;
  padding: 0.35rem 0.5rem 0.35rem 0;
  border-bottom: 1px solid #14120f;
  font-size: 0.72rem;
  text-transform: uppercase;
  letter-spacing: 0.07em;
  color: #58524a;
  font-weight: 600;
}
tbody td { padding: 0.36rem 0.5rem 0.36rem 0; border-bottom: 1px solid #ece7de; }
tbody tr:last-child td { border-bottom: 0; }
td:first-child, th:first-child { padding-left: 0; width: 55%; word-break: break-word; }
.n { text-align: right; padding-right: 0 !important; font-variant-numeric: tabular-nums; }
.empty, .more { font-size: 0.85rem; color: #58524a; margin: 0.5rem 0 0; }
footer {
  margin-top: 2.75rem;
  padding-top: 0.9rem;
  border-top: 1px solid #d8d2c8;
  font-size: 0.75rem;
  color: #58524a;
}
footer p { margin: 0.15rem 0; }
section { break-inside: auto; }
tr, h2 { break-inside: avoid; }
h2 { break-after: avoid; }
</style>
"#;

// ---------------------------------------------------------------------------
// Formatting and safety
// ---------------------------------------------------------------------------

/// Escape text for HTML.
///
/// Every string in the report is a page title, a path or a referring host: text
/// the site's own visitors caused to be stored. It is escaped on the way into
/// the document, without exception, because a report is a file a person opens.
fn esc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// A whole number with thin thousands separators.
fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A percentage that is already expressed in whole percent. An em dash for a
/// value that was never measured, never a zero.
fn percent(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{v:.1}%"),
        None => "—".to_string(),
    }
}

/// A duration in seconds, written the way a person reads one.
fn seconds(value: Option<f64>) -> String {
    let Some(v) = value else {
        return "—".to_string();
    };
    let total = v.round().max(0.0) as u64;
    if total < 60 {
        format!("{total}s")
    } else {
        format!("{}m {:02}s", total / 60, total % 60)
    }
}

/// A small ratio, to one decimal place.
fn decimal(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("{v:.1}"),
        None => "—".to_string(),
    }
}

/// Wrap a document as a download.
///
/// The filename is built here from a report slug this module owns and two dates,
/// so nothing a caller sent reaches the header. That matters more than it looks:
/// `content-disposition` is parsed by every browser, and a filename carrying a
/// newline is a header-splitting bug rather than an odd download name.
fn attachment(body: String, content_type: &'static str, filename: &str) -> Response {
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();

    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    // An export is a reading of one moment. A cache between here and the reader
    // would hand them yesterday's file for today's window.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );

    response
}
