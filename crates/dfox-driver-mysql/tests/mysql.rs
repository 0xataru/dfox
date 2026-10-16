//! What any MySQL session must get right, from the first fixes (M0): column
//! order, statements that aren't SELECT, session state, scripts, row limits,
//! names that are attacks, passwords that look like URLs and every kind of
//! object. Each value's text is checked by the driver suite in `native.rs`.

use dfox_core::{
    catalog::{NodeKind, NodePath},
    driver::Driver,
    models::connections::ConnectionParams,
    session::Session,
};
use dfox_driver_mysql::MySqlDriver;
use dfox_testkit::{execute, only_row, params_from_url, rows, run, scalar, server_url, unique};

const URL: &str = "DFOX_TEST_MYSQL_URL";

async fn session() -> Option<Box<dyn Session>> {
    let url = server_url(URL)?;
    Some(
        MySqlDriver
            .open(&params_from_url(&url))
            .await
            .expect("open a session"),
    )
}

fn database() -> String {
    let url = server_url(URL).expect("url");
    params_from_url(&url).database.expect("a database")
}

#[tokio::test]
async fn columns_keep_query_order_and_duplicates() {
    let Some(mut s) = session().await else { return };
    let out = run(s.as_mut(), "SELECT 3 AS c, 1 AS a, 2 AS b, 4 AS a", 10)
        .await
        .expect("run");
    assert_eq!(out.columns, ["c", "a", "b", "a"]);
    assert_eq!(only_row(&out), [Some("3"), Some("1"), Some("2"), Some("4")]);
}

#[tokio::test]
async fn non_select_statements_return_rows() {
    let Some(mut s) = session().await else { return };
    let s = s.as_mut();

    let with = run(s, "WITH t AS (SELECT 7 AS x) SELECT x FROM t", 10)
        .await
        .expect("with");
    assert_eq!(scalar(&with), Some("7"));

    let show = run(s, "SHOW DATABASES", 100).await.expect("show");
    assert!(rows(&show).iter().any(|r| r[0] == Some("mysql")));

    let explain = run(s, "EXPLAIN SELECT 1", 10).await.expect("explain");
    assert!(!explain.columns.is_empty());

    let values = run(s, "VALUES ROW(1, 'a'), ROW(2, 'b')", 10)
        .await
        .expect("values");
    assert_eq!(
        rows(&values),
        [[Some("1"), Some("a")], [Some("2"), Some("b")]]
    );
}

#[tokio::test]
async fn session_state_carries_over_between_runs() {
    let Some(mut s) = session().await else { return };
    let s = s.as_mut();

    execute(s, "SET @dfox = 42").await;
    let var = run(s, "SELECT @dfox", 10).await.expect("run");
    assert_eq!(scalar(&var), Some("42"));

    execute(s, "CREATE TEMPORARY TABLE tx_t (x INT) ENGINE=InnoDB").await;
    execute(s, "START TRANSACTION").await;
    execute(s, "INSERT INTO tx_t VALUES (1)").await;
    execute(s, "ROLLBACK").await;
    let after = run(s, "SELECT COUNT(*) FROM tx_t", 10)
        .await
        .expect("count");
    assert_eq!(scalar(&after), Some("0"));
}

#[tokio::test]
async fn scripts_report_changes_and_the_last_result_set() {
    let Some(mut s) = session().await else { return };
    let out = run(
        s.as_mut(),
        "CREATE TEMPORARY TABLE script_t (x INT);
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
    let Some(mut s) = session().await else { return };
    let s = s.as_mut();
    let fifty = "WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 50) \
                 SELECT n FROM s";

    let limited = run(s, fifty, 10).await.expect("run");
    assert_eq!(limited.rows.len(), 10);

    execute(s, "CREATE TEMPORARY TABLE after_t (x INT)").await;
    let after = run(
        s,
        &format!("{fifty}; INSERT INTO after_t VALUES (1); SELECT 'after'"),
        10,
    )
    .await
    .expect("run");
    assert_eq!(scalar(&after), Some("after"));
    let count = run(s, "SELECT COUNT(*) FROM after_t", 10)
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
    let Some(mut s) = session().await else { return };
    let database = database();
    let table = unique("dfox_it_describe");
    execute(
        s.as_mut(),
        &format!(
            "CREATE TABLE {table} (id INT AUTO_INCREMENT PRIMARY KEY, \
             name VARCHAR(20) NOT NULL DEFAULT 'x', note TEXT)"
        ),
    )
    .await;

    let databases: Vec<String> = s
        .catalog()
        .children(&NodePath::root())
        .await
        .expect("databases")
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert!(databases.contains(&"mysql".to_string()));

    let path = |name: &str| -> NodePath { [database.as_str(), name].into_iter().collect() };
    let info = s.catalog().describe(&path(&table)).await.expect("describe");
    let columns: Vec<_> = info
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_name.as_str(), c.nullable))
        .collect();
    assert_eq!(
        columns,
        [
            ("id", "int", false),
            ("name", "varchar(20)", false),
            ("note", "text", true),
        ]
    );
    assert_eq!(info.columns[1].default.as_deref(), Some("x"));

    let attack = format!("x'; DROP TABLE {table}; --");
    assert!(s.catalog().describe(&path(&attack)).await.is_err());
    let still = run(s.as_mut(), &format!("SELECT COUNT(*) FROM {table}"), 10)
        .await
        .expect("the table is still there");
    assert_eq!(scalar(&still), Some("0"));

    execute(s.as_mut(), &format!("DROP TABLE {table}")).await;
}

