//! A MongoDB session on the official driver. What the user types is read as
//! mongo shell commands ([`shell`]) and run with the driver's API; documents
//! come back as Arrow columns learnt from them ([`convert`]).

pub mod catalog;
pub mod convert;
pub mod literal;
pub mod shell;

use std::{
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use dfox_core::{
    arrow::{Frame, ResultStream},
    catalog::Catalog,
    errors::DbError,
    models::connections::{ConnectionParams, TlsMode},
    session::{
        ApplyReport, CancelHandle, Capabilities, ChangeSet, QueryLanguage, Request, Session,
    },
};
use futures_util::{future::BoxFuture, stream, Stream, StreamExt};
use mongodb::{
    bson::{doc, Bson, Document},
    options::{ClientOptions, Credential, ServerAddress, Tls, TlsOptions},
    Client, Database,
};
use tokio::sync::mpsc;

use self::{
    catalog::MongoCatalog,
    convert::Shape,
    shell::{Call, Command},
};

/// Rows in one batch, at most.
const BATCH_ROWS: usize = 4096;
/// A batch goes out after this long even when not full, so the first rows
/// show soon.
const BATCH_INTERVAL: Duration = Duration::from_millis(16);
/// The database a session starts in when none is given, as in mongosh.
pub const DEFAULT_DATABASE: &str = "test";

/// Methods that change data or schema, refused on a read-only connection.
const WRITES: &[&str] = &[
    "insertOne",
    "insertMany",
    "insert",
    "updateOne",
    "updateMany",
    "update",
    "replaceOne",
    "deleteOne",
    "deleteMany",
    "remove",
    "drop",
    "createIndex",
    "createIndexes",
    "dropIndex",
    "dropIndexes",
    "renameCollection",
    "findOneAndUpdate",
    "findOneAndReplace",
    "findOneAndDelete",
    "bulkWrite",
    "createCollection",
    "createView",
    "dropDatabase",
];

pub(crate) fn db_error(err: mongodb::error::Error) -> DbError {
    use mongodb::error::ErrorKind;
    match *err.kind {
        ErrorKind::Authentication { .. }
        | ErrorKind::ServerSelection { .. }
        | ErrorKind::Io(_)
        | ErrorKind::DnsResolve { .. } => DbError::Connection(err.to_string()),
        ErrorKind::InvalidArgument { .. } => DbError::Config(err.to_string()),
        _ => DbError::General(err.to_string()),
    }
}

/// The client options for `params`. Through a tunnel it connects to the
/// tunnel, straight to that one server.
pub fn options(params: &ConnectionParams) -> ClientOptions {
    let (host, port) = match params.tunnel {
        Some(tunnel) => (tunnel.ip().to_string(), tunnel.port()),
        None => (params.host.clone(), params.port),
    };
    let mut options = ClientOptions::default();
    options.hosts = vec![ServerAddress::Tcp {
        host,
        port: Some(if port == 0 { 27017 } else { port }),
    }];
    options.app_name = Some("dfox".to_string());
    options.connect_timeout = Some(Duration::from_secs(10));
    options.server_selection_timeout = Some(Duration::from_secs(10));
    if params.tunnel.is_some() {
        options.direct_connection = Some(true);
    }
    if !params.username.is_empty() {
        let mut credential = Credential::default();
        credential.username = Some(params.username.clone());
        credential.password = Some(params.password.clone());
        // Users usually live in `admin`; the driver asks there.
        credential.source = Some("admin".to_string());
        options.credential = Some(credential);
    }
    options.tls = match params.tls {
        // MongoDB doesn't negotiate: without a choice, plain.
        TlsMode::Disable | TlsMode::Prefer => None,
        TlsMode::Require => {
            let mut tls = TlsOptions::default();
            tls.allow_invalid_certificates = Some(true);
            Some(Tls::Enabled(tls))
        }
        TlsMode::VerifyFull => {
            let mut tls = TlsOptions::default();
            tls.ca_file_path = params.ca_file.as_ref().map(Into::into);
            // Through a tunnel the address is the tunnel's, which the
            // server's certificate doesn't name: use Require there.
            Some(Tls::Enabled(tls))
        }
    };
    options
}

/// What the session's runs share.
#[derive(Debug)]
struct State {
    /// The database `use` switched to.
    database: String,
    /// The comment of the run in progress, to find it on the server.
    running: Option<String>,
}

pub struct MongoSession {
    client: Client,
    state: Arc<Mutex<State>>,
    catalog: MongoCatalog,
    read_only: bool,
    runs: Arc<AtomicU64>,
}

pub async fn open(params: &ConnectionParams) -> Result<Box<dyn Session>, DbError> {
    let client = Client::with_options(options(params)).map_err(db_error)?;
    // The driver connects lazily: ping now, so a wrong password fails here.
    client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await
        .map_err(db_error)?;
    let database = params
        .database
        .clone()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| DEFAULT_DATABASE.to_string());
    Ok(Box::new(MongoSession {
        catalog: MongoCatalog::new(client.clone()),
        client,
        state: Arc::new(Mutex::new(State {
            database,
            running: None,
        })),
        read_only: params.read_only,
        runs: Arc::new(AtomicU64::new(0)),
    }))
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Session for MongoSession {
    fn caps(&self) -> Capabilities {
        Capabilities::CANCEL | Capabilities::ROW_COUNT_ESTIMATE | Capabilities::MULTI_STATEMENT
    }

    fn language(&self) -> QueryLanguage {
        QueryLanguage::MongoShell
    }

    fn catalog(&mut self) -> &mut dyn Catalog {
        &mut self.catalog
    }

    fn run(&mut self, request: Request) -> BoxFuture<'_, Result<ResultStream, DbError>> {
        let parsed = shell::parse(&request.text);
        let client = self.client.clone();
        let state = self.state.clone();
        let read_only = self.read_only;
        let limit = request.row_limit.unwrap_or(usize::MAX);
        let comment = format!("dfox-{}", self.runs.fetch_add(1, Ordering::Relaxed));
        Box::pin(async move {
            let commands = parsed.map_err(|err| DbError::General(err.to_string()))?;
            if read_only {
                if let Some(write) = commands.iter().find_map(write_of) {
                    return Err(DbError::General(format!(
                        "this connection is read-only: {write} would write"
                    )));
                }
            }
            Ok(spawn_run(move |tx| async move {
                lock(&state).running = Some(comment.clone());
                let mut run = Run {
                    client,
                    state: state.clone(),
                    limit,
                    comment,
                    tx,
                };
                let mut outcome = Ok(());
                for command in commands {
                    outcome = run.command(command).await;
                    if outcome.is_err() {
                        break;
                    }
                }
                lock(&state).running = None;
                outcome
            }))
        })
    }

    fn cancel_handle(&self) -> CancelHandle {
        let client = self.client.clone();
        let state = self.state.clone();
        CancelHandle::new(move || {
            let client = client.clone();
            let running = lock(&state).running.clone();
            Box::pin(async move {
                let Some(comment) = running else {
                    return Ok(false);
                };
                kill(&client, &comment).await
            })
        })
    }

    fn apply(&mut self, _changes: ChangeSet) -> BoxFuture<'_, Result<ApplyReport, DbError>> {
        Box::pin(async {
            Err(DbError::General(
                "editing documents isn't supported yet".into(),
            ))
        })
    }
}

