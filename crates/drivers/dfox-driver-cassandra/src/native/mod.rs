//! A Cassandra or ScyllaDB session on the `scylla` driver. A script runs one
//! CQL statement at a time; SELECTs are paged, so a big table streams in
//! batches, and the rest run whole.

pub mod catalog;
pub mod convert;

use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use dfox_core::{
    arrow::{Frame, ResultStream},
    catalog::Catalog,
    errors::DbError,
    models::connections::{ConnectionParams, TlsMode},
    session::{
        ApplyReport, CancelHandle, Capabilities, Change, ChangeSet, QueryLanguage, Request, Session,
    },
};
use futures_util::{future::BoxFuture, stream, StreamExt};
use scylla::{
    client::{
        execution_profile::ExecutionProfile, session::Session as Scylla,
        session_builder::SessionBuilder,
    },
    errors::TranslationError,
    frame::types::Consistency,
    policies::address_translator::{AddressTranslator, UntranslatedPeer},
    statement::Statement,
    value::Row,
};
use tokio::sync::mpsc;

use self::{catalog::CqlCatalog, convert::Columns};

/// Rows in one batch, at most.
const BATCH_ROWS: usize = 4096;
/// A batch goes out after this long even when not full.
const BATCH_INTERVAL: Duration = Duration::from_millis(16);
/// Rows fetched from the server at a time.
const PAGE_SIZE: i32 = 5000;
/// The keyspace a session starts in when none is given: one every cluster
/// has.
pub const DEFAULT_KEYSPACE: &str = "system";

/// Statements that change data or schema, refused on a read-only
/// connection: Cassandra has no read-only sessions.
const WRITES: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "CREATE", "ALTER", "DROP", "TRUNCATE", "BATCH", "BEGIN", "APPLY",
    "GRANT", "REVOKE",
];

/// A syntax error with the place CQL gives it, `line 2:7 …` (the column
/// from 0), as a [`DbError::At`] into the statement of `lines`.
fn located(err: DbError, lines: &[String]) -> DbError {
    let DbError::General(message) = err else {
        return err;
    };
    let place = message.split("line ").skip(1).find_map(|rest| {
        let (line, rest) = rest.split_once(':')?;
        let col: String = rest.chars().take_while(char::is_ascii_digit).collect();
        Some((
            line.parse::<usize>().ok()?.checked_sub(1)?,
            col.parse::<usize>().ok()?,
        ))
    });
    match place.filter(|(line, _)| *line < lines.len()) {
        Some(pos) => DbError::At {
            at: dfox_sql::offset(lines, pos),
            message,
        },
        None => DbError::General(message),
    }
}

pub(crate) fn db_error(err: impl std::fmt::Display) -> DbError {
    DbError::General(err.to_string())
}

pub struct CqlSession {
    session: Arc<Scylla>,
    catalog: CqlCatalog,
    read_only: bool,
}

pub async fn open(params: &ConnectionParams) -> Result<Box<dyn Session>, DbError> {
    let address = match params.tunnel {
        Some(tunnel) => tunnel,
        None => {
            let port = if params.port == 0 { 9042 } else { params.port };
            tokio::net::lookup_host((params.host.as_str(), port))
                .await
                .map_err(|err| DbError::Connection(format!("{}: {err}", params.host)))?
                .next()
                .ok_or_else(|| DbError::Connection(format!("{} has no address", params.host)))?
        }
    };
    // Nodes tell each other's addresses as the cluster sees them, which
    // Docker, NAT or a tunnel make unreachable from here: every connection
    // goes to the address given, and that node coordinates.
    // One replica answers, as in cqlsh: a client shows what a node has, and
    // keeps working on a single node or with replicas down.
    let profile = ExecutionProfile::builder()
        .consistency(Consistency::One)
        .build()
        .into_handle();
    let builder = SessionBuilder::new()
        .default_execution_profile_handle(profile)
        .connection_timeout(Duration::from_secs(10))
        .known_node_addr(address)
        .address_translator(Arc::new(Only(address)));
    let mut builder = builder;
    if !params.username.is_empty() {
        builder = builder.user(&params.username, &params.password);
    }
    // Cassandra doesn't negotiate TLS, so `prefer` connects plain.
    if matches!(params.tls, TlsMode::Require | TlsMode::VerifyFull) {
        return Err(DbError::Config(
            "dfox can't encrypt Cassandra connections yet: set TLS to prefer or disable".into(),
        ));
    }
    let session = builder
        .build()
        .await
        .map_err(|err| DbError::Connection(err.to_string()))?;
    let keyspace = params
        .database
        .clone()
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| DEFAULT_KEYSPACE.to_string());
    session
        .use_keyspace(&keyspace, true)
        .await
        .map_err(|err| DbError::Connection(format!("keyspace {keyspace}: {err}")))?;
    let session = Arc::new(session);
    Ok(Box::new(CqlSession {
        catalog: CqlCatalog::new(session.clone()),
        session,
        read_only: params.read_only,
    }))
}

