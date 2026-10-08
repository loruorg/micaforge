# `apps/backend`: the Micaforge server

One Rust binary. It serves the collection endpoints, the analytics reads, the
management API, the MCP endpoint and the embedded job worker, and it brings its
own schema with it.

`PRODUCT.md` promises `docker compose up` on a single VPS with Postgres,
ClickHouse and Redis beside it. That budget has room for one process, so there is
no separate worker image, no migration container and no scheduler.

---

## The crates

Nine crates, in dependency order. Each one has a module-level doc comment that
explains what it is for; this is the map.

| crate | what it owns |
|---|---|
| `micaforge-core` | The vocabulary: dimensions, metrics, filters, time ranges, the one error enum, configuration, crypto. No I/O, no framework. |
| `micaforge-store` | Postgres (identity and configuration), ClickHouse (the event stream), Redis (what is allowed to disappear). |
| `micaforge-agents` | The catalog of named non-human clients, `robots.txt` and `llms.txt` parsing, and the verification that makes `agent_events.verified` mean something. |
| `micaforge-enrich` | Turning a raw hit into a row: user agent, referrer, URL normalisation, geography, the visitor digest. |
| `micaforge-query` | The SQL builder and every analytics read, including the Citation Gap. |
| `micaforge-mail` | Outgoing mail over SMTP or Cloudflare Email Sending, and the templates. Logs instead of sending when neither is configured. |
| `micaforge-jobs` | Everything Micaforge does when nobody asked: crawls, sweeps, reports, imports, retention. |
| `micaforge-server` | The HTTP surface, the extractors, the middleware, and `main`. |

The two files that outrank all of them are `docs/CONTRACT.md` (schemas, API,
payloads) and `PRODUCT.md` (what this is for). If an implementation needs to
deviate from the contract, the deviation is written back into it in the same
change.

---

## Running it

### With Docker, which is the supported path

From the repository root, not from here:

```sh
cp .env.example .env         # fill in the values it marks REQUIRED
docker compose up -d         # or ./install.sh, which does both and waits
```

The server, Postgres, ClickHouse, Valkey and Caddy come up together. First boot
runs the Postgres migrations and creates every ClickHouse table before the port
opens, which on a cold VPS is tens of seconds. Hence the healthcheck's
ninety-second `start_period`.

### Locally, against the development stack

`docker-compose.dev.yml` runs only the three stores, so the server can be
rebuilt and restarted without waiting for a container image:

```sh
docker compose -f docker-compose.dev.yml up -d      # postgres, clickhouse, valkey
cd apps/backend
cargo run -p micaforge-server
```

The binary reads a `.env` beside it if there is one. The four values it cannot
start without:

```sh
MICAFORGE_SECRET_KEY_BASE=$(openssl rand -base64 48)
MICAFORGE_DATABASE_URL=postgres://micaforge:micaforge_dev@127.0.0.1:5432/micaforge
MICAFORGE_CLICKHOUSE_URL=http://127.0.0.1:8123
MICAFORGE_REDIS_URL=redis://127.0.0.1:6379
```

Everything else has a default that describes a single-node install. The full list
with commentary is in `.env.example` at the repository root; `crates/core/src/config.rs`
is the authority.

### Checking it works

```sh
curl -fsS http://127.0.0.1:8080/api/health | jq
```

`200` means every dependency answered. `503` names the one that did not, per
store, with the latency of each probe. It is the same call the container
healthcheck makes.

---

## Building and testing

```sh
cargo check --workspace --all-targets     # what CI gates on
cargo test  --workspace                   # no database required
cargo clippy --workspace --all-targets
cargo build --release -p micaforge-server
```

**No test needs a live database.** `sqlx` is used without its macros, with
runtime-checked `query` and `query_as` only, so the build never needs a running
Postgres or an offline query cache. The one macro in the workspace is
`sqlx::migrate!`, which reads the `migrations/` directory at compile time and
embeds it in the binary. That is what lets the image migrate itself.

---

## Migrations

`migrations/` holds the Postgres schema, applied at boot in filename order by
`micaforge_store::pg::migrate`. They are embedded in the binary, so there is
nothing to mount and no `sqlx-cli` to install.

| file | what it does |
|---|---|
| `0001_init.sql` | The whole schema of `docs/CONTRACT.md` §2. |
| `0002_seed_agent_catalog.sql` | Seeds `agent_catalog` from the compiled-in catalog, so a fresh install has it before the first refresh job runs. |
| `0005_one_recurring_job_per_kind.sql` | At most one queued occurrence of each recurring job, enforced by a unique index. |
| `0006_drop_password_hash.sql` | Drops `users.password_hash`: sign-in is by emailed code, so accounts hold no credential. |

To add one, write `000N_what_it_does.sql`. Rules that are not negotiable:

