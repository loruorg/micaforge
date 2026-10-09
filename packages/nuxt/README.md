# @micaforge/nuxt

Micaforge for Nuxt: the tracker tag, pageviews from the Nuxt router, `useMicaforge()` and
`useFlag()`, server helpers for Nitro, and the DevTools overlay in development.

MIT licensed. Nuxt 3.13 or later, and Nuxt 4.

## Install

```sh
npm install @micaforge/nuxt
```

```ts
// nuxt.config.ts
export default defineNuxtConfig({
  modules: ["@micaforge/nuxt"],
  micaforge: {
    siteKey: 1,
    host: "https://analytics.example.com",
    plugins: ["errors", "forms"],
    devtools: true,
  },
});
```

`siteKey` is the numeric site id (`data-site`). Both it and `host` can also come from the
environment at run time: `NUXT_PUBLIC_MICAFORGE_SITE_KEY`, `NUXT_PUBLIC_MICAFORGE_HOST`.

## Options

| Option | Default | What it does |
| --- | --- | --- |
| `siteKey` | none | The numeric site id. |
| `host` | none | Your Micaforge server. |
| `scriptSrc` | `<host>/mf.js` | Where the tracker loads from; set it when you proxy the script. |
| `plugins` | vitals only | `["errors", "frustration", "forms", "track404", "tags"]`, or `{ vitals: false, errors: true }`. |
| `enabled` | `true` | `false` adds nothing and makes every call a no-op. |
| `routeTracking` | `true` | Report route changes from the Nuxt router. |
| `devtools` | `false` | `true` loads the overlay in development; `{ knownEvents, open, production }` for more. |
| `tracker` | | `exclude`, `mask`, `stripQuery`, `sample`, `respectDnt`, `hash`, `debug`, `outbound`, `downloads`, `scroll`, `engagement`, `release`: the tag's other `data-*` settings. |
| `flags` | | `bootstrap`, `environment`, `context`, `ttlMs`, `stats` for `useFlag()`. |

## Route changes

The router reports each completed navigation once, after it has finished, and the tag is
given `data-spa="false"` so the tracker's own history listener stays off. Without that,
both would report the same navigation. The rule is the tracker's: the address without
its hash (unless `tracker.hash`), never the same address twice in a row, nothing for an
aborted navigation. `routeTracking: false` hands navigation back to the tracker.

## In components

```vue
<script setup lang="ts">
const { track, identify } = useMicaforge();
const { enabled: newCheckout, variant } = useFlag("new-checkout");

function signUp() {
  track("signup", { plan: "pro" });
}
</script>

<template>
  <NewCheckout v-if="newCheckout" :variant="variant" />
</template>
```

Both are safe during server rendering. `useMicaforge()` does nothing on the server;
calls made before the deferred tag has run are queued and replayed. `useFlag()` renders
the `flags.bootstrap` value on the server, hydrates with the same value, and takes the
client's evaluated answer once the app is ready, so there is no hydration mismatch.
`identify()` also moves flags to the identified id.

## In Nitro

```ts
// server/middleware/micaforge.ts
export default defineEventHandler((event) => {
  useMicaforgeServer()?.pageview(event.node.req);
});
```

`useMicaforgeServer()` is the `@micaforge/sdk-server` client (server-rendered pageviews,
and the AI crawler fetches a browser tag never sees). It needs the site's ingest key in
`NUXT_MICAFORGE_INGEST_KEY`. `useMicaforgeFlags()` decides flags on the server from a
cached ruleset and needs an API key with `manage:read` in `NUXT_MICAFORGE_API_KEY`. Both
are private runtime config, never sent to the browser, and both return `undefined` when
their key is not set.

## Tests

`pnpm test` runs unit tests with no Nuxt app: the module runs against a hand-built Nuxt
object through the real `@nuxt/kit`, and the runtime logic runs against the real tracker
and flag client.