/// The write a command would make, if any.
fn write_of(command: &Command) -> Option<String> {
    match command {
        Command::Database(call) if WRITES.contains(&call.name.as_str()) => {
            Some(format!("db.{}()", call.name))
        }
        Command::Database(call) if call.name == "runCommand" || call.name == "adminCommand" => {
            // A command can do anything; only reads are let through.
            let first = match call.args.first() {
                Some(Bson::Document(d)) => d.keys().next().cloned(),
                _ => None,
            };
            let reads = [
                "ping",
                "buildInfo",
                "buildinfo",
                "dbStats",
                "collStats",
                "serverStatus",
                "listCollections",
                "listDatabases",
                "listIndexes",
                "count",
                "find",
                "aggregate",
                "distinct",
                "hello",
                "isMaster",
                "explain",
                "connectionStatus",
                "currentOp",
                "hostInfo",
                "getParameter",
                "validate",
            ];
            match first {
                Some(name) if reads.contains(&name.as_str()) => None,
                other => Some(format!(
                    "db.{}({{{}: …}})",
                    call.name,
                    other.unwrap_or_default()
                )),
            }
        }
        Command::Collection {
            collection, call, ..
        } if WRITES.contains(&call.name.as_str()) => {
            Some(format!("db.{collection}.{}()", call.name))
        }
        Command::Collection {
            collection, call, ..
        } if call.name == "aggregate" => {
            // `$out` and `$merge` write the pipeline's result.
            let writes = matches!(call.args.first(), Some(Bson::Array(stages)) if stages.iter().any(|s| {
                matches!(s, Bson::Document(d) if d.contains_key("$out") || d.contains_key("$merge"))
            }));
            writes.then(|| format!("db.{collection}.aggregate([…$out/$merge])"))
        }
        _ => None,
    }
}

