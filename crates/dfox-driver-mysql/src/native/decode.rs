//! MySQL values, decoded into Arrow columns.
//!
//! Each result column gets a [`Column`] chosen from its type and flags:
//! integers keep their width and sign, `TINYINT(1)` is a boolean, `DECIMAL`
//! stays exact (as text tagged decimal), strings in a binary character set
//! are bytes. Dates and times keep MySQL's own text, which holds what Arrow
//! can't: zero dates, and `TIME` spans beyond a day.
//!
//! Values arrive as text in the text protocol (what the user runs) and typed
//! in the binary protocol (browsing); both decode the same.

use std::sync::Arc;

use arrow_array::{
    builder::{
        BinaryBuilder, BooleanBuilder, Float32Builder, Float64Builder, Int16Builder, Int32Builder,
        Int64Builder, Int8Builder, StringBuilder, UInt16Builder, UInt32Builder, UInt64Builder,
        UInt8Builder,
    },
    ArrayRef,
};
use arrow_schema::{DataType, Field};
use dfox_core::{
    arrow::{decimal_field, field, json_field},
    errors::DbError,
};
use mysql_async::{
    consts::{ColumnFlags, ColumnType},
    Value,
};

/// The character set number of binary strings.
const BINARY: u16 = 63;

/// Builds one Arrow column from a MySQL column's values.
pub enum Column {
    Bool(BooleanBuilder),
    Int8(Int8Builder),
    Int16(Int16Builder),
    Int32(Int32Builder),
    Int64(Int64Builder),
    UInt8(UInt8Builder),
    UInt16(UInt16Builder),
    UInt32(UInt32Builder),
    UInt64(UInt64Builder),
    Float32(Float32Builder),
    Float64(Float64Builder),
    /// Text as sent: strings, enums, sets, JSON, decimals, dates and times.
    Text(StringBuilder),
    /// Binary strings, blobs, bits and geometry.
    Bytes(BinaryBuilder),
}

/// The field and the builder for `column`.
pub fn column(column: &mysql_async::Column) -> (Field, Column) {
    let name = column.name_str();
    let flags = column.flags();
    let unsigned = flags.contains(ColumnFlags::UNSIGNED_FLAG);
    let binary = column.character_set() == BINARY;
    let type_name = type_name(column);
    let int = |signed: Column, unsigned_column: Column, signed_type, unsigned_type| {
        if unsigned {
            (unsigned_type, unsigned_column)
        } else {
            (signed_type, signed)
        }
    };
    use ColumnType::*;
    let (data_type, builder) = match column.column_type() {
        MYSQL_TYPE_TINY if column.column_length() == 1 && !unsigned => {
            (DataType::Boolean, Column::Bool(BooleanBuilder::new()))
        }
        MYSQL_TYPE_TINY => int(
            Column::Int8(Int8Builder::new()),
            Column::UInt8(UInt8Builder::new()),
            DataType::Int8,
            DataType::UInt8,
        ),
        MYSQL_TYPE_SHORT => int(
            Column::Int16(Int16Builder::new()),
            Column::UInt16(UInt16Builder::new()),
            DataType::Int16,
            DataType::UInt16,
        ),
        MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG => int(
            Column::Int32(Int32Builder::new()),
            Column::UInt32(UInt32Builder::new()),
            DataType::Int32,
            DataType::UInt32,
        ),
        MYSQL_TYPE_LONGLONG => int(
            Column::Int64(Int64Builder::new()),
            Column::UInt64(UInt64Builder::new()),
            DataType::Int64,
            DataType::UInt64,
        ),
        MYSQL_TYPE_YEAR => (DataType::UInt16, Column::UInt16(UInt16Builder::new())),
        MYSQL_TYPE_FLOAT => (DataType::Float32, Column::Float32(Float32Builder::new())),
        MYSQL_TYPE_DOUBLE => (DataType::Float64, Column::Float64(Float64Builder::new())),
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => {
            return (
                decimal_field(&name, &type_name),
                Column::Text(StringBuilder::new()),
            )
        }
        MYSQL_TYPE_JSON => {
            return (
                json_field(&name, &type_name),
                Column::Text(StringBuilder::new()),
            )
        }
        MYSQL_TYPE_BIT | MYSQL_TYPE_GEOMETRY | MYSQL_TYPE_VECTOR => {
            (DataType::Binary, Column::Bytes(BinaryBuilder::new()))
        }
        MYSQL_TYPE_VARCHAR
        | MYSQL_TYPE_VAR_STRING
        | MYSQL_TYPE_STRING
        | MYSQL_TYPE_TINY_BLOB
        | MYSQL_TYPE_MEDIUM_BLOB
        | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_BLOB
            if binary && !flags.intersects(ColumnFlags::ENUM_FLAG | ColumnFlags::SET_FLAG) =>
        {
            (DataType::Binary, Column::Bytes(BinaryBuilder::new()))
        }
        _ => (DataType::Utf8, Column::Text(StringBuilder::new())),
    };
    (field(&name, data_type, &type_name), builder)
}

