//! The Redis session against a real server (Valkey in CI): commands and
//! their replies as tables, the key tree, describing and opening keys,
//! read-only connections and errors. Every test keeps to keys under a
//! prefix of its own in database 0.

use dfox_core::{
    catalog::{BrowseQuery, NodeKind, NodePath},
    driver::Driver,
    errors::DbError,
    models::connections::ConnectionParams,
    result::ResultSet,
    session::Session,
};
use dfox_driver_redis::RedisDriver;
use dfox_testkit::{execute, params_from_url, rows, run, scalar, server_url, unique};

const URL: &str = "DFOX_TEST_REDIS_URL";

fn params() -> Option<ConnectionParams> {
    server_url(URL).map(|url| params_from_url(&url))
}

async fn session() -> Option<Box<dyn Session>> {
    let params = params()?;
    Some(RedisDriver.open(&params).await.expect("open a session"))
}

/// Deletes every key under `prefix`.
async fn clean(s: &mut dyn Session, prefix: &str) {
    let found = run(s, &format!("SCAN 0 MATCH {prefix}* COUNT 10000"), 100_000)
        .await
        .expect("scan");
    // SCAN answers [cursor, [keys]]: the keys are the second row.
    let keys = rows(&found)
        .get(1)
        .and_then(|row| row[0])
        .map(|list| list.trim_matches(['[', ']']).replace(',', " "))
        .unwrap_or_default();
    if !keys.trim().is_empty() {
        execute(s, &format!("DEL {keys}")).await;
    }
}

#[tokio::test]
async fn replies_come_back_as_tables() {
    let Some(mut s) = session().await else {
        return;
    };
    let p = unique("dfox_it");
    let script = format!(
        "SET {p}:greeting \"hello there\"\n\
         HSET {p}:user name Ann age 30\n\
         RPUSH {p}:jobs a b c\n\
         ZADD {p}:board 1.5 ann 2 bob\n\
         XADD {p}:events 1-1 temp 20"
    );
    execute(s.as_mut(), &script).await;

    let get = run(s.as_mut(), &format!("GET {p}:greeting"), 100)
        .await
        .expect("get");
    assert_eq!(get.columns, ["value"]);
    assert_eq!(scalar(&get), Some("hello there"));

    let hash = run(s.as_mut(), &format!("HGETALL {p}:user"), 100)
        .await
        .expect("hgetall");
    assert_eq!(hash.columns, ["field", "value"]);
    let mut fields = rows(&hash);
    fields.sort();
    assert_eq!(
        fields,
        [[Some("age"), Some("30")], [Some("name"), Some("Ann")]]
    );

    let list = run(s.as_mut(), &format!("LRANGE {p}:jobs 0 -1"), 100)
        .await
        .expect("lrange");
    assert_eq!(rows(&list), [[Some("a")], [Some("b")], [Some("c")]]);

    let board = run(
        s.as_mut(),
        &format!("ZRANGE {p}:board 0 -1 WITHSCORES"),
        100,
    )
    .await
    .expect("zrange");
    assert_eq!(board.columns, ["member", "score"]);
    assert_eq!(
        rows(&board),
        [[Some("ann"), Some("1.5")], [Some("bob"), Some("2.0")]]
    );

    let events = run(s.as_mut(), &format!("XRANGE {p}:events - +"), 100)
        .await
        .expect("xrange");
    assert_eq!(events.columns, ["id", "fields"]);
    assert_eq!(rows(&events), [[Some("1-1"), Some("[temp, 20]")]]);

    let n = run(s.as_mut(), &format!("INCR {p}:n"), 100)
        .await
        .expect("incr");
    assert_eq!(scalar(&n), Some("1"));

    let info = run(s.as_mut(), "INFO server", 1000).await.expect("info");
    assert_eq!(info.columns, ["section", "key", "value"]);
    assert!(rows(&info).iter().any(|r| r[1] == Some("tcp_port")));

    // The row limit keeps the first rows of a long reply.
    let many: Vec<String> = (0..100).map(|i| i.to_string()).collect();
    execute(s.as_mut(), &format!("RPUSH {p}:many {}", many.join(" "))).await;
    let limited = run(s.as_mut(), &format!("LRANGE {p}:many 0 -1"), 10)
        .await
        .expect("limited");
    assert_eq!(limited.rows.len(), 10);

    clean(s.as_mut(), &p).await;
}

