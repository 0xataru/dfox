//! Postgres's binary wire format, decoded straight into Arrow columns.
//!
//! Each result column gets a [`Column`] chosen from its type: numbers stay
//! numbers, dates stay dates, `numeric` stays exact (as text tagged decimal),
//! arrays become lists, enums and domains decode as their labels and base
//! types. A type dfox doesn't know keeps its raw bytes, with its name in the
//! field, so nothing is ever shown as NULL that isn't.

use std::{net::Ipv6Addr, sync::Arc};

use arrow_array::{
    builder::{
        BinaryBuilder, BooleanBuilder, Date32Builder, FixedSizeBinaryBuilder, Float32Builder,
        Float64Builder, Int16Builder, Int32Builder, Int64Builder, IntervalMonthDayNanoBuilder,
        StringBuilder, Time64MicrosecondBuilder, TimestampMicrosecondBuilder, UInt32Builder,
    },
    types::IntervalMonthDayNano,
    ArrayRef, ListArray,
};
use arrow_buffer::{NullBufferBuilder, OffsetBuffer};
use arrow_schema::{DataType, Field, TimeUnit};
use dfox_core::{
    arrow::{decimal_field, field, json_field},
    errors::DbError,
};
use tokio_postgres::types::{Kind, Type};

/// Days from 1970-01-01, Arrow's epoch, to 2000-01-01, Postgres's.
const EPOCH_DAYS: i32 = 10_957;
/// The same in microseconds.
const EPOCH_MICROS: i64 = 946_684_800_000_000;

/// Builds one Arrow column from a Postgres column's binary values.
pub enum Column {
    Bool(BooleanBuilder),
    Int2(Int16Builder),
    Int4(Int32Builder),
    Int8(Int64Builder),
    Oid(UInt32Builder),
    Float4(Float32Builder),
    Float8(Float64Builder),
    /// UTF-8 text as sent: text, varchar, char, name, enum labels, json, xml.
    Text(StringBuilder),
    /// `jsonb`: a version byte, then the text.
    Jsonb(StringBuilder),
    /// `numeric`, as exact decimal text.
    Numeric(StringBuilder),
    /// `money`, in cents, as decimal text.
    Money(StringBuilder),
    /// The single-byte `"char"` type.
    Char(StringBuilder),
    /// Values with their own text form: inet, cidr, macaddr, timetz, bit, point.
    Formatted(StringBuilder, fn(&[u8]) -> Result<String, String>),
    Bytes(BinaryBuilder),
    Uuid(FixedSizeBinaryBuilder),
    Date(Date32Builder),
    Time(Time64MicrosecondBuilder),
    Timestamp(TimestampMicrosecondBuilder),
    TimestampTz(TimestampMicrosecondBuilder),
    Interval(IntervalMonthDayNanoBuilder),
    /// An array of any of these, flattened to one dimension.
    List {
        item: Box<Column>,
        item_field: Arc<Field>,
        offsets: Vec<i32>,
        nulls: NullBufferBuilder,
    },
    /// A type dfox doesn't decode: its bytes.
    Raw(BinaryBuilder),
}

