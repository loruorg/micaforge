//! `GET /mf.js` — the tracker itself.
//!
//! The script tag on a customer's site points here, so this is the most-requested
//! route in the product and the only one whose response is identical for every
//! caller. It is therefore held in memory, hashed once, and served from a
//! `OnceLock` with no allocation per request beyond the response envelope.
//!
//! ## Why the bundle is read at startup rather than compiled in
//!
//! The obvious shape is `include_str!("../../../../../packages/sdk/dist/mf.js")`.
//! It is also a build that fails on a clean checkout, because the tracker is
//! built by `pnpm`, in a different toolchain, into a directory that does not
//! exist until it has run. A Rust workspace that cannot `cargo check` until
//! somebody has run a JavaScript bundler is a workspace that wastes an hour of
//! every new contributor's first day.
//!
//! So the file is located at startup: `MICAFORGE_TRACKER_PATH` first, then the
//! conventional locations for a container image and a development checkout, and
//! finally a compiled-in fallback. The fallback is not a placeholder — it is a
//! small, working tracker that records pageviews — because a self-hoster whose
//! image was built without the bundler should collect *something* rather than
//! serve a file that silently does nothing.
//!
//! ## Caching
//!
//! `public, max-age=86400, must-revalidate` with a strong `ETag`. A day is long
//! enough that the script is not re-fetched on every navigation and short enough
//! that an install which upgrades on Tuesday is fully rolled over by Wednesday.
//! `must-revalidate` is the important half: it forbids a cache from serving the
//! stale copy after expiry, which is what stops an old tracker outliving the
//! collector it talks to.
//!
//! ## The plugins
//!
//! `GET /api/track/p/<name>.js` serves the bundles the tracker loads on demand
//! (`packages/sdk/dist/p/*.js`): web vitals by default, and error capture,
//! frustration signals, form submissions, the 404 marker and click tags when the
//! site turns them on. They live under the ingest path on purpose. Every proxy
//! a customer has written to keep the tracker first-party already forwards
//! `/api/track*`, so a plugin reaches the page through the same rule the hits
//! use, and nobody's vitals stop arriving because a new path was not proxied.
//!
//! The names are a closed set ([`PLUGINS`]); anything else is `404`. A plugin
//! that was not built is served as an empty script rather than a `404`, for the
//! same reason the core has a fallback: a missing file would put an error in the
//! console of the customer's page, and an absent optional feature should not.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use axum::extract::{Path as UrlPath, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::state::AppState;

/// How long a browser may reuse the tracker before revalidating.
const CACHE_CONTROL: &str = "public, max-age=86400, must-revalidate";

/// The tracker served when no built bundle can be found.
///
/// Deliberately complete rather than a stub that throws: it configures itself
/// from the same `data-site` and `data-host` attributes as the real build,
/// sends a pageview on load and on every SPA navigation, and is wrapped so that
/// nothing inside it can ever reach the host page. `sdk_version` is reported as
/// `fallback` so a dashboard showing traffic from it says plainly where the hits
/// came from, instead of implying a release that was never installed.
const FALLBACK_TRACKER: &str = r#"/*! Micaforge tracker (fallback build) */
(function () {
  try {
    var d = document, w = window, n = navigator;
    var el = d.currentScript || d.querySelector('script[data-site]');
    if (!el) return;
    var site = parseInt(el.getAttribute('data-site') || '0', 10);
    if (!site) return;
    var host = (el.getAttribute('data-host') || String(el.src || '').replace(/^(\w+:\/\/[^/]*).*$/, '$1')).replace(/\/+$/, '');
    var endpoint = host + '/api/track';
    var last = '';

    function send(kind, referrer) {
      try {
        var loc = w.location;
        if (loc.protocol === 'file:') return;
        var body = JSON.stringify({
          s: site,
          t: kind,
          u: loc.href.split('#')[0],
          r: referrer,
          l: n.language,
          w: screen.width,
          h: screen.height,
          v: 'fallback'
        });
        if (n.sendBeacon && n.sendBeacon(endpoint, body)) return;
        w.fetch(endpoint, { method: 'POST', body: body, keepalive: true }).catch(function () {});
      } catch (e) {}
    }

    function page() {
      try {
        var here = w.location.href.split('#')[0];
        if (here === last) return;
        var from = last || d.referrer;
        last = here;
        send('pageview', from);
      } catch (e) {}
    }

    function wrap(name) {
      try {
        var original = history[name];
        if (typeof original !== 'function') return;
        history[name] = function () {
          var result = original.apply(this, arguments);
          page();
          return result;
        };
      } catch (e) {}
    }

    wrap('pushState');
    wrap('replaceState');
    w.addEventListener('popstate', page);
    page();
  } catch (e) {}
})();
"#;

