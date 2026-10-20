# DFox — Changelog

All notable changes are documented here. Format follows [Keep a Changelog](https://keepachangelog.com/),
and the project uses [Semantic Versioning](https://semver.org/).

Versions below 1.0.0 may change behaviour, keybindings and the `dfox-core` API between releases.

## [Unreleased]

## [0.3.5] - 2026-10-20

### Added

- Install with Homebrew (`brew install 0xataru/tap/dfox`), an install script
  for macOS and Linux, a PowerShell script or an `.msi` for Windows. Releases
  now include Linux ARM64.

### Changed

- Release archives are named `dfox-<platform>.tar.xz` (`.zip` on Windows),
  each with a `.sha256` file and all listed in `sha256.sum`, in place of `SHA256SUMS`.

## [0.3.4] - 2026-10-20

### Changed

- The binary is now called `dfox` (it was `dfox-tui`).

### Security

- Updated `rustls-webpki` and `webpki-roots` so TLS connections no longer
  trust distrusted intermediate certificates (RUSTSEC-2026-0381, RUSTSEC-2026-0382).

## [0.3.3] - 2026-10-16

### Added

- MongoDB. Connect with `mongodb://` or the connection form; the explorer
  lists databases and their collections and views, with document counts, the
  fields each collection's documents have and its indexes. The editor runs
  mongo shell commands: `db.users.find({ age: { $gt: 30 } }).sort({ name: 1
  }).limit(10)`, `findOne`, `aggregate`, `countDocuments`, `distinct`,
  `insertOne`/`insertMany`, `updateOne`/`updateMany`, `deleteOne`/
  `deleteMany`, `createIndex`, `db.runCommand`, `show dbs`, `show
  collections` and `use <db>`, with the shell's `ObjectId(…)`, `ISODate(…)`,
  `NumberLong(…)` and `/regex/`. Documents show as columns learnt from them:
  numbers, text, dates and ids as such, objects and arrays as JSON. Running
  commands can be cancelled, and a read-only connection refuses those that
  write
- The editor reads mongo shell commands as such: F5 runs the command under
  the cursor, across lines, and commands are coloured, with `$operators`
  standing out. Completion offers collections after `db.`, methods after
  `db.orders.`, and the collection's fields as keys inside `{ … }`.
  Deleting or updating without a filter, `drop()` and `dropDatabase()` ask
  first, as `DELETE` without `WHERE` does. The table menu shows a
  collection's rows with `find` queries, writes `find`, `insertOne`,
  `updateOne` and `deleteOne` scripts, and deletes every document or drops
  the collection
- Cassandra and ScyllaDB. Connect with `cassandra://` (or `scylla://`) or the
  connection form; the explorer lists keyspaces and their tables and
  materialized views, with the partition and clustering keys, secondary
  indexes and the `CREATE` statement the server describes. CQL runs one
  statement at a time, batches whole, and SELECTs page through big tables in
  batches. Rows keep their types: integers, exact decimals and varints,
  timestamps, dates, times, uuids, and collections, tuples and user-defined
  types as JSON. A read-only connection refuses statements that write. dfox
  connects through the address given, so clusters behind Docker, NAT or an
  SSH tunnel work, and reads with consistency `ONE`, as `cqlsh` does, so a
  single node or a cluster with replicas down still answers. Encrypted
  connections to Cassandra aren't supported yet
- CQL has its own colours in the editor (`KEYSPACE`, `ALLOW FILTERING`,
  `BATCH`, `TTL`, `frozen`, `map`, …). The table menu's filtered rows add
  `ALLOW FILTERING`, and "Partition rows…" shows the last 100 rows of the
  partition you name, the highest clustering keys first (the newest, for
  time series), as CQL can order rows only within a partition
- Clicking a tab's name switches to it, and the status bar shows F1 for the
  connection list

### Changed

- Release binaries are stripped of debug symbols, which makes them about
  2.3 MB smaller

## [0.3.2] - 2026-10-16

### Added

- `DELIMITER` lines in MySQL scripts, as the mysql client reads them:
  `DELIMITER $$` makes `$$` end a statement until `DELIMITER ;`. Running a
  script sends it to the server without them

### Fixed

- Running the statement under the cursor sends a whole procedure, function,
  trigger or event: the `;` inside its `BEGIN … END` no longer cut it short.
  The same goes for SQLite triggers and PostgreSQL `BEGIN ATOMIC` bodies
- The destructive-query check reads statements the same way, so a
  `DELETE` inside a procedure's body no longer asks for confirmation

## [0.3.1] - 2026-10-16

### Added

- Enter on a table or view opens a menu, as in pgAdmin: its first 100 rows,
  its last 100 by primary key, all rows, rows matching a condition, an exact
  row count, SELECT/INSERT/UPDATE/DELETE/CREATE scripts in a new tab,
  inserting its name, its properties, refreshing it, and truncating or
  dropping it (asked first, never on a read-only connection). Each item has
  a letter; Enter twice shows the first rows. The rows come from a query put
  in a tab named after the table, such as `SELECT * FROM calls ORDER BY id
  DESC LIMIT 100`, to read, change and run again. Tables and views show ▸,
  and → opens them
- `i` in the explorer puts the selected name at the editor's cursor: a
  column by itself, a table without its schema when the schema is in use
- Enter on a function or procedure, or `e` on a view, opens the SQL that
  creates it in a tab named after it: every overload of a PostgreSQL
  function, `SHOW CREATE` on MySQL, the view's own SQL on SQLite
- Functions and procedures show their arguments and result in the explorer,
  or how many overloads they have
- A table opened in the explorer holds folders, as in pgAdmin: Columns,
  Constraints (primary key, foreign keys, unique, check), Indexes, Triggers
  and, on PostgreSQL, row-level security Policies. Enter on a constraint,
  index, trigger or policy opens the SQL that creates it
- Enter on a column shows a card with everything about it: type, null,
  default, generated expression, keys, the column a foreign key points at
  (Enter goes there), indexes, collation and comment. Below, what its values
  look like: nulls, distinct values, the most common ones and how values
  spread, from PostgreSQL's `pg_stats` or MySQL 8 histograms. `p` counts them
  over the first 100k rows on any database

### Fixed

- On MySQL, an error in a statement after one that returns no rows (`CREATE`,
  `INSERT`, `DO`) was lost, and the script looked as if it had succeeded
- Names that are reserved words, such as `order` or `user`, are quoted in
  completions and in copied INSERT statements

## [0.3.0] - 2026-10-16

### Added

- A native engine. Every database runs on its own client and decodes rows
  straight into Apache Arrow: PostgreSQL on `tokio-postgres` and its binary
  protocol, MySQL and MariaDB on `mysql_async`, SQLite on `rusqlite`.
  Reading a million rows is 1.8× faster on PostgreSQL, 1.85× on MySQL and
  4.9× on SQLite, and the first rows show sooner. A million rows of four
  columns take about 54 MB
- PostgreSQL shows `numeric` exactly, and decodes intervals, `uuid`,
  `inet`/`cidr`/`macaddr`, `money`, bit strings, points, arrays, enums and
  domains; a type with no decoder keeps its bytes rather than turning into
  NULL. `RAISE NOTICE` messages show as toasts
- MySQL keeps each integer's width and sign, reads `TINYINT(1)` as a
  boolean, keeps `DECIMAL` exact, shows binary strings and `BIT` as bytes,
  and prints dates and times as MySQL does, zero dates and long `TIME`
  values included. A script goes to the server whole, so procedures and
  triggers need no `DELIMITER`, and `CALL` returns every result set.
  Warnings show as toasts
- SQLite handles each value's own type: a column takes the type of its first
  value and widens as others arrive, without losing any. Trigger bodies stay
  whole in scripts
- Cancelling stops a query within milliseconds on every database
- SSH jump hosts: a connection can go through one, logging in with your
  ssh-agent, a key file or a password. The jump host's key must be in
  `known_hosts` (or a file you choose): an unknown key is refused with the
  `ssh-keyscan` command that adds it, and a changed key is always refused.
  TLS still checks the database's own name. The SSH password or key
  passphrase is kept in the keychain, and asked for when it isn't saved
- TLS `verify` checks the server's certificate and name against the
  system's roots and an optional CA file, for PostgreSQL and MySQL. A
  failed handshake says why. On MySQL, `prefer` falls back to a plain
  connection when the server has no TLS
- The explorer shows every MySQL database with its tables, and SQLite's
  `main` and attached databases; it keeps browsing while a query runs, and
  lists 5,000 tables in about 30 ms
- Benchmarks (`make bench`) for reading, memory and drawing the grid. Pull
  requests are compared with `main` and fail on a clear slowdown

### Changed

- The release binary is 13.2 MB, down from 18.8 MB in v0.2.0, with
  whole-program optimisation, one codegen unit, abort on panic and the
  mimalloc allocator
- Each database is its own crate (`dfox-driver-postgres`,
  `dfox-driver-mysql`, `dfox-driver-sqlite`) and a cargo feature of
  `dfox-tui`, all on by default: `--no-default-features --features sqlite`
  builds a SQLite-only binary
- The SQL helpers (highlighting, statement splitting, autocomplete context,
  the destructive-statement guard) are a crate of their own, `dfox-sql`
- For library users: `dfox-core` knows no database. A `Driver` opens a
  `Session` that streams Arrow batches, exposes a `Catalog` tree, applies
  edits by primary key and can be cancelled; `Capabilities` say what it
  supports. The engine runs on sessions and opens them through an `Opener`,
  such as `dfox_ssh::open`. `dfox-testkit` holds the suite every driver
  passes

### Removed

- sqlx, and with it `DbClient`, `RowStore`, `DbManager`, `ConnectionConfig`
  and `async-trait`

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
