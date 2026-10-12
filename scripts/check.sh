#!/usr/bin/env bash
#
# Everything CI requires, in the same order: rustfmt, clippy with warnings as
# errors, then the test suite. Run it before pushing.
#
#   ./scripts/check.sh

set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

step() { printf '\n\033[36m==> %s\033[0m\n' "$*"; }

step "rustfmt"
cargo fmt --all -- --check

step "clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "test"
cargo test --workspace --locked

printf '\n\033[32mok\033[0m\n'
