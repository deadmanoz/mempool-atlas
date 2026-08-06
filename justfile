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
    just test-rust
    just test-web

test-release:
    ./scripts/test-release-head.sh

test-rust:
    cargo test

test-web:
    npm --prefix web test

# Browser-level viewport coverage. Playwright starts the fixture Atlas API and
# Vite itself; run `just test-web-e2e-install` once to fetch the browser.
test-web-e2e:
    npm --prefix web run test:e2e

test-web-e2e-install:
    npm --prefix web exec -- playwright install --with-deps chromium

lint:
    just structure
    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings
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

# Run the deterministic fixture Atlas API for frontend-only development.
web-fixtures:
    node web/dev/fixture-server.mjs

smoke-public base_url source_id:
    ./scripts/smoke-public.sh "{{base_url}}" "{{source_id}}"

clean:
    cargo clean
    npm --prefix web run clean
