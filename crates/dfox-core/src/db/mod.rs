use std::time::Duration;

use crate::{
    errors::DbError,
    models::schema::{DbObject, TableColumn, TableSchema},
    result::{ColumnMeta, OwnedCell},
};
use async_trait::async_trait;
use tokio::sync::mpsc;

pub mod sqlx_support;

/// How many rows [`DbClient::run`] keeps by default. Rows past the limit are
/// read from the server and dropped, so every statement still runs to the end.
pub const DEFAULT_ROW_LIMIT: usize = 1000;

/// A streamed chunk holds at most this many rows...
pub(crate) const CHUNK_ROWS: usize = 1000;
/// ...and is sent at most this long after its first row arrived, so the first
/// rows show up quickly even when the rest are slow.
pub(crate) const CHUNK_INTERVAL: Duration = Duration::from_millis(16);

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
    /// Tables in the default schema: `public`, or the current database.
    async fn list_tables(&self) -> Result<Vec<String>, DbError>;
    /// Schemas of the current database, without system schemas. MySQL has
    /// one, the database itself; SQLite has `main` and any attached databases.
    async fn list_schemas(&self) -> Result<Vec<String>, DbError>;
    /// Tables, views and routines in `schema`, sorted by name.
    async fn list_objects(&self, schema: &str) -> Result<Vec<DbObject>, DbError>;
    async fn describe_table(&self, schema: &str, table: &str) -> Result<TableSchema, DbError>;
    /// Every column of every table and view in `schema`, in one query, by
    /// table and then column position. For autocomplete.
    async fn list_columns(&self, schema: &str) -> Result<Vec<TableColumn>, DbError>;
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

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::hex;

    #[test]
    fn hex_has_a_prefix_even_when_empty() {
        assert_eq!(hex(&[0xde, 0xad]), "0xdead");
        assert_eq!(hex(&[]), "0x");
    }
}
