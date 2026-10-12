use async_trait::async_trait;
use sqlx::{
    pool::PoolConnection,
    postgres::{PgConnectOptions, PgPoolOptions, PgRow, PgValueFormat},
    PgPool, Postgres, Row, TypeInfo, ValueRef,
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

pub struct PostgresClient {
    /// Used for metadata, and to replace a dead session connection.
    pub pool: PgPool,
    /// Pinned connection for user SQL, so session state survives between runs.
    session: Mutex<PoolConnection<Postgres>>,
}

impl PostgresClient {
    pub async fn connect(database_url: &str) -> Result<Self, DbError> {
        let options = database_url
            .parse::<PgConnectOptions>()
            .map_err(|e| DbError::Config(e.to_string()))?;
        Self::connect_with(options).await
    }

    pub async fn connect_params(params: &ConnectionParams) -> Result<Self, DbError> {
        let mut options = PgConnectOptions::new()
            .host(&params.host)
            .port(params.port)
            .username(&params.username)
            .password(&params.password);
        if let Some(database) = &params.database {
            options = options.database(database);
        }
        Self::connect_with(options).await
    }

    pub async fn connect_with(options: PgConnectOptions) -> Result<Self, DbError> {
        let pool = PgPoolOptions::new()
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

/// With the simple-query protocol Postgres sends every value as text, in the
/// same form psql prints it, so no type is ever misread.
pub(crate) fn pg_cell(row: &PgRow, index: usize) -> Option<String> {
    let value = match row.try_get_raw(index) {
        Ok(value) => value,
        Err(err) => return undecodable("?", err),
    };
    if value.is_null() {
        return None;
    }
    let decoded = match value.format() {
        PgValueFormat::Text => value.as_str().map(str::to_owned),
        PgValueFormat::Binary => value.as_bytes().map(bytes_to_display),
    };
    match decoded {
        Ok(text) => Some(text),
        Err(err) => undecodable(value.type_info().name(), err),
    }
}

#[async_trait]
impl DbClient for PostgresClient {
    async fn execute(&self, query: &str) -> Result<(), DbError> {
        run_on_session(&self.pool, &self.session, query, 0, pg_cell).await?;
        Ok(())
    }

    async fn run(&self, query: &str, row_limit: usize) -> Result<QueryOutput, DbError> {
        run_on_session(&self.pool, &self.session, query, row_limit, pg_cell).await
    }

    async fn list_databases(&self) -> Result<Vec<String>, DbError> {
        let query = r#"
            SELECT datname::text
            FROM pg_database
            WHERE datistemplate = false
            ORDER BY datname
        "#;
        let databases = sqlx::query_scalar(query).fetch_all(&self.pool).await?;
        Ok(databases)
    }

    async fn list_tables(&self) -> Result<Vec<String>, DbError> {
        let query = r#"
            SELECT table_name::text
            FROM information_schema.tables
            WHERE table_schema = 'public'
            ORDER BY table_name
        "#;
        let tables = sqlx::query_scalar(query).fetch_all(&self.pool).await?;
        Ok(tables)
    }

    async fn describe_table(&self, table_name: &str) -> Result<TableSchema, DbError> {
        let query = r#"
            SELECT a.attname::text,
                   format_type(a.atttypid, a.atttypmod),
                   NOT a.attnotnull,
                   pg_get_expr(d.adbin, d.adrelid)
            FROM pg_attribute a
            JOIN pg_class c ON c.oid = a.attrelid
            JOIN pg_namespace n ON n.oid = c.relnamespace
            LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
            WHERE n.nspname = 'public'
              AND c.relname = $1
              AND a.attnum > 0
              AND NOT a.attisdropped
            ORDER BY a.attnum
        "#;
        let rows = sqlx::query(query)
            .bind(table_name)
            .fetch_all(&self.pool)
            .await?;

        let columns = rows
            .iter()
            .map(|row| {
                Ok(ColumnSchema {
                    name: row.try_get(0)?,
                    data_type: row.try_get(1)?,
                    is_nullable: row.try_get(2)?,
                    default: row.try_get(3)?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()?;

        Ok(TableSchema {
            table_name: table_name.to_string(),
            columns,
            indexes: Vec::new(),
        })
    }
}
