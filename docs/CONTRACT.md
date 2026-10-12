# Micaforge — build contract

**This file is authoritative.** Every crate, page, and package is built against the shapes
below. If an implementation needs to deviate, the deviation is written back here in the
same change. Do not invent a field name that this file does not list.

Product truth lives in `PRODUCT.md`. Visual truth lives in `apps/web/src/styles/tokens.css`
and, at finish, `DESIGN.md`.

---

## 0. Vocabulary

| Term | Meaning |
|---|---|
| **site** | One tracked property. Has a numeric `site_id` (UInt16) used everywhere in ClickHouse. |
| **visitor** | A cookieless daily-rotating hash. Never a person. |
| **session** | A visitor's activity with < 30 min inactivity gap. |
| **human event** | A hit Micaforge believes came from a person in a browser. |
| **agent event** | A hit from a non-human client Micaforge could name (crawler, fetcher, AI agent). |
| **answer engine** | A referrer host that is an AI answer surface (`chatgpt.com`, `perplexity.ai`, ...). |
| **Citation Gap** | Per URL: `agent_fetches` versus `answer_engine_arrivals` over a window. |
| **channel** | Acquisition bucket: `direct`, `search`, `answer_engine`, `social`, `referral`, `email`, `paid`, `internal`. |

`answer_engine` is a first-class sibling of `search`. That is the product.

---

## 1. ClickHouse schema (`micaforge` database)

All tables `PARTITION BY toYYYYMM(timestamp)`. Timestamps are UTC `DateTime64(3)`.

### 1.1 `events` — human + custom events

```sql
CREATE TABLE IF NOT EXISTS events (
  site_id             UInt16,
  timestamp           DateTime64(3),
  event_id            UUID,
  session_id          String,
  visitor_id          String,                 -- daily-rotating salted hash
  identified_id       String DEFAULT '',      -- from identify(), opt-in
  kind                LowCardinality(String) DEFAULT 'pageview', -- pageview|custom|outbound|file_download|form|error|web_vitals|engagement|identify|frustration
  event_name          String DEFAULT '',
  hostname            String,
  pathname            String,
  page_title          String DEFAULT '',
  querystring         String DEFAULT '',
  url_params          Map(String, String),
  hash                String DEFAULT '',
  referrer            String DEFAULT '',
  referrer_host       String DEFAULT '',
  channel             LowCardinality(String) DEFAULT 'direct',
  source              LowCardinality(String) DEFAULT '',  -- resolved: google | chatgpt | ...
  utm_source          String DEFAULT '',
  utm_medium          String DEFAULT '',
  utm_campaign        String DEFAULT '',
  utm_term            String DEFAULT '',
  utm_content         String DEFAULT '',
  click_id            String DEFAULT '',      -- gclid/fbclid/msclkid/ttclid
  browser             LowCardinality(String) DEFAULT '',
  browser_version     LowCardinality(String) DEFAULT '',
  os                  LowCardinality(String) DEFAULT '',
  os_version          LowCardinality(String) DEFAULT '',
  device_type         LowCardinality(String) DEFAULT '',   -- desktop|mobile|tablet|tv|wearable|console
  screen_w            UInt16 DEFAULT 0,
  screen_h            UInt16 DEFAULT 0,
  viewport_w          UInt16 DEFAULT 0,
  viewport_h          UInt16 DEFAULT 0,
  language            LowCardinality(String) DEFAULT '',
  timezone            LowCardinality(String) DEFAULT '',
  country             LowCardinality(FixedString(2)) DEFAULT '  ',
  region              LowCardinality(String) DEFAULT '',
  city                String DEFAULT '',
  lat                 Float32 DEFAULT 0,
  lon                 Float32 DEFAULT 0,
  asn                 UInt32 DEFAULT 0,
  asn_org             LowCardinality(String) DEFAULT '',
  is_datacenter       UInt8 DEFAULT 0,
  entry_path          String DEFAULT '',
  exit_path           String DEFAULT '',
  duration_ms         UInt32 DEFAULT 0,       -- engagement time on the previous page
  scroll_pct          UInt8 DEFAULT 0,
  props               JSON,
  revenue             Nullable(Decimal64(4)),
  currency            LowCardinality(String) DEFAULT '',
  flags               Map(String, String),    -- feature flag exposures
  experiment          LowCardinality(String) DEFAULT '',
  variant             LowCardinality(String) DEFAULT '',
  lcp                 Nullable(Float32),
  cls                 Nullable(Float32),
  inp                 Nullable(Float32),
  fcp                 Nullable(Float32),
  ttfb                Nullable(Float32),
  import_id           Nullable(UUID),
  sdk_version         LowCardinality(String) DEFAULT '',
  sample_rate         UInt8 DEFAULT 100        -- the tracker's data-sample rate; 100 = everyone
) ENGINE = MergeTree
PARTITION BY toYYYYMM(timestamp)
ORDER BY (site_id, timestamp, session_id);
```

