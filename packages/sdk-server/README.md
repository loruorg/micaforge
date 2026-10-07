# @micaforge/sdk-server

The Micaforge server-side SDK: server-rendered pageviews, and the AI agent traffic your
browser tracker will never see.

Micaforge is open-source, cookieless web and product analytics that treats machine
readers as a first-class audience. AI crawlers do not run JavaScript. A script tag cannot
see GPTBot, ClaudeBot or PerplexityBot, by construction and forever. The only place those
fetches can be observed is the server that answered them, which is what this package is
for.

- **No dependencies.** Not one, at runtime.
- **Node 20+**, and it also runs on Cloudflare Workers, Deno and Bun.
- **It cannot throw into your request path.** Every method is wrapped. A bad key, a dead
  collector, a malformed request: all of them end as one line through your logger and a
  dropped event.
- **It never blocks a response.** Hits queue and go out on a 200 ms window, or as soon as
  50 are waiting.
- **MIT.** The Micaforge server is AGPL-3.0. The parts you embed are not, so embedding
  them does not reach into your own code.

---

## Install

```bash
npm install @micaforge/sdk-server
```

You need three things from **Settings, Tracking** in your Micaforge dashboard: the host,
the numeric site id, and the site's **ingest key**. The ingest key is a server-side
secret. It is not the site id, and a build that ships it to the browser should be treated
as a leak.

```bash
MICAFORGE_HOST=https://micaforge.example.com
MICAFORGE_SITE_ID=1
MICAFORGE_INGEST_KEY=...
```

---

## The fastest way to see agent traffic

You do not have to change your application at all. If you run nginx, Caddy or Apache, the
answer is already in your access log.

```bash
export MICAFORGE_HOST=https://micaforge.example.com
export MICAFORGE_SITE_ID=1
export MICAFORGE_INGEST_KEY=...

# Read the log, classify every line, send nothing.
npx --package=@micaforge/sdk-server micaforge-shipper \
  --dry-run --from-beginning --no-follow /var/log/nginx/access.log
```

```
Dry run. Nothing was sent.
  lines read        18422
  agent fetches     1184
  skipped           17238

  By agent, as this tool's mirror named it. The server decides the
  operator, the purpose and whether the fetch could be verified.

    gptbot             412
    claudebot          337
    perplexitybot      201
    ...
```

Then drop the `--dry-run` and let it follow the file.

```bash
npx --package=@micaforge/sdk-server micaforge-shipper /var/log/nginx/access.log
```

If the package is already installed in the project, the binary is on the path and
`npx micaforge-shipper ...` is enough. The `--package=` form is for running it on a web
server that has nothing installed, where the package name and the command name differ and
`npx` needs telling which is which.

`micaforge-shipper --help` is written to be read. Full guides:
[nginx](./src/nginx/README.md) and [Caddy](./src/caddy/README.md).

---

## The client

```ts
import { Micaforge } from "@micaforge/sdk-server";

export const micaforge = new Micaforge({
  host: process.env.MICAFORGE_HOST!,
  siteId: Number(process.env.MICAFORGE_SITE_ID),
  ingestKey: process.env.MICAFORGE_INGEST_KEY!,
});
```

One instance per process. It holds a queue and a timer, and nothing else.

```ts
// A request, classified for you: a named crawler becomes an agent fetch, anything else
// becomes a pageview.
micaforge.pageview(req);

// A crawl you already know about, from your own edge rules or a log line.
micaforge.agentHit({
  userAgent: "Mozilla/5.0 (compatible; ClaudeBot/1.0)",
  url: "https://docs.example.com/guide",
  status: 200,
  bytes: 4210,
  ip: "203.0.113.9",
});

// A custom event. Named events are taken at your word and never reclassified.
micaforge.track({
  name: "signup",
  url: "https://docs.example.com/pricing",
  props: { plan: "pro" },
  revenue: 19,
  currency: "EUR",
  userAgent: req.headers["user-agent"],
  ip: req.socket.remoteAddress,
});

// A mixed batch, which is what a log file gives you.
micaforge.batch([
  { type: "agent", userAgent: "GPTBot/1.2", url: "https://x.example/a" },
  { type: "event", url: "https://x.example/b", userAgent: chromeUa },
]);

// Send what is queued and wait for it. Never rejects.
await micaforge.flush();

// Flush, then stop. Call this from your own SIGTERM handler.
await micaforge.shutdown();
```

### Why `pageview` may not record a pageview

