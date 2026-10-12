//! Shared helpers for the integration tests.
//!
//! Postgres and MySQL tests read their server from `DFOX_TEST_POSTGRES_URL` /
//! `DFOX_TEST_MYSQL_URL` and are skipped when it is unset; `make test-it`
//! starts both servers and sets the variables. With `DFOX_IT_REQUIRED=1`
//! (set in CI) a missing variable fails the test instead.

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};

use dfox_core::db::QueryOutput;

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

/// The rows of `output` with borrowed cells, for compact assertions.
pub fn rows(output: &QueryOutput) -> Vec<Vec<Option<&str>>> {
    output
        .rows
        .iter()
        .map(|row| row.iter().map(Option::as_deref).collect())
        .collect()
}

/// The single row of `output`.
pub fn only_row(output: &QueryOutput) -> Vec<Option<&str>> {
    let mut all = rows(output);
    assert_eq!(all.len(), 1, "expected one row, got {all:?}");
    all.remove(0)
}

/// The single value of `output`.
pub fn scalar(output: &QueryOutput) -> Option<&str> {
    let row = only_row(output);
    assert_eq!(row.len(), 1, "expected one column, got {row:?}");
    row[0]
}
