#!/usr/bin/env bash
#
# Back up a Micaforge install: Postgres, and the ClickHouse partitions.
#
#   ./backup.sh
#   ./backup.sh --dir /mnt/backups --keep 14
#
# Everything lands in one dated directory, together with the restore drill for
# exactly what is in it. A backup whose restore procedure lives somewhere else
# is a backup nobody has tested.
#
# Two stores, two methods, because they are not the same kind of database.
#
#   Postgres    pg_dump, custom format. Identity and configuration: users,
#               organizations, sites, goals, funnels, dashboards, API keys,
#               the agent catalog. Small, and the part that cannot be
#               recomputed.
#
#   ClickHouse  ALTER TABLE ... FREEZE, then a tar of what it produced. FREEZE
#               makes hard links to the parts, so it costs almost nothing and
#               nothing is copied twice; the tar is the only real read. Every
#               event table is partitioned by month, so all but the current
#               month is immutable and the same bytes come out every time.
#
# What is NOT here, on purpose: .env. It holds MICAFORGE_SECRET_KEY_BASE, and
# sealed values in the Postgres dump (TOTP secrets, SMTP credentials) can only
# be read with it. Putting the key in the same directory as the data it protects
# would mean one stolen tarball is a total compromise. Keep it in a password
# manager, and read the reminder this script prints.

set -euo pipefail

# A backup is every account, key and event on this install. Nobody else on the
# machine gets to read it.
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

ROOT_DIR="backups"
KEEP=0
WANT_PG=1
WANT_CH=1

while [ $# -gt 0 ]; do
	case "$1" in
	--dir)
		[ $# -ge 2 ] || die "--dir needs a value." "Pass a directory: --dir /mnt/backups"
		ROOT_DIR="$2"
		shift 2
		;;
	--dir=*)
		ROOT_DIR="${1#*=}"
		shift
		;;
	--keep)
		[ $# -ge 2 ] || die "--keep needs a number." "Pass how many dated directories to keep: --keep 14"
		KEEP="$2"
		shift 2
		;;
	--keep=*)
		KEEP="${1#*=}"
		shift
		;;
	--postgres-only)
		WANT_CH=0
		shift
		;;
	--events-only)
		WANT_PG=0
		shift
		;;
	-h | --help)
		awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"
		printf '\nOptions:\n'
		printf '  --dir <path>       Where the dated directory goes. Default: ./backups\n'
		printf '  --keep <n>         Delete all but the newest n backups in that directory.\n'
		printf '  --postgres-only    Skip the event store.\n'
		printf '  --events-only      Skip Postgres.\n'
		exit 0
		;;
	*) die "Unknown option: $1" "Run ./backup.sh --help for the list." ;;
	esac
done

case "$KEEP" in
'' | *[!0-9]*) die "--keep takes a whole number, not \"$KEEP\"." "For example: --keep 14" ;;
esac

cd "$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"

[ -f docker-compose.yml ] || die "docker-compose.yml is not next to this script." "Run backup.sh from inside the Micaforge checkout."
[ -f .env ] || die "There is no .env here, so there is no install to back up." "Run ./install.sh first, or run this from the directory that holds your .env."
docker compose version >/dev/null 2>&1 || die "Docker Compose v2 is not available." "Install the plugin: sudo apt-get install docker-compose-plugin"

# Read one key out of .env without sourcing it: .env holds values with spaces
# and angle brackets, and `source` would try to run them.
env_value() { sed -n "s/^$1=//p" .env | tail -n1; }

PG_USER=$(env_value POSTGRES_USER)
PG_DB=$(env_value POSTGRES_DB)
CH_USER=$(env_value CLICKHOUSE_USER)
CH_DB=$(env_value CLICKHOUSE_DB)
CH_PASS=$(env_value CLICKHOUSE_PASSWORD)
PG_USER=${PG_USER:-micaforge}
PG_DB=${PG_DB:-micaforge}
CH_USER=${CH_USER:-micaforge}
CH_DB=${CH_DB:-micaforge}

running() { docker compose ps --status running 2>/dev/null | grep -q "$1"; }

