#!/usr/bin/env bash
set -euo pipefail

script_path=${BASH_SOURCE[0]}
script_dir=${script_path%/*}
if [[ "$script_dir" == "$script_path" ]]; then
    script_dir=.
fi
# shellcheck source=lib/v2-manifest-stages.sh
source "$script_dir/lib/v2-manifest-stages.sh"

# Test-only mode permits loopback HTTP and makes the Cloudflare header pair
# optional. Every origin response, payload, validator, and cache-policy check
# below remains active.
local_fixture=0
stage_cache_control_expected='public, max-age=31536000, immutable, must-revalidate'
if [[ ${1:-} == --local-fixture ]]; then
    local_fixture=1
    shift
fi

if (( $# != 2 )); then
    printf 'usage: %s [--local-fixture] https://atlas.example.com SOURCE_ID\n' "$0" >&2
    exit 2
fi

base_url=${1%/}
source_id=$2
for required_tool in awk cp curl grep head jq mktemp openssl rm xxd; do
    command -v "$required_tool" >/dev/null 2>&1 || {
        printf 'public smoke tests require %s on PATH\n' "$required_tool" >&2
        exit 2
    }
done
if (( local_fixture )); then
    if [[ ! "$base_url" =~ ^https?://(127\.0\.0\.1|localhost)(:[0-9]+)?$ ]]; then
        printf 'local fixture smoke tests require a loopback URL\n' >&2
        exit 2
    fi
else
    case "$base_url" in
        https://*) ;;
        *)
            printf 'public smoke tests require an https:// URL\n' >&2
            exit 2
            ;;
    esac
fi

temp_dir=$(mktemp -d)
cleanup() {
    rm -r -- "$temp_dir"
}
trap cleanup EXIT

header_value() {
    local name=$1
    local path=$2
    awk -v wanted="$name" '
        {
            line = $0
            sub(/\r$/, "", line)
            separator = index(line, ":")
            if (separator == 0) next
            header = substr(line, 1, separator - 1)
            if (tolower(header) == tolower(wanted)) {
                value = substr(line, separator + 1)
                sub(/^[[:space:]]*/, "", value)
            }
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
    if (( local_fixture )) &&
        [[ -z $(header_value cf-ray "$path") ]] &&
        [[ -z $(header_value cf-cache-status "$path") ]]; then
        return
    fi
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

require_cloudflare_cache_handling() {
    local path=$1
    local context=$2
    local cache_status
    if (( local_fixture )) &&
        [[ -z $(header_value cf-ray "$path") ]] &&
        [[ -z $(header_value cf-cache-status "$path") ]]; then
        return
    fi
    require_header cf-ray "$path" >/dev/null
    cache_status=$(require_header cf-cache-status "$path")
    case "$cache_status" in
        HIT|MISS|REVALIDATED|EXPIRED) ;;
        *)
            printf '%s was not handled by Cloudflare cache: %s\n' \
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

for operational_path in healthz readyz; do
    operational_status=$(curl --silent --show-error --max-time 30 \
        --output /dev/null --write-out '%{http_code}' \
        "$base_url/$operational_path")
    if [[ "$operational_status" != 404 ]]; then
        printf 'public /%s returned %s instead of 404\n' \
            "$operational_path" "$operational_status" >&2
        exit 1
    fi
done

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

require_header content-encoding "$temp_dir/manifest.headers" >/dev/null
require_cloudflare_cache_handling "$temp_dir/manifest.headers" "manifest"

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

bare_query_status=$(curl --silent --show-error --max-time 30 \
    --dump-header "$temp_dir/bare-query.headers" \
    --output "$temp_dir/bare-query.json" --write-out '%{http_code}' \
    "$manifest_url?")
[[ "$bare_query_status" == 400 ]] || {
    printf 'bare manifest query probe returned %s instead of 400\n' \
        "$bare_query_status" >&2
    exit 1
}
[[ $(require_header cache-control "$temp_dir/bare-query.headers") == no-store ]] || {
    printf 'bare manifest query rejection must remain non-cacheable\n' >&2
    exit 1
}
require_cloudflare_cache_bypass \
    "$temp_dir/bare-query.headers" "bare manifest query rejection"

jq -e --arg source_id "$source_id" '
    .transaction_count as $rows |
    .schema_version == 2 and
    .source_id == $source_id and
    (.transaction_count | type == "number" and . >= 0 and floor == .) and
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

stage_records="$temp_dir/stages.records"
atlas_v2_manifest_stage_records "$temp_dir/manifest.json" >"$stage_records"

