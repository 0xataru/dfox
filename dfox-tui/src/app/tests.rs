use std::path::PathBuf;

use super::*;
use crate::{
    connections::{pgpass, secrets::Memory, Tag},
    testing::{render, TestCtx},
};
use dfox_core::{
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
            detected: Vec::new(),
            connect: None,
            problems: Vec::new(),
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
        self.app.apply(EngineEvent::Connected(Ok(())));
        assert_eq!(self.app.screen, Screen::Workspace);
        assert_eq!(self.sent(), [DbCommand::ListDatabases]);
        self.app.apply(EngineEvent::Databases(Ok(vec![
            "postgres".into(),
            "shop".into(),
        ])));
        self.app
            .apply(EngineEvent::Schemas(Ok(vec!["public".into()])));
        self.app.apply(EngineEvent::Objects {
            schema: "public".into(),
            result: Ok(crate::testing::sample_objects()),
        });
        self.sent();
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
            detected: Vec::new(),
            connect: None,
            problems: Vec::new(),
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
    test.app.apply(EngineEvent::Connected(Ok(())));
    let workspace = test.app.workspace.as_ref().expect("workspace");
    assert_eq!(workspace.explorer.active, "postgres");
    assert_eq!(test.sent(), [DbCommand::ListSchemas]);
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
            detected: vec![Detected {
                connection,
                password,
                source: "the command line".into(),
            }],
            connect: Some(EntryKey::Detected(0)),
            problems: Vec::new(),
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
    test.app.apply(EngineEvent::Columns {
        id,
        columns: vec![
            ColumnMeta::new("id", "int4"),
            ColumnMeta::new("name", "text"),
        ],
    });
    test.app.apply(EngineEvent::Rows {
        id,
        rows: vec![
            vec![OwnedCell::Int(1), OwnedCell::Text("Ada".into())],
            vec![OwnedCell::Int(2), OwnedCell::Null],
        ],
    });
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
        [DbCommand::ListObjects {
            schema: "public".into()
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
            detected: Vec::new(),
            connect: None,
            problems: vec!["connections.toml: oops".into()],
        },
    );
    let texts: Vec<_> = app.toasts.items().iter().map(|t| t.text.as_str()).collect();
    assert_eq!(
        texts,
        ["connections.toml: oops", "unknown theme \"missing\""]
    );
}
