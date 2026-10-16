use super::*;
use crate::testing::{render, Harness, TestCtx};
use dfox_core::{
    catalog::{Node, NodeKind, NodePath},
    engine::{Command as DbCommand, QueryStats},
    result::ResultSet,
};

fn workspace() -> Workspace {
    Workspace::new("local", DbType::Postgres, "shop")
}

/// Answers the explorer as Postgres would: `postgres` and `shop`, which
/// holds `public` and its objects.
fn answer_catalog(w: &mut Workspace, harness: &mut Harness) {
    let databases = vec![
        Node::new(NodeKind::Database, "postgres", false),
        Node::new(NodeKind::Database, "shop", true),
    ];
    w.on_children(&NodePath::root(), Ok(databases), &mut harness.ctx());
    w.on_children(
        &["shop"].into_iter().collect(),
        Ok(vec![Node::new(NodeKind::Schema, "public", true)]),
        &mut harness.ctx(),
    );
    w.on_children(
        &["shop", "public"].into_iter().collect(),
        Ok(crate::testing::sample_objects()),
        &mut harness.ctx(),
    );
}

/// The id of the query the workspace just sent.
fn sent_query(harness: &mut Harness) -> QueryId {
    match harness.sent().as_slice() {
        [DbCommand::Run { id, .. }] => *id,
        other => panic!("expected one query, got {other:?}"),
    }
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn tab_cycles_focus_and_text_goes_to_the_editor() {
    let mut harness = Harness::new();
    let mut w = workspace();
    assert!(w.wants_text());

    w.handle(Action::Input('x'), &mut harness.ctx());
    assert_eq!(w.editor().text(), "x");

    w.handle(Action::NextFocus, &mut harness.ctx());
    assert_eq!(w.focused(), Panel::Results);
    assert!(!w.wants_text());
    w.handle(Action::PrevFocus, &mut harness.ctx());
    w.handle(Action::PrevFocus, &mut harness.ctx());
    assert_eq!(w.focused(), Panel::Explorer);
}

#[test]
fn execute_runs_the_statement_under_the_cursor() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.editor_mut().set_text("select 1;\nselect 2;");
    w.handle(Action::Execute, &mut harness.ctx());
    assert!(matches!(
        &harness.sent()[..],
        [DbCommand::Run { sql, .. }] if sql == "select 2;"
    ));
}

#[test]
fn execute_all_runs_the_script() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.editor_mut().set_text("select 1;\nselect 2;");
    w.handle(Action::ExecuteAll, &mut harness.ctx());
    assert!(matches!(
        &harness.sent()[..],
        [DbCommand::Run { sql, .. }] if sql == "select 1;\nselect 2;"
    ));
    w.handle(Action::ExecuteAll, &mut harness.ctx());
    assert_eq!(harness.toast_texts(), ["A query is already running"]);
}

#[test]
fn nothing_to_run_says_so() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.handle(Action::Execute, &mut harness.ctx());
    assert!(harness.sent().is_empty());
    assert_eq!(harness.toast_texts(), ["Nothing to run"]);
}

#[test]
fn results_go_to_the_tab_that_ran_the_query() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.editor_mut().set_text("select 1 as n");
    w.handle(Action::Execute, &mut harness.ctx());
    let id = sent_query(&mut harness);

    w.handle(Action::NewTab, &mut harness.ctx());
    assert_eq!(w.tab_count(), 2);
    w.editor_mut().set_text("select 2");
    w.handle(Action::Execute, &mut harness.ctx());
    assert_eq!(
        harness.toast_texts(),
        ["A query is already running in Query 1"]
    );

    let mut ctx = harness.ctx();
    let (schema, batch) = crate::testing::ints(&[1]);
    w.on_query_event(EngineEvent::Schema { id, schema }, &mut ctx);
    w.on_query_event(EngineEvent::Batch { id, batch }, &mut ctx);
    let changed = w.on_query_event(
        EngineEvent::QueryDone {
            id,
            outcome: Ok(QueryStats {
                rows: 1,
                ..Default::default()
            }),
        },
        &mut ctx,
    );
    assert!(changed);
    assert_eq!(w.results().store.row_count(), 0);
    w.handle(Action::PrevTab, &mut harness.ctx());
    assert_eq!(w.results().store.row_count(), 1);

    let entry = &harness.history.search("", 10)[0].clone();
    assert_eq!(
        (
            entry.sql.as_str(),
            entry.database.as_str(),
            entry.ok,
            entry.rows
        ),
        ("select 1 as n", "shop", true, Some(1))
    );
    // Events for unknown queries are dropped.
    assert!(!w.on_query_event(
        EngineEvent::QueryDone {
            id: 99,
            outcome: Ok(QueryStats::default())
        },
        &mut harness.ctx()
    ));
}

