//! Results as JSON documents, a row each: collapsed to a one-line preview,
//! opened field by field. Cells that hold JSON (MongoDB's nested documents,
//! a Redis string of JSON) open as JSON too.
//!
//! Only the documents that are open or on screen are read, so a result of
//! a hundred thousand rows stays quick.

use std::{
    cell::Cell as Shared,
    collections::{BTreeMap, HashMap, HashSet},
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

/// Where a line is in its document: the keys and indexes into it, none
/// for the document itself.
type Path = Vec<String>;

/// The JSON view's place: the line at the cursor, the open documents with
/// their lines, and the first line shown.
#[derive(Debug, Default)]
pub struct JsonView {
    pub cursor: usize,
    /// What is open in each open document, the document itself as the
    /// empty path.
    opened: HashMap<usize, HashSet<Path>>,
    /// The lines of each open document, its own line first.
    lines: BTreeMap<usize, Vec<Entry>>,
    top: Shared<usize>,
}

/// One line of an open document.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    path: Path,
    /// The field's name or the item's index; `None` for the document.
    label: Option<String>,
    value: Value,
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
    /// How many lines the tree has: a line a document, and the lines of
    /// the open ones.
    fn total(&self, result: &impl ResultSet) -> usize {
        result.row_count() + self.lines.values().map(|l| l.len() - 1).sum::<usize>()
    }

    /// The row of `line`, and which of its lines it is: 0 for the
    /// document's own.
    fn locate(&self, line: usize) -> (usize, usize) {
        let mut before = 0;
        for (&row, lines) in &self.lines {
            let start = row + before;
            if line < start {
                break;
            }
            if line < start + lines.len() {
                return (row, line - start);
            }
            before += lines.len() - 1;
        }
        (line - before, 0)
    }

    /// The first line of `row`.
    fn line_of(&self, row: usize) -> usize {
        row + self
            .lines
            .range(..row)
            .map(|(_, l)| l.len() - 1)
            .sum::<usize>()
    }

    /// Reads `row` again after something in it opened or closed.
    fn relist(&mut self, result: &impl ResultSet, row: usize) {
        let opened = self.opened.get(&row);
        if !opened.is_some_and(|o| o.contains(&Path::new())) {
            self.opened.remove(&row);
            self.lines.remove(&row);
            return;
        }
        let doc = document(result, row);
        let mut lines = vec![Entry {
            path: Path::new(),
            label: None,
            value: doc.clone(),
        }];
        walk(
            &doc,
            &Path::new(),
            opened.unwrap_or(&HashSet::new()),
            &mut lines,
        );
        self.lines.insert(row, lines);
    }

    /// The line at `line`: of an open document, or `None` for a closed one.
    fn entry(&self, line: usize) -> (usize, Option<&Entry>) {
        let (row, inner) = self.locate(line);
        (row, self.lines.get(&row).and_then(|l| l.get(inner)))
    }

    /// The JSON at the cursor, pretty-printed: for copying.
    pub fn selected(&self, result: &impl ResultSet) -> Option<String> {
        let last = self.total(result).checked_sub(1)?;
        let value = match self.entry(self.cursor.min(last)) {
            (_, Some(entry)) => entry.value.clone(),
            (row, None) => document(result, row),
        };
        serde_json::to_string_pretty(&value).ok()
    }

    /// Moves, opens and closes. Returns false for keys it doesn't use.
    pub fn handle(&mut self, action: Action, result: &impl ResultSet, page: usize) -> bool {
        let Some(last) = self.total(result).checked_sub(1) else {
            return false;
        };
        self.cursor = self.cursor.min(last);
        let (row, entry) = self.entry(self.cursor);
        let path = entry.map_or(Path::new(), |e| e.path.clone());
        let container = entry.is_none_or(|e| is_container(&e.value));
        let open = self.opened.get(&row).is_some_and(|o| o.contains(&path));
        match action {
            Action::Up => self.cursor = self.cursor.saturating_sub(1),
            Action::Down => self.cursor = (self.cursor + 1).min(last),
            Action::PageUp => self.cursor = self.cursor.saturating_sub(page.max(1)),
            Action::PageDown => self.cursor = (self.cursor + page.max(1)).min(last),
            Action::Home => self.cursor = 0,
            Action::End => self.cursor = last,
            Action::Right if container && !open => self.toggle(result, row, path),
            Action::Right if open => self.cursor = (self.cursor + 1).min(last),
            Action::Left if open => self.toggle(result, row, path),
            Action::Left if !path.is_empty() => {
                let parent = &path[..path.len() - 1];
                if let Some(i) = self.lines[&row].iter().position(|e| e.path == parent) {
                    self.cursor = self.line_of(row) + i;
                }
            }
            Action::Confirm if container => self.toggle(result, row, path),
            _ => return false,
        }
        true
    }

    /// Opens `path` of `row` when closed, closes it when open.
    fn toggle(&mut self, result: &impl ResultSet, row: usize, path: Path) {
        let opened = self.opened.entry(row).or_default();
        if path.is_empty() && opened.contains(&path) {
            // Closing a document forgets what was open in it.
            opened.clear();
        } else if !opened.remove(&path) {
            opened.insert(path);
        }
        self.relist(result, row);
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
        let total = self.total(result);
        let height = area.height as usize;
        let cursor = self.cursor.min(total.saturating_sub(1));
        // Keep the cursor in sight.
        let mut top = self.top.get();
        if cursor < top {
            top = cursor;
        } else if height > 0 && cursor >= top + height {
            top = cursor + 1 - height;
        }
        self.top.set(top);
        let width = area.width as usize;
        let lines: Vec<Line> = (top..total.min(top + height))
            .map(|i| {
                let (row, entry) = self.entry(i);
                let closed;
                let (path, label, value) = match entry {
                    Some(e) => (&e.path, e.label.as_ref(), &e.value),
                    None => {
                        closed = document(result, row);
                        (&Path::new(), None, &closed)
                    }
                };
                let open = self.opened.get(&row).is_some_and(|o| o.contains(path));
                let mut spans = vec![Span::raw("  ".repeat(path.len()))];
                spans.push(Span::styled(
                    match (is_container(value), open) {
                        (true, true) => "▾ ",
                        (true, false) => "▸ ",
                        _ => "  ",
                    },
                    theme.dim(),
                ));
                match label {
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
                    Value::String(_) => (value.to_string(), Style::default().fg(theme.string)),
                    Value::Number(_) => (value.to_string(), Style::default().fg(theme.number)),
                    Value::Bool(_) => (value.to_string(), Style::default().fg(theme.keyword)),
                    Value::Null => ("null".to_string(), Style::default().fg(theme.null)),
                    container => (preview(container, room), Style::default().fg(theme.text)),
                };
                spans.push(Span::styled(crate::widgets::truncate(&text, room), style));
                let line = Line::from(spans);
                if i == cursor {
                    line.style(if focused {
                        theme.selection()
                    } else {
                        theme.surface()
                    })
                } else {
                    line
                }
            })
            .collect();
        Paragraph::new(lines).render(area, buf);
    }
}

