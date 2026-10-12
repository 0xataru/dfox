use super::*;
use crate::testing::{render, TestCtx};
use dfox_core::{
    engine::{EngineHandle, QueryStats},
    errors::DbError,
    result::{ColumnMeta, OwnedCell},
};

fn app() -> (App, mpsc::UnboundedReceiver<DbCommand>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let app = App::new(
        EngineHandle::new(tx),
        Config::default(),
        None,
        ColorDepth::TrueColor,
        Vec::new(),
    );
    (app, rx)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
}

fn sent(rx: &mut mpsc::UnboundedReceiver<DbCommand>) -> Vec<DbCommand> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn snapshot(app: &App, width: u16, height: u16) -> String {
    let test = TestCtx::new();
    render(&test, width, height, |f, _, ctx| app.draw(f, ctx))
}

/// Connects to a server and opens `shop`, answering for the engine.
fn open_shop(app: &mut App, rx: &mut mpsc::UnboundedReceiver<DbCommand>) {
    app.handle_key(key(KeyCode::Enter));
    typed(app, "me");
    app.handle_key(key(KeyCode::Enter));
    typed(app, "secret");
    for _ in 0..3 {
        app.handle_key(key(KeyCode::Enter));
    }
    assert!(matches!(
        &sent(rx)[..],
        [DbCommand::Connect {
            db_type: DbType::Postgres,
            ..
        }]
    ));
    app.apply(EngineEvent::Connected(Ok(())));
    assert_eq!(sent(rx), [DbCommand::ListDatabases]);
    app.apply(EngineEvent::Databases(Ok(vec![
        "postgres".into(),
        "shop".into(),
    ])));

    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    match &sent(rx)[..] {
        [DbCommand::Connect { params, .. }] => {
            assert_eq!(params.database.as_deref(), Some("shop"));
            assert_eq!(params.username, "me");
            assert_eq!(params.host, "localhost");
        }
        other => panic!("expected a connect, got {other:?}"),
    }
    app.apply(EngineEvent::Connected(Ok(())));
    assert_eq!(sent(rx), [DbCommand::ListTables]);
    app.apply(EngineEvent::Tables(Ok(vec![
        "customers".into(),
        "orders".into(),
    ])));
}

#[test]
fn connecting_and_opening_a_database() {
    let (mut app, mut rx) = app();
    open_shop(&mut app, &mut rx);
    assert_eq!(app.screen, Screen::Workspace);
    assert_eq!(app.workspace.tables.tables, ["customers", "orders"]);
}

#[test]
fn a_failed_connect_shows_the_error_on_the_form() {
    let (mut app, mut rx) = app();
    app.handle_key(key(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_key(key(KeyCode::Enter));
    }
    sent(&mut rx);
    app.apply(EngineEvent::Connected(Err(DbError::Connection(
        "refused".into(),
    ))));

    assert_eq!(app.screen, Screen::Connect);
    assert_eq!(
        app.connect.error.as_deref(),
        Some("Connection error: refused")
    );
    insta::assert_snapshot!(snapshot(&app, 80, 24));
}

#[test]
fn running_a_query_end_to_end() {
    let (mut app, mut rx) = app();
    open_shop(&mut app, &mut rx);

    typed(&mut app, "SELECT * FROM customers");
    app.handle_key(key(KeyCode::F(5)));
    let id = match &sent(&mut rx)[..] {
        [DbCommand::Run { id, sql, .. }] => {
            assert_eq!(sql, "SELECT * FROM customers");
            *id
        }
        other => panic!("expected a run, got {other:?}"),
    };
    app.apply(EngineEvent::Columns {
        id,
        columns: vec![
            ColumnMeta::new("id", "int4"),
            ColumnMeta::new("name", "text"),
        ],
    });
    app.apply(EngineEvent::Rows {
        id,
        rows: vec![
            vec![OwnedCell::Int(1), OwnedCell::Text("Ada".into())],
            vec![OwnedCell::Int(2), OwnedCell::Null],
        ],
    });
    insta::assert_snapshot!("query_streaming", snapshot(&app, 120, 40));

    app.apply(EngineEvent::QueryDone {
        id,
        outcome: Ok(QueryStats {
            elapsed: Duration::from_millis(7),
            rows: 2,
            ..Default::default()
        }),
    });
    assert_eq!(sent(&mut rx), [DbCommand::ListTables]);
    insta::assert_snapshot!("query_done", snapshot(&app, 80, 24));
}

#[test]
fn typing_q_in_the_editor_does_not_quit() {
    let (mut app, mut rx) = app();
    open_shop(&mut app, &mut rx);
    typed(&mut app, "q");
    assert!(!app.should_quit);
    assert_eq!(app.workspace.editor.text(), "q");
}

#[test]
fn quitting_while_connected_asks_first() {
    let (mut app, mut rx) = app();
    open_shop(&mut app, &mut rx);
    app.handle_key(key(KeyCode::Esc));
    insta::assert_snapshot!(snapshot(&app, 80, 24));

    app.handle_key(key(KeyCode::Char('n')));
    assert!(!app.should_quit);
    app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));
    app.handle_key(key(KeyCode::Char('y')));
    assert!(app.should_quit);
}

#[test]
fn q_quits_right_away_before_connecting() {
    let (mut app, _rx) = app();
    app.handle_key(key(KeyCode::Char('q')));
    assert!(app.should_quit);
}

#[test]
fn the_menu_runs_the_key_pressed() {
    let (mut app, mut rx) = app();
    open_shop(&mut app, &mut rx);
    app.handle_key(key(KeyCode::Tab));
    app.handle_key(key(KeyCode::Char(' ')));
    insta::assert_snapshot!(snapshot(&app, 120, 40));

    app.handle_key(key(KeyCode::Char('d')));
    assert_eq!(app.screen, Screen::Databases);
}

#[test]
fn help_opens_and_closes() {
    let (mut app, _rx) = app();
    app.handle_key(key(KeyCode::Char('?')));
    insta::assert_snapshot!(snapshot(&app, 120, 40));
    app.handle_key(key(KeyCode::Char('x')));
    assert!(app.overlay.is_none());
}

#[test]
fn the_theme_switches_and_says_so() {
    let (mut app, _rx) = app();
    app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
    assert_eq!(app.theme.name, "tokyo-night");
    assert_eq!(app.toasts.items()[0].text, "Theme: tokyo-night");
}

#[test]
fn config_problems_show_as_toasts() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let config = Config {
        theme: "missing".into(),
        ..Default::default()
    };
    let app = App::new(
        EngineHandle::new(tx),
        config,
        None,
        ColorDepth::TrueColor,
        vec!["config.toml: oops".into()],
    );
    let texts: Vec<_> = app.toasts.items().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(texts, ["config.toml: oops", "unknown theme \"missing\""]);
    assert_eq!(app.theme.name, "dfox");
}

#[test]
fn first_screen() {
    let (app, _rx) = app();
    insta::assert_snapshot!("first_screen_wide", snapshot(&app, 120, 40));
    insta::assert_snapshot!("first_screen_narrow", snapshot(&app, 80, 24));
}

#[test]
fn database_screen() {
    let (mut app, mut rx) = app();
    app.handle_key(key(KeyCode::Enter));
    for _ in 0..4 {
        app.handle_key(key(KeyCode::Enter));
    }
    sent(&mut rx);
    app.apply(EngineEvent::Connected(Ok(())));
    app.apply(EngineEvent::Databases(Ok(vec![
        "postgres".into(),
        "shop".into(),
    ])));
    insta::assert_snapshot!(snapshot(&app, 120, 40));
}