/// The field and the builder for a column of type `ty`, named `name`.
pub fn column(name: &str, ty: &Type) -> (Field, Column) {
    let type_name = ty.name();
    let kind = ty.kind();
    // Domains decode as their base type, enums as their labels.
    if let Kind::Domain(base) = kind {
        let (field, column) = column(name, base);
        return (field_named(field, type_name), column);
    }
    if let Kind::Enum(_) = kind {
        return (
            field(name, DataType::Utf8, type_name),
            Column::Text(StringBuilder::new()),
        );
    }
    if let Kind::Array(element) = kind {
        let (item_field, item) = column("item", element);
        let item_field = Arc::new(item_field);
        return (
            field(name, DataType::List(item_field.clone()), type_name),
            Column::List {
                item: Box::new(item),
                item_field,
                offsets: vec![0],
                nulls: NullBufferBuilder::new(0),
            },
        );
    }
    let text = || Column::Text(StringBuilder::new());
    let formatted = |f| Column::Formatted(StringBuilder::new(), f);
    let (data_type, builder) = match *ty {
        Type::BOOL => (DataType::Boolean, Column::Bool(BooleanBuilder::new())),
        Type::INT2 => (DataType::Int16, Column::Int2(Int16Builder::new())),
        Type::INT4 => (DataType::Int32, Column::Int4(Int32Builder::new())),
        Type::INT8 => (DataType::Int64, Column::Int8(Int64Builder::new())),
        Type::OID => (DataType::UInt32, Column::Oid(UInt32Builder::new())),
        Type::FLOAT4 => (DataType::Float32, Column::Float4(Float32Builder::new())),
        Type::FLOAT8 => (DataType::Float64, Column::Float8(Float64Builder::new())),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::XML | Type::UNKNOWN => {
            (DataType::Utf8, text())
        }
        Type::JSON => return (json_field(name, type_name), text()),
        Type::JSONB => {
            return (
                json_field(name, type_name),
                Column::Jsonb(StringBuilder::new()),
            )
        }
        Type::NUMERIC => {
            return (
                decimal_field(name, type_name),
                Column::Numeric(StringBuilder::new()),
            )
        }
        Type::MONEY => {
            return (
                decimal_field(name, type_name),
                Column::Money(StringBuilder::new()),
            )
        }
        Type::CHAR => (DataType::Utf8, Column::Char(StringBuilder::new())),
        Type::INET | Type::CIDR => (DataType::Utf8, formatted(inet)),
        Type::MACADDR | Type::MACADDR8 => (DataType::Utf8, formatted(macaddr)),
        Type::TIMETZ => (DataType::Utf8, formatted(timetz)),
        Type::BIT | Type::VARBIT => (DataType::Utf8, formatted(bits)),
        Type::POINT => (DataType::Utf8, formatted(point)),
        Type::BYTEA => (DataType::Binary, Column::Bytes(BinaryBuilder::new())),
        Type::UUID => (
            DataType::FixedSizeBinary(16),
            Column::Uuid(FixedSizeBinaryBuilder::new(16)),
        ),
        Type::DATE => (DataType::Date32, Column::Date(Date32Builder::new())),
        Type::TIME => (
            DataType::Time64(TimeUnit::Microsecond),
            Column::Time(Time64MicrosecondBuilder::new()),
        ),
        Type::TIMESTAMP => (
            DataType::Timestamp(TimeUnit::Microsecond, None),
            Column::Timestamp(TimestampMicrosecondBuilder::new()),
        ),
        Type::TIMESTAMPTZ => (
            DataType::Timestamp(TimeUnit::Microsecond, Some("+00:00".into())),
            Column::TimestampTz(TimestampMicrosecondBuilder::new().with_timezone("+00:00")),
        ),
        Type::INTERVAL => (
            DataType::Interval(arrow_schema::IntervalUnit::MonthDayNano),
            Column::Interval(IntervalMonthDayNanoBuilder::new()),
        ),
        // Extensions whose binary form is their text.
        _ if matches!(type_name, "citext" | "ltree" | "lquery") && ty.oid() > 16_383 => {
            (DataType::Utf8, text())
        }
        _ => (DataType::Binary, Column::Raw(BinaryBuilder::new())),
    };
    (field(name, data_type, type_name), builder)
}

/// `field` with the database type name `type_name`.
fn field_named(field: Field, type_name: &str) -> Field {
    let mut metadata = field.metadata().clone();
    metadata.insert(
        dfox_core::arrow::TYPE_NAME.to_string(),
        type_name.to_string(),
    );
    field.with_metadata(metadata)
}

fn error(what: &str) -> DbError {
    DbError::General(format!("malformed {what} value from the server"))
}

fn take<const N: usize>(raw: &[u8], what: &str) -> Result<[u8; N], DbError> {
    raw.get(..N)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| error(what))
}

fn utf8<'a>(raw: &'a [u8], what: &str) -> Result<&'a str, DbError> {
    std::str::from_utf8(raw).map_err(|_| error(what))
}

