#!/usr/bin/env bash
#
# Smoke test a running Micaforge stack, end to end, through its front door.
#
#   scripts/smoke.sh                          # http://localhost, fresh install
#   scripts/smoke.sh https://analytics.example.com
#   SMOKE_EMAIL=you@example.com scripts/smoke.sh https://...
#
# What it proves, in order: the proxy answers, the server is live and ready,
# the dashboard is served, /mf.js is the real tracker and not the fallback, an
# account can sign in with a code, a site can be created, a pageview sent the
# way a browser sends it is accepted, written to ClickHouse, and read back by
# the stats API.
#
# Sign-in is by a six-digit code, so the script needs a way to read one. By
# default it asks the server to print it, the way an operator with no mail
# transport would, which needs the stack's docker compose project in the
# current directory:
#
#   docker compose exec -T micaforge micaforge-server sign-in-code <email>
#
# Point SMOKE_CODE_COMMAND at anything else that prints the code on its first
# line when given the address as its last argument, for instance
# "ssh host docker compose exec -T micaforge micaforge-server sign-in-code".
#
# On a fresh install it creates a throwaway account (the first account, which
# becomes the instance admin), so run it against a stack you are about to
# throw away, as CI does. Against a real install, pass an existing account in
# SMOKE_EMAIL; it then adds one site named smoke-<time>.
#
# Needs curl and jq, and docker unless SMOKE_CODE_COMMAND says otherwise.

set -euo pipefail

BASE="${1:-http://localhost}"
BASE="${BASE%/}"
JAR=$(mktemp "${TMPDIR:-/tmp}/micaforge-smoke.XXXXXX")
trap 'rm -f "$JAR"' EXIT

for tool in curl jq; do
	command -v "$tool" >/dev/null 2>&1 || {
		echo "smoke: $tool is required" >&2
		exit 2
	}
done

pass() { printf 'ok    %s\n' "$1"; }
fail() {
	printf 'FAIL  %s\n' "$1" >&2
	[ -z "${2:-}" ] || printf '      %s\n' "$2" >&2
	exit 1
}

# A browser's request, as far as the ingest path can tell. The headless-bot
# rule drops a request that does not look like one, silently and on purpose.
UA='Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36'

api() {
	# api <method> <path> [json body]; prints the body, fails on a non-2xx.
	local method="$1" path="$2" body="${3:-}" out code
	out=$(mktemp "${TMPDIR:-/tmp}/micaforge-smoke-body.XXXXXX")
	if [ -n "$body" ]; then
		code=$(curl -sS -o "$out" -w '%{http_code}' -X "$method" -b "$JAR" -c "$JAR" \
			-H 'content-type: application/json' -H "origin: $BASE" --data "$body" "$BASE$path")
	else
		code=$(curl -sS -o "$out" -w '%{http_code}' -X "$method" -b "$JAR" -c "$JAR" \
			-H "origin: $BASE" "$BASE$path")
	fi
	if [ "${code:0:1}" != "2" ]; then
		fail "$method $path answered $code" "$(head -c 400 "$out")"
	fi
	cat "$out"
	rm -f "$out"
}

# --- the edge and the process ------------------------------------------------

code=$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/api/health/live") || fail "nothing answers at $BASE"
[ "$code" = 200 ] || fail "/api/health/live answered $code"
pass "server is live"

ready=$(curl -sS -w '\n%{http_code}' "$BASE/api/health/ready")
[ "$(tail -n1 <<<"$ready")" = 200 ] || fail "/api/health/ready answered $(tail -n1 <<<"$ready")" "$(head -n1 <<<"$ready")"
pass "every store is ready"

code=$(curl -sS -o /dev/null -w '%{http_code}' "$BASE/app/")
[ "$code" = 200 ] || fail "the dashboard (/app/) answered $code"
pass "dashboard is served"

tracker=$(curl -sS "$BASE/mf.js")
grep -q 'fallback' <<<"$tracker" && fail "/mf.js is the compiled-in fallback tracker, not the built one" "the server image should carry /app/tracker/mf.js"
grep -q 'Micaforge' <<<"$tracker" || fail "/mf.js does not look like the tracker" "$(head -c 200 <<<"$tracker")"
pass "/mf.js is the real tracker ($(printf '%s' "$tracker" | wc -c | tr -d ' ') bytes)"

# --- an account and a site ---------------------------------------------------

