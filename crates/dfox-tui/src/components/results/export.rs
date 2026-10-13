//! Turning part of a result into text to paste elsewhere.

use std::{fmt::Write, ops::Range};

use dfox_core::result::{Cell, ResultSet};

use crate::sql::{self, Dialect};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Tsv,
    Csv,
    Json,
    Markdown,
    Insert,
}

impl Format {
    /// The key that picks each format, and its name.
    pub const ALL: &'static [(char, Format, &'static str)] = &[
        ('t', Format::Tsv, "TSV"),
        ('c', Format::Csv, "CSV"),
        ('j', Format::Json, "JSON"),
        ('m', Format::Markdown, "Markdown"),
        ('i', Format::Insert, "SQL INSERT"),
    ];

    pub fn for_key(key: char) -> Option<(Format, &'static str)> {
        Self::ALL
            .iter()
            .find(|(k, _, _)| *k == key.to_ascii_lowercase())
            .map(|(_, format, name)| (*format, *name))
    }
}

/// A rectangle of a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub rows: Range<usize>,
    pub cols: Range<usize>,
}

impl Region {
    pub fn cells(&self) -> usize {
        self.rows.len() * self.cols.len()
    }
}

/// Where INSERT statements go.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    pub table: &'a str,
    pub dialect: Dialect,
}

/// What `Ctrl+C` copies: one value as it is, or tab-separated lines without
/// a header.
pub fn plain(result: &impl ResultSet, region: &Region) -> String {
    if region.cells() == 1 {
        return match result.cell(region.rows.start, region.cols.start) {
            Some(Cell::Null) | None => String::new(),
            Some(value) => value.to_string(),
        };
    }
    let mut out = String::new();
    for row in region.rows.clone() {
        delimited_row(
            &mut out,
            '\t',
            region.cols.clone().map(|col| text(result, row, col)),
        );
    }
    out
}

pub fn export(result: &impl ResultSet, region: &Region, format: Format, target: Target) -> String {
    let names: Vec<&str> = region
        .cols
        .clone()
        .map(|col| result.columns().get(col).map_or("", |c| c.name.as_str()))
        .collect();
    let mut out = String::new();
    match format {
        Format::Tsv | Format::Csv => {
            let separator = if format == Format::Tsv { '\t' } else { ',' };
            delimited_row(&mut out, separator, names.iter().map(|n| n.to_string()));
            for row in region.rows.clone() {
                delimited_row(
                    &mut out,
                    separator,
                    region.cols.clone().map(|col| text(result, row, col)),
                );
            }
        }
        Format::Json => {
            out.push('[');
            for (i, row) in region.rows.clone().enumerate() {
                out.push_str(if i == 0 { "\n  {" } else { ",\n  {" });
                for (j, col) in region.cols.clone().enumerate() {
                    if j > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&json_string(names[j]));
                    out.push_str(": ");
                    out.push_str(&json_value(result.cell(row, col).unwrap_or(Cell::Null)));
                }
                out.push('}');
            }
            out.push_str(if region.rows.is_empty() {
                "]\n"
            } else {
                "\n]\n"
            });
        }
        Format::Markdown => {
            let header: Vec<String> = names.iter().map(|n| markdown(n)).collect();
            let _ = writeln!(out, "| {} |", header.join(" | "));
            let rules: Vec<&str> = region
                .cols
                .clone()
                .map(|col| {
                    let numeric = region
                        .rows
                        .clone()
                        .filter_map(|row| result.cell(row, col))
                        .find(|cell| !cell.is_null())
                        .is_some_and(|cell| cell.is_numeric());
                    if numeric {
                        "--:"
                    } else {
                        "---"
                    }
                })
                .collect();
            let _ = writeln!(out, "| {} |", rules.join(" | "));
            for row in region.rows.clone() {
                let cells: Vec<String> = region
                    .cols
                    .clone()
                    .map(|col| match result.cell(row, col) {
                        Some(Cell::Null) | None => "NULL".to_string(),
                        Some(value) => markdown(&value.to_string()),
                    })
                    .collect();
                let _ = writeln!(out, "| {} |", cells.join(" | "));
            }
        }
        Format::Insert => {
            let columns: Vec<String> = names
                .iter()
                .map(|n| sql::quote_ident(n, target.dialect))
                .collect();
            let _ = write!(
                out,
                "INSERT INTO {} ({}) VALUES",
                target.table,
                columns.join(", ")
            );
            for (i, row) in region.rows.clone().enumerate() {
                let values: Vec<String> = region
                    .cols
                    .clone()
                    .map(|col| literal(result.cell(row, col).unwrap_or(Cell::Null), target.dialect))
                    .collect();
                out.push_str(if i == 0 { "\n  (" } else { ",\n  (" });
                out.push_str(&values.join(", "));
                out.push(')');
            }
            out.push_str(";\n");
        }
    }
    out
}

