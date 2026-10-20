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
```

From a source checkout, `make run` (or `cargo run --bin dfox`).

### Commands

```bash
dfox connect "local pg"   # open a saved connection by name
dfox list                 # the saved connections
dfox paths                # where settings, connections and history live
dfox version              # the version and the databases this build supports
dfox help                 # all commands and options
```

`--version`, `-V`, `--help` and `-h` work too. A mistake on the command line,
such as an unknown command or a bad URL, is reported in the terminal before
the TUI starts.

### Saved connections

Every query run is added to `history.jsonl` in `~/.local/state/dfox/` (or
`$XDG_STATE_HOME/dfox/`, `%LOCALAPPDATA%\dfox\` on Windows, or the directory
in `DFOX_STATE_DIR`), readable only by you.

Saved connections live in `connections.toml` in the config directory, and
their passwords in the OS keychain (Keychain on macOS, Credential Manager on
Windows, the Secret Service on Linux), never in the file. Without a saved
password, dfox looks in `~/.pgpass` (or `$PGPASSFILE`) for Postgres, and
otherwise asks.

The manager also lists connections found on the machine: `DATABASE_URL`
(from the environment or a `.env` file) and the entries of `~/.pgpass`. Press
**e** on one to save it. Every connection is checked in the background, so a
dot shows whether it is reachable and a sparkline shows its latency.

A connection can be **read-only**: the server refuses writes (Postgres and
MySQL start the session read-only, SQLite opens the file read-only; for
MongoDB, which has no read-only sessions, dfox refuses commands that write). **TLS**
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
- **Ctrl+N** / **Ctrl+W** - New / close query tab; **Ctrl+PgDn** / **Ctrl+PgUp** (or
  **Alt+]** / **Alt+[**, or a click on a tab's name) switch tabs. Each tab keeps its own
  results. On a Mac keyboard, Ctrl+PgDn is **Ctrl+Fn+↓**

### Results
- **←/→/↑/↓** - Move between cells; **Ctrl+←/→** first/last column
- **Shift+arrows** - Select a range; **V** whole rows; **Ctrl+A** everything; **Esc** clears
- **Ctrl+C** - Copy the cell or the selection (tab-separated)
- **y** - Copy as TSV, CSV, JSON, Markdown or `INSERT` statements (the selection, or every row)
- **>** / **<** - Widen / narrow the column
- **Enter** or **i** - Inspect the row: every field in full, JSON pretty-printed, binary as a hex dump

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
icons = "auto"        # "nerd" for Nerd Font glyphs, "ascii" for plain text
mouse = true          # false leaves the mouse to the terminal
confirm_destructive = true  # false runs DELETE without WHERE, DROP, … without asking
row_limit = 100000    # rows kept from a query; the rest are read and dropped
update_check = true   # false: never ask GitHub whether a newer dfox is out

[keys]                # replaces all keys of an action
execute = ["f5", "ctrl+r"]
```

Once a day dfox asks GitHub, in the background, for the latest release. When
a newer one is out, the connection manager's status bar says so from the next
start, with the way to upgrade that fits how dfox was installed: `brew
upgrade dfox` for Homebrew, `dfox-update` for the install scripts, or the
releases page. Nothing is sent but the request itself.

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
