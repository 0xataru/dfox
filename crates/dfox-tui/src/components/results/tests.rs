use super::*;
use crate::testing::{ints, render, Harness, TestCtx};

fn panel() -> ResultsPanel {
    ResultsPanel::new(Dialect::Postgres)
}

/// A panel showing `store`, as if a query had returned it.
fn showing(store: ArrowResultSet) -> ResultsPanel {
    ResultsPanel { store, ..panel() }
}

fn store(rows: usize) -> ArrowResultSet {
    let columns = vec![
        ColumnMeta::new("id", "int4"),
        ColumnMeta::new("name", "text"),
        ColumnMeta::new("price", "numeric"),
        ColumnMeta::new("note", "text"),
    ];
    let rows = (1..=rows)
        .map(|i| {
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
        })
        .collect();
    ArrowResultSet::from_cells(columns, rows)
}

/// Sends query `id` the integers `values`, as the engine would.
fn send_ints(results: &mut ResultsPanel, id: QueryId, values: &[i64]) {
    let (schema, batch) = ints(values);
    results.on_schema(id, schema);
    results.on_batch(id, batch);
}

#[test]
fn rows_stream_in_and_finish_with_a_message() {
    let mut harness = Harness::new();
    let mut results = panel();
    results.run(7, "SELECT 1", &mut harness.ctx());
    let id = results.running.as_ref().expect("running").id;
    assert!(matches!(
        &harness.sent()[..],
        [DbCommand::Run { id: 7, sql, row_limit: 100_000 }] if sql == "SELECT 1"
    ));

    send_ints(&mut results, id, &[1, 2]);
    assert_eq!(results.store.row_count(), 2);
    let stats = QueryStats {
        elapsed: Duration::from_millis(5),
        rows: 2,
        ..Default::default()
    };
    let finished = results
        .on_done(id, Ok(stats), &mut harness.ctx())
        .expect("finished");
    assert_eq!(
        finished,
        Finished {
            sql: "SELECT 1".into(),
            ok: true,
            rows: Some(2),
            elapsed: Duration::from_millis(5),
            changed: true,
        }
    );

    assert!(results.running.is_none());
    assert!(results.store.is_complete());
    assert_eq!(results.message.as_deref(), Some("2 row(s) · 5 ms"));
}

#[test]
fn errors_replace_the_rows() {
    let mut harness = Harness::new();
    let mut results = panel();
    results.run(1, "SELECT", &mut harness.ctx());
    send_ints(&mut results, 1, &[1]);

    let finished = results
        .on_done(1, Err(DbError::General("boom".into())), &mut harness.ctx())
        .expect("finished");
    assert!(!finished.ok && !finished.changed);
    assert_eq!(results.error.as_deref(), Some("Error: boom"));
    assert_eq!(results.store.row_count(), 0);
}

#[test]
fn cancel_is_sent_once_and_announced() {
    let mut harness = Harness::new();
    let mut results = panel();
    results.run(1, "SELECT pg_sleep(5)", &mut harness.ctx());
    let id = 1;
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
    let mut results = panel();
    results.run(1, "SELECT 1", &mut harness.ctx());
    results.abandon(&mut harness.ctx());
    results.run(2, "SELECT 2", &mut harness.ctx());

    results.on_schema(1, ints(&[]).0);
    assert_eq!(
        results.on_done(1, Ok(QueryStats::default()), &mut harness.ctx()),
        None
    );
    assert!(results.store.columns().is_empty());
    assert!(results.running.is_some());
}

#[test]
fn copy_takes_the_cell_or_the_selection() {
    let mut harness = Harness::new();
    let mut results = showing(store(3));
    results.handle(Action::Copy, &mut harness.ctx());
    assert_eq!(harness.clipboard.text.as_deref(), Some("1"));
    results.handle(Action::SelectRight, &mut harness.ctx());
    results.handle(Action::SelectDown, &mut harness.ctx());
    results.handle(Action::Copy, &mut harness.ctx());
    assert_eq!(
        harness.clipboard.text.as_deref(),
        Some("1\titem 1\n2\titem 2\n")
    );
    assert_eq!(harness.toast_texts(), ["Copied the cell", "Copied 2 rows"]);
    results.handle(Action::Back, &mut harness.ctx());
    results.handle(Action::SelectRight, &mut harness.ctx());
    results.handle(Action::Copy, &mut harness.ctx());
    assert_eq!(harness.toast_texts()[2], "Copied 2 cells");
}

