use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct TableSchema {
    /// The schema (Postgres), database (MySQL) or attached database (SQLite).
    #[serde(default)]
    pub schema: String,
    pub table_name: String,
    pub columns: Vec<ColumnSchema>,
    pub indexes: Vec<IndexSchema>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ColumnSchema {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
    pub default: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct IndexSchema {
    pub name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
}

/// What kind of thing an object in a schema is.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    ForeignTable,
    Function,
    Procedure,
}

impl ObjectKind {
    /// Tables and views hold rows; functions and procedures don't.
    pub fn has_rows(self) -> bool {
        !matches!(self, ObjectKind::Function | ObjectKind::Procedure)
    }
}

/// A table, view or routine in a schema.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DbObject {
    pub name: String,
    pub kind: ObjectKind,
    /// The planner's row estimate, when the database keeps one.
    pub rows: Option<i64>,
}
