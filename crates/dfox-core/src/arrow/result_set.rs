//! A result held as Arrow batches, read cell by cell.

use std::sync::OnceLock;

use arrow_array::{
    cast::AsArray,
    types::{
        Float32Type, Float64Type, Int16Type, Int32Type, Int64Type, Int8Type, UInt16Type,
        UInt32Type, UInt64Type, UInt8Type,
    },
    Array, RecordBatch,
};
use arrow_cast::display::{ArrayFormatter, FormatOptions};
use arrow_schema::{DataType, SchemaRef};

use super::{JSON, TYPE_NAME};
use crate::result::{Cell, ColumnMeta, ResultSet};

/// Text of the values of one batch column that have no borrowable form, such
/// as decimals and dates, made the first time each is read.
type Shown = OnceLock<Vec<OnceLock<Box<str>>>>;

/// Rows held as Arrow batches. Numbers, booleans, text and bytes are read
/// straight from the arrays; other types are formatted once, when first
/// read, so only the cells on screen are ever turned into text.
#[derive(Debug)]
pub struct ArrowResultSet {
    schema: SchemaRef,
    columns: Vec<ColumnMeta>,
    json: Vec<bool>,
    batches: Vec<RecordBatch>,
    /// The row each batch starts at.
    starts: Vec<usize>,
    /// Per batch, per column.
    shown: Vec<Vec<Shown>>,
    rows: usize,
    bytes: usize,
    complete: bool,
}

impl ArrowResultSet {
    /// An empty result with `schema`, waiting for batches.
    pub fn new(schema: SchemaRef) -> Self {
        let columns = schema
            .fields()
            .iter()
            .map(|f| {
                let type_name = f
                    .metadata()
                    .get(TYPE_NAME)
                    .cloned()
                    .unwrap_or_else(|| f.data_type().to_string());
                ColumnMeta::new(f.name().clone(), type_name)
            })
            .collect();
        let json = schema
            .fields()
            .iter()
            .map(|f| f.metadata().get(JSON).is_some_and(|v| v == "true"))
            .collect();
        Self {
            schema,
            columns,
            json,
            batches: Vec::new(),
            starts: Vec::new(),
            shown: Vec::new(),
            rows: 0,
            bytes: 0,
            complete: false,
        }
    }

    /// The columns' names and types.
    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    /// Adds rows. Empty batches are skipped.
    pub fn push(&mut self, batch: RecordBatch) {
        if batch.num_rows() == 0 {
            return;
        }
        self.starts.push(self.rows);
        self.rows += batch.num_rows();
        self.bytes += batch.get_array_memory_size();
        self.shown
            .push((0..batch.num_columns()).map(|_| OnceLock::new()).collect());
        self.batches.push(batch);
    }

    /// Marks the result as complete: no more rows will come.
    pub fn finish(&mut self) {
        self.complete = true;
    }

    /// Bytes held by the batches.
    pub fn memory_bytes(&self) -> usize {
        self.bytes
    }

    /// The rows, as the driver sent them.
    pub fn batches(&self) -> &[RecordBatch] {
        &self.batches
    }

    /// The text of a value with no borrowable form.
    fn shown(&self, batch: usize, col: usize, index: usize, array: &dyn Array) -> &str {
        let column = self.shown[batch][col]
            .get_or_init(|| (0..array.len()).map(|_| OnceLock::new()).collect());
        column[index].get_or_init(|| {
            ArrayFormatter::try_new(array, &FormatOptions::default())
                .map(|f| f.value(index).to_string())
                .unwrap_or_else(|err| format!("⟨{err}⟩"))
                .into()
        })
    }
}

impl ResultSet for ArrowResultSet {
    fn columns(&self) -> &[ColumnMeta] {
        &self.columns
    }

    fn row_count(&self) -> usize {
        self.rows
    }

