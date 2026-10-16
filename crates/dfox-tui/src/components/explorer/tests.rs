use dfox_core::catalog::{ColumnInfo, EntityInfo};

use super::*;
use crate::testing::{render, Harness, TestCtx};

fn path(parts: &[&str]) -> NodePath {
    parts.iter().copied().collect()
}

fn children(parts: &[&str]) -> DbCommand {
    DbCommand::Children { path: path(parts) }
}

/// An explorer on `shop`, with the database list and `schemas` answered,
/// as Postgres would: only the connected database can be browsed.
fn loaded(harness: &mut Harness, schemas: &[&str]) -> Explorer {
    let mut explorer = Explorer::new("shop");
    explorer.focused = true;
    explorer.load(&mut harness.ctx());
    explorer.on_children(
        &NodePath::root(),
        Ok(vec![
            Node::new(NodeKind::Database, "postgres", false),
            Node::new(NodeKind::Database, "shop", true),
        ]),
        &mut harness.ctx(),
    );
    explorer.on_children(
        &path(&["shop"]),
        Ok(schemas
            .iter()
            .map(|s| Node::new(NodeKind::Schema, *s, true))
            .collect()),
        &mut harness.ctx(),
    );
    explorer
}

fn objects(explorer: &mut Explorer, harness: &mut Harness, schema: &str) {
    explorer.on_children(
        &path(&["shop", schema]),
        Ok(crate::testing::sample_objects()),
        &mut harness.ctx(),
    );
}

fn snapshot(explorer: &Explorer, height: u16) -> String {
    let test = TestCtx::new();
    render(&test, 40, height, |f, area, ctx| {
        explorer.render(f, area, ctx)
    })
}

#[test]
fn loading_opens_the_active_database_and_its_only_schema() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public"]);
    assert_eq!(
        harness.sent(),
        [
            children(&[]),
            children(&["shop"]),
            children(&["shop", "public"])
        ]
    );
    objects(&mut explorer, &mut harness, "public");
    insta::assert_snapshot!(snapshot(&explorer, 16));
}

#[test]
fn several_schemas_show_a_schema_level() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public", "audit"]);
    objects(&mut explorer, &mut harness, "public");
    insta::assert_snapshot!(snapshot(&explorer, 14));
}

#[test]
fn another_database_is_opened_by_switching_to_it() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public"]);
    explorer
        .state
        .borrow_mut()
        .select(vec!["d/postgres".into()]);
    assert_eq!(
        explorer.handle(Action::Confirm, &mut harness.ctx()),
        Some(Command::UseDatabase("postgres".into()))
    );

    harness.sent();
    explorer.set_active("postgres", &mut harness.ctx());
    assert_eq!(explorer.active, "postgres");
    assert_eq!(harness.sent(), [children(&["postgres"])]);
}

#[test]
fn opening_a_table_describes_it() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public"]);
    objects(&mut explorer, &mut harness, "public");
    harness.sent();

    let ids: Vec<String> = ["d/shop", "g/Table", "o/orders"].map(String::from).to_vec();
    explorer.state.borrow_mut().open(ids[..2].to_vec());
    explorer.state.borrow_mut().select(ids);
    explorer.handle(Action::Right, &mut harness.ctx());
    let table = path(&["shop", "public", "orders"]);
    assert_eq!(
        harness.sent(),
        [DbCommand::Describe {
            path: table.clone()
        }]
    );

    explorer.on_described(
        &table,
        Ok(EntityInfo {
            columns: vec![ColumnInfo {
                name: "id".into(),
                type_name: "integer".into(),
                nullable: false,
                default: None,
                primary_key: true,
            }],
            ..Default::default()
        }),
    );
    insta::assert_snapshot!(snapshot(&explorer, 14));
}

/// MySQL and SQLite have no schemas: tables sit right under the database,
/// and every database can be opened without switching.
#[test]
fn databases_without_schemas_hold_their_tables() {
    let mut harness = Harness::new();
    let mut explorer = Explorer::new("shop");
    explorer.load(&mut harness.ctx());
    explorer.on_children(
        &NodePath::root(),
        Ok(vec![
            Node::new(NodeKind::Database, "mysql", true),
            Node::new(NodeKind::Database, "shop", true),
        ]),
        &mut harness.ctx(),
    );
    explorer.on_children(
        &path(&["shop"]),
        Ok(crate::testing::sample_objects()),
        &mut harness.ctx(),
    );
    assert_eq!(harness.sent(), [children(&[]), children(&["shop"])]);
    assert!(explorer
        .state
        .borrow()
        .opened()
        .contains(&vec!["d/shop".to_string(), "g/Table".to_string()]));

    explorer.state.borrow_mut().select(vec!["d/mysql".into()]);
    assert_eq!(explorer.handle(Action::Confirm, &mut harness.ctx()), None);
    assert_eq!(harness.sent(), [children(&["mysql"])]);
}

