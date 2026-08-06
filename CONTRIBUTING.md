# Contributing

Thanks for helping improve Mempool Atlas. Open an issue before undertaking a
large behavioural or architectural change so the scope and policy semantics can
be agreed first.

## Development setup

Install Rust 1.88 or newer, Node.js 20.19.x or 22.12 or newer, npm 10 or newer,
and [`just`](https://github.com/casey/just). Then install the locked frontend
dependencies and run the quality gate:

```sh
npm --prefix web ci
just lint
just test
just build
```

Rust code must remain compatible with the declared 1.88 MSRV. Check it with:

```sh
cargo +1.88.0 check --locked
```

### Browser tests

`just test` covers Rust and the frontend unit suites. Layout and viewport
behaviour is covered separately by Playwright, which drives real pages against
the deterministic fixture Atlas API rather than a live node. Fetch the browser
once, then run the suite:

```sh
just test-web-e2e-install
just test-web-e2e
```

Playwright starts the fixture API and its own Vite server on port 5174, so it
does not disturb a `just web-dev` session on the usual port.

## Changes

Keep changes focused, add tests for behavioural changes, and update the relevant
public and architecture documentation. Keep the changelog release-focused.
Never commit node addresses, credentials, private infrastructure details, or
generated build output. Use conventional commit messages such as
`fix(runtime): preserve the last good snapshot`.

Work on a branch, open a pull request, and let CI finish. The `quality`, `e2e`,
and `msrv` checks must pass, and the branch should be current with `main` before
it merges. Pull requests use squash merging, so the pull request title becomes
the commit subject. Write it in the same conventional format. After `v1.0.0`,
`fix` advances the patch version, `feat` advances the minor version, and a `!`
or `BREAKING CHANGE:` footer advances the major version. Release Please owns
the generated changelog and version pull requests; see
[Releasing](docs/releasing.md).

By contributing, you agree that your contribution is licensed under the MIT
License.