EMAIL="${SMOKE_EMAIL:-smoke+$(date +%s)@example.com}"
read -r -a CODE_COMMAND <<<"${SMOKE_CODE_COMMAND:-docker compose exec -T micaforge micaforge-server sign-in-code}"

# The endpoint the sign-in screen calls. It answers the same for every address,
# so this proves the route and its limits answer, not that a code was sent.
sent=$(api POST /api/auth/code "$(jq -nc --arg e "$EMAIL" '{email:$e,name:"Smoke"}')")
[ "$(jq -r '.data.status' <<<"$sent")" = ok ] || fail "POST /api/auth/code did not answer ok" "$sent"
pass "asked for a sign-in code for $EMAIL"

# The printed code replaces the one just asked for, so it is the one that works.
CODE=$("${CODE_COMMAND[@]}" "$EMAIL" | head -n 1 | tr -d '[:space:]') ||
	fail "could not get a sign-in code" "ran: ${CODE_COMMAND[*]} $EMAIL"
[[ "$CODE" =~ ^[0-9]{6}$ ]] || fail "the code command did not print a code" "got: $CODE"

signed=$(api POST /api/auth/code/verify "$(jq -nc --arg e "$EMAIL" --arg c "$CODE" '{email:$e,code:$c,name:"Smoke"}')")
case "$(jq -r '.data.status' <<<"$signed")" in
ok) pass "signed in with a code (account created: $(jq -r '.data.created' <<<"$signed"))" ;;
totp_required) fail "$EMAIL has two-factor on" "use an account without it for the smoke test" ;;
*) fail "POST /api/auth/code/verify did not sign in" "$signed" ;;
esac

org=$(api GET /api/auth/me | jq -r '.data.organizations[0].id // empty')
[ -n "$org" ] || fail "the account has no organisation"

DOMAIN="smoke-$(date +%s).example.com"
site=$(api POST /api/sites "$(jq -nc --arg o "$org" --arg d "$DOMAIN" '{org_id:$o,domain:$d,name:"Smoke test"}')")
SITE_ID=$(jq -r '.data.site_id' <<<"$site")
if [ -z "$SITE_ID" ] || [ "$SITE_ID" = null ]; then
	fail "creating a site returned no site_id" "$site"
fi
pass "created site $SITE_ID ($DOMAIN)"

# --- a pageview, the way the tracker sends it --------------------------------

payload=$(jq -nc --argjson s "$SITE_ID" --arg u "https://$DOMAIN/smoke?from=ci" \
	'{s:$s,t:"pageview",u:$u,r:"https://chatgpt.com/",w:1512,h:982,vw:1280,vh:720,l:"en-GB",tz:"Europe/Lisbon",v:"smoke"}')
# The headers Chrome sends with a beacon. Without the client hints and the
# Sec-Fetch family a Chrome user agent reads as headless, and the hit is filed
# as a bot (bot_events), not counted, which is the classifier doing its job.
code=$(curl -sS -o /dev/null -w '%{http_code}' -X POST "$BASE/api/track" \
	-H 'content-type: text/plain;charset=UTF-8' -H "origin: https://$DOMAIN" -H "referer: https://$DOMAIN/smoke" \
	-H "user-agent: $UA" -H 'accept: */*' -H 'accept-language: en-GB,en;q=0.9' \
	-H 'sec-ch-ua: "Chromium";v="128", "Google Chrome";v="128", "Not;A=Brand";v="24"' \
	-H 'sec-ch-ua-mobile: ?0' -H 'sec-ch-ua-platform: "macOS"' \
	-H 'sec-fetch-mode: no-cors' -H 'sec-fetch-site: cross-site' -H 'sec-fetch-dest: empty' \
	--data "$payload")
case "$code" in
2*) pass "pageview accepted ($code)" ;;
*) fail "POST /api/track answered $code" ;;
esac

# The writer flushes on a timer, so the row lands a moment after the 202.
for _ in $(seq 1 20); do
	views=$(api GET "/api/stats/overview?site=$SITE_ID&range=today&audience=human" | jq -r '.data.pageviews | if type == "object" then .value else . end // 0')
	[ "${views:-0}" -ge 1 ] 2>/dev/null && break
	sleep 1
done
[ "${views:-0}" -ge 1 ] 2>/dev/null || fail "the pageview never reached the stats API" "overview pageviews: ${views:-none}"
pass "pageview read back from ClickHouse (pageviews=$views)"

printf '\nsmoke test passed against %s\n' "$BASE"