/// A loaded tracker bundle and everything needed to serve it.
struct Bundle {
    /// The JavaScript, verbatim.
    body: &'static str,
    /// A strong `ETag`, quotes included, over the exact bytes served.
    etag: String,
    /// Where it came from, for the one log line written at first request.
    origin: String,
}

/// The tracker routes.
///
/// `/mf.min.js` is the alias `docs/CONTRACT.md` §3.3 promises. It is a separate
/// entry rather than a redirect because a redirect on the most-requested asset
/// in the product costs every visitor a round trip to save nothing.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/mf.js", get(serve_tracker))
        .route("/mf.min.js", get(serve_minified))
        .route("/api/track/p/{file}", get(serve_plugin))
}

/// The plugin bundles, by name. The order is the index into [`plugin`]'s table.
pub const PLUGINS: &[&str] = &[
    "vitals",
    "errors",
    "frustration",
    "forms",
    "track404",
    "tags",
];

/// `GET /api/track/p/<name>.js`.
async fn serve_plugin(UrlPath(file): UrlPath<String>, headers: HeaderMap) -> Response {
    match plugin_index(&file) {
        Some(index) => respond(plugin(index), &headers),
        None => {
            let mut res = StatusCode::NOT_FOUND.into_response();
            put(res.headers_mut(), "cache-control", "no-store");
            put(res.headers_mut(), "access-control-allow-origin", "*");
            res
        }
    }
}

/// Which plugin a requested file name is, or `None` for one that is not a plugin.
fn plugin_index(file: &str) -> Option<usize> {
    let name = file.strip_suffix(".js")?;
    PLUGINS.iter().position(|p| *p == name)
}

/// One plugin bundle, loaded on first use.
fn plugin(index: usize) -> &'static Bundle {
    static BUNDLES: OnceLock<Vec<Bundle>> = OnceLock::new();
    &BUNDLES.get_or_init(|| PLUGINS.iter().map(|name| load_plugin(name)).collect())[index]
}

/// Locate and read one plugin bundle.
///
/// Looked for beside the tracker: in a `p/` directory next to the file
/// `MICAFORGE_TRACKER_PATH` names when it is set, then in the same
/// conventional places as the tracker. No new variable: an operator who
/// relocated the tracker relocated its plugins with it.
fn load_plugin(name: &str) -> Bundle {
    let filename = format!("{name}.js");
    let mut paths = Vec::new();
    if let Some(configured) = std::env::var("MICAFORGE_TRACKER_PATH")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        if let Some(dir) = Path::new(&configured).parent() {
            paths.push(dir.join("p").join(&filename));
        }
    }
    for base in [
        "/app/tracker/p",
        "packages/sdk/dist/p",
        "../../packages/sdk/dist/p",
    ] {
        paths.push(Path::new(base).join(&filename));
    }

    for candidate in paths {
        match std::fs::read_to_string(&candidate) {
            Ok(source) if !source.trim().is_empty() => {
                tracing::info!(path = %candidate.display(), bytes = source.len(), "tracker plugin loaded");
                // Leaked once per plugin per process, for the reason `load` gives.
                let body: &'static str = Box::leak(source.into_boxed_str());
                return Bundle {
                    body,
                    etag: etag_of(body),
                    origin: candidate.display().to_string(),
                };
            }
            Ok(_) | Err(_) => {}
        }
    }

    tracing::warn!(
        plugin = name,
        "no built tracker plugin found; serving an empty script. \
         Build the SDK (`pnpm --filter @micaforge/sdk build`)."
    );
    let body: &'static str =
        Box::leak(format!("/*! Micaforge plugin {name}: not built */").into_boxed_str());
    Bundle {
        body,
        etag: etag_of(body),
        origin: "empty fallback".to_string(),
    }
}

/// `GET /mf.js`.
async fn serve_tracker(req: Request) -> Response {
    respond(tracker(), req.headers())
}

/// `GET /mf.min.js`.
async fn serve_minified(req: Request) -> Response {
    respond(minified(), req.headers())
}

/// Answer with the bundle, or `304` when the caller already has it.
fn respond(bundle: &'static Bundle, headers: &HeaderMap) -> Response {
    if if_none_match(headers, &bundle.etag) {
        let mut res = StatusCode::NOT_MODIFIED.into_response();
        decorate(&mut res, bundle);
        return res;
    }

    let mut res = bundle.body.into_response();
    decorate(&mut res, bundle);
    put(
        res.headers_mut(),
        "content-type",
        "text/javascript; charset=utf-8",
    );
    res
}

