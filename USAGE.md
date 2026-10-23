# Using dfox

How to connect, the keys, and the settings. For building dfox and changing
it, see [CONTRIBUTING.md](CONTRIBUTING.md).

## Connecting

```bash
dfox                                        # the connection manager
dfox postgres://user@localhost:5432/shop    # connect right away
dfox mysql://root@127.0.0.1/app
dfox ./data.sqlite                          # a SQLite file
dfox mongodb://root@127.0.0.1:27017/shop
dfox cassandra://cassandra@127.0.0.1:9042/shop  # Cassandra or ScyllaDB; the path is the keyspace
dfox redis://:password@127.0.0.1:6379/0      # Redis or Valkey; the path is the database number
```

From a source checkout, `make run` (or `cargo run --bin dfox`).

### Commands

```bash
dfox connect "local pg"   # open a saved connection by name
dfox list                 # the saved connections
dfox paths                # where settings, connections and history live
dfox version              # the version
dfox help                 # all commands and options
```

Rows can be exported and copied without the TUI, for scripts and cron jobs:

```bash
dfox export prod "SELECT * FROM orders" -o orders.parquet    # .csv, .tsv, .jsonl or .parquet
dfox copy prod "SELECT * FROM orders" warehouse orders --create
dfox export postgres://app@db.local/shop "SELECT 1" -o one.csv
```

A connection is a saved one's name or a URL. Passwords come from the keychain,
`~/.pgpass`, `~/.my.cnf` or the URL: these commands never ask, and never print a URL's
password.
The rows written show on stderr as they go, then a summary; the exit code is 1 when it
fails. `--create` makes the target table from the rows' columns.

`--version`, `-V`, `--help` and `-h` work too. A mistake on the command line,
such as an unknown command or a bad URL, is reported in the terminal before
the TUI starts.

### Redis and Valkey

The editor takes Redis commands as `redis-cli` does: one a line, with `"double quotes"`
(and escapes such as `\n`) or `'single quotes'` around words with spaces; a line starting
with `#` is a comment. **F5** runs the command under the cursor, **Shift+F5** all of
them. Replies show as tables: a hash as `field` and `value`, a sorted set with
`WITHSCORES` as `member` and `score`, `INFO` as `section`, `key` and `value`. dfox needs
Redis 6 or Valkey 7 or newer, and finds keys with `SCAN`, never `KEYS`; a database with
more than 10,000 keys under one prefix shows the first 10,000. `FLUSHDB`, `FLUSHALL` and
`SHUTDOWN` ask first.

### Saved connections

