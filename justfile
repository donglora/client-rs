set shell := ["bash", "-c"]

default: check

# Run clippy and tests
check:
    cargo clippy -- -D warnings && cargo test

# Build release
build:
    cargo build --release
