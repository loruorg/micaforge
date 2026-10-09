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
    /// Most sites one organisation may hold. `0` means no limit, the normal
    /// setting for a self-hosted install. A shared install that lets anybody
    /// sign up sets it, so one account cannot point unbounded traffic at it.
    /// Instance admins are not limited.
    pub max_sites_per_org: u32,
    /// Most organisations one account may own. `0` means no limit. Paired with
    /// [`Config::max_sites_per_org`], because a site limit alone is undone by
    /// creating another organisation.
    pub max_orgs_per_user: u32,
    /// True for a self-hosted install. Gates nothing (there is no feature gate,
    /// per `PRODUCT.md`); it selects the admin surface and guarantees no
    /// outbound call is made at runtime.
    pub self_hosted: bool,
    /// SMTP URL for outgoing mail. Used when the Cloudflare pair below is not
    /// set. With neither, no mail leaves the process: sign-in codes come from
    /// `micaforge-server sign-in-code`, invitations fall back to copyable
    /// links, and scheduled reports do not run. See [`Config::mail_transport`].
    pub smtp_url: Option<String>,
    /// API token for Cloudflare Email Sending. Set together with
    /// [`Config::cloudflare_account_id`] or not at all; [`Config::from_env`]
    /// refuses one without the other. When both are set, mail goes through
    /// Cloudflare's HTTP API and `smtp_url` is ignored.
    pub cloudflare_email_token: Option<String>,
    /// The Cloudflare account the token sends mail for.
    pub cloudflare_account_id: Option<String>,
    /// `From` address on outgoing mail.
    pub mail_from: String,
    /// `text` for human-readable logs, `json` for structured ones.
    pub log_format: String,
    /// How many background jobs run at once in the embedded worker.
    pub worker_concurrency: usize,

    // -- AI analyst, chat apps and feedback ------------------------------------
    //
    // Every one of these is off until its variables are set, because a
    // self-hosted install must make no outbound call it was not configured to
    // make. Each group is all or nothing, and half a group stops the boot with
    // the missing variable named, the same rule the Cloudflare mail pair keeps.
    /// The OpenAI-compatible model behind the AI analyst, the investigations'
    /// explanations, the MCP `ask` tool and the chat apps. `None` switches every
    /// AI feature off, and the dashboard and the API say so.
    pub llm: Option<LlmConfig>,
    /// Instance-level credentials for the Slack, Discord and Telegram apps.
    pub chat: ChatApps,
    /// Where new feedback is announced besides the instance admins' inboxes.
    pub feedback_channel: FeedbackChannel,

    // -- Monitoring and growth: uptime checks, status pages, short links --
    /// The probe region label written on every uptime check
    /// (`MICAFORGE_REGION`). One install checks from one place, so this is a
    /// name for that place rather than a choice of where to check from; it is
    /// stored per check so that results from two installs, or a later
    /// multi-region one, can be told apart.
    pub region: String,
    /// A hostname that serves short links at its root (`MICAFORGE_LINKS_HOST`),
    /// lower-cased and without a scheme. `None` means short links are served
    /// only at `<public_url>/l/<slug>`.
    pub links_host: Option<String>,

    // -- Sign-in with Google and GitHub, and billing --------------------------
    //
    // Every one of these is off unless configured: a self-hosted install with
    // none of them set makes no call to Google, GitHub or Stripe, ever.
    /// OAuth client for "Continue with Google" (`MICAFORGE_GOOGLE_CLIENT_ID`
    /// and `_SECRET`, both or neither). See [`Config::oauth_client`].
    pub google_oauth: Option<OAuthClient>,
    /// OAuth client for "Continue with GitHub" (`MICAFORGE_GITHUB_CLIENT_ID`
    /// and `_SECRET`, both or neither).
    pub github_oauth: Option<OAuthClient>,
    /// `MICAFORGE_BILLING_ENABLED`: plans, quotas and usage emails apply.
    /// Off by default, and meant for the hosted service only.
    pub billing_enabled: bool,
    /// `MICAFORGE_BILLING_PLANS`, parsed. Empty when billing is off.
    pub billing_plans: Vec<BillingPlan>,
    /// Stripe, when `MICAFORGE_STRIPE_SECRET_KEY` and
    /// `MICAFORGE_STRIPE_WEBHOOK_SECRET` are both set. Used only while billing
    /// is on.
    pub stripe: Option<StripeKeys>,

    // -- Google Search Console --------------------------------------------
    //
    // Off unless configured, like the sign-in providers above: with neither
    // variable set, no site can connect and no call is made to Google.
    /// The OAuth client a site admin connects Google Search Console through
    /// (`MICAFORGE_SEARCH_CONSOLE_CLIENT_ID` and `_SECRET`, both or neither).
    ///
    /// A client of its own rather than [`Config::google_oauth`]: the sign-in
    /// client asks only for `openid email profile`, which Google treats as
    /// non-sensitive and never reviews, while Search Console needs
    /// `webmasters.readonly`, a sensitive scope that puts the whole consent
    /// screen it is added to through Google's verification. Keeping the two
    /// apart leaves sign-in working while that review runs, lets an install
    /// offer one without the other, and keeps a sign-in identity and a stored
    /// Search Console grant from ever sharing a credential. An operator who
    /// prefers one Google client may paste the same id and secret into both
    /// pairs and register both redirect addresses on it.
    pub search_console: Option<OAuthClient>,
    // -- GitHub App: deploy annotations and feedback issues -------------------
    //
    // Off until all six variables are set, so an install without them never
    // calls GitHub for this. Separate from `github_oauth`, the OAuth app behind
    // "Continue with GitHub": a GitHub App is another kind of registration,
    // with its own id, private key, webhook and client, and an install may run
    // either one without the other.
    /// The GitHub App, when `MICAFORGE_GITHUB_APP_*` is set.
    pub github_app: Option<GitHubApp>,
}

