//! The Cassandra session against a real ScyllaDB: the driver test suite,
//! every type, keys and indexes in the catalog, definitions, and read-only
//! connections.

use dfox_core::{
    catalog::{ConstraintKind, NodeKind, NodePath},
    driver::Driver,
    models::connections::ConnectionParams,
    session::Session,
};
use dfox_driver_cassandra::CassandraDriver;
use dfox_testkit::{
    conformance::{check, Fixture},
    execute, only_row, params_from_url, rows, run, scalar, server_url, unique,
};

const URL: &str = "DFOX_TEST_CASSANDRA_URL";
/// Rows the streaming check reads.
const NUMBERS: usize = 20_000;

/// A session in a new keyspace of this run's own, which `drop_keyspace`
/// removes.
async fn keyspace(prefix: &str) -> Option<(String, ConnectionParams, Box<dyn Session>)> {
    let url = server_url(URL)?;
    let keyspace = unique(prefix);
    let mut admin = CassandraDriver
        .open(&params_from_url(&url))
        .await
        .expect("open a session");
    execute(
        admin.as_mut(),
        &format!(
            "CREATE KEYSPACE {keyspace} WITH replication = \
             {{'class': 'SimpleStrategy', 'replication_factor': 1}}"
        ),
    )
    .await;
    let params = ConnectionParams {
        database: Some(keyspace.clone()),
        ..params_from_url(&url)
    };
    let session = CassandraDriver
        .open(&params)
        .await
        .expect("open in the keyspace");
    Some((keyspace, params, session))
}

async fn drop_keyspace(s: &mut dyn Session, keyspace: &str) {
    execute(s, &format!("DROP KEYSPACE {keyspace}")).await;
}

#[tokio::test]
async fn passes_the_driver_suite() {
    let Some((keyspace, params, mut s)) = keyspace("dfox_it").await else {
        return;
    };
    let s = s.as_mut();
    execute(
        s,
        "CREATE TABLE people (id int PRIMARY KEY, name text);
         INSERT INTO people (id, name) VALUES (1, 'a');
         INSERT INTO people (id, name) VALUES (2, 'b');
         INSERT INTO people (id, name) VALUES (3, 'c');
         CREATE TABLE numbers (n int PRIMARY KEY);
         CREATE TABLE kinds (
             id int PRIMARY KEY, b boolean, i int, big bigint, small smallint, d double,
             dec decimal, v varint, t text, ts timestamp, day date, at time, u uuid,
             ip inet, raw blob, l list<int>, m map<text, int>, s set<text>, tu tuple<int, text>
         );
         INSERT INTO kinds (id, b, i, big, small, d, dec, v, t, ts, day, at, u, ip, raw, l, m, s, tu)
         VALUES (1, true, 7, 9007199254740993, -3, 1.5, 12.340, 123456789012345678901234567890,
                 'text', '2024-01-02T03:04:05Z', '2024-01-02', '13:45:00.123',
                 0f2d8b5c-6a1e-4c3b-9d7f-8e9a0b1c2d3e, '10.0.0.1', 0xdeadbeef,
                 [1, 2], {'a': 1}, {'x'}, (1, 'one'))",
    )
    .await;
    // Rows to stream, in batches the server takes at once.
    for start in (0..NUMBERS).step_by(500) {
        let inserts: Vec<String> = (start..start + 500)
            .map(|n| format!("INSERT INTO numbers (n) VALUES ({n})"))
            .collect();
        execute(
            s,
            &format!("BEGIN UNLOGGED BATCH {}; APPLY BATCH", inserts.join("; ")),
        )
        .await;
    }
    let fixture = Fixture {
        table: [keyspace.as_str(), "people"].into_iter().collect::<NodePath>(),
        key: "id",
        select_all: "SELECT * FROM people".into(),
        count: "SELECT COUNT(*) FROM people".into(),
        generate: |n| format!("SELECT n FROM numbers LIMIT {n}"),
        long_running: String::new(),
        insert: |id, name| format!("INSERT INTO people (id, name) VALUES ({id}, '{name}')"),
        begin: String::new(),
        rollback: String::new(),
        types: (
            "SELECT b, i, big, small, d, dec, v, t, ts, day, at, u, ip, raw, l, m, s, tu FROM kinds".into(),
            vec![
                "true",
                "7",
                "9007199254740993",
                "-3",
                "1.5",
                "12.340",
                "123456789012345678901234567890",
                "text",
                "2024-01-02 03:04:05+00:00",
                "2024-01-02",
                "13:45:00.123",
                "0f2d8b5c-6a1e-4c3b-9d7f-8e9a0b1c2d3e",
                "10.0.0.1",
                "0xdeadbeef",
                "[1,2]",
                r#"{"a":1}"#,
                r#"["x"]"#,
                r#"[1,"one"]"#,
            ],
        ),
        stream_rows: NUMBERS,
    };
    check(&CassandraDriver, &params, &fixture).await;
    drop_keyspace(s, &keyspace).await;
}

