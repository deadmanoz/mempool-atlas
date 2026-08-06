#!/usr/bin/env bash
set -euo pipefail

failed=0

while IFS= read -r path; do
    maximum_lines=2000
    case "$path" in
        src/classification/tests.rs) maximum_lines=2800 ;;
        src/classification.rs) maximum_lines=2650 ;;
        web/src/styles.css) maximum_lines=2100 ;;
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

# Repository hygiene. README.md and docs/configuration.md tell operators to
# write local RPC addresses, usernames, and credential filenames into these
# paths, so the ignore rules that back those instructions are asserted here.
# A regression fails `just lint` instead of surfacing as a disclosure.
# `--no-index` evaluates the ignore rules themselves; tracking is a separate
# failure, checked below, because Git never ignores an already-tracked path.
if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    for ignored_path in \
        config/sources.json \
        var/credentials/node-a.password \
        .env; do
        if ! git check-ignore --no-index -q -- "$ignored_path"; then
            printf '%s is documented as local configuration but is not ignored by Git\n' \
                "$ignored_path" >&2
            failed=1
        fi
    done

    for unignored_path in \
        config/sources.example.json \
        .env.example; do
        if git check-ignore --no-index -q -- "$unignored_path"; then
            printf '%s is a shipped example and must not be ignored by Git\n' \
                "$unignored_path" >&2
            failed=1
        fi
    done

    # Ignore rules do not protect a path that is already tracked.
    while IFS= read -r tracked_path; do
        [[ -n "$tracked_path" ]] || continue
        printf '%s is tracked and would disclose local configuration\n' \
            "$tracked_path" >&2
        failed=1
    done < <(git ls-files -- 'config/sources.json' '*.password')
else
    printf 'not a Git work tree; skipped repository hygiene checks\n' >&2
fi

exit "$failed"
