#!/usr/bin/env bash
#
# Micaforge installer.
#
#   ./install.sh
#   ./install.sh --domain analytics.example.com --email you@example.com --yes
#   ./install.sh --no-tls --email you@example.com --yes
#   ./install.sh --no-tls --public-url https://analytics.example.com --email you@example.com --yes
#
# What it does, in order: checks that this machine can run the stack, generates
# the three secrets, writes .env, pulls the store images, builds the server and
# web images (or pulls published ones with --version), starts everything, waits
# until the server reports healthy, and prints the address and the tracking
# snippet. Docker is the only thing the host needs.
#
# It is safe to run twice. An existing .env is never overwritten without
# --force, because the key in it is the key every stored secret is sealed with.
#
# Every failure below stops the script and says what to do next. If one does
# not, that is a bug worth reporting.

set -euo pipefail

#------------------------------------------------------------------------------
# Output
#------------------------------------------------------------------------------

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
	BOLD=$(printf '\033[1m')
	DIM=$(printf '\033[2m')
	RED=$(printf '\033[31m')
	YELLOW=$(printf '\033[33m')
	RESET=$(printf '\033[0m')
else
	BOLD='' DIM='' RED='' YELLOW='' RESET=''
fi

step() { printf '\n%s==>%s %s\n' "$BOLD" "$RESET" "$1"; }
info() { printf '    %s\n' "$1"; }
note() { printf '    %s%s%s\n' "$DIM" "$1" "$RESET"; }
warn() { printf '\n%swarning:%s %s\n' "$YELLOW" "$RESET" "$1" >&2; }

# Two arguments: what went wrong, and what to do about it. Both are required,
# because an error message without a next step is an error message that wastes
# somebody's evening.
die() {
	printf '\n%serror:%s %s\n' "$RED" "$RESET" "$1" >&2
	printf '       %s\n\n' "$2" >&2
	exit 1
}

#------------------------------------------------------------------------------
# Arguments
#------------------------------------------------------------------------------

DOMAIN=""
EMAIL=""
ASSUME_YES=0
FORCE=0
NO_TLS=0
PUBLIC_URL=""
VERSION=""

usage() {
	cat <<'EOF'
Micaforge installer.

Usage: ./install.sh [options]

Options:
  --domain <host>     The hostname this install answers on, for example
                      analytics.example.com. Its DNS must already point at this
                      machine: the certificate is ordered during this script.
  --email <address>   Where Let's Encrypt sends certificate expiry warnings, and
                      the address you will sign in with.
  --no-tls            Serve plain HTTP on port 80 and order no certificate. For
                      a laptop, or a machine behind a proxy that already
                      terminates TLS. Cannot be combined with --domain.
  --public-url <url>  With --no-tls: the address browsers actually use, for
                      example https://analytics.example.com when a proxy in
                      front terminates TLS. It goes into the tracking snippet,
                      emails and share links, and makes Caddy trust the proxy's
                      X-Forwarded-For. Default: http://localhost.
  --version <tag>     Run the published images for this release instead of
                      building from this checkout, for example --version 0.1.0.
  -y, --yes           Do not prompt. Requires --email, and either --domain or
                      --no-tls.
  --force             Overwrite an existing .env with fresh secrets. This
                      invalidates every session and makes already-sealed
                      secrets, such as stored SMTP credentials, unreadable.
  -h, --help          This text.
EOF
}

while [ $# -gt 0 ]; do
	case "$1" in
	--domain)
		[ $# -ge 2 ] || die "--domain needs a value." "Pass a hostname, for example: --domain analytics.example.com"
		DOMAIN="$2"
		shift 2
		;;
	--domain=*)
		DOMAIN="${1#*=}"
		shift
		;;
	--email)
		[ $# -ge 2 ] || die "--email needs a value." "Pass an address, for example: --email you@example.com"
		EMAIL="$2"
		shift 2
		;;
	--email=*)
		EMAIL="${1#*=}"
		shift
		;;
	--no-tls)
		NO_TLS=1
		shift
		;;
	-y | --yes)
		ASSUME_YES=1
		shift
		;;
	--force)
		FORCE=1
		shift
		;;
	--public-url)
		[ $# -ge 2 ] || die "--public-url needs a value." "Pass the address browsers use, for example: --public-url https://analytics.example.com"
		PUBLIC_URL="$2"
		shift 2
		;;
	--public-url=*)
		PUBLIC_URL="${1#*=}"
		shift
		;;
	--version)
		[ $# -ge 2 ] || die "--version needs a value." "Pass a release tag, for example: --version 0.1.0"
		VERSION="$2"
		shift 2
		;;
	--version=*)
		VERSION="${1#*=}"
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		die "Unknown option: $1" "Run ./install.sh --help for the list of options."
		;;
	esac
