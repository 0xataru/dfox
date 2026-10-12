use std::time::Duration;

use crate::{
    errors::DbError,
    models::schema::TableSchema,
    result::{ColumnMeta, OwnedCell},
};
use async_trait::async_trait;
use futures_util::TryStreamExt;
use sqlx::{pool::PoolConnection, Column, Database, Either, Executor, Pool, Row, TypeInfo};
use tokio::{sync::mpsc, time::Instant};

pub mod mysql;
pub mod postgres;
pub mod sqlite;

pub use mysql::MySqlClient;
pub use postgres::PostgresClient;

/// How many rows [`DbClient::run`] keeps by default. Rows past the limit are
/// read from the server and dropped, so every statement still runs to the end.
pub const DEFAULT_ROW_LIMIT: usize = 1000;

/// A streamed chunk holds at most this many rows...
const CHUNK_ROWS: usize = 1000;
/// ...and is sent at most this long after its first row arrived, so the first
/// rows show up quickly even when the rest are slow.
const CHUNK_INTERVAL: Duration = Duration::from_millis(16);

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

/// Part of a streamed query's output, in the order the server sent it.
#[derive(Debug, Clone, PartialEq)]
pub enum Chunk {
    /// A statement returned rows. Its rows follow, and replace those of any
    /// earlier statement in the script.
    Columns(Vec<ColumnMeta>),
    Rows(Vec<Vec<OwnedCell>>),
}

/// What [`DbClient::stream`] reports once every statement has finished.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamSummary {
    /// Rows changed by every statement in the script, summed.
    pub rows_affected: u64,
    /// True when rows of the last result set were dropped at the row limit.
    pub truncated: bool,
}

#[async_trait]
pub trait DbClient: Send + Sync {
    /// Runs one or more `;`-separated statements on this client's session
    /// connection, so `BEGIN`, `SET` and temp tables carry over between calls,
    /// and sends their rows to `tx` as they arrive. Statements of any kind can
    /// return rows (`SELECT`, `WITH`, `SHOW`, `EXPLAIN`, `... RETURNING`,
    /// `PRAGMA`, ...).
    ///
    /// Each result set keeps at most `row_limit` rows; the rest are read and
    /// dropped. Stops early with [`DbError::Cancelled`] if `tx` is closed.
    async fn stream(
        &self,
        query: &str,
        row_limit: usize,
        tx: mpsc::Sender<Chunk>,
    ) -> Result<StreamSummary, DbError>;

    /// Asks the server to stop the statement running on the session
    /// connection. Returns `false` when this database has no way to do that,
    /// and the caller should drop the running [`DbClient::stream`] instead.
    async fn cancel(&self) -> Result<bool, DbError>;

    /// Like [`DbClient::stream`], but collects the output.
    async fn run(&self, query: &str, row_limit: usize) -> Result<QueryOutput, DbError> {
        collect_output(|tx| self.stream(query, row_limit, tx)).await
    }

    /// Like [`DbClient::run`], for statements whose rows aren't needed.
    async fn execute(&self, query: &str) -> Result<(), DbError> {
        self.run(query, 0).await.map(drop)
    }

    async fn list_databases(&self) -> Result<Vec<String>, DbError>;
    async fn list_tables(&self) -> Result<Vec<String>, DbError>;
    async fn describe_table(&self, table_name: &str) -> Result<TableSchema, DbError>;
}

