#!/usr/bin/env bash

set -euo pipefail

mode="${1:-}"
database="${2:-}"
component="${3:-server}"

if [[ "$mode" != "migrate" && "$mode" != "backup-only" ]]; then
    echo "usage: $0 <migrate|backup-only> <database-path> [server|agent]" >&2
    exit 2
fi

if [[ "$component" != "server" && "$component" != "agent" ]]; then
    echo "error: component must be server or agent" >&2
    exit 2
fi

if [[ -z "$database" ]]; then
    echo "error: database path is required" >&2
    exit 2
fi

backup=""
if [[ -f "$database" ]]; then
    backup_dir="${ATLAS_BACKUP_DIR:-backups}"
    mkdir -p "$backup_dir"
    timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
    backup="$backup_dir/atlas-$component-$timestamp-$$.db"
    sqlite3 "$database" ".timeout 5000" ".backup '$backup'"
    integrity="$(sqlite3 "$backup" 'PRAGMA integrity_check;')"
    if [[ "$integrity" != "ok" ]]; then
        echo "error: backup integrity check failed: $integrity" >&2
        rm -f "$backup" "${backup}-wal" "${backup}-shm"
        exit 1
    fi
    rm -f "${backup}-wal" "${backup}-shm"
    echo "backup: $backup"
elif [[ "$mode" == "backup-only" ]]; then
    echo "error: database does not exist: $database" >&2
    exit 1
fi

if [[ "$mode" == "backup-only" ]]; then
    exit 0
fi

mkdir -p "$(dirname "$database")"
if [[ "$component" == "agent" ]]; then
    migration_command=(cargo run -p atlas-agent -- migrate --database "$database")
else
    migration_command=(cargo run -p atlas-server -- migrate --database "$database")
fi
if "${migration_command[@]}"; then
    exit 0
fi

if [[ -n "$backup" ]]; then
    echo "migration failed; restoring $backup" >&2
    rm -f "$database" "$database-wal" "$database-shm"
    cp "$backup" "$database"
fi
exit 1