`pageview(req)` reads the user agent and decides. A named crawler is written to
`agent_events`, and everything else to `events`. `track()` takes you at your word, with
one exception: a `pageview` whose user agent is a named crawler is filed as an agent
fetch anyway. Counting a crawler as a reader is the exact error this product exists to
correct, and it would be strange to make it here.

The final word belongs to the server, which has the full catalog, the operators'
published IP ranges and your own `robots.txt`. This package carries a small pattern list
to decide what to send; it never decides which agent it was.

---

## Express

```ts
import express from "express";
import { Micaforge } from "@micaforge/sdk-server";

const app = express();
const micaforge = new Micaforge({ host, siteId: 1, ingestKey });

app.use(micaforge.express());
```

Mount it first, before your routes and before any body parser. It watches the response's
`finish` event, so it needs to be in place before a handler answers, and it reads nothing
from the body.

```ts
app.use(
  micaforge.express({
    exclude: ["/health", "/_next/**"],
    humans: false, // the browser tracker already counts those
  }),
);
```

Behind a proxy, either leave the SDK's `trustProxy` on (the default) or set Express's own
`app.set("trust proxy", true)`. Both end at the same address.

There is also a functional form, if you would rather pass the client in:

```ts
import { micaforgeExpress } from "@micaforge/sdk-server/express";

app.use(micaforgeExpress(micaforge, { exclude: ["/health"] }));
```

---

## Fastify

```ts
import Fastify from "fastify";
import { Micaforge } from "@micaforge/sdk-server";
import { micaforgeFastify } from "@micaforge/sdk-server/fastify";

const app = Fastify();
const micaforge = new Micaforge({ host, siteId: 1, ingestKey });

await app.register(micaforgeFastify, {
  client: micaforge,
  exclude: ["/health"],
});
```

The plugin adds one `onResponse` hook, which is the point at which the status, the size
and the response time are all facts rather than guesses. It also flushes on Fastify's
`onClose`, so a graceful shutdown does not lose the last window. Pass
`closeWithApp: false` if you want to own that yourself.

It registers at the top level rather than inside its own scope, using the same
`skip-override` symbol `fastify-plugin` uses. That is how one registration covers every
route without this package taking a dependency.

---

## Hono, on Workers, Deno and Bun

```ts
import { Hono } from "hono";
import { Micaforge } from "@micaforge/sdk-server";
import { micaforgeHono } from "@micaforge/sdk-server/hono";

const app = new Hono();

app.use("*", (c, next) => {
  const micaforge = new Micaforge({
    host: c.env.MICAFORGE_HOST,
    siteId: Number(c.env.MICAFORGE_SITE_ID),
    ingestKey: c.env.MICAFORGE_INGEST_KEY,
  });
  return micaforgeHono(micaforge)(c, next);
});
```

Two things about the edge, both handled for you but worth knowing:

- **`waitUntil`.** On Workers, anything still running when the response returns is
  cancelled with the isolate. The middleware hands the flush to
  `c.executionCtx.waitUntil()`, which keeps the isolate alive exactly long enough to
  finish the send.
- **Per-request client.** An isolate can be recycled between requests, so a queue built
  at module scope can outlive the request that filled it. Building the client per request
  and flushing through `waitUntil` is the shape that always sends.

On Workers, signing needs `node:crypto`:

```toml
# wrangler.toml
compatibility_flags = ["nodejs_compat"]
```

Bun and Deno ship `node:crypto` natively and need nothing.

---

## Next.js

```ts
// instrumentation.ts
import { createInstrumentation } from "@micaforge/sdk-server/next";

export const { register, client: micaforge } = createInstrumentation({
  host: process.env.MICAFORGE_HOST!,
  siteId: Number(process.env.MICAFORGE_SITE_ID),
  ingestKey: process.env.MICAFORGE_INGEST_KEY!,
});
```

```ts
// middleware.ts
import { NextResponse } from "next/server";
import { createMiddleware } from "@micaforge/sdk-server/next";
import { micaforge } from "./instrumentation";

const record = createMiddleware(micaforge, { exclude: ["/_next/**", "/favicon.ico"] });

export function middleware(request, event) {
  record(request, event);
  return NextResponse.next();
}

// Signing is HMAC-SHA256 through node:crypto, which the Edge runtime does not have.
export const config = {
  runtime: "nodejs",
  matcher: ["/((?!_next/static|_next/image).*)"],
};
```

Middleware is the recommended install because it sees requests for static pages that no
React code runs for, which is most of what a crawler asks for.

