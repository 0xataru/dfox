//! SQLite values, decoded into Arrow columns.
//!
//! SQLite types each value, not each column, while an Arrow column has one
//! type. A column takes the type of its first value and widens when a value
//! doesn't fit, without losing anything:
//!
//! - NULL widens to anything;
//! - booleans (0 and 1 in a column declared `BOOLEAN`) widen to integers;
//! - integers and reals together become exact decimal text, so a large
//!   integer isn't rounded through a float;
//! - anything with text becomes text, with blobs written `X'…'`.
//!
//! [`Columns`] widens the batch it is building on the spot; widening rows
//! already sent is the session's job.

use std::{fmt::Write, sync::Arc};

use arrow_array::{
    builder::{BinaryBuilder, BooleanBuilder, Float64Builder, Int64Builder, StringBuilder},
    Array, ArrayRef, NullArray,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use dfox_core::{
    arrow::{decimal_field, field},
    errors::DbError,
};
use rusqlite::types::ValueRef;

/// What a column holds so far. Each kind widens only upwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Null,
    Bool,
    Int,
    Real,
    /// Integers and reals, as exact decimal text.
    Number,
    Blob,
    Text,
}

impl Kind {
    /// The kind of one value. `boolean` is true when the column is declared
    /// a boolean, where 0 and 1 are booleans.
    fn of(value: ValueRef<'_>, boolean: bool) -> Kind {
        match value {
            ValueRef::Null => Kind::Null,
            ValueRef::Integer(0 | 1) if boolean => Kind::Bool,
            ValueRef::Integer(_) => Kind::Int,
            ValueRef::Real(_) => Kind::Real,
            // Text that isn't UTF-8 keeps its bytes.
            ValueRef::Text(bytes) if std::str::from_utf8(bytes).is_err() => Kind::Blob,
            ValueRef::Text(_) => Kind::Text,
            ValueRef::Blob(_) => Kind::Blob,
        }
    }

    /// The narrowest kind that holds both.
    pub fn join(self, other: Kind) -> Kind {
        use Kind::*;
        match (self, other) {
            (a, b) if a == b => a,
            (Null, k) | (k, Null) => k,
            (Bool, Int) | (Int, Bool) => Int,
            (Bool | Int | Real | Number, Bool | Int | Real | Number) => Number,
            _ => Text,
        }
    }

    fn data_type(self) -> DataType {
        match self {
            Kind::Null => DataType::Null,
            Kind::Bool => DataType::Boolean,
            Kind::Int => DataType::Int64,
            Kind::Real => DataType::Float64,
            Kind::Number | Kind::Text => DataType::Utf8,
            Kind::Blob => DataType::Binary,
        }
    }
}

/// A result column: its name, declared type and what it holds so far.
#[derive(Debug, Clone)]
pub struct ColumnMeta {
    pub name: String,
    /// The type the column was declared with, when it comes from a table.
    pub declared: Option<String>,
    pub kind: Kind,
}

impl ColumnMeta {
    pub fn new(name: &str, declared: Option<&str>) -> Self {
        Self {
            name: name.to_string(),
            declared: declared.map(str::to_string),
            kind: Kind::Null,
        }
    }

    fn boolean(&self) -> bool {
        self.declared
            .as_deref()
            .is_some_and(|t| t.to_ascii_uppercase().contains("BOOL"))
    }

    fn field(&self) -> Field {
        let type_name = self.declared.as_deref().unwrap_or(match self.kind {
            Kind::Null => "NULL",
            Kind::Bool | Kind::Int => "INTEGER",
            Kind::Real => "REAL",
            Kind::Number => "NUMERIC",
            Kind::Blob => "BLOB",
            Kind::Text => "TEXT",
        });
        match self.kind {
            Kind::Number => decimal_field(&self.name, type_name),
            kind => field(&self.name, kind.data_type(), type_name),
        }
    }
}

/// The schema of `columns` as they are now.
pub fn schema(columns: &[ColumnMeta]) -> SchemaRef {
    Arc::new(Schema::new(
        columns.iter().map(ColumnMeta::field).collect::<Vec<_>>(),
    ))
}

/// One column's values in the batch being built.
enum Builder {
    Null(usize),
    Bool(BooleanBuilder),
    Int(Int64Builder),
    Real(Float64Builder),
    Text(StringBuilder),
    Blob(BinaryBuilder),
}

