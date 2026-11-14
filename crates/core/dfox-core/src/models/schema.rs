use serde::{Deserialize, Serialize};

/// A column of a table or view, from
/// [`Catalog::all_columns`](crate::catalog::Catalog::all_columns).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct TableColumn {
    pub table: String,
    pub name: String,
    pub data_type: String,
    /// The values an enum column takes, in order; empty for other types.
    #[serde(default)]
    pub values: Vec<String>,
}
