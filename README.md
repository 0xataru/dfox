<p align="center">
  <img src="examples/mini_logo.png" alt="DFox logo" width="300">
</p>

# DFox - ⚡️ Blazing Fast Terminal Database Manager

<p align="center">
  <a href="https://github.com/0xataru/dfox/actions/workflows/ci.yml"><img src="https://github.com/0xataru/dfox/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://codecov.io/gh/0xataru/dfox"><img src="https://codecov.io/gh/0xataru/dfox/graph/badge.svg" alt="Coverage" /></a>
  <a href="https://scorecard.dev/viewer/?uri=github.com/0xataru/dfox"><img src="https://api.scorecard.dev/projects/github.com/0xataru/dfox/badge" alt="OpenSSF Scorecard" /></a>
  <a href="https://github.com/0xataru/dfox/releases/latest"><img src="https://img.shields.io/github/v/release/0xataru/dfox?label=release" alt="Latest release" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache%202.0-blue?logo=apache&logoColor=white" alt="License: Apache 2.0" /></a>
  <a href="https://ratatui.rs/"><img src="https://img.shields.io/badge/Built_With_Ratatui-000?logo=ratatui&logoColor=fff" alt="Built With Ratatui" /></a>
</p>

**DFox** is a Rust-based database management tool that provides a terminal user interface (TUI) for interacting with PostgreSQL, MySQL, SQLite and MongoDB databases.
It allows users to perform database operations easily and efficiently through a simple and intuitive interface.

## A Tour

Connect to a saved connection, write a query with autocomplete, browse the results and
inspect a row, then find any command in the palette:

![dfox: connections, autocomplete, results and the inspector](./examples/demo/overview.gif)

The editor runs the statement under the cursor, keeps query tabs and a searchable history,
and asks before a `DELETE` without `WHERE`:

![dfox: statements, tabs, history and the destructive-query guard](./examples/demo/editor.gif)

The grid streams 50,000 rows, moves by cell, selects rows, widens columns and copies as JSON:

![dfox: the result grid](./examples/demo/grid.gif)

## Installation

### Prebuilt binaries

Download the archive for your platform from the
[latest release](https://github.com/0xataru/dfox/releases/latest) (Linux x86_64,
macOS Intel and Apple silicon, Windows x86_64), extract it and run `dfox-tui`.
Each release includes a `SHA256SUMS` file to verify the download.

### From source

You need stable [Rust](https://www.rust-lang.org/tools/install):

```bash
git clone https://github.com/0xataru/dfox.git
cd dfox
cargo build --release
./target/release/dfox-tui
```

Then `dfox-tui` opens the connection manager, or connects right away to a URL:
`dfox-tui postgres://user@localhost:5432/shop`, `mysql://…`, `mongodb://…`, or a SQLite file.

## More

- [USAGE.md](USAGE.md): connecting, keyboard shortcuts, configuration and themes.
- [CONTRIBUTING.md](CONTRIBUTING.md): building, testing and the project's layout;
  [CHANGELOG.md](CHANGELOG.md) for what changed.
- Security issues: [SECURITY.md](SECURITY.md). This project follows a
  [code of conduct](CODE_OF_CONDUCT.md).

Thanks to the [Ukrainian Rust Community](https://github.com/rust-lang-ua) for their support.
