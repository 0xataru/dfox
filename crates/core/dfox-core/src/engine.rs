//! Runs database work in the background, so a UI never waits on a database.
//!
//! The UI sends [`Command`]s through an [`EngineHandle`] and receives
//! [`EngineEvent`]s on a channel. The engine holds one [`Session`]; every
//! command runs in its own task, so a slow query never holds up browsing the
//! catalog or cancelling it.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use futures_util::{future::BoxFuture, StreamExt};
use tokio::{
    sync::{mpsc, Mutex},
    task::{AbortHandle, JoinError},
    time::Instant,
};

use crate::{
    arrow::Frame,
    catalog::{BrowseQuery, Catalog, ColumnStats, EntityInfo, Node, NodeKind, NodePath},
    driver::{ping, Driver, Drivers},
    errors::DbError,
    models::{
        connections::{ConnectionParams, DbType},
        schema::TableColumn,
    },
    session::{
        ApplyReport, CancelHandle, Capabilities, ChangeSet, QueryLanguage, Request, Session,
    },
};

/// Chosen by the caller to tell its queries' events apart.
pub type QueryId = u64;

/// How long a query may keep running after the server was asked to cancel it,
/// before the engine drops it.
const CANCEL_GRACE: Duration = Duration::from_secs(2);

/// Opens a session with a driver. The app passes one that goes through an
/// SSH tunnel when the connection has one.
pub type Opener = Arc<
    dyn Fn(
            Arc<dyn Driver>,
            ConnectionParams,
        ) -> BoxFuture<'static, Result<Box<dyn Session>, DbError>>
        + Send
        + Sync,
>;

/// Opens the driver's session directly.
pub fn direct() -> Opener {
    Arc::new(|driver, params| Box::pin(async move { driver.open(&params).await }))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Connects, replacing the current session. Queries still running on the
    /// old one are dropped.
    Connect {
        db_type: DbType,
        params: ConnectionParams,
    },
    /// The nodes under `path` in the catalog tree.
    Children {
        path: NodePath,
    },
    /// The columns, keys and indexes of the table at `path`.
    Describe {
        path: NodePath,
    },
    /// Every column of every table under `path`, for autocomplete.
    AllColumns {
        path: NodePath,
    },
    /// The SQL that creates the view or routine at `path`.
    Definition {
        path: NodePath,
        kind: NodeKind,
    },
    /// What is known about the values of `column` in the table at `path`;
    /// with `profile`, counted now. See [`Catalog::column_stats`].
    ColumnStats {
        path: NodePath,
        column: String,
        profile: bool,
    },
    /// Measures how long it takes to reach a database, without logging in
    /// and without touching the session. See [`ping`].
    Ping {
        id: u64,
        db_type: DbType,
        params: ConnectionParams,
    },
    /// Runs a script on the session. Each result set keeps at most
    /// `row_limit` rows.
    Run {
        id: QueryId,
        sql: String,
        row_limit: usize,
    },
    /// Streams the rows of the table at `path`, as `query` picks them, as
    /// query `id`.
    Browse {
        id: QueryId,
        path: NodePath,
        query: BrowseQuery,
    },
    /// Counts the rows of the table at `path`, exactly.
    Count {
        path: NodePath,
    },
    /// Writes edits made in the grid to their table. See [`Session::apply`].
    Apply {
        changes: ChangeSet,
    },
    /// Runs `sql` with no row limit and writes its rows to `file`, in the
    /// format its extension names, as query `id`: it reports
    /// [`EngineEvent::Progress`] and ends with [`EngineEvent::QueryDone`],
    /// and can be cancelled. A failed or cancelled export leaves no file.
    Export {
        id: QueryId,
        sql: String,
        file: std::path::PathBuf,
    },
    /// Adds the rows of `file` to the table at `table`, as query `id`: its
    /// columns go to the table's `columns` names, one per file column, `None`
    /// skipping it, or with no `columns` to the table's of the same name.
    /// Empty cells are NULL, and columns left out get their defaults. Rows go in chunks, each in its
    /// own transaction; it
    /// reports [`EngineEvent::Progress`] and ends with
    /// [`EngineEvent::QueryDone`], and can be cancelled.
    Import {
        id: QueryId,
        table: NodePath,
        file: std::path::PathBuf,
        columns: Option<Vec<Option<String>>>,
    },
    /// Runs `sql` with no row limit and adds its rows to a table of another
    /// connection, as query `id`, the way an import adds a file's. It opens
    /// the target's own session for that; it reports
    /// [`EngineEvent::Progress`], ends with [`EngineEvent::QueryDone`], and
    /// can be cancelled.
    Copy {
        id: QueryId,
        sql: String,
        target: Box<CopyTarget>,
    },
    Cancel {
        id: QueryId,
    },
}

