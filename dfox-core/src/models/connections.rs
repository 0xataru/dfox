use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub enum DbType {
    Postgres,
    MySql,
    Sqlite,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ConnectionConfig {
    pub db_type: DbType,
    pub database_url: String,
}

/// Server connection details as typed by the user. Clients build driver
/// options from these fields directly, so no value is ever spliced into a URL
/// and passwords may contain any character.
#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct ConnectionParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub database: Option<String>,
}