/// A value for TSV and CSV: NULL is empty.
fn text(result: &impl ResultSet, row: usize, col: usize) -> String {
    match result.cell(row, col) {
        Some(Cell::Null) | None => String::new(),
        Some(value) => value.to_string(),
    }
}

/// One line of separated values, quoted where a value needs it.
fn delimited_row(out: &mut String, separator: char, values: impl Iterator<Item = String>) {
    for (i, value) in values.enumerate() {
        if i > 0 {
            out.push(separator);
        }
        if value.contains([separator, '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&value.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(&value);
        }
    }
    out.push('\n');
}

fn json_string(text: &str) -> String {
    serde_json::Value::String(text.to_string()).to_string()
}

fn json_value(cell: Cell<'_>) -> String {
    match cell {
        Cell::Null => "null".to_string(),
        Cell::Bool(v) => v.to_string(),
        Cell::Int(v) => v.to_string(),
        Cell::UInt(v) => v.to_string(),
        Cell::Float(v) if v.is_finite() => cell.to_string(),
        // Exact decimals stay exact; JSON has no limit on digits.
        Cell::Decimal(v) if serde_json::from_str::<serde_json::Number>(v).is_ok() => v.to_string(),
        Cell::Json(v) if serde_json::from_str::<serde::de::IgnoredAny>(v).is_ok() => {
            v.trim().to_string()
        }
        other => json_string(&other.to_string()),
    }
}

fn markdown(text: &str) -> String {
    text.replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace('\n', "<br>")
}

/// A value written as SQL.
fn literal(cell: Cell<'_>, dialect: Dialect) -> String {
    match cell {
        Cell::Null => "NULL".to_string(),
        Cell::Bool(true) => "TRUE".to_string(),
        Cell::Bool(false) => "FALSE".to_string(),
        Cell::Int(_) | Cell::UInt(_) => cell.to_string(),
        Cell::Float(v) if v.is_finite() => cell.to_string(),
        Cell::Decimal(v) if v.parse::<f64>().is_ok_and(f64::is_finite) => v.to_string(),
        Cell::Bytes(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            match dialect {
                Dialect::Postgres => format!("'\\x{hex}'"),
                Dialect::MySql | Dialect::Sqlite => format!("X'{hex}'"),
            }
        }
        other => {
            let mut text = other.to_string().replace('\'', "''");
            // MySQL reads backslashes in strings as escapes by default.
            if dialect == Dialect::MySql {
                text = text.replace('\\', "\\\\");
            }
            format!("'{text}'")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dfox_core::result::{ColumnMeta, OwnedCell, RowStore};

    fn sample() -> RowStore {
        let mut store = RowStore::new(vec![
            ColumnMeta::new("id", "int4"),
            ColumnMeta::new("Name", "text"),
            ColumnMeta::new("price", "numeric"),
            ColumnMeta::new("data", "jsonb"),
        ]);
        store.push(vec![
            OwnedCell::Int(1),
            OwnedCell::Text("O'Brien, \"Bob\"".into()),
            OwnedCell::Decimal("12.50".into()),
            OwnedCell::Json("{\"a\": 1}".into()),
        ]);
        store.push(vec![
            OwnedCell::Int(2),
            OwnedCell::Text("a|b\nc\\d".into()),
            OwnedCell::Null,
            OwnedCell::Bytes(vec![0xde, 0xad].into()),
        ]);
        store.finish();
        store
    }

    fn all() -> Region {
        Region {
            rows: 0..2,
            cols: 0..4,
        }
    }

    fn target(dialect: Dialect) -> Target<'static> {
        Target {
            table: "public.items",
            dialect,
        }
    }

    #[test]
    fn one_cell_copies_as_itself() {
        let store = sample();
        let cell = |row, col| Region {
            rows: row..row + 1,
            cols: col..col + 1,
        };
        assert_eq!(plain(&store, &cell(0, 1)), "O'Brien, \"Bob\"");
        assert_eq!(plain(&store, &cell(1, 2)), "");
        assert_eq!(
            plain(
                &store,
                &Region {
                    rows: 0..2,
                    cols: 0..1
                }
            ),
            "1\n2\n"
        );
    }

    #[test]
    fn csv_and_tsv_quote_what_needs_it() {
        let store = sample();
        assert_eq!(
            export(&store, &all(), Format::Csv, target(Dialect::Postgres)),
            "id,Name,price,data\n\
             1,\"O'Brien, \"\"Bob\"\"\",12.50,\"{\"\"a\"\": 1}\"\n\
             2,\"a|b\nc\\d\",,0xdead\n"
        );
        assert_eq!(
            export(&store, &all(), Format::Tsv, target(Dialect::Postgres)),
            "id\tName\tprice\tdata\n\
             1\t\"O'Brien, \"\"Bob\"\"\"\t12.50\t\"{\"\"a\"\": 1}\"\n\
             2\t\"a|b\nc\\d\"\t\t0xdead\n"
        );
    }

    #[test]
    fn json_keeps_types_and_embeds_json_values() {
        let store = sample();
        assert_eq!(
            export(&store, &all(), Format::Json, target(Dialect::Postgres)),
            "[\n  \
             {\"id\": 1, \"Name\": \"O'Brien, \\\"Bob\\\"\", \"price\": 12.50, \"data\": {\"a\": 1}},\n  \
             {\"id\": 2, \"Name\": \"a|b\\nc\\\\d\", \"price\": null, \"data\": \"0xdead\"}\n]\n"
        );
        let empty = Region {
            rows: 0..0,
            cols: 0..1,
        };
        assert_eq!(
            export(&store, &empty, Format::Json, target(Dialect::Postgres)),
            "[]\n"
        );
    }

    #[test]
    fn markdown_escapes_pipes_and_aligns_numbers() {
        let store = sample();
        assert_eq!(
            export(&store, &all(), Format::Markdown, target(Dialect::Postgres)),
            "| id | Name | price | data |\n\
             | --: | --- | --: | --- |\n\
             | 1 | O'Brien, \"Bob\" | 12.50 | {\"a\": 1} |\n\
             | 2 | a\\|b<br>c\\d | NULL | 0xdead |\n"
        );
    }

    #[test]
    fn inserts_quote_for_the_driver() {
        let store = sample();
        assert_eq!(
            export(&store, &all(), Format::Insert, target(Dialect::Postgres)),
            "INSERT INTO public.items (id, \"Name\", price, data) VALUES\n  \
             (1, 'O''Brien, \"Bob\"', 12.50, '{\"a\": 1}'),\n  \
             (2, 'a|b\nc\\d', NULL, '\\xdead');\n"
        );
        let mysql = export(&store, &all(), Format::Insert, target(Dialect::MySql));
        assert!(mysql.contains("(id, `Name`, price, data)"), "{mysql}");
        assert!(mysql.contains("'a|b\nc\\\\d', NULL, X'dead'"), "{mysql}");
    }

    #[test]
    fn literals_cover_every_kind() {
        assert_eq!(literal(Cell::Bool(true), Dialect::Sqlite), "TRUE");
        assert_eq!(literal(Cell::Bool(false), Dialect::Sqlite), "FALSE");
        assert_eq!(literal(Cell::Float(1.5), Dialect::Sqlite), "1.5");
        assert_eq!(literal(Cell::Float(f64::NAN), Dialect::Postgres), "'NaN'");
        assert_eq!(literal(Cell::Decimal("NaN"), Dialect::Postgres), "'NaN'");
        assert_eq!(literal(Cell::UInt(7), Dialect::MySql), "7");
        assert_eq!(json_value(Cell::Float(f64::INFINITY)), "\"inf\"");
        assert_eq!(json_value(Cell::Json("not json")), "\"not json\"");
        assert_eq!(json_value(Cell::Bool(true)), "true");
        assert_eq!(json_value(Cell::UInt(3)), "3");
        assert_eq!(sql::quote_ident("a\"b", Dialect::Sqlite), "\"a\"\"b\"");
    }

    #[test]
    fn format_keys() {
        assert_eq!(Format::for_key('J'), Some((Format::Json, "JSON")));
        assert_eq!(Format::for_key('x'), None);
    }
}
