//! What any Postgres session must get right, from the first fixes (M0): column
//! order, statements that aren't SELECT, session state, scripts, row limits,
//! names that are attacks, passwords that look like URLs, every kind of
//! object, read-only connections, TLS and pings. Each value's text is checked
//! by the driver suite in `native.rs`.

use dfox_core::{
    catalog::{NodeKind, NodePath},
    driver::{ping, Driver},
    models::connections::{ConnectionParams, DbType, TlsMode},
    session::Session,
};
use dfox_driver_postgres::PostgresDriver;
use dfox_testkit::{execute, only_row, params_from_url, rows, run, scalar, server_url, unique};

const URL: &str = "DFOX_TEST_POSTGRES_URL";

async fn session() -> Option<Box<dyn Session>> {
    let url = server_url(URL)?;
    Some(
        PostgresDriver
            .open(&params_from_url(&url))
            .await
            .expect("open a session"),
    )
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

    let values = run(s, "VALUES (1, 'a'), (2, 'b')", 10)
        .await
        .expect("values");
    assert_eq!(
        rows(&values),
        [[Some("1"), Some("a")], [Some("2"), Some("b")]]
    );

    let show = run(s, "SHOW server_version", 10).await.expect("show");
    assert!(scalar(&show).is_some());

    let explain = run(s, "EXPLAIN SELECT 1", 10).await.expect("explain");
    assert_eq!(explain.columns, ["QUERY PLAN"]);

    execute(s, "CREATE TEMP TABLE returning_t (x int)").await;
    let returning = run(
        s,
        "INSERT INTO returning_t VALUES (5), (6) RETURNING x * 10",
        10,
    )
    .await
    .expect("returning");
    assert_eq!(rows(&returning), [[Some("50")], [Some("60")]]);
}

#[tokio::test]
async fn session_state_carries_over_between_runs() {
    let Some(mut s) = session().await else { return };
    let s = s.as_mut();

    execute(s, "SET application_name = 'dfox-it'").await;
    let name = run(s, "SHOW application_name", 10).await.expect("show");
    assert_eq!(scalar(&name), Some("dfox-it"));

    execute(s, "CREATE TEMP TABLE tx_t (x int)").await;
    execute(s, "BEGIN").await;
    execute(s, "INSERT INTO tx_t VALUES (1)").await;
    let inside = run(s, "SELECT count(*) FROM tx_t", 10)
        .await
        .expect("count");
    assert_eq!(scalar(&inside), Some("1"));
    execute(s, "ROLLBACK").await;

    let after = run(s, "SELECT count(*) FROM tx_t", 10)
        .await
        .expect("count");
    assert_eq!(scalar(&after), Some("0"));
}

#[tokio::test]
async fn scripts_report_changes_and_the_last_result_set() {
    let Some(mut s) = session().await else { return };
    let out = run(
        s.as_mut(),
        "CREATE TEMP TABLE script_t (x int);
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
    // 3 inserted and 2 updated.
    assert_eq!(out.rows_affected, 5);
}

#[tokio::test]
async fn row_limit_truncates_but_runs_every_statement() {
    let Some(mut s) = session().await else { return };
    let s = s.as_mut();

    let limited = run(s, "SELECT generate_series(1, 50)", 10)
        .await
        .expect("run");
    assert_eq!(limited.rows.len(), 10);

    execute(s, "CREATE TEMP TABLE after_t (x int)").await;
    let after = run(
        s,
        "SELECT generate_series(1, 50); INSERT INTO after_t VALUES (1); SELECT 'after'",
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
    let Some(mut s) = session().await else { return };
    let url = server_url(URL).expect("url");
    let database = params_from_url(&url).database.expect("a database");
    let table = unique("dfox_it_describe");
    execute(
        s.as_mut(),
        &format!(
            "CREATE TABLE {table} (id serial PRIMARY KEY, name text NOT NULL DEFAULT 'x', \
             note varchar(20))"
        ),
    )
    .await;

    let path =
        |name: &str| -> NodePath { [database.as_str(), "public", name].into_iter().collect() };
    let info = s.catalog().describe(&path(&table)).await.expect("describe");
    let columns: Vec<_> = info
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_name.as_str(), c.nullable))
        .collect();
    assert_eq!(
        columns,
        [
            ("id", "integer", false),
            ("name", "text", false),
            ("note", "character varying(20)", true),
        ]
    );
    assert_eq!(info.columns[1].default.as_deref(), Some("'x'::text"));

    let attack = format!("x'); DROP TABLE {table}; --");
    assert!(s.catalog().describe(&path(&attack)).await.is_err());
    let still = run(s.as_mut(), &format!("SELECT count(*) FROM {table}"), 10)
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
    let role = unique("dfox_it_role");
    let password = "p@ss:w/rd#1%?&'";
    execute(
        admin.as_mut(),
        &format!("CREATE ROLE {role} LOGIN PASSWORD 'p@ss:w/rd#1%?&'''"),
    )
    .await;

    let params = ConnectionParams {
        username: role.clone(),
        password: password.to_string(),
        ..params_from_url(&url)
    };
    let user = match PostgresDriver.open(&params).await {
        Ok(mut s) => run(s.as_mut(), "SELECT current_user", 10)
            .await
            .map(|out| scalar(&out).map(str::to_string)),
        Err(err) => Err(err),
    };
    execute(admin.as_mut(), &format!("DROP ROLE {role}")).await;
    assert_eq!(user.expect("logged in").as_deref(), Some(role.as_str()));
}

