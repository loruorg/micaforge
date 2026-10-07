# Feeding Micaforge from an nginx access log

AI crawlers do not run JavaScript. The tracker on your pages cannot see GPTBot,
ClaudeBot or PerplexityBot, and it never will. nginx already writes down every one of
those fetches. This is how that file becomes the agent side of your Citation Gap.

Nothing here changes how nginx serves a request. You add a log format and you run a
reader beside it.

---

## 1. The log format

nginx's built-in `combined` format works: it carries the user agent, which is the one
field that cannot be worked around. Use it if you would rather not touch your config.

```nginx
access_log /var/log/nginx/access.log combined;
```

The format below is better, and takes one paste. It adds the host (so one log can serve
several sites), the response time, and the content type, all of which have columns
waiting for them in `agent_events`. `escape=json` is what makes a user agent containing
a quote safe.

```nginx
# /etc/nginx/nginx.conf, inside the http { } block.
log_format micaforge escape=json
  '{'
    '"time":"$time_iso8601",'
    '"remote_addr":"$remote_addr",'
    '"host":"$host",'
    '"request_method":"$request_method",'
    '"request_uri":"$request_uri",'
    '"status":$status,'
    '"body_bytes_sent":$body_bytes_sent,'
    '"request_time":$request_time,'
    '"sent_http_content_type":"$sent_http_content_type",'
    '"http_referer":"$http_referer",'
    '"http_user_agent":"$http_user_agent"'
  '}';

access_log /var/log/nginx/micaforge.log micaforge;
```

```bash
sudo nginx -t && sudo systemctl reload nginx
```

If you are behind Cloudflare or another proxy, `$remote_addr` is the proxy. Set
`real_ip_header CF-Connecting-IP;` (or `X-Forwarded-For`) with the matching
`set_real_ip_from` ranges, and `$remote_addr` becomes the true client again. That address
is what lets Micaforge check a fetch against an operator's published IP ranges, which is
the difference between a crawl that claims to be GPTBot and one that is verified to be.

### What not to use

`common` is the same line as `combined` with the referrer and the user agent removed.
There is nothing in it to classify, so every line is counted as skipped and nothing is
sent. The shipper says so once and then reads out the rest of the file, so a dry run on a
`common` log is a quick way to confirm that is the problem. If your log looks like this,
it is the format, not the tool:

```
203.0.113.9 - - [21/Aug/2026:10:00:00 +0000] "GET /docs HTTP/1.1" 200 4210
```

---

## 2. Look before you send

```bash
export MICAFORGE_HOST=https://micaforge.example.com
export MICAFORGE_SITE_ID=1
export MICAFORGE_INGEST_KEY=...   # Settings, Tracking, Ingest key

npx --package=@micaforge/sdk-server micaforge-shipper \
  --dry-run --from-beginning --no-follow \
  /var/log/nginx/micaforge.log
```

A dry run parses, classifies and counts. It sends nothing. You get a list of which agents
appear in that file and how often, which is usually the first time anyone sees it.

---

## 3. Ship it

```bash
npx --package=@micaforge/sdk-server micaforge-shipper /var/log/nginx/micaforge.log
```

That follows the file, ships agent fetches as they arrive, and remembers where it got to
in `/var/log/nginx/.micaforge.log.micaforge-state` so a restart neither replays nor skips.

Add `--hostname docs.example.com` if you are using the `combined` format, which has no
host field, and you serve more than one site. Add `--exclude '/assets/**'` to leave build
output out. Leave `--include-humans` off while the browser tracker is on your pages: the
tracker already counts those visits, and both would count them twice.

### Backfilling what you already have

```bash
zcat /var/log/nginx/micaforge.log.*.gz | \
  npx --package=@micaforge/sdk-server micaforge-shipper --no-follow --format json -
```

Old logs are worth reading. A crawl history is what turns the Citation Gap from a number
into a trend.

---

## 4. Run it as a service

```ini
# /etc/systemd/system/micaforge-shipper.service
[Unit]
Description=Micaforge log shipper
After=network-online.target nginx.service

[Service]
Type=simple
User=micaforge
SupplementaryGroups=adm
Environment=MICAFORGE_HOST=https://micaforge.example.com
Environment=MICAFORGE_SITE_ID=1
# Keep the key out of the unit file and out of "ps".
ExecStart=/usr/bin/npx --yes --package=@micaforge/sdk-server micaforge-shipper \
  --key-file /etc/micaforge/ingest.key \
  --state /var/lib/micaforge/nginx.state \
  /var/log/nginx/micaforge.log
Restart=always
RestartSec=5
StateDirectory=micaforge

[Install]
WantedBy=multi-user.target
```

```bash
sudo install -d -m 0750 -o micaforge -g micaforge /etc/micaforge
sudo install -m 0400 -o micaforge /dev/stdin /etc/micaforge/ingest.key <<< "$MICAFORGE_INGEST_KEY"
sudo systemctl enable --now micaforge-shipper
sudo journalctl -u micaforge-shipper -f
```

`SupplementaryGroups=adm` is how the service reads `/var/log/nginx` on Debian and Ubuntu
without running as root. On other distributions, give the user read access to the log
directory in whatever way that distribution expects. The shipper only ever reads.

### Log rotation

Rotation is handled. The shipper holds the file open by descriptor, so when logrotate
moves it aside the last lines are read out of the old file before the new one is opened.
`copytruncate` is handled too, both while the shipper is running and across a restart.

No `postrotate` hook is needed. Do not add `sudo systemctl restart micaforge-shipper` to
one: restarting during a rotation is the one way to lose lines that would otherwise be
read.

---

## 5. As a cron job instead

If you would rather not run a daemon, run it on a timer. `--no-follow` reads to the end
of the file and exits, and the state file makes the next run pick up exactly where this
one stopped.

```
*/5 * * * * micaforge /usr/bin/npx --yes --package=@micaforge/sdk-server micaforge-shipper \
  --no-follow --key-file /etc/micaforge/ingest.key --quiet \
  /var/log/nginx/micaforge.log
```

---

## What ends up in Micaforge

One row in `agent_events` per agent fetch: the time, the user agent, the method, the
host, the path, the query string, the status, the byte count, the response time, the
content type, and the source address.

The source address is sent so the server can check it against the operator's published
ranges. Micaforge stores a salted, rotating hash of it and never the address itself.

Which agent it was, which operator runs it, what the crawl was for, whether it verified,
and whether your `robots.txt` allowed it are all decided by the server against the full
catalog. The shipper carries a small pattern list to decide what is worth sending, and
nothing more than that.
