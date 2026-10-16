use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use dfox_core::{
    arrow::ArrowResultSet,
    catalog::{NodeKind, NodePath},
    driver::Drivers,
    engine::{self, Command, EngineEvent, EngineHandle, QueryId, QueryStats},
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
    result::{Cell, ResultSet},
};
use dfox_testkit::{params_from_url, server_url, unique};
use tokio::sync::mpsc;

/// Every driver, as the app is built by default.
fn drivers() -> Drivers {
    Drivers::new(vec![
        Arc::new(dfox_driver_postgres::PostgresDriver),
        Arc::new(dfox_driver_mysql::MySqlDriver),
        Arc::new(dfox_driver_sqlite::SqliteDriver),
    ])
}

/// Drives the engine the way a UI would, with no UI.
struct Driver {
    engine: EngineHandle,
    events: mpsc::Receiver<EngineEvent>,
    next_id: QueryId,
}

/// Everything a query sent, in order.
struct Finished {
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    batches: usize,
    notices: Vec<String>,
    outcome: Result<QueryStats, DbError>,
}

impl Driver {
    fn new() -> Self {
        let (engine, events) = engine::spawn(drivers());
        Self {
            engine,
            events,
            next_id: 0,
        }
    }

    async fn connect(db_type: DbType, params: ConnectionParams) -> Self {
        let mut driver = Self::new();
        driver.engine.send(Command::Connect { db_type, params });
        match driver.next().await {
            EngineEvent::Connected(result) => drop(result.expect("connect")),
            other => panic!("expected Connected, got {other:?}"),
        }
        driver
    }

    async fn next(&mut self) -> EngineEvent {
        tokio::time::timeout(Duration::from_secs(10), self.events.recv())
            .await
            .expect("timed out waiting for the engine")
            .expect("the engine stopped")
    }

    fn start(&mut self, sql: &str, row_limit: usize) -> QueryId {
        self.next_id += 1;
        self.engine.send(Command::Run {
            id: self.next_id,
            sql: sql.to_string(),
            row_limit,
        });
        self.next_id
    }

    async fn finish(&mut self, id: QueryId) -> Finished {
        let mut result: Option<ArrowResultSet> = None;
        let mut batches = 0;
        let mut notices = Vec::new();
        let outcome = loop {
            match self.next().await {
                EngineEvent::Schema { id: got, schema } if got == id => {
                    result = Some(ArrowResultSet::new(schema));
                }
                EngineEvent::Batch { id: got, batch } if got == id => {
                    batches += 1;
                    result.as_mut().expect("a schema first").push(batch);
                }
                EngineEvent::Notice { id: got, message } if got == id => notices.push(message),
                EngineEvent::QueryDone { id: got, outcome } if got == id => break outcome,
                other => panic!("unexpected event {other:?}"),
            }
        };
        let (columns, rows) = match &result {
            Some(result) => (
                result.columns().iter().map(|c| c.name.clone()).collect(),
                (0..result.row_count())
                    .map(|row| {
                        (0..result.columns().len())
                            .map(|col| match result.cell(row, col) {
                                Some(Cell::Null) | None => None,
                                Some(cell) => Some(cell.to_string()),
                            })
                            .collect()
                    })
                    .collect(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        Finished {
            columns,
            rows,
            batches,
            notices,
            outcome,
        }
    }

    /// Asks for `command` and returns the first event.
    async fn ask(&mut self, command: Command) -> EngineEvent {
        self.engine.send(command);
        self.next().await
    }

    async fn query(&mut self, sql: &str, row_limit: usize) -> Finished {
        let id = self.start(sql, row_limit);
        self.finish(id).await
    }

    /// Cancels a query that has been running for a moment, and returns how
    /// long it took to stop.
    async fn cancel_after_start(&mut self, sql: &str) -> Duration {
        let id = self.start(sql, 10);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let asked = Instant::now();
        self.engine.send(Command::Cancel { id });
        let finished = self.finish(id).await;
        assert!(
            matches!(finished.outcome, Err(DbError::Cancelled)),
            "expected a cancelled query, got {:?}",
            finished.outcome
        );
        asked.elapsed()
    }

    async fn scalar(&mut self, sql: &str) -> Option<String> {
        let finished = self.query(sql, 10).await;
        finished.outcome.expect("query");
        assert_eq!(finished.rows.len(), 1);
        finished.rows[0][0].clone()
    }
}

/// A SQLite file that is removed when the test ends.
struct TempDb(std::path::PathBuf);

impl TempDb {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("{}.db", unique("dfox_engine")));
        std::fs::File::create(&path).expect("create the database file");
        Self(path)
    }

    fn params(&self) -> ConnectionParams {
        ConnectionParams {
            database: Some(self.0.to_string_lossy().into_owned()),
            ..Default::default()
        }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        // Windows may still hold the file open while the pool closes.
        let _ = std::fs::remove_file(&self.0);
    }
}

#[tokio::test]
async fn sqlite_rows_stream_in_batches() {
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;

    let finished = engine
        .query(
            "CREATE TABLE t (n INTEGER);
             INSERT INTO t WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 25000)
                 SELECT n FROM s;
             SELECT n, 'row ' || n FROM t ORDER BY n",
            100_000,
        )
        .await;

