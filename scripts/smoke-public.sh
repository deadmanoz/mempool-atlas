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

require_cloudflare_cache_bypass() {
    local path=$1
    local context=$2
    local cache_status
    require_header cf-ray "$path" >/dev/null
    cache_status=$(require_header cf-cache-status "$path")
    case "$cache_status" in
        DYNAMIC|BYPASS) ;;
        *)
            printf '%s was not bypassed by Cloudflare cache: %s\n' \
                "$context" "$cache_status" >&2
            exit 1
            ;;
    esac
}

curl --silent --show-error --fail-with-body --max-time 30 \
    --dump-header "$temp_dir/sources.headers" \
    --output "$temp_dir/sources.json" \
    "$base_url/api/v2/sources"

if [[ $(status_code "$temp_dir/sources.headers") != 200 ]]; then
    printf 'source discovery did not return 200\n' >&2
    exit 1
fi
if [[ $(require_header cache-control "$temp_dir/sources.headers") != no-store ]]; then
    printf 'source discovery must remain non-cacheable\n' >&2
    exit 1
fi
require_cloudflare_cache_bypass "$temp_dir/sources.headers" "source discovery"
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

manifest_url="$base_url/api/v2/sources/$source_id/mempool"
curl --silent --show-error --fail-with-body --max-time 120 --compressed \
    --dump-header "$temp_dir/manifest.headers" \
    --output "$temp_dir/manifest.json" \
    "$manifest_url"

if [[ $(status_code "$temp_dir/manifest.headers") != 200 ]]; then
    printf 'manifest request did not return 200\n' >&2
    exit 1
fi

etag=$(require_header etag "$temp_dir/manifest.headers")
cache_control=$(require_header cache-control "$temp_dir/manifest.headers")
case "$cache_control" in
    *public*no-cache*must-revalidate*) ;;
    *)
        printf 'unexpected manifest cache policy: %s\n' "$cache_control" >&2
        exit 1
        ;;
esac

require_header cf-ray "$temp_dir/manifest.headers" >/dev/null
require_header content-encoding "$temp_dir/manifest.headers" >/dev/null
cache_status=$(require_header cf-cache-status "$temp_dir/manifest.headers")
case "$cache_status" in
    HIT|MISS|REVALIDATED|EXPIRED) ;;
    *)
        printf 'manifest was not handled by Cloudflare cache: %s\n' "$cache_status" >&2
        exit 1
        ;;
esac

query_status=$(curl --silent --show-error --max-time 30 \
    --dump-header "$temp_dir/query.headers" \
    --output "$temp_dir/query.json" --write-out '%{http_code}' \
    "$manifest_url?cache-probe=1")
[[ "$query_status" == 400 ]] || {
    printf 'manifest query probe returned %s instead of 400\n' "$query_status" >&2
    exit 1
}
[[ $(require_header cache-control "$temp_dir/query.headers") == no-store ]] || {
    printf 'manifest query rejection must remain non-cacheable\n' >&2
    exit 1
}
require_cloudflare_cache_bypass "$temp_dir/query.headers" "manifest query rejection"

jq -e --arg source_id "$source_id" '
    .transaction_count as $rows |
    .schema_version == 2 and
    .source_id == $source_id and
    (.publication_id | test("^[0-9a-f]{64}$")) and
    (.population_id | test("^[0-9a-f]{64}$")) and
    (.classification_set_id | test("^[0-9a-f]{64}$")) and
    (.stages | length >= 4) and
    all(.stages[];
        (.content_id | test("^[0-9a-f]{64}$")) and
        (.uncompressed_bytes > 0) and
        (.row_count == $rows)
    )
' "$temp_dir/manifest.json" >/dev/null