    fn cell(&self, row: usize, col: usize) -> Option<Cell<'_>> {
        if row >= self.rows || col >= self.columns.len() {
            return None;
        }
        let batch = self.starts.partition_point(|&start| start <= row) - 1;
        let index = row - self.starts[batch];
        let array = self.batches[batch].column(col).as_ref();
        if array.is_null(index) {
            return Some(Cell::Null);
        }
        let text = |s| {
            if self.json[col] {
                Cell::Json(s)
            } else {
                Cell::Text(s)
            }
        };
        Some(match array.data_type() {
            DataType::Null => Cell::Null,
            DataType::Boolean => Cell::Bool(array.as_boolean().value(index)),
            DataType::Int8 => Cell::Int(array.as_primitive::<Int8Type>().value(index).into()),
            DataType::Int16 => Cell::Int(array.as_primitive::<Int16Type>().value(index).into()),
            DataType::Int32 => Cell::Int(array.as_primitive::<Int32Type>().value(index).into()),
            DataType::Int64 => Cell::Int(array.as_primitive::<Int64Type>().value(index)),
            DataType::UInt8 => Cell::UInt(array.as_primitive::<UInt8Type>().value(index).into()),
            DataType::UInt16 => Cell::UInt(array.as_primitive::<UInt16Type>().value(index).into()),
            DataType::UInt32 => Cell::UInt(array.as_primitive::<UInt32Type>().value(index).into()),
            DataType::UInt64 => Cell::UInt(array.as_primitive::<UInt64Type>().value(index)),
            DataType::Float32 => {
                Cell::Float(array.as_primitive::<Float32Type>().value(index).into())
            }
            DataType::Float64 => Cell::Float(array.as_primitive::<Float64Type>().value(index)),
            DataType::Utf8 => text(array.as_string::<i32>().value(index)),
            DataType::LargeUtf8 => text(array.as_string::<i64>().value(index)),
            DataType::Utf8View => text(array.as_string_view().value(index)),
            DataType::Binary => Cell::Bytes(array.as_binary::<i32>().value(index)),
            DataType::LargeBinary => Cell::Bytes(array.as_binary::<i64>().value(index)),
            DataType::BinaryView => Cell::Bytes(array.as_binary_view().value(index)),
            DataType::Decimal32(..)
            | DataType::Decimal64(..)
            | DataType::Decimal128(..)
            | DataType::Decimal256(..) => Cell::Decimal(self.shown(batch, col, index, array)),
            // Dates, times, intervals, UUIDs, lists, structs, …
            _ => Cell::Text(self.shown(batch, col, index, array)),
        })
    }

    fn is_complete(&self) -> bool {
        self.complete
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{
        builder::{ListBuilder, StringBuilder},
        ArrayRef, BinaryArray, BooleanArray, Date32Array, Decimal128Array, Float32Array,
        Int16Array, Int64Array, LargeStringArray, NullArray, StringArray, StringViewArray,
        TimestampMicrosecondArray, UInt64Array, UInt8Array,
    };

    use super::*;
    use crate::arrow::{field, json_field, schema};

    fn result(fields: Vec<arrow_schema::Field>, arrays: Vec<ArrayRef>) -> ArrowResultSet {
        let schema = schema(fields);
        let batch = RecordBatch::try_new(schema.clone(), arrays).expect("batch");
        let mut result = ArrowResultSet::new(schema);
        result.push(batch);
        result.finish();
        result
    }

    #[test]
    fn every_kind_of_value_reads_back() {
        let mut list = ListBuilder::new(StringBuilder::new());
        list.values().append_value("a");
        list.values().append_value("b");
        list.append(true);
        let result = result(
            vec![
                field("i", DataType::Int16, "int2"),
                field("big", DataType::Int64, "int8"),
                field("u", DataType::UInt8, "tinyint unsigned"),
                field("ubig", DataType::UInt64, "bigint unsigned"),
                field("f", DataType::Float32, "float4"),
                field("b", DataType::Boolean, "bool"),
                field("t", DataType::Utf8, "text"),
                json_field("j", "jsonb"),
                field("lt", DataType::LargeUtf8, "text"),
                field("vt", DataType::Utf8View, "text"),
                field("bin", DataType::Binary, "bytea"),
                field("d", DataType::Decimal128(10, 2), "numeric"),
                field("day", DataType::Date32, "date"),
                field(
                    "at",
                    DataType::Timestamp(arrow_schema::TimeUnit::Microsecond, None),
                    "timestamp",
                ),
                field(
                    "l",
                    DataType::List(Arc::new(arrow_schema::Field::new_list_field(
                        DataType::Utf8,
                        true,
                    ))),
                    "text[]",
                ),
                field("n", DataType::Null, "void"),
            ],
            vec![
                Arc::new(Int16Array::from(vec![-7])),
                Arc::new(Int64Array::from(vec![i64::MAX])),
                Arc::new(UInt8Array::from(vec![255])),
                Arc::new(UInt64Array::from(vec![u64::MAX])),
                Arc::new(Float32Array::from(vec![1.5])),
                Arc::new(BooleanArray::from(vec![true])),
                Arc::new(StringArray::from(vec!["hi"])),
                Arc::new(StringArray::from(vec![r#"{"a":1}"#])),
                Arc::new(LargeStringArray::from(vec!["large"])),
                Arc::new(StringViewArray::from(vec!["view"])),
                Arc::new(BinaryArray::from(vec![&b"\xde\xad"[..]])),
                Arc::new(
                    Decimal128Array::from(vec![12345])
                        .with_precision_and_scale(10, 2)
                        .expect("decimal"),
                ),
                Arc::new(Date32Array::from(vec![20_000])),
                Arc::new(TimestampMicrosecondArray::from(vec![1_700_000_000_000_000])),
                Arc::new(list.finish()),
                Arc::new(NullArray::new(1)),
            ],
        );
        let cells: Vec<String> = (0..result.columns().len())
            .map(|col| result.cell(0, col).expect("cell").to_string())
            .collect();
        assert_eq!(
            cells,
            [
                "-7",
                "9223372036854775807",
                "255",
                "18446744073709551615",
                "1.5",
                "true",
                "hi",
                r#"{"a":1}"#,
                "large",
                "view",
                "0xdead",
                "123.45",
                "2024-10-04",
                "2023-11-14T22:13:20",
                "[a, b]",
                "NULL",
            ]
        );
        assert!(matches!(result.cell(0, 7), Some(Cell::Json(_))));
        assert!(matches!(result.cell(0, 11), Some(Cell::Decimal("123.45"))));
        assert_eq!(result.columns()[0], ColumnMeta::new("i", "int2"));
        assert!(result.is_complete());
    }

    #[test]
    fn rows_span_batches_and_nulls_stay_null() {
        let schema = schema(vec![field("n", DataType::Int64, "int8")]);
        let mut result = ArrowResultSet::new(schema.clone());
        for chunk in [vec![Some(1), None], vec![], vec![Some(3)]] {
            let array: ArrayRef = Arc::new(Int64Array::from(chunk));
            result.push(RecordBatch::try_new(schema.clone(), vec![array]).expect("batch"));
        }
        assert!(!result.is_complete());
        assert_eq!(result.row_count(), 3);
        assert_eq!(result.batches().len(), 2);
        assert_eq!(result.cell(0, 0), Some(Cell::Int(1)));
        assert_eq!(result.cell(1, 0), Some(Cell::Null));
        assert_eq!(result.cell(2, 0), Some(Cell::Int(3)));
        assert_eq!(result.cell(3, 0), None);
        assert_eq!(result.cell(0, 1), None);
        assert!(result.memory_bytes() > 0);
        assert_eq!(result.schema().fields().len(), 1);
    }

    #[test]
    fn a_column_without_a_type_name_uses_the_arrow_type() {
        let schema = Arc::new(arrow_schema::Schema::new(vec![arrow_schema::Field::new(
            "x",
            DataType::Int32,
            true,
        )]));
        assert_eq!(ArrowResultSet::new(schema).columns()[0].type_name, "Int32");
    }

    /// The plan's acceptance: 10 million integers in under 100 MB.
    #[test]
    fn ten_million_integers_fit_in_100_mb() {
        let schema = schema(vec![field("n", DataType::Int64, "int8")]);
        let mut result = ArrowResultSet::new(schema.clone());
        for start in (0..10_000_000i64).step_by(100_000) {
            let array: ArrayRef = Arc::new(Int64Array::from_iter_values(start..start + 100_000));
            result.push(RecordBatch::try_new(schema.clone(), vec![array]).expect("batch"));
        }
        assert_eq!(result.row_count(), 10_000_000);
        assert_eq!(result.cell(9_999_999, 0), Some(Cell::Int(9_999_999)));
        assert!(
            result.memory_bytes() < 100 * 1024 * 1024,
            "{}",
            result.memory_bytes()
        );
    }
}