/// Sends every node's connections to one address.
#[derive(Debug)]
struct Only(SocketAddr);

impl AddressTranslator for Only {
    fn translate_address<'a, 'b, 'f>(
        &'a self,
        _peer: &'b UntranslatedPeer<'_>,
    ) -> Pin<Box<dyn Future<Output = Result<SocketAddr, TranslationError>> + Send + 'f>>
    where
        'a: 'f,
        'b: 'f,
        Self: 'f,
    {
        let address = self.0;
        Box::pin(async move { Ok(address) })
    }
}

/// The statement's first word, upper-cased.
fn verb(statement: &str) -> String {
    statement
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .find(|w| !w.is_empty())
        .unwrap_or_default()
        .to_uppercase()
}

impl Session for CqlSession {
    fn caps(&self) -> Capabilities {
        Capabilities::MULTI_STATEMENT
    }

    fn language(&self) -> QueryLanguage {
        QueryLanguage::Cql
    }

    fn catalog(&mut self) -> &mut dyn Catalog {
        &mut self.catalog
    }

    fn run(&mut self, request: Request) -> BoxFuture<'_, Result<ResultStream, DbError>> {
        let lines: Vec<String> = dfox_sql::lines(&request.text);
        let statements: Vec<(usize, String)> =
            dfox_sql::statements(&request.text, dfox_sql::Dialect::Cql)
                .iter()
                .map(|s| {
                    let text = dfox_sql::slice(&lines, s.start, s.end);
                    let start = dfox_sql::offset(&lines, s.start);
                    (start, text.trim_end().trim_end_matches(';').to_string())
                })
                .collect();
        let session = self.session.clone();
        let read_only = self.read_only;
        let limit = request.row_limit.unwrap_or(usize::MAX);
        Box::pin(async move {
            if read_only {
                if let Some((_, write)) = statements
                    .iter()
                    .find(|(_, s)| WRITES.contains(&verb(s).as_str()))
                {
                    return Err(DbError::General(format!(
                        "this connection is read-only: {} would write",
                        verb(write)
                    )));
                }
            }
            Ok(spawn_run(move |tx| async move {
                for (start, statement) in statements {
                    let lines: Vec<String> = dfox_sql::lines(&statement);
                    run_one(&session, statement, limit, &tx)
                        .await
                        .map_err(|err| located(err, &lines).shifted(start))?;
                }
                Ok(())
            }))
        })
    }

    fn cancel_handle(&self) -> CancelHandle {
        // CQL can't stop a statement; dropping the stream stops paging.
        CancelHandle::unsupported()
    }

    /// Adds rows, as an import or a copy brings them: `INSERT … JSON`, so
    /// the server reads each value as its column's type. Editing rows in
    /// place isn't supported.
    fn apply(&mut self, changes: ChangeSet) -> BoxFuture<'_, Result<ApplyReport, DbError>> {
        let session = self.session.clone();
        let read_only = self.read_only;
        Box::pin(async move {
            if read_only {
                return Err(DbError::General(
                    "this connection is read-only: adding rows would write".into(),
                ));
            }
            let [keyspace, table] = changes.table.parts() else {
                return Err(DbError::General(format!(
                    "not a table: {:?}",
                    changes.table
                )));
            };
            let quote = |name: &str| dfox_sql::quote_ident(name, dfox_sql::Dialect::Cql);
            let insert = session
                .prepare(format!(
                    "INSERT INTO {}.{} JSON ? DEFAULT UNSET",
                    quote(keyspace),
                    quote(table)
                ))
                .await
                .map_err(db_error)?;
            let rows: Vec<String> = changes
                .changes
                .iter()
                .map(|change| match change {
                    Change::Insert(values) => convert::row_json(values),
                    _ => Err(DbError::General(
                        "Cassandra rows can be added, not edited in place".into(),
                    )),
                })
                .collect::<Result<_, _>>()?;
            // Rows of different partitions go to different nodes: in
            // parallel, a bounded number at a time.
            for chunk in rows.chunks(64) {
                futures_util::future::try_join_all(
                    chunk
                        .iter()
                        .map(|row| session.execute_unpaged(&insert, (row,))),
                )
                .await
                .map_err(db_error)?;
            }
            Ok(ApplyReport {
                inserted: rows.len() as u64,
                ..ApplyReport::default()
            })
        })
    }
}

