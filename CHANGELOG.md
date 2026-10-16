# DFox — Changelog

All notable changes are documented here. Format follows [Keep a Changelog](https://keepachangelog.com/),
and the project uses [Semantic Versioning](https://semver.org/).

Versions below 1.0.0 may change behaviour, keybindings and the `dfox-core` API between releases.

## [Unreleased]

### Removed

- sqlx, and with it the `DbClient` interface, the cell storage (`RowStore`)
  and `async-trait`: every database runs on its native session. The release
  binary is 13.2 MB (from 14.4 MB with both, and 10.6 MB in v0.2.0 on sqlx
  alone, which had no Arrow, SSH or TLS verification)

### Changed

- SQLite 3.51.3 (was 3.46) through rusqlite 0.40, which no longer has to share
  sqlx's SQLite build
- The app runs on the native sessions: rows arrive as Arrow batches, the
  explorer walks each driver's catalog tree (so MySQL shows every database,
  with tables right under it, and SQLite shows `main` and attached
  databases), and the SQL dialect comes from the session. Sessions open
  through `dfox-ssh`, so a connection with SSH details goes through a
  tunnel; the connection form gets the fields next. Server notices and
  MySQL warnings show as toasts. The explorer keeps browsing while a query
  runs

### Added

- Connections can go through an SSH jump host: the connection form has an
  SSH section (host, port, user, and login by ssh-agent, key file or
  password, with an optional known_hosts file). The SSH password or key
  passphrase is kept in the keychain, and asked for when it isn't saved.
  The manager shows the jump host, and checks that it is reachable
- A CA file for TLS `verify`, to trust a private certificate authority

- A native SQLite session on `rusqlite`, with the connection on a thread of
  its own, decoding rows straight into Arrow. It reads a million rows 4.9×
  faster than sqlx and passes the driver test suite. SQLite types each value,
  so a column takes the type of its first value and widens as others arrive
  (integers and reals together stay exact as decimals; text and blobs make
  text) without losing a value. Scripts are split by SQLite itself, so
  trigger bodies stay whole; a cancel interrupts in microseconds; the
  catalog lists `main`, `temp` and attached databases with their tables,
  views, columns and indexes; edits apply inside a savepoint, so they work
  in an open transaction too. A missing file is an error, not a new empty
  database
- A native MySQL session on `mysql_async`, decoding rows straight into
  Arrow: integers keep their width and sign, `TINYINT(1)` is a boolean,
  `DECIMAL` stays exact, binary strings and `BIT` are bytes, and dates and
  times keep MySQL's own text (zero dates, `TIME` past a day). It reads a
  million rows 1.85× faster than sqlx, with the first rows in a third of the
  time, and passes the driver test suite. A script goes to the server whole,
  so procedures and triggers need no `DELIMITER`; procedures return every
  result set, and the last statement's warnings show as notices
- The native MySQL session cancels with `KILL QUERY` in a few milliseconds,
  browses every database on the server with filters, sorting and paging,
  applies edits by primary key in a transaction, and lists 5,000 tables in
  about 35 ms (row estimates are left out above 1,000 tables, where MySQL
  would open each one). TLS `prefer` falls back to a plain connection when
  the server has none, `verify-full` checks the system's roots and the CA
  file, and the connection works through an SSH tunnel

- `dfox-ssh`: SSH tunnels for any driver, on `russh`. It logs in with
  ssh-agent, a key file (with or without a passphrase) or a password, and
  forwards a local port to the database. The jump host's key must be in
  `known_hosts`: an unknown key is refused with its fingerprint and the
  `ssh-keyscan` command that adds it, and a changed key is always refused.
  TLS through the tunnel still verifies the database's own host name, and
  cancelling works through it. Connections gain an `ssh` section; the app
  gets fields for it in C4

- The decode benchmark reads a stored table instead of generating rows in
  the query, which made the server's work hide the client's. On it, the
  native Postgres session reads a million rows 1.8× faster than sqlx (3.2 M
  rows/s), with the first rows in half the time
- A native PostgreSQL session on `tokio-postgres` and the binary protocol,
  decoding rows straight into Arrow: integers, floats, exact `numeric`, text,
  `bytea`, `uuid`, dates, times, timestamps (with `infinity`), intervals,
  JSON, `inet`/`cidr`/`macaddr`, `money`, bit strings, points, arrays as
  lists, enums and domains. Types without a decoder keep their bytes and
  type name. It passes the driver test suite against a real server. The app
  moves over in C4
- The native session runs each statement of a script, keeps the last result,
  reports `RAISE NOTICE` messages, cancels in milliseconds, browses tables
  with filters, sorting and paging, and applies edits by primary key. A
  catalog of 5,000 tables lists in about 25 ms
- TLS `verify-full`: checks the server's certificate and host name against
  the system's roots and an optional CA file (`ca_file`), for both Postgres
  drivers and MySQL (`VerifyIdentity`). Connection errors now say why a TLS
  handshake failed

- Benchmarks (`make bench`): rows per second and time to the first rows for
  each database, memory for a million rows, and drawing the grid. Pull
  requests are compared with `main` on the same runner and fail on a clear
  slowdown of more than 10%. The numbers to beat are in
  `crates/dfox-bench/BASELINE.md`

- `dfox-core`: the session interfaces for drivers on native clients. A
  `Session` runs requests and streams results as Apache Arrow record batches,
  exposes a `Catalog` tree (children, describe, and browsing with filters,
  sorting and paging), applies `ChangeSet` edits by primary key, and has a
  `CancelHandle`. `Capabilities` tell the app what a session supports.
  Drivers describe themselves with `DriverInfo` and their connection form
  with a `FormSpec`
- `dfox-core`: `ArrowResultSet` serves Arrow batches to the result grid
  through the existing `ResultSet` interface, formatting only the cells that
  are read. `Fetcher` reads a result within a memory budget (256 MB by
  default) and fetches more on request. Ten million integers take 80 MB
- `dfox-testkit`: a conformance suite every driver's session must pass, and
  a mock driver that passes it

### Changed

- Release builds use whole-program optimisation, a single codegen unit,
  abort on panic and the mimalloc allocator: the binary went from 18.8 MB to
  10.6 MB
- The workspace moved to `crates/`, and each database is now its own crate:
  `dfox-driver-postgres`, `dfox-driver-mysql` and `dfox-driver-sqlite`. They
  are cargo features of `dfox-tui`, all on by default, so
  `--no-default-features --features sqlite` builds a SQLite-only binary
- The SQL helpers (highlighting, statement splitting, autocomplete context,
  the destructive-statement guard) moved to a new `dfox-sql` crate
- `dfox-core` no longer knows any database. A `Driver` trait connects, and
  `engine::spawn` takes the `Drivers` an app was built with. `DbManager` and
  `ConnectionConfig` are gone: `ping` is `dfox_core::driver::ping`, and each
  driver crate exports its client and its `Driver`

## [0.2.0] - 2026-10-13

### Added

- Motion, with `tachyonfx`: the focused panel's border lights up, screens and
  query tabs fade in, toasts slide in, and the results border flashes green
  when a query finishes, red when it fails and amber when it is cancelled.
  A failed connection flashes the connection list red. Every transition
  takes at most 250 ms, and any key finishes it at once
- A splash screen with the logo and a bar that fills as dfox checks the saved
  connections. It shows for 0.4 to 0.6 s, and any key skips it
- While a query runs, the status bar shows rows per second, and the cancel
  hint fades in after two seconds
- `--reduced-motion` (also `NO_COLOR`, or `animations = false`) turns off the
  splash, transitions and spinners; clocks and toasts then update once a
  second
- Effects draw at 60 frames a second, and at 20 when drawing is slow, such
  as over SSH
- `vhs` tapes in `examples/tapes/` and `make demo` to record the README GIFs.
  The README shows three of them in place of the screenshots of the old UI

### Changed

- SQLite: 0 and 1 in a column declared `BOOLEAN` read as booleans

## [0.1.4] - 2026-10-13

### Added

- Autocomplete in the editor: tables after `FROM`/`JOIN`/`INTO`/`UPDATE`,
  columns of the statement's tables after `SELECT`, `WHERE`, `ON`, `SET`,
  commas and operators, and tables or columns after `schema.`/`alias.`.
  `Ctrl+Space` asks anywhere. Names are quoted where needed. Tables and
  columns load in the background as schemas open
- A command palette (`Ctrl+P`) that finds any command by name and shows its
  key
- A record inspector (`Enter` or `i` in the results): every field of the row
  in full, JSON pretty-printed and binary as a hex dump, beside the grid or
  over it on narrow screens
- A confirmation before running `DELETE` or `UPDATE` without `WHERE`, `DROP`,
  `TRUNCATE` or `ALTER … DROP`, naming each one. Only `y` confirms.
  `confirm_destructive = false` turns it off
- Read-only connections refuse queries that write before sending them
- `dfox-core`: `DbClient::list_columns(schema)` and the engine command
  `ListColumns`, returning every column of a schema in one query

### Fixed

- Text typed right after inserting a suggestion or a paste with an empty
  selection replaced the inserted text

### Added

- The SQL editor highlights SQL for the connected database, and has undo and
  redo, selection with Shift, word moves, copy, cut and paste (including
  bracketed paste from the terminal) and indentation kept on Enter
- `F5` / `Ctrl+Enter` runs the statement under the cursor, or the selection;
  `Shift+F5` / `Ctrl+Shift+Enter` runs the whole script. A bar in the margin
  marks the statement that would run
- Query tabs (`Ctrl+N`, `Ctrl+W`, `Ctrl+PgDn`/`Ctrl+PgUp`), each with its own
  results
- Query history in `history.jsonl` in the state directory, searchable with
  `Ctrl+R`
- The result grid has a cell cursor, range and row selection, and columns
  sized from their values and resizable with `>` and `<`. NULL shows as `∅`,
  booleans as `✓`/`✗`, numbers are right-aligned and long ones grouped in
  thousands. Only the cells on screen are drawn, so a million rows scroll as
  fast as ten
- `Ctrl+C` copies the cell or the selection; `y` copies the selection or all
  rows as TSV, CSV, JSON, Markdown or `INSERT` statements for the queried
  table. Copying falls back to the terminal clipboard (OSC 52) over SSH
- Mouse support: click to focus a panel, place the cursor or pick a cell, drag
  to select, scroll with the wheel. `mouse = false` in `config.toml` turns it
  off
- The help screen scrolls

### Changed

- Postgres and MySQL values come back as numbers, booleans, JSON and bytes
  where the column type says so, instead of text. Booleans print as
  `true`/`false` and Postgres `bytea` as `0x…`, as MySQL binary already did
- The `copy_row` and `copy_all` key actions are now `copy` and `select_all`;
  the old names still work in `[keys]`

### Fixed

- Editing in the middle of a query no longer deletes from the end
- On Linux, copied text no longer disappears from the clipboard

## [0.1.3] - 2026-10-12

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
