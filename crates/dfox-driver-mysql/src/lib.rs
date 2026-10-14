//! MySQL for dfox.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use sqlx::{
    mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlRow, MySqlSslMode},
    pool::PoolConnection,
    Executor, MySql, MySqlPool, Row, TypeInfo, ValueRef,
};
use tokio::sync::{mpsc, Mutex};

use dfox_core::{
    errors::DbError,
    models::{
        connections::{ConnectionParams, DbType, TlsMode},
        schema::{ColumnSchema, DbObject, ObjectKind, TableColumn, TableSchema},
    },
    result::OwnedCell,
};

use dfox_core::db::sqlx_support::{
    bytes_cell, replace_if_lost, replace_session, stream_raw, typed_cell, undecodable, RunGuard,
};
use dfox_core::db::{Chunk, DbClient, StreamSummary};
use dfox_core::driver::{server_form, Driver, DriverInfo, FormSpec};

/// MySQL and MariaDB, through sqlx.
#[derive(Debug, Clone, Copy, Default)]
pub struct MySqlDriver;

impl Driver for MySqlDriver {
    fn info(&self) -> DriverInfo {
        DriverInfo {
            id: "mysql",
            name: "MySQL",
            default_port: Some(3306),
            url_schemes: &["mysql", "mariadb"],
        }
    }

    fn connection_form(&self) -> FormSpec {
        server_form(3306)
    }

    fn db_type(&self) -> DbType {
        DbType::MySql
    }

    fn connect<'a>(
        &'a self,
        params: &'a ConnectionParams,
    ) -> BoxFuture<'a, Result<Box<dyn DbClient>, DbError>> {
        Box::pin(async move {
            let client = MySqlClient::connect_params(params).await?;
            Ok(Box::new(client) as Box<dyn DbClient>)
        })
    }
}

pub struct MySqlClient {
    /// Used for metadata, and to replace a dead session connection.
    pub pool: MySqlPool,
    /// Pinned connection for user SQL, so session state survives between runs.
    session: Mutex<PoolConnection<MySql>>,
    /// Connection id of the session connection, for cancelling; 0 until the
    /// first query on a connection looks it up.
    connection_id: AtomicU64,
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
            .password(&params.password)
            .ssl_mode(match params.tls {
                TlsMode::Prefer => MySqlSslMode::Preferred,
                TlsMode::Require => MySqlSslMode::Required,
                TlsMode::VerifyFull => MySqlSslMode::VerifyIdentity,
                TlsMode::Disable => MySqlSslMode::Disabled,
            });
        if let Some(ca_file) = &params.ca_file {
            options = options.ssl_ca(ca_file);
        }
        if let Some(database) = &params.database {
            options = options.database(database);
        }
        Self::open(options, params.read_only).await
    }

    pub async fn connect_with(options: MySqlConnectOptions) -> Result<Self, DbError> {
        Self::open(options, false).await
    }

    async fn open(options: MySqlConnectOptions, read_only: bool) -> Result<Self, DbError> {
        let mut pool_options = MySqlPoolOptions::new().max_connections(5);
        if read_only {
            // Every pooled connection, including ones made later.
            pool_options = pool_options.after_connect(|conn, _| {
                Box::pin(async move {
                    conn.execute("SET SESSION TRANSACTION READ ONLY").await?;
                    Ok(())
                })
            });
        }
        let pool = pool_options
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
            connection_id: AtomicU64::new(0),
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
/// integers, DECIMAL, BIT and columns with a binary collation. Numbers,
/// booleans and JSON are then read as such.
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
        Ok(bytes) => std::str::from_utf8(bytes)
            .ok()
            .and_then(|text| typed_cell(&type_name, text))
            .unwrap_or_else(|| bytes_cell(bytes)),
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
            self.connection_id.store(0, Ordering::Relaxed);
        }
        if self.connection_id.load(Ordering::Relaxed) == 0 {
            match sqlx::query_scalar::<_, u64>("SELECT CONNECTION_ID()")
                .fetch_one(&mut **conn)
                .await
            {
                Ok(id) => self.connection_id.store(id, Ordering::Relaxed),
                Err(err) => log::warn!("could not read the session's connection id: {err}"),
            }
        }

        let result = stream_raw(
            &mut **conn,
            query,
            row_limit,
            mysql_cell,
            sqlx::mysql::MySqlQueryResult::rows_affected,
            &tx,
        )
        .await;
        self.run_guard.finish();
        if replace_if_lost(&self.pool, &mut conn, &result).await {
            self.connection_id.store(0, Ordering::Relaxed);
        }
        result
    }

    async fn cancel(&self) -> Result<bool, DbError> {
        let id = self.connection_id.load(Ordering::Relaxed);
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

    /// In MySQL a database is a schema, so the current one is the only one.
    async fn list_schemas(&self) -> Result<Vec<String>, DbError> {
        let row = sqlx::query("SELECT DATABASE()")
            .fetch_one(&self.pool)
            .await?;
        Ok(mysql_cell(&row, 0).into_text().into_iter().collect())
    }

    async fn list_objects(&self, schema: &str) -> Result<Vec<DbObject>, DbError> {
        let query = r#"
            SELECT TABLE_NAME, TABLE_TYPE, TABLE_ROWS
            FROM information_schema.TABLES
            WHERE TABLE_SCHEMA = ?
            UNION ALL
            SELECT ROUTINE_NAME, ROUTINE_TYPE, NULL
            FROM information_schema.ROUTINES
            WHERE ROUTINE_SCHEMA = ?
            ORDER BY 1
        "#;
        let rows = sqlx::query(query)
            .bind(schema)
            .bind(schema)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let kind = match text(row, 1).as_str() {
                    "BASE TABLE" => ObjectKind::Table,
                    "VIEW" | "SYSTEM VIEW" => ObjectKind::View,
                    "FUNCTION" => ObjectKind::Function,
                    "PROCEDURE" => ObjectKind::Procedure,
                    _ => return None,
                };
                Some(DbObject {
                    name: text(row, 0),
                    kind,
                    rows: mysql_cell(row, 2)
                        .into_text()
                        .and_then(|rows| rows.parse().ok()),
                })
            })
            .collect())
    }

    async fn list_columns(&self, schema: &str) -> Result<Vec<TableColumn>, DbError> {
        let query = r#"
            SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE
            FROM information_schema.COLUMNS
            WHERE TABLE_SCHEMA = ?
            ORDER BY TABLE_NAME, ORDINAL_POSITION
        "#;
        let rows = sqlx::query(query)
            .bind(schema)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|row| TableColumn {
                table: text(row, 0),
                name: text(row, 1),
                data_type: text(row, 2),
            })
            .collect())
    }

    async fn describe_table(&self, schema: &str, table: &str) -> Result<TableSchema, DbError> {
        let query = r#"
            SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT
            FROM information_schema.COLUMNS
            WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?
            ORDER BY ORDINAL_POSITION
        "#;
        let rows = sqlx::query(query)
            .bind(schema)
            .bind(table)
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
            schema: schema.to_string(),
            table_name: table.to_string(),
            columns,
            indexes: Vec::new(),
        })
    }
}
