//! `dfox export` and `dfox copy` against real servers: the code the
//! commands run, from URLs as they are given on the command line.
//!
//! Skips itself unless `DFOX_TEST_POSTGRES_URL` and `DFOX_TEST_MYSQL_URL` are
//! set (`make test-it`).

use std::sync::Arc;

use dfox_core::{
    driver::Drivers,
    engine::{self, Command, CopyTarget},
};
use dfox_testkit::{server_url, unique};
use dfox_tui::{
    components::copy::target_path,
    connections::{secrets::Keychain, Store},
    headless,
};

#[tokio::test]
async fn export_and_copy_run_from_urls_against_postgres_and_mysql() {
    let (Some(pg), Some(my)) = (
        server_url("DFOX_TEST_POSTGRES_URL"),
        server_url("DFOX_TEST_MYSQL_URL"),
    ) else {
        return;
    };
    let (engine, mut events) = engine::spawn(Drivers::new(vec![
        Arc::new(dfox_driver_postgres::PostgresDriver),
        Arc::new(dfox_driver_mysql::MySqlDriver),
    ]));
    let store = Store::default();
    // URLs carry their passwords: nothing is looked up in the keychain.
    let resolve = |url: &str| headless::resolve(url, &store, &Keychain, &[]).expect("a URL");
    let (from, from_params) = resolve(&pg);
    let (to, to_params) = resolve(&my);
    let rows = "SELECT g AS id, 'n' || g AS label FROM generate_series(1, 100) g";

    // dfox export <postgres> <query> -o rows.csv
    let file = std::env::temp_dir().join(format!("{}.csv", unique("dfox_headless")));
    let export = Command::Export {
        id: 1,
        sql: rows.into(),
        file: file.clone(),
    };
    let stats = headless::run(
        &engine,
        &mut events,
        &from,
        from_params.clone(),
        export,
        false,
    )
    .await
    .expect("export");
    assert_eq!(stats.rows, 100);
    let text = std::fs::read_to_string(&file).expect("the file");
    assert_eq!(text.lines().count(), 101, "a header and 100 rows");
    assert!(text.starts_with("id,label\n1,n1\n"), "{text}");
    let _ = std::fs::remove_file(&file);

    // dfox copy <postgres> <query> <mysql> <table> --create
    let table = unique("dfox_headless");
    let copy = Command::Copy {
        id: 2,
        sql: rows.into(),
        target: Box::new(CopyTarget {
            db_type: to.driver,
            params: to_params.clone(),
            table: target_path(&to, &table),
            create: true,
        }),
    };
    let stats = headless::run(&engine, &mut events, &from, from_params, copy, false)
        .await
        .expect("copy");
    assert_eq!(stats.rows, 100);

    // The rows are in MySQL: read them back the way export would.
    let check = std::env::temp_dir().join(format!("{table}.csv"));
    let count = Command::Export {
        id: 3,
        sql: format!("SELECT count(*) AS n, sum(id) AS total FROM {table}"),
        file: check.clone(),
    };
    headless::run(&engine, &mut events, &to, to_params.clone(), count, false)
        .await
        .expect("count");
    assert_eq!(
        std::fs::read_to_string(&check).expect("the count"),
        "n,total\n100,5050\n"
    );
    let _ = std::fs::remove_file(&check);
    let drop = Command::Run {
        id: 4,
        sql: format!("DROP TABLE {table}"),
        row_limit: 0,
    };
    headless::run(&engine, &mut events, &to, to_params, drop, false)
        .await
        .expect("drop");
}
