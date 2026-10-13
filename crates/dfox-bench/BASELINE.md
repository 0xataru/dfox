# Baseline

The numbers the native drivers in C2 and C3 must beat. Recorded on 2026-10-13
for v0.2.0's sqlx drivers, on an Apple M3 laptop with Rust 1.97.1, against
PostgreSQL 16.15 and MySQL 8.4.11 in Docker on the same machine.

Run them with `make bench` (Postgres and MySQL need `make test-it`'s
databases). Expect ±10% between runs on a laptop.

## Decoding a million rows

One integer, one text and one float column, streamed through the driver into
cells.

| Database | Rows/s | Whole result | First rows |
|---|---:|---:|---:|
| SQLite (in memory) | 1.07 M | 936 ms | 1.6 ms |
| PostgreSQL | 1.97 M | 508 ms | 45.8 ms |
| MySQL | 1.94 M | 517 ms | 7.8 ms |

Postgres's first rows wait on the server: `generate_series` builds its whole
result before sending any of it.

Targets from the plan: the native Postgres driver at ≥ 1.5× these rows/s
(about 3 M rows/s), the native SQLite driver at ≥ 2× (about 2.1 M rows/s), and
native MySQL faster than above.

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
