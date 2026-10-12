use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
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
    #[serde(default)]
    pub tls: TlsMode,
    /// Refuse writes. The server enforces it: Postgres and MySQL start every
    /// session read-only, and SQLite opens the file read-only.
    #[serde(default)]
    pub read_only: bool,
}

/// Whether the connection to a server is encrypted.
#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// Encrypt when the server supports it.
    #[default]
    Prefer,
    /// Fail unless the connection is encrypted.
    Require,
    Disable,
}
