# Baseline

The numbers the native drivers in C2 and C3 must beat. Recorded on
2026-10-14, on an Apple M3 laptop with Rust 1.97.1, against
PostgreSQL 16.15 and MySQL 8.4.11 in Docker on the same machine.

Run them with `make bench` (Postgres and MySQL need `make test-it`'s
databases). Expect ±10% between runs on a laptop.

## Reading a million rows

A stored table of a million rows (an integer, a text, an exact decimal, a
timestamp and a boolean), read with `SELECT *`. sqlx drivers decode into
cells; the native sessions decode into Arrow batches.

| Database | Rows/s | Whole table | First rows |
|---|---:|---:|---:|
| SQLite (in memory), sqlx | 0.68 M | 1,472 ms | 1.6 ms |
| PostgreSQL, sqlx | 1.77 M | 564 ms | 7.9 ms |
| **PostgreSQL, native** | **3.20 M** | **313 ms** | **3.7 ms** |
| MySQL, sqlx | 1.45 M | 692 ms | 10.7 ms |
| **MySQL, native** | **2.78 M** | **360 ms** | **3.6 ms** |

The native Postgres session reads the table **1.80×** faster than sqlx, with
the first rows in half the time, so C2's ≥ 1.5× target is met. Postgres
itself takes about 172 ms to send this table (`COPY … TO '/dev/null'`), so
the native client's own work is about 140 ms of the 313.

On a GitHub Actions runner (Linux, Postgres as a service container on the
same 4-vCPU machine) the same benchmark gives sqlx 826 ms and native 668 ms:
**1.24×**. The server and the client share those cores there, and the
server's share of the time is larger, so the client's lead shrinks. Judge
drivers on the laptop numbers above and use CI only to catch regressions.

An earlier version of this benchmark generated the rows in the query
(`generate_series` with arithmetic). That made the server's work dominate:
about 340 ms just to receive the rows with no client at all. Every client
looked alike, 1.09× on macOS and 1.04× on Linux CI. A stored table is what
people actually read.

The native MySQL session (C3, recorded 2026-10-15 on the same machine) reads
the table **1.85×** faster than sqlx (667 ms in the same run), with the first
rows in a third of the time. It reads rows over the text protocol, as the
`mysql` client does.

Target still ahead: native SQLite at ≥ 2× the sqlx rows/s (about 1.4 M
rows/s), in C3.

## Memory for a million rows

An integer, a float and two texts per row; every allocation counted.

| Storage | Memory |
|---|---:|
| Cells (`RowStore`) | 137.9 MB |
| Arrow batches (`ArrowResultSet`) | 53.6 MB |

Arrow takes 2.6× less. Ten million integers alone take 80 MB in Arrow (a test
in `dfox-core`).

## Drawing the grid

200×60 cells, scrolled to the end of a million rows of 20 columns. Budget:
2 ms a frame.

| Storage | Frame |
|---|---:|
| Cells | 323 µs |
| Arrow | 341 µs |
