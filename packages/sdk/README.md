# @micaforge/sdk

The Micaforge browser tracker and its typed SDK.

Micaforge is open-source, cookieless web and product analytics that treats machine
readers as a first-class audience. This package is the part you put on your site: a
dependency-free script tag, a typed npm package, wrappers for React, Next, Vue and
Svelte, and a feature-flag client.

- **Under 3 KB gzipped.** The budget is 3072 bytes and it is enforced, not aspired to:
  `npm run size` prints the current figure and fails the build above it.
- **No dependencies.** Not one, at runtime.
- **No cookies.** No storage either, except one opt-out key you control.
- **It cannot throw into your page.** Every public entry point and every listener is
  wrapped, so a fault in the tracker stays inside the tracker.
- **MIT.** The server is AGPL-3.0. The parts you embed are not, so embedding them does
  not reach into your own code.

**Status: pre-release.** There is no tagged release and no hosted service. The payload
and the option names below are settled in `docs/CONTRACT.md` and are what the server is
built against, but nothing here has run in production yet.

---

## Install: plain HTML

Put this before `</body>`, or in `<head>` with `defer`. Replace the host with your
Micaforge origin and the site id with yours.

```html
<script
  defer
  data-site="1"
  data-host="https://micaforge.example.com"
  src="https://micaforge.example.com/mf.js"
></script>
```

That is the whole install. It sends a pageview at once, follows client-side navigation,
and reports outbound clicks and file downloads.

### Calling it before it loads

The tracker is deferred, so it is not there yet when the page starts. Put the queue
snippet above your own scripts and call `micaforge(...)` whenever you like. The tracker
plays back everything that is queued as soon as it arrives.

```html
<script>
  window.micaforge =
    window.micaforge ||
    function () {
      (window.micaforge.q ||= []).push(arguments);
    };
</script>

<script>
  micaforge("event", "signup", { plan: "pro" });
</script>
```

### The global API

`window.micaforge` is a function and an object. Both forms do the same thing, so the
queued form keeps working after the tracker has loaded.

```js
micaforge("event", "signup", { plan: "pro" });
micaforge.track("signup", { plan: "pro" });

micaforge.pageview(); // send one by hand
micaforge.pageview({ url: "https://example.com/virtual/step-2" });
micaforge.identify("u_123", { plan: "pro" }); // attach an id to later events
micaforge.setTraits({ plan: "team" }); // record traits on the profile only
micaforge.setProps({ theme: "dark" }); // merged into every later event
micaforge.optOut(); // stop, on this browser, until optIn()
micaforge.optIn();
micaforge.isOptedOut(); // -> boolean
micaforge.flush(); // report engagement time and vitals now
await micaforge.checkoutRef(); // this visit's "mfr_…" reference for a Stripe or Paddle checkout
```

`identify()` and `setTraits()` send one small `identify` event when the id or the traits
change, so the server can keep the trait history and link today's visitor hash to the id.
A repeat of the same call sends nothing, and `identify(null)` sends nothing.

---

## Script tag options

Every option is a `data-*` attribute on the script tag. To turn an option off, set it to
`false`, `0`, `off` or `no`. To turn it on, set it to anything else, including an empty
value: `data-forms` on its own means the same as `data-forms="true"`.

