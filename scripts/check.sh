#!/usr/bin/env bash
#
# Everything CI requires, in the same order: rustfmt, clippy with warnings as
# errors, the guards in guards.sh, then the test suite. Run it before pushing.
#
#   ./scripts/check.sh

set -euo pipefail

cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

step() { printf '\n\033[36m==> %s\033[0m\n' "$*"; }

step "rustfmt"
cargo fmt --all -- --check

step "clippy"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "clippy, one driver at a time"
for driver in postgres mysql sqlite; do
	cargo clippy -p dfox-tui --no-default-features --features "$driver" --locked -- -D warnings
done

step "guards"
./scripts/guards.sh

step "test"
cargo test --workspace --locked

printf '\n\033[32mok\033[0m\n'