/// The model the AI features talk to.
///
/// Any endpoint that speaks the OpenAI `chat/completions` dialect with tool
/// calling will do: a hosted API, or a local server on the same network. The
/// key is optional because a local server usually takes none.
#[derive(Clone, PartialEq, Eq)]
pub struct LlmConfig {
    /// `MICAFORGE_LLM_BASE_URL`, without a trailing slash. Requests go to
    /// `{base_url}/chat/completions`.
    pub base_url: String,
    /// `MICAFORGE_LLM_MODEL`.
    pub model: String,
    /// `MICAFORGE_LLM_API_KEY`, sent as a bearer token when set.
    pub api_key: Option<String>,
    /// `MICAFORGE_LLM_TIMEOUT_SECS`: the ceiling on one model call.
    pub timeout_secs: u64,
    /// `MICAFORGE_LLM_DAILY_LIMIT`: questions per organisation per UTC day,
    /// across every surface. `0` means no limit.
    pub daily_limit: u32,
}

impl std::fmt::Debug for LlmConfig {
    /// Hand-written so the key can never reach a log line through `{:?}`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[set]"))
            .field("timeout_secs", &self.timeout_secs)
            .field("daily_limit", &self.daily_limit)
            .finish()
    }
}

/// The three chat platforms, each configured or not.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ChatApps {
    /// The Slack app, when `MICAFORGE_SLACK_*` is set.
    pub slack: Option<SlackApp>,
    /// The Discord app, when `MICAFORGE_DISCORD_*` is set.
    pub discord: Option<DiscordApp>,
    /// The Telegram bot, when `MICAFORGE_TELEGRAM_*` is set.
    pub telegram: Option<TelegramApp>,
}

impl std::fmt::Debug for ChatApps {
    /// Says which platforms are on and nothing else: every field below them is
    /// a credential.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatApps")
            .field("slack", &self.slack.is_some())
            .field("discord", &self.discord.is_some())
            .field("telegram", &self.telegram.is_some())
            .finish()
    }
}

/// The Slack app's credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct SlackApp {
    /// Verifies that a request came from Slack (`X-Slack-Signature`).
    pub signing_secret: String,
    /// `xoxb-…`, for `chat.postMessage`.
    pub bot_token: String,
}

/// The Discord application's credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct DiscordApp {
    /// The application (client) id, for command registration and follow-ups.
    pub application_id: String,
    /// The hex Ed25519 public key that signs every interaction.
    pub public_key: String,
    /// The bot token, for posting investigations to a channel.
    pub bot_token: String,
}

/// The Telegram bot's credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct TelegramApp {
    /// The token BotFather issued.
    pub bot_token: String,
    /// The `secret_token` given to `setWebhook`, echoed by Telegram in
    /// `X-Telegram-Bot-Api-Secret-Token` on every update.
    pub webhook_secret: String,
}

/// The optional instance feedback channel.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct FeedbackChannel {
    /// `MICAFORGE_FEEDBACK_WEBHOOK_URL`: a Slack-compatible or Discord webhook.
    pub webhook_url: Option<String>,
    /// `MICAFORGE_FEEDBACK_TELEGRAM_CHAT_ID`: a chat the instance bot posts to.
    pub telegram_chat_id: Option<String>,
}

impl std::fmt::Debug for FeedbackChannel {
    /// A webhook URL carries its secret in the path, so only presence is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedbackChannel")
            .field("webhook_url", &self.webhook_url.is_some())
            .field("telegram_chat_id", &self.telegram_chat_id.is_some())
            .finish()
    }
}

/// One OAuth application's credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthClient {
    /// The public client id.
    pub client_id: String,
    /// The client secret. Never logged: see the `Debug` impl.
    pub client_secret: String,
}

impl std::fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClient")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// The two Stripe secrets.
#[derive(Clone, PartialEq, Eq)]
pub struct StripeKeys {
    /// The API key (`sk_...`), sent as a bearer token to Stripe.
    pub secret_key: String,
    /// The webhook signing secret (`whsec_...`), the HMAC key of
    /// `Stripe-Signature`.
    pub webhook_secret: String,
}

impl std::fmt::Debug for StripeKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StripeKeys")
    }
}

/// The GitHub App's registration, as `MICAFORGE_GITHUB_APP_*` gives it.
///
/// Six values because a GitHub App is three credentials in one: the app itself
/// (its id and private key sign the JWT that reads an installation and mints
/// its tokens), the webhook (its secret signs every delivery), and an OAuth
/// client (its id and secret prove, during an install, that the person coming
/// back from GitHub really can reach the installation they name).
#[derive(Clone, PartialEq, Eq)]
pub struct GitHubApp {
    /// `MICAFORGE_GITHUB_APP_ID`: the numeric App ID, the `iss` of every app JWT.
    pub app_id: u64,
    /// `MICAFORGE_GITHUB_APP_SLUG`: the app's name in its public address,
    /// `github.com/apps/<slug>`, where an install starts.
    pub slug: String,
    /// `MICAFORGE_GITHUB_APP_PRIVATE_KEY`: the PEM key GitHub generated, with
    /// literal `\n` sequences turned back into line breaks.
    pub private_key: String,
    /// `MICAFORGE_GITHUB_APP_WEBHOOK_SECRET`: the HMAC key of
    /// `X-Hub-Signature-256`.
    pub webhook_secret: String,
    /// `MICAFORGE_GITHUB_APP_CLIENT_ID`: the app's own OAuth client id (not the
    /// sign-in app's `MICAFORGE_GITHUB_CLIENT_ID`).
    pub client_id: String,
    /// `MICAFORGE_GITHUB_APP_CLIENT_SECRET`.
    pub client_secret: String,
}

