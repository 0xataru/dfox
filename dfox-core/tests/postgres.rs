mod common;

use common::{only_row, params_from_url, rows, scalar, server_url, unique};
use dfox_core::{
    db::{postgres::PostgresClient, DbClient},
    errors::DbError,
    models::{
        connections::{ConnectionParams, DbType, TlsMode},
        schema::ObjectKind,
    },
    DbManager,
};

const URL: &str = "DFOX_TEST_POSTGRES_URL";

async fn client() -> Option<PostgresClient> {
    let url = server_url(URL)?;
    Some(PostgresClient::connect(&url).await.expect("connect"))
}

#[tokio::test]
async fn every_type_reads_as_postgres_prints_it() {
    let Some(db) = client().await else { return };

    let out = db
        .run(
            r#"
            SET TimeZone = 'UTC';
            SET lc_monetary = 'C';
            SELECT 1::int2, 2::int4, 3::int8, 1.5::numeric(10,2), 1.25::float4, 2.5::float8,
                   true, 'txt'::text, 'v'::varchar, 'c'::char(3), '\xdeadbeef'::bytea,
                   '2026-10-12'::date, '13:14:15'::time, '2026-10-12 13:14:15'::timestamp,
                   '2026-10-12 13:14:15+00'::timestamptz, '1 day 2 hours'::interval,
                   'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid, '{"a":1}'::json,
                   '{"a":1}'::jsonb, ARRAY[1,2,3], '192.168.0.1'::inet, '10.0.0.0/8'::cidr,
                   '08:00:2b:01:02:03'::macaddr, '(1,2)'::point, 12.5::money, NULL::int4
            "#,
            10,
        )
        .await
        .expect("run");

    assert_eq!(
        only_row(&out),
        [
            Some("1"),
            Some("2"),
            Some("3"),
            Some("1.50"),
            Some("1.25"),
            Some("2.5"),
            Some("true"),
            Some("txt"),
            Some("v"),
            Some("c  "),
            Some("0xdeadbeef"),
            Some("2026-10-12"),
            Some("13:14:15"),
            Some("2026-10-12 13:14:15"),
            Some("2026-10-12 13:14:15+00"),
            Some("1 day 02:00:00"),
            Some("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            Some(r#"{"a":1}"#),
            Some(r#"{"a": 1}"#),
            Some("{1,2,3}"),
            Some("192.168.0.1"),
            Some("10.0.0.0/8"),
            Some("08:00:2b:01:02:03"),
            Some("(1,2)"),
            Some("$12.50"),
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

    let values = db.run("VALUES (1, 'a'), (2, 'b')", 10).await.unwrap();
    assert_eq!(
        rows(&values),
        [[Some("1"), Some("a")], [Some("2"), Some("b")]]
    );

    let show = db.run("SHOW server_version", 10).await.unwrap();
    assert!(scalar(&show).is_some());

    let explain = db.run("EXPLAIN SELECT 1", 10).await.unwrap();
    assert_eq!(explain.columns, ["QUERY PLAN"]);

    db.run("CREATE TEMP TABLE returning_t (x int)", 0)
        .await
        .unwrap();
    let returning = db
        .run(
            "INSERT INTO returning_t VALUES (5), (6) RETURNING x * 10",
            10,
        )
        .await
        .unwrap();
    assert_eq!(rows(&returning), [[Some("50")], [Some("60")]]);
    assert_eq!(returning.rows_affected, 2);
}

#[tokio::test]
async fn session_state_carries_over_between_runs() {
    let Some(db) = client().await else { return };

    db.execute("SET application_name = 'dfox-it'")
        .await
        .unwrap();
    let name = db.run("SHOW application_name", 10).await.unwrap();
    assert_eq!(scalar(&name), Some("dfox-it"));

    db.execute("CREATE TEMP TABLE tx_t (x int)").await.unwrap();
    db.execute("BEGIN").await.unwrap();
    db.execute("INSERT INTO tx_t VALUES (1)").await.unwrap();
    let inside = db.run("SELECT count(*) FROM tx_t", 10).await.unwrap();
    assert_eq!(scalar(&inside), Some("1"));
    db.execute("ROLLBACK").await.unwrap();

    let after = db.run("SELECT count(*) FROM tx_t", 10).await.unwrap();
    assert_eq!(scalar(&after), Some("0"));
}

#[tokio::test]
async fn scripts_report_changes_and_the_last_result_set() {
    let Some(db) = client().await else { return };

    let out = db
        .run(
            "CREATE TEMP TABLE script_t (x int);
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
    // 3 inserted + 2 updated; SELECT row counts are included by Postgres too.
    assert!(
        out.rows_affected >= 5,
        "rows_affected = {}",
        out.rows_affected
    );
}

#[tokio::test]
async fn row_limit_truncates_but_runs_every_statement() {
    let Some(db) = client().await else { return };

    let limited = db.run("SELECT generate_series(1, 50)", 10).await.unwrap();
    assert_eq!(limited.rows.len(), 10);
    assert!(limited.truncated);

    let after = db
        .run("SELECT generate_series(1, 50); SELECT 'after'", 10)
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
        "CREATE TABLE {table} (id serial PRIMARY KEY, name text NOT NULL DEFAULT 'x', note varchar(20))"
    ))
    .await
    .unwrap();

    assert!(db.list_tables().await.unwrap().contains(&table));
    assert!(db
        .list_databases()
        .await
        .unwrap()
        .contains(&"postgres".to_string()));

    let schema = db.describe_table("public", &table).await.unwrap();
    let columns: Vec<_> = schema
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str(), c.is_nullable))
        .collect();
    assert_eq!(
        columns,
        [
            ("id", "integer", false),
            ("name", "text", false),
            ("note", "character varying(20)", true),
        ]
    );
    assert_eq!(schema.columns[1].default.as_deref(), Some("'x'::text"));

    let attack = format!("x'); DROP TABLE {table}; --");
    assert!(db
        .describe_table("public", &attack)
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
    let admin = PostgresClient::connect(&url).await.unwrap();
    let role = unique("dfox_it_role");
    let password = "p@ss:w/rd#1%?&'";

    admin
        .execute(&format!(
            "CREATE ROLE {role} LOGIN PASSWORD 'p@ss:w/rd#1%?&'''"
        ))
        .await
        .unwrap();

    let params = ConnectionParams {
        username: role.clone(),
        password: password.to_string(),
        ..params_from_url(&url)
    };
    let user = match PostgresClient::connect_params(&params).await {
        Ok(db) => {
            let user = db.run("SELECT current_user", 10).await;
            db.close().await;
            user.map(|out| scalar(&out).map(str::to_string))
        }
        Err(err) => Err(err),
    };
    admin.execute(&format!("DROP ROLE {role}")).await.unwrap();

    assert_eq!(user.unwrap().as_deref(), Some(role.as_str()));
}

#[tokio::test]
async fn schemas_and_their_objects() {
    let Some(db) = client().await else { return };
    let schema = unique("dfox_it_schema");

    db.execute(&format!(
        "CREATE SCHEMA {schema};
         CREATE TABLE {schema}.t (id int);
         INSERT INTO {schema}.t VALUES (1), (2), (3);
         ANALYZE {schema}.t;
         CREATE VIEW {schema}.v AS SELECT 1 AS one;
         CREATE MATERIALIZED VIEW {schema}.m AS SELECT 1 AS one;
         CREATE FUNCTION {schema}.f() RETURNS int LANGUAGE sql AS 'SELECT 1';
         CREATE PROCEDURE {schema}.p() LANGUAGE sql AS 'SELECT 1';"
    ))
    .await
    .unwrap();

    let schemas = db.list_schemas().await.unwrap();
    assert_eq!(schemas[0], "public");
    assert!(schemas.contains(&schema));
    assert!(!schemas
        .iter()
        .any(|s| s.starts_with("pg_") || s == "information_schema"));

    let objects: Vec<_> = db
        .list_objects(&schema)
        .await
        .unwrap()
        .into_iter()
        .map(|o| (o.name, o.kind, o.rows))
        .collect();
    assert_eq!(
        objects,
        [
            ("f".to_string(), ObjectKind::Function, None),
            ("m".to_string(), ObjectKind::MaterializedView, None),
            ("p".to_string(), ObjectKind::Procedure, None),
            ("t".to_string(), ObjectKind::Table, Some(3)),
            ("v".to_string(), ObjectKind::View, None),
        ]
    );

    let described = db.describe_table(&schema, "t").await.unwrap();
    assert_eq!(described.schema, schema);
    assert_eq!(described.columns[0].name, "id");
    assert!(db
        .describe_table("public", "t")
        .await
        .unwrap()
        .columns
        .is_empty());

    db.execute(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}

#[tokio::test]
async fn read_only_connections_refuse_writes() {
    let Some(url) = server_url(URL) else { return };
    let params = ConnectionParams {
        read_only: true,
        ..params_from_url(&url)
    };
    let db = DbManager::connect(&DbType::Postgres, &params)
        .await
        .unwrap();

    assert_eq!(scalar(&db.run("SELECT 1", 10).await.unwrap()), Some("1"));
    let err = db
        .execute("CREATE TABLE dfox_it_ro (i int)")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("read-only"), "{err}");
}

#[tokio::test]
async fn tls_can_be_required_or_turned_off() {
    let Some(url) = server_url(URL) else { return };
    // The test server has no TLS.
    let required = ConnectionParams {
        tls: TlsMode::Require,
        ..params_from_url(&url)
    };
    assert!(matches!(
        DbManager::connect(&DbType::Postgres, &required).await,
        Err(DbError::Connection(_))
    ));

    let disabled = ConnectionParams {
        tls: TlsMode::Disable,
        ..params_from_url(&url)
    };
    let db = DbManager::connect(&DbType::Postgres, &disabled)
        .await
        .unwrap();
    assert_eq!(scalar(&db.run("SELECT 1", 10).await.unwrap()), Some("1"));
}

#[tokio::test]
async fn ping_reaches_the_server_without_logging_in() {
    let Some(url) = server_url(URL) else { return };
    let params = ConnectionParams {
        password: "wrong".into(),
        ..params_from_url(&url)
    };
    assert!(DbManager::ping(&DbType::Postgres, &params).await.is_ok());

    let closed = ConnectionParams { port: 1, ..params };
    assert!(DbManager::ping(&DbType::Postgres, &closed).await.is_err());
}