If you cannot run middleware on the Node runtime, record from a route handler or a server
component instead:

```ts
import { headers } from "next/headers";
import { trackRequest } from "@micaforge/sdk-server/next";
import { micaforge } from "@/instrumentation";

export default async function Page() {
  trackRequest(micaforge, await headers(), "/docs/getting-started");
  // ...
}
```

That path only sees requests that reach your code. For a static page it will miss most
crawls, so treat it as the fallback it is. The log shipper closes that gap completely.

---

## Bare `node:http`

```ts
import { createServer } from "node:http";
import { Micaforge } from "@micaforge/sdk-server";

const micaforge = new Micaforge({ host, siteId: 1, ingestKey });
const record = micaforge.middleware();

createServer((req, res) => {
  record(req, res);
  res.writeHead(200, { "content-type": "text/html" });
  res.end("<h1>hello</h1>");
}).listen(3000);

process.on("SIGTERM", () => void micaforge.shutdown().then(() => process.exit(0)));
```

---

## The log shipper

```
micaforge-shipper [options] <access.log>
micaforge-shipper [options] -            read from stdin
```

It tails an nginx, Caddy or Apache access log, or reads a pipe. It parses the combined,
common and JSON formats, classifies each line's user agent, and posts the agent fetches
in batches.

| Option | What it does |
|---|---|
| `-H, --host <url>` | Micaforge origin. Env `MICAFORGE_HOST`. |
| `-s, --site <id>` | Numeric site id. Env `MICAFORGE_SITE_ID`. |
| `-k, --key <key>` | Ingest key. Env `MICAFORGE_INGEST_KEY`. |
| `--key-file <path>` | Read the key from a file, so it is not visible in `ps`. |
| `-f, --format <name>` | `auto`, `combined`, `common` or `json`. Default `auto`. |
| `--hostname <host>` | Host to file hits under when the format has no host field. |
| `--from-beginning` | Start at byte 0 instead of the end. |
| `--no-follow` | Read to the end, ship, exit. The mode for cron and backfills. |
| `--state <path>` | Where the read position is remembered. |
| `--no-state` | Do not read or write a state file. |
| `--include-humans` | Also ship unnamed traffic as pageviews. |
| `--exclude <glob>` | Skip matching paths. Repeatable. |
| `--methods <list>` | Default `GET,HEAD`. |
| `-n, --dry-run` | Parse, classify, count. Send nothing. |
| `-v, --verbose` / `-q, --quiet` | |

`--help` has the rest, with examples for systemd, cron, `journalctl` and Docker.

**Rotation is handled.** The file is held open by descriptor, so when logrotate moves it
aside the last lines are read out of the old file before the new one is opened.
`copytruncate` is handled too, both while it is running and across a restart. No
`postrotate` hook is needed, and restarting the shipper inside one is the one thing that
can lose lines.

**Resumption is handled.** The state file records the byte after the last complete line
that was actually delivered, along with the file's inode and a hash of its first bytes,
so a restart neither replays nor skips.

---

## What goes on the wire

One `POST` to `/api/log/edge`, signed, with two arrays named after the two ClickHouse
tables they land in.

```jsonc
{
  "site_id": 1,
  "agent_events": [
    {
      "timestamp": "2026-08-21T10:00:00.000Z",
      "user_agent": "Mozilla/5.0 (compatible; ClaudeBot/1.0)",
      "method": "GET",
      "hostname": "docs.example.com",
      "pathname": "/docs/guide",
      "querystring": "a=1",
      "status": 200,
      "bytes": 4210,
      "duration_ms": 42,
      "content_type": "text/html",
      "conditional": 1,
      "ip": "203.0.113.9"
    }
  ],
  "events": [
    {
      "timestamp": "2026-08-21T10:00:05.000Z",
      "kind": "pageview",
      "hostname": "docs.example.com",
      "pathname": "/pricing",
      "referrer": "https://www.perplexity.ai/",
      "sdk_version": "0.1.0",
      "user_agent": "Mozilla/5.0 (X11; Linux x86_64) Chrome/128.0",
      "ip": "198.51.100.4"
    }
  ]
}
```

Every key is a column in `docs/CONTRACT.md` section 1.1 or 1.2, with two exceptions:
`user_agent` and `ip` on an events row are transport fields. The server reads them to
derive the columns it does have and writes down neither raw address.

### What is deliberately absent

