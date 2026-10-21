//! CQL rows as Arrow columns, typed from the result's column specs before
//! any row arrives: integers as integers, `decimal` and `varint` exact,
//! timestamps in UTC, and collections, tuples and user-defined types as
//! JSON.

use std::sync::Arc;

use arrow_array::{
    builder::{
        BinaryBuilder, BooleanBuilder, Float64Builder, Int64Builder, StringBuilder,
        TimestampMillisecondBuilder,
    },
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, SchemaRef, TimeUnit};
use dfox_core::{
    arrow::{decimal_field, field, json_field, schema},
    errors::DbError,
};
use scylla::{
    frame::response::result::{CollectionType, ColumnSpec, ColumnType, NativeType},
    value::{CqlDate, CqlDuration, CqlTime, CqlValue, Row},
};

/// How a column's values are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bool,
    Int,
    Float,
    /// `decimal` or `varint`, as exact text.
    Decimal,
    Timestamp,
    Blob,
    /// Anything else with a text form: text, dates, times, uuids, addresses.
    Text,
    /// Collections, tuples, user-defined types and vectors.
    Json,
}

fn kind(typ: &ColumnType) -> Kind {
    match typ {
        ColumnType::Native(native) => match native {
            NativeType::Boolean => Kind::Bool,
            NativeType::Int
            | NativeType::BigInt
            | NativeType::SmallInt
            | NativeType::TinyInt
            | NativeType::Counter => Kind::Int,
            NativeType::Float | NativeType::Double => Kind::Float,
            NativeType::Decimal | NativeType::Varint => Kind::Decimal,
            NativeType::Timestamp => Kind::Timestamp,
            NativeType::Blob => Kind::Blob,
            _ => Kind::Text,
        },
        _ => Kind::Json,
    }
}

/// The type as CQL writes it: `int`, `list<text>`, `map<text, int>`.
pub fn type_name(typ: &ColumnType) -> String {
    match typ {
        ColumnType::Native(native) => match native {
            NativeType::Ascii => "ascii",
            NativeType::Boolean => "boolean",
            NativeType::Blob => "blob",
            NativeType::Counter => "counter",
            NativeType::Date => "date",
            NativeType::Decimal => "decimal",
            NativeType::Double => "double",
            NativeType::Duration => "duration",
            NativeType::Float => "float",
            NativeType::Int => "int",
            NativeType::BigInt => "bigint",
            NativeType::Text => "text",
            NativeType::Timestamp => "timestamp",
            NativeType::Inet => "inet",
            NativeType::SmallInt => "smallint",
            NativeType::TinyInt => "tinyint",
            NativeType::Time => "time",
            NativeType::Timeuuid => "timeuuid",
            NativeType::Uuid => "uuid",
            NativeType::Varint => "varint",
            _ => "unknown",
        }
        .to_string(),
        ColumnType::Collection { typ, .. } => match typ {
            CollectionType::List(item) => format!("list<{}>", type_name(item)),
            CollectionType::Set(item) => format!("set<{}>", type_name(item)),
            CollectionType::Map(key, value) => {
                format!("map<{}, {}>", type_name(key), type_name(value))
            }
            _ => "collection".to_string(),
        },
        ColumnType::Vector { typ, dimensions } => {
            format!("vector<{}, {dimensions}>", type_name(typ))
        }
        ColumnType::UserDefinedType { definition, .. } => definition.name.to_string(),
        ColumnType::Tuple(items) => {
            let items: Vec<String> = items.iter().map(type_name).collect();
            format!("tuple<{}>", items.join(", "))
        }
        _ => "unknown".to_string(),
    }
}

/// The columns of a result, from its column specs.
#[derive(Debug, Clone)]
pub struct Columns {
    kinds: Vec<Kind>,
    schema: SchemaRef,
}

