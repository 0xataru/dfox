//! The native session against a real server: the driver test suite, every
//! type the decoder knows, scripts, notices, errors, read-only connections
//! and a large catalog.

use std::time::{Duration, Instant};

use dfox_core::{
    arrow::{FetchStatus, Fetcher, Overflow, DEFAULT_BUDGET},
    catalog::NodePath,
    driver::Driver,
    models::connections::ConnectionParams,
    result::ResultSet,
    session::{Request, Session},
};
use dfox_driver_postgres::PostgresDriver;
use dfox_testkit::{
    conformance::{check, Fixture},
    params_from_url, server_url, unique,
};

const URL: &str = "DFOX_TEST_POSTGRES_URL";

async fn session(params: &ConnectionParams) -> Box<dyn Session> {
    PostgresDriver.open(params).await.expect("open a session")
}

/// Runs `sql` and returns the last result as text, and the notices.
async fn run(session: &mut dyn Session, sql: &str) -> (Vec<Vec<String>>, Vec<String>) {
    let stream = session.run(Request::new(sql)).await.expect("run");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    let status = fetcher
        .fetch()
        .await
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
    assert_eq!(status, FetchStatus::Done);
    let notices = fetcher.notices().to_vec();
    let Some(result) = fetcher.result() else {
        return (Vec::new(), notices);
    };
    let rows = (0..result.row_count())
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
        .collect();
    (rows, notices)
}

#[tokio::test]
async fn the_native_session_passes_the_driver_suite() {
    let Some(url) = server_url(URL) else { return };
    let params = params_from_url(&url);
    let schema = unique("dfox_conf");
    let mut setup = session(&params).await;
    run(
        setup.as_mut(),
        &format!(
            "CREATE SCHEMA {schema};
             CREATE TABLE {schema}.people (id integer PRIMARY KEY, name text);
             INSERT INTO {schema}.people VALUES (1, 'a'), (2, 'b'), (3, 'c')"
        ),
    )
    .await;

    let database = params.database.clone().expect("a database");
    let fixture = Fixture {
        table: [database.as_str(), schema.as_str(), "people"]
            .into_iter()
            .collect::<NodePath>(),
        key: "id",
        select_all: format!("SELECT * FROM {schema}.people ORDER BY id"),
        count: format!("SELECT count(*) FROM {schema}.people"),
        generate: |n| format!("SELECT i FROM generate_series(1, {n}) AS i"),
        long_running: "SELECT pg_sleep(10)".into(),
        insert: {
            // A function pointer can't capture the schema; the search path
            // finds the table instead.
            |id, name| format!("INSERT INTO people VALUES ({id}, '{name}')")
        },
        begin: format!("BEGIN; SET LOCAL search_path TO {schema}"),
        rollback: "ROLLBACK".into(),
        types: (
            "SELECT 1::int2, 2::int4, 3::int8, 1.50::numeric(10,2), 1.25::float4, 2.5::float8, \
             true, 'txt'::text, 'c'::char(3), '\\xdeadbeef'::bytea, \
             '2026-10-12'::date, 'infinity'::date, '13:14:15'::time, \
             '2026-10-12 13:14:15'::timestamp, '2026-10-12 13:14:15+00'::timestamptz, \
             '1 day 2 hours'::interval, 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid, \
             '{\"a\":1}'::json, '{\"a\":1}'::jsonb, ARRAY[1, NULL, 3], \
             '192.168.0.1'::inet, '10.0.0.0/8'::cidr, '08:00:2b:01:02:03'::macaddr, \
             '(1,2)'::point, 12.5::money, NULL::int4, 42::oid, 'x'::\"char\", B'101'::bit(3)"
                .into(),
            vec![
                "1",
                "2",
                "3",
                "1.50",
                "1.25",
                "2.5",
                "true",
                "txt",
                "c  ",
                "0xdeadbeef",
                "2026-10-12",
                "infinity",
                "13:14:15",
                "2026-10-12 13:14:15",
                "2026-10-12 13:14:15+00:00",
                "1 day 02:00:00",
                "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
                r#"{"a":1}"#,
                r#"{"a": 1}"#,
                "[1, , 3]",
                "192.168.0.1",
                "10.0.0.0/8",
                "08:00:2b:01:02:03",
                "(1,2)",
                "12.50",
                "NULL",
                "42",
                "x",
                "101",
            ],
        ),
        stream_rows: 1_000_000,
    };
    check(&PostgresDriver, &params, &fixture).await;

    run(setup.as_mut(), &format!("DROP SCHEMA {schema} CASCADE")).await;
}

