# DFox — Changelog

All notable changes are documented here. Format follows [Keep a Changelog](https://keepachangelog.com/),
and the project uses [Semantic Versioning](https://semver.org/).

Versions below 1.0.0 may change behaviour, keybindings and the `dfox-core` API between releases.

## [Unreleased]

### Added

- Terminal UI for PostgreSQL and MySQL: database type selection, connection form,
  database and table browsing, table descriptions and SQL query execution
- Scrollable query results with horizontal scrolling and paging
- Copy the selected row (`Ctrl+C`) or all results (`Ctrl+A`) to the clipboard
- Debug logging to `dfox-debug.log`, controlled by `RUST_LOG`, and an in-app
  debug panel on `F12`
- `Makefile` and `scripts/` with a release flow: `make release VERSION=X.Y.Z`
  opens the release pull request, `make publish` tags it once merged
- CI on Linux, macOS and Windows (rustfmt, clippy, tests), `cargo audit`,
  coverage on Codecov, CodeQL, OpenSSF Scorecard, Dependabot, and a release
  workflow that attaches prebuilt binaries
- `make cover` for a local HTML coverage report
- `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, issue and pull request
  templates

### Changed

- UI code split into `ui/components/`, with a `TerminalGuard` that restores the
  terminal on exit
- `dotenv` replaced with its maintained fork `dotenvy`, and dependencies updated
  to clear RustSec advisories
- The workspace shares one version, edition and licence through
  `[workspace.package]`, and uses Cargo resolver 2
- `mockall` moved to `dfox-core`'s dev-dependencies, so it is no longer built
  into the library or the binary

### Removed

- Unused dependencies: `log`, `env_logger` and `tempfile` from `dfox-core`, and
  `serde_json` from `dfox-tui`
