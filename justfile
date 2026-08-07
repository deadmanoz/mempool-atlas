set shell := ["bash", "-uc"]
set dotenv-load := true

export ATLAS_BIND := env_var_or_default("ATLAS_BIND", "127.0.0.1:3101")

default:
    @just --list

build:
    just build-rust
    just build-web

build-rust:
    cargo build

build-web:
    npm --prefix web run build

test:
    just test-release
    just test-smoke-public-stages
    just test-smoke-public-fixture
    just test-rust
    just test-web

test-release:
    ./scripts/test-release-head.sh

test-smoke-public-stages:
    ./scripts/test-smoke-public-stages.sh

test-smoke-public-fixture: functional-fixtures
    ./scripts/test-smoke-public-fixture.sh

test-rust:
    cargo test --features perf-fixtures

test-web:
    npm --prefix web test

# Browser-level viewport coverage. Playwright starts the fixture Atlas API and
# Vite itself; run `just test-web-e2e-install` once to fetch the browser.
test-web-e2e:
    just functional-fixtures
    npm --prefix web run test:e2e

test-web-e2e-install:
    npm --prefix web exec -- playwright install --with-deps chromium

lint:
    just structure
    cargo fmt --all -- --check
    cargo clippy --all-targets --features perf-fixtures -- -D warnings
    npm --prefix web run check

structure:
    ./scripts/check-structure.sh

format:
    cargo fmt --all
    npm --prefix web run format

dev:
    cargo run

run: dev

web-dev:
    npm --prefix web run dev

# Export the small canonical fixture set used by frontend development and E2E.
functional-fixtures:
    cargo run --profile fixtures --features perf-fixtures --bin export-perf-fixture -- --profile functional

# Export the functional fixtures plus the production-scale performance set.
perf-fixtures: functional-fixtures
    cargo run --profile fixtures --features perf-fixtures --bin export-perf-fixture -- --profile performance

# Refresh the compact cross-language digest fixture from Rust-owned model data.
publication-digest-fixture:
    cargo run --profile fixtures --features perf-fixtures --bin export-perf-fixture -- --profile performance --output-root target/publication-digest-fixture --transaction-count 3 --source-count 1
    mkdir -p tests/fixtures
    cp target/publication-digest-fixture/performance/snapshots/perf-node-01/manifest.json tests/fixtures/publication-digest-v2.json

# Record exact identity/gzip projections and enforce the v2 staged byte gates.
stage-projection: perf-fixtures
    node web/perf/project-stages.mjs

# Benchmark the production web build against the production-scale fixtures.
perf-web: stage-projection
    npm --prefix web run build
    rm -rf web/.perf-results/raw
    npm --prefix web run test:perf
    node web/perf/merge-results.mjs

# Run the generated fixture Atlas API for frontend-only development.
web-fixtures: functional-fixtures
    node web/dev/fixture-server.mjs

smoke-public base_url source_id:
    ./scripts/smoke-public.sh "{{base_url}}" "{{source_id}}"

clean:
    cargo clean
    npm --prefix web run clean
    rm -rf web/.perf-fixtures
    rm -rf web/.perf-results
