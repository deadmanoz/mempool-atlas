#!/usr/bin/env bash
set -euo pipefail

if (( $# != 2 )); then
    printf 'usage: %s https://atlas.example.com SOURCE_ID\n' "$0" >&2
    exit 2
fi

base_url=${1%/}
source_id=$2
case "$base_url" in
    https://*) ;;
    *)
        printf 'public smoke tests require an https:// URL\n' >&2
        exit 2
        ;;
esac

temp_dir=$(mktemp -d)
cleanup() {
    rm -r -- "$temp_dir"
}
trap cleanup EXIT

header_value() {
    local name=$1
    local path=$2
    awk -v wanted="$name" '
        BEGIN { IGNORECASE = 1 }
        $0 ~ "^" wanted ":" {
            sub(/^[^:]+:[[:space:]]*/, "")
            sub(/\r$/, "")
            value = $0
        }
        END { print value }
    ' "$path"
}

status_code() {
    awk '/^HTTP\// { code = $2 } END { print code }' "$1"
}

require_header() {
    local name=$1
    local path=$2
    local value
    value=$(header_value "$name" "$path")
    if [[ -z "$value" ]]; then
        printf 'missing %s header in %s\n' "$name" "$path" >&2
        exit 1
    fi
    printf '%s' "$value"
}

curl --silent --show-error --fail-with-body --max-time 30 \
    --dump-header "$temp_dir/sources.headers" \
    --output "$temp_dir/sources.json" \
    "$base_url/api/v1/sources"

if [[ $(status_code "$temp_dir/sources.headers") != 200 ]]; then
    printf 'source discovery did not return 200\n' >&2
    exit 1
fi
if [[ $(require_header cache-control "$temp_dir/sources.headers") != no-store ]]; then
    printf 'source discovery must remain non-cacheable\n' >&2
    exit 1
fi
if ! grep -Eq '"atlas_version"[[:space:]]*:[[:space:]]*"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)([-+][0-9A-Za-z.-]+)?"' \
    "$temp_dir/sources.json"; then
    printf 'source discovery is missing a valid atlas_version\n' >&2
    exit 1
fi

health_status=$(curl --silent --show-error --max-time 30 \
    --output /dev/null --write-out '%{http_code}' "$base_url/healthz")
if [[ "$health_status" != 404 ]]; then
    printf 'public /healthz returned %s instead of 404\n' "$health_status" >&2
    exit 1
fi

snapshot_url="$base_url/api/v1/sources/$source_id/mempool"
curl --silent --show-error --fail-with-body --max-time 120 --compressed \
    --dump-header "$temp_dir/snapshot.headers" \
    --output "$temp_dir/snapshot.json" \
    "$snapshot_url"

if [[ $(status_code "$temp_dir/snapshot.headers") != 200 ]]; then
    printf 'snapshot request did not return 200\n' >&2
    exit 1
fi

etag=$(require_header etag "$temp_dir/snapshot.headers")
cache_control=$(require_header cache-control "$temp_dir/snapshot.headers")
case "$cache_control" in
    *public*no-cache*must-revalidate*) ;;
    *)
        printf 'unexpected snapshot cache policy: %s\n' "$cache_control" >&2
        exit 1
        ;;
esac

require_header cf-ray "$temp_dir/snapshot.headers" >/dev/null
require_header content-encoding "$temp_dir/snapshot.headers" >/dev/null
cache_status=$(require_header cf-cache-status "$temp_dir/snapshot.headers")
case "$cache_status" in
    HIT|MISS|REVALIDATED|EXPIRED) ;;
    *)
        printf 'snapshot was not handled by Cloudflare cache: %s\n' "$cache_status" >&2
        exit 1
        ;;
esac

curl --silent --show-error --max-time 30 \
    --header "If-None-Match: $etag" \
    --dump-header "$temp_dir/conditional.headers" \
    --output "$temp_dir/conditional.body" \
    "$snapshot_url"

if [[ $(status_code "$temp_dir/conditional.headers") != 304 ]]; then
    printf 'conditional snapshot request did not return 304\n' >&2
    exit 1
fi
if [[ -s "$temp_dir/conditional.body" ]]; then
    printf '304 snapshot response unexpectedly contained a body\n' >&2
    exit 1
fi

printf 'public Cloudflare smoke checks passed for %s\n' "$snapshot_url"
