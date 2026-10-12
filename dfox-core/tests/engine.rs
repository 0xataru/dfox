mod common;

use std::time::{Duration, Instant};

use common::{server_url, unique};
use dfox_core::{
    engine::{self, Command, EngineEvent, EngineHandle, QueryId, QueryStats},
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
};
use sqlx::{mysql::MySqlConnectOptions, postgres::PgConnectOptions};
use tokio::sync::mpsc;

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
    chunks: usize,
    outcome: Result<QueryStats, DbError>,
}

impl Driver {
    fn new() -> Self {
        let (engine, events) = engine::spawn();
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
            EngineEvent::Connected(result) => result.expect("connect"),
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
        let mut finished = Finished {
            columns: Vec::new(),
            rows: Vec::new(),
            chunks: 0,
            outcome: Ok(QueryStats::default()),
        };
        loop {
            match self.next().await {
                EngineEvent::Columns { id: got, columns } if got == id => {
                    finished.columns = columns.into_iter().map(|c| c.name).collect();
                    finished.rows.clear();
                }
                EngineEvent::Rows { id: got, rows } if got == id => {
                    finished.chunks += 1;
                    finished.rows.extend(
                        rows.into_iter()
                            .map(|row| row.into_iter().map(|cell| cell.into_text()).collect()),
                    );
                }
                EngineEvent::QueryDone { id: got, outcome } if got == id => {
                    finished.outcome = outcome;
                    return finished;
                }
                other => panic!("unexpected event {other:?}"),
            }
        }
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

fn server_params(url: &str, db_type: &DbType) -> ConnectionParams {
    match db_type {
        DbType::Postgres => {
            let options: PgConnectOptions = url.parse().expect("postgres url");
            ConnectionParams {
                host: options.get_host().to_string(),
                port: options.get_port(),
                username: options.get_username().to_string(),
                password: url_password(url),
                database: options.get_database().map(str::to_string),
            }
        }
        _ => {
            let options: MySqlConnectOptions = url.parse().expect("mysql url");
            ConnectionParams {
                host: options.get_host().to_string(),
                port: options.get_port(),
                username: options.get_username().to_string(),
                password: url_password(url),
                database: options.get_database().map(str::to_string),
            }
        }
    }
}

/// The test URLs use plain passwords, so no percent-decoding is needed.
fn url_password(url: &str) -> String {
    let credentials = url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('@').next());
    credentials
        .and_then(|c| c.split_once(':'))
        .map(|(_, password)| password.to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn sqlite_rows_stream_in_chunks() {
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;

    let finished = engine
        .query(
            "CREATE TABLE t (n INTEGER);
             INSERT INTO t WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 2500)
                 SELECT n FROM s;
             SELECT n, 'row ' || n FROM t ORDER BY n",
            10_000,
        )
        .await;

    let stats = finished.outcome.expect("query");
    assert_eq!(finished.columns.len(), 2);
    assert_eq!(finished.rows.len(), 2500);
    assert_eq!(finished.rows[2499][1].as_deref(), Some("row 2500"));
    assert!(finished.chunks >= 3, "{} chunks", finished.chunks);
    assert_eq!(stats.rows, 2500);
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
async fn sqlite_metadata_and_errors() {
    let db = TempDb::new();
    let mut engine = Driver::connect(DbType::Sqlite, db.params()).await;
    engine
        .query("CREATE TABLE people (id INTEGER, name TEXT)", 0)
        .await
        .outcome
        .expect("create");

    engine.engine.send(Command::ListTables);
    match engine.next().await {
        EngineEvent::Tables(tables) => assert_eq!(tables.expect("tables"), ["people"]),
        other => panic!("expected Tables, got {other:?}"),
    }

    engine.engine.send(Command::Describe {
        table: "people".to_string(),
    });
    match engine.next().await {
        EngineEvent::Schema(schema) => {
            let names: Vec<_> = schema
                .expect("schema")
                .columns
                .into_iter()
                .map(|c| c.name)
                .collect();
            assert_eq!(names, ["id", "name"]);
        }
        other => panic!("expected Schema, got {other:?}"),
    }

    let bad = engine.query("SELECT * FROM no_such_table", 10).await;
    assert!(matches!(bad.outcome, Err(DbError::Sqlx(_))));
    assert_eq!(engine.scalar("SELECT 1").await.as_deref(), Some("1"));
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

    engine.engine.send(Command::ListTables);
    assert!(matches!(engine.next().await, EngineEvent::Tables(Err(_))));
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
    let mut engine =
        Driver::connect(DbType::Postgres, server_params(&url, &DbType::Postgres)).await;

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
    assert!(finished.chunks >= 100, "{} chunks", finished.chunks);
    let first_row = stats.first_row.expect("first row");
    assert!(
        first_row < Duration::from_millis(500),
        "first row after {first_row:?}"
    );

    let took = engine.cancel_after_start("SELECT pg_sleep(30)").await;
    assert!(took < Duration::from_millis(300), "took {took:?}");
    assert_eq!(engine.scalar("SELECT 1").await.as_deref(), Some("1"));
}

#[tokio::test]
async fn mysql_cancels() {
    let Some(url) = server_url("DFOX_TEST_MYSQL_URL") else {
        return;
    };
    let mut engine = Driver::connect(DbType::MySql, server_params(&url, &DbType::MySql)).await;

    let took = engine.cancel_after_start("SELECT SLEEP(30)").await;
    assert!(took < Duration::from_millis(300), "took {took:?}");
    assert_eq!(engine.scalar("SELECT 1").await.as_deref(), Some("1"));
}
