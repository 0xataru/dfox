use dfox_core::{
    db::DbClient,
    driver::Driver,
    models::{connections::ConnectionParams, schema::ObjectKind},
};
use dfox_driver_mysql::{MySqlClient, MySqlDriver};
use dfox_testkit::{only_row, params_from_url, rows, scalar, server_url, unique};

const URL: &str = "DFOX_TEST_MYSQL_URL";

async fn client() -> Option<MySqlClient> {
    let url = server_url(URL)?;
    Some(MySqlClient::connect(&url).await.expect("connect"))
}

#[tokio::test]
async fn every_type_reads_as_mysql_prints_it() {
    let Some(db) = client().await else { return };

    db.execute(
        "CREATE TEMPORARY TABLE types_t (
            ti TINYINT, si SMALLINT, mi MEDIUMINT, i INT, bi BIGINT,
            tiu TINYINT UNSIGNED, biu BIGINT UNSIGNED, b BOOLEAN,
            d DECIMAL(10,2), f FLOAT, db DOUBLE,
            c CHAR(3), vc VARCHAR(10), t TEXT, bin VARBINARY(4), bl BLOB,
            dt DATE, tm TIME, dtm DATETIME, ts TIMESTAMP NULL, y YEAR,
            bt BIT(8), e ENUM('a','b'), st SET('x','y'), j JSON, n INT
        )",
    )
    .await
    .unwrap();
    db.execute(
        "INSERT INTO types_t VALUES (
            -5, -300, 70000, -2000000000, 9000000000000000000,
            255, 18446744073709551615, TRUE,
            1.5, 1.25, 2.5,
            'c', 'v', 'txt', 0xDEADBEEF, 'hello',
            '2026-10-12', '13:14:15', '2026-10-12 13:14:15', '2026-10-12 13:14:15', 2026,
            b'10100101', 'b', 'x,y', '{\"a\":1}', NULL
        )",
    )
    .await
    .unwrap();

    let out = db.run("SELECT * FROM types_t", 10).await.unwrap();
    assert_eq!(
        only_row(&out),
        [
            Some("-5"),
            Some("-300"),
            Some("70000"),
            Some("-2000000000"),
            Some("9000000000000000000"),
            Some("255"),
            Some("18446744073709551615"),
            Some("true"),
            Some("1.50"),
            Some("1.25"),
            Some("2.5"),
            Some("c"),
            Some("v"),
            Some("txt"),
            Some("0xdeadbeef"),
            Some("hello"),
            Some("2026-10-12"),
            Some("13:14:15"),
            Some("2026-10-12 13:14:15"),
            Some("2026-10-12 13:14:15"),
            Some("2026"),
            Some("0xa5"),
            Some("b"),
            Some("x,y"),
            Some(r#"{"a": 1}"#),
            None,
        ]
    );
}

#[tokio::test]
async fn columns_keep_query_order_and_duplicates() {
    let Some(db) = client().await else { return };

    let out = db
        .run("SELECT 3 AS c, 1 AS a, 2 AS b, 4 AS a", 10)
        .await
        .unwrap();

    assert_eq!(out.columns, ["c", "a", "b", "a"]);
    assert_eq!(only_row(&out), [Some("3"), Some("1"), Some("2"), Some("4")]);
}

#[tokio::test]
async fn non_select_statements_return_rows() {
    let Some(db) = client().await else { return };

    let with = db
        .run("WITH t AS (SELECT 7 AS x) SELECT x FROM t", 10)
        .await
        .unwrap();
    assert_eq!(scalar(&with), Some("7"));

    let show = db.run("SHOW DATABASES", 100).await.unwrap();
    assert!(rows(&show).iter().any(|r| r[0] == Some("mysql")));

    let explain = db.run("EXPLAIN SELECT 1", 10).await.unwrap();
    assert!(!explain.columns.is_empty());

    let values = db.run("VALUES ROW(1, 'a'), ROW(2, 'b')", 10).await.unwrap();
    assert_eq!(
        rows(&values),
        [[Some("1"), Some("a")], [Some("2"), Some("b")]]
    );
}

#[tokio::test]
async fn session_state_carries_over_between_runs() {
    let Some(db) = client().await else { return };

    db.execute("SET @dfox = 42").await.unwrap();
    let var = db.run("SELECT @dfox", 10).await.unwrap();
    assert_eq!(scalar(&var), Some("42"));

    db.execute("CREATE TEMPORARY TABLE tx_t (x INT) ENGINE=InnoDB")
        .await
        .unwrap();
    db.execute("START TRANSACTION").await.unwrap();
    db.execute("INSERT INTO tx_t VALUES (1)").await.unwrap();
    db.execute("ROLLBACK").await.unwrap();

    let after = db.run("SELECT COUNT(*) FROM tx_t", 10).await.unwrap();
    assert_eq!(scalar(&after), Some("0"));
}

#[tokio::test]
async fn scripts_report_changes_and_the_last_result_set() {
    let Some(db) = client().await else { return };

    let out = db
        .run(
            "CREATE TEMPORARY TABLE script_t (x INT);
             INSERT INTO script_t VALUES (1), (2), (3);
             UPDATE script_t SET x = x + 1 WHERE x > 1;
             SELECT 'first';
             SELECT x FROM script_t ORDER BY x",
            10,
        )
        .await
        .unwrap();

    assert_eq!(out.columns, ["x"]);
    assert_eq!(rows(&out), [[Some("1")], [Some("3")], [Some("4")]]);
    assert_eq!(out.rows_affected, 5);
}

