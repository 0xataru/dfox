//! Where to copy a table's rows: a saved connection, a table there, and
//! whether to create it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dfox_core::{catalog::NodePath, models::connections::DbType};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use super::manager::driver_name;
use crate::{component::RenderCtx, connections::Connection};

/// What the dialog was closed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyEvent {
    Close,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyDialog {
    /// The query whose rows are copied, and the table they come from.
    pub sql: String,
    pub source: String,
    pub estimate: Option<u64>,
    /// Saved connections that can take rows, by name.
    targets: Vec<(String, DbType)>,
    target: usize,
    pub table: String,
    pub create: bool,
    /// The field being changed: connection, table, create.
    field: usize,
}

/// Databases whose sessions can add rows.
pub fn takes_rows(driver: DbType) -> bool {
    !matches!(driver, DbType::Cassandra | DbType::Redis)
}

/// The path, in `connection`'s catalog, of the table named `name` there:
/// `schema.table` in Postgres, `public` otherwise.
pub fn target_path(connection: &Connection, name: &str) -> NodePath {
    let database = connection.target().database.unwrap_or_default();
    match connection.driver {
        DbType::Postgres => match name.split_once('.') {
            Some((schema, table)) => [database.as_str(), schema, table].into_iter().collect(),
            None => [database.as_str(), "public", name].into_iter().collect(),
        },
        DbType::Sqlite => ["main", name].into_iter().collect(),
        _ => [database.as_str(), name].into_iter().collect(),
    }
}

impl CopyDialog {
    /// A dialog to copy `sql`'s rows, from the table `source`, into one of
    /// `connections`. `None` when none can take rows.
    pub fn new(
        sql: String,
        source: &str,
        estimate: Option<u64>,
        connections: &[Connection],
    ) -> Option<Self> {
        let targets: Vec<(String, DbType)> = connections
            .iter()
            .filter(|c| takes_rows(c.driver))
            .map(|c| (c.name.clone(), c.driver))
            .collect();
        if targets.is_empty() {
            return None;
        }
        Some(Self {
            sql,
            source: source.to_string(),
            estimate,
            targets,
            target: 0,
            table: source.to_string(),
            create: true,
            field: 0,
        })
    }

    /// The connection chosen, by name.
    pub fn target(&self) -> &str {
        &self.targets[self.target].0
    }

    pub fn handle(&mut self, key: KeyEvent) -> Option<CopyEvent> {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Some(CopyEvent::Close),
            KeyCode::Enter if !self.table.trim().is_empty() => return Some(CopyEvent::Copy),
            KeyCode::Up | KeyCode::BackTab => self.field = (self.field + 2) % 3,
            KeyCode::Down | KeyCode::Tab => self.field = (self.field + 1) % 3,
            KeyCode::Left | KeyCode::Right if self.field == 0 => {
                let n = self.targets.len();
                self.target = if key.code == KeyCode::Left {
                    (self.target + n - 1) % n
                } else {
                    (self.target + 1) % n
                };
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if self.field == 2 => {
                self.create = !self.create
            }
            KeyCode::Backspace if self.field == 1 => {
                self.table.pop();
            }
            KeyCode::Char(c) if self.field == 1 && !control => self.table.push(c),
            _ => {}
        }
        None
    }

    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let width = area.width.saturating_sub(4).min(60);
        let height = 8.min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let (name, driver) = &self.targets[self.target];
        let row = |i: usize, label: &str, value: Vec<Span<'static>>| {
            let marker = if i == self.field { "▸" } else { " " };
            let mut spans = vec![
                Span::styled(format!(" {marker} "), theme.key()),
                Span::styled(format!("{label:<11}"), theme.dim()),
            ];
            spans.extend(value);
            Line::from(spans)
        };
        let lines = vec![
            row(
                0,
                "Connection",
                vec![
                    Span::raw(format!("‹ {name} ›")),
                    Span::styled(format!("  {}", driver_name(*driver)), theme.dim()),
                ],
            ),
            row(1, "Table", vec![Span::raw(self.table.clone())]),
            row(
                2,
                "Create it",
                vec![Span::raw(if self.create {
                    "‹ yes ›"
                } else {
                    "‹ no ›"
                })],
            ),
            Line::raw(""),
            Line::from(vec![
                Span::styled(" Enter", theme.key()),
                Span::styled(" copy  ", theme.dim()),
                Span::styled("←→", theme.key()),
                Span::styled(" change  ", theme.dim()),
                Span::styled("Esc", theme.key()),
                Span::styled(" cancel", theme.dim()),
            ]),
        ];
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(
                format!(" Copy the rows of {} to ", self.source),
                theme.title(true),
            ))
            .style(theme.surface());
        f.render_widget(Clear, popup);
        f.render_widget(Paragraph::new(lines).block(block), popup);
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyEventKind;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    #[test]
    fn targets_are_connections_that_take_rows() {
        let connections = [
            Connection::new("cache", DbType::Redis),
            Connection::new("pg", DbType::Postgres),
            Connection::new("my", DbType::MySql),
        ];
        let mut dialog =
            CopyDialog::new("SELECT 1".into(), "orders", None, &connections).expect("a target");
        assert_eq!(dialog.target(), "pg");
        dialog.handle(key(KeyCode::Right));
        assert_eq!(dialog.target(), "my");
        // The table: renamed; then don't create it.
        dialog.handle(key(KeyCode::Down));
        dialog.handle(key(KeyCode::Char('2')));
        dialog.handle(key(KeyCode::Down));
        dialog.handle(key(KeyCode::Char(' ')));
        assert_eq!((dialog.table.as_str(), dialog.create), ("orders2", false));
        assert_eq!(dialog.handle(key(KeyCode::Enter)), Some(CopyEvent::Copy));
        assert!(CopyDialog::new("x".into(), "t", None, &connections[..1]).is_none());
    }

    #[test]
    fn tables_are_found_where_each_database_keeps_them() {
        let mut pg = Connection::new("pg", DbType::Postgres);
        pg.database = Some("shop".into());
        assert_eq!(
            target_path(&pg, "orders").parts(),
            ["shop", "public", "orders"]
        );
        assert_eq!(
            target_path(&pg, "audit.log").parts(),
            ["shop", "audit", "log"]
        );
        let lite = Connection::new("lite", DbType::Sqlite);
        assert_eq!(target_path(&lite, "t").parts(), ["main", "t"]);
        let mut my = Connection::new("my", DbType::MySql);
        my.database = Some("app".into());
        assert_eq!(target_path(&my, "t").parts(), ["app", "t"]);
    }
}
