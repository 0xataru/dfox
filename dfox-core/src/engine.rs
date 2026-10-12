//! Runs database work in the background, so a UI never waits on a database.
//!
//! The UI sends [`Command`]s through an [`EngineHandle`] and receives
//! [`EngineEvent`]s on a channel. Every command runs in its own task, so a slow
//! query never holds up listing tables or cancelling it.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::{
    sync::mpsc,
    task::{AbortHandle, JoinError},
    time::Instant,
};

use crate::{
    db::{Chunk, DbClient},
    errors::DbError,
    models::{
        connections::{ConnectionParams, DbType},
        schema::TableSchema,
    },
    result::{ColumnMeta, OwnedCell},
    DbManager,
};

/// Chosen by the caller to tell its queries' events apart.
pub type QueryId = u64;

/// How long a query may keep running after the server was asked to cancel it,
/// before the engine drops it.
const CANCEL_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub enum Command {
    /// Connects, replacing the current connection. Queries still running on
    /// the old connection are dropped.
    Connect {
        db_type: DbType,
        params: ConnectionParams,
    },
    ListDatabases,
    ListTables,
    Describe {
        table: String,
    },
    /// Runs SQL on the session connection. Each result set keeps at most
    /// `row_limit` rows.
    Run {
        id: QueryId,
        sql: String,
        row_limit: usize,
    },
    Cancel {
        id: QueryId,
    },
}

#[derive(Debug)]
pub enum EngineEvent {
    Connected(Result<(), DbError>),
    Databases(Result<Vec<String>, DbError>),
    Tables(Result<Vec<String>, DbError>),
    Schema(Result<TableSchema, DbError>),
    /// A statement of query `id` returned rows. Its rows follow, and replace
    /// those of any earlier statement in the same query.
    Columns {
        id: QueryId,
        columns: Vec<ColumnMeta>,
    },
    Rows {
        id: QueryId,
        rows: Vec<Vec<OwnedCell>>,
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
    /// True when rows past the row limit were dropped.
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

/// Starts the engine on the current tokio runtime. It stops once every
/// [`EngineHandle`] is dropped.
pub fn spawn() -> (EngineHandle, mpsc::Receiver<EngineEvent>) {
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    // Bounded, so a query can't outrun a slow consumer and fill memory.
    let (event_tx, event_rx) = mpsc::channel(64);
    tokio::spawn(Engine::new(event_tx).run(command_rx));
    (EngineHandle::new(command_tx), event_rx)
}

type Client = Arc<dyn DbClient + Send + Sync>;

/// Results of background tasks that change the engine's own state.
enum Internal {
    Connected {
        generation: u64,
        result: Result<Client, DbError>,
    },
    QueryFinished(QueryId),
}

struct RunningQuery {
    abort: AbortHandle,
    cancelled: Arc<AtomicBool>,
}

struct Engine {
    events: mpsc::Sender<EngineEvent>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    internal_rx: mpsc::UnboundedReceiver<Internal>,
    client: Option<Client>,
    /// Bumped by every connect, so only the latest one is kept.
    generation: u64,
    queries: HashMap<QueryId, RunningQuery>,
}

impl Engine {
    fn new(events: mpsc::Sender<EngineEvent>) -> Self {
        let (internal_tx, internal_rx) = mpsc::unbounded_channel();
        Self {
            events,
            internal_tx,
            internal_rx,
            client: None,
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
            Command::Connect { db_type, params } => {
                self.generation += 1;
                let generation = self.generation;
                let internal = self.internal_tx.clone();
                tokio::spawn(async move {
                    let result = DbManager::connect(&db_type, &params)
                        .await
                        .map(Client::from);
                    let _ = internal.send(Internal::Connected { generation, result });
                });
            }
            Command::ListDatabases => self.metadata(EngineEvent::Databases, |client| async move {
                client.list_databases().await
            }),
            Command::ListTables => self.metadata(EngineEvent::Tables, |client| async move {
                client.list_tables().await
            }),
            Command::Describe { table } => self
                .metadata(EngineEvent::Schema, |client| async move {
                    client.describe_table(&table).await
                }),
            Command::Run { id, sql, row_limit } => self.run_query(id, sql, row_limit),
            Command::Cancel { id } => self.cancel(id),
        }
    }

    async fn update(&mut self, internal: Internal) {
        match internal {
            Internal::Connected { generation, result } => {
                if generation != self.generation {
                    return;
                }
                let result = result.map(|client| {
                    self.drop_queries();
                    self.client = Some(client);
                });
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

    /// Runs a metadata call in the background and sends its result as an event.
    fn metadata<T, F>(
        &self,
        event: fn(Result<T, DbError>) -> EngineEvent,
        call: impl FnOnce(Client) -> F + Send + 'static,
    ) where
        T: Send + 'static,
        F: std::future::Future<Output = Result<T, DbError>> + Send,
    {
        let client = self.client.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = match client {
                Some(client) => call(client).await,
                None => Err(Self::not_connected()),
            };
            let _ = events.send(event(result)).await;
        });
    }

    fn run_query(&mut self, id: QueryId, sql: String, row_limit: usize) {
        let events = self.events.clone();
        let Some(client) = self.client.clone() else {
            tokio::spawn(async move {
                let outcome = Err(Self::not_connected());
                let _ = events.send(EngineEvent::QueryDone { id, outcome }).await;
            });
            return;
        };

        // The query runs in its own task so it can be aborted; the outer task
        // always reports how it ended.
        let query = tokio::spawn(stream_query(client, id, sql, row_limit, events.clone()));
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
        let client = self.client.clone();
        tokio::spawn(async move {
            let sent = match client {
                Some(client) => client.cancel().await.unwrap_or_else(|err| {
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

/// Streams one query's rows to `events` and measures it.
async fn stream_query(
    client: Client,
    id: QueryId,
    sql: String,
    row_limit: usize,
    events: mpsc::Sender<EngineEvent>,
) -> Result<QueryStats, DbError> {
    let started = Instant::now();
    let (tx, mut rx) = mpsc::channel(2);

    let forward = async {
        let mut rows = 0;
        let mut first_row = None;
        while let Some(chunk) = rx.recv().await {
            let event = match chunk {
                Chunk::Columns(columns) => {
                    rows = 0;
                    EngineEvent::Columns { id, columns }
                }
                Chunk::Rows(chunk) => {
                    first_row.get_or_insert_with(|| started.elapsed());
                    rows += chunk.len() as u64;
                    EngineEvent::Rows { id, rows: chunk }
                }
            };
            // Nobody is listening any more: stop reading rows.
            if events.send(event).await.is_err() {
                break;
            }
        }
        (rows, first_row)
    };

    let (summary, (rows, first_row)) =
        futures_util::future::join(client.stream(&sql, row_limit, tx), forward).await;
    let summary = summary?;
    Ok(QueryStats {
        elapsed: started.elapsed(),
        first_row,
        rows,
        rows_affected: summary.rows_affected,
        truncated: summary.truncated,
    })
}

fn finished(joined: Result<Result<QueryStats, DbError>, JoinError>) -> Result<QueryStats, DbError> {
    match joined {
        Ok(outcome) => outcome,
        Err(err) if err.is_cancelled() => Err(DbError::Cancelled),
        Err(err) => Err(DbError::General(format!("the query task failed: {err}"))),
    }
}
