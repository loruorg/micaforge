//! Process configuration.
//!
//! Read once at boot from `MICAFORGE_*` environment variables and then treated
//! as immutable. Loading a `.env` file is the binary's job, not this crate's, so
//! that a test can set variables directly without a file on disk.
//!
//! The defaults describe a single-node `docker compose up` install, because that
//! is the deployment `PRODUCT.md` treats as first class. Only three values have
//! no sensible default: the two data stores, and the secret everything else is
//! keyed off.

use crate::error::{Error, Result};

/// Everything the process needs to know about its environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// `host:port` the HTTP server binds. Defaults to all interfaces because the
    /// process normally runs inside a container.
    pub bind_addr: String,
    /// Postgres connection string for identity and configuration. **Required.**
    pub database_url: String,
    /// ClickHouse HTTP endpoint for the event store. **Required.**
    pub clickhouse_url: String,
    /// ClickHouse database name.
    pub clickhouse_db: String,
    /// ClickHouse user.
    pub clickhouse_user: String,
    /// ClickHouse password.
    pub clickhouse_password: String,
    /// Redis (or Dragonfly) URL for sessions, rate limits and the realtime gauge.
    pub redis_url: String,
    /// Public origin of this API. Used in emails, share links and the snippet
    /// the tracking-setup screen shows, so it has to be the address a browser
    /// can actually reach, not the bind address.
    pub public_url: String,
    /// Public origin of the dashboard. Separate from `public_url` because the
    /// static app is normally served from a CDN while the API is not, and CORS
    /// and cookie scope both depend on the distinction.
    pub dashboard_url: String,
    /// The one operator secret. Every other key is derived from it by
    /// [`crypto::derive_key`](crate::crypto::derive_key), so rotating it
    /// invalidates sessions and makes sealed secrets unreadable.
    pub secret_key_base: String,
    /// Path to a MaxMind-compatible city database. `None` disables city and
    /// region resolution; the product still works, with coarser geography.
    pub geoip_city_path: Option<String>,
    /// Path to a MaxMind-compatible ASN database. `None` disables ASN and
    /// datacenter detection, which weakens agent verification but does not
    /// break it, since UA patterns and rDNS still apply.
    pub geoip_asn_path: Option<String>,
    /// Days of event history to keep. `0` means forever, per
    /// `docs/CONTRACT.md` §1.9.
    pub retention_days: u32,
    /// Days of session-replay history to keep. Replay is far heavier per session
    /// than events, so it has its own, shorter, budget.
    pub replay_retention_days: u32,
    /// When true, only an existing admin can create accounts. The normal setting
    /// for a personal self-hosted install once the owner has signed up.
    pub disable_registration: bool,
    /// True for a self-hosted install. Gates nothing (there is no feature gate,
    /// per `PRODUCT.md`); it selects the admin surface and guarantees no
    /// outbound call is made at runtime.
    pub self_hosted: bool,
    /// SMTP URL for outgoing mail. `None` disables every email path: invitations
    /// fall back to copyable links, and scheduled reports do not run.
    pub smtp_url: Option<String>,
    /// `From` address on outgoing mail.
    pub mail_from: String,
    /// `text` for human-readable logs, `json` for structured ones.
    pub log_format: String,
    /// How many background jobs run at once in the embedded worker.
    pub worker_concurrency: usize,
}

