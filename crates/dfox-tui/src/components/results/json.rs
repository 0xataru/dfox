//! Results as JSON documents, a row each: collapsed to a one-line preview,
//! opened field by field. Cells that hold JSON (MongoDB's nested documents,
//! a Redis string of JSON) open as JSON too.
//!
//! Only the documents that are open or on screen are read, so a result of
//! a hundred thousand rows stays quick.

use std::{
    cell::Cell as Shared,
    collections::{HashMap, HashSet},
};

use dfox_core::result::{Cell, ResultSet};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use serde_json::{Map, Value};
use unicode_width::UnicodeWidthStr;

use crate::{action::Action, component::RenderCtx};

/// Where a line is: the row, then the keys and indexes into it.
type Path = Vec<String>;

/// The JSON view's place: the line at the cursor, what is open, and the
/// first line shown.
#[derive(Debug, Default)]
pub struct JsonView {
    pub cursor: usize,
    opened: HashSet<Path>,
    top: Shared<usize>,
}

/// One line of the tree.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    path: Path,
    /// The field's name or the item's index; `None` for a document.
    label: Option<String>,
    /// The value, for lines inside an open document; read when shown for
    /// the documents themselves.
    value: Option<Value>,
}

impl Entry {
    fn depth(&self) -> usize {
        self.path.len() - 1
    }
}

/// The value of `cell` as JSON. Text that is a JSON object or list is
/// read as one.
fn json(cell: Cell<'_>) -> Value {
    match cell {
        Cell::Null => Value::Null,
        Cell::Bool(b) => Value::Bool(b),
        Cell::Int(n) => n.into(),
        Cell::UInt(n) => n.into(),
        Cell::Float(n) => {
            serde_json::Number::from_f64(n).map_or(Value::String(n.to_string()), Value::Number)
        }
        // Kept as text, so no digits are lost.
        Cell::Decimal(text) => Value::String(text.to_string()),
        Cell::Json(text) => {
            serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into()))
        }
        Cell::Text(text) => {
            let trimmed = text.trim_start();
            let parsed = (trimmed.starts_with('{') || trimmed.starts_with('['))
                .then(|| serde_json::from_str(text).ok())
                .flatten();
            parsed.unwrap_or_else(|| Value::String(text.into()))
        }
        Cell::Bytes(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            Value::String(format!("0x{hex}"))
        }
        // Dates, times and whatever else: as the grid shows them.
        other => Value::String(other.to_string()),
    }
}

/// Row `row` as a document of its columns. A row of one column that holds
/// a JSON object or list is that value itself.
pub fn document(result: &impl ResultSet, row: usize) -> Value {
    let mut fields = Map::new();
    for (col, meta) in result.columns().iter().enumerate() {
        let cell = result.cell(row, col).unwrap_or(Cell::Null);
        fields.insert(meta.name.clone(), json(cell));
    }
    if fields.len() == 1 {
        if let Some(value) = fields
            .values()
            .next()
            .filter(|v| v.is_object() || v.is_array())
        {
            return value.clone();
        }
    }
    Value::Object(fields)
}

/// The fields of an object or the items of a list, with their labels.
fn children(value: &Value) -> Vec<(String, &Value)> {
    match value {
        Value::Object(fields) => fields.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        _ => Vec::new(),
    }
}

fn is_container(value: &Value) -> bool {
    matches!(value, Value::Object(o) if !o.is_empty())
        || matches!(value, Value::Array(a) if !a.is_empty())
}

/// `value` on one line, cut at about `budget` columns: `{_id: 1, name:
/// "Ann", tags: […]}`.
fn preview(value: &Value, budget: usize) -> String {
    let mut out = String::new();
    write_preview(value, budget, 0, &mut out);
    out
}

