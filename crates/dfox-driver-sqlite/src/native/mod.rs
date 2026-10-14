//! The native SQLite session: `rusqlite` on a thread of its own, decoding
//! rows straight into Arrow batches.
//!
//! SQLite runs in this process and blocks while it works, so each session
//! owns an OS thread with the connection on it. The async side sends it jobs
//! and receives frames back. A script is split by SQLite itself, so trigger
//! bodies with their own semicolons stay whole. Cancelling interrupts the
//! connection (`sqlite3_interrupt`).

mod catalog;
pub mod decode;

use std::{
    sync::mpsc as std_mpsc,
    time::{Duration, Instant},
};

use arrow_array::{ArrayRef, RecordBatch};
use dfox_core::{
    arrow::{Frame, ResultStream},
    catalog::Catalog,
    errors::DbError,
    models::connections::ConnectionParams,
    session::{
        ApplyReport, CancelHandle, Capabilities, ChangeSet, QueryLanguage, Request, Session,
        SqlDialect,
    },
};
use futures_util::{future::BoxFuture, stream};
use rusqlite::{types::Value, Connection, ErrorCode, InterruptHandle, OpenFlags, Statement};
use tokio::sync::{mpsc, oneshot};

use self::{
    catalog::SqliteCatalog,
    decode::{ColumnMeta, Columns},
};

/// Rows per batch.
const BATCH_ROWS: usize = 8192;
/// A batch goes out at most this long after its first row, so the first
/// rows show quickly even when the rest are slow.
const BATCH_INTERVAL: Duration = Duration::from_millis(16);
/// How long a statement waits for another connection's lock.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

type FrameTx = mpsc::Sender<Result<Frame, DbError>>;

/// Work for the connection's thread.
enum Job {
    /// Runs a script, sending its frames.
    Run {
        sql: String,
        limit: usize,
        tx: FrameTx,
    },
    /// Runs one statement with parameters, sending its frames.
    Query {
        sql: String,
        params: Vec<Value>,
        tx: FrameTx,
    },
    /// Anything else that needs the connection.
    Call(Box<dyn FnOnce(&Connection) + Send>),
}

/// The way to the connection's thread. The thread ends when every worker is
/// dropped.
#[derive(Clone)]
pub(crate) struct Worker {
    jobs: std_mpsc::Sender<Job>,
}

impl Worker {
    fn send(&self, job: Job) -> Result<(), DbError> {
        self.jobs
            .send(job)
            .map_err(|_| DbError::Connection("the SQLite connection is closed".into()))
    }

    /// Runs `work` on the connection and returns what it gives.
    pub(crate) async fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, DbError> + Send + 'static,
    ) -> Result<T, DbError> {
        let (reply, answer) = oneshot::channel();
        self.send(Job::Call(Box::new(move |conn| {
            let _ = reply.send(work(conn));
        })))?;
        answer
            .await
            .map_err(|_| DbError::Connection("the SQLite connection is closed".into()))?
    }

    /// Runs `job`, made with the stream's sender, and streams its frames.
    fn stream(&self, job: impl FnOnce(FrameTx) -> Job) -> Result<ResultStream, DbError> {
        let (tx, rx) = mpsc::channel(4);
        self.send(job(tx))?;
        Ok(Box::pin(stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|frame| (frame, rx))
        })))
    }

    pub(crate) fn query(&self, sql: String, params: Vec<Value>) -> Result<ResultStream, DbError> {
        self.stream(|tx| Job::Query { sql, params, tx })
    }
}

