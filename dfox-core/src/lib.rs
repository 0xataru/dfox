use db::{mysql::MySqlClient, postgres::PostgresClient, sqlite::SqliteClient, DbClient};
use errors::DbError;
use models::connections::{ConnectionConfig, ConnectionParams, DbType};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub mod db;
pub mod engine;
pub mod errors;
pub mod models;
pub mod result;

#[derive(Default)]
pub struct DbManager {
    pub connections: Arc<Mutex<Vec<Box<dyn DbClient + Send + Sync>>>>,
}

impl DbManager {
    pub fn new() -> Self {
        DbManager {
            connections: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn add_connection(&self, config: ConnectionConfig) -> Result<(), DbError> {
        let client: Box<dyn DbClient + Send + Sync> = match config.db_type {
            DbType::Postgres => Box::new(PostgresClient::connect(&config.database_url).await?),
            DbType::MySql => Box::new(MySqlClient::connect(&config.database_url).await?),
            DbType::Sqlite => Box::new(SqliteClient::connect(&config.database_url).await?),
        };
        self.connections.lock().await.push(client);
        Ok(())
    }

    /// Connects to a server from typed-in fields. For SQLite, `params.database`
    /// is the file path and the other fields are ignored.
    pub async fn connect(
        db_type: &DbType,
        params: &ConnectionParams,
    ) -> Result<Box<dyn DbClient + Send + Sync>, DbError> {
        Ok(match db_type {
            DbType::Postgres => Box::new(PostgresClient::connect_params(params).await?),
            DbType::MySql => Box::new(MySqlClient::connect_params(params).await?),
            DbType::Sqlite => {
                let path = params.database.as_deref().ok_or_else(|| {
                    DbError::Config("SQLite needs a database file path".to_string())
                })?;
                let options = sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(path)
                    .read_only(params.read_only);
                Box::new(SqliteClient::connect_with(options).await?)
            }
        })
    }

    /// How long it takes to reach the database, without logging in: a TCP
    /// connect to a server, or opening a SQLite file's metadata. Needs no
    /// password.
    pub async fn ping(db_type: &DbType, params: &ConnectionParams) -> Result<Duration, DbError> {
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
}