STAMP=$(date -u +%Y-%m-%dT%H%M%SZ)
DEST="$ROOT_DIR/$STAMP"
# The freeze name becomes a directory name inside ClickHouse's data directory,
# and ClickHouse percent-escapes anything outside [A-Za-z0-9_] when it makes it:
# a name holding the dashes of the timestamp turns into micaforge_2026%2D08%2D22,
# and the tar that follows looks for a directory that does not exist. Dashes out,
# therefore, and the result stays readable.
#
# Named here rather than inside the ClickHouse block, because the restore drill
# below quotes it whether or not that block ran.
FREEZE_NAME="micaforge_$(printf '%s' "$STAMP" | tr -d '-')"
mkdir -p "$DEST"

printf '%sMicaforge backup%s  %s\n' "$BOLD" "$RESET" "$STAMP"
note "into $DEST"

#------------------------------------------------------------------------------
# Postgres
#------------------------------------------------------------------------------

if [ "$WANT_PG" = 1 ]; then
	step "Postgres"

	running postgres || die \
		"The postgres container is not running." \
		"Start the stack with: docker compose up -d . A stopped database cannot be dumped; if the container will not start, copy the postgres_data volume instead and say so in your notes."

	if ! docker compose exec -T postgres pg_dump -U "$PG_USER" -d "$PG_DB" -Fc >"$DEST/postgres.dump"; then
		rm -f "$DEST/postgres.dump"
		die "pg_dump failed." "The error is above. Check that POSTGRES_USER and POSTGRES_DB in .env match the running database: docker compose exec postgres psql -U $PG_USER -l"
	fi

	info "postgres.dump   $(du -h "$DEST/postgres.dump" | cut -f1)"
fi

#------------------------------------------------------------------------------
# ClickHouse
#------------------------------------------------------------------------------

if [ "$WANT_CH" = 1 ]; then
	step "ClickHouse"

	running clickhouse || die \
		"The clickhouse container is not running." \
		"Start the stack with: docker compose up -d . FREEZE is a server command, so the server has to be up to take a consistent backup."

	[ -n "$CH_PASS" ] || die \
		"CLICKHOUSE_PASSWORD is empty in .env, and clickhouse-client would stop to ask for it." \
		"Set it to the password the server was started with. If you no longer have it, read it from the container: docker compose exec clickhouse env | grep CLICKHOUSE_PASSWORD"

	# `</dev/null` is load-bearing. Both loops below feed a list of table names
	# into a `while read` over a heredoc, and `docker compose exec` inherits that
	# heredoc as its own stdin and swallows the rest of it. Without this, exactly
	# one table gets processed and the script reports success.
	ch() {
		docker compose exec -T clickhouse \
			clickhouse-client --user "$CH_USER" --password "$CH_PASS" --database "$CH_DB" "$@" </dev/null
	}

	# The schema first. Parts without the DDL that describes them are not a
	# restore, they are archaeology.
	mt_tables=$(ch --query "SELECT name FROM system.tables WHERE database = '$CH_DB' AND engine LIKE '%MergeTree%' ORDER BY name" 2>/dev/null) || die \
		"Could not list the ClickHouse tables." \
		"Check CLICKHOUSE_USER, CLICKHOUSE_PASSWORD and CLICKHOUSE_DB in .env against what the server is running: docker compose logs clickhouse | tail"

	[ -n "$mt_tables" ] || die \
		"The ClickHouse database \"$CH_DB\" has no MergeTree tables in it." \
		"That means the server has not created its schema yet. Start it and let it finish booting: docker compose up -d micaforge"

	{
		printf -- '-- Micaforge ClickHouse schema, %s\n' "$STAMP"
		printf -- '-- Run these against an empty server before attaching any partition.\n\n'
		printf 'CREATE DATABASE IF NOT EXISTS %s;\n\n' "$CH_DB"
	} >"$DEST/clickhouse-schema.sql"

	# Views are included: sessions_mv is what keeps the session rollup fed, and a
	# restore without it silently stops rolling sessions up.
	all_objects=$(ch --query "SELECT name FROM system.tables WHERE database = '$CH_DB' ORDER BY engine = 'MaterializedView', name")
	while IFS= read -r t; do
		[ -n "$t" ] || continue
		# SHOW CREATE writes a bare CREATE. The restore notes promise the file
		# is safe to run twice, so each statement is made IF NOT EXISTS here.
		ch --query "SHOW CREATE TABLE \`$CH_DB\`.\`$t\`" --format TabSeparatedRaw |
			sed -E '1s/^CREATE (TABLE|MATERIALIZED VIEW|VIEW|DICTIONARY) /CREATE \1 IF NOT EXISTS /' >>"$DEST/clickhouse-schema.sql"
		printf ';\n\n' >>"$DEST/clickhouse-schema.sql"
	done <<EOF
