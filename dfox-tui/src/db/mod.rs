use std::sync::Arc;

use dfox_core::{
    db::{QueryOutput, DEFAULT_ROW_LIMIT},
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
    DbManager,
};

use crate::ui::DatabaseClientUI;

/// Database calls for a Postgres or MySQL server, using the connection
/// details typed into the connection screen.
pub struct ServerDatabaseUI {
    client: DatabaseClientUI,
    db_type: DbType,
}

impl ServerDatabaseUI {
    pub fn new(client: DatabaseClientUI, db_type: DbType) -> Self {
        Self { client, db_type }
    }

    fn db_manager(&self) -> &Arc<DbManager> {
        &self.client.db_manager
    }

    fn default_database(&self) -> &'static str {
        match self.db_type {
            DbType::Postgres => "postgres",
            DbType::MySql => "mysql",
            DbType::Sqlite => "main",
        }
    }

    fn connection_params(&self, database: &str) -> Result<ConnectionParams, DbError> {
        let input = &self.client.connection_input;
        let port = match input.port.trim() {
            "" => match self.db_type {
                DbType::MySql => 3306,
                _ => 5432,
            },
            port => port
                .parse()
                .map_err(|_| DbError::Config(format!("Invalid port: {port}")))?,
        };
        Ok(ConnectionParams {
            host: input.hostname.trim().to_string(),
            port,
            username: input.username.clone(),
            password: input.password.clone(),
            database: Some(database.to_string()),
        })
    }

    pub async fn execute_sql_query(&self, query: &str) -> Result<QueryOutput, DbError> {
        let connections = self.db_manager().connections.lock().await;
        let client = connections
            .first()
            .ok_or_else(|| DbError::Connection("No database connection available.".into()))?;
        client.run(query.trim(), DEFAULT_ROW_LIMIT).await
    }

    pub async fn describe_table(&self, table_name: &str) -> Result<Vec<String>, DbError> {
        let connections = self.db_manager().connections.lock().await;
        let client = connections
            .first()
            .ok_or_else(|| DbError::Connection("No database connection found".into()))?;
        let schema = client.describe_table(table_name).await?;
        Ok(schema.columns.into_iter().map(|c| c.name).collect())
    }

    pub async fn fetch_databases(&self) -> Result<Vec<String>, DbError> {
        let connections = self.db_manager().connections.lock().await;
        let client = connections
            .first()
            .ok_or_else(|| DbError::Connection("No database connection found".into()))?;
        client.list_databases().await
    }

    pub async fn fetch_tables(&self) -> Result<Vec<String>, DbError> {
        let connections = self.db_manager().connections.lock().await;
        match connections.first() {
            Some(client) => client.list_tables().await,
            None => Ok(Vec::new()),
        }
    }

    pub async fn connect_to_selected_db(&self, db_name: &str) -> Result<(), DbError> {
        let params = self.connection_params(db_name)?;
        self.replace_connection(params).await
    }

    pub async fn connect_to_default_db(&self) -> Result<(), DbError> {
        let params = self.connection_params(self.default_database())?;
        self.replace_connection(params).await
    }

    async fn replace_connection(&self, params: ConnectionParams) -> Result<(), DbError> {
        let client = DbManager::connect(&self.db_type, &params).await?;
        let mut connections = self.db_manager().connections.lock().await;
        connections.clear();
        connections.push(client);
        Ok(())
    }
}
