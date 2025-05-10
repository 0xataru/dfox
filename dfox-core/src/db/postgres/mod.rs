mod types;
pub use types::ColumnType;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::{postgres::PgPoolOptions, PgPool, Row, Column, TypeInfo};

use crate::{
    errors::DbError,
    models::schema::{ColumnSchema, TableSchema},
};

use super::{DbClient, Transaction};

pub struct PostgresClient {
    pub pool: PgPool,
}

impl PostgresClient {
    pub async fn connect(database_url: &str) -> Result<Self, DbError> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(|e| DbError::Connection(e.to_string()))?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl DbClient for PostgresClient {
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

    async fn begin_transaction<'a>(&'a self) -> Result<Box<dyn Transaction + 'a>, DbError> {
        let tx = self
            .pool
            .begin()
            .await
            .map_err(|e| DbError::Transaction(e.to_string()))?;
        Ok(Box::new(PostgresTransaction { tx }))
    }

    async fn list_databases(&self) -> Result<Vec<String>, DbError> {
        let query = r#"
            SELECT datname
            FROM pg_database
            WHERE datistemplate = false
        "#;

        let rows = sqlx::query(query)
            .fetch_all(&self.pool)
            .await
            .map_err(DbError::Sqlx)?;

        let databases: Vec<String> = rows
            .iter()
            .map(|row| row.try_get::<String, _>("datname").unwrap_or_default())
            .collect();

        Ok(databases)
    }

    async fn list_tables(&self) -> Result<Vec<String>, DbError> {
        let query = r#"
            SELECT table_name
            FROM information_schema.tables
            WHERE table_schema = 'public'
        "#;
        let rows = sqlx::query(query)
            .fetch_all(&self.pool)
            .await
            .map_err(DbError::Sqlx)?;

        let tables = rows
            .iter()
            .map(|row| row.try_get::<String, _>("table_name").unwrap_or_default())
            .collect();

        Ok(tables)
    }

    async fn describe_table(&self, table_name: &str) -> Result<TableSchema, DbError> {
        let query = format!(
            r#"
            SELECT column_name, data_type, is_nullable, column_default
            FROM information_schema.columns
            WHERE table_name = '{}'
            "#,
            table_name
        );
        let rows = sqlx::query(&query)
            .fetch_all(&self.pool)
            .await
            .map_err(DbError::Sqlx)?;

        let columns = rows
            .iter()
            .map(|row| ColumnSchema {
                name: row.try_get("column_name").unwrap(),
                data_type: row.try_get("data_type").unwrap(),
                is_nullable: row.try_get::<String, _>("is_nullable").unwrap() == "YES",
                default: row.try_get("column_default").ok(),
            })
            .collect();

        Ok(TableSchema {
            table_name: table_name.to_string(),
            columns,
            indexes: Vec::new(),
        })
    }
}

pub struct PostgresTransaction<'a> {
    tx: sqlx::Transaction<'a, sqlx::Postgres>,
}

#[async_trait]
impl<'a> Transaction for PostgresTransaction<'a> {
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