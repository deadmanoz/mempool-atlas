set shell := ["bash", "-uc"]
set dotenv-load := true

export ATLAS_DATABASE := env_var_or_default("ATLAS_DATABASE", "var/atlas.db")
export ATLAS_AGENT_DATABASE := env_var_or_default("ATLAS_AGENT_DATABASE", "var/atlas-agent.db")
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

test-baseline-scale:
    cargo test --release -p atlas-server --test baseline_scale -- --ignored --nocapture

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

agent-dev:
    cargo run -p atlas-agent -- run --database "$ATLAS_AGENT_DATABASE"

web-dev:
    npm --prefix web run dev

db-migrate-dev:
    ./scripts/migrate-safe.sh migrate "$ATLAS_DATABASE"

db-migrate-deploy:
    ./scripts/migrate-safe.sh migrate "$ATLAS_DATABASE"

db-backup:
    ./scripts/migrate-safe.sh backup-only "$ATLAS_DATABASE"

agent-db-migrate-dev:
    ./scripts/migrate-safe.sh migrate "$ATLAS_AGENT_DATABASE" agent

agent-db-migrate-deploy:
    ./scripts/migrate-safe.sh migrate "$ATLAS_AGENT_DATABASE" agent

agent-db-backup:
    ./scripts/migrate-safe.sh backup-only "$ATLAS_AGENT_DATABASE" agent

clean:
    cargo clean
    npm --prefix web run clean
