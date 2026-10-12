# Security policy

## Supported versions

Only the latest release receives fixes. Before 1.0.0 there are no backports:
upgrade to the newest `v0.x` to get a fix.

## Reporting a vulnerability

Please **do not open a public issue.** Report it privately through
[GitHub's vulnerability reporting](https://github.com/0xataru/dfox/security/advisories/new)
instead.

Include what you can of:

- the version or commit you tested, your OS and the database server and version
- steps to reproduce the problem
- what an attacker gains

For a database client, the issues we care most about are:

- credentials or query results leaking to logs (`dfox-debug.log`), the
  clipboard or the terminal scrollback when they should not
- connections made without TLS when TLS was expected
- input from a database server (names, values, error messages) that can
  break or take over the terminal, for example through escape sequences

You should get a first response within a week. Once a fix is released, the
advisory is published with credit to you, unless you prefer otherwise.

## Debug logs

`RUST_LOG=debug` and `trace` write detailed information to `dfox-debug.log`,
which can include queries and the data they return. Logging is off by default.
Review a log before attaching it to an issue.