#[test]
fn tabs_open_switch_and_close() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.editor_mut().set_text("one");
    w.handle(Action::NewTab, &mut harness.ctx());
    w.editor_mut().set_text("two");
    w.handle(Action::NextTab, &mut harness.ctx());
    assert_eq!(w.editor().text(), "one");
    w.handle(Action::NextTab, &mut harness.ctx());
    assert_eq!(w.editor().text(), "two");

    w.handle(Action::CloseTab, &mut harness.ctx());
    assert_eq!((w.tab_count(), w.editor().text()), (1, "one".to_string()));
    // The last tab is emptied rather than closed.
    w.handle(Action::CloseTab, &mut harness.ctx());
    assert_eq!((w.tab_count(), w.editor().text()), (1, String::new()));
}

#[test]
fn closing_a_tab_cancels_its_query() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.editor_mut().set_text("select pg_sleep(5)");
    w.handle(Action::Execute, &mut harness.ctx());
    let id = sent_query(&mut harness);
    w.handle(Action::CloseTab, &mut harness.ctx());
    assert_eq!(harness.sent(), [DbCommand::Cancel { id }]);
    assert!(w.running().is_none());
}

#[test]
fn copy_cancels_a_running_query() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.editor_mut().set_text("SELECT pg_sleep(5)");
    w.handle(Action::Execute, &mut harness.ctx());
    harness.sent();

    w.handle(Action::Copy, &mut harness.ctx());
    assert!(matches!(&harness.sent()[..], [DbCommand::Cancel { .. }]));
}

#[test]
fn copy_goes_to_the_focused_panel() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.editor_mut().set_text("select 1");
    w.handle(Action::Copy, &mut harness.ctx());
    assert_eq!(harness.clipboard.text.as_deref(), Some("select 1"));

    w.handle(Action::NextFocus, &mut harness.ctx());
    w.handle(Action::Copy, &mut harness.ctx());
    assert_eq!(harness.toast_texts()[1], "No results to copy");
}

#[test]
fn back_clears_a_grid_selection_before_asking_to_quit() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.handle(Action::Debug, &mut harness.ctx());
    w.handle(Action::SelectDown, &mut harness.ctx());
    assert_eq!(w.handle(Action::Back, &mut harness.ctx()), None);
    assert_eq!(
        w.handle(Action::Back, &mut harness.ctx()),
        Some(Command::ConfirmQuit)
    );
}

#[test]
fn copy_as_takes_the_next_key() {
    let mut harness = Harness::new();
    harness.log.push("connected".into());
    let mut w = workspace();
    w.handle(Action::Debug, &mut harness.ctx());
    w.handle(Action::CopyAs, &mut harness.ctx());
    assert!(w.wants_text());
    // While the menu is open, Back closes it instead of quitting.
    assert_eq!(w.handle(Action::Back, &mut harness.ctx()), None);
    w.handle(Action::CopyAs, &mut harness.ctx());
    w.handle(Action::Input('j'), &mut harness.ctx());
    let copied = harness.clipboard.text.clone().expect("copied");
    assert!(
        copied.starts_with("[\n  {\"#\": 1, \"Event\": "),
        "{copied}"
    );
    assert_eq!(
        harness.toast_texts().last().map(String::as_str),
        Some("Copied 1 row(s) as JSON")
    );
}

#[test]
fn history_focuses_the_editor() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.handle(Action::NextFocus, &mut harness.ctx());
    w.handle(Action::History, &mut harness.ctx());
    assert_eq!(w.focused(), Panel::Editor);
    // Keys go to the search, Back included.
    assert_eq!(w.handle(Action::Back, &mut harness.ctx()), None);
}