impl Column {
    /// Adds one value: its binary bytes, or `None` for NULL.
    pub fn push(&mut self, raw: Option<&[u8]>) -> Result<(), DbError> {
        let Some(raw) = raw else {
            self.push_null();
            return Ok(());
        };
        match self {
            Column::Bool(b) => b.append_value(take::<1>(raw, "bool")?[0] != 0),
            Column::Int2(b) => b.append_value(i16::from_be_bytes(take(raw, "int2")?)),
            Column::Int4(b) => b.append_value(i32::from_be_bytes(take(raw, "int4")?)),
            Column::Int8(b) => b.append_value(i64::from_be_bytes(take(raw, "int8")?)),
            Column::Oid(b) => b.append_value(u32::from_be_bytes(take(raw, "oid")?)),
            Column::Float4(b) => b.append_value(f32::from_be_bytes(take(raw, "float4")?)),
            Column::Float8(b) => b.append_value(f64::from_be_bytes(take(raw, "float8")?)),
            Column::Text(b) => b.append_value(utf8(raw, "text")?),
            Column::Jsonb(b) => match raw.split_first() {
                Some((1, json)) => b.append_value(utf8(json, "jsonb")?),
                _ => return Err(error("jsonb")),
            },
            Column::Numeric(b) => b.append_value(numeric(raw).ok_or_else(|| error("numeric"))?),
            Column::Money(b) => b.append_value(money(i64::from_be_bytes(take(raw, "money")?))),
            Column::Char(b) => b.append_value(char::from(take::<1>(raw, "char")?[0]).to_string()),
            Column::Formatted(b, format) => {
                b.append_value(format(raw).map_err(|what| error(&what))?)
            }
            Column::Bytes(b) => b.append_value(raw),
            Column::Uuid(b) => b
                .append_value(take::<16>(raw, "uuid")?)
                .map_err(|_| error("uuid"))?,
            Column::Date(b) => {
                let days = i32::from_be_bytes(take(raw, "date")?);
                // ±infinity keep the extremes, which the result set prints as such.
                b.append_value(match days {
                    i32::MAX | i32::MIN => days,
                    days => days.saturating_add(EPOCH_DAYS),
                })
            }
            Column::Time(b) => b.append_value(i64::from_be_bytes(take(raw, "time")?)),
            Column::Timestamp(b) | Column::TimestampTz(b) => {
                let micros = i64::from_be_bytes(take(raw, "timestamp")?);
                b.append_value(match micros {
                    i64::MAX | i64::MIN => micros,
                    micros => micros.saturating_add(EPOCH_MICROS),
                })
            }
            Column::Interval(b) => {
                let bytes = take::<16>(raw, "interval")?;
                let micros = i64::from_be_bytes(take(&bytes[0..8], "interval")?);
                let days = i32::from_be_bytes(take(&bytes[8..12], "interval")?);
                let months = i32::from_be_bytes(take(&bytes[12..16], "interval")?);
                b.append_value(IntervalMonthDayNano::new(
                    months,
                    days,
                    micros.saturating_mul(1000),
                ))
            }
            Column::List {
                item,
                offsets,
                nulls,
                ..
            } => {
                let count = array_items(raw, item)?;
                let last = offsets.last().copied().unwrap_or(0);
                offsets.push(last + count);
                nulls.append_non_null();
            }
            Column::Raw(b) => b.append_value(raw),
        }
        Ok(())
    }

    fn push_null(&mut self) {
        match self {
            Column::Bool(b) => b.append_null(),
            Column::Int2(b) => b.append_null(),
            Column::Int4(b) => b.append_null(),
            Column::Int8(b) => b.append_null(),
            Column::Oid(b) => b.append_null(),
            Column::Float4(b) => b.append_null(),
            Column::Float8(b) => b.append_null(),
            Column::Text(b)
            | Column::Jsonb(b)
            | Column::Numeric(b)
            | Column::Money(b)
            | Column::Char(b)
            | Column::Formatted(b, _) => b.append_null(),
            Column::Bytes(b) | Column::Raw(b) => b.append_null(),
            Column::Uuid(b) => b.append_null(),
            Column::Date(b) => b.append_null(),
            Column::Time(b) => b.append_null(),
            Column::Timestamp(b) | Column::TimestampTz(b) => b.append_null(),
            Column::Interval(b) => b.append_null(),
            Column::List { offsets, nulls, .. } => {
                let last = offsets.last().copied().unwrap_or(0);
                offsets.push(last);
                nulls.append_null();
            }
        }
    }