An agent row never carries `agent_id`, `operator`, `purpose`, `verified`, `asn`,
`asn_org`, `country`, `ip_hash`, `robots_allowed` or `llms_listed`. Every one of those is
a fact the server establishes against the catalog, the operator's published IP ranges and
your own policy files. A client guessing at them would be a number this product did not
earn.

The referrer travels unresolved for the same reason. Deciding that
`https://www.perplexity.ai/` is an `answer_engine` arrival rather than a `referral` is
the server's job, and it is the half of the Citation Gap that faces the reader.

### The signature

```
x-micaforge-signature: t=1787306400,v1=<64 hex characters>
```

`t` is the Unix time in whole seconds at which the request was signed. `v1` is
HMAC-SHA256, keyed with the site's ingest key, over the string `${t}.${body}`: the same
digits as `t`, one `.`, then the raw request body. The body is serialised once and those
exact bytes are both signed and sent, so the two can never drift apart.

The collector refuses a `t` more than five minutes from its own clock, and answers `409`
to an exact repeat of a signature it has already accepted. Because `t` is signed, a
captured request cannot be replayed later under a new timestamp. Keep the shipping host's
clock in sync (NTP), and sign every attempt afresh; the SDK and the shipper both do.

Verifying it yourself is a few lines:

```ts
import { createHmac, timingSafeEqual } from "node:crypto";

const [, t, v1] = /^t=(\d+),v1=([0-9a-f]{64})$/.exec(signature) ?? [];
const fresh = Math.abs(Date.now() / 1000 - Number(t)) <= 300;
const expected = createHmac("sha256", ingestKey).update(`${t}.${rawBody}`, "utf8").digest("hex");
const ok = fresh && timingSafeEqual(Buffer.from(expected), Buffer.from(v1));
```

---

## Options

| Option | Default | |
|---|---|---|
| `host` | required | Your Micaforge origin. A path prefix is kept. |
| `siteId` | required | The numeric site id. |
| `ingestKey` | required | The site's ingest key. A server-side secret. |
| `flushInterval` | `200` | Milliseconds a partial batch waits. |
| `maxBatch` | `50` | Send as soon as this many are queued. |
| `timeout` | `10000` | Milliseconds before a send is abandoned. |
| `retries` | `2` | Retries for a timeout, a `429` or a `5xx`. A `4xx` is not retried. |
| `logger` | `console` | Your own logger, or `null` to silence it. |
| `enabled` | `true` | `false` builds every event and sends none. |
| `flushOnExit` | `true` | Hooks `beforeExit`. Signals still need `shutdown()`. |
| `trustProxy` | `true` | Read the client address from the usual proxy headers. |
| `excludePaths` | none | Globs. `*` is one segment, `**` is any depth. |
| `fetch` | global | Replace the transport, for a proxy agent or a test. |

Middleware options, for `express()`, `middleware()`, the Fastify plugin, the Hono
middleware and the Next middleware:

| Option | Default | |
|---|---|---|
| `exclude` | none | Path globs, on top of `excludePaths`. |
| `humans` | `true` | `false` reports only agents, for when the tracker is on the page. |
| `methods` | `["GET","HEAD"]` | A crawl is a read. A `POST` to your API is not a page fetch. |
| `countBytes` | `false` | Wrap `write`/`end` to size a chunked response exactly. |

Nothing is excluded by default, on purpose. An agent fetching `/sitemap.xml` or an `.md`
file is a real crawl, and filtering assets away would understate the taken side of the
Citation Gap. Exclude a health check, not a content type.

---

## Privacy

- The client address is sent so the server can resolve geography and check the fetch
  against an operator's published IP ranges. Micaforge stores a salted, rotating hash and
  never the address itself. No raw IP is persisted on a human event row.
- No cookies. Nothing is stored on the client, because there is no client.
- `identifiedId` is opt-in and is only ever an id you already own.
- The self-hosted build makes no outbound call at runtime, this package included: it
  talks to your Micaforge and to nothing else.

---

## Development

```bash
pnpm --filter @micaforge/sdk-server build       # tsc to dist/
pnpm --filter @micaforge/sdk-server typecheck
pnpm --filter @micaforge/sdk-server test
```

The shipper is deliberately standalone: zero imports outside `node:` builtins, and no
import of this package's own `dist/`. That is what lets it run from `npx` on a web server
with no build step. The cost is that it keeps its own copy of the agent pattern mirror in
`src/agents.ts`. Both are filters, both name the server catalog as their source, and the
server overrules both.

---

MIT. The Micaforge server is AGPL-3.0.
