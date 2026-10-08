# Product

<!-- impeccable:product-schema 1 -->

> **Provenance.** The user directed `/impeccable init` with an explicit "don't ask me
> questions". No interview was held. Every fact below is drawn from the written brief.
> Facts the brief did not settle are marked **[assumed]** and are open to correction;
> nothing marked **[assumed]** may be treated as user-confirmed.

## Platform

web

## Stack

Delegated (the brief constrained the choice but did not fully specify it).

- **Monorepo** — pnpm workspaces + Turborepo, Node >= 24, pnpm 11.
- **`apps/web`** — Astro 7 static build, Tailwind v4, shadcn primitives on React 19
  islands. Marketing and docs are pure Astro; the dashboard is one `client:only` React
  island under `/app` with React Router in library mode.
- **`apps/backend`** — Rust cargo workspace, one deployed binary (axum API + embedded
  tokio workers). Postgres via sqlx for identity/config; **ClickHouse for the event
  store**; Redis/Dragonfly for sessions, rate limits and the realtime counter.
  ClickHouse is forced by the domain: columnar storage is the only sane home for an
  append-only event stream.
- **`packages/sdk`** — the browser tracker, authored in TypeScript, bundled with esbuild
  to a single dependency-free IIFE, plus a typed npm package with framework wrappers.
- **Deploy** — self-host via one `docker compose up`; the same image runs the managed
  build. Cloudflare Pages for the static web app.

**Why Rust for ingest [assumed rationale, not a user requirement]:** a single small
Rust binary that holds the whole ingest path is both the honest engineering choice for a
write-heavy pipeline and the reason a self-hoster can run this on small hardware.

## Users

**Ideal customer for the hosted beta (user-directed, 2026-10-08).** Small SaaS and
developer-tool teams, one to fifty people, who grow through content: docs, a blog,
landing pages, a changelog. They left Google Analytics over cookie banners and GA4's
complexity, they are now losing search traffic to AI answers, and they want hosted,
open-source analytics that shows which AI tools send them visitors and which AI crawlers
read their pages. The marketing site targets them first; self-hosting is the second path.

**Primary — the content-owning engineer.** Runs a developer tool, an open-source
project, an API, or a documentation-led product. Owns the docs site, the changelog, the
blog and the marketing pages, usually alone or in a team of two or three. They are the
person who both writes the content and can install a script tag, so they will not
tolerate an analytics tool that needs a data team to operate.

Their situation, specifically: a growing and *invisible* share of their traffic is no
longer a person with a browser. It is a crawler collecting training or retrieval data,
or a human who arrived from an AI answer that already summarised the page. Their current
analytics tool either discards that traffic as "bots" or silently counts it as human.
Both answers are wrong, and both are the reason their numbers stopped meaning anything.

The job they are doing: deciding what to write next, and proving that what they already
wrote is working — for human readers *and* for the machines that now stand between their
content and its readers.

**Secondary — the privacy-bound operator.** Runs a site under GDPR/CCPA/PECR and needs
cookieless, no-personal-data measurement with a self-hostable deployment and an auditable
data path. This audience is served by the same product, not a separate one.

## Product Purpose

Micaforge is open-source, cookieless web and product analytics that treats **machine
readers as a first-class audience** rather than as noise to be filtered.

It does everything a modern analytics tool is expected to do — sessions, users,
pageviews, funnels, goals, journeys, retention, replays, errors, web vitals, real-time —
and it adds the layer none of them have: a full account of which AI agents fetched which
pages, when, how often, and whether any of that crawling ever came back as a human
arriving from an answer engine.

Success means an operator can answer, in one screen and without writing a query: *which
pages are feeding the machines, which of those ever return a reader, and which are being
taken for free with nothing coming back.*

## Positioning

**The Citation Gap.** For every URL, Micaforge holds two numbers side by side: how much
AI agents crawled it, and how many humans arrived at it from an answer engine. The ratio
between them is the product's signature metric and the thing Plausible cannot report,
because it does not store the two facts in the same place.

The supporting claims a neighbouring product could not truthfully copy today:

- **Agent traffic is a channel, not a filter.** AI crawlers are classified by operator
  and purpose (training, retrieval-augmentation, search indexing, evaluation), verified
  against published IP ranges where the operator publishes them, and reported with crawl
  coverage, crawl frequency and freshness gap per URL.
- **Answer engines are an acquisition channel.** Referrals from ChatGPT, Perplexity,
  Claude, Gemini, Copilot and their peers are grouped as a top-level channel next to
  Search, Social and Direct, with the same conversion and quality reporting.
- **Your stated policy versus observed behaviour.** Micaforge reads the site's own
  `robots.txt` and `llms.txt` and reports which agents were allowed, which were
  disallowed, and which fetched anyway.
- **Genuinely free while it is young, AGPL-3.0 forever.** The self-hosted build is the
  same build. There is no feature-gated community edition. *(The brief sets this; the
  contrast with Plausible's restricted Community Edition is stated in Plausible's own
  README.)*

## Operating Context

