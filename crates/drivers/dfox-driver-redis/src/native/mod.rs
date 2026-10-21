//! A Redis or Valkey session on redis-rs. Each line the user types is one
//! command, sent as written; replies come back as tables ([`convert`]).
//! It speaks RESP3, so hashes arrive as maps: Redis 6 or newer.

pub mod catalog;
pub mod convert;

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
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
use futures_util::{future::BoxFuture, stream};
use redis::{
    aio::MultiplexedConnection, AsyncConnectionConfig, Client, ConnectionAddr, IntoConnectionInfo,
    ProtocolVersion, RedisConnectionInfo, TlsCertificates, Value,
};
use tokio::sync::mpsc;

use self::{catalog::RedisCatalog, convert::Table};

/// Rows in one batch, at most.
const BATCH_ROWS: usize = 4096;

/// What a read-only connection may run besides the commands the server
/// marks `readonly`: harmless ones it doesn't, and subcommands that only
/// look.
const READS: &[&str] = &[
    "ping",
    "echo",
    "info",
    "time",
    "select",
    "role",
    "lastsave",
    "command",
    "acl cat",
    "acl list",
    "acl users",
    "acl whoami",
    "client getname",
    "client id",
    "client info",
    "client list",
    "cluster info",
    "cluster nodes",
    "cluster shards",
    "cluster slots",
    "config get",
    "function list",
    "latency doctor",
    "latency history",
    "latency latest",
    "memory doctor",
    "memory stats",
    "memory usage",
    "module list",
    "object encoding",
    "object freq",
    "object idletime",
    "object refcount",
    "pubsub channels",
    "pubsub numpat",
    "pubsub numsub",
    "script exists",
    "slowlog get",
    "slowlog len",
    "xinfo consumers",
    "xinfo groups",
    "xinfo stream",
];

pub(crate) fn db_error(err: redis::RedisError) -> DbError {
    if err.is_io_error()
        || err.is_connection_refusal()
        || err.is_connection_dropped()
        || err.kind() == redis::ErrorKind::AuthenticationFailed
        // A wrong password or a missing one, as the server says.
        || matches!(err.code(), Some("WRONGPASS" | "NOAUTH"))
    {
        DbError::Connection(err.to_string())
    } else {
        DbError::General(err.to_string())
    }
}

/// The database number in `params`: `0` when none is given.
pub fn database(params: &ConnectionParams) -> Result<i64, DbError> {
    match params.database.as_deref().map(str::trim) {
        None | Some("") => Ok(0),
        Some(n) => n.parse().map_err(|_| {
            DbError::Config(format!("Redis databases are numbers, such as 0, not {n:?}"))
        }),
    }
}

/// The client for `params`. Through a tunnel it connects to the tunnel.
pub fn client(params: &ConnectionParams) -> Result<Client, DbError> {
    let (host, port) = match params.tunnel {
        Some(tunnel) => (tunnel.ip().to_string(), tunnel.port()),
        None => (params.host.clone(), params.port),
    };
    let port = if port == 0 { 6379 } else { port };
    let mut redis = RedisConnectionInfo::default()
        .set_db(database(params)?)
        .set_protocol(ProtocolVersion::RESP3);
    if !params.username.is_empty() {
        redis = redis.set_username(&params.username);
    }
    if !params.password.is_empty() {
        redis = redis.set_password(&params.password);
    }
    let addr = match params.tls {
        // Redis doesn't negotiate: without a choice, plain.
        TlsMode::Disable | TlsMode::Prefer => ConnectionAddr::Tcp(host, port),
        TlsMode::Require | TlsMode::VerifyFull => ConnectionAddr::TcpTls {
            host,
            port,
            insecure: params.tls == TlsMode::Require,
            tls_params: None,
        },
    };
    let info = addr
        .into_connection_info()
        .map_err(db_error)?
        .set_redis_settings(redis);
    match (&params.tls, &params.ca_file) {
        (TlsMode::VerifyFull, Some(ca_file)) => {
            let root_cert = std::fs::read(ca_file)
                .map_err(|err| DbError::Config(format!("{ca_file}: {err}")))?;
            let certificates = TlsCertificates {
                client_tls: None,
                root_cert: Some(root_cert),
            };
            Client::build_with_tls(info, certificates).map_err(db_error)
        }
        _ => Client::open(info).map_err(db_error),
    }
}

/// A new connection of `client`, with no limit on how long a command may
/// take.
pub(crate) async fn connect(client: &Client) -> Result<MultiplexedConnection, DbError> {
    let config = AsyncConnectionConfig::new()
        .set_connection_timeout(Some(Duration::from_secs(10)))
        .set_response_timeout(None);
    client
        .get_multiplexed_async_connection_with_config(&config)
        .await
        .map_err(db_error)
}

/// Sends `command`, its words, and returns the reply.
pub(crate) async fn query(
    conn: &mut MultiplexedConnection,
    command: &[String],
) -> Result<Value, DbError> {
    let (name, args) = command
        .split_first()
        .ok_or_else(|| DbError::General("an empty command".into()))?;
    redis::cmd(name)
        .arg(args)
        .query_async(conn)
        .await
        .map_err(db_error)
}

pub struct RedisSession {
    conn: MultiplexedConnection,
    catalog: RedisCatalog,
    read_only: bool,
    /// Whether the server marks each command `readonly`, as asked.
    readonly: Arc<Mutex<HashMap<String, bool>>>,
}

