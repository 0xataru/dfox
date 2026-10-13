//! Drivers: what the engine connects through. Each database lives in its own
//! crate behind a cargo feature of the app, and the app hands the engine the
//! drivers it was built with.

use std::{fmt, sync::Arc, time::Duration};

use futures_util::future::BoxFuture;
use tokio::time::Instant;

use crate::{
    db::DbClient,
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
};

/// A database dfox can connect to.
pub trait Driver: Send + Sync + 'static {
    /// The kind of database this driver connects to.
    fn db_type(&self) -> DbType;

    /// Opens a session from typed-in connection details.
    fn connect<'a>(
        &'a self,
        params: &'a ConnectionParams,
    ) -> BoxFuture<'a, Result<Box<dyn DbClient>, DbError>>;
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
    pub fn new(drivers: Vec<Arc<dyn Driver>>) -> Self {
        Self(Arc::new(drivers))
    }

    pub fn get(&self, db_type: DbType) -> Option<&Arc<dyn Driver>> {
        self.0.iter().find(|d| d.db_type() == db_type)
    }

    /// Connects with the driver for `db_type`, or says that this build has
    /// none.
    pub async fn connect(
        &self,
        db_type: DbType,
        params: &ConnectionParams,
    ) -> Result<Box<dyn DbClient>, DbError> {
        let driver = self.get(db_type).ok_or_else(|| {
            DbError::Config(format!("this build of dfox has no {db_type:?} driver"))
        })?;
        driver.connect(params).await
    }
}

/// How long it takes to reach the database, without logging in: a TCP
/// connect to a server, or opening a SQLite file's metadata. Needs no
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
            let address = (params.host.as_str(), params.port);
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

    #[tokio::test]
    async fn a_missing_driver_is_an_error() {
        let drivers = Drivers::default();
        assert!(drivers.get(DbType::Postgres).is_none());
        let err = drivers
            .connect(DbType::Postgres, &ConnectionParams::default())
            .await
            .err()
            .expect("an error");
        assert_eq!(
            err.to_string(),
            "Configuration error: this build of dfox has no Postgres driver"
        );
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
