pub const MAX_VISIBLE_COLUMNS: usize = 8;

/// Rows kept from a query's result. Rows past this are read and dropped.
pub const MAX_RESULT_ROWS: usize = 100_000;

#[derive(Clone)]
pub enum ScreenState {
    MessagePopup,
    DbTypeSelection,
    ConnectionInput,
    DatabaseSelection,
    TableView,
}

#[derive(Clone, PartialEq, Debug)]
pub enum FocusedWidget {
    TablesList,
    SqlEditor,
    QueryResult,
}

#[derive(Debug, Clone)]
pub enum DatabaseType {
    Postgres,
    MySQL,
    SQLite,
}

impl DatabaseType {
    pub fn as_str(&self) -> &str {
        match self {
            DatabaseType::Postgres => "Postgres",
            DatabaseType::MySQL => "MySQL",
            DatabaseType::SQLite => "SQLite",
        }
    }
}
