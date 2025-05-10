use serde_json::Value;
use sqlx::{Row, mysql::MySqlRow};
use chrono::{NaiveDate, NaiveTime, NaiveDateTime};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

#[derive(Debug)]
pub enum ColumnType {
    // Numeric types
    TinyInt,
    SmallInt,
    MediumInt,
    Int,
    BigInt,
    Decimal,
    Float,
    Double,
    
    // String types
    Char,
    Varchar,
    TinyText,
    Text,
    MediumText,
    LongText,
    
    // Date and Time types
    Date,
    Time,
    Year,
    DateTime,
    Timestamp,
    
    // Binary types
    Binary,
    Varbinary,
    TinyBlob,
    Blob,
    MediumBlob,
    LongBlob,
    
    // JSON type
    Json,
    
    // Boolean type
    Boolean,
    
    // Enum and Set types
    Enum,
    Set,
    
    // Unknown type
    Unknown,
}

impl ColumnType {
    pub fn from_type_name(type_name: &str) -> Self {
        match type_name.to_uppercase().as_str() {
            // Numeric types
            "TINYINT" => ColumnType::TinyInt,
            "SMALLINT" => ColumnType::SmallInt,
            "MEDIUMINT" => ColumnType::MediumInt,
            "INT" | "INTEGER" => ColumnType::Int,
            "BIGINT" => ColumnType::BigInt,
            "DECIMAL" | "DEC" | "NUMERIC" => ColumnType::Decimal,
            "FLOAT" => ColumnType::Float,
            "DOUBLE" | "DOUBLE PRECISION" | "REAL" => ColumnType::Double,
            
            // String types
            "CHAR" => ColumnType::Char,
            "VARCHAR" => ColumnType::Varchar,
            "TINYTEXT" => ColumnType::TinyText,
            "TEXT" => ColumnType::Text,
            "MEDIUMTEXT" => ColumnType::MediumText,
            "LONGTEXT" => ColumnType::LongText,
            
            // Date and Time types
            "DATE" => ColumnType::Date,
            "TIME" => ColumnType::Time,
            "YEAR" => ColumnType::Year,
            "DATETIME" => ColumnType::DateTime,
            "TIMESTAMP" => ColumnType::Timestamp,
            
            // Binary types
            "BINARY" => ColumnType::Binary,
            "VARBINARY" => ColumnType::Varbinary,
            "TINYBLOB" => ColumnType::TinyBlob,
            "BLOB" => ColumnType::Blob,
            "MEDIUMBLOB" => ColumnType::MediumBlob,
            "LONGBLOB" => ColumnType::LongBlob,
            
            // JSON type
            "JSON" => ColumnType::Json,
            
            // Boolean type
            "BOOLEAN" | "BOOL" => ColumnType::Boolean,
            
            // Enum and Set types
            "ENUM" => ColumnType::Enum,
            "SET" => ColumnType::Set,
            
            _ => ColumnType::Unknown,
        }
    }

    pub fn to_json_value<'a>(&self, row: &'a MySqlRow, index: usize) -> Value {
        match self {
            ColumnType::DateTime | ColumnType::Timestamp => match row.try_get::<NaiveDateTime, _>(index) {
                Ok(timestamp) => Value::String(timestamp.to_string()),
                Err(_) => Value::Null,
            },
            ColumnType::Date => match row.try_get::<NaiveDate, _>(index) {
                Ok(date) => Value::String(date.to_string()),
                Err(_) => Value::Null,
            },
            ColumnType::Time => match row.try_get::<NaiveTime, _>(index) {
                Ok(time) => Value::String(time.to_string()),
                Err(_) => Value::Null,
            },
            ColumnType::TinyInt | ColumnType::SmallInt | ColumnType::MediumInt | ColumnType::Int => match row.try_get::<i32, _>(index) {
                Ok(int_val) => Value::Number(int_val.into()),
                Err(_) => Value::Null,
            },
            ColumnType::BigInt => match row.try_get::<i64, _>(index) {
                Ok(int_val) => Value::Number(int_val.into()),
                Err(_) => Value::Null,
            },
            ColumnType::Decimal => match row.try_get::<String, _>(index) {
                Ok(val) => Value::String(val),
                Err(_) => Value::Null,
            },
            ColumnType::Float | ColumnType::Double => match row.try_get::<f64, _>(index) {
                Ok(val) => Value::Number(serde_json::Number::from_f64(val).unwrap_or(serde_json::Number::from(0))),
                Err(_) => Value::Null,
            },
            ColumnType::Boolean => match row.try_get::<bool, _>(index) {
                Ok(val) => Value::Bool(val),
                Err(_) => Value::Null,
            },
            ColumnType::Json => match row.try_get::<Value, _>(index) {
                Ok(val) => val,
                Err(_) => Value::Null,
            },
            ColumnType::Binary | ColumnType::Varbinary | ColumnType::TinyBlob | ColumnType::Blob | ColumnType::MediumBlob | ColumnType::LongBlob => match row.try_get::<Vec<u8>, _>(index) {
                Ok(val) => Value::String(BASE64.encode(val)),
                Err(_) => Value::Null,
            },
            ColumnType::Char | ColumnType::Varchar | ColumnType::TinyText | ColumnType::Text | ColumnType::MediumText | ColumnType::LongText => match row.try_get::<String, _>(index) {
                Ok(text) => Value::String(text),
                Err(_) => Value::Null,
            },
            ColumnType::Enum | ColumnType::Set => match row.try_get::<String, _>(index) {
                Ok(val) => Value::String(val),
                Err(_) => Value::Null,
            },
            ColumnType::Year => match row.try_get::<i32, _>(index) {
                Ok(year) => Value::Number(year.into()),
                Err(_) => Value::Null,
            },
            ColumnType::Unknown => match row.try_get::<String, _>(index) {
                Ok(val) => Value::String(val),
                Err(_) => Value::Null,
            },
        }
    }
} 