/// What a new session can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionInfo {
    pub caps: Capabilities,
    pub language: QueryLanguage,
}

#[derive(Debug)]
pub enum EngineEvent {
    Connected(Result<SessionInfo, DbError>),
    Children {
        path: NodePath,
        result: Result<Vec<Node>, DbError>,
    },
    Described {
        path: NodePath,
        result: Result<EntityInfo, DbError>,
    },
    AllColumns {
        path: NodePath,
        result: Result<Vec<TableColumn>, DbError>,
    },
    Definition {
        path: NodePath,
        kind: NodeKind,
        result: Result<String, DbError>,
    },
    ColumnStats {
        path: NodePath,
        column: String,
        profile: bool,
        result: Result<Option<ColumnStats>, DbError>,
    },
    Counted {
        path: NodePath,
        result: Result<u64, DbError>,
    },
    /// The edits of the table at `path` were written, or not.
    Applied {
        path: NodePath,
        result: Result<ApplyReport, DbError>,
    },
    Pong {
        id: u64,
        result: Result<Duration, DbError>,
    },
    /// A statement of query `id` returned rows with this schema. Its batches
    /// follow, and replace those of any earlier result set in the same query.
    Schema {
        id: QueryId,
        schema: SchemaRef,
    },
    Batch {
        id: QueryId,
        batch: RecordBatch,
    },
    /// A message the server sent while running query `id`.
    Notice {
        id: QueryId,
        message: String,
    },
    /// An export has written, or an import added, `rows` rows so far.
    Progress {
        id: QueryId,
        rows: u64,
    },
    /// Always the last event of query `id`. A cancelled query ends with
    /// [`DbError::Cancelled`].
    QueryDone {
        id: QueryId,
        outcome: Result<QueryStats, DbError>,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryStats {
    pub elapsed: Duration,
    /// Time until the first row arrived, if any did.
    pub first_row: Option<Duration>,
    /// Rows kept from the last result set.
    pub rows: u64,
    pub rows_affected: u64,
    /// True when the last result set filled the row limit, so rows past it
    /// may have been dropped.
    pub truncated: bool,
}

/// Sends commands to the engine. Cheap to clone.
#[derive(Debug, Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl EngineHandle {
    /// A handle whose commands arrive on `tx`'s receiver, for testing code that
    /// drives the engine.
    pub fn new(tx: mpsc::UnboundedSender<Command>) -> Self {
        Self { tx }
    }

    pub fn send(&self, command: Command) {
        if self.tx.send(command).is_err() {
            log::warn!("the database engine has stopped");
        }
    }
}

/// Starts the engine on the current tokio runtime, opening sessions directly.
/// It stops once every [`EngineHandle`] is dropped.
pub fn spawn(drivers: Drivers) -> (EngineHandle, mpsc::Receiver<EngineEvent>) {
    spawn_with(drivers, direct())
}

/// Starts the engine, opening sessions with `open`.
pub fn spawn_with(drivers: Drivers, open: Opener) -> (EngineHandle, mpsc::Receiver<EngineEvent>) {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    // Bounded, so a query can't outrun a slow consumer and fill memory.
    let (event_tx, event_rx) = mpsc::channel(64);
    tokio::spawn(Engine::new(event_tx, drivers, open).run(command_rx));
    (EngineHandle::new(command_tx), event_rx)
}

/// The session, shared by the tasks that use it in turn.
type Shared = Arc<Mutex<Box<dyn Session>>>;

/// A connected session and the way to stop what it runs.
#[derive(Clone)]
struct Live {
    session: Shared,
    cancel: CancelHandle,
}

/// Results of background tasks that change the engine's own state.
enum Internal {
    Connected {
        generation: u64,
        result: Result<Live, DbError>,
    },
    QueryFinished(QueryId),
}

struct RunningQuery {
    abort: AbortHandle,
    cancelled: Arc<AtomicBool>,
}

/// A catalog call made on the session.
type CatalogCall<T> =
    for<'a> fn(&'a mut dyn Catalog, &'a NodePath) -> BoxFuture<'a, Result<T, DbError>>;

struct Engine {
    drivers: Drivers,
    open: Opener,
    events: mpsc::Sender<EngineEvent>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    internal_rx: mpsc::UnboundedReceiver<Internal>,
    live: Option<Live>,
    /// Bumped by every connect, so only the latest one is kept.
    generation: u64,
    queries: HashMap<QueryId, RunningQuery>,
}

impl Engine {
    fn new(events: mpsc::Sender<EngineEvent>, drivers: Drivers, open: Opener) -> Self {
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        Self {
            drivers,
            open,
            events,
            internal_tx,
            internal_rx,
            live: None,
            generation: 0,
            queries: HashMap::new(),
        }
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => self.handle(command),
                    None => break,
                },
                Some(internal) = self.internal_rx.recv() => self.update(internal).await,
            }
        }
        self.drop_queries();
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Connect { db_type, params } => self.connect(db_type, params),
            Command::Children { path } => self.catalog(
                path,
                |path, result| EngineEvent::Children { path, result },
                |catalog, path| catalog.children(path),
            ),
            Command::Describe { path } => self.catalog(
                path,
                |path, result| EngineEvent::Described { path, result },
                |catalog, path| catalog.describe(path),
            ),
            Command::AllColumns { path } => self.catalog(
                path,
                |path, result| EngineEvent::AllColumns { path, result },
                |catalog, path| catalog.all_columns(path),
            ),
            Command::Definition { path, kind } => {
                let session = self.live.as_ref().map(|live| live.session.clone());
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = match session {
                        Some(session) => {
                            let mut session = session.lock().await;
                            session.catalog().definition(&path, kind).await
                        }
                        None => Err(Self::not_connected()),
                    };
                    let event = EngineEvent::Definition { path, kind, result };
                    let _ = events.send(event).await;
                });
            }
            Command::ColumnStats {
                path,
                column,
                profile,
            } => {
                let session = self.live.as_ref().map(|live| live.session.clone());
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = match session {
                        Some(session) => {
                            let mut session = session.lock().await;
                            session
                                .catalog()
                                .column_stats(&path, &column, profile)
                                .await
                        }
                        None => Err(Self::not_connected()),
                    };
                    let event = EngineEvent::ColumnStats {
                        path,
                        column,
                        profile,
                        result,
                    };
                    let _ = events.send(event).await;
                });
            }
            Command::Ping {
                id,
                db_type,
                params,
            } => {
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = ping(db_type, &params).await;
                    let _ = events.send(EngineEvent::Pong { id, result }).await;
                });
            }
            Command::Run { id, sql, row_limit } => {
                let request = Request::new(sql).with_row_limit(row_limit);
                self.run_query(id, Start::Run(request), row_limit);
            }
            Command::Browse { id, path, query } => {
                let limit = query.limit.map_or(0, |l| l as usize);
                self.run_query(id, Start::Browse(path, query), limit);
            }
            Command::Count { path } => self.catalog(
                path,
                |path, result| EngineEvent::Counted { path, result },
                |catalog, path| catalog.count_rows(path),
            ),
            Command::Apply { changes } => self.apply(changes),
            Command::Export { id, sql, file } => self.export(id, sql, file),
            Command::Import {
                id,
                table,
                file,
                columns,
            } => self.import(id, table, file, columns),
            Command::Copy { id, sql, target } => self.copy(id, sql, *target),
            Command::Cancel { id } => self.cancel(id),
        }
    }

    fn apply(&self, changes: ChangeSet) {
        let session = self.live.as_ref().map(|live| live.session.clone());
        let events = self.events.clone();
        tokio::spawn(async move {
            let path = changes.table.clone();
            let result = match session {
                Some(session) => session.lock().await.apply(changes).await,
                None => Err(Self::not_connected()),
            };
            let _ = events.send(EngineEvent::Applied { path, result }).await;
        });
    }

    fn connect(&mut self, db_type: DbType, params: ConnectionParams) {
        self.generation += 1;
        let generation = self.generation;
        let internal = self.internal_tx.clone();
        let driver = self.drivers.get(db_type).cloned();
        let open = self.open.clone();
        tokio::spawn(async move {
            let result = match driver {
                Some(driver) => open(driver, params).await.map(|session| Live {
                    cancel: session.cancel_handle(),
                    session: Arc::new(Mutex::new(session)),
                }),
                None => Err(DbError::Config(format!(
                    "this build of dfox has no {db_type:?} driver"
                ))),
            };
            let _ = internal.send(Internal::Connected { generation, result });
        });
    }

    async fn update(&mut self, internal: Internal) {
        match internal {
            Internal::Connected { generation, result } => {
                if generation != self.generation {
                    return;
                }
                let result = match result {
                    Ok(live) => {
                        self.drop_queries();
                        let info = {
                            let session = live.session.lock().await;
                            SessionInfo {
                                caps: session.caps(),
                                language: session.language(),
                            }
                        };
                        self.live = Some(live);
                        Ok(info)
                    }
                    Err(err) => Err(err),
                };
                let _ = self.events.send(EngineEvent::Connected(result)).await;
            }
            Internal::QueryFinished(id) => {
                self.queries.remove(&id);
            }
        }
    }

    fn not_connected() -> DbError {
        DbError::Connection("Not connected to a database".to_string())
    }

    /// Runs a catalog call in the background and sends its result as an
    /// event.
    fn catalog<T: Send + 'static>(
        &self,
        path: NodePath,
        event: fn(NodePath, Result<T, DbError>) -> EngineEvent,
        call: CatalogCall<T>,
    ) {
        let session = self.live.as_ref().map(|live| live.session.clone());
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = match session {
                Some(session) => {
                    let mut session = session.lock().await;
                    call(session.catalog(), &path).await
                }
                None => Err(Self::not_connected()),
            };
            let _ = events.send(event(path, result)).await;
        });
    }

    fn run_query(&mut self, id: QueryId, start: Start, row_limit: usize) {
        let events = self.events.clone();
        let Some(live) = self.live.clone() else {
            tokio::spawn(async move {
                let outcome = Err(Self::not_connected());
                let _ = events.send(EngineEvent::QueryDone { id, outcome }).await;
            });
            return;
        };

        // The query runs in its own task so it can be aborted; the outer task
        // always reports how it ended.
        let query = tokio::spawn(stream_query(
            live.session,
            id,
            start,
            row_limit,
            events.clone(),
        ));
        self.track(id, query, None);
    }

    fn export(&mut self, id: QueryId, sql: String, file: std::path::PathBuf) {
        let events = self.events.clone();
        let Some(live) = self.live.clone() else {
            tokio::spawn(async move {
                let outcome = Err(Self::not_connected());
                let _ = events.send(EngineEvent::QueryDone { id, outcome }).await;
            });
            return;
        };
        let task = tokio::spawn(export_query(live.session, id, sql, file.clone(), events));
        self.track(id, task, Some(file));
    }

    fn import(
        &mut self,
        id: QueryId,
        table: NodePath,
        file: std::path::PathBuf,
        mapping: Option<Vec<Option<String>>>,
    ) {
        let events = self.events.clone();
        let Some(live) = self.live.clone() else {
            tokio::spawn(async move {
                let outcome = Err(Self::not_connected());
                let _ = events.send(EngineEvent::QueryDone { id, outcome }).await;
            });
            return;
        };
        let task = tokio::spawn(import_file(live.session, id, table, file, mapping, events));
        // The file is the source: whatever happens, it stays.
        self.track(id, task, None);
    }

    fn copy(&mut self, id: QueryId, sql: String, target: CopyTarget) {
        let events = self.events.clone();
        let Some(live) = self.live.clone() else {
            tokio::spawn(async move {
                let outcome = Err(Self::not_connected());
                let _ = events.send(EngineEvent::QueryDone { id, outcome }).await;
            });
            return;
        };
        let open = self
            .drivers
            .get(target.db_type)
            .cloned()
            .map(|driver| (driver, self.open.clone()));
        let task = tokio::spawn(copy_rows(live.session, id, sql, target, open, events));
        self.track(id, task, None);
    }

    /// Keeps `query` cancellable as `id`, and reports how it ended. A
    /// failed export's `file` is removed.
    fn track(
        &mut self,
        id: QueryId,
        query: tokio::task::JoinHandle<Result<QueryStats, DbError>>,
        file: Option<std::path::PathBuf>,
    ) {
        let events = self.events.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.queries.insert(
            id,
            RunningQuery {
                abort: query.abort_handle(),
                cancelled: cancelled.clone(),
            },
        );

        let internal = self.internal_tx.clone();
        tokio::spawn(async move {
            let outcome = finished(query.await);
            let outcome = if cancelled.load(Ordering::Relaxed) {
                Err(DbError::Cancelled)
            } else {
                outcome
            };
            if let (Err(_), Some(file)) = (&outcome, &file) {
                // shortcut: on Windows the writer may still hold the file
                // open for a moment and the removal fails; it is then left.
                let _ = std::fs::remove_file(file);
            }
            let _ = events.send(EngineEvent::QueryDone { id, outcome }).await;
            let _ = internal.send(Internal::QueryFinished(id));
        });
    }

    /// Asks the server to stop the query, and drops it if the server can't or
    /// doesn't stop it in time.
    fn cancel(&self, id: QueryId) {
        let Some(query) = self.queries.get(&id) else {
            return;
        };
        query.cancelled.store(true, Ordering::Relaxed);
        let abort = query.abort.clone();
        let cancel = self.live.as_ref().map(|live| live.cancel.clone());
        tokio::spawn(async move {
            let sent = match cancel {
                Some(cancel) => cancel.cancel().await.unwrap_or_else(|err| {
                    log::warn!("could not cancel query {id} on the server: {err}");
                    false
                }),
                None => false,
            };
            if sent {
                tokio::time::sleep(CANCEL_GRACE).await;
            }
            abort.abort();
        });
    }

    fn drop_queries(&mut self) {
        for (_, query) in self.queries.drain() {
            query.cancelled.store(true, Ordering::Relaxed);
            query.abort.abort();
        }
    }
}

