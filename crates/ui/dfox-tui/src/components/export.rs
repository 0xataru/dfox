//! Which file to export rows to, or to import rows from: CSV, TSV, JSON
//! Lines or Parquet, by its extension.

use std::path::PathBuf;

use dfox_core::{catalog::NodePath, export::Format};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthChar;

use super::editor::buffer::{Buffer, Motion};
use crate::{action::Action, component::RenderCtx, connections::expand_home, widgets::truncate};

/// What the dialog was closed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogEvent {
    Close,
    Export(PathBuf),
}

#[derive(Debug)]
pub struct ExportDialog {
    /// The query whose rows go to the file.
    pub sql: String,
    /// About how many rows it returns, when the table says.
    pub estimate: Option<u64>,
    /// The table the file's rows go into, when importing.
    pub import: Option<NodePath>,
    /// That table's columns, when known: the file's are matched to them.
    table_columns: Vec<String>,
    /// Once an import's file is chosen: which column each of its goes into.
    mapping: Option<Mapping>,
    path: Buffer,
    error: Option<String>,
}

/// The file's columns, and the table column each goes into.
#[derive(Debug)]
struct Mapping {
    file: PathBuf,
    from: Vec<String>,
    /// An index into the table's columns; `None` leaves the column out.
    to: Vec<Option<usize>>,
    row: usize,
}

impl ExportDialog {
    /// A dialog to export `sql`'s rows, offering `<name>.csv` in the
    /// current folder.
    pub fn new(name: &str, sql: String, estimate: Option<u64>) -> Self {
        let folder = std::env::current_dir().unwrap_or_default();
        let mut path = Buffer::default();
        path.set_text(&folder.join(format!("{name}.csv")).to_string_lossy());
        path.move_cursor(Motion::End, false);
        Self {
            sql,
            estimate,
            import: None,
            table_columns: Vec::new(),
            mapping: None,
            path,
            error: None,
        }
    }

    /// A dialog to import a file's rows into the table at `table`, whose
    /// `columns` the file's are matched to (none: by name, in the engine).
    pub fn import(table: NodePath, columns: Vec<String>) -> Self {
        let name = table
            .name()
            .unwrap_or("rows")
            .replace(['/', '\\', ':'], "_");
        Self {
            import: Some(table),
            table_columns: columns,
            ..Self::new(&name, String::new(), None)
        }
    }

    /// The table column for each of the file's, `None` skipping it, once
    /// they are mapped.
    pub fn columns(&self) -> Option<Vec<Option<String>>> {
        let mapping = self.mapping.as_ref()?;
        Some(
            mapping
                .to
                .iter()
                .map(|to| to.map(|i| self.table_columns[i].clone()))
                .collect(),
        )
    }

    /// Keys while the file's columns are being mapped.
    fn handle_mapping(&mut self, action: Action) -> Option<DialogEvent> {
        let mapping = self.mapping.as_mut()?;
        let choices = self.table_columns.len() + 1;
        match action {
            Action::Up => mapping.row = mapping.row.saturating_sub(1),
            Action::Down => mapping.row = (mapping.row + 1).min(mapping.from.len() - 1),
            Action::Left | Action::Right => {
                // Skip, then each of the table's columns, round.
                let at = mapping.to[mapping.row].map_or(0, |i| i + 1);
                let step = if action == Action::Right {
                    1
                } else {
                    choices - 1
                };
                let next = (at + step) % choices;
                mapping.to[mapping.row] = next.checked_sub(1);
            }
            Action::Confirm if mapping.to.iter().all(Option::is_none) => {
                self.error = Some("map at least one column".into());
                return None;
            }
            Action::Confirm => {
                let mut seen = std::collections::HashSet::new();
                if let Some(twice) = mapping.to.iter().flatten().find(|i| !seen.insert(**i)) {
                    self.error = Some(format!("{} is mapped twice", self.table_columns[*twice]));
                    return None;
                }
                return Some(DialogEvent::Export(mapping.file.clone()));
            }
            Action::Back => self.mapping = None,
            _ => return None,
        }
        self.error = None;
        None
    }