/// Runs one statement: a SELECT a page at a time, anything else whole.
async fn run_one(
    session: &Scylla,
    text: String,
    limit: usize,
    tx: &mpsc::Sender<Result<Frame, DbError>>,
) -> Result<(), DbError> {
    let mut statement = Statement::new(text.clone());
    statement.set_page_size(PAGE_SIZE);
    if verb(&text) == "SELECT" {
        let pager = session.query_iter(statement, &[]).await.map_err(db_error)?;
        let columns = Columns::new(pager.column_specs().as_slice());
        let rows = pager.rows_stream::<Row>().map_err(db_error)?;
        return send_rows(columns, rows.map(|r| r.map_err(db_error)), limit, tx).await;
    }
    let result = session
        .query_unpaged(statement, &[])
        .await
        .map_err(db_error)?;
    for warning in result.warnings() {
        send(tx, Frame::Notice(warning.to_string())).await?;
    }
    if !result.is_rows() {
        return send(tx, Frame::Affected(0)).await;
    }
    // `DESCRIBE`, `LIST`, and conditional writes answer with rows.
    let rows = result.into_rows_result().map_err(db_error)?;
    let columns = Columns::new(rows.column_specs().as_slice());
    let all: Vec<Result<Row, DbError>> = rows
        .rows::<Row>()
        .map_err(db_error)?
        .map(|r| r.map_err(db_error))
        .collect();
    send_rows(columns, stream::iter(all), limit, tx).await
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

/// Streams rows as one result set, at most `limit` of them.
pub(crate) async fn send_rows<S>(
    columns: Columns,
    mut rows: S,
    limit: usize,
    tx: &mpsc::Sender<Result<Frame, DbError>>,
) -> Result<(), DbError>
where
    S: futures_util::Stream<Item = Result<Row, DbError>> + Unpin,
{
    send(tx, Frame::Schema(columns.schema())).await?;
    let mut batch = Vec::new();
    let mut kept = 0;
    let mut since = Instant::now();
    while kept < limit {
        let Some(row) = rows.next().await else { break };
        batch.push(row?);
        kept += 1;
        if batch.len() >= BATCH_ROWS || since.elapsed() > BATCH_INTERVAL {
            send(tx, Frame::Batch(columns.batch(&batch)?)).await?;
            batch.clear();
            since = Instant::now();
        }
    }
    if !batch.is_empty() {
        send(tx, Frame::Batch(columns.batch(&batch)?)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_errors_are_placed_by_their_line_and_column() {
        let lines: Vec<String> = ["SELECT *", "FORM t"].map(String::from).to_vec();
        let err = DbError::General(
            "Database returned an error: The submitted query has a syntax error, \
             Error message: line 2:0 no viable alternative at input 'FORM'"
                .into(),
        );
        match located(err, &lines).shifted(10) {
            DbError::At { at, .. } => assert_eq!(at, 19),
            other => panic!("expected a place, got {other:?}"),
        }
    }

    #[test]
    fn statements_are_known_by_their_verb() {
        assert_eq!(verb("  select * from t"), "SELECT");
        assert_eq!(verb("INSERT INTO t (a) VALUES (1)"), "INSERT");
        assert_eq!(verb("BEGIN BATCH"), "BEGIN");
        assert!(WRITES.contains(&verb("truncate t").as_str()));
    }
}