    let stats = finished.outcome.expect("query");
    assert_eq!(finished.columns.len(), 2);
    assert_eq!(finished.rows.len(), 25_000);
    assert_eq!(finished.rows[24_999][1].as_deref(), Some("row 25000"));
    assert!(finished.batches >= 3, "{} batches", finished.batches);
    assert_eq!(stats.rows, 25_000);
    assert_eq!(stats.rows_affected, 25_000);
    assert!(!stats.truncated);
    assert!(stats.first_row.is_some());
}

#[tokio::test]
async fn sqlite_row_limit_truncates() {
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;

    let finished = engine
        .query(
            "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 50) \
             SELECT n FROM s",
            10,
        )
        .await;

    let stats = finished.outcome.expect("query");
    assert_eq!(finished.rows.len(), 10);
    assert_eq!(stats.rows, 10);
    assert!(stats.truncated);
}

#[tokio::test]
async fn sqlite_catalog_and_errors() {
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;
    engine
        .query("CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT)", 0)
        .await
        .outcome
        .expect("create");

    match engine
        .ask(Command::Children {
            path: NodePath::root(),
        })
        .await
    {
        EngineEvent::Children { path, result } => {
            assert_eq!(path, NodePath::root());
            assert_eq!(result.expect("databases")[0].name, "main");
        }
        other => panic!("expected Children, got {other:?}"),
    }

    let main = NodePath::root().child("main");
    match engine.ask(Command::Children { path: main.clone() }).await {
        EngineEvent::Children { result, .. } => {
            let tables: Vec<_> = result
                .expect("tables")
                .into_iter()
                .map(|n| (n.name, n.kind))
                .collect();
            assert_eq!(tables, [("people".to_string(), NodeKind::Table)]);
        }
        other => panic!("expected Children, got {other:?}"),
    }

    match engine
        .ask(Command::Describe {
            path: main.child("people"),
        })
        .await
    {
        EngineEvent::Described { path, result } => {
            assert_eq!(path.name(), Some("people"));
            let info = result.expect("describe");
            let names: Vec<_> = info.columns.iter().map(|c| c.name.as_str()).collect();
            assert_eq!(names, ["id", "name"]);
            assert!(info.columns[0].primary_key);
        }
        other => panic!("expected Described, got {other:?}"),
    }

    match engine.ask(Command::AllColumns { path: main }).await {
        EngineEvent::AllColumns { result, .. } => {
            let columns: Vec<_> = result
                .expect("columns")
                .into_iter()
                .map(|c| format!("{}.{}", c.table, c.name))
                .collect();
            assert_eq!(columns, ["people.id", "people.name"]);
        }
        other => panic!("expected AllColumns, got {other:?}"),
    }

    let bad = engine.query("SELECT * FROM no_such_table", 10).await;
    let err = bad.outcome.expect_err("an error");
    assert!(err.to_string().contains("no such table"), "{err}");
    assert_eq!(engine.scalar("SELECT 1").await.as_deref(), Some("1"));
}

#[tokio::test]
async fn sqlite_tables_browse_like_queries() {
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;
    engine
        .query(
            "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT);
             INSERT INTO people VALUES (1, 'ada'), (2, 'grace'), (3, 'edsger')",
            0,
        )
        .await
        .outcome
        .expect("create");

    let people = NodePath::root().child("main").child("people");
    engine.next_id += 1;
    let id = engine.next_id;
    engine.engine.send(Command::Browse {
        id,
        path: people.clone(),
        row_limit: 2,
    });
    let finished = engine.finish(id).await;
    let stats = finished.outcome.expect("browse");
    assert_eq!(finished.columns, ["id", "name"]);
    assert_eq!(
        finished.rows,
        [
            [Some("1".to_string()), Some("ada".to_string())],
            [Some("2".to_string()), Some("grace".to_string())],
        ]
    );
    assert!(stats.truncated);

    engine.next_id += 1;
    let id = engine.next_id;
    engine.engine.send(Command::Browse {
        id,
        path: NodePath::root().child("main").child("nobody"),
        row_limit: 0,
    });
    assert!(engine.finish(id).await.outcome.is_err());
}

#[tokio::test]
async fn sqlite_cancel_drops_the_query_and_the_session_survives() {
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;

    // Never ends on its own: rows past the limit are read and dropped.
    let took = engine
        .cancel_after_start(
            "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s) SELECT n FROM s",
        )
        .await;

    assert!(took < Duration::from_millis(300), "took {took:?}");
    assert_eq!(engine.scalar("SELECT 1").await.as_deref(), Some("1"));
}

#[tokio::test]
async fn commands_without_a_connection_fail() {
    let mut engine = Driver::new();

    let finished = engine.query("SELECT 1", 10).await;
    assert!(matches!(finished.outcome, Err(DbError::Connection(_))));

    let event = engine
        .ask(Command::Children {
            path: NodePath::root(),
        })
        .await;
    assert!(matches!(
        event,
        EngineEvent::Children { result: Err(_), .. }
    ));
}