/// Finds the run tagged with `comment` on the server and stops it.
async fn kill(client: &Client, comment: &str) -> Result<bool, DbError> {
    let admin = client.database("admin");
    let ops = admin
        .run_command(doc! { "currentOp": true, "command.comment": comment })
        .await
        .map_err(db_error)?;
    let mut killed = false;
    for op in ops
        .get_array("inprog")
        .map(|a| a.to_vec())
        .unwrap_or_default()
    {
        let Some(id) = op.as_document().and_then(|d| d.get("opid")).cloned() else {
            continue;
        };
        admin
            .run_command(doc! { "killOp": 1, "op": id })
            .await
            .map_err(db_error)?;
        killed = true;
    }
    Ok(killed)
}

/// Runs `work` on its own task, which sends frames as they are ready.
pub(crate) fn spawn_run<F, Fut>(work: F) -> ResultStream
where
    F: FnOnce(mpsc::Sender<Result<Frame, DbError>>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), DbError>> + Send + 'static,
{
    let (tx, rx) = mpsc::channel(4);
    let errors = tx.clone();
    tokio::spawn(async move {
        if let Err(err) = work(tx).await {
            let _ = errors.send(Err(err)).await;
        }
    });
    Box::pin(stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|frame| (frame, rx))
    }))
}

pub(crate) async fn send(
    tx: &mpsc::Sender<Result<Frame, DbError>>,
    frame: Frame,
) -> Result<(), DbError> {
    tx.send(Ok(frame)).await.map_err(|_| DbError::Cancelled)
}

/// Streams documents as a result set: columns learnt as they come, at most
/// `limit` rows kept. When a document brings a new field the result starts
/// over, wider.
pub(crate) async fn send_docs<S>(
    mut docs: S,
    limit: usize,
    tx: &mpsc::Sender<Result<Frame, DbError>>,
) -> Result<(), DbError>
where
    S: Stream<Item = Result<Document, DbError>> + Unpin,
{
    let mut shape = Shape::default();
    let mut kept: Vec<Document> = Vec::new();
    let mut sent = 0;
    let mut started = false;
    let mut since = Instant::now();
    while kept.len() < limit {
        let Some(doc) = docs.next().await else { break };
        let doc = doc?;
        let wider = shape.learn(&doc);
        kept.push(doc);
        if wider && started {
            // Every row so far, again, with the new columns.
            send(tx, Frame::Schema(shape.schema())).await?;
            for chunk in kept[..sent].chunks(BATCH_ROWS) {
                send(tx, Frame::Batch(shape.batch(chunk)?)).await?;
            }
        }
        if kept.len() - sent >= BATCH_ROWS || since.elapsed() > BATCH_INTERVAL {
            if !started {
                send(tx, Frame::Schema(shape.schema())).await?;
                started = true;
            }
            send(tx, Frame::Batch(shape.batch(&kept[sent..])?)).await?;
            sent = kept.len();
            since = Instant::now();
        }
    }
    if !started {
        send(tx, Frame::Schema(shape.schema())).await?;
    }
    if kept.len() > sent {
        send(tx, Frame::Batch(shape.batch(&kept[sent..])?)).await?;
    }
    Ok(())
}

