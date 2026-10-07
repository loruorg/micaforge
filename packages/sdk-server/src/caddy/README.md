# Feeding Micaforge from a Caddy access log

AI crawlers do not run JavaScript, so the tracker on your pages cannot see GPTBot,
ClaudeBot or PerplexityBot. Caddy already writes every one of those fetches down, and it
writes them as JSON, which means there is no log format to design. Turn the access log on
and point the shipper at it.

---

## 1. Turn the access log on

Caddy does not write an access log until you ask for one. Inside the site block:

```caddyfile
docs.example.com {
	log {
		output file /var/log/caddy/access.log {
			roll_size 100MiB
			roll_keep 10
		}
		format json
	}

	root * /srv/docs
	file_server
}
```

```bash
caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

`format json` is Caddy's default for a file output, so it can be left out. It is written
here because the shipper reads the JSON shape, and being explicit about it means a future
change to your config cannot quietly break the feed.

A line looks like this, folded for reading:

```json
{
  "ts": 1787306400.123,
  "request": {
    "remote_ip": "203.0.113.9",
    "client_ip": "203.0.113.9",
    "method": "GET",
    "host": "docs.example.com",
    "uri": "/docs/getting-started",
    "headers": { "User-Agent": ["Mozilla/5.0 (compatible; ClaudeBot/1.0)"] }
  },
  "duration": 0.0421,
  "size": 4210,
  "status": 200,
  "resp_headers": { "Content-Type": ["text/html; charset=utf-8"] }
}
```

Every field Micaforge has a column for is already in there: the time, the address, the
method, the host, the path, the query string, the status, the size, the response time and
the content type. Nothing needs adding.

### Behind a proxy

If Cloudflare or another proxy sits in front of Caddy, set `trusted_proxies` in the
global options so `client_ip` is the real client rather than the proxy:

```caddyfile
{
	servers {
		trusted_proxies static 173.245.48.0/20 103.21.244.0/22
		client_ip_headers Cf-Connecting-Ip X-Forwarded-For
	}
}
```

The shipper reads `client_ip` first and falls back to `remote_ip`. That address is what
lets Micaforge check a fetch against an operator's published IP ranges, which is the
difference between a request that claims to be GPTBot and one that is verified to be.

### One log for several sites

Caddy writes the host on every line, so a shared log needs nothing extra. The shipper
files each hit under the host in the line it came from.

---

## 2. Look before you send

```bash
export MICAFORGE_HOST=https://micaforge.example.com
export MICAFORGE_SITE_ID=1
export MICAFORGE_INGEST_KEY=...   # Settings, Tracking, Ingest key

npx --package=@micaforge/sdk-server micaforge-shipper \
  --dry-run --from-beginning --no-follow \
  /var/log/caddy/access.log
```

A dry run parses, classifies and counts. It sends nothing. What comes back is a list of
which agents have been reading your site, which is usually the first time anyone has
seen it.

---

## 3. Ship it

```bash
npx --package=@micaforge/sdk-server micaforge-shipper /var/log/caddy/access.log
```

It follows the file, ships agent fetches as they arrive, and remembers its position in
`/var/log/caddy/.access.log.micaforge-state`, so a restart neither replays nor skips.

Leave `--include-humans` off while the browser tracker is on your pages. The tracker
already counts those visits, and it sees engagement and client-side navigation that a log
line cannot; running both would count every visit twice.

Caddy's own `roll_size` rotation is handled. The shipper holds the file open by
descriptor, reads the tail of the rolled file, then opens the new one.

---

## 4. Run it as a service

```ini
# /etc/systemd/system/micaforge-shipper.service
[Unit]
Description=Micaforge log shipper
After=network-online.target caddy.service

[Service]
Type=simple
User=micaforge
SupplementaryGroups=caddy
Environment=MICAFORGE_HOST=https://micaforge.example.com
Environment=MICAFORGE_SITE_ID=1
ExecStart=/usr/bin/npx --yes --package=@micaforge/sdk-server micaforge-shipper \
  --key-file /etc/micaforge/ingest.key \
  --state /var/lib/micaforge/caddy.state \
  /var/log/caddy/access.log
Restart=always
RestartSec=5
StateDirectory=micaforge

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now micaforge-shipper
sudo journalctl -u micaforge-shipper -f
```

`SupplementaryGroups=caddy` is what gives the service read access to `/var/log/caddy`
without running as root. The shipper only ever reads.

---

## 5. When Caddy logs to stdout

In a container, or with `output stdout`, there is no file to follow. Pipe it instead.
`--no-state` because a pipe has no position to remember.

```bash
docker logs -f caddy | \
  npx --package=@micaforge/sdk-server micaforge-shipper --format json --no-state -
```

```bash
journalctl -u caddy -f -o cat | \
  npx --package=@micaforge/sdk-server micaforge-shipper --format json --no-state -
```

Caddy writes its own runtime messages to the same stream. Those lines are JSON too, but
they carry no user agent, so they are counted as skipped and nothing else happens to
them. If you would rather keep the streams apart, give the access log its own `output
file` and follow that.

---

## 6. Backfilling

```bash
zcat /var/log/caddy/access.log.*.gz | \
  npx --package=@micaforge/sdk-server micaforge-shipper --no-follow --format json -
```

Old logs are worth reading. A crawl history is what turns the Citation Gap from a number
into a trend.

---

## What ends up in Micaforge

One row in `agent_events` per agent fetch: the time, the user agent, the method, the
host, the path, the query string, the status, the byte count, the response time, the
content type, and the source address.

The source address is sent so the server can verify it. Micaforge stores a salted,
rotating hash of it and never the address itself.

Which agent it was, which operator runs it, what the crawl was for, whether it verified,
and whether your `robots.txt` allowed it are all decided by the server against the full
catalog. The shipper carries a small pattern list to decide what is worth sending, and
nothing more.
