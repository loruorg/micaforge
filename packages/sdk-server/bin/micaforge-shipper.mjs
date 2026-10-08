#!/usr/bin/env node
/**
 * micaforge-shipper: read an access log, ship the agent traffic to Micaforge.
 *
 * AI crawlers do not run JavaScript, so a script tag can never see them. Your web
 * server already wrote every one of those fetches down. This turns that file into the
 * agent side of the Citation Gap.
 *
 * # Deliberately standalone
 *
 * Zero imports outside `node:` builtins, and no import of this package's own `dist/`.
 * That is what lets it run straight from `npx @micaforge/sdk-server micaforge-shipper`
 * on a web server that has no build step, no bundler and no reason to acquire either.
 * The cost is the small pattern list below, which is a second copy of the mirror in
 * `src/agents.ts`. Both are filters; the server decides.
 *
 * The parsers and the signer are exported so the package's tests can import this file.
 * Importing it runs nothing when `MICAFORGE_SHIPPER_NO_MAIN=1` is set, which only the
 * tests do.
 */
import { createHmac } from "node:crypto";
import { open, stat, readFile, writeFile, rename, mkdir } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { StringDecoder } from "node:string_decoder";
import process from "node:process";

const VERSION = "0.1.0";
const EDGE_PATH = "/api/log/edge";
const SIGNATURE_HEADER = "x-micaforge-signature";

/* ========================================================================== *
 * The agent mirror
 *
 * A deliberately small, deliberately partial copy of the server's catalog, which lives
 * at `apps/backend/crates/agents/src/catalog.rs` and is the only thing allowed to
 * decide what an agent IS. Every token here is copied from there; none is invented.
 *
 * This list answers one question: is this line worth sending? The server answers all
 * the others: which agent, which operator, what purpose, whether the source address
 * really belongs to that operator, and whether robots.txt allowed the fetch. A line
 * this list does not recognise is not silently dropped when --include-humans is on; it
 * is sent as a pageview and the server reclassifies it. Being out of date is safe.
 * ========================================================================== */
const MIRROR = [
  ["gptbot", "gptbot", "openai"],
  ["oai-searchbot", "oai-searchbot", "openai"],
  ["chatgpt-user", "chatgpt-user", "openai"],
  ["claudebot", "claudebot", "anthropic"],
  ["claude-user", "claude-user", "anthropic"],
  ["claude-searchbot", "claude-searchbot", "anthropic"],
  ["anthropic-ai", "anthropic-ai", "anthropic"],
  ["claude-web", "claude-web", "anthropic"],
  ["perplexitybot", "perplexitybot", "perplexity"],
  ["perplexity-user", "perplexity-user", "perplexity"],
  ["googlebot", "googlebot", "google"],
  ["googleother", "googleother", "google"],
  ["google-cloudvertexbot", "google-cloudvertexbot", "google"],
  ["google-inspectiontool", "google-inspectiontool", "google"],
  ["bingbot", "bingbot", "microsoft"],
  ["bingpreview", "bingpreview", "microsoft"],
  ["meta-externalagent", "meta-externalagent", "meta"],
  ["meta-externalfetcher", "meta-externalfetcher", "meta"],
  ["facebookbot", "facebookbot", "meta"],
  ["facebookexternalhit", "facebookexternalhit", "meta"],
  ["applebot-extended", "applebot-extended", "apple"],
  ["amazonbot", "amazonbot", "amazon"],
  ["bytespider", "bytespider", "bytedance"],
  ["ccbot", "ccbot", "common-crawl"],
  ["cohere-training-data-crawler", "cohere-training-data-crawler", "cohere"],
  ["mistralai-user", "mistralai-user", "mistral"],
  ["duckassistbot", "duckassistbot", "duckduckgo"],
  ["ai2bot", "ai2bot", "allen-institute"],
  ["diffbot", "diffbot", "diffbot"],
  ["firecrawlagent", "firecrawl", "firecrawl"],
  ["imagesiftbot", "imagesiftbot", "imagesift"],
  ["semrushbot-ocob", "semrushbot-ocob", "semrush"],
  ["semrushbot", "semrushbot", "semrush"],
  ["ahrefsbot", "ahrefsbot", "ahrefs"],
  ["baiduspider", "baiduspider", "baidu"],
  ["duckduckbot", "duckduckbot", "duckduckgo"],
  ["twitterbot", "twitterbot", "x"],
  ["linkedinbot", "linkedinbot", "linkedin"],
  ["discordbot", "discordbot", "discord"],
  ["telegrambot", "telegrambot", "telegram"],
]
  // Longest token first. `semrushbot` is a substring of `semrushbot-ocob`, so
  // first-match-in-list-order would be wrong about half the time. Ties break
  // alphabetically so the result never depends on the sort being stable. Same rule as
  // the server's `find_by_ua`.
  .sort((a, b) => b[0].length - a[0].length || (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));

/** Name the client behind a user agent, or null for "the mirror does not know". */
function classify(userAgent) {
  if (!userAgent) return null;
  const haystack = userAgent.toLowerCase();
  for (const [token, slug, operator] of MIRROR) {
    if (haystack.includes(token)) return { slug, operator };
  }
  return null;
}

/* ========================================================================== *
 * Help
 * ========================================================================== */

