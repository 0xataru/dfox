use async_trait::async_trait;
use sqlx::{
    mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlRow},
    pool::PoolConnection,
    MySql, MySqlPool, Row, TypeInfo, ValueRef,
};
use tokio::sync::Mutex;

use crate::{
    errors::DbError,
    models::{
        connections::ConnectionParams,
        schema::{ColumnSchema, TableSchema},
    },
};

use super::{bytes_to_display, run_on_session, undecodable, DbClient, QueryOutput};

pub struct MySqlClient {
    /// Used for metadata, and to replace a dead session connection.
    pub pool: MySqlPool,
    /// Pinned connection for user SQL, so session state survives between runs.
    session: Mutex<PoolConnection<MySql>>,
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
pub(crate) fn mysql_cell(row: &MySqlRow, index: usize) -> Option<String> {
    let value = match row.try_get_raw(index) {
        Ok(value) => value,
        Err(err) => return undecodable("?", err),
    };
    if value.is_null() {
        return None;
    }
    let type_name = value.type_info().name().to_string();
    match row.try_get_unchecked::<&[u8], _>(index) {
        Ok(bytes) => Some(bytes_to_display(bytes)),
        Err(err) => undecodable(&type_name, err),
    }
}

/// Text value of a metadata column that is never NULL.
fn text(row: &MySqlRow, index: usize) -> String {
    mysql_cell(row, index).unwrap_or_default()
}

#[async_trait]
impl DbClient for MySqlClient {
    async fn execute(&self, query: &str) -> Result<(), DbError> {
        run_on_session(&self.pool, &self.session, query, 0, mysql_cell).await?;
        Ok(())
    }

    async fn run(&self, query: &str, row_limit: usize) -> Result<QueryOutput, DbError> {
        run_on_session(&self.pool, &self.session, query, row_limit, mysql_cell).await
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
                default: mysql_cell(row, 3),
            })
            .collect();

        Ok(TableSchema {
            table_name: table_name.to_string(),
            columns,
            indexes: Vec::new(),
        })
    }
}
