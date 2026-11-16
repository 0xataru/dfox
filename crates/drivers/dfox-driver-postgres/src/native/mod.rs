//! The native Postgres session: `tokio-postgres` on the binary protocol,
//! decoding rows straight into Arrow batches.
//!
//! A session holds two connections: one pinned for what the user runs, so
//! `BEGIN`, `SET` and temporary tables carry over, and one for the catalog
//! and edits, so browsing never waits behind a long query.

mod catalog;
pub mod decode;
mod tls;

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use arrow_array::RecordBatch;
use dfox_core::{
    arrow::{schema, Frame, ResultStream},
    catalog::Catalog,
    errors::DbError,
    models::connections::ConnectionParams,
    session::{
        ApplyReport, CancelHandle, Capabilities, ChangeSet, QueryLanguage, Request, Session,
        SqlDialect,
    },
};
use futures_util::{future::BoxFuture, stream, StreamExt, TryStreamExt};
use tokio::sync::mpsc;
use tokio_postgres::{
    types::{FromSql, ToSql, Type},
    AsyncMessage, CancelToken, Client, Config,
};

use self::{catalog::PgCatalog, decode::Column, tls::Tls};

/// Rows per batch.
const BATCH_ROWS: usize = 8192;
/// A batch goes out at most this long after its first row, so the first
/// rows show quickly even when the rest are slow.
const BATCH_INTERVAL: Duration = Duration::from_millis(16);

/// Connects a native session with `params`.
pub async fn open(params: &ConnectionParams) -> Result<Box<dyn Session>, DbError> {
    Ok(Box::new(connect(params, false).await?))
}

/// Connects; `each` makes every statement run read-only on its own, as when
/// the server didn't take read-only for the session.
async fn connect(params: &ConnectionParams, each: bool) -> Result<PgSession, DbError> {
    let tls = Tls::new(params)?;
    let mut config = config(params);
    let notices = Arc::new(Mutex::new(Vec::new()));
    let client = match tls.connect(&config, Some(notices.clone())).await {
        // A pooler such as pgbouncer may refuse the startup `options` that
        // make the session read-only: then without, read-only per statement.
        Err(err)
            if params.read_only && err.to_string().contains("unsupported startup parameter") =>
        {
            config = self::config(&ConnectionParams {
                read_only: false,
                ..params.clone()
            });
            tls.connect(&config, Some(notices.clone())).await?
        }
        other => other?,
    };
    let meta = tls.connect(&config, None).await?;
    // A pooler may also drop the setting without a word: ask the server.
    let read_only_each = each || (params.read_only && !session_read_only(&client).await);
    if read_only_each {
        lock(&notices).push(
            "Read-only is enforced statement by statement: the server, or a pooler such as \
             pgbouncer, didn't take it for the session"
                .to_string(),
        );
    }
    let cancel = client.cancel_token();
    let database = params
        .database
        .clone()
        .unwrap_or_else(|| params.username.clone());
    Ok(PgSession {
        client: Arc::new(client),
        catalog: PgCatalog::new(Arc::new(meta), database),
        cancel,
        tls,
        notices,
        read_only_each,
    })
}

/// Whether the server holds the session read-only.
async fn session_read_only(client: &Client) -> bool {
    let Ok(messages) = client
        .simple_query("SHOW default_transaction_read_only")
        .await
    else {
        return false;
    };
    messages.iter().any(|m| match m {
        tokio_postgres::SimpleQueryMessage::Row(row) => row.get(0) == Some("on"),
        _ => false,
    })
}

fn config(params: &ConnectionParams) -> Config {
    // Through a tunnel: connect to its local end, verify TLS for the host.
    let port = params.tunnel.map_or(params.port, |tunnel| tunnel.port());
    let mut config = Config::new();
    config
        .host(&params.host)
        .port(port)
        .user(&params.username)
        .password(&params.password)
        .application_name("dfox")
        .ssl_mode(tls::ssl_mode(params.tls))
        .connect_timeout(Duration::from_secs(10));
    if let Some(database) = &params.database {
        config.dbname(database);
    }
    if let Some(tunnel) = params.tunnel {
        config.hostaddr(tunnel.ip());
    }
    if params.read_only {
        config.options("-c default_transaction_read_only=on");
    }
    config
}