| Attribute | Default | What it does |
| --- | --- | --- |
| `data-site` | **required** | The numeric site id. Without it the tracker does nothing. |
| `data-host` | the script's own origin | Where events are sent. Point it at your Micaforge deployment. |
| `data-auto-pageview` | `true` | Send a pageview as soon as the tracker starts. |
| `data-spa` | `true` | Follow client-side navigation: patches `pushState` and `replaceState`, listens for `popstate` and `hashchange`. Repeat URLs are never sent twice. |
| `data-outbound` | `true` | Report a click on a link that leaves your site. Subdomains of your own site are not outbound. |
| `data-downloads` | `true` | Report a click on a file link. A comma list sets the extensions: `data-downloads="pdf,zip,dmg"`. One extension on its own needs a leading dot, as in `data-downloads=".dmg"`, so that `data-downloads="off"` stays unambiguous. |
| `data-forms` | `false` | Report a form submit. The form's `id` or `name` is sent, and nothing else. Field values never leave the page. Loads the `forms` plugin. |
| `data-404` | `false` | Report a not-found page. See [404 pages](#404-pages). Loads the `track404` plugin. |
| `data-hash` | `false` | Treat the URL hash as part of the page identity, for hash routers. |
| `data-exclude` | none | Comma-separated glob paths that are never reported: `data-exclude="/admin/**, /preview/*"`. `*` matches one segment, `**` matches any depth, `?` matches one character. |
| `data-respect-dnt` | `false` | Send nothing when `navigator.doNotTrack` is `"1"`. |
| `data-vitals` | `true` | Collect LCP, CLS, INP, FCP and TTFB, and report them once per page. Loads the `vitals` plugin. |
| `data-scroll` | `true` | Track the deepest scroll position reached on each page. |
| `data-engagement` | `true` | Track how long each page was actually visible. |
| `data-debug` | `false` | Log every payload to the console, and report from `localhost` and `file://` too. |
| `data-sample` | everyone | Keep this percent of visitors (`0`-`100`), drawn in the browser per visitor and day from the date and the user agent. Nothing is stored; the rate is sent as `sr`. |
| `data-mask` | none | Comma-separated path globs reported as the glob itself: `data-mask="/users/*"` sends `/users/*`, never the id. Applies to the page and the referrer. |
| `data-strip-query` | off | On its own, drop the whole query string. With a list, drop those parameters: `data-strip-query="token,email"`. |
| `data-errors` | `false` | Capture uncaught errors and unhandled rejections. Loads the `errors` plugin. |
| `data-release` | none | Your release name, sent with every captured error. |
| `data-frustration` | `false` | Report rage clicks, dead clicks and abandoned forms (form id and the last field's name only). Loads the `frustration` plugin. |
| `data-tags` | `false` | Send a custom event for every click on an element with `data-track="name"`, with its `data-track-*` attributes as properties. Loads the `tags` plugin. |

### Plugins

The core (`/mf.js`) stays under 3 KB. Every optional capability is a plugin, a second
small file the core appends to the page only when its attribute asks for it, served from
`/api/track/p/<name>.js` on the ingest host: `vitals` (on by default), `errors`,
`frustration`, `forms`, `track404` and `tags`. A plugin sends through the core, so it
passes the same opt-out, sample, exclude and mask rules as a pageview, and a browser that
would send nothing loads none of them. Each plugin has its own gzip budget in
`scripts/size.mjs`.

A tracker with everything on:

```html
<script
  defer
  data-site="1"
  data-host="https://micaforge.example.com"
  data-forms
  data-404
  data-respect-dnt
  data-exclude="/admin/**, /account/**"
  data-downloads="pdf,zip,dmg,csv"
  src="https://micaforge.example.com/mf.js"
></script>
```

`/mf.min.js` serves the same file, for people who expect that name.

### 404 pages

Set `data-404` and put the marker on your not-found template. Any element will do.

```html
<meta data-mf-404 />
```

The tracker reports one `error` event named `404` with the path that was missing.

### Local development

The tracker sends nothing from `localhost`, `127.0.0.1`, `*.local`, `*.localhost` or
`file://`. Your own refreshes are not data. Set `data-debug` while you are testing the
install and it will report from there too, and log every payload.

---

## Install: npm

```sh
npm install @micaforge/sdk
```

```ts
import { init, track, identify, pageview } from "@micaforge/sdk";

init({
  site: 1,
  host: "https://micaforge.example.com",
  respectDnt: true,
  exclude: ["/admin/**"],
});

track("signup", { plan: "pro" });
identify("u_123", { plan: "pro" });
```

Every option in the table above is here too, in camelCase: `autoPageview`, `spa`,
`outbound`, `downloads`, `downloadExtensions`, `forms`, `track404`, `hash`, `exclude`,
`respectDnt`, `vitals`, `scroll`, `engagement`, `debug`, plus `props` (defaults for every
event), `identity`, `enabled` and `autoStart`.

Calling `init()` twice returns the first tracker. On a server there is no `window`, so
every call is a no-op and nothing is sent. You can import this package into a server
component without guarding it.

If the page already loaded `/mf.js`, the npm SDK adopts that tracker instead of starting
a second one.

### React

```tsx
import { MicaforgeProvider, useTrackEvent, usePageviews, TrackClick } from "@micaforge/sdk/react";
import { useLocation } from "react-router";

function App() {
  return (
    <MicaforgeProvider config={{ site: 1, host: "https://micaforge.example.com" }}>
      <Checkout />
    </MicaforgeProvider>
  );
}

function Checkout() {
  const track = useTrackEvent();
  usePageviews(useLocation().pathname); // router-agnostic: pass whatever your router calls the location

  return (
    <TrackClick event="checkout_started" props={{ plan: "pro" }}>
      <button onClick={() => track("cta_clicked")}>Start</button>
    </TrackClick>
  );
}
```

`usePageviews()` is optional. Without it the tracker's own SPA listener already follows
`pushState`, `replaceState`, `popstate` and `hashchange`. Use the hook when you have
turned that off, or when your router does something unusual.

`useMicaforge()` returns the tracker itself, or `undefined` before it starts.

### Next.js

`MicaforgeAnalytics` is a client component that renders nothing.

```tsx
// app/layout.tsx
import { MicaforgeAnalytics } from "@micaforge/sdk/next";

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>
        {children}
        <MicaforgeAnalytics site={1} host="https://micaforge.example.com" />
      </body>
    </html>
  );
}
```

App Router navigation is already covered, because Next moves through the History API. If
you want a pageview for every search-param change as well, drive it yourself:

```tsx
"use client";
import { usePathname, useSearchParams } from "next/navigation";
import { useNextPageviews } from "@micaforge/sdk/next";

export function Pageviews() {
  useNextPageviews(usePathname(), useSearchParams()?.toString());
  return null;
}
```

This entry imports nothing from `next`, so it holds no version opinion.

### Vue 3

```ts
import { createApp } from "vue";
import { micaforge } from "@micaforge/sdk/vue";
import App from "./App.vue";

createApp(App).use(micaforge, { site: 1, host: "https://micaforge.example.com" }).mount("#app");
```

Inside a component: `inject("micaforge")`, `this.$micaforge`, or `useMicaforge()`. The
plugin also registers a `v-mf` directive for click tracking.

```vue
<button v-mf="'cta_clicked'">Start</button>
<button v-mf="{ event: 'checkout_started', props: { plan: 'pro' } }">Buy</button>
```

### Svelte and SvelteKit

```svelte
<script>
  import { micaforge, trackClick } from "@micaforge/sdk/svelte";
  micaforge.init({ site: 1, host: "https://micaforge.example.com" });
</script>

<button use:trackClick={{ event: "cta_clicked", props: { plan: "pro" } }}>Start</button>
```

`micaforge` is a Svelte store as well as the API, so `$micaforge` in a component is the
tracker or `undefined`. There is also a `pageviewOn` action if you would rather send
pageviews from `$page.url.pathname` yourself.

---

## The no-JS pixel

A 1x1 GIF endpoint for pages with no JavaScript at all: an email, an RSS item, or a
`<noscript>` block. The tracker itself sends with `sendBeacon` or `fetch`. The keys are the ones in the table below.

```html
<noscript>
  <img
    src="https://micaforge.example.com/api/track/pixel.gif?s=1&t=pageview&u=https%3A%2F%2Fexample.com%2Fdocs%2Fx"
    alt=""
    width="1"
    height="1"
    style="position:absolute"
  />
</noscript>
```

---

## Feature flags

The flag client answers synchronously and deterministically, so a flag holds the same
value on the first render as on the tenth. Before the network answers, and if it never
answers, the value comes from a ruleset you hand it (`rules`), the bootstrap payload your
server rendered into the page, or a local fallback rule. Local decisions run the same
evaluator as the Micaforge server, with the server's own bucketing hash, so a fallback
rule that mirrors a flag's settings gives each visitor the server's answer.

```ts
import { flags } from "@micaforge/sdk";

const f = flags({
  visitor: currentUser.id, // any stable string; without one, bucketing is stable for this page load only
  environment: "production", // flags can override their settings per environment
  context: { plan: currentUser.plan, country: "PT" }, // what conditions and target groups test
  bootstrap: window.__FLAGS__, // optional, rendered by your server
  fallback: {
    "new-nav": { rolloutPct: 50 },
    hero: { rolloutPct: 100, variants: [{ key: "a", weight: 1 }, { key: "b", weight: 1 }] },
  },
  onExposure: (key, record) => track("flag_exposure", { flag: key, variant: record.variant }),
});

if (f.isEnabled("new-nav")) renderNewNav();
f.variant("hero", "a");
await f.load(); // refresh from the server; never rejects
f.onChange((all) => rerender(all));
```

The client reads `GET /api/flags/evaluate?site=<id>&visitor=<subject>` (plus `identity`,
`environment` and `context` when set) and accepts `{ "data": { "flags": {...} } }`,
`{ "data": {...} }` or a bare map. Each entry may be `true`, a variant string, or
`{ enabled, variant, payload }`. Nothing is cached to disk: the cache lives in memory for
`ttlMs` (60s by default).

**Stats.** The client counts each evaluated set and the first read of each flag on a
page (an exposure) and sends the counts in one `text/plain` beacon to
`POST /api/flags/events` ten seconds after the first one, and when the page is hidden. The
flag's screen in the dashboard draws them per variant over time. `stats: false` turns
this off; `f.flushStats()` sends now.

**The evaluator on its own.** `evaluateFlag(ruleset, key, { subject, context,
environment })`, `evaluateFlags` and `flagBucket` are exported for code that has a ruleset
(from `GET /api/flags/ruleset`) and wants decisions without a client, such as an edge
function.

---

## Opting out

Two ways, and the stored one wins over everything else.

```js
micaforge.optOut(); // writes localStorage["mf-opt-out"] = "1"
micaforge.optIn(); // removes it
```

Your visitors can also set `localStorage["mf-opt-out"] = "1"` themselves. Set
`data-respect-dnt` and the tracker will honour `navigator.doNotTrack` as well.

---

## What gets sent

One small JSON body per event, by `navigator.sendBeacon` first, `fetch(keepalive: true)`
second. Nothing is awaited, so nothing the tracker does can hold up
your page. The full shape is `docs/CONTRACT.md` section 4.

| Key | Meaning |
| --- | --- |
| `s` | site id |
| `t` | kind: `pageview`, `custom`, `outbound`, `file_download`, `form`, `error`, `web_vitals`, `engagement`, `identify`, `frustration` |
| `n` | event name, for custom events and auto-captured ones |
| `u` | the page URL |
| `r` | `document.referrer` |
| `w`, `h` | screen size |
| `vw`, `vh` | viewport size |
| `l` | `navigator.language` |
| `tz` | IANA time zone |
| `d` | engagement time in ms on the page just left |
| `sc` | deepest scroll percent on the page just left |
| `p` | your properties |
| `id` | the id from `identify()` |
| `v` | SDK version |
| `e` | web vitals, once per page |
| `sr` | the `data-sample` rate, only from a sampled visitor |
| `a` | browser automation bitmask (1 webdriver, 2 CDP globals, 4 Selenium-family globals, 8 HeadlessChrome brand), only when nonzero; such hits are filed as bots |

Empty, zero and absent fields are left out rather than sent.

**Never sent:** cookies, `navigator.userAgent` (the server reads the request header), form
field values, input contents, canvas or font fingerprints, or any other signal not in the
table above. The server derives country and network from the request IP and never stores
the raw IP on an event row.

---

## Working on this package

```sh
npm run build      # esbuild -> dist, then declarations, then the size gate
npm run size       # gzipped size of dist/mf.js (fails over 3072 bytes) and of each plugin
npm run typecheck
npm test           # vitest
```

`src/core.ts` is the engine that both the script tag and the npm package use.
`src/tracker/index.ts` is the script-tag layer that reads the `data-*` attributes and
publishes the global. `src/plugins/*` are the plugins. The core's tests stub the browser
globals by hand rather than using jsdom, so the tracker cannot quietly start depending on
a global it has not declared; the plugin tests run in jsdom, because reading the DOM is
their whole job.

The size budget is real. If a change pushes `dist/mf.js` over 3072 bytes gzipped, the
build fails, and the fix is to make the tracker smaller rather than to raise the number.
