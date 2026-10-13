//! Shared code for the drivers built on sqlx: streaming rows in chunks,
//! replacing a lost session connection, and reading values. Goes away as the
//! drivers move to their native clients (core track C2 onwards).

use futures_util::TryStreamExt;
use sqlx::{pool::PoolConnection, Column, Database, Either, Executor, Pool, Row, TypeInfo};
use tokio::{sync::mpsc, time::Instant};

use super::{Chunk, StreamSummary, CHUNK_INTERVAL, CHUNK_ROWS};
use crate::{
    errors::DbError,
    result::{ColumnMeta, OwnedCell},
};

/// Runs raw SQL and sends its rows to `tx` in chunks. `rows_affected` reads
/// the count from the driver's statement result.
///
/// The SQL is passed as a plain `&str`, so sqlx uses the simple-query
/// protocol: several statements are allowed, and Postgres and MySQL send every
/// value in its text form, which `cell` reads without any type mapping that
/// could fail.
pub async fn stream_raw<'c, DB, E>(
    executor: E,
    sql: &str,
    row_limit: usize,
    cell: fn(&DB::Row, usize) -> OwnedCell,
    rows_affected: fn(&DB::QueryResult) -> u64,
    tx: &mpsc::Sender<Chunk>,
) -> Result<StreamSummary, DbError>
where
    DB: Database,
    E: Executor<'c, Database = DB>,
{
    let mut summary = StreamSummary::default();
    // A statement result ends one result set; the next row starts a new one.
    let mut new_result_set = true;
    let mut kept = 0;
    let mut batch = Vec::new();
    let mut deadline = Instant::now();
    let mut stream = executor.fetch_many(sql);

    loop {
        let next = if batch.is_empty() {
            stream.try_next().await?
        } else {
            match tokio::time::timeout_at(deadline, stream.try_next()).await {
                Ok(next) => next?,
                Err(_) => {
                    send(tx, Chunk::Rows(std::mem::take(&mut batch))).await?;
                    continue;
                }
            }
        };
        let Some(item) = next else { break };

        match item {
            Either::Left(result) => {
                summary.rows_affected += rows_affected(&result);
                new_result_set = true;
            }
            Either::Right(row) => {
                if new_result_set {
                    new_result_set = false;
                    if !batch.is_empty() {
                        send(tx, Chunk::Rows(std::mem::take(&mut batch))).await?;
                    }
                    kept = 0;
                    summary.truncated = false;
                    let columns = row
                        .columns()
                        .iter()
                        .map(|c| ColumnMeta::new(c.name(), c.type_info().name()))
                        .collect();
                    send(tx, Chunk::Columns(columns)).await?;
                }
                if kept == row_limit {
                    summary.truncated = true;
                    continue;
                }
                kept += 1;
                if batch.is_empty() {
                    deadline = Instant::now() + CHUNK_INTERVAL;
                }
                batch.push((0..row.len()).map(|i| cell(&row, i)).collect());
                if batch.len() == CHUNK_ROWS {
                    send(tx, Chunk::Rows(std::mem::take(&mut batch))).await?;
                }
            }
        }
    }

    if !batch.is_empty() {
        send(tx, Chunk::Rows(batch)).await?;
    }
    Ok(summary)
}

async fn send(tx: &mpsc::Sender<Chunk>, chunk: Chunk) -> Result<(), DbError> {
    tx.send(chunk).await.map_err(|_| DbError::Cancelled)
}

/// After a failed run on the pinned session connection: if the connection
/// itself is dead, swaps it for a fresh one from the pool so the next run
/// works, and returns `true`. The error is still returned to the user, because
/// session state was lost.
pub async fn replace_if_lost<DB: Database>(
    pool: &Pool<DB>,
    conn: &mut PoolConnection<DB>,
    result: &Result<StreamSummary, DbError>,
) -> bool {
    let Err(DbError::Sqlx(err)) = result else {
        return false;
    };
    if !is_connection_lost(err) {
        return false;
    }
    log::warn!("session connection lost, reconnecting: {err}");
    replace_session(pool, conn).await
}

/// Swaps the session connection for a fresh one from the pool and closes the
/// old one. Returns `false` if no new connection could be made.
pub async fn replace_session<DB: Database>(pool: &Pool<DB>, conn: &mut PoolConnection<DB>) -> bool {
    match pool.acquire().await {
        Ok(fresh) => {
            let mut old = std::mem::replace(conn, fresh);
            old.close_on_drop();
            true
        }
        Err(err) => {
            log::warn!("could not reconnect: {err}");
            false
        }
    }
}

/// Notices a run that was dropped before it finished. Its statement may still
/// be running on the server, and would hold up the next run on the same
/// connection, so that connection should be replaced.
#[derive(Debug, Default)]
pub struct RunGuard(std::sync::atomic::AtomicBool);

