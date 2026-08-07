#!/usr/bin/env bash

# Emit each validated v2 stage as six NUL-delimited fields:
# kind, classifier ID, content ID, uncompressed bytes, row count, and route path.
# NUL delimiters preserve the empty classifier ID required by non-classifier
# stages without relying on Bash's whitespace-sensitive IFS behavior.
atlas_v2_manifest_stage_records() {
    if (( $# != 1 )); then
        printf 'usage: atlas_v2_manifest_stage_records MANIFEST_PATH\n' >&2
        return 2
    fi

    local manifest_path=$1
    jq --exit-status --join-output '
        def unsigned_integer:
            type == "number" and . >= 0 and floor == .;
        def positive_integer:
            unsigned_integer and . > 0;
        def content_id:
            type == "string" and test("^[0-9a-f]{64}$");
        def classifier_id:
            type == "string" and test("^[a-z][a-z0-9_]*$");

        .transaction_count as $transaction_count |
        if ($transaction_count | unsigned_integer | not) then
            error("invalid manifest transaction_count")
        elif (.stages | type != "array" or length < 4) then
            error("invalid manifest stages")
        elif ([.stages[] | select(.kind == "population")] | length) != 1 then
            error("manifest must declare one population stage")
        elif ([.stages[] | select(.kind == "membership")] | length) != 1 then
            error("manifest must declare one membership stage")
        elif ([.stages[] | select(.kind == "structure")] | length) != 1 then
            error("manifest must declare one structure stage")
        elif ([.stages[] | select(.kind == "classifier")] | length) < 1 then
            error("manifest must declare a classifier stage")
        else
            .stages[] |
            if (type != "object") then
                error("invalid stage descriptor")
            elif (.content_id | content_id | not) then
                error("invalid stage content_id")
            elif (.uncompressed_bytes | positive_integer | not) then
                error("invalid stage uncompressed_bytes")
            elif (.row_count != $transaction_count) then
                error("stage row_count does not match transaction_count")
            elif (.dependency_ids | type != "array") then
                error("invalid stage dependency_ids")
            elif any(.dependency_ids[]; content_id | not) then
                error("invalid stage dependency_ids")
            else
                . as $stage |
                if $stage.kind == "classifier" then
                    if ($stage.classifier_id | classifier_id | not) then
                        error("invalid classifier stage classifier_id")
                    else
                        "classifier/\($stage.classifier_id)/\($stage.content_id)"
                    end
                elif $stage.kind == "population" or
                    $stage.kind == "membership" or
                    $stage.kind == "structure"
                then
                    if ($stage | has("classifier_id")) then
                        error("non-classifier stage has classifier_id")
                    else
                        "\($stage.kind)/\($stage.content_id)"
                    end
                else
                    error("invalid stage kind")
                end as $stage_path |
                [
                    $stage.kind,
                    ($stage.classifier_id // ""),
                    $stage.content_id,
                    ($stage.uncompressed_bytes | tostring),
                    ($stage.row_count | tostring),
                    $stage_path
                ] |
                .[] | "\(.)\u0000"
            end
        end
    ' "$manifest_path"
}
