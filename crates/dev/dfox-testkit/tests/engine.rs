use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use dfox_core::{
    arrow::ArrowResultSet,
    catalog::{BrowseQuery, NodeKind, NodePath},
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
        query: BrowseQuery {
            limit: Some(2),
            ..BrowseQuery::default()
        },
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
        query: BrowseQuery::default(),
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

/// Edits as the grid sends them, to `table` made by `create`: values as
/// text, keys as the result holds them, all applied in one go.
async fn grid_edits(engine: &mut Driver, table: NodePath, create: &str) {
    use dfox_core::{
        result::OwnedCell,
        session::{Change, ChangeSet},
    };

    engine.query(create, 0).await.outcome.expect("create");
    let name = table.name().unwrap_or_default().to_string();
    engine.engine.send(Command::Apply {
        changes: ChangeSet {
            table: table.clone(),
            key: vec!["id".into()],
            changes: vec![
                // Only some columns: `age` gets its default.
                Change::Insert(vec![
                    ("id".into(), OwnedCell::Text("4".into())),
                    ("name".into(), OwnedCell::Text("Alan".into())),
                ]),
                Change::Update {
                    key: vec![OwnedCell::Int(1)],
                    set: vec![
                        ("name".into(), OwnedCell::Text("Ada".into())),
                        ("age".into(), OwnedCell::Text("37".into())),
                    ],
                },
                Change::Update {
                    key: vec![OwnedCell::Int(2)],
                    set: vec![("name".into(), OwnedCell::Null)],
                },
                Change::Delete(vec![OwnedCell::Int(3)]),
            ],
        },
    });
    match engine.next().await {
        EngineEvent::Applied { path, result } => {
            assert_eq!(path, table);
            let report = result.expect("apply");
            assert_eq!((report.inserted, report.updated, report.deleted), (1, 2, 1));
        }
        other => panic!("expected Applied, got {other:?}"),
    }
    let rows = engine
        .query(&format!("SELECT id, name, age FROM {name} ORDER BY id"), 10)
        .await;
    assert_eq!(
        rows.rows,
        [
            [
                Some("1".to_string()),
                Some("Ada".to_string()),
                Some("37".to_string())
            ],
            [Some("2".to_string()), None, Some("2".to_string())],
            [Some("4".to_string()), Some("Alan".to_string()), None],
        ]
    );
}

const PEOPLE: &str = "(id INTEGER PRIMARY KEY, name TEXT, age INTEGER)";
const ROWS: &str = "(1, 'ada', 1), (2, 'grace', 2), (3, 'edsger', 3)";

#[tokio::test]
async fn sqlite_grid_edits_are_written_in_one_go() {
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;
    let create = format!("CREATE TABLE people {PEOPLE}; INSERT INTO people VALUES {ROWS}");
    let table = NodePath::root().child("main").child("people");
    grid_edits(&mut engine, table, &create).await;
}

#[tokio::test]
async fn postgres_grid_edits_are_written_in_one_go() {
    let Some(url) = server_url("DFOX_TEST_POSTGRES_URL") else {
        return;
    };
    let params = params_from_url(&url);
    let database = params.database.clone().unwrap_or_default();
    let mut engine = Driver::connect(DbType::Postgres, params).await;
    let name = unique("edits");
    let create = format!("CREATE TABLE {name} {PEOPLE}; INSERT INTO {name} VALUES {ROWS}");
    let table: NodePath = [database.as_str(), "public", name.as_str()]
        .into_iter()
        .collect();
    grid_edits(&mut engine, table, &create).await;
    engine
        .query(&format!("DROP TABLE {name}"), 0)
        .await
        .outcome
        .expect("drop");
}

#[tokio::test]
async fn mysql_grid_edits_are_written_in_one_go() {
    let Some(url) = server_url("DFOX_TEST_MYSQL_URL") else {
        return;
    };
    let params = params_from_url(&url);
    let database = params.database.clone().unwrap_or_default();
    let mut engine = Driver::connect(DbType::MySql, params).await;
    let name = unique("edits");
    let create = format!("CREATE TABLE {name} {PEOPLE}; INSERT INTO {name} VALUES {ROWS}");
    let table: NodePath = [database.as_str(), name.as_str()].into_iter().collect();
    grid_edits(&mut engine, table, &create).await;
    engine
        .query(&format!("DROP TABLE {name}"), 0)
        .await
        .outcome
        .expect("drop");
}

#[tokio::test]
async fn sqlite_queries_export_to_files_and_failures_leave_none() {
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;
    engine
        .query(
            "CREATE TABLE t (n INTEGER, s TEXT);
             INSERT INTO t WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 25000)
                 SELECT n, 'row ' || n FROM r",
            0,
        )
        .await
        .outcome
        .expect("create");

    let dir = std::env::temp_dir().join(unique("dfox_export"));
    std::fs::create_dir_all(&dir).expect("dir");
    let export = |engine: &mut Driver, file: &std::path::Path, sql: &str| {
        engine.next_id += 1;
        let id = engine.next_id;
        engine.engine.send(Command::Export {
            id,
            sql: sql.to_string(),
            file: file.to_path_buf(),
        });
        id
    };
    // Waits for the export to end, counting its progress reports.
    async fn done(engine: &mut Driver, id: QueryId) -> (Result<QueryStats, DbError>, usize) {
        let mut progress = 0;
        loop {
            match engine.next().await {
                EngineEvent::Progress { id: of, .. } if of == id => progress += 1,
                EngineEvent::QueryDone { id: of, outcome } if of == id => {
                    return (outcome, progress)
                }
                _ => {}
            }
        }
    }

    let csv = dir.join("t.csv");
    let id = export(&mut engine, &csv, "SELECT n, s FROM t ORDER BY n");
    let (outcome, _) = done(&mut engine, id).await;
    assert_eq!(outcome.expect("export").rows, 25_000);
    let text = std::fs::read_to_string(&csv).expect("read");
    assert_eq!(text.lines().count(), 25_001, "a header and every row");
    assert!(text.starts_with("n,s\n1,row 1\n"));

    let parquet = dir.join("t.parquet");
    let id = export(&mut engine, &parquet, "SELECT n FROM t");
    assert_eq!(done(&mut engine, id).await.0.expect("export").rows, 25_000);
    assert_eq!(&std::fs::read(&parquet).expect("read")[..4], b"PAR1");

    // A failing query, and one with no rows to write, leave no file.
    let bad = dir.join("bad.csv");
    let id = export(&mut engine, &bad, "SELECT nope FROM t");
    assert!(done(&mut engine, id).await.0.is_err());
    assert!(!bad.exists());
    let id = export(&mut engine, &bad, "DELETE FROM t WHERE n < 0");
    let err = done(&mut engine, id).await.0.expect_err("no rows");
    assert!(err.to_string().contains("no rows"), "{err}");
    assert!(!bad.exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sqlite_files_import_into_tables_by_column_name() {
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;
    engine
        .query(
            "CREATE TABLE source (n INTEGER, s TEXT);
             INSERT INTO source WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 2500)
                 SELECT n, CASE WHEN n % 10 = 0 THEN NULL ELSE 'row ' || n END FROM r;
             CREATE TABLE target (id INTEGER PRIMARY KEY, N INTEGER, s TEXT DEFAULT 'none');
             CREATE TABLE narrow (n INTEGER)",
            0,
        )
        .await
        .outcome
        .expect("create");
    let dir = std::env::temp_dir().join(unique("dfox_import"));
    std::fs::create_dir_all(&dir).expect("dir");

    async fn done(engine: &mut Driver, id: QueryId) -> Result<QueryStats, DbError> {
        loop {
            if let EngineEvent::QueryDone { id: of, outcome } = engine.next().await {
                if of == id {
                    return outcome;
                }
            }
        }
    }
    let target = NodePath::root().child("main").child("target");
    for name in ["source.csv", "source.jsonl", "source.parquet"] {
        let file = dir.join(name);
        engine.next_id += 1;
        let id = engine.next_id;
        engine.engine.send(Command::Export {
            id,
            sql: "SELECT n, s FROM source ORDER BY n".into(),
            file: file.clone(),
        });
        done(&mut engine, id).await.expect("export");

        engine
            .query("DELETE FROM target", 0)
            .await
            .outcome
            .expect("empty");
        engine.next_id += 1;
        let id = engine.next_id;
        engine.engine.send(Command::Import {
            id,
            table: target.clone(),
            file: file.clone(),
        });
        // Columns match by name whatever their case; empty cells are NULL.
        assert_eq!(
            done(&mut engine, id).await.expect(name).rows,
            2500,
            "{name}"
        );
        assert_eq!(
            engine
                .scalar("SELECT count(*) FROM target WHERE s IS NULL")
                .await
                .as_deref(),
            Some("250"),
            "{name}"
        );
        assert!(file.exists(), "the file stays");
    }

    // A column the table doesn't have stops the import before any row.
    engine.next_id += 1;
    let id = engine.next_id;
    engine.engine.send(Command::Import {
        id,
        table: NodePath::root().child("main").child("narrow"),
        file: dir.join("source.csv"),
    });
    let err = done(&mut engine, id).await.expect_err("no column s");
    assert!(err.to_string().contains("column s"), "{err}");
    assert_eq!(
        engine
            .scalar("SELECT count(*) FROM narrow")
            .await
            .as_deref(),
        Some("0")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Imports a CSV of numbers, decimals, booleans, text and empty cells into
/// `table`, made by `create` in the database `engine` is on.
async fn import_csv(engine: &mut Driver, table: NodePath, create: &str) {
    engine.query(create, 0).await.outcome.expect("create");
    let file = std::env::temp_dir().join(format!("{}.csv", unique("dfox_import")));
    std::fs::write(&file, "id,x,b,s\n1,1.5,true,a\n2,,false,\n").expect("file");
    engine.next_id += 1;
    let id = engine.next_id;
    engine.engine.send(Command::Import {
        id,
        table: table.clone(),
        file: file.clone(),
    });
    let outcome = loop {
        if let EngineEvent::QueryDone { id: of, outcome } = engine.next().await {
            if of == id {
                break outcome;
            }
        }
    };
    assert_eq!(outcome.expect("import").rows, 2);
    let name = table.name().unwrap_or_default().to_string();
    let rows = engine
        .query(
            &format!(
                "SELECT id, x, CASE WHEN b THEN 'yes' ELSE 'no' END, s FROM {name} ORDER BY id"
            ),
            10,
        )
        .await;
    assert_eq!(
        rows.rows,
        [
            [
                Some("1".into()),
                Some("1.5".into()),
                Some("yes".into()),
                Some("a".to_string())
            ],
            [Some("2".into()), None, Some("no".into()), None],
        ]
    );
    let _ = std::fs::remove_file(file);
}

#[tokio::test]
async fn postgres_imports_files() {
    let Some(url) = server_url("DFOX_TEST_POSTGRES_URL") else {
        return;
    };
    let params = params_from_url(&url);
    let database = params.database.clone().unwrap_or_default();
    let mut engine = Driver::connect(DbType::Postgres, params).await;
    let name = unique("import");
    let create = format!(
        "CREATE TABLE {name} (id INTEGER PRIMARY KEY, x DOUBLE PRECISION, b BOOLEAN, s TEXT DEFAULT 'none')"
    );
    let table: NodePath = [database.as_str(), "public", name.as_str()]
        .into_iter()
        .collect();
    import_csv(&mut engine, table, &create).await;
    engine
        .query(&format!("DROP TABLE {name}"), 0)
        .await
        .outcome
        .expect("drop");
}

#[tokio::test]
async fn mysql_imports_files() {
    let Some(url) = server_url("DFOX_TEST_MYSQL_URL") else {
        return;
    };
    let params = params_from_url(&url);
    let database = params.database.clone().unwrap_or_default();
    let mut engine = Driver::connect(DbType::MySql, params).await;
    let name = unique("import");
    let create = format!(
        "CREATE TABLE {name} (id INTEGER PRIMARY KEY, x DOUBLE, b BOOLEAN, s VARCHAR(20) DEFAULT 'none')"
    );
    let table: NodePath = [database.as_str(), name.as_str()].into_iter().collect();
    import_csv(&mut engine, table, &create).await;
    engine
        .query(&format!("DROP TABLE {name}"), 0)
        .await
        .outcome
        .expect("drop");
}

/// Imports 50,000 rows into a table made by `create`, and says how fast.
async fn import_rows(engine: &mut Driver, what: &str, table: NodePath, create: &str) {
    let rows = 50_000;
    let file = std::env::temp_dir().join(format!("{}.csv", unique("speed")));
    let mut text = String::from("id,name,price\n");
    for i in 0..rows {
        text.push_str(&format!("{i},item {i},{}.5\n", i % 1000));
    }
    std::fs::write(&file, text).expect("file");
    if !create.is_empty() {
        engine.query(create, 0).await.outcome.expect("create");
    }
    engine.next_id += 1;
    let id = engine.next_id;
    let started = Instant::now();
    engine.engine.send(Command::Import {
        id,
        table,
        file: file.clone(),
    });
    let outcome = loop {
        if let EngineEvent::QueryDone { id: of, outcome } = engine.next().await {
            if of == id {
                break outcome;
            }
        }
    };
    let took = started.elapsed();
    assert_eq!(outcome.expect("import").rows, rows, "{what}");
    println!(
        "imported {rows} rows into {what} in {took:?}: {:.0} rows/s",
        rows as f64 / took.as_secs_f64()
    );
    let _ = std::fs::remove_file(file);
}

/// `DFOX_TEST_…_URL=… cargo test -p dfox-testkit --release --test engine -- --ignored import_speed --nocapture`
#[tokio::test]
#[ignore = "timing"]
async fn import_speed() {
    let name = unique("speed");
    let create = format!("CREATE TABLE {name} (id INTEGER PRIMARY KEY, name TEXT, price NUMERIC)");
    if let Some(url) = server_url("DFOX_TEST_POSTGRES_URL") {
        let params = params_from_url(&url);
        let database = params.database.clone().unwrap_or_default();
        let mut engine = Driver::connect(DbType::Postgres, params).await;
        let table = [database.as_str(), "public", name.as_str()]
            .into_iter()
            .collect();
        import_rows(&mut engine, "Postgres", table, &create).await;
        engine
            .query(&format!("DROP TABLE {name}"), 0)
            .await
            .outcome
            .expect("drop");
    }
    if let Some(url) = server_url("DFOX_TEST_MYSQL_URL") {
        let params = params_from_url(&url);
        let database = params.database.clone().unwrap_or_default();
        let mut engine = Driver::connect(DbType::MySql, params).await;
        let table = [database.as_str(), name.as_str()].into_iter().collect();
        import_rows(&mut engine, "MySQL", table, &create).await;
        engine
            .query(&format!("DROP TABLE {name}"), 0)
            .await
            .outcome
            .expect("drop");
    }
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;
    let table = NodePath::root().child("main").child(name.clone());
    import_rows(&mut engine, "SQLite", table, &create).await;
}

/// Copies `sql`'s rows from `engine`'s session into `table` of the target,
/// and returns how it ended.
async fn copy(
    engine: &mut Driver,
    sql: &str,
    db_type: DbType,
    params: ConnectionParams,
    table: NodePath,
    create: bool,
) -> Result<QueryStats, DbError> {
    engine.next_id += 1;
    let id = engine.next_id;
    engine.engine.send(Command::Copy {
        id,
        sql: sql.into(),
        target: Box::new(engine::CopyTarget {
            db_type,
            params,
            table,
            create,
        }),
    });
    loop {
        if let EngineEvent::QueryDone { id: of, outcome } = engine.next().await {
            if of == id {
                return outcome;
            }
        }
    }
}

const COPY_SOURCE: &str = "CREATE TABLE src (id INTEGER, name TEXT, price REAL);
     INSERT INTO src WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 1500)
         SELECT n, 'item ' || n, n * 1.5 FROM r";

#[tokio::test]
async fn sqlite_rows_copy_into_a_new_table_anywhere() {
    let source = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, source.params()).await;
    engine.query(COPY_SOURCE, 0).await.outcome.expect("source");
    let sql = "SELECT id, name, price FROM src ORDER BY id";

    // Into another SQLite file, creating the table.
    let target = TempDb::new();
    let table = NodePath::root().child("main").child("dst");
    let stats = copy(
        &mut engine,
        sql,
        DbType::Sqlite,
        target.params(),
        table.clone(),
        true,
    )
    .await
    .expect("copy");
    assert_eq!(stats.rows, 1500);
    let mut check = Driver::connect(DbType::Sqlite, target.params()).await;
    assert_eq!(
        check
            .scalar("SELECT name || ' ' || price FROM dst WHERE id = 2")
            .await
            .as_deref(),
        Some("item 2 3.0")
    );

    // Into a table that is there: columns match by name; one it lacks fails.
    check
        .query(
            "CREATE TABLE kept (ID INTEGER, NAME TEXT, price REAL, extra TEXT DEFAULT 'x')",
            0,
        )
        .await
        .outcome
        .expect("kept");
    let kept = NodePath::root().child("main").child("kept");
    copy(
        &mut engine,
        sql,
        DbType::Sqlite,
        target.params(),
        kept,
        false,
    )
    .await
    .expect("copy into kept");
    assert_eq!(
        check
            .scalar("SELECT count(*) FROM kept WHERE extra = 'x'")
            .await
            .as_deref(),
        Some("1500")
    );
    let err = copy(
        &mut engine,
        "SELECT id, name AS nope FROM src",
        DbType::Sqlite,
        target.params(),
        NodePath::root().child("main").child("kept"),
        false,
    )
    .await
    .expect_err("no column nope");
    assert!(err.to_string().contains("nope"), "{err}");

    // Into Postgres and MySQL, creating the table there.
    for (db_type, var) in [
        (DbType::Postgres, "DFOX_TEST_POSTGRES_URL"),
        (DbType::MySql, "DFOX_TEST_MYSQL_URL"),
    ] {
        let Some(url) = server_url(var) else {
            continue;
        };
        let params = params_from_url(&url);
        let database = params.database.clone().unwrap_or_default();
        let name = unique("copied");
        let table: NodePath = match db_type {
            DbType::Postgres => [database.as_str(), "public", name.as_str()]
                .into_iter()
                .collect(),
            _ => [database.as_str(), name.as_str()].into_iter().collect(),
        };
        let stats = copy(&mut engine, sql, db_type, params.clone(), table, true)
            .await
            .unwrap_or_else(|err| panic!("{db_type:?}: {err}"));
        assert_eq!(stats.rows, 1500, "{db_type:?}");
        let mut there = Driver::connect(db_type, params).await;
        assert_eq!(
            there
                .scalar(&format!("SELECT count(*) FROM {name} WHERE price = 3"))
                .await
                .as_deref(),
            Some("1"),
            "{db_type:?}"
        );
        there
            .query(&format!("DROP TABLE {name}"), 0)
            .await
            .outcome
            .expect("drop");
    }
}

#[tokio::test]
async fn postgres_and_mysql_copy_into_each_other() {
    let (Some(pg), Some(my)) = (
        server_url("DFOX_TEST_POSTGRES_URL"),
        server_url("DFOX_TEST_MYSQL_URL"),
    ) else {
        return;
    };
    let (pg, my) = (params_from_url(&pg), params_from_url(&my));
    let (pg_db, my_db) = (
        pg.database.clone().unwrap_or_default(),
        my.database.clone().unwrap_or_default(),
    );
    let name = unique("both");
    let mut from_pg = Driver::connect(DbType::Postgres, pg.clone()).await;
    from_pg
        .query(
            &format!(
                "CREATE TABLE {name} (id BIGINT, label TEXT, amount NUMERIC(10, 2), seen TIMESTAMPTZ, ok BOOLEAN);
                 INSERT INTO {name} SELECT g, 'n' || g, g / 4.0, '2024-01-02 03:04:05+00', g % 2 = 0
                 FROM generate_series(1, 1000) g"
            ),
            0,
        )
        .await
        .outcome
        .expect("pg source");
    let to_my: NodePath = [my_db.as_str(), name.as_str()].into_iter().collect();
    let stats = copy(
        &mut from_pg,
        &format!("SELECT * FROM {name}"),
        DbType::MySql,
        my.clone(),
        to_my,
        true,
    )
    .await
    .expect("pg → mysql");
    assert_eq!(stats.rows, 1000);

    // And back, into a new Postgres table.
    let mut from_my = Driver::connect(DbType::MySql, my).await;
    let back = format!("{name}_back");
    let to_pg: NodePath = [pg_db.as_str(), "public", back.as_str()]
        .into_iter()
        .collect();
    copy(
        &mut from_my,
        &format!("SELECT * FROM {name}"),
        DbType::Postgres,
        pg,
        to_pg,
        true,
    )
    .await
    .expect("mysql → pg");
    assert_eq!(
        from_pg
            .scalar(&format!(
                "SELECT count(*) FROM {back} b JOIN {name} a USING (id) \
                 WHERE a.label = b.label AND a.amount = b.amount AND a.ok = (b.ok::int = 1)"
            ))
            .await
            .as_deref(),
        Some("1000")
    );
    from_pg
        .query(&format!("DROP TABLE {name}; DROP TABLE {back}"), 0)
        .await
        .outcome
        .expect("drop");
    from_my
        .query(&format!("DROP TABLE {name}"), 0)
        .await
        .outcome
        .expect("drop");
}