/// Opens a native session on the file in `params.database`.
pub async fn open(params: &ConnectionParams) -> Result<Box<dyn Session>, DbError> {
    let path = params
        .database
        .clone()
        .ok_or_else(|| DbError::Config("SQLite needs a database file path".to_string()))?;
    // A missing file is an error, not a new empty database.
    let mode = if params.read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let flags = mode | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let (jobs, queue) = std_mpsc::channel();
    let (opened, ready) = oneshot::channel();
    std::thread::Builder::new()
        .name("dfox-sqlite".into())
        .spawn(move || {
            let conn = match Connection::open_with_flags(&path, flags) {
                Ok(conn) => conn,
                Err(err) => {
                    let _ = opened.send(Err(DbError::Connection(format!("{path}: {err}"))));
                    return;
                }
            };
            if let Err(err) = conn.busy_timeout(BUSY_TIMEOUT) {
                log::warn!("sqlite: can't set the busy timeout: {err}");
            }
            let _ = opened.send(Ok(conn.get_interrupt_handle()));
            serve(&conn, queue);
        })
        .map_err(|err| DbError::Connection(format!("can't start the SQLite thread: {err}")))?;
    let interrupt = ready
        .await
        .map_err(|_| DbError::Connection("the SQLite thread stopped".into()))??;
    let worker = Worker { jobs };
    Ok(Box::new(SqliteSession {
        catalog: SqliteCatalog::new(worker.clone()),
        worker,
        interrupt: std::sync::Arc::new(interrupt),
    }))
}

/// The connection's thread: runs jobs until every worker is gone.
fn serve(conn: &Connection, queue: std_mpsc::Receiver<Job>) {
    while let Ok(job) = queue.recv() {
        match job {
            Job::Run { sql, limit, tx } => {
                if let Err(err) = run_script(conn, &sql, limit, &tx) {
                    let _ = tx.blocking_send(Err(err));
                }
            }
            Job::Query { sql, params, tx } => {
                let result = conn.prepare(&sql).map_err(db_error).and_then(|mut stmt| {
                    for (i, value) in params.iter().enumerate() {
                        stmt.raw_bind_parameter(i + 1, value).map_err(db_error)?;
                    }
                    run_statement(conn, &mut stmt, usize::MAX, &tx)
                });
                if let Err(err) = result {
                    let _ = tx.blocking_send(Err(err));
                }
            }
            Job::Call(work) => work(conn),
        }
    }
}

/// A rusqlite error as a dfox error: SQLite's message, and interrupted
/// statements as [`DbError::Cancelled`].
pub(crate) fn db_error(err: rusqlite::Error) -> DbError {
    match &err {
        rusqlite::Error::SqliteFailure(e, _) if e.code == ErrorCode::OperationInterrupted => {
            DbError::Cancelled
        }
        rusqlite::Error::SqliteFailure(_, Some(message)) => DbError::General(message.clone()),
        _ => DbError::General(err.to_string()),
    }
}

fn send(tx: &FrameTx, frame: Frame) -> Result<(), DbError> {
    tx.blocking_send(Ok(frame)).map_err(|_| DbError::Cancelled)
}

/// Runs each statement of `sql`, as SQLite splits it.
fn run_script(conn: &Connection, sql: &str, limit: usize, tx: &FrameTx) -> Result<(), DbError> {
    let mut batch = rusqlite::Batch::new(conn, sql);
    while let Some(mut stmt) = batch.next().map_err(db_error)? {
        run_statement(conn, &mut stmt, limit, tx)?;
    }
    Ok(())
}

/// Runs one statement and streams its rows, or what it changed.
fn run_statement(
    conn: &Connection,
    stmt: &mut Statement<'_>,
    limit: usize,
    tx: &FrameTx,
) -> Result<(), DbError> {
    if stmt.column_count() == 0 {
        stmt.raw_execute().map_err(db_error)?;
        return send(tx, Frame::Affected(conn.changes()));
    }
    let meta: Vec<ColumnMeta> = stmt
        .columns()
        .iter()
        .map(|c| ColumnMeta::new(c.name(), c.decl_type()))
        .collect();
    let mut out = Output::new(Columns::new(meta), tx);
    let mut rows = stmt.raw_query();
    let mut kept = 0;
    let mut started = Instant::now();
    while let Some(row) = rows.next().map_err(db_error)? {
        // Past the limit, rows are stepped and dropped so the statement ends.
        if kept == limit {
            continue;
        }
        if out.columns.rows() == 0 {
            started = Instant::now();
        }
        out.columns.push(row)?;
        kept += 1;
        let in_batch = out.columns.rows();
        if in_batch == BATCH_ROWS
            || (in_batch.is_multiple_of(256) && started.elapsed() > BATCH_INTERVAL)
        {
            out.flush()?;
        }
    }
    out.finish()
}