done

if [ "$NO_TLS" = 1 ] && [ -n "$DOMAIN" ]; then
	die "--no-tls and --domain contradict each other." "Use --domain for an HTTPS install, or --no-tls for a plain HTTP one on port 80."
fi

if [ -n "$PUBLIC_URL" ]; then
	case "$PUBLIC_URL" in
	http://* | https://*) PUBLIC_URL="${PUBLIC_URL%/}" ;;
	*) die "\"$PUBLIC_URL\" is not a URL." "Pass a full origin with its scheme, for example: --public-url https://analytics.example.com" ;;
	esac
	case "${PUBLIC_URL#*://}" in
	*/*) die "\"$PUBLIC_URL\" has a path in it." "Pass an origin only, with no path, for example: --public-url https://analytics.example.com" ;;
	esac
fi

# The repository root, whatever directory the script was called from.
cd "$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"

[ -f docker-compose.yml ] || die \
	"docker-compose.yml is not next to this script." \
	"Run install.sh from inside the Micaforge checkout: git clone https://github.com/micaforge/micaforge && cd micaforge && ./install.sh"

#------------------------------------------------------------------------------
# 1. Can this machine run it
#------------------------------------------------------------------------------

step "Checking this machine"

command -v docker >/dev/null 2>&1 || die \
	"Docker is not installed." \
	"Install it with the official script: curl -fsSL https://get.docker.com | sh   (see https://docs.docker.com/engine/install/ for other options), then run this script again."

docker info >/dev/null 2>&1 || die \
	"Docker is installed but this user cannot talk to the daemon." \
	"Start it with: sudo systemctl start docker. If it is already running, add yourself to the docker group with: sudo usermod -aG docker \$USER, then log out and back in."

docker compose version >/dev/null 2>&1 || die \
	"Docker Compose v2 is not available. The old docker-compose script will not work: this file uses the v2 schema." \
	"Install the plugin with: sudo apt-get install docker-compose-plugin, or follow https://docs.docker.com/compose/install/"

command -v openssl >/dev/null 2>&1 || die \
	"openssl is not installed, and it is what generates the secrets." \
	"Install it with: sudo apt-get install openssl   (or: sudo dnf install openssl)"

info "docker $(docker version --format '{{.Server.Version}}' 2>/dev/null || echo 'ok'), compose $(docker compose version --short 2>/dev/null || echo 'v2')"

# Advisory checks. None of these stop the install, because the operator knows
# their machine better than this script does.
if command -v free >/dev/null 2>&1; then
	mem_mb=$(free -m | awk '/^Mem:/ {print $2}')
	if [ -n "${mem_mb:-}" ] && [ "$mem_mb" -lt 1800 ]; then
		warn "This machine reports ${mem_mb} MB of RAM. Micaforge is tuned for 4 GB and ClickHouse alone is capped at 1.4 GB, so expect the kernel to kill something. Lower max_server_memory_usage in infra/clickhouse/config.xml, or use a larger machine."
	fi
fi

avail_kb=$(df -Pk . | awk 'NR==2 {print $4}')
if [ -n "${avail_kb:-}" ] && [ "$avail_kb" -lt 5000000 ]; then
	warn "Less than 5 GB free on this filesystem. The images alone are about 1.5 GB, and the event store grows from there."
fi

#------------------------------------------------------------------------------
# 2. Domain and email
#------------------------------------------------------------------------------

ask() {
	# ask <prompt> <default>; answer on stdout, prompt on stderr so command
	# substitution captures only the answer.
	local prompt="$1" default="${2:-}" reply=""
	if [ -n "$default" ]; then
		printf '    %s [%s]: ' "$prompt" "$default" >&2
	else
		printf '    %s: ' "$prompt" >&2
	fi
	read -r reply || true
	printf '%s' "${reply:-$default}"
}

step "Where this install lives"

# A re-run must not quietly move a working install to a different address, so an
# existing .env answers both questions before either is asked. `./install.sh -y`
# on an installed machine is therefore "bring it back up", not "reconfigure it".
KNOWN_DOMAIN=""
KNOWN_EMAIL=""
if [ -f .env ]; then
	KNOWN_DOMAIN=$(sed -n 's/^MICAFORGE_DOMAIN=//p' .env | tail -n1)
	KNOWN_EMAIL=$(sed -n 's/^ACME_EMAIL=//p' .env | tail -n1)
fi

if [ "$NO_TLS" = 1 ]; then
	DOMAIN=":80"
fi

if [ -z "$DOMAIN" ] && [ -n "$KNOWN_DOMAIN" ] && [ "$ASSUME_YES" = 1 ]; then
	DOMAIN="$KNOWN_DOMAIN"
	info "Using the domain already in .env: $DOMAIN"
fi

if [ -z "$DOMAIN" ]; then
	if [ "$ASSUME_YES" = 1 ]; then
		die "No domain was given and --yes forbids asking." "Pass --domain analytics.example.com, or --no-tls to serve plain HTTP on port 80."
	fi
	info "The hostname this install will answer on. Its DNS must already point"
	info "at this machine, because the certificate is ordered in a moment."
	info "Enter :80 to skip TLS and serve plain HTTP instead."
	DOMAIN=$(ask "Domain" "${KNOWN_DOMAIN:-:80}")
fi

case "$DOMAIN" in
:[0-9]*) : ;; # :80, :8080. Plain HTTP, no certificate.
*[!a-zA-Z0-9.-]* | "" | .* | *.)
	die "\"$DOMAIN\" is not a hostname this can use." "Pass a bare hostname with no scheme and no path, for example analytics.example.com, or :80 for plain HTTP."
	;;
*.*) : ;; # something.something. Good enough; Caddy validates the rest.
localhost) : ;;
*)
	die "\"$DOMAIN\" has no dot in it, so no public certificate can be issued for it." "Use a real hostname such as analytics.example.com, or :80 to serve plain HTTP."
	;;
esac

if [ -z "$EMAIL" ] && [ -n "$KNOWN_EMAIL" ] && [ "$ASSUME_YES" = 1 ]; then
	EMAIL="$KNOWN_EMAIL"
	info "Using the email already in .env: $EMAIL"
fi

if [ -z "$EMAIL" ]; then
	if [ "$ASSUME_YES" = 1 ]; then
		die "No email was given and --yes forbids asking." "Pass --email you@example.com. Let's Encrypt sends certificate expiry warnings there, and it is the address you will sign in with."
	fi
	info ""
	info "An address for certificate expiry warnings. It is also the account you"
	info "will create in a moment."
	EMAIL=$(ask "Email" "$KNOWN_EMAIL")
fi

case "$EMAIL" in
*@*.*) : ;;
*) die "\"$EMAIL\" does not look like an email address." "Pass one with --email, for example --email you@example.com. Let's Encrypt rejects an address it cannot deliver to." ;;
esac

# A `:PORT` site address is plain HTTP whatever the port, and the address
# browsers use is either what --public-url says or this machine itself.
case "$DOMAIN" in
:[0-9]*)
	SCHEME="http"
	BASE_URL="${PUBLIC_URL:-http://localhost}"
	note "Plain HTTP, no certificate. Browsers reach it at $BASE_URL."
	;;
*)
	[ -z "$PUBLIC_URL" ] || die "--public-url is for --no-tls installs." "With --domain the address is https://$DOMAIN already. Drop --public-url, or use --no-tls behind your own proxy."
	SCHEME="https"
	BASE_URL="https://$DOMAIN"
	note "HTTPS on $DOMAIN. Caddy orders the certificate on first start."
	;;
esac

# Port 80 has to be free, and has to be reachable from the internet, for the
# ACME challenge. Only the first of those is checkable from here.
if command -v ss >/dev/null 2>&1; then
	if ss -ltn 2>/dev/null | awk '{print $4}' | grep -qE '(^|[:.])80$'; then
		if ! docker compose ps --status running 2>/dev/null | grep -q caddy; then
			warn "Something is already listening on port 80. Caddy will fail to bind. Stop it, or set HTTP_PORT and HTTPS_PORT in .env and put your own proxy in front."
		fi
	fi
fi

#------------------------------------------------------------------------------
# 3. Secrets and .env
#------------------------------------------------------------------------------

step "Writing .env"

# Replace KEY=... in place, or append the line when the key is absent.
#
# The value travels through the environment rather than through the awk program
# text, so nothing in it is ever interpreted: a password containing & or | or a
# backslash is written exactly as generated. That is worth the awkwardness,
# because the one thing this function must never do is silently mangle a secret.
set_env() {
	local key="$1" value="$2" tmp
	tmp=$(mktemp "${TMPDIR:-/tmp}/micaforge.env.XXXXXX")
	MF_KEY="$key" MF_VALUE="$value" awk '
		BEGIN { k = ENVIRON["MF_KEY"]; v = ENVIRON["MF_VALUE"]; seen = 0 }
		index($0, k "=") == 1 { print k "=" v; seen = 1; next }
		{ print }
		END { if (!seen) print k "=" v }
	' .env >"$tmp"
	mv "$tmp" .env
	chmod 600 .env
}

if [ -f .env ] && [ "$FORCE" = 0 ]; then
	info ".env already exists, so it is being kept as it is."
	note "Its secrets are the ones your data is already sealed with. Run with --force to replace them, knowing that doing so logs everyone out and makes stored credentials unreadable."
else
	[ -f .env.example ] || die \
		".env.example is missing, and .env is generated from it." \
		"Restore it from the repository: git checkout .env.example"

	if [ -f .env ]; then
		backup=".env.replaced-$(date -u +%Y%m%dT%H%M%SZ)"
		cp .env "$backup"
		chmod 600 "$backup"
		warn "The previous .env was saved as $backup. Keep it until you are sure the new secrets work: it is the only copy of the old key."
	fi

	cp .env.example .env
	chmod 600 .env

	# 48 random bytes, base64. The binary refuses anything under 32 characters.
	set_env MICAFORGE_SECRET_KEY_BASE "$(openssl rand -base64 48 | tr -d '\n')"
	# Hex, because both of these are interpolated into a connection URL and hex
	# has no character that would need percent-encoding there.
	set_env POSTGRES_PASSWORD "$(openssl rand -hex 24)"
	set_env CLICKHOUSE_PASSWORD "$(openssl rand -hex 24)"
	info "Three secrets generated with openssl and written to .env (mode 600)."
fi

set_env MICAFORGE_DOMAIN "$DOMAIN"
set_env ACME_EMAIL "$EMAIL"
set_env MICAFORGE_PUBLIC_URL "$BASE_URL"
set_env MICAFORGE_DASHBOARD_URL "$BASE_URL"
info "Addresses set to $BASE_URL"

# A public URL on a plain-HTTP install means a proxy in front of Caddy. Unless
# Caddy believes that proxy's X-Forwarded-For, every visitor arrives with the
# proxy's address and they all count as one. The proxy is on this machine or
# this network, so its address is a private one.
if [ -n "$PUBLIC_URL" ]; then
	set_env MICAFORGE_TRUSTED_PROXIES "private_ranges"
	note "Caddy will trust X-Forwarded-For from private addresses (your proxy). If the proxy is a CDN with public addresses, put its ranges in MICAFORGE_TRUSTED_PROXIES in .env."
fi

if [ -n "$VERSION" ]; then
	set_env MICAFORGE_IMAGE "ghcr.io/micaforge/micaforge"
	set_env MICAFORGE_WEB_IMAGE "ghcr.io/micaforge/micaforge-web"
	set_env MICAFORGE_VERSION "$VERSION"
	info "Images: the published $VERSION release."
fi

#------------------------------------------------------------------------------
# 4. Images
#------------------------------------------------------------------------------

step "Pulling the store images"
note "postgres, clickhouse, valkey. A few hundred megabytes the first time."

docker compose pull postgres clickhouse valkey || die \
	"Pulling the images failed." \
	"That is almost always the network or a rate limit. Check the machine can reach the internet, then run this script again; it resumes where it stopped."

# Read back from .env rather than from the flag, so a re-run of an install
# that was set up with --version keeps using published images. A registry path
# (anything with a slash) means published images; a bare name means build.
IMAGE_NAME=$(sed -n 's/^MICAFORGE_IMAGE=//p' .env | tail -n1)
IMAGE_VERSION=$(sed -n 's/^MICAFORGE_VERSION=//p' .env | tail -n1)

if [ "${IMAGE_NAME#*/}" != "$IMAGE_NAME" ]; then
	step "Pulling the Micaforge $IMAGE_VERSION images"
	docker compose pull micaforge caddy || die \
		"The Micaforge $IMAGE_VERSION images could not be pulled." \
		"Check the tag exists at https://github.com/micaforge/micaforge/releases. To build from this checkout instead, set MICAFORGE_IMAGE=micaforge, MICAFORGE_WEB_IMAGE=micaforge-web and MICAFORGE_VERSION=local in .env and run this script again."
