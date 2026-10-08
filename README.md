# Micaforge

Open-source, cookieless web and product analytics that counts machine readers as
a first-class audience.

Analytics tools are built for an audience of people using browsers. A growing
share of the traffic to a documentation site, a changelog or a developer blog is
not that. It is a crawler collecting training or retrieval data, or a person who
arrived already knowing what the page said, because an AI answer summarised it
for them. Existing tools either throw that traffic away as "bots" or quietly
count it as human. Micaforge does neither: it names the agent, records what it
fetched, and puts that next to the human numbers where you can compare them.

## The Citation Gap

For every URL, Micaforge holds two numbers side by side: how often AI agents
fetched it, and how many people arrived at it from an answer engine such as
ChatGPT, Perplexity or Gemini. The ratio between those two is the Citation Gap.
A page that is crawled constantly and never sends a reader back is being taken
and not returned, and that is the number this project exists to put in front of
you.

## Status

Pre-release, and approaching its first tagged version.

The data model, the HTTP surface and the tracker payload are settled and written
down in [`docs/CONTRACT.md`](docs/CONTRACT.md), and the code behind them is
written and tested: the Rust workspace, the tracker and SDKs, and the web app
each have their own suites in CI, and the self-hosting stack is started and
checked in CI as well. Tagging a version publishes multi-architecture images and
the npm packages (see [`.github/workflows/release.yml`](.github/workflows/release.yml)).

There is no hosted service, no public benchmark, and no production install with
years of data behind it yet. Where something is not finished, the interface says
so rather than showing you a number that is not real. If you want to shape what
it becomes, now is the useful moment.

## What it is for

**The measurement you expect.** Sessions, unique visitors, pageviews, bounce
rate, session duration, real time. Pages with entries, exits, scroll depth and
engagement time. Referrers and channel attribution, UTM campaigns, geography
down to city, device, browser and OS. Custom events with JSON properties, goals,
funnels, user journeys, retention cohorts, session replay, error tracking, Core
Web Vitals. Per-visitor profiles, `identify()`, saved segments, custom
dashboards, feature flags and A/B experiments. Organisations, teams, roles,
public and private shared dashboards, an API with keys, CSV and PDF export, GA4
and Plausible imports, scheduled reports and alerts.

**The part that is different.** Agent traffic classified by operator and by
purpose, training, retrieval, search indexing or evaluation, and verified against
the address ranges those operators publish. Crawl coverage, crawl frequency and
freshness gap per URL. Answer-engine referrals as a top-level acquisition channel
next to Search and Social, with the same conversion reporting. The Citation Gap
report. A policy audit that reads your own `robots.txt` and `llms.txt` and tells
you which agents obeyed them and which did not. Content decay detection: pages
whose human traffic fell while agent traffic held. And an MCP server, so an agent
can query the analytics it is helping to produce.

**What it does not do.** No cookies, and no persistent identifier. A visitor is a
daily-rotating salted hash, the salt is discarded after 48 hours, and no raw IP
address is ever stored on a human event row. The self-hosted build makes no
outbound call at runtime, for anything, including geolocation. There is no
feature gate: what a hosted service would run is what `docker compose up` runs.

## Self-hosting

One machine, five containers, about ten minutes. A 4 GB VPS with two vCPUs is
the size the whole stack is tuned for.

You need Docker with the Compose v2 plugin, `openssl`, and a domain whose DNS
already points at the machine if you want HTTPS. Nothing else: the server and the
web app are built inside Docker.

```sh
git clone https://github.com/loruorg/micaforge
cd micaforge
./install.sh --domain analytics.example.com --email you@example.com
```

The installer checks the machine, generates the three secrets with `openssl`,
writes `.env`, pulls the store images, builds the server and web images, starts
everything, waits until it reports healthy, and prints your dashboard address and
tracking snippet. Run it with no arguments to be asked instead, with `--no-tls`
to serve plain HTTP on port 80 for a laptop (add `--public-url https://...`
behind your own TLS proxy), or with `--version <tag>` to pull a published release
instead of compiling.

Migrations are not a step you run. The server carries its Postgres migrations
inside the binary and converges the ClickHouse schema on boot, so starting the
container is what applies them.

Then:

```sh
./backup.sh --keep 14      # pg_dump plus a ClickHouse partition freeze
./update.sh                # dump, pull, rebuild, restart, with a rollback note
docker compose logs -f micaforge
```

Every setting lives in [`.env.example`](.env.example), which is the complete
inventory: each variable, what reads it, whether it is required, and what happens
when it is not set.

### Tracking