Every query run is added to `history.jsonl` in `~/.local/state/dfox/` (or
`$XDG_STATE_HOME/dfox/`, `%LOCALAPPDATA%\dfox\` on Windows, or the directory
in `DFOX_STATE_DIR`), readable only by you.

Saved connections live in `connections.toml` in the config directory, and
their passwords in the OS keychain (Keychain on macOS, Credential Manager on
Windows, the Secret Service on Linux), never in the file. Without a saved
password, dfox looks in `~/.pgpass` (or `$PGPASSFILE`) for Postgres and in
`~/.my.cnf` (its `[client]` and `[mysql]` sections, for the user it names) for
MySQL, and otherwise asks.

The manager also lists connections found on the machine: `DATABASE_URL`
(from the environment or a `.env` file), the entries of `~/.pgpass`, and
`~/.my.cnf` when it names a host. Press
**e** on one to save it. Every connection is checked in the background, so a
dot shows whether it is reachable and a sparkline shows its latency.

A connection can be **read-only**: the server refuses writes (Postgres and
MySQL start the session read-only, SQLite opens the file read-only; for
MongoDB and Redis, which have no read-only sessions, dfox refuses commands that
write). **TLS**
can be `prefer` (the default), `require`, `verify` (the server's certificate
and name are checked against the system's roots and an optional **CA file**)
or turned off, and a coloured **tag** marks it in the list and the status bar.

A connection can go through an **SSH jump host** when the database isn't
reachable directly. dfox logs in with your ssh-agent, a key file or a
password, and forwards a local port to the database; TLS still checks the
database's own name. The jump host's key must be in `~/.ssh/known_hosts` (or
the file you choose): an unknown key is refused with the `ssh-keyscan`
command that adds it, and a changed key is always refused. The SSH password
or key passphrase goes to the keychain like the database password, and dfox
asks for an SSH password that isn't saved. In `connections.toml`:

```toml
[[connection]]
name = "prod"
driver = "postgres"
host = "db.internal"
database = "shop"
user = "app"

[connection.ssh]
host = "bastion.example.com"
user = "me"
login = "key"               # agent, key or password
key = "~/.ssh/id_ed25519"
```

## Keyboard Shortcuts

Press **?** (or **F2**) in the app for the full list, generated from your keymap.
**↑/↓** move through the list and **←/→** between its columns. To change a key
there, move to the action, press **Enter**, then the new key, then
**Enter** again to confirm (any other key cancels): it replaces the action's keys and
is saved in `config.toml`. The confirmation says when the key is taken from another
action. **Delete**, confirmed the same way, brings the action's default keys back.
**Ctrl+P** opens the command palette: type part of a command's name and press **Enter**.
**Space** opens a command menu outside text fields.

### Connections
- **Enter** - Connect
- **n** / **e** / **d** - New, edit (or save a found connection), delete
- **R** - Check every connection again

### Explorer
- **Enter** on a table, view or collection - Its menu: first or last 100 rows, all rows,
  filtered rows, an exact count, scripts, properties, refresh, truncate or drop. On
  Cassandra, the last 100 rows of a partition you name instead
- **Enter** on a Redis key - Its menu: the first 100 items or all of them (as `GET`,
  `HGETALL`, `LRANGE`, `SMEMBERS`, `ZRANGE … WITHSCORES` or `XRANGE`, by its type), a count,
  properties (type, size, expiry), refresh or delete. Keys are grouped by `:` into prefixes
- **→** - Open a database, schema or table: a table holds its columns, constraints,
  indexes, triggers and policies
- **Enter** on a column - Its card: type, keys, indexes, comment, and what its values look
  like (**p** profiles the first 100k rows)
- **Enter** on a function, procedure, constraint, index or trigger, or **e** on a view - The
  SQL that creates it, in a tab
- **i** - Put the selected name at the editor's cursor
- **←** - Close, or go to the parent
- **/** - Fuzzy-find a table, view or routine; **Enter** jumps to it
- Opening another database switches the session to it

### General Navigation
- **Tab** / **Shift+Tab** - Move between the explorer, the query editor and the results
- **↑/↓** - Move in lists and results
- **Page Up/Page Down** - Scroll a page
- **Home/End** - Jump to the first/last row
- The mouse: click a panel, a cell or a place in the text; drag to select; the wheel
  scrolls (with **Shift**, sideways in the results). Hold **Shift** (**Option** in some
  macOS terminals) to select text the terminal's way, or set `mouse = false`

### Query Operations
- **F5**, **Ctrl+E** or **Ctrl+Enter** - Run the statement under the cursor, or the selection
- **Shift+F5** or **Ctrl+Shift+Enter** - Run the whole script
- **Ctrl+C** - Cancel the running query
- **Ctrl+R** - Search the query history; **Enter** puts the query in the editor
- `DELETE` or `UPDATE` without `WHERE`, `DROP`, `TRUNCATE` and `ALTER … DROP` ask first
  (**y** runs them; **Enter** doesn't). Read-only connections don't send queries that write
- A **TXN** badge in the status bar marks a transaction you opened (`BEGIN`, `START
  TRANSACTION`) and haven't closed (`COMMIT`, `ROLLBACK`, `END`) yet
- **Ctrl+N** / **Ctrl+W** - New / close query tab; **Ctrl+PgDn** / **Ctrl+PgUp** (or
  **Alt+]** / **Alt+[**, or a click on a tab's name) switch tabs. Each tab keeps its own
  results. On a Mac keyboard, Ctrl+PgDn is **Ctrl+Fn+↓**

### Results
- **←/→/↑/↓** - Move between cells; **Ctrl+←/→** first/last column
- **Shift+arrows** - Select a range; **V** whole rows; **Ctrl+A** everything; **Esc** clears
- **Ctrl+C** - Copy the cell or the selection (tab-separated)
- **y** - Copy as TSV, CSV, JSON, Markdown or `INSERT` statements (the selection, or every row)
- **>** / **<** - Widen / narrow the column
- **s** - Sort by the column: ascending (▲), descending (▼), then back to the order the rows
  came in. It sorts the rows fetched, once they are all in; decimals sort as numbers
- **Enter** or **i** - Inspect the row: every field in full, JSON pretty-printed, binary as a hex dump
- **e** - Edit the cell, in rows opened from a table that has a primary key: type, then
  **Enter** keeps the change, **Esc** drops it, **Del** makes it NULL. **o** adds a row
  below, whose cells left blank get their defaults. **d** marks the row to delete (again
  to keep it), **Ctrl+Z** undoes the last change. Changed cells
  and deleted rows show until **Ctrl+S**, which lists them and saves them together in
  one transaction (**x** there discards them all); the rows then load again.
  Postgres, MySQL, SQLite and MongoDB. In MongoDB a document is found by its `_id`, a
  changed field keeps its type (a number stays a number), and a new field reads as the
  mongo shell reads it (`42` a number, `"42"` or `Ann` text, `{…}` a document); the
  changes are written one by one, as a single server has no transactions
- **x** - Export the rows to a file: the query runs again, whole, without the row limit,
  and its rows are written as they come. The file's extension picks the format: `.csv`,
  `.tsv`, `.jsonl` (or `.ndjson`) or `.parquet`. The status bar shows how far it has got;
  **Ctrl+C** cancels it, and a cancelled or failed export leaves no file. Only a query
  that reads can be exported. A table's menu has **Export…** for all its rows, and
  **Import…** to add a file's rows to the table: after the file, each of its columns is
  shown with the table column it goes into, matched by name (whatever its case) or
  skipped; **↑/↓** pick a column, **←/→** change where it goes (or **skip**), **Enter**
  imports. Empty cells are NULL and the table's columns left out get their defaults.
  When the table's columns aren't known yet, the file's match them by name, and a column
  the table doesn't have stops the import before any row.
  Rows go in ten thousand at a time, each batch in its own transaction (a COPY in
  Postgres, multi-row inserts in MySQL, `insertMany` in MongoDB), so a failed import
  says how many went in. **Copy to…** sends all the rows to a table of another saved
  connection (Postgres, MySQL, SQLite or MongoDB): pick it with ←/→, name the table
  (`schema.table` in Postgres), and say whether to create it from the rows' columns, in
  that database's own types. The connection's password must be saved, or in `~/.pgpass`
- **J** - Show the rows as JSON documents, and back to the grid. Each row is a document
  shown on one line; **→** or **Enter** opens it, and the objects and lists in it, **←**
  closes them or goes up. Columns that hold JSON (MongoDB's nested documents, a Redis
  string of JSON) open as JSON. **Ctrl+C** copies the JSON at the cursor

### Interface Controls
- **F1** - Back to the connections
- **Ctrl+T** - Next theme
- **F12** - Event log
- **Esc**, **q** or **Ctrl+Q** - Quit (asks first while connected)

### Editor
- Typing inserts at the cursor; **Enter** starts a new line with the same indentation
- **Ctrl+Z** / **Ctrl+Y** - Undo / redo
- **Shift+arrows**, **Shift+Home/End** - Select; **Ctrl+A** selects everything
- **Ctrl+C** / **Ctrl+X** / **Ctrl+V** - Copy / cut / paste (copy without a selection takes the line)
- **Ctrl+←/→** or **Alt+←/→** - Move by word; **Alt+Backspace** deletes a word
- A bar in the margin marks the statement **F5** would run
- Table names are suggested after `FROM`/`JOIN`, columns after `SELECT`/`WHERE`/…, and members
  after `alias.`; **Ctrl+Space** asks anywhere. **↑/↓** pick, **Tab** or **Enter** inserts, **Esc** closes

## Configuration

dfox reads `config.toml` from `~/.config/dfox/` (or `$XDG_CONFIG_HOME/dfox/`,
`%APPDATA%\dfox\` on Windows, or the directory in `DFOX_CONFIG_DIR`). Every
setting is optional:

```toml
theme = "dfox"        # dfox, tokyo-night, catppuccin-mocha, catppuccin-latte,
                      # gruvbox, nord, high-contrast, or a file in themes/
keymap = "standard"   # or "vim": hjkl, g/G, Ctrl+D/Ctrl+U outside text fields
animations = true     # false: no splash, transitions or spinners (or --reduced-motion, NO_COLOR)
icons = "auto"        # "nerd": Nerd Font glyphs, a logo per database; "ascii": plain text
mouse = true          # false leaves the mouse to the terminal
confirm_destructive = true  # false runs DELETE without WHERE, DROP, … without asking
row_limit = 100000    # rows kept from a query; the rest are read and dropped
update_check = true   # false: never ask GitHub whether a newer dfox is out

[keys]                # replaces all keys of an action
execute = ["f5", "ctrl+r"]
```

Keys under `[keys]` are written like `ctrl+e`, `alt+]`, `shift+tab`, `ctrl+shift+z`,
`f5`, `G`, `?`, `space`, `enter`, `esc`, `tab`, `backspace`, `delete`, `up`, `down`,
`left`, `right`, `pageup`, `pagedown`, `home` and `end`. An empty list leaves the
action without a key. The actions and their default keys:

| Action | Does | Default keys |
|---|---|---|
| `palette` | Command palette | `ctrl+p` |
| `execute` | Run statement/selection | `f5`, `ctrl+e`, `ctrl+enter` |
| `execute_all` | Run the whole script | `shift+f5`, `ctrl+shift+enter`, `ctrl+shift+e` |
| `complete` | Suggest names | `ctrl+space` |
| `history` | Query history | `ctrl+r` |
| `new_tab` | New query tab | `ctrl+n` |
| `close_tab` | Close query tab | `ctrl+w` |
| `next_tab` | Next query tab | `ctrl+pagedown`, `alt+]` |
| `prev_tab` | Previous query tab | `ctrl+pageup`, `alt+[` |
| `copy` | Copy / cancel query | `ctrl+c` |
| `copy_as` | Copy as CSV, JSON, … | `y` |
| `cut` | Cut | `ctrl+x` |
| `paste` | Paste | `ctrl+v` |
| `select_all` | Select all | `ctrl+a` |
| `select_row` | Select whole rows | `V` |
| `undo` | Undo | `ctrl+z` |
| `redo` | Redo | `ctrl+y`, `ctrl+shift+z` |
| `inspect` | Inspect / insert name | `i` |
| `json_view` | Results as JSON / grid | `J` |
| `add_row` | Add a row | `o` |
| `export` | Export rows to a file | `x` |
| `wider` | Widen column | `>` |
| `sort` | Sort by column | `s` |
| `narrower` | Narrow column | `<` |
| `next_focus` | Next panel | `tab` |
| `prev_focus` | Previous panel | `shift+tab` |
| `filter` | Filter | `/` |
| `refresh` | Refresh | `R` |
| `new` | New connection | `n` |
| `edit` | Edit / definition | `e` |
| `delete` | Delete | `d`, `delete` |
| `save` | Save | `ctrl+s` |
| `connections` | Connection list | `f1` |
| `next_theme` | Next theme | `ctrl+t` |
| `debug` | Event log | `f12` |
| `menu` | Command menu | `space` |
| `help` | Show keys | `?`, `f2` |
| `back` | Back / close | `esc` |
| `quit` | Quit | `ctrl+q`, `q` |
| `confirm` | Select / new line | `enter` |
| `up` | Up | `up` (vim adds `k`) |
| `down` | Down | `down` (vim adds `j`) |
| `left` | Left / previous column | `left` (vim adds `h`) |
| `right` | Right / next column | `right` (vim adds `l`) |
| `page_up` | Page up | `pageup` (vim adds `ctrl+u`) |
| `page_down` | Page down | `pagedown` (vim adds `ctrl+d`) |
| `home` | First row / line start | `home` (vim adds `g`) |
| `end` | Last row / line end | `end` (vim adds `G`) |
| `backspace` | Delete back | `backspace` |
| `delete_word` | Delete a word | `alt+backspace`, `ctrl+backspace` |
| `word_left` | Previous word | `ctrl+left`, `alt+left`, `alt+b` |
| `word_right` | Next word | `ctrl+right`, `alt+right`, `alt+f` |
| `select_left` | Select left | `shift+left` |
| `select_right` | Select right | `shift+right` |
| `select_up` | Select up | `shift+up` |
| `select_down` | Select down | `shift+down` |
| `select_home` | Select to line start | `shift+home` |
| `select_end` | Select to line end | `shift+end` |

Once a day dfox asks GitHub, in the background, for the latest release. When
a newer one is out, the connection manager's status bar says so from the next
start, with the way to upgrade that fits how dfox was installed: `brew
update && brew upgrade dfox` for Homebrew, `apt` or `dnf` for their repositories,
`dfox-update` for the install scripts, or the releases page. Nothing is sent but the request itself.

A theme file in `themes/<name>.toml` sets any colours as `"#rrggbb"` and takes
the rest from the theme it `extends`:

```toml
extends = "nord"
accent = "#ff9e3b"
border_focus = "#ff9e3b"
```

The colours are `bg`, `surface`, `surface_alt`, `border`, `border_focus`,
`text`, `text_dim`, `accent`, `accent_alt`, `success`, `warning`, `danger`,
`null`, `number`, `string`, `keyword`, `comment` and `selection`. On terminals
without true colour, dfox picks the nearest of 256 or 16 colours.

## Debug logging

DFox includes a comprehensive logging system to help with troubleshooting and development. By default, logging is disabled for better performance. To enable debug logging:

1. Create a `.env` file in the project root directory:
```bash
# .env file
RUST_LOG=debug
```

2. Run the application as usual. Debug logs will be written to `dfox-debug.log` in the current directory.

3. You can also view the event log directly in the application by pressing **F12** while using the interface.

### Log Levels
- `off` - No logging (default)
- `error` - Only errors
- `warn` - Warnings and errors
- `info` - General information, warnings, and errors  
- `debug` - Detailed debugging information (recommended for troubleshooting)
- `trace` - Very verbose logging

Example `.env` configurations:
```bash
# For normal usage (default)
RUST_LOG=off

# For basic error tracking
RUST_LOG=error

# For troubleshooting
RUST_LOG=debug

# For development
RUST_LOG=trace
```
