//! Documents as Arrow columns. A collection has no schema, so the columns
//! are learnt from the documents: one per top-level field, in the order
//! fields first appear, typed when every value agrees and JSON otherwise.
//! Objects and arrays are JSON too.

use std::{collections::HashMap, sync::Arc};

use arrow_array::{
    builder::{
        BooleanBuilder, Float64Builder, Int64Builder, StringBuilder, TimestampMillisecondBuilder,
    },
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, TimeUnit};
use dfox_core::{
    arrow::{decimal_field, field, json_field, schema},
    errors::DbError,
};
use mongodb::bson::{Bson, Document};

/// What a column's values have been so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Only nulls, or missing.
    Unknown,
    Bool,
    Int,
    Double,
    Text,
    Date,
    ObjectId,
    Decimal,
    /// Every value an object.
    Object,
    /// Every value an array.
    Array,
    /// Values of different kinds.
    Mixed,
}

impl Kind {
    pub fn of(value: &Bson) -> Kind {
        match value {
            Bson::Null | Bson::Undefined => Kind::Unknown,
            Bson::Boolean(_) => Kind::Bool,
            Bson::Int32(_) | Bson::Int64(_) => Kind::Int,
            Bson::Double(_) => Kind::Double,
            Bson::String(_) | Bson::Symbol(_) => Kind::Text,
            Bson::DateTime(_) => Kind::Date,
            Bson::ObjectId(_) => Kind::ObjectId,
            Bson::Decimal128(_) => Kind::Decimal,
            Bson::Document(_) => Kind::Object,
            Bson::Array(_) => Kind::Array,
            _ => Kind::Mixed,
        }
    }

    /// The kind that holds values of both.
    fn merge(self, other: Kind) -> Kind {
        match (self, other) {
            (a, b) if a == b => a,
            (Kind::Unknown, k) | (k, Kind::Unknown) => k,
            (Kind::Int, Kind::Double) | (Kind::Double, Kind::Int) => Kind::Double,
            _ => Kind::Mixed,
        }
    }

    /// The name shown for the type.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Unknown => "null",
            Kind::Bool => "bool",
            Kind::Int => "int",
            Kind::Double => "double",
            Kind::Text => "string",
            Kind::Date => "date",
            Kind::ObjectId => "objectId",
            Kind::Decimal => "decimal",
            Kind::Object => "object",
            Kind::Array => "array",
            Kind::Mixed => "mixed",
        }
    }

    fn json(self) -> bool {
        matches!(self, Kind::Object | Kind::Array | Kind::Mixed)
    }
}

/// A value as relaxed extended JSON: what objects, arrays and values of a
/// mixed column show as.
pub fn json(value: &Bson) -> String {
    value.clone().into_relaxed_extjson().to_string()
}

/// The columns learnt so far.
#[derive(Debug, Clone, Default)]
pub struct Shape {
    columns: Vec<(String, Kind)>,
    index: HashMap<String, usize>,
}

