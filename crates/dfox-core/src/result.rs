//! Query results as the UI reads them.
//!
//! The result grid depends only on [`ResultSet`], so it doesn't care how the
//! rows are stored: [`ArrowResultSet`](crate::arrow::ArrowResultSet) holds
//! them as Arrow batches.

use std::fmt;

/// Bytes as `0x…` hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

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

/// One owned value: a parameter, an edited cell, or a value the app made.
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

    /// The value as it is displayed, or `None` for NULL.
    pub fn into_text(self) -> Option<String> {
        match self {
            OwnedCell::Null => None,
            OwnedCell::Text(v) => Some(v.into()),
            other => Some(other.as_cell().to_string()),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_has_a_prefix_even_when_empty() {
        assert_eq!(hex(&[0xde, 0xad]), "0xdead");
        assert_eq!(hex(&[]), "0x");
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
    fn into_text_keeps_null_apart() {
        assert_eq!(OwnedCell::Null.into_text(), None);
        assert_eq!(OwnedCell::Text("".into()).into_text().as_deref(), Some(""));
        assert_eq!(OwnedCell::Int(7).into_text().as_deref(), Some("7"));
    }
}
