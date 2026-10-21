//! What any SQLite session must get right, from the first fixes (M0): values
//! by their storage class, column order, PRAGMA and RETURNING, session
//! state, row limits and names that are attacks.

use dfox_core::{
    catalog::{ConstraintKind, Distinct, NodeKind, NodePath, StatsSource},
    driver::Driver,
    models::connections::ConnectionParams,
    session::Session,
};
use dfox_driver_sqlite::SqliteDriver;
use dfox_testkit::{execute, only_row, rows, run, scalar};

async fn session() -> Box<dyn Session> {
    let params = ConnectionParams {
        database: Some(":memory:".into()),
        ..Default::default()
    };
    SqliteDriver.open(&params).await.expect("open a session")
}

#[tokio::test]
async fn values_render_by_their_storage_class() {
    let mut s = session().await;
    let out = run(
        s.as_mut(),
        "SELECT 1, 1.0, 2.5, 'txt', x'deadbeef', NULL",
        10,
    )
    .await
    .expect("run");
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
async fn mixed_type_columns_keep_every_value() {
    let mut s = session().await;
    let out = run(
        s.as_mut(),
        "CREATE TABLE mixed (v);
         INSERT INTO mixed VALUES (1), ('a'), (2.5), (NULL), (x'00ff');
         SELECT v FROM mixed ORDER BY rowid",
        10,
    )
    .await
    .expect("run");
    // Text joins the column, so the blob is written as a literal.
    assert_eq!(
        rows(&out),
        [
            [Some("1")],
            [Some("a")],
            [Some("2.5")],
            [None],
            [Some("X'00FF'")]
        ]
    );
}

#[tokio::test]
async fn columns_keep_query_order_and_duplicates() {
    let mut s = session().await;
    let out = run(s.as_mut(), "SELECT 3 AS c, 1 AS a, 2 AS b, 4 AS a", 10)
        .await
        .expect("run");
    assert_eq!(out.columns, ["c", "a", "b", "a"]);
    assert_eq!(only_row(&out), [Some("3"), Some("1"), Some("2"), Some("4")]);
}

#[tokio::test]
async fn pragma_and_returning_return_rows() {
    let mut s = session().await;
    let s = s.as_mut();
    execute(s, "CREATE TABLE t (x INTEGER, y TEXT)").await;

    let pragma = run(s, "PRAGMA table_info(t)", 10).await.expect("pragma");
    assert_eq!(pragma.rows.len(), 2);

    let returning = run(
        s,
        "INSERT INTO t VALUES (5, 'a'), (6, 'b') RETURNING x * 10",
        10,
    )
    .await
    .expect("returning");
    assert_eq!(rows(&returning), [[Some("50")], [Some("60")]]);
}

#[tokio::test]
async fn session_state_carries_over_between_runs() {
    let mut s = session().await;
    let s = s.as_mut();
    execute(s, "CREATE TABLE tx_t (x INTEGER)").await;
    execute(s, "BEGIN").await;
    execute(s, "INSERT INTO tx_t VALUES (1)").await;
    execute(s, "ROLLBACK").await;
    let after = run(s, "SELECT count(*) FROM tx_t", 10)
        .await
        .expect("count");
    assert_eq!(scalar(&after), Some("0"));
}

#[tokio::test]
async fn scripts_report_changes_and_the_last_result_set() {
    let mut s = session().await;
    let out = run(
        s.as_mut(),
        "CREATE TABLE script_t (x INTEGER);
         INSERT INTO script_t VALUES (1), (2), (3);
         UPDATE script_t SET x = x + 1 WHERE x > 1;
         SELECT 'first';
         SELECT x FROM script_t ORDER BY x",
        10,
    )
    .await
    .expect("run");
    assert_eq!(out.columns, ["x"]);
    assert_eq!(rows(&out), [[Some("1")], [Some("3")], [Some("4")]]);
    assert_eq!(out.rows_affected, 5);
}

#[tokio::test]
async fn row_limit_truncates_but_runs_every_statement() {
    let mut s = session().await;
    let s = s.as_mut();
    let fifty = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 50) \
                 SELECT n FROM s";

    let limited = run(s, fifty, 10).await.expect("run");
    assert_eq!(limited.rows.len(), 10);

    execute(s, "CREATE TABLE after_t (x INTEGER)").await;
    let after = run(
        s,
        &format!("{fifty}; INSERT INTO after_t VALUES (1); SELECT 'after'"),
        10,
    )
    .await
    .expect("run");
    assert_eq!(scalar(&after), Some("after"));
    let count = run(s, "SELECT count(*) FROM after_t", 10)
        .await
        .expect("count");
    assert_eq!(
        scalar(&count),
        Some("1"),
        "the statement past the limit ran"
    );
}