const HELP = `micaforge-shipper ${VERSION}

Read a web server access log and ship the AI crawler traffic in it to Micaforge.

AI crawlers do not run JavaScript, so the tracker on your pages cannot see them.
Your access log already has every one of those fetches. This reads that file,
picks out the requests made by named agents, and posts them to Micaforge, where
they become the "taken" side of the Citation Gap.

It reads. It never writes to your log, and it never changes your web server.

USAGE
  micaforge-shipper [options] <access.log>
  micaforge-shipper [options] -              read from stdin
  tail -F access.log | micaforge-shipper [options] -

QUICK START
  export MICAFORGE_HOST=https://micaforge.example.com
  export MICAFORGE_SITE_ID=1
  export MICAFORGE_INGEST_KEY=...

  # See what a week of logs holds, without sending anything.
  micaforge-shipper --dry-run --from-beginning --no-follow /var/log/nginx/access.log

  # Then run it for real, following the file as it grows.
  micaforge-shipper /var/log/nginx/access.log

CONNECTION
  -H, --host <url>        Your Micaforge origin, e.g. https://mf.example.com
                          Env: MICAFORGE_HOST
  -s, --site <id>         Numeric site id.        Env: MICAFORGE_SITE_ID
  -k, --key <key>         Site ingest key.        Env: MICAFORGE_INGEST_KEY
      --key-file <path>   Read the ingest key from a file instead. Safer than
                          an argument: arguments are visible in "ps".

READING
  -f, --format <name>     auto | combined | common | json     (default: auto)
                          auto sniffs the first line it can read.
      --hostname <host>   File hits under this host when the log line has no
                          host field. Combined format has none, so set this if
                          you serve more than one site from one log.
      --from-beginning    Start at byte 0. Without it a file is read from the
                          end, so a first run does not replay your whole
                          history. Ignored when a state file already has a
                          position.
      --no-follow         Read to the end of the file, ship, and exit. This is
                          the mode for a cron job or a one-off backfill.
      --poll <ms>         How often to check the file for new bytes.
                          (default: 1000)
      --state <path>      Where to remember the read position, so a restart
                          resumes instead of replaying or skipping.
                          (default: alongside the log, <name>.micaforge-state)
      --no-state          Do not read or write a state file.

FILTERING
      --include-humans    Also ship requests the mirror could not name, as
                          human pageviews. Leave this off when the browser
                          tracker is on your pages: it already counts them, and
                          both would count every visit twice.
      --exclude <glob>    Skip matching paths. Repeatable. "*" is one segment,
                          "**" is any depth: --exclude '/_next/**'
      --methods <list>    Comma separated. (default: GET,HEAD)
                          A crawl is a read; a POST to your API is not a page
                          fetch and does not belong in the page report.

SENDING
      --batch <n>         Hits per request.       (default: 50)
      --interval <ms>     Longest a partial batch waits.  (default: 2000)
      --timeout <ms>      Give up on one request after this. (default: 10000)
      --retries <n>       Retries for a timeout, a 429 or a 5xx. (default: 2)

OUTPUT
  -n, --dry-run           Parse, classify and count. Send nothing. Prints a
                          summary of what would have gone, by agent.
  -v, --verbose           One line per hit.
  -q, --quiet             Errors only.
      --version           Print the version and exit.
  -h, --help              This.

LOG FORMATS
  combined   The nginx and Apache default. Carries the user agent, which is the
             one field this tool cannot work without.
               10.0.0.1 - - [21/Aug/2026:10:00:00 +0000] "GET /docs HTTP/1.1"
               200 4210 "https://example.com/" "Mozilla/5.0 (compatible; GPTBot/1.2)"

  common     The same without the referrer and user agent. There is nothing to
             classify, so every line is counted as skipped and nothing is sent.
             The shipper says so once. Switch nginx to the combined format, or
             use the log_format in the README.

  json       Caddy's structured log, and any JSON log with recognisable keys.
             Caddy needs no configuration: "log { format json }" is enough.

  A hostname, a status, a byte count, a response time and a content type are all
  used when the format has them and left at the column default when it does not.
  Nothing is guessed.

WHAT IS SENT
  One row per agent fetch: the timestamp, the user agent, the method, the host,
  the path, the query string, the status, the byte count, the response time, the
  content type, and the source address.

  The source address is sent so the server can check it against the operator's
  published IP ranges, which is what marks a fetch verified rather than merely
  claimed. Micaforge stores only a salted, rotating hash of it. The raw address
  is never written down.

  Which agent it was, which operator runs it, what the crawl is for, whether it
  was verified, and whether your robots.txt allowed it are all decided by the
  server against the full catalog. This tool does not guess at any of them.

EXAMPLES
  # Backfill three months of rotated logs, then exit.
  zcat /var/log/nginx/access.log.*.gz | micaforge-shipper --no-follow -

  # Follow a Caddy JSON log as a systemd service.
  micaforge-shipper --format json /var/log/caddy/access.log

  # Follow the journal for a service that logs to stdout.
  journalctl -u web -f -o cat | micaforge-shipper --format json --no-state -

  # One site out of a shared log, ignoring build assets.
  micaforge-shipper --hostname docs.example.com --exclude '/assets/**' \\
    /var/log/nginx/access.log

EXIT CODES
  0  finished, or stopped cleanly on SIGINT or SIGTERM
  1  bad usage or bad configuration
  2  could not read the log

Micaforge is AGPL-3.0. This tool is MIT. https://github.com/loruorg/micaforge
`;