/// MySQL's name for a column's type, as `SHOW COLUMNS` would write it
/// without lengths: `BIGINT UNSIGNED`, `VARCHAR`, `BLOB`.
fn type_name(column: &mysql_async::Column) -> String {
    let flags = column.flags();
    let binary = column.character_set() == BINARY;
    use ColumnType::*;
    let name = match column.column_type() {
        MYSQL_TYPE_TINY if column.column_length() == 1 => "BOOLEAN",
        MYSQL_TYPE_TINY => "TINYINT",
        MYSQL_TYPE_SHORT => "SMALLINT",
        MYSQL_TYPE_INT24 => "MEDIUMINT",
        MYSQL_TYPE_LONG => "INT",
        MYSQL_TYPE_LONGLONG => "BIGINT",
        MYSQL_TYPE_FLOAT => "FLOAT",
        MYSQL_TYPE_DOUBLE => "DOUBLE",
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "DECIMAL",
        MYSQL_TYPE_NULL => "NULL",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "TIMESTAMP",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "DATE",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "TIME",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "DATETIME",
        MYSQL_TYPE_YEAR => "YEAR",
        MYSQL_TYPE_BIT => "BIT",
        MYSQL_TYPE_JSON => "JSON",
        MYSQL_TYPE_GEOMETRY => "GEOMETRY",
        MYSQL_TYPE_VECTOR => "VECTOR",
        MYSQL_TYPE_ENUM => "ENUM",
        MYSQL_TYPE_SET => "SET",
        _ if flags.contains(ColumnFlags::ENUM_FLAG) => "ENUM",
        _ if flags.contains(ColumnFlags::SET_FLAG) => "SET",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary {
                "BLOB"
            } else {
                "TEXT"
            }
        }
        MYSQL_TYPE_STRING if binary => "BINARY",
        MYSQL_TYPE_STRING => "CHAR",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING if binary => "VARBINARY",
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING => "VARCHAR",
        _ => "UNKNOWN",
    };
    if flags.contains(ColumnFlags::UNSIGNED_FLAG) && column.column_type().is_numeric_type() {
        format!("{name} UNSIGNED")
    } else {
        name.to_string()
    }
}

fn error(what: &str, value: &Value) -> DbError {
    DbError::General(format!("can't read {value:?} as {what}"))
}

/// A value's text, as the text protocol sends everything.
fn text<'a>(value: &'a Value, what: &str) -> Result<&'a str, DbError> {
    match value {
        Value::Bytes(bytes) => std::str::from_utf8(bytes).map_err(|_| error(what, value)),
        _ => Err(error(what, value)),
    }
}

/// An integer, sent as a number or as its text.
fn int<T>(value: &Value, what: &str) -> Result<T, DbError>
where
    T: TryFrom<i64> + TryFrom<u64> + std::str::FromStr,
{
    match value {
        Value::Int(n) => T::try_from(*n).map_err(|_| error(what, value)),
        Value::UInt(n) => T::try_from(*n).map_err(|_| error(what, value)),
        _ => text(value, what)?.parse().map_err(|_| error(what, value)),
    }
}

fn float(value: &Value, what: &str) -> Result<f64, DbError> {
    match value {
        Value::Float(n) => Ok((*n).into()),
        Value::Double(n) => Ok(*n),
        _ => text(value, what)?.parse().map_err(|_| error(what, value)),
    }
}

impl Column {
    /// Adds one value; `None` and [`Value::NULL`] are NULL.
    pub fn push(&mut self, value: Option<&Value>) -> Result<(), DbError> {
        let value = match value {
            None | Some(Value::NULL) => {
                self.push_null();
                return Ok(());
            }
            Some(value) => value,
        };
        match self {
            Column::Bool(b) => b.append_value(int::<i64>(value, "a boolean")? != 0),
            Column::Int8(b) => b.append_value(int(value, "TINYINT")?),
            Column::Int16(b) => b.append_value(int(value, "SMALLINT")?),
            Column::Int32(b) => b.append_value(int(value, "INT")?),
            Column::Int64(b) => b.append_value(int(value, "BIGINT")?),
            Column::UInt8(b) => b.append_value(int(value, "TINYINT UNSIGNED")?),
            Column::UInt16(b) => b.append_value(int(value, "SMALLINT UNSIGNED")?),
            Column::UInt32(b) => b.append_value(int(value, "INT UNSIGNED")?),
            Column::UInt64(b) => b.append_value(int(value, "BIGINT UNSIGNED")?),
            Column::Float32(b) => b.append_value(float(value, "FLOAT")? as f32),
            Column::Float64(b) => b.append_value(float(value, "DOUBLE")?),
            Column::Text(b) => match value {
                Value::Date(..) | Value::Time(..) => b.append_value(temporal(value)),
                _ => b.append_value(text(value, "text")?),
            },
            Column::Bytes(b) => match value {
                Value::Bytes(bytes) => b.append_value(bytes),
                _ => return Err(error("bytes", value)),
            },
        }
        Ok(())
    }