else
	step "Building the Micaforge images"
	note "The server (Rust) and the web app (Node), inside Docker. Several minutes on a small machine, and only the first time."

	docker compose build micaforge caddy || die \
		"The images did not build." \
		"Read the output above: a compiler error names the file and the line. A build killed with no message is usually the machine running out of memory; add swap, or run published images with --version."
fi

#------------------------------------------------------------------------------
# 5. Start, and migrate
#------------------------------------------------------------------------------

step "Starting the stack"
note "The stores come up first. The server then runs its Postgres migrations and creates every ClickHouse table before it opens its port, so the first start is the slow one."

docker compose up -d || die \
	"docker compose up failed." \
	"Read the message above. If it names a variable, that variable is missing from .env; if it names a port, something else on this machine is holding it."

step "Waiting for the server to report healthy"

health_of() {
	local cid
	cid=$(docker compose ps -q micaforge 2>/dev/null || true)
	if [ -z "$cid" ]; then
		printf 'missing'
		return
	fi
	docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$cid" 2>/dev/null || printf 'missing'
}

deadline=$((SECONDS + 300))
last=""
while :; do
	state=$(health_of)

	if [ "$state" = "healthy" ]; then
		printf '\n'
		info "The server is up."
		break
	fi

	if [ "$state" = "exited" ] || [ "$state" = "dead" ]; then
		printf '\n'
		docker compose logs --tail 40 micaforge || true
		die "The server container stopped." "The last 40 log lines are above. A configuration error names the variable it is about; a connection error names the store it could not reach."
	fi

	if [ "$SECONDS" -ge "$deadline" ]; then
		printf '\n'
		docker compose logs --tail 40 micaforge || true
		die "The server did not become healthy within five minutes." "The last 40 log lines are above. Watch it live with: docker compose logs -f micaforge . The stack is still running, so nothing needs to be undone before you try again."
	fi

	if [ "$state" != "$last" ]; then
		printf '    %s' "$state"
		last="$state"
	fi
	printf '.'
	sleep 5
