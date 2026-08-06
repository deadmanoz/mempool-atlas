#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
checker="$repository_root/scripts/check-release-head.sh"
workflow="$repository_root/.github/workflows/release-please.yml"
config="$repository_root/release-please-config.json"
scratch=$(mktemp -d)
test_repository="$scratch/release-repository"
mkdir -p "$test_repository"
trap 'rm -rf "$scratch"' EXIT

# The release workflow checks out the generated release head and runs `just
# test`, so this script must pass on a bumped head as readily as on the current
# one. Every repository-level assertion below therefore validates version
# metadata shape and cross-file agreement, never one pinned version. Explicit
# version literals belong only in the disposable fixtures further down.

package_version() {
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

locked_version() {
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

changelog_version() {
    sed -n \
        's/^## \[\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)\].*/\1/p' \
        "$1" | head -n 1
}

# Validate release version metadata for one repository root: the manifest holds
# exactly one "." entry in X.Y.Z form, and Cargo.toml, Cargo.lock, and the first
# CHANGELOG.md release heading all agree with it.
check_version_metadata() {
    local root="$1"
    local manifest="$root/.release-please-manifest.json"
    local declared cargo locked changelog

    jq -e '
        type == "object" and
        keys == ["."] and
        (."." | type == "string") and
        (."." | test("^[0-9]+\\.[0-9]+\\.[0-9]+$"))
    ' "$manifest" >/dev/null || {
        printf 'release manifest must hold exactly one "." version in X.Y.Z form\n' >&2
        return 1
    }
    declared=$(jq -r '."."' "$manifest")

    cargo=$(package_version "$root/Cargo.toml") || {
        printf 'could not resolve the Cargo package version\n' >&2
        return 1
    }
    [[ "$cargo" == "$declared" ]] || {
        printf 'Cargo.toml version %s does not match manifest version %s\n' \
            "$cargo" "$declared" >&2
        return 1
    }

    locked=$(locked_version "$root/Cargo.lock") || {
        printf 'could not resolve the Cargo.lock package version\n' >&2
        return 1
    }
    [[ "$locked" == "$declared" ]] || {
        printf 'Cargo.lock version %s does not match manifest version %s\n' \
            "$locked" "$declared" >&2
        return 1
    }

    changelog=$(changelog_version "$root/CHANGELOG.md")
    [[ -n "$changelog" ]] || {
        printf 'CHANGELOG.md has no X.Y.Z release heading\n' >&2
        return 1
    }
    [[ "$changelog" == "$declared" ]] || {
        printf 'CHANGELOG.md release %s does not match manifest version %s\n' \
            "$changelog" "$declared" >&2
        return 1
    }
}

jq -e '
    ."draft-pull-request" == true and
    ."include-v-in-tag" == true and
    ."include-component-in-tag" == false and
    ."release-type" == "rust" and
    .plugins == ["cargo-workspace"] and
    .packages == {".": {}}
' "$config" >/dev/null
check_version_metadata "$repository_root" || {
    printf 'repository release version metadata is inconsistent\n' >&2
    exit 1
}
# `v1.0.0` here is the permanent root-release anchor the workflow verifies
# against main's single root commit. It does not track the current version and
# stays literal across every later release.
for required_workflow_text in \
    'gh release view v1.0.0' \
    'parent_count' \
    'merge_base_commit.sha' \
    'googleapis/release-please-action@45996ed1f6d02564a971a2fa1b5860e934307cf7' \
    'scripts/check-release-head.sh' \
    'required_status_checks' \
    '--match-head-commit' \
    '.release-please-manifest.json' \
    'Cargo.lock' \
    'Cargo.toml'; do
    grep -Fq -- "$required_workflow_text" "$workflow" || {
        printf 'release workflow is missing %s\n' "$required_workflow_text" >&2
        exit 1
    }
done
if grep -Eq 'git (commit|push)([[:space:]]|$)' "$workflow"; then
    printf 'release workflow must not repair generated heads with commits or pushes\n' >&2
    exit 1
fi

git -C "$test_repository" init -q
git -C "$test_repository" config user.name "Release Test"
git -C "$test_repository" config user.email "release-test@example.invalid"

cat > "$test_repository/.release-please-manifest.json" <<'EOF'
{
  ".": "1.0.0"
}
EOF
cat > "$test_repository/Cargo.toml" <<'EOF'
[package]
name = "mempool-atlas"
version = "1.0.0"
edition = "2024"
EOF
cat > "$test_repository/Cargo.lock" <<'EOF'
version = 4

[[package]]
name = "mempool-atlas"
version = "1.0.0"
EOF
cat > "$test_repository/CHANGELOG.md" <<'EOF'
# Changelog

## [1.0.0] - 2026-08-06

Initial release.
EOF
git -C "$test_repository" add .
git -C "$test_repository" commit -q -m "feat(atlas): initial release"
base_sha=$(git -C "$test_repository" rev-parse HEAD)

cat > "$test_repository/.release-please-manifest.json" <<'EOF'
{
  ".": "1.1.0"
}
EOF
cat > "$test_repository/Cargo.toml" <<'EOF'
[package]
name = "mempool-atlas"
version = "1.1.0"
edition = "2024"
EOF
cat > "$test_repository/Cargo.lock" <<'EOF'
version = 4

[[package]]
name = "mempool-atlas"
version = "1.1.0"
EOF
cat > "$test_repository/CHANGELOG.md" <<'EOF'
# Changelog

## [1.1.0](https://github.com/deadmanoz/mempool-atlas/compare/v1.0.0...v1.1.0) (2026-08-07)

### Features

* Add a feature.

## [1.0.0] - 2026-08-06

Initial release.
EOF
git -C "$test_repository" add .
git -C "$test_repository" commit -q -m "chore(main): release 1.1.0"

(
    cd "$test_repository"
    "$checker" --base-sha "$base_sha" --expected-version 1.1.0
)

expect_rejected() {
    local scenario="$1"
    local version="${2:-1.1.0}"
    if (
        cd "$test_repository"
        "$checker" --base-sha "$base_sha" --expected-version "$version" >/dev/null 2>&1
    ); then
        printf 'release validator accepted %s\n' "$scenario" >&2
        exit 1
    fi
}

printf '\ndescription = "unexpected release-branch edit"\n' >> "$test_repository/Cargo.toml"
expect_rejected "a non-version Cargo.toml change"
git -C "$test_repository" show HEAD:Cargo.toml > "$test_repository/Cargo.toml"

printf '\n[[package]]\nname = "injected"\nversion = "9.9.9"\n' >> "$test_repository/Cargo.lock"
expect_rejected "a non-version Cargo.lock change"
git -C "$test_repository" show HEAD:Cargo.lock > "$test_repository/Cargo.lock"

printf '\nRewritten history.\n' >> "$test_repository/CHANGELOG.md"
expect_rejected "rewritten changelog history"
git -C "$test_repository" show HEAD:CHANGELOG.md > "$test_repository/CHANGELOG.md"

cat > "$test_repository/.release-please-manifest.json" <<'EOF'
{
  ".": "1.1.0",
  "injected": "9.9.9"
}
EOF
expect_rejected "an expanded release manifest"
git -C "$test_repository" show HEAD:.release-please-manifest.json > \
    "$test_repository/.release-please-manifest.json"

expect_rejected "a mismatched expected version" 1.2.0

# Cover the repository-level version metadata check itself. The bumped fixture
# stands in for the generated release head the workflow checks out and runs
# `just test` against; it must pass without this script knowing any version.
write_version_fixture() {
    local root="$1"
    local manifest_version="$2"
    local cargo_toml_version="$3"
    local cargo_lock_version="$4"
    local changelog_heading_version="$5"

    mkdir -p "$root"
    cat > "$root/.release-please-manifest.json" <<EOF
{
  ".": "$manifest_version"
}
EOF
    cat > "$root/Cargo.toml" <<EOF
[package]
name = "mempool-atlas"
version = "$cargo_toml_version"
edition = "2024"
EOF
    cat > "$root/Cargo.lock" <<EOF
version = 4

[[package]]
name = "mempool-atlas"
version = "$cargo_lock_version"
EOF
    cat > "$root/CHANGELOG.md" <<EOF
# Changelog

## [$changelog_heading_version] - 2026-08-07

A release.

## [1.0.0] - 2026-08-06

Initial release.
EOF
}

expect_version_metadata_accepted() {
    local scenario="$1"
    local root="$2"
    if ! check_version_metadata "$root" >/dev/null 2>&1; then
        printf 'version metadata check rejected %s\n' "$scenario" >&2
        exit 1
    fi
}

expect_version_metadata_rejected() {
    local scenario="$1"
    local root="$2"
    if check_version_metadata "$root" >/dev/null 2>&1; then
        printf 'version metadata check accepted %s\n' "$scenario" >&2
        exit 1
    fi
}

bumped_head="$scratch/bumped-head"
write_version_fixture "$bumped_head" 1.1.0 1.1.0 1.1.0 1.1.0
expect_version_metadata_accepted "a consistently bumped release head" "$bumped_head"

major_head="$scratch/major-head"
write_version_fixture "$major_head" 2.0.0 2.0.0 2.0.0 2.0.0
expect_version_metadata_accepted "a consistently bumped major release head" "$major_head"

mismatched_cargo="$scratch/mismatched-cargo"
write_version_fixture "$mismatched_cargo" 1.1.0 1.0.0 1.1.0 1.1.0
expect_version_metadata_rejected "a stale Cargo.toml version" "$mismatched_cargo"

mismatched_lock="$scratch/mismatched-lock"
write_version_fixture "$mismatched_lock" 1.1.0 1.1.0 1.0.0 1.1.0
expect_version_metadata_rejected "a stale Cargo.lock version" "$mismatched_lock"

mismatched_changelog="$scratch/mismatched-changelog"
write_version_fixture "$mismatched_changelog" 1.1.0 1.1.0 1.1.0 1.0.0
expect_version_metadata_rejected "a missing changelog release" "$mismatched_changelog"

malformed_manifest="$scratch/malformed-manifest"
write_version_fixture "$malformed_manifest" 1.1.0 1.1.0 1.1.0 1.1.0
cat > "$malformed_manifest/.release-please-manifest.json" <<'EOF'
{
  ".": "1.1",
  "injected": "9.9.9"
}
EOF
expect_version_metadata_rejected "a malformed release manifest" "$malformed_manifest"

printf 'release-head validation tests passed\n'