/// The headers a `200` and a `304` must both carry.
///
/// A `304` without the `ETag` and the freshness directives tells the cache
/// nothing about how long its copy is now good for, so it revalidates on the
/// very next request and the cache stops being one.
fn decorate(res: &mut Response, bundle: &'static Bundle) {
    let headers = res.headers_mut();
    put(headers, "etag", &bundle.etag);
    put(headers, "cache-control", CACHE_CONTROL);
    // Served cross-origin by definition: the script tag lives on the customer's
    // domain and this file does not.
    put(headers, "access-control-allow-origin", "*");
    put(headers, "x-content-type-options", "nosniff");
    put(headers, "vary", "accept-encoding");
}

/// Does the caller's `If-None-Match` cover this entity?
///
/// Handles the three shapes that actually arrive: a single tag, a
/// comma-separated list, and `*`. The weak-comparison prefix is stripped before
/// comparing, which is correct here — the tag is a content hash, so a weak match
/// and a strong match are the same statement about the bytes.
fn if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers.get("if-none-match").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    if value.trim() == "*" {
        return true;
    }
    value
        .split(',')
        .map(|candidate| candidate.trim().trim_start_matches("W/").trim())
        .any(|candidate| candidate == etag)
}

/// The `/mf.js` bundle, loaded on first use.
fn tracker() -> &'static Bundle {
    static BUNDLE: OnceLock<Bundle> = OnceLock::new();
    BUNDLE.get_or_init(|| load("MICAFORGE_TRACKER_PATH", "mf.js"))
}

/// The `/mf.min.js` bundle.
///
/// Falls back to the unminified build rather than to the compiled-in tracker,
/// because a site that asked for the minified alias wants *this* install's
/// tracker, and the two files are the same script through a different pass.
fn minified() -> &'static Bundle {
    static BUNDLE: OnceLock<Bundle> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        let loaded = load("MICAFORGE_TRACKER_MIN_PATH", "mf.min.js");
        if loaded.origin == "compiled-in fallback" {
            let full = tracker();
            if full.origin != "compiled-in fallback" {
                return Bundle {
                    body: full.body,
                    etag: full.etag.clone(),
                    origin: full.origin.clone(),
                };
            }
        }
        loaded
    })
}

