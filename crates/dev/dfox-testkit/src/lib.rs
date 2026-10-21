//! Shared helpers for the driver integration tests, the [`conformance`]
//! suite every driver's session must pass, and a [`mock`] driver that passes
//! it without a server.
//!
//! Postgres and MySQL tests read their server from `DFOX_TEST_POSTGRES_URL` /
//! `DFOX_TEST_MYSQL_URL` and are skipped when it is unset; `make test-it`
//! starts both servers and sets the variables. With `DFOX_IT_REQUIRED=1`
//! (set in CI) a missing variable fails the test instead.

pub mod conformance;
pub mod mock;

use std::sync::atomic::{AtomicUsize, Ordering};

use dfox_core::{
    arrow::{Fetcher, Overflow, DEFAULT_BUDGET},
    errors::DbError,
    models::connections::ConnectionParams,
    result::{Cell, ResultSet},
    session::{Request, Session},
};

/// Returns the server URL in `var`, or `None` when the test should be skipped.
pub fn server_url(var: &str) -> Option<String> {
    match std::env::var(var) {
        Ok(url) if !url.is_empty() => Some(url),
        _ if std::env::var("DFOX_IT_REQUIRED").as_deref() == Ok("1") => {
            panic!("{var} must be set when DFOX_IT_REQUIRED=1")
        }
        _ => {
            eprintln!("skipping: {var} is not set (run `make test-it`)");
            None
        }
    }
}

/// A name no other test in this run uses, for tables and roles that outlive
/// a session.
pub fn unique(prefix: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    format!(
        "{prefix}_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// What running a script on a session gave: the last result set as text,
/// and everything the statements changed or said.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    /// Column names of the last result set, in order, duplicates kept.
    pub columns: Vec<String>,
    /// Its rows; `None` is NULL.
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows changed by every statement, summed.
    pub rows_affected: u64,
    pub notices: Vec<String>,
}

/// Runs `sql` on `session`, keeping at most `row_limit` rows per result set.
pub async fn run(
    session: &mut dyn Session,
    sql: &str,
    row_limit: usize,
) -> Result<Output, DbError> {
    let stream = session
        .run(Request::new(sql).with_row_limit(row_limit))
        .await?;
    let mut fetcher = Fetcher::new(stream, DEFAULT_BUDGET, Overflow::Stop);
    fetcher.fetch().await?;
    let mut output = Output {
        rows_affected: fetcher.rows_affected(),
        notices: fetcher.notices().to_vec(),
        ..Default::default()
    };
    if let Some(result) = fetcher.result() {
        output.columns = result.columns().iter().map(|c| c.name.clone()).collect();
        output.rows = (0..result.row_count())
            .map(|row| {
                (0..result.columns().len())
                    .map(|col| match result.cell(row, col) {
                        Some(Cell::Null) | None => None,
                        Some(cell) => Some(cell.to_string()),
                    })
                    .collect()
            })
            .collect();
    }
    Ok(output)
}

/// Runs `sql` for its effect, failing the test if it fails.
pub async fn execute(session: &mut dyn Session, sql: &str) {
    run(session, sql, 0)
        .await
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
}

/// The rows of `output` with borrowed cells, for compact assertions.
pub fn rows(output: &Output) -> Vec<Vec<Option<&str>>> {
    output
        .rows
        .iter()
        .map(|row| row.iter().map(Option::as_deref).collect())
        .collect()
}

/// The single row of `output`.
pub fn only_row(output: &Output) -> Vec<Option<&str>> {
    let mut all = rows(output);
    assert_eq!(all.len(), 1, "expected one row, got {all:?}");
    all.remove(0)
}

/// The single value of `output`.
pub fn scalar(output: &Output) -> Option<&str> {
    let row = only_row(output);
    assert_eq!(row.len(), 1, "expected one column, got {row:?}");
    row[0]
}

/// Connection details from a test URL such as
/// `postgres://user:password@host:port/db`. The test URLs use plain
/// passwords, so no percent-decoding is needed.
pub fn params_from_url(url: &str) -> ConnectionParams {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let (credentials, location) = rest.rsplit_once('@').unwrap_or(("", rest));
    let (username, password) = credentials.split_once(':').unwrap_or((credentials, ""));
    let (address, database) = location.split_once('/').unwrap_or((location, ""));
    let (host, port) = address.rsplit_once(':').unwrap_or((address, "0"));
    ConnectionParams {
        host: host.to_string(),
        port: port.parse().expect("port"),
        username: username.to_string(),
        password: password.to_string(),
        database: (!database.is_empty()).then(|| database.to_string()),
        ..Default::default()
    }
}
