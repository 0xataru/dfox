use super::*;
use crate::testing::{render, Harness, TestCtx};
use dfox_core::models::connections::TlsMode;

fn manager() -> Manager {
    let prod = Connection {
        host: "db.example.com".into(),
        user: "app".into(),
        database: Some("shop".into()),
        tag: Tag::Red,
        read_only: true,
        tls: TlsMode::Require,
        ..Connection::new("prod", DbType::Postgres)
    };
    let local = Connection {
        database: Some("/home/me/app.db".into()),
        ..Connection::new("local", DbType::Sqlite)
    };
    let found = Detected {
        connection: Connection {
            user: "root".into(),
            ..Connection::new("DATABASE_URL", DbType::MySql)
        },
        password: Some("pw".into()),
        source: "DATABASE_URL".into(),
    };
    Manager::new(vec![prod, local], vec![found])
}

#[test]
fn every_connection_is_pinged_once_at_a_time() {
    let mut harness = Harness::new();
    let mut manager = manager();
    manager.ping_all(&mut harness.ctx());
    manager.ping_all(&mut harness.ctx());
    let ids: Vec<u64> = harness
        .sent()
        .into_iter()
        .map(|c| match c {
            DbCommand::Ping { id, params, .. } => {
                assert!(params.password.is_empty());
                id
            }
            other => panic!("expected a ping, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, [1, 2, 3]);

    manager.on_pong(1, Ok(Duration::from_millis(12)));
    manager.on_pong(2, Err(DbError::Connection("no such file".into())));
    assert!(manager.is_animating());
    manager.on_pong(3, Ok(Duration::from_millis(3)));
    assert!(!manager.is_animating());
    assert_eq!(manager.health["prod"].latencies, [12]);
}

#[test]
fn keys_ask_for_the_right_command() {
    let mut harness = Harness::new();
    let mut manager = manager();
    let mut press = |manager: &mut Manager, action| manager.handle(action, &mut harness.ctx());

    assert_eq!(
        press(&mut manager, Action::Confirm),
        Some(Command::Connect(EntryKey::Saved("prod".into())))
    );
    assert_eq!(
        press(&mut manager, Action::Delete),
        Some(Command::ConfirmDelete("prod".into()))
    );
    press(&mut manager, Action::End);
    assert_eq!(
        press(&mut manager, Action::Edit),
        Some(Command::SaveDetected(0))
    );
    assert_eq!(press(&mut manager, Action::Delete), None);
    assert_eq!(
        press(&mut manager, Action::New),
        Some(Command::NewConnection)
    );
}

#[test]
fn manager_with_health() {
    let test = TestCtx::new();
    let mut manager = manager();
    let mut harness = Harness::new();
    manager.ping_all(&mut harness.ctx());
    for ms in [12, 15, 9, 30, 11] {
        manager.on_pong(1, Ok(Duration::from_millis(ms)));
        manager.pings.insert(1, "prod".into());
    }
    manager.on_pong(
        2,
        Err(DbError::Connection("/home/me/app.db: No such file".into())),
    );
    insta::assert_snapshot!(render(&test, 120, 20, |f, area, ctx| manager.render(f, area, ctx)));
}

#[test]
fn manager_with_a_found_connection_selected() {
    let test = TestCtx::new();
    let mut manager = manager();
    manager.select(2);
    insta::assert_snapshot!(render(&test, 80, 16, |f, area, ctx| manager.render(f, area, ctx)));
}

#[test]
fn manager_when_empty() {
    let test = TestCtx::new();
    let manager = Manager::default();
    insta::assert_snapshot!(render(&test, 80, 16, |f, area, ctx| manager.render(f, area, ctx)));
}
