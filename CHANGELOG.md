# DFox — Changelog

All notable changes are documented here. Format follows [Keep a Changelog](https://keepachangelog.com/),
and the project uses [Semantic Versioning](https://semver.org/).

Versions below 1.0.0 may change behaviour, keybindings and the `dfox-core` API between releases.

## [Unreleased]

### Added

- A connection manager: saved connections in `connections.toml`, passwords in
  the OS keychain, never in the file. A background check shows whether each
  connection is reachable, with a latency sparkline. Connections can be
  read-only, require or turn off TLS, and carry a coloured tag
- `DATABASE_URL` and `~/.pgpass` entries show up as connections to use or save,
  and `.pgpass` supplies passwords for saved Postgres connections
- `dfox <url>` connects right away; `dfox --help` and `--version`
- SQLite works in the TUI: a connection is a file path
- An explorer of databases, schemas, tables, views, materialized views,
  functions and procedures, loaded as they are opened, with row-count
  estimates and a table's columns under it. All schemas are shown, not only
  `public`. Opening another database switches the session to it
- `/` fuzzy-finds a table, view or routine in the explorer
- `dfox-core`: `list_schemas`, `list_objects` and a schema-aware
  `describe_table`; `ConnectionParams` gains `tls` and `read_only`;
  `DbManager::ping`; engine commands `ListSchemas`, `ListObjects`, `Ping`

- Themes: dfox, tokyo-night, catppuccin-mocha, catppuccin-latte, gruvbox, nord
  and high-contrast, plus your own in `themes/<name>.toml`. `Ctrl+T` switches
  theme. Colours fall back to 256 or 16 colours on terminals without true
  colour
- `config.toml` for the theme, keymap (`standard` or `vim`), animations, icons
  (`auto`, `nerd`, `ascii`), the row limit and key overrides. Mistakes in it
  show as a warning instead of stopping dfox
- A help screen (`?` or `F2`) listing every key, generated from the keymap
- A command menu on `Space`
- Toasts for copies, cancelled queries, theme changes and errors, a status bar
  that shows the connection and the running query, and a progress bar along
  the results while a query runs
- Quitting while connected asks for confirmation
- Snapshot tests of every screen at 120×40 and 80×24

### Changed

- The database type, connection and database list screens are replaced by
  the connection manager and the explorer. `F1` goes back to the connections
- `dfox-core`: `describe_table` takes a schema; `DbType` serializes in lower
  case; the engine's `ListTables` and `Describe` events are replaced by
  `ListObjects` and `Described`
- The screens are rebuilt as components with rounded panels, and result values
  are coloured by kind, with numbers right-aligned
- The SQL editor inserts and deletes at the cursor, and joins lines on
  Backspace
- SQLite on the first screen shows a notice instead of a separate screen

### Added

- Queries run in the background: the screen stays responsive, shows a spinner
  and the elapsed time while a query runs, and rows appear as they arrive
- `Ctrl+C` cancels the running query (Postgres and MySQL cancel it on the
  server); the connection stays usable
- Spinners while connecting and while loading databases and tables
- `dfox-core`: an `engine` module that runs database work as background tasks
  and reports back with events; the `ResultSet` trait, `Cell` values and the
  appendable `RowStore` that hold results

### Changed

- Results keep up to 100,000 rows instead of 1,000, and the status line shows
  how long the query took
- The screen redraws only when something changed, and uses no CPU while idle
- `dfox-core`: `DbClient::stream` sends rows in chunks as they arrive, and
  `DbClient::cancel` stops the running statement; `run` and `execute` are now
  built on `stream`. New `DbError::Cancelled`. `DbType` is `Copy` and
  `PartialEq`

### Fixed

- Keys no longer register twice on Windows
- Expanding a table in the list shows its column types, nullability and
  defaults again
- Tabs and line breaks inside a value no longer break the result grid's layout

## [0.1.2] - 2026-10-12

### Fixed

- Values are no longer shown as `NULL` when dfox can't map their type: every
  cell now shows the server's own text form (Postgres `int2`, `numeric`,
  `timestamptz`, `money`, `inet`, arrays; MySQL `TINYINT`, `SMALLINT`,
  unsigned integers, `DECIMAL`, `BIT`)
- MySQL results showed each row as a single JSON string; they now fill the
  result grid like Postgres results
- Columns keep the order of the query, and repeated names (`id` from two
  joined tables) no longer overwrite each other
- Statements other than `SELECT` that return rows (`WITH`, `VALUES`, `SHOW`,
  `EXPLAIN`, `... RETURNING`, `PRAGMA`) now show their results, and a script
  with several statements shows the last result set
- `BEGIN`/`COMMIT`, `SET` and temporary tables now work across runs: queries
  use one pinned connection instead of any connection from a pool
- Passwords containing `@`, `:`, `/`, `#` or `%` can connect
- Describing a table no longer splices its name into SQL
- Connection errors are shown on the connection screen instead of being ignored
- Errors while loading databases or tables no longer write over the screen
- Values containing tabs no longer split into extra columns, and empty strings
  are no longer shown as `NULL`
- Large result sets keep only the first 1,000 rows in memory

### Changed

- `dfox-core`: `DbClient::run` replaces `query` and `query_with_column_order`
  and returns `QueryOutput`. The unused `Transaction` API, `DbError::Transaction`
  and the per-driver `ColumnType` mappings are removed. Clients can connect
  from `ConnectionParams` and have a `close` method.

### Added

- Integration tests against real Postgres 16, MySQL 8.4 and SQLite, run with
  `make test-it` locally and in a new CI job

## [0.1.1] - 2026-10-12

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
- Terminal UI upgraded to ratatui 0.30 and crossterm 0.29, which also clears the
  `lru` soundness advisories
- The workspace shares one version, edition and licence through
  `[workspace.package]`, and uses Cargo resolver 2
- `mockall` moved to `dfox-core`'s dev-dependencies, so it is no longer built
  into the library or the binary

### Removed

- Unused dependencies: `log`, `env_logger` and `tempfile` from `dfox-core`, and
  `serde_json` from `dfox-tui`