#[tokio::test]
async fn passwords_with_url_characters_connect() {
    let Some(mut admin) = session().await else {
        return;
    };
    let url = server_url(URL).expect("url");
    let user = unique("dfox_it_user");
    let password = "p@ss:w/rd#1%?&'";
    execute(
        admin.as_mut(),
        &format!("CREATE USER '{user}'@'%' IDENTIFIED BY 'p@ss:w/rd#1%?&'''"),
    )
    .await;

    let params = ConnectionParams {
        username: user.clone(),
        password: password.to_string(),
        database: None,
        ..params_from_url(&url)
    };
    let current = match MySqlDriver.open(&params).await {
        Ok(mut s) => run(s.as_mut(), "SELECT CURRENT_USER()", 10)
            .await
            .map(|out| scalar(&out).map(str::to_string)),
        Err(err) => Err(err),
    };
    execute(admin.as_mut(), &format!("DROP USER '{user}'@'%'")).await;
    assert_eq!(current.expect("logged in"), Some(format!("{user}@%")));
}

#[tokio::test]
async fn databases_hold_every_kind_of_object() {
    let Some(mut s) = session().await else { return };
    let database = database();
    let prefix = unique("dfox_it_obj");
    execute(
        s.as_mut(),
        &format!(
            "CREATE TABLE {prefix}_t (id INT);
             CREATE VIEW {prefix}_v AS SELECT 1 AS one;
             CREATE FUNCTION {prefix}_f(a INT, b VARCHAR(10)) RETURNS INT DETERMINISTIC RETURN a;
             CREATE PROCEDURE {prefix}_p(IN n INT, OUT m INT) SET m = n;
             CREATE PROCEDURE {prefix}_q() SELECT 1;"
        ),
    )
    .await;

    let db = NodePath::root().child(database.clone());
    let objects: Vec<_> = s
        .catalog()
        .children(&db)
        .await
        .expect("objects")
        .into_iter()
        .filter(|n| n.name.starts_with(&prefix))
        .map(|n| {
            let detail = n.detail.filter(|_| n.kind != NodeKind::Table);
            (
                n.name.trim_start_matches(&prefix).to_string(),
                n.kind,
                detail,
            )
        })
        .collect();
    let object = |name: &str, kind, detail: Option<&str>| {
        (name.to_string(), kind, detail.map(str::to_string))
    };
    assert_eq!(
        objects,
        [
            object(
                "_f",
                NodeKind::Function,
                Some("(a int, b varchar(10)) → int")
            ),
            object("_p", NodeKind::Procedure, Some("(n int, OUT m int)")),
            object("_q", NodeKind::Procedure, Some("()")),
            object("_t", NodeKind::Table, None),
            object("_v", NodeKind::View, None),
        ]
    );

    let at = |name: &str| db.child(format!("{prefix}{name}"));
    let view = s
        .catalog()
        .definition(&at("_v"), NodeKind::View)
        .await
        .expect("view");
    assert!(view.starts_with("CREATE "), "{view}");
    assert!(
        view.contains(&format!("VIEW `{prefix}_v` AS select 1 AS `one`")),
        "{view}"
    );
    assert!(view.ends_with(";\n"));
    let function = s
        .catalog()
        .definition(&at("_f"), NodeKind::Function)
        .await
        .expect("function");
    assert!(
        function.contains(&format!("FUNCTION `{prefix}_f`(a INT, b VARCHAR(10))")),
        "{function}"
    );
    assert!(s
        .catalog()
        .definition(&at("_f"), NodeKind::Procedure)
        .await
        .is_err());
    assert!(s
        .catalog()
        .definition(&at("_t"), NodeKind::Table)
        .await
        .is_err());

    let columns: Vec<_> = s
        .catalog()
        .all_columns(&db)
        .await
        .expect("columns")
        .into_iter()
        .filter(|c| c.table.starts_with(&prefix))
        .map(|c| {
            (
                c.table.trim_start_matches(&prefix).to_string(),
                c.name,
                c.data_type,
            )
        })
        .collect();
    assert_eq!(
        columns,
        [
            ("_t".to_string(), "id".to_string(), "int".to_string()),
            ("_v".to_string(), "one".to_string(), "int".to_string()),
        ]
    );

    execute(
        s.as_mut(),
        &format!(
            "DROP TABLE {prefix}_t; DROP VIEW {prefix}_v;
             DROP FUNCTION {prefix}_f; DROP PROCEDURE {prefix}_p; DROP PROCEDURE {prefix}_q;"
        ),
    )
    .await;
}
