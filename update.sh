#!/usr/bin/env bash
#
# Update a running Micaforge install.
#
#   ./update.sh
#   ./update.sh --no-pull      # rebuild from the source already in this checkout
#
# Order: dump Postgres, record what is running now, fetch the new source, pull
# the store images, rebuild the server and web images (or pull the published
# ones when .env names a registry image), restart, wait for healthy. The dump and
# the record are taken first so that a rollback is possible from the moment
# anything changes.
#
# The migrations run themselves. The server applies its Postgres migrations and
# converges the ClickHouse schema on boot, so "migrate" here is one of the
# things "start the new container" means, and there is no separate step to run.
#
# ClickHouse is not dumped by this script. Its schema only ever gains tables and
# columns (apps/backend/crates/store/src/ch/schema.rs never drops or retypes
# one), so an update cannot destroy event data. Postgres migrations can, which
# is why that one is dumped every time. For a full backup of both, run
# ./backup.sh, which this script does not do for you because it would turn a two
# minute update into a long one.

set -euo pipefail

# The dump below is every account, key and setting on this install. Nobody else
# on the machine gets to read it.
umask 077

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
die() {
	printf '\n%serror:%s %s\n' "$RED" "$RESET" "$1" >&2
	printf '       %s\n\n' "$2" >&2
	exit 1
}

PULL_SOURCE=1
while [ $# -gt 0 ]; do
	case "$1" in
	--no-pull)
		PULL_SOURCE=0
		shift
		;;
	-h | --help)
		awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
		exit 0
		;;
	*) die "Unknown option: $1" "This script takes --no-pull or --help." ;;
	esac
done

cd "$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"

[ -f docker-compose.yml ] || die "docker-compose.yml is not next to this script." "Run update.sh from inside the Micaforge checkout."
[ -f .env ] || die "There is no .env here, so there is nothing installed to update." "Run ./install.sh first."
docker compose version >/dev/null 2>&1 || die "Docker Compose v2 is not available." "Install the plugin: sudo apt-get install docker-compose-plugin"

# Read one key out of .env without sourcing it. .env holds values with spaces
# and angle brackets (MICAFORGE_MAIL_FROM), and `source` would try to run them.
env_value() { sed -n "s/^$1=//p" .env | tail -n1; }

STAMP=$(date -u +%Y-%m-%dT%H%M%SZ)
BACKUP_DIR="backups/$STAMP"

#------------------------------------------------------------------------------
# 1. Dump Postgres before anything moves
#------------------------------------------------------------------------------

step "Dumping Postgres"

PG_USER=$(env_value POSTGRES_USER)
PG_DB=$(env_value POSTGRES_DB)
PG_USER=${PG_USER:-micaforge}
PG_DB=${PG_DB:-micaforge}

if ! docker compose ps --status running 2>/dev/null | grep -q postgres; then
	die "Postgres is not running, so no pre-update dump can be taken." "Start the stack with: docker compose up -d . If it will not start, fix that before updating: an update on top of a broken install is two problems."
fi

mkdir -p "$BACKUP_DIR"

# Custom format: compressed, and restorable table by table with pg_restore.
if ! docker compose exec -T postgres pg_dump -U "$PG_USER" -d "$PG_DB" -Fc >"$BACKUP_DIR/postgres.dump"; then
	rm -f "$BACKUP_DIR/postgres.dump"
	die "pg_dump failed, so the update stopped before changing anything." "The error is above. Check that POSTGRES_USER and POSTGRES_DB in .env match the running database: docker compose exec postgres psql -U $PG_USER -l"
fi

info "$BACKUP_DIR/postgres.dump ($(du -h "$BACKUP_DIR/postgres.dump" | cut -f1))"

#------------------------------------------------------------------------------
# 2. Record what is running, so it can be put back
#------------------------------------------------------------------------------

step "Recording the current version"

