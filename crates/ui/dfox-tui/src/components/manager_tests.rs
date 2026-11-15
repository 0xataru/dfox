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
    assert_eq!(manager.health["prod"].latencies, [12_000]);
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

#[test]
fn manager_while_looking_for_databases() {
    let test = TestCtx::new();
    let mut manager = Manager {
        searching: true,
        ..Manager::default()
    };
    insta::assert_snapshot!(
        "manager_looking_when_empty",
        render(&test, 80, 16, |f, area, ctx| manager.render(f, area, ctx))
    );
    manager
        .saved
        .push(Connection::new("local", DbType::Postgres));
    insta::assert_snapshot!(
        "manager_looking_with_one",
        render(&test, 80, 8, |f, area, ctx| manager.render(f, area, ctx))
    );
    assert!(manager.is_animating());
}

#[test]
fn scans_add_new_databases_and_drop_gone_ones() {
    let at = |name: &str, host: &str, port: u16| {
        let mut connection = Connection::new(name, DbType::Postgres);
        connection.host = host.into();
        connection.port = Some(port);
        connection
    };
    let detected = |connection| Detected {
        connection,
        password: None,
        source: "docker: postgres:16".into(),
    };
    let names = |m: &Manager| -> Vec<String> {
        m.detected
            .iter()
            .map(|d| d.connection.name.clone())
            .collect()
    };
    let mut manager = Manager::new(
        vec![at("saved", "localhost", 5432)],
        vec![detected(at("from env", "db.example.com", 5432))],
    );
    manager.rescanned(vec![
        detected(at("same", "127.0.0.1", 5432)),
        detected(at("forward", "127.0.0.1", 15432)),
        detected(at("again", "127.0.0.1", 15432)),
    ]);
    assert_eq!(names(&manager), ["from env", "forward"]);
    // Still there, and one more: nothing doubles.
    manager.rescanned(vec![
        detected(at("forward", "127.0.0.1", 15432)),
        detected(at("cache", "127.0.0.1", 16379)),
    ]);
    assert_eq!(names(&manager), ["from env", "forward", "cache"]);
    // The forward closed: it goes, what wasn't scanned stays.
    manager.rescanned(vec![detected(at("cache", "127.0.0.1", 16379))]);
    assert_eq!(names(&manager), ["from env", "cache"]);
    // Saved since it was found: the found one goes.
    manager.saved.push(at("my cache", "localhost", 16379));
    manager.rescanned(vec![detected(at("cache", "127.0.0.1", 16379))]);
    assert_eq!(names(&manager), ["from env"]);
}

#[test]
fn the_same_database_found_twice_is_listed_once() {
    let found = |name: &str, url: &str| {
        let (connection, password) = crate::connections::url::parse(name, url).expect("url");
        Detected {
            connection,
            password,
            source: name.into(),
        }
    };
    let manager = Manager::new(
        Vec::new(),
        vec![
            found("DATABASE_URL", "postgres://app@localhost:5432/shop"),
            found("POSTGRES_URL", "postgres://app@127.0.0.1/shop"),
            found("a.db", "a.db"),
            found("b.db", "b.db"),
        ],
    );
    let names: Vec<&str> = manager
        .detected
        .iter()
        .map(|d| d.connection.name.as_str())
        .collect();
    assert_eq!(names, ["DATABASE_URL", "a.db", "b.db"]);
}

