//! Query results as Apache Arrow record batches: typed and columnar, so a
//! million integers are a million integers, not a million strings.
//!
//! A [`ResultStream`] carries a run's results as [`Frame`]s: each result set
//! starts with its schema, its rows follow in batches, and statements that
//! change rows report how many. [`ArrowResultSet`] keeps the batches and
//! serves cells to the grid through [`ResultSet`](crate::result::ResultSet);
//! [`Fetcher`] fills one within a memory budget.

mod fetch;
mod result_set;

use std::{collections::HashMap, sync::Arc};

use arrow_array::RecordBatch;
use arrow_schema::{DataType, Field, SchemaRef};
use futures_util::stream::BoxStream;

pub use fetch::{FetchStatus, Fetcher, Overflow, DEFAULT_BUDGET};
pub use result_set::ArrowResultSet;

use crate::errors::DbError;

/// Field metadata key: the database's own name for the column's type
/// (`int4`, `VARCHAR(20)`, …).
pub const TYPE_NAME: &str = "dfox.type";
/// Field metadata key: set to `"true"` on text columns that hold JSON.
pub const JSON: &str = "dfox.json";

/// One step of a run's output.
#[derive(Debug, Clone)]
pub enum Frame {
    /// A statement returns rows with this schema. The batches that follow
    /// belong to it, until the next `Schema`.
    Schema(SchemaRef),
    /// Rows of the current result set.
    Batch(RecordBatch),
    /// A statement changed this many rows.
    Affected(u64),
}

/// What a run sends back, in order. Ends after the last statement, or with
/// an error.
pub type ResultStream = BoxStream<'static, Result<Frame, DbError>>;

/// A field for a column of `data_type`, named `name`, with the database's
/// type name kept in its metadata.
pub fn field(name: &str, data_type: DataType, type_name: &str) -> Field {
    Field::new(name, data_type, true).with_metadata(HashMap::from([(
        TYPE_NAME.to_string(),
        type_name.to_string(),
    )]))
}

/// A text field that holds JSON.
pub fn json_field(name: &str, type_name: &str) -> Field {
    let mut field = field(name, DataType::Utf8, type_name);
    let mut metadata = field.metadata().clone();
    metadata.insert(JSON.to_string(), "true".to_string());
    field = field.with_metadata(metadata);
    field
}

/// The schema of `fields`, ready to share.
pub fn schema(fields: Vec<Field>) -> SchemaRef {
    Arc::new(arrow_schema::Schema::new(fields))
}
