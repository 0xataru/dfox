use std::sync::Arc;
use async_trait::async_trait;
use dfox_core::{db::{DbClient, postgres::PostgresClient}, errors::DbError};
use crate::db::{Connect, DatabaseUI, DatabaseManager};
use crate::ui::DatabaseClientUI;

pub struct PostgresDatabaseUI {
    client: DatabaseClientUI,
}

impl PostgresDatabaseUI {
    pub fn new(client: DatabaseClientUI) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DatabaseUI for PostgresDatabaseUI {
    fn db_manager(&self) -> &Arc<DatabaseManager> {
        &self.client.db_manager
    }

    fn connection_string(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/{}",
            self.client.connection_input.username,
            self.client.connection_input.password,
            self.client.connection_input.hostname,
            self.client.connection_input.port,
            "postgres"
        )
    }

    async fn execute_sql_query(&self, query: &str) -> Result<(Vec<String>, String), DbError> {
        let connections = self.db_manager().connections.lock().await;
        if let Some(client) = connections.first() {
            let query_trimmed = query.trim();
            let query_upper = query_trimmed.to_uppercase();

            if query_upper.starts_with("SELECT") {
                let (column_names, data_rows) = client.query_with_column_order(query_trimmed).await?;
                
                if column_names.is_empty() {
                    return Ok((Vec::new(), "Query returned no results.".to_string()));
                }

                // Create header row
                let header_row = column_names.join("\t");

                // Convert data rows to tab-separated strings
                let data_strings: Vec<String> = data_rows
                    .into_iter()
                    .map(|row| row.join("\t"))
                    .collect();

                // Combine header and data
                let mut results = vec![header_row];
                results.extend(data_strings);

                Ok((results, String::new()))
            } else {
                client.execute(query_trimmed).await?;
                Ok((Vec::new(), "Non-SELECT query executed successfully.".to_string()))
            }
        } else {
            Err(DbError::Connection("No database connection available.".into()))
        }
    }

    async fn describe_table(&self, table_name: &str) -> Result<Vec<String>, DbError> {
        let connections = self.db_manager().connections.lock().await;
        if let Some(client) = connections.first() {
            let schema = client.describe_table(table_name).await?;
            Ok(schema.columns.into_iter().map(|c| c.name).collect())
        } else {
            Err(DbError::Connection("No database connection found".into()))
        }
    }

    async fn fetch_databases(&self) -> Result<Vec<String>, DbError> {
        let connections = self.db_manager().connections.lock().await;
        if let Some(client) = connections.first() {
            let databases = client.list_databases().await?;
            Ok(databases)
        } else {
            Err(DbError::Connection("No database connection found".into()))
        }
    }

    async fn fetch_tables(&self) -> Result<Vec<String>, DbError> {
        let connections = self.db_manager().connections.lock().await;
        if let Some(client) = connections.first() {
            let tables = client.list_tables().await?;
            Ok(tables)
        } else {
            Ok(Vec::new())
        }
    }

    async fn update_tables(&self) -> Result<(), DbError> {
        match self.fetch_tables().await {
            Ok(tables) => {
                let mut client = self.client.clone();
                client.tables = tables;
                client.selected_table = 0;
                Ok(())
            }
            Err(err) => {
                let mut client = self.client.clone();
                client.tables = Vec::new();
                client.selected_table = 0;
                Err(err)
            }
        }
    }

    async fn connect_to_selected_db(&self, db_name: &str) -> Result<(), DbError> {
        let mut connections = self.db_manager().connections.lock().await;
        connections.clear();

        let connection_string = format!(
            "postgres://{}:{}@{}:{}/{}",
            self.client.connection_input.username,
            self.client.connection_input.password,
            self.client.connection_input.hostname,
            self.client.connection_input.port,
            db_name
        );

        let client = <PostgresClient as Connect>::connect(&connection_string).await?;
        connections.push(Box::new(client) as Box<dyn DbClient + Send + Sync>);

        Ok(())
    }

    async fn connect_to_default_db(&self) -> Result<(), DbError> {
        let mut connections = self.db_manager().connections.lock().await;
        connections.clear();

        let connection_string = self.connection_string();
        let client = <PostgresClient as Connect>::connect(&connection_string).await?;
        connections.push(Box::new(client) as Box<dyn DbClient + Send + Sync>);

        Ok(())
    }
}

#[async_trait]
impl Connect for PostgresClient {
    async fn connect(database_url: &str) -> Result<Self, DbError> {
        PostgresClient::connect(database_url).await
    }
} 