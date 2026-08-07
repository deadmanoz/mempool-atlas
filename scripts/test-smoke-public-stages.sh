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
    [[ "$content_id" =~ ^[0-9a-f]{64}$ ]] || {
        printf 'invalid content ID in stage %s\n' "$stage_count" >&2
        exit 1
    }
    [[ "$uncompressed_bytes" =~ ^[1-9][0-9]*$ ]] || {
        printf 'invalid uncompressed byte count in stage %s\n' "$stage_count" >&2
        exit 1
    }
    [[ "$row_count" == 3 ]] || {
        printf 'unexpected row count in stage %s: %s\n' \
            "$stage_count" "$row_count" >&2
        exit 1
    }
    uncompressed_sizes="${uncompressed_sizes:+$uncompressed_sizes }$uncompressed_bytes"
    stage_url="$manifest_url/stages/$stage_path"

    case "$kind" in
        population)
            population_count=$((population_count + 1))
            # Regression coverage: the empty classifier field must not shift
            # the following content ID and byte-count fields left.
            [[ -z "$classifier_id" ]]
            [[ "$stage_path" == "population/$content_id" ]]
            [[ "$stage_url" == "$manifest_url/stages/population/$content_id" ]]
            ;;
        membership)
            membership_count=$((membership_count + 1))
            [[ -z "$classifier_id" ]]
            [[ "$stage_path" == "membership/$content_id" ]]
            [[ "$stage_url" == "$manifest_url/stages/membership/$content_id" ]]
            ;;
        structure)
            structure_count=$((structure_count + 1))
            [[ -z "$classifier_id" ]]
            [[ "$stage_path" == "structure/$content_id" ]]
            [[ "$stage_url" == "$manifest_url/stages/structure/$content_id" ]]
            ;;
        classifier)
            classifier_count=$((classifier_count + 1))
            [[ "$classifier_id" =~ ^[a-z][a-z0-9_]*$ ]]
            [[ "$stage_path" == "classifier/$classifier_id/$content_id" ]]
            [[ "$stage_url" == \
                "$manifest_url/stages/classifier/$classifier_id/$content_id" ]]
            classifier_ids="${classifier_ids:+$classifier_ids }$classifier_id"
            ;;
        *)
            printf 'unexpected stage kind in fixture: %s\n' "$kind" >&2
            exit 1
            ;;
    esac

done <"$records"

[[ "$stage_count" == 7 ]]
[[ "$population_count" == 1 ]]
[[ "$membership_count" == 1 ]]
[[ "$structure_count" == 1 ]]
[[ "$classifier_count" == 4 ]]
[[ "$classifier_ids" == \
    "transaction_properties transaction_shape data_protocols knots_bip110" ]]
[[ "$uncompressed_sizes" == "269 906 706 580 638 530 605" ]]

printf 'v2 smoke stage parser checks passed\n'
