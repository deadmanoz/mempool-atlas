#!/usr/bin/env bash
set -euo pipefail

failed=0

while IFS= read -r path; do
    maximum_lines=2000
    case "$path" in
        src/classification/tests.rs) maximum_lines=2800 ;;
        src/classification.rs) maximum_lines=2650 ;;
        web/src/main.ts) maximum_lines=2700 ;;
        web/src/comparison-main.ts) maximum_lines=1950 ;;
        web/src/comparison-distributions-view.ts) maximum_lines=450 ;;
        web/src/snapshot-distributions-view.ts) maximum_lines=375 ;;
    esac
    lines=$(wc -l < "$path")
    if (( lines > maximum_lines )); then
        printf '%s has %s lines (maximum %s)\n' "$path" "$lines" "$maximum_lines" >&2
        failed=1
    fi
done < <(find src web/src -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.css' \) -print | sort)

exit "$failed"