/// Drives a connection until it closes, keeping notices in `notices` when
/// given.
pub(crate) async fn drive<S, T>(
    mut connection: tokio_postgres::Connection<S, T>,
    notices: Option<Arc<Mutex<Vec<String>>>>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut messages = stream::poll_fn(move |cx| connection.poll_message(cx));
    while let Some(message) = messages.next().await {
        match message {
            Ok(AsyncMessage::Notice(notice)) => {
                if let Some(notices) = &notices {
                    lock(notices).push(format!("{}: {}", notice.severity(), notice.message()));
                }
            }
            Ok(_) => {}
            Err(err) => {
                log::warn!("postgres connection: {err}");
                break;
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A tokio-postgres error as a dfox error: the server's message with its
/// detail and hint, and cancelled queries as [`DbError::Cancelled`].
pub(crate) fn db_error(err: tokio_postgres::Error) -> DbError {
    let Some(db) = err.as_db_error() else {
        // The cause is often a level down: why the TLS handshake failed.
        let mut message = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(cause) = source {
            message.push_str(&format!(": {cause}"));
            source = cause.source();
        }
        return DbError::Connection(message);
    };
    if db.code() == &tokio_postgres::error::SqlState::QUERY_CANCELED {
        return DbError::Cancelled;
    }
    let mut message = format!("{}: {}", db.severity(), db.message());
    if let Some(detail) = db.detail() {
        message.push_str(&format!("\nDETAIL: {detail}"));
    }
    if let Some(hint) = db.hint() {
        message.push_str(&format!("\nHINT: {hint}"));
    }
    match db.position() {
        // 1-based, in characters.
        Some(tokio_postgres::error::ErrorPosition::Original(at)) => DbError::At {
            message,
            at: (*at as usize).saturating_sub(1),
        },
        _ => DbError::General(message),
    }
}

/// A value's bytes, whatever its type: the decoder reads them.
struct Raw<'a>(&'a [u8]);

impl<'a> FromSql<'a> for Raw<'a> {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Raw(raw))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

/// A native Postgres session.
pub struct PgSession {
    client: Arc<Client>,
    catalog: PgCatalog,
    cancel: CancelToken,
    tls: Tls,
    notices: Arc<Mutex<Vec<String>>>,
    /// Read-only but not held so by the server: each statement runs in a
    /// read-only transaction of its own, which a pooler keeps on one server
    /// connection, so the server refuses writes all the same.
    read_only_each: bool,
}

impl Session for PgSession {
    fn caps(&self) -> Capabilities {
        Capabilities::TRANSACTIONS
            | Capabilities::SCHEMAS
            | Capabilities::EXPLAIN
            | Capabilities::CANCEL
            | Capabilities::EDIT_BY_PK
            | Capabilities::ROW_COUNT_ESTIMATE
            | Capabilities::MULTI_STATEMENT
    }

    fn language(&self) -> QueryLanguage {
        QueryLanguage::Sql(SqlDialect::Postgres)
    }

    fn catalog(&mut self) -> &mut dyn Catalog {
        &mut self.catalog
    }

    fn run(&mut self, request: Request) -> BoxFuture<'_, Result<ResultStream, DbError>> {
        let statements: Vec<(usize, String)> = {
            let lines: Vec<String> = request.text.split('\n').map(str::to_string).collect();
            dfox_sql::statements(&request.text, dfox_sql::Dialect::Postgres)
                .into_iter()
                .map(|s| {
                    (
                        dfox_sql::offset(&lines, s.start),
                        dfox_sql::slice(&lines, s.start, s.end),
                    )
                })
                .collect()
        };
        let client = self.client.clone();
        let notices = self.notices.clone();
        let limit = request.row_limit.unwrap_or(usize::MAX);
        let each = self.read_only_each;
        Box::pin(async move {
            Ok(spawn_run(move |tx| async move {
                for (start, sql) in statements {
                    let start = start + sql.chars().take_while(|c| c.is_whitespace()).count();
                    let sql = sql.trim().trim_end_matches(';');
                    if each {
                        client
                            .batch_execute("BEGIN READ ONLY")
                            .await
                            .map_err(db_error)?;
                    }
                    let ran = execute(&client, sql, &[], limit, &tx)
                        .await
                        .map_err(|err| err.shifted(start));
                    if each {
                        let end = if ran.is_ok() { "COMMIT" } else { "ROLLBACK" };
                        client.batch_execute(end).await.map_err(db_error)?;
                    }
                    ran?;
                    send_notices(&notices, &tx).await?;
                }
                Ok(())
            }))
        })
    }

    fn cancel_handle(&self) -> CancelHandle {
        let cancel = self.cancel.clone();
        let tls = self.tls.clone();
        CancelHandle::new(move || {
            let cancel = cancel.clone();
            let tls = tls.clone();
            Box::pin(async move {
                tls.cancel(&cancel).await?;
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
    Fut: std::future::Future<Output = Result<(), DbError>> + Send + 'static,
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

async fn send_notices(
    notices: &Mutex<Vec<String>>,
    tx: &mpsc::Sender<Result<Frame, DbError>>,
) -> Result<(), DbError> {
    let pending: Vec<String> = std::mem::take(&mut *lock(notices));
    for notice in pending {
        send(tx, Frame::Notice(notice)).await?;
    }
    Ok(())
}

/// Runs one statement and streams its rows. `params` are text, cast by the
/// SQL where needed (`$1::text`); `None` is NULL.
pub(crate) async fn execute(
    client: &Client,
    sql: &str,
    params: &[Option<String>],
    limit: usize,
    tx: &mpsc::Sender<Result<Frame, DbError>>,
) -> Result<(), DbError> {
    let statement = client.prepare(sql).await.map_err(db_error)?;
    let params: Vec<&(dyn ToSql + Sync)> =
        params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    if statement.columns().is_empty() {
        let affected = client
            .execute(&statement, &params)
            .await
            .map_err(db_error)?;
        return send(tx, Frame::Affected(affected)).await;
    }

    let (fields, mut columns): (Vec<_>, Vec<Column>) = statement
        .columns()
        .iter()
        .map(|c| decode::column(c.name(), c.type_()))
        .unzip();
    let schema = schema(fields);
    send(tx, Frame::Schema(schema.clone())).await?;

    let rows = client
        .query_raw(&statement, params.iter().copied())
        .await
        .map_err(db_error)?;
    futures_util::pin_mut!(rows);
    let mut kept = 0;
    let mut in_batch = 0;
    let mut started = Instant::now();
    while let Some(row) = rows.try_next().await.map_err(db_error)? {
        // Past the limit, rows are read and dropped so the statement ends.
        if kept == limit {
            continue;
        }
        if in_batch == 0 {
            started = Instant::now();
        }
        for (i, column) in columns.iter_mut().enumerate() {
            let raw: Option<Raw> = row.try_get(i).map_err(db_error)?;
            column.push(raw.map(|r| r.0))?;
        }
        kept += 1;
        in_batch += 1;
        if in_batch == BATCH_ROWS || (in_batch % 256 == 0 && started.elapsed() > BATCH_INTERVAL) {
            flush(&schema, &mut columns, tx).await?;
            in_batch = 0;
        }
    }
    if in_batch > 0 {
        flush(&schema, &mut columns, tx).await?;
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
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use tokio_postgres::config::Host;

    use super::*;

    #[test]
    fn a_tunnel_replaces_the_address_but_keeps_the_host_name_for_tls() {
        let params = ConnectionParams {
            host: "db.internal".into(),
            port: 5432,
            ..Default::default()
        };
        let direct = config(&params);
        assert_eq!(direct.get_ports(), [5432]);
        assert!(direct.get_hostaddrs().is_empty());

        let through = config(&ConnectionParams {
            tunnel: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 40000))),
            ..params
        });
        assert_eq!(through.get_ports(), [40000]);
        assert_eq!(through.get_hostaddrs(), [IpAddr::from(Ipv4Addr::LOCALHOST)]);
        assert!(matches!(through.get_hosts(), [Host::Tcp(host)] if host == "db.internal"));
    }

    /// Against the test server, as when a pooler dropped the session
    /// setting: writes are refused by the server, reads run.
    #[tokio::test]
    async fn read_only_each_statement_is_held_by_the_server() {
        let Some(url) = dfox_testkit::server_url("DFOX_TEST_POSTGRES_URL") else {
            return;
        };
        let params = dfox_testkit::params_from_url(&url);
        let mut session: Box<dyn Session> =
            Box::new(connect(&params, true).await.expect("connect"));
        let table = dfox_testkit::unique("dfox_ro_each");
        let write = dfox_testkit::run(
            session.as_mut(),
            &format!("CREATE TABLE {table} (id int)"),
            10,
        )
        .await;
        let err = write.expect_err("refused").to_string();
        assert!(err.contains("read-only"), "{err}");
        let read = dfox_testkit::run(session.as_mut(), "SELECT 1 AS one", 10)
            .await
            .expect("read");
        assert_eq!(read.rows, [[Some("1".to_string())]]);
    }
}