/// The lines under `value` at `path`, and under what of it is `opened`.
fn walk(value: &Value, path: &Path, opened: &HashSet<Path>, out: &mut Vec<Entry>) {
    for (label, child) in children(value) {
        let mut path = path.clone();
        path.push(label.clone());
        let open = is_container(child) && opened.contains(&path);
        out.push(Entry {
            path: path.clone(),
            label: Some(label),
            value: child.clone(),
        });
        if open {
            walk(child, &path, opened, out);
        }
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
        (0..view.total(result))
            .map(|i| {
                let (row, entry) = view.entry(i);
                let mut parts = vec![row.to_string()];
                parts.extend(entry.iter().flat_map(|e| e.path.clone()));
                parts.join("/")
            })
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

/// `cargo test -p dfox --release -- --ignored json_view_time --nocapture`
#[cfg(test)]
#[test]
#[ignore = "timing; run in release"]
fn json_view_time() {
    use std::time::{Duration, Instant};

    use dfox_core::{
        arrow::ArrowResultSet,
        result::{ColumnMeta, OwnedCell},
    };

    use crate::testing::TestCtx;

    // 100,000 documents as MongoDB sends them: scalars and nested JSON.
    let rows = 100_000;
    let result = ArrowResultSet::from_cells(
        ["_id", "name", "age", "email", "address", "tags", "orders"]
            .iter()
            .map(|c| ColumnMeta::new(*c, "string"))
            .collect(),
        (0..rows)
            .map(|i| {
                vec![
                    OwnedCell::Text(format!("6650a1{i:06}").into()),
                    OwnedCell::Text(format!("user {i}").into()),
                    OwnedCell::Int(i as i64 % 90),
                    OwnedCell::Text(format!("user{i}@example.com").into()),
                    OwnedCell::Json(
                        format!(
                            r#"{{"city":"Kyiv","zip":"{i:05}","geo":{{"lat":50.45,"lon":30.52}}}}"#
                        )
                        .into(),
                    ),
                    OwnedCell::Json(r#"["a","b","c"]"#.into()),
                    OwnedCell::Json(
                        r#"[{"sku":"x1","qty":2,"price":9.5},{"sku":"y2","qty":1,"price":20}]"#
                            .into(),
                    ),
                ]
            })
            .collect(),
    );
    let test = TestCtx::new();
    let ctx = test.render_ctx();
    let area = Rect::new(0, 0, 200, 60);
    let mut buf = Buffer::empty(area);
    let p99 = |mut times: Vec<Duration>| {
        times.sort();
        times[times.len() * 99 / 100]
    };

    let mut view = JsonView::default();
    view.handle(Action::End, &result, 60);
    let render = p99((0..200)
        .map(|_| {
            let start = Instant::now();
            view.render(&mut buf, area, &result, true, &ctx);
            start.elapsed()
        })
        .collect());
    let keys = p99((0..200)
        .map(|_| {
            let start = Instant::now();
            view.handle(Action::Up, &result, 60);
            start.elapsed()
        })
        .collect());
    // A hundred documents open, each with its address and orders.
    for row in 0..100 {
        for path in [
            vec![],
            vec!["address".to_string()],
            vec!["orders".to_string()],
        ] {
            view.toggle(&result, row, path);
        }
    }
    view.handle(Action::Home, &result, 60);
    let opened = p99((0..200)
        .map(|_| {
            let start = Instant::now();
            view.handle(Action::Down, &result, 60);
            view.render(&mut buf, area, &result, true, &ctx);
            start.elapsed()
        })
        .collect());
    println!("json view, {rows} rows: render p99 {render:?}, key p99 {keys:?}, key+render with 100 open p99 {opened:?}");
    // The grid's budget: a frame in 2 ms.
    assert!(render < Duration::from_millis(2), "{render:?}");
    assert!(opened < Duration::from_millis(2), "{opened:?}");
}