    /// The column so far, as an Arrow array; the builder starts over.
    pub fn finish(&mut self) -> ArrayRef {
        match self {
            Column::Bool(b) => Arc::new(b.finish()),
            Column::Int2(b) => Arc::new(b.finish()),
            Column::Int4(b) => Arc::new(b.finish()),
            Column::Int8(b) => Arc::new(b.finish()),
            Column::Oid(b) => Arc::new(b.finish()),
            Column::Float4(b) => Arc::new(b.finish()),
            Column::Float8(b) => Arc::new(b.finish()),
            Column::Text(b)
            | Column::Jsonb(b)
            | Column::Numeric(b)
            | Column::Money(b)
            | Column::Char(b)
            | Column::Formatted(b, _) => Arc::new(b.finish()),
            Column::Bytes(b) | Column::Raw(b) => Arc::new(b.finish()),
            Column::Uuid(b) => Arc::new(b.finish()),
            Column::Date(b) => Arc::new(b.finish()),
            Column::Time(b) => Arc::new(b.finish()),
            Column::Timestamp(b) | Column::TimestampTz(b) => Arc::new(b.finish()),
            Column::Interval(b) => Arc::new(b.finish()),
            Column::List {
                item,
                item_field,
                offsets,
                nulls,
            } => {
                let values = item.finish();
                let offsets = OffsetBuffer::new(std::mem::replace(offsets, vec![0]).into());
                // Offsets and values are built together, so they always agree.
                Arc::new(ListArray::new(
                    item_field.clone(),
                    offsets,
                    values,
                    nulls.finish(),
                ))
            }
        }
    }
}

/// Reads an array's binary form into `item`, flattening dimensions. Returns
/// how many items it added.
fn array_items(raw: &[u8], item: &mut Column) -> Result<i32, DbError> {
    let int = |at: usize| -> Result<i32, DbError> {
        Ok(i32::from_be_bytes(take(
            raw.get(at..).unwrap_or_default(),
            "array",
        )?))
    };
    let dimensions = int(0)?;
    if dimensions == 0 {
        return Ok(0);
    }
    let mut count: i32 = 1;
    for d in 0..dimensions as usize {
        count = count.saturating_mul(int(12 + d * 8)?);
    }
    let mut at = 12 + dimensions as usize * 8;
    for _ in 0..count {
        let len = int(at)?;
        at += 4;
        if len < 0 {
            item.push(None)?;
        } else {
            let end = at + len as usize;
            item.push(Some(raw.get(at..end).ok_or_else(|| error("array"))?))?;
            at = end;
        }
    }
    Ok(count)
}

/// `numeric`'s binary form as exact decimal text: base-10000 digits, a
/// weight, a sign and a display scale.
pub fn numeric(raw: &[u8]) -> Option<String> {
    let word =
        |i: usize| -> Option<u16> { Some(u16::from_be_bytes(raw.get(i..i + 2)?.try_into().ok()?)) };
    let ndigits = word(0)? as usize;
    let weight = word(2)? as i16 as i32;
    let sign = word(4)?;
    let dscale = word(6)? as usize;
    match sign {
        0xC000 => return Some("NaN".into()),
        0xD000 => return Some("Infinity".into()),
        0xF000 => return Some("-Infinity".into()),
        _ => {}
    }
    let digits: Vec<u16> = (0..ndigits)
        .map(|i| word(8 + i * 2))
        .collect::<Option<_>>()?;
    let digit = |i: i32| -> u16 {
        if i >= 0 && (i as usize) < digits.len() {
            digits[i as usize]
        } else {
            0
        }
    };
    let mut out = String::new();
    if sign == 0x4000 {
        out.push('-');
    }
    // The integer part: groups 0..=weight.
    if weight < 0 {
        out.push('0');
    } else {
        for i in 0..=weight {
            let group = digit(i);
            if i == 0 {
                out.push_str(&group.to_string());
            } else {
                out.push_str(&format!("{group:04}"));
            }
        }
    }
    if dscale > 0 {
        out.push('.');
        let mut frac = String::new();
        let mut i = weight + 1;
        while frac.len() < dscale {
            frac.push_str(&format!("{:04}", digit(i)));
            i += 1;
        }
        frac.truncate(dscale);
        out.push_str(&frac);
    }
    Some(out)
}

