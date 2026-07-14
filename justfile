set shell := ["bash", "-uc"]
set dotenv-load := true

export ATLAS_DATABASE := env_var_or_default("ATLAS_DATABASE", "var/atlas.db")
export ATLAS_BIND := env_var_or_default("ATLAS_BIND", "127.0.0.1:3101")

default:
    @just --list

build:
    just build-rust
    just build-web

build-rust:
    cargo build --workspace

build-web:
    npm --prefix web run build

test:
    just test-rust
    just test-web

test-rust:
    cargo test --workspace

test-web:
    npm --prefix web test

proto-check:
    ./scripts/check-peer-observer-protos.sh

proto-check-upstream:
    ./scripts/check-peer-observer-protos.sh --check-upstream

lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    npm --prefix web run check

format:
    cargo fmt --all
    npm --prefix web run format

dev:
    cargo run -p atlas-server -- serve --database "$ATLAS_DATABASE" --bind "$ATLAS_BIND"

web-dev:
    npm --prefix web run dev

db-migrate-dev:
    ./scripts/migrate-safe.sh migrate "$ATLAS_DATABASE"

db-migrate-deploy:
    ./scripts/migrate-safe.sh migrate "$ATLAS_DATABASE"

db-backup:
    ./scripts/migrate-safe.sh backup-only "$ATLAS_DATABASE"

clean:
    cargo clean
    npm --prefix web run clean
