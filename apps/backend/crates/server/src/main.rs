//! The Micaforge server process.
//!
//! Boot order matters and is not arbitrary. Each step below either has to happen
//! before the next one can, or has to happen before the process starts accepting
//! traffic:
//!
//! 1. **Logging**, before anything that can fail. A configuration error read out
//!    of a container that would not start is the most common first experience a
//!    self-hoster has, and it has to be legible.
//! 2. **Configuration**, which fails fast and names the variable.
//! 3. **Postgres, then its migrations.** The schema is compiled into this
//!    binary, so `docker compose up` migrates itself. Nothing else can run until
//!    it has.
//! 4. **ClickHouse, then its schema and retention.** Tables are created and
//!    healed on every boot, so an upgrade that adds a column needs no operator
//!    action.
//! 5. **Redis**, which is where the rotating visitor salt lives, so ingest
//!    cannot attribute a hit without it.
//! 6. **The GeoIP readers**, which are optional: without them the product
//!    reports coarser geography and keeps working.
//! 7. **The writer, the worker and the range reloader**, all background tasks.
//! 8. **The listener**, last, so the process never accepts a request it cannot
//!    serve.
//!
//! Shutdown runs in reverse and is the half that is easy to get wrong: the
//! listener stops accepting, in-flight requests finish, and only then is the
//! event writer drained. Dropping the writer while a `POST /api/track` is still
//! queueing rows would silently lose the last few seconds of every site's
//! traffic on every deploy.
//!
//! Every step of it is bounded, because the container runtime is not patient:
//! compose sends `SIGKILL` at the end of `stop_grace_period` (45 s), and a kill
//! loses whatever the writer still holds. In-flight requests get
//! [`HTTP_DRAIN`]; then the writer ([`WRITER_DRAIN`]) and the worker
//! ([`micaforge_jobs::worker::DRAIN_TIMEOUT`]) drain side by side. The worst
//! case is about thirty seconds, and [`SHUTDOWN_LIMIT`] is a last-resort exit
//! that still lands inside the grace period if something unforeseen hangs.

use std::future::IntoFuture;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ipnet::IpNet;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use micaforge_agents::verify::RangeStore;
use micaforge_core::{Config, Error, Result};
use micaforge_enrich::Geo;
use micaforge_jobs::{refresh_agent_ranges, JobCtx, Worker};
use micaforge_mail::Mailer;
use micaforge_server::state::AppState;
use micaforge_store::models::catalog;
use micaforge_store::{ch::Ch, kv::Kv, pg, EventWriter, WriterCfg};

/// How long in-flight requests are given after the listener stops accepting.
///
/// Ten seconds is longer than any ordinary request this server serves. A long
/// upload still running at the deadline is cut off: holding the process open
/// for it would push the writer drain past the container's grace period, and
/// the writer's rows matter more than one upload that can be retried.
const HTTP_DRAIN: Duration = Duration::from_secs(10);

/// How long the event writer keeps trying to write its buffer at shutdown.
const WRITER_DRAIN: Duration = Duration::from_secs(15);

/// The hard ceiling from the shutdown signal to process exit.
///
/// Forty seconds: the bounded steps above finish in about thirty, and compose
/// waits forty-five (`stop_grace_period`) before it kills. Exiting here first
/// means the last log line says what was still running, instead of nothing.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(40);

/// How often the in-memory agent ranges are compared against Postgres.
const RANGE_POLL: Duration = Duration::from_secs(10 * 60);

/// Reload the ranges unconditionally at least this often.
///
/// The poll normally reloads only when Redis says the refresh job has run. This
/// is the floor for an install whose Redis was flushed, or whose hint expired
/// because the job has not run in a day.
const RANGE_MAX_AGE: Duration = Duration::from_secs(6 * 60 * 60);

/// What `micaforge-server --help` prints.
const USAGE: &str = "\
usage: micaforge-server                      run the server (the container's default)
       micaforge-server reset-link <email>   print a password-reset link for an account

