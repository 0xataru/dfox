use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use sqlx::{
    mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlRow},
    pool::PoolConnection,
    Executor, MySql, MySqlPool, Row, TypeInfo, ValueRef,
};
use tokio::sync::{mpsc, Mutex};

use crate::{
    errors::DbError,
    models::{
        connections::ConnectionParams,
        schema::{ColumnSchema, TableSchema},
    },
    result::OwnedCell,
};

use super::{
    bytes_cell, replace_if_lost, replace_session, stream_raw, undecodable, Chunk, DbClient,
    RunGuard, StreamSummary,
};

pub struct MySqlClient {
    /// Used for metadata, and to replace a dead session connection.
    pub pool: MySqlPool,
    /// Pinned connection for user SQL, so session state survives between runs.
    session: Mutex<PoolConnection<MySql>>,
    /// Connection id of the session connection, for cancelling; 0 until the
    /// first query on a connection looks it up.
    session_id: AtomicU64,
    run_guard: RunGuard,
}

impl MySqlClient {
    pub async fn connect(database_url: &str) -> Result<Self, DbError> {
        let options = database_url
            .parse::<MySqlConnectOptions>()
            .map_err(|e| DbError::Config(e.to_string()))?;
        Self::connect_with(options).await
    }

    pub async fn connect_params(params: &ConnectionParams) -> Result<Self, DbError> {
        let mut options = MySqlConnectOptions::new()
            .host(&params.host)
            .port(params.port)
            .username(&params.username)
            .password(&params.password);
        if let Some(database) = &params.database {
            options = options.database(database);
        }
        Self::connect_with(options).await
    }

    pub async fn connect_with(options: MySqlConnectOptions) -> Result<Self, DbError> {
        let pool = MySqlPoolOptions::new()
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
            session_id: AtomicU64::new(0),
            run_guard: RunGuard::default(),
        })
    }

    /// Closes every connection. The session connection is released first,
    /// because the pool waits for all of its connections to come back.
    pub async fn close(self) {
        drop(self.session);
        self.pool.close().await;
    }
}

/// MySQL sends every value as length-prefixed bytes: text in the text
/// protocol, and text or raw bytes for strings in the binary protocol. Reading
/// the bytes directly works for every column type, including unsigned
/// integers, DECIMAL, BIT and columns with a binary collation.
pub(crate) fn mysql_cell(row: &MySqlRow, index: usize) -> OwnedCell {
    let value = match row.try_get_raw(index) {
        Ok(value) => value,
        Err(err) => return undecodable("?", err),
    };
    if value.is_null() {
        return OwnedCell::Null;
    }
    let type_name = value.type_info().name().to_string();
    match row.try_get_unchecked::<&[u8], _>(index) {
        Ok(bytes) => bytes_cell(bytes),
        Err(err) => undecodable(&type_name, err),
    }
}

/// Text value of a metadata column that is never NULL.
fn text(row: &MySqlRow, index: usize) -> String {
    mysql_cell(row, index).into_text().unwrap_or_default()
}

#[async_trait]
impl DbClient for MySqlClient {
    async fn stream(
        &self,
        query: &str,
        row_limit: usize,
        tx: mpsc::Sender<Chunk>,
    ) -> Result<StreamSummary, DbError> {
        let mut conn = self.session.lock().await;
        if self.run_guard.start() && replace_session(&self.pool, &mut conn).await {
            log::warn!("replaced the session connection after an interrupted query");
            self.session_id.store(0, Ordering::Relaxed);
        }
        if self.session_id.load(Ordering::Relaxed) == 0 {
            match sqlx::query_scalar::<_, u64>("SELECT CONNECTION_ID()")
                .fetch_one(&mut **conn)
                .await
            {
                Ok(id) => self.session_id.store(id, Ordering::Relaxed),
                Err(err) => log::warn!("could not read the session's connection id: {err}"),
            }
        }

        let result = stream_raw(&mut **conn, query, row_limit, mysql_cell, &tx).await;
        self.run_guard.finish();
        if replace_if_lost(&self.pool, &mut conn, &result).await {
            self.session_id.store(0, Ordering::Relaxed);
        }
        result
    }

    async fn cancel(&self) -> Result<bool, DbError> {
        let id = self.session_id.load(Ordering::Relaxed);
        if id == 0 {
            return Ok(false);
        }
        // KILL can't be a prepared statement. The id is a number the server
        // gave us, so formatting it into the SQL is safe.
        self.pool
            .execute(format!("KILL QUERY {id}").as_str())
            .await?;
        Ok(true)
    }

    async fn list_databases(&self) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query("SHOW DATABASES").fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|row| text(row, 0)).collect())
    }

    async fn list_tables(&self) -> Result<Vec<String>, DbError> {
        let rows = sqlx::query("SHOW TABLES").fetch_all(&self.pool).await?;
        Ok(rows.iter().map(|row| text(row, 0)).collect())
    }

    async fn describe_table(&self, table_name: &str) -> Result<TableSchema, DbError> {
        let query = r#"
            SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT
            FROM information_schema.COLUMNS
            WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ?
            ORDER BY ORDINAL_POSITION
        "#;
        let rows = sqlx::query(query)
            .bind(table_name)
            .fetch_all(&self.pool)
            .await?;

        let columns = rows
            .iter()
            .map(|row| ColumnSchema {
                name: text(row, 0),
                data_type: text(row, 1),
                is_nullable: text(row, 2) == "YES",
                default: mysql_cell(row, 3).into_text(),
            })
            .collect();

        Ok(TableSchema {
            table_name: table_name.to_string(),
            columns,
            indexes: Vec::new(),
        })
    }
}