impl RunGuard {
    /// Marks a run as started. Returns `true` if the previous run never
    /// finished.
    pub fn start(&self) -> bool {
        self.0.swap(true, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn finish(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

fn is_connection_lost(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::Protocol(_)
            | sqlx::Error::WorkerCrashed
    )
}

/// Keeps bytes as text when they are printable UTF-8, otherwise as bytes,
/// which are shown as `0x` hex.
pub fn bytes_cell(bytes: &[u8]) -> OwnedCell {
    match std::str::from_utf8(bytes) {
        Ok(s)
            if !s
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r')) =>
        {
            OwnedCell::Text(s.into())
        }
        _ => OwnedCell::Bytes(bytes.into()),
    }
}

/// A value the server sent as text, read as the kind its column type says:
/// numbers, booleans and JSON. `None` for other types, and for text that
/// doesn't parse, which is then kept as text so nothing is misread.
pub fn typed_cell(type_name: &str, text: &str) -> Option<OwnedCell> {
    let upper = type_name.to_ascii_uppercase();
    let unsigned = upper.ends_with(" UNSIGNED");
    let base = upper.trim_end_matches(" UNSIGNED");
    let cell = match base {
        // Postgres.
        "INT2" | "INT4" | "INT8" => OwnedCell::Int(text.parse().ok()?),
        "OID" => OwnedCell::UInt(text.parse().ok()?),
        "FLOAT4" | "FLOAT8" => OwnedCell::Float(finite(text)?),
        "BOOL" => match text {
            "t" => OwnedCell::Bool(true),
            "f" => OwnedCell::Bool(false),
            _ => return None,
        },
        "BYTEA" => OwnedCell::Bytes(unhex(text.strip_prefix("\\x")?)?.into()),
        // MySQL.
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" if unsigned => {
            OwnedCell::UInt(text.parse().ok()?)
        }
        "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" => {
            OwnedCell::Int(text.parse().ok()?)
        }
        "FLOAT" | "DOUBLE" => OwnedCell::Float(finite(text)?),
        "BOOLEAN" => match text {
            "1" => OwnedCell::Bool(true),
            "0" => OwnedCell::Bool(false),
            _ => return None,
        },
        // Both.
        "NUMERIC" | "DECIMAL" => OwnedCell::Decimal(text.into()),
        "JSON" | "JSONB" => OwnedCell::Json(text.into()),
        _ => return None,
    };
    Some(cell)
}

/// A float that prints back the way the server sent it. `NaN`, infinities
/// and exponents stay text.
fn finite(text: &str) -> Option<f64> {
    let value: f64 = text.parse().ok()?;
    (value.is_finite() && !text.contains(['e', 'E'])).then_some(value)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Placeholder for a value that could not be read, so it is never shown as NULL.
pub fn undecodable(type_name: &str, err: impl std::fmt::Display) -> OwnedCell {
    log::warn!("could not decode {type_name} value: {err}");
    OwnedCell::Undecodable(format!("undecodable {type_name}").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::hex;

    #[test]
    fn printable_utf8_stays_text() {
        assert_eq!(
            bytes_cell(b"hello\tworld\n"),
            OwnedCell::Text("hello\tworld\n".into())
        );
        assert_eq!(
            bytes_cell("привет 🦊".as_bytes()),
            OwnedCell::Text("привет 🦊".into())
        );
    }

    #[test]
    fn binary_becomes_hex() {
        let shown = |bytes: &[u8]| bytes_cell(bytes).as_cell().to_string();
        assert_eq!(shown(&[0xde, 0xad, 0xbe, 0xef]), "0xdeadbeef");
        assert_eq!(shown(&[0x00, 0x41]), "0x0041");
        assert_eq!(hex(&[]), "0x");
    }

    #[test]
    fn text_values_are_read_by_column_type() {
        let cases = [
            ("int4", "-42", Some(OwnedCell::Int(-42))),
            (
                "INT8",
                "9223372036854775807",
                Some(OwnedCell::Int(i64::MAX)),
            ),
            ("oid", "16384", Some(OwnedCell::UInt(16384))),
            ("float8", "2.5", Some(OwnedCell::Float(2.5))),
            ("float8", "NaN", None),
            ("float4", "1e+20", None),
            ("bool", "t", Some(OwnedCell::Bool(true))),
            ("bool", "f", Some(OwnedCell::Bool(false))),
            ("bool", "maybe", None),
            (
                "bytea",
                "\\xdead",
                Some(OwnedCell::Bytes(vec![0xde, 0xad].into())),
            ),
            ("bytea", "\\xabc", None),
            ("bytea", "\\xzz", None),
            ("numeric", "1.50", Some(OwnedCell::Decimal("1.50".into()))),
            ("jsonb", "{}", Some(OwnedCell::Json("{}".into()))),
            (
                "BIGINT UNSIGNED",
                "18446744073709551615",
                Some(OwnedCell::UInt(u64::MAX)),
            ),
            ("INT", "7", Some(OwnedCell::Int(7))),
            ("INT", "x", None),
            ("DOUBLE", "0.1", Some(OwnedCell::Float(0.1))),
            ("BOOLEAN", "1", Some(OwnedCell::Bool(true))),
            ("BOOLEAN", "0", Some(OwnedCell::Bool(false))),
            ("BOOLEAN", "2", None),
            ("DECIMAL", "10.00", Some(OwnedCell::Decimal("10.00".into()))),
            ("JSON", "[1]", Some(OwnedCell::Json("[1]".into()))),
            ("text", "1", None),
            ("YEAR", "2024", None),
        ];
        for (type_name, text, expected) in cases {
            assert_eq!(typed_cell(type_name, text), expected, "{type_name} {text}");
        }
    }
}
