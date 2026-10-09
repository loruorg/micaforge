# Micaforge for Swift

Cookieless analytics and feature flags for iOS, iPadOS, macOS, tvOS, watchOS and visionOS
apps, reporting to your Micaforge server.

- **No dependencies.** Swift Package Manager adds one library and nothing else.
- **Swift 6, strict concurrency.** The queue lives in an actor; every public call is
  synchronous, ordered, and safe from any thread.
- **Offline first.** Events wait in a bounded queue on disk (Application Support, file
  protection until first unlock, excluded from backups) and are retried with exponential
  backoff and jitter.
- **No identifiers of its own.** No IDFA, no vendor id, no install id. The one value it
  stores is the opt-out.
- **Flags decided like the server decides them.** The local evaluator passes the same
  shared test vectors as the server, the browser SDK and the Node SDK.
- **MIT.** The Micaforge server is AGPL-3.0; the parts you embed are not.

Platforms: iOS 15, macOS 12, tvOS 15, watchOS 8, visionOS 1 and later.

**Status: pre-release.** The wire format is the one in `docs/CONTRACT.md`; nothing here
has run in a shipped app yet.

---

## Install

In Xcode, **File, Add Package Dependencies**, and enter the repository URL:

```
https://github.com/loruorg/micaforge
```

Or in `Package.swift`:

```swift
.package(url: "https://github.com/loruorg/micaforge", branch: "main"),
// ...
.product(name: "Micaforge", package: "micaforge"),
```

## Configure

Once, at launch:

```swift
import Micaforge

@main
struct MyApp: App {
    init() {
        Micaforge.configure(
            siteKey: "1",
            host: URL(string: "https://analytics.example.com")!,
            options: MicaforgeOptions(domain: "example.com", pathPrefix: "/ios")
        )
    }
    var body: some Scene { WindowGroup { ContentView() } }
}
```

`siteKey` is the numeric site id from the tracking snippet (`data-site="1"`). It is
public. The site's **ingest key** is a server secret and must never ship in an app.

### Why `domain` matters

The server accepts a hit only when its page address is on one of the site's own domains,
because a site id is public and that check is what keeps strangers out of your numbers.
An app has no page address, so each screen is reported as
`https://<domain><pathPrefix>/<screen>`. Set `domain` to the site's domain (or a
subdomain, or a domain listed in the site's settings). Without it, the hostname of `host`
is used, which works when your Micaforge server runs on a subdomain of the site. A `403`
in the debug log means this is the setting to fix.

## Record

```swift
Micaforge.screen("Order history")                       // reported as /ios/order-history
Micaforge.screen("Product", path: "/products/\(id)")    // pass a path for one item of many
Micaforge.track("signup", props: ["plan": "pro", "seats": 5])
Micaforge.revenue(19.99, currency: "EUR")               // a "purchase" with revenue + currency
Micaforge.identify(id: "u_123", traits: ["plan": "pro"])
Micaforge.setProps(["theme": "dark"])                   // merged into every later event
Micaforge.reset()                                       // sign-out
Micaforge.optOut()                                      // until optIn()
await Micaforge.flush()                                 // send now and wait
```

In SwiftUI, let the view report itself:

```swift
SettingsView()
    .trackScreen("Settings")
```

A view that appears again while it is still the current screen (a tab switched back to,
a sheet dismissed) is not counted twice.

## What it sends

Screens are pageviews, with the screen's name as the page title. Each screen carries the
time the previous one spent in the foreground, and going to the background reports the
current screen's time, so the dashboard's engagement figures mean what they mean on the
web. Events go to `POST /api/track/batch` in batches: when 20 are waiting, 10 seconds
after the first, when the app goes to the background, and on `flush()`.

The server stamps each hit with the time it arrives. An event recorded offline is
therefore counted when it is delivered, not when it happened.

The request's user agent names the platform (`Mozilla/5.0 (iPhone; CPU iPhone OS 17_5
like Mac OS X) Micaforge-Swift/0.1.0`) and never your app's name, so the server reads the
right OS and device type and no app name is ever mistaken for a crawler.

## Options

| Option | Default | What it does |
| --- | --- | --- |
| `domain` | the host's hostname | The hostname screens are reported under. |
| `pathPrefix` | none | A path in front of every screen, such as `/ios`. |
| `flushAt` | 20 | Send when this many events are waiting. |
| `flushInterval` | 10 s | Send this long after the first waiting event. |
| `maxBatchSize` | 50 | Events per request (the server reads at most 200). |
| `maxQueueSize` | 1000 | Events kept offline; the oldest go first. |
| `persistQueue` | true | Keep the queue on disk across launches. |
| `sampleRate` | everyone | Percent of devices to keep, drawn per device and day. |
| `mask` | none | Path globs reported as the glob itself: `["/products/*"]`. |
| `exclude` | none | Path globs never reported. |
| `stripQuery` | `.none` | `.all`, or `.parameters(["token"])`. |
| `defaultProps` | none | Properties on every event. |
| `debug` | false | Log every event and request outcome (subsystem `com.micaforge.sdk`). |
| `enabled` | true | A kill switch. |
| `trackLifecycle` | true | Engagement time and background flushes. |
| `flags` | | See below. |
| `transport` | private `URLSession` | Your own `MicaforgeTransport`, for a proxy or a test. |

## Feature flags

```swift
let flags = Micaforge.flags!
await flags.load()                       // GET /api/flags/evaluate, cached for a minute
if flags.isEnabled("new-checkout") { ... }
let variant = flags.variant("pricing-test")
```

Reads are synchronous and answer, in order, from the server's last answer, a ruleset you
handed the client (`FlagOptions(rules:)`, decided on the device), `bootstrap` values, and
`fallback` rules. The two local steps run the same evaluator as the server, held to it by
`packages/sdk/test/flag-vectors.json`, which this package's tests read in place.

Anonymous users are bucketed by a random id that lives for one launch, because the SDK
stores no identifier. Pass `FlagOptions(subject:)` with your own install id for stable
bucketing; after `identify`, the identified id is the subject.

## Development

From the repository root:

```sh
swift build
swift test
```