impl Builder {
    fn new(kind: Kind) -> Builder {
        match kind {
            Kind::Null => Builder::Null(0),
            Kind::Bool => Builder::Bool(BooleanBuilder::new()),
            Kind::Int => Builder::Int(Int64Builder::new()),
            Kind::Real => Builder::Real(Float64Builder::new()),
            Kind::Number | Kind::Text => Builder::Text(StringBuilder::new()),
            Kind::Blob => Builder::Blob(BinaryBuilder::new()),
        }
    }

    /// Appends `value`, which fits the builder's kind.
    fn push(&mut self, value: ValueRef<'_>) {
        match (self, value) {
            (Builder::Null(n), _) => *n += 1,
            (b, ValueRef::Null) => b.push_null(),
            (Builder::Bool(b), ValueRef::Integer(n)) => b.append_value(n != 0),
            (Builder::Int(b), ValueRef::Integer(n)) => b.append_value(n),
            (Builder::Real(b), ValueRef::Real(f)) => b.append_value(f),
            (Builder::Blob(b), ValueRef::Blob(bytes) | ValueRef::Text(bytes)) => {
                b.append_value(bytes)
            }
            (Builder::Text(b), value) => b.append_value(text(value)),
            // A value of another kind never reaches here: the column widens
            // first.
            (b, _) => b.push_null(),
        }
    }

    fn push_null(&mut self) {
        match self {
            Builder::Null(n) => *n += 1,
            Builder::Bool(b) => b.append_null(),
            Builder::Int(b) => b.append_null(),
            Builder::Real(b) => b.append_null(),
            Builder::Text(b) => b.append_null(),
            Builder::Blob(b) => b.append_null(),
        }
    }

    fn finish(&mut self) -> ArrayRef {
        match self {
            Builder::Null(n) => Arc::new(NullArray::new(std::mem::take(n))),
            Builder::Bool(b) => Arc::new(b.finish()),
            Builder::Int(b) => Arc::new(b.finish()),
            Builder::Real(b) => Arc::new(b.finish()),
            Builder::Text(b) => Arc::new(b.finish()),
            Builder::Blob(b) => Arc::new(b.finish()),
        }
    }
}

/// A value as text, for a `Number` or `Text` column.
fn text(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => String::new(),
        ValueRef::Integer(n) => n.to_string(),
        ValueRef::Real(f) => real(f),
        ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
            Err(_) => blob_literal(bytes),
        },
        ValueRef::Blob(bytes) => blob_literal(bytes),
    }
}

/// A real as SQLite prints it: `2.5`, `3.0`, `1.0e+20`.
fn real(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        f.to_string()
    }
}

/// A blob as an SQL literal: `X'DEADBEEF'`.
pub fn blob_literal(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2 + 3);
    out.push_str("X'");
    for b in bytes {
        let _ = write!(out, "{b:02X}");
    }
    out.push('\'');
    out
}

/// Builds the columns of one statement's batches, widening as values
/// arrive.
pub struct Columns {
    pub meta: Vec<ColumnMeta>,
    builders: Vec<Builder>,
    /// Rows in the batch so far.
    rows: usize,
    /// True when a column widened since the last schema went out.
    pub widened: bool,
}