- Installation is a `<script>` tag or an npm import, done by the same person who reads
  the dashboard. First data must appear within seconds of the first pageview.
- Dashboards are read daily in short bursts and studied in depth roughly monthly.
  Real-time is checked after every ship and every launch post.
- Self-hosting is a first-class path, not a downgrade: `docker compose up` on a single
  VPS, with Postgres, ClickHouse and Redis in the same compose file.
- Public/shared dashboards matter to this audience specifically: open-source projects
  routinely publish their own traffic numbers as a trust signal.
- The tool is compared side by side against Google Analytics 4, Plausible, Fathom and
  Umami before adoption, usually by reading a feature table.

## Capabilities and Constraints

**Parity surface (must ship):**
sessions · unique users · pageviews · bounce rate · session duration · real-time
dashboard · pages/entry/exit · referrers and channel attribution · UTM campaign
reporting · geography to country → region → city · maps and globe · device, browser, OS,
screen · custom events with JSON properties · goals with conditions · funnels · user
journeys (Sankey) · retention cohorts · session replay · error tracking · Core Web Vitals
and performance-by-dimension · per-user profiles and identify() · 15+ filter dimensions
with AND/OR · saved segments · custom dashboards · feature flags · A/B experiments ·
organisations, teams, roles and per-site member access · public and private-link shared
dashboards · API keys and a stats API · CSV/PDF export · GA4 and Plausible imports ·
Google Search Console integration · scheduled email/Slack reports · traffic spike and
drop alerts · bot detection and exclusion · a self-host admin surface.

**Differentiating surface (the wedge, in the brief's terms "more modern and valuable"):**
agent traffic classification and verification · crawl coverage and freshness per URL ·
answer-engine referral channel · the Citation Gap report · `robots.txt`/`llms.txt`
policy-versus-behaviour audit · content decay detection · an MCP server so an agent can
query the analytics it is helping to produce.

**Constraints:**

- Cookieless and no persistent identifier by default. Visitor identity is a daily-rotating
  salted hash; the raw IP is never stored on a human event row.
- AGPL-3.0. The tracker script and the SDK package are the parts a customer embeds, and
  must be licensed so that embedding them does not implicate the customer's own code
  **[assumed: SDK under MIT/Apache-2.0, server under AGPL-3.0 — this is the standard
  split and needs the user's confirmation]**.
- The tracker must stay under 3 KB gzipped for the core build.
- Ingest must never block the page: `sendBeacon` first, `fetch` with `keepalive` second.
- Self-hosted installs must run without any outbound network call, including for
  geolocation (bundled MaxMind-compatible database) and for updates.

**Hosted beta (decided 2026-10-08):** micaforge.com runs a free hosted beta with open
sign-up by emailed code, operated by Internet Technology Services LLC, with per-account
limits (sites per organisation, organisations per user).

**Explicitly undecided:** pricing and the eventual paid tier (the brief says "completely
free for now", which sets today's state and not the long-term model).

## Brand Commitments

- **Name: Micaforge** — inherited from the directory the brief points at. Treated as
  binding.
- **License: AGPL-3.0** — stated in the brief, binding, and a positioning fact rather
  than a footnote.
- **Free for now, self-hostable forever** — stated in the brief, binding.
- **One theme, the category standard (user-directed, 2026-10-08).** No theme selection.
  One light, modern look that is easy to understand, at the craft level of Plausible,
  PostHog and Vercel; plain words over metaphor; a few useful animations rather than
  decoration. Blue marks people and orange marks AI everywhere.
- **Sign-in by emailed code, no passwords (2026-10-08).**

## Evidence on Hand

- **No real traffic data, no customers, no testimonials, no press, no logos and no
  benchmarks exist.** Nothing in this product may present invented customers, invented
  metrics, invented quotes or invented brand marks. Demo numbers must be visibly labelled
  as demo data.

## Product Principles

1. **Machines are readers, not noise.** Every screen that reports on humans has an honest
   answer for what the machines did. Filtering agent traffic away is a view, never the
   default truth.
2. **One question per screen, answered without a query.** The audience installs a script
   tag; it does not write SQL. Depth is available, never required.
3. **Measure traffic, not individuals.** Cookieless by default, no raw IP at rest, no
   personal data. The privacy claim is an engineering constraint before it is a marketing
   line.
4. **The self-hosted build is the whole build.** No feature gate, no "community edition"
   asterisk. What the hosted service runs is what `docker compose up` runs.
5. **Never invent a number.** Empty states say empty. Demo data is labelled demo data.
   With no customers yet, the product's credibility rests entirely on not faking any.

## Accessibility & Inclusion

- WCAG 2.2 AA as the floor: every text/background pair clears its contrast ratio, every
  control is keyboard-reachable with a visible focus ring, and every chart's meaning
  survives without colour (data series carry shape, label or position as well as hue).
- Charts are the product. Colour alone may never be the only encoding of a series, and
  the palette must hold for the common colour-vision deficiencies.
- All motion is gated behind `prefers-reduced-motion`, and no content's existence ever
  depends on an animation running.
