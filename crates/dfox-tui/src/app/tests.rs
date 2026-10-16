use std::path::PathBuf;

use super::*;
use crate::{
    connections::{pgpass, secrets::Memory, Tag},
    testing::{render, TestCtx},
};
use crossterm::event::MouseButton;
use dfox_core::{
    catalog::{Node, NodeKind, NodePath},
    engine::{Command as DbCommand, QueryStats},
    errors::DbError,
    models::connections::DbType,
    result::{ColumnMeta, OwnedCell},
};

struct Test {
    app: App,
    rx: mpsc::UnboundedReceiver<DbCommand>,
    dir: PathBuf,
}

impl Drop for Test {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn prod() -> Connection {
    Connection {
        host: "db.example.com".into(),
        user: "app".into(),
        database: Some("shop".into()),
        tag: Tag::Red,
        ..Connection::new("prod", DbType::Postgres)
    }
}

/// An app with `prod` saved, `secret` in the keychain if `password` is set,
/// and extra `pgpass` lines.
fn test_app(name: &str, password: Option<&str>, pgpass_text: &str) -> Test {
    let dir = std::env::temp_dir().join(format!("dfox-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut store, _) = Store::load(Some(&dir));
    store.save(None, prod()).expect("save");
    let secrets = Memory::default();
    if let Some(password) = password {
        secrets.set("prod", password).expect("secret");
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let app = App::new(
        EngineHandle::new(tx),
        Setup {
            config: Config::default(),
            themes_dir: None,
            depth: ColorDepth::TrueColor,
            store,
            secrets: Box::new(secrets),
            pgpass: pgpass::parse(pgpass_text),
            history: Default::default(),
            clipboard: Box::new(crate::clipboard::Memory::default()),
            detected: Vec::new(),
            connect: None,
            problems: Vec::new(),
            splash: false,
        },
    );
    let mut test = Test { app, rx, dir };
    test.sent();
    test
}

impl Test {
    fn sent(&mut self) -> Vec<DbCommand> {
        std::iter::from_fn(|| self.rx.try_recv().ok()).collect()
    }

    fn key(&mut self, code: KeyCode) {
        self.app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn ctrl(&mut self, c: char) {
        self.app
            .handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    fn typed(&mut self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
    }

    fn snapshot(&self, width: u16, height: u16) -> String {
        let test = TestCtx::new();
        render(&test, width, height, |f, _, ctx| self.app.draw(f, ctx))
    }

    /// The password of the connect command sent, after checking there was one.
    fn connected_with(&mut self) -> String {
        match &self.sent()[..] {
            [DbCommand::Connect { params, .. }] => params.password.clone(),
            other => panic!("expected a connect, got {other:?}"),
        }
    }

    /// Connects to `prod` and answers for the engine.
    fn open_prod(&mut self) {
        self.key(KeyCode::Enter);
        self.sent();
        self.app.apply(EngineEvent::Connected(Ok(session_info())));
        assert_eq!(self.app.screen, Screen::Workspace);
        assert_eq!(
            self.sent(),
            [DbCommand::Children {
                path: NodePath::root()
            }]
        );
        self.answer_catalog();
        self.sent();
    }

    /// Answers the explorer as Postgres would: `postgres` and `shop`, which
    /// holds `public` and its objects.
    fn answer_catalog(&mut self) {
        self.app.apply(EngineEvent::Children {
            path: NodePath::root(),
            result: Ok(vec![
                Node::new(NodeKind::Database, "postgres", false),
                Node::new(NodeKind::Database, "shop", true),
            ]),
        });
        self.app.apply(EngineEvent::Children {
            path: path(&["shop"]),
            result: Ok(vec![Node::new(NodeKind::Schema, "public", true)]),
        });
        self.app.apply(EngineEvent::Children {
            path: path(&["shop", "public"]),
            result: Ok(crate::testing::sample_objects()),
        });
    }
}

#[test]
fn the_manager_pings_every_connection_on_start() {
    let dir = std::env::temp_dir().join(format!("dfox-app-ping-{}", std::process::id()));
    let (mut store, _) = Store::load(Some(&dir));
    store.save(None, prod()).expect("save");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let app = App::new(
        EngineHandle::new(tx),
        Setup {
            config: Config::default(),
            themes_dir: None,
            depth: ColorDepth::TrueColor,
            store,
            secrets: Box::new(Memory::default()),
            pgpass: Vec::new(),
            history: Default::default(),
            clipboard: Box::new(crate::clipboard::Memory::default()),
            detected: Vec::new(),
            connect: None,
            problems: Vec::new(),
            splash: false,
        },
    );
    assert_eq!(app.screen, Screen::Manager);
    assert!(matches!(rx.try_recv(), Ok(DbCommand::Ping { .. })));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn connecting_uses_the_keychain_password() {
    let mut test = test_app("keychain", Some("secret"), "");
    test.key(KeyCode::Enter);
    assert_eq!(test.connected_with(), "secret");
    insta::assert_snapshot!("connecting", test.snapshot(80, 10));
}

#[test]
fn without_a_saved_password_dfox_asks() {
    let mut test = test_app("prompt", None, "");
    test.key(KeyCode::Enter);
    assert!(test.sent().is_empty());
    test.typed("q?x");
    insta::assert_snapshot!(test.snapshot(80, 16));

    test.key(KeyCode::Enter);
    assert_eq!(test.connected_with(), "q?x");
    assert!(!test.app.should_quit);
}

#[test]
fn pgpass_supplies_a_missing_password() {
    let mut test = test_app("pgpass", None, "db.example.com:5432:shop:app:from-pgpass\n");
    test.key(KeyCode::Enter);
    assert_eq!(test.connected_with(), "from-pgpass");
}

#[test]
fn a_failed_connect_stays_on_the_manager() {
    let mut test = test_app("failed", Some("pw"), "");
    test.key(KeyCode::Enter);
    test.sent();
    test.app
        .apply(EngineEvent::Connected(Err(DbError::Connection(
            "password authentication failed".into(),
        ))));
    assert_eq!(test.app.screen, Screen::Manager);
    assert!(test.app.session.is_none());
    let toast = &test.app.toasts.items()[0].text;
    assert_eq!(
        toast,
        "Couldn't connect to prod: Connection error: password authentication failed"
    );
}

#[test]
fn a_new_connection_is_saved_with_its_password_in_the_keychain() {
    let mut test = test_app("new", None, "");
    test.key(KeyCode::Char('n'));
    assert_eq!(test.app.screen, Screen::Form);
    test.typed("local");
    test.key(KeyCode::Down);
    test.key(KeyCode::Left);
    test.key(KeyCode::Down);
    test.typed("/tmp/app.db");
    test.ctrl('s');

    assert_eq!(test.app.screen, Screen::Manager);
    let saved = std::fs::read_to_string(test.dir.join("connections.toml")).expect("file");
    assert!(saved.contains("name = \"local\""), "{saved}");
    assert!(saved.contains("driver = \"sqlite\""), "{saved}");
    assert_eq!(
        test.app.manager.selected(),
        Some(EntryKey::Saved("local".into()))
    );
    test.sent();

    // SQLite needs no password, so it connects right away.
    test.key(KeyCode::Enter);
    match &test.sent()[..] {
        [DbCommand::Connect { db_type, params }] => {
            assert_eq!(*db_type, DbType::Sqlite);
            assert_eq!(params.database.as_deref(), Some("/tmp/app.db"));
        }
        other => panic!("expected a connect, got {other:?}"),
    }
}

#[test]
fn editing_keeps_the_password_unless_a_new_one_is_typed() {
    let mut test = test_app("edit", Some("old"), "");
    test.key(KeyCode::Char('e'));
    test.typed("uction");
    test.ctrl('s');

    assert!(test.app.store.get("production").is_some());
    assert_eq!(
        test.app.secrets.get("production").expect("get").as_deref(),
        Some("old")
    );
    assert_eq!(test.app.secrets.get("prod").expect("get"), None);
}

#[test]
fn deleting_asks_first_and_removes_the_password() {
    let mut test = test_app("delete", Some("pw"), "");
    test.key(KeyCode::Char('d'));
    insta::assert_snapshot!(test.snapshot(80, 16));
    test.key(KeyCode::Char('y'));
    assert!(test.app.store.connections.is_empty());
    assert_eq!(test.app.secrets.get("prod").expect("get"), None);
}

#[test]
fn switching_database_reconnects_with_the_same_login() {
    let mut test = test_app("switch", Some("pw"), "");
    test.open_prod();
    // The tree knows its rows once drawn.
    test.snapshot(80, 24);
    // Editor → results → explorer, then from `shop` up to `postgres`.
    test.key(KeyCode::Tab);
    test.key(KeyCode::Tab);
    test.key(KeyCode::Up);
    test.key(KeyCode::Enter);

    match &test.sent()[..] {
        [DbCommand::Connect { params, .. }] => {
            assert_eq!(params.database.as_deref(), Some("postgres"));
            assert_eq!(params.password, "pw");
        }
        other => panic!("expected a connect, got {other:?}"),
    }
    test.app.apply(EngineEvent::Connected(Ok(session_info())));
    let workspace = test.app.workspace.as_ref().expect("workspace");
    assert_eq!(workspace.explorer.active, "postgres");
    assert_eq!(
        test.sent(),
        [DbCommand::Children {
            path: path(&["postgres"])
        }]
    );
}

#[test]
fn a_url_on_the_command_line_connects_right_away() {
    let (connection, password) =
        crate::connections::url::parse("command line", "postgres://me:pw@localhost/shop")
            .expect("url");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let app = App::new(
        EngineHandle::new(tx),
        Setup {
            config: Config::default(),
            themes_dir: None,
            depth: ColorDepth::TrueColor,
            store: Store::default(),
            secrets: Box::new(Memory::default()),
            pgpass: Vec::new(),
            history: Default::default(),
            clipboard: Box::new(crate::clipboard::Memory::default()),
            detected: vec![Detected {
                connection,
                password,
                source: "the command line".into(),
            }],
            connect: Some(EntryKey::Detected(0)),
            problems: Vec::new(),
            splash: false,
        },
    );
    let sent: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(matches!(
        sent.last(),
        Some(DbCommand::Connect { params, .. }) if params.password == "pw"
    ));
    assert!(app.connecting.is_some());
}

#[test]
fn running_a_query_in_the_workspace() {
    let mut test = test_app("query", Some("pw"), "");
    test.open_prod();
    test.typed("SELECT * FROM customers");
    test.key(KeyCode::F(5));
    let id = match &test.sent()[..] {
        [DbCommand::Run { id, sql, .. }] => {
            assert_eq!(sql, "SELECT * FROM customers");
            *id
        }
        other => panic!("expected a run, got {other:?}"),
    };
    let (schema, batch) = crate::testing::frames(
        vec![
            ColumnMeta::new("id", "int4"),
            ColumnMeta::new("name", "text"),
        ],
        vec![
            vec![OwnedCell::Int(1), OwnedCell::Text("Ada".into())],
            vec![OwnedCell::Int(2), OwnedCell::Null],
        ],
    );
    test.app.apply(EngineEvent::Schema { id, schema });
    test.app.apply(EngineEvent::Batch { id, batch });
    test.app.apply(EngineEvent::QueryDone {
        id,
        outcome: Ok(QueryStats {
            elapsed: Duration::from_millis(7),
            rows: 2,
            ..Default::default()
        }),
    });
    assert_eq!(
        test.sent(),
        [DbCommand::Children {
            path: path(&["shop", "public"])
        }]
    );
    insta::assert_snapshot!("workspace_wide", test.snapshot(120, 32));
    insta::assert_snapshot!("workspace_narrow", test.snapshot(80, 24));
}

#[test]
fn f1_goes_back_to_the_connections() {
    let mut test = test_app("back", Some("pw"), "");
    test.open_prod();
    test.key(KeyCode::F(1));
    assert_eq!(test.app.screen, Screen::Manager);
    assert!(test
        .sent()
        .iter()
        .all(|c| matches!(c, DbCommand::Ping { .. })));
}

#[test]
fn quitting_while_connected_asks_first() {
    let mut test = test_app("quit", Some("pw"), "");
    test.open_prod();
    test.key(KeyCode::Esc);
    test.key(KeyCode::Char('n'));
    assert!(!test.app.should_quit);
    test.ctrl('q');
    test.key(KeyCode::Char('y'));
    assert!(test.app.should_quit);
}

#[test]
fn q_quits_right_away_before_connecting() {
    let mut test = test_app("q", None, "");
    test.key(KeyCode::Char('q'));
    assert!(test.app.should_quit);
}

#[test]
fn the_menu_and_help() {
    let mut test = test_app("menu", Some("pw"), "");
    test.key(KeyCode::Char('?'));
    insta::assert_snapshot!("help", test.snapshot(120, 32));
    test.key(KeyCode::Char('x'));
    test.open_prod();
    test.key(KeyCode::Tab);
    test.key(KeyCode::Char(' '));
    insta::assert_snapshot!("menu", test.snapshot(120, 32));
    test.key(KeyCode::Char('m'));
    assert_eq!(test.app.screen, Screen::Manager);
}

#[test]
fn the_theme_switches_and_says_so() {
    let mut test = test_app("theme", None, "");
    test.ctrl('t');
    assert_eq!(test.app.theme.name, "tokyo-night");
    assert_eq!(test.app.toasts.items()[0].text, "Theme: tokyo-night");
}

#[test]
fn setup_problems_show_as_toasts() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let app = App::new(
        EngineHandle::new(tx),
        Setup {
            config: Config {
                theme: "missing".into(),
                ..Default::default()
            },
            themes_dir: None,
            depth: ColorDepth::TrueColor,
            store: Store::default(),
            secrets: Box::new(Memory::default()),
            pgpass: Vec::new(),
            history: Default::default(),
            clipboard: Box::new(crate::clipboard::Memory::default()),
            detected: Vec::new(),
            connect: None,
            problems: vec!["connections.toml: oops".into()],
            splash: false,
        },
    );
    let texts: Vec<_> = app.toasts.items().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(
        texts,
        ["connections.toml: oops", "unknown theme \"missing\""]
    );
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
fn pasted_text_goes_where_typing_would() {
    let mut test = test_app("paste", None, "");
    // The password prompt takes it, without line breaks.
    test.key(KeyCode::Enter);
    test.app.paste("se\ncret");
    test.key(KeyCode::Enter);
    assert_eq!(test.connected_with(), "secret");

    // The form takes it as typed characters.
    test.key(KeyCode::Char('n'));
    test.app.paste("local");
    let form = test.app.form.as_ref().expect("form");
    assert!(format!("{form:?}").contains("local"));
    test.key(KeyCode::Esc);

    // Screens without text fields ignore it.
    test.app.paste("x");
    assert_eq!(test.app.screen, Screen::Manager);
}

#[test]
fn pasting_into_the_editor_keeps_lines() {
    let mut test = test_app("paste-editor", Some("pw"), "");
    test.open_prod();
    test.app.paste("select 1;\nselect 2;");
    assert_eq!(
        test.app
            .workspace
            .as_ref()
            .expect("workspace")
            .editor()
            .text(),
        "select 1;\nselect 2;"
    );
    // An open overlay takes nothing.
    test.key(KeyCode::F(2));
    test.app.paste("x");
    assert!(test.app.overlay.is_some());
}

#[test]
fn the_mouse_reaches_the_workspace() {
    let mut test = test_app("mouse", Some("pw"), "");
    test.open_prod();
    test.snapshot(120, 32);
    test.app
        .handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 5, 3));
    let workspace = test.app.workspace.as_ref().expect("workspace");
    assert_eq!(
        workspace.focused(),
        crate::components::workspace::Panel::Explorer
    );

    // The wheel moves the manager's selection; overlays ignore the mouse.
    test.key(KeyCode::F(1));
    test.app
        .handle_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
    test.app.handle_mouse(mouse(MouseEventKind::ScrollUp, 5, 5));
    test.app.handle_mouse(mouse(MouseEventKind::Moved, 5, 5));
    test.key(KeyCode::Char('?'));
    test.app
        .handle_mouse(mouse(MouseEventKind::ScrollDown, 5, 5));
    assert!(test.app.overlay.is_some());
}

#[test]
fn help_scrolls_and_closes_on_other_keys() {
    let mut test = test_app("help", None, "");
    test.key(KeyCode::Char('?'));
    test.key(KeyCode::Down);
    test.key(KeyCode::PageDown);
    test.key(KeyCode::Char('k'));
    test.key(KeyCode::PageUp);
    test.key(KeyCode::Char('j'));
    test.key(KeyCode::Up);
    assert_eq!(test.app.overlay, Some(Overlay::Help(0)));
    test.key(KeyCode::Char('x'));
    assert_eq!(test.app.overlay, None);
}

#[test]
fn the_palette_runs_a_command_by_name() {
    let mut test = test_app("palette", None, "");
    test.ctrl('p');
    test.typed("thme");
    test.key(KeyCode::Down);
    test.key(KeyCode::Up);
    test.key(KeyCode::Backspace);
    test.typed("e");
    test.app.paste("ignored");
    test.key(KeyCode::Enter);
    assert_eq!(test.app.theme.name, "tokyo-night");
    assert_eq!(test.app.overlay, None);

    test.ctrl('p');
    test.key(KeyCode::Tab);
    test.ctrl('x');
    test.key(KeyCode::Esc);
    assert_eq!(test.app.overlay, None);
}

#[test]
fn destructive_queries_run_only_after_y() {
    let mut test = test_app("guard", Some("pw"), "");
    test.open_prod();
    test.typed("delete from orders");
    test.key(KeyCode::F(5));
    assert!(test.sent().is_empty());
    insta::assert_snapshot!(test.snapshot(80, 20));

    // Enter doesn't confirm a destructive query; other keys keep asking.
    test.key(KeyCode::Enter);
    test.key(KeyCode::Char('x'));
    assert!(test.sent().is_empty());
    assert!(test.app.overlay.is_some());
    test.key(KeyCode::Char('y'));
    assert!(matches!(
        &test.sent()[..],
        [DbCommand::Run { sql, .. }] if sql == "delete from orders"
    ));
}

#[test]
fn columns_arrive_for_autocomplete() {
    let mut test = test_app("columns", Some("pw"), "");
    test.key(KeyCode::Enter);
    test.sent();
    test.app.apply(EngineEvent::Connected(Ok(session_info())));
    test.answer_catalog();
    let public = path(&["shop", "public"]);
    assert!(test.sent().contains(&DbCommand::AllColumns {
        path: public.clone()
    }));
    test.app.apply(EngineEvent::AllColumns {
        path: public,
        result: Ok(vec![dfox_core::models::schema::TableColumn {
            table: "customers".into(),
            name: "email".into(),
            data_type: "text".into(),
        }]),
    });
    let workspace = test.app.workspace.as_ref().expect("workspace");
    assert_eq!(
        workspace.catalog.columns(None, "customers")[0].name,
        "email"
    );
}

#[test]
fn tab_accepts_a_suggestion() {
    let mut test = test_app("accept", Some("pw"), "");
    test.open_prod();
    test.app.apply(EngineEvent::AllColumns {
        path: path(&["shop", "public"]),
        result: Ok(vec![dfox_core::models::schema::TableColumn {
            table: "customers".into(),
            name: "email".into(),
            data_type: "text".into(),
        }]),
    });
    test.typed("select * from customers c where c.");
    test.key(KeyCode::Down);
    test.key(KeyCode::Tab);
    // Typing goes on after the name instead of replacing it.
    test.typed(" is null");
    let workspace = test.app.workspace.as_ref().expect("workspace");
    assert_eq!(
        workspace.editor().text(),
        "select * from customers c where c.email is null"
    );
}

/// An app with `prod` saved that starts with the splash, when animations
/// are on.
fn splash_app(name: &str, animations: bool) -> Test {
    let dir = std::env::temp_dir().join(format!("dfox-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut store, _) = Store::load(Some(&dir));
    store.save(None, prod()).expect("save");
    let (tx, rx) = mpsc::unbounded_channel();
    let app = App::new(
        EngineHandle::new(tx),
        Setup {
            config: Config {
                animations,
                ..Default::default()
            },
            themes_dir: None,
            depth: ColorDepth::TrueColor,
            store,
            secrets: Box::new(Memory::default()),
            pgpass: Vec::new(),
            history: Default::default(),
            clipboard: Box::new(crate::clipboard::Memory::default()),
            detected: Vec::new(),
            connect: None,
            problems: Vec::new(),
            splash: true,
        },
    );
    Test { app, rx, dir }
}

/// Draws a frame the way the event loop does, effects included.
fn draw_frame(app: &mut App) {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).expect("terminal");
    terminal.draw(|f| app.render(f)).expect("draw");
}

#[test]
fn the_splash_waits_for_the_first_checks() {
    let mut test = splash_app("splash", true);
    assert_eq!(test.app.screen, Screen::Splash);
    assert!(test.app.is_animating());
    assert!(!test.app.wants_text());
    insta::assert_snapshot!(test.snapshot(80, 16));
    draw_frame(&mut test.app);
    assert!(test.app.motion.is_running());

    let started = test.app.splash.expect("splash");
    test.app.update_splash(started + Duration::from_millis(100));
    assert_eq!(test.app.screen, Screen::Splash);
    let id = match &test.sent()[..] {
        [DbCommand::Ping { id, .. }] => *id,
        other => panic!("expected a ping, got {other:?}"),
    };
    test.app.on_engine_event(EngineEvent::Pong {
        id,
        result: Ok(Duration::from_millis(3)),
    });
    assert_eq!(
        test.app.splash_progress(started).1,
        "checking connections 1/1"
    );
    // Checks are done, but the logo gets its moment.
    test.app.update_splash(started + Duration::from_millis(300));
    assert_eq!(test.app.screen, Screen::Splash);
    let before = test.app.seen();
    test.app.update_splash(started + Duration::from_millis(450));
    assert_eq!(test.app.screen, Screen::Manager);
    test.app.react(before);
    assert!(test.app.motion.is_running());
}

#[test]
fn the_splash_never_outstays_600_ms() {
    let mut test = splash_app("splash-max", true);
    let started = test.app.splash.expect("splash");
    test.app.update_splash(started + Duration::from_millis(600));
    assert_eq!(test.app.screen, Screen::Manager);
    // Once over, it stays over.
    test.app.update_splash(started + Duration::from_secs(5));
    assert_eq!(test.app.screen, Screen::Manager);
}

#[test]
fn a_key_skips_the_splash_and_does_nothing_else() {
    let mut test = splash_app("splash-key", true);
    test.key(KeyCode::Char('q'));
    assert_eq!(test.app.screen, Screen::Manager);
    assert!(!test.app.should_quit);
}

#[test]
fn reduced_motion_skips_the_splash_and_every_effect() {
    let mut test = splash_app("still", false);
    assert_eq!(test.app.screen, Screen::Manager);
    let before = test.app.seen();
    test.key(KeyCode::Char('n'));
    test.app.react(before);
    assert!(!test.app.motion.is_running());
}

#[test]
fn changes_start_effects() {
    let mut test = test_app("effects", Some("pw"), "");
    draw_frame(&mut test.app);
    // A failed connect flashes the list.
    test.key(KeyCode::Enter);
    test.app
        .on_engine_event(EngineEvent::Connected(Err(DbError::General("down".into()))));
    assert!(test.app.motion.is_running());

    // A new screen fades in; a new toast slides in.
    test.key(KeyCode::Enter);
    let before = test.app.seen();
    test.app
        .on_engine_event(EngineEvent::Connected(Ok(session_info())));
    test.app
        .toasts
        .push(Level::Info, "hi".into(), Instant::now());
    test.app.react(before);
    assert!(test.app.motion.is_running());
    test.app.motion.finish();
    draw_frame(&mut test.app);
    assert!(!test.app.motion.is_running());

    // Focus and tab changes in the workspace.
    let before = test.app.seen();
    test.key(KeyCode::Tab);
    test.app.react(before);
    assert!(test.app.motion.is_running());
    test.app.motion.finish();
    draw_frame(&mut test.app);
    let before = test.app.seen();
    test.ctrl('n');
    test.app.react(before);
    assert!(test.app.motion.is_running());
    test.app.motion.finish();
    draw_frame(&mut test.app);
}

#[test]
fn finished_queries_flash_the_results() {
    let mut test = test_app("flash", Some("pw"), "");
    test.open_prod();
    draw_frame(&mut test.app);
    for (sql, outcome) in [
        ("select 1", Ok(QueryStats::default())),
        ("select 2", Err(DbError::Cancelled)),
        ("select 3", Err(DbError::General("boom".into()))),
    ] {
        test.app.paste(sql);
        test.key(KeyCode::F(5));
        let id = match &test.sent()[..] {
            [DbCommand::Run { id, .. }] => *id,
            other => panic!("expected a run, got {other:?}"),
        };
        test.app
            .on_engine_event(EngineEvent::QueryDone { id, outcome });
        assert!(test.app.motion.is_running());
        test.app.motion.finish();
        draw_frame(&mut test.app);
        assert!(!test.app.motion.is_running());
        test.ctrl('a');
        test.key(KeyCode::Backspace);
        test.sent();
    }
    // Events of queries not on screen don't flash anything.
    test.app.on_engine_event(EngineEvent::QueryDone {
        id: 999,
        outcome: Ok(QueryStats::default()),
    });
    assert!(!test.app.motion.is_running());
}

fn session_info() -> dfox_core::engine::SessionInfo {
    dfox_core::engine::SessionInfo {
        caps: dfox_core::session::Capabilities::all(),
        language: dfox_core::session::QueryLanguage::Sql(dfox_core::session::SqlDialect::Postgres),
    }
}

fn path(parts: &[&str]) -> NodePath {
    parts.iter().copied().collect()
}