impl Columns {
    pub fn new(specs: &[ColumnSpec<'_>]) -> Self {
        let kinds: Vec<Kind> = specs.iter().map(|s| kind(s.typ())).collect();
        let fields = specs
            .iter()
            .zip(&kinds)
            .map(|(spec, kind)| {
                let (name, type_name) = (spec.name(), type_name(spec.typ()));
                match kind {
                    Kind::Bool => field(name, DataType::Boolean, &type_name),
                    Kind::Int => field(name, DataType::Int64, &type_name),
                    Kind::Float => field(name, DataType::Float64, &type_name),
                    Kind::Decimal => decimal_field(name, &type_name),
                    Kind::Timestamp => field(
                        name,
                        DataType::Timestamp(TimeUnit::Millisecond, Some("+00:00".into())),
                        &type_name,
                    ),
                    Kind::Blob => field(name, DataType::Binary, &type_name),
                    Kind::Json => json_field(name, &type_name),
                    Kind::Text => field(name, DataType::Utf8, &type_name),
                }
            })
            .collect();
        Self {
            kinds,
            schema: schema(fields),
        }
    }

    pub fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    pub fn batch(&self, rows: &[Row]) -> Result<RecordBatch, DbError> {
        let columns: Vec<ArrayRef> = self
            .kinds
            .iter()
            .enumerate()
            .map(|(i, kind)| column(rows, i, *kind))
            .collect();
        RecordBatch::try_new(self.schema.clone(), columns)
            .map_err(|err| DbError::General(format!("couldn't build the result: {err}")))
    }
}

fn column(rows: &[Row], i: usize, kind: Kind) -> ArrayRef {
    let values = rows.iter().map(|row| match row.columns.get(i) {
        Some(Some(CqlValue::Empty)) | Some(None) | None => None,
        Some(Some(value)) => Some(value),
    });
    match kind {
        Kind::Bool => {
            let mut b = BooleanBuilder::with_capacity(rows.len());
            for v in values {
                b.append_option(v.and_then(CqlValue::as_boolean));
            }
            Arc::new(b.finish())
        }
        Kind::Int => {
            let mut b = Int64Builder::with_capacity(rows.len());
            for v in values {
                b.append_option(v.and_then(|v| match v {
                    CqlValue::Int(n) => Some(i64::from(*n)),
                    CqlValue::BigInt(n) => Some(*n),
                    CqlValue::SmallInt(n) => Some(i64::from(*n)),
                    CqlValue::TinyInt(n) => Some(i64::from(*n)),
                    CqlValue::Counter(c) => Some(c.0),
                    _ => None,
                }));
            }
            Arc::new(b.finish())
        }
        Kind::Float => {
            let mut b = Float64Builder::with_capacity(rows.len());
            for v in values {
                b.append_option(v.and_then(|v| match v {
                    CqlValue::Float(n) => Some(f64::from(*n)),
                    CqlValue::Double(n) => Some(*n),
                    _ => None,
                }));
            }
            Arc::new(b.finish())
        }
        Kind::Timestamp => {
            let mut b =
                TimestampMillisecondBuilder::with_capacity(rows.len()).with_timezone("+00:00");
            for v in values {
                b.append_option(v.and_then(|v| match v {
                    CqlValue::Timestamp(t) => Some(t.0),
                    _ => None,
                }));
            }
            Arc::new(b.finish())
        }
        Kind::Blob => {
            let mut b = BinaryBuilder::new();
            for v in values {
                b.append_option(v.and_then(|v| match v {
                    CqlValue::Blob(bytes) => Some(bytes.as_slice()),
                    _ => None,
                }));
            }
            Arc::new(b.finish())
        }
        Kind::Json => {
            let mut b = StringBuilder::new();
            for v in values {
                b.append_option(v.map(|v| json(v).to_string()));
            }
            Arc::new(b.finish())
        }
        Kind::Decimal | Kind::Text => {
            let mut b = StringBuilder::new();
            for v in values {
                b.append_option(v.map(text));
            }
            Arc::new(b.finish())
        }
    }
}

/// A value as CQL prints it.
pub fn text(value: &CqlValue) -> String {
    match value {
        CqlValue::Ascii(s) | CqlValue::Text(s) => s.clone(),
        CqlValue::Boolean(b) => b.to_string(),
        CqlValue::Int(n) => n.to_string(),
        CqlValue::BigInt(n) => n.to_string(),
        CqlValue::SmallInt(n) => n.to_string(),
        CqlValue::TinyInt(n) => n.to_string(),
        CqlValue::Counter(c) => c.0.to_string(),
        CqlValue::Float(n) => n.to_string(),
        CqlValue::Double(n) => n.to_string(),
        CqlValue::Varint(v) => varint(v.as_signed_bytes_be_slice()),
        CqlValue::Decimal(d) => {
            let (digits, scale) = d.as_signed_be_bytes_slice_and_exponent();
            decimal(&varint(digits), scale)
        }
        CqlValue::Date(d) => date(*d),
        CqlValue::Time(t) => time(*t),
        CqlValue::Timestamp(t) => t.0.to_string(),
        CqlValue::Duration(d) => duration(*d),
        CqlValue::Uuid(u) => u.to_string(),
        CqlValue::Timeuuid(u) => u.to_string(),
        CqlValue::Inet(ip) => ip.to_string(),
        CqlValue::Blob(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            format!("0x{hex}")
        }
        CqlValue::Empty => String::new(),
        other => json(other).to_string(),
    }
}

/// A value as JSON: collections as arrays, maps and user-defined types as
/// objects.
pub fn json(value: &CqlValue) -> serde_json::Value {
    use serde_json::Value;
    let item = |v: &Option<CqlValue>| v.as_ref().map_or(Value::Null, json);
    match value {
        CqlValue::Boolean(b) => Value::Bool(*b),
        CqlValue::Int(n) => Value::from(*n),
        CqlValue::BigInt(n) => Value::from(*n),
        CqlValue::SmallInt(n) => Value::from(*n),
        CqlValue::TinyInt(n) => Value::from(*n),
        CqlValue::Counter(c) => Value::from(c.0),
        CqlValue::Float(n) => {
            serde_json::Number::from_f64(f64::from(*n)).map_or(Value::Null, Value::Number)
        }
        CqlValue::Double(n) => serde_json::Number::from_f64(*n).map_or(Value::Null, Value::Number),
        CqlValue::List(items) | CqlValue::Set(items) | CqlValue::Vector(items) => {
            Value::Array(items.iter().map(json).collect())
        }
        CqlValue::Tuple(items) => Value::Array(items.iter().map(item).collect()),
        CqlValue::Map(pairs) => {
            // JSON keys are text; other keys keep their text form.
            Value::Object(pairs.iter().map(|(k, v)| (text(k), json(v))).collect())
        }
        CqlValue::UserDefinedType { fields, .. } => Value::Object(
            fields
                .iter()
                .map(|(name, v)| (name.clone(), item(v)))
                .collect(),
        ),
        CqlValue::Empty => Value::Null,
        other => Value::String(text(other)),
    }
}

/// A two's-complement big-endian integer in decimal.
fn varint(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "0".to_string();
    }
    let negative = bytes[0] & 0x80 != 0;
    // The magnitude, as base-256 digits.
    let mut digits: Vec<u8> = bytes.to_vec();
    if negative {
        for b in &mut digits {
            *b = !*b;
        }
        for b in digits.iter_mut().rev() {
            let (sum, carry) = b.overflowing_add(1);
            *b = sum;
            if !carry {
                break;
            }
        }
    }
    // Repeated division by a billion, nine decimal digits at a time.
    let mut groups = Vec::new();
    while digits.iter().any(|d| *d != 0) {
        let mut rest = 0u64;
        for d in &mut digits {
            let n = (rest << 8) | u64::from(*d);
            *d = (n / 1_000_000_000) as u8;
            rest = n % 1_000_000_000;
        }
        groups.push(rest);
    }
    let mut out = match groups.pop() {
        Some(top) => top.to_string(),
        None => "0".to_string(),
    };
    for group in groups.iter().rev() {
        out.push_str(&format!("{group:09}"));
    }
    if negative && out != "0" {
        out.insert(0, '-');
    }
    out
}

