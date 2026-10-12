//! Query results as the UI reads them.
//!
//! The result grid depends only on [`ResultSet`], so the storage behind it can
//! change (rows today, Arrow batches later) without touching the UI.

use std::fmt;

use crate::db::hex;

/// A result column: its name as the query returned it, and the database's
/// name for its type (`int4`, `VARCHAR`, …), empty when unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
}

impl ColumnMeta {
    pub fn new(name: impl Into<String>, type_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
        }
    }
}

/// One value, borrowed from its result set.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Cell<'a> {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    /// Exact decimal, kept as the server's text so no digits are lost.
    Decimal(&'a str),
    Text(&'a str),
    Bytes(&'a [u8]),
    Json(&'a str),
    /// A value that could not be read, with the reason. Never shown as NULL.
    Undecodable(&'a str),
}

impl Cell<'_> {
    pub fn is_null(&self) -> bool {
        matches!(self, Cell::Null)
    }

    /// Numbers are right-aligned in the grid.
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Cell::Int(_) | Cell::UInt(_) | Cell::Float(_) | Cell::Decimal(_)
        )
    }
}

impl fmt::Display for Cell<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cell::Null => f.write_str("NULL"),
            Cell::Bool(v) => write!(f, "{v}"),
            Cell::Int(v) => write!(f, "{v}"),
            Cell::UInt(v) => write!(f, "{v}"),
            // `{:?}` keeps the decimal point on whole numbers: `1.0`, not `1`.
            Cell::Float(v) => write!(f, "{v:?}"),
            Cell::Decimal(v) | Cell::Text(v) | Cell::Json(v) => f.write_str(v),
            Cell::Bytes(v) => f.write_str(&hex(v)),
            Cell::Undecodable(reason) => write!(f, "⟨{reason}⟩"),
        }
    }
}

/// One owned value, as stored in a [`RowStore`].
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum OwnedCell {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Decimal(Box<str>),
    Text(Box<str>),
    Bytes(Box<[u8]>),
    Json(Box<str>),
    Undecodable(Box<str>),
}

impl OwnedCell {
    pub fn as_cell(&self) -> Cell<'_> {
        match self {
            OwnedCell::Null => Cell::Null,
            OwnedCell::Bool(v) => Cell::Bool(*v),
            OwnedCell::Int(v) => Cell::Int(*v),
            OwnedCell::UInt(v) => Cell::UInt(*v),
            OwnedCell::Float(v) => Cell::Float(*v),
            OwnedCell::Decimal(v) => Cell::Decimal(v),
            OwnedCell::Text(v) => Cell::Text(v),
            OwnedCell::Bytes(v) => Cell::Bytes(v),
            OwnedCell::Json(v) => Cell::Json(v),
            OwnedCell::Undecodable(v) => Cell::Undecodable(v),
        }
    }
}

impl From<Option<String>> for OwnedCell {
    fn from(value: Option<String>) -> Self {
        value.map_or(OwnedCell::Null, |v| OwnedCell::Text(v.into()))
    }
}

/// A table of results that may still be arriving.
pub trait ResultSet {
    fn columns(&self) -> &[ColumnMeta];
    fn row_count(&self) -> usize;
    /// The value at `row`, `col`, or `None` when either is out of range.
    fn cell(&self, row: usize, col: usize) -> Option<Cell<'_>>;
    /// `false` while more rows may still be appended.
    fn is_complete(&self) -> bool;
}

/// In-memory [`ResultSet`] that grows as rows arrive. Column names are stored
/// once, not per row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowStore {
    columns: Vec<ColumnMeta>,
    rows: Vec<Box<[OwnedCell]>>,
    complete: bool,
}

impl RowStore {
    pub fn new(columns: Vec<ColumnMeta>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
            complete: false,
        }
    }

    /// Appends a row. A row with the wrong number of values is padded with
    /// NULLs or cut to fit, so the grid never reads out of range.
    pub fn push(&mut self, row: impl Into<Vec<OwnedCell>>) {
        let mut row = row.into();
        if row.len() != self.columns.len() {
            log::warn!(
                "row has {} values for {} columns",
                row.len(),
                self.columns.len()
            );
            row.resize(self.columns.len(), OwnedCell::Null);
        }
        self.rows.push(row.into_boxed_slice());
    }

    /// Marks the result as complete: no more rows will be appended.
    pub fn finish(&mut self) {
        self.complete = true;
    }
}

