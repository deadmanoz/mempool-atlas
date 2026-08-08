#!/usr/bin/env bash

atlas_smoke_header_value() {
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
                if (found) values = values ", " value
                else values = value
                found = 1
            }
        }
        END { print values }
    ' "$path"
}

atlas_smoke_first_txid() {
    local population_path=$1
    local decoded_hex
    local encoded_prefix
    local first_txid

    encoded_prefix=$(jq --exit-status --raw-output '
        .txids_base64 |
        select(type == "string" and length >= 44) |
        .[0:44]
    ' "$population_path") || return 1
    decoded_hex=$(printf '%s' "$encoded_prefix" |
        openssl base64 -d -A |
        xxd -p -c 64) || return 1
    first_txid=${decoded_hex:0:64}
    [[ "$first_txid" =~ ^[0-9a-f]{64}$ ]] || return 1
    printf '%s\n' "$first_txid"
}
