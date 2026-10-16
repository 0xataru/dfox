//! The native MySQL session: `mysql_async`, decoding rows straight into
//! Arrow batches.
//!
//! A session holds two connections: one pinned for what the user runs, so
//! transactions, variables and temporary tables carry over, and one for the
//! catalog and edits, so browsing never waits behind a long query. What the
//! user runs goes over the text protocol in one piece, and the server splits
//! it; browsing uses prepared statements, with values as parameters.

mod catalog;
pub mod decode;

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use arrow_array::RecordBatch;
use dfox_core::{
    arrow::{schema, Frame, ResultStream},
    catalog::Catalog,
    errors::DbError,
    models::connections::{ConnectionParams, TlsMode},
    session::{
        ApplyReport, CancelHandle, Capabilities, ChangeSet, QueryLanguage, Request, Session,
        SqlDialect,
    },
};
use futures_util::{future::BoxFuture, stream};
use mysql_async::{
    prelude::Queryable, BinaryProtocol, Conn, DriverError, Opts, OptsBuilder, QueryResult, Row,
    SslOpts, TextProtocol,
};
use tokio::sync::{mpsc, Mutex};

use self::{catalog::MyCatalog, decode::Column};

/// Rows per batch.
const BATCH_ROWS: usize = 8192;
/// A batch goes out at most this long after its first row, so the first
/// rows show quickly even when the rest are slow.
const BATCH_INTERVAL: Duration = Duration::from_millis(16);
/// How long connecting may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The server's code for a statement stopped by `KILL QUERY`.
const ER_QUERY_INTERRUPTED: u16 = 1317;

/// Connects a native session with `params`.
pub async fn open(params: &ConnectionParams) -> Result<Box<dyn Session>, DbError> {
    let opts = opts(params)?;
    let (conn, opts) = connect(opts, params.tls).await?;
    let meta = connect(OptsBuilder::from_opts(opts.clone()), params.tls)
        .await?
        .0;
    Ok(Box::new(MySession {
        id: conn.id(),
        conn: Arc::new(Mutex::new(conn)),
        catalog: MyCatalog::new(Arc::new(Mutex::new(meta))),
        opts,
        cancelled: Arc::new(AtomicBool::new(false)),
    }))
}

/// The options for `params`. Through a tunnel, it connects to the tunnel's
/// end but checks TLS against the database's own host name.
fn opts(params: &ConnectionParams) -> Result<OptsBuilder, DbError> {
    let (host, port) = match params.tunnel {
        Some(tunnel) => (tunnel.ip().to_string(), tunnel.port()),
        None => (params.host.clone(), params.port),
    };
    let mut init = Vec::new();
    if params.read_only {
        init.push("SET SESSION TRANSACTION READ ONLY".to_string());
    }
    let ssl = match params.tls {
        TlsMode::Disable => None,
        // Encryption without checking the certificate, as libpq's require.
        TlsMode::Prefer | TlsMode::Require => Some(
            SslOpts::default()
                .with_danger_accept_invalid_certs(true)
                .with_danger_skip_domain_validation(true),
        ),
        TlsMode::VerifyFull => Some(
            SslOpts::default()
                .with_root_certs(
                    roots(params.ca_file.as_deref())?
                        .into_iter()
                        .map(Into::into)
                        .collect(),
                )
                .with_disable_built_in_roots(true),
        ),
    }
    .map(|ssl| match params.tunnel {
        Some(_) => ssl.with_danger_tls_hostname_override(Some(params.host.clone())),
        None => ssl,
    });
    Ok(OptsBuilder::default()
        .ip_or_hostname(host)
        .tcp_port(port)
        .user(Some(&params.username))
        .pass(Some(&params.password))
        .db_name(params.database.clone())
        // It would move to a Unix socket on localhost, past the tunnel and TLS.
        .prefer_socket(false)
        .init(init)
        .ssl_opts(ssl))
}

