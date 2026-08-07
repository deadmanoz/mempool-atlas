#!/usr/bin/env bash
set -euo pipefail

script_path=${BASH_SOURCE[0]}
script_dir=${script_path%/*}
if [[ "$script_dir" == "$script_path" ]]; then
    script_dir=.
fi
repo_root=$(cd "$script_dir/.." && pwd)

fixture_manifest="$repo_root/web/.perf-fixtures/functional/manifest.json"
fixture_server="$repo_root/web/dev/fixture-server.mjs"
smoke_script="$repo_root/scripts/smoke-public.sh"
test_tmp_dir=$(mktemp -d)
server_pid=

cleanup() {
    if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
        kill "$server_pid"
        wait "$server_pid" 2>/dev/null || true
    fi
    rm -r -- "$test_tmp_dir"
}
trap cleanup EXIT

fail() {
    printf 'offline public smoke test failed: %s\n' "$*" >&2
    exit 1
}

stop_fixture_server() {
    if [[ -n "$server_pid" ]] && kill -0 "$server_pid" 2>/dev/null; then
        kill "$server_pid"
        wait "$server_pid" 2>/dev/null || true
    fi
    server_pid=
}

start_fixture_server() {
    local cloudflare_headers=$1
    local detail_status=$2
    local log_path="$test_tmp_dir/server-$cloudflare_headers-$detail_status.log"
    local line=
    local attempt=0

    ATLAS_FIXTURE_PORT=0 \
        ATLAS_FIXTURE_CLOUDFLARE_HEADERS="$cloudflare_headers" \
        ATLAS_FIXTURE_DETAIL_STATUS="$detail_status" \
        node "$fixture_server" >"$log_path" 2>&1 &
    server_pid=$!

    while (( attempt < 200 )); do
        line=$(grep -m 1 '^fixture atlas v2 api on 127\.0\.0\.1:' \
            "$log_path" 2>/dev/null || true)
        if [[ -n "$line" ]]; then
            fixture_port=${line#*127.0.0.1:}
            fixture_port=${fixture_port%% *}
            [[ "$fixture_port" =~ ^[1-9][0-9]*$ ]] ||
                fail "fixture server reported invalid port '$fixture_port'"
            fixture_base_url="http://127.0.0.1:$fixture_port"
            return
        fi
        if ! kill -0 "$server_pid" 2>/dev/null; then
            fail "fixture server exited before becoming ready: $(<"$log_path")"
        fi
        sleep 0.05
        attempt=$((attempt + 1))
    done
    fail "fixture server did not become ready: $(<"$log_path")"
}

[[ -r "$fixture_manifest" ]] ||
    fail "functional fixture is missing; run just functional-fixtures"
source_id=$(jq --exit-status --raw-output \
    '[.snapshots[] | select(.transaction_count > 0)][0].source_id' \
    "$fixture_manifest") || fail "functional fixture has no populated source"
detail_txid=$(jq --exit-status --raw-output --arg source_id "$source_id" '
    [.transaction_details[] | select(.source_id == $source_id)][0].txid |
    select(type == "string" and test("^[0-9a-f]{64}$"))
' "$fixture_manifest") ||
    fail "functional fixture has no canonical transaction detail for source '$source_id'"

if "$smoke_script" \
    "http://127.0.0.1:1" "$source_id" \
    >"$test_tmp_dir/public-http.stdout" \
    2>"$test_tmp_dir/public-http.stderr"; then
    fail "public mode accepted an HTTP URL"
fi
grep -F 'public smoke tests require an https:// URL' \
    "$test_tmp_dir/public-http.stderr" >/dev/null ||
    fail "public mode did not report its HTTPS requirement"

if "$smoke_script" --local-fixture \
    "https://atlas.example.test" "$source_id" \
    >"$test_tmp_dir/non-loopback.stdout" \
    2>"$test_tmp_dir/non-loopback.stderr"; then
    fail "local fixture mode accepted a non-loopback URL"
fi
grep -F 'local fixture smoke tests require a loopback URL' \
    "$test_tmp_dir/non-loopback.stderr" >/dev/null ||
    fail "local fixture mode did not report its loopback restriction"

if ATLAS_SMOKE_DETAIL_TXID="$detail_txid" "$smoke_script" \
    "https://atlas.example.test" "$source_id" \
    >"$test_tmp_dir/public-detail-override.stdout" \
    2>"$test_tmp_dir/public-detail-override.stderr"; then
    fail "public mode accepted the local transaction-detail override"
fi
grep -F 'ATLAS_SMOKE_DETAIL_TXID is available only with --local-fixture' \
    "$test_tmp_dir/public-detail-override.stderr" >/dev/null ||
    fail "public mode did not reject the local transaction-detail override"

# First prove the explicit local mode tolerates the absence of Cloudflare-only
# headers while retaining every origin contract check.
start_fixture_server 0 200
ATLAS_SMOKE_DETAIL_TXID="$detail_txid" \
    "$smoke_script" --local-fixture "$fixture_base_url" "$source_id" \
    >"$test_tmp_dir/no-cloudflare.stdout"
grep -F 'public v2-only Cloudflare smoke checks passed' \
    "$test_tmp_dir/no-cloudflare.stdout" >/dev/null ||
    fail "smoke script did not report success without Cloudflare headers"
stop_fixture_server

# Repeat with lower-case, CRLF-terminated HTTP headers and synthetic edge cache
# statuses so the portable parser and both cache-status policies execute too.
start_fixture_server 1 200
ATLAS_SMOKE_DETAIL_TXID="$detail_txid" \
    "$smoke_script" --local-fixture "$fixture_base_url" "$source_id" \
    >"$test_tmp_dir/cloudflare.stdout"
grep -F 'public v2-only Cloudflare smoke checks passed' \
    "$test_tmp_dir/cloudflare.stdout" >/dev/null ||
    fail "smoke script did not report success with Cloudflare headers"
stop_fixture_server

# Exercise the expected non-success detail outcomes against the same full
# publication contract. The smoke script must validate each exact error body
# and still require the non-cacheable edge path.
for detail_status in 404 503 503-unavailable; do
    start_fixture_server 1 "$detail_status"
    ATLAS_SMOKE_DETAIL_TXID="$detail_txid" \
        "$smoke_script" --local-fixture "$fixture_base_url" "$source_id" \
        >"$test_tmp_dir/detail-$detail_status.stdout"
    grep -F 'public v2-only Cloudflare smoke checks passed' \
        "$test_tmp_dir/detail-$detail_status.stdout" >/dev/null ||
        fail "smoke script did not accept expected detail status $detail_status"
    stop_fixture_server
done

printf 'offline full-v2 public smoke checks passed\n'