$all_objects
EOF

	info "clickhouse-schema.sql   $(grep -c 'CREATE ' "$DEST/clickhouse-schema.sql" || true) objects"

	# ClickHouse names its data directories after a table's UUID, so the archive
	# below is a set of UUIDs with no table names in it, and a restored table
	# gets a new UUID the moment it is created. This map is what turns those
	# directories back into tables. Without it the archive is unrestorable, and
	# the fact that it looks fine is the trap.
	ch --query "SELECT name, uuid, data_paths[1] AS path FROM system.tables WHERE database = '$CH_DB' ORDER BY name FORMAT TabSeparatedWithNames" >"$DEST/clickhouse-tables.tsv"
	info "clickhouse-tables.tsv   the UUID map the restore needs"

	# FREEZE hard-links every part into shadow/<name>. It is atomic per table and
	# costs no disk beyond the directory entries.
	frozen=0
	while IFS= read -r t; do
		[ -n "$t" ] || continue
		if ch --query "ALTER TABLE \`$CH_DB\`.\`$t\` FREEZE WITH NAME '$FREEZE_NAME'" >/dev/null; then
			frozen=$((frozen + 1))
		else
			warn "Could not freeze $t. It is not in this backup; everything frozen before it is."
		fi
	done <<EOF
$mt_tables
EOF

	[ "$frozen" -gt 0 ] || die \
		"No table could be frozen, so there is nothing to archive." \
		"The errors are above. A permission error here usually means the container's data directory is owned by another user; check: docker compose exec clickhouse ls -ld /var/lib/clickhouse/shadow"

	info "froze $frozen tables as $FREEZE_NAME"

	# Prove the directory is there before tar goes looking for it, so that a
	# name ClickHouse decided to write differently fails with an explanation
	# instead of a bare "No such file or directory".
	if ! docker compose exec -T clickhouse test -d "/var/lib/clickhouse/shadow/$FREEZE_NAME"; then
		docker compose exec -T clickhouse ls -1 /var/lib/clickhouse/shadow || true
		die "The frozen copy is not at /var/lib/clickhouse/shadow/$FREEZE_NAME" "What the shadow directory does hold is listed above. Archive the matching directory by hand, or open an issue with that listing: the name is generated, so a mismatch is a bug here."
	fi

	# Now the one real read. tar streams out of the container so nothing lands
	# on the container's own filesystem twice.
	if docker compose exec -T clickhouse \
		tar -cf - -C /var/lib/clickhouse/shadow "$FREEZE_NAME" >"$DEST/clickhouse-parts.tar"; then
		info "clickhouse-parts.tar   $(du -h "$DEST/clickhouse-parts.tar" | cut -f1)"
	else
		rm -f "$DEST/clickhouse-parts.tar"
		ch --query "SYSTEM UNFREEZE WITH NAME '$FREEZE_NAME'" >/dev/null 2>&1 || true
		die "Archiving the frozen parts failed." "The frozen copy was released again, so nothing is left behind inside the container. Check free space on this machine (df -h) and try again."
	fi

	# Release the hard links. Skipping this leaves the parts pinned: dropping an
	# old partition would then free no disk at all, which is a slow and very
	# confusing way to fill a server.
	#
	# SYSTEM UNFREEZE is the clean way and needs `enable_system_unfreeze` in
	# infra/clickhouse/config.xml, which this stack sets. Against a ClickHouse
	# that does not have it, deleting the directory does the same job: the
	# frozen copy is nothing but hard links, so removing it cannot touch live
	# data.
	if ! ch --query "SYSTEM UNFREEZE WITH NAME '$FREEZE_NAME'" >/dev/null 2>&1; then
		if docker compose exec -T clickhouse rm -rf "/var/lib/clickhouse/shadow/$FREEZE_NAME" >/dev/null 2>&1; then
			note "SYSTEM UNFREEZE was refused, so the frozen copy was deleted directly. Set enable_system_unfreeze in infra/clickhouse/config.xml to use the supported path."
		else
			warn "The frozen copy at /var/lib/clickhouse/shadow/$FREEZE_NAME could not be released, and it pins disk until it is. Remove it with: docker compose exec clickhouse rm -rf /var/lib/clickhouse/shadow/$FREEZE_NAME"
		fi
	fi
