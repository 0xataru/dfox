use crate::{errors::DbError, models::schema::TableSchema};
use async_trait::async_trait;
use serde_json::Value;

pub mod mysql;
pub mod postgres;
pub mod sqlite;

pub use mysql::MySqlClient;
pub use postgres::PostgresClient;

#[async_trait]
pub trait DbClient: Send + Sync {
    async fn execute(&self, query: &str) -> Result<(), DbError>;
    async fn query(&self, query: &str) -> Result<Vec<Value>, DbError>;
    async fn query_with_column_order(&self, query: &str) -> Result<(Vec<String>, Vec<Vec<String>>), DbError>;
    async fn begin_transaction<'a>(&'a self) -> Result<Box<dyn Transaction + 'a>, DbError>;
    async fn list_databases(&self) -> Result<Vec<String>, DbError>;
    async fn list_tables(&self) -> Result<Vec<String>, DbError>;
    async fn describe_table(&self, table_name: &str) -> Result<TableSchema, DbError>;
}

#[async_trait]
pub trait Transaction {
    async fn execute_transaction(&mut self, query: &str) -> Result<(), DbError>;
    async fn commit_transaction(self: Box<Self>) -> Result<(), DbError>;
    async fn rollback_transaction(self: Box<Self>) -> Result<(), DbError>;
}