impl Config {
    /// Read the configuration from the environment.
    ///
    /// Fails only when a required variable is missing or a value cannot be
    /// parsed. Every failure names the variable, because the reader of this
    /// error is an operator looking at a container that will not start.
    pub fn from_env() -> Result<Self> {
        let secret_key_base = required("SECRET_KEY_BASE")?;
        // Short of 32 bytes there is not enough entropy to derive keys from, and
        // an operator who typed "changeme" should find that out at boot rather
        // than after the first session cookie is forged.
        if secret_key_base.len() < 32 {
            return Err(Error::validation(
                "MICAFORGE_SECRET_KEY_BASE",
                "must be at least 32 characters; generate one with `openssl rand -base64 48`",
            ));
        }

        // Read by their own consumers (the rate limiter in the server's
        // middleware, the writer budget in `main`), and validated here so a typo
        // stops the boot with the variable's name instead of quietly becoming
        // the default.
        number::<u32>("RATE_LIMIT_PER_MIN", 0)?;
        writer_buffer_mb()?;

        Ok(Config {
            bind_addr: optional("BIND_ADDR").unwrap_or_else(|| "0.0.0.0:8080".to_string()),
            database_url: required("DATABASE_URL")?,
            clickhouse_url: required("CLICKHOUSE_URL")?,
            clickhouse_db: optional("CLICKHOUSE_DB").unwrap_or_else(|| "micaforge".to_string()),
            clickhouse_user: optional("CLICKHOUSE_USER").unwrap_or_else(|| "default".to_string()),
            // An empty password is the ClickHouse default and is legitimate on a
            // container network that is not exposed, so it is not required.
            clickhouse_password: optional("CLICKHOUSE_PASSWORD").unwrap_or_default(),
            redis_url: optional("REDIS_URL")
                .unwrap_or_else(|| "redis://127.0.0.1:6379".to_string()),
            public_url: trim_trailing_slash(
                optional("PUBLIC_URL").unwrap_or_else(|| "http://localhost:8080".to_string()),
            ),
            dashboard_url: trim_trailing_slash(
                optional("DASHBOARD_URL").unwrap_or_else(|| "http://localhost:4321".to_string()),
            ),
            secret_key_base,
            geoip_city_path: optional("GEOIP_CITY_PATH"),
            geoip_asn_path: optional("GEOIP_ASN_PATH"),
            retention_days: number("RETENTION_DAYS", 0)?,
            replay_retention_days: number("REPLAY_RETENTION_DAYS", 30)?,
            disable_registration: flag("DISABLE_REGISTRATION", false)?,
            self_hosted: flag("SELF_HOSTED", true)?,
            smtp_url: optional("SMTP_URL"),
            mail_from: optional("MAIL_FROM")
                .unwrap_or_else(|| "Micaforge <noreply@localhost>".to_string()),
            log_format: log_format_from_env()?.to_string(),
            worker_concurrency: number("WORKER_CONCURRENCY", default_concurrency())?,
        })
    }

    /// True when the session cookie carries `Secure`: either public origin is
    /// `https://`. The same rule the session cookie itself applies.
    pub fn cookies_are_secure(&self) -> bool {
        self.public_url.starts_with("https://") || self.dashboard_url.starts_with("https://")
    }

    /// The origin variables set to plain `http://` on a host other than this
    /// machine.
    ///
    /// `main` warns about these at boot when [`Config::cookies_are_secure`] is
    /// false: a session cookie without `Secure` on a real hostname travels in
    /// clear text, and nothing else would tell the operator.
    pub fn plain_http_origins(&self) -> Vec<&'static str> {
        [
            ("MICAFORGE_PUBLIC_URL", self.public_url.as_str()),
            ("MICAFORGE_DASHBOARD_URL", self.dashboard_url.as_str()),
        ]
        .into_iter()
        .filter(|(_, url)| is_plain_http_remote(url))
        .map(|(name, _)| name)
        .collect()
    }

    /// True when logs should be emitted as JSON.
    pub fn log_json(&self) -> bool {
        self.log_format == "json"
    }

    /// True when outgoing mail is configured. Callers use this to choose between
    /// sending an invitation and showing the operator a link to copy.
    pub fn mail_enabled(&self) -> bool {
        self.smtp_url.is_some()
    }

    /// A 32-byte key for one subsystem, derived from [`Config::secret_key_base`].
    ///
    /// Convenience wrapper so callers never handle the raw secret themselves.
    pub fn derive_key(&self, purpose: &str) -> [u8; 32] {
        crate::crypto::derive_key(&self.secret_key_base, purpose)
    }
}

