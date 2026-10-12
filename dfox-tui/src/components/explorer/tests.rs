use super::*;
use crate::testing::{render, Harness, TestCtx};

/// An explorer on `shop`, with the database list and `schemas` answered.
fn loaded(harness: &mut Harness, schemas: &[&str]) -> Explorer {
    let mut explorer = Explorer::new("shop");
    explorer.focused = true;
    explorer.load(&mut harness.ctx());
    explorer.on_databases(
        Ok(vec!["postgres".into(), "shop".into()]),
        &mut harness.ctx(),
    );
    explorer.on_schemas(
        Ok(schemas.iter().map(|s| s.to_string()).collect()),
        &mut harness.ctx(),
    );
    explorer
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
            DbCommand::ListDatabases,
            DbCommand::ListSchemas,
            DbCommand::ListObjects {
                schema: "public".into()
            }
        ]
    );
    explorer.on_objects("public", Ok(crate::testing::sample_objects()));
    insta::assert_snapshot!(snapshot(&explorer, 16));
}

#[test]
fn several_schemas_show_a_schema_level() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public", "audit"]);
    explorer.on_objects("public", Ok(crate::testing::sample_objects()));
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
    assert_eq!(harness.sent(), [DbCommand::ListSchemas]);
}

#[test]
fn opening_a_table_describes_it() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public"]);
    explorer.on_objects("public", Ok(crate::testing::sample_objects()));
    harness.sent();

    let path: Vec<String> = ["d/shop", "g/Table", "o/orders"].map(String::from).to_vec();
    explorer.state.borrow_mut().open(path[..2].to_vec());
    explorer.state.borrow_mut().select(path);
    explorer.handle(Action::Right, &mut harness.ctx());
    assert_eq!(
        harness.sent(),
        [DbCommand::Describe {
            schema: "public".into(),
            table: "orders".into()
        }]
    );

    explorer.on_described(
        "public",
        "orders",
        Ok(TableSchema {
            schema: "public".into(),
            table_name: "orders".into(),
            columns: vec![ColumnSchema {
                name: "id".into(),
                data_type: "integer".into(),
                is_nullable: false,
                default: None,
            }],
            indexes: Vec::new(),
        }),
    );
    insta::assert_snapshot!(snapshot(&explorer, 14));
}

#[test]
fn filter_finds_and_jumps_to_an_object() {
    let mut harness = Harness::new();
    let mut explorer = loaded(&mut harness, &["public"]);
    explorer.on_objects("public", Ok(crate::testing::sample_objects()));

    explorer.handle(Action::Filter, &mut harness.ctx());
    assert!(explorer.wants_text());
    for c in "ordr".chars() {
        explorer.handle(Action::Input(c), &mut harness.ctx());
    }
    insta::assert_snapshot!(snapshot(&explorer, 8));
    assert_eq!(explorer.matches[0].object.name, "orders");

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
    explorer.on_objects("public", Ok(crate::testing::sample_objects()));
    harness.sent();

    explorer.refresh_loaded(&mut harness.ctx());
    assert_eq!(
        harness.sent(),
        [DbCommand::ListObjects {
            schema: "public".into()
        }]
    );
}

#[test]
fn row_counts_are_compact() {
    assert_eq!(render::compact(950), "950");
    assert_eq!(render::compact(12_400), "12.4k");
    assert_eq!(render::compact(3_400_000), "3.4M");
}