/// What a query streams: a script's results, or a table's rows.
enum Start {
    Run(Request),
    Browse(NodePath, BrowseQuery),
}

/// Streams one query's results to `events` and measures it.
async fn stream_query(
    session: Shared,
    id: QueryId,
    start: Start,
    limit: usize,
    events: mpsc::Sender<EngineEvent>,
) -> Result<QueryStats, DbError> {
    let started = Instant::now();
    // The session is needed only to start the run; the stream lives on its
    // own, so the catalog can be browsed meanwhile.
    let mut frames = {
        let mut session = session.lock().await;
        match start {
            Start::Run(request) => session.run(request).await?,
            Start::Browse(path, query) => session.catalog().browse(&path, query).await?,
        }
    };
    let mut stats = QueryStats::default();
    let mut has_rows = false;
    while let Some(frame) = frames.next().await {
        let event = match frame? {
            Frame::Schema(schema) => {
                has_rows = true;
                stats.rows = 0;
                EngineEvent::Schema { id, schema }
            }
            Frame::Batch(batch) => {
                stats.first_row.get_or_insert_with(|| started.elapsed());
                stats.rows += batch.num_rows() as u64;
                EngineEvent::Batch { id, batch }
            }
            Frame::Affected(rows) => {
                stats.rows_affected += rows;
                continue;
            }
            Frame::Notice(message) => EngineEvent::Notice { id, message },
        };
        // Nobody is listening any more: stop reading.
        if events.send(event).await.is_err() {
            break;
        }
    }
    stats.elapsed = started.elapsed();
    stats.truncated = has_rows && limit > 0 && stats.rows >= limit as u64;
    Ok(stats)
}