#[tokio::test]
async fn keys_show_as_a_tree_by_prefix() {
    let Some(mut s) = session().await else {
        return;
    };
    let p = unique("dfox_it");
    execute(
        s.as_mut(),
        &format!(
            "SET {p}:config on\nHSET {p}:user:1 name Ann\nHSET {p}:user:2 name Bob\n\
             RPUSH {p}:user:orders:7 x\nSET {p}:temp 1 EX 600"
        ),
    )
    .await;
    let catalog = s.catalog();

    let databases = catalog
        .children(&NodePath::root())
        .await
        .expect("databases");
    let zero = databases
        .iter()
        .find(|n| n.name == "0")
        .expect("database 0");
    assert_eq!(zero.kind, NodeKind::Database);

    let top = catalog
        .children(&NodePath::root().child("0"))
        .await
        .expect("top-level keys");
    let ours = top
        .iter()
        .find(|n| n.name == format!("{p}:"))
        .expect("our prefix");
    assert_eq!(ours.kind, NodeKind::KeyPrefix);
    assert_eq!(ours.detail.as_deref(), Some("5 keys"));

    let under: NodePath = ["0".to_string(), format!("{p}:")].into_iter().collect();
    let children = catalog.children(&under).await.expect("our keys");
    let shown: Vec<(&str, NodeKind, Option<&str>)> = children
        .iter()
        .map(|n| (n.name.as_str(), n.kind, n.detail.as_deref()))
        .collect();
    assert_eq!(
        shown,
        [
            ("user:", NodeKind::KeyPrefix, Some("3 keys")),
            ("config", NodeKind::Key, Some("string")),
            ("temp", NodeKind::Key, Some("string")),
        ]
    );

    let users = catalog
        .children(&under.child("user:"))
        .await
        .expect("users");
    let names: Vec<&str> = users.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, ["orders:", "1", "2"]);

    let user = under.child("user:").child("1");
    let info = catalog.describe(&user).await.expect("describe");
    assert_eq!(info.comment.as_deref(), Some("hash, 1 items, no expiry"));
    assert_eq!(catalog.count_rows(&user).await.expect("count"), 1);
    let temp = catalog.describe(&under.child("temp")).await.expect("temp");
    assert!(
        temp.comment
            .as_deref()
            .is_some_and(|c| c.contains("expires in")),
        "{temp:?}"
    );

    let orders = under.child("user:").child("orders:").child("7");
    let query = BrowseQuery {
        limit: Some(10),
        ..Default::default()
    };
    let stream = catalog.browse(&orders, query).await.expect("browse");
    let mut fetcher = dfox_core::arrow::Fetcher::new(
        stream,
        dfox_core::arrow::DEFAULT_BUDGET,
        dfox_core::arrow::Overflow::Stop,
    );
    fetcher.fetch().await.expect("rows");
    assert_eq!(fetcher.result().map(|r| r.row_count()), Some(1));

    let missing = catalog.describe(&under.child("nope")).await;
    assert!(missing.is_err());

    clean(s.as_mut(), &p).await;
}

#[tokio::test]
async fn read_only_connections_refuse_writes() {
    let Some(params) = params() else {
        return;
    };
    let mut s = RedisDriver
        .open(&ConnectionParams {
            read_only: true,
            ..params
        })
        .await
        .expect("open read-only");
    for reads in [
        "GET nothing",
        "SCAN 0",
        "CONFIG GET maxmemory",
        "INFO",
        "PING",
    ] {
        run(s.as_mut(), reads, 100)
            .await
            .unwrap_or_else(|err| panic!("{reads}: {err}"));
    }
    for writes in [
        "SET a 1",
        "DEL a",
        "CONFIG SET maxmemory 0",
        "FLUSHDB",
        "EVAL \"return 1\" 0",
    ] {
        let err = run(s.as_mut(), writes, 100).await.expect_err(writes);
        assert!(err.to_string().contains("read-only"), "{writes}: {err}");
    }
    // A script with a write in it runs nothing at all.
    let err = run(s.as_mut(), "PING\nSET a 1", 100)
        .await
        .expect_err("script");
    assert!(err.to_string().contains("SET"), "{err}");
}

#[tokio::test]
async fn errors_say_what_went_wrong() {
    let Some(params) = params() else {
        return;
    };
    let wrong = ConnectionParams {
        password: "wrong".into(),
        ..params.clone()
    };
    match RedisDriver.open(&wrong).await {
        Err(DbError::Connection(_)) => {}
        Err(other) => panic!("a wrong password is a connection error, not {other:?}"),
        Ok(_) => panic!("a wrong password connects"),
    }
    let text = ConnectionParams {
        database: Some("cache".into()),
        ..params
    };
    assert!(matches!(
        RedisDriver.open(&text).await,
        Err(DbError::Config(_))
    ));

    let Some(mut s) = session().await else {
        return;
    };
    let err = run(s.as_mut(), "NOTACOMMAND x", 100)
        .await
        .expect_err("unknown");
    assert!(
        err.to_string().to_lowercase().contains("unknown command"),
        "{err}"
    );
    let err = run(s.as_mut(), "GET \"open", 100).await.expect_err("quote");
    assert!(err.to_string().contains("line 1"), "{err}");
    // The session still works.
    let pong = run(s.as_mut(), "PING", 100).await.expect("ping");
    assert_eq!(scalar(&pong), Some("PONG"));
}