#[tokio::test]
async fn failed_connect_is_reported() {
    let mut engine = Driver::new();
    engine.engine.send(Command::Connect {
        db_type: DbType::Sqlite,
        params: ConnectionParams::default(),
    });

    assert!(matches!(
        engine.next().await,
        EngineEvent::Connected(Err(DbError::Config(_)))
    ));
}

#[tokio::test]
async fn postgres_streams_and_cancels() {
    let Some(url) = server_url("DFOX_TEST_POSTGRES_URL") else {
        return;
    };
    let mut engine = Driver::connect(DbType::Postgres, params_from_url(&url)).await;

    let finished = engine
        .query(
            "SELECT n, md5(n::text) FROM generate_series(1, 200000) n",
            100_000,
        )
        .await;
    let stats = finished.outcome.expect("query");
    assert_eq!(finished.rows.len(), 100_000);
    assert_eq!(stats.rows, 100_000);
    assert!(stats.truncated);
    assert!(finished.batches >= 10, "{} batches", finished.batches);
    let first_row = stats.first_row.expect("first row");
    assert!(
        first_row < Duration::from_millis(500),
        "first row after {first_row:?}"
    );

    let took = engine.cancel_after_start("SELECT pg_sleep(30)").await;
    assert!(took < Duration::from_millis(300), "took {took:?}");
    assert_eq!(engine.scalar("SELECT 1").await.as_deref(), Some("1"));
}

/// The catalog answers while a query runs: browsing never waits behind it.
#[tokio::test]
async fn postgres_catalog_answers_during_a_query() {
    let Some(url) = server_url("DFOX_TEST_POSTGRES_URL") else {
        return;
    };
    let mut engine = Driver::connect(DbType::Postgres, params_from_url(&url)).await;
    let id = engine.start("SELECT pg_sleep(2)", 10);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let asked = Instant::now();
    engine.engine.send(Command::Children {
        path: NodePath::root(),
    });
    loop {
        match engine.next().await {
            EngineEvent::Children { result, .. } => {
                assert!(!result.expect("databases").is_empty());
                break;
            }
            EngineEvent::QueryDone { .. } => panic!("the catalog waited for the query"),
            _ => {}
        }
    }
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "{:?}",
        asked.elapsed()
    );
    loop {
        if let EngineEvent::QueryDone { id: done, outcome } = engine.next().await {
            assert_eq!(done, id);
            outcome.expect("the query");
            break;
        }
    }
}

#[tokio::test]
async fn postgres_notices_reach_the_ui() {
    let Some(url) = server_url("DFOX_TEST_POSTGRES_URL") else {
        return;
    };
    let mut engine = Driver::connect(DbType::Postgres, params_from_url(&url)).await;
    let finished = engine
        .query("DO $$ BEGIN RAISE NOTICE 'hello %', 7; END $$", 10)
        .await;
    finished.outcome.expect("query");
    assert_eq!(finished.notices, ["NOTICE: hello 7"]);
}

#[tokio::test]
async fn mysql_cancels() {
    let Some(url) = server_url("DFOX_TEST_MYSQL_URL") else {
        return;
    };
    let mut engine = Driver::connect(DbType::MySql, params_from_url(&url)).await;

    let took = engine.cancel_after_start("SELECT SLEEP(30)").await;
    assert!(took < Duration::from_millis(300), "took {took:?}");
    assert_eq!(engine.scalar("SELECT 1").await.as_deref(), Some("1"));
}

/// Sessions open through the opener the app passes, such as one that
/// tunnels through SSH.
#[tokio::test]
async fn sessions_open_through_the_opener() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let opened = Arc::new(AtomicUsize::new(0));
    let counted = opened.clone();
    let opener: engine::Opener = Arc::new(move |driver, params| {
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { driver.open(&params).await })
    });
    let (handle, events) = engine::spawn_with(drivers(), opener);
    let mut engine = Driver {
        engine: handle,
        events,
        next_id: 0,
    };
    let db = TempDb::new();
    let event = engine
        .ask(Command::Connect {
            db_type: DbType::Sqlite,
            params: db.params(),
        })
        .await;
    let info = match event {
        EngineEvent::Connected(result) => result.expect("connect"),
        other => panic!("expected Connected, got {other:?}"),
    };
    assert!(info.caps.contains(dfox_core::session::Capabilities::CANCEL));
    assert_eq!(opened.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ping_needs_no_connection() {
    let db = TempDb::new();
    let mut engine = Driver::new();
    engine.engine.send(Command::Ping {
        id: 7,
        db_type: DbType::Sqlite,
        params: db.params(),
    });
    assert!(matches!(
        engine.next().await,
        EngineEvent::Pong {
            id: 7,
            result: Ok(_)
        }
    ));

    engine.engine.send(Command::Ping {
        id: 8,
        db_type: DbType::Sqlite,
        params: ConnectionParams {
            database: Some("/no/such/file.db".into()),
            ..Default::default()
        },
    });
    assert!(matches!(
        engine.next().await,
        EngineEvent::Pong {
            id: 8,
            result: Err(DbError::Connection(_))
        }
    ));
}