stage_number=0
while IFS= read -r -d '' kind &&
    IFS= read -r -d '' classifier_id &&
    IFS= read -r -d '' content_id &&
    IFS= read -r -d '' uncompressed_bytes &&
    IFS= read -r -d '' row_count &&
    IFS= read -r -d '' stage_path; do
    stage_number=$((stage_number + 1))
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
    [[ "$stage_cache_control" == "$stage_cache_control_expected" ]] || {
        printf 'unexpected stage cache policy: %s\n' "$stage_cache_control" >&2
        exit 1
    }
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
    require_cloudflare_cache_handling \
        "$temp_dir/stage-$stage_number.headers" "stage $stage_url"
    curl --silent --show-error --max-time 30 \
        --header "If-None-Match: $stage_etag" \
        --dump-header "$temp_dir/stage-$stage_number-conditional.headers" \
        --output "$temp_dir/stage-$stage_number-conditional.body" \
        "$stage_url"
    [[ $(status_code "$temp_dir/stage-$stage_number-conditional.headers") == 304 ]] || {
        printf 'conditional stage request did not return 304: %s\n' "$stage_url" >&2
        exit 1
    }
    conditional_stage_cache_control=$(require_header \
        cache-control "$temp_dir/stage-$stage_number-conditional.headers")
    [[ "$conditional_stage_cache_control" == "$stage_cache_control_expected" ]] || {
        printf 'conditional stage returned an unexpected cache policy: %s\n' \
            "$conditional_stage_cache_control" >&2
        exit 1
    }
    require_cloudflare_cache_handling \
        "$temp_dir/stage-$stage_number-conditional.headers" \
        "conditional stage $stage_url"
    [[ ! -s "$temp_dir/stage-$stage_number-conditional.body" ]] || {
        printf '304 stage response unexpectedly contained a body: %s\n' "$stage_url" >&2
        exit 1
    }
    if [[ "$kind" == population ]]; then
        cp "$temp_dir/stage-$stage_number.json" "$temp_dir/population.json"
    fi
done <"$stage_records"

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
conditional_manifest_cache_control=$(require_header cache-control "$temp_dir/conditional.headers")
case "$conditional_manifest_cache_control" in
    *public*no-cache*must-revalidate*) ;;
    *)
        printf 'conditional manifest returned an unexpected cache policy: %s\n' \
            "$conditional_manifest_cache_control" >&2
        exit 1
        ;;
esac
require_cloudflare_cache_handling \
    "$temp_dir/conditional.headers" "conditional manifest"
if [[ -s "$temp_dir/conditional.body" ]]; then
    printf '304 manifest response unexpectedly contained a body\n' >&2
    exit 1
fi

if jq -e '.transaction_count > 0' "$temp_dir/manifest.json" >/dev/null; then
    jq -r '.txids_base64' "$temp_dir/population.json" |
        openssl base64 -d -A >"$temp_dir/txids.bin"
    first_txid=$(head -c 32 "$temp_dir/txids.bin" | xxd -p -c 64)
    [[ "$first_txid" =~ ^[0-9a-f]{64}$ ]] || {
        printf 'population did not decode to a canonical first txid\n' >&2
        exit 1
    }
    detail_url="$base_url/api/v2/sources/$source_id/transactions/$first_txid"
    detail_status=$(curl --silent --show-error --max-time 30 \
        --dump-header "$temp_dir/detail.headers" \
        --output "$temp_dir/detail.json" \
        --write-out '%{http_code}' "$detail_url")
    case "$detail_status" in
        200) ;;
        404)
            jq -e --arg txid "$first_txid" '
                .error == ("transaction \"" + $txid + "\" is not in the current snapshot")
            ' "$temp_dir/detail.json" >/dev/null || {
                printf 'transaction detail returned an unexpected 404 response\n' >&2
                exit 1
            }
            ;;
        503)
            jq -e --arg txid "$first_txid" '
                .error == ("transaction \"" + $txid + "\" is present but has no policy assessment in the current snapshot")
            ' "$temp_dir/detail.json" >/dev/null || {
                printf 'transaction detail returned an unexpected 503 response\n' >&2
                exit 1
            }
            ;;
        *)
            printf 'transaction detail returned unexpected status %s\n' \
                "$detail_status" >&2
            exit 1
            ;;
    esac
    [[ $(require_header cache-control "$temp_dir/detail.headers") == no-store ]] || {
        printf 'transaction detail must remain non-cacheable\n' >&2
        exit 1
    }
    require_cloudflare_cache_bypass "$temp_dir/detail.headers" "transaction detail"
fi

printf 'public v2-only Cloudflare smoke checks passed for %s\n' "$manifest_url"