{
	printf '# Micaforge state before the update of %s\n\n' "$STAMP"
	printf '## Source\n\n'
	if git rev-parse --git-dir >/dev/null 2>&1; then
		printf 'commit %s\n' "$(git rev-parse HEAD)"
		printf 'branch %s\n' "$(git rev-parse --abbrev-ref HEAD)"
	else
		printf 'not a git checkout\n'
	fi
	printf '\n## Images\n\n'
	docker compose images 2>/dev/null || true
	# Tags move; digests do not. These are what to pin in .env to put an
	# image back exactly as it was.
	printf '\n## Image digests\n\n'
	for image in $(docker compose config --images 2>/dev/null); do
		printf '%s %s\n' "$image" "$(docker image inspect --format '{{join .RepoDigests " "}}' "$image" 2>/dev/null || echo '(built locally, no digest)')"
	done
} >"$BACKUP_DIR/version-before.txt"

info "$BACKUP_DIR/version-before.txt"

ROLLBACK_NOTE="$(
	cat <<EOF
${BOLD}If this update goes wrong${RESET}

  1. Put the old source back:
       git checkout $(git rev-parse HEAD 2>/dev/null || echo '<the commit in version-before.txt>')

  2. Rebuild and restart from it:
       docker compose up -d --build

  3. Only if the new version already migrated the database, and the old one
     cannot read it, restore the dump taken a moment ago. This replaces every
     row in Postgres; event history in ClickHouse is untouched by it:
       docker compose stop micaforge
       docker compose exec -T postgres psql -U $PG_USER -d postgres \\
         -c "DROP DATABASE $PG_DB WITH (FORCE)" -c "CREATE DATABASE $PG_DB"
       docker compose exec -T postgres pg_restore -U $PG_USER -d $PG_DB \\
         --no-owner < $BACKUP_DIR/postgres.dump
       docker compose start micaforge

  The exact commands are also written into $BACKUP_DIR/ROLLBACK.md
EOF
)"

cat >"$BACKUP_DIR/ROLLBACK.md" <<EOF
# Rollback, from the update of $STAMP

