CURRENT := $(shell ./scripts/version.sh 2>/dev/null)

.DEFAULT_GOAL := help

.PHONY: help run build release-build demo bench fmt fmt-check lint test test-it test-it-down cover check audit version bump release publish clean

help: ## Show available commands
	@echo "dfox — v$(CURRENT)"
	@echo ""
	@grep -E '^[a-zA-Z0-9_-]+:.*##' $(MAKEFILE_LIST) | sort | awk 'BEGIN {FS = ":.*## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

run: ## Run the TUI
	cargo run --bin dfox-tui

build: ## Build the workspace
	cargo build --workspace

release-build: ## Optimised build of the TUI binary (target/release/dfox-tui)
	cargo build --release --locked --bin dfox-tui

fmt: ## Format the workspace
	cargo fmt --all

fmt-check: ## Fail if anything is unformatted
	cargo fmt --all -- --check

lint: ## Run clippy with warnings as errors
	cargo clippy --workspace --all-targets --locked -- -D warnings

test: ## Run the test suite (Postgres/MySQL integration tests skip themselves)
	cargo test --workspace --locked

test-it: ## Run the test suite against real Postgres and MySQL (needs Docker)
	docker compose -f docker-compose.test.yml up -d --wait
	DFOX_IT_REQUIRED=1 \
	DFOX_TEST_POSTGRES_URL=postgres://postgres:dfox@127.0.0.1:55432/postgres \
	DFOX_TEST_MYSQL_URL=mysql://root:dfox@127.0.0.1:53306/dfox \
	cargo test --workspace --locked

test-it-down: ## Stop the integration-test databases
	docker compose -f docker-compose.test.yml down

bench: ## Benchmarks; Postgres and MySQL ones run against `make test-it`'s databases
	DFOX_TEST_POSTGRES_URL=postgres://postgres:dfox@127.0.0.1:55432/postgres \
	DFOX_TEST_MYSQL_URL=mysql://root:dfox@127.0.0.1:53306/dfox \
	cargo bench -p dfox-bench --benches

cover: ## Line coverage; HTML report in target/llvm-cov/html (needs cargo-llvm-cov)
	@command -v cargo-llvm-cov >/dev/null || { echo "install it with: cargo install cargo-llvm-cov --locked"; exit 1; }
	cargo llvm-cov --workspace --locked --html
	@cargo llvm-cov report --summary-only | tail -n 1
	@echo "html: target/llvm-cov/html/index.html"

check: ## Everything CI requires: fmt-check, lint, test
	@./scripts/check.sh

audit: ## Check dependencies for known vulnerabilities (needs cargo-audit)
	@command -v cargo-audit >/dev/null || { echo "install it with: cargo install cargo-audit --locked"; exit 1; }
	cargo audit

version: ## Print the current workspace version
	@echo $(CURRENT)

bump: ## Bump only (VERSION=X.Y.Z): Cargo.toml, Cargo.lock, CHANGELOG
	@test -n "$(VERSION)" || { echo "Usage: make bump VERSION=0.2.0"; exit 1; }
	@./scripts/bump.sh $(VERSION)

release: ## Maintainer: open the release pull request (VERSION=X.Y.Z)
	@test -n "$(VERSION)" || { echo "Usage: make release VERSION=0.2.0"; exit 1; }
	@./scripts/release.sh $(VERSION)

publish: ## Maintainer: tag the merged release on main and push the tag
	@./scripts/publish.sh

demo: release-build ## Record the README GIFs into examples/demo (needs vhs and sqlite3)
	@./examples/tapes/setup.sh
	@for tape in examples/tapes/overview.tape examples/tapes/editor.tape examples/tapes/grid.tape; do vhs "$$tape"; done

clean: ## Remove build artefacts
	cargo clean
