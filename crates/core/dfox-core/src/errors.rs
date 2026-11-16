use thiserror::Error;

/// Custom error type for database operations.
#[derive(Error, Debug)]
pub enum DbError {
    #[error("Import error: {0}")]
    Import(String),
    #[error("Export error: {0}")]
    Export(String),
    /// Configuration error (e.g., invalid database URL or missing parameters).
    #[error("Configuration error: {0}")]
    Config(String),
    /// Connection error (e.g., issues with network or database connection).
    #[error("Connection error: {0}")]
    Connection(String),
    /// The query was cancelled before it finished.
    #[error("Query cancelled")]
    Cancelled,
    /// General error with a custom message.
    #[error("Error: {0}")]
    General(String),
    /// The server refused the query and said where: `at` characters into
    /// the text it was given.
    #[error("Error: {message}")]
    At { message: String, at: usize },
}

impl DbError {
    /// The error with its place `by` characters further on, for a statement
    /// that starts there in a longer text.
    pub fn shifted(self, by: usize) -> Self {
        match self {
            DbError::At { message, at } => DbError::At {
                message,
                at: at + by,
            },
            other => other,
        }
    }
}