/// The system's trusted roots (DER), plus the certificates in `ca_file`
/// (PEM).
fn roots(ca_file: Option<&str>) -> Result<Vec<Vec<u8>>, DbError> {
    let mut roots: Vec<Vec<u8>> = rustls_native_certs::load_native_certs()
        .certs
        .into_iter()
        .map(|cert| cert.to_vec())
        .collect();
    if let Some(path) = ca_file {
        let pem = std::fs::read(path)
            .map_err(|err| DbError::Config(format!("can't read {path}: {err}")))?;
        if !String::from_utf8_lossy(&pem).contains("-----BEGIN CERTIFICATE-----") {
            return Err(DbError::Config(format!("{path} holds no certificates")));
        }
        roots.push(pem);
    }
    Ok(roots)
}

/// Connects with `opts`. `prefer` falls back to a plain connection when the
/// server has no TLS; the options that worked come back with the connection.
async fn connect(opts: OptsBuilder, tls: TlsMode) -> Result<(Conn, Opts), DbError> {
    let attempt = |opts: OptsBuilder| async move {
        let opts = Opts::from(opts);
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, Conn::new(opts.clone()))
            .await
            .map_err(|_| DbError::Connection("timed out connecting".into()))?;
        Ok::<_, DbError>((conn, opts))
    };
    match attempt(opts.clone()).await? {
        (Ok(conn), opts) => Ok((conn, opts)),
        (Err(mysql_async::Error::Driver(DriverError::NoClientSslFlagFromServer)), _)
            if tls == TlsMode::Prefer =>
        {
            log::info!("the MySQL server has no TLS; connecting without it");
            let (conn, opts) = attempt(opts.ssl_opts(None)).await?;
            Ok((conn.map_err(db_error)?, opts))
        }
        (Err(err), _) => Err(db_error(err)),
    }
}

/// A mysql_async error as a dfox error: the server's message as the `mysql`
/// client prints it, and stopped statements as [`DbError::Cancelled`].
pub(crate) fn db_error(err: mysql_async::Error) -> DbError {
    match err {
        mysql_async::Error::Server(err) if err.code == ER_QUERY_INTERRUPTED => DbError::Cancelled,
        mysql_async::Error::Server(err) => DbError::General(format!(
            "ERROR {} ({}): {}",
            err.code, err.state, err.message
        )),
        err => {
            // The cause is often a level down: why the TLS handshake failed.
            let mut message = err.to_string();
            let mut source = std::error::Error::source(&err);
            while let Some(cause) = source {
                message.push_str(&format!(": {cause}"));
                source = cause.source();
            }
            DbError::Connection(message)
        }
    }
}

/// A native MySQL session.
pub struct MySession {
    conn: Arc<Mutex<Conn>>,
    /// The pinned connection's id, for `KILL QUERY`.
    id: u32,
    catalog: MyCatalog,
    /// The options that connected, for the connection a cancel makes.
    opts: Opts,
    /// Set by a cancel, so a statement it stopped reports so: `SLEEP()`
    /// returns 1 when killed instead of failing.
    cancelled: Arc<AtomicBool>,
}

impl Session for MySession {
    fn caps(&self) -> Capabilities {
        Capabilities::TRANSACTIONS
            | Capabilities::EXPLAIN
            | Capabilities::CANCEL
            | Capabilities::EDIT_BY_PK
            | Capabilities::ROW_COUNT_ESTIMATE
            | Capabilities::MULTI_STATEMENT
    }

    fn language(&self) -> QueryLanguage {
        QueryLanguage::Sql(SqlDialect::MySql)
    }

    fn catalog(&mut self) -> &mut dyn Catalog {
        &mut self.catalog
    }

