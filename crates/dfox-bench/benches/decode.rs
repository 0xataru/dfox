//! Streaming a million rows (an integer, a text and a float) from each
//! database: rows per second, and how soon the first rows arrive.
//!
//! SQLite always runs, on an in-memory database. Postgres and MySQL run when
//! `DFOX_TEST_POSTGRES_URL` / `DFOX_TEST_MYSQL_URL` are set.

use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use dfox_bench::{MYSQL_ROWS, POSTGRES_ROWS, ROWS, SQLITE_ROWS};
use dfox_core::db::{Chunk, DbClient};
use dfox_driver_mysql::MySqlClient;
use dfox_driver_postgres::PostgresClient;
use dfox_driver_sqlite::SqliteClient;
use tokio::{runtime::Runtime, sync::mpsc};

/// Streams `sql` once. Returns when the first rows arrived and when the
/// last did.
async fn stream(client: &dyn DbClient, sql: &str) -> (Duration, Duration) {
    let (tx, mut rx) = mpsc::channel(16);
    let started = Instant::now();
    let read = async {
        let mut first = None;
        let mut rows = 0;
        while let Some(chunk) = rx.recv().await {
            if let Chunk::Rows(batch) = chunk {
                first.get_or_insert_with(|| started.elapsed());
                rows += batch.len();
            }
        }
        (first.unwrap_or_default(), rows)
    };
    let (result, (first, rows)) = tokio::join!(client.stream(sql, usize::MAX, tx), read);
    if let Err(err) = result {
        panic!("{sql}: {err}");
    }
    assert_eq!(rows, ROWS, "{sql}");
    (first, started.elapsed())
}

fn bench(c: &mut Criterion, rt: &Runtime, name: &str, client: &dyn DbClient, sql: &str) {
    let mut group = c.benchmark_group(format!("decode/{name}"));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.throughput(Throughput::Elements(ROWS as u64));
    group.bench_function("1M rows", |b| {
        b.iter_custom(|iters| (0..iters).map(|_| rt.block_on(stream(client, sql)).1).sum())
    });
    group.finish();

    let mut group = c.benchmark_group(format!("first_row/{name}"));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.bench_function("1M rows", |b| {
        b.iter_custom(|iters| (0..iters).map(|_| rt.block_on(stream(client, sql)).0).sum())
    });
    group.finish();
}

fn decode(c: &mut Criterion) {
    let rt = Runtime::new().expect("a tokio runtime");
    // Clients close their connections on the runtime when dropped.
    let _runtime = rt.enter();
    let sqlite = rt
        .block_on(SqliteClient::connect("sqlite::memory:"))
        .expect("an in-memory SQLite database");
    bench(c, &rt, "sqlite", &sqlite, SQLITE_ROWS);

    if let Ok(url) = std::env::var("DFOX_TEST_POSTGRES_URL") {
        let client = rt
            .block_on(PostgresClient::connect(&url))
            .expect("Postgres");
        bench(c, &rt, "postgres", &client, POSTGRES_ROWS);
    }
    if let Ok(url) = std::env::var("DFOX_TEST_MYSQL_URL") {
        let client = rt.block_on(MySqlClient::connect(&url)).expect("MySQL");
        bench(c, &rt, "mysql", &client, MYSQL_ROWS);
    }
}

criterion_group!(benches, decode);
criterion_main!(benches);
