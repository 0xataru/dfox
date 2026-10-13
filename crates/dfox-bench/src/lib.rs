//! Shared data for the benchmarks: results of a given size in both storages,
//! and the queries that produce a million rows on each database.
//!
//! The benchmarks:
//!
//! - `decode`: time to the first row, and rows per second, streaming a
//!   million rows from each database. SQLite always runs; Postgres and MySQL
//!   run when `DFOX_TEST_POSTGRES_URL` / `DFOX_TEST_MYSQL_URL` are set, as in
//!   `make test-it`.
//! - `grid`: drawing a 200×60 grid over a million rows.
//! - `memory`: bytes taken by a million rows (not timed; prints a table).

use std::sync::Arc;

use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::DataType;
use dfox_core::{
    arrow::{field, schema, ArrowResultSet},
    result::{ColumnMeta, OwnedCell, RowStore},
};

/// Rows the decode and memory benchmarks read.
pub const ROWS: usize = 1_000_000;

/// The text in row `i`, column `c`.
fn text(i: usize, c: usize) -> String {
    format!("row {i} col {c}")
}

/// A `rows`×`columns` result kept as cells: an integer, a float, then text.
pub fn row_store(rows: usize, columns: usize) -> RowStore {
    let mut meta = vec![
        ColumnMeta::new("id", "int8"),
        ColumnMeta::new("score", "float8"),
    ];
    meta.extend((2..columns).map(|c| ColumnMeta::new(format!("text_{c}"), "text")));
    let mut store = RowStore::new(meta);
    store.extend((0..rows).map(|i| {
        let mut row = vec![OwnedCell::Int(i as i64), OwnedCell::Float(i as f64 * 1.5)];
        row.extend((2..columns).map(|c| OwnedCell::Text(text(i, c).into())));
        row
    }));
    store.finish();
    store
}

/// The same result kept as Arrow batches of 8192 rows.
pub fn arrow_result(rows: usize, columns: usize) -> ArrowResultSet {
    let mut fields = vec![
        field("id", DataType::Int64, "int8"),
        field("score", DataType::Float64, "float8"),
    ];
    fields.extend((2..columns).map(|c| field(&format!("text_{c}"), DataType::Utf8, "text")));
    let schema = schema(fields);
    let mut result = ArrowResultSet::new(schema.clone());
    for start in (0..rows).step_by(8192) {
        let end = rows.min(start + 8192);
        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from_iter_values(start as i64..end as i64)),
            Arc::new(Float64Array::from_iter_values(
                (start..end).map(|i| i as f64 * 1.5),
            )),
        ];
        arrays.extend((2..columns).map(|c| {
            Arc::new(StringArray::from_iter_values(
                (start..end).map(|i| text(i, c)),
            )) as ArrayRef
        }));
        if let Ok(batch) = RecordBatch::try_new(schema.clone(), arrays) {
            result.push(batch);
        }
    }
    result.finish();
    result
}

/// A million rows of an integer, a text and a float, on Postgres.
pub const POSTGRES_ROWS: &str =
    "SELECT i, 'row ' || i, i * 1.5 FROM generate_series(1, 1000000) AS i";

/// The same on MySQL: a thousand rows joined with themselves, so the server
/// doesn't recurse a million times.
pub const MYSQL_ROWS: &str =
    "WITH RECURSIVE t(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM t WHERE n < 999) \
     SELECT a.n * 1000 + b.n AS i, CONCAT('row ', a.n * 1000 + b.n), (a.n * 1000 + b.n) * 1.5 \
     FROM t AS a, t AS b";

/// The same on SQLite.
pub const SQLITE_ROWS: &str =
    "WITH RECURSIVE t(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM t WHERE i < 1000000) \
     SELECT i, 'row ' || i, i * 1.5 FROM t";

#[cfg(test)]
mod tests {
    use dfox_core::result::ResultSet;

    use super::*;

    #[test]
    fn both_storages_hold_the_same_values() {
        let store = row_store(10_000, 4);
        let arrow = arrow_result(10_000, 4);
        assert_eq!(store.row_count(), arrow.row_count());
        for (row, col) in [(0, 0), (9_999, 1), (8_192, 3), (5_000, 2)] {
            assert_eq!(store.cell(row, col), arrow.cell(row, col), "{row},{col}");
        }
    }
}