#[test]
fn paste_goes_to_the_editor_only() {
    let mut w = workspace();
    assert!(w.paste("select\n1"));
    assert_eq!(w.editor().text(), "select\n1");
    w.handle(Action::NextFocus, &mut Harness::new().ctx());
    assert!(!w.paste("x"));
}

#[test]
fn the_mouse_focuses_and_scrolls_panels() {
    let test = TestCtx::new();
    let mut harness = Harness::new();
    let mut w = workspace();
    w.editor_mut().set_text("select 1\nfrom t");
    w.handle(Action::Debug, &mut harness.ctx());
    render(&test, 100, 30, |f, area, ctx| w.render(f, area, ctx));
    let [explorer, editor, results] = w.areas.get();

    w.mouse(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            explorer.x + 3,
            explorer.y + 3,
        ),
        &mut harness.ctx(),
    );
    assert_eq!(w.focused(), Panel::Explorer);
    w.mouse(
        mouse(MouseEventKind::ScrollDown, explorer.x + 3, explorer.y + 3),
        &mut harness.ctx(),
    );

    w.mouse(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            editor.x + 6,
            editor.y + 1,
        ),
        &mut harness.ctx(),
    );
    assert_eq!(w.focused(), Panel::Editor);
    assert_eq!(w.editor().buffer.cursor(), (0, 1));
    w.mouse(
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            editor.x + 8,
            editor.y + 2,
        ),
        &mut harness.ctx(),
    );
    assert_eq!(
        w.editor().buffer.selected_text().as_deref(),
        Some("elect 1\nfro")
    );
    w.mouse(
        mouse(MouseEventKind::ScrollUp, editor.x + 6, editor.y + 2),
        &mut harness.ctx(),
    );
    assert_eq!(w.editor().buffer.cursor().0, 0);

    harness.log.push("more".into());
    w.mouse(
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            results.x + 6,
            results.y + 3,
        ),
        &mut harness.ctx(),
    );
    assert_eq!(w.focused(), Panel::Results);
    w.mouse(
        mouse(MouseEventKind::ScrollRight, results.x + 6, results.y + 3),
        &mut harness.ctx(),
    );
    assert_eq!(w.results().grid.cursor(), (0, 1));
    w.mouse(
        MouseEvent {
            modifiers: KeyModifiers::SHIFT,
            ..mouse(MouseEventKind::ScrollUp, results.x + 6, results.y + 3)
        },
        &mut harness.ctx(),
    );
    assert_eq!(w.results().grid.cursor(), (0, 0));
    w.mouse(
        mouse(MouseEventKind::ScrollDown, results.x + 6, results.y + 3),
        &mut harness.ctx(),
    );
    w.mouse(
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            results.x + 6,
            results.y + 3,
        ),
        &mut harness.ctx(),
    );
    w.mouse(
        mouse(MouseEventKind::Down(MouseButton::Left), 0, 29),
        &mut harness.ctx(),
    );
}

#[test]
fn leaving_abandons_the_query() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.editor_mut().set_text("SELECT 1");
    w.handle(Action::Execute, &mut harness.ctx());

    let command = w.handle(Action::Connections, &mut harness.ctx());
    assert_eq!(command, Some(Command::Connections));
    assert!(w.running().is_none());
    assert_eq!(
        w.handle(Action::Back, &mut harness.ctx()),
        Some(Command::ConfirmQuit)
    );
}

#[test]
fn the_event_log_shows_in_the_results() {
    let mut harness = Harness::new();
    harness.log.push("connected".into());
    let mut w = workspace();
    w.handle(Action::Debug, &mut harness.ctx());
    assert_eq!(w.results().store.columns()[1].name, "Event");
    assert_eq!(w.focused(), Panel::Results);
}

#[test]
fn workspace_wide() {
    let test = TestCtx::new();
    let mut w = workspace();
    let mut harness = Harness::new();
    answer_catalog(&mut w, &mut harness);
    w.editor_mut().set_text("SELECT * FROM customers");
    insta::assert_snapshot!(render(&test, 120, 40, |f, area, ctx| w.render(f, area, ctx)));
}

#[test]
fn workspace_narrow() {
    let test = TestCtx::new();
    let mut w = workspace();
    w.explorer.load(&mut Harness::new().ctx());
    insta::assert_snapshot!(render(&test, 80, 24, |f, area, ctx| w.render(f, area, ctx)));
}