#[test]
fn one_database_found_several_ways_merges_what_each_knows() {
    let found = |source: &str, url: &str, password: Option<&str>| {
        let (connection, _) = crate::connections::url::parse(source, url).expect("url");
        Detected {
            connection,
            password: password.map(str::to_string),
            source: source.into(),
        }
    };
    let mut manager = Manager::new(
        Vec::new(),
        vec![
            found("PG variables", "postgres://127.0.0.1:15432", None),
            found("DATABASE_URL", "postgres://app@localhost:15432/shop", None),
        ],
    );
    let container = || {
        found(
            "docker: postgres:16",
            "postgres://postgres@127.0.0.1:15432/postgres",
            Some("dfox"),
        )
    };
    manager.rescanned(vec![container()]);
    // Again on the next scan: the source isn't added twice.
    manager.rescanned(vec![container()]);
    assert_eq!(manager.detected.len(), 1);
    let one = &manager.detected[0];
    assert_eq!(
        one.source,
        "PG variables · DATABASE_URL · docker: postgres:16"
    );
    assert_eq!(one.connection.database.as_deref(), Some("shop"));
    // The container's password is postgres's, not app's: it stays out.
    assert_eq!(
        (one.connection.user.as_str(), one.password.as_deref()),
        ("app", None)
    );

    // Without a user of its own, the entry takes the container's with its
    // password.
    let mut manager = Manager::new(
        Vec::new(),
        vec![found("PG variables", "postgres://127.0.0.1:15432", None)],
    );
    manager.rescanned(vec![container()]);
    let one = &manager.detected[0];
    assert_eq!(
        (one.connection.user.as_str(), one.password.as_deref()),
        ("postgres", Some("dfox"))
    );
}

#[test]
fn found_connections_sit_in_sections_and_the_cursor_stays_put() {
    let found = |name: &str, port: u16, source: &str| {
        let mut connection = Connection::new(name, DbType::Postgres);
        connection.host = "127.0.0.1".into();
        connection.port = Some(port);
        Detected {
            connection,
            password: None,
            source: source.into(),
        }
    };
    let mut manager = Manager::new(
        Vec::new(),
        vec![
            found("cli", 1, "the command line"),
            found("forward", 2, "port-forward from k9s"),
            found("env", 3, "DATABASE_URL"),
        ],
    );
    let names = |m: &Manager| -> Vec<String> {
        m.detected
            .iter()
            .map(|d| d.connection.name.clone())
            .collect()
    };
    assert_eq!(names(&manager), ["cli", "env", "forward"]);
    // On the forward, then a scan brings containers and a cluster's.
    manager.select(2);
    manager.rescanned(vec![
        found("pg (stage)", 4, "kubernetes: do-ams3-stage"),
        found("db-1", 5, "docker: postgres:16"),
    ]);
    assert_eq!(
        names(&manager),
        ["cli", "env", "db-1", "pg (stage)", "forward"]
    );
    assert_eq!(
        manager
            .entry(manager.selected)
            .map(|(_, c)| c.name.clone())
            .as_deref(),
        Some("forward")
    );
    assert_eq!(group(&manager.detected[3]).1, "Kubernetes · stage (do)");
}

#[test]
fn each_cluster_has_one_section() {
    let found = |name: &str, port: u16, source: &str| {
        let mut connection = Connection::new(name, DbType::Postgres);
        connection.host = "127.0.0.1".into();
        connection.port = Some(port);
        Detected {
            connection,
            password: None,
            source: source.into(),
        }
    };
    let mut manager = Manager::new(
        Vec::new(),
        vec![
            found("a", 1, "kubernetes: do-ams3-stage"),
            found("b", 2, "kubernetes: gke_p_z_prod"),
        ],
    );
    // A stage one found later joins its cluster, not the end; clusters
    // follow their headings' order.
    manager.rescanned(vec![found("c", 3, "kubernetes: do-ams3-stage")]);
    let names: Vec<&str> = manager
        .detected
        .iter()
        .map(|d| d.connection.name.as_str())
        .collect();
    assert_eq!(names, ["b", "a", "c"]);
}

#[test]
fn a_database_saved_under_an_alias_isnt_found_again() {
    let kube = |namespace: &str, service: &str| {
        let mut connection = Connection::new(service, DbType::Postgres);
        connection.port = Some(5432);
        connection.kube = Some(dfox_core::models::connections::KubeParams {
            context: Some("do".into()),
            namespace: namespace.into(),
            service: service.into(),
        });
        connection
    };
    let found = Detected {
        connection: kube("apps", "db"),
        password: None,
        source: "kubernetes: do · also svc/stage-postgresql in ns stage".into(),
    };
    let mut manager = Manager::new(vec![kube("stage", "stage-postgresql")], Vec::new());
    manager.rescanned(vec![found]);
    assert!(manager.detected.is_empty());
}