#[tokio::test]
async fn scripts_run_every_statement_and_keep_notices() {
    let Some(url) = server_url(URL) else { return };
    let mut s = session(&params_from_url(&url)).await;
    let (rows, notices) = run(
        s.as_mut(),
        "CREATE TEMP TABLE t (n int); INSERT INTO t VALUES (1), (2);
         DO $$ BEGIN RAISE NOTICE 'hello %', 42; END $$;
         SELECT n FROM t ORDER BY n; SELECT 'last' AS word",
    )
    .await;
    assert_eq!(rows, [["last"]], "the last result set wins");
    assert_eq!(notices, ["NOTICE: hello 42"]);
    // The temp table lives on in the session.
    assert_eq!(run(s.as_mut(), "SELECT count(*) FROM t").await.0, [["2"]]);
}

#[tokio::test]
async fn enums_domains_and_unknown_types_decode() {
    let Some(url) = server_url(URL) else { return };
    let mut s = session(&params_from_url(&url)).await;
    let mood = unique("dfox_mood");
    let positive = unique("dfox_positive");
    run(
        s.as_mut(),
        &format!(
            "CREATE TYPE {mood} AS ENUM ('sad', 'happy');
             CREATE DOMAIN {positive} AS int CHECK (VALUE > 0)"
        ),
    )
    .await;
    let (rows, _) = run(
        s.as_mut(),
        &format!(
            "SELECT 'happy'::{mood}, 5::{positive}, ARRAY['sad', 'happy']::{mood}[], \
             'a fat cat'::tsvector, '[1,5)'::int4range"
        ),
    )
    .await;
    assert_eq!(rows[0][0], "happy");
    assert_eq!(rows[0][1], "5");
    assert_eq!(rows[0][2], "[sad, happy]");
    // Types without a decoder keep their bytes rather than becoming NULL.
    assert!(rows[0][3].starts_with("0x"), "{}", rows[0][3]);
    assert!(rows[0][4].starts_with("0x"), "{}", rows[0][4]);
    run(
        s.as_mut(),
        &format!("DROP DOMAIN {positive}; DROP TYPE {mood}"),
    )
    .await;
}

#[tokio::test]
async fn errors_read_like_the_server_wrote_them() {
    let Some(url) = server_url(URL) else { return };
    let mut s = session(&params_from_url(&url)).await;
    let stream = s
        .run(Request::new("SELECT * FROM nope_not_here"))
        .await
        .expect("run");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    let err = fetcher.fetch().await.expect_err("an error");
    assert_eq!(
        err.to_string(),
        "Error: ERROR: relation \"nope_not_here\" does not exist"
    );
    // The session is still usable.
    assert_eq!(run(s.as_mut(), "SELECT 1").await.0, [["1"]]);
}

#[tokio::test]
async fn read_only_sessions_refuse_writes() {
    let Some(url) = server_url(URL) else { return };
    let params = ConnectionParams {
        read_only: true,
        ..params_from_url(&url)
    };
    let mut s = session(&params).await;
    let stream = s
        .run(Request::new("CREATE TEMP TABLE x (n int)"))
        .await
        .expect("run");
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    let err = fetcher.fetch().await.expect_err("refused");
    assert!(err.to_string().contains("read-only"), "{err}");
}

