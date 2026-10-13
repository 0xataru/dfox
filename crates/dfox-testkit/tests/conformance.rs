//! The driver test suite against the mock driver, and proof that the suite
//! catches a driver that gets things wrong.

use dfox_core::{catalog::NodePath, models::connections::ConnectionParams};
use dfox_testkit::{
    conformance::{check, Fixture},
    mock::MockDriver,
};

fn fixture() -> Fixture {
    Fixture {
        table: ["mock", "main", "people"].into_iter().collect::<NodePath>(),
        select_all: "SELECT * FROM people".into(),
        count: "COUNT people".into(),
        generate: |n| format!("GENERATE {n}"),
        long_running: "SLEEP 10000".into(),
        insert: |id, name| format!("INSERT {id} {name}"),
        begin: "BEGIN".into(),
        rollback: "ROLLBACK".into(),
        types: (
            "TYPES".into(),
            vec![
                "-42",
                "2.5",
                "true",
                "héllo",
                "123.45",
                "2022-01-08",
                "0xdead",
                r#"{"a":1}"#,
                "NULL",
            ],
        ),
        stream_rows: 1_000_000,
    }
}

#[tokio::test]
async fn the_mock_driver_passes() {
    check(&MockDriver, &ConnectionParams::default(), &fixture()).await;
}

#[tokio::test]
#[should_panic(expected = "values show as expected")]
async fn a_wrong_value_fails_the_suite() {
    let mut fixture = fixture();
    fixture.types.1[0] = "42";
    check(&MockDriver, &ConnectionParams::default(), &fixture).await;
}

#[tokio::test]
#[should_panic(expected = "is not under")]
async fn a_missing_table_fails_the_suite() {
    let mut fixture = fixture();
    fixture.table = ["mock", "main", "nobody"].into_iter().collect();
    check(&MockDriver, &ConnectionParams::default(), &fixture).await;
}
