//! Drivers: what the engine connects through. Each database lives in its own
//! crate behind a cargo feature of the app, and the app hands the engine the
//! drivers it was built with.
//!
//! A driver opens a [`Session`] (Arrow batches, a catalog tree, edits) with
//! [`Driver::open`].

use std::{fmt, sync::Arc, time::Duration};

use futures_util::future::BoxFuture;
use tokio::time::Instant;

use crate::{
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
    session::Session,
};

/// How a driver is shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverInfo {
    /// A short stable id: `postgres`, `mysql`, `sqlite`.
    pub id: &'static str,
    /// The name people know: `PostgreSQL`.
    pub name: &'static str,
    /// The usual port, for servers.
    pub default_port: Option<u16>,
    /// URL schemes it accepts: `postgres`, `postgresql`.
    pub url_schemes: &'static [&'static str],
}

/// What a connection form field holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKind {
    /// Free text.
    Text,
    /// A port number.
    Port,
    /// A secret, kept in the keychain and never shown.
    Password,
    /// A file on disk.
    Path,
    /// Yes or no.
    Bool,
    /// One of these values.
    Choice(Vec<String>),
}

/// One field of a connection form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    /// The key it is saved under: `host`, `port`, `tls`.
    pub key: &'static str,
    /// What the form shows beside it.
    pub label: &'static str,
    /// What it holds.
    pub kind: FieldKind,
    /// Its value in a new connection.
    pub default: Option<String>,
    /// True when a connection can't be made without it.
    pub required: bool,
}

/// The fields a driver needs to connect, so the app can draw its form
/// without knowing the database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormSpec {
    /// The fields, in the order they are shown.
    pub fields: Vec<FormField>,
}

/// The usual form of a database server: host, port, user, password,
/// database, TLS and read-only.
pub fn server_form(port: u16) -> FormSpec {
    let field = |key, label, kind, default: Option<&str>, required| FormField {
        key,
        label,
        kind,
        default: default.map(str::to_string),
        required,
    };
    let tls = ["prefer", "require", "disable"]
        .map(str::to_string)
        .to_vec();
    FormSpec {
        fields: vec![
            field("host", "Host", FieldKind::Text, Some("localhost"), true),
            field(
                "port",
                "Port",
                FieldKind::Port,
                Some(&port.to_string()),
                true,
            ),
            field("user", "User", FieldKind::Text, None, false),
            field("password", "Password", FieldKind::Password, None, false),
            field("database", "Database", FieldKind::Text, None, false),
            field("tls", "TLS", FieldKind::Choice(tls), Some("prefer"), false),
            field(
                "read_only",
                "Read-only",
                FieldKind::Bool,
                Some("false"),
                false,
            ),
        ],
    }
}

/// A database dfox can connect to.
pub trait Driver: Send + Sync + 'static {
    /// The kind of database this driver connects to.
    fn db_type(&self) -> DbType;

    /// How the driver is shown to the user.
    fn info(&self) -> DriverInfo;

    /// The fields its connection form needs.
    fn connection_form(&self) -> FormSpec {
        FormSpec::default()
    }

    /// Opens a [`Session`] from typed-in connection details.
    fn open<'a>(
        &'a self,
        params: &'a ConnectionParams,
    ) -> BoxFuture<'a, Result<Box<dyn Session>, DbError>> {
        let _ = params;
        let name = self.info().name;
        Box::pin(async move { Err(unsupported(name)) })
    }
}

fn unsupported(driver: &str) -> DbError {
    DbError::Config(format!("the {driver} driver can't open a session yet"))
}

/// The drivers an app was built with.
#[derive(Clone, Default)]
pub struct Drivers(Arc<Vec<Arc<dyn Driver>>>);

impl fmt::Debug for Drivers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|d| d.db_type()))
            .finish()
    }
}

impl Drivers {
    /// The drivers a build has.
    pub fn new(drivers: Vec<Arc<dyn Driver>>) -> Self {
        Self(Arc::new(drivers))
    }

    /// The driver for `db_type`, if the build has one.
    pub fn get(&self, db_type: DbType) -> Option<&Arc<dyn Driver>> {
        self.0.iter().find(|d| d.db_type() == db_type)
    }
}

/// How long it takes to reach the database, without logging in: a TCP
/// connect to the server (or its SSH jump host), or reading a SQLite file's
/// metadata. Needs no
/// password and no driver.
pub async fn ping(db_type: DbType, params: &ConnectionParams) -> Result<Duration, DbError> {
    const TIMEOUT: Duration = Duration::from_secs(3);
    let started = Instant::now();
    match db_type {
        DbType::Sqlite => {
            let path = params.database.as_deref().unwrap_or_default();
            tokio::fs::metadata(path)
                .await
                .map_err(|err| DbError::Connection(format!("{path}: {err}")))?;
        }
        _ => {
            // Through a tunnel, the jump host is what has to be reachable.
            let address = match &params.ssh {
                Some(ssh) => (ssh.host.as_str(), ssh.port.unwrap_or(22)),
                None => (params.host.as_str(), params.port),
            };
            tokio::time::timeout(TIMEOUT, tokio::net::TcpStream::connect(address))
                .await
                .map_err(|_| DbError::Connection("timed out".to_string()))?
                .map_err(|err| DbError::Connection(err.to_string()))?;
        }
    }
    Ok(started.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nothing;

    impl Driver for Nothing {
        fn db_type(&self) -> DbType {
            DbType::Sqlite
        }

        fn info(&self) -> DriverInfo {
            DriverInfo {
                id: "nothing",
                name: "Nothing",
                default_port: None,
                url_schemes: &[],
            }
        }
    }

    #[tokio::test]
    async fn a_driver_says_what_it_can_not_open() {
        let params = ConnectionParams::default();
        let session = Nothing.open(&params).await.err().expect("an error");
        assert_eq!(
            session.to_string(),
            "Configuration error: the Nothing driver can't open a session yet"
        );
        assert_eq!(Nothing.connection_form(), FormSpec::default());
        let form = server_form(5432);
        assert_eq!(form.fields[1].default.as_deref(), Some("5432"));
        assert!(form.fields[0].required);
    }

    #[test]
    fn a_build_without_a_driver_has_none() {
        let drivers = Drivers::default();
        assert!(drivers.get(DbType::Postgres).is_none());
        assert_eq!(format!("{drivers:?}"), "[]");
    }

    #[tokio::test]
    async fn ping_checks_sqlite_files_without_a_driver() {
        let file = std::env::temp_dir().join(format!("dfox-ping-{}.db", std::process::id()));
        std::fs::write(&file, b"").expect("write");
        let params = ConnectionParams {
            database: Some(file.to_string_lossy().into_owned()),
            ..Default::default()
        };
        assert!(ping(DbType::Sqlite, &params).await.is_ok());
        let _ = std::fs::remove_file(&file);
        assert!(ping(DbType::Sqlite, &params).await.is_err());
    }
}