/// One result of a single document, such as a command's reply.
async fn send_one(doc: Document, tx: &mpsc::Sender<Result<Frame, DbError>>) -> Result<(), DbError> {
    send_docs(stream::iter([Ok(doc)]), 1, tx).await
}

/// A run in progress: the commands of one request, in turn.
struct Run {
    client: Client,
    state: Arc<Mutex<State>>,
    limit: usize,
    comment: String,
    tx: mpsc::Sender<Result<Frame, DbError>>,
}

/// The document at `i`, or an empty one when there is none.
fn doc_arg(call: &Call, i: usize) -> Result<Document, DbError> {
    match call.args.get(i) {
        None | Some(Bson::Null | Bson::Undefined) => Ok(Document::new()),
        Some(Bson::Document(d)) => Ok(d.clone()),
        Some(other) => Err(DbError::General(format!(
            "{}() expects a document, not {other}",
            call.name
        ))),
    }
}

fn number_arg(call: &Call) -> Result<i64, DbError> {
    match call.args.first() {
        Some(Bson::Int32(n)) => Ok(i64::from(*n)),
        Some(Bson::Int64(n)) => Ok(*n),
        Some(Bson::Double(n)) if n.fract() == 0.0 => Ok(*n as i64),
        _ => Err(DbError::General(format!(
            "{}() expects a number",
            call.name
        ))),
    }
}

fn docs_arg(call: &Call, i: usize) -> Result<Vec<Document>, DbError> {
    match call.args.get(i) {
        Some(Bson::Array(items)) => items
            .iter()
            .map(|item| match item {
                Bson::Document(d) => Ok(d.clone()),
                other => Err(DbError::General(format!(
                    "{}() expects documents, not {other}",
                    call.name
                ))),
            })
            .collect(),
        _ => Err(DbError::General(format!(
            "{}() expects an array",
            call.name
        ))),
    }
}

/// An update: a document of operators, or an aggregation pipeline.
fn update_arg(call: &Call) -> Result<mongodb::options::UpdateModifications, DbError> {
    use mongodb::options::UpdateModifications;
    match call.args.get(1) {
        Some(Bson::Document(d)) => Ok(UpdateModifications::Document(d.clone())),
        Some(Bson::Array(_)) => Ok(UpdateModifications::Pipeline(docs_arg(call, 1)?)),
        _ => Err(DbError::General(format!(
            "{}() expects an update",
            call.name
        ))),
    }
}

fn unknown(what: &str) -> DbError {
    DbError::General(format!("dfox doesn't run {what} yet"))
}

impl Run {
    fn database(&self) -> Database {
        let name = lock(&self.state).database.clone();
        self.client.database(&name)
    }

    async fn command(&mut self, command: Command) -> Result<(), DbError> {
        match command {
            Command::ShowDatabases => {
                let databases = self
                    .client
                    .list_databases()
                    .await
                    .map_err(db_error)?
                    .into_iter()
                    .map(|d| Ok(doc! { "name": d.name, "sizeOnDisk": d.size_on_disk as i64, "empty": d.empty }));
                send_docs(stream::iter(databases), self.limit, &self.tx).await
            }
            Command::ShowCollections => {
                let mut names = self
                    .database()
                    .list_collection_names()
                    .await
                    .map_err(db_error)?;
                names.sort();
                let rows = names.into_iter().map(|name| Ok(doc! { "name": name }));
                send_docs(stream::iter(rows), self.limit, &self.tx).await
            }
            Command::Use(name) => {
                lock(&self.state).database = name.clone();
                send(&self.tx, Frame::Notice(format!("switched to db {name}"))).await
            }
            Command::Database(call) => self.database_call(call).await,
            Command::Collection {
                collection,
                call,
                then,
            } => self.collection_call(&collection, call, then).await,
        }
    }