impl Columns {
    pub fn new(meta: Vec<ColumnMeta>) -> Self {
        let builders = meta.iter().map(|m| Builder::new(m.kind)).collect();
        Self {
            meta,
            builders,
            rows: 0,
            widened: false,
        }
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Adds a row of values.
    pub fn push(&mut self, row: &rusqlite::Row<'_>) -> Result<(), DbError> {
        for i in 0..self.meta.len() {
            let value = row
                .get_ref(i)
                .map_err(|err| DbError::General(err.to_string()))?;
            let kind = Kind::of(value, self.meta[i].boolean());
            let current = self.meta[i].kind;
            let joined = current.join(kind);
            if joined != current {
                self.widen(i, joined);
            }
            self.builders[i].push(value);
        }
        self.rows += 1;
        Ok(())
    }

    /// Widens column `i` to `kind`, rebuilding what the batch holds of it.
    fn widen(&mut self, i: usize, kind: Kind) {
        let before = self.builders[i].finish();
        self.meta[i].kind = kind;
        let mut rebuilt = Builder::new(kind);
        for row in 0..before.len() {
            let value = owned(&before, row);
            rebuilt.push(value.as_ref());
        }
        self.builders[i] = rebuilt;
        self.widened = true;
    }

    /// The batch's arrays; the builders start over.
    pub fn finish(&mut self) -> Vec<ArrayRef> {
        self.rows = 0;
        self.builders.iter_mut().map(Builder::finish).collect()
    }
}

/// A value read back from an array, to rebuild it as another kind.
enum Owned {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Owned {
    fn as_ref(&self) -> ValueRef<'_> {
        match self {
            Owned::Null => ValueRef::Null,
            Owned::Integer(n) => ValueRef::Integer(*n),
            Owned::Real(f) => ValueRef::Real(*f),
            Owned::Text(t) => ValueRef::Text(t.as_bytes()),
            Owned::Blob(b) => ValueRef::Blob(b),
        }
    }
}

fn owned(array: &ArrayRef, row: usize) -> Owned {
    use arrow_array::cast::AsArray;
    if array.is_null(row) || array.data_type() == &DataType::Null {
        return Owned::Null;
    }
    match array.data_type() {
        DataType::Boolean => Owned::Integer(i64::from(array.as_boolean().value(row))),
        DataType::Int64 => Owned::Integer(
            array
                .as_primitive::<arrow_array::types::Int64Type>()
                .value(row),
        ),
        DataType::Float64 => Owned::Real(
            array
                .as_primitive::<arrow_array::types::Float64Type>()
                .value(row),
        ),
        DataType::Binary => Owned::Blob(array.as_binary::<i32>().value(row).to_vec()),
        _ => Owned::Text(array.as_string::<i32>().value(row).to_string()),
    }
}

/// `arrays`, a batch sent when the columns were narrower, widened to the
/// kinds in `meta`.
pub fn widen_batch(arrays: &[ArrayRef], meta: &[ColumnMeta]) -> Vec<ArrayRef> {
    arrays
        .iter()
        .zip(meta)
        .map(|(array, meta)| {
            let mut builder = Builder::new(meta.kind);
            for row in 0..array.len() {
                builder.push(owned(array, row).as_ref());
            }
            builder.finish()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use arrow_array::cast::AsArray;

    use super::*;

    #[test]
    fn kinds_widen_without_losing_values() {
        use Kind::*;
        assert_eq!(Null.join(Int), Int);
        assert_eq!(Bool.join(Int), Int);
        assert_eq!(Int.join(Real), Number);
        assert_eq!(Real.join(Bool), Number);
        assert_eq!(Int.join(Text), Text);
        assert_eq!(Blob.join(Int), Text);
        assert_eq!(Text.join(Null), Text);
    }

    #[test]
    fn values_print_as_sqlite_prints_them() {
        assert_eq!(real(3.0), "3.0");
        assert_eq!(real(2.5), "2.5");
        assert_eq!(blob_literal(&[0xde, 0xad]), "X'DEAD'");
        assert_eq!(
            text(ValueRef::Integer(9_007_199_254_740_993)),
            "9007199254740993"
        );
        assert_eq!(text(ValueRef::Blob(&[1])), "X'01'");
    }

    #[test]
    fn a_batch_widens_in_place() {
        let mut columns = Columns::new(vec![ColumnMeta::new("v", None)]);
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        let mut stmt = conn
            .prepare("SELECT NULL UNION ALL SELECT 1 UNION ALL SELECT 2.5 UNION ALL SELECT 'x'")
            .expect("prepare");
        let mut rows = stmt.raw_query();
        while let Some(row) = rows.next().expect("row") {
            columns.push(row).expect("push");
        }
        assert!(columns.widened);
        assert_eq!(columns.meta[0].kind, Kind::Text);
        let arrays = columns.finish();
        let text = arrays[0].as_string::<i32>();
        assert!(text.is_null(0));
        assert_eq!(
            (1..4).map(|i| text.value(i)).collect::<Vec<_>>(),
            ["1", "2.5", "x"]
        );
    }

    #[test]
    fn sent_batches_widen_to_the_new_kinds() {
        let ints: ArrayRef = Arc::new(arrow_array::Int64Array::from(vec![Some(1), None]));
        let mut meta = ColumnMeta::new("n", None);
        meta.kind = Kind::Number;
        let widened = widen_batch(&[ints], &[meta]);
        let text = widened[0].as_string::<i32>();
        assert_eq!(text.value(0), "1");
        assert!(text.is_null(1));
    }
}
