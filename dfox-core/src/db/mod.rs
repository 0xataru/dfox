use crate::{errors::DbError, models::schema::TableSchema};
use async_trait::async_trait;
use futures_util::TryStreamExt;
use sqlx::{pool::PoolConnection, Column, Database, Either, Executor, Pool, Row};
use tokio::sync::Mutex;

pub mod mysql;
pub mod postgres;
pub mod sqlite;

pub use mysql::MySqlClient;
pub use postgres::PostgresClient;

/// How many rows [`DbClient::run`] keeps by default. Rows past the limit are
/// read from the server and dropped, so every statement still runs to the end.
pub const DEFAULT_ROW_LIMIT: usize = 1000;

/// The outcome of running user SQL: the rows of the last statement that
/// returned any, plus the total number of rows changed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryOutput {
    /// Column names in the order the server sent them. Duplicates are kept.
    pub columns: Vec<String>,
    /// Each cell is the server's own text form of the value; `None` is SQL NULL.
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows changed by every statement in the script, summed.
    pub rows_affected: u64,
    /// True when rows past the limit were dropped.
    pub truncated: bool,
}

#[async_trait]
pub trait DbClient: Send + Sync {
    /// Runs one or more `;`-separated statements on this client's session
    /// connection, so `BEGIN`, `SET` and temp tables carry over between calls.
    async fn execute(&self, query: &str) -> Result<(), DbError>;
    /// Like [`DbClient::execute`], but returns the rows of any statement kind
    /// (`SELECT`, `WITH`, `SHOW`, `EXPLAIN`, `... RETURNING`, `PRAGMA`, ...).
    async fn run(&self, query: &str, row_limit: usize) -> Result<QueryOutput, DbError>;
    async fn list_databases(&self) -> Result<Vec<String>, DbError>;
    async fn list_tables(&self) -> Result<Vec<String>, DbError>;
    async fn describe_table(&self, table_name: &str) -> Result<TableSchema, DbError>;
}

/// `rows_affected` is an inherent method on each driver's result type.
pub(crate) trait RowsAffected {
    fn rows_affected(&self) -> u64;
}

impl RowsAffected for sqlx::postgres::PgQueryResult {
    fn rows_affected(&self) -> u64 {
        self.rows_affected()
    }
}

impl RowsAffected for sqlx::mysql::MySqlQueryResult {
    fn rows_affected(&self) -> u64 {
        self.rows_affected()
    }
}

impl RowsAffected for sqlx::sqlite::SqliteQueryResult {
    fn rows_affected(&self) -> u64 {
        self.rows_affected()
    }
}

/// Runs raw SQL and collects the output.
///
/// The SQL is passed as a plain `&str`, so sqlx uses the simple-query
/// protocol: several statements are allowed, and Postgres and MySQL send every
/// value in its text form, which `cell` turns into a display string without
/// any type mapping that could fail.
pub(crate) async fn run_raw<'c, DB, E>(
    executor: E,
    sql: &str,
    row_limit: usize,
    cell: fn(&DB::Row, usize) -> Option<String>,
) -> Result<QueryOutput, DbError>
where
    DB: Database,
    DB::QueryResult: RowsAffected,
    E: Executor<'c, Database = DB>,
{
    let mut out = QueryOutput::default();
    // A statement result ends one result set; the next row starts a new one.
    let mut new_result_set = true;
    let mut stream = executor.fetch_many(sql);

    while let Some(item) = stream.try_next().await? {
        match item {
            Either::Left(result) => {
                out.rows_affected += result.rows_affected();
                new_result_set = true;
            }
            Either::Right(row) => {
                if new_result_set {
                    new_result_set = false;
                    out.truncated = false;
                    out.rows.clear();
                    out.columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                }
                if out.rows.len() < row_limit {
                    out.rows
                        .push((0..row.len()).map(|i| cell(&row, i)).collect());
                } else {
                    out.truncated = true;
                }
            }
        }
    }

    Ok(out)
}

/// Runs raw SQL on the pinned session connection. If the connection turns out
/// to be dead, it is swapped for a fresh one from the pool so the next run
/// works; the error is still returned because session state was lost.
pub(crate) async fn run_on_session<DB>(
    pool: &Pool<DB>,
    session: &Mutex<PoolConnection<DB>>,
    sql: &str,
    row_limit: usize,
    cell: fn(&DB::Row, usize) -> Option<String>,
) -> Result<QueryOutput, DbError>
where
    DB: Database,
    DB::QueryResult: RowsAffected,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
{
    let mut conn = session.lock().await;
    let result = run_raw(&mut **conn, sql, row_limit, cell).await;

    if let Err(DbError::Sqlx(err)) = &result {
        if is_connection_lost(err) {
            log::warn!("session connection lost, reconnecting: {err}");
            if let Ok(fresh) = pool.acquire().await {
                let mut dead = std::mem::replace(&mut *conn, fresh);
                dead.close_on_drop();
            }
        }
    }

    result
}

fn is_connection_lost(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::Protocol(_)
            | sqlx::Error::WorkerCrashed
    )
}

/// Shows bytes as text when they are printable UTF-8, otherwise as `0x` hex.
pub(crate) fn bytes_to_display(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s)
            if !s
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r')) =>
        {
            s.to_string()
        }
        _ => hex(bytes),
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Placeholder for a value that could not be read, so it is never shown as NULL.
pub(crate) fn undecodable(type_name: &str, err: impl std::fmt::Display) -> Option<String> {
    log::warn!("could not decode {type_name} value: {err}");
    Some(format!("⟨undecodable {type_name}⟩"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_utf8_stays_text() {
        assert_eq!(bytes_to_display(b"hello\tworld\n"), "hello\tworld\n");
        assert_eq!(bytes_to_display("привет 🦊".as_bytes()), "привет 🦊");
    }

    #[test]
    fn binary_becomes_hex() {
        assert_eq!(bytes_to_display(&[0xde, 0xad, 0xbe, 0xef]), "0xdeadbeef");
        assert_eq!(bytes_to_display(&[0x00, 0x41]), "0x0041");
        assert_eq!(hex(&[]), "0x");
    }
}