/// Rows an import adds in one transaction: one COPY or a few multi-row
/// inserts, where the driver can.
const IMPORT_CHUNK: usize = 10_000;

/// Reads `file` on a blocking thread and adds its rows to `table`, a chunk
/// at a time.
async fn import_file(
    session: Shared,
    id: QueryId,
    table: NodePath,
    file: std::path::PathBuf,
    mapping: Option<Vec<Option<String>>>,
    events: mpsc::Sender<EngineEvent>,
) -> Result<QueryStats, DbError> {
    let started = Instant::now();
    let columns: Vec<String> = {
        let mut session = session.lock().await;
        let info = session.catalog().describe(&table).await?;
        info.columns.into_iter().map(|c| c.name).collect()
    };
    let (tx, mut rx) = mpsc::channel::<Result<RecordBatch, DbError>>(4);
    let read = tokio::task::spawn_blocking(move || {
        let batches = match crate::export::read(&file) {
            Ok(batches) => batches,
            Err(err) => {
                let _ = tx.blocking_send(Err(err));
                return;
            }
        };
        for batch in batches {
            let failed = batch.is_err();
            if tx.blocking_send(batch).is_err() || failed {
                return;
            }
        }
    });
    let mut load = Loader::new(table, columns, "the file's");
    load.names = mapping;
    while let Some(batch) = rx.recv().await {
        load.add(&session, batch?, id, &events).await?;
    }
    if load.names.is_none() {
        return Err(DbError::General("the file has no rows".into()));
    }
    let _ = read.await;
    load.stats.elapsed = started.elapsed();
    Ok(load.stats)
}