impl Extend<Vec<OwnedCell>> for RowStore {
    fn extend<I: IntoIterator<Item = Vec<OwnedCell>>>(&mut self, rows: I) {
        for row in rows {
            self.push(row);
        }
    }
}

impl ResultSet for RowStore {
    fn columns(&self) -> &[ColumnMeta] {
        &self.columns
    }

    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn cell(&self, row: usize, col: usize) -> Option<Cell<'_>> {
        self.rows.get(row)?.get(col).map(OwnedCell::as_cell)
    }

    fn is_complete(&self) -> bool {
        self.complete
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> RowStore {
        RowStore::new(vec![
            ColumnMeta::new("id", "int8"),
            ColumnMeta::new("name", "text"),
        ])
    }

    #[test]
    fn rows_append_and_read_back() {
        let mut rs = store();
        assert_eq!(rs.row_count(), 0);
        assert!(!rs.is_complete());

        rs.push(vec![OwnedCell::Int(1), OwnedCell::Text("a".into())]);
        rs.extend([vec![OwnedCell::Int(2), OwnedCell::Null]]);
        rs.finish();

        assert_eq!(rs.row_count(), 2);
        assert!(rs.is_complete());
        assert_eq!(rs.columns()[1].name, "name");
        assert_eq!(rs.cell(0, 1), Some(Cell::Text("a")));
        assert_eq!(rs.cell(1, 0), Some(Cell::Int(2)));
        assert_eq!(rs.cell(1, 1), Some(Cell::Null));
    }

    #[test]
    fn out_of_range_is_none() {
        let mut rs = store();
        rs.push(vec![OwnedCell::Int(1), OwnedCell::Null]);

        assert_eq!(rs.cell(0, 2), None);
        assert_eq!(rs.cell(1, 0), None);
    }

    #[test]
    fn rows_of_the_wrong_width_are_fitted_to_the_columns() {
        let mut rs = store();
        rs.push(vec![OwnedCell::Int(1)]);
        rs.push(vec![OwnedCell::Int(2), OwnedCell::Null, OwnedCell::Int(3)]);

        assert_eq!(rs.cell(0, 1), Some(Cell::Null));
        assert_eq!(rs.cell(1, 2), None);
    }

    #[test]
    fn cells_display_like_the_server_prints_them() {
        let shown: Vec<String> = [
            Cell::Null,
            Cell::Bool(true),
            Cell::Int(-5),
            Cell::UInt(u64::MAX),
            Cell::Float(1.0),
            Cell::Float(2.5),
            Cell::Decimal("1.50"),
            Cell::Text("txt"),
            Cell::Bytes(&[0xde, 0xad]),
            Cell::Json(r#"{"a":1}"#),
            Cell::Undecodable("undecodable money"),
        ]
        .iter()
        .map(ToString::to_string)
        .collect();

        assert_eq!(
            shown,
            [
                "NULL",
                "true",
                "-5",
                "18446744073709551615",
                "1.0",
                "2.5",
                "1.50",
                "txt",
                "0xdead",
                r#"{"a":1}"#,
                "⟨undecodable money⟩",
            ]
        );
    }

    #[test]
    fn numbers_are_numeric() {
        assert!(Cell::Decimal("1").is_numeric());
        assert!(Cell::UInt(1).is_numeric());
        assert!(!Cell::Text("1").is_numeric());
        assert!(!Cell::Null.is_numeric());
        assert!(Cell::Null.is_null());
    }

    #[test]
    fn text_values_convert_from_query_output() {
        assert_eq!(OwnedCell::from(None), OwnedCell::Null);
        assert_eq!(
            OwnedCell::from(Some("x".to_string())).as_cell(),
            Cell::Text("x")
        );
    }
}
