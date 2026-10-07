# Contributing

The project is early, which means the useful contributions right now are the ones
that change its shape, not only the ones that fix a line. Disagreeing with a
decision in `docs/CONTRACT.md` is a legitimate pull request.

## Read the contract first

[`docs/CONTRACT.md`](docs/CONTRACT.md) is authoritative. It holds the ClickHouse
and Postgres schemas, the HTTP surface, the tracker payload and the route map,
and every crate, page and package is built against it.

Two rules follow from that:

1. Do not invent a field name, an endpoint or a column that the contract does not
   list. If your change needs one, add it to the contract in the same commit.
2. Do not invent a product claim. [`PRODUCT.md`](PRODUCT.md) is the other
   authoritative file. There are no customers, no testimonials, no benchmarks and
   no logos, so nothing in the interface or the documentation may imply
   otherwise. Demo data is labelled as demo data, and an empty state says empty.

## Getting set up

You need Node 22 or newer, pnpm 11, a recent stable Rust toolchain, and Docker
with the Compose v2 plugin.

```sh
pnpm install
docker compose -f docker-compose.dev.yml up -d
cargo run --manifest-path apps/backend/Cargo.toml -p micaforge-server
pnpm dev
```

The dev compose file runs Postgres, ClickHouse and Valkey on 127.0.0.1 with the
password `micaforge_dev`. Section 10 of [`.env.example`](.env.example) is the
matching configuration block: copy it into `.env` with the comment markers
removed. The API binds 8330 and the web app runs on 4330, which is what
`apps/web/src/lib/api.ts` expects with no further configuration.

The server creates its own schema on boot, in both stores. There is no migration
command to run.

## Before you open a pull request

```sh
cargo fmt --all --manifest-path apps/backend/Cargo.toml
cargo clippy --manifest-path apps/backend/Cargo.toml --workspace --all-targets
cargo test --manifest-path apps/backend/Cargo.toml --workspace
pnpm typecheck
pnpm test
pnpm --filter @micaforge/sdk build     # includes the 3 KB tracker size check
```

CI runs the same things. The tracker's size budget is enforced, not advisory: the
build fails over 3072 bytes gzipped.

## What good looks like here

- **One change per pull request**, with a description that says what changed and
  why. The why is the part reviewers cannot reconstruct.
- **Comments explain decisions, not syntax.** The existing code says why a
  timeout is five seconds and why Postgres is off the ingest path. Match that.
- **Tests for anything with a rule in it.** Parsing, classification, time zones,
  filters and permissions all have edge cases that are cheaper to write down than
  to rediscover.
- **Accessibility is not a later pass.** WCAG 2.2 AA is the floor: every control
  keyboard-reachable with a visible focus ring, every chart series carrying shape
  or label as well as colour, and all motion behind `prefers-reduced-motion`. No
  content's existence may depend on an animation running.
- **Design tokens, never hex.** Colour and spacing come from
  `apps/web/src/styles/tokens.css` as `var(--mf-*)`.

## Adding an agent to the catalog

The agent catalog is the most useful small contribution there is, and it needs no
Rust. An entry needs the operator's own documentation URL, the user-agent
patterns it actually sends, and its published address ranges or reverse DNS
suffixes if it has them. Do not guess a pattern from one log line: an entry that
misidentifies an agent is worse than a missing one, because the dashboard reports
it as fact.

## Licence

The server is AGPL-3.0-only and the SDK in `packages/sdk` is MIT. A contribution
is offered under the licence of the files it touches. There is no CLA.