/* ========================================================================== *
 * Arguments
 * ========================================================================== */

const FLAGS = new Set([
  "--from-beginning",
  "--no-follow",
  "--no-state",
  "--include-humans",
  "--dry-run",
  "-n",
  "--verbose",
  "-v",
  "--quiet",
  "-q",
  "--help",
  "-h",
  "--version",
]);

const ALIASES = {
  "-H": "--host",
  "-s": "--site",
  "-k": "--key",
  "-f": "--format",
  "-n": "--dry-run",
  "-v": "--verbose",
  "-q": "--quiet",
  "-h": "--help",
};

function parseArgs(argv) {
  const options = {
    host: process.env.MICAFORGE_HOST ?? "",
    site: process.env.MICAFORGE_SITE_ID ?? "",
    key: process.env.MICAFORGE_INGEST_KEY ?? "",
    keyFile: "",
    format: "auto",
    hostname: "",
    state: undefined,
    useState: true,
    fromBeginning: false,
    follow: true,
    poll: 1000,
    includeHumans: false,
    exclude: [],
    methods: "GET,HEAD",
    batch: 50,
    interval: 2000,
    timeout: 10000,
    retries: 2,
    dryRun: false,
    verbose: false,
    quiet: false,
    help: false,
    version: false,
    input: "",
  };

  for (let i = 0; i < argv.length; i++) {
    let arg = argv[i];
    if (!arg.startsWith("-") || arg === "-") {
      if (options.input) fail(`more than one input given: ${options.input} and ${arg}`);
      options.input = arg;
      continue;
    }
    // --name=value
    let inline;
    const eq = arg.indexOf("=");
    if (eq > 2) {
      inline = arg.slice(eq + 1);
      arg = arg.slice(0, eq);
    }
    const name = ALIASES[arg] ?? arg;
    const value = () => {
      if (inline !== undefined) return inline;
      const next = argv[++i];
      if (next === undefined) fail(`${arg} needs a value`);
      return next;
    };

    switch (name) {
      case "--host": options.host = value(); break;
      case "--site": options.site = value(); break;
      case "--key": options.key = value(); break;
      case "--key-file": options.keyFile = value(); break;
      case "--format": options.format = value().toLowerCase(); break;
      case "--hostname": options.hostname = value(); break;
      case "--state": options.state = value(); break;
      case "--no-state": options.useState = false; break;
      case "--from-beginning": options.fromBeginning = true; break;
      case "--no-follow": options.follow = false; break;
      case "--poll": options.poll = int(value(), "--poll"); break;
      case "--include-humans": options.includeHumans = true; break;
      case "--exclude": options.exclude.push(value()); break;
      case "--methods": options.methods = value(); break;
      case "--batch": options.batch = int(value(), "--batch"); break;
      case "--interval": options.interval = int(value(), "--interval"); break;
      case "--timeout": options.timeout = int(value(), "--timeout"); break;
      case "--retries": options.retries = int(value(), "--retries"); break;
      case "--dry-run": options.dryRun = true; break;
      case "--verbose": options.verbose = true; break;
      case "--quiet": options.quiet = true; break;
      case "--help": options.help = true; break;
      case "--version": options.version = true; break;
      default:
        if (FLAGS.has(arg)) break;
        fail(`unknown option ${arg}. Try --help.`);
    }
  }
  return options;
}

function int(raw, what) {
  const n = Number(raw);
  if (!Number.isFinite(n) || n < 0) fail(`${what} needs a number, got "${raw}"`);
  return Math.floor(n);
}

function fail(message) {
  process.stderr.write(`micaforge-shipper: ${message}\n`);
  process.exit(1);
}

/* ========================================================================== *
 * Parsing
 * ========================================================================== */

/**
 * The combined log format, as nginx and Apache write it by default.
 *
 * Quoted fields allow an escaped quote inside them, because nginx escapes a quote in a
 * user agent as `\x22` and Apache as `\"`. Getting that wrong truncates the user agent
 * and loses the classification, which is the one field that matters here.
 */
const COMBINED = new RegExp(
  "^(\\S+)\\s+(\\S+)\\s+(\\S+)\\s+\\[([^\\]]+)\\]\\s+" +
    '"((?:[^"\\\\]|\\\\.)*)"\\s+(\\d{3})\\s+(\\S+)' +
    '(?:\\s+"((?:[^"\\\\]|\\\\.)*)"\\s+"((?:[^"\\\\]|\\\\.)*)")?',
);

/** The common log format: the same line with no referrer and no user agent. */
const COMMON = new RegExp(
  "^(\\S+)\\s+(\\S+)\\s+(\\S+)\\s+\\[([^\\]]+)\\]\\s+" + '"((?:[^"\\\\]|\\\\.)*)"\\s+(\\d{3})\\s+(\\S+)\\s*$',
);

const CLF_MONTHS = {
  Jan: 0, Feb: 1, Mar: 2, Apr: 3, May: 4, Jun: 5,
  Jul: 6, Aug: 7, Sep: 8, Oct: 9, Nov: 10, Dec: 11,
};