`identify` rows are written by `identify()` and `setTraits()`: `identified_id` is the
site's id, `visitor_id` that day's hash, `props` the traits sent with the call. They are
what a profile's trait history, its linked visitor hashes and its search read
(`/api/stats/profiles`). A `revenue` trait never becomes the `revenue` column.
`frustration` rows come from the frustration plugin: `event_name` is `rage_click`,
`dead_click` or `form_abandon`; `props` holds `selector` (and `clicks`), or `form` and
`field` (the last field's name, never a value).

### 1.2 `agent_events` — the wedge

One row per fetch Micaforge attributes to a named non-human client. Written by the
**server-side** ingest path (`/api/log/edge`, log shipper, or reverse-proxy module),
because agents do not run JavaScript.

```sql
CREATE TABLE IF NOT EXISTS agent_events (
  site_id         UInt16,
  timestamp       DateTime64(3),
  agent_id        LowCardinality(String),   -- catalog slug: gptbot | claudebot | perplexitybot | ...
  operator        LowCardinality(String),   -- openai | anthropic | perplexity | google | meta | bytedance | common-crawl | ...
  purpose         LowCardinality(String),   -- training | rag | search | preview | eval | unknown
  verified        UInt8 DEFAULT 0,          -- 1 = source IP matched the operator's published ranges / rDNS
  user_agent      String,
  method          LowCardinality(String) DEFAULT 'GET',
  hostname        String,
  pathname        String,
  querystring     String DEFAULT '',
  status          UInt16 DEFAULT 200,
  bytes           UInt32 DEFAULT 0,
  duration_ms     UInt32 DEFAULT 0,
  content_type    LowCardinality(String) DEFAULT '',
  asn             UInt32 DEFAULT 0,
  asn_org         LowCardinality(String) DEFAULT '',
  country         LowCardinality(FixedString(2)) DEFAULT '  ',
  ip_hash         String DEFAULT '',        -- salted, rotating; never the raw IP
  robots_allowed  Int8 DEFAULT -1,          -- -1 unknown, 0 disallowed by robots.txt, 1 allowed
  llms_listed     Int8 DEFAULT -1,          -- -1 no llms.txt, 0 absent from it, 1 listed in it
  conditional     UInt8 DEFAULT 0           -- 1 = If-None-Match / If-Modified-Since (a re-crawl check)
) ENGINE = MergeTree
PARTITION BY toYYYYMM(timestamp)
ORDER BY (site_id, timestamp, agent_id);
```

### 1.3 `bot_events` — unnamed / suspected non-human traffic

Same column set as `agent_events` minus `operator`/`purpose`/`verified`, plus
`reasons String` and `score UInt8`. `TTL timestamp + INTERVAL 3 MONTH`.

### 1.4 `sessions_mv` — session rollup

`AggregatingMergeTree` over `(site_id, session_id)` holding `min(timestamp) AS started`,
`max(timestamp) AS ended`, `count()` pageviews, entry/exit path, channel, source, country,
device, `sum(duration_ms)`, `max(revenue)`. Fed by a materialized view from `events`.

### 1.5 `replay_events`

`(site_id, session_id, timestamp, seq UInt32, type UInt8, data String)` —
rrweb-compatible, `ZSTD(3)` codec on `data`, `TTL timestamp + INTERVAL 30 DAY` (config).

### 1.6 `errors`

`(site_id, timestamp, session_id, visitor_id, name, message, stack, filename, lineno,
colno, pathname, browser, os, release, handled UInt8, fingerprint String)`.
`release` is the site's own release (`data-release`, or a `release` property), empty when
it sent none; it is never the tracker's version.

### 1.7 `page_index` — content register

`ReplacingMergeTree(updated_at)` over `(site_id, pathname)`: `title`, `first_seen`,
`last_modified`, `word_count`, `in_sitemap UInt8`, `in_llms_txt UInt8`,
`robots_directive String`, `updated_at`. Populated by the crawler job. This is the table
the Citation Gap joins both traces against, so a page with zero human traffic still
appears.

### 1.8 `imports_raw`

Staging for GA4 / Plausible / Umami imports; the importer normalises into `events` with
`import_id` set.

### 1.9 Retention

Every table respects `MICAFORGE_RETENTION_DAYS` (default `0` = forever) applied as a TTL
at init.

### 1.10 `uptime_checks`, `link_clicks`, `revenue` — monitoring and growth

- `uptime_checks` `(site_id, timestamp, monitor_id UUID, region, ok UInt8, status_code UInt16
  (0 = no response), latency_ms UInt32, error String, ssl_days_left Nullable(Int32))`,
  `ORDER BY (site_id, monitor_id, timestamp)`. One row per check.
- `link_clicks` `(site_id (0 = organisation link), timestamp, link_id UUID, org_id UUID,
  referrer, referrer_host, country FixedString(2), region, device_type, browser, os,
  is_bot UInt8, bot_name, target)`, `ORDER BY (link_id, timestamp)`. `target` is `web` |
  `ios` | `android` | `deep_link` | `preview` | `expired`. No address is stored.
- `revenue` `(site_id, timestamp, provider, event_id, transaction_id, type, amount Int64,
  currency, customer, session_id, visitor_id, attribution, channel, source, referrer_host,
  utm_source, utm_medium, utm_campaign, country, device_type, browser, entry_path)`,
  `ORDER BY (site_id, timestamp, event_id)`. `amount` is minor units, negative for a refund;
  `type` is `sale` | `subscription` | `refund`; `attribution` is `session` | `visitor` |
  `email` | `none`; `customer` is the provider's id or `email:<keyed hash>`, never an email.

### 1.11 `mcp_calls` — MCP server analytics

`(site_id, timestamp, server_name, server_version, method, name, client_name,
client_version, protocol_version, transport, session_hash, duration_ms UInt32, status,
error_type, error_code Int32, arg_bytes UInt32, result_bytes UInt32, arguments String,
sdk_version)`, `ORDER BY (site_id, timestamp, method)`. One row per `tools/call`,
`resources/read` or `prompts/get` an MCP server answered, measured in that server by
`@micaforge/sdk-server` and sent to `POST /api/log/mcp` (signed like `/api/log/edge`).
`status` is `ok` | `error` | `tool_error`; `session_hash` is a digest keyed per process in
the SDK, never the session id; `arguments` is empty unless the owner opted in, at most
4 KB. No error message and no result is ever stored.

---

## 2. Postgres schema (identity, config, everything non-event)

sqlx migrations in `apps/backend/migrations/`. UUIDv7 primary keys generated app-side.
Enums are `TEXT` + `CHECK`. Every secret at rest sealed with XChaCha20-Poly1305.

| table | key columns |
|---|---|
| `users` | `id`, `email` (citext unique), `name`, `avatar_url`, `email_verified_at`, `totp_secret_sealed`, `created_at` — no credential column: sign-in is by emailed code (§3.6) |
| `organizations` | `id`, `name`, `slug` unique, `owner_id`, `created_at` |
| `org_members` | `org_id`, `user_id`, `role` (`owner`\|`admin`\|`member`\|`viewer`), `created_at` |
| `teams` | `id`, `org_id`, `name` |
| `team_members` | `team_id`, `user_id` |
| `sites` | `id`, `site_id` (serial UInt16 mirror), `org_id`, `domain`, `name`, `timezone`, `public` bool, `salt_rotated_at`, `created_at`, `settings` jsonb |
| `site_access` | `site_id`, `user_id`, `role` — per-site override of org role |
| `share_links` | `id`, `site_id`, `slug`, `password_hash` nullable, `expires_at` nullable, `scopes` jsonb |
| `api_keys` | `id`, `org_id`, `site_id` nullable, `name`, `prefix`, `hash`, `scopes` jsonb, `last_used_at`, `expires_at` |
| `goals` | `id`, `site_id`, `name`, `kind` (`path`\|`event`\|`outbound`\|`file`\|`revenue`), `conditions` jsonb, `value_cents` |
| `funnels` | `id`, `site_id`, `name`, `steps` jsonb, `window_hours` |
| `segments` | `id`, `site_id`, `name`, `filters` jsonb, `shared` bool |
| `dashboards` | `id`, `site_id`, `name`, `layout` jsonb |
| `feature_flags` | `id`, `site_id`, `key`, `enabled`, `rollout_pct`, `variants` jsonb, `conditions` jsonb |
| `experiments` | `id`, `site_id`, `key`, `hypothesis`, `variants` jsonb, `goal_id`, `status`, `started_at`, `ended_at` |
| `alerts` | `id`, `site_id`, `kind` (`spike`\|`drop`\|`goal`\|`error`\|`agent_surge`\|`citation_gap`), `config` jsonb, `channels` jsonb, `last_fired_at` |
| `reports` | `id`, `site_id`, `cadence` (`daily`\|`weekly`\|`monthly`), `recipients` jsonb, `channels` jsonb, `next_run_at` |
| `imports` | `id`, `site_id`, `source` (`ga4`\|`plausible`\|`umami`\|`posthog`\|`simple_analytics`), `status`, `progress`, `error`, `created_at`, `credentials_sealed` bytea (an API import's key, sealed; NULL once done or given up) |
| `error_states` | `site_id`, `fingerprint`, `status` (`resolved`\|`ignored`), `updated_by`, `resolved_at`; no row = open |
| `log_drains` | `id`, `site_id`, `provider` (`vercel`), `secret_sealed`, `verify_token`, `created_by`, `created_at`, `last_received_at` |
| `agent_catalog` | `slug` pk, `display_name`, `operator`, `purpose`, `ua_patterns` jsonb, `ip_ranges` jsonb, `rdns_suffixes` jsonb, `docs_url`, `respects_robots` bool, `updated_at` |
| `site_policy` | `site_id`, `robots_txt` text, `llms_txt` text, `fetched_at`, `parsed` jsonb |
| `jobs` | `id`, `kind`, `payload` jsonb, `run_at`, `attempts`, `locked_by`, `locked_at` (heartbeat of the running lane), `status`; unique `(kind)` where `status IN ('pending','running')` and `payload` has `repeat_secs` |
| `sessions_auth` | `id`, `user_id`, `token_hash`, `user_agent`, `ip_hash`, `expires_at` |
| `uptime_monitors` | `id`, `site_id`, `name`, `url`, `method`, `interval_secs` (60-3600), `timeout_ms`, `status_min`, `status_max`, `keyword`, `json_path`, `json_expected`, `ssl_check`, `ssl_expiry_days`, `failure_threshold`, `channels` jsonb, `enabled`, `state` (`unknown`\|`up`\|`down`), `consecutive_failures`, `last_checked_at`, `next_check_at`, `lease_until` |
| `uptime_incidents` | `id`, `monitor_id`, `site_id`, `started_at`, `resolved_at`, `cause`, `failed_checks`; at most one open per monitor |
| `status_pages` | `id`, `site_id`, `slug` unique, `title`, `description`, `logo_url`, `accent_color`, `footer_label`, `footer_url`, `published` |
| `status_page_monitors` | `page_id`, `monitor_id`, `position`, `display_name` |
| `status_incidents` | `id`, `page_id`, `kind` (`incident`\|`maintenance`), `title`, `status`, `impact`, `uptime_incident_id`, `automatic`, `monitor_ids` jsonb, `scheduled_start`, `scheduled_end`, `resolved_at` |
| `status_incident_updates` | `id`, `incident_id`, `status`, `body`, `automatic`, `created_at` |
| `link_folders` | `id`, `org_id`, `name` (unique per organisation) |
| `links` | `id`, `org_id`, `site_id` nullable, `folder_id`, `slug` unique on the install, `target_url`, `utm` jsonb, `title`, `description`, `image_url`, `ios_url`, `android_url`, `deep_link`, `expires_at`, `created_by` |
| `revenue_integrations` | `id`, `site_id`, `provider` (`stripe`\|`paddle`), `secret_sealed` (nullable until pasted), `enabled`, `last_event_at`; unique `(site_id, provider)` |
| `revenue_events_seen` | `integration_id`, `event_id`, `received_at` — webhook idempotency |
| `revenue_customer_refs` | `integration_id`, `customer_id`, `checkout_ref` |
| `revenue_refund_totals` | `integration_id`, `object_id`, `recorded_minor` |

Two `sites.settings` keys are written by the server only and are never accepted from a
settings `PATCH`: `ingest_key_sealed` (the sealed ingest key) and `public_slug` (the
address of a public site, §3.4). `share_links.scopes` is a JSON array drawn from the share
scope vocabulary of §3.4; anything else in it is ignored, and an empty list means
`["overview"]`.

---

## 3. HTTP API

Base `/api`. JSON. Auth: session cookie (dashboard) **or** `Authorization: Bearer mfg_...`
(API keys, bounded as §3.5). A shared dashboard reads with `?share=` and no account (§3.4).
All analytics reads take the **standard query envelope** below.

### 3.1 Standard query envelope (query string)

```
site        required  numeric site id
start,end   ISO-8601, or  range=today|yesterday|24h|7d|30d|90d|12mo|mtd|ytd|all
compare     none|previous|year        (default none)
tz          IANA, default site tz
granularity minute|hour|day|week|month  (auto when omitted; coarsened when the window would
                                         need more than 1,500 buckets at the requested width)
filters     JSON array, see 3.2
segment     saved segment id (merged into filters)
audience    human|agent|all           (default human — the toggle that makes the wedge visible)
limit,page  pagination for list endpoints
```

### 3.2 Filter shape

```ts
type Filter = {
  dimension: Dimension;
  op: "is" | "is_not" | "contains" | "not_contains" | "starts_with" | "ends_with"
    | "gt" | "lt" | "gte" | "lte" | "matches" | "is_set" | "is_not_set";
  value: string | number | (string | number)[];
};
type FilterGroup = { logic: "and" | "or"; filters: (Filter | FilterGroup)[] };
```

`Dimension` (the 15+ requirement — this list is the contract):
`pathname` `entry_path` `exit_path` `hostname` `page_title` `querystring` `hash`
`referrer` `referrer_host` `channel` `source` `utm_source` `utm_medium` `utm_campaign`
`utm_term` `utm_content` `browser` `browser_version` `os` `os_version` `device_type`
`screen_class` `language` `timezone` `country` `region` `city` `asn_org` `is_datacenter`
`event_name` `props.<key>` `goal` `visitor_id` `identified_id` `experiment` `variant`
`flag.<key>` `revenue` `agent_id` `operator` `purpose` `verified` `robots_allowed`
`status` `content_type`.

### 3.3 Endpoints

**Ingest** (no auth; CORS `*`)
```
POST /api/track                 browser SDK payload (§4)
POST /api/track/batch           array form, for the server SDKs
GET  /api/track/pixel.gif       no-JS fallback, query-string encoded
POST /api/log/edge              server-side hit (agent + human), HMAC-signed with the site ingest key
POST /api/log/vercel/:drain     a Vercel log drain delivery, x-vercel-signature = hex HMAC-SHA1 of
                                the raw body with the drain secret; agents and bots only
POST /api/log/mcp               MCP server calls (§1.11), signed like /api/log/edge; no CORS,
                                1 MB, 500 calls, 202 { accepted, rejected }
GET  /mf.js                     the tracker, immutable-cached, ETagged
GET  /mf.min.js                 alias
GET  /api/track/p/:name.js      a tracker plugin: vitals | errors | frustration | forms | track404 | tags
```

Rows from the three unauthenticated routes are stamped with the server's receive time; a
`timestamp` in the payload is ignored. Only `/api/log/edge`, which is signed, carries an
event time of its own.

Edge signature: `x-micaforge-signature: t=<unix seconds>,v1=<hex>`, where `v1` is
HMAC-SHA256 keyed with the site ingest key over the bytes `"<t>.<raw body>"` (the decimal
`t` exactly as sent, one `.`, then the body exactly as sent). The server refuses a `t`
more than 300 s from its clock (`401 unauthorized`, the same answer as a wrong key) and an
exact repeat of an accepted signature while it is still inside that window
(`409 conflict`). Clients sign every attempt afresh.

Edge body: `{ site_id, agent_events: [...], events: [...] }`. A row in `events` may carry
transport fields that the server reads and never stores: `user_agent`, `ip`, and the
visitor's `accept`, `accept_language`, `sec_fetch_mode` and `sec_ch_ua` request headers.
The bot check reads the four headers; a field that is absent or empty counts as a header
the visitor did not send, so a Chromium user agent without them scores as a bot.

Edge rate limits: 6000 signed batches per minute per site (`429`, retry under the same
signature). Failed signatures are counted per caller address; past 600 a minute, that
address gets `429` before verification.

**Analytics read** (all take the envelope)
```
GET /api/stats/overview          totals + deltas: visitors, sessions, pageviews, bounce, avg_duration, views_per_session
GET /api/stats/timeseries        bucketed series for any metric, plus the compare series
GET /api/stats/breakdown         ?dimension=…  ranked rows with metrics + share
GET /api/stats/realtime          last 5 min: live visitors, live agents, current pages
GET /api/stats/pages             pages with entries, exits, scroll, time
GET /api/stats/sources           channels → sources → campaigns, hierarchical
GET /api/stats/locations         country/region/city + map geometry keys
GET /api/stats/devices           browser/os/device/screen
GET /api/stats/performance       LCP/CLS/INP/FCP/TTFB p50/p75/p95 by dimension
GET /api/stats/goals             conversions, rate, revenue per goal
GET /api/stats/funnel/:id        step counts, drop-off, per-step sessions
GET /api/stats/journeys          Sankey nodes+links, depth configurable
GET /api/stats/retention         cohort grid
GET /api/stats/events            custom event names, counts, property breakdowns
GET /api/stats/errors            grouped by fingerprint, with sparkline, totals and facets;
                                 ?release= narrows, ?fingerprint= returns one group in full
POST /api/stats/errors/:fp/status { status: open|resolved|ignored }, member
GET /api/stats/frustration       rage clicks, dead clicks, abandoned forms by page/element/form
GET /api/stats/sampling          whether the window's pageviews came from a data-sample tracker
GET /api/stats/mcp/{overview,timeseries,tools,clients,calls}
                                 MCP server calls; ?server= ?method= ?client=, calls takes
                                 ?view=slowest|failing; filters and audience do not apply
GET /api/stats/profiles          ?q= searches identified ids and traits
GET /api/stats/profiles/:id      traits, trait history, linked visitor hashes, anonymous sessions
GET /api/stats/sessions          session list + detail
GET /api/stats/visitors          visitor list, traits, session count
GET /api/stats/export.csv        any of the above as CSV
GET /api/stats/export.pdf        rendered report
GET /api/export/raw              ZIP of raw events, errors, web vitals (csv|json), scope export:read
```

**Agent + Citation Gap (the wedge)**
```
GET /api/agents/overview         fetches, unique agents, verified share, bytes, top operators
GET /api/agents/breakdown        ?dimension=agent_id|operator|purpose|pathname|status
GET /api/agents/timeseries       crawl volume over time, per agent
GET /api/agents/coverage         per URL: first_crawl, last_crawl, fetch_count, freshness_gap_hours, agents[]
GET /api/agents/catalog          the known-agent catalog with docs links
GET /api/citation-gap            per URL: agent_fetches, answer_arrivals, ratio, verdict
GET /api/citation-gap/summary    site-level: taken, returned, gap ratio, trend
GET /api/policy                  robots.txt + llms.txt as parsed, per-agent allow/deny
GET /api/policy/violations       agents that fetched a path their robots rule disallowed
POST /api/policy/refetch         re-read robots.txt / llms.txt now
GET  /api/content/decay          pages whose human traffic fell while agent traffic held
```

**Management** — REST CRUD under
`/api/auth/*` `/api/sites/*` `/api/orgs/*` `/api/teams/*` `/api/goals/*` `/api/funnels/*`
`/api/segments/*` `/api/dashboards/*` `/api/flags/*` `/api/experiments/*` `/api/alerts/*`
`/api/reports/*` `/api/keys/*` `/api/imports/*` `/api/share/*` `/api/admin/*`
plus `GET /api/health/live`, `GET /api/health/ready`, `GET /api/health`, `GET /api/config`,
and `POST /api/mcp` (MCP server, tools mirror the read endpoints).

**Monitoring and growth**
```
/api/uptime/monitors[/{id}]          CRUD (member writes); /{id}/test, /{id}/series, /{id}/checks
POST /api/uptime/test?site=          one check of an unsaved monitor, recorded nowhere
GET  /api/uptime/summary|incidents   per site
/api/status-pages[/{id}]             CRUD (admin writes); /{id}/incidents (member posts)
/api/status-incidents/{id}[/updates] edit, delete, post a timeline update (member)
GET  /api/status/{slug}              no auth: a published page, monitors without URLs
/api/links[/{id}], /api/link-folders ?site= or ?org=; /{id}/qr.svg, /{id}/stats, /api/links/config
GET  /l/{slug}                       no auth: the redirect (also /{slug} on MICAFORGE_LINKS_HOST)
/api/revenue/integrations[/{id}]     admin; the signing secret is write-only
POST /api/revenue/webhooks/stripe|paddle/{integration id}   signed by the provider
GET  /api/revenue/ref?s=             no auth, CORS *: { ref: "mfr_…" } checkout reference
GET  /api/revenue/overview|timeseries|breakdown|customers|payments   standard envelope + currency
```
Uptime checks run in the `run_uptime_checks` job (every minute; due monitors only, one check
in flight per monitor, at most `MICAFORGE_WORKER_CONCURRENCY` at once; no monitor, no outbound
request). Stripe signatures are `Stripe-Signature: t=,v1=` over `"<t>.<body>"`, Paddle's
`Paddle-Signature: ts=;h1=` over `"<ts>:<body>"`, both HMAC-SHA256 with a 300 s tolerance; each
event id is recorded once. Redirects are `302` with `Cache-Control: no-store`; link-preview bots
get an Open Graph page, an expired link a `410` page.

**Health.** `GET /api/health/live` is liveness: `200` whenever the process can answer, no
store is touched, and it is not rate limited. Container healthchecks and liveness probes
use it. `GET /api/health/ready` is readiness for load balancers: store probes (cached for
2 s, one round at a time), `503` when Postgres is down, `200` with
`"status": "degraded"` when ClickHouse or Valkey is down (collection carries on through
both: the event writer buffers until ClickHouse returns, and the ingest path uses an
in-memory salt while Valkey is away). Its body also reports `writer` (queued rows and bytes, dropped rows, per
table), and `jobs` (`due`, `lanes_live`, `lanes_configured`). `GET /api/health` is the same
answer as `/ready`, kept for existing monitors.

**Event writer and shutdown.** The writer retries a failed batch on a capped exponential
backoff (0.25 s doubling to 30 s) for as long as ClickHouse is away. Each table's buffer is
bounded by `MICAFORGE_WRITER_BUFFER_MB` (default `32`, range 1-1024, per table, six tables)
and 200 000 rows; above either bound the oldest rows are dropped, counted and logged. On
`SIGTERM` in-flight requests get 10 s, then the writer (15 s) and the job worker (20 s) drain
in parallel, with a hard exit at 40 s; the compose `stop_grace_period` must be at least 45 s.

**Response envelope**

```json
{ "data": <payload>, "meta": { "query": {...}, "rows": 42, "sampled": false, "took_ms": 18 } }
```

Errors: `{ "error": { "code": "snake_case", "message": "human sentence", "field": "…" } }`
with the matching HTTP status.

Time budgets: 5 s for the collection routes (`/api/track*`, `/api/log/edge`, `/api/log/mcp`,
`/mf.js`, `/api/flags/evaluate`), 30 s for everything else, none for the import upload. A request
past its budget is answered `503` with code `timeout` and `Retry-After`, except that the
two browser routes, `POST /api/track` and the pixel, still answer as if the hit had been
taken (`204`, or the pixel). `/api/track/batch` and `/api/log/edge` are server routes and
get the `503`, so their callers retry.

### 3.4 Shared dashboards

Two ways in, both read at `/share/:slug` with no account:

- **A share link** (`share_links`): its own slug, an optional password (Argon2id), an
  optional expiry, and `scopes`.
- **A public site** (`sites.public = true`): the server issues a slug into
  `sites.settings.public_slug` when the site is published (or its share settings are first
  read). It carries every share scope and stops answering the moment `public` is cleared.
  The slug survives unpublishing, so publishing again restores the same address.

Share scopes: `overview` `agents` `citation_gap` `pages` `sources` `locations` `devices`.
`locations` and `devices` are accepted and stored but open no endpoint yet, because the
shared view does not draw those panels.

**The credential is one query parameter, `?share=`**, on every request the shared view
makes. It carries the slug of an open link or of a public site, or an unlock token
(`mfs_…`) for a password-protected link. No header is read. A request carrying `?share=` is
answered on the share alone; a cookie or API key on the same request is ignored.

```
GET  /api/share/:slug          no auth: what the slug publishes
POST /api/share/:slug/unlock   no auth: { "password": "…" } → { token, expires_at, param: "share" }
GET  /api/share?site=          admin: { public, public_slug, public_url, links: [ShareLink] }
POST /api/share?site=          admin: { slug?, password?, expires_at?, scopes? } → ShareLink
GET  /api/share/:id            admin: one ShareLink (`:id` is a UUID; a slug is never one)
PATCH /api/share/:id           admin: { password?, expires_at?, scopes? }
DELETE /api/share/:id          admin
```

`GET /api/share/:slug` answers
`{ slug, kind: "link"|"public", site: { site_id, domain, name, timezone } | null, scopes,
password_required, unlocked, expires_at, expired }`. While a link is locked (password, no
valid token in `?share=`) or expired, `site` is `null` and `scopes` is empty. An unknown
slug is `404`.

`ShareLink` is `{ id, site_id, slug, url, has_password, password_protected, expires_at,
live, scopes }`; it never carries the password digest. Scopes on create and update are
validated against the vocabulary (unknown is `422`, absent is `["overview"]`). A slug is 3
to 64 characters of `[a-z0-9_-]`, never UUID-shaped and never starting with `mfs_`.

Unlock: `401` for a wrong password. Limited per address (30 per 15 minutes) and per link
(10 per 15 minutes), both counted before the password is checked. A token lives 60
minutes, never past the link's own expiry, and stops working the moment the link is
deleted, expires, or has its password changed or cleared.

What a share credential may read. `GET` only; any request with `filters`, `segment` or
`goal` is refused; everything not in this table is refused:

| endpoint | needs scope |
|---|---|
| `GET /api/stats/overview` (audience `human`) | `overview` |
| `GET /api/stats/timeseries` (audience `human`; metric one of visitors, sessions, pageviews, bounce_rate, avg_duration, views_per_session) | `overview` |
| `GET /api/stats/breakdown?dimension=pathname` (audience `human`, same metrics) | `pages` |
| `GET /api/stats/breakdown?dimension=channel` (audience `human`, same metrics) | `sources` |
| `GET /api/agents/overview`, `GET /api/agents/timeseries` | `agents` |
| `GET /api/citation-gap/summary` | `citation_gap` |

A share never reaches sessions, visitors, replays, errors, exports or any management route.

### 3.5 API keys

`Authorization: Bearer mfg_…`. A key belongs to one organisation (`api_keys.org_id`),
optionally one site (`api_keys.site_id`), and holds `scopes` from `stats:read`,
`manage:read`, `manage:write`, `ingest:write`, `export:read` (default `["stats:read"]`). It resolves
against the organisation owner's memberships but never carries the owner's authority:

- **Containment.** A site outside the key's organisation, any site but its own when
  `site_id` is set, and, for a site-bound key, every organisation-level object, reads as
  `404`. Lists (`GET /api/sites`, `GET /api/orgs`, MCP `list_sites`) show only what the key
  reaches.
- **Role ceiling.** `member` with `manage:write`, otherwise `viewer`. A key never performs
  an `admin` or `owner` action: creating, editing or deleting a site, reading or rotating
  its ingest key, managing share links, erasing visitors or recordings.
- **Scope by method.** `GET` under `/api/stats`, `/api/agents`, `/api/citation-gap`,
  `/api/policy`, `/api/content`, `/api/replay`, `/api/revenue`, and `POST /api/mcp`, need
  `stats:read`. `GET` under the management prefixes (including `/api/uptime`,
  `/api/status-pages`, `/api/status-incidents`, `/api/links`, `/api/link-folders`) needs
  `manage:read`. `GET /api/export/*` needs `export:read` (never implied by `stats:read`);
  any other method there is refused. `GET /api/audit*` needs `audit:read`. Every other
  method needs `manage:write`. A missing scope is `403`.
- **Refused outright** (`403`, whatever the scopes): `/api/auth/*`, `/api/keys*`,
  `/api/admin/*`, `/api/invitations/*`, `POST /api/orgs`, `DELETE /api/orgs/:id`,
  `/api/orgs/:id/owner`, `/api/orgs/:id/members*`, `/api/orgs/:id/invitations*`,
  `/api/sites/:id/access*`, `/api/teams/:id/members*`, and any route not classified above.

### 3.6 Accounts and roles

- There are no passwords. Sign-in is a six-digit code sent to the address, and the same
  code creates the account when the address has none and may have one:

  ```
  POST /api/auth/code          { email, name?, org_name? } → { status: "ok", mail_delivered }
  POST /api/auth/code/verify   { email, code, totp?, name?, org_name? }
                                 → { status: "totp_required" }
                                 | { status: "ok", user, expires_at, created, joined: [org_id] }
  POST /api/auth/logout        → { status: "signed_out" }
  GET  /api/auth/me            PATCH /api/auth/me { name?, avatar_url? }
  POST /api/auth/totp/enroll   POST /api/auth/totp/verify { code }
  POST /api/auth/totp/disable  { code }   (a current authenticator code, not the session alone)
  ```

- An address may be sent a code when it has an account, when the install has no account
  yet, when registration is open, or when an unexpired invitation is waiting for it.
  `POST /api/auth/code` answers the same body for every address, sent or not, padded to a
  400 ms floor; `mail_delivered` describes the install, not the address. Anybody else is
  sent nothing.
- A code is drawn uniformly from a CSPRNG, lives 10 minutes, and is stored in Valkey only
  as an HMAC (keyed off `MICAFORGE_SECRET_KEY_BASE`) under a keyed digest of the address.
  A new request replaces the previous code. It is consumed with an atomic compare-and-swap,
  so it works once; five wrong guesses (a wrong authenticator code counts) delete it.
  Every refusal is `401` with one sentence, whatever the reason.
- Limits: 10 code requests per 10 minutes per client address and 3 per 10 minutes per email
  address; 30 verifications per 10 minutes per client address and 10 per 10 minutes per email
  address, counted with an atomic increment before the code is read. All answer `429`.
- `totp_required` does not consume the code; the follow-up sends the same code with `totp`.
- A new account with invitations waiting joins those organisations (`joined`) instead of
  getting a workspace of its own. `micaforge-server sign-in-code <email>` mints a code with
  the same rules and storage for an install with no mail transport.
- The first account on an install owns it (instance admin, address verified at creation).
  Deciding "first" and creating the account happen under one Postgres advisory lock.
- `MICAFORGE_DISABLE_REGISTRATION=true` closes registration to everyone except that first
  account and addresses with an invitation. `GET /api/config` reports
  `registration_open = !disabled || no users yet`.
- `MICAFORGE_ADMIN_EMAILS` makes an account an instance admin only once its address is
  verified. Every code sign-in verifies it.
- Per-address limits on sign-in and share unlock key on the address the trusted-proxy rule
  resolves (`MICAFORGE_TRUSTED_PROXY_HEADER`), never the leftmost `X-Forwarded-For`.
- `DELETE /api/stats/visitors/:id` and `DELETE /api/replay/:session` need `admin` on the
  site.
- A per-site override (`/api/sites/:id/access`) can only be granted to, or revoked from, a
  member ranked below the caller (the higher of their organisation role and any existing
  override), and never the organisation's owner.
- Re-inviting an address revokes the earlier invitation's token.
- `POST /api/invitations/preview { token }`, with no account, answers
  `{ email, org_id, org_name, role, invited_by, expires_at }` for a live invitation and
  `404` otherwise; the token travels in the body because paths are logged.
  `POST /api/invitations/accept { token }` needs a session whose address is the invited one.

---

## 4. SDK payload (`POST /api/track`)

```jsonc
{
  "s": 1,                 // site id
  "t": "pageview",        // kind
  "n": "",                // event name (custom)
  "u": "https://…/docs/x?a=1#h",  // full URL
  "r": "https://chatgpt.com/",    // document.referrer
  "w": 1512, "h": 982,    // screen
  "vw": 1280, "vh": 720,  // viewport
  "l": "en-GB",           // language
  "tz": "Europe/Lisbon",
  "d": 4210,              // engagement ms on previous page; absent on the first pageview
  "sc": 68,               // max scroll %
  "p": { "plan": "pro" }, // props
  "id": "u_123",          // identify()
  "v": "1.0.0",           // sdk version
  "e": { "lcp": 1180 },   // web vitals, when present
  "sr": 25                // data-sample rate, only from a sampled visitor
}
```

Keys are one to two characters on purpose: the payload rides in a `sendBeacon` and the
tracker's whole job is to be small. There is no time key: the server stamps each hit
with its own receive time. The server derives IP-based fields (country, ASN) and
never returns them to the page. `navigator.userAgent` is not sent; the server reads the
request header.

**Tracker requirements:** < 3 KB gzipped core; zero dependencies; `sendBeacon` then
`fetch(keepalive)` then image fallback; SPA `pushState`/`replaceState`/`hashchange`;
respects `do-not-track` when `data-respect-dnt` is set; `data-*` attribute configuration;
auto-capture of outbound links, downloads, form submits and 404s behind flags; never
throws into the host page; queues before init.

`data-sample` (decided in the browser per visitor and day, never stored), `data-mask` and
`data-strip-query` are applied in the core, before anything is sent. Every other optional
capability is a plugin (`/api/track/p/<name>.js`, each with its own gzip budget) that the
core appends only when its attribute asks: `vitals` (default on), `errors`, `frustration`,
`forms`, `track404`, `tags`. A plugin sends through the core's internal `x(kind, event)`,
so it passes the same opt-out, sample, exclude and mask gates as a pageview.

**Native apps** (`packages/sdk-swift`) send this same payload, in arrays, to
`POST /api/track/batch`. A screen is a `pageview` whose `u` is
`https://<site domain><path>`, because the hostname check applies to every hit and an app
has no page address; the screen's name rides in `title`, the page-title key the batch
route already reads from SDKs. No other key is added. The request's user agent is a fixed
browser-shaped string naming the platform (`Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like
Mac OS X) Micaforge-Swift/0.1.0`), never the app's name.

---

## 5. Frontend route map (`apps/web`)

Pure Astro (static, marketing + docs):
```
/                        landing
/why-agent-analytics     the wedge explained
/vs/plausible  /vs/google-analytics
/self-host               the one-command install
/docs/*                  content collection
/blog/*                  content collection
/changelog
/privacy  /terms  /dpa
```

React island at `/app` (`client:only`, React Router in library mode, `/app/*` rewrite):
```
/app                                     site chooser / org home
/app/:site                               THE RECORD  (overview + the drum)
/app/:site/agents                        agent traffic
/app/:site/citation-gap                  the Citation Gap
/app/:site/policy                        robots.txt + llms.txt vs observed
/app/:site/pages
/app/:site/sources
/app/:site/locations
/app/:site/devices
/app/:site/events
/app/:site/goals
/app/:site/funnels
/app/:site/journeys
/app/:site/retention
/app/:site/performance
/app/:site/errors
/app/:site/frustration
/app/:site/sessions       /app/:site/sessions/:id
/app/:site/visitors       /app/:site/visitors/:id
/app/:site/replay         /app/:site/replay/:id
/app/:site/flags
/app/:site/experiments
/app/:site/dashboards     /app/:site/dashboards/:id
/app/:site/query                         the custom query console
/app/:site/uptime                        uptime monitors, checks, incidents
/app/:site/status-pages                  public status pages and their incidents
/app/:site/links                         short links, folders, QR codes, clicks
/app/:site/revenue                       Stripe and Paddle revenue
/app/:site/search                        Google Search Console: search terms, pages, countries, devices
/app/:site/mcp                           MCP servers: calls, tools, clients, latency, errors
/app/:site/settings/*                    tracking, goals, alerts, reports, imports, share, danger, log-drains, guards, chat, ai, github
/app/settings/*                          account, organization, teams, billing, api keys
/app/admin/*                             self-host admin
/share/:slug                             public dashboard (no auth)
/status/:slug                            public status page (no auth, a standalone island)
```

---

## 6. Design tokens

Authoritative file: `apps/web/src/styles/tokens.css`. Never hard-code a hex in a
component; always a `var(--mf-*)`.

**The world:** a continuous-trace recording station. The default surface is a
*smoked drum record* — lampblack soot with the trace scratched through to the bright paper
beneath. Light mode is the same record transcribed onto chart paper.

- Ground is warm soot (`#13110E`), never a cool blue-charcoal.
- The trace is light scratched out of dark; a scratched line carries a faint asymmetric
  burr of displaced soot along one side. That burr is the house edge treatment and it
  replaces every glow.
- Panels carry the instrument's own paper ruling *inside* them, clipped to the panel.
  There is never a page-wide grid background.
- Primary record panels carry a pin-feed perforation column in a dedicated left gutter.
- The one accent is shellac (the fixative varnish), used tonally.
- Series colour is a shellac lightness ramp plus two annotation inks (oxide red,
  verdigris). No blue, no violet, no neon.
- Type: **Trench Slab** (display, wordmark, big figures) · **system-ui** (all UI text) ·
  **Tabular** (monospace, for genuine data only — timestamps, counts in columns, user-agent
  strings, status codes). Never mono as the house voice.

---

## 7. Non-negotiables

1. No raw IP is ever persisted on a human event row.
2. `visitor_id` is `blake3(daily_salt || site_id || ip || user_agent)`, salt rotated at
   00:00 site-local and never stored beyond 48 h.
3. The self-hosted build has no feature gate and makes no outbound call at runtime.
4. Every number rendered in a demo or empty state is visibly labelled as such.
5. Every chart encodes its series by shape or label as well as by colour.
6. All motion behind `prefers-reduced-motion`; no content's existence depends on motion.

---

## 8. AI analyst, insights, chat apps, feedback, annotations, MCP OAuth

Every feature here is **off** until its variables are set (`MICAFORGE_LLM_*`,
`MICAFORGE_SLACK_*`, `MICAFORGE_DISCORD_*`, `MICAFORGE_TELEGRAM_*`,
`MICAFORGE_FEEDBACK_*`); each group is all or nothing and the boot names a missing half.
With nothing set, no outbound call is made. Full variable list:
`docs/self-hosting/configuration`.

### 8.1 Postgres (migrations 0030–0035)

| table | key columns |
|---|---|
| `ai_conversations` | `id`, `site_id`, `user_id`, `title`, `created_at`, `updated_at` |
| `ai_messages` | `id`, `conversation_id`, `role` (`user`\|`assistant`), `content`, `tool_calls` jsonb, `error`, `created_at` |
| `investigations` | `id`, `site_id`, `signal_key`, `title`, `status` (`open`\|`monitoring`\|`resolved`), `severity`, `metric`, `dimension`, `dimension_value`, `direction`, `current_value`, `baseline_value`, `change_pct`, `window_label`, `findings`, `evidence`, `explanation`, `explanation_status` (`ready`\|`not_configured`\|`failed`), `calm_checks`, `created_at`, `updated_at`, `next_check_at`, `resolved_at`; unique `(site_id, signal_key)` where status is `open` or `monitoring` |
| `investigation_events` | `id`, `investigation_id`, `kind` (`opened`\|`checked`\|`explained`\|`status`\|`posted`), `summary`, `data`, `created_at` |
| `insight_subscriptions` | `site_id`, `user_id`, `weekly_digest` |
| `chat_links` | `id`, `org_id`, `site_id`, `platform` (`slack`\|`discord`\|`telegram`), `workspace_id`, `channel_id`, `label`, `created_by`; unique `(platform, workspace_id, channel_id)` |
| `chat_investigation_posts` | `investigation_id`, `link_id`, `thread_ref` |
| `feedback` | `id`, `kind` (`bug`\|`problem`\|`feature`\|`other`), `title`, `body`, `status` (`new`\|`triaged`\|`planned`\|`done`\|`wont_fix`), `severity`, `source` (`dashboard`\|`mcp`\|`api`\|`chat`), `reporter_user_id`, `reporter_email`, `reporter_name`, `client_name`, `api_key_id`, `oauth_grant_id`, `chat_reporter`, `org_id`, `site_id`, `page_url`, `app_version`, `user_agent`, `created_at`, `updated_at`, `github_issue_url`, `github_issue_number`, `github_issue_repo` (§9) |
| `annotations` | `id`, `site_id`, `starts_at`, `ends_at`, `text`, `color` (`neutral`\|`green`\|`red`), `category`, `source` (`dashboard`\|`api`\|`mcp`\|`github`), `url` (https, GitHub notes only), `author_id`, `created_at`, `updated_at` |
| `oauth_clients` | `id`, `client_id`, `client_name`, `redirect_uris`, `token_endpoint_auth_method`, `client_secret_hash`, `client_uri`, `created_at` |
| `oauth_grants` | `id`, `client_id`, `user_id`, `org_id`, `scopes`, `resource`, `access_prefix`, `access_hash`, `access_expires_at`, `refresh_hash`, `refresh_expires_at`, `previous_refresh_hash`, `created_at`, `last_used_at`, `revoked_at` |

A Telegram alert channel's bot token is sealed (`alerts.channels.telegram.bot_token_sealed`,
XChaCha20-Poly1305, bound to the chat id). OAuth tokens and client secrets are stored as
SHA-256 digests; authorization codes and chat link codes live in Valkey only.

### 8.2 HTTP

```
GET  /api/ai/status                     POST /api/ai/ask (JSON, or SSE with Accept: text/event-stream)
GET  /api/ai/conversations?site=        GET|DELETE /api/ai/conversations/:id
GET  /api/investigations?site=&status=  GET|PATCH /api/investigations/:id
POST /api/investigations/scan?site=     GET|PUT /api/insights/subscription?site=
GET  /api/chat/status                   GET /api/chat/links?site=   DELETE /api/chat/links/:id
POST /api/chat/link-codes?site=
POST /api/chat/slack/commands           POST /api/chat/slack/events
POST /api/chat/discord/interactions     POST /api/chat/telegram/webhook
POST /api/admin/chat/discord/commands   POST /api/admin/chat/telegram/webhook
POST /api/feedback                      GET /api/feedback/mine
GET  /api/admin/feedback                PATCH /api/admin/feedback/:id
GET|POST /api/annotations?site=         PATCH|DELETE /api/annotations/:id
GET  /.well-known/oauth-protected-resource[/api/mcp]
GET  /.well-known/oauth-authorization-server
POST /api/oauth/register                GET /api/oauth/authorize
POST /api/oauth/token                   POST /api/oauth/revoke
GET|POST /api/oauth/consent             GET /api/oauth/grants   DELETE /api/oauth/grants/:id
```

- **AI.** No model configured: `503`, code `ai_not_configured`. Daily allowance per
  organisation (`MICAFORGE_LLM_DAILY_LIMIT`) exceeded: `429`. Every answer stores its tool
  calls with the equivalent stats request and the exact SQL each read ran. The model's tools
  are bounded to one authorised site and keep the caller's filters; `visitor_id` and
  `identified_id` are refused. `/api/ai/ask` and `/api/mcp` have a 115 s budget.
- **API keys.** `/api/feedback*` needs `stats:read`; `/api/annotations` needs `stats:read`
  to read and `manage:write` to write. `/api/ai/*`, `/api/investigations*`, `/api/insights/*`,
  `/api/chat/*` and `/api/oauth/*` refuse keys.
- **Chat webhooks** answer `404` when their app is not configured and `401` on a failed
  signature (Slack v0 HMAC-SHA256 with a 300 s window; Discord Ed25519 over
  timestamp + body; Telegram secret-token header). They acknowledge at once and answer
  on a background task.
- **OAuth.** PKCE S256 only; exact redirect matching (a loopback `http` address may change
  its port); the `resource` must be `{MICAFORGE_PUBLIC_URL}/api/mcp`; access tokens `mfo_…`
  live 1 hour and work only on `/api/mcp`; refresh tokens `mfr_…` live 30 days and rotate,
  and a replayed refresh token revokes its grant; codes live 10 minutes and work once.
  Scopes `stats:read` (always) and `manage:write`; a token never acts above `member`.

### 8.3 MCP

Protocol revisions `2025-06-18`, `2025-03-26`, `2024-11-05`, negotiated; `initialize`
answers with `Mcp-Session-Id`. A `401` carries `WWW-Authenticate: Bearer
resource_metadata="…/.well-known/oauth-protected-resource/api/mcp"`. Tools added: `ask`,
`submit_feedback`, `list_my_feedback`, `create_annotation`, `list_annotations`,
`delete_annotation`, `create_goal` (writes need `manage:write`). Another area adds tools in
`routes/mcp_<area>.rs` with one line in each of `extension_tools` and `call_extension`.
The site-scoped reads `get_mcp_server_stats` and `list_mcp_calls` (`routes/mcp_servers.rs`)
read `mcp_calls` (§1.11) through the same functions as `GET /api/stats/mcp/*`.

### 8.4 Jobs

`investigate` (every 6 h, or one site on demand) and `insight_digest` (hourly; sends on
Monday from 08:00 in the site's timezone to subscribers who still hold a role on the site).

---

## 9. Google Search Console, and MCP writes for flags and links

Off until `MICAFORGE_SEARCH_CONSOLE_CLIENT_ID` and `_SECRET` are set (both or neither). A
client of its own, separate from the Google sign-in client: Search Console's scope
(`webmasters.readonly`) is one Google reviews. With it unset, no site connects and nothing
is sent to Google.

### 9.1 Stores

- Postgres `search_console_connections` (migration 0055): `site_id` pk, `connected_by`,
  `refresh_token_sealed` (XChaCha20-Poly1305, purpose `search-console-token`, AAD bound to
  the site; NULL once Google refuses it), `property`, `status` (`pending`\|`active`\|`revoked`),
  `backfill_until`, `backfilled_from`, `synced_through` (Google's days, Pacific time),
  `lease_until`, `last_sync_started_at`, `last_sync_finished_at`, `last_error`,
  `last_error_at`, `rows_synced`.
- ClickHouse `search_console` `(site_id, property, grain, date Date, query, page, hostname,
  pathname, country (alpha-2), device, clicks UInt32, impressions UInt32, position Float64,
  synced_at)`, `ReplacingMergeTree(synced_at)`, `ORDER BY (site_id, property, grain, date,
  query, page, country, device)`, `PARTITION BY toYYYYMM(date)`. `grain` is `total` (by date),
  `query` (date, query, page, country, device), `page`, `country` or `device`; each report
  reads the grain Google counts it at. Reads keep only the newest `synced_at` per
  `(grain, date)`.

### 9.2 HTTP

```
GET    /api/search-console?site=             viewer: { configured, redirect_uri, can_manage, connection }
POST   /api/search-console/connect?site=     admin, session: { url } (503 search_console_not_configured when off)
GET    /api/search-console/callback          Google's redirect: signed expiring state + PKCE S256, session must be the admin who started
GET    /api/search-console/properties?site=  admin, session: [{ site_url, permission, kind }]
PUT    /api/search-console/property?site=    admin, session: { property }, must be readable; starts a sync
POST   /api/search-console/sync?site=        member, session: 4 per hour per site
DELETE /api/search-console?site=             admin, session: revokes at Google (best effort), deletes the row
GET    /api/stats/search/overview|timeseries|queries|pages|countries|devices   envelope; stats:read
```

`/api/search-console*` refuses API keys. The list reads take `q`, `path`, `country`,
`device`, `sort` (`clicks`\|`impressions`\|`ctr`\|`position`); `pages` adds Micaforge's own
`visitors`, `pageviews` and `from_google` for the same path.

### 9.3 Job

`sync_search_console`: daily (first run 105 min after boot), one site on "Sync now" or as a
backfill follow-up. Per run: the last three days again, anything new, and up to 62 days of
the sixteen-month backfill, in 7-day chunks; `searchAnalytics.query` paged by 25 000 rows up
to 50 000 per grain and window, `type: web`, `dataState: all`. `invalid_grant` marks the
connection `revoked`.

### 9.4 MCP

Writes (`manage:write` and `member`, audited with the MCP client in the actor label):
`create_flag`, `update_flag`, `set_flag_enabled`, `delete_flag`, `create_link`, `update_link`,
`delete_link`; the deletes need `confirm: true`. Reads: `list_flags`, `get_search_overview`,
`get_search_queries`, `get_search_breakdown`. AI analyst tool: `get_search_queries`. Audit
actions: `search_console.connected|property_chosen|disconnected`, `link.created|updated|deleted`
(the link routes record them too).

## 10. GitHub App

Off until all six `MICAFORGE_GITHUB_APP_*` variables are set (`ID`, `SLUG`, `PRIVATE_KEY`,
`WEBHOOK_SECRET`, `CLIENT_ID`, `CLIENT_SECRET`); part of the group stops the boot, and a
private key that is not an RSA PEM stops it too. Separate from the sign-in OAuth app
(`MICAFORGE_GITHUB_CLIENT_*`). No GitHub credential is stored in Postgres: app JWTs (RS256,
`iat` now − 60 s, `exp` now + 9 min, `iss` the App ID) are signed per call, and installation
tokens are cached in Valkey sealed (XChaCha20-Poly1305, bound to the installation id) until
five minutes before they expire.

### 10.1 Postgres (migrations 0050–0052)

| table | key columns |
|---|---|
| `github_installations` | `id`, `org_id`, `installation_id` (GitHub's), `account_login`, `account_type`, `repository_selection`, `suspended_at`, `installed_by`, `created_at`, `updated_at`; unique `(org_id, installation_id)` |
| `github_repo_links` | `id`, `site_id` unique, `installation_ref`, `repo_id`, `repo_full_name`, `default_branch`, `html_url`, `annotate_deploys` (default true), `annotate_releases` (true), `annotate_pushes` (false), `environments` jsonb (default `["production"]`, empty = all), `agent_issues` (false), `created_by`, `created_at`, `updated_at` |
| `github_deliveries` | `delivery_id` pk (`X-GitHub-Delivery`), `event`, `action`, `installation_id`, `repo_id`, `repo_full_name`, `outcome` (`processing`\|`recorded`\|`ignored`\|`failed`), `detail`, `received_at`; kept 30 days |
| `github_events` | `id`, `site_id`, `kind` (`deploy`\|`release`\|`push`), `external_id` (deployment id, release id, head commit), `repo_full_name`, `git_ref`, `sha`, `title`, `environment`, `actor`, `url`, `commits` jsonb (≤ 20, logins only), `occurred_at`, `annotation_id`, `delivery_id`, `created_at`; unique `(site_id, kind, external_id)` |

`annotations.source` gains `github` and `annotations` gains `url` (0051, which drops the
unnamed 0034 source CHECK by looking it up and re-adds it as `annotations_source_check`).
`feedback` gains `github_issue_url`, `github_issue_number`, `github_issue_repo` (0052).

### 10.2 HTTP

```
GET    /api/github/status?site=              viewer: configured, app, webhook_url, callback_url,
                                             can_install, can_link, installations (admins), link
POST   /api/github/install?site=             org admin, session: { url, expires_in } (signed state)
GET    /api/github/callback                  GitHub's return: redirects to
                                             /app/:site/settings/github?github=installed|requested
                                             or ?github_error=state|signed_out|denied|forbidden|not_yours|github|disabled
DELETE /api/github/installations/:id         org admin, session: { removed, uninstalled_on_github }
GET    /api/github/repos?site=&installation= site admin: { repositories: [{id, full_name, private,
                                             default_branch, html_url}], truncated }
PUT    /api/github/link?site=                site admin: { installation, repo: "owner/name",
                                             deploys?, releases?, pushes?, environments?, agent_issues? }
PATCH  /api/github/link?site=                site admin: the options above
DELETE /api/github/link?site=                site admin
GET    /api/github/events?site=&limit=       viewer: recorded deploys, releases, pushes, newest first
GET    /api/github/deliveries?site=&limit=   site admin: deliveries for the linked repository
POST   /api/github/feedback/:id/issue        { site? }: { url, number, repo, created }
POST   /api/github/webhook                   GitHub only; X-Hub-Signature-256 (HMAC-SHA256 of the raw
                                             body, constant-time); 4 MiB body limit
```

- **Install.** The state is `base64url(json).base64url(hmac)` with organisation, person, site,
  nonce and a 15-minute expiry; the nonce works once (Valkey). The callback needs the same
  person signed in and still an organisation admin, exchanges GitHub's OAuth code for the
  person's token, and records the installation only when `GET /user/installations` lists it
  for them; then it reads the installation with the app JWT and mints a token.
- **Webhook.** `401` on a bad signature (nothing stored), `404` when not configured. A delivery
  id already `recorded` or `ignored` is answered `{ received, duplicate: true }`; a `failed`
  one may be claimed again. Recorded: `push` to the default branch, `release` `published`,
  `deployment_status` `success` in an environment the link names, `installation`
  `deleted`/`suspend`/`unsuspend`/`created`/`new_permissions_accepted`,
  `installation_repositories` `removed`. Each event is recorded once per site, with its
  annotation in the same transaction.
- **Issues.** A session needs to read the report (`feedback::may_read`) and hold `admin` on the
  target site. A key or OAuth grant needs `manage:write`, to have sent the report, `member` on
  the site, and the link's `agent_issues`. The issue holds the report text in a fence and
  non-personal facts only. Idempotent.
- **API keys** are refused on every `/api/github/*` route (no policy entry). Audit actions:
  `github.app_installed`, `github.app_uninstalled`, `github.app_suspended`,
  `github.app_unsuspended`, `github.repo_linked`, `github.repo_changed`,
  `github.repo_unlinked`, `github.issue_created`.

### 10.3 MCP and the AI analyst

MCP tools `list_deploys` (site-scoped read: `site`, a window, `kinds`, `limit`) and
`create_github_issue_from_feedback` (`feedback`, optional `site`; the rule above). The AI
analyst's tool `list_deploys` reads the same rows for the conversation's site and window.
