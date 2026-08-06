#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'usage: %s --base-sha <sha> --expected-version <X.Y.Z>\n' "$0" >&2
    exit 2
}

base_sha=""
expected_version=""
while (( $# > 0 )); do
    case "$1" in
        --base-sha)
            (( $# >= 2 )) || usage
            base_sha="$2"
            shift 2
            ;;
        --expected-version)
            (( $# >= 2 )) || usage
            expected_version="$2"
            shift 2
            ;;
        *) usage ;;
    esac
done

[[ -n "$base_sha" && -n "$expected_version" ]] || usage
[[ "$expected_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    printf 'expected release version must use numeric X.Y.Z form\n' >&2
    exit 1
}
git cat-file -e "${base_sha}^{commit}" 2>/dev/null || {
    printf 'release base is not a commit: %s\n' "$base_sha" >&2
    exit 1
}

expected_paths=$(printf '%s\n' \
    .release-please-manifest.json \
    CHANGELOG.md \
    Cargo.lock \
    Cargo.toml | LC_ALL=C sort)
changed_paths=$(git diff --name-only "$base_sha"...HEAD | LC_ALL=C sort)
if [[ "$changed_paths" != "$expected_paths" ]]; then
    printf 'release head must modify exactly:\n%s\n' "$expected_paths" >&2
    printf 'observed:\n%s\n' "$changed_paths" >&2
    exit 1
fi

work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT
for path in .release-please-manifest.json CHANGELOG.md Cargo.lock Cargo.toml; do
    git show "$base_sha:$path" > "$work_dir/base-${path//\//_}"
    cp "$path" "$work_dir/head-${path//\//_}"
done

base_manifest="$work_dir/base-.release-please-manifest.json"
head_manifest="$work_dir/head-.release-please-manifest.json"
for manifest in "$base_manifest" "$head_manifest"; do
    jq -e 'type == "object" and keys == ["."] and (."." | type == "string")' \
        "$manifest" >/dev/null || {
        printf 'release manifest must contain only a string "." version\n' >&2
        exit 1
    }
done
base_manifest_version=$(jq -r '."."' "$base_manifest")
head_manifest_version=$(jq -r '."."' "$head_manifest")
[[ "$base_manifest_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    printf 'base release manifest version is invalid\n' >&2
    exit 1
}
[[ "$head_manifest_version" == "$expected_version" ]] || {
    printf 'release manifest version does not match expected version\n' >&2
    exit 1
}
[[ "$base_manifest_version" != "$head_manifest_version" ]] || {
    printf 'release version must advance\n' >&2
    exit 1
}

cargo_version() {
    awk '
        $0 == "[package]" { in_package = 1; next }
        in_package && /^\[/ { in_package = 0 }
        in_package && /^version = "[^"]+"$/ {
            value = $0
            sub(/^version = "/, "", value)
            sub(/"$/, "", value)
            print value
            found += 1
        }
        END { if (found != 1) exit 1 }
    ' "$1"
}

normalize_cargo() {
    awk '
        $0 == "[package]" { in_package = 1; print; next }
        in_package && /^\[/ { in_package = 0 }
        in_package && /^version = "[^"]+"$/ {
            print "version = \"<RELEASE_VERSION>\""
            found += 1
            next
        }
        { print }
        END { if (found != 1) exit 1 }
    ' "$1"
}

base_cargo="$work_dir/base-Cargo.toml"
head_cargo="$work_dir/head-Cargo.toml"
base_cargo_version=$(cargo_version "$base_cargo") || {
    printf 'could not resolve the base Cargo package version\n' >&2
    exit 1
}
head_cargo_version=$(cargo_version "$head_cargo") || {
    printf 'could not resolve the release Cargo package version\n' >&2
    exit 1
}
[[ "$base_cargo_version" == "$base_manifest_version" ]] || {
    printf 'base Cargo and manifest versions disagree\n' >&2
    exit 1
}
[[ "$head_cargo_version" == "$expected_version" ]] || {
    printf 'release Cargo version does not match expected version\n' >&2
    exit 1
}
normalize_cargo "$base_cargo" > "$work_dir/base-Cargo.normalized"
normalize_cargo "$head_cargo" > "$work_dir/head-Cargo.normalized"
cmp -s "$work_dir/base-Cargo.normalized" "$work_dir/head-Cargo.normalized" || {
    printf 'Cargo.toml changes more than the package version\n' >&2
    exit 1
}

lock_version() {
    awk '
        /^\[\[package\]\]$/ { target = 0 }
        $0 == "name = \"mempool-atlas\"" { target = 1; next }
        target && /^version = "[^"]+"$/ {
            value = $0
            sub(/^version = "/, "", value)
            sub(/"$/, "", value)
            print value
            found += 1
            target = 0
        }
        END { if (found != 1) exit 1 }
    ' "$1"
}

normalize_lock() {
    awk '
        /^\[\[package\]\]$/ { target = 0 }
        $0 == "name = \"mempool-atlas\"" { target = 1; print; next }
        target && /^version = "[^"]+"$/ {
            print "version = \"<RELEASE_VERSION>\""
            found += 1
            target = 0
            next
        }
        { print }
        END { if (found != 1) exit 1 }
    ' "$1"
}

base_lock="$work_dir/base-Cargo.lock"
head_lock="$work_dir/head-Cargo.lock"
base_lock_version=$(lock_version "$base_lock") || {
    printf 'could not resolve the base Cargo.lock package version\n' >&2
    exit 1
}
head_lock_version=$(lock_version "$head_lock") || {
    printf 'could not resolve the release Cargo.lock package version\n' >&2
    exit 1
}
[[ "$base_lock_version" == "$base_manifest_version" ]] || {
    printf 'base Cargo.lock and manifest versions disagree\n' >&2
    exit 1
}
[[ "$head_lock_version" == "$expected_version" ]] || {
    printf 'release Cargo.lock version does not match expected version\n' >&2
    exit 1
}
normalize_lock "$base_lock" > "$work_dir/base-Cargo.lock.normalized"
normalize_lock "$head_lock" > "$work_dir/head-Cargo.lock.normalized"
cmp -s "$work_dir/base-Cargo.lock.normalized" "$work_dir/head-Cargo.lock.normalized" || {
    printf 'Cargo.lock changes more than the Atlas package version\n' >&2
    exit 1
}

base_changelog="$work_dir/base-CHANGELOG.md"
head_changelog="$work_dir/head-CHANGELOG.md"
base_first_line=$(grep -n -m1 '^## \[' "$base_changelog" | cut -d: -f1)
head_first_line=$(grep -n -m1 '^## \[' "$head_changelog" | cut -d: -f1)
[[ -n "$base_first_line" && -n "$head_first_line" ]] || {
    printf 'CHANGELOG.md must contain release headings\n' >&2
    exit 1
}
base_first_heading=$(sed -n "${base_first_line}p" "$base_changelog")
head_first_heading=$(sed -n "${head_first_line}p" "$head_changelog")
case "$head_first_heading" in
    "## [$expected_version]"*) ;;
    *)
        printf 'CHANGELOG.md first release does not match expected version\n' >&2
        exit 1
        ;;
esac
head_old_line=$(grep -n -m1 -F -x "$base_first_heading" "$head_changelog" | cut -d: -f1)
[[ -n "$head_old_line" && "$head_first_line" -lt "$head_old_line" ]] || {
    printf 'CHANGELOG.md must prepend one release without replacing history\n' >&2
    exit 1
}
new_heading_count=$(sed -n "${head_first_line},$((head_old_line - 1))p" "$head_changelog" | grep -c '^## \[' || true)
[[ "$new_heading_count" == "1" ]] || {
    printf 'CHANGELOG.md must prepend exactly one release section\n' >&2
    exit 1
}
head -n "$((base_first_line - 1))" "$base_changelog" > "$work_dir/base-changelog-prefix"
head -n "$((head_first_line - 1))" "$head_changelog" > "$work_dir/head-changelog-prefix"
cmp -s "$work_dir/base-changelog-prefix" "$work_dir/head-changelog-prefix" || {
    printf 'CHANGELOG.md header changed\n' >&2
    exit 1
}
tail -n "+$base_first_line" "$base_changelog" > "$work_dir/base-changelog-history"
tail -n "+$head_old_line" "$head_changelog" > "$work_dir/head-changelog-history"
cmp -s "$work_dir/base-changelog-history" "$work_dir/head-changelog-history" || {
    printf 'CHANGELOG.md rewrites existing release history\n' >&2
    exit 1
}

printf 'validated release head for %s\n' "$expected_version"