impl std::fmt::Debug for GitHubApp {
    /// The id and the slug are public; the other four are credentials.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubApp")
            .field("app_id", &self.app_id)
            .field("slug", &self.slug)
            .finish_non_exhaustive()
    }
}

/// A plan, as `MICAFORGE_BILLING_PLANS` declares it.
///
/// `[{"id":"free","name":"Free","events":10000,"sites":2,"members":3},
///   {"id":"team","name":"Team","events":1000000,"sites":20,"members":25,
///    "price_id":"price_123","price_label":"$19 a month"}]`
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BillingPlan {
    /// Stable id, stored on the organisation: 1 to 32 of `[a-z0-9_-]`.
    pub id: String,
    /// What the Billing screen calls it.
    pub name: String,
    /// Monthly event quota. `null` means no quota.
    #[serde(default)]
    pub events: Option<u64>,
    /// Most sites an organisation on this plan may hold. `null` means no limit.
    #[serde(default)]
    pub sites: Option<u32>,
    /// Most members (invitations included). `null` means no limit.
    #[serde(default)]
    pub members: Option<u32>,
    /// The Stripe price that buys it. A plan with none is the free plan an
    /// organisation is on before it pays; exactly one plan must have none.
    #[serde(default)]
    pub price_id: Option<String>,
    /// The price as the Billing screen prints it ("$19 a month"). Stripe is
    /// not asked: the screen must work with Stripe unreachable.
    #[serde(default)]
    pub price_label: Option<String>,
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

        // Half a Cloudflare configuration is a typo, not a choice. Falling back
        // to SMTP or to no mail at all would leave an operator wondering why
        // the codes they configured a relay for never arrive, so the boot stops
        // and names the variable that is missing.
        let cloudflare_email_token = optional("CLOUDFLARE_EMAIL_TOKEN");
        let cloudflare_account_id = optional("CLOUDFLARE_ACCOUNT_ID");
        match (&cloudflare_email_token, &cloudflare_account_id) {
            (Some(_), None) => {
                return Err(Error::validation(
                    "MICAFORGE_CLOUDFLARE_ACCOUNT_ID",
                    "is required when MICAFORGE_CLOUDFLARE_EMAIL_TOKEN is set",
                ))
            }
            (None, Some(_)) => {
                return Err(Error::validation(
                    "MICAFORGE_CLOUDFLARE_EMAIL_TOKEN",
                    "is required when MICAFORGE_CLOUDFLARE_ACCOUNT_ID is set",
                ))
            }
            _ => {}
        }

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
            max_sites_per_org: number("MAX_SITES_PER_ORG", 0)?,
            max_orgs_per_user: number("MAX_ORGS_PER_USER", 0)?,
            self_hosted: flag("SELF_HOSTED", true)?,
            smtp_url: optional("SMTP_URL"),
            cloudflare_email_token,
            cloudflare_account_id,
            mail_from: optional("MAIL_FROM")
                .unwrap_or_else(|| "Micaforge <noreply@localhost>".to_string()),
            log_format: log_format_from_env()?.to_string(),
            worker_concurrency: number("WORKER_CONCURRENCY", default_concurrency())?,
            // -- AI analyst, chat apps and feedback
            llm: llm_from_env()?,
            chat: chat_from_env()?,
            feedback_channel: feedback_channel_from_env()?,

            // -- Monitoring and growth --
            region: region_from(optional("REGION"))?,
            links_host: links_host_from(optional("LINKS_HOST"))?,

            // -- Sign-in with Google and GitHub, and billing ------------------
            google_oauth: oauth_pair("GOOGLE")?,
            github_oauth: oauth_pair("GITHUB")?,
            billing_enabled: flag("BILLING_ENABLED", false)?,
            billing_plans: billing_plans(flag("BILLING_ENABLED", false)?)?,
            stripe: stripe_keys()?,

            // -- Google Search Console --
            search_console: oauth_pair("SEARCH_CONSOLE")?,
            // -- GitHub App ---------------------------------------------------
            github_app: github_app_from(GITHUB_APP_VARS.map(optional))?,
        })
    }

    /// The OAuth client for a provider (`google` or `github`), when configured.
    pub fn oauth_client(&self, provider: &str) -> Option<&OAuthClient> {
        match provider {
            "google" => self.google_oauth.as_ref(),
            "github" => self.github_oauth.as_ref(),
            _ => None,
        }
    }

    /// The providers the sign-in screen offers, in the order it shows them.
    pub fn oauth_providers(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.google_oauth.is_some() {
            out.push("google");
        }
        if self.github_oauth.is_some() {
            out.push("github");
        }
        out
    }

    /// Stripe, when billing is on and both keys are set.
    pub fn stripe_keys(&self) -> Option<&StripeKeys> {
        if self.billing_enabled {
            self.stripe.as_ref()
        } else {
            None
        }
    }

    /// The plan an organisation is on before it pays: the one with no price.
    /// `None` when billing is off.
    pub fn default_plan(&self) -> Option<&BillingPlan> {
        if !self.billing_enabled {
            return None;
        }
        self.billing_plans.iter().find(|p| p.price_id.is_none())
    }

    /// A configured plan by id. `None` when billing is off.
    pub fn plan(&self, id: &str) -> Option<&BillingPlan> {
        if !self.billing_enabled {
            return None;
        }
        self.billing_plans.iter().find(|p| p.id == id)
    }

    /// The plan a Stripe price buys.
    pub fn plan_for_price(&self, price_id: &str) -> Option<&BillingPlan> {
        self.billing_plans
            .iter()
            .find(|p| p.price_id.as_deref() == Some(price_id))
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

    /// How outgoing mail leaves the process.
    ///
    /// The one place the precedence is decided: Cloudflare when both of its
    /// variables are set, otherwise SMTP when a URL is set, otherwise nothing
    /// is sent. [`Config::from_env`] has already refused half a Cloudflare
    /// configuration, so "both or neither" holds here.
    pub fn mail_transport(&self) -> MailTransport<'_> {
        match (
            self.cloudflare_account_id.as_deref(),
            self.cloudflare_email_token.as_deref(),
        ) {
            (Some(account_id), Some(token)) => MailTransport::Cloudflare { account_id, token },
            _ => match self.smtp_url.as_deref() {
                Some(url) => MailTransport::Smtp(url),
                None => MailTransport::Log,
            },
        }
    }

    /// True when outgoing mail is configured. Callers use this to choose between
    /// sending an invitation and showing the operator a link to copy.
    pub fn mail_enabled(&self) -> bool {
        !matches!(self.mail_transport(), MailTransport::Log)
    }

    /// A 32-byte key for one subsystem, derived from [`Config::secret_key_base`].
    ///
    /// Convenience wrapper so callers never handle the raw secret themselves.
    pub fn derive_key(&self, purpose: &str) -> [u8; 32] {
        crate::crypto::derive_key(&self.secret_key_base, purpose)
    }
}