Taken before the update: \`postgres.dump\` (pg_dump custom format) and
\`version-before.txt\` (the commit and the image ids that were running).

## 1. Put the old source back

    git checkout $(git rev-parse HEAD 2>/dev/null || echo '<commit from version-before.txt>')
    docker compose up -d --build

That is the whole rollback in most cases. The Micaforge server tolerates a
database that is ahead of it for additive migrations, which is what almost all
of them are.

## 2. If the old server refuses to start against the new schema

Restore Postgres. This replaces identity and configuration: users, sites, goals,
funnels, dashboards, keys. It does not touch event history, which lives in
ClickHouse and is not part of this dump.

    docker compose stop micaforge
    docker compose exec -T postgres psql -U $PG_USER -d postgres \\
      -c "DROP DATABASE $PG_DB WITH (FORCE)" -c "CREATE DATABASE $PG_DB"
    docker compose exec -T postgres pg_restore -U $PG_USER -d $PG_DB \\
      --no-owner < postgres.dump
    docker compose start micaforge

Run it from this directory, or give pg_restore the full path to postgres.dump.

## 3. Images

\`version-before.txt\` lists the image digests that were running. Postgres,
Valkey and the Micaforge images can be put back by pinning those in .env
(for example \`POSTGRES_IMAGE=postgres@sha256:...\`). ClickHouse cannot: once
a newer ClickHouse has written its parts, an older one may refuse to read them.
That is why CLICKHOUSE_IMAGE is pinned to an LTS line and should only be moved
on purpose, after ./backup.sh.

## 4. Events

Nothing in an update deletes from ClickHouse. Its schema pass only creates
tables and appends columns; it never drops or retypes one
(apps/backend/crates/store/src/ch/schema.rs). If event data is genuinely missing
after an update, that is a bug, not an expected rollback step. Restore from
./backup.sh output only if you have a reason to believe the store itself was
damaged.
EOF

#------------------------------------------------------------------------------
# 3. New source
#------------------------------------------------------------------------------

if [ "$PULL_SOURCE" = 1 ]; then
	step "Fetching the new source"

	if ! git rev-parse --git-dir >/dev/null 2>&1; then
		note "Not a git checkout, so there is nothing to pull. Rebuilding from the source that is here."
	elif [ -n "$(git status --porcelain)" ]; then
		warn "This checkout has local changes, so it was not pulled. Commit or stash them and run again, or use --no-pull to rebuild what is here on purpose."
	elif ! git pull --ff-only; then
		die "git pull could not fast-forward." "Your branch has diverged from the remote. Sort that out first: git status will say how. Nothing has been changed by this script beyond the dump in $BACKUP_DIR."
	fi
fi

#------------------------------------------------------------------------------
# 4. Rebuild and restart
#------------------------------------------------------------------------------

# An install made before the stores were pinned may still float on a tag that
# moves. Say so, because the next pull is an upgrade nobody chose.
case "$(env_value CLICKHOUSE_IMAGE)" in
*:latest | *:latest-alpine | *:head | *:head-alpine)
	warn "CLICKHOUSE_IMAGE in .env is $(env_value CLICKHOUSE_IMAGE), which moves with every ClickHouse release and cannot be rolled back. Pin it to the LTS line in .env.example (after ./backup.sh) and run this again."
	;;
esac

step "Pulling the store images"
docker compose pull postgres clickhouse valkey || die \
	"Pulling images failed." \
	"Usually the network or a registry rate limit. Nothing has changed on this machine yet; run ./update.sh again when it is back."

# A registry path (anything with a slash) means published images; a bare name
# means images built from this checkout.
case "$(env_value MICAFORGE_IMAGE)" in
*/*)
	step "Pulling the Micaforge $(env_value MICAFORGE_VERSION) images"
	note "Set MICAFORGE_VERSION in .env to the release you want before running this."
	docker compose pull micaforge caddy || die \
		"The Micaforge images could not be pulled." \
		"Check MICAFORGE_VERSION in .env names a published release. The running install is untouched."
	;;
*)
	step "Rebuilding the server and the web app"
	docker compose build micaforge caddy || die \
		"The images did not build." \
		"The output above names the file and the line. The running install is untouched, so you are not down: fix the build, or roll the source back with git checkout, and run ./update.sh again."
	;;
esac

step "Restarting"
note "Recreates only the containers whose image or configuration changed. The stores keep running unless their image moved."

docker compose up -d || die \
	"docker compose up failed." \
	"Read the message above, then roll back with the steps in $BACKUP_DIR/ROLLBACK.md"

#------------------------------------------------------------------------------
# 5. Wait, and prove it came back
#------------------------------------------------------------------------------

step "Waiting for the server to report healthy"
note "It migrates on boot, so this is where the new schema is applied."

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
while :; do
	state=$(health_of)

	if [ "$state" = "healthy" ]; then
		printf '\n'
		break
	fi

	if [ "$state" = "exited" ] || [ "$state" = "dead" ]; then
		printf '\n'
		docker compose logs --tail 40 micaforge || true
		printf '%s\n' "$ROLLBACK_NOTE"
		die "The updated server stopped." "The last 40 log lines are above and the rollback steps are just before them."
	fi

	if [ "$SECONDS" -ge "$deadline" ]; then
		printf '\n'
		docker compose logs --tail 40 micaforge || true
		printf '%s\n' "$ROLLBACK_NOTE"
		die "The updated server did not become healthy within five minutes." "A long migration can legitimately take this long; watch it with docker compose logs -f micaforge before rolling back."
	fi

	printf '.'
	sleep 5
done

step "Updated"
info "Version now running:"
docker compose images micaforge 2>/dev/null || true

cat <<EOF

  Pre-update dump   $BACKUP_DIR/postgres.dump
  Rollback steps    $BACKUP_DIR/ROLLBACK.md

  Old backup directories are not pruned by this script. ./backup.sh --keep does
  that, or delete them yourself once an update has proven itself.

EOF