/** `21/Aug/2026:10:00:00 +0000` to a Date. Returns null rather than an Invalid Date. */
function parseClfTime(raw) {
  const m = /^(\d{2})\/(\w{3})\/(\d{4}):(\d{2}):(\d{2}):(\d{2})\s*([+-]\d{4})?$/.exec(raw ?? "");
  if (!m) return null;
  const month = CLF_MONTHS[m[2]];
  if (month === undefined) return null;
  const utc = Date.UTC(+m[3], month, +m[1], +m[4], +m[5], +m[6]);
  const zone = m[7];
  if (!zone) return new Date(utc);
  const offset = (zone[0] === "-" ? 1 : -1) * (+zone.slice(1, 3) * 60 + +zone.slice(3, 5)) * 60000;
  return new Date(utc + offset);
}

/** Undo the escapes nginx and Apache put in a quoted field. */
function unescapeField(value) {
  if (!value || value.indexOf("\\") === -1) return value ?? "";
  return value.replace(/\\x([0-9a-fA-F]{2})/g, (_, hex) => String.fromCharCode(parseInt(hex, 16)))
    .replace(/\\"/g, '"')
    .replace(/\\\\/g, "\\");
}

/** Split `GET /docs?a=1 HTTP/1.1` into its three parts. */
function parseRequestLine(request) {
  const parts = (request ?? "").split(" ");
  if (parts.length < 2) return { method: "GET", target: parts[0] || "/" };
  return { method: parts[0].toUpperCase(), target: parts[1] || "/" };
}

/** A parsed line, or null when the line is not one. */
function parseCombined(line) {
  const m = COMBINED.exec(line);
  if (!m) return null;
  const { method, target } = parseRequestLine(unescapeField(m[5]));
  return {
    ip: m[1] === "-" ? "" : m[1],
    time: parseClfTime(m[4]),
    method,
    target,
    status: Number(m[6]),
    bytes: m[7] === "-" ? undefined : Number(m[7]),
    referrer: m[8] && m[8] !== "-" ? unescapeField(m[8]) : "",
    userAgent: m[9] && m[9] !== "-" ? unescapeField(m[9]) : "",
    host: "",
  };
}

function parseCommon(line) {
  const m = COMMON.exec(line);
  if (!m) return null;
  const { method, target } = parseRequestLine(unescapeField(m[5]));
  return {
    ip: m[1] === "-" ? "" : m[1],
    time: parseClfTime(m[4]),
    method,
    target,
    status: Number(m[6]),
    bytes: m[7] === "-" ? undefined : Number(m[7]),
    referrer: "",
    userAgent: "",
    host: "",
  };
}

/** The first present key, searched case-insensitively at one level. */
function pick(object, keys) {
  if (!object || typeof object !== "object") return undefined;
  for (const key of keys) {
    if (object[key] !== undefined && object[key] !== null && object[key] !== "") return object[key];
  }
  const lower = new Map(Object.keys(object).map((k) => [k.toLowerCase(), k]));
  for (const key of keys) {
    const actual = lower.get(key.toLowerCase());
    if (actual !== undefined) {
      const value = object[actual];
      if (value !== undefined && value !== null && value !== "") return value;
    }
  }
  return undefined;
}

function firstOf(value) {
  if (Array.isArray(value)) return value.length ? String(value[0]) : "";
  return value === undefined || value === null ? "" : String(value);
}

/**
 * Caddy's structured log, and any JSON log with recognisable keys.
 *
 * Caddy nests the request under `request` and the headers under
 * `request.headers`, with every header value an array because a header can repeat.
 * nginx's `escape=json` writes a flat object with `http_user_agent` and friends. Both
 * are read here rather than asking anyone to reshape their log.
 */
function parseJsonLine(line) {
  let row;
  try {
    row = JSON.parse(line);
  } catch {
    return null;
  }
  if (!row || typeof row !== "object") return null;

  const request = (typeof row.request === "object" && row.request) || {};
  const headers = (typeof request.headers === "object" && request.headers) || {};

  const userAgent =
    firstOf(pick(headers, ["User-Agent", "user-agent"])) ||
    firstOf(pick(row, ["http_user_agent", "user_agent", "useragent", "ua"]));

  const referrer =
    firstOf(pick(headers, ["Referer", "Referrer", "referer"])) ||
    firstOf(pick(row, ["http_referer", "http_referrer", "referer", "referrer"]));

  const target =
    firstOf(pick(request, ["uri", "url", "path"])) ||
    firstOf(pick(row, ["request_uri", "uri", "url", "path", "request"])) ||
    "/";

  const ts = pick(row, ["ts", "time", "timestamp", "time_local", "@timestamp"]);
  let time = null;
  if (typeof ts === "number") {
    // Caddy writes float seconds; a millisecond epoch is far larger than any
    // plausible second epoch, so the two are told apart by magnitude.
    time = new Date(ts > 1e11 ? ts : ts * 1000);
  } else if (typeof ts === "string") {
    const parsed = new Date(ts);
    time = Number.isNaN(parsed.getTime()) ? parseClfTime(ts) : parsed;
  }

  const duration = pick(row, ["duration", "request_time", "response_time", "latency"]);

  return {
    // `client_ip` first. Behind trusted proxies Caddy puts the real client there and
    // the proxy in `remote_ip`, and verifying a crawler against its operator's
    // published ranges only means something for the crawler's own address.
    ip:
      firstOf(pick(request, ["client_ip", "remote_ip", "remote_addr"])) ||
      firstOf(pick(row, ["client_ip", "remote_ip", "remote_addr", "ip"])),
    time,
    method: (firstOf(pick(request, ["method"])) || firstOf(pick(row, ["request_method", "method"])) || "GET").toUpperCase(),
    target: target.startsWith("/") || target.includes("://") ? target : parseRequestLine(target).target,
    status: Number(firstOf(pick(row, ["status", "status_code", "response_status"])) || 0) || undefined,
    bytes: numberOrUndefined(pick(row, ["size", "bytes", "body_bytes_sent", "bytes_sent", "response_size"])),
    durationMs: durationToMs(duration),
    contentType: firstOf(
      pick((typeof row.resp_headers === "object" && row.resp_headers) || {}, ["Content-Type", "content-type"]) ??
        pick(row, ["content_type", "sent_http_content_type"]),
    ),
    referrer,
    userAgent,
    host:
      firstOf(pick(request, ["host"])) ||
      firstOf(pick(row, ["host", "server_name", "http_host", "hostname"])),
  };
}

function numberOrUndefined(value) {
  if (value === undefined || value === null || value === "") return undefined;
  const n = Number(value);
  return Number.isFinite(n) ? n : undefined;
}

/** Caddy and nginx both report seconds as a float; a value in ms arrives as an integer. */
function durationToMs(value) {
  const n = numberOrUndefined(value);
  if (n === undefined) return undefined;
  return n < 1000 && !Number.isInteger(n) ? Math.round(n * 1000) : Math.round(n);
}

const PARSERS = { combined: parseCombined, common: parseCommon, json: parseJsonLine };

/** Decide a format from one real line. */
function sniff(line) {
  const trimmed = line.trim();
  if (!trimmed) return null;
  if (trimmed.startsWith("{")) return "json";
  if (COMBINED.test(trimmed)) {
    // The combined regex matches a common line too, because the referrer and user agent
    // group is optional. Tell them apart by whether that group was actually there.
    const m = COMBINED.exec(trimmed);
    return m && m[9] !== undefined ? "combined" : "common";
  }
  if (COMMON.test(trimmed)) return "common";
  return null;
}

/* ========================================================================== *
 * Paths and filtering
 * ========================================================================== */

function globToRegExp(glob) {
  return new RegExp(
    "^" +
      glob.replace(/\*\*|[*?]|[.+^${}()|[\]\\]/g, (m) =>
        m === "**" ? ".*" : m === "*" ? "[^/]*" : m === "?" ? "[^/]" : "\\" + m,
      ) +
      "$",
  );
}

/** Split a request target into the columns that hold it. Never throws. */
function splitTarget(target, fallbackHost) {
  try {
    const absolute = /^[a-z][a-z0-9+.-]*:\/\//i.test(target);
    const url = new URL(target, absolute ? undefined : "http://placeholder.invalid");
    return {
      hostname: absolute ? url.hostname : fallbackHost,
      pathname: url.pathname || "/",
      querystring: url.search.replace(/^\?/, ""),
    };
  } catch {
    const q = target.indexOf("?");
    return {
      hostname: fallbackHost,
      pathname: (q === -1 ? target : target.slice(0, q)) || "/",
      querystring: q === -1 ? "" : target.slice(q + 1),
    };
  }
}

/* ========================================================================== *
 * Sending
 * ========================================================================== */

/**
 * `t=<unix seconds>,v1=<hex>`, where `v1` is HMAC-SHA256 over `${t}.${body}` keyed with
 * the ingest key. CONTRACT.md 3.3. Called once per attempt, so a retry carries a fresh
 * `t` and is never mistaken for a replay.
 */
function signBody(ingestKey, body, atSeconds) {
  const t = Math.floor(atSeconds ?? Date.now() / 1000);
  const v1 = createHmac("sha256", ingestKey).update(`${t}.`, "utf8").update(body, "utf8").digest("hex");
  return `t=${t},v1=${v1}`;
}

class Sender {
  constructor(config, log) {
    this.config = config;
    this.log = log;
    this.endpoint = config.host.replace(/\/+$/, "") + EDGE_PATH;
    this.events = [];
    this.agents = [];
    this.sent = 0;
    this.dropped = 0;
  }

  get size() {
    return this.events.length + this.agents.length;
  }

  addAgent(row) {
    this.agents.push(row);
  }

  addEvent(row) {
    this.events.push(row);
  }

  /** Send everything queued. Resolves true when the collector took it. */
  async flush() {
    if (this.size === 0) return true;
    const payload = { site_id: this.config.siteId };
    if (this.events.length) payload.events = this.events;
    if (this.agents.length) payload.agent_events = this.agents;
    const count = this.size;
    this.events = [];
    this.agents = [];

    if (this.config.dryRun) {
      this.sent += count;
      return true;
    }

    const body = JSON.stringify(payload);
    for (let attempt = 0; attempt <= this.config.retries; attempt++) {
      const controller = new AbortController();
      const abort = setTimeout(() => controller.abort(), this.config.timeout);
      try {
        const response = await fetch(this.endpoint, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            "user-agent": `micaforge-shipper/${VERSION}`,
            [SIGNATURE_HEADER]: signBody(this.config.ingestKey, body),
          },
          body,
          signal: controller.signal,
        });
        if (response.ok) {
          this.sent += count;
          this.log.debug(`sent ${count} hit(s)`);
          return true;
        }
        if (response.status === 401 || response.status === 403) {
          this.log.error(
            `the collector rejected the signature (${response.status}). Check --key ` +
              `against the site's ingest key, and --site against its numeric id.`,
          );
          this.dropped += count;
          return false;
        }
        if (response.status < 500 && response.status !== 429) {
          const detail = await response.text().catch(() => "");
          this.log.error(`the collector returned ${response.status}: ${detail.slice(0, 300)}`);
          this.dropped += count;
          return false;
        }
        if (attempt === this.config.retries) {
          this.log.error(`the collector returned ${response.status}; dropped ${count} hit(s)`);
          this.dropped += count;
          return false;
        }
      } catch (error) {
        if (attempt === this.config.retries) {
          this.log.error(`could not reach ${this.endpoint}: ${error?.message ?? error}`);
          this.dropped += count;
          return false;
        }
      } finally {
        clearTimeout(abort);
      }
      await sleep(Math.min(250 * 2 ** attempt, 5000));
    }
    return false;
  }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/* ========================================================================== *
 * State
 * ========================================================================== */