/// The way out for mail, as [`Config::mail_transport`] chose it.
///
/// Borrowed from the configuration rather than copied, because two of the
/// three carry a credential and the fewer places one is held the better.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MailTransport<'a> {
    /// Cloudflare Email Sending, over its HTTP API.
    Cloudflare {
        /// `MICAFORGE_CLOUDFLARE_ACCOUNT_ID`.
        account_id: &'a str,
        /// `MICAFORGE_CLOUDFLARE_EMAIL_TOKEN`.
        token: &'a str,
    },
    /// An SMTP relay, as a lettre connection URL (`MICAFORGE_SMTP_URL`).
    Smtp(&'a str),
    /// Nothing is sent. Each message is rendered and one line naming its
    /// recipient and subject is logged, without its body.
    Log,
}

impl std::fmt::Debug for MailTransport<'_> {
    /// Hand-written: the token and an SMTP URL's password must never reach a
    /// log line through a derived `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            MailTransport::Cloudflare { .. } => "Cloudflare",
            MailTransport::Smtp(_) => "Smtp",
            MailTransport::Log => "Log",
        })
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

// ---------------------------------------------------------------------------
// AI analyst, chat apps and feedback
// ---------------------------------------------------------------------------

/// Read a group of variables that only make sense together.
///
/// `Ok(None)` when none is set, `Ok(Some(values))` in the order asked when all
/// are, and an error naming the first missing one otherwise. Half a group is a
/// typo, and quietly running with the feature off would leave the operator
/// wondering why the app they configured never answers.
fn all_or_none(keys: &[&str]) -> Result<Option<Vec<String>>> {
    let values: Vec<Option<String>> = keys.iter().map(|key| optional(key)).collect();
    all_or_none_of(keys, values)
}

/// [`all_or_none`] over values already read, so a group can be checked
/// without touching the process environment (which tests share).
fn all_or_none_of(keys: &[&str], values: Vec<Option<String>>) -> Result<Option<Vec<String>>> {
    if values.iter().all(Option::is_none) {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(keys.len());
    for (key, value) in keys.iter().zip(values) {
        match value {
            Some(value) => out.push(value),
            None => {
                let others: Vec<String> = keys
                    .iter()
                    .filter(|other| *other != key)
                    .map(|other| format!("MICAFORGE_{other}"))
                    .collect();
                return Err(Error::validation(
                    format!("MICAFORGE_{key}"),
                    format!("is required when {} is set", others.join(" or ")),
                ));
            }
        }
    }
    Ok(Some(out))
}

/// `MICAFORGE_LLM_*`.
///
/// The base URL and the model are the pair that switches AI on. The key, the
/// timeout and the limit are refused on their own, so a key pasted into the
/// environment of an install with no endpoint is reported rather than ignored.
fn llm_from_env() -> Result<Option<LlmConfig>> {
    let Some(pair) = all_or_none(&["LLM_BASE_URL", "LLM_MODEL"])? else {
        for stray in ["LLM_API_KEY", "LLM_TIMEOUT_SECS", "LLM_DAILY_LIMIT"] {
            if optional(stray).is_some() {
                return Err(Error::validation(
                    "MICAFORGE_LLM_BASE_URL",
                    format!("is required when MICAFORGE_{stray} is set"),
                ));
            }
        }
        return Ok(None);
    };
    let base_url = trim_trailing_slash(pair[0].clone());
    let parsed = url::Url::parse(&base_url).map_err(|_| {
        Error::validation(
            "MICAFORGE_LLM_BASE_URL",
            "must be an absolute URL such as https://api.openai.com/v1",
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Error::validation(
            "MICAFORGE_LLM_BASE_URL",
            "must be an http or https URL",
        ));
    }

    let timeout_secs: u64 = number("LLM_TIMEOUT_SECS", 45)?;
    if !(5..=120).contains(&timeout_secs) {
        return Err(Error::validation(
            "MICAFORGE_LLM_TIMEOUT_SECS",
            "must be a whole number of seconds from 5 to 120",
        ));
    }

    Ok(Some(LlmConfig {
        base_url,
        model: pair[1].clone(),
        api_key: optional("LLM_API_KEY"),
        timeout_secs,
        daily_limit: number("LLM_DAILY_LIMIT", 0)?,
    }))
}

/// `MICAFORGE_SLACK_*`, `MICAFORGE_DISCORD_*` and `MICAFORGE_TELEGRAM_*`.
///
/// The two values that have a fixed shape are checked here, at boot, because
/// the alternative is a webhook that rejects every request with a signature
/// error nobody can trace back to a mistyped variable.
fn chat_from_env() -> Result<ChatApps> {
    let slack = all_or_none(&["SLACK_SIGNING_SECRET", "SLACK_BOT_TOKEN"])?.map(|v| SlackApp {
        signing_secret: v[0].clone(),
        bot_token: v[1].clone(),
    });

    let discord = match all_or_none(&[
        "DISCORD_APPLICATION_ID",
        "DISCORD_PUBLIC_KEY",
        "DISCORD_BOT_TOKEN",
    ])? {
        Some(v) => {
            let key = v[1].to_ascii_lowercase();
            if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::validation(
                    "MICAFORGE_DISCORD_PUBLIC_KEY",
                    "must be the 64-character hex public key from the Discord developer portal",
                ));
            }
            Some(DiscordApp {
                application_id: v[0].clone(),
                public_key: key,
                bot_token: v[2].clone(),
            })
        }
        None => None,
    };

    let telegram = match all_or_none(&["TELEGRAM_BOT_TOKEN", "TELEGRAM_WEBHOOK_SECRET"])? {
        Some(v) => {
            let secret = &v[1];
            let shaped = (1..=256).contains(&secret.len())
                && secret
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
            if !shaped {
                return Err(Error::validation(
                    "MICAFORGE_TELEGRAM_WEBHOOK_SECRET",
                    "must be 1 to 256 characters of A-Z, a-z, 0-9, _ and -, as Telegram requires",
                ));
            }
            Some(TelegramApp {
                bot_token: v[0].clone(),
                webhook_secret: secret.clone(),
            })
        }
        None => None,
    };

    Ok(ChatApps {
        slack,
        discord,
        telegram,
    })
}