/// `unscaled` × 10^-`scale`, written out.
fn decimal(unscaled: &str, scale: i32) -> String {
    let (sign, digits) = match unscaled.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", unscaled),
    };
    if scale <= 0 {
        let zeros = "0".repeat(scale.unsigned_abs() as usize);
        return format!("{sign}{digits}{zeros}");
    }
    let scale = scale as usize;
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (whole, fraction) = padded.split_at(padded.len() - scale);
    format!("{sign}{whole}.{fraction}")
}

/// `2024-01-02`: days counted from 2^31 before the epoch.
fn date(d: CqlDate) -> String {
    let days = i64::from(d.0) - (1 << 31);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `13:45:00.123`: nanoseconds since midnight.
fn time(t: CqlTime) -> String {
    let nanos = t.0;
    let seconds = nanos / 1_000_000_000;
    let fraction = nanos % 1_000_000_000;
    let mut out = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    if fraction > 0 {
        let digits = format!("{fraction:09}");
        out.push('.');
        out.push_str(digits.trim_end_matches('0'));
    }
    out
}

/// `1mo2d3h4m5s`, as CQL writes durations.
fn duration(d: CqlDuration) -> String {
    let mut out = String::new();
    if d.months != 0 {
        out.push_str(&format!("{}mo", d.months));
    }
    if d.days != 0 {
        out.push_str(&format!("{}d", d.days));
    }
    let mut nanos = d.nanoseconds;
    for (unit, size) in [
        ("h", 3_600_000_000_000),
        ("m", 60_000_000_000),
        ("s", 1_000_000_000),
        ("ms", 1_000_000),
        ("us", 1_000),
        ("ns", 1),
    ] {
        if nanos.abs() >= size {
            out.push_str(&format!("{}{unit}", nanos / size));
            nanos %= size;
        }
    }
    if out.is_empty() {
        out.push('0');
    }
    out
}

#[cfg(test)]
mod tests {
    use scylla::value::CqlVarint;

    use super::*;

    #[test]
    fn numbers_dates_and_times_print_as_cql_does() {
        assert_eq!(varint(&[0x01, 0x00]), "256");
        assert_eq!(varint(&[0xff]), "-1");
        assert_eq!(varint(&[0x80]), "-128");
        assert_eq!(varint(&[0x00]), "0");
        // 2^70
        let big = CqlVarint::from_signed_bytes_be(vec![0x40, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(text(&CqlValue::Varint(big)), "1180591620717411303424");
        assert_eq!(decimal("12345", 2), "123.45");
        assert_eq!(decimal("-5", 3), "-0.005");
        assert_eq!(decimal("7", -2), "700");
        assert_eq!(date(CqlDate(1 << 31)), "1970-01-01");
        assert_eq!(date(CqlDate((1 << 31) + 19_724)), "2024-01-02");
        assert_eq!(date(CqlDate((1 << 31) - 1)), "1969-12-31");
        assert_eq!(time(CqlTime(49_500_123_000_000)), "13:45:00.123");
        assert_eq!(
            duration(CqlDuration {
                months: 1,
                days: 2,
                nanoseconds: 3_723_000_000_000
            }),
            "1mo2d1h2m3s"
        );
        let map = CqlValue::Map(vec![(CqlValue::Text("a".into()), CqlValue::Int(1))]);
        assert_eq!(json(&map).to_string(), r#"{"a":1}"#);
    }
}
