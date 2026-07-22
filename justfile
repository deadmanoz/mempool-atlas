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
    cargo test --workspace --all-features

test-web:
    npm --prefix web test

test-baseline-scale:
    cargo test --release -p atlas-server --test baseline_scale -- --ignored --nocapture

regen-api-fixtures:
    cargo test -p atlas-server --test api_fixture_contract regenerate_api_fixtures -- --ignored

seed-dev source="demo-node" count="20000":
    cargo run -p atlas-server --features seed-tool --bin atlas-seed -- --server "http://${ATLAS_BIND}" single --source {{source}} --count {{count}}

seed-forks count="5000":
    cargo run -p atlas-server --features seed-tool --bin atlas-seed -- --server "http://${ATLAS_BIND}" forks --count {{count}}

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

db-reinitialize-dev:
    ./scripts/migrate-safe.sh reinitialize "$ATLAS_DATABASE"

db-reinitialize-deploy:
    ./scripts/migrate-safe.sh reinitialize "$ATLAS_DATABASE"

db-backup:
    ./scripts/migrate-safe.sh backup-only "$ATLAS_DATABASE"

agent-db-migrate-dev:
    ./scripts/migrate-safe.sh migrate "$ATLAS_AGENT_DATABASE" agent

agent-db-migrate-deploy:
    ./scripts/migrate-safe.sh migrate "$ATLAS_AGENT_DATABASE" agent

agent-db-reinitialize-dev:
    ./scripts/migrate-safe.sh reinitialize "$ATLAS_AGENT_DATABASE" agent

agent-db-reinitialize-deploy:
    ./scripts/migrate-safe.sh reinitialize "$ATLAS_AGENT_DATABASE" agent

agent-db-backup:
    ./scripts/migrate-safe.sh backup-only "$ATLAS_AGENT_DATABASE" agent

clean:
    cargo clean
    npm --prefix web run clean
