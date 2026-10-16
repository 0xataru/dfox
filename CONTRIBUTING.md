# Contributing

Thanks for taking the time. A few things worth knowing before you start.

## What to work on

Bug reports, fixes, tests and documentation improvements are always welcome
without asking first.

For new features, new database backends or changes to keybindings, please
[open an issue](https://github.com/0xataru/dfox/issues/new/choose) first so we
can agree on the approach before you spend time on code.

Questions and open-ended ideas belong in
[Discussions](https://github.com/0xataru/dfox/discussions).

## Project layout

DFox is a Cargo workspace; every crate lives in `crates/`:

- **`dfox-core`**: the driver interfaces, query results, connection models,
  and the engine that runs database work in the background. It knows no
  particular database. The app runs on `Session` + `Catalog`: Arrow
  batches, a catalog tree, edits by primary key. The engine holds one
  session and opens it through an `Opener`, which the app points at
  `dfox_ssh::open` for tunnels.
- **`dfox-driver-postgres`**, **`dfox-driver-mysql`**, **`dfox-driver-sqlite`**:
  one crate per database, each with its integration tests.
- **`dfox-ssh`**: SSH tunnels on `russh`, for any driver: `dfox_ssh::open`
  logs in to the jump host (agent, key or password), checks its key against
  `known_hosts`, and opens the driver's session through a forwarded local
  port. A driver connects to `ConnectionParams::tunnel` when it is set, but
  keeps the original host name for TLS.
- **`dfox-sql`**: reading SQL without running it: highlighting, splitting
  scripts, autocomplete context and the destructive-statement guard.
- **`dfox-tui`**: the terminal interface, built with `ratatui` and `crossterm`.
  Each driver is a cargo feature (`postgres`, `mysql`, `sqlite`, all on by
  default), so `cargo build -p dfox-tui --no-default-features --features sqlite`
  builds a SQLite-only binary.
- **`dfox-bench`**: benchmarks of decoding, memory and drawing (`make
  bench`), with the numbers to beat in `crates/dfox-bench/BASELINE.md`. Pull
  requests run them against `main` and fail on a clear slowdown of more than
  10%. Not published.
- **`dfox-testkit`**: helpers for the driver tests, the conformance suite
  every driver's `Session` must pass, a mock driver that passes it without a
  server, and tests of the engine over every driver. Not published.

New database support is a new driver crate implementing `Driver` (with
`open` returning a `Session`), a test that runs
`dfox_testkit::conformance::check` against the database, and a feature in
`dfox-tui`. The TUI should not need to know which database it is talking to.

## Workflow

You need stable Rust (install with [rustup](https://rustup.rs/)) and `make`.
To try your changes against a real database, a local PostgreSQL or MySQL server
is enough. Docker works well:

```sh
docker run --rm -e POSTGRES_PASSWORD=postgres -p 5432:5432 postgres
```

1. **Fork and clone**, then create a branch:

   ```sh
   git clone https://github.com/<you>/dfox && cd dfox
   git switch -c fix/query-scroll
   ```

2. **Make your change.** Add a test where the code can be tested without a
   terminal, for example type mapping in a driver crate. Anything drawn on screen
   gets an `insta` snapshot test; after changing how something looks, run
   `cargo insta review` (from `cargo install cargo-insta`) and accept the
   snapshots that show what you meant. Colours come only from the theme in
   `crates/dfox-tui/src/theme/`, and components never `.await`; `scripts/guards.sh`
   checks both.

3. **Run what CI runs:**

   ```sh
   make fmt     # format the workspace
   make check   # rustfmt check, clippy with -D warnings, guards, tests: exactly what CI runs
   ```

   `make cover` shows which lines your tests reach. Pull requests also get a
   Codecov comment with the coverage of the changed lines.

4. **Run it:** `make run`, and try the screens your change touches.

5. **Add a line under `[Unreleased]`** in [CHANGELOG.md](CHANGELOG.md) if users
   would notice the change.

6. **Commit, push and open a pull request** against `main`.

### Commit messages and PR titles

Use a short [conventional commit](https://www.conventionalcommits.org/) line,
for example:

```text
fix(tui): keep the selected row visible after scrolling
feat(core): support SQLite connections
docs: describe the F12 debug panel
```

Pull requests are squash-merged, so **the PR title becomes the commit message
on `main`**.

### Commands

`make help` lists them all:

| Command | What it does |
| --- | --- |
| `make check` | Everything CI requires: `fmt-check`, `lint`, `test` |
| `make run` | Run the TUI |
| `make test` | The test suite |
| `make test-it` | The test suite against Postgres and MySQL in Docker, plus a second Postgres with TLS and a throwaway CA (`scripts/tls-postgres.sh`) for the `verify-full` tests, and an SSH jump host with test keys (`scripts/ssh-server.sh`) for the tunnel tests |
| `make bench` | The benchmarks; Postgres and MySQL ones need `make test-it`'s databases |
| `make fmt` | Format the workspace with rustfmt |
| `make lint` | clippy with warnings as errors |
| `make cover` | Line coverage with an HTML report (needs `cargo-llvm-cov`) |
| `make build` | Build the workspace |
| `make release-build` | Optimised build of `dfox-tui` |
| `make audit` | Check dependencies against the RustSec advisory database |
| `make clean` | Remove build artefacts |
| `make release VERSION=X.Y.Z` | Maintainer only: open the release pull request |
| `make publish` | Maintainer only: tag the merged release |

### Debugging

Set `RUST_LOG=debug` in a `.env` file at the repository root and the app logs
to `dfox-debug.log`. Press `F12` in the app to toggle the debug panel.

## How pull requests are merged

`main` is protected, and the rules are enforced by GitHub rather than by
convention:

- **Every change goes through a pull request.** Nobody pushes to `main`
  directly, including the maintainer.
- **The maintainer must approve it.** [`CODEOWNERS`](.github/CODEOWNERS) makes
  @0xataru the required reviewer for every file.
- **Pushing after approval resets it.** A new commit dismisses the approval, so
  what gets merged is exactly what was reviewed.
- **CI must pass:** rustfmt, clippy and tests on Linux, macOS and Windows.
- **The branch must be up to date with `main`.** If `main` moved on, rebase or
  use the *Update branch* button.
- **Every review thread must be resolved** before merging.
- **History is linear.** Pull requests are squash- or rebase-merged.
- **Release tags are immutable.** Once a `vX.Y.Z` tag is pushed it cannot be
  moved or deleted.

## Releasing (maintainer)

The version lives in one place, `[workspace.package]` in the root
`Cargo.toml`. Releasing takes two commands, separated by a merged pull request.

1. **Open the release pull request.** `[Unreleased]` in the changelog must have
   entries, or the bump refuses:

   ```sh
   git switch main && git pull
   make release VERSION=0.2.0
   ```

   This runs `make check`, creates `release/v0.2.0`, bumps `Cargo.toml`,
   `Cargo.lock` and `CHANGELOG.md`, commits
   `chore: release v0.2.0`, pushes it and opens the pull request with the
   changelog section as its description.

2. **Wait for CI and merge it.**

3. **Publish:**

   ```sh
   git switch main && git pull
   make publish
   ```

   This runs `make check` again, then tags `main` as `v0.2.0` and pushes the
   tag. The [release workflow](.github/workflows/release.yml) builds `dfox-tui`
   for Linux, macOS (Intel and Apple silicon) and Windows, and publishes the
   GitHub release with the changelog section as its notes.

`make bump VERSION=X.Y.Z` runs only the file edits from step 1, if you want to
do the rest by hand.