/// `MICAFORGE_LOG_FORMAT`: `"text"` (the default) or `"json"`.
///
/// The one parser for this variable. The logger reads it before
/// [`Config::from_env`] runs, because the first thing the process might have to
/// say is that the configuration is wrong, and both readers have to agree on
/// what the value means. Case and surrounding space are ignored; any other
/// value is an error here, and the logger, which cannot fail, falls back to
/// text and lets [`Config::from_env`] report it.
pub fn log_format_from_env() -> Result<&'static str> {
    match optional("LOG_FORMAT") {
        None => Ok("text"),
        Some(raw) if raw.eq_ignore_ascii_case("json") => Ok("json"),
        Some(raw) if raw.eq_ignore_ascii_case("text") => Ok("text"),
        Some(_) => Err(Error::validation(
            "MICAFORGE_LOG_FORMAT",
            "must be `text` or `json`",
        )),
    }
}

/// `MICAFORGE_WRITER_BUFFER_MB`: how much memory each event-writer lane may
/// hold while ClickHouse is unavailable, in mebibytes.
///
/// `None` when unset, which means the writer's own default. Six lanes share
/// the process, so the worst case is six times this.
pub fn writer_buffer_mb() -> Result<Option<usize>> {
    /// A gigabyte per lane. More than that on the machine this product targets
    /// would trade an event-store outage for an out-of-memory kill.
    const MAX_MB: usize = 1024;

    match optional("WRITER_BUFFER_MB") {
        None => Ok(None),
        Some(raw) => match raw.parse::<usize>() {
            Ok(mb) if (1..=MAX_MB).contains(&mb) => Ok(Some(mb)),
            _ => Err(Error::validation(
                "MICAFORGE_WRITER_BUFFER_MB",
                format!("must be a whole number of mebibytes from 1 to {MAX_MB}"),
            )),
        },
    }
}

/// True for an `http://` URL whose host is not this machine.
fn is_plain_http_remote(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        // `[::1]:8080`
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    let host = host.to_ascii_lowercase();
    !(host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host.starts_with("127."))
}

