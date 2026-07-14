#!/usr/bin/env bash

set -euo pipefail

mode="${1:-}"
database="${2:-}"

if [[ "$mode" != "migrate" && "$mode" != "backup-only" ]]; then
    echo "usage: $0 <migrate|backup-only> <database-path>" >&2
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
    backup="$backup_dir/atlas-$timestamp-$$.db"
    sqlite3 "$database" ".timeout 5000" ".backup '$backup'"
    integrity="$(sqlite3 "$backup" 'PRAGMA integrity_check;')"
    if [[ "$integrity" != "ok" ]]; then
        echo "error: backup integrity check failed: $integrity" >&2
        rm -f "$backup"
        exit 1
    fi
    echo "backup: $backup"
elif [[ "$mode" == "backup-only" ]]; then
    echo "error: database does not exist: $database" >&2
    exit 1
fi

if [[ "$mode" == "backup-only" ]]; then
    exit 0
fi

mkdir -p "$(dirname "$database")"
if cargo run -p atlas-server -- migrate --database "$database"; then
    exit 0
fi

if [[ -n "$backup" ]]; then
    echo "migration failed; restoring $backup" >&2
    rm -f "$database" "$database-wal" "$database-shm"
    cp "$backup" "$database"
fi
exit 1