/**
 * Remember where we were.
 *
 * `ino` and `dev` are stored beside the offset so that a rotated file is recognised as
 * a different file rather than resumed at an offset that now points into the middle of
 * a new line. Written by rename, so a kill in the middle leaves the old state intact
 * rather than a truncated one.
 */
async function readState(path) {
  if (!path) return null;
  try {
    const raw = await readFile(path, "utf8");
    const state = JSON.parse(raw);
    return typeof state?.offset === "number" ? state : null;
  } catch {
    return null;
  }
}

/**
 * A hash of the first bytes of the file, stored beside the offset.
 *
 * The inode catches a rotation. It does not catch a truncation that happened while the
 * shipper was not running: `logrotate copytruncate` empties the file in place, so the
 * inode is unchanged and the new content can already be longer than the saved offset by
 * the time the shipper starts again. Resuming at that offset would land in the middle
 * of a line and ship a fragment. Comparing the head of the file catches it: different
 * head, different content, start again from zero.
 */
async function fingerprint(handle, size) {
  if (size <= 0) return "";
  const want = Math.min(256, size);
  const buffer = Buffer.allocUnsafe(want);
  const { bytesRead } = await handle.read(buffer, 0, want, 0);
  return createHmac("sha256", "micaforge-shipper-head")
    .update(buffer.subarray(0, bytesRead))
    .digest("hex")
    .slice(0, 32);
}