/// Read `MICAFORGE_{key}`, treating whitespace-only as absent.
///
/// Compose files habitually pass `FOO=` for "unset", so an empty string has to
/// mean absent or every default in this file would be unreachable.
fn optional(key: &str) -> Option<String> {
    std::env::var(format!("MICAFORGE_{key}"))
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn required(key: &str) -> Result<String> {
    optional(key)
        .ok_or_else(|| Error::validation(format!("MICAFORGE_{key}"), "is required and was not set"))
}

fn number<T>(key: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
{
    match optional(key) {
        None => Ok(default),
        Some(raw) => raw
            .parse::<T>()
            .map_err(|_| Error::validation(format!("MICAFORGE_{key}"), "must be a whole number")),
    }
}

fn flag(key: &str, default: bool) -> Result<bool> {
    match optional(key) {
        None => Ok(default),
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(Error::validation(
                format!("MICAFORGE_{key}"),
                "must be one of: true, false, 1, 0, yes, no, on, off",
            )),
        },
    }
}

/// A trailing slash on an origin turns every joined path into a double slash,
/// which breaks share links and CORS matching in ways that are tedious to trace.
fn trim_trailing_slash(mut url: String) -> String {
    while url.ends_with('/') {
        url.pop();
    }
    url
}

/// Half the cores, floor 2, cap 16.
///
/// The worker shares the process with the ingest path, and ingest is the part
/// that must not stall, so background work does not get the whole machine by
/// default.
fn default_concurrency() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    (cores / 2).clamp(2, 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env vars are process-global, so the cases that touch them run as one test.
    #[test]
    fn reads_required_values_and_applies_defaults() {
        std::env::set_var("MICAFORGE_DATABASE_URL", "postgres://localhost/micaforge");
        std::env::set_var("MICAFORGE_CLICKHOUSE_URL", "http://localhost:8123");
        std::env::set_var("MICAFORGE_SECRET_KEY_BASE", "x".repeat(48));
        std::env::set_var("MICAFORGE_PUBLIC_URL", "https://mf.example.com/");
        std::env::set_var("MICAFORGE_CLICKHOUSE_PASSWORD", "  ");

        let cfg = Config::from_env().expect("config with the three required values must load");
        assert_eq!(cfg.bind_addr, "0.0.0.0:8080");
        assert_eq!(cfg.clickhouse_db, "micaforge");
        assert_eq!(cfg.retention_days, 0, "0 means keep forever");
        assert_eq!(cfg.replay_retention_days, 30);
        assert!(cfg.self_hosted);
        assert!(!cfg.mail_enabled());
        assert!(!cfg.log_json());
        assert_eq!(
            cfg.public_url, "https://mf.example.com",
            "a trailing slash must be trimmed"
        );
        assert_eq!(cfg.clickhouse_password, "", "a blank value counts as unset");
        assert!(cfg.worker_concurrency >= 2);

        // A short secret is refused, and the error names the variable.
        std::env::set_var("MICAFORGE_SECRET_KEY_BASE", "changeme");
        let err = Config::from_env().unwrap_err();
        assert_eq!(err.field(), Some("MICAFORGE_SECRET_KEY_BASE"));

        // A missing required value is refused by name too.
        std::env::set_var("MICAFORGE_SECRET_KEY_BASE", "x".repeat(48));
        std::env::remove_var("MICAFORGE_DATABASE_URL");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_DATABASE_URL")
        );

        // A malformed number is refused rather than silently defaulted.
        std::env::set_var("MICAFORGE_DATABASE_URL", "postgres://localhost/micaforge");
        std::env::set_var("MICAFORGE_RETENTION_DAYS", "forever");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_RETENTION_DAYS")
        );

        std::env::remove_var("MICAFORGE_RETENTION_DAYS");

        // The log format has one reading, whoever asks.
        std::env::set_var("MICAFORGE_LOG_FORMAT", " JSON ");
        assert_eq!(log_format_from_env().unwrap(), "json");
        assert!(Config::from_env().unwrap().log_json());
        std::env::set_var("MICAFORGE_LOG_FORMAT", "yaml");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_LOG_FORMAT")
        );
        std::env::remove_var("MICAFORGE_LOG_FORMAT");

        // A rate limit that would silently become the default is refused.
        std::env::set_var("MICAFORGE_RATE_LIMIT_PER_MIN", "lots");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_RATE_LIMIT_PER_MIN")
        );
        std::env::set_var("MICAFORGE_RATE_LIMIT_PER_MIN", "0");
        assert!(Config::from_env().is_ok(), "0 switches the limiter off");
        std::env::remove_var("MICAFORGE_RATE_LIMIT_PER_MIN");

        // So is a writer budget outside its range.
        assert_eq!(writer_buffer_mb().unwrap(), None);
        std::env::set_var("MICAFORGE_WRITER_BUFFER_MB", "0");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_WRITER_BUFFER_MB")
        );
        std::env::set_var("MICAFORGE_WRITER_BUFFER_MB", "64");
        assert_eq!(writer_buffer_mb().unwrap(), Some(64));

        for key in [
            "MICAFORGE_DATABASE_URL",
            "MICAFORGE_CLICKHOUSE_URL",
            "MICAFORGE_SECRET_KEY_BASE",
            "MICAFORGE_PUBLIC_URL",
            "MICAFORGE_CLICKHOUSE_PASSWORD",
            "MICAFORGE_WRITER_BUFFER_MB",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn plain_http_is_flagged_only_off_this_machine() {
        assert!(is_plain_http_remote("http://analytics.example.com"));
        assert!(is_plain_http_remote("http://203.0.113.7:8080/path"));
        assert!(!is_plain_http_remote("http://localhost:8080"));
        assert!(!is_plain_http_remote("http://127.0.0.1:4321"));
        assert!(!is_plain_http_remote("http://[::1]:8080"));
        assert!(!is_plain_http_remote("http://app.localhost"));
        assert!(!is_plain_http_remote("https://analytics.example.com"));
    }
}
