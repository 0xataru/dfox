mod common;

use common::{only_row, rows, scalar, unique};
use dfox_core::{
    db::{sqlite::SqliteClient, DbClient},
    models::{
        connections::{ConnectionParams, DbType},
        schema::ObjectKind,
    },
    DbManager,
};

async fn client() -> SqliteClient {
    SqliteClient::connect("sqlite::memory:")
        .await
        .expect("connect")
}

#[tokio::test]
async fn values_render_by_their_storage_class() {
    let db = client().await;

    let out = db
        .run("SELECT 1, 1.0, 2.5, 'txt', x'deadbeef', NULL", 10)
        .await
        .unwrap();

    assert_eq!(
        only_row(&out),
        [
            Some("1"),
            Some("1.0"),
            Some("2.5"),
            Some("txt"),
            Some("0xdeadbeef"),
            None
        ]
    );
}

#[tokio::test]
async fn mixed_type_columns_decode_per_value() {
    let db = client().await;

    let out = db
        .run(
            "CREATE TABLE mixed (v);
             INSERT INTO mixed VALUES (1), ('a'), (2.5), (NULL), (x'00ff');
             SELECT v FROM mixed ORDER BY rowid",
            10,
        )
        .await
        .unwrap();

    assert_eq!(
        rows(&out),
        [
            [Some("1")],
            [Some("a")],
            [Some("2.5")],
            [None],
            [Some("0x00ff")]
        ]
    );
}

#[tokio::test]
async fn columns_keep_query_order_and_duplicates() {
    let db = client().await;

    let out = db
        .run("SELECT 3 AS c, 1 AS a, 2 AS b, 4 AS a", 10)
        .await
        .unwrap();

    assert_eq!(out.columns, ["c", "a", "b", "a"]);
    assert_eq!(only_row(&out), [Some("3"), Some("1"), Some("2"), Some("4")]);
}

#[tokio::test]
async fn pragma_and_returning_return_rows() {
    let db = client().await;
    db.execute("CREATE TABLE t (x INTEGER, y TEXT)")
        .await
        .unwrap();

    let pragma = db.run("PRAGMA table_info(t)", 10).await.unwrap();
    assert_eq!(pragma.rows.len(), 2);

    let returning = db
        .run(
            "INSERT INTO t VALUES (5, 'a'), (6, 'b') RETURNING x * 10",
            10,
        )
        .await
        .unwrap();
    assert_eq!(rows(&returning), [[Some("50")], [Some("60")]]);
}

#[tokio::test]
async fn session_state_carries_over_between_runs() {
    let db = client().await;

    db.execute("CREATE TABLE tx_t (x INTEGER)").await.unwrap();
    db.execute("BEGIN").await.unwrap();
    db.execute("INSERT INTO tx_t VALUES (1)").await.unwrap();
    db.execute("ROLLBACK").await.unwrap();

    let after = db.run("SELECT count(*) FROM tx_t", 10).await.unwrap();
    assert_eq!(scalar(&after), Some("0"));
}

#[tokio::test]
async fn row_limit_truncates_but_runs_every_statement() {
    let db = client().await;
    let fifty = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 50) \
                 SELECT n FROM s";

    let limited = db.run(fifty, 10).await.unwrap();
    assert_eq!(limited.rows.len(), 10);
    assert!(limited.truncated);

    let after = db
        .run(&format!("{fifty}; SELECT 'after'"), 10)
        .await
        .unwrap();
    assert_eq!(scalar(&after), Some("after"));
    assert!(!after.truncated);
}

#[tokio::test]
async fn metadata_and_describe_are_injection_safe() {
    let db = client().await;
    db.execute(
        "CREATE TABLE people (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL DEFAULT 'x', note TEXT)",
    )
    .await
    .unwrap();
    db.execute("INSERT INTO people (note) VALUES ('creates sqlite_sequence')")
        .await
        .unwrap();

    // Internal tables such as sqlite_sequence are hidden.
    assert_eq!(db.list_tables().await.unwrap(), ["people"]);

    let schema = db.describe_table("main", "people").await.unwrap();
    let columns: Vec<_> = schema
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str(), c.is_nullable))
        .collect();
    assert_eq!(
        columns,
        [
            ("id", "INTEGER", true),
            ("name", "TEXT", false),
            ("note", "TEXT", true),
        ]
    );
    assert_eq!(schema.columns[1].default.as_deref(), Some("'x'"));

    let attack = "x'); DROP TABLE people; --";
    assert!(db
        .describe_table("main", attack)
        .await
        .unwrap()
        .columns
        .is_empty());
    assert_eq!(db.list_tables().await.unwrap(), ["people"]);
}

#[tokio::test]
async fn connects_to_a_file_by_path() {
    let path = std::env::temp_dir().join(format!("{}.db", unique("dfox_it")));
    std::fs::File::create(&path).unwrap();
    let params = ConnectionParams {
        database: Some(path.to_string_lossy().into_owned()),
        ..Default::default()
    };

    let db = DbManager::connect(&DbType::Sqlite, &params).await.unwrap();
    db.execute("CREATE TABLE on_disk (x INTEGER)")
        .await
        .unwrap();
    assert_eq!(db.list_tables().await.unwrap(), ["on_disk"]);
    drop(db);

    // Dropping the pool closes its connections in the background, so Windows
    // may still hold the file open here.
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn main_is_the_schema_and_lists_tables_and_views() {
    let db = client().await;
    db.execute("CREATE TABLE t (i INTEGER); CREATE VIEW v AS SELECT 1")
        .await
        .unwrap();

    assert_eq!(db.list_schemas().await.unwrap(), ["main"]);
    let objects: Vec<_> = db
        .list_objects("main")
        .await
        .unwrap()
        .into_iter()
        .map(|o| (o.name, o.kind, o.rows))
        .collect();
    assert_eq!(
        objects,
        [
            ("t".to_string(), ObjectKind::Table, None),
            ("v".to_string(), ObjectKind::View, None),
        ]
    );
}

#[tokio::test]
async fn read_only_files_refuse_writes() {
    let path = std::env::temp_dir().join(format!("{}.db", unique("dfox_ro")));
    std::fs::File::create(&path).unwrap();
    let params = ConnectionParams {
        database: Some(path.to_string_lossy().into_owned()),
        read_only: true,
        ..Default::default()
    };

    let db = DbManager::connect(&DbType::Sqlite, &params).await.unwrap();
    let err = db.execute("CREATE TABLE t (i INTEGER)").await.unwrap_err();
    assert!(err.to_string().contains("readonly"), "{err}");
    drop(db);
    let _ = std::fs::remove_file(&path);
}