/// The frames of one statement's rows. A column that widens after batches
/// went out sends the schema again, with those batches widened.
struct Output<'a> {
    columns: Columns,
    tx: &'a FrameTx,
    schema: Option<arrow_schema::SchemaRef>,
    sent: Vec<Vec<ArrayRef>>,
}

impl<'a> Output<'a> {
    fn new(columns: Columns, tx: &'a FrameTx) -> Self {
        Self {
            columns,
            tx,
            schema: None,
            sent: Vec::new(),
        }
    }

    fn batch(schema: &arrow_schema::SchemaRef, arrays: Vec<ArrayRef>) -> Result<Frame, DbError> {
        RecordBatch::try_new(schema.clone(), arrays)
            .map(Frame::Batch)
            .map_err(|err| DbError::General(format!("building a batch: {err}")))
    }

    fn flush(&mut self) -> Result<(), DbError> {
        let arrays = self.columns.finish();
        if self.schema.is_none() || self.columns.widened {
            let schema = decode::schema(&self.columns.meta);
            send(self.tx, Frame::Schema(schema.clone()))?;
            for arrays in &mut self.sent {
                *arrays = decode::widen_batch(arrays, &self.columns.meta);
                send(self.tx, Self::batch(&schema, arrays.clone())?)?;
            }
            self.schema = Some(schema);
            self.columns.widened = false;
        }
        let schema = self.schema.clone().expect("the schema went out");
        send(self.tx, Self::batch(&schema, arrays.clone())?)?;
        self.sent.push(arrays);
        Ok(())
    }

    fn finish(mut self) -> Result<(), DbError> {
        if self.columns.rows() > 0 || self.schema.is_none() {
            self.flush()?;
        }
        Ok(())
    }
}

/// A native SQLite session.
pub struct SqliteSession {
    worker: Worker,
    catalog: SqliteCatalog,
    interrupt: std::sync::Arc<InterruptHandle>,
}

impl Drop for SqliteSession {
    /// Stops what the thread is running, so it ends with the session.
    fn drop(&mut self) {
        self.interrupt.interrupt();
    }
}

impl Session for SqliteSession {
    fn caps(&self) -> Capabilities {
        Capabilities::TRANSACTIONS
            | Capabilities::EXPLAIN
            | Capabilities::CANCEL
            | Capabilities::EDIT_BY_PK
            | Capabilities::MULTI_STATEMENT
    }

    fn language(&self) -> QueryLanguage {
        QueryLanguage::Sql(SqlDialect::Sqlite)
    }

    fn catalog(&mut self) -> &mut dyn Catalog {
        &mut self.catalog
    }

    fn run(&mut self, request: Request) -> BoxFuture<'_, Result<ResultStream, DbError>> {
        let limit = request.row_limit.unwrap_or(usize::MAX);
        let stream = self.worker.stream(|tx| Job::Run {
            sql: request.text,
            limit,
            tx,
        });
        Box::pin(async move { stream })
    }

    fn cancel_handle(&self) -> CancelHandle {
        let interrupt = self.interrupt.clone();
        CancelHandle::new(move || {
            interrupt.interrupt();
            Box::pin(async { Ok(true) })
        })
    }

    fn apply(&mut self, changes: ChangeSet) -> BoxFuture<'_, Result<ApplyReport, DbError>> {
        Box::pin(self.catalog.apply(changes))
    }
}