async function writeState(path, state) {
  if (!path) return;
  try {
    await mkdir(dirname(path), { recursive: true });
    const temporary = `${path}.tmp`;
    await writeFile(temporary, JSON.stringify(state), "utf8");
    await rename(temporary, path);
  } catch {
    // A state file that cannot be written is a restart that replays or skips, not a
    // reason to stop shipping. The warning is printed once, by the caller.
  }
}

/* ========================================================================== *
 * The run
 * ========================================================================== */

function makeLogger(options) {
  const write = (stream, message) => stream.write(`${message}\n`);
  return {
    debug: (message) => options.verbose && !options.quiet && write(process.stderr, `      ${message}`),
    info: (message) => !options.quiet && write(process.stderr, message),
    warn: (message) => !options.quiet && write(process.stderr, `warning: ${message}`),
    error: (message) => write(process.stderr, `error: ${message}`),
    out: (message) => write(process.stdout, message),
  };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    process.stdout.write(HELP);
    return 0;
  }
  if (options.version) {
    process.stdout.write(`${VERSION}\n`);
    return 0;
  }

  const log = makeLogger(options);

  if (options.keyFile) {
    try {
      options.key = (await readFile(resolve(options.keyFile), "utf8")).trim();
    } catch (error) {
      fail(`could not read --key-file ${options.keyFile}: ${error?.message ?? error}`);
    }
  }

  if (!options.input) fail("no log file given. Pass a path, or - for stdin. Try --help.");
  if (!options.dryRun) {
    if (!options.host) fail("no --host. Pass it, or set MICAFORGE_HOST.");
    if (!options.site) fail("no --site. Pass it, or set MICAFORGE_SITE_ID.");
    if (!options.key) fail("no --key. Pass it, --key-file, or set MICAFORGE_INGEST_KEY.");
  }
  const siteId = Number(options.site || 0);
  if (!options.dryRun && (!Number.isInteger(siteId) || siteId < 0 || siteId > 65535)) {
    fail(`--site must be an integer from 0 to 65535, got "${options.site}"`);
  }
  if (options.format !== "auto" && !PARSERS[options.format]) {
    fail(`unknown --format "${options.format}". Use auto, combined, common or json.`);
  }

  const config = {
    host: options.host || "https://micaforge.invalid",
    siteId,
    ingestKey: options.key,
    dryRun: options.dryRun,
    timeout: options.timeout,
    retries: options.retries,
  };

  const sender = new Sender(config, log);
  const excluded = options.exclude.map(globToRegExp);
  const methods = options.methods
    .split(",")
    .map((m) => m.trim().toUpperCase())
    .filter(Boolean);

  const counts = { lines: 0, agents: 0, humans: 0, skipped: 0, unparsed: 0 };
  const byAgent = new Map();
  let format = options.format === "auto" ? null : options.format;
  let warnedCommon = false;

  /** Turn one line into a queued row. */
  const handle = (line) => {
    if (!line.trim()) return;
    counts.lines++;

    if (!format) {
      format = sniff(line);
      if (!format) {
        counts.unparsed++;
        return;
      }
      log.debug(`detected format: ${format}`);
    }

    const parsed = PARSERS[format](line);
    if (!parsed) {
      counts.unparsed++;
      return;
    }

    if (!parsed.userAgent) {
      if (format === "common" && !warnedCommon) {
        warnedCommon = true;
        log.error(
          "this log is in the common format, which has no user agent field. There is " +
            "nothing to classify. Switch the server to the combined format; the README " +
            "in src/nginx has a log_format you can paste.",
        );
      }
      counts.skipped++;
      return;
    }

    if (methods.length && !methods.includes(parsed.method)) {
      counts.skipped++;
      return;
    }

    const parts = splitTarget(parsed.target, parsed.host || options.hostname);
    if (excluded.some((re) => re.test(parts.pathname))) {
      counts.skipped++;
      return;
    }

    const timestamp = (parsed.time ?? new Date()).toISOString();
    const agent = classify(parsed.userAgent);

    if (agent) {
      counts.agents++;
      byAgent.set(agent.slug, (byAgent.get(agent.slug) ?? 0) + 1);
      sender.addAgent(
        drop({
          timestamp,
          user_agent: parsed.userAgent,
          method: parsed.method,
          hostname: parts.hostname,
          pathname: parts.pathname,
          querystring: parts.querystring,
          status: parsed.status,
          bytes: parsed.bytes,
          duration_ms: parsed.durationMs,
          content_type: parsed.contentType ? parsed.contentType.split(";")[0].trim() : undefined,
          ip: parsed.ip,
        }),
      );
      if (options.verbose) {
        log.debug(`${agent.slug.padEnd(24)} ${parsed.method} ${parts.pathname}`);
      }
    } else if (options.includeHumans) {
      counts.humans++;
      sender.addEvent(
        drop({
          timestamp,
          kind: "pageview",
          hostname: parts.hostname,
          pathname: parts.pathname,
          querystring: parts.querystring,
          referrer: parsed.referrer,
          sdk_version: `shipper/${VERSION}`,
          user_agent: parsed.userAgent,
          ip: parsed.ip,
        }),
      );
    } else {
      counts.skipped++;
    }
  };

  /* ---------------------------------------------------------------- the loop */

  let stopping = false;
  const stop = () => {
    if (!stopping) log.debug("stopping; flushing what is queued");
    stopping = true;
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);

  /* -- stdin ------------------------------------------------------------- *
   * A pipe has no size, no inode and no end to seek to, so there is no state to
   * keep and no rotation to notice. It ends when the writer stops.
   * ---------------------------------------------------------------------- */
  if (options.input === "-") {
    process.stdin.setEncoding("utf8");
    let carry = "";
    try {
      for await (const chunk of process.stdin) {
        if (stopping) break;
        carry += chunk;
        const lines = carry.split("\n");
        carry = lines.pop() ?? "";
        for (const line of lines) {
          handle(line);
          if (sender.size >= options.batch) await sender.flush();
        }
      }
      if (carry) handle(carry);
    } catch (error) {
      log.error(`reading stdin failed: ${error?.message ?? error}`);
      await sender.flush();
      summarise(log, options, counts, byAgent, sender);
      return 2;
    }
    await sender.flush();
    summarise(log, options, counts, byAgent, sender);
    return 0;
  }

  /* -- a file ------------------------------------------------------------ *
   * The file is held open by descriptor, not reopened by path on every pass.
   * That is what makes rotation correct: when logrotate moves the file aside,
   * the open descriptor still points at the bytes that were written to it, so
   * the last lines are read before the new file is picked up. Reopening by path
   * each time would read those offsets out of the new file, which is a
   * different file, and ship whatever happened to be sitting at that byte.
   * ---------------------------------------------------------------------- */
  const file = resolve(options.input);
  let onDisk;
  try {
    onDisk = await stat(file);
  } catch (error) {
    log.error(`cannot read ${file}: ${error?.message ?? error}`);
    return 2;
  }

  const statePath =
    options.useState === false
      ? null
      : (options.state ?? join(dirname(file), `.${basename(file)}.micaforge-state`));

  let handle_ = await open(file, "r");
  let identity = await handle_.stat();
  let offset;

  const saved = await readState(statePath);
  const head = await fingerprint(handle_, identity.size);
  const resumable =
    saved &&
    saved.ino === identity.ino &&
    saved.dev === identity.dev &&
    saved.offset <= identity.size &&
    (saved.head === undefined || saved.head === head);

  if (resumable) {
    offset = saved.offset;
    log.debug(`resuming at byte ${offset} of ${identity.size}`);
  } else if (saved && saved.ino === identity.ino && saved.head !== head) {
    log.debug("the log was replaced or truncated while this was not running; reading it from the start");
    offset = 0;
  } else if (options.fromBeginning) {
    offset = 0;
  } else {
    offset = identity.size;
    if (!saved) {
      log.info(
        `following ${file} from its end, so nothing already written is sent. ` +
          `Use --from-beginning to read the whole file first.`,
      );
    } else {
      log.debug("the saved position belongs to a different file; starting at the end");
    }
  }

  // The byte after the last COMPLETE line that has been shipped. The state file only
  // ever records this, never the raw read position, so a restart cannot resume in the
  // middle of a line and cannot skip one that was read but never delivered.
  let committed = offset;
  let carry = "";
  const decoder = new StringDecoder("utf8");
  const buffer = Buffer.allocUnsafe(64 * 1024);

  const saveState = async () => {
    if (!statePath || options.dryRun) return;
    await writeState(statePath, {
      file,
      ino: identity.ino,
      dev: identity.dev,
      head: await fingerprint(handle_, identity.size).catch(() => ""),
      offset: committed,
      updated: new Date().toISOString(),
    });
  };

  /** Read from `offset` up to `size` on the open descriptor, one buffer at a time. */
  const drain = async (size) => {
    while (offset < size && !stopping) {
      const want = Math.min(buffer.length, size - offset);
      const { bytesRead } = await handle_.read(buffer, 0, want, offset);
      if (bytesRead <= 0) break;
      offset += bytesRead;
      carry += decoder.write(buffer.subarray(0, bytesRead));
      const lines = carry.split("\n");
      carry = lines.pop() ?? "";
      for (const line of lines) handle(line);
      // A partial trailing line is not committed: its bytes are read but its content
      // is not known yet.
      committed = offset - Buffer.byteLength(carry, "utf8");
      if (sender.size >= options.batch) {
        if (await sender.flush()) await saveState();
      }
    }
  };

  /** One pass: read what is new, and notice truncation and rotation. */
  const tick = async () => {
    identity = await handle_.stat();

    if (identity.size < offset) {
      // Truncated in place, which is what `logrotate copytruncate` and `> file` do.
      log.debug("the log was truncated; reading it from the beginning again");
      offset = 0;
      committed = 0;
      carry = "";
      decoder.end(); // Drop any half-decoded multi-byte character from the old content.
    }
    await drain(identity.size);

    let current = null;
    try {
      current = await stat(file);
    } catch {
      // Moved aside and not replaced yet. Keep the descriptor and look again next pass.
      return;
    }
    if (current.ino !== identity.ino || current.dev !== identity.dev) {
      log.debug("the log was rotated; finishing the old file and opening the new one");
      await drain((await handle_.stat()).size);
      // The last line of a rotated file has no newline after it, but it is complete.
      const tail = carry + decoder.end();
      if (tail) handle(tail);
      carry = "";
      await handle_.close().catch(() => {});
      handle_ = await open(file, "r");
      identity = await handle_.stat();
      offset = 0;
      committed = 0;
    }
  };

  const finish = async (code) => {
    if (!options.follow && carry) {
      handle(carry + decoder.end());
      carry = "";
      committed = offset;
    }
    await sender.flush();
    await saveState();
    await handle_.close().catch(() => {});
    summarise(log, options, counts, byAgent, sender);
    return code;
  };

  try {
    await tick();
  } catch (error) {
    log.error(`reading ${file} failed: ${error?.message ?? error}`);
    return await finish(2);
  }

  if (!options.follow) return await finish(0);

  const poll = Math.max(100, options.poll);
  let lastFlush = Date.now();
  while (!stopping) {
    await sleep(poll);
    if (stopping) break;
    try {
      await tick();
    } catch (error) {
      log.warn(`reading ${file} failed: ${error?.message ?? error}`);
    }
    if (sender.size > 0 && Date.now() - lastFlush >= options.interval) {
      const ok = await sender.flush();
      lastFlush = Date.now();
      if (ok) await saveState();
    }
  }

  return await finish(0);
}

