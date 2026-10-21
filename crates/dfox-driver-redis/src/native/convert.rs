//! Redis replies as tables. A value is one row; a list or set, a row per
//! item; a map, a row per pair. Columns are named for the command when it
//! is known (`HGETALL` gives `field` and `value`, `ZRANGE … WITHSCORES`
//! `member` and `score`), and typed as integers or numbers when every value
//! in them is one.

use std::sync::Arc;

use arrow_array::{
    builder::{Float64Builder, Int64Builder, StringBuilder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, SchemaRef};
use dfox_core::{
    arrow::{field, schema},
    errors::DbError,
};
use redis::Value;

/// A reply laid out as rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub columns: Vec<&'static str>,
    pub rows: Vec<Vec<Value>>,
}

impl Table {
    fn new(columns: &[&'static str], rows: Vec<Vec<Value>>) -> Self {
        Self {
            columns: columns.to_vec(),
            rows,
        }
    }
}

/// `value`, the reply to `command` (its words), as a table.
pub fn table(command: &[String], value: Value) -> Table {
    let name = command
        .first()
        .map(|c| c.to_uppercase())
        .unwrap_or_default();
    let with_scores = command.iter().any(|w| w.eq_ignore_ascii_case("WITHSCORES"));
    match value {
        Value::Attribute { data, .. } => table(command, *data),
        Value::Map(pairs) => {
            let columns: &[&str] = match name.as_str() {
                "HGETALL" | "HRANDFIELD" => &["field", "value"],
                "CONFIG" => &["name", "value"],
                _ => &["key", "value"],
            };
            Table::new(
                columns,
                pairs.into_iter().map(|(k, v)| vec![k, v]).collect(),
            )
        }
        Value::Array(items) | Value::Set(items) => {
            let pairs = !items.is_empty()
                && items
                    .iter()
                    .all(|item| matches!(item, Value::Array(a) if a.len() == 2));
            let rows = || {
                items
                    .clone()
                    .into_iter()
                    .map(|item| match item {
                        Value::Array(cells) => cells,
                        other => vec![other],
                    })
                    .collect()
            };
            match name.as_str() {
                "XRANGE" | "XREVRANGE" if pairs => Table::new(&["id", "fields"], rows()),
                _ if pairs && with_scores => Table::new(&["member", "score"], rows()),
                _ => Table::new(&["value"], items.into_iter().map(|v| vec![v]).collect()),
            }
        }
        // `INFO`: `# Section` headings over `key:value` lines.
        Value::BulkString(_) | Value::VerbatimString { .. } if name == "INFO" => {
            let mut section = String::new();
            let mut rows = Vec::new();
            for line in text(&value).lines() {
                if let Some(heading) = line.strip_prefix("# ") {
                    section = heading.trim().to_string();
                } else if let Some((key, value)) = line.split_once(':') {
                    rows.push(vec![
                        Value::SimpleString(section.clone()),
                        Value::SimpleString(key.to_string()),
                        Value::SimpleString(value.trim_end().to_string()),
                    ]);
                }
            }
            Table::new(&["section", "key", "value"], rows)
        }
        value => Table::new(&["value"], vec![vec![value]]),
    }
}

/// A value as text: strings as they are, lists in `[ ]`, maps in `{ }`.
pub fn text(value: &Value) -> String {
    match value {
        Value::Nil => "(nil)".to_string(),
        Value::Int(n) => n.to_string(),
        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        Value::SimpleString(s) => s.clone(),
        Value::Okay => "OK".to_string(),
        Value::Double(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::VerbatimString { text, .. } => text.clone(),
        Value::BigNumber(digits) => String::from_utf8_lossy(digits).into_owned(),
        Value::Array(items) | Value::Set(items) | Value::Push { data: items, .. } => {
            let items: Vec<String> = items.iter().map(text).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Map(pairs) => {
            let pairs: Vec<String> = pairs
                .iter()
                .map(|(k, v)| format!("{}: {}", text(k), text(v)))
                .collect();
            format!("{{{}}}", pairs.join(", "))
        }
        Value::Attribute { data, .. } => text(data),
        Value::ServerError(err) => err.to_string(),
        // Kinds of reply newer than this code.
        other => format!("{other:?}"),
    }
}

/// How a column's values are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    Float,
    Text,
}

fn kind(rows: &[Vec<Value>], column: usize) -> Kind {
    let mut kind = Kind::Int;
    for value in rows.iter().filter_map(|row| row.get(column)) {
        match value {
            Value::Nil | Value::Int(_) => {}
            Value::Double(_) => kind = Kind::Float,
            _ => return Kind::Text,
        }
    }
    kind
}

impl Table {
    /// The columns' Arrow schema.
    pub fn schema(&self) -> SchemaRef {
        schema(
            self.columns
                .iter()
                .enumerate()
                .map(|(i, name)| match kind(&self.rows, i) {
                    Kind::Int => field(name, DataType::Int64, "integer"),
                    Kind::Float => field(name, DataType::Float64, "double"),
                    Kind::Text => field(name, DataType::Utf8, "string"),
                })
                .collect(),
        )
    }

    /// `rows` of this table, as a batch of `schema`.
    pub fn batch(&self, schema: &SchemaRef, rows: &[Vec<Value>]) -> Result<RecordBatch, DbError> {
        let columns: Vec<ArrayRef> = schema
            .fields()
            .iter()
            .enumerate()
            .map(|(i, field)| -> ArrayRef {
                let cells = rows.iter().map(|row| row.get(i).unwrap_or(&Value::Nil));
                match field.data_type() {
                    DataType::Int64 => {
                        let mut b = Int64Builder::new();
                        for cell in cells {
                            b.append_option(match cell {
                                Value::Int(n) => Some(*n),
                                _ => None,
                            });
                        }
                        Arc::new(b.finish())
                    }
                    DataType::Float64 => {
                        let mut b = Float64Builder::new();
                        for cell in cells {
                            b.append_option(match cell {
                                Value::Int(n) => Some(*n as f64),
                                Value::Double(n) => Some(*n),
                                _ => None,
                            });
                        }
                        Arc::new(b.finish())
                    }
                    _ => {
                        let mut b = StringBuilder::new();
                        for cell in cells {
                            match cell {
                                Value::Nil => b.append_null(),
                                cell => b.append_value(text(cell)),
                            }
                        }
                        Arc::new(b.finish())
                    }
                }
            })
            .collect();
        RecordBatch::try_new(schema.clone(), columns).map_err(|e| DbError::General(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn replies_lay_out_by_command() {
        let hash = table(
            &words("HGETALL user:1"),
            Value::Map(vec![(bulk("name"), bulk("Ann")), (bulk("age"), bulk("30"))]),
        );
        assert_eq!(hash.columns, ["field", "value"]);
        assert_eq!(hash.rows.len(), 2);

        let scores = table(
            &words("ZRANGE board 0 -1 WITHSCORES"),
            Value::Array(vec![
                Value::Array(vec![bulk("ann"), Value::Double(1.5)]),
                Value::Array(vec![bulk("bob"), Value::Double(2.0)]),
            ]),
        );
        assert_eq!(scores.columns, ["member", "score"]);
        assert_eq!(scores.schema().field(1).data_type(), &DataType::Float64);

        let list = table(
            &words("LRANGE l 0 -1"),
            Value::Array(vec![bulk("a"), bulk("b")]),
        );
        assert_eq!((list.columns.clone(), list.rows.len()), (vec!["value"], 2));

        let stream = table(
            &words("XRANGE s - +"),
            Value::Array(vec![Value::Array(vec![
                bulk("1-0"),
                Value::Array(vec![bulk("temp"), bulk("20")]),
            ])]),
        );
        assert_eq!(stream.columns, ["id", "fields"]);
        assert_eq!(text(&stream.rows[0][1]), "[temp, 20]");

        let one = table(&words("INCR n"), Value::Int(5));
        assert_eq!(one.schema().field(0).data_type(), &DataType::Int64);
    }

    #[test]
    fn info_becomes_keys_by_section() {
        let info = table(
            &words("INFO"),
            bulk("# Server\r\nredis_version:7.4.0\r\n\r\n# Clients\r\nconnected_clients:1\r\n"),
        );
        assert_eq!(info.columns, ["section", "key", "value"]);
        assert_eq!(
            info.rows[1].iter().map(text).collect::<Vec<_>>(),
            ["Clients", "connected_clients", "1"]
        );
    }

    #[test]
    fn batches_keep_types_and_nulls() {
        let t = table(
            &words("MGET a b"),
            Value::Array(vec![bulk("x"), Value::Nil]),
        );
        let schema = t.schema();
        let batch = t.batch(&schema, &t.rows).expect("batch");
        assert_eq!(batch.num_rows(), 2);
        assert!(batch.column(0).is_null(1));
    }
}
