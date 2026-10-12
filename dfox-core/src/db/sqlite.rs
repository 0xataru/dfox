use async_trait::async_trait;
use sqlx::{
    pool::PoolConnection,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
    Pool, Row, Sqlite, TypeInfo, ValueRef,
};
use tokio::sync::{mpsc, Mutex};

use crate::{
    errors::DbError,
    models::schema::{ColumnSchema, DbObject, ObjectKind, TableSchema},
    result::OwnedCell,
};

use super::{replace_if_lost, stream_raw, undecodable, Chunk, DbClient, StreamSummary};

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

    async fn metadata(&self, query: &str, binds: &[&str]) -> Result<Vec<SqliteRow>, DbError> {
        let mut conn = self.session.lock().await;
        let mut query = sqlx::query(query);
        for value in binds {
            query = query.bind(*value);
        }
        Ok(query.fetch_all(&mut **conn).await?)
    }
}

/// SQLite types each value, not each column, so the value's own storage
/// class decides how it is read.
pub(crate) fn sqlite_cell(row: &SqliteRow, index: usize) -> OwnedCell {
    let value = match row.try_get_raw(index) {
        Ok(value) => value,
        Err(err) => return undecodable("?", err),
    };
    if value.is_null() {
        return OwnedCell::Null;
    }
    let type_name = value.type_info().name().to_string();
    let decoded = match type_name.as_str() {
        "INTEGER" => row.try_get_unchecked::<i64, _>(index).map(OwnedCell::Int),
        "REAL" => row.try_get_unchecked::<f64, _>(index).map(OwnedCell::Float),
        "BLOB" => row
            .try_get_unchecked::<&[u8], _>(index)
            .map(|v| OwnedCell::Bytes(v.into())),
        _ => row
            .try_get_unchecked::<&str, _>(index)
            .map(|v| OwnedCell::Text(v.into())),
    };
    decoded.unwrap_or_else(|err| undecodable(&type_name, err))
}

fn text(row: &SqliteRow, index: usize) -> String {
    sqlite_cell(row, index).into_text().unwrap_or_default()
}

#[async_trait]
impl DbClient for SqliteClient {
    async fn stream(
        &self,
        query: &str,
        row_limit: usize,
        tx: mpsc::Sender<Chunk>,
    ) -> Result<StreamSummary, DbError> {
        let mut conn = self.session.lock().await;
        let result = stream_raw(&mut **conn, query, row_limit, sqlite_cell, &tx).await;
        replace_if_lost(&self.pool, &mut conn, &result).await;
        result
    }

    /// SQLite runs in this process, so there is no server to ask. Dropping the
    /// stream stops the query at its next row.
    async fn cancel(&self) -> Result<bool, DbError> {
        Ok(false)
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
        let rows = self.metadata(query, &[]).await?;
        Ok(rows.iter().map(|row| text(row, 0)).collect())
    }

    /// `main`, then any attached databases.
    async fn list_schemas(&self) -> Result<Vec<String>, DbError> {
        let rows = self
            .metadata("SELECT name FROM pragma_database_list ORDER BY seq", &[])
            .await?;
        Ok(rows.iter().map(|row| text(row, 0)).collect())
    }

    async fn list_objects(&self, schema: &str) -> Result<Vec<DbObject>, DbError> {
        let query = r#"
            SELECT name, type
            FROM pragma_table_list
            WHERE schema = ?1 AND name NOT LIKE 'sqlite_%'
            ORDER BY name
        "#;
        let rows = self.metadata(query, &[schema]).await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let kind = match text(row, 1).as_str() {
                    "table" | "virtual" => ObjectKind::Table,
                    "view" => ObjectKind::View,
                    // Shadow tables belong to virtual tables.
                    _ => return None,
                };
                Some(DbObject {
                    name: text(row, 0),
                    kind,
                    rows: None,
                })
            })
            .collect())
    }

    async fn describe_table(&self, schema: &str, table: &str) -> Result<TableSchema, DbError> {
        let query = r#"SELECT name, type, "notnull", dflt_value FROM pragma_table_info(?1, ?2)"#;
        let rows = self.metadata(query, &[table, schema]).await?;

        let columns = rows
            .iter()
            .map(|row| ColumnSchema {
                name: text(row, 0),
                data_type: text(row, 1),
                is_nullable: text(row, 2) == "0",
                default: sqlite_cell(row, 3).into_text(),
            })
            .collect();

        Ok(TableSchema {
            schema: schema.to_string(),
            table_name: table.to_string(),
            columns,
            indexes: Vec::new(),
        })
    }
}
