#!/usr/bin/env bash

set -euo pipefail

repo="${PEER_OBSERVER_REPO:-../peer-observer}"
pin="$(tr -d '[:space:]' < proto/peer-observer/PINNED_COMMIT)"

files=(
    archive/header.proto
    bitcoin_primitives.proto
    ebpf_extractor.proto
    ebpf_extractor/connection.proto
    ebpf_extractor/mempool.proto
    ebpf_extractor/message.proto
    ebpf_extractor/validation.proto
    event.proto
    ipc_extractor.proto
    log_extractor.proto
    p2p_extractor.proto
    rpc_extractor.proto
)

if ! git -C "$repo" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "error: peer-observer repository not found at $repo" >&2
    echo "set PEER_OBSERVER_REPO to a local checkout" >&2
    exit 2
fi

git -C "$repo" cat-file -e "$pin^{commit}"

for relative in "${files[@]}"; do
    vendored="proto/peer-observer/$relative"
    if [[ ! -f "$vendored" ]]; then
        echo "error: missing vendored protobuf: $vendored" >&2
        exit 1
    fi
    if ! cmp -s "$vendored" <(git -C "$repo" show "$pin:protobuf/$relative"); then
        echo "error: vendored protobuf differs from $pin:protobuf/$relative" >&2
        exit 1
    fi
done

vendored_count="$(find proto/peer-observer -type f -name '*.proto' | wc -l | tr -d '[:space:]')"
if [[ "$vendored_count" != "${#files[@]}" ]]; then
    echo "error: unexpected protobuf file count: $vendored_count" >&2
    exit 1
fi

if [[ "${1:-}" == "--check-upstream" ]] && ! git -C "$repo" diff --quiet "$pin" HEAD -- protobuf; then
    echo "error: upstream peer-observer protobufs changed after pinned commit $pin" >&2
    exit 1
fi

echo "peer-observer protobufs match $pin"