/// `money` in cents, as decimal text.
fn money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.unsigned_abs();
    format!("{sign}{}.{:02}", cents / 100, cents % 100)
}

/// `inet` and `cidr`: family, bits, is-cidr, length, address.
fn inet(raw: &[u8]) -> Result<String, String> {
    let bad = || "inet".to_string();
    let [family, bits, is_cidr, len] =
        <[u8; 4]>::try_from(raw.get(..4).ok_or_else(bad)?).map_err(|_| bad())?;
    let address = raw.get(4..4 + len as usize).ok_or_else(bad)?;
    let (text, full) = match family {
        2 => {
            let b: [u8; 4] = address.try_into().map_err(|_| bad())?;
            (std::net::Ipv4Addr::from(b).to_string(), 32)
        }
        _ => {
            let b: [u8; 16] = address.try_into().map_err(|_| bad())?;
            (Ipv6Addr::from(b).to_string(), 128)
        }
    };
    Ok(if is_cidr == 1 || bits != full {
        format!("{text}/{bits}")
    } else {
        text
    })
}

fn macaddr(raw: &[u8]) -> Result<String, String> {
    if raw.len() != 6 && raw.len() != 8 {
        return Err("macaddr".into());
    }
    Ok(raw
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":"))
}

/// `timetz`: microseconds, then the zone's offset in seconds west of UTC.
fn timetz(raw: &[u8]) -> Result<String, String> {
    let bytes: [u8; 12] = raw.try_into().map_err(|_| "timetz".to_string())?;
    let micros = i64::from_be_bytes(bytes[0..8].try_into().map_err(|_| "timetz".to_string())?);
    let west = i32::from_be_bytes(bytes[8..12].try_into().map_err(|_| "timetz".to_string())?);
    let secs = micros / 1_000_000;
    let frac = micros % 1_000_000;
    let mut out = format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    );
    if frac != 0 {
        out.push_str(format!(".{frac:06}").trim_end_matches('0'));
    }
    let east = -west;
    let sign = if east < 0 { '-' } else { '+' };
    let east = east.unsigned_abs();
    out.push_str(&format!("{sign}{:02}", east / 3600));
    if east % 3600 != 0 {
        out.push_str(&format!(":{:02}", east % 3600 / 60));
    }
    Ok(out)
}

/// `bit` and `varbit`: a length in bits, then the bits.
fn bits(raw: &[u8]) -> Result<String, String> {
    let len = i32::from_be_bytes(
        raw.get(..4)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| "bit".to_string())?,
    ) as usize;
    let bytes = &raw[4..];
    Ok((0..len)
        .map(|i| {
            let set = bytes.get(i / 8).is_some_and(|b| b & (0x80 >> (i % 8)) != 0);
            if set {
                '1'
            } else {
                '0'
            }
        })
        .collect())
}

fn point(raw: &[u8]) -> Result<String, String> {
    let bytes: [u8; 16] = raw.try_into().map_err(|_| "point".to_string())?;
    let x = f64::from_be_bytes(bytes[0..8].try_into().map_err(|_| "point".to_string())?);
    let y = f64::from_be_bytes(bytes[8..16].try_into().map_err(|_| "point".to_string())?);
    Ok(format!("({x},{y})"))
}

#[cfg(test)]
mod tests {
    use arrow_array::Array;

    use super::*;

    fn numeric_bytes(ndigits: &[u16], weight: i16, sign: u16, dscale: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend((ndigits.len() as u16).to_be_bytes());
        out.extend(weight.to_be_bytes());
        out.extend(sign.to_be_bytes());
        out.extend(dscale.to_be_bytes());
        for d in ndigits {
            out.extend(d.to_be_bytes());
        }
        out
    }

