//! Reading a stored table of a million rows (an integer, a text, an exact
//! decimal, a timestamp and a boolean) from each database: rows per second,
//! and how soon the first rows arrive. The table is made before measuring.
//!
//! SQLite always runs, on an in-memory database. Postgres and MySQL run when
//! `DFOX_TEST_POSTGRES_URL` / `DFOX_TEST_MYSQL_URL` are set.
//!
//! The groups keep their `-native` names from when the sqlx drivers ran
//! beside them, so pull requests still compare against `main`.

use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use dfox_bench::{MYSQL_SETUP, POSTGRES_SETUP, ROWS, SELECT_ROWS, SQLITE_SETUP};
use dfox_core::{
    arrow::{Fetcher, Frame, Overflow, DEFAULT_BUDGET},
    driver::Driver,
    models::connections::ConnectionParams,
    result::ResultSet,
    session::{Request, Session},
};
use futures_util::StreamExt;
use tokio::runtime::Runtime;

/// Streams `sql` once through a native session into an Arrow result.
async fn stream_native(session: &mut dyn Session, sql: &str) -> Duration {
    let started = Instant::now();
    let frames = session.run(Request::new(sql)).await.expect("run");
    let mut fetcher = Fetcher::new(frames, DEFAULT_BUDGET, Overflow::Stop);
    fetcher.fetch().await.expect("fetch");
    assert_eq!(fetcher.result().map_or(0, |r| r.row_count()), ROWS, "{sql}");
    started.elapsed()
}

/// How soon the first batch of `sql` arrives through a native session. The
/// rest is read too, so the next run starts clean.
async fn first_native(session: &mut dyn Session, sql: &str) -> Duration {
    let started = Instant::now();
    let mut frames = session.run(Request::new(sql)).await.expect("run");
    let mut first = None;
    while let Some(frame) = frames.next().await {
        if let Frame::Batch(_) = frame.expect("frame") {
            first.get_or_insert_with(|| started.elapsed());
        }
    }
    first.unwrap_or_default()
}

fn bench_native(c: &mut Criterion, rt: &Runtime, name: &str, session: &mut dyn Session, sql: &str) {
    let mut group = c.benchmark_group(format!("decode/{name}"));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.throughput(Throughput::Elements(ROWS as u64));
    group.bench_function("1M table rows", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| rt.block_on(stream_native(&mut *session, sql)))
                .sum()
        })
    });
    group.finish();

    let mut group = c.benchmark_group(format!("first_row/{name}"));
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));
    group.bench_function("1M table rows", |b| {
        b.iter_custom(|iters| {
            (0..iters)
                .map(|_| rt.block_on(first_native(&mut *session, sql)))
                .sum()
        })
    });
    group.finish();
}

/// Opens a session with `driver` and fills the benchmark table through it,
/// one statement at a time.
fn prepared(
    rt: &Runtime,
    driver: &dyn Driver,
    params: &ConnectionParams,
    statements: &[&str],
) -> Box<dyn Session> {
    let mut session = rt
        .block_on(driver.open(params))
        .unwrap_or_else(|err| panic!("{}: {err}", driver.info().name));
    for sql in statements {
        rt.block_on(async {
            let mut frames = session.run(Request::new(*sql)).await.expect("setup");
            while let Some(frame) = frames.next().await {
                frame.unwrap_or_else(|err| panic!("{sql}: {err}"));
            }
        });
    }
    session
}

fn decode(c: &mut Criterion) {
    let rt = Runtime::new().expect("a tokio runtime");
    // Sessions close their connections on the runtime when dropped.
    let _runtime = rt.enter();
    let memory = ConnectionParams {
        database: Some(":memory:".into()),
        ..Default::default()
    };
    let mut sqlite = prepared(
        &rt,
        &dfox_driver_sqlite::SqliteDriver,
        &memory,
        SQLITE_SETUP,
    );
    bench_native(c, &rt, "sqlite-native", sqlite.as_mut(), SELECT_ROWS);

    if let Ok(url) = std::env::var("DFOX_TEST_POSTGRES_URL") {
        let params = dfox_testkit::params_from_url(&url);
        let driver = dfox_driver_postgres::PostgresDriver;
        let mut session = prepared(&rt, &driver, &params, POSTGRES_SETUP);
        bench_native(c, &rt, "postgres-native", session.as_mut(), SELECT_ROWS);
    }
    if let Ok(url) = std::env::var("DFOX_TEST_MYSQL_URL") {
        let params = dfox_testkit::params_from_url(&url);
        let driver = dfox_driver_mysql::MySqlDriver;
        let mut session = prepared(&rt, &driver, &params, MYSQL_SETUP);
        bench_native(c, &rt, "mysql-native", session.as_mut(), SELECT_ROWS);
    }
}

criterion_group!(benches, decode);
criterion_main!(benches);