/// Locate and read one bundle.
///
/// Order: the environment variable, then the conventional locations, then the
/// compiled-in fallback. Reading fails closed onto the next candidate rather
/// than onto an error, because a tracker route that returns `500` takes every
/// customer's analytics down at once and a working fallback does not.
fn load(env_var: &str, filename: &str) -> Bundle {
    for candidate in candidates(env_var, filename) {
        match std::fs::read_to_string(&candidate) {
            Ok(source) if !source.trim().is_empty() => {
                tracing::info!(path = %candidate.display(), bytes = source.len(), "tracker loaded");
                // Leaked on purpose, exactly once per process: the bundle lives
                // for the life of the server, and a `&'static str` is what lets
                // every response borrow it instead of cloning a few kilobytes.
                let body: &'static str = Box::leak(source.into_boxed_str());
                return Bundle {
                    body,
                    etag: etag_of(body),
                    origin: candidate.display().to_string(),
                };
            }
            Ok(_) => tracing::warn!(path = %candidate.display(), "tracker file is empty; skipping"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(path = %candidate.display(), error = %e, "tracker unreadable"),
        }
    }

    tracing::warn!(
        env_var,
        "no built tracker found; serving the compiled-in fallback. \
         Build the SDK (`pnpm --filter @micaforge/sdk build`) or set the variable \
         to the bundle's path."
    );
    Bundle {
        body: FALLBACK_TRACKER,
        etag: etag_of(FALLBACK_TRACKER),
        origin: "compiled-in fallback".to_string(),
    }
}

/// Where to look for a bundle, in order.
///
/// The container image path comes before the source-tree paths so that a
/// production image never picks up a stale checkout mounted beside it, and the
/// source-tree paths are relative to the working directory so `cargo run` from
/// either the workspace root or `apps/backend` finds the same file.
fn candidates(env_var: &str, filename: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(configured) = std::env::var(env_var).ok().map(|v| v.trim().to_string()) {
        if !configured.is_empty() {
            paths.push(PathBuf::from(configured));
        }
    }
    for base in [
        "/app/tracker",
        "packages/sdk/dist",
        "../../packages/sdk/dist",
    ] {
        paths.push(Path::new(base).join(filename));
    }
    paths
}

/// A strong `ETag` over the served bytes.
///
/// A content hash rather than a version string or a modification time: two
/// installs of the same release serve the same tag, and rebuilding an unchanged
/// file does not invalidate anyone's cache.
fn etag_of(body: &str) -> String {
    let digest = blake3::hash(body.as_bytes());
    format!("\"{}\"", hex::encode(&digest.as_bytes()[..16]))
}

/// Set a response header, ignoring a value that cannot be one.
fn put(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(value) = value.parse() {
        headers.insert(name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn with(header: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert("if-none-match", HeaderValue::from_str(header).unwrap());
        map
    }

    #[test]
    fn an_etag_is_a_quoted_content_hash() {
        let tag = etag_of("console.log(1)");
        assert!(tag.starts_with('"') && tag.ends_with('"'));
        assert_eq!(
            tag,
            etag_of("console.log(1)"),
            "the same bytes hash the same"
        );
        assert_ne!(tag, etag_of("console.log(2)"));
        // 16 bytes of hash, hex, plus the two quotes.
        assert_eq!(tag.len(), 34);
    }

    #[test]
    fn conditional_requests_are_matched_in_every_shape_they_arrive_in() {
        let tag = etag_of("x");
        assert!(if_none_match(&with(&tag), &tag));
        assert!(
            if_none_match(&with(&format!("W/{tag}")), &tag),
            "weak tags hash the same bytes"
        );
        assert!(if_none_match(&with(&format!("\"other\", {tag}")), &tag));
        assert!(if_none_match(&with("*"), &tag));
        assert!(!if_none_match(&with("\"stale\""), &tag));
        assert!(!if_none_match(&HeaderMap::new(), &tag));
    }

    #[test]
    fn the_configured_path_is_tried_before_the_conventions() {
        const VAR: &str = "MICAFORGE_TRACKER_PATH_UNDER_TEST";

        // Unset: the conventions are all there is, image path first.
        std::env::remove_var(VAR);
        let conventions = candidates(VAR, "mf.js");
        assert_eq!(
            conventions.first().unwrap(),
            Path::new("/app/tracker/mf.js")
        );
        assert!(conventions
            .iter()
            .any(|p| p.ends_with("packages/sdk/dist/mf.js")));

        // Set: an operator who names a file gets that file, or nothing.
        std::env::set_var(VAR, "/srv/custom/mf.js");
        let configured = candidates(VAR, "mf.js");
        assert_eq!(configured.first().unwrap(), Path::new("/srv/custom/mf.js"));
        assert_eq!(configured.len(), conventions.len() + 1);
        std::env::remove_var(VAR);
    }

    #[test]
    fn only_the_known_plugins_are_served() {
        assert_eq!(plugin_index("vitals.js"), Some(0));
        assert_eq!(plugin_index("frustration.js"), Some(2));
        assert_eq!(plugin_index("tags.js"), Some(PLUGINS.len() - 1));
        // Anything else is not a file this route will look for, so a request
        // can never steer the loader at a path of its choosing.
        assert_eq!(plugin_index("vitals"), None);
        assert_eq!(plugin_index("../mf.js"), None);
        assert_eq!(plugin_index("..%2Fsecret.js"), None);
        assert_eq!(plugin_index("unknown.js"), None);
    }

    #[test]
    fn the_plugin_list_matches_the_sdk_build() {
        // The tracker asks for these names and the SDK build writes these
        // files; a name in one list and not the other is a feature that loads
        // an empty script on every page.
        let build = include_str!("../../../../../../packages/sdk/scripts/build.mjs");
        let line = build
            .lines()
            .find(|l| l.trim_start().starts_with("const PLUGINS = ["))
            .expect("the SDK build names its plugins");
        for name in PLUGINS {
            assert!(
                line.contains(&format!("\"{name}\"")),
                "{name} is not built by the SDK"
            );
        }
        assert_eq!(
            line.matches('"').count(),
            PLUGINS.len() * 2,
            "the SDK builds a plugin this route does not serve"
        );
    }

    #[test]
    fn the_fallback_tracker_is_a_real_script() {
        // It has to survive being served: a placeholder that throws would break
        // the page it was meant to protect.
        assert!(FALLBACK_TRACKER.contains("/api/track"));
        assert!(FALLBACK_TRACKER.contains("data-site"));
        assert!(FALLBACK_TRACKER.contains("sendBeacon"));
        assert!(
            FALLBACK_TRACKER.contains("try {"),
            "nothing may reach the host page"
        );
    }
}