    /// Reads the file's columns and matches each to the table's of the same
    /// name, whatever its case.
    fn map(&mut self, file: PathBuf) -> Result<(), String> {
        let from = dfox_core::export::columns(&file).map_err(|err| err.to_string())?;
        if from.is_empty() {
            return Err(format!("{} has no columns", file.display()));
        }
        let to = from
            .iter()
            .map(|name| {
                self.table_columns
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(name))
            })
            .collect();
        self.mapping = Some(Mapping {
            file,
            from,
            to,
            row: 0,
        });
        Ok(())
    }

    pub fn handle(&mut self, action: Action) -> Option<DialogEvent> {
        if self.mapping.is_some() {
            return self.handle_mapping(action);
        }
        match action {
            Action::Input(c) => self.path.insert_char(c),
            Action::Backspace => self.path.backspace(),
            Action::DeleteWord => self.path.delete_word(),
            Action::Left => self.path.move_cursor(Motion::Left, false),
            Action::Right => self.path.move_cursor(Motion::Right, false),
            Action::WordLeft => self.path.move_cursor(Motion::WordLeft, false),
            Action::WordRight => self.path.move_cursor(Motion::WordRight, false),
            Action::Home => self.path.move_cursor(Motion::Home, false),
            Action::End => self.path.move_cursor(Motion::End, false),
            Action::Back => return Some(DialogEvent::Close),
            Action::Confirm => match self.target() {
                // An import into a table whose columns are known asks how
                // the file's map to them first.
                Ok(path) if self.import.is_some() && !self.table_columns.is_empty() => {
                    if let Err(why) = self.map(path) {
                        self.error = Some(why);
                    }
                    return None;
                }
                Ok(path) => return Some(DialogEvent::Export(path)),
                Err(why) => {
                    self.error = Some(why);
                    return None;
                }
            },
            _ => return None,
        }
        self.error = None;
        None
    }

    /// The file named, if it can be written.
    fn target(&self) -> Result<PathBuf, String> {
        let path = PathBuf::from(expand_home(self.path.text().trim()));
        Format::of(&path)?;
        if self.import.is_some() {
            return match path.is_file() {
                true => Ok(path),
                false => Err(format!("there is no file {}", path.display())),
            };
        }
        match path.parent() {
            Some(folder) if !folder.as_os_str().is_empty() && !folder.is_dir() => {
                Err(format!("there is no folder {}", folder.display()))
            }
            _ => Ok(path),
        }
    }

    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        if let Some(mapping) = &self.mapping {
            return self.render_mapping(mapping, f, area, ctx);
        }
        let theme = ctx.theme;
        let width = area.width.saturating_sub(4).min(76);
        let height = 6.min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let text = self.path.text();
        let room = width.saturating_sub(4) as usize;
        let cursor: usize = text
            .chars()
            .take(self.path.cursor().1)
            .map(|c| c.width().unwrap_or(0))
            .sum();
        // Long paths scroll to keep the cursor in sight.
        let skip = cursor.saturating_sub(room.saturating_sub(1));
        let shown: String = text.chars().skip(skip).collect();
        let note = match &self.error {
            Some(why) => Line::styled(format!(" {why}"), theme.danger()),
            None if self.import.is_some() && !self.table_columns.is_empty() => Line::styled(
                " Next: which table column each of the file's goes into",
                theme.dim(),
            ),
            None if self.import.is_some() => Line::styled(
                " Columns match the table's by name; empty cells are NULL",
                theme.dim(),
            ),
            None if self.target().is_ok_and(|p| p.exists()) => {
                Line::styled(" The file exists and will be replaced", theme.warning())
            }
            None => Line::styled(" .csv, .tsv, .jsonl or .parquet", theme.dim()),
        };
        let lines = vec![
            Line::raw(format!(" {shown}")),
            note,
            Line::from(vec![
                Span::styled(" Enter", theme.key()),
                Span::styled(
                    if self.import.is_some() {
                        " import  "
                    } else {
                        " export  "
                    },
                    theme.dim(),
                ),
                Span::styled("Esc", theme.key()),
                Span::styled(" cancel", theme.dim()),
            ]),
        ];
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(
                match self.import.as_ref().and_then(NodePath::name) {
                    Some(table) => format!(" Import rows into {table} from "),
                    None => " Export rows to ".to_string(),
                },
                theme.title(true),
            ))
            .style(theme.surface());
        f.render_widget(Clear, popup);
        f.render_widget(Paragraph::new(lines).block(block), popup);
        f.set_cursor_position((popup.x + 2 + (cursor - skip) as u16, popup.y + 1));
    }
}

