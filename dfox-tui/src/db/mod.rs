
use std::sync::Arc;

use async_trait::async_trait;
use dfox_core::{DbManager, errors::DbError};

pub mod postgres;
pub mod mysql;

pub type DatabaseManager = DbManager;

#[async_trait]
pub trait Connect {
    async fn connect(database_url: &str) -> Result<Self, DbError>
    where
        Self: Sized;
}

#[async_trait]
pub trait DatabaseUI {
    fn db_manager(&self) -> &Arc<DbManager>;
    fn connection_string(&self) -> String;
    async fn execute_sql_query(&self, query: &str) -> Result<(Vec<String>, String), DbError>;
    async fn describe_table(&self, table_name: &str) -> Result<Vec<String>, DbError>;
    async fn fetch_databases(&self) -> Result<Vec<String>, DbError>;
    async fn fetch_tables(&self) -> Result<Vec<String>, DbError>;
    async fn update_tables(&self) -> Result<(), DbError>;
    async fn connect_to_selected_db(&self, db_name: &str) -> Result<(), DbError>;
    async fn connect_to_default_db(&self) -> Result<(), DbError>;
} 