reset-link is for an install with no SMTP relay, where a reset email is never
sent. Run it inside the container, where the configuration already is:

  docker compose exec micaforge micaforge-server reset-link you@example.com

The link works once and expires in an hour.";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let outcome = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => run().await,
        ["reset-link", email] => reset_link(email).await,
        ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            return;
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };

    // A failure here is an operator's problem to fix, not a stack trace's, so
    // every exit path prints one sentence and stops.
    if let Err(e) = outcome {
        tracing::error!(error = %e, "micaforge could not start");
        eprintln!("micaforge: {e}");
        std::process::exit(1);
    }
}

/// `micaforge-server reset-link <email>`: mint a reset token and print its link.
///
/// The same token `POST /api/auth/forgot-password` would mail. It reaches the
/// two stores it needs and nothing else: no migration, no listener, no worker,
/// so it is safe to run beside the server that is already up. Whoever can run
/// it already holds the machine and its `.env`, which is the strongest proof of
/// authority this install has.
async fn reset_link(email: &str) -> Result<()> {
    let _ = dotenvy::dotenv();
    let cfg = Config::from_env()?;
    let pg = pg::connect(&cfg.database_url).await?;
    let kv = Kv::connect(&cfg.redis_url).await?;

    let email = email.trim().to_lowercase();
    match micaforge_server::routes::auth::issue_reset_token(&pg, &kv, &email).await? {
        Some((_, token)) => {
            println!(
                "{}",
                micaforge_mail::templates::password_reset_link(&cfg.dashboard_url, &token)
            );
            Ok(())
        }
        None => {
            eprintln!("micaforge: no account has the address {email}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<()> {
    // A `.env` beside the binary is a development convenience. In a container
    // every value comes from the environment and this finds nothing, which is
    // why the failure is discarded rather than reported.
    let _ = dotenvy::dotenv();

    init_tracing();

    let cfg = Arc::new(Config::from_env()?);
    tracing::info!(
        version = micaforge_server::VERSION,
        self_hosted = cfg.self_hosted,
        bind = %cfg.bind_addr,
        "micaforge starting"
    );

    let insecure = cfg.plain_http_origins();
    if !cfg.cookies_are_secure() && !insecure.is_empty() {
        tracing::warn!(
            variables = ?insecure,
            "the public origin is plain http on a real hostname, so session cookies \
             will not be marked Secure and travel unencrypted; serve the dashboard \
             and API over https"
        );
    }

    // 3. Postgres, and the schema this binary carries.
    let pg = pg::connect(&cfg.database_url).await?;
    pg::migrate(&pg).await?;
    tracing::info!("postgres ready");

    // 4. ClickHouse. `init_schema` creates the database and every table and
    // heals a table that is missing a column, so an upgrade is a restart.
    let ch = Ch::connect(
        &cfg.clickhouse_url,
        &cfg.clickhouse_db,
        &cfg.clickhouse_user,
        &cfg.clickhouse_password,
    )?;
    ch.init_schema().await?;
    ch.apply_retention(cfg.retention_days, cfg.replay_retention_days)
        .await?;
    tracing::info!(
        db = %cfg.clickhouse_db,
        retention_days = cfg.retention_days,
        replay_retention_days = cfg.replay_retention_days,
        "clickhouse ready"
    );

    // 5. Redis.
    let kv = Kv::connect(&cfg.redis_url).await?;
    tracing::info!("redis ready");

    // 6. Geography. Optional by design: `PRODUCT.md` does not make a MaxMind
    // licence a condition of running the product, so an install with no database
    // resolves country from nothing and says so rather than refusing to boot.
    let geo = Arc::new(Geo::open(
        cfg.geoip_city_path.as_deref(),
        cfg.geoip_asn_path.as_deref(),
    ));
    if geo.is_loaded() {
        tracing::info!("geoip databases loaded");
    } else {
        tracing::warn!(
            "no geoip database found; country, region, city and ASN will be empty. \
             Set MICAFORGE_GEOIP_CITY_PATH and MICAFORGE_GEOIP_ASN_PATH to enable them"
        );
    }

    // The one outbound client, shared by the policy crawls, the webhooks and the
    // range refresh. Built with the crawl crate's user agent and timeouts.
    let http = micaforge_jobs::fetch::client()?;

    // Two mailers from one configuration: `Mailer` holds an SMTP transport and
    // is not `Clone`, and the worker needs an owned one for its `JobCtx` while
    // the request path needs a shared one. Constructing it is parsing a URL.
    let mailer = Arc::new(build_mailer(&cfg)?);
    let worker_mailer = build_mailer(&cfg)?;
    if !cfg.mail_enabled() {
        tracing::warn!(
            "no MICAFORGE_SMTP_URL; no mail is sent and no link is logged: invitation links \
             are copied from the dashboard, a password reset comes from \
             `micaforge-server reset-link <email>`, and scheduled reports will not run"
        );
    }

    // 7a. The event writer. Every collection route pushes onto it and returns.
    let mut writer_cfg = WriterCfg::default();
    if let Some(mb) = micaforge_core::config::writer_buffer_mb()? {
        writer_cfg = writer_cfg.with_buffer_mb(mb);
    }
    tracing::info!(
        buffer_mb_per_table = writer_cfg.max_buffer_bytes / (1024 * 1024),
        "event writer started"
    );
    let writer = EventWriter::spawn(ch.clone(), writer_cfg);

    // 7b. Agent verification ranges, loaded from the rows the refresh job wrote.
    // Reading Postgres rather than the operators' endpoints keeps boot offline:
    // a first install verifies nothing until the job has run once, which reports
    // as `unverified` and is an honest answer.
    let ranges = Arc::new(RwLock::new(RangeStore::new()));
    let loaded = reload_ranges(&pg, &ranges).await;
    tracing::info!(agents = loaded, "agent ranges loaded");

    // 7c. The job queue: seed the recurring rows, then start the lanes.
    micaforge_jobs::schedule_recurring(&pg).await?;
    let worker = Worker::spawn(
        JobCtx {
            pg: pg.clone(),
            ch: ch.clone(),
            kv: kv.clone(),
            mailer: worker_mailer,
            http: http.clone(),
            cfg: Arc::clone(&cfg),
        },
        cfg.worker_concurrency,
    );

    let state = AppState {
        cfg: Arc::clone(&cfg),
        pg: pg.clone(),
        ch,
        kv: kv.clone(),
        writer: writer.clone(),
        geo,
        ranges: Arc::clone(&ranges),
        mailer,
        http,
    };

    let ranges_task = tokio::spawn(watch_ranges(pg, kv, Arc::clone(&ranges)));

    // 8. The listener. `into_make_service_with_connect_info` is not optional:
    // the ingest path reads the socket address as its last-resort client
    // address, and without it every hit from an install with no proxy would
    // hash to the same visitor.
    // Layers, innermost first. `.layer` wraps what came before it, so the last
    // call here is the outermost and sees the request first.
    //
    // The order is the point. The panic catcher is outermost, so a panic in any
    // other layer is still an answer rather than a dropped connection. The
    // access log sits inside it and outside the rate limiter, so a refused
    // request is logged with its `429` instead of vanishing. The security
    // headers are set on the way out, on every response including a refusal.
    //
    // The liveness and readiness probes are merged after the rate limiter, so
    // it does not wrap them: its counter lives in Valkey, and a liveness answer
    // must not depend on a store. They still get the headers, the access log
    // and the panic catcher.
    let probes = micaforge_server::routes::health::probes().with_state(state.clone());
    let app = micaforge_server::routes::router(&cfg)
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state,
            micaforge_server::middleware::rate_limit,
        ))
        .merge(probes)
        .layer(micaforge_server::middleware::no_sniff())
        .layer(micaforge_server::middleware::no_frame())
        .layer(micaforge_server::middleware::referrer_policy())
        .layer(micaforge_server::middleware::permissions_policy())
        .layer(axum::middleware::from_fn(
            micaforge_server::middleware::trace,
        ))
        .layer(micaforge_server::middleware::catch_panic());

    let listener = TcpListener::bind(&cfg.bind_addr)
        .await
        .map_err(|e| Error::Internal(format!("could not bind {}: {e}", cfg.bind_addr)))?;

    tracing::info!(
        addr = %cfg.bind_addr,
        public_url = %cfg.public_url,
        dashboard_url = %cfg.dashboard_url,
        "listening"
    );

    // The signal starts the graceful shutdown, arms the last-resort exit, and
    // starts the HTTP drain clock.
    let (stopping, stopped_accepting) = tokio::sync::oneshot::channel::<()>();
    let signal = async move {
        shutdown_signal().await;
        arm_shutdown_limit();
        let _ = stopping.send(());
    };

    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(signal)
    .into_future();
    let mut server = Box::pin(server);

    let http_deadline = async {
        if stopped_accepting.await.is_err() {
            // The server ended on its own; its branch has the answer.
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(HTTP_DRAIN).await;
    };

    tokio::select! {
        result = &mut server => {
            result.map_err(|e| Error::Internal(format!("the server stopped: {e}")))?;
        }
        () = http_deadline => {
            // Dropping the server future below stops waiting. Connections
            // still open are abandoned and end with the process.
            tracing::warn!(
                seconds = HTTP_DRAIN.as_secs(),
                "requests were still in flight at the drain deadline; they are cut off"
            );
        }
    }
    drop(server);

    // The listener has stopped accepting and in-flight requests have finished
    // or been cut off. Now the background work is wound down.
    tracing::info!("draining");
    ranges_task.abort();

    // The writer holds rows no other process can recover, so it keeps
    // retrying until its own deadline. The worker stops claiming and waits for
    // the jobs in hand. They are independent (no job writes through the event
    // writer), so they drain side by side.
    let (lost, worker_stopped) = tokio::join!(
        writer.shutdown(WRITER_DRAIN),
        worker.shutdown(micaforge_jobs::worker::DRAIN_TIMEOUT),
    );

    if lost == 0 {
        tracing::info!("event writer drained");
    } else {
        tracing::error!(
            rows = lost,
            "the event writer could not write every buffered row before its deadline"
        );
    }
    if !worker_stopped {
        // A job that outlives this is not lost: its row stays `running`, its
        // heartbeat stops with the process, and a janitor reclaims it.
        tracing::warn!("a job was still running at shutdown; it will be reclaimed");
    }

    tracing::info!("stopped");
    Ok(())
}

/// Exit the process if shutdown has not finished within [`SHUTDOWN_LIMIT`].
///
/// Every drain step is bounded on its own, so this should never fire. It is
/// here so that something unforeseen cannot hold the process until the
/// container runtime kills it without a word.
fn arm_shutdown_limit() {
    tokio::spawn(async {
        tokio::time::sleep(SHUTDOWN_LIMIT).await;
        tracing::error!(
            seconds = SHUTDOWN_LIMIT.as_secs(),
            "shutdown did not finish in time; exiting now"
        );
        eprintln!("micaforge: shutdown did not finish in time; exiting");
        std::process::exit(1);
    });
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Start the subscriber.
///
/// Deliberately before [`Config::from_env`]: the first thing this process might
/// have to say is that the configuration is wrong, and it has to be able to say
/// it in whatever format the operator's log pipeline is expecting. The format
/// is read through [`micaforge_core::config::log_format_from_env`], the same
/// parser the configuration uses, so the two can never disagree; an invalid
/// value logs as text here and is then reported by name by `Config::from_env`.
///
/// `RUST_LOG` wins when it is set. The default is `info` everywhere: `debug`
/// from this product's crates is a line per request and per job, which is a
/// level to opt into while investigating, not one to ship.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let json = matches!(micaforge_core::config::log_format_from_env(), Ok("json"));

    let registry = tracing_subscriber::registry().with(filter);
    if json {
        registry
            .with(tracing_subscriber::fmt::layer().json().flatten_event(true))
            .init();
    } else {
        registry
            .with(tracing_subscriber::fmt::layer().with_target(true))
            .init();
    }
}

// ---------------------------------------------------------------------------
// Mail
// ---------------------------------------------------------------------------

/// Build a mailer from the configuration.
///
/// Links in mail point at the **dashboard** origin, never the API origin: every
/// link in every message lands a person in the app, and a verification link that
/// opened a JSON endpoint would be a broken product rather than a broken URL.
fn build_mailer(cfg: &Config) -> Result<Mailer> {
    Mailer::new(cfg.smtp_url.as_deref(), &cfg.mail_from, &cfg.dashboard_url)
}

// ---------------------------------------------------------------------------
// Agent verification ranges
// ---------------------------------------------------------------------------

/// Fill a [`RangeStore`] from `agent_catalog.ip_ranges`.
///
/// Returns how many agents had at least one usable prefix. A row whose ranges do
/// not parse is skipped with a warning rather than failing the reload: one
/// operator publishing something odd must not cost every other operator their
/// verification.
async fn reload_ranges(pg: &micaforge_store::PgPool, into: &RwLock<RangeStore>) -> usize {
    let rows = match catalog::list_agents(pg).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "could not read the agent catalog; verification stays unknown");
            return 0;
        }
    };

    let mut store = RangeStore::new();
    let mut filled = 0usize;
    for row in rows {
        let nets: Vec<IpNet> = row
            .ip_ranges
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .filter_map(|cidr| match cidr.parse::<IpNet>() {
                        Ok(net) => Some(net),
                        Err(_) => {
                            tracing::warn!(agent = %row.slug, cidr, "unparseable published range");
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        if !nets.is_empty() {
            filled += 1;
            store.insert(&row.slug, nets);
        }
    }

    *into.write().await = store;
    filled
}

/// Keep the in-memory ranges in step with the ones the refresh job writes.
///
/// The job stamps `mf:agents:ranges:refreshed_at` in Redis when it finishes, so
/// this only reads Postgres when that stamp has moved. It is a hint and nothing
/// depends on it: a cold Redis costs one late reload, which
/// [`RANGE_MAX_AGE`] bounds.
async fn watch_ranges(pg: micaforge_store::PgPool, kv: Kv, ranges: Arc<RwLock<RangeStore>>) {
    let mut last_seen: Option<DateTime<Utc>> = kv
        .get_json::<DateTime<Utc>>(refresh_agent_ranges::REFRESHED_AT_KEY)
        .await
        .ok()
        .flatten();
    let mut since_reload = Duration::ZERO;

    loop {
        tokio::time::sleep(RANGE_POLL).await;
        since_reload += RANGE_POLL;

        let stamp = kv
            .get_json::<DateTime<Utc>>(refresh_agent_ranges::REFRESHED_AT_KEY)
            .await
            .ok()
            .flatten();

        let moved = stamp.is_some() && stamp != last_seen;
        if !moved && since_reload < RANGE_MAX_AGE {
            continue;
        }

        let filled = reload_ranges(&pg, &ranges).await;
        tracing::info!(agents = filled, "agent ranges reloaded");
        last_seen = stamp;
        since_reload = Duration::ZERO;
    }
}

// ---------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------

/// Resolve on `SIGINT` or `SIGTERM`.
///
/// Both, because they arrive from different places and mean the same thing:
/// `SIGINT` from a terminal, `SIGTERM` from Docker, Kubernetes and systemd. A
/// process that only handled `SIGINT` would be killed rather than drained on
/// every single deploy.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "could not listen for ctrl-c");
            // Never resolving is correct: a broken signal handler must not look
            // like a shutdown request.
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "could not listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("SIGINT received"),
        () = terminate => tracing::info!("SIGTERM received"),
    }
}