#[test]
fn copy_as_inserts_into_the_queried_table() {
    let mut harness = Harness::new();
    let mut results = panel();
    results.run(1, "select * from shop.items", &mut harness.ctx());
    let (schema, batch) = crate::testing::frames(
        vec![ColumnMeta::new("id", "int4")],
        vec![vec![OwnedCell::Int(1)], vec![OwnedCell::Int(2)]],
    );
    results.on_schema(1, schema);
    results.on_batch(1, batch);
    results.on_done(1, Ok(QueryStats::default()), &mut harness.ctx());

    results.handle(Action::CopyAs, &mut harness.ctx());
    assert!(results.is_modal() && results.takes_back());
    // Keys that pick no format leave the menu open.
    results.handle(Action::Input('z'), &mut harness.ctx());
    results.handle(Action::Down, &mut harness.ctx());
    assert!(results.is_modal());
    results.handle(Action::Input('i'), &mut harness.ctx());
    assert!(!results.is_modal());
    assert_eq!(
        harness.clipboard.text.as_deref(),
        Some("INSERT INTO shop.items (id) VALUES\n  (1),\n  (2);\n")
    );

    let mut empty = panel();
    empty.handle(Action::CopyAs, &mut harness.ctx());
    empty.handle(Action::Copy, &mut harness.ctx());
    assert!(!empty.is_modal());
    assert_eq!(
        harness.toast_texts()[1..],
        ["No results to copy", "No results to copy"]
    );
}

#[test]
fn copy_as_without_a_known_table() {
    let mut harness = Harness::new();
    let mut results = showing(store(2));
    results.handle(Action::SelectDown, &mut harness.ctx());
    results.handle(Action::CopyAs, &mut harness.ctx());
    results.handle(Action::Input('c'), &mut harness.ctx());
    assert_eq!(harness.clipboard.text.as_deref(), Some("id\n1\n2\n"));
    results.handle(Action::CopyAs, &mut harness.ctx());
    results.handle(Action::Input('i'), &mut harness.ctx());
    assert!(harness
        .clipboard
        .text
        .as_deref()
        .is_some_and(|t| t.starts_with("INSERT INTO table_name (id)")));
}

#[test]
fn copy_as_menu() {
    let test = TestCtx::new();
    let mut results = showing(store(5));
    results.focused = true;
    results.handle(Action::CopyAs, &mut Harness::new().ctx());
    insta::assert_snapshot!(render(&test, 60, 12, |f, area, ctx| results.render(f, area, ctx)));
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
        focused: true,
        message: Some("40 row(s) · 12 ms".into()),
        ..showing(store(40))
    };
    insta::assert_snapshot!(render(&test, 120, 20, |f, area, ctx| results.render(f, area, ctx)));
}

#[test]
fn grid_narrow_while_streaming() {
    let test = TestCtx::new();
    let results = ResultsPanel {
        running: Some(RunningQuery {
            id: 1,
            started: test.now,
            cancelling: false,
        }),
        ..showing(store(3))
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
        ..panel()
    };
    insta::assert_snapshot!(render(&test, 60, 5, |f, area, ctx| results.render(f, area, ctx)));
}

#[test]
fn query_error() {
    let test = TestCtx::new();
    let results = ResultsPanel {
        error: Some("Database error: relation \"nope\" does not exist".into()),
        ..panel()
    };
    insta::assert_snapshot!(render(&test, 60, 5, |f, area, ctx| results.render(f, area, ctx)));
}

#[test]
fn inspector_beside_the_grid() {
    let test = TestCtx::new();
    let mut harness = Harness::new();
    let mut results = showing(store(3));
    results.focused = true;
    results.handle(Action::Down, &mut harness.ctx());
    results.handle(Action::Inspect, &mut harness.ctx());
    insta::assert_snapshot!(render(&test, 100, 16, |f, area, ctx| results.render(f, area, ctx)));
    // Moving to another row starts it from the top again.
    results.handle(Action::Down, &mut harness.ctx());
    assert!(results.takes_back());
    results.handle(Action::Back, &mut harness.ctx());
    assert!(!results.takes_back());
}

#[test]
fn inspector_over_a_narrow_grid() {
    let test = TestCtx::new();
    let store = ArrowResultSet::from_cells(
        vec![
            ColumnMeta::new("doc", "jsonb"),
            ColumnMeta::new("blob", "bytea"),
            ColumnMeta::new("gone", "text"),
        ],
        vec![vec![
            OwnedCell::Json("{\"a\":[1,2],\"b\":\"x\"}".into()),
            OwnedCell::Bytes(b"\x00\x01dfox".to_vec().into()),
            OwnedCell::Null,
        ]],
    );
    let mut results = showing(store);
    results.handle(Action::Inspect, &mut Harness::new().ctx());
    insta::assert_snapshot!(render(&test, 60, 20, |f, area, ctx| results.render(f, area, ctx)));
    assert!(results.scroll_inspector(30, 5, 3));
    assert!(!results.scroll_inspector(0, 0, 3));
}
