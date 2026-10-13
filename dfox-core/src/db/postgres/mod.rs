use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use sqlx::{
    pool::PoolConnection,
    postgres::{PgConnectOptions, PgPoolOptions, PgRow, PgSslMode, PgValueFormat},
    PgPool, Postgres, Row, TypeInfo, ValueRef,
};
use tokio::sync::{mpsc, Mutex};

use crate::{
    errors::DbError,
    models::{
        connections::{ConnectionParams, TlsMode},
        schema::{ColumnSchema, DbObject, ObjectKind, TableColumn, TableSchema},
    },
    result::OwnedCell,
};

use super::{
    bytes_cell, replace_if_lost, replace_session, stream_raw, typed_cell, undecodable, Chunk,
    DbClient, RunGuard, StreamSummary,
};

pub struct PostgresClient {
    /// Used for metadata, and to replace a dead session connection.
    pub pool: PgPool,
    /// Pinned connection for user SQL, so session state survives between runs.
    session: Mutex<PoolConnection<Postgres>>,
    /// Server process id of the session connection, for cancelling; 0 until
    /// the first query on a connection looks it up.
    session_pid: AtomicU32,
    run_guard: RunGuard,
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
            .password(&params.password)
            .ssl_mode(match params.tls {
                TlsMode::Prefer => PgSslMode::Prefer,
                TlsMode::Require => PgSslMode::Require,
                TlsMode::Disable => PgSslMode::Disable,
            });
        if let Some(database) = &params.database {
            options = options.database(database);
        }
        if params.read_only {
            options = options.options([("default_transaction_read_only", "on")]);
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
            session_pid: AtomicU32::new(0),
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

/// With the simple-query protocol Postgres sends every value as text, in the
/// same form psql prints it. Numbers, booleans, JSON and bytea are read as
/// such when the text parses; everything else stays text, so no type is ever
/// misread.
pub(crate) fn pg_cell(row: &PgRow, index: usize) -> OwnedCell {
    let value = match row.try_get_raw(index) {
        Ok(value) => value,
        Err(err) => return undecodable("?", err),
    };
    if value.is_null() {
        return OwnedCell::Null;
    }
    let decoded = match value.format() {
        PgValueFormat::Text => value.as_str().map(|s| {
            typed_cell(value.type_info().name(), s).unwrap_or_else(|| OwnedCell::Text(s.into()))
        }),
        PgValueFormat::Binary => value.as_bytes().map(bytes_cell),
    };
    decoded.unwrap_or_else(|err| undecodable(value.type_info().name(), err))
}

/// A row of the objects query, or `None` for a kind dfox doesn't show.
fn pg_object(row: &PgRow) -> Result<Option<DbObject>, sqlx::Error> {
    let kind = match row.try_get::<String, _>(1)?.as_str() {
        "r" | "p" => ObjectKind::Table,
        "v" => ObjectKind::View,
        "m" => ObjectKind::MaterializedView,
        "f" => ObjectKind::ForeignTable,
        "F" => ObjectKind::Function,
        "P" => ObjectKind::Procedure,
        _ => return Ok(None),
    };
    Ok(Some(DbObject {
        name: row.try_get(0)?,
        kind,
        rows: row.try_get(2)?,
    }))
}

#[async_trait]
impl DbClient for PostgresClient {
    async fn stream(
        &self,
        query: &str,
        row_limit: usize,
        tx: mpsc::Sender<Chunk>,
    ) -> Result<StreamSummary, DbError> {
        let mut conn = self.session.lock().await;
        if self.run_guard.start() && replace_session(&self.pool, &mut conn).await {
            log::warn!("replaced the session connection after an interrupted query");
            self.session_pid.store(0, Ordering::Relaxed);
        }
        if self.session_pid.load(Ordering::Relaxed) == 0 {
            match sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
                .fetch_one(&mut **conn)
                .await
            {
                Ok(pid) => self.session_pid.store(pid as u32, Ordering::Relaxed),
                Err(err) => log::warn!("could not read the session's backend pid: {err}"),
            }
        }

        let result = stream_raw(&mut **conn, query, row_limit, pg_cell, &tx).await;
        self.run_guard.finish();
        if replace_if_lost(&self.pool, &mut conn, &result).await {
            self.session_pid.store(0, Ordering::Relaxed);
        }
        result
    }

    async fn cancel(&self) -> Result<bool, DbError> {
        let pid = self.session_pid.load(Ordering::Relaxed);
        if pid == 0 {
            return Ok(false);
        }
        sqlx::query("SELECT pg_cancel_backend($1)")
            .bind(pid as i32)
            .execute(&self.pool)
            .await?;
        Ok(true)
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

    async fn list_schemas(&self) -> Result<Vec<String>, DbError> {
        let query = r#"
            SELECT nspname::text
            FROM pg_namespace
            WHERE nspname NOT IN ('information_schema', 'pg_catalog', 'pg_toast')
              AND nspname NOT LIKE 'pg_temp_%'
              AND nspname NOT LIKE 'pg_toast_temp_%'
            ORDER BY nspname <> 'public', nspname
        "#;
        Ok(sqlx::query_scalar(query).fetch_all(&self.pool).await?)
    }

    async fn list_objects(&self, schema: &str) -> Result<Vec<DbObject>, DbError> {
        // reltuples is -1 for a table that was never analysed.
        let query = r#"
            SELECT c.relname::text,
                   c.relkind::text,
                   CASE WHEN c.reltuples >= 0 THEN c.reltuples::int8 END
            FROM pg_class c
            JOIN pg_namespace n ON n.oid = c.relnamespace
            WHERE n.nspname = $1 AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
            UNION ALL
            SELECT DISTINCT p.proname::text,
                   CASE p.prokind WHEN 'p' THEN 'P' ELSE 'F' END,
                   NULL::int8
            FROM pg_proc p
            JOIN pg_namespace n ON n.oid = p.pronamespace
            WHERE n.nspname = $1 AND p.prokind IN ('f', 'p')
            ORDER BY 1
        "#;
        let rows = sqlx::query(query)
            .bind(schema)
            .fetch_all(&self.pool)
            .await?;
        let mut objects = Vec::with_capacity(rows.len());
        for row in &rows {
            if let Some(object) = pg_object(row)? {
                objects.push(object);
            }
        }
        Ok(objects)
    }

    async fn list_columns(&self, schema: &str) -> Result<Vec<TableColumn>, DbError> {
        let query = r#"
            SELECT c.relname::text, a.attname::text, format_type(a.atttypid, a.atttypmod)
            FROM pg_attribute a
            JOIN pg_class c ON c.oid = a.attrelid
            JOIN pg_namespace n ON n.oid = c.relnamespace
            WHERE n.nspname = $1
              AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
              AND a.attnum > 0
              AND NOT a.attisdropped
            ORDER BY c.relname, a.attnum
        "#;
        let rows = sqlx::query(query)
            .bind(schema)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                Ok(TableColumn {
                    table: row.try_get(0)?,
                    name: row.try_get(1)?,
                    data_type: row.try_get(2)?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(Into::into)
    }

    async fn describe_table(&self, schema: &str, table: &str) -> Result<TableSchema, DbError> {
        let query = r#"
            SELECT a.attname::text,
                   format_type(a.atttypid, a.atttypmod),
                   NOT a.attnotnull,
                   pg_get_expr(d.adbin, d.adrelid)
            FROM pg_attribute a
            JOIN pg_class c ON c.oid = a.attrelid
            JOIN pg_namespace n ON n.oid = c.relnamespace
            LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
            WHERE n.nspname = $1
              AND c.relname = $2
              AND a.attnum > 0
              AND NOT a.attisdropped
            ORDER BY a.attnum
        "#;
        let rows = sqlx::query(query)
            .bind(schema)
            .bind(table)
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
            schema: schema.to_string(),
            table_name: table.to_string(),
            columns,
            indexes: Vec::new(),
        })
    }
}