#[test]
fn filter_finds_and_jumps_to_an_object() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public"]);
    objects(&mut explorer, &mut harness, "public");

    explorer.handle(Action::Filter, &mut harness.ctx());
    assert!(explorer.wants_text());
    for c in "ordr".chars() {
        explorer.handle(Action::Input(c), &mut harness.ctx());
    }
    insta::assert_snapshot!(snapshot(&explorer, 8));
    assert_eq!(explorer.matches[0].node.name, "orders");

    explorer.handle(Action::Confirm, &mut harness.ctx());
    assert!(!explorer.wants_text());
    assert_eq!(
        explorer.state.borrow().selected(),
        ["d/shop", "g/Table", "o/orders"]
    );
}

#[test]
fn escape_leaves_the_filter() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public"]);
    explorer.handle(Action::Filter, &mut harness.ctx());
    explorer.handle(Action::Back, &mut harness.ctx());
    assert!(!explorer.wants_text());
}

#[test]
fn queries_refresh_the_loaded_schemas() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public", "audit"]);
    objects(&mut explorer, &mut harness, "public");
    harness.sent();

    explorer.refresh_loaded(&mut harness.ctx());
    assert_eq!(harness.sent(), [children(&["shop", "public"])]);
}

#[test]
fn enter_browses_objects_with_rows_and_opens_the_rest() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public"]);
    objects(&mut explorer, &mut harness, "public");
    harness.sent();

    explorer.select(&["d/shop", "g/View", "o/active_customers"].map(String::from));
    assert_eq!(
        explorer.handle(Action::Confirm, &mut harness.ctx()),
        Some(Command::Browse {
            path: path(&["shop", "public", "active_customers"]),
            name: vec!["active_customers".into()],
        })
    );

    // A group opens and closes.
    let group = ["d/shop", "g/Function"].map(String::from);
    explorer.select(&group);
    assert_eq!(explorer.handle(Action::Confirm, &mut harness.ctx()), None);
    assert!(explorer.state.borrow().opened().contains(&group.to_vec()));
    assert!(harness.sent().is_empty());
}

#[test]
fn names_leave_out_the_schema_and_database_in_use() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public", "audit"]);
    objects(&mut explorer, &mut harness, "public");
    objects(&mut explorer, &mut harness, "audit");
    let mut name = |ids: &[&str]| {
        explorer.select(&ids.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        match explorer.handle(Action::Inspect, &mut harness.ctx()) {
            Some(Command::InsertName(name)) => Some(name),
            None => None,
            other => panic!("expected a name, got {other:?}"),
        }
    };

    let public = ["d/shop", "s/public", "g/Table", "o/orders"];
    assert_eq!(name(&public), Some(vec!["orders".to_string()]));
    let audit = ["d/shop", "s/audit", "g/Table", "o/orders"];
    assert_eq!(
        name(&audit),
        Some(vec!["audit".to_string(), "orders".to_string()])
    );
    let column = ["d/shop", "s/audit", "g/Table", "o/orders", "c/total"];
    assert_eq!(name(&column), Some(vec!["total".to_string()]));
    assert_eq!(
        name(&["d/shop", "s/audit"]),
        Some(vec!["audit".to_string()])
    );
    assert_eq!(name(&["d/shop", "s/audit", "g/Table"]), None);
}

#[test]
fn routines_and_views_open_their_definitions() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public"]);
    objects(&mut explorer, &mut harness, "public");
    let at = |name: &str| path(&["shop", "public", name]);

    explorer.select(&["d/shop", "g/Function", "o/total_spent"].map(String::from));
    assert_eq!(
        explorer.handle(Action::Confirm, &mut harness.ctx()),
        Some(Command::Definition {
            path: at("total_spent"),
            kind: NodeKind::Function,
        })
    );
    explorer.select(&["d/shop", "g/View", "o/active_customers"].map(String::from));
    assert_eq!(
        explorer.handle(Action::Edit, &mut harness.ctx()),
        Some(Command::Definition {
            path: at("active_customers"),
            kind: NodeKind::View,
        })
    );
    explorer.select(&["d/shop", "g/Table", "o/orders"].map(String::from));
    assert_eq!(explorer.handle(Action::Edit, &mut harness.ctx()), None);
}