    fn run(&mut self, request: Request) -> BoxFuture<'_, Result<ResultStream, DbError>> {
        // The server splits the script itself, so procedure bodies with
        // their own semicolons need no DELIMITER. Comments alone are no
        // statement, and the server would call them an error.
        let empty = dfox_sql::statements(&request.text, dfox_sql::Dialect::MySql).is_empty();
        let sql = request.text;
        let conn = self.conn.clone();
        let cancelled = self.cancelled.clone();
        let limit = request.row_limit.unwrap_or(usize::MAX);
        Box::pin(async move {
            Ok(spawn_run(move |tx| async move {
                if empty {
                    return Ok(());
                }
                let mut conn = conn.lock().await;
                cancelled.store(false, Ordering::SeqCst);
                let result = match conn.query_iter(sql).await {
                    Ok(result) => stream_result(result, limit, &tx).await,
                    Err(err) => Err(db_error(err)),
                };
                if cancelled.swap(false, Ordering::SeqCst) {
                    return Err(DbError::Cancelled);
                }
                result?;
                send_warnings(&mut conn, &tx).await
            }))
        })
    }

    fn cancel_handle(&self) -> CancelHandle {
        let opts = self.opts.clone();
        let id = self.id;
        let cancelled = self.cancelled.clone();
        CancelHandle::new(move || {
            let opts = opts.clone();
            let cancelled = cancelled.clone();
            Box::pin(async move {
                // The session's connections may both be busy; a fresh one
                // asks the server to stop. The id is a number the server
                // gave, so it can go into the SQL.
                let mut conn = Conn::new(opts).await.map_err(db_error)?;
                cancelled.store(true, Ordering::SeqCst);
                let killed = conn.query_drop(format!("KILL QUERY {id}")).await;
                let _ = conn.disconnect().await;
                killed.map_err(db_error)?;
                Ok(true)
            })
        })
    }

    fn apply(&mut self, changes: ChangeSet) -> BoxFuture<'_, Result<ApplyReport, DbError>> {
        Box::pin(self.catalog.apply(changes))
    }
}