/// `MICAFORGE_FEEDBACK_WEBHOOK_URL` and `MICAFORGE_FEEDBACK_TELEGRAM_CHAT_ID`.
fn feedback_channel_from_env() -> Result<FeedbackChannel> {
    let webhook_url = optional("FEEDBACK_WEBHOOK_URL");
    if let Some(raw) = webhook_url.as_deref() {
        let ok = url::Url::parse(raw)
            .map(|u| u.scheme() == "https" || u.scheme() == "http")
            .unwrap_or(false);
        if !ok {
            return Err(Error::validation(
                "MICAFORGE_FEEDBACK_WEBHOOK_URL",
                "must be an http or https webhook URL",
            ));
        }
    }
    let telegram_chat_id = optional("FEEDBACK_TELEGRAM_CHAT_ID");
    if telegram_chat_id.is_some() && optional("TELEGRAM_BOT_TOKEN").is_none() {
        return Err(Error::validation(
            "MICAFORGE_TELEGRAM_BOT_TOKEN",
            "is required when MICAFORGE_FEEDBACK_TELEGRAM_CHAT_ID is set",
        ));
    }
    Ok(FeedbackChannel {
        webhook_url,
        telegram_chat_id,
    })
}

/// `MICAFORGE_REGION`: the probe region label, default `default`.
///
/// Held to a short identifier because it is written on every uptime check and
/// shown on the dashboard as it is; a sentence or a URL there would be a paste
/// error, not a region.
fn region_from(raw: Option<String>) -> Result<String> {
    /// Longest label kept. A region is a short name, `eu-west-1` or `home`.
    const MAX_REGION: usize = 32;

    let Some(raw) = raw else {
        return Ok("default".to_string());
    };
    let valid = raw.len() <= MAX_REGION
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !valid {
        return Err(Error::validation(
            "MICAFORGE_REGION",
            format!("must be up to {MAX_REGION} letters, digits, `-` or `_`"),
        ));
    }
    Ok(raw)
}

/// `MICAFORGE_LINKS_HOST`: a bare hostname, lower-cased.
///
/// A scheme, a path or a port here is refused rather than stripped. The value
/// is compared against the `Host` header of every request to decide whether a
/// path is a short link, so a value that could never equal a host header would
/// silently serve no link at all.
fn links_host_from(raw: Option<String>) -> Result<Option<String>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let host = raw.to_ascii_lowercase();
    let labels_ok = host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    });
    if host.len() > 253 || !host.contains('.') || !labels_ok {
        return Err(Error::validation(
            "MICAFORGE_LINKS_HOST",
            "must be a bare hostname such as go.example.com, with no scheme, port or path",
        ));
    }
    Ok(Some(host))
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

/// `MICAFORGE_{provider}_CLIENT_ID` and `_SECRET`: both, or neither.
///
/// Half a pair is refused at boot for the same reason half a Cloudflare
/// configuration is: a sign-in button that appears and then fails at the
/// provider is worse than a boot that names the missing variable.
fn oauth_pair(provider: &str) -> Result<Option<OAuthClient>> {
    let id_var = format!("{provider}_CLIENT_ID");
    let secret_var = format!("{provider}_CLIENT_SECRET");
    let pair = both_or_neither(
        (&id_var, optional(&id_var)),
        (&secret_var, optional(&secret_var)),
    )?;
    Ok(pair.map(|(client_id, client_secret)| OAuthClient {
        client_id,
        client_secret,
    }))
}

/// `MICAFORGE_STRIPE_SECRET_KEY` and `MICAFORGE_STRIPE_WEBHOOK_SECRET`: both,
/// or neither. With only the API key, checkout would work and every
/// subscription change would be refused at the webhook, so the boot stops.
fn stripe_keys() -> Result<Option<StripeKeys>> {
    let pair = both_or_neither(
        ("STRIPE_SECRET_KEY", optional("STRIPE_SECRET_KEY")),
        ("STRIPE_WEBHOOK_SECRET", optional("STRIPE_WEBHOOK_SECRET")),
    )?;
    Ok(pair.map(|(secret_key, webhook_secret)| StripeKeys {
        secret_key,
        webhook_secret,
    }))
}

