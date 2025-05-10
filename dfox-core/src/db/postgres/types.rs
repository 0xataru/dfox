use serde_json::Value;
use sqlx::{Row, postgres::PgRow};
use uuid::Uuid;
use chrono::{NaiveDate, NaiveTime, NaiveDateTime};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

#[derive(Debug)]
pub enum ColumnType {
    // Numeric types
    SmallInt,
    Integer,
    BigInt,
    Decimal,
    Real,
    DoublePrecision,
    Serial,
    BigSerial,
    
    // Character types
    Char,
    Varchar,
    Text,
    
    // Binary data types
    Bytea,
    
    // Date/Time types
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Interval,
    
    // Boolean type
    Boolean,
    
    // UUID type
    Uuid,
    
    // JSON types
    Json,
    Jsonb,
    
    // Array type
    Array,
    
    // Network address types
    Inet,
    Cidr,
    MacAddr,
    
    // Geometric types
    Point,
    Line,
    Circle,
    Box,
    
    // Money type
    Money,
    
    // Unknown type
    Unknown,
}

impl ColumnType {
    pub fn from_type_name(type_name: &str) -> Self {
        match type_name {
            // Numeric types
            "INT2" => ColumnType::SmallInt,
            "INT4" => ColumnType::Integer,
            "INT8" => ColumnType::BigInt,
            "NUMERIC" | "DECIMAL" => ColumnType::Decimal,
            "REAL" | "FLOAT4" => ColumnType::Real,
            "DOUBLE PRECISION" | "FLOAT8" => ColumnType::DoublePrecision,
            "SERIAL" | "SERIAL4" => ColumnType::Serial,
            "BIGSERIAL" | "SERIAL8" => ColumnType::BigSerial,
            
            // Character types
            "CHAR" | "CHARACTER" => ColumnType::Char,
            "VARCHAR" | "CHARACTER VARYING" => ColumnType::Varchar,
            "TEXT" => ColumnType::Text,
            
            // Binary data types
            "BYTEA" => ColumnType::Bytea,
            
            // Date/Time types
            "DATE" => ColumnType::Date,
            "TIME" | "TIME WITHOUT TIME ZONE" => ColumnType::Time,
            "TIMESTAMP" | "TIMESTAMP WITHOUT TIME ZONE" => ColumnType::Timestamp,
            "TIMESTAMPTZ" | "TIMESTAMP WITH TIME ZONE" => ColumnType::TimestampTz,
            "INTERVAL" => ColumnType::Interval,
            
            // Boolean type
            "BOOLEAN" | "BOOL" => ColumnType::Boolean,
            
            // UUID type
            "UUID" => ColumnType::Uuid,
            
            // JSON types
            "JSON" => ColumnType::Json,
            "JSONB" => ColumnType::Jsonb,
            
            // Array type
            "ARRAY" => ColumnType::Array,
            
            // Network address types
            "INET" => ColumnType::Inet,
            "CIDR" => ColumnType::Cidr,
            "MACADDR" => ColumnType::MacAddr,
            
            // Geometric types
            "POINT" => ColumnType::Point,
            "LINE" => ColumnType::Line,
            "CIRCLE" => ColumnType::Circle,
            "BOX" => ColumnType::Box,
            
            // Money type
            "MONEY" => ColumnType::Money,
            
            _ => ColumnType::Unknown,
        }
    }

    pub fn to_json_value<'a>(&self, row: &'a PgRow, index: usize) -> Value {
        match self {
            ColumnType::Uuid => match row.try_get::<Uuid, _>(index) {
                Ok(uuid) => Value::String(uuid.to_string()),
                Err(_) => Value::Null,
            },
            ColumnType::Timestamp | ColumnType::TimestampTz => match row.try_get::<NaiveDateTime, _>(index) {
                Ok(timestamp) => Value::String(timestamp.to_string()),
                Err(_) => Value::Null,
            },
            ColumnType::SmallInt | ColumnType::Integer | ColumnType::Serial => match row.try_get::<i32, _>(index) {
                Ok(int_val) => Value::Number(int_val.into()),
                Err(_) => Value::Null,
            },
            ColumnType::BigInt | ColumnType::BigSerial => match row.try_get::<i64, _>(index) {
                Ok(int_val) => Value::Number(int_val.into()),
                Err(_) => Value::Null,
            },
            ColumnType::Decimal => match row.try_get::<String, _>(index) {
                Ok(val) => Value::String(val),
                Err(_) => Value::Null,
            },
            ColumnType::Real | ColumnType::DoublePrecision => match row.try_get::<f64, _>(index) {
                Ok(val) => Value::Number(serde_json::Number::from_f64(val).unwrap_or(serde_json::Number::from(0))),
                Err(_) => Value::Null,
            },
            ColumnType::Boolean => match row.try_get::<bool, _>(index) {
                Ok(val) => Value::Bool(val),
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
            ColumnType::Interval => match row.try_get::<String, _>(index) {
                Ok(val) => Value::String(val),
                Err(_) => Value::Null,
            },
            ColumnType::Json | ColumnType::Jsonb => match row.try_get::<Value, _>(index) {
                Ok(val) => val,
                Err(_) => Value::Null,
            },
            ColumnType::Bytea => match row.try_get::<Vec<u8>, _>(index) {
                Ok(val) => Value::String(BASE64.encode(val)),
                Err(_) => Value::Null,
            },
            ColumnType::Money => match row.try_get::<String, _>(index) {
                Ok(val) => Value::String(val),
                Err(_) => Value::Null,
            },
            ColumnType::Inet | ColumnType::Cidr | ColumnType::MacAddr => match row.try_get::<String, _>(index) {
                Ok(val) => Value::String(val),
                Err(_) => Value::Null,
            },
            ColumnType::Point | ColumnType::Line | ColumnType::Circle | ColumnType::Box => match row.try_get::<String, _>(index) {
                Ok(val) => Value::String(val),
                Err(_) => Value::Null,
            },
            ColumnType::Char | ColumnType::Varchar | ColumnType::Text => match row.try_get::<String, _>(index) {
                Ok(text) => Value::String(text),
                Err(_) => Value::Null,
            },
            ColumnType::Array => match row.try_get::<String, _>(index) {
                Ok(val) => Value::String(val),
                Err(_) => Value::Null,
            },
            ColumnType::Unknown => match row.try_get::<String, _>(index) {
                Ok(val) => Value::String(val),
                Err(_) => Value::Null,
            },
        }
    }
} 