stage_number=0
while IFS=$'\t' read -r kind classifier_id content_id uncompressed_bytes; do
    stage_number=$((stage_number + 1))
    case "$kind" in
        classifier)
            stage_path="classifier/$classifier_id/$content_id"
            ;;
        population|membership|structure)
            stage_path="$kind/$content_id"
            ;;
        *)
            printf 'unexpected v2 stage kind: %s\n' "$kind" >&2
            exit 1
            ;;
    esac
    stage_url="$manifest_url/stages/$stage_path"
    curl --silent --show-error --fail-with-body --max-time 120 --compressed \
        --dump-header "$temp_dir/stage-$stage_number.headers" \
        --output "$temp_dir/stage-$stage_number.json" \
        "$stage_url"
    [[ $(status_code "$temp_dir/stage-$stage_number.headers") == 200 ]] || {
        printf 'stage request did not return 200: %s\n' "$stage_url" >&2
        exit 1
    }
    stage_cache_control=$(require_header cache-control "$temp_dir/stage-$stage_number.headers")
    case "$stage_cache_control" in
        *public*no-cache*must-revalidate*) ;;
        *)
            printf 'unexpected stage cache policy: %s\n' "$stage_cache_control" >&2
            exit 1
            ;;
    esac
    stage_etag=$(require_header etag "$temp_dir/stage-$stage_number.headers")
    [[ "$stage_etag" == *"$content_id"* ]] || {
        printf 'stage ETag does not bind its content ID: %s\n' "$stage_etag" >&2
        exit 1
    }
    [[ $(require_header x-atlas-content-id "$temp_dir/stage-$stage_number.headers") == "$content_id" ]] || {
        printf 'stage content header does not match its descriptor: %s\n' "$stage_url" >&2
        exit 1
    }
    [[ $(require_header x-atlas-uncompressed-length "$temp_dir/stage-$stage_number.headers") == "$uncompressed_bytes" ]] || {
        printf 'stage length header does not match its descriptor: %s\n' "$stage_url" >&2
        exit 1
    }
    require_header content-encoding "$temp_dir/stage-$stage_number.headers" >/dev/null
    require_header cf-ray "$temp_dir/stage-$stage_number.headers" >/dev/null
    stage_cache_status=$(require_header cf-cache-status "$temp_dir/stage-$stage_number.headers")
    case "$stage_cache_status" in
        HIT|MISS|REVALIDATED|EXPIRED) ;;
        *)
            printf 'stage was not handled by Cloudflare cache: %s\n' "$stage_cache_status" >&2
            exit 1
            ;;
    esac
    curl --silent --show-error --max-time 30 \
        --header "If-None-Match: $stage_etag" \
        --dump-header "$temp_dir/stage-$stage_number-conditional.headers" \
        --output "$temp_dir/stage-$stage_number-conditional.body" \
        "$stage_url"
    [[ $(status_code "$temp_dir/stage-$stage_number-conditional.headers") == 304 ]] || {
        printf 'conditional stage request did not return 304: %s\n' "$stage_url" >&2
        exit 1
    }
    [[ $(require_header cf-cache-status "$temp_dir/stage-$stage_number-conditional.headers") == REVALIDATED ]] || {
        printf 'conditional stage request was not synchronously revalidated: %s\n' "$stage_url" >&2
        exit 1
    }
    [[ ! -s "$temp_dir/stage-$stage_number-conditional.body" ]] || {
        printf '304 stage response unexpectedly contained a body: %s\n' "$stage_url" >&2
        exit 1
    }
    if [[ "$kind" == population ]]; then
        cp "$temp_dir/stage-$stage_number.json" "$temp_dir/population.json"
    fi
done < <(jq -r '.stages[] | [.kind, (.classifier_id // ""), .content_id, .uncompressed_bytes] | @tsv' \
    "$temp_dir/manifest.json")

[[ -s "$temp_dir/population.json" ]] || {
    printf 'manifest did not expose a population stage\n' >&2
    exit 1
}

curl --silent --show-error --max-time 30 \
    --header "If-None-Match: $etag" \
    --dump-header "$temp_dir/conditional.headers" \
    --output "$temp_dir/conditional.body" \
    "$manifest_url"

if [[ $(status_code "$temp_dir/conditional.headers") != 304 ]]; then
    printf 'conditional manifest request did not return 304\n' >&2
    exit 1
fi
[[ $(require_header cf-cache-status "$temp_dir/conditional.headers") == REVALIDATED ]] || {
    printf 'conditional manifest request was not synchronously revalidated\n' >&2
    exit 1
}
if [[ -s "$temp_dir/conditional.body" ]]; then
    printf '304 manifest response unexpectedly contained a body\n' >&2
    exit 1
fi

first_txid=$(jq -r '.txids_base64' "$temp_dir/population.json" |
    openssl base64 -d -A | dd bs=32 count=1 2>/dev/null | xxd -p -c 64)
[[ "$first_txid" =~ ^[0-9a-f]{64}$ ]] || {
    printf 'population did not decode to a canonical first txid\n' >&2
    exit 1
}
detail_url="$base_url/api/v2/sources/$source_id/transactions/$first_txid"
curl --silent --show-error --fail-with-body --max-time 30 \
    --dump-header "$temp_dir/detail.headers" \
    --output "$temp_dir/detail.json" \
    "$detail_url"
[[ $(status_code "$temp_dir/detail.headers") == 200 ]] || {
    printf 'transaction detail did not return 200\n' >&2
    exit 1
}
[[ $(require_header cache-control "$temp_dir/detail.headers") == no-store ]] || {
    printf 'transaction detail must remain non-cacheable\n' >&2
    exit 1
}
require_cloudflare_cache_bypass "$temp_dir/detail.headers" "transaction detail"

printf 'public v2-only Cloudflare smoke checks passed for %s\n' "$manifest_url"