#[tokio::test]
async fn the_catalog_shows_keys_indexes_and_definitions() {
    let Some((keyspace, _, mut s)) = keyspace("dfox_it_catalog").await else {
        return;
    };
    execute(
        s.as_mut(),
        "CREATE TABLE events (
             tenant text, region text, day date, seq int, kind text, payload text,
             total int STATIC,
             PRIMARY KEY ((tenant, region), day, seq)
         ) WITH CLUSTERING ORDER BY (day DESC, seq ASC) AND comment = 'what happened';
         CREATE INDEX by_kind ON events (kind);
         CREATE MATERIALIZED VIEW by_payload AS SELECT tenant, region, day, seq, payload FROM events
             WHERE payload IS NOT NULL AND tenant IS NOT NULL AND region IS NOT NULL
               AND day IS NOT NULL AND seq IS NOT NULL
             PRIMARY KEY (payload, tenant, region, day, seq);
         INSERT INTO events (tenant, region, day, seq, kind, payload)
             VALUES ('t', 'eu', '2024-01-02', 1, 'login', 'x')",
    )
    .await;
    let ks = NodePath::root().child(keyspace.clone());
    let objects: Vec<_> = s
        .catalog()
        .children(&ks)
        .await
        .expect("tables")
        .into_iter()
        .map(|n| (n.name, n.kind))
        .collect();
    assert_eq!(
        objects,
        [
            ("by_payload".to_string(), NodeKind::MaterializedView),
            ("events".to_string(), NodeKind::Table),
        ],
        "the index's own view is hidden"
    );

    let info = s
        .catalog()
        .describe(&ks.child("events"))
        .await
        .expect("describe");
    let columns: Vec<_> = info
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.primary_key, c.comment.as_deref()))
        .collect();
    assert_eq!(
        columns,
        [
            ("tenant", true, Some("partition key")),
            ("region", true, Some("partition key")),
            ("day", true, Some("clustering key, DESC")),
            ("seq", true, Some("clustering key, ASC")),
            ("total", false, Some("static")),
            ("kind", false, None),
            ("payload", false, None),
        ]
    );
    assert_eq!(info.comment.as_deref(), Some("what happened"));
    assert_eq!(info.constraints[0].kind, ConstraintKind::PrimaryKey);
    assert_eq!(
        info.constraints[0].definition,
        "PRIMARY KEY ((tenant, region), day, seq)\n"
    );
    assert_eq!(info.indexes.len(), 1);
    assert_eq!(info.indexes[0].name, "by_kind");
    assert_eq!(info.indexes[0].columns, ["kind"]);

    let create = s
        .catalog()
        .definition(&ks.child("events"), NodeKind::Table)
        .await
        .expect("definition");
    assert!(
        create.contains(&format!("CREATE TABLE {keyspace}.events (")),
        "{create}"
    );
    assert!(
        create.contains("CLUSTERING ORDER BY (day DESC, seq ASC)"),
        "{create}"
    );
    let view = s
        .catalog()
        .definition(&ks.child("by_payload"), NodeKind::MaterializedView)
        .await
        .expect("view");
    assert!(view.contains("CREATE MATERIALIZED VIEW"), "{view}");

    assert_eq!(
        s.catalog()
            .count_rows(&ks.child("events"))
            .await
            .expect("count"),
        1
    );
    let columns = s.catalog().all_columns(&ks).await.expect("columns");
    let events: Vec<&str> = columns
        .iter()
        .filter(|c| c.table == "events")
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        events,
        ["tenant", "region", "day", "seq", "total", "kind", "payload"]
    );
    drop_keyspace(s.as_mut(), &keyspace).await;
}

#[tokio::test]
async fn scripts_run_statement_by_statement() {
    let Some((keyspace, params, mut s)) = keyspace("dfox_it_script").await else {
        return;
    };
    let out = run(
        s.as_mut(),
        "CREATE TABLE t (id int PRIMARY KEY, v text);
         INSERT INTO t (id, v) VALUES (1, 'one');
         -- a comment between statements
         SELECT v FROM t WHERE id = 1",
        10,
    )
    .await
    .expect("run");
    assert_eq!(only_row(&out), [Some("one")]);
    // A conditional write answers with a row.
    let applied = run(
        s.as_mut(),
        "INSERT INTO t (id, v) VALUES (1, 'again') IF NOT EXISTS",
        10,
    )
    .await
    .expect("insert");
    assert_eq!(rows(&applied)[0][0], Some("false"));
    let err = run(s.as_mut(), "SELECT * FROM nope", 10)
        .await
        .expect_err("an error");
    assert!(err.to_string().contains("nope"), "{err}");

    let mut read_only = CassandraDriver
        .open(&ConnectionParams {
            read_only: true,
            ..params
        })
        .await
        .expect("open");
    let err = run(
        read_only.as_mut(),
        "INSERT INTO t (id, v) VALUES (2, 'x')",
        10,
    )
    .await
    .expect_err("refused");
    assert!(err.to_string().contains("read-only"), "{err}");
    let count = run(read_only.as_mut(), "SELECT COUNT(*) FROM t", 10)
        .await
        .expect("count");
    assert_eq!(scalar(&count), Some("1"));
    drop_keyspace(s.as_mut(), &keyspace).await;
}

#[tokio::test]
async fn a_wrong_password_fails_to_connect() {
    let Some(url) = server_url(URL) else { return };
    let err = CassandraDriver
        .open(&ConnectionParams {
            password: "wrong".into(),
            ..params_from_url(&url)
        })
        .await
        .err()
        .expect("refused");
    assert!(err.to_string().starts_with("Connection error"), "{err}");
}