#[test]
fn destructive_statements_ask_first() {
    let mut harness = Harness::new();
    let mut w = workspace();
    w.editor_mut().set_text("delete from orders");
    let command = w.handle(Action::Execute, &mut harness.ctx());
    assert_eq!(
        command,
        Some(Command::ConfirmRun {
            sql: "delete from orders".into(),
            reasons: vec!["DELETE without WHERE on orders".into()],
        })
    );
    assert!(harness.sent().is_empty());

    w.run_checked("delete from orders".into(), &mut harness.ctx());
    assert!(matches!(&harness.sent()[..], [DbCommand::Run { .. }]));
}

#[test]
fn the_guard_can_be_turned_off() {
    let mut harness = Harness::new();
    harness.config.confirm_destructive = false;
    let mut w = workspace();
    w.editor_mut().set_text("drop table t");
    assert_eq!(w.handle(Action::Execute, &mut harness.ctx()), None);
    assert!(matches!(&harness.sent()[..], [DbCommand::Run { .. }]));
}

#[test]
fn read_only_connections_refuse_writes() {
    let mut harness = Harness::new();
    let mut w = workspace().with_read_only(true);
    w.editor_mut().set_text("insert into t values (1)");
    w.handle(Action::Execute, &mut harness.ctx());
    assert!(harness.sent().is_empty());
    assert_eq!(
        harness.toast_texts(),
        ["This connection is read-only: the query would write"]
    );
    w.editor_mut().set_text("select 1");
    w.handle(Action::Execute, &mut harness.ctx());
    assert_eq!(harness.sent().len(), 1);
}

#[test]
fn loaded_tables_feed_autocomplete() {
    let mut harness = Harness::new();
    let mut w = workspace();
    answer_catalog(&mut w, &mut harness);
    assert!(harness.sent().contains(&DbCommand::AllColumns {
        path: ["shop", "public"].into_iter().collect()
    }));
    assert_eq!(w.catalog.schemas(), ["public"]);
    // A failed load asks for nothing.
    w.on_children(
        &["shop", "other"].into_iter().collect(),
        Err(dfox_core::errors::DbError::General("x".into())),
        &mut harness.ctx(),
    );
    assert!(harness.sent().is_empty());

    for c in "select * from ord".chars() {
        w.handle(Action::Input(c), &mut harness.ctx());
    }
    assert!(w.editor().completing());
    w.handle(Action::Down, &mut harness.ctx());
    w.handle(Action::Up, &mut harness.ctx());
    w.handle(Action::NextFocus, &mut harness.ctx());
    assert_eq!(w.editor().text(), "select * from order_items");
    assert_eq!(w.focused(), Panel::Editor);

    // Ctrl+Space asks for suggestions anywhere; Esc closes them.
    w.handle(Action::Input(' '), &mut harness.ctx());
    w.handle(Action::Complete, &mut harness.ctx());
    assert!(w.editor().completing());
    assert_eq!(w.handle(Action::Back, &mut harness.ctx()), None);
    assert!(!w.editor().completing());

    // Other keys close the popup and do their own thing.
    w.handle(Action::Complete, &mut harness.ctx());
    w.handle(Action::Execute, &mut harness.ctx());
    assert!(!w.editor().completing());
}

#[test]
fn enter_in_the_results_opens_the_inspector() {
    let test = TestCtx::new();
    let mut harness = Harness::new();
    harness.log.push("connected".into());
    let mut w = workspace();
    w.handle(Action::Debug, &mut harness.ctx());
    w.handle(Action::Confirm, &mut harness.ctx());
    assert!(w.results().takes_back());
    render(&test, 120, 30, |f, area, ctx| w.render(f, area, ctx));
    let [_, _, results] = w.areas.get();
    // The wheel over the inspector scrolls it, not the grid.
    w.mouse(
        mouse(
            MouseEventKind::ScrollDown,
            results.right() - 5,
            results.y + 3,
        ),
        &mut harness.ctx(),
    );
    assert_eq!(w.results().grid.cursor(), (0, 0));
    w.handle(Action::Back, &mut harness.ctx());
    assert!(!w.results().takes_back());
}