/// The plan's target: a catalog walk of 5,000 tables in under 300 ms.
#[tokio::test]
async fn a_large_schema_lists_quickly() {
    let Some(url) = server_url(URL) else { return };
    let params = params_from_url(&url);
    let mut s = session(&params).await;
    let schema = unique("dfox_big");
    run(s.as_mut(), &format!("CREATE SCHEMA {schema}")).await;
    // One transaction can't lock 5,000 new tables; make them 500 at a time.
    for start in (1..=5000).step_by(500) {
        run(
            s.as_mut(),
            &format!(
                "DO $$ BEGIN
                   FOR i IN {start}..{} LOOP
                     EXECUTE format('CREATE TABLE {schema}.t%s (id int PRIMARY KEY, name text)', i);
                   END LOOP;
                 END $$",
                start + 499
            ),
        )
        .await;
    }
    let database = params.database.clone().expect("a database");
    let path: NodePath = [database.as_str(), schema.as_str()].into_iter().collect();
    let started = Instant::now();
    let tables = s.catalog().children(&path).await.expect("children");
    let took = started.elapsed();
    assert_eq!(tables.len(), 5000);
    eprintln!("listing 5000 tables took {took:?}");
    assert!(took < Duration::from_millis(300), "{took:?}");
    for start in (1..=5000).step_by(500) {
        let tables: Vec<String> = (start..start + 500)
            .map(|i| format!("{schema}.t{i}"))
            .collect();
        run(s.as_mut(), &format!("DROP TABLE {}", tables.join(", "))).await;
    }
    run(s.as_mut(), &format!("DROP SCHEMA {schema}")).await;
}

/// `verify-full` against a server with a certificate for "localhost" from a
/// test CA (`scripts/tls-postgres.sh`).
#[tokio::test]
async fn verify_full_checks_the_certificate_and_the_host() {
    use dfox_core::models::connections::TlsMode;

    let Some(url) = server_url("DFOX_TEST_POSTGRES_TLS_URL") else {
        return;
    };
    let Some(ca) = server_url("DFOX_TEST_POSTGRES_CA") else {
        return;
    };
    let base = params_from_url(&url);
    let with = |host: &str, tls, ca_file: Option<&str>| ConnectionParams {
        host: host.to_string(),
        tls,
        ca_file: ca_file.map(str::to_string),
        ..base.clone()
    };
    let encrypted = "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()";

    let mut s = session(&with("localhost", TlsMode::VerifyFull, Some(&ca))).await;
    assert_eq!(run(s.as_mut(), encrypted).await.0, [["true"]]);

    // A CA nobody trusts, and a name the certificate doesn't carry.
    for (host, ca_file, why) in [
        ("localhost", None, "UnknownIssuer"),
        ("127.0.0.1", Some(ca.as_str()), "not valid for name"),
    ] {
        let err = PostgresDriver
            .open(&with(host, TlsMode::VerifyFull, ca_file))
            .await
            .err()
            .expect("refused");
        assert!(err.to_string().contains(why), "{host}: {err}");
    }

    // require encrypts without checking; disable doesn't encrypt.
    let mut s = session(&with("127.0.0.1", TlsMode::Require, None)).await;
    assert_eq!(run(s.as_mut(), encrypted).await.0, [["true"]]);
    let mut s = session(&with("127.0.0.1", TlsMode::Disable, None)).await;
    assert_eq!(run(s.as_mut(), encrypted).await.0, [["false"]]);
}

/// The plan's target: a running query stops within 300 ms of the cancel.
#[tokio::test]
async fn cancel_stops_a_query_quickly() {
    use futures_util::StreamExt;

    let Some(url) = server_url(URL) else { return };
    let mut s = session(&params_from_url(&url)).await;
    let handle = s.cancel_handle();
    let mut frames = s
        .run(Request::new("SELECT pg_sleep(30)"))
        .await
        .expect("run");
    let cancelled = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let asked = Instant::now();
        handle.cancel().await.expect("cancel");
        asked
    });
    let mut last = None;
    while let Some(frame) = frames.next().await {
        last = Some(frame);
    }
    let took = cancelled.await.expect("join").elapsed();
    assert!(
        matches!(last, Some(Err(dfox_core::errors::DbError::Cancelled))),
        "{last:?}"
    );
    eprintln!("cancelled in {took:?}");
    assert!(took < Duration::from_millis(300), "{took:?}");
}
