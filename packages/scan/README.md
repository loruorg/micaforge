# @micaforge/scan

Find every analytics event in a codebase, check the names and the properties, and compare
them with what your Micaforge site really receives.

```bash
npx @micaforge/scan
```

```
12 events in 214 files, and 1 computed name

  EVENT              SENT  PROPERTIES
  checkout_started   2     plan: string, seats?: number
                           src/pricing/Checkout.tsx:41, src/server/billing.ts:88
  cta_click          3     placement: string
                           src/pages/index.astro:12, src/components/Hero.vue:9 and 1 more
  ...

warning "Sign-Up" and "signup" differ only in case or separators. Every report shows them as two events; pick one name.
warning The property key "email" names personal data or a secret.  src/account/Profile.tsx:57

0 errors, 2 warnings
```

It reads the code; it never runs it. Node 20+, one dependency (`@babel/parser`, the
parser behind Babel, for JavaScript, TypeScript and JSX with error recovery). MIT.

---

## What it finds

| Where | What |
|---|---|
| `@micaforge/sdk` and its `/react`, `/vue`, `/svelte`, `/next` entries | `track("name", props)`, aliased and namespace imports, `getTracker()?.track(…)`, `init(…).track(…)` |
| The script tag's global | `window.micaforge.track("name")`, `micaforge.track("name")`, the queue `micaforge("event", "name", props)` |
| Hooks and composables | `useMicaforge().track(…)`, `const { track } = useMicaforge()`, `useTrackEvent()`. `useMicaforge` is matched by name, so the Nuxt composable works with Nuxt's automatic imports |
| `@micaforge/sdk-server` | `micaforge.track({ name, props })`, `pageview(req, { name })`, `batch([{ type: "event", name }])`, a client from `createInstrumentation` |
| Components and directives | `<TrackClick event="name" props={…}>`, Vue `v-mf="'name'"` and `v-mf="{ event, props }"`, Svelte `use:trackClick` |
| Click tags | `data-track="name"` with `data-track-*` properties, static or bound, in JSX, TSX, Vue, Svelte, Astro, HTML, and HTML inside a JavaScript string |

Files: `.js .jsx .mjs .cjs .ts .tsx .mts .cts .vue .svelte .astro .html .htm`. In a Vue,
Svelte or Astro file the scripts, the Astro frontmatter and the template attributes are
all read, with the lines of the file.

A name is read when the source says it: a string, a template without holes, a `const`, a
member of a `const` object (`EVENTS.SIGNUP`), either side of `cond ? "a" : "b"`, and a
constant exported by another module of the project (`import { SIGNUP } from
"./events"`). Anything else is a **computed name**: reported with its source text, and
with a glob when it has fixed parts (`` `step_${n}` `` is `step_*`).

---

## The checks

| Check | Severity | Why |
|---|---|---|
| Empty name | error | The server stores it with no name and no report lists it. An empty `data-track` sends nothing. |
| Name over 200 bytes | error | The server cuts it, so it is stored under a name nobody wrote. |
| Spaces at either end, control characters | warning | The server trims the name. |
| Near duplicates | warning | `signup`, `Signup`, `sign_up`, `sing_up`: one event, several rows in every report. A numbered series (`step_1`, `step_2`) is left alone. |
| Personal data | error for a literal email or credential, warning for a key such as `email`, `phone`, `name`, `token` or a value read from `user.email` | Properties are stored exactly as sent. |
| Revenue | warning | A `revenue` property becomes revenue only as a number; `currency` is an ISO 4217 code. |
| Computed name | info | It cannot be checked here. |
| Unreadable file | warning | Events in it may be missing from the report. |

---

## Compare with your site

```bash
export MICAFORGE_API_KEY=mfg_...   # scope: stats:read
npx @micaforge/scan --site 3 --host https://analytics.example.com --range 30d
```

It reads `GET /api/stats/events` (the Events screen's own endpoint) and reports:

- **in the code, never received**: not deployed yet, behind a flag, or the call never runs;
- **received, not in the code**: an event a deleted or renamed call still sends from old
  clients, or one sent from a codebase you did not scan.

A computed name with a glob accounts for every received name it matches. The key is sent
only in the `Authorization` header and is never printed. Prefer the environment variable
to `--api-key`, so the key does not sit in your shell history.

---

## A typed `track()`

```bash
npx @micaforge/scan --types src/analytics/events.ts
```

`@micaforge/sdk`'s `track(name: string, props?)` accepts anything. The generated file
wraps it with the events your code already sends:

```ts
import { track } from "./analytics/events";

track("checkout_started", { plan: "pro" }); // ok
track("checkout_strated", { plan: "pro" }); // does not compile: no such event
track("checkout_started", { plan: 5 });     // does not compile: plan is a string
```

A property is typed from the literals seen at every call site, required when every call
site sends it, and `PropValue` when one passes a value the scanner cannot see. Run it
again after adding an event.

---

## In CI

```yaml
- run: npx @micaforge/scan --ci --strict --markdown --output tracking-plan.md
```

| Option | |
|---|---|
| `--format table\|json\|markdown` | Default: `table` in a terminal, `json` otherwise. `--json` and `--markdown` are short forms. |
| `--output <file>` | Write the report to a file. |
| `--types <file>` | Write the typed `track()`. |
| `--include <glob>`, `--exclude <glob>` | Narrow the files. Repeatable. A glob without `/` matches a name at any depth; a directory matches everything in it. |
| `--no-gitignore` | Read ignored files too. |
| `--ci` | Exit 1 when there are errors. |
| `--strict` | With `--ci`, warnings fail too. |
| `--site`, `--api-key`, `--host`, `--range` | Compare with a site. Env `MICAFORGE_SITE_ID`, `MICAFORGE_API_KEY`, `MICAFORGE_HOST`. |

Exit codes: `0` done, `1` failed `--ci`, `2` a usage error or a failed comparison.

**Which files.** Inside a git work tree the list is git's own (`git ls-files --cached
--others --exclude-standard`), so every `.gitignore`, the global excludes file and
`.git/info/exclude` apply exactly as git applies them. Outside one, the tree is walked and
each `.gitignore` is read. `node_modules`, `dist`, `build`, `.next`, `.nuxt`, `.output`,
`.svelte-kit`, `.astro`, `coverage`, `*.min.js` and `*.d.ts` are always skipped.

## As a library

```ts
import { scan, formatReport, generateTypes } from "@micaforge/scan";

const report = await scan({ root: ".", exclude: ["**/*.stories.tsx"] });
console.log(formatReport(report, "markdown"));
```

## Limits, plainly

- Names are bound by file, not by scope: a local function that shadows `track` inside a
  nested scope is still read as the SDK's.
- A constant from another module is followed for relative imports only, not for path
  aliases such as `@/events`.
- A tracker passed around as a parameter, or stored in a store, is not followed. Name
  the variable `micaforge`, or use the hooks, and it is.

MIT.