/// Adds batches of rows to a table, a chunk at a time, as an import or a
/// copy brings them.
struct Loader {
    table: NodePath,
    /// The table's columns; none when it knows none yet (a collection with
    /// no documents), and then the batches' are taken as they are.
    columns: Vec<String>,
    /// Whose columns they are, for the error that one is missing.
    whose: &'static str,
    /// The table's name for each of the batches' columns, matched once or
    /// given; `None` leaves a column out.
    names: Option<Vec<Option<String>>>,
    stats: QueryStats,
}

impl Loader {
    fn new(table: NodePath, columns: Vec<String>, whose: &'static str) -> Self {
        Self {
            table,
            columns,
            whose,
            names: None,
            stats: QueryStats::default(),
        }
    }

    /// Matches the batch's columns to the table's, the first time: by name,
    /// whatever their case.
    fn names(&mut self, batch: &RecordBatch) -> Result<Vec<Option<String>>, DbError> {
        if let Some(names) = &self.names {
            if names.len() != batch.num_columns() {
                return Err(DbError::General(format!(
                    "{} {} columns don't match the {} mapped",
                    self.whose,
                    batch.num_columns(),
                    names.len()
                )));
            }
            if names.iter().all(Option::is_none) {
                return Err(DbError::General("no columns to import".into()));
            }
            return Ok(names.clone());
        }
        let mut matched = Vec::new();
        for field in batch.schema().fields() {
            if self.columns.is_empty() {
                matched.push(Some(field.name().clone()));
                continue;
            }
            let name = self
                .columns
                .iter()
                .find(|c| c.eq_ignore_ascii_case(field.name()))
                .ok_or_else(|| {
                    DbError::General(format!(
                        "{} column {} isn't in the table",
                        self.whose,
                        field.name()
                    ))
                })?;
            matched.push(Some(name.clone()));
        }
        Ok(self.names.insert(matched).clone())
    }