    async fn database_call(&mut self, call: Call) -> Result<(), DbError> {
        let db = self.database();
        let reply = match call.name.as_str() {
            "runCommand" => db.run_command(doc_arg(&call, 0)?).await,
            "adminCommand" => {
                self.client
                    .database("admin")
                    .run_command(doc_arg(&call, 0)?)
                    .await
            }
            "stats" => db.run_command(doc! { "dbStats": 1 }).await,
            // A pipeline of the database, such as `$documents` or `$currentOp`.
            "aggregate" => {
                let cursor = db
                    .aggregate(docs_arg(&call, 0)?)
                    .comment(Bson::String(self.comment.clone()))
                    .await
                    .map_err(db_error)?;
                return send_docs(cursor.map(|d| d.map_err(db_error)), self.limit, &self.tx).await;
            }
            "version" => {
                let info = db
                    .run_command(doc! { "buildInfo": 1 })
                    .await
                    .map_err(db_error)?;
                Ok(doc! { "version": info.get_str("version").unwrap_or_default() })
            }
            "getName" => Ok(doc! { "name": db.name() }),
            "getCollectionNames" => return Box::pin(self.command(Command::ShowCollections)).await,
            "createCollection" => {
                let Some(Bson::String(name)) = call.args.first() else {
                    return Err(DbError::General("createCollection() needs a name".into()));
                };
                let mut command = doc! { "create": name };
                command.extend(doc_arg(&call, 1)?);
                db.run_command(command).await
            }
            "createView" => {
                let (Some(Bson::String(name)), Some(Bson::String(source))) =
                    (call.args.first(), call.args.get(1))
                else {
                    return Err(DbError::General(
                        "createView() needs a name and a source".into(),
                    ));
                };
                let pipeline = docs_arg(&call, 2)?;
                db.run_command(doc! { "create": name, "viewOn": source, "pipeline": pipeline })
                    .await
            }
            "dropDatabase" => db.drop().await.map(|()| doc! { "ok": 1 }),
            other => return Err(unknown(&format!("db.{other}()"))),
        }
        .map_err(db_error)?;
        send_one(reply, &self.tx).await
    }

