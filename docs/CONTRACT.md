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
  kind                LowCardinality(String) DEFAULT 'pageview', -- pageview|custom|outbound|file_download|form|error|web_vitals|engagement
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
  sdk_version         LowCardinality(String) DEFAULT ''
) ENGINE = MergeTree
PARTITION BY toYYYYMM(timestamp)
ORDER BY (site_id, timestamp, session_id);
```

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

---

## 2. Postgres schema (identity, config, everything non-event)

sqlx migrations in `apps/backend/migrations/`. UUIDv7 primary keys generated app-side.
Enums are `TEXT` + `CHECK`. Every secret at rest sealed with XChaCha20-Poly1305.

| table | key columns |
|---|---|
| `users` | `id`, `email` (citext unique), `password_hash`, `name`, `avatar_url`, `email_verified_at`, `totp_secret_sealed`, `created_at` |
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
| `imports` | `id`, `site_id`, `source`, `status`, `progress`, `error`, `created_at` |
| `agent_catalog` | `slug` pk, `display_name`, `operator`, `purpose`, `ua_patterns` jsonb, `ip_ranges` jsonb, `rdns_suffixes` jsonb, `docs_url`, `respects_robots` bool, `updated_at` |
| `site_policy` | `site_id`, `robots_txt` text, `llms_txt` text, `fetched_at`, `parsed` jsonb |
| `jobs` | `id`, `kind`, `payload` jsonb, `run_at`, `attempts`, `locked_by`, `locked_at` (heartbeat of the running lane), `status`; unique `(kind)` where `status IN ('pending','running')` and `payload` has `repeat_secs` |
| `sessions_auth` | `id`, `user_id`, `token_hash`, `user_agent`, `ip_hash`, `expires_at` |

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
GET  /mf.js                     the tracker, immutable-cached, ETagged
GET  /mf.min.js                 alias
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
GET /api/stats/errors            grouped by fingerprint, with sparkline
GET /api/stats/sessions          session list + detail
GET /api/stats/visitors          visitor list, traits, session count
GET /api/stats/export.csv        any of the above as CSV
GET /api/stats/export.pdf        rendered report
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

Time budgets: 5 s for the collection routes (`/api/track*`, `/api/log/edge`, `/mf.js`,
`/api/flags/evaluate`), 30 s for everything else, none for the import upload. A request
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
`manage:read`, `manage:write`, `ingest:write` (default `["stats:read"]`). It resolves
against the organisation owner's memberships but never carries the owner's authority:

- **Containment.** A site outside the key's organisation, any site but its own when
  `site_id` is set, and, for a site-bound key, every organisation-level object, reads as
  `404`. Lists (`GET /api/sites`, `GET /api/orgs`, MCP `list_sites`) show only what the key
  reaches.
- **Role ceiling.** `member` with `manage:write`, otherwise `viewer`. A key never performs
  an `admin` or `owner` action: creating, editing or deleting a site, reading or rotating
  its ingest key, managing share links, erasing visitors or recordings.
- **Scope by method.** `GET` under `/api/stats`, `/api/agents`, `/api/citation-gap`,
  `/api/policy`, `/api/content`, `/api/replay`, and `POST /api/mcp`, need `stats:read`.
  `GET` under the management prefixes needs `manage:read`. Every other method needs
  `manage:write`. A missing scope is `403`.
- **Refused outright** (`403`, whatever the scopes): `/api/auth/*`, `/api/keys*`,
  `/api/admin/*`, `/api/invitations/*`, `POST /api/orgs`, `DELETE /api/orgs/:id`,
  `/api/orgs/:id/owner`, `/api/orgs/:id/members*`, `/api/orgs/:id/invitations*`,
  `/api/sites/:id/access*`, `/api/teams/:id/members*`, and any route not classified above.

### 3.6 Accounts and roles

- The first account on an install owns it (instance admin, address verified at creation).
  Deciding "first" and creating the account happen under one Postgres advisory lock.
- `MICAFORGE_DISABLE_REGISTRATION=true` closes registration to everyone except that first
  account. `GET /api/config` reports `registration_open = !disabled || no users yet`.
- `MICAFORGE_ADMIN_EMAILS` makes an account an instance admin only once its address is
  verified.
- Sign-in admits one password guess per backoff state of an address, claimed atomically
  before the hash is checked, so parallel requests gain nothing. Per-address limits on
  sign-in, registration, recovery and share unlock key on the address the trusted-proxy
  rule resolves (`MICAFORGE_TRUSTED_PROXY_HEADER`), never the leftmost `X-Forwarded-For`.
- `DELETE /api/stats/visitors/:id` and `DELETE /api/replay/:session` need `admin` on the
  site.
- A per-site override (`/api/sites/:id/access`) can only be granted to, or revoked from, a
  member ranked below the caller (the higher of their organisation role and any existing
  override), and never the organisation's owner.
- Re-inviting an address revokes the earlier invitation's token.

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
  "e": { "lcp": 1180 }    // web vitals, when present
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
/app/:site/sessions       /app/:site/sessions/:id
/app/:site/visitors       /app/:site/visitors/:id
/app/:site/replay         /app/:site/replay/:id
/app/:site/flags
/app/:site/experiments
/app/:site/dashboards     /app/:site/dashboards/:id
/app/:site/query                         the custom query console
/app/:site/settings/*                    tracking, goals, alerts, reports, imports, share, danger
/app/settings/*                          account, organization, teams, billing, api keys
/app/admin/*                             self-host admin
/share/:slug                             public dashboard (no auth)
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