    fn push_null(&mut self) {
        match self {
            Column::Bool(b) => b.append_null(),
            Column::Int8(b) => b.append_null(),
            Column::Int16(b) => b.append_null(),
            Column::Int32(b) => b.append_null(),
            Column::Int64(b) => b.append_null(),
            Column::UInt8(b) => b.append_null(),
            Column::UInt16(b) => b.append_null(),
            Column::UInt32(b) => b.append_null(),
            Column::UInt64(b) => b.append_null(),
            Column::Float32(b) => b.append_null(),
            Column::Float64(b) => b.append_null(),
            Column::Text(b) => b.append_null(),
            Column::Bytes(b) => b.append_null(),
        }
    }

    /// The column so far, as an Arrow array; the builder starts over.
    pub fn finish(&mut self) -> ArrayRef {
        match self {
            Column::Bool(b) => Arc::new(b.finish()),
            Column::Int8(b) => Arc::new(b.finish()),
            Column::Int16(b) => Arc::new(b.finish()),
            Column::Int32(b) => Arc::new(b.finish()),
            Column::Int64(b) => Arc::new(b.finish()),
            Column::UInt8(b) => Arc::new(b.finish()),
            Column::UInt16(b) => Arc::new(b.finish()),
            Column::UInt32(b) => Arc::new(b.finish()),
            Column::UInt64(b) => Arc::new(b.finish()),
            Column::Float32(b) => Arc::new(b.finish()),
            Column::Float64(b) => Arc::new(b.finish()),
            Column::Text(b) => Arc::new(b.finish()),
            Column::Bytes(b) => Arc::new(b.finish()),
        }
    }
}

/// A binary-protocol date or time as MySQL prints it: `2026-10-12`,
/// `2026-10-12 13:14:15.5`, `-838:59:59`.
fn temporal(value: &Value) -> String {
    let fraction = |micros: u32| {
        if micros == 0 {
            String::new()
        } else {
            format!(".{micros:06}").trim_end_matches('0').to_string()
        }
    };
    match *value {
        Value::Date(year, month, day, 0, 0, 0, 0) => format!("{year:04}-{month:02}-{day:02}"),
        Value::Date(year, month, day, hour, minute, second, micros) => format!(
            "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}{}",
            fraction(micros)
        ),
        Value::Time(negative, days, hours, minutes, seconds, micros) => format!(
            "{}{:02}:{minutes:02}:{seconds:02}{}",
            if negative { "-" } else { "" },
            u64::from(days) * 24 + u64::from(hours),
            fraction(micros)
        ),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_times_print_as_mysql_prints_them() {
        assert_eq!(
            temporal(&Value::Date(2026, 10, 12, 0, 0, 0, 0)),
            "2026-10-12"
        );
        assert_eq!(
            temporal(&Value::Date(2026, 10, 12, 13, 14, 15, 500_000)),
            "2026-10-12 13:14:15.5"
        );
        assert_eq!(temporal(&Value::Date(0, 0, 0, 0, 0, 0, 0)), "0000-00-00");
        assert_eq!(
            temporal(&Value::Time(true, 34, 22, 59, 59, 0)),
            "-838:59:59"
        );
        assert_eq!(
            temporal(&Value::Time(false, 0, 1, 2, 3, 40)),
            "01:02:03.00004"
        );
    }

    #[test]
    fn integers_read_from_text_or_numbers() {
        assert_eq!(int::<i8>(&Value::Bytes(b"-5".to_vec()), "x").ok(), Some(-5));
        assert_eq!(int::<u64>(&Value::UInt(u64::MAX), "x").ok(), Some(u64::MAX));
        assert_eq!(int::<i16>(&Value::Int(-300), "x").ok(), Some(-300));
        // Out of range or not a number is an error, never NULL.
        assert!(int::<i8>(&Value::Int(300), "x").is_err());
        assert!(int::<i32>(&Value::Bytes(b"abc".to_vec()), "x").is_err());
        assert_eq!(float(&Value::Bytes(b"2.5".to_vec()), "x").ok(), Some(2.5));
        assert_eq!(float(&Value::Float(1.25), "x").ok(), Some(1.25));
    }

    #[test]
    fn nulls_and_values_build_arrays() {
        let mut column = Column::Int32(Int32Builder::new());
        column.push(Some(&Value::Bytes(b"7".to_vec()))).expect("7");
        column.push(Some(&Value::NULL)).expect("null");
        column.push(None).expect("none");
        let array = column.finish();
        assert_eq!(array.len(), 3);
        assert_eq!(array.null_count(), 2);
    }
}