/// A workspace with the `orders` table selected in the explorer.
fn on_orders(harness: &mut Harness) -> Workspace {
    let mut w = workspace();
    answer_catalog(&mut w, harness);
    harness.sent();
    w.set_focus(Panel::Explorer);
    w.explorer
        .select(&["d/shop", "g/Table", "o/orders"].map(String::from));
    w
}

fn orders() -> NodePath {
    ["shop", "public", "orders"].into_iter().collect()
}

fn orders_info() -> dfox_core::catalog::EntityInfo {
    use dfox_core::catalog::{ColumnInfo, EntityInfo};
    let column = |name: &str, primary_key| ColumnInfo {
        name: name.into(),
        type_name: "integer".into(),
        primary_key,
        ..ColumnInfo::default()
    };
    EntityInfo {
        columns: vec![column("id", true), column("total", false)],
        row_estimate: Some(12_400),
        ..EntityInfo::default()
    }
}

#[test]
fn enter_on_a_table_shows_its_rows() {
    let mut harness = Harness::new();
    let mut w = on_orders(&mut harness);

    // Enter opens the menu, which reads the table meanwhile; Enter again
    // picks its first rows.
    w.handle(Action::Confirm, &mut harness.ctx());
    assert_eq!(harness.sent(), [DbCommand::Describe { path: orders() }]);
    assert!(w.wants_text());
    w.handle(Action::Confirm, &mut harness.ctx());
    let sql = "SELECT * FROM orders LIMIT 100";
    let id = match harness.sent().as_slice() {
        [DbCommand::Run { id, sql: sent, .. }] if sent == sql => *id,
        other => panic!("expected the query, got {other:?}"),
    };
    // The query shows in a tab of the table's own, and the explorer keeps
    // the keys.
    assert_eq!((w.tab_count(), w.title(0)), (1, "orders".to_string()));
    assert_eq!(w.editor().text(), sql);
    assert_eq!(w.focused(), Panel::Explorer);
    assert!(!w.wants_text());

    let mut ctx = harness.ctx();
    let (schema, batch) = crate::testing::ints(&[1, 2]);
    w.on_query_event(EngineEvent::Schema { id, schema }, &mut ctx);
    w.on_query_event(EngineEvent::Batch { id, batch }, &mut ctx);
    let changed = w.on_query_event(
        EngineEvent::QueryDone {
            id,
            outcome: Ok(QueryStats {
                rows: 2,
                ..Default::default()
            }),
        },
        &mut ctx,
    );
    assert!(changed);
    assert_eq!(w.results().store.row_count(), 2);
    assert_eq!(harness.history.search("", 10)[0].sql, sql);

    // Another view of the same table reuses its tab.
    w.explorer.on_described(&orders(), Ok(orders_info()));
    w.handle(Action::Confirm, &mut harness.ctx());
    w.handle(Action::Input('a'), &mut harness.ctx());
    assert!(matches!(
        harness.sent().as_slice(),
        [DbCommand::Run { sql, .. }] if sql == "SELECT * FROM orders"
    ));
    assert_eq!(w.tab_count(), 1);
    w.on_query_event(
        EngineEvent::QueryDone {
            id: id + 1,
            outcome: Ok(QueryStats::default()),
        },
        &mut harness.ctx(),
    );

    // One query at a time.
    w.editor_mut().set_text("select 1");
    w.handle(Action::NextFocus, &mut harness.ctx());
    w.handle(Action::Execute, &mut harness.ctx());
    harness.sent();
    w.set_focus(Panel::Explorer);
    w.handle(Action::Confirm, &mut harness.ctx());
    w.handle(Action::Confirm, &mut harness.ctx());
    assert!(harness.sent().is_empty());
    assert_eq!(harness.toast_texts(), ["A query is already running"]);
}

#[test]
fn the_last_rows_wait_for_the_primary_key() {
    let mut harness = Harness::new();
    let mut w = on_orders(&mut harness);
    w.handle(Action::Confirm, &mut harness.ctx());
    harness.sent();
    w.handle(Action::Input('l'), &mut harness.ctx());
    assert!(harness.sent().is_empty(), "the menu waits for describe");

    let command = w.on_described(&orders(), Ok(orders_info()), &mut harness.ctx());
    assert_eq!(command, None);
    assert!(matches!(
        harness.sent().as_slice(),
        [DbCommand::Run { sql, .. }] if sql == "SELECT * FROM orders ORDER BY id DESC LIMIT 100"
    ));
}