- **Never edit a migration that has shipped.** sqlx records a checksum per file
  and refuses to start when one changes, which is the correct behaviour and a
  bad afternoon.
- **Additive first.** A rolling restart runs both the old and the new binary at
  once. Add a nullable column, deploy, backfill, then tighten.
- **Match the row structs.** The structs in `micaforge_store::models` *are* the
  schema; the migrations exist to make Postgres agree with them.

ClickHouse has no migration files. `Ch::init_schema` creates the database and
every table and adds a column that a table is missing, on every boot, so an
upgrade is a restart.

---

## What the process does at boot

In order, and the order matters. Each step either has to happen before the next
one can, or before the process accepts traffic:

1. Logging, before anything that can fail. `MICAFORGE_LOG_FORMAT=json` for a
   structured pipeline; `RUST_LOG` overrides the level.
2. Configuration, which fails fast and names the variable it is unhappy about.
3. Postgres, then the migrations.
4. ClickHouse, then `init_schema` and the retention TTLs.
5. Redis, which holds the rotating visitor salt.
6. The GeoIP readers, which are optional: without them geography is empty and a
   warning says so.
7. The event writer, the job worker, and the agent-range reloader.
8. The listener, last.

Shutdown runs in reverse on `SIGTERM` or `SIGINT`: the listener stops accepting,
in-flight requests finish, and only then is the event writer drained. That last
part is why `docker-compose.yml` sets `stop_grace_period: 45s`. Killing the
process at Docker's default ten seconds would drop the last few seconds of every
site's traffic on every deploy.

---

## Configuration

Every variable the server reads is in [`.env.example`](../../.env.example), with
what reads it and what happens when it is unset. Three are worth knowing before
you run the binary outside the shipped stack:

| variable | default | what it is for |
|---|---|---|
| `MICAFORGE_TRUSTED_PROXY_HEADER` | unset: trust no header | The one header the reverse proxy writes the client address into. Unset or `none`, only the socket address is used, which is right for a binary with nothing in front of it. The shipped stack sets `x-real-ip`, which Caddy always overwrites. Behind a CDN that writes its own, `cf-connecting-ip` or `true-client-ip`. See `crates/server/src/client_ip.rs`. |
| `MICAFORGE_TRACKER_PATH` | none | Where `mf.js` is. Unset, the server looks in `/app/tracker` (where the image puts it), then `packages/sdk/dist`, then falls back to a small compiled-in tracker that reports itself as `sdk_version=fallback`. |
| `RUST_LOG` | `info` | Collection routes log under `micaforge::access` at `debug`, so ingest traffic stays out of the log unless you ask for it: `RUST_LOG=info,micaforge::access=debug`. |

---

## The image

The image is built from the repository root, not from this directory, because
the server image carries the real tracker and the tracker is built by pnpm. See
the root `Dockerfile`: a Node stage builds `mf.js` and the web app, a cargo-chef
stage builds this workspace (dependencies are compiled from a recipe of the
manifests and the lockfile in a layer of their own, so an ordinary code change
rebuilds this workspace's crates and not the four hundred beneath them), and the
runtime stage is `debian:bookworm-slim` with the binary, the tracker,
`ca-certificates` and `curl`, running as uid 10001.

```sh
docker build --target server -t micaforge:local .    # from the repository root
```

One mount point, optional and empty by default:

- `/geoip`: a MaxMind-compatible city and ASN database. Point
  `MICAFORGE_GEOIP_CITY_PATH` and `MICAFORGE_GEOIP_ASN_PATH` at the files.

---

## Things worth knowing before you change something

**No raw IP is ever persisted on a human event row.** The address is read,
resolved to a coarse location, folded into the visitor digest and dropped.
`docs/CONTRACT.md` §7 is not a policy the ingest module implements; it is the
shape of that code.

**The ingest path never awaits a store it does not have to.** ClickHouse is
written through a channel, Postgres is read from a process-local cache, and only
Redis is on the path of every request. A slow event store must never become a
slow `POST /api/track`.

**Collection answers `204` to almost everything.** A malformed beacon, a payload
with a field of the wrong type, a Redis that is down: none of them produce a
status a browser console will complain about, because a page owner debugging
their own site must never be sent looking at our error instead of theirs. The one
deliberate exception is a hostname that does not belong to the site, which is
`403`.

**CORS is not one layer.** The collection endpoints are open to every origin by
contract and the rest of the API is closed to all but the dashboard. Stacking a
permissive layer on a strict one emits two `access-control-allow-origin` headers,
which every browser treats as a CORS *failure*. See `crates/server/src/routes/mod.rs`.

**An unauthorised site reads as absent.** Every failure to establish a role on a
site returns `404`, never `403`. Site ids are small integers anybody can count
through, and a `403` would confirm which of them exist.
