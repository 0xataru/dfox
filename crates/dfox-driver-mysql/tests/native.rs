//! The native session against a real server: the driver test suite, every
//! type the decoder knows, scripts, warnings, errors, read-only connections,
//! TLS, cancelling and a large catalog.

use std::time::{Duration, Instant};

use dfox_core::{
    arrow::{FetchStatus, Fetcher, Overflow, DEFAULT_BUDGET},
    catalog::NodePath,
    driver::Driver,
    errors::DbError,
    models::connections::{ConnectionParams, TlsMode},
    result::ResultSet,
    session::{Request, Session},
};
use dfox_driver_mysql::MySqlDriver;
use dfox_testkit::{
    conformance::{check, Fixture},
    params_from_url, server_url, unique,
};

const URL: &str = "DFOX_TEST_MYSQL_URL";

async fn session(params: &ConnectionParams) -> Box<dyn Session> {
    MySqlDriver.open(params).await.expect("open a session")
}

/// Runs `sql` and returns the last result as text, and the notices.
async fn run(session: &mut dyn Session, sql: &str) -> (Vec<Vec<String>>, Vec<String>) {
    let stream = session.run(Request::new(sql)).await.expect("run");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    let status = fetcher
        .fetch()
        .await
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
    assert_eq!(status, FetchStatus::Done);
    let notices = fetcher.notices().to_vec();
    let Some(result) = fetcher.result() else {
        return (Vec::new(), notices);
    };
    let rows = (0..result.row_count())
        .map(|row| {
            (0..result.columns().len())
                .map(|col| {
                    result
                        .cell(row, col)
                        .map(|c| c.to_string())
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();
    (rows, notices)
}

/// Runs `sql` and returns the error it ends with.
async fn fails(session: &mut dyn Session, sql: &str) -> DbError {
    let stream = session.run(Request::new(sql)).await.expect("run");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    fetcher.fetch().await.expect_err("an error")
}

/// A database of its own, so tests don't meet each other's tables.
async fn database(params: &ConnectionParams) -> (String, ConnectionParams) {
    let name = unique("dfox_native");
    let mut admin = session(params).await;
    run(admin.as_mut(), &format!("CREATE DATABASE {name}")).await;
    let params = ConnectionParams {
        database: Some(name.clone()),
        ..params.clone()
    };
    (name, params)
}

async fn drop_database(params: &ConnectionParams, name: &str) {
    let mut admin = session(params).await;
    run(admin.as_mut(), &format!("DROP DATABASE {name}")).await;
}

#[tokio::test]
async fn the_native_session_passes_the_driver_suite() {
    let Some(url) = server_url(URL) else { return };
    let base = params_from_url(&url);
    let (name, params) = database(&base).await;
    let mut setup = session(&params).await;
    run(
        setup.as_mut(),
        "CREATE TABLE people (id INT PRIMARY KEY, name TEXT);
         INSERT INTO people VALUES (1, 'a'), (2, 'b'), (3, 'c')",
    )
    .await;

    let fixture = Fixture {
        table: [name.as_str(), "people"].into_iter().collect::<NodePath>(),
        select_all: "SELECT * FROM people ORDER BY id".into(),
        count: "SELECT count(*) FROM people".into(),
        // Recursion stops at 1,000 by default; a thousand squared is enough.
        generate: |n| {
            format!(
                "WITH RECURSIVE t(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM t WHERE n < 999) \
                 SELECT a.n * 1000 + b.n + 1 AS i FROM t AS a, t AS b LIMIT {n}"
            )
        },
        long_running: "SELECT SLEEP(10)".into(),
        insert: |id, name| format!("INSERT INTO people VALUES ({id}, '{name}')"),
        begin: "START TRANSACTION".into(),
        rollback: "ROLLBACK".into(),
        types: (
            "DROP TEMPORARY TABLE IF EXISTS types_t;
             CREATE TEMPORARY TABLE types_t (
                ti TINYINT, si SMALLINT, mi MEDIUMINT, i INT, bi BIGINT,
                tiu TINYINT UNSIGNED, biu BIGINT UNSIGNED, b BOOLEAN,
                d DECIMAL(10,2), f FLOAT, db DOUBLE,
                c CHAR(3), vc VARCHAR(10), t TEXT, bin VARBINARY(4), bl BLOB,
                dt DATE, tm TIME, dtm DATETIME(3), ts TIMESTAMP NULL, y YEAR,
                bt BIT(8), e ENUM('a','b'), st SET('x','y'), j JSON, n INT
             );
             INSERT INTO types_t VALUES (
                -5, -300, 70000, -2000000000, 9000000000000000000,
                255, 18446744073709551615, TRUE,
                1.5, 1.25, 2.5,
                'c', 'v', 'txt', 0xDEADBEEF, 'hi',
                '2026-10-12', '-838:59:59', '2026-10-12 13:14:15.5', '2026-10-12 13:14:15',
                2026, b'10100101', 'b', 'x,y', '{\"a\":1}', NULL
             );
             SELECT * FROM types_t"
                .into(),
            vec![
                "-5",
                "-300",
                "70000",
                "-2000000000",
                "9000000000000000000",
                "255",
                "18446744073709551615",
                "true",
                "1.50",
                "1.25",
                "2.5",
                "c",
                "v",
                "txt",
                "0xdeadbeef",
                "0x6869",
                "2026-10-12",
                "-838:59:59",
                "2026-10-12 13:14:15.500",
                "2026-10-12 13:14:15",
                "2026",
                "0xa5",
                "b",
                "x,y",
                r#"{"a": 1}"#,
                "NULL",
            ],
        ),
        stream_rows: 1_000_000,
    };
    check(&MySqlDriver, &params, &fixture).await;

    drop(setup);
    drop_database(&base, &name).await;
}

#[tokio::test]
async fn browsing_reads_types_over_the_binary_protocol() {
    use dfox_core::catalog::BrowseQuery;

    let Some(url) = server_url(URL) else { return };
    let base = params_from_url(&url);
    let (name, params) = database(&base).await;
    let mut s = session(&params).await;
    run(
        s.as_mut(),
        "CREATE TABLE t (id INT UNSIGNED PRIMARY KEY, f FLOAT, d DECIMAL(6,2), \
                         at DATETIME(6), span TIME, day DATE, flag BOOLEAN, raw BLOB);
         INSERT INTO t VALUES (1, 1.25, 3.10, '2026-10-12 13:14:15.000001', '-01:02:03', \
                               '2026-10-12', FALSE, 0x00ff)",
    )
    .await;
    let path: NodePath = [name.as_str(), "t"].into_iter().collect();
    let stream = s
        .catalog()
        .browse(&path, BrowseQuery::default())
        .await
        .expect("browse");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    fetcher.fetch().await.expect("rows");
    let result = fetcher.result().expect("a result");
    let row: Vec<String> = (0..result.columns().len())
        .map(|col| result.cell(0, col).expect("cell").to_string())
        .collect();
    assert_eq!(
        row,
        [
            "1",
            "1.25",
            "3.10",
            "2026-10-12 13:14:15.000001",
            "-01:02:03",
            "2026-10-12",
            "false",
            "0x00ff"
        ]
    );
    drop(s);
    drop_database(&base, &name).await;
}

#[tokio::test]
async fn scripts_run_every_statement_and_report_warnings() {
    let Some(url) = server_url(URL) else { return };
    let mut s = session(&params_from_url(&url)).await;
    let (rows, notices) = run(
        s.as_mut(),
        "-- a comment with a ; in it
         CREATE TEMPORARY TABLE t (n TINYINT);
         INSERT INTO t VALUES (1), (2);
         SELECT n FROM t ORDER BY n; SELECT CAST('12abc' AS SIGNED) AS loose",
    )
    .await;
    assert_eq!(rows, [["12"]], "the last result set wins");
    // The server keeps the last statement's warnings.
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].starts_with("Warning 1292:"), "{notices:?}");
    // Comments alone run nothing.
    assert_eq!(
        run(s.as_mut(), "-- nothing").await.0,
        Vec::<Vec<String>>::new()
    );
    // The temporary table lives on in the session.
    assert_eq!(run(s.as_mut(), "SELECT count(*) FROM t").await.0, [["2"]]);
}

#[tokio::test]
async fn procedures_return_every_result_set() {
    let Some(url) = server_url(URL) else { return };
    let base = params_from_url(&url);
    let (name, params) = database(&base).await;
    let mut s = session(&params).await;
    run(
        s.as_mut(),
        "CREATE PROCEDURE two() BEGIN SELECT 1 AS first; SELECT 2 AS second; END",
    )
    .await;
    let stream = s.run(Request::new("CALL two()")).await.expect("run");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    fetcher.fetch().await.expect("call");
    let result = fetcher.result().expect("a result");
    assert_eq!(result.columns()[0].name, "second");
    // The session is ready for more.
    assert_eq!(run(s.as_mut(), "SELECT 3").await.0, [["3"]]);
    drop(s);
    drop_database(&base, &name).await;
}

#[tokio::test]
async fn errors_read_like_the_mysql_client_prints_them() {
    let Some(url) = server_url(URL) else { return };
    let mut s = session(&params_from_url(&url)).await;
    let err = fails(s.as_mut(), "SELECT * FROM nope_not_here").await;
    let message = err.to_string();
    assert!(message.contains("ERROR 1146 (42S02)"), "{message}");
    assert!(
        message.contains("nope_not_here' doesn't exist"),
        "{message}"
    );
    // The session is still usable.
    assert_eq!(run(s.as_mut(), "SELECT 1").await.0, [["1"]]);
}

#[tokio::test]
async fn read_only_sessions_refuse_writes() {
    let Some(url) = server_url(URL) else { return };
    let base = params_from_url(&url);
    let (name, params) = database(&base).await;
    let mut setup = session(&params).await;
    run(setup.as_mut(), "CREATE TABLE t (n INT)").await;
    let mut s = session(&ConnectionParams {
        read_only: true,
        ..params
    })
    .await;
    let err = fails(s.as_mut(), "INSERT INTO t VALUES (1)").await;
    assert!(err.to_string().contains("READ ONLY"), "{err}");
    drop((s, setup));
    drop_database(&base, &name).await;
}

/// MySQL 8 makes itself a certificate nobody signed: `require` encrypts
/// without checking it, `verify-full` refuses it, `disable` doesn't encrypt.
#[tokio::test]
async fn tls_modes_encrypt_and_verify() {
    let Some(url) = server_url(URL) else { return };
    let base = params_from_url(&url);
    let with = |tls| ConnectionParams {
        tls,
        ..base.clone()
    };
    let cipher = "SHOW SESSION STATUS LIKE 'Ssl_cipher'";

    for tls in [TlsMode::Prefer, TlsMode::Require] {
        let mut s = session(&with(tls)).await;
        let rows = run(s.as_mut(), cipher).await.0;
        assert!(!rows[0][1].is_empty(), "{tls:?} encrypts: {rows:?}");
    }
    let mut s = session(&with(TlsMode::Disable)).await;
    assert_eq!(run(s.as_mut(), cipher).await.0[0][1], "");

    let err = MySqlDriver
        .open(&with(TlsMode::VerifyFull))
        .await
        .err()
        .expect("refused");
    assert!(err.to_string().contains("UnknownIssuer"), "{err}");
}

/// The plan's target: a running query stops within 300 ms of the cancel.
#[tokio::test]
async fn cancel_stops_a_query_quickly() {
    use futures_util::StreamExt;

    let Some(url) = server_url(URL) else { return };
    let mut s = session(&params_from_url(&url)).await;
    let handle = s.cancel_handle();
    let mut frames = s.run(Request::new("SELECT SLEEP(30)")).await.expect("run");
    let cancelled = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let asked = Instant::now();
        handle.cancel().await.expect("cancel");
        asked
    });
    let mut last = None;
    while let Some(frame) = frames.next().await {
        last = Some(frame);
    }
    let took = cancelled.await.expect("join").elapsed();
    assert!(matches!(last, Some(Err(DbError::Cancelled))), "{last:?}");
    eprintln!("cancelled in {took:?}");
    assert!(took < Duration::from_millis(300), "{took:?}");
    drop(frames);
    // A cancel only stops what was running.
    assert_eq!(run(s.as_mut(), "SELECT 'ok'").await.0, [["ok"]]);
}

/// The plan's target: a catalog walk of 5,000 tables in under 300 ms.
#[tokio::test]
async fn a_large_database_lists_quickly() {
    let Some(url) = server_url(URL) else { return };
    let base = params_from_url(&url);
    let (name, params) = database(&base).await;
    let mut s = session(&params).await;
    let created = Instant::now();
    for start in (1..=5000).step_by(500) {
        let script: Vec<String> = (start..start + 500)
            .map(|i| format!("CREATE TABLE t{i} (id INT PRIMARY KEY, name TEXT)"))
            .collect();
        run(s.as_mut(), &script.join(";\n")).await;
    }
    eprintln!("creating 5000 tables took {:?}", created.elapsed());
    let path = NodePath::root().child(name.clone());
    let started = Instant::now();
    let tables = s.catalog().children(&path).await.expect("children");
    let took = started.elapsed();
    assert_eq!(tables.len(), 5000);
    eprintln!("listing 5000 tables took {took:?}");
    assert!(took < Duration::from_millis(300), "{took:?}");
    drop(s);
    drop_database(&base, &name).await;
}
