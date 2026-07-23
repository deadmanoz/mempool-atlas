set shell := ["bash", "-uc"]
set dotenv-load := true

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

lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    npm --prefix web run check

format:
    cargo fmt --all
    npm --prefix web run format

dev:
    cargo run -p mempool-atlas

run: dev

web-dev:
    npm --prefix web run dev

clean:
    cargo clean
    npm --prefix web run clean
