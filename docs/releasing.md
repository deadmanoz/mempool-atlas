# Releasing

Mempool Atlas follows Semantic Versioning. The `version` field in
[`Cargo.toml`](../Cargo.toml) is the product version source, and the matching
annotated `vX.Y.Z` tag identifies a release. The running service publishes that
compiled version through `/api/v1/sources`, and both browser products display
it beside the Atlas name.

Release Please owns future changelog generation, Cargo version bumps, release
pull requests, tags, and GitHub Releases. The first `v1.0.0` release is a
one-time bootstrap because the complete initial codebase is intentionally one
root commit.

## Bootstrap `v1.0.0`

Complete this only after the final root commit has been reviewed, pushed, and
is ready to become public.

1. Confirm that `HEAD` is the only root commit and that all initial release
   metadata agrees:

   ```bash
   test "$(git rev-list --max-parents=0 HEAD)" = "$(git rev-parse HEAD)"
   test "$(git show -s --format=%P HEAD)" = ""
   cargo metadata --locked --no-deps --format-version 1 >/dev/null
   just lint
   just test
   just build
   ```

2. Create and push the annotated tag on that exact commit:

   ```bash
   git tag -a v1.0.0 -m "Mempool Atlas v1.0.0" HEAD
   git push origin v1.0.0
   ```

3. Publish the GitHub Release from the existing tag:

   ```bash
   gh release create v1.0.0 \
     --verify-tag \
     --title "v1.0.0" \
     --notes "Initial release."
   ```

4. Verify that `v1.0.0` resolves to the root commit and is neither a draft nor
   a prerelease. The release workflow performs the same check before it will
   create or update any later release pull request.

The first push of the root commit is intentionally a release-automation no-op
while `v1.0.0` is absent. This prevents its `feat(atlas)` subject from being
misread as unreleased work and producing an unintended `1.1.0` pull request.

## Automated releases after `v1.0.0`

Pull requests use squash merging, so their titles become the commits Release
Please evaluates:

- `fix(scope): ...` produces a patch release.
- `feat(scope): ...` produces a minor release.
- `type(scope)!: ...` or a `BREAKING CHANGE:` footer produces a major release.
- `chore`, `docs`, `test`, and other non-releasable changes do not advance the
  product version on their own.

After a releasable commit lands on `main`:

1. Release Please opens or updates the canonical draft release pull request.
2. The workflow requires the triggering `main` commit to remain current and
   permits changes only to `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, and
   `.release-please-manifest.json`.
3. The immutable release head is checked out without persisted credentials.
   `scripts/check-release-head.sh` verifies that only version fields changed
   in Cargo metadata, the manifest version agrees, and exactly one changelog
   section was prepended without rewriting history.
4. The workflow runs `just lint`, `just test`, and `just build` before marking
   the pull request ready.
5. It rechecks the branch, base, title, and head, approves the validated pull
   request with the separate GitHub Actions identity, and enables squash
   auto-merge for that exact head.
6. After the release pull request merges, the next Release Please run creates
   the matching `vX.Y.Z` tag and GitHub Release.

Historical queued runs do not mutate the release pull request after `main` has
advanced. Validation failures leave it in draft. Do not repair, mark ready, or
merge the automation-owned `release-please--branches--main` branch manually.

## Repository settings

Create a `RELEASE_PLEASE_TOKEN` Actions secret. A classic token needs `repo`
scope. A fine-grained token needs repository Contents, Pull requests, and
Issues read/write permissions plus Administration read permission. The
workflow reads branch protection but does not change it.

The repository must also:

- allow GitHub Actions to create and approve pull requests;
- allow squash merging and auto-merge;
- protect `main` with strict required status checks; and
- require the intended CI jobs before merge.

Release Please creates the pull request with `RELEASE_PLEASE_TOKEN`. The
repository-scoped `GITHUB_TOKEN` supplies the independent automated approval.

After the bootstrap, do not hand-edit `CHANGELOG.md` or
`.release-please-manifest.json`. Fix release generation on `main`, then let the
serialized workflow regenerate the draft.