async fn collect_output<F>(
    produce: impl FnOnce(mpsc::Sender<Chunk>) -> F,
) -> Result<QueryOutput, DbError>
where
    F: std::future::Future<Output = Result<StreamSummary, DbError>>,
{
    let (tx, mut rx) = mpsc::channel(4);
    let collect = async {
        let mut out = QueryOutput::default();
        while let Some(chunk) = rx.recv().await {
            match chunk {
                Chunk::Columns(columns) => {
                    out.columns = columns.into_iter().map(|c| c.name).collect();
                    out.rows.clear();
                }
                Chunk::Rows(rows) => out.rows.extend(
                    rows.into_iter()
                        .map(|row| row.into_iter().map(OwnedCell::into_text).collect()),
                ),
            }
        }
        out
    };
    let (summary, mut out) = futures_util::future::join(produce(tx), collect).await;
    let summary = summary?;
    out.rows_affected = summary.rows_affected;
    out.truncated = summary.truncated;
    Ok(out)
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

/// Runs raw SQL and sends its rows to `tx` in chunks.
///
/// The SQL is passed as a plain `&str`, so sqlx uses the simple-query
/// protocol: several statements are allowed, and Postgres and MySQL send every
/// value in its text form, which `cell` reads without any type mapping that
/// could fail.
pub(crate) async fn stream_raw<'c, DB, E>(
    executor: E,
    sql: &str,
    row_limit: usize,
    cell: fn(&DB::Row, usize) -> OwnedCell,
    tx: &mpsc::Sender<Chunk>,
) -> Result<StreamSummary, DbError>
where
    DB: Database,
    DB::QueryResult: RowsAffected,
    E: Executor<'c, Database = DB>,
{
    let mut summary = StreamSummary::default();
    // A statement result ends one result set; the next row starts a new one.
    let mut new_result_set = true;
    let mut kept = 0;
    let mut batch = Vec::new();
    let mut deadline = Instant::now();
    let mut stream = executor.fetch_many(sql);

    loop {
        let next = if batch.is_empty() {
            stream.try_next().await?
        } else {
            match tokio::time::timeout_at(deadline, stream.try_next()).await {
                Ok(next) => next?,
                Err(_) => {
                    send(tx, Chunk::Rows(std::mem::take(&mut batch))).await?;
                    continue;
                }
            }
        };
        let Some(item) = next else { break };

        match item {
            Either::Left(result) => {
                summary.rows_affected += result.rows_affected();
                new_result_set = true;
            }
            Either::Right(row) => {
                if new_result_set {
                    new_result_set = false;
                    if !batch.is_empty() {
                        send(tx, Chunk::Rows(std::mem::take(&mut batch))).await?;
                    }
                    kept = 0;
                    summary.truncated = false;
                    let columns = row
                        .columns()
                        .iter()
                        .map(|c| ColumnMeta::new(c.name(), c.type_info().name()))
                        .collect();
                    send(tx, Chunk::Columns(columns)).await?;
                }
                if kept == row_limit {
                    summary.truncated = true;
                    continue;
                }
                kept += 1;
                if batch.is_empty() {
                    deadline = Instant::now() + CHUNK_INTERVAL;
                }
                batch.push((0..row.len()).map(|i| cell(&row, i)).collect());
                if batch.len() == CHUNK_ROWS {
                    send(tx, Chunk::Rows(std::mem::take(&mut batch))).await?;
                }
            }
        }
    }

    if !batch.is_empty() {
        send(tx, Chunk::Rows(batch)).await?;
    }
    Ok(summary)
}

async fn send(tx: &mpsc::Sender<Chunk>, chunk: Chunk) -> Result<(), DbError> {
    tx.send(chunk).await.map_err(|_| DbError::Cancelled)
}

/// After a failed run on the pinned session connection: if the connection
/// itself is dead, swaps it for a fresh one from the pool so the next run
/// works, and returns `true`. The error is still returned to the user, because
/// session state was lost.
pub(crate) async fn replace_if_lost<DB: Database>(
    pool: &Pool<DB>,
    conn: &mut PoolConnection<DB>,
    result: &Result<StreamSummary, DbError>,
) -> bool {
    let Err(DbError::Sqlx(err)) = result else {
        return false;
    };
    if !is_connection_lost(err) {
        return false;
    }
    log::warn!("session connection lost, reconnecting: {err}");
    replace_session(pool, conn).await
}

/// Swaps the session connection for a fresh one from the pool and closes the
/// old one. Returns `false` if no new connection could be made.
pub(crate) async fn replace_session<DB: Database>(
    pool: &Pool<DB>,
    conn: &mut PoolConnection<DB>,
) -> bool {
    match pool.acquire().await {
        Ok(fresh) => {
            let mut old = std::mem::replace(conn, fresh);
            old.close_on_drop();
            true
        }
        Err(err) => {
            log::warn!("could not reconnect: {err}");
            false
        }
    }
}

/// Notices a run that was dropped before it finished. Its statement may still
/// be running on the server, and would hold up the next run on the same
/// connection, so that connection should be replaced.
#[derive(Debug, Default)]
pub(crate) struct RunGuard(std::sync::atomic::AtomicBool);

impl RunGuard {
    /// Marks a run as started. Returns `true` if the previous run never
    /// finished.
    pub(crate) fn start(&self) -> bool {
        self.0.swap(true, std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn finish(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
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

/// Keeps bytes as text when they are printable UTF-8, otherwise as bytes,
/// which are shown as `0x` hex.
pub(crate) fn bytes_cell(bytes: &[u8]) -> OwnedCell {
    match std::str::from_utf8(bytes) {
        Ok(s)
            if !s
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r')) =>
        {
            OwnedCell::Text(s.into())
        }
        _ => OwnedCell::Bytes(bytes.into()),
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
pub(crate) fn undecodable(type_name: &str, err: impl std::fmt::Display) -> OwnedCell {
    log::warn!("could not decode {type_name} value: {err}");
    OwnedCell::Undecodable(format!("undecodable {type_name}").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_utf8_stays_text() {
        assert_eq!(
            bytes_cell(b"hello\tworld\n"),
            OwnedCell::Text("hello\tworld\n".into())
        );
        assert_eq!(
            bytes_cell("привет 🦊".as_bytes()),
            OwnedCell::Text("привет 🦊".into())
        );
    }

    #[test]
    fn binary_becomes_hex() {
        let shown = |bytes: &[u8]| bytes_cell(bytes).as_cell().to_string();
        assert_eq!(shown(&[0xde, 0xad, 0xbe, 0xef]), "0xdeadbeef");
        assert_eq!(shown(&[0x00, 0x41]), "0x0041");
        assert_eq!(hex(&[]), "0x");
    }
}