done

#------------------------------------------------------------------------------
# 6. What now
#------------------------------------------------------------------------------

cat <<EOF

${BOLD}Micaforge is running.${RESET}

  Dashboard    ${BASE_URL}/app
  API          ${BASE_URL}/api
  Config       $(pwd)/.env      (mode 600, and the only copy of your key)

EOF

if [ "$SCHEME" = "https" ]; then
	cat <<EOF
  The certificate is ordered on the first request to ${DOMAIN}. If the page does
  not load, the usual cause is DNS: the A record has to resolve to this machine
  and port 80 has to be reachable from the internet. Watch it with:
  docker compose logs -f caddy

EOF
fi

cat <<EOF
${BOLD}Next${RESET}

  1. Open ${BASE_URL}/app and create the first account with ${EMAIL}.
     It becomes the owner of this install. Registration is closed to everyone
     else already: invite people from the dashboard, or set
     MICAFORGE_DISABLE_REGISTRATION=false in .env to open signup.

  2. Add your site. The setup screen shows its id and this snippet:

     <script
       defer
       data-site="1"
       data-host="${BASE_URL}"
       src="${BASE_URL}/mf.js"
     ></script>

     data-site="1" is the first site. The setup screen shows the real id.

  3. Agent traffic. Crawlers do not run JavaScript, so the snippet above never
     sees one. That half of the product is fed from the access log of the server
     that serves your site. Run micaforge-shipper next to it; the setup screen
     shows the command with your site id and ingest key filled in. Without it
     the Citation Gap has only one of its two numbers.

${BOLD}Operating it${RESET}

  Logs       docker compose logs -f micaforge
  Update     ./update.sh
  Back up    ./backup.sh
  Stop       docker compose down          (data survives; volumes are kept)

EOF