/// Runs `work` on its own task, which sends frames as they are ready. The
/// stream ends after its last frame, or with its error; dropping the stream
/// stops the work at its next frame.
pub(crate) fn spawn_run<F, Fut>(work: F) -> ResultStream
where
    F: FnOnce(mpsc::Sender<Result<Frame, DbError>>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), DbError>> + Send + 'static,
{
    let (tx, rx) = mpsc::channel(4);
    let errors = tx.clone();
    tokio::spawn(async move {
        if let Err(err) = work(tx).await {
            // Dropped streams report Cancelled; nobody is listening then.
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

/// Sends the warnings of the run's last statement as notices; the server
/// keeps no others.
async fn send_warnings(
    conn: &mut Conn,
    tx: &mpsc::Sender<Result<Frame, DbError>>,
) -> Result<(), DbError> {
    if conn.get_warnings() == 0 {
        return Ok(());
    }
    let warnings: Vec<(String, u32, String)> =
        conn.query("SHOW WARNINGS").await.map_err(db_error)?;
    for (level, code, message) in warnings {
        send(tx, Frame::Notice(format!("{level} {code}: {message}"))).await?;
    }
    Ok(())
}

/// The rows of a statement's result, in either protocol.
pub(crate) trait Rows: Send {
    fn columns(&self) -> Option<Arc<[mysql_async::Column]>>;
    fn next_row(&mut self) -> impl Future<Output = mysql_async::Result<Option<Row>>> + Send;
    fn more(&self) -> bool;
    fn affected(&self) -> u64;
}

macro_rules! rows {
    ($protocol:ty) => {
        impl Rows for QueryResult<'_, '_, $protocol> {
            fn columns(&self) -> Option<Arc<[mysql_async::Column]>> {
                QueryResult::columns(self)
            }

            fn next_row(
                &mut self,
            ) -> impl Future<Output = mysql_async::Result<Option<Row>>> + Send {
                self.next()
            }

            fn more(&self) -> bool {
                !self.is_empty()
            }

            fn affected(&self) -> u64 {
                self.affected_rows()
            }
        }
    };
}

rows!(TextProtocol);
rows!(BinaryProtocol);

/// Streams every result set of a statement: rows as batches, and what a
/// statement without rows changed. A procedure can return several.
pub(crate) async fn stream_result(
    mut result: impl Rows,
    limit: usize,
    tx: &mpsc::Sender<Result<Frame, DbError>>,
) -> Result<(), DbError> {
    loop {
        let columns = result.columns().unwrap_or_else(|| Arc::from([]));
        if columns.is_empty() {
            let affected = result.affected();
            // Moves past the statement's OK packet.
            result.next_row().await.map_err(db_error)?;
            send(tx, Frame::Affected(affected)).await?;
        } else {
            stream_rows(&mut result, &columns, limit, tx).await?;
        }
        if !result.more() {
            // A later statement's error can still be waiting even though no
            // result set is: reading once more brings it up.
            result.next_row().await.map_err(db_error)?;
            return Ok(());
        }
    }
}

/// Streams the rows of the current result set.
async fn stream_rows(
    result: &mut impl Rows,
    columns: &[mysql_async::Column],
    limit: usize,
    tx: &mpsc::Sender<Result<Frame, DbError>>,
) -> Result<(), DbError> {
    let (fields, mut builders): (Vec<_>, Vec<Column>) = columns.iter().map(decode::column).unzip();
    let schema = schema(fields);
    send(tx, Frame::Schema(schema.clone())).await?;
    let mut kept = 0;
    let mut in_batch = 0;
    let mut started = Instant::now();
    while let Some(row) = result.next_row().await.map_err(db_error)? {
        // Past the limit, rows are read and dropped so the statement ends.
        if kept == limit {
            continue;
        }
        if in_batch == 0 {
            started = Instant::now();
        }
        for (i, builder) in builders.iter_mut().enumerate() {
            builder.push(row.as_ref(i))?;
        }
        kept += 1;
        in_batch += 1;
        if in_batch == BATCH_ROWS || (in_batch % 256 == 0 && started.elapsed() > BATCH_INTERVAL) {
            flush(&schema, &mut builders, tx).await?;
            in_batch = 0;
        }
    }
    if in_batch > 0 {
        flush(&schema, &mut builders, tx).await?;
    }
    Ok(())
}

async fn flush(
    schema: &arrow_schema::SchemaRef,
    columns: &mut [Column],
    tx: &mpsc::Sender<Result<Frame, DbError>>,
) -> Result<(), DbError> {
    let arrays = columns.iter_mut().map(Column::finish).collect();
    let batch = RecordBatch::try_new(schema.clone(), arrays)
        .map_err(|err| DbError::General(format!("building a batch: {err}")))?;
    send(tx, Frame::Batch(batch)).await
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use super::*;

    #[test]
    fn a_tunnel_replaces_the_address_but_keeps_the_host_name_for_tls() {
        let params = ConnectionParams {
            host: "db.internal".into(),
            port: 3306,
            tls: TlsMode::Require,
            ..Default::default()
        };
        let direct = Opts::from(opts(&params).expect("opts"));
        assert_eq!(direct.ip_or_hostname(), "db.internal");
        assert_eq!(direct.tcp_port(), 3306);
        assert!(!direct.prefer_socket());

        let through = Opts::from(
            opts(&ConnectionParams {
                tunnel: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 40000))),
                ..params
            })
            .expect("opts"),
        );
        assert_eq!(through.ip_or_hostname(), "127.0.0.1");
        assert_eq!(through.tcp_port(), 40000);
        let ssl = through.ssl_opts().expect("tls");
        assert_eq!(ssl.tls_hostname_override(), Some("db.internal"));
    }

    #[test]
    fn modes_choose_tls_options() {
        let with = |tls| {
            Opts::from(
                opts(&ConnectionParams {
                    tls,
                    ..Default::default()
                })
                .expect("opts"),
            )
        };
        assert!(with(TlsMode::Disable).ssl_opts().is_none());
        let require = with(TlsMode::Require);
        let require = require.ssl_opts().expect("tls");
        assert!(require.accept_invalid_certs());
        let verify = with(TlsMode::VerifyFull);
        let verify = verify.ssl_opts().expect("tls");
        assert!(!verify.accept_invalid_certs());
        assert!(!verify.skip_domain_validation());
        assert!(verify.disable_built_in_roots());
    }

    #[test]
    fn a_missing_or_empty_ca_file_is_an_error() {
        let err = roots(Some("/nonexistent/ca.pem")).expect_err("an error");
        assert!(err.to_string().contains("can't read"), "{err}");
        let empty =
            std::env::temp_dir().join(format!("dfox-my-empty-ca-{}.pem", std::process::id()));
        std::fs::write(&empty, "").expect("write");
        let err = roots(empty.to_str()).expect_err("an error");
        assert!(err.to_string().contains("no certificates"), "{err}");
        let _ = std::fs::remove_file(&empty);
    }
}
