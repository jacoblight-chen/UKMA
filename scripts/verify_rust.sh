#!/usr/bin/env bash
set -Eeuo pipefail
cd "$(dirname "$0")/.."
if ! command -v cargo >/dev/null 2>&1; then
  echo 'BLOCKED: cargo is not installed. Install Rust via https://rustup.rs/ and rerun.' >&2
  exit 127
fi
cargo --version
rustc --version
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