fi

#------------------------------------------------------------------------------
# Manifest and the drill
#------------------------------------------------------------------------------

step "Writing the manifest and the restore drill"

cat >"$DEST/RESTORE.md" <<EOF
# Restore drill, for the backup of $STAMP

Read this before you need it. Then do it once, on a machine that does not
matter, so that the first time is not the day the disk failed.

## Before anything: the key

\`MICAFORGE_SECRET_KEY_BASE\` is not in this directory, on purpose. Without it
the restored install will start, but every sealed value in Postgres stays
unreadable and every existing session cookie is void. Have it in hand from your
password manager before you begin. Everything below assumes the restored
\`.env\` carries the same key the backup was taken with.

## 1. An empty stack

    git clone https://github.com/micaforge/micaforge && cd micaforge
    cp /path/to/your/.env .env        # or ./install.sh, then paste the old key in
    docker compose up -d postgres clickhouse valkey

Wait for all three to report healthy: \`docker compose ps\`.

Do not start the \`micaforge\` container yet. It creates the ClickHouse schema on
boot, and the schema in this backup is the one the parts below belong to.

## 2. Postgres

Replaces identity and configuration in full.

    docker compose exec -T postgres psql -U $PG_USER -d postgres \\
      -c "DROP DATABASE IF EXISTS $PG_DB WITH (FORCE)" \\
      -c "CREATE DATABASE $PG_DB"
    docker compose exec -T postgres pg_restore -U $PG_USER -d $PG_DB --no-owner \\
      < postgres.dump

\`--no-owner\` matters when the restore target uses a different role name than
the source did.

## 3. ClickHouse: the schema

Substitute your own ClickHouse password (CLICKHOUSE_PASSWORD in .env). Step 4
uses the same variable.

    CHPASS=your_clickhouse_password
    docker compose exec -T clickhouse clickhouse-client \\
      --user $CH_USER --password "\$CHPASS" \\
      --multiquery < clickhouse-schema.sql

Every statement is \`CREATE ... IF NOT EXISTS\`, so this is safe to run twice.

## 4. ClickHouse: the data

Frozen parts are attached, not copied into place, and ClickHouse names their
directories after each table's UUID. A table recreated in step 3 has a new UUID,
so nothing lines up by path: \`clickhouse-tables.tsv\` is the map from the UUID in
the archive back to the table name, and it is why that file is in here.

Run this from inside this directory, in the same shell as step 3. It stages
every table's parts, then attaches everything that is waiting.

    CH="docker compose exec -T clickhouse clickhouse-client \\
        --user $CH_USER --password \$CHPASS --database $CH_DB"

    # a. put the frozen tree back inside the container
    docker compose cp clickhouse-parts.tar clickhouse:/tmp/parts.tar
    docker compose exec -T clickhouse tar -xf /tmp/parts.tar \\
      -C /var/lib/clickhouse/shadow

    # b. stage each table's parts into that table's detached directory
    tail -n +2 clickhouse-tables.tsv | while IFS=\$'\t' read -r table uuid _; do
      src="/var/lib/clickhouse/shadow/$FREEZE_NAME/store/\$(printf '%s' "\$uuid" | cut -c1-3)/\$uuid"
      dst=\$(\$CH --query \\
        "SELECT data_paths[1] FROM system.tables
         WHERE database='$CH_DB' AND name='\$table'" </dev/null)
      docker compose exec -T clickhouse sh -c "
        test -d '\$src' || exit 0
        mkdir -p '\${dst}detached' &&
        cp -r '\$src'/* '\${dst}detached/' &&
        chown -R clickhouse:clickhouse '\${dst}detached'" </dev/null
    done

    # c. attach everything now waiting, in one pass
    \$CH --query \\
      "SELECT DISTINCT concat('ALTER TABLE $CH_DB.', table,
              ' ATTACH PARTITION ID ''', partition_id, ''';')
       FROM system.detached_parts WHERE database='$CH_DB'" \\
      --format TabSeparatedRaw </dev/null |
      docker compose exec -T clickhouse clickhouse-client \\
        --user $CH_USER --password \$CHPASS --multiquery

\`--format TabSeparatedRaw\` in the last step is not decoration: the default
format escapes the quotes in the generated SQL and every statement then fails to
parse.

Check as you go:

    SELECT DISTINCT table, partition FROM system.detached_parts
    WHERE database = '$CH_DB';
    SELECT count() FROM $CH_DB.events;
    SELECT min(timestamp), max(timestamp) FROM $CH_DB.events;

The first query should be empty once everything has attached. Then remove the
shadow copy so it stops pinning disk:

    docker compose exec clickhouse rm -rf \\
      /var/lib/clickhouse/shadow/$FREEZE_NAME

## 5. Start the server

    docker compose up -d

It heals the ClickHouse schema on boot: any column added by a newer version is
appended to the restored tables, and nothing is dropped or retyped
(\`apps/backend/crates/store/src/ch/schema.rs\`). Postgres migrations run the
same way.

## 6. Prove it

- Sign in. If the password is refused, the key in \`.env\` is not the key this
  backup was taken with.
- Open a site's overview for "all time" and check the earliest date matches what
  \`min(timestamp)\` reported.
- Check the agent screen: agent traffic lives in a separate table, so an empty
  one here means \`agent_events\` was not attached.

## What is not in this backup

- Valkey. Deliberately: it holds the daily visitor salt, rate-limit counters and
  the live gauge, all of which expire in hours and none of which is a system of
  record. A restored install starts with a fresh salt, which means visitors seen
  today are counted again today. History is unaffected.
- Caddy's certificates. They re-issue themselves on the first request.
- The GeoIP databases in \`infra/geoip/\`. Download them again.
EOF

# Written last, so that it can checksum everything else. It excludes itself for
# the same reason: a file cannot hold its own hash.
{
	printf 'micaforge backup %s\n\n' "$STAMP"
	printf 'host        %s\n' "$(hostname 2>/dev/null || echo unknown)"
	printf 'postgres    user=%s db=%s\n' "$PG_USER" "$PG_DB"
	printf 'clickhouse  user=%s db=%s\n' "$CH_USER" "$CH_DB"
	printf 'freeze      %s\n' "$FREEZE_NAME"
	printf '\nfiles\n\n'
	if command -v sha256sum >/dev/null 2>&1; then
		(cd "$DEST" && find . -maxdepth 1 -type f ! -name MANIFEST.txt -exec sha256sum {} + 2>/dev/null | sort -k2 || true)
	else
		(cd "$DEST" && find . -maxdepth 1 -type f ! -name MANIFEST.txt -exec ls -l {} + 2>/dev/null || true)
	fi
} >"$DEST/MANIFEST.txt"

info "RESTORE.md"
info "MANIFEST.txt"

#------------------------------------------------------------------------------
# Pruning
#------------------------------------------------------------------------------

if [ "$KEEP" -gt 0 ]; then
	step "Pruning old backups"

	# Only directories that look like one of ours, newest first, everything past
	# the keep count removed. A stray directory in here is never touched, and the
	# name pattern is the whole safety mechanism, so it stays narrow.
	pruned=0
	while IFS= read -r old; do
		[ -n "$old" ] || continue
		rm -rf "$old"
		info "removed $(basename "$old")"
		pruned=$((pruned + 1))
	done <<EOF
$(find "$ROOT_DIR" -maxdepth 1 -type d -name '20*T*Z' 2>/dev/null | sort -r | tail -n +$((KEEP + 1)))
EOF

	kept=$(find "$ROOT_DIR" -maxdepth 1 -type d -name '20*T*Z' 2>/dev/null | wc -l | tr -d " ")
	note "$pruned removed, $kept kept."
fi

#------------------------------------------------------------------------------

cat <<EOF

${BOLD}Done.${RESET}  $DEST  ($(du -sh "$DEST" | cut -f1))

  The restore drill for this exact backup is in $DEST/RESTORE.md

  ${BOLD}MICAFORGE_SECRET_KEY_BASE is not in there.${RESET} It is in your .env, and it is
  what makes the sealed values in the dump readable. Keep a copy of it somewhere
  this directory is not, or a restore gets you a database you cannot open.

  Copy this directory off the machine. A backup on the disk that fails is a
  file, not a backup.

EOF
