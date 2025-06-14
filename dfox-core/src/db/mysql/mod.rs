mod types;
pub use types::ColumnType;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::{mysql::MySqlPoolOptions, MySqlPool, Row, Column, TypeInfo};

use crate::{
    errors::DbError,
    models::schema::{ColumnSchema, TableSchema},
};

use super::{DbClient, Transaction};

pub struct MySqlClient {
    pub pool: MySqlPool,
}

impl MySqlClient {
    pub async fn connect(database_url: &str) -> Result<Self, DbError> {
        let pool = MySqlPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| DbError::Connection(e.to_string()))?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl DbClient for MySqlClient {
    async fn execute(&self, query: &str) -> Result<(), DbError> {
        sqlx::query(query)
            .execute(&self.pool)
            .await
            .map_err(DbError::Sqlx)?;
        Ok(())
    }

    async fn query(&self, query: &str) -> Result<Vec<serde_json::Value>, DbError> {
        let rows = sqlx::query(query)
            .fetch_all(&self.pool)
            .await
            .map_err(DbError::Sqlx)?;

        let results = rows
            .iter()
            .map(|row| {
                let json_map = row
                    .columns()
                    .iter()
                    .enumerate()
                    .map(|(i, column)| {
                        let column_name = column.name().to_string();
                        let column_type = ColumnType::from_type_name(column.type_info().name());
                        let value = column_type.to_json_value(row, i);
                        (column_name, value)
                    })
                    .collect();

                Value::Object(json_map)
            })
            .collect();

        Ok(results)
    }

    async fn query_with_column_order(&self, query: &str) -> Result<(Vec<String>, Vec<Vec<String>>), DbError> {
        // TODO: Implement proper column order preservation for MySQL
        let rows = self.query(query).await?;
        
        if rows.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        // Extract column names from first row (alphabetical order for now)
        let column_names: Vec<String> = if let Value::Object(map) = &rows[0] {
            map.keys().cloned().collect()
        } else {
            Vec::new()
        };

        // Convert rows to string vectors
        let data_rows: Vec<Vec<String>> = rows
            .into_iter()
            .map(|row| {
                if let Value::Object(map) = row {
                    column_names
                        .iter()
                        .map(|col| {
                            map.get(col)
                                .map(|v| match v {
                                    Value::Null => "NULL".to_string(),
                                    Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                })
                                .unwrap_or_else(|| "NULL".to_string())
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            })
            .collect();

        Ok((column_names, data_rows))
    }

    async fn begin_transaction<'a>(&'a self) -> Result<Box<dyn Transaction + 'a>, DbError> {
        let tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DbError::Transaction(e.to_string()))?;
        Ok(Box::new(MySqlTransaction { tx }))
    }

    async fn list_databases(&self) -> Result<Vec<String>, DbError> {
        let query = "SHOW DATABASES";
        let rows = sqlx::query(query)
            .fetch_all(&self.pool)
            .await
            .map_err(DbError::Sqlx)?;

        let databases: Vec<String> = rows
            .iter()
            .map(|row| row.try_get::<String, _>(0).unwrap_or_default())
            .collect();

        Ok(databases)
    }

    async fn list_tables(&self) -> Result<Vec<String>, DbError> {
        let query = "SHOW TABLES";
        let rows = sqlx::query(query)
            .fetch_all(&self.pool)
            .await
            .map_err(DbError::Sqlx)?;

        let tables = rows
            .iter()
            .map(|row| row.try_get::<String, _>(0).unwrap_or_default())
            .collect();

        Ok(tables)
    }

    async fn describe_table(&self, table_name: &str) -> Result<TableSchema, DbError> {
        let query = format!("DESCRIBE {}", table_name);
        let rows = sqlx::query(&query)
            .fetch_all(&self.pool)
            .await
            .map_err(DbError::Sqlx)?;

        let columns = rows
            .iter()
            .map(|row| ColumnSchema {
                name: row.try_get("Field").unwrap(),
                data_type: row.try_get("Type").unwrap(),
                is_nullable: row.try_get::<String, _>("Null").unwrap() == "YES",
                default: row.try_get("Default").ok(),
            })
            .collect();

        Ok(TableSchema {
            table_name: table_name.to_string(),
            columns,
            indexes: Vec::new(),
        })
    }
}

pub struct MySqlTransaction<'a> {
    tx: sqlx::Transaction<'a, sqlx::MySql>,
}

#[async_trait]
impl<'a> Transaction for MySqlTransaction<'a> {
    async fn execute_transaction(&mut self, query: &str) -> Result<(), DbError> {
        sqlx::query(query)
            .execute(&mut *self.tx)
            .await
            .map_err(|e| DbError::Transaction(e.to_string()))?;
        Ok(())
    }

    async fn commit_transaction(self: Box<Self>) -> Result<(), DbError> {
        self.tx
            .commit()
            .await
            .map_err(|e| DbError::Transaction(e.to_string()))
    }

    async fn rollback_transaction(self: Box<Self>) -> Result<(), DbError> {
        self.tx
            .rollback()
            .await
            .map_err(|e| DbError::Transaction(e.to_string()))
    }
} 