# syntax=docker/dockerfile:1

# The Micaforge images. Two targets, one build:
#
#   server   the Rust binary, with the real tracker (mf.js) inside it
#   web      Caddy, with the built dashboard, marketing site and docs inside it
#
#   docker build --target server -t micaforge .
#   docker build --target web    -t micaforge-web .
#
# docker-compose.yml builds both. Nothing here needs Node, pnpm or Rust on the
# host: an operator with Docker alone gets the whole product.
#
# The server brings its own schema with it: the Postgres migrations are compiled
# in by `sqlx::migrate!` and the ClickHouse tables are created and healed on boot.
# That is why docker-compose.yml has no migration container and this image has no
# `sqlx-cli` in it.
#
# The build context is the repository root. See .dockerignore for what is kept
# out of it.

ARG NODE_IMAGE=node:24-bookworm-slim
ARG RUST_IMAGE=rust:1-slim-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim
ARG CADDY_IMAGE=caddy:2.11-alpine

# ---------------------------------------------------------------------------
# 0. The browser half: the tracker and the web app
# ---------------------------------------------------------------------------
# One stage builds both, because they share one pnpm install. The manifests are
# copied before the sources so the install layer survives an ordinary edit.
FROM ${NODE_IMAGE} AS web-build

# corepack reads the pnpm version from the `packageManager` field, so the image
# uses exactly the pnpm the lockfile was written by. It is fetched in a layer of
# its own, so a retried dependency install does not download pnpm again.
ENV COREPACK_ENABLE_DOWNLOAD_PROMPT=0
RUN corepack enable
WORKDIR /src

COPY package.json ./
RUN corepack install

COPY pnpm-lock.yaml pnpm-workspace.yaml ./
COPY apps/web/package.json apps/web/
COPY packages/sdk/package.json packages/sdk/
COPY packages/sdk-server/package.json packages/sdk-server/
RUN --mount=type=cache,id=micaforge-pnpm,target=/pnpm-store \
    pnpm install --frozen-lockfile --store-dir /pnpm-store

COPY packages/sdk packages/sdk
COPY apps/web apps/web

# The canonical address the marketing pages and the sitemap name. It is not the
# address this install answers on: the dashboard calls the API on whatever origin
# served it, so one image works on every domain.
ARG PUBLIC_SITE_URL=https://micaforge.com
ENV PUBLIC_SITE_URL=${PUBLIC_SITE_URL}

# The SDK build bundles the tracker and then fails if it is over its size budget,
# so an image with an oversized tracker cannot be built at all.
RUN pnpm --filter @micaforge/sdk build \
 && pnpm --filter @micaforge/web build

# ---------------------------------------------------------------------------
# 1. The Rust toolchain, shared by the planner and the builder
# ---------------------------------------------------------------------------
# Debian rather than Alpine on purpose. The TLS stack is rustls, whose default
# crypto provider (aws-lc-rs) needs cmake and a C toolchain and ships
# pre-generated bindings for glibc targets only. Building it against musl is a
# fight with no prize at the end: the runtime image below is about the same size
# either way.
#
# Why cargo-chef: a Rust workspace with this many dependencies takes minutes to
# compile and seconds to change. `prepare` reduces the workspace to a recipe (the
# manifests and the lockfile), `cook` builds the dependency graph from that recipe
# alone in a layer that only changes when a manifest or the lockfile does, and the
# real source arrives after it.
FROM ${RUST_IMAGE} AS chef

# build-essential + cmake + perl: aws-lc-rs and blake3 compile C.
# clang/libclang-dev: the fallback bindgen path, for architectures with no
#   pre-generated bindings (arm64 hosts, mostly).
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential \
      cmake \
      perl \
      clang \
      libclang-dev \
      pkg-config \
      ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Installed from crates.io rather than pulled from a third-party base image, so
# the only images this build trusts are the official `rust` and `debian` ones.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo install cargo-chef --locked --version ^0.1

WORKDIR /app

FROM chef AS planner
COPY apps/backend/ .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo chef cook --release --locked --recipe-path recipe.json

COPY apps/backend/ .

# `--locked` because a deployed binary should build from the resolved lockfile
# and not from whatever the registry offers that day.
#
# The binary is copied out inside the same RUN as the build: `target` is a cache
# mount, so the directory is gone the moment the mount is released. Nothing is
# appended with `|| true`, which would swallow a failed compile and ship an image
# with no binary in it.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/app/target,sharing=locked \
    cargo build --release --locked -p micaforge-server \
 && cp target/release/micaforge-server /usr/local/bin/micaforge-server

# ---------------------------------------------------------------------------
# 2. web: Caddy with the static site inside it
# ---------------------------------------------------------------------------
# The Caddyfile is baked in so the image runs on its own; docker-compose.yml
# also mounts infra/caddy/Caddyfile over it, so an operator can edit the proxy
# without a rebuild.
FROM ${CADDY_IMAGE} AS web
COPY infra/caddy/Caddyfile /etc/caddy/Caddyfile
COPY --from=web-build /src/apps/web/dist /srv

# ---------------------------------------------------------------------------
# 3. server: the runtime image. Last, so it is the default target.
# ---------------------------------------------------------------------------
FROM ${RUNTIME_IMAGE} AS server

# ca-certificates: outgoing TLS to the SMTP relay, to the sites whose robots.txt
#   the policy job reads, and to the operators' published IP-range documents.
#   Nothing phones home; see PRODUCT.md.
# curl: the container healthcheck, and the only reason it is installed.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /var/lib/micaforge --create-home micaforge

COPY --from=builder /usr/local/bin/micaforge-server /usr/local/bin/micaforge-server

# The real tracker, at the first place routes/script.rs looks for it. Without
# it the server would fall back to its compiled-in pageview-only tracker.
COPY --from=web-build /src/packages/sdk/dist/mf.js /src/packages/sdk/dist/mf.min.js /app/tracker/
# Its plugins (web vitals, error capture, frustration signals, ...), served at
# /api/track/p/<name>.js. Without them those features load an empty script.
COPY --from=web-build /src/packages/sdk/dist/p/ /app/tracker/p/

# /geoip: where an operator drops a MaxMind-compatible city and ASN database.
#   Without one Micaforge resolves no geography at all, which is a smaller
#   product but not a broken one.
RUN mkdir -p /geoip \
 && chown micaforge:micaforge /geoip

USER micaforge
WORKDIR /var/lib/micaforge

# Matches MICAFORGE_BIND_ADDR's default. Publishing it is docker-compose.yml's
# decision, not this file's; in the shipped stack only Caddy is reachable.
EXPOSE 8080

ENV MICAFORGE_BIND_ADDR=0.0.0.0:8080 \
    MICAFORGE_SELF_HOSTED=true \
    MICAFORGE_LOG_FORMAT=json \
    RUST_LOG=info

# Liveness only: the process answers. Whether the stores behind it are well is
# /api/health/ready, which a load balancer should use instead.
HEALTHCHECK --interval=15s --timeout=6s --start-period=90s --retries=5 \
  CMD ["curl", "-fsS", "--max-time", "5", "http://127.0.0.1:8080/api/health/live"]

# No shell in front of it: the server drains its event writer on SIGTERM, and a
# shell wrapper would swallow the signal and cost every site the last few seconds
# of its traffic on every deploy.
ENTRYPOINT ["/usr/local/bin/micaforge-server"]