    async fn collection_call(
        &mut self,
        name: &str,
        call: Call,
        then: Vec<Call>,
    ) -> Result<(), DbError> {
        let coll = self.database().collection::<Document>(name);
        let comment = Bson::String(self.comment.clone());
        let affected = |n: u64| Frame::Affected(n);
        match call.name.as_str() {
            "find" => {
                let mut find = coll.find(doc_arg(&call, 0)?).comment(comment);
                if call.args.get(1).is_some() {
                    find = find.projection(doc_arg(&call, 1)?);
                }
                for step in &then {
                    find = match step.name.as_str() {
                        "sort" => find.sort(doc_arg(step, 0)?),
                        "limit" => find.limit(number_arg(step)?),
                        "skip" => find.skip(number_arg(step)? as u64),
                        "projection" => find.projection(doc_arg(step, 0)?),
                        "batchSize" => find.batch_size(number_arg(step)? as u32),
                        "maxTimeMS" => {
                            find.max_time(Duration::from_millis(number_arg(step)? as u64))
                        }
                        "hint" => match step.args.first() {
                            Some(Bson::String(index)) => {
                                find.hint(mongodb::options::Hint::Name(index.clone()))
                            }
                            _ => find.hint(mongodb::options::Hint::Keys(doc_arg(step, 0)?)),
                        },
                        "pretty" | "toArray" => find,
                        "count" | "itcount" | "size" => {
                            let n = coll
                                .count_documents(doc_arg(&call, 0)?)
                                .await
                                .map_err(db_error)?;
                            return send_one(doc! { "count": n as i64 }, &self.tx).await;
                        }
                        other => return Err(unknown(&format!("find().{other}()"))),
                    };
                }
                let cursor = find.await.map_err(db_error)?;
                send_docs(cursor.map(|d| d.map_err(db_error)), self.limit, &self.tx).await
            }
            "findOne" => {
                let mut find = coll.find_one(doc_arg(&call, 0)?).comment(comment);
                if call.args.get(1).is_some() {
                    find = find.projection(doc_arg(&call, 1)?);
                }
                let found = find.await.map_err(db_error)?;
                send_docs(stream::iter(found.map(Ok)), 1, &self.tx).await
            }
            "aggregate" => {
                let pipeline = docs_arg(&call, 0)?;
                let options = doc_arg(&call, 1)?;
                let mut aggregate = coll.aggregate(pipeline).comment(comment);
                if let Ok(allow) = options.get_bool("allowDiskUse") {
                    aggregate = aggregate.allow_disk_use(allow);
                }
                let cursor = aggregate.await.map_err(db_error)?;
                send_docs(cursor.map(|d| d.map_err(db_error)), self.limit, &self.tx).await
            }
            "countDocuments" | "count" => {
                let n = coll
                    .count_documents(doc_arg(&call, 0)?)
                    .comment(comment)
                    .await
                    .map_err(db_error)?;
                send_one(doc! { "count": n as i64 }, &self.tx).await
            }
            "estimatedDocumentCount" => {
                let n = coll.estimated_document_count().await.map_err(db_error)?;
                send_one(doc! { "count": n as i64 }, &self.tx).await
            }
            "distinct" => {
                let Some(Bson::String(field)) = call.args.first() else {
                    return Err(DbError::General("distinct() needs a field".into()));
                };
                let values = coll
                    .distinct(field.as_str(), doc_arg(&call, 1)?)
                    .comment(comment)
                    .await
                    .map_err(db_error)?;
                let field = field.clone();
                let rows = values
                    .into_iter()
                    .map(move |v| Ok(doc! { field.clone(): v }));
                send_docs(stream::iter(rows), self.limit, &self.tx).await
            }
            "getIndexes" => {
                let reply = self
                    .database()
                    .run_command(doc! { "listIndexes": name })
                    .await
                    .map_err(db_error)?;
                let indexes = reply
                    .get_document("cursor")
                    .and_then(|c| c.get_array("firstBatch"))
                    .map(|batch| batch.to_vec())
                    .unwrap_or_default();
                let rows = indexes.into_iter().filter_map(|i| match i {
                    Bson::Document(d) => Some(Ok(d)),
                    _ => None,
                });
                send_docs(stream::iter(rows), self.limit, &self.tx).await
            }
            "insertOne" | "insert" if matches!(call.args.first(), Some(Bson::Document(_))) => {
                coll.insert_one(doc_arg(&call, 0)?)
                    .await
                    .map_err(db_error)?;
                send(&self.tx, affected(1)).await
            }
            "insertMany" | "insert" => {
                let docs = docs_arg(&call, 0)?;
                let inserted = coll.insert_many(docs).await.map_err(db_error)?;
                send(&self.tx, affected(inserted.inserted_ids.len() as u64)).await
            }
            "updateOne" | "updateMany" | "replaceOne" => {
                let filter = doc_arg(&call, 0)?;
                let upsert = doc_arg(&call, 2)?.get_bool("upsert").unwrap_or(false);
                let result = match call.name.as_str() {
                    "updateOne" => {
                        coll.update_one(filter, update_arg(&call)?)
                            .upsert(upsert)
                            .await
                    }
                    "updateMany" => {
                        coll.update_many(filter, update_arg(&call)?)
                            .upsert(upsert)
                            .await
                    }
                    _ => {
                        coll.replace_one(filter, doc_arg(&call, 1)?)
                            .upsert(upsert)
                            .await
                    }
                }
                .map_err(db_error)?;
                let upserted = u64::from(result.upserted_id.is_some());
                send(&self.tx, affected(result.modified_count + upserted)).await
            }
            "deleteOne" | "deleteMany" | "remove" => {
                let filter = doc_arg(&call, 0)?;
                let result = if call.name == "deleteOne" {
                    coll.delete_one(filter).comment(comment).await
                } else {
                    coll.delete_many(filter).comment(comment).await
                }
                .map_err(db_error)?;
                send(&self.tx, affected(result.deleted_count)).await
            }
            "drop" => {
                coll.drop().await.map_err(db_error)?;
                send(&self.tx, affected(0)).await
            }
            "createIndex" => {
                let keys = doc_arg(&call, 0)?;
                let mut index = doc! { "key": keys.clone() };
                index.extend(doc_arg(&call, 1)?);
                if !index.contains_key("name") {
                    let name: Vec<String> = keys.iter().map(|(k, v)| format!("{k}_{v}")).collect();
                    index.insert("name", name.join("_"));
                }
                let reply = self
                    .database()
                    .run_command(doc! { "createIndexes": name, "indexes": [index] })
                    .await
                    .map_err(db_error)?;
                send_one(reply, &self.tx).await
            }
            "dropIndex" => {
                let index = call.args.first().cloned().unwrap_or(Bson::Null);
                let reply = self
                    .database()
                    .run_command(doc! { "dropIndexes": name, "index": index })
                    .await
                    .map_err(db_error)?;
                send_one(reply, &self.tx).await
            }
            "stats" => {
                let reply = self
                    .database()
                    .run_command(doc! { "collStats": name })
                    .await
                    .map_err(db_error)?;
                send_one(reply, &self.tx).await
            }
            other => Err(unknown(&format!("db.{name}.{other}()"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use super::*;

    #[test]
    fn options_come_from_the_fields() {
        let params = ConnectionParams {
            host: "db.example.com".into(),
            port: 27018,
            username: "ada".into(),
            password: "p@ss:w/rd".into(),
            tls: TlsMode::VerifyFull,
            ..Default::default()
        };
        let options = options(&params);
        assert_eq!(options.hosts[0].to_string(), "db.example.com:27018");
        let credential = options.credential.expect("a credential");
        assert_eq!(credential.password.as_deref(), Some("p@ss:w/rd"));
        assert_eq!(credential.source.as_deref(), Some("admin"));
        assert!(matches!(options.tls, Some(Tls::Enabled(_))));
        assert_eq!(options.direct_connection, None);

        let tunnelled = ConnectionParams {
            tunnel: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 40000))),
            username: String::new(),
            tls: TlsMode::Prefer,
            ..params
        };
        let options = super::options(&tunnelled);
        assert_eq!(options.hosts[0].to_string(), "127.0.0.1:40000");
        assert_eq!(options.direct_connection, Some(true));
        assert!(options.credential.is_none());
        assert!(options.tls.is_none());
    }

    #[test]
    fn read_only_connections_refuse_writes() {
        let write = |text: &str| shell::parse(text).expect("parse").iter().find_map(write_of);
        assert_eq!(write("db.users.find({})"), None);
        assert_eq!(write("db.runCommand({ ping: 1 })"), None);
        assert_eq!(
            write("db.users.find(); db.users.deleteMany({})"),
            Some("db.users.deleteMany()".into())
        );
        assert_eq!(
            write("db.runCommand({ drop: 'users' })"),
            Some("db.runCommand({drop: …})".into())
        );
        assert_eq!(
            write("db.a.aggregate([{ $match: {} }, { $out: 'b' }])"),
            Some("db.a.aggregate([…$out/$merge])".into())
        );
        assert_eq!(write("db.a.aggregate([{ $match: {} }])"), None);
    }
}
