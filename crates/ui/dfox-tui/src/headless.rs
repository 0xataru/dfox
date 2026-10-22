//! `dfox export` and `dfox copy`: the engine's export and copy without the
//! TUI, for scripts. Connections are saved names or URLs; secrets come from
//! the keychain, `~/.pgpass` or the URL, and nothing is asked.

use std::io::{IsTerminal, Write};

use dfox_core::{
    engine::{Command as DbCommand, EngineEvent, EngineHandle, QueryStats},
    errors::DbError,
    models::connections::ConnectionParams,
};
use tokio::sync::mpsc;

use crate::connections::{self, pgpass, secrets::Secrets, ssh_secret_name, Connection, Store};

/// `name` as it can be shown: a URL without its password.
pub fn shown(name: &str) -> String {
    match url::Url::parse(name) {
        Ok(mut url) if url.password().is_some() => {
            let _ = url.set_password(Some("***"));
            url.to_string()
        }
        _ => name.to_string(),
    }
}

/// The connection `name` names, a saved one or a URL, and how to open it.
pub fn resolve(
    name: &str,
    store: &Store,
    secrets: &dyn Secrets,
    pgpass: &[pgpass::Entry],
) -> Result<(Connection, ConnectionParams), String> {
    let (connection, password) = match store.get(name) {
        Some(saved) => (saved.clone(), secrets.get(name)?),
        None => connections::url::parse(&shown(name), name).map_err(|_| {
            format!(
                "{:?} is not a saved connection, a database URL or a SQLite file",
                shown(name)
            )
        })?,
    };
    let password = password
        .or_else(|| pgpass::lookup(pgpass, &connection).map(str::to_string))
        .unwrap_or_default();
    let ssh = match connection.ssh {
        Some(_) => secrets.get(&ssh_secret_name(&connection.name))?,
        None => None,
    };
    if ssh.is_none() && connection.needs_ssh_password() {
        return Err(format!(
            "{name}'s SSH password isn't saved: save it in the connection list first"
        ));
    }
    let params = connection.params(&password, ssh.as_deref());
    Ok((connection, params))
}

/// Connects to `connection` and runs `command`, an export, import or copy,
/// to its end. With `progress`, the rows so far are shown on stderr.
pub async fn run(
    engine: &EngineHandle,
    events: &mut mpsc::Receiver<EngineEvent>,
    connection: &Connection,
    params: ConnectionParams,
    command: DbCommand,
    progress: bool,
) -> Result<QueryStats, String> {
    engine.send(DbCommand::Connect {
        db_type: connection.driver,
        params,
    });
    let failed = |err: DbError| err.to_string();
    loop {
        match events.recv().await {
            Some(EngineEvent::Connected(result)) => {
                result.map_err(|err| format!("couldn't connect to {}: {err}", connection.name))?;
                break;
            }
            Some(_) => {}
            None => return Err("the engine stopped".into()),
        }
    }
    engine.send(command);
    let mut stderr = std::io::stderr();
    loop {
        match events.recv().await {
            Some(EngineEvent::Progress { rows, .. }) if progress => {
                let _ = write!(
                    stderr,
                    "\r{} rows",
                    crate::components::results::grid::group_digits(&rows.to_string())
                );
                let _ = stderr.flush();
            }
            Some(EngineEvent::Notice { message, .. }) => eprintln!("{message}"),
            Some(EngineEvent::QueryDone { outcome, .. }) => {
                if progress {
                    let _ = write!(stderr, "\r\x1b[K");
                }
                return outcome.map_err(failed);
            }
            Some(_) => {}
            None => return Err("the engine stopped".into()),
        }
    }
}