pub async fn open(params: &ConnectionParams) -> Result<Box<dyn Session>, DbError> {
    let client = client(params)?;
    let mut conn = connect(&client).await?;
    query(&mut conn, &["PING".to_string()]).await?;
    // The catalog has a connection of its own, to SELECT any database.
    let catalog = RedisCatalog::new(connect(&client).await?, database(params)?);
    Ok(Box::new(RedisSession {
        conn,
        catalog,
        read_only: params.read_only,
        readonly: Arc::default(),
    }))
}

/// Whether `command` only reads, by [`READS`] or what the server says of it.
async fn reads_only(
    conn: &mut MultiplexedConnection,
    known: &Mutex<HashMap<String, bool>>,
    command: &[String],
) -> Result<bool, DbError> {
    let name = command[0].to_lowercase();
    let container = dfox_sql::redis::CONTAINERS.contains(&command[0].to_uppercase().as_str());
    if container {
        let sub = command.get(1).map(|w| w.to_lowercase()).unwrap_or_default();
        return Ok(READS.contains(&format!("{name} {sub}").as_str()));
    }
    if READS.contains(&name.as_str()) {
        return Ok(true);
    }
    if let Some(&reads) = lock(known).get(&name) {
        return Ok(reads);
    }
    // `COMMAND INFO name`: a list of [name, arity, flags, …] per command.
    let info = query(conn, &["COMMAND".into(), "INFO".into(), name.clone()]).await?;
    let reads = match info {
        Value::Array(commands) => match commands.first() {
            Some(Value::Array(fields)) => match fields.get(2) {
                Some(Value::Set(flags) | Value::Array(flags)) => flags
                    .iter()
                    .any(|flag| convert::text(flag).eq_ignore_ascii_case("readonly")),
                _ => false,
            },
            _ => false,
        },
        _ => false,
    };
    lock(known).insert(name, reads);
    Ok(reads)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The commands of `text`, a line each, as words.
fn commands(text: &str) -> Result<Vec<Vec<String>>, DbError> {
    let lines: Vec<&str> = text.split('\n').collect();
    dfox_sql::redis::statements(text)
        .into_iter()
        .map(|s| {
            dfox_sql::redis::words(lines[s.start.0])
                .map_err(|err| DbError::General(format!("line {}: {err}", s.start.0 + 1)))
        })
        .collect()
}

impl Session for RedisSession {
    fn caps(&self) -> Capabilities {
        Capabilities::MULTI_STATEMENT | Capabilities::ROW_COUNT_ESTIMATE
    }

    fn language(&self) -> QueryLanguage {
        QueryLanguage::RedisCmd
    }

    fn catalog(&mut self) -> &mut dyn Catalog {
        &mut self.catalog
    }

    fn run(&mut self, request: Request) -> BoxFuture<'_, Result<ResultStream, DbError>> {
        let mut conn = self.conn.clone();
        let read_only = self.read_only;
        let known = self.readonly.clone();
        let limit = request.row_limit.unwrap_or(usize::MAX);
        Box::pin(async move {
            let commands = commands(&request.text)?;
            if read_only {
                for command in &commands {
                    if !reads_only(&mut conn, &known, command).await? {
                        return Err(DbError::General(format!(
                            "this connection is read-only: {} may write",
                            command[0].to_uppercase()
                        )));
                    }
                }
            }
            Ok(spawn_run(move |tx| async move {
                for command in commands {
                    let value = query(&mut conn, &command).await?;
                    send_table(convert::table(&command, value), limit, &tx).await?;
                }
                Ok(())
            }))
        })
    }

    fn cancel_handle(&self) -> CancelHandle {
        // shortcut: Redis runs one command at a time and a client can't stop
        // it short of killing the connection; blocking commands such as
        // BLPOP 0 wait until they are answered.
        CancelHandle::unsupported()
    }

    fn apply(&mut self, _changes: ChangeSet) -> BoxFuture<'_, Result<ApplyReport, DbError>> {
        Box::pin(async {
            Err(DbError::General(
                "editing values isn't supported yet".into(),
            ))
        })
    }
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

async fn send(tx: &mpsc::Sender<Result<Frame, DbError>>, frame: Frame) -> Result<(), DbError> {
    tx.send(Ok(frame)).await.map_err(|_| DbError::Cancelled)
}

/// Sends `table` as one result set, at most `limit` rows of it.
pub(crate) async fn send_table(
    table: Table,
    limit: usize,
    tx: &mpsc::Sender<Result<Frame, DbError>>,
) -> Result<(), DbError> {
    let schema = table.schema();
    send(tx, Frame::Schema(schema.clone())).await?;
    let rows = &table.rows[..table.rows.len().min(limit)];
    for chunk in rows.chunks(BATCH_ROWS) {
        send(tx, Frame::Batch(table.batch(&schema, chunk)?)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_is_a_command_a_line() {
        let all =
            commands("# warm up\nSET greeting \"hello there\"\n\nGET greeting").expect("read");
        assert_eq!(
            all,
            [
                vec!["SET".to_string(), "greeting".into(), "hello there".into()],
                vec!["GET".to_string(), "greeting".into()],
            ]
        );
        let err = commands("GET a\nGET \"open").unwrap_err();
        assert!(err.to_string().contains("line 2"), "{err}");
    }

    #[test]
    fn databases_are_numbers() {
        let params = |database: Option<&str>| ConnectionParams {
            database: database.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(database(&params(None)).ok(), Some(0));
        assert_eq!(database(&params(Some("3"))).ok(), Some(3));
        assert!(database(&params(Some("cache"))).is_err());
    }
}
