use async_trait::async_trait;
use sqlx::{
    pool::PoolConnection,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
    Pool, Row, Sqlite, TypeInfo, ValueRef,
};
use tokio::sync::Mutex;

use crate::{
    errors::DbError,
    models::schema::{ColumnSchema, TableSchema},
};

use super::{hex, run_on_session, undecodable, DbClient, QueryOutput};

pub struct SqliteClient {
    /// Used to replace a dead session connection.
    pub pool: Pool<Sqlite>,
    /// Pinned connection for user SQL and metadata. SQLite gains nothing from
    /// a second connection, and one would block on the session's write locks.
    session: Mutex<PoolConnection<Sqlite>>,
}

impl SqliteClient {
    pub async fn connect(database_url: &str) -> Result<Self, DbError> {
        let options = database_url
            .parse::<SqliteConnectOptions>()
            .map_err(|e| DbError::Config(e.to_string()))?;
        Self::connect_with(options).await
    }

    pub async fn connect_with(options: SqliteConnectOptions) -> Result<Self, DbError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|e| DbError::Connection(e.to_string()))?;
        let session = pool
            .acquire()
            .await
            .map_err(|e| DbError::Connection(e.to_string()))?;

        Ok(Self {
            pool,
            session: Mutex::new(session),
        })
    }

    /// Closes every connection. The session connection is released first,
    /// because the pool waits for all of its connections to come back.
    pub async fn close(self) {
        drop(self.session);
        self.pool.close().await;
    }

    async fn metadata(&self, query: &str, bind: Option<&str>) -> Result<Vec<SqliteRow>, DbError> {
        let mut conn = self.session.lock().await;
        let mut query = sqlx::query(query);
        if let Some(value) = bind {
            query = query.bind(value);
        }
        Ok(query.fetch_all(&mut **conn).await?)
    }
}

/// SQLite types each value, not each column, so the value's own storage
/// class decides how it is read.
pub(crate) fn sqlite_cell(row: &SqliteRow, index: usize) -> Option<String> {
    let value = match row.try_get_raw(index) {
        Ok(value) => value,
        Err(err) => return undecodable("?", err),
    };
    if value.is_null() {
        return None;
    }
    let type_name = value.type_info().name().to_string();
    let decoded = match type_name.as_str() {
        "INTEGER" => row
            .try_get_unchecked::<i64, _>(index)
            .map(|v| v.to_string()),
        // Debug formatting keeps the ".0" on whole numbers, like the sqlite3 shell.
        "REAL" => row
            .try_get_unchecked::<f64, _>(index)
            .map(|v| format!("{v:?}")),
        "BLOB" => row.try_get_unchecked::<&[u8], _>(index).map(hex),
        _ => row.try_get_unchecked::<String, _>(index),
    };
    match decoded {
        Ok(text) => Some(text),
        Err(err) => undecodable(&type_name, err),
    }
}

fn text(row: &SqliteRow, index: usize) -> String {
    sqlite_cell(row, index).unwrap_or_default()
}

#[async_trait]
impl DbClient for SqliteClient {
    async fn execute(&self, query: &str) -> Result<(), DbError> {
        run_on_session(&self.pool, &self.session, query, 0, sqlite_cell).await?;
        Ok(())
    }

    async fn run(&self, query: &str, row_limit: usize) -> Result<QueryOutput, DbError> {
        run_on_session(&self.pool, &self.session, query, row_limit, sqlite_cell).await
    }

    async fn list_databases(&self) -> Result<Vec<String>, DbError> {
        // SQLite doesn't support listing databases as it works with a single database file
        Ok(vec!["main".to_string()])
    }

    async fn list_tables(&self) -> Result<Vec<String>, DbError> {
        let query = r#"
            SELECT name
            FROM sqlite_schema
            WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
            ORDER BY name
        "#;
        let rows = self.metadata(query, None).await?;
        Ok(rows.iter().map(|row| text(row, 0)).collect())
    }

    async fn describe_table(&self, table_name: &str) -> Result<TableSchema, DbError> {
        let query = r#"SELECT name, type, "notnull", dflt_value FROM pragma_table_info(?1)"#;
        let rows = self.metadata(query, Some(table_name)).await?;

        let columns = rows
            .iter()
            .map(|row| ColumnSchema {
                name: text(row, 0),
                data_type: text(row, 1),
                is_nullable: text(row, 2) == "0",
                default: sqlite_cell(row, 3),
            })
            .collect();

        Ok(TableSchema {
            table_name: table_name.to_string(),
            columns,
            indexes: Vec::new(),
        })
    }
}
