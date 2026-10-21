use serde::{Deserialize, Serialize};

/// A column of a table or view, from
/// [`Catalog::all_columns`](crate::catalog::Catalog::all_columns).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct TableColumn {
    pub table: String,
    pub name: String,
    pub data_type: String,
}