    async fn add(
        &mut self,
        session: &Shared,
        batch: RecordBatch,
        id: QueryId,
        events: &mpsc::Sender<EngineEvent>,
    ) -> Result<(), DbError> {
        use crate::{
            arrow::ArrowResultSet,
            result::{OwnedCell, ResultSet},
            session::{Change, ChangeSet},
        };

        let names = self.names(&batch)?;
        let mut rows = ArrowResultSet::new(batch.schema());
        rows.push(batch);
        rows.finish();
        let count = rows.row_count();
        for first in (0..count).step_by(IMPORT_CHUNK) {
            let changes = (first..count.min(first + IMPORT_CHUNK))
                .map(|row| {
                    // Every row names the same columns, so a driver can send
                    // the chunk as one bulk insert.
                    Change::Insert(
                        names
                            .iter()
                            .enumerate()
                            .filter_map(|(col, name)| {
                                let cell =
                                    rows.cell(row, col).map_or(OwnedCell::Null, OwnedCell::from);
                                Some((name.clone()?, cell))
                            })
                            .collect(),
                    )
                })
                .collect();
            let report = session
                .lock()
                .await
                .apply(ChangeSet {
                    table: self.table.clone(),
                    key: Vec::new(),
                    changes,
                })
                .await
                .map_err(|err| {
                    DbError::General(format!("{err} (after {} rows went in)", self.stats.rows))
                })?;
            self.stats.rows += report.inserted;
            let rows = self.stats.rows;
            let _ = events.send(EngineEvent::Progress { id, rows }).await;
        }
        Ok(())
    }
}

/// Where a copy's rows go.
#[derive(Debug, Clone, PartialEq)]
pub struct CopyTarget {
    pub db_type: DbType,
    pub params: ConnectionParams,
    /// The table, as the target's catalog names it.
    pub table: NodePath,
    /// Create the table first, from the rows' columns.
    pub create: bool,
}

