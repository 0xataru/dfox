//! A session: one pinned connection that runs what the user types, browses
//! the catalog and applies edits from the grid.

use std::{fmt, sync::Arc};

use bitflags::bitflags;
use futures_util::future::BoxFuture;

use crate::{
    arrow::ResultStream, catalog::Catalog, catalog::NodePath, errors::DbError, result::OwnedCell,
};

bitflags! {
    /// What a session can do. The app shows or hides features by these,
    /// never by which database it is.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Capabilities: u32 {
        /// `BEGIN`/`COMMIT` carry over between runs on the session.
        const TRANSACTIONS = 1;
        /// Databases hold schemas.
        const SCHEMAS = 1 << 1;
        /// Queries can be explained.
        const EXPLAIN = 1 << 2;
        /// A running query can be stopped through a [`CancelHandle`].
        const CANCEL = 1 << 3;
        /// Results stream from a server-side cursor instead of arriving at once.
        const SERVER_CURSOR = 1 << 4;
        /// Rows can be edited by primary key with a [`ChangeSet`].
        const EDIT_BY_PK = 1 << 5;
        /// Rows can be loaded in bulk.
        const BULK_LOAD = 1 << 6;
        /// The catalog knows roughly how many rows a table has.
        const ROW_COUNT_ESTIMATE = 1 << 7;
        /// One run may hold several statements.
        const MULTI_STATEMENT = 1 << 8;
    }
}

/// The SQL dialect of a SQL database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SqlDialect {
    /// PostgreSQL and compatible servers.
    Postgres,
    /// MySQL and MariaDB.
    MySql,
    /// SQLite.
    Sqlite,
    /// Any other SQL.
    Generic,
}

/// What the editor holds for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryLanguage {
    /// SQL in a dialect.
    Sql(SqlDialect),
    /// Cassandra Query Language.
    Cql,
    /// MongoDB shell commands.
    MongoShell,
    /// Redis commands.
    RedisCmd,
}

/// Something to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// What the user wrote: one or more statements.
    pub text: String,
    /// Keep at most this many rows per result set; the rest are read and
    /// dropped, so every statement still runs to the end.
    pub row_limit: Option<usize>,
}

impl Request {
    /// A request for `text` with no row limit.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            row_limit: None,
        }
    }

    /// The request with a row limit.
    pub fn with_row_limit(mut self, limit: usize) -> Self {
        self.row_limit = Some(limit);
        self
    }
}

/// Asks the server to stop what a session is running. Cheap to clone, and
/// usable from another task while the session is busy.
#[derive(Clone)]
pub struct CancelHandle(Arc<dyn Fn() -> BoxFuture<'static, Result<bool, DbError>> + Send + Sync>);

impl fmt::Debug for CancelHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CancelHandle")
    }
}

impl CancelHandle {
    /// A handle that runs `cancel` when used.
    pub fn new(
        cancel: impl Fn() -> BoxFuture<'static, Result<bool, DbError>> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(cancel))
    }

    /// A handle for sessions that can't be stopped: it always answers false.
    pub fn unsupported() -> Self {
        Self::new(|| Box::pin(async { Ok(false) }))
    }

    /// Asks the server to stop. Returns false when the session has no way to
    /// do that, and the caller should drop the run's stream instead.
    pub async fn cancel(&self) -> Result<bool, DbError> {
        (self.0)().await
    }
}

/// One edit of a table's rows.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    /// A new row: a value per column of [`ChangeSet::columns`].
    Insert(Vec<OwnedCell>),
    /// Set columns of the row with `key`.
    Update {
        /// The row's key, a value per [`ChangeSet::key`] column.
        key: Vec<OwnedCell>,
        /// The new values, by column name.
        set: Vec<(String, OwnedCell)>,
    },
    /// Delete the row with this key.
    Delete(Vec<OwnedCell>),
}

/// Edits of one table, keyed by its primary key. The driver writes them as
/// parametrised statements, in a transaction where it can.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeSet {
    /// The table.
    pub table: NodePath,
    /// The primary key columns.
    pub key: Vec<String>,
    /// The columns of inserted rows, in order.
    pub columns: Vec<String>,
    /// The edits, applied in order.
    pub changes: Vec<Change>,
}

/// What applying a [`ChangeSet`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyReport {
    /// Rows inserted.
    pub inserted: u64,
    /// Rows updated.
    pub updated: u64,
    /// Rows deleted.
    pub deleted: u64,
}

/// One pinned connection, so `BEGIN`, `SET` and temporary tables carry over
/// between runs.
pub trait Session: Send {
    /// What this session can do.
    fn caps(&self) -> Capabilities;

    /// The language the editor should speak.
    fn language(&self) -> QueryLanguage;

    /// The tree of things in the database.
    fn catalog(&mut self) -> &mut dyn Catalog;

    /// Runs `request`, streaming its results as they arrive.
    fn run(&mut self, request: Request) -> BoxFuture<'_, Result<ResultStream, DbError>>;

    /// A handle that stops the running query.
    fn cancel_handle(&self) -> CancelHandle;

    /// Applies edits from the grid. Sessions without
    /// [`Capabilities::EDIT_BY_PK`] refuse.
    fn apply(&mut self, changes: ChangeSet) -> BoxFuture<'_, Result<ApplyReport, DbError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unsupported_cancel_says_so() {
        let handle = CancelHandle::unsupported();
        assert!(!handle.clone().cancel().await.expect("cancel"));
        assert_eq!(format!("{handle:?}"), "CancelHandle");
    }

    #[test]
    fn requests_carry_a_row_limit() {
        let request = Request::new("select 1").with_row_limit(10);
        assert_eq!(request.row_limit, Some(10));
        assert_eq!(Request::new("x").row_limit, None);
    }

    #[test]
    fn capabilities_combine() {
        let caps = Capabilities::TRANSACTIONS | Capabilities::CANCEL;
        assert!(caps.contains(Capabilities::CANCEL));
        assert!(!caps.contains(Capabilities::EDIT_BY_PK));
    }
}
