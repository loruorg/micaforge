# @micaforge/devtools

A development overlay for the Micaforge tracker. It shows, live, on the page you are
building:

- **every event the tracker sends**: kind, name, properties, address, timing, and whether
  it left by beacon, fetch or pixel;
- **every event it did not send, and why**: opted out, outside the sample, localhost
  without `data-debug`, an excluded path, Do Not Track, an `identify` that changed nothing;
- **addresses rewritten** by `data-mask` or `data-strip-query`;
- **flags**: the value the server answered and the rule that decided it (rollout,
  conditions, target group, prerequisite);
- **identity**: the identified id, the last traits, the opt-out;
- **warnings**: unknown or malformed event names, property sets over the server's 8 KB,
  values that look like emails, phone numbers or card numbers, revenue that is not a
  number, and a browser the server will file as a bot.

It costs the tracker nothing: the core gains no byte and no hook. The overlay watches the
transport the tracker already uses (`sendBeacon`, `fetch`, the pixel) and replays the
tracker's own rules to explain a refusal. It lives in a Shadow DOM, so your CSS cannot
reach it and its CSS cannot reach your page, and it sends nothing anywhere.

MIT licensed.

## Use it

### Script tag

Put it **before** the tracker, so the first pageview is seen too:

```html
<script src="https://cdn.jsdelivr.net/npm/@micaforge/devtools/dist/devtools.js" data-devtools></script>
<script defer data-site="1" src="https://analytics.example.com/mf.js"></script>
```

With `data-devtools` it opens on every page. Without it, it stays out of the way until
the address carries `?mf_devtools=1`, and then stays on for the rest of the tab's session
(`?mf_devtools=0` turns it off). Other attributes:

| Attribute | What it does |
| --- | --- |
| `data-events` | The event names your site is meant to send, comma-separated. Any other name is flagged, with a "did you mean" for a near miss. |
| `data-open` | Open the panel at once. |
| `data-hotkey` | The toggle. Default `Alt+Shift+M`. |

### From your bundle

```ts
import { mountDevtools } from "@micaforge/devtools";

if (import.meta.env.DEV) {
  mountDevtools({
    knownEvents: ["signup", "purchase"],
    // With init() from @micaforge/sdk, pass the same settings so refusals can be explained:
    tracker: { site: 1, host: "https://analytics.example.com", mask: ["/users/*"] },
  });
}
```

`autoload()` mounts only when the address asks (`?mf_devtools=1`), for a build that goes
to staging.

### Nuxt

`@micaforge/nuxt` loads it in development with `devtools: true`.

## Keys

`Alt+Shift+M` opens and closes the panel; `Escape` closes it. Each event row expands to
its payload, with **Copy JSON**. **Clear** forgets the list; the filter and the "Not sent"
and "With warnings" views narrow it.