#[tokio::test]
async fn schemas_hold_every_kind_of_object() {
    let Some(mut s) = session().await else { return };
    let url = server_url(URL).expect("url");
    let database = params_from_url(&url).database.expect("a database");
    let schema = unique("dfox_it_schema");
    execute(
        s.as_mut(),
        &format!(
            "CREATE SCHEMA {schema};
             CREATE TABLE {schema}.t (id int);
             INSERT INTO {schema}.t VALUES (1), (2), (3);
             ANALYZE {schema}.t;
             CREATE VIEW {schema}.v AS SELECT 1 AS one;
             CREATE MATERIALIZED VIEW {schema}.m AS SELECT 1 AS one;
             CREATE FUNCTION {schema}.f() RETURNS int LANGUAGE sql AS 'SELECT 1';
             CREATE FUNCTION {schema}.g(a int, b text) RETURNS text LANGUAGE sql AS 'SELECT b';
             CREATE FUNCTION {schema}.g(a int) RETURNS int LANGUAGE sql AS 'SELECT a';
             CREATE PROCEDURE {schema}.p(n int) LANGUAGE sql AS 'SELECT 1';"
        ),
    )
    .await;

    let db = NodePath::root().child(database.clone());
    let schemas: Vec<String> = s
        .catalog()
        .children(&db)
        .await
        .expect("schemas")
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(schemas[0], "public");
    assert!(schemas.contains(&schema));
    assert!(!schemas
        .iter()
        .any(|s| s.starts_with("pg_") || s == "information_schema"));

    let objects: Vec<_> = s
        .catalog()
        .children(&db.child(schema.clone()))
        .await
        .expect("objects")
        .into_iter()
        .map(|n| (n.name, n.kind, n.detail))
        .collect();
    let object = |name: &str, kind, detail: Option<&str>| {
        (name.to_string(), kind, detail.map(str::to_string))
    };
    assert_eq!(
        objects,
        [
            object("f", NodeKind::Function, Some("() → integer")),
            object("g", NodeKind::Function, Some("2 overloads")),
            object("m", NodeKind::MaterializedView, None),
            object("p", NodeKind::Procedure, Some("(IN n integer)")),
            object("t", NodeKind::Table, Some("~3")),
            object("v", NodeKind::View, None),
        ]
    );

    let columns: Vec<_> = s
        .catalog()
        .all_columns(&db.child(schema.clone()))
        .await
        .expect("columns")
        .into_iter()
        .map(|c| (c.table, c.name, c.data_type))
        .collect();
    let column = |t: &str, n: &str, ty: &str| (t.to_string(), n.to_string(), ty.to_string());
    assert_eq!(
        columns,
        [
            column("m", "one", "integer"),
            column("t", "id", "integer"),
            column("v", "one", "integer"),
        ]
    );

    let at = |name: &str| db.child(schema.clone()).child(name);
    let definition = |text: Result<String, _>| text.expect("a definition");
    let view = definition(s.catalog().definition(&at("v"), NodeKind::View).await);
    assert_eq!(
        view,
        format!("CREATE OR REPLACE VIEW {schema}.v AS\n SELECT 1 AS one;\n")
    );
    let matview = definition(
        s.catalog()
            .definition(&at("m"), NodeKind::MaterializedView)
            .await,
    );
    assert!(matview.starts_with(&format!("CREATE MATERIALIZED VIEW {schema}.m AS\n")));
    let overloads = definition(s.catalog().definition(&at("g"), NodeKind::Function).await);
    assert_eq!(overloads.matches("CREATE OR REPLACE FUNCTION").count(), 2);
    assert!(overloads.ends_with("$function$;\n"), "{overloads}");
    let procedure = definition(s.catalog().definition(&at("p"), NodeKind::Procedure).await);
    assert!(procedure.starts_with(&format!(
        "CREATE OR REPLACE PROCEDURE {schema}.p(IN n integer)"
    )));
    // The kind picks between objects that share a name.
    assert!(s
        .catalog()
        .definition(&at("f"), NodeKind::Procedure)
        .await
        .is_err());
    assert!(s
        .catalog()
        .definition(&at("t"), NodeKind::Table)
        .await
        .is_err());

    execute(s.as_mut(), &format!("DROP SCHEMA {schema} CASCADE")).await;
}

#[tokio::test]
async fn tls_can_be_required_or_turned_off() {
    let Some(url) = server_url(URL) else { return };
    // The test server has no TLS.
    let required = ConnectionParams {
        tls: TlsMode::Require,
        ..params_from_url(&url)
    };
    let err = PostgresDriver.open(&required).await.err().expect("refused");
    assert!(err.to_string().contains("Connection"), "{err}");

    let disabled = ConnectionParams {
        tls: TlsMode::Disable,
        ..params_from_url(&url)
    };
    let mut s = PostgresDriver.open(&disabled).await.expect("open");
    let one = run(s.as_mut(), "SELECT 1", 10).await.expect("run");
    assert_eq!(scalar(&one), Some("1"));
}

#[tokio::test]
async fn ping_reaches_the_server_without_logging_in() {
    let Some(url) = server_url(URL) else { return };
    let params = ConnectionParams {
        password: "wrong".into(),
        ..params_from_url(&url)
    };
    assert!(ping(DbType::Postgres, &params).await.is_ok());

    let closed = ConnectionParams { port: 1, ..params };
    assert!(ping(DbType::Postgres, &closed).await.is_err());
}
