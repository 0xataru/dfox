//! The native session on real SQLite files: the driver test suite, types
//! that widen as values arrive, scripts with triggers, errors, read-only
//! files, cancelling and the catalog.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use dfox_core::{
    arrow::{FetchStatus, Fetcher, Frame, Overflow, DEFAULT_BUDGET},
    catalog::{NodeKind, NodePath},
    driver::Driver,
    errors::DbError,
    models::connections::ConnectionParams,
    result::{Cell, ResultSet},
    session::{Request, Session},
};
use dfox_driver_sqlite::SqliteDriver;
use dfox_testkit::{
    conformance::{check, Fixture},
    unique,
};
use futures_util::StreamExt;

/// A new, empty database file, removed when dropped.
struct File(PathBuf);

impl File {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("{}.db", unique("dfox_native")));
        std::fs::File::create(&path).expect("create the file");
        Self(path)
    }

    fn params(&self) -> ConnectionParams {
        ConnectionParams {
            database: Some(self.0.to_string_lossy().into_owned()),
            ..Default::default()
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn session(params: &ConnectionParams) -> Box<dyn Session> {
    SqliteDriver.open(params).await.expect("open a session")
}

/// Runs `sql` and returns the last result as text.
async fn run(session: &mut dyn Session, sql: &str) -> Vec<Vec<String>> {
    let stream = session.run(Request::new(sql)).await.expect("run");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    let status = fetcher
        .fetch()
        .await
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
    assert_eq!(status, FetchStatus::Done);
    let Some(result) = fetcher.result() else {
        return Vec::new();
    };
    (0..result.row_count())
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
        .collect()
}

async fn fails(session: &mut dyn Session, sql: &str) -> DbError {
    let stream = session.run(Request::new(sql)).await.expect("run");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    fetcher.fetch().await.expect_err("an error")
}

#[tokio::test]
async fn the_native_session_passes_the_driver_suite() {
    let file = File::new();
    let params = file.params();
    let mut setup = session(&params).await;
    run(
        setup.as_mut(),
        "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT);
         INSERT INTO people VALUES (1, 'a'), (2, 'b'), (3, 'c')",
    )
    .await;
    drop(setup);

    let fixture = Fixture {
        table: ["main", "people"].into_iter().collect::<NodePath>(),
        select_all: "SELECT * FROM people ORDER BY id".into(),
        count: "SELECT count(*) FROM people".into(),
        generate: |n| {
            format!(
                "WITH RECURSIVE t(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM t WHERE i < {n}) \
                 SELECT i FROM t"
            )
        },
        // Counts forever; only a cancel ends it.
        long_running: "WITH RECURSIVE t(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM t) \
                       SELECT count(*) FROM t"
            .into(),
        insert: |id, name| format!("INSERT INTO people VALUES ({id}, '{name}')"),
        begin: "BEGIN".into(),
        rollback: "ROLLBACK".into(),
        types: (
            "DROP TABLE IF EXISTS temp.types_t;
             CREATE TEMP TABLE types_t (
                i INTEGER, r REAL, n NUMERIC, t TEXT, b BLOB, flag BOOLEAN, day DATE, untyped
             );
             INSERT INTO types_t VALUES (1, 2.5, 1.5, 'txt', X'DEADBEEF', 1, '2026-10-12', NULL);
             SELECT * FROM types_t"
                .into(),
            vec![
                "1",
                "2.5",
                "1.5",
                "txt",
                "0xdeadbeef",
                "true",
                "2026-10-12",
                "NULL",
            ],
        ),
        stream_rows: 1_000_000,
    };
    check(&SqliteDriver, &params, &fixture).await;
}

/// Twenty thousand rows, so several batches, whose first column turns real
/// and then text late.
const MIXED: &str =
    "WITH RECURSIVE t(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM t WHERE i < 20000) \
     SELECT CASE WHEN i = 20000 THEN 'last' WHEN i = 15000 THEN 0.5 ELSE i END AS v, \
            CASE WHEN i = 1 THEN NULL ELSE 9007199254740993 END AS big \
     FROM t";

/// A column whose values change type after batches went out: the schema
/// goes out again with every row widened, and nothing is lost.
#[tokio::test]
async fn mixed_columns_widen_without_losing_values() {
    let file = File::new();
    let mut s = session(&file.params()).await;
    let stream = s.run(Request::new(MIXED)).await.expect("run");
    let frames: Vec<Frame> = stream.map(|frame| frame.expect("a frame")).collect().await;
    let schemas = frames
        .iter()
        .filter(|f| matches!(f, Frame::Schema(_)))
        .count();
    assert!(schemas > 1, "the schema went out again: {schemas}");

    let stream = s.run(Request::new(MIXED)).await.expect("run");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    fetcher.fetch().await.expect("rows");
    let result = fetcher.result().expect("a result");
    assert_eq!(result.row_count(), 20_000);
    let text = |row, col| result.cell(row, col).expect("cell").to_string();
    assert_eq!(text(0, 0), "1");
    assert_eq!(text(14_999, 0), "0.5");
    assert_eq!(text(19_999, 0), "last");
    // An integer past 2^53 stays exact, and a NULL first doesn't fix the type.
    assert!(matches!(result.cell(0, 1), Some(Cell::Null)));
    assert!(matches!(
        result.cell(1, 1),
        Some(Cell::Int(9_007_199_254_740_993))
    ));
}

#[tokio::test]
async fn booleans_are_zero_and_one_in_boolean_columns() {
    let file = File::new();
    let mut s = session(&file.params()).await;
    let rows = run(
        s.as_mut(),
        "CREATE TABLE f (flag BOOLEAN, n INTEGER);
         INSERT INTO f VALUES (1, 1), (0, 0), (NULL, NULL);
         SELECT * FROM f",
    )
    .await;
    assert_eq!(rows, [["true", "1"], ["false", "0"], ["NULL", "NULL"]]);
    // A 2 in a boolean column is no boolean: the column becomes integers.
    let rows = run(
        s.as_mut(),
        "INSERT INTO f VALUES (2, 2); SELECT flag FROM f",
    )
    .await;
    assert_eq!(rows, [["1"], ["0"], ["NULL"], ["2"]]);
}

#[tokio::test]
async fn scripts_keep_trigger_bodies_whole() {
    let file = File::new();
    let mut s = session(&file.params()).await;
    let rows = run(
        s.as_mut(),
        "CREATE TABLE t (n INTEGER); CREATE TABLE log (n INTEGER);
         CREATE TRIGGER t_log AFTER INSERT ON t BEGIN
           INSERT INTO log VALUES (new.n);
           INSERT INTO log VALUES (new.n * 10);
         END;
         INSERT INTO t VALUES (1), (2);
         -- a comment; with a semicolon
         SELECT n FROM log ORDER BY n",
    )
    .await;
    assert_eq!(rows, [["1"], ["2"], ["10"], ["20"]]);
    // Temporary tables live on in the session.
    run(s.as_mut(), "CREATE TEMP TABLE keep (x)").await;
    assert_eq!(run(s.as_mut(), "SELECT count(*) FROM keep").await, [["0"]]);
}

#[tokio::test]
async fn errors_read_like_sqlite_wrote_them() {
    let file = File::new();
    let mut s = session(&file.params()).await;
    let err = fails(s.as_mut(), "SELECT * FROM nope_not_here").await;
    assert_eq!(err.to_string(), "Error: no such table: nope_not_here");
    assert_eq!(run(s.as_mut(), "SELECT 1").await, [["1"]]);
}

#[tokio::test]
async fn missing_and_read_only_files() {
    let missing = std::env::temp_dir().join(format!("{}.db", unique("dfox_missing")));
    let err = SqliteDriver
        .open(&ConnectionParams {
            database: Some(missing.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .await
        .err()
        .expect("a missing file is an error");
    assert!(err.to_string().contains("unable to open"), "{err}");
    assert!(!missing.exists(), "no empty file is made");

    let file = File::new();
    let mut setup = session(&file.params()).await;
    run(setup.as_mut(), "CREATE TABLE t (n INTEGER)").await;
    drop(setup);
    let mut s = session(&ConnectionParams {
        read_only: true,
        ..file.params()
    })
    .await;
    let err = fails(s.as_mut(), "INSERT INTO t VALUES (1)").await;
    assert!(err.to_string().contains("readonly"), "{err}");
}

/// The plan's target: a running query stops within 300 ms of the cancel.
#[tokio::test]
async fn cancel_interrupts_a_query_quickly() {
    let file = File::new();
    let mut s = session(&file.params()).await;
    let handle = s.cancel_handle();
    let mut frames = s
        .run(Request::new(
            "WITH RECURSIVE t(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM t) \
             SELECT count(*) FROM t",
        ))
        .await
        .expect("run");
    let cancelled = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let asked = Instant::now();
        assert!(handle.cancel().await.expect("cancel"));
        asked
    });
    let mut last = None;
    while let Some(frame) = frames.next().await {
        last = Some(frame);
    }
    let took = cancelled.await.expect("join").elapsed();
    assert!(matches!(last, Some(Err(DbError::Cancelled))), "{last:?}");
    eprintln!("interrupted in {took:?}");
    assert!(took < Duration::from_millis(300), "{took:?}");
    assert_eq!(run(s.as_mut(), "SELECT 'ok'").await, [["ok"]]);
}

#[tokio::test]
async fn the_catalog_lists_databases_tables_views_and_indexes() {
    let file = File::new();
    let mut s = session(&file.params()).await;
    run(
        s.as_mut(),
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT 'x');
         CREATE UNIQUE INDEX t_name ON t (name, id);
         CREATE VIEW v AS SELECT 1 AS one",
    )
    .await;
    let databases = s.catalog().children(&NodePath::root()).await.expect("root");
    assert_eq!(databases[0].name, "main");
    let objects = s
        .catalog()
        .children(&NodePath::root().child("main"))
        .await
        .expect("objects");
    let objects: Vec<(&str, NodeKind)> =
        objects.iter().map(|n| (n.name.as_str(), n.kind)).collect();
    assert_eq!(objects, [("t", NodeKind::Table), ("v", NodeKind::View)]);

    let table: NodePath = ["main", "t"].into_iter().collect();
    let info = s.catalog().describe(&table).await.expect("describe");
    assert!(info.columns[0].primary_key);
    assert!(!info.columns[1].nullable);
    assert_eq!(info.columns[1].default.as_deref(), Some("'x'"));
    let index = info
        .indexes
        .iter()
        .find(|i| i.name == "t_name")
        .expect("the index");
    assert!(index.unique);
    assert_eq!(index.columns, ["name", "id"]);

    let err = s
        .catalog()
        .describe(&["main", "nope"].into_iter().collect())
        .await
        .expect_err("no such table");
    assert!(err.to_string().contains("no table"), "{err}");
}

/// Edits from the grid work inside a transaction the user has open.
#[tokio::test]
async fn edits_apply_inside_an_open_transaction() {
    use dfox_core::{
        result::OwnedCell,
        session::{Change, ChangeSet},
    };

    let file = File::new();
    let mut s = session(&file.params()).await;
    run(
        s.as_mut(),
        "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT); BEGIN",
    )
    .await;
    let report = s
        .apply(ChangeSet {
            table: ["main", "t"].into_iter().collect(),
            key: vec!["id".into()],
            columns: vec!["id".into(), "name".into()],
            changes: vec![Change::Insert(vec![
                OwnedCell::Int(1),
                OwnedCell::Text("a".into()),
            ])],
        })
        .await
        .expect("apply");
    assert_eq!(report.inserted, 1);
    run(s.as_mut(), "ROLLBACK").await;
    assert_eq!(run(s.as_mut(), "SELECT count(*) FROM t").await, [["0"]]);
}
