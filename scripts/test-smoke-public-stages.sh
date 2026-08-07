#!/usr/bin/env bash
set -euo pipefail

script_path=${BASH_SOURCE[0]}
script_dir=${script_path%/*}
if [[ "$script_dir" == "$script_path" ]]; then
    script_dir=.
fi
repo_root=$(cd "$script_dir/.." && pwd)

command -v jq >/dev/null 2>&1 || {
    printf 'v2 smoke stage parser tests require jq on PATH\n' >&2
    exit 2
}

# shellcheck source=lib/v2-manifest-stages.sh
source "$script_dir/lib/v2-manifest-stages.sh"

fixture="$repo_root/tests/fixtures/publication-digest-v2.json"
manifest_url=https://atlas.example.test/api/v2/sources/perf-node-01/mempool
test_tmp_dir=$(mktemp -d)
records="$test_tmp_dir/records"
cleanup() {
    rm -r -- "$test_tmp_dir"
}
trap cleanup EXIT

atlas_v2_manifest_stage_records "$fixture" >"$records"

fail() {
    printf 'v2 smoke stage parser test failed: %s\n' "$*" >&2
    exit 1
}

require_equal() {
    local context=$1
    local actual=$2
    local expected=$3
    [[ "$actual" == "$expected" ]] ||
        fail "$context: expected '$expected', got '$actual'"
}

require_empty() {
    local context=$1
    local actual=$2
    [[ -z "$actual" ]] || fail "$context: expected an empty value, got '$actual'"
}

require_match() {
    local context=$1
    local actual=$2
    local pattern=$3
    [[ "$actual" =~ $pattern ]] ||
        fail "$context: '$actual' does not match $pattern"
}

require_contains() {
    local context=$1
    local actual=$2
    local expected=$3
    [[ "$actual" == *"$expected"* ]] ||
        fail "$context: expected message '$expected', got '$actual'"
}

negative_case_count=0
expect_manifest_error() {
    local context=$1
    local expected_message=$2
    local mutation=$3
    negative_case_count=$((negative_case_count + 1))
    local mutated_manifest="$test_tmp_dir/negative-$negative_case_count.json"
    local stdout_file="$test_tmp_dir/negative-$negative_case_count.stdout"
    local stderr_file="$test_tmp_dir/negative-$negative_case_count.stderr"

    jq "$mutation" "$fixture" >"$mutated_manifest" ||
        fail "$context: failed to create mutated manifest"
    if atlas_v2_manifest_stage_records "$mutated_manifest" >"$stdout_file" 2>"$stderr_file"; then
        fail "$context: parser unexpectedly accepted the mutated manifest"
    fi
    require_contains "$context" "$(<"$stderr_file")" "$expected_message"
}

stage_count=0
population_count=0
membership_count=0
structure_count=0
classifier_count=0
classifier_ids=
uncompressed_sizes=
while IFS= read -r -d '' kind &&
    IFS= read -r -d '' classifier_id &&
    IFS= read -r -d '' content_id &&
    IFS= read -r -d '' uncompressed_bytes &&
    IFS= read -r -d '' row_count &&
    IFS= read -r -d '' stage_path; do
    stage_count=$((stage_count + 1))
    require_match "stage $stage_count content ID" "$content_id" '^[0-9a-f]{64}$'
    require_match \
        "stage $stage_count uncompressed byte count" \
        "$uncompressed_bytes" \
        '^[1-9][0-9]*$'
    require_equal "stage $stage_count row count" "$row_count" 3
    uncompressed_sizes="${uncompressed_sizes:+$uncompressed_sizes }$uncompressed_bytes"
    stage_url="$manifest_url/stages/$stage_path"

    case "$kind" in
        population)
            population_count=$((population_count + 1))
            # Regression coverage: the empty classifier field must not shift
            # the following content ID and byte-count fields left.
            require_empty "population classifier ID" "$classifier_id"
            require_equal \
                "population stage path" \
                "$stage_path" \
                "population/$content_id"
            require_equal \
                "population stage URL" \
                "$stage_url" \
                "$manifest_url/stages/population/$content_id"
            ;;
        membership)
            membership_count=$((membership_count + 1))
            require_empty "membership classifier ID" "$classifier_id"
            require_equal \
                "membership stage path" \
                "$stage_path" \
                "membership/$content_id"
            require_equal \
                "membership stage URL" \
                "$stage_url" \
                "$manifest_url/stages/membership/$content_id"
            ;;
        structure)
            structure_count=$((structure_count + 1))
            require_empty "structure classifier ID" "$classifier_id"
            require_equal \
                "structure stage path" \
                "$stage_path" \
                "structure/$content_id"
            require_equal \
                "structure stage URL" \
                "$stage_url" \
                "$manifest_url/stages/structure/$content_id"
            ;;
        classifier)
            classifier_count=$((classifier_count + 1))
            require_match \
                "classifier stage $stage_count ID" \
                "$classifier_id" \
                '^[a-z][a-z0-9_]*$'
            require_equal \
                "classifier stage $stage_count path" \
                "$stage_path" \
                "classifier/$classifier_id/$content_id"
            require_equal \
                "classifier stage $stage_count URL" \
                "$stage_url" \
                "$manifest_url/stages/classifier/$classifier_id/$content_id"
            classifier_ids="${classifier_ids:+$classifier_ids }$classifier_id"
            ;;
        *)
            fail "stage $stage_count has unexpected kind '$kind'"
            ;;
    esac

done <"$records"

require_equal "stage count" "$stage_count" 7
require_equal "population stage count" "$population_count" 1
require_equal "membership stage count" "$membership_count" 1
require_equal "structure stage count" "$structure_count" 1
require_equal "classifier stage count" "$classifier_count" 4
require_equal \
    "classifier stage order" \
    "$classifier_ids" \
    "transaction_properties transaction_shape data_protocols knots_bip110"
expected_uncompressed_sizes=$(
    jq --raw-output \
        '[.stages[].uncompressed_bytes | tostring] | join(" ")' \
        "$fixture"
)
require_equal \
    "uncompressed stage sizes" \
    "$uncompressed_sizes" \
    "$expected_uncompressed_sizes"

expect_manifest_error \
    "negative transaction count" \
    "invalid manifest transaction_count" \
    '.transaction_count = -1'
expect_manifest_error \
    "negative stages container" \
    "invalid manifest stages" \
    '.stages = {}'
expect_manifest_error \
    "negative duplicate population" \
    "manifest must declare one population stage" \
    '.stages += [(.stages[] | select(.kind == "population"))]'
expect_manifest_error \
    "negative missing membership" \
    "manifest must declare one membership stage" \
    '.stages |= map(select(.kind != "membership"))'
expect_manifest_error \
    "negative missing structure" \
    "manifest must declare one structure stage" \
    '.stages |= map(select(.kind != "structure"))'
expect_manifest_error \
    "negative missing classifier" \
    "manifest must declare a classifier stage" \
    '.stages |= map(if .kind == "classifier" then .kind = "unknown" else . end)'
expect_manifest_error \
    "negative descriptor type" \
    "invalid stage descriptor" \
    '.stages += [null]'
expect_manifest_error \
    "negative content ID" \
    "invalid stage content_id" \
    '(.stages[] | select(.kind == "membership") | .content_id) = "bad"'
expect_manifest_error \
    "negative uncompressed bytes" \
    "invalid stage uncompressed_bytes" \
    '(.stages[] | select(.kind == "membership") | .uncompressed_bytes) = 0'
expect_manifest_error \
    "negative row count" \
    "stage row_count does not match transaction_count" \
    '(.stages[] | select(.kind == "membership") | .row_count) += 1'
expect_manifest_error \
    "negative dependency container" \
    "invalid stage dependency_ids" \
    '(.stages[] | select(.kind == "membership") | .dependency_ids) = {}'
expect_manifest_error \
    "negative dependency content ID" \
    "invalid stage dependency_ids" \
    '(.stages[] | select(.kind == "membership") | .dependency_ids) = ["bad"]'
expect_manifest_error \
    "negative classifier ID" \
    "invalid classifier stage classifier_id" \
    '(.stages[] | select(.kind == "classifier") | .classifier_id) = "Bad-ID"'
expect_manifest_error \
    "negative non-classifier classifier ID" \
    "non-classifier stage has classifier_id" \
    '(.stages[] | select(.kind == "population") | .classifier_id) = "not_allowed"'
expect_manifest_error \
    "negative stage kind" \
    "invalid stage kind" \
    '(.stages[] | select(.classifier_id == "transaction_properties") | .kind) = "unknown"'

printf 'v2 smoke stage parser checks passed\n'
