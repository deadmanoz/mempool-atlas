#!/usr/bin/env bash
set -euo pipefail

script_path=${BASH_SOURCE[0]}
script_dir=${script_path%/*}
if [[ "$script_dir" == "$script_path" ]]; then
    script_dir=.
fi
# shellcheck source=lib/v2-manifest-stages.sh
source "$script_dir/lib/v2-manifest-stages.sh"
# shellcheck source=lib/smoke-public-helpers.sh
source "$script_dir/lib/smoke-public-helpers.sh"

# Test-only mode permits loopback HTTP and makes the Cloudflare header pair
# optional. Every origin response, payload, validator, and cache-policy check
# below remains active.
local_fixture=0
manifest_cache_control_expected='public, no-cache, must-revalidate'
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
detail_txid_override=${ATLAS_SMOKE_DETAIL_TXID:-}
if [[ -n "$detail_txid_override" ]]; then
    if (( !local_fixture )); then
        printf 'ATLAS_SMOKE_DETAIL_TXID is available only with --local-fixture\n' >&2
        exit 2
    fi
    if [[ ! "$detail_txid_override" =~ ^[0-9a-f]{64}$ ]]; then
        printf 'ATLAS_SMOKE_DETAIL_TXID must be a canonical txid\n' >&2
        exit 2
    fi
fi
for required_tool in awk cp curl grep head jq mkdir mktemp openssl rm xxd; do
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

status_code() {
    awk '/^HTTP\// { code = $2 } END { print code }' "$1"
}

require_header() {
    local name=$1
    local path=$2
    local value
    value=$(atlas_smoke_header_value "$name" "$path")
    if [[ -z "$value" ]]; then
        printf 'missing %s header in %s\n' "$name" "$path" >&2
        return 1
    fi
    printf '%s' "$value"
}

header_contains_token() {
    local value=$1
    local wanted=$2
    awk -v value="$value" -v wanted="$wanted" '
        BEGIN {
            count = split(value, tokens, ",")
            for (token_index = 1; token_index <= count; token_index += 1) {
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", tokens[token_index])
                if (tolower(tokens[token_index]) == tolower(wanted)) exit 0
            }
            exit 1
        }
    '
}

require_cloudflare_cache_bypass() {
    local path=$1
    local context=$2
    local cache_status
    if (( local_fixture )) &&
        [[ -z $(atlas_smoke_header_value cf-ray "$path") ]] &&
        [[ -z $(atlas_smoke_header_value cf-cache-status "$path") ]]; then
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
        [[ -z $(atlas_smoke_header_value cf-ray "$path") ]] &&
        [[ -z $(atlas_smoke_header_value cf-cache-status "$path") ]]; then
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

stage_error_probe_number=0
validate_stage_error_response() {
    local context=$1
    local expected_error=$2
    local headers=$3
    local body=$4
    local cache_control

    cache_control=$(require_header cache-control "$headers")
    [[ "$cache_control" == no-store ]] || {
        printf '%s must remain non-cacheable\n' "$context" >&2
        exit 1
    }
    [[ -z $(atlas_smoke_header_value etag "$headers") ]] || {
        printf '%s unexpectedly returned an ETag\n' "$context" >&2
        exit 1
    }
    jq --exit-status --arg error "$expected_error" '
        type == "object" and keys == ["error"] and .error == $error
    ' "$body" >/dev/null || {
        printf '%s returned an unexpected error body\n' "$context" >&2
        exit 1
    }
    require_cloudflare_cache_bypass "$headers" "$context"
}

require_stage_error() {
    local context=$1
    local url=$2
    local expected_status=$3
    local expected_error=$4
    local allow_superseded=${5:-0}
    local status
    local headers
    local body

    stage_error_probe_number=$((stage_error_probe_number + 1))
    headers="$temp_dir/stage-error-$stage_error_probe_number.headers"
    body="$temp_dir/stage-error-$stage_error_probe_number.json"
    status=$(curl --silent --show-error --max-time 30 \
        --dump-header "$headers" \
        --output "$body" \
        --write-out '%{http_code}' \
        "$url")
    if [[ "$status" == 409 && "$expected_status" != 409 ]] &&
        (( allow_superseded )); then
        validate_stage_error_response \
            "$context supersession" \
            "requested stage is not part of the current v2 publication" \
            "$headers" \
            "$body"
        stage_error_superseded=1
        return
    fi
    [[ "$status" == "$expected_status" ]] || {
        printf '%s returned %s instead of %s\n' \
            "$context" "$status" "$expected_status" >&2
        exit 1
    }
    validate_stage_error_response \
        "$context" "$expected_error" "$headers" "$body"
}

curl --silent --show-error --fail-with-body --max-time 30 \
    --dump-header "$temp_dir/sources.headers" \
    --output "$temp_dir/sources.json" \
    "$base_url/api/v2/sources"

if [[ $(status_code "$temp_dir/sources.headers") != 200 ]]; then
    printf 'source discovery did not return 200\n' >&2
    exit 1
fi
source_cache_control=$(require_header cache-control "$temp_dir/sources.headers")
if [[ "$source_cache_control" != no-store ]]; then
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
query_status=$(curl --silent --show-error --max-time 30 \
    --dump-header "$temp_dir/query.headers" \
    --output "$temp_dir/query.json" --write-out '%{http_code}' \
    "$manifest_url?cache-probe=1")
[[ "$query_status" == 400 ]] || {
    printf 'manifest query probe returned %s instead of 400\n' "$query_status" >&2
    exit 1
}
query_cache_control=$(require_header cache-control "$temp_dir/query.headers")
[[ "$query_cache_control" == no-store ]] || {
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
bare_query_cache_control=$(require_header cache-control "$temp_dir/bare-query.headers")
[[ "$bare_query_cache_control" == no-store ]] || {
    printf 'bare manifest query rejection must remain non-cacheable\n' >&2
    exit 1
}
require_cloudflare_cache_bypass \
    "$temp_dir/bare-query.headers" "bare manifest query rejection"

absent_stage_id=$(printf 'atlas-smoke-absent-stage' |
    openssl dgst -sha256 -r |
    awk '{ print $1 }')
publication_restart_limit=3
publication_attempt_limit=$((publication_restart_limit + 1))
publication_attempt=1
verified_manifest=
verified_population=

while (( publication_attempt <= publication_attempt_limit )); do
    attempt_dir="$temp_dir/publication-attempt-$publication_attempt"
    mkdir "$attempt_dir"
    publication_superseded=0
    stage_error_superseded=0

    curl --silent --show-error --fail-with-body --max-time 120 --compressed \
        --dump-header "$attempt_dir/manifest.headers" \
        --output "$attempt_dir/manifest.json" \
        "$manifest_url"

    if [[ $(status_code "$attempt_dir/manifest.headers") != 200 ]]; then
        printf 'manifest request did not return 200\n' >&2
        exit 1
    fi

    etag=$(require_header etag "$attempt_dir/manifest.headers")
    cache_control=$(require_header cache-control "$attempt_dir/manifest.headers")
    [[ "$cache_control" == "$manifest_cache_control_expected" ]] || {
        printf 'unexpected manifest cache policy: %s\n' "$cache_control" >&2
        exit 1
    }

    require_header content-encoding "$attempt_dir/manifest.headers" >/dev/null
    require_cloudflare_cache_handling "$attempt_dir/manifest.headers" "manifest"

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
    ' "$attempt_dir/manifest.json" >/dev/null

    population_id=$(jq --exit-status --raw-output '.population_id' \
        "$attempt_dir/manifest.json")
    require_stage_error \
        "malformed stage content ID" \
        "$manifest_url/stages/population/not-a-digest" \
        400 \
        "invalid v2 stage content ID"
    require_stage_error \
        "current content ID on the wrong stage lane" \
        "$manifest_url/stages/membership/$population_id" \
        404 \
        "v2 stage does not exist for the requested kind or classifier" \
        1
    if (( stage_error_superseded )); then
        publication_superseded=1
    fi

    if (( !publication_superseded )); then
        if jq --exit-status --arg content_id "$absent_stage_id" \
            'any(.stages[]; .content_id == $content_id)' \
            "$attempt_dir/manifest.json" >/dev/null; then
            printf 'stage error probe digest unexpectedly belongs to the publication\n' >&2
            exit 1
        fi
        require_stage_error \
            "well-formed absent stage content ID" \
            "$manifest_url/stages/population/$absent_stage_id" \
            409 \
            "requested stage is not part of the current v2 publication"
    fi

    if (( !publication_superseded )); then
        stage_records="$attempt_dir/stages.records"
        atlas_v2_manifest_stage_records \
            "$attempt_dir/manifest.json" >"$stage_records"

        stage_number=0
        while IFS= read -r -d '' kind &&
            IFS= read -r -d '' classifier_id &&
            IFS= read -r -d '' content_id &&
            IFS= read -r -d '' uncompressed_bytes &&
            IFS= read -r -d '' row_count &&
            IFS= read -r -d '' stage_path; do
            stage_number=$((stage_number + 1))
            stage_url="$manifest_url/stages/$stage_path"
            stage_headers="$attempt_dir/stage-$stage_number.headers"
            stage_body="$attempt_dir/stage-$stage_number.json"
            stage_status=$(curl --silent --show-error --max-time 120 --compressed \
                --dump-header "$stage_headers" \
                --output "$stage_body" \
                --write-out '%{http_code}' \
                "$stage_url")
            case "$stage_status" in
                200) ;;
                409)
                    validate_stage_error_response \
                        "stage $stage_url supersession" \
                        "requested stage is not part of the current v2 publication" \
                        "$stage_headers" \
                        "$stage_body"
                    publication_superseded=1
                    break
                    ;;
                *)
                    printf 'stage request returned unexpected status %s: %s\n' \
                        "$stage_status" "$stage_url" >&2
                    exit 1
                    ;;
            esac
            stage_cache_control=$(require_header cache-control "$stage_headers")
            [[ "$stage_cache_control" == "$stage_cache_control_expected" ]] || {
                printf 'unexpected stage cache policy: %s\n' "$stage_cache_control" >&2
                exit 1
            }
            stage_etag=$(require_header etag "$stage_headers")
            [[ "$stage_etag" == *"$content_id"* ]] || {
                printf 'stage ETag does not bind its content ID: %s\n' "$stage_etag" >&2
                exit 1
            }
            stage_content_id=$(require_header x-atlas-content-id "$stage_headers")
            [[ "$stage_content_id" == "$content_id" ]] || {
                printf 'stage content header does not match its descriptor: %s\n' \
                    "$stage_url" >&2
                exit 1
            }
            stage_uncompressed_length=$(require_header \
                x-atlas-uncompressed-length "$stage_headers")
            [[ "$stage_uncompressed_length" == "$uncompressed_bytes" ]] || {
                printf 'stage length header does not match its descriptor: %s\n' \
                    "$stage_url" >&2
                exit 1
            }
            require_header content-encoding "$stage_headers" >/dev/null
            stage_vary=$(require_header vary "$stage_headers")
            header_contains_token "$stage_vary" accept-encoding || {
                printf 'stage Vary header does not include Accept-Encoding: %s\n' \
                    "$stage_url" >&2
                exit 1
            }
            require_cloudflare_cache_handling "$stage_headers" "stage $stage_url"

            conditional_stage_headers="$attempt_dir/stage-$stage_number-conditional.headers"
            conditional_stage_body="$attempt_dir/stage-$stage_number-conditional.body"
            conditional_stage_status=$(curl --silent --show-error --max-time 30 \
                --header "If-None-Match: $stage_etag" \
                --dump-header "$conditional_stage_headers" \
                --output "$conditional_stage_body" \
                --write-out '%{http_code}' \
                "$stage_url")
            case "$conditional_stage_status" in
                304) ;;
                409)
                    validate_stage_error_response \
                        "conditional stage $stage_url supersession" \
                        "requested stage is not part of the current v2 publication" \
                        "$conditional_stage_headers" \
                        "$conditional_stage_body"
                    publication_superseded=1
                    break
                    ;;
                *)
                    printf 'conditional stage request returned unexpected status %s: %s\n' \
                        "$conditional_stage_status" "$stage_url" >&2
                    exit 1
                    ;;
            esac
            conditional_stage_cache_control=$(require_header \
                cache-control "$conditional_stage_headers")
            [[ "$conditional_stage_cache_control" == "$stage_cache_control_expected" ]] || {
                printf 'conditional stage returned an unexpected cache policy: %s\n' \
                    "$conditional_stage_cache_control" >&2
                exit 1
            }
            conditional_stage_vary=$(require_header vary "$conditional_stage_headers")
            header_contains_token "$conditional_stage_vary" accept-encoding || {
                printf 'conditional stage Vary header does not include Accept-Encoding: %s\n' \
                    "$stage_url" >&2
                exit 1
            }
            require_cloudflare_cache_handling \
                "$conditional_stage_headers" "conditional stage $stage_url"
            [[ ! -s "$conditional_stage_body" ]] || {
                printf '304 stage response unexpectedly contained a body: %s\n' \
                    "$stage_url" >&2
                exit 1
            }
            if [[ "$kind" == population ]]; then
                cp "$stage_body" "$attempt_dir/population.json"
            fi
        done <"$stage_records"
    fi

    if (( !publication_superseded )); then
        [[ -s "$attempt_dir/population.json" ]] || {
            printf 'manifest did not expose a population stage\n' >&2
            exit 1
        }

        conditional_manifest_headers="$attempt_dir/conditional.headers"
        conditional_manifest_body="$attempt_dir/conditional.body"
        conditional_manifest_status=$(curl --silent --show-error --max-time 30 \
            --header "If-None-Match: $etag" \
            --dump-header "$conditional_manifest_headers" \
            --output "$conditional_manifest_body" \
            --write-out '%{http_code}' \
            "$manifest_url")
        conditional_manifest_cache_control=$(require_header \
            cache-control "$conditional_manifest_headers")
        [[ "$conditional_manifest_cache_control" == "$manifest_cache_control_expected" ]] || {
            printf 'conditional manifest returned an unexpected cache policy: %s\n' \
                "$conditional_manifest_cache_control" >&2
            exit 1
        }
        require_cloudflare_cache_handling \
            "$conditional_manifest_headers" "conditional manifest"
        case "$conditional_manifest_status" in
            304)
                [[ ! -s "$conditional_manifest_body" ]] || {
                    printf '304 manifest response unexpectedly contained a body\n' >&2
                    exit 1
                }
                ;;
            200)
                replacement_etag=$(require_header etag "$conditional_manifest_headers")
                [[ "$replacement_etag" != "$etag" ]] || {
                    printf 'conditional manifest returned 200 with the prior ETag\n' >&2
                    exit 1
                }
                publication_superseded=1
                ;;
            *)
                printf 'conditional manifest request returned unexpected status %s\n' \
                    "$conditional_manifest_status" >&2
                exit 1
                ;;
        esac
    fi

    if (( publication_superseded )); then
        if (( publication_attempt == publication_attempt_limit )); then
            printf 'publication changed during all %s smoke verification attempts\n' \
                "$publication_attempt_limit" >&2
            exit 1
        fi
        printf 'publication changed during smoke verification; restarting with a fresh manifest (%s/%s)\n' \
            "$publication_attempt" "$publication_restart_limit" >&2
        publication_attempt=$((publication_attempt + 1))
        continue
    fi

    verified_manifest="$attempt_dir/manifest.json"
    verified_population="$attempt_dir/population.json"
    break
done

[[ -n "$verified_manifest" && -n "$verified_population" ]] || {
    printf 'publication verification did not produce a complete snapshot\n' >&2
    exit 1
}

if jq -e '.transaction_count > 0' "$verified_manifest" >/dev/null; then
    first_txid=$(atlas_smoke_first_txid "$verified_population") || {
        printf 'population did not decode to a canonical first txid\n' >&2
        exit 1
    }
    detail_txid=${detail_txid_override:-$first_txid}
    detail_url="$base_url/api/v2/sources/$source_id/transactions/$detail_txid"
    detail_status=$(curl --silent --show-error --max-time 30 \
        --dump-header "$temp_dir/detail.headers" \
        --output "$temp_dir/detail.json" \
        --write-out '%{http_code}' "$detail_url")
    case "$detail_status" in
        200) ;;
        404)
            jq -e --arg txid "$detail_txid" '
                .error == ("transaction \"" + $txid + "\" is not in the current snapshot")
            ' "$temp_dir/detail.json" >/dev/null || {
                printf 'transaction detail returned an unexpected 404 response\n' >&2
                exit 1
            }
            ;;
        503)
            jq -e --arg txid "$detail_txid" '
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
    detail_cache_control=$(require_header cache-control "$temp_dir/detail.headers")
    [[ "$detail_cache_control" == no-store ]] || {
        printf 'transaction detail must remain non-cacheable\n' >&2
        exit 1
    }
    require_cloudflare_cache_bypass "$temp_dir/detail.headers" "transaction detail"
fi

printf 'public v2-only Cloudflare smoke checks passed for %s\n' "$manifest_url"