    #[test]
    fn numerics_keep_every_digit() {
        let cases: &[(&[u16], i16, u16, u16, &str)] = &[
            (&[1, 5000], 0, 0, 2, "1.50"),
            (&[12, 3456, 7800], 1, 0x4000, 4, "-123456.7800"),
            (&[], 0, 0, 0, "0"),
            (&[5], -1, 0, 4, "0.0005"),
            (&[1], 2, 0, 0, "100000000"),
            (
                &[9999, 9999, 9999, 9999, 9999, 9999, 9999, 9999, 9999, 9999],
                9,
                0,
                0,
                "9999999999999999999999999999999999999999",
            ),
            (&[], 0, 0xC000, 0, "NaN"),
            (&[], 0, 0xD000, 0, "Infinity"),
            (&[], 0, 0xF000, 0, "-Infinity"),
        ];
        for (digits, weight, sign, dscale, text) in cases {
            let raw = numeric_bytes(digits, *weight, *sign, *dscale);
            assert_eq!(numeric(&raw).as_deref(), Some(*text));
        }
        assert_eq!(numeric(&[0, 1]), None);
    }

    #[test]
    fn small_types_have_their_text_forms() {
        assert_eq!(money(-1250), "-12.50");
        assert_eq!(money(7), "0.07");
        assert_eq!(
            inet(&[2, 32, 0, 4, 192, 168, 0, 1]).as_deref(),
            Ok("192.168.0.1")
        );
        assert_eq!(
            inet(&[2, 8, 1, 4, 10, 0, 0, 0]).as_deref(),
            Ok("10.0.0.0/8")
        );
        let mut v6 = vec![3, 128, 0, 16];
        v6.extend(Ipv6Addr::LOCALHOST.octets());
        assert_eq!(inet(&v6).as_deref(), Ok("::1"));
        assert!(inet(&[2]).is_err());
        assert_eq!(
            macaddr(&[8, 0, 0x2b, 1, 2, 3]).as_deref(),
            Ok("08:00:2b:01:02:03")
        );
        assert!(macaddr(&[1]).is_err());
        let mut tz = (13 * 3_600_000_000i64 + 500_000).to_be_bytes().to_vec();
        tz.extend((-19_800i32).to_be_bytes());
        assert_eq!(timetz(&tz).as_deref(), Ok("13:00:00.5+05:30"));
        let mut b = 5i32.to_be_bytes().to_vec();
        b.push(0b1010_1000);
        assert_eq!(bits(&b).as_deref(), Ok("10101"));
        let mut p = 1.5f64.to_be_bytes().to_vec();
        p.extend((-2.0f64).to_be_bytes());
        assert_eq!(point(&p).as_deref(), Ok("(1.5,-2)"));
    }

    #[test]
    fn arrays_flatten_into_lists_with_nulls() {
        let (_, mut list) = column("a", &Type::INT4_ARRAY);
        // One dimension of three: 1, NULL, 3.
        let mut raw = Vec::new();
        for v in [1i32, 1, 23, 3, 1] {
            raw.extend(v.to_be_bytes());
        }
        raw.extend(4i32.to_be_bytes());
        raw.extend(1i32.to_be_bytes());
        raw.extend((-1i32).to_be_bytes());
        raw.extend(4i32.to_be_bytes());
        raw.extend(3i32.to_be_bytes());
        list.push(Some(&raw)).expect("array");
        list.push(None).expect("null");
        // An empty array.
        list.push(Some(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 23]))
            .expect("empty");
        let array = list.finish();
        let list = array.as_any().downcast_ref::<ListArray>().expect("a list");
        assert_eq!(list.len(), 3);
        assert!(list.is_null(1));
        assert_eq!(list.value(0).len(), 3);
        assert_eq!(list.value(2).len(), 0);
    }

    #[test]
    fn malformed_values_are_errors_not_nulls() {
        let (_, mut int) = column("n", &Type::INT8);
        assert!(int.push(Some(&[1, 2])).is_err());
        let (_, mut jsonb) = column("j", &Type::JSONB);
        assert!(jsonb.push(Some(&[2, b'{', b'}'])).is_err());
    }

    #[test]
    fn fields_carry_their_type_names() {
        let (field, _) = column("price", &Type::NUMERIC);
        assert_eq!(
            field.metadata().get("dfox.decimal").map(String::as_str),
            Some("true")
        );
        let (field, _) = column("doc", &Type::JSONB);
        assert_eq!(
            field.metadata().get("dfox.json").map(String::as_str),
            Some("true")
        );
        let (field, _) = column("id", &Type::UUID);
        assert_eq!(field.data_type(), &DataType::FixedSizeBinary(16));
        assert_eq!(
            field.metadata().get("dfox.type").map(String::as_str),
            Some("uuid")
        );
    }
}
