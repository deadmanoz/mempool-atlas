# Contributing

Thanks for helping improve Mempool Atlas. Open an issue before undertaking a
large behavioural or architectural change so the scope and policy semantics can
be agreed first.

## Development setup

Install Rust 1.88 or newer, Node.js 20.19 or newer, npm 10 or newer, and
[`just`](https://github.com/casey/just). Then install the locked frontend
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

## Changes

Keep changes focused, add tests for behavioural changes, and update the
changelog and relevant architecture documentation. Never commit node addresses,
credentials, private infrastructure details, or generated build output. Use
conventional commit messages such as `fix(runtime): preserve the last good
snapshot`.

By contributing, you agree that your contribution is licensed under the MIT
License.