/// The six `MICAFORGE_GITHUB_APP_*` variables, without the prefix, in the
/// order [`github_app_from`] reads them.
const GITHUB_APP_VARS: [&str; 6] = [
    "GITHUB_APP_ID",
    "GITHUB_APP_SLUG",
    "GITHUB_APP_PRIVATE_KEY",
    "GITHUB_APP_WEBHOOK_SECRET",
    "GITHUB_APP_CLIENT_ID",
    "GITHUB_APP_CLIENT_SECRET",
];

/// The GitHub App from its six variables: all of them, or none.
///
/// The shapes that can be checked without GitHub are checked here, so a
/// mistyped value stops the boot with its name instead of failing at the
/// first install. The key is only checked for its PEM armour here; the server
/// parses it as an RSA key at boot, because this crate holds no JWT signer.
///
/// A PEM pasted into a one-line variable usually arrives with literal `\n`
/// sequences where its line breaks were (that is how GitHub's own docs and
/// most secret stores carry it), so those are turned back into line breaks.
fn github_app_from(values: [Option<String>; 6]) -> Result<Option<GitHubApp>> {
    let Some(v) = all_or_none_of(&GITHUB_APP_VARS, values.to_vec())? else {
        return Ok(None);
    };
    let app_id = match v[0].parse::<u64>() {
        Ok(id) if id > 0 => id,
        _ => {
            return Err(Error::validation(
                "MICAFORGE_GITHUB_APP_ID",
                "must be the numeric App ID from the app's settings page on GitHub",
            ))
        }
    };
    let slug = v[1].to_ascii_lowercase();
    let slug_ok = (1..=100).contains(&slug.len())
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !slug_ok {
        return Err(Error::validation(
            "MICAFORGE_GITHUB_APP_SLUG",
            "must be the app's name as it appears in github.com/apps/<slug>",
        ));
    }
    let private_key = v[2].replace("\\n", "\n");
    if !(private_key.contains("-----BEGIN") && private_key.contains("PRIVATE KEY-----")) {
        return Err(Error::validation(
            "MICAFORGE_GITHUB_APP_PRIVATE_KEY",
            "must be the PEM private key GitHub generated for the app, \
             starting with -----BEGIN RSA PRIVATE KEY-----",
        ));
    }
    Ok(Some(GitHubApp {
        app_id,
        slug,
        private_key,
        webhook_secret: v[3].clone(),
        client_id: v[4].clone(),
        client_secret: v[5].clone(),
    }))
}

/// Two variables that only mean something together: both values, neither, or
/// an error naming the one that is missing. Names are without the
/// `MICAFORGE_` prefix.
fn both_or_neither(
    first: (&str, Option<String>),
    second: (&str, Option<String>),
) -> Result<Option<(String, String)>> {
    match (first.1, second.1) {
        (Some(a), Some(b)) => Ok(Some((a, b))),
        (None, None) => Ok(None),
        (Some(_), None) => Err(Error::validation(
            format!("MICAFORGE_{}", second.0),
            format!("is required when MICAFORGE_{} is set", first.0),
        )),
        (None, Some(_)) => Err(Error::validation(
            format!("MICAFORGE_{}", first.0),
            format!("is required when MICAFORGE_{} is set", second.0),
        )),
    }
}

/// `MICAFORGE_BILLING_PLANS`, read only when billing is on.
fn billing_plans(enabled: bool) -> Result<Vec<BillingPlan>> {
    if !enabled {
        return Ok(Vec::new());
    }
    let raw = optional("BILLING_PLANS").ok_or_else(|| {
        Error::validation(
            "MICAFORGE_BILLING_PLANS",
            "is required when MICAFORGE_BILLING_ENABLED is true",
        )
    })?;
    parse_billing_plans(&raw)
}

