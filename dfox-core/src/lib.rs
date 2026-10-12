use db::{mysql::MySqlClient, postgres::PostgresClient, sqlite::SqliteClient, DbClient};
use errors::DbError;
use models::connections::{ConnectionConfig, ConnectionParams, DbType};
use std::sync::Arc;
use tokio::sync::Mutex;

pub mod db;
pub mod errors;
pub mod models;

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
                let options = sqlx::sqlite::SqliteConnectOptions::new().filename(path);
                Box::new(SqliteClient::connect_with(options).await?)
            }
        })
    }
}
