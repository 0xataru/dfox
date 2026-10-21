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

fn finished(joined: Result<Result<QueryStats, DbError>, JoinError>) -> Result<QueryStats, DbError> {
    match joined {
        Ok(outcome) => outcome,
        Err(err) if err.is_cancelled() => Err(DbError::Cancelled),
        Err(err) => Err(DbError::General(format!("the query task failed: {err}"))),
    }
}