/// Parse and check a plan list.
///
/// Refused: anything that is not a JSON array of plans, a plan id outside
/// `[a-z0-9_-]{1,32}`, two plans with one id or one price, an empty name, and
/// a list that does not have exactly one plan without a price (the free plan
/// every organisation starts on).
pub fn parse_billing_plans(raw: &str) -> Result<Vec<BillingPlan>> {
    let field = "MICAFORGE_BILLING_PLANS";
    let plans: Vec<BillingPlan> = serde_json::from_str(raw)
        .map_err(|e| Error::validation(field, format!("is not a JSON list of plans: {e}")))?;
    if plans.is_empty() {
        return Err(Error::validation(field, "lists no plans"));
    }

    let mut ids = std::collections::BTreeSet::new();
    let mut prices = std::collections::BTreeSet::new();
    for plan in &plans {
        let usable = (1..=32).contains(&plan.id.len())
            && plan
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if !usable {
            return Err(Error::validation(
                field,
                format!("plan id `{}` must be 1 to 32 of a-z, 0-9, _ and -", plan.id),
            ));
        }
        if plan.name.trim().is_empty() {
            return Err(Error::validation(
                field,
                format!("plan `{}` needs a name", plan.id),
            ));
        }
        if !ids.insert(plan.id.clone()) {
            return Err(Error::validation(
                field,
                format!("plan id `{}` appears twice", plan.id),
            ));
        }
        if let Some(price) = &plan.price_id {
            if price.trim().is_empty() || !prices.insert(price.clone()) {
                return Err(Error::validation(
                    field,
                    format!("plan `{}` has an empty or repeated price_id", plan.id),
                ));
            }
        }
    }

    let free = plans.iter().filter(|p| p.price_id.is_none()).count();
    if free != 1 {
        return Err(Error::validation(
            field,
            "exactly one plan must have no price_id: the free plan every organisation starts on",
        ));
    }
    Ok(plans)
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
        assert_eq!(cfg.mail_transport(), MailTransport::Log);
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

        // Mail: SMTP alone is SMTP.
        std::env::set_var("MICAFORGE_SMTP_URL", "smtp://relay.example.com:587");
        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.mail_transport(),
            MailTransport::Smtp("smtp://relay.example.com:587")
        );
        assert!(cfg.mail_enabled());

        // Half a Cloudflare configuration stops the boot and names the half
        // that is missing, whichever half that is.
        std::env::set_var("MICAFORGE_CLOUDFLARE_EMAIL_TOKEN", "cf-token");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_CLOUDFLARE_ACCOUNT_ID")
        );
        std::env::remove_var("MICAFORGE_CLOUDFLARE_EMAIL_TOKEN");
        std::env::set_var("MICAFORGE_CLOUDFLARE_ACCOUNT_ID", "cf-account");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_CLOUDFLARE_EMAIL_TOKEN")
        );

        // Both halves win over SMTP.
        std::env::set_var("MICAFORGE_CLOUDFLARE_EMAIL_TOKEN", "cf-token");
        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.mail_transport(),
            MailTransport::Cloudflare {
                account_id: "cf-account",
                token: "cf-token"
            }
        );
        assert!(cfg.mail_enabled());
        // And the token never reaches a log through the transport's Debug.
        assert_eq!(format!("{:?}", cfg.mail_transport()), "Cloudflare");

        // AI, chat and feedback are off until configured.
        assert!(cfg.llm.is_none());
        assert_eq!(cfg.chat, ChatApps::default());
        assert_eq!(cfg.feedback_channel, FeedbackChannel::default());

        // The model pair is all or nothing, and a stray key names the base URL.
        std::env::set_var("MICAFORGE_LLM_BASE_URL", "https://llm.example.com/v1/");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_LLM_MODEL")
        );
        std::env::remove_var("MICAFORGE_LLM_BASE_URL");
        std::env::set_var("MICAFORGE_LLM_API_KEY", "sk-secret");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_LLM_BASE_URL")
        );
        std::env::set_var("MICAFORGE_LLM_BASE_URL", "https://llm.example.com/v1/");
        std::env::set_var("MICAFORGE_LLM_MODEL", "small-model");
        let llm = Config::from_env()
            .unwrap()
            .llm
            .expect("the pair switches AI on");
        assert_eq!(llm.base_url, "https://llm.example.com/v1");
        assert_eq!(llm.timeout_secs, 45);
        assert_eq!(llm.daily_limit, 0);
        assert!(!format!("{llm:?}").contains("sk-secret"));
        std::env::set_var("MICAFORGE_LLM_TIMEOUT_SECS", "600");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_LLM_TIMEOUT_SECS")
        );
        std::env::remove_var("MICAFORGE_LLM_TIMEOUT_SECS");

        // Each chat platform is all or nothing, and the shaped values are checked.
        std::env::set_var("MICAFORGE_SLACK_SIGNING_SECRET", "s");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_SLACK_BOT_TOKEN")
        );
        std::env::set_var("MICAFORGE_SLACK_BOT_TOKEN", "xoxb-1");
        assert!(Config::from_env().unwrap().chat.slack.is_some());
        std::env::set_var("MICAFORGE_DISCORD_APPLICATION_ID", "1");
        std::env::set_var("MICAFORGE_DISCORD_PUBLIC_KEY", "not-hex");
        std::env::set_var("MICAFORGE_DISCORD_BOT_TOKEN", "b");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_DISCORD_PUBLIC_KEY")
        );
        std::env::set_var("MICAFORGE_DISCORD_PUBLIC_KEY", "AB".repeat(32));
        let cfg = Config::from_env().unwrap();
        assert_eq!(
            cfg.chat.discord.as_ref().unwrap().public_key,
            "ab".repeat(32)
        );
        assert!(!format!("{:?}", cfg.chat).contains("xoxb"));
        std::env::set_var("MICAFORGE_FEEDBACK_TELEGRAM_CHAT_ID", "-100");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_TELEGRAM_BOT_TOKEN")
        );
        std::env::set_var("MICAFORGE_TELEGRAM_BOT_TOKEN", "123:abc");
        std::env::set_var("MICAFORGE_TELEGRAM_WEBHOOK_SECRET", "has space");
        assert_eq!(
            Config::from_env().unwrap_err().field(),
            Some("MICAFORGE_TELEGRAM_WEBHOOK_SECRET")
        );
        std::env::set_var("MICAFORGE_TELEGRAM_WEBHOOK_SECRET", "s3cret_value-1");
        assert!(Config::from_env().unwrap().chat.telegram.is_some());

        for key in [
            "MICAFORGE_LLM_BASE_URL",
            "MICAFORGE_LLM_MODEL",
            "MICAFORGE_LLM_API_KEY",
            "MICAFORGE_SLACK_SIGNING_SECRET",
            "MICAFORGE_SLACK_BOT_TOKEN",
            "MICAFORGE_DISCORD_APPLICATION_ID",
            "MICAFORGE_DISCORD_PUBLIC_KEY",
            "MICAFORGE_DISCORD_BOT_TOKEN",
            "MICAFORGE_TELEGRAM_BOT_TOKEN",
            "MICAFORGE_TELEGRAM_WEBHOOK_SECRET",
            "MICAFORGE_FEEDBACK_TELEGRAM_CHAT_ID",
        ] {
            std::env::remove_var(key);
        }

        for key in [
            "MICAFORGE_DATABASE_URL",
            "MICAFORGE_CLICKHOUSE_URL",
            "MICAFORGE_SECRET_KEY_BASE",
            "MICAFORGE_PUBLIC_URL",
            "MICAFORGE_CLICKHOUSE_PASSWORD",
            "MICAFORGE_WRITER_BUFFER_MB",
            "MICAFORGE_SMTP_URL",
            "MICAFORGE_CLOUDFLARE_EMAIL_TOKEN",
            "MICAFORGE_CLOUDFLARE_ACCOUNT_ID",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn the_region_is_a_short_identifier_with_a_default() {
        assert_eq!(region_from(None).unwrap(), "default");
        assert_eq!(region_from(Some("eu-west_1".into())).unwrap(), "eu-west_1");
        let e = region_from(Some("eu west".into())).unwrap_err();
        assert_eq!(e.field(), Some("MICAFORGE_REGION"));
        assert!(region_from(Some("x".repeat(33))).is_err());
    }

    #[test]
    fn the_links_host_is_a_bare_hostname_or_nothing() {
        assert_eq!(links_host_from(None).unwrap(), None);
        assert_eq!(
            links_host_from(Some("Go.Example.com".into())).unwrap(),
            Some("go.example.com".to_string())
        );
        for bad in [
            "https://go.example.com",
            "go.example.com:8443",
            "go.example.com/x",
            "localhost",
            "-go.example.com",
            "go..example.com",
        ] {
            let e = links_host_from(Some(bad.into())).unwrap_err();
            assert_eq!(e.field(), Some("MICAFORGE_LINKS_HOST"), "{bad}");
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

    #[test]
    fn paired_secrets_are_both_or_neither_and_name_the_missing_half() {
        let some = |v: &str| Some(v.to_string());
        assert_eq!(
            both_or_neither(("A", some("1")), ("B", some("2"))).unwrap(),
            Some(("1".to_string(), "2".to_string()))
        );
        assert_eq!(both_or_neither(("A", None), ("B", None)).unwrap(), None);
        assert_eq!(
            both_or_neither(
                ("GOOGLE_CLIENT_ID", some("x")),
                ("GOOGLE_CLIENT_SECRET", None)
            )
            .unwrap_err()
            .field(),
            Some("MICAFORGE_GOOGLE_CLIENT_SECRET")
        );
        assert_eq!(
            both_or_neither(
                ("STRIPE_SECRET_KEY", None),
                ("STRIPE_WEBHOOK_SECRET", some("w"))
            )
            .unwrap_err()
            .field(),
            Some("MICAFORGE_STRIPE_SECRET_KEY")
        );
    }

    #[test]
    fn billing_plans_need_exactly_one_free_plan_and_unique_ids() {
        let plans = parse_billing_plans(
            r#"[{"id":"free","name":"Free","events":10000,"sites":2,"members":3},
                {"id":"team","name":"Team","events":1000000,"sites":null,"members":25,
                 "price_id":"price_1","price_label":"$19 a month"}]"#,
        )
        .unwrap();
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].events, Some(10_000));
        assert_eq!(plans[1].sites, None, "null is no limit");

        let refused = [
            "{}",
            "[]",
            r#"[{"id":"Free","name":"Free"}]"#,
            r#"[{"id":"free","name":" "}]"#,
            r#"[{"id":"a","name":"A"},{"id":"a","name":"B","price_id":"p"}]"#,
            r#"[{"id":"a","name":"A","price_id":"p"},{"id":"b","name":"B","price_id":"p"}]"#,
            r#"[{"id":"a","name":"A"},{"id":"b","name":"B"}]"#,
            r#"[{"id":"a","name":"A","price_id":"p"}]"#,
            r#"[{"id":"a","name":"A","seats":3}]"#,
        ];
        for raw in refused {
            assert_eq!(
                parse_billing_plans(raw).unwrap_err().field(),
                Some("MICAFORGE_BILLING_PLANS"),
                "{raw}"
            );
        }
    }

    #[test]
    fn secrets_never_reach_a_debug_line() {
        let client = OAuthClient {
            client_id: "id-123".into(),
            client_secret: "very-secret".into(),
        };
        assert!(!format!("{client:?}").contains("very-secret"));
        let stripe = StripeKeys {
            secret_key: "sk_live_x".into(),
            webhook_secret: "whsec_y".into(),
        };
        assert_eq!(format!("{stripe:?}"), "StripeKeys");
    }

    #[test]
    fn the_github_app_is_all_six_or_nothing_and_its_key_regains_its_lines() {
        let full = || {
            [
                Some("123456".to_string()),
                Some("Micaforge-Deploys".to_string()),
                Some(
                    "-----BEGIN RSA PRIVATE KEY-----\\nMIIE\\n-----END RSA PRIVATE KEY-----"
                        .to_string(),
                ),
                Some("hook-secret".to_string()),
                Some("Iv23liExample".to_string()),
                Some("client-secret".to_string()),
            ]
        };
        assert_eq!(github_app_from(Default::default()).unwrap(), None);

        let app = github_app_from(full())
            .unwrap()
            .expect("six values switch it on");
        assert_eq!(app.app_id, 123_456);
        assert_eq!(app.slug, "micaforge-deploys");
        assert_eq!(
            app.private_key,
            "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----"
        );
        let shown = format!("{app:?}");
        for secret in ["hook-secret", "client-secret", "MIIE"] {
            assert!(!shown.contains(secret), "{secret} reached a Debug line");
        }

        // Half a group names the first missing variable.
        let mut half = full();
        half[3] = None;
        assert_eq!(
            github_app_from(half).unwrap_err().field(),
            Some("MICAFORGE_GITHUB_APP_WEBHOOK_SECRET")
        );

        // The shapes that can be checked without GitHub are.
        for (index, bad, field) in [
            (0, "app-123", "MICAFORGE_GITHUB_APP_ID"),
            (0, "0", "MICAFORGE_GITHUB_APP_ID"),
            (1, "has space", "MICAFORGE_GITHUB_APP_SLUG"),
            (2, "not a key", "MICAFORGE_GITHUB_APP_PRIVATE_KEY"),
        ] {
            let mut values = full();
            values[index] = Some(bad.to_string());
            assert_eq!(
                github_app_from(values).unwrap_err().field(),
                Some(field),
                "{bad}"
            );
        }
    }
}