impl Shape {
    /// Takes in a document's fields. True when the columns changed: a new
    /// field, or one whose values no longer fit its type.
    pub fn learn(&mut self, doc: &Document) -> bool {
        let mut changed = false;
        for (name, value) in doc {
            let kind = Kind::of(value);
            match self.index.get(name) {
                Some(&i) => {
                    let merged = self.columns[i].1.merge(kind);
                    if merged != self.columns[i].1 {
                        self.columns[i].1 = merged;
                        changed = true;
                    }
                }
                None => {
                    self.index.insert(name.clone(), self.columns.len());
                    self.columns.push((name.clone(), kind));
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn columns(&self) -> &[(String, Kind)] {
        &self.columns
    }

    pub fn schema(&self) -> arrow_schema::SchemaRef {
        schema(
            self.columns
                .iter()
                .map(|(name, kind)| match kind {
                    Kind::Bool => field(name, DataType::Boolean, kind.name()),
                    Kind::Int => field(name, DataType::Int64, kind.name()),
                    Kind::Double => field(name, DataType::Float64, kind.name()),
                    Kind::Date => field(
                        name,
                        DataType::Timestamp(TimeUnit::Millisecond, Some("+00:00".into())),
                        kind.name(),
                    ),
                    Kind::Decimal => decimal_field(name, kind.name()),
                    k if k.json() => json_field(name, kind.name()),
                    _ => field(name, DataType::Utf8, kind.name()),
                })
                .collect(),
        )
    }

    /// `docs` as a batch of these columns.
    pub fn batch(&self, docs: &[Document]) -> Result<RecordBatch, DbError> {
        let columns: Vec<ArrayRef> = self
            .columns
            .iter()
            .map(|(name, kind)| column(docs, name, *kind))
            .collect();
        RecordBatch::try_new(self.schema(), columns)
            .map_err(|err| DbError::General(format!("couldn't build the result: {err}")))
    }
}

fn column(docs: &[Document], name: &str, kind: Kind) -> ArrayRef {
    let values = docs.iter().map(|doc| match doc.get(name) {
        None | Some(Bson::Null | Bson::Undefined) => None,
        Some(value) => Some(value),
    });
    match kind {
        Kind::Bool => {
            let mut b = BooleanBuilder::with_capacity(docs.len());
            for v in values {
                b.append_option(v.and_then(Bson::as_bool));
            }
            Arc::new(b.finish())
        }
        Kind::Int => {
            let mut b = Int64Builder::with_capacity(docs.len());
            for v in values {
                b.append_option(v.and_then(|v| match v {
                    Bson::Int32(n) => Some(i64::from(*n)),
                    Bson::Int64(n) => Some(*n),
                    _ => None,
                }));
            }
            Arc::new(b.finish())
        }
        Kind::Double => {
            let mut b = Float64Builder::with_capacity(docs.len());
            for v in values {
                b.append_option(v.and_then(|v| match v {
                    Bson::Int32(n) => Some(f64::from(*n)),
                    Bson::Int64(n) => Some(*n as f64),
                    Bson::Double(n) => Some(*n),
                    _ => None,
                }));
            }
            Arc::new(b.finish())
        }
        Kind::Date => {
            let mut b =
                TimestampMillisecondBuilder::with_capacity(docs.len()).with_timezone("+00:00");
            for v in values {
                b.append_option(v.and_then(|v| match v {
                    Bson::DateTime(d) => Some(d.timestamp_millis()),
                    _ => None,
                }));
            }
            Arc::new(b.finish())
        }
        kind => {
            let mut b = StringBuilder::new();
            for v in values {
                b.append_option(v.map(|v| match (kind, v) {
                    (Kind::Text, Bson::String(s)) => s.clone(),
                    (Kind::ObjectId, Bson::ObjectId(id)) => id.to_hex(),
                    (Kind::Decimal, Bson::Decimal128(d)) => d.to_string(),
                    (_, v) => json(v),
                }));
            }
            Arc::new(b.finish())
        }
    }
}

#[cfg(test)]
mod tests {
    use dfox_core::{
        arrow::ArrowResultSet,
        result::{Cell, ResultSet},
    };
    use mongodb::bson::{doc, oid::ObjectId, DateTime};

    use super::*;

    fn cells(shape: &Shape, docs: &[Document]) -> Vec<Vec<String>> {
        let mut result = ArrowResultSet::new(shape.schema());
        result.push(shape.batch(docs).expect("batch"));
        result.finish();
        (0..result.row_count())
            .map(|row| {
                (0..result.columns().len())
                    .map(|col| match result.cell(row, col) {
                        Some(Cell::Null) | None => "NULL".to_string(),
                        Some(cell) => cell.to_string(),
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn columns_follow_the_documents() {
        let id = ObjectId::parse_str("64b7f0c2a1b2c3d4e5f60718").expect("oid");
        let docs = vec![
            doc! { "_id": id, "n": 1, "x": 1.5, "tags": ["a"], "at": DateTime::from_millis(0) },
            doc! { "_id": id, "n": 2_i64, "x": 2, "maybe": null, "mixed": "a" },
            doc! { "_id": id, "n": 3, "x": 2.5, "tags": ["b", "c"], "mixed": { "deep": true } },
        ];
        let mut shape = Shape::default();
        assert!(shape.learn(&docs[0]));
        assert!(shape.learn(&docs[1]));
        assert!(shape.learn(&docs[2]));
        assert!(!shape.learn(&docs[2]), "nothing new");
        let kinds: Vec<(&str, &str)> = shape
            .columns()
            .iter()
            .map(|(n, k)| (n.as_str(), k.name()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("_id", "objectId"),
                ("n", "int"),
                ("x", "double"),
                ("tags", "array"),
                ("at", "date"),
                ("maybe", "null"),
                ("mixed", "mixed"),
            ]
        );
        let rows = cells(&shape, &docs);
        assert_eq!(rows[0][0], "64b7f0c2a1b2c3d4e5f60718");
        assert_eq!(rows[1][1], "2");
        assert_eq!(rows[1][2], "2.0");
        assert_eq!(rows[2][3], r#"["b","c"]"#);
        assert_eq!(rows[1][6], r#""a""#);
        assert_eq!(rows[2][6], r#"{"deep":true}"#);
        // Missing fields and nulls are NULL.
        assert_eq!((rows[1][3].as_str(), rows[0][5].as_str()), ("NULL", "NULL"));
    }
}