#[tokio::test]
async fn catalog_names_are_parameters_not_sql() {
    let mut s = session().await;
    execute(
        s.as_mut(),
        "CREATE TABLE people (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         name TEXT NOT NULL DEFAULT 'x', note TEXT);
         INSERT INTO people (note) VALUES ('creates sqlite_sequence')",
    )
    .await;

    // Internal tables such as sqlite_sequence are hidden.
    let main = NodePath::root().child("main");
    let tables: Vec<_> = s
        .catalog()
        .children(&main)
        .await
        .expect("tables")
        .into_iter()
        .map(|n| (n.name, n.kind))
        .collect();
    assert_eq!(tables, [("people".to_string(), NodeKind::Table)]);

    let info = s
        .catalog()
        .describe(&main.child("people"))
        .await
        .expect("describe");
    let columns: Vec<_> = info
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_name.as_str(), c.nullable))
        .collect();
    assert_eq!(
        columns,
        [
            ("id", "INTEGER", true),
            ("name", "TEXT", false),
            ("note", "TEXT", true),
        ]
    );
    assert_eq!(info.columns[1].default.as_deref(), Some("'x'"));

    execute(
        s.as_mut(),
        "CREATE VIEW grown_ups AS SELECT name FROM people   ",
    )
    .await;
    let view = s
        .catalog()
        .definition(&main.child("grown_ups"), NodeKind::View)
        .await
        .expect("view");
    assert_eq!(view, "CREATE VIEW grown_ups AS SELECT name FROM people;\n");
    assert!(s
        .catalog()
        .definition(&main.child("people"), NodeKind::View)
        .await
        .is_err());
    let table = s
        .catalog()
        .definition(&main.child("people"), NodeKind::Table)
        .await
        .expect("table");
    assert_eq!(
        table,
        "CREATE TABLE people (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         name TEXT NOT NULL DEFAULT 'x', note TEXT);\n"
    );
    assert_eq!(
        s.catalog()
            .count_rows(&main.child("people"))
            .await
            .expect("count"),
        1
    );
    assert!(s
        .catalog()
        .definition(&main.child("people"), NodeKind::Function)
        .await
        .is_err());

    let attack = "x'); DROP TABLE people; --";
    assert!(s.catalog().describe(&main.child(attack)).await.is_err());
    let still = run(s.as_mut(), "SELECT count(*) FROM people", 10)
        .await
        .expect("the table is still there");
    assert_eq!(scalar(&still), Some("1"));
}

#[tokio::test]
async fn columns_know_their_keys_and_values() {
    let mut s = session().await;
    execute(
        s.as_mut(),
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, region TEXT);
         CREATE TABLE orders (id INTEGER PRIMARY KEY, customer INTEGER REFERENCES customers,
             code TEXT UNIQUE);
         CREATE INDEX by_customer ON orders (customer);
         CREATE TRIGGER touch AFTER INSERT ON orders BEGIN SELECT 1; END;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200)
             INSERT INTO customers SELECT i, CASE WHEN i % 4 = 0 THEN 'eu' ELSE 'us' END FROM n;
         INSERT INTO customers VALUES (201, NULL);",
    )
    .await;
    let main = NodePath::root().child("main");

    let info = s
        .catalog()
        .describe(&main.child("orders"))
        .await
        .expect("describe");
    assert_eq!(info.foreign_keys.len(), 1);
    let key = &info.foreign_keys[0];
    assert_eq!(key.columns, ["customer"]);
    assert_eq!(key.target, main.child("customers"));
    assert_eq!(key.target_columns, ["id"]);
    let constraints: Vec<_> = info
        .constraints
        .iter()
        .map(|c| (c.kind, c.definition.as_str()))
        .collect();
    assert_eq!(
        constraints,
        [
            (
                ConstraintKind::PrimaryKey,
                "-- in CREATE TABLE \"orders\"\nPRIMARY KEY (\"id\")\n"
            ),
            (
                ConstraintKind::ForeignKey,
                "-- in CREATE TABLE \"orders\"\nFOREIGN KEY (\"customer\") REFERENCES \"customers\" (\"id\")\n"
            ),
            (
                ConstraintKind::Unique,
                "-- in CREATE TABLE \"orders\"\nUNIQUE (\"code\")\n"
            ),
        ]
    );
    let index = info
        .indexes
        .iter()
        .find(|i| i.name == "by_customer")
        .expect("index");
    assert_eq!(
        index.definition,
        "CREATE INDEX by_customer ON orders (customer);\n"
    );
    assert_eq!(info.triggers.len(), 1);
    assert_eq!(
        info.triggers[0].definition,
        "CREATE TRIGGER touch AFTER INSERT ON orders BEGIN SELECT 1; END;\n"
    );

    let customers = main.child("customers");
    let planner = s.catalog().column_stats(&customers, "region", false).await;
    assert_eq!(planner.expect("stats"), None);
    let profile = s
        .catalog()
        .column_stats(&customers, "region", true)
        .await
        .expect("profile")
        .expect("a profile");
    assert_eq!(profile.source, StatsSource::Profile);
    assert_eq!(profile.rows, Some(201));
    assert_eq!(profile.distinct, Some(Distinct::Count(2.0)));
    assert_eq!(
        (profile.min.as_deref(), profile.max.as_deref()),
        (Some("eu"), Some("us"))
    );
    let common: Vec<&str> = profile.common.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(common, ["us", "eu"]);
    assert!(s
        .catalog()
        .column_stats(&customers, "nope", true)
        .await
        .is_err());
}

#[tokio::test]
async fn an_error_later_in_a_script_fails_it() {
    let mut s = session().await;
    let out = run(
        s.as_mut(),
        "CREATE TABLE later_t (x INTEGER); SELECT * FROM no_such_table",
        10,
    )
    .await;
    assert!(out.is_err());
}