/// Opens the table menu and types `keys`: what the last one did, and what
/// went to the engine.
fn pick(w: &mut Workspace, harness: &mut Harness, keys: &str) -> (Option<Command>, Vec<DbCommand>) {
    w.handle(Action::Confirm, &mut harness.ctx());
    let mut command = None;
    for c in keys.chars() {
        command = w.handle(Action::Input(c), &mut harness.ctx());
    }
    (command, harness.sent())
}

#[test]
fn the_menu_counts_scripts_and_drops() {
    let mut harness = Harness::new();
    let mut w = on_orders(&mut harness);
    w.explorer.on_described(&orders(), Ok(orders_info()));

    let (_, sent) = pick(&mut w, &mut harness, "c");
    assert_eq!(sent, [DbCommand::Count { path: orders() }]);
    w.on_counted(&orders(), Ok(12_345), &mut harness.ctx());
    assert_eq!(harness.toast_texts(), ["orders holds 12345 rows"]);

    pick(&mut w, &mut harness, "su");
    assert_eq!(w.title(w.active_tab()), "UPDATE orders");
    assert_eq!(
        w.editor().text(),
        "UPDATE orders\nSET total = ?\nWHERE id = ?;\n"
    );
    assert_eq!(w.focused(), Panel::Editor);

    w.set_focus(Panel::Explorer);
    let (_, sent) = pick(&mut w, &mut harness, "sc");
    assert_eq!(
        sent,
        [DbCommand::Definition {
            path: orders(),
            kind: NodeKind::Table,
        }]
    );

    // Dropping asks first, as any destructive query does.
    let (command, sent) = pick(&mut w, &mut harness, "D");
    assert!(sent.is_empty());
    assert!(matches!(
        command,
        Some(Command::ConfirmRun { sql, .. }) if sql == "DROP TABLE orders CASCADE"
    ));

    w.set_focus(Panel::Explorer);
    w.handle(Action::Confirm, &mut harness.ctx());
    w.handle(Action::Input('w'), &mut harness.ctx());
    for c in "total > 10".chars() {
        w.handle(Action::Input(c), &mut harness.ctx());
    }
    w.handle(Action::Confirm, &mut harness.ctx());
    assert!(matches!(
        harness.sent().as_slice(),
        [DbCommand::Run { sql, .. }] if sql == "SELECT * FROM orders WHERE total > 10"
    ));
}

#[test]
fn read_only_connections_and_properties() {
    let mut harness = Harness::new();
    let mut w = on_orders(&mut harness).with_read_only(true);
    w.explorer.on_described(&orders(), Ok(orders_info()));
    w.handle(Action::Confirm, &mut harness.ctx());
    // No drop on a read-only connection.
    assert_eq!(w.handle(Action::Input('d'), &mut harness.ctx()), None);
    w.handle(Action::Input('p'), &mut harness.ctx());
    let test = TestCtx::new();
    insta::assert_snapshot!(render(&test, 80, 16, |f, area, ctx| w.render(f, area, ctx)));
    w.handle(Action::Back, &mut harness.ctx());
    assert!(!w.wants_text());
}

#[test]
fn names_from_the_explorer_go_into_the_editor_quoted() {
    let mut harness = Harness::new();
    let mut w = workspace();
    answer_catalog(&mut w, &mut harness);
    w.on_children(
        &["shop", "public"].into_iter().collect(),
        Ok(vec![Node::new(NodeKind::Table, "order", true)]),
        &mut harness.ctx(),
    );
    w.editor_mut().set_text("select * from ");
    w.editor_mut().handle(Action::End, &mut harness.ctx());
    w.set_focus(Panel::Explorer);
    w.explorer
        .select(&["d/shop", "g/Table", "o/order"].map(String::from));

    w.handle(Action::Inspect, &mut harness.ctx());
    assert_eq!(w.editor().text(), "select * from \"order\"");
    assert_eq!(w.focused(), Panel::Explorer);
}