/// Runs `sql` on the session and adds its rows to the target's table, on a
/// session of its own.
async fn copy_rows(
    source: Shared,
    id: QueryId,
    sql: String,
    target: CopyTarget,
    open: Option<(Arc<dyn Driver>, Opener)>,
    events: mpsc::Sender<EngineEvent>,
) -> Result<QueryStats, DbError> {
    let started = Instant::now();
    let Some((driver, open)) = open else {
        return Err(DbError::Config(format!(
            "this build of dfox has no {:?} driver",
            target.db_type
        )));
    };
    let into: Shared = Arc::new(Mutex::new(open(driver, target.params.clone()).await?));
    let mut frames = source.lock().await.run(Request::new(sql)).await?;
    let mut load: Option<Loader> = None;
    while let Some(frame) = frames.next().await {
        match frame? {
            Frame::Schema(schema) => {
                if load.is_some() {
                    return Err(DbError::General(
                        "the script returns several results: copy one query at a time".into(),
                    ));
                }
                let columns = if target.create {
                    if let Some(ddl) =
                        crate::copy::create_table(target.db_type, &target.table, &schema)
                    {
                        let mut session = into.lock().await;
                        let mut created = session.run(Request::new(ddl)).await?;
                        while let Some(frame) = created.next().await {
                            frame?;
                        }
                    }
                    schema.fields().iter().map(|f| f.name().clone()).collect()
                } else {
                    let mut session = into.lock().await;
                    let info = session.catalog().describe(&target.table).await?;
                    info.columns.into_iter().map(|c| c.name).collect()
                };
                load = Some(Loader::new(target.table.clone(), columns, "the query's"));
            }
            Frame::Batch(batch) => {
                if let Some(load) = &mut load {
                    load.add(&into, batch, id, &events).await?;
                }
            }
            Frame::Affected(_) => {}
            Frame::Notice(message) => {
                let _ = events.send(EngineEvent::Notice { id, message }).await;
            }
        }
    }
    let Some(mut load) = load else {
        return Err(DbError::General(
            "the query returned no rows to copy".into(),
        ));
    };
    load.stats.elapsed = started.elapsed();
    Ok(load.stats)
}

/// How often an export reports its progress.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);

/// Runs `sql` and writes its one result to `file` on a blocking thread, as
/// the rows arrive.
async fn export_query(
    session: Shared,
    id: QueryId,
    sql: String,
    file: std::path::PathBuf,
    events: mpsc::Sender<EngineEvent>,
) -> Result<QueryStats, DbError> {
    let started = Instant::now();
    let mut frames = session.lock().await.run(Request::new(sql)).await?;
    let mut writer = None;
    let mut stats = QueryStats::default();
    let mut reported = Instant::now();
    while let Some(frame) = frames.next().await {
        match frame? {
            Frame::Schema(schema) => {
                if writer.is_some() {
                    return Err(DbError::General(
                        "the script returns several results: export one query at a time".into(),
                    ));
                }
                let (tx, mut rx) = mpsc::channel::<RecordBatch>(4);
                let path = file.clone();
                let write = tokio::task::spawn_blocking(move || {
                    let mut out = crate::export::Writer::create(&path, &schema)?;
                    while let Some(batch) = rx.blocking_recv() {
                        out.write(&batch)?;
                    }
                    out.finish()
                });
                writer = Some((tx, write));
            }
            Frame::Batch(batch) => {
                stats.first_row.get_or_insert_with(|| started.elapsed());
                stats.rows += batch.num_rows() as u64;
                let Some((tx, _)) = &writer else { continue };
                // The writer stopped: its error comes out below.
                if tx.send(batch).await.is_err() {
                    break;
                }
                if reported.elapsed() >= PROGRESS_EVERY {
                    reported = Instant::now();
                    let rows = stats.rows;
                    let _ = events.send(EngineEvent::Progress { id, rows }).await;
                }
            }
            Frame::Affected(rows) => stats.rows_affected += rows,
            Frame::Notice(message) => {
                let _ = events.send(EngineEvent::Notice { id, message }).await;
            }
        }
    }
    let Some((tx, write)) = writer else {
        return Err(DbError::General(
            "the query returned no rows to export".into(),
        ));
    };
    drop(tx);
    write
        .await
        .map_err(|err| DbError::General(format!("the export failed: {err}")))??;
    stats.elapsed = started.elapsed();
    Ok(stats)
}

fn finished(joined: Result<Result<QueryStats, DbError>, JoinError>) -> Result<QueryStats, DbError> {
    match joined {
        Ok(outcome) => outcome,
        Err(err) if err.is_cancelled() => Err(DbError::Cancelled),
        Err(err) => Err(DbError::General(format!("the query task failed: {err}"))),
    }
}