/// True when progress can be redrawn in place: stderr is a terminal.
pub fn shows_progress() -> bool {
    std::io::stderr().is_terminal()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use std::sync::Arc;

    use dfox_core::{catalog::NodePath, driver::Drivers, engine::CopyTarget};

    use super::*;
    use crate::connections::secrets::Memory;

    /// A fresh SQLite file's path, removed when the test ends.
    struct File(std::path::PathBuf);

    impl File {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("dfox-headless-{name}-{}.db", std::process::id()));
            let _ = std::fs::remove_file(&path);
            std::fs::File::create(&path).expect("create");
            Self(path)
        }

        fn url(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for File {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn engine() -> (EngineHandle, mpsc::Receiver<EngineEvent>) {
        dfox_core::engine::spawn(Drivers::new(vec![Arc::new(
            dfox_driver_sqlite::SqliteDriver,
        )]))
    }

    #[tokio::test]
    async fn queries_export_and_copy_without_the_tui() {
        let source = File::new("source");
        let target = File::new("target");
        let store = Store::default();
        let (from, from_params) =
            resolve(&source.url(), &store, &Memory::default(), &[]).expect("a SQLite file");
        let (to, to_params) =
            resolve(&target.url(), &store, &Memory::default(), &[]).expect("a SQLite file");

        // Make the source's rows, through the same engine.
        let (engine, mut events) = engine();
        let make = DbCommand::Run {
            id: 1,
            sql: "CREATE TABLE t (n INTEGER, s TEXT); INSERT INTO t VALUES (1, 'a'), (2, 'b')"
                .into(),
            row_limit: 0,
        };
        run(
            &engine,
            &mut events,
            &from,
            from_params.clone(),
            make,
            false,
        )
        .await
        .expect("make");

        let csv = std::env::temp_dir().join(format!("dfox-headless-{}.csv", std::process::id()));
        let export = DbCommand::Export {
            id: 2,
            sql: "SELECT n, s FROM t ORDER BY n".into(),
            file: csv.clone(),
        };
        let stats = run(
            &engine,
            &mut events,
            &from,
            from_params.clone(),
            export,
            false,
        )
        .await
        .expect("export");
        assert_eq!(stats.rows, 2);
        assert_eq!(
            std::fs::read_to_string(&csv).expect("csv"),
            "n,s\n1,a\n2,b\n"
        );
        let _ = std::fs::remove_file(&csv);

        let copy = DbCommand::Copy {
            id: 3,
            sql: "SELECT n, s FROM t".into(),
            target: Box::new(CopyTarget {
                db_type: to.driver,
                params: to_params.clone(),
                table: NodePath::root().child("main").child("copied"),
                create: true,
            }),
        };
        let stats = run(&engine, &mut events, &from, from_params, copy, false)
            .await
            .expect("copy");
        assert_eq!(stats.rows, 2);

        // The copy is there: export it from the target.
        let check =
            std::env::temp_dir().join(format!("dfox-headless-check-{}.csv", std::process::id()));
        let export = DbCommand::Export {
            id: 4,
            sql: "SELECT count(*) AS rows FROM copied".into(),
            file: check.clone(),
        };
        run(&engine, &mut events, &to, to_params, export, false)
            .await
            .expect("check");
        assert_eq!(std::fs::read_to_string(&check).expect("csv"), "rows\n2\n");
        let _ = std::fs::remove_file(&check);
    }

    #[test]
    fn unknown_names_and_urls_are_explained() {
        let err = resolve("nope", &Store::default(), &Memory::default(), &[]).unwrap_err();
        assert!(err.contains("not a saved connection"), "{err}");
        let err = resolve(
            "oracle://u:secret@h/db",
            &Store::default(),
            &Memory::default(),
            &[],
        )
        .unwrap_err();
        assert!(!err.contains("secret"), "{err}");
    }

    #[test]
    fn passwords_are_never_shown() {
        assert_eq!(
            shown("mysql://root:dfox@127.0.0.1:13306/app"),
            "mysql://root:***@127.0.0.1:13306/app"
        );
        assert_eq!(shown("prod"), "prod");
        let (connection, params) = resolve(
            "postgres://app:s3cret@db.example.com/shop",
            &Store::default(),
            &Memory::default(),
            &[],
        )
        .expect("a URL");
        assert_eq!(params.password, "s3cret");
        assert!(!connection.name.contains("s3cret"), "{}", connection.name);
    }
}