#[test]
fn definitions_open_in_tabs_named_for_their_object() {
    let mut harness = Harness::new();
    let mut w = workspace();
    answer_catalog(&mut w, &mut harness);
    harness.sent();
    let at = |name: &str| -> NodePath { ["shop", "public", name].into_iter().collect() };
    let open = |w: &mut Workspace, harness: &mut Harness, name: &str, kind| {
        w.explorer_command(
            Command::Definition {
                path: at(name),
                kind,
            },
            &mut harness.ctx(),
        );
    };

    open(&mut w, &mut harness, "total_spent", NodeKind::Function);
    assert_eq!(
        harness.sent(),
        [DbCommand::Definition {
            path: at("total_spent"),
            kind: NodeKind::Function,
        }]
    );
    // The empty first tab takes it.
    let body = "CREATE FUNCTION total_spent() RETURNS int\nAS 'select 1';\n";
    w.on_definition(
        at("total_spent"),
        NodeKind::Function,
        Ok(body.into()),
        &mut harness.ctx(),
    );
    assert_eq!((w.tab_count(), w.focused()), (1, Panel::Editor));
    assert_eq!(w.editor().text(), body);
    let test = TestCtx::new();
    insta::assert_snapshot!(render(&test, 60, 6, |f, area, ctx| w.editor().render_with(
        f,
        area,
        ctx,
        &[w.label(0)]
    )));

    w.on_definition(
        at("active_customers"),
        NodeKind::View,
        Ok("CREATE VIEW active_customers AS SELECT 1;\n".into()),
        &mut harness.ctx(),
    );
    assert_eq!((w.tab_count(), w.active_tab()), (2, 1));
    assert_eq!(w.title(1), "active_customers");

    // Asking again goes back to its tab.
    open(&mut w, &mut harness, "total_spent", NodeKind::Function);
    assert!(harness.sent().is_empty());
    assert_eq!(w.active_tab(), 0);

    w.on_definition(
        at("place_order"),
        NodeKind::Procedure,
        Err(DbError::General("permission denied".into())),
        &mut harness.ctx(),
    );
    assert_eq!(w.tab_count(), 2);
    assert_eq!(
        harness.toast_texts(),
        ["Couldn't open place_order: Error: permission denied"]
    );
}

#[test]
fn enter_on_a_column_shows_its_card() {
    use dfox_core::catalog::{ColumnInfo, EntityInfo, ForeignKey};

    let mut harness = Harness::new();
    let mut w = workspace();
    answer_catalog(&mut w, &mut harness);
    let orders: NodePath = ["shop", "public", "orders"].into_iter().collect();
    let customers: NodePath = ["shop", "public", "customers"].into_iter().collect();
    w.set_focus(Panel::Explorer);
    w.explorer
        .select(&["d/shop", "g/Table", "o/orders"].map(String::from));
    w.handle(Action::Right, &mut harness.ctx());
    harness.sent();
    w.explorer.on_described(
        &orders,
        Ok(EntityInfo {
            columns: vec![ColumnInfo {
                name: "customer".into(),
                type_name: "integer".into(),
                ..ColumnInfo::default()
            }],
            foreign_keys: vec![ForeignKey {
                name: "orders_customer_fkey".into(),
                columns: vec!["customer".into()],
                target: customers.clone(),
                target_columns: vec!["id".into()],
            }],
            ..EntityInfo::default()
        }),
    );
    w.explorer
        .select(&["d/shop", "g/Table", "o/orders", "f/c", "c/customer"].map(String::from));

    w.handle(Action::Confirm, &mut harness.ctx());
    assert_eq!(
        harness.sent(),
        [DbCommand::ColumnStats {
            path: orders.clone(),
            column: "customer".into(),
            profile: false,
        }]
    );
    // The card takes every key, letters too.
    assert!(w.wants_text());
    w.handle(Action::Input('p'), &mut harness.ctx());
    assert!(matches!(
        harness.sent().as_slice(),
        [DbCommand::ColumnStats { profile: true, .. }]
    ));
    w.on_column_stats(&orders, "customer", false, Ok(None));
    w.handle(Action::Back, &mut harness.ctx());
    assert!(!w.wants_text());

    // Enter on the card goes to the table the key points at.
    w.handle(Action::Confirm, &mut harness.ctx());
    harness.sent();
    w.handle(Action::Confirm, &mut harness.ctx());
    assert_eq!(w.focused(), Panel::Explorer);
    assert!(!w.wants_text());
    w.handle(Action::Inspect, &mut harness.ctx());
    assert_eq!(w.editor().text(), "customers");
}