impl ExportDialog {
    /// The file's columns, one a line, each with the table column it goes
    /// into.
    fn render_mapping(&self, mapping: &Mapping, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let width = area.width.saturating_sub(4).min(76);
        // Header, the columns, the note and the keys, in the border.
        let shown = mapping
            .from
            .len()
            .min(area.height.saturating_sub(6) as usize)
            .max(1);
        let height = (shown as u16 + 5).min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let name_width = mapping
            .from
            .iter()
            .map(|n| n.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(8, 28);
        let first = mapping.row.saturating_sub(shown - 1);
        let mut lines = vec![Line::styled(
            format!("   {:<name_width$}  into", "file column"),
            theme.dim(),
        )];
        lines.extend(
            mapping
                .from
                .iter()
                .zip(&mapping.to)
                .enumerate()
                .skip(first)
                .take(shown)
                .map(|(i, (from, to))| {
                    let marker = if i == mapping.row { "▸" } else { " " };
                    let target = match to {
                        Some(c) => Span::raw(format!("‹ {} ›", self.table_columns[*c])),
                        None => Span::styled("‹ skip ›", theme.dim()),
                    };
                    Line::from(vec![
                        Span::styled(format!(" {marker} "), theme.key()),
                        Span::raw(format!("{:<name_width$}  ", truncate(from, name_width))),
                        target,
                    ])
                }),
        );
        lines.push(match &self.error {
            Some(why) => Line::styled(format!(" {why}"), theme.danger()),
            None => Line::styled(
                " Empty cells are NULL; skipped columns get their defaults",
                theme.dim(),
            ),
        });
        lines.push(Line::from(vec![
            Span::styled(" ↑↓", theme.key()),
            Span::styled(" column  ", theme.dim()),
            Span::styled("←→", theme.key()),
            Span::styled(" change  ", theme.dim()),
            Span::styled("Enter", theme.key()),
            Span::styled(" import  ", theme.dim()),
            Span::styled("Esc", theme.key()),
            Span::styled(" back", theme.dim()),
        ]));
        let table = self
            .import
            .as_ref()
            .and_then(NodePath::name)
            .unwrap_or("the table");
        let file = mapping
            .file
            .file_name()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(
                format!(" {file} into {table} "),
                theme.title(true),
            ))
            .style(theme.surface());
        f.render_widget(Clear, popup);
        f.render_widget(Paragraph::new(lines).block(block), popup);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(dialog: &mut ExportDialog, text: &str) {
        for _ in 0..dialog.path.text().chars().count() {
            dialog.handle(Action::Backspace);
        }
        for c in text.chars() {
            dialog.handle(Action::Input(c));
        }
    }

    #[test]
    fn the_extension_picks_the_format() {
        let mut dialog = ExportDialog::new("orders", "SELECT 1".into(), None);
        assert!(dialog.path.text().ends_with("orders.csv"));
        let folder = std::env::temp_dir();
        typed(&mut dialog, &folder.join("orders.xlsx").to_string_lossy());
        assert_eq!(dialog.handle(Action::Confirm), None);
        assert!(dialog
            .error
            .as_deref()
            .is_some_and(|e| e.contains(".parquet")));
        typed(
            &mut dialog,
            &folder.join("nowhere/at/all.csv").to_string_lossy(),
        );
        assert_eq!(dialog.handle(Action::Confirm), None);
        assert!(dialog
            .error
            .as_deref()
            .is_some_and(|e| e.contains("no folder")));
        let good = folder.join("orders.parquet");
        typed(&mut dialog, &good.to_string_lossy());
        assert_eq!(
            dialog.handle(Action::Confirm),
            Some(DialogEvent::Export(good))
        );
        assert_eq!(dialog.handle(Action::Back), Some(DialogEvent::Close));
    }

    #[test]
    fn an_import_needs_a_file_that_exists() {
        let table: NodePath = ["shop", "public", "orders"].into_iter().collect();
        let mut dialog = ExportDialog::import(table, Vec::new());
        let missing = std::env::temp_dir().join("dfox-no-such-file.csv");
        typed(&mut dialog, &missing.to_string_lossy());
        assert_eq!(dialog.handle(Action::Confirm), None);
        assert!(dialog
            .error
            .as_deref()
            .is_some_and(|e| e.contains("no file")));
        let file = std::env::temp_dir().join(format!("dfox-import-{}.csv", std::process::id()));
        std::fs::write(&file, "id\n1\n").expect("file");
        typed(&mut dialog, &file.to_string_lossy());
        assert_eq!(
            dialog.handle(Action::Confirm),
            Some(DialogEvent::Export(file.clone()))
        );
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn an_imports_columns_are_mapped_to_the_tables() {
        let table: NodePath = ["shop", "public", "orders"].into_iter().collect();
        let columns = vec!["id".to_string(), "total".to_string()];
        let mut dialog = ExportDialog::import(table, columns);
        let file = std::env::temp_dir().join(format!("dfox-map-{}.csv", std::process::id()));
        std::fs::write(&file, "ID,note,amount\n1,x,2.5\n").expect("file");
        typed(&mut dialog, &file.to_string_lossy());
        // The file's columns first: matched by name, the rest skipped.
        assert_eq!(dialog.handle(Action::Confirm), None);
        assert_eq!(dialog.columns(), Some(vec![Some("id".into()), None, None]));
        // amount into total: ← from skip goes round to the last column.
        dialog.handle(Action::Down);
        dialog.handle(Action::Down);
        dialog.handle(Action::Left);
        // note into id too: twice is refused.
        dialog.handle(Action::Up);
        dialog.handle(Action::Right);
        assert_eq!(dialog.handle(Action::Confirm), None);
        assert!(dialog.error.as_deref().is_some_and(|e| e.contains("twice")));
        dialog.handle(Action::Left);
        assert_eq!(
            dialog.columns(),
            Some(vec![Some("id".into()), None, Some("total".into())])
        );
        assert_eq!(
            dialog.handle(Action::Confirm),
            Some(DialogEvent::Export(file.clone()))
        );
        // Esc goes back to the file.
        dialog.handle(Action::Back);
        assert_eq!(dialog.columns(), None);
        let _ = std::fs::remove_file(file);
    }
}
