set shell := ["nu", "-c"]

default: fmt clippy test check build

fmt:
    cargo fmt --all

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

check:
    cargo check --workspace --all-targets

build:
    cargo build --workspace