/** Drop keys the collector would only replace with the column's own DEFAULT. */
function drop(row) {
  for (const key of Object.keys(row)) {
    const value = row[key];
    if (value === undefined || value === null || value === "" || (typeof value === "number" && !Number.isFinite(value))) {
      delete row[key];
    }
  }
  return row;
}

function summarise(log, options, counts, byAgent, sender) {
  if (options.quiet) return;
  const lines = [];
  lines.push("");
  lines.push(options.dryRun ? "Dry run. Nothing was sent." : "Done.");
  lines.push(`  lines read        ${counts.lines}`);
  lines.push(`  agent fetches     ${counts.agents}`);
  if (options.includeHumans) lines.push(`  human pageviews   ${counts.humans}`);
  lines.push(`  skipped           ${counts.skipped}`);
  if (counts.unparsed) lines.push(`  unparsed lines    ${counts.unparsed}`);
  if (!options.dryRun) {
    lines.push(`  delivered         ${sender.sent}`);
    if (sender.dropped) lines.push(`  dropped           ${sender.dropped}`);
  }

  if (byAgent.size) {
    lines.push("");
    lines.push("  By agent, as this tool's mirror named it. The server decides the");
    lines.push("  operator, the purpose and whether the fetch could be verified.");
    lines.push("");
    const sorted = [...byAgent.entries()].sort((a, b) => b[1] - a[1]);
    const width = Math.max(...sorted.map(([slug]) => slug.length));
    for (const [slug, count] of sorted) {
      lines.push(`    ${slug.padEnd(width)}  ${String(count).padStart(7)}`);
    }
  } else if (counts.lines > 0) {
    lines.push("");
    lines.push("  No named agents in this log. That is a real answer, not an error:");
    lines.push("  it means nothing in the catalog fetched these pages in this window.");
  }
  lines.push("");
  log.info(lines.join("\n"));
}

export { parseCombined, parseCommon, parseJsonLine, signBody, sniff };

if (process.env.MICAFORGE_SHIPPER_NO_MAIN !== "1") {
  main()
    .then((code) => process.exit(code ?? 0))
    .catch((error) => {
      process.stderr.write(`micaforge-shipper: ${error?.stack ?? error}\n`);
      process.exit(2);
    });
}
