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
use crate::{action::Action, component::RenderCtx, connections::expand_home};

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
    path: Buffer,
    error: Option<String>,
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
            path,
            error: None,
        }
    }

    /// A dialog to import a file's rows into the table at `table`.
    pub fn import(table: NodePath) -> Self {
        let name = table
            .name()
            .unwrap_or("rows")
            .replace(['/', '\\', ':'], "_");
        Self {
            import: Some(table),
            ..Self::new(&name, String::new(), None)
        }
    }

    pub fn handle(&mut self, action: Action) -> Option<DialogEvent> {
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
        let mut dialog = ExportDialog::import(table);
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
}
