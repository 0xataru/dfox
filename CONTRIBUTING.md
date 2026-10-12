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

DFox is a Cargo workspace with two crates:

- **`dfox-core`**: database access. A `DbClient` trait with PostgreSQL, MySQL
  and SQLite implementations on top of `sqlx`, plus the type mapping from rows
  to JSON values.
- **`dfox-tui`**: the terminal interface, built with `ratatui` and `crossterm`.
  Screens, components and key handlers live under `src/ui/`.

New database support goes in `dfox-core` behind the `DbClient` trait. The TUI
should not need to know which backend it is talking to.

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
   terminal, for example type mapping in `dfox-core`.

3. **Run what CI runs:**

   ```sh
   make fmt     # format the workspace
   make check   # rustfmt check, clippy with -D warnings, tests: exactly what CI runs
   ```

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
| `make fmt` | Format the workspace with rustfmt |
| `make lint` | clippy with warnings as errors |
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
   `Cargo.lock`, `CHANGELOG.md` and the README badge, commits
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
