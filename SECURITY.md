# Security

## Reporting a vulnerability

Use GitHub's private vulnerability reporting: the **Security** tab of
[loruorg/micaforge](https://github.com/loruorg/micaforge), then **Report a
vulnerability**. It opens a private thread with the maintainers.

If that is not available to you, open a normal issue that says only that you have
a report and asks for a contact address. Do not put the detail in it.

Please include what you can: the version or commit, whether the install is
self-hosted or a development checkout, the steps to reproduce, and what the bug
lets an attacker do. A proof of concept against your own install is worth more
than a description of one.

We will confirm receipt, tell you what we found, and say when a fix lands. If a
report turns out not to be a vulnerability, we will explain why rather than close
it silently. You will be credited in the fix unless you would rather not be.

Please do not test against somebody else's install.

## Supported versions

There are no releases yet. The default branch is the only supported version, and
a fix lands there.

## What is in scope

Anything that breaks one of the guarantees the product actually makes:

- **Visitor privacy.** A raw IP address reaching a stored human event row, a
  visitor hash that can be correlated across days, or a daily salt outliving its
  48 hour window. These are stated in `docs/CONTRACT.md` section 7, and a break
  of one is a serious bug, not a design discussion.
- **Tenant isolation.** Any path where one site's or one organisation's data is
  readable through another's credentials, including through a share link, an API
  key or the MCP server.
- **Authentication and session handling.** Forged session cookies, a sign-in
  code that works twice, outlives its ten minutes or can be guessed past its
  limits, any answer or timing that tells whether an address has an account, a
  code or token reaching a log, share links that ignore their password or
  expiry, API keys that outlive their revocation.
- **The ingest path.** Injecting events into a site you do not control, forging
  an edge-log submission past its HMAC, or getting a stored value out of the
  event store into the dashboard as script.
- **The tracker.** Anything in `mf.js` that can affect the page it is embedded
  in. The tracker is meant to be incapable of throwing into its host page; a way
  to make it do so is a bug worth reporting.
- **The self-hosted build phoning home.** It is documented as making no outbound
  call at runtime. Finding one is a report.

## What is out of scope

- Missing hardening headers, TLS configuration or rate limits on an install you
  configured yourself. The shipped Caddy configuration is a starting point and
  your deployment is yours.
- Reports produced by a scanner with no working proof of concept.
- Anything requiring an operator to already hold the `MICAFORGE_SECRET_KEY_BASE`.
  That key is the root of every other secret; whoever holds it holds the install.
- Social engineering, and denial of service by volume.

## Operating one safely

The parts an operator owns:

- **`MICAFORGE_SECRET_KEY_BASE`** seals every stored secret and signs every
  session. Keep it out of backups that hold the data it protects, which is why
  `backup.sh` deliberately does not copy `.env`. Rotating it invalidates sessions
  and makes already-sealed values unreadable.
- **Close registration** once your own account exists:
  `MICAFORGE_DISABLE_REGISTRATION=true`. An open signup form on a public
  dashboard collects strangers.
- **Ingest keys** are per site and can write events to that site. Treat one like
  a password, and rotate it if a log shipper's configuration leaks.
- **The stores are not published.** In the shipped compose file Postgres,
  ClickHouse and Valkey have no host port, and Caddy is the only thing listening.
  Publishing a store port to `0.0.0.0` undoes that in one line.
- **`docker-compose.dev.yml` is for development.** It binds to 127.0.0.1 with a
  password committed to this repository. It is safe exactly as long as it stays
  on a machine nobody else can reach.
