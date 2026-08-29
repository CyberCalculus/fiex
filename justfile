# fiex justfile
# `just` is a command runner — `cargo install just` if you don't have it.
# All recipes are CI-safe; nothing here does `cargo install` or local builds
# of the workspace itself, since this repo is built in CI.

set shell := ["zsh", "-cu"]

# Default recipe: show available targets.
default:
    @just --list

# Run formatting check.
fmt:
    cargo fmt --all -- --check

# Apply formatting.
fmt-fix:
    cargo fmt --all

# Run clippy on the whole workspace, deny warnings.
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Build everything in release mode.
build:
    cargo build --workspace --release

# Build everything in debug mode.
build-debug:
    cargo build --workspace

# Run all unit tests, headless.
test:
    cargo test --workspace --all-features

# Run all checks CI would run.
ci: fmt clippy test build
    @echo "all CI checks passed"

# Clean build artifacts (safe; just removes target/).
clean:
    cargo clean