```html
<script
  defer
  data-site="1"
  data-host="https://analytics.example.com"
  src="https://analytics.example.com/mf.js"
></script>
```

That is the whole browser install. Under 3 KB gzipped, no dependencies, no
cookies, and it cannot throw an exception into your page. It sends a pageview
immediately, follows client-side navigation, and reports outbound clicks and file
downloads. See [`packages/sdk`](packages/sdk) for the npm package, the framework
wrappers, and the queue snippet that lets you call `micaforge()` before the
script has loaded.

### The other half of the install

Crawlers do not run JavaScript. No agent will ever execute the snippet above, so
the snippet alone gives you the human half of the product and an empty agent
screen.

The machine half is fed from the access log of the server that serves your
site. Micaforge takes those hits at `POST /api/log/edge`, signed with the site's
ingest key. `micaforge-shipper`, in
[`packages/sdk-server`](packages/sdk-server), follows an nginx, Apache or Caddy
log and does the signing; the setup screen in the dashboard prints the command
with your site id and key filled in. The Caddy in this stack fronts the analytics
host, not your site, so it keeps no access log at all: it would hold every
visitor's address and feed nothing.

## Architecture

```
   browser  ─┐                                    ┌─ Postgres 17
             │                                    │  identity, sites, goals,
   mf.js ────┤                                    │  dashboards, keys, jobs
             │                                    │
             ├──▶  Caddy  ──▶  micaforge  ────────┼─ ClickHouse
             │     TLS         one Rust binary    │  events, agent_events,
   shipper ──┘     proxy       axum + workers     │  replays, errors, page_index
   (your site's    web app                        │
    access log)                                   └─ Valkey
                                                     salts, rate limits,
                                                     the live counter
```

**One binary.** `apps/backend` is a Cargo workspace that builds a single
executable: the HTTP API, the ingest path, the tracker file and the background
workers, all in one process. Rust because ingest is write-heavy and has to stay
cheap on a small machine, and because a self-hoster should not have to operate a
cluster to count pageviews.

**Three stores, three jobs.** ClickHouse holds the events, because an
append-only, column-shaped stream belongs in a column store and nowhere else.
Postgres holds identity and configuration, and is deliberately kept off the
ingest path, so a Postgres stall degrades the dashboard without stopping
collection. Valkey holds only what is allowed to disappear: the daily visitor
salt, rate-limit counters and the live gauge. Valkey rather than Redis because
Redis is no longer under an OSI-approved licence, and Valkey is the BSD-licensed
fork of it; nothing in the code depends on the difference.

**The web app.** `apps/web` is Astro. The marketing pages and docs are static;
the dashboard is one React island under `/app`. The web image is Caddy with the
built files inside it; it serves them and proxies `/api` and `/mf.js` to the
server, on one origin, so the same image works on every domain.

**The SDK.** `packages/sdk` is the tracker, authored in TypeScript, bundled to a
dependency-free IIFE, with a typed npm package and wrappers for React, Next, Vue
and Svelte. Its size budget is enforced by the build: over 3 KB gzipped and it
fails.

Full schemas, the API surface and the route map are in
[`docs/CONTRACT.md`](docs/CONTRACT.md). What the product is for and who it is for
are in [`PRODUCT.md`](PRODUCT.md). Both are authoritative, and both are kept
current with the code rather than written afterwards.

## Licence

Two licences, deliberately.

- **The server, the dashboard and everything you run: AGPL-3.0-only.** If you run
  a modified Micaforge as a service, the modifications are owed back. See
  [`LICENSE`](LICENSE).
- **The SDK and the tracker: MIT.** These are the parts you embed in your own
  site, and embedding them must not reach into your code. See
  [`packages/sdk/LICENSE`](packages/sdk/LICENSE).

The self-hosted build is the whole build. There is no community edition with
features removed from it.

## Contributing

Contributions are welcome, and the project is early enough that the shape of
things is still worth arguing about. Read
[`CONTRACT.md`](docs/CONTRACT.md) first: it is the agreement every crate, page
and package is built against, and a change that deviates from it is expected to
update it in the same commit.

Getting a development environment up:

```sh
pnpm install
docker compose -f docker-compose.dev.yml up -d     # the three stores, on localhost
cargo run --manifest-path apps/backend/Cargo.toml -p micaforge-server
pnpm dev
```

The dev compose file publishes Postgres, ClickHouse and Valkey on 127.0.0.1 with
a development password, and section 10 of `.env.example` is the matching
configuration block.

[`CONTRIBUTING.md`](CONTRIBUTING.md) covers the rest. Security reports go the way
[`SECURITY.md`](SECURITY.md) describes, not to the issue tracker.
