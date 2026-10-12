use super::*;
use crate::testing::{render, Harness, TestCtx};

fn store(rows: usize) -> RowStore {
    let mut store = RowStore::new(vec![
        ColumnMeta::new("id", "int4"),
        ColumnMeta::new("name", "text"),
        ColumnMeta::new("price", "numeric"),
        ColumnMeta::new("note", "text"),
    ]);
    store.extend((1..=rows).map(|i| {
        vec![
            OwnedCell::Int(i as i64),
            OwnedCell::Text(format!("item {i}").into()),
            OwnedCell::Decimal(format!("{}.50", i * 3).into()),
            if i % 2 == 0 {
                OwnedCell::Null
            } else {
                OwnedCell::Text("tab\there".into())
            },
        ]
    }));
    store.finish();
    store
}

#[test]
fn rows_stream_in_and_finish_with_a_message() {
    let mut harness = Harness::new();
    let mut results = ResultsPanel::default();
    results.run("  SELECT 1  ", &mut harness.ctx());
    let id = results.running.as_ref().expect("running").id;
    assert!(matches!(
        &harness.sent()[..],
        [DbCommand::Run { sql, row_limit: 100_000, .. }] if sql == "SELECT 1"
    ));

    results.on_columns(id, vec![ColumnMeta::new("n", "int4")]);
    results.on_rows(id, vec![vec![OwnedCell::Int(1)], vec![OwnedCell::Int(2)]]);
    assert_eq!(results.store.row_count(), 2);
    let stats = QueryStats {
        elapsed: Duration::from_millis(5),
        rows: 2,
        ..Default::default()
    };
    assert!(results.on_done(id, Ok(stats), &mut harness.ctx()));

    assert!(results.running.is_none());
    assert!(results.store.is_complete());
    assert_eq!(results.message.as_deref(), Some("2 row(s) · 5 ms"));
}

#[test]
fn errors_replace_the_rows() {
    let mut harness = Harness::new();
    let mut results = ResultsPanel::default();
    results.run("SELECT", &mut harness.ctx());
    let id = results.running.as_ref().expect("running").id;
    results.on_rows(id, vec![vec![OwnedCell::Int(1)]]);

    let refresh = results.on_done(id, Err(DbError::General("boom".into())), &mut harness.ctx());
    assert!(!refresh);
    assert_eq!(results.error.as_deref(), Some("Error: boom"));
    assert_eq!(results.store.row_count(), 0);
}

#[test]
fn cancel_is_sent_once_and_announced() {
    let mut harness = Harness::new();
    let mut results = ResultsPanel::default();
    results.run("SELECT pg_sleep(5)", &mut harness.ctx());
    let id = results.running.as_ref().expect("running").id;
    harness.sent();

    results.cancel(&mut harness.ctx());
    results.cancel(&mut harness.ctx());
    assert_eq!(harness.sent(), [DbCommand::Cancel { id }]);

    results.on_done(id, Err(DbError::Cancelled), &mut harness.ctx());
    assert_eq!(results.message.as_deref(), Some("Query cancelled"));
    assert_eq!(harness.toast_texts(), ["Query cancelled"]);
}

#[test]
fn an_abandoned_query_is_ignored() {
    let mut harness = Harness::new();
    let mut results = ResultsPanel::default();
    results.run("SELECT 1", &mut harness.ctx());
    let old = results.running.as_ref().expect("running").id;
    results.abandon(&mut harness.ctx());
    results.run("SELECT 2", &mut harness.ctx());

    results.on_columns(old, vec![ColumnMeta::new("stale", "")]);
    assert!(!results.on_done(old, Ok(QueryStats::default()), &mut harness.ctx()));
    assert!(results.store.columns().is_empty());
    assert!(results.running.is_some());
}

#[test]
fn a_second_query_waits_for_the_first() {
    let mut harness = Harness::new();
    let mut results = ResultsPanel::default();
    results.run("SELECT 1", &mut harness.ctx());
    results.run("SELECT 2", &mut harness.ctx());
    assert_eq!(harness.sent().len(), 1);
    assert_eq!(harness.toast_texts(), ["A query is already running"]);
}

#[test]
fn results_copy_as_tab_separated_lines() {
    let mut result = RowStore::new(vec![ColumnMeta::new("id", ""), ColumnMeta::new("id", "")]);
    result.push(vec![OwnedCell::Int(1), OwnedCell::Null]);
    result.push(vec![OwnedCell::Int(2), OwnedCell::Text("b".into())]);
    assert_eq!(results_as_tsv(&result, 0..2), "id\tid\n1\tNULL\n2\tb\n");
    assert_eq!(results_as_tsv(&result, 5..6), "id\tid\n\t\n");
}

#[test]
fn messages_and_durations_read_naturally() {
    let stats = |rows, affected, truncated| QueryStats {
        elapsed: Duration::from_millis(1250),
        rows,
        rows_affected: affected,
        truncated,
        ..Default::default()
    };
    assert_eq!(
        query_message(&stats(100, 0, true), true),
        "first 100 rows · 1.2 s"
    );
    assert_eq!(
        query_message(&stats(0, 3, false), false),
        "3 row(s) affected · 1.2 s"
    );
    assert_eq!(query_message(&stats(0, 0, false), false), "done · 1.2 s");
    assert_eq!(format_duration(Duration::from_millis(42)), "42 ms");
}

#[test]
fn grid_wide() {
    let test = TestCtx::new();
    let results = ResultsPanel {
        store: store(40),
        focused: true,
        message: Some("40 row(s) · 12 ms".into()),
        ..Default::default()
    };
    insta::assert_snapshot!(render(&test, 120, 20, |f, area, ctx| results.render(f, area, ctx)));
}

#[test]
fn grid_narrow_while_streaming() {
    let test = TestCtx::new();
    let results = ResultsPanel {
        store: store(3),
        running: Some(RunningQuery {
            id: 1,
            started: test.now,
            cancelling: false,
        }),
        ..Default::default()
    };
    insta::assert_snapshot!(render(&test, 80, 10, |f, area, ctx| results.render(f, area, ctx)));
}

#[test]
fn waiting_for_the_first_row() {
    let test = TestCtx::new();
    let results = ResultsPanel {
        running: Some(RunningQuery {
            id: 1,
            started: test.now,
            cancelling: false,
        }),
        ..Default::default()
    };
    insta::assert_snapshot!(render(&test, 60, 5, |f, area, ctx| results.render(f, area, ctx)));
}

#[test]
fn query_error() {
    let test = TestCtx::new();
    let results = ResultsPanel {
        error: Some("Database error: relation \"nope\" does not exist".into()),
        ..Default::default()
    };
    insta::assert_snapshot!(render(&test, 60, 5, |f, area, ctx| results.render(f, area, ctx)));
}
