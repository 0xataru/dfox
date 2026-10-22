//! Copying rows to another database: the `CREATE TABLE` that holds them,
//! from their Arrow columns, in the target's own types.

use arrow_schema::{DataType, Field, SchemaRef};

use crate::{
    arrow::{DECIMAL, JSON},
    catalog::NodePath,
    models::connections::DbType,
};

/// `name` quoted for `db`: backticks in MySQL, double quotes otherwise.
fn quote(db: DbType, name: &str) -> String {
    match db {
        DbType::MySql => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// The type `db` gives a column like `field`.
fn sql_type(db: DbType, field: &Field) -> String {
    let tagged = |tag: &str| field.metadata().contains_key(tag);
    let mysql = db == DbType::MySql;
    let sqlite = db == DbType::Sqlite;
    let text = if mysql { "LONGTEXT" } else { "TEXT" };
    match field.data_type() {
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View if tagged(JSON) => match db {
            DbType::Postgres => "JSONB",
            DbType::MySql => "JSON",
            _ => "TEXT",
        }
        .to_string(),
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View if tagged(DECIMAL) => {
            if mysql { "DECIMAL(65, 30)" } else { "NUMERIC" }.to_string()
        }
        DataType::Boolean => "BOOLEAN".to_string(),
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::UInt8 | DataType::UInt16 => {
            "INTEGER".to_string()
        }
        DataType::Int64 | DataType::UInt32 => if sqlite { "INTEGER" } else { "BIGINT" }.to_string(),
        DataType::UInt64 => match db {
            DbType::MySql => "BIGINT UNSIGNED",
            DbType::Sqlite => "INTEGER",
            _ => "NUMERIC(20)",
        }
        .to_string(),
        DataType::Float16 | DataType::Float32 | DataType::Float64 => match db {
            DbType::MySql => "DOUBLE",
            DbType::Sqlite => "REAL",
            _ => "DOUBLE PRECISION",
        }
        .to_string(),
        DataType::Decimal128(p, s) | DataType::Decimal256(p, s) => match db {
            DbType::Sqlite => "NUMERIC".to_string(),
            DbType::MySql => format!("DECIMAL({}, {s})", (*p).min(65)),
            _ => format!("NUMERIC({p}, {s})"),
        },
        DataType::Date32 | DataType::Date64 => "DATE".to_string(),
        DataType::Timestamp(_, zone) => match db {
            DbType::MySql => "DATETIME(6)".to_string(),
            DbType::Sqlite => "TEXT".to_string(),
            _ if zone.is_some() => "TIMESTAMPTZ".to_string(),
            _ => "TIMESTAMP".to_string(),
        },
        DataType::Time32(_) | DataType::Time64(_) => {
            if sqlite { "TEXT" } else { "TIME" }.to_string()
        }
        DataType::Binary | DataType::LargeBinary | DataType::FixedSizeBinary(_) => match db {
            DbType::Postgres => "BYTEA",
            DbType::MySql => "LONGBLOB",
            _ => "BLOB",
        }
        .to_string(),
        _ => text.to_string(),
    }
}

/// The `CREATE TABLE` for the table at `table` in `db`, with a column for
/// each of `schema`'s. `None` where there are no tables to create: MongoDB
/// makes a collection on its first document.
pub fn create_table(db: DbType, table: &NodePath, schema: &SchemaRef) -> Option<String> {
    let name = match (db, table.parts()) {
        (DbType::Postgres, [_, schema, table]) => {
            format!("{}.{}", quote(db, schema), quote(db, table))
        }
        (DbType::MySql | DbType::Sqlite, [database, table]) => {
            format!("{}.{}", quote(db, database), quote(db, table))
        }
        _ => return None,
    };
    let columns: Vec<String> = schema
        .fields()
        .iter()
        .map(|field| format!("{} {}", quote(db, field.name()), sql_type(db, field)))
        .collect();
    Some(format!("CREATE TABLE {name} ({})", columns.join(", ")))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_schema::{Schema, TimeUnit};

    use super::*;
    use crate::arrow::{decimal_field, field, json_field};

    fn schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            field("id", DataType::Int64, "int8"),
            field("name", DataType::Utf8, "text"),
            field("price", DataType::Float64, "float8"),
            decimal_field("total", "numeric"),
            json_field("tags", "jsonb"),
            field(
                "at",
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                "timestamptz",
            ),
            field("odd \"name\"", DataType::Boolean, "bool"),
        ]))
    }

    #[test]
    fn each_database_gets_its_own_types() {
        let pg: NodePath = ["shop", "public", "orders"].into_iter().collect();
        assert_eq!(
            create_table(DbType::Postgres, &pg, &schema()).as_deref(),
            Some(
                "CREATE TABLE \"public\".\"orders\" (\"id\" BIGINT, \"name\" TEXT, \"price\" DOUBLE PRECISION, \
                 \"total\" NUMERIC, \"tags\" JSONB, \"at\" TIMESTAMPTZ, \"odd \"\"name\"\"\" BOOLEAN)"
            )
        );
        let my: NodePath = ["shop", "orders"].into_iter().collect();
        assert_eq!(
            create_table(DbType::MySql, &my, &schema()).as_deref(),
            Some(
                "CREATE TABLE `shop`.`orders` (`id` BIGINT, `name` LONGTEXT, `price` DOUBLE, \
                 `total` DECIMAL(65, 30), `tags` JSON, `at` DATETIME(6), `odd \"name\"` BOOLEAN)"
            )
        );
        let lite: NodePath = ["main", "orders"].into_iter().collect();
        assert!(
            create_table(DbType::Sqlite, &lite, &schema()).is_some_and(
                |ddl| ddl.starts_with("CREATE TABLE \"main\".\"orders\" (\"id\" INTEGER")
            )
        );
        assert_eq!(create_table(DbType::Mongo, &my, &schema()), None);
    }
}