fn write_preview(value: &Value, budget: usize, depth: usize, out: &mut String) {
    if out.len() > budget {
        return;
    }
    match value {
        Value::Object(fields) if depth > 0 && !fields.is_empty() => out.push_str("{…}"),
        Value::Array(items) if depth > 0 && !items.is_empty() => out.push_str("[…]"),
        Value::Object(fields) => {
            out.push('{');
            for (i, (key, value)) in fields.iter().enumerate() {
                if out.len() > budget {
                    out.push('…');
                    break;
                }
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&key_text(key));
                out.push_str(": ");
                write_preview(value, budget, depth + 1, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if out.len() > budget {
                    out.push('…');
                    break;
                }
                if i > 0 {
                    out.push_str(", ");
                }
                write_preview(item, budget, depth + 1, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// A key as JavaScript writes it: bare when it can be.
fn key_text(key: &str) -> String {
    let bare = key
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && key
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    if bare {
        key.to_string()
    } else {
        Value::String(key.to_string()).to_string()
    }
}

impl JsonView {
    /// Every line of the tree, with the open documents read.
    fn entries(&self, result: &impl ResultSet) -> Vec<Entry> {
        let mut out = Vec::new();
        for row in 0..result.row_count() {
            let path = vec![row.to_string()];
            if !self.opened.contains(&path) {
                out.push(Entry {
                    path,
                    label: None,
                    value: None,
                });
                continue;
            }
            let doc = document(result, row);
            out.push(Entry {
                path: path.clone(),
                label: None,
                value: Some(doc.clone()),
            });
            self.walk(&doc, &path, &mut out);
        }
        out
    }

    fn walk(&self, value: &Value, path: &Path, out: &mut Vec<Entry>) {
        for (label, child) in children(value) {
            let mut path = path.clone();
            path.push(label.clone());
            let open = is_container(child) && self.opened.contains(&path);
            out.push(Entry {
                path: path.clone(),
                label: Some(label),
                value: Some(child.clone()),
            });
            if open {
                self.walk(child, &path, out);
            }
        }
    }

    /// The JSON at the cursor, pretty-printed: for copying.
    pub fn selected(&self, result: &impl ResultSet) -> Option<String> {
        let entries = self.entries(result);
        let entry = entries.get(self.cursor.min(entries.len().checked_sub(1)?))?;
        let value = match &entry.value {
            Some(value) => value.clone(),
            None => document(result, entry.path[0].parse().ok()?),
        };
        serde_json::to_string_pretty(&value).ok()
    }

    /// Moves, opens and closes. Returns false for keys it doesn't use.
    pub fn handle(&mut self, action: Action, result: &impl ResultSet, page: usize) -> bool {
        let entries = self.entries(result);
        let Some(last) = entries.len().checked_sub(1) else {
            return false;
        };
        self.cursor = self.cursor.min(last);
        let entry = &entries[self.cursor];
        let container = |e: &Entry| e.value.as_ref().is_none_or(is_container);
        let open = self.opened.contains(&entry.path);
        match action {
            Action::Up => self.cursor = self.cursor.saturating_sub(1),
            Action::Down => self.cursor = (self.cursor + 1).min(last),
            Action::PageUp => self.cursor = self.cursor.saturating_sub(page.max(1)),
            Action::PageDown => self.cursor = (self.cursor + page.max(1)).min(last),
            Action::Home => self.cursor = 0,
            Action::End => self.cursor = last,
            Action::Right if container(entry) && !open => {
                self.opened.insert(entry.path.clone());
            }
            Action::Right if open => self.cursor = (self.cursor + 1).min(last),
            Action::Left if open => {
                self.opened.remove(&entry.path);
            }
            Action::Left if entry.path.len() > 1 => {
                let parent = &entry.path[..entry.path.len() - 1];
                if let Some(i) = entries.iter().position(|e| e.path == parent) {
                    self.cursor = i;
                }
            }
            Action::Confirm if container(entry) => {
                if !self.opened.remove(&entry.path) {
                    self.opened.insert(entry.path.clone());
                }
            }
            _ => return false,
        }
        true
    }

    pub fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        result: &impl ResultSet,
        focused: bool,
        ctx: &RenderCtx,
    ) {
        let theme = ctx.theme;
        let entries = self.entries(result);
        let height = area.height as usize;
        let cursor = self.cursor.min(entries.len().saturating_sub(1));
        // Keep the cursor in sight.
        let mut top = self.top.get();
        if cursor < top {
            top = cursor;
        } else if height > 0 && cursor >= top + height {
            top = cursor + 1 - height;
        }
        self.top.set(top);
        // Documents on screen, read once each.
        let mut docs: HashMap<usize, Value> = HashMap::new();
        let width = area.width as usize;
        let lines: Vec<Line> = entries
            .iter()
            .enumerate()
            .skip(top)
            .take(height)
            .map(|(i, entry)| {
                let row: usize = entry.path[0].parse().unwrap_or(0);
                let value = match &entry.value {
                    Some(value) => value,
                    None => docs.entry(row).or_insert_with(|| document(result, row)),
                };
                let mut spans = vec![Span::raw("  ".repeat(entry.depth()))];
                let open = self.opened.contains(&entry.path);
                spans.push(Span::styled(
                    match (is_container(value), open) {
                        (true, true) => "▾ ",
                        (true, false) => "▸ ",
                        _ => "  ",
                    },
                    theme.dim(),
                ));
                match &entry.label {
                    None => spans.push(Span::styled(format!("{} ", row + 1), theme.dim())),
                    Some(label) => spans.push(Span::styled(
                        format!("{}: ", key_text(label)),
                        theme.header(),
                    )),
                }
                let used: usize = spans.iter().map(|s| s.content.width()).sum();
                let room = width.saturating_sub(used);
                let (text, style) = match value {
                    Value::Object(fields) if open => (format!("{{{}}}", fields.len()), theme.dim()),
                    Value::Array(items) if open => (format!("[{}]", items.len()), theme.dim()),
                    Value::String(s) => (
                        Value::String(s.clone()).to_string(),
                        Style::default().fg(theme.string),
                    ),
                    Value::Number(_) => (value.to_string(), Style::default().fg(theme.number)),
                    Value::Bool(_) => (value.to_string(), Style::default().fg(theme.keyword)),
                    Value::Null => ("null".to_string(), Style::default().fg(theme.null)),
                    container => (preview(container, room), Style::default().fg(theme.text)),
                };
                spans.push(Span::styled(crate::widgets::truncate(&text, room), style));
                let line = Line::from(spans);
                if i == cursor {
                    let selected = if focused {
                        theme.selection()
                    } else {
                        theme.surface()
                    };
                    line.style(selected)
                } else {
                    line
                }
            })
            .collect();
        Paragraph::new(lines).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use dfox_core::{
        arrow::ArrowResultSet,
        result::{ColumnMeta, OwnedCell},
    };

    use super::*;

    fn people() -> ArrowResultSet {
        ArrowResultSet::from_cells(
            vec![
                ColumnMeta::new("_id", "int"),
                ColumnMeta::new("name", "string"),
                ColumnMeta::new("address", "object"),
            ],
            vec![
                vec![
                    OwnedCell::Int(1),
                    OwnedCell::Text("Ann".into()),
                    OwnedCell::Json(r#"{"city":"Kyiv","zip":"01001","tags":["a","b"]}"#.into()),
                ],
                vec![
                    OwnedCell::Int(2),
                    OwnedCell::Text("Bob".into()),
                    OwnedCell::Null,
                ],
            ],
        )
    }

    fn paths(view: &JsonView, result: &ArrowResultSet) -> Vec<String> {
        view.entries(result)
            .iter()
            .map(|e| e.path.join("/"))
            .collect()
    }

    #[test]
    fn rows_become_documents_in_column_order() {
        let doc = document(&people(), 0);
        assert_eq!(
            serde_json::to_string(&doc).expect("json"),
            r#"{"_id":1,"name":"Ann","address":{"city":"Kyiv","zip":"01001","tags":["a","b"]}}"#
        );
        assert_eq!(preview(&doc, 80), r#"{_id: 1, name: "Ann", address: {…}}"#);
        assert!(preview(&doc, 10).ends_with("…}"), "{}", preview(&doc, 10));
    }

    #[test]
    fn a_json_string_alone_is_the_document() {
        let result = ArrowResultSet::from_cells(
            vec![ColumnMeta::new("value", "string")],
            vec![vec![OwnedCell::Text(r#"{"theme":"dark"}"#.into())]],
        );
        assert_eq!(document(&result, 0), serde_json::json!({ "theme": "dark" }));
        let plain = ArrowResultSet::from_cells(
            vec![ColumnMeta::new("value", "string")],
            vec![vec![OwnedCell::Text("{not json".into())]],
        );
        assert_eq!(
            document(&plain, 0),
            serde_json::json!({ "value": "{not json" })
        );
    }

    #[test]
    fn documents_open_and_close_by_keys() {
        let result = people();
        let mut view = JsonView::default();
        assert_eq!(paths(&view, &result), ["0", "1"]);
        view.handle(Action::Right, &result, 10);
        assert_eq!(
            paths(&view, &result),
            ["0", "0/_id", "0/name", "0/address", "1"]
        );
        // Down to address, and open it.
        for _ in 0..3 {
            view.handle(Action::Down, &result, 10);
        }
        view.handle(Action::Confirm, &result, 10);
        assert_eq!(
            paths(&view, &result)[3..7],
            [
                "0/address",
                "0/address/city",
                "0/address/zip",
                "0/address/tags"
            ]
        );
        assert_eq!(
            view.selected(&result).as_deref(),
            Some("{\n  \"city\": \"Kyiv\",\n  \"zip\": \"01001\",\n  \"tags\": [\n    \"a\",\n    \"b\"\n  ]\n}")
        );
        // Left on a field goes to its parent; Left again closes it.
        view.handle(Action::Down, &result, 10);
        view.handle(Action::Left, &result, 10);
        assert_eq!(view.cursor, 3);
        view.handle(Action::Left, &result, 10);
        assert_eq!(paths(&view, &result).len(), 5);
        // A scalar doesn't open.
        view.handle(Action::Up, &result, 10);
        view.handle(Action::Right, &result, 10);
        assert_eq!(paths(&view, &result).len(), 5);
    }
}
