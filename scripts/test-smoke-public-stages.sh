#!/usr/bin/env bash
set -euo pipefail

script_path=${BASH_SOURCE[0]}
script_dir=${script_path%/*}
if [[ "$script_dir" == "$script_path" ]]; then
    script_dir=.
fi
repo_root=$(cd "$script_dir/.." && pwd)

# shellcheck source=lib/v2-manifest-stages.sh
source "$script_dir/lib/v2-manifest-stages.sh"

fixture="$repo_root/tests/fixtures/publication-digest-v2.json"
manifest_url=https://atlas.example.test/api/v2/sources/perf-node-01/mempool
records=$(mktemp)
cleanup() {
    rm -- "$records"
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
require_equal \
    "uncompressed stage sizes" \
    "$uncompressed_sizes" \
    "269 906 706 580 638 530 605"

printf 'v2 smoke stage parser checks passed\n'
