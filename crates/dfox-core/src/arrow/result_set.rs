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
use arrow_schema::{DataType, IntervalUnit, SchemaRef, TimeUnit};

use super::{decimal_field, field, json_field, schema, DECIMAL, JSON, TYPE_NAME};
use crate::result::{Cell, ColumnMeta, OwnedCell, ResultSet};

fn present(cell: &OwnedCell) -> bool {
    !matches!(cell, OwnedCell::Null)
}

/// Text of the values of one batch column that have no borrowable form, such
/// as decimals and dates, made the first time each is read.
type Shown = OnceLock<Vec<OnceLock<Box<str>>>>;

/// What a text column holds, from its field metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextKind {
    Plain,
    Json,
    /// An exact decimal kept as text, so no digits are lost.
    Decimal,
}

/// Dates and times as Postgres and most databases print them.
const FORMAT: FormatOptions<'static> = FormatOptions::new()
    .with_timestamp_format(Some("%Y-%m-%d %H:%M:%S%.f"))
    .with_timestamp_tz_format(Some("%Y-%m-%d %H:%M:%S%.f%:z"))
    .with_time_format(Some("%H:%M:%S%.f"));

/// Rows held as Arrow batches. Numbers, booleans, text and bytes are read
/// straight from the arrays; other types are formatted once, when first
/// read, so only the cells on screen are ever turned into text.
#[derive(Debug)]
pub struct ArrowResultSet {
    schema: SchemaRef,
    columns: Vec<ColumnMeta>,
    kinds: Vec<TextKind>,
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
        let kinds = schema
            .fields()
            .iter()
            .map(|f| {
                let flag = |key| f.metadata().get(key).is_some_and(|v| v == "true");
                if flag(JSON) {
                    TextKind::Json
                } else if flag(DECIMAL) {
                    TextKind::Decimal
                } else {
                    TextKind::Plain
                }
            })
            .collect();
        Self {
            schema,
            columns,
            kinds,
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

    /// A complete result from rows of cells, such as one the app makes up
    /// itself. A column takes the type its cells share, or text when they
    /// differ.
    pub fn from_cells(columns: Vec<ColumnMeta>, rows: Vec<Vec<OwnedCell>>) -> Self {
        use std::sync::Arc;

        use arrow_array::{
            ArrayRef, BinaryArray, BooleanArray, Float64Array, Int64Array, StringArray, UInt64Array,
        };

        let mut fields = Vec::new();
        let mut arrays: Vec<ArrayRef> = Vec::new();
        for (i, column) in columns.iter().enumerate() {
            let cells: Vec<&OwnedCell> = rows.iter().map(|row| &row[i]).collect();
            let kinds: Vec<std::mem::Discriminant<OwnedCell>> = cells
                .iter()
                .filter(|c| present(c))
                .map(|c| std::mem::discriminant(*c))
                .collect();
            let shared = kinds.windows(2).all(|w| w[0] == w[1]);
            let first = cells.iter().copied().find(|c| present(c));
            let (name, type_name) = (column.name.as_str(), column.type_name.as_str());
            let (f, array): (_, ArrayRef) = match first {
                Some(OwnedCell::Bool(_)) if shared => (
                    field(name, DataType::Boolean, type_name),
                    Arc::new(BooleanArray::from_iter(cells.iter().map(|c| match c {
                        OwnedCell::Bool(b) => Some(*b),
                        _ => None,
                    }))),
                ),
                Some(OwnedCell::Int(_)) if shared => (
                    field(name, DataType::Int64, type_name),
                    Arc::new(Int64Array::from_iter(cells.iter().map(|c| match c {
                        OwnedCell::Int(n) => Some(*n),
                        _ => None,
                    }))),
                ),
                Some(OwnedCell::UInt(_)) if shared => (
                    field(name, DataType::UInt64, type_name),
                    Arc::new(UInt64Array::from_iter(cells.iter().map(|c| match c {
                        OwnedCell::UInt(n) => Some(*n),
                        _ => None,
                    }))),
                ),
                Some(OwnedCell::Float(_)) if shared => (
                    field(name, DataType::Float64, type_name),
                    Arc::new(Float64Array::from_iter(cells.iter().map(|c| match c {
                        OwnedCell::Float(f) => Some(*f),
                        _ => None,
                    }))),
                ),
                Some(OwnedCell::Bytes(_)) if shared => (
                    field(name, DataType::Binary, type_name),
                    Arc::new(BinaryArray::from_iter(cells.iter().map(|c| match c {
                        OwnedCell::Bytes(b) => Some(b.as_ref()),
                        _ => None,
                    }))),
                ),
                first => {
                    let f = match first {
                        Some(OwnedCell::Decimal(_)) if shared => decimal_field(name, type_name),
                        Some(OwnedCell::Json(_)) if shared => json_field(name, type_name),
                        _ => field(name, DataType::Utf8, type_name),
                    };
                    let text = StringArray::from_iter(
                        cells
                            .iter()
                            .map(|c| present(c).then(|| c.as_cell().to_string())),
                    );
                    (f, Arc::new(text))
                }
            };
            fields.push(f);
            arrays.push(array);
        }
        let schema = schema(fields);
        let mut result = Self::new(schema.clone());
        if !rows.is_empty() {
            if let Ok(batch) = RecordBatch::try_new(schema, arrays) {
                result.push(batch);
            }
        }
        result.finish();
        result
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
        column[index]
            .get_or_init(|| format_value(array, index, &self.columns[col].type_name).into())
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
        let text = |s| match self.kinds[col] {
            TextKind::Plain => Cell::Text(s),
            TextKind::Json => Cell::Json(s),
            TextKind::Decimal => Cell::Decimal(s),
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

/// The text of a value with no borrowable form. `infinity` dates and
/// timestamps are stored as the type's extremes; UUIDs as 16 bytes.
fn format_value(array: &dyn Array, index: usize, type_name: &str) -> String {
    let infinite = |max: bool| if max { "infinity" } else { "-infinity" }.to_string();
    match array.data_type() {
        DataType::Date32 => {
            let days = array
                .as_primitive::<arrow_array::types::Date32Type>()
                .value(index);
            if days == i32::MAX || days == i32::MIN {
                return infinite(days == i32::MAX);
            }
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let micros = array
                .as_primitive::<arrow_array::types::TimestampMicrosecondType>()
                .value(index);
            if micros == i64::MAX || micros == i64::MIN {
                return infinite(micros == i64::MAX);
            }
        }
        DataType::Interval(IntervalUnit::MonthDayNano) => {
            let value = array
                .as_primitive::<arrow_array::types::IntervalMonthDayNanoType>()
                .value(index);
            return interval(value.months, value.days, value.nanoseconds / 1000);
        }
        DataType::FixedSizeBinary(16) if type_name.eq_ignore_ascii_case("uuid") => {
            let b = array.as_fixed_size_binary().value(index);
            let hex: String = b.iter().map(|b| format!("{b:02x}")).collect();
            return format!(
                "{}-{}-{}-{}-{}",
                &hex[0..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..32]
            );
        }
        _ => {}
    }
    ArrayFormatter::try_new(array, &FORMAT)
        .map(|f| f.value(index).to_string())
        .unwrap_or_else(|err| format!("⟨{err}⟩"))
}

/// An interval the way Postgres prints one: `1 year 2 mons 3 days 04:05:06`.
pub(crate) fn interval(months: i32, days: i32, micros: i64) -> String {
    let mut parts = Vec::new();
    let (years, months) = (months / 12, months % 12);
    let plural = |n: i64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    if years != 0 {
        parts.push(plural(years.into(), "year", "years"));
    }
    if months != 0 {
        parts.push(plural(months.into(), "mon", "mons"));
    }
    if days != 0 {
        parts.push(plural(days.into(), "day", "days"));
    }
    if micros != 0 || parts.is_empty() {
        let sign = if micros < 0 { "-" } else { "" };
        let total = micros.unsigned_abs();
        let (secs, frac) = (total / 1_000_000, total % 1_000_000);
        let mut time = format!(
            "{sign}{:02}:{:02}:{:02}",
            secs / 3600,
            secs % 3600 / 60,
            secs % 60
        );
        if frac != 0 {
            time.push_str(format!(".{frac:06}").trim_end_matches('0'));
        }
        parts.push(time);
    }
    parts.join(" ")
}

#[cfg(test)]
mod from_cells_tests {
    use super::*;

    #[test]
    fn cells_become_typed_columns_or_text_when_mixed() {
        let result = ArrowResultSet::from_cells(
            vec![
                ColumnMeta::new("n", "int"),
                ColumnMeta::new("mixed", ""),
                ColumnMeta::new("price", "numeric"),
            ],
            vec![
                vec![
                    OwnedCell::Int(1),
                    OwnedCell::Int(1),
                    OwnedCell::Decimal("1.50".into()),
                ],
                vec![
                    OwnedCell::Null,
                    OwnedCell::Text("x".into()),
                    OwnedCell::Null,
                ],
            ],
        );
        assert!(result.is_complete());
        assert_eq!(result.row_count(), 2);
        assert!(matches!(result.cell(0, 0), Some(Cell::Int(1))));
        assert!(matches!(result.cell(1, 0), Some(Cell::Null)));
        assert!(matches!(result.cell(0, 1), Some(Cell::Text("1"))));
        assert!(matches!(result.cell(0, 2), Some(Cell::Decimal("1.50"))));
        assert_eq!(result.columns()[2].type_name, "numeric");
        let empty = ArrowResultSet::from_cells(vec![ColumnMeta::new("a", "")], Vec::new());
        assert_eq!(empty.row_count(), 0);
        assert_eq!(empty.columns().len(), 1);
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
                "2023-11-14 22:13:20",
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
    fn database_values_print_as_the_database_prints_them() {
        use arrow_array::types::IntervalMonthDayNano;
        use arrow_array::{Date32Array, FixedSizeBinaryArray, IntervalMonthDayNanoArray};
        let uuid = FixedSizeBinaryArray::try_from_iter(
            [[
                0xa0, 0xee, 0xbc, 0x99, 0x9c, 0x0b, 0x4e, 0xf8, 0xbb, 0x6d, 0x6b, 0xb9, 0xbd, 0x38,
                0x0a, 0x11,
            ]]
            .into_iter(),
        )
        .expect("uuid");
        let result = result(
            vec![
                crate::arrow::decimal_field("price", "numeric"),
                field("id", DataType::FixedSizeBinary(16), "uuid"),
                field("ever", DataType::Date32, "date"),
                field("never", DataType::Date32, "date"),
                field(
                    "at",
                    DataType::Timestamp(arrow_schema::TimeUnit::Microsecond, None),
                    "timestamp",
                ),
                field(
                    "took",
                    DataType::Interval(arrow_schema::IntervalUnit::MonthDayNano),
                    "interval",
                ),
            ],
            vec![
                Arc::new(StringArray::from(vec!["-12.50"])),
                Arc::new(uuid),
                Arc::new(Date32Array::from(vec![i32::MAX])),
                Arc::new(Date32Array::from(vec![i32::MIN])),
                Arc::new(TimestampMicrosecondArray::from(vec![i64::MAX])),
                Arc::new(IntervalMonthDayNanoArray::from(vec![
                    IntervalMonthDayNano::new(
                        14,
                        3,
                        (4 * 3600 + 5 * 60 + 6) * 1_000_000_000 + 500_000_000,
                    ),
                ])),
            ],
        );
        let cells: Vec<String> = (0..6)
            .map(|col| result.cell(0, col).expect("cell").to_string())
            .collect();
        assert_eq!(
            cells,
            [
                "-12.50",
                "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
                "infinity",
                "-infinity",
                "infinity",
                "1 year 2 mons 3 days 04:05:06.5",
            ]
        );
        assert!(result.cell(0, 0).is_some_and(|c| c.is_numeric()));
    }

    #[test]
    fn intervals_read_like_postgres() {
        assert_eq!(interval(0, 0, 0), "00:00:00");
        assert_eq!(interval(0, 1, 0), "1 day");
        assert_eq!(interval(-12, -2, 0), "-1 years -2 days");
        assert_eq!(interval(0, 0, -90_000_000), "-00:01:30");
        assert_eq!(interval(1, 0, 3_600_000_123), "1 mon 01:00:00.000123");
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