#[tokio::test]
async fn row_limit_truncates_but_runs_every_statement() {
    let Some(db) = client().await else { return };
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
async fn errors_are_returned_and_the_session_survives() {
    let Some(db) = client().await else { return };

    assert!(db.run("SELECT * FROM no_such_table", 10).await.is_err());
    let out = db.run("SELECT 1", 10).await.unwrap();
    assert_eq!(scalar(&out), Some("1"));
}

#[tokio::test]
async fn metadata_and_describe_are_injection_safe() {
    let Some(db) = client().await else { return };
    let table = unique("dfox_it_describe");

    db.execute(&format!(
        "CREATE TABLE {table} (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(20) NOT NULL DEFAULT 'x', note TEXT)"
    ))
    .await
    .unwrap();

    assert!(db.list_tables().await.unwrap().contains(&table));
    assert!(db
        .list_databases()
        .await
        .unwrap()
        .contains(&"mysql".to_string()));

    let database = db.list_schemas().await.unwrap().remove(0);
    let schema = db.describe_table(&database, &table).await.unwrap();
    let columns: Vec<_> = schema
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str(), c.is_nullable))
        .collect();
    assert_eq!(
        columns,
        [
            ("id", "int", false),
            ("name", "varchar(20)", false),
            ("note", "text", true),
        ]
    );
    assert_eq!(schema.columns[1].default.as_deref(), Some("x"));

    let attack = format!("x'; DROP TABLE {table}; --");
    assert!(db
        .describe_table(&database, &attack)
        .await
        .unwrap()
        .columns
        .is_empty());
    assert!(db.list_tables().await.unwrap().contains(&table));

    db.execute(&format!("DROP TABLE {table}")).await.unwrap();
}

#[tokio::test]
async fn passwords_with_url_characters_connect() {
    let Some(url) = server_url(URL) else { return };
    let admin = MySqlClient::connect(&url).await.unwrap();
    let user = unique("dfox_it_user");
    let password = "p@ss:w/rd#1%?&'";

    admin
        .execute(&format!(
            "CREATE USER '{user}'@'%' IDENTIFIED BY 'p@ss:w/rd#1%?&'''"
        ))
        .await
        .unwrap();

    let params = ConnectionParams {
        username: user.clone(),
        password: password.to_string(),
        database: None,
        ..params_from_url(&url)
    };
    let current = match MySqlClient::connect_params(&params).await {
        Ok(db) => {
            let current = db.run("SELECT CURRENT_USER()", 10).await;
            db.close().await;
            current.map(|out| scalar(&out).map(str::to_string))
        }
        Err(err) => Err(err),
    };
    admin
        .execute(&format!("DROP USER '{user}'@'%'"))
        .await
        .unwrap();

    assert_eq!(current.unwrap(), Some(format!("{user}@%")));
}

#[tokio::test]
async fn the_database_is_the_only_schema() {
    let Some(db) = client().await else { return };
    let prefix = unique("dfox_it_obj");

    db.execute(&format!(
        "CREATE TABLE {prefix}_t (id INT);
         CREATE VIEW {prefix}_v AS SELECT 1 AS one;
         CREATE FUNCTION {prefix}_f() RETURNS INT DETERMINISTIC RETURN 1;
         CREATE PROCEDURE {prefix}_p() SELECT 1;"
    ))
    .await
    .unwrap();

    let schemas = db.list_schemas().await.unwrap();
    assert_eq!(schemas, ["dfox"]);
    let objects: Vec<_> = db
        .list_objects("dfox")
        .await
        .unwrap()
        .into_iter()
        .filter(|o| o.name.starts_with(&prefix))
        .map(|o| (o.name.trim_start_matches(&prefix).to_string(), o.kind))
        .collect();
    assert_eq!(
        objects,
        [
            ("_f".to_string(), ObjectKind::Function),
            ("_p".to_string(), ObjectKind::Procedure),
            ("_t".to_string(), ObjectKind::Table),
            ("_v".to_string(), ObjectKind::View),
        ]
    );

    let columns: Vec<_> = db
        .list_columns("dfox")
        .await
        .unwrap()
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

    db.execute(&format!(
        "DROP TABLE {prefix}_t; DROP VIEW {prefix}_v;
         DROP FUNCTION {prefix}_f; DROP PROCEDURE {prefix}_p;"
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn read_only_connections_refuse_writes() {
    let Some(url) = server_url(URL) else { return };
    let admin = MySqlClient::connect(&url).await.unwrap();
    let table = unique("dfox_it_ro");
    admin
        .execute(&format!("CREATE TABLE {table} (i INT)"))
        .await
        .unwrap();

    let params = ConnectionParams {
        read_only: true,
        ..params_from_url(&url)
    };
    let db = MySqlDriver.connect(&params).await.unwrap();
    let err = db
        .execute(&format!("INSERT INTO {table} VALUES (1)"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("READ ONLY"), "{err}");
    assert_eq!(
        scalar(
            &db.run(&format!("SELECT COUNT(*) FROM {table}"), 10)
                .await
                .unwrap()
        ),
        Some("0")
    );

    admin.execute(&format!("DROP TABLE {table}")).await.unwrap();
}
