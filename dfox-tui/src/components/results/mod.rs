//! Query results: a grid that fills as rows stream in, or the query's status.

pub mod export;
pub mod grid;

use std::time::Duration;

use dfox_core::{
    engine::{Command as DbCommand, QueryId, QueryStats},
    errors::DbError,
    models::connections::DbType,
    result::{ColumnMeta, OwnedCell, ResultSet, RowStore},
};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use tokio::time::Instant;

use self::{
    export::{Format, Target},
    grid::Grid,
};
use super::{inner, panel};
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, RenderCtx},
    sql,
    widgets::{progress::ProgressBar, throbber::spinner, toast::Level},
};

#[derive(Debug)]
pub struct RunningQuery {
    pub id: QueryId,
    pub started: Instant,
    pub cancelling: bool,
}

/// A query that ended, for the history.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    pub sql: String,
    pub ok: bool,
    pub rows: Option<u64>,
    pub elapsed: Duration,
    /// The query may have created or dropped tables.
    pub changed: bool,
}

#[derive(Debug)]
pub struct ResultsPanel {
    pub store: RowStore,
    pub error: Option<String>,
    pub message: Option<String>,
    pub running: Option<RunningQuery>,
    pub grid: Grid,
    pub focused: bool,
    driver: DbType,
    /// The query the results came from, for INSERT statements.
    sql: String,
    /// The copy-as menu is open.
    picker: bool,
}

impl ResultsPanel {
    pub fn new(driver: DbType) -> Self {
        Self {
            store: RowStore::default(),
            error: None,
            message: None,
            running: None,
            grid: Grid::default(),
            focused: false,
            driver,
            sql: String::new(),
            picker: false,
        }
    }

    /// Starts `sql` as query `id`.
    pub fn run(&mut self, id: QueryId, sql: &str, ctx: &mut Ctx) {
        ctx.engine.send(DbCommand::Run {
            id,
            sql: sql.to_string(),
            row_limit: ctx.config.row_limit,
        });
        self.running = Some(RunningQuery {
            id,
            started: ctx.now,
            cancelling: false,
        });
        self.sql = sql.to_string();
        self.show(RowStore::default());
    }

    pub fn cancel(&mut self, ctx: &mut Ctx) {
        if let Some(query) = &mut self.running {
            if !query.cancelling {
                query.cancelling = true;
                ctx.engine.send(DbCommand::Cancel { id: query.id });
            }
        }
    }

    /// Cancels the running query and ignores the rest of its events.
    pub fn abandon(&mut self, ctx: &mut Ctx) {
        self.cancel(ctx);
        self.running = None;
    }

    pub fn is_current(&self, id: QueryId) -> bool {
        self.running.as_ref().is_some_and(|q| q.id == id)
    }

    fn show(&mut self, store: RowStore) {
        self.store = store;
        self.error = None;
        self.message = None;
        self.grid = Grid::default();
        self.picker = false;
    }

    pub fn on_columns(&mut self, id: QueryId, columns: Vec<ColumnMeta>) {
        if self.is_current(id) {
            self.show(RowStore::new(columns));
        }
    }

    pub fn on_rows(&mut self, id: QueryId, rows: Vec<Vec<OwnedCell>>) {
        if self.is_current(id) {
            self.store.extend(rows);
        }
    }

    /// Ends query `id`. Returns what happened, unless the query was
    /// abandoned.
    pub fn on_done(
        &mut self,
        id: QueryId,
        outcome: Result<QueryStats, DbError>,
        ctx: &mut Ctx,
    ) -> Option<Finished> {
        if !self.is_current(id) {
            return None;
        }
        let started = self.running.take().map(|q| q.started);
        self.store.finish();
        let mut finished = Finished {
            sql: self.sql.clone(),
            ok: false,
            rows: None,
            elapsed: started.map_or(Duration::ZERO, |s| ctx.now.saturating_duration_since(s)),
            changed: false,
        };
        match outcome {
            Ok(stats) => {
                let has_columns = !self.store.columns().is_empty();
                self.message = Some(query_message(&stats, has_columns));
                finished.ok = true;
                finished.changed = true;
                finished.elapsed = stats.elapsed;
                finished.rows = Some(if has_columns {
                    stats.rows
                } else {
                    stats.rows_affected
                });
            }
            Err(DbError::Cancelled) => {
                self.message = Some("Query cancelled".to_string());
                ctx.toast(Level::Info, "Query cancelled");
            }
            Err(err) => {
                self.show(RowStore::default());
                self.error = Some(err.to_string());
            }
        }
        Some(finished)
    }

    /// Shows the event log in the grid.
    pub fn show_log(&mut self, entries: &[String]) {
        let mut store = RowStore::new(vec![ColumnMeta::new("#", ""), ColumnMeta::new("Event", "")]);
        store.extend(entries.iter().enumerate().map(|(i, entry)| {
            vec![
                OwnedCell::Int(i as i64 + 1),
                OwnedCell::Text(entry.as_str().into()),
            ]
        }));
        store.finish();
        self.show(store);
        self.message = Some(format!("Event log ({} entries)", entries.len()));
    }

    /// True while the copy-as menu has the keys.
    pub fn is_modal(&self) -> bool {
        self.picker
    }

    /// True when Back has something to undo here: an open menu or a
    /// selection.
    pub fn takes_back(&self) -> bool {
        self.picker || self.grid.has_selection()
    }

    fn size(&self) -> (usize, usize) {
        (self.store.row_count(), self.store.columns().len())
    }

    /// Copies the selected cells, or the current one.
    pub fn copy(&mut self, ctx: &mut Ctx) {
        if self.store.row_count() == 0 {
            ctx.toast(Level::Warning, "No results to copy");
            return;
        }
        let region = self.grid.region(self.store.columns().len());
        let what = describe(region.rows.len(), region.cells());
        ctx.copy(export::plain(&self.store, &region), &what);
    }

    fn copy_as(&mut self, format: Format, name: &str, ctx: &mut Ctx) {
        let region = self.export_region();
        let table =
            sql::single_table(&self.sql, self.driver).unwrap_or_else(|| "table_name".into());
        let target = Target {
            table: &table,
            driver: self.driver,
        };
        let text = export::export(&self.store, &region, format, target);
        let rows = region.rows.len();
        ctx.copy(text, &format!("{rows} row(s) as {name}"));
    }

    /// What copy-as covers: the selection, or else every row.
    fn export_region(&self) -> export::Region {
        let columns = self.store.columns().len();
        if self.grid.has_selection() {
            self.grid.region(columns)
        } else {
            export::Region {
                rows: 0..self.store.row_count(),
                cols: 0..columns,
            }
        }
    }

    pub fn click(&mut self, column: u16, row: u16, drag: bool) -> bool {
        match self.grid.cell_at(column, row) {
            Some(cell) => {
                let size = self.size();
                self.grid.click(cell, drag, size);
                true
            }
            None => false,
        }
    }

    pub fn scroll_by(&mut self, rows: isize) {
        let size = self.size();
        self.grid.scroll_by(rows, size);
    }

    pub fn scroll_columns(&mut self, cols: isize) {
        let size = self.size();
        self.grid.scroll_columns(cols, size);
    }

    fn elapsed(&self, now: Instant) -> Option<Duration> {
        self.running
            .as_ref()
            .map(|q| now.saturating_duration_since(q.started))
    }
}

/// "the cell", "3 cells", "2 rows".
fn describe(rows: usize, cells: usize) -> String {
    match (rows, cells) {
        (_, 1) => "the cell".to_string(),
        (1, n) => format!("{n} cells"),
        (rows, _) => format!("{rows} rows"),
    }
}

impl Component for ResultsPanel {
    fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        if self.picker {
            match action {
                Action::Input(c) => {
                    if let Some((format, name)) = Format::for_key(c) {
                        self.picker = false;
                        self.copy_as(format, name, ctx);
                    }
                }
                Action::Back | Action::CopyAs => self.picker = false,
                _ => {}
            }
            return None;
        }
        match action {
            Action::Copy => self.copy(ctx),
            Action::CopyAs if self.store.row_count() == 0 => {
                ctx.toast(Level::Warning, "No results to copy")
            }
            Action::CopyAs => self.picker = true,
            action => {
                let size = self.size();
                self.grid.handle(action, size);
            }
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let mut title = vec![Span::raw("Results")];
        if let Some(elapsed) = self.elapsed(ctx.now) {
            title.push(Span::raw(" "));
            title.push(spinner(ctx, theme.key()));
            title.push(Span::styled(
                format!(" {}", format_duration(elapsed)),
                theme.dim(),
            ));
        }
        let rows = self.store.row_count();
        let columns = self.store.columns().len();
        if rows > 0 {
            let (row, col) = self.grid.cursor();
            title.push(Span::styled(
                format!(" {}/{rows}", (row + 1).min(rows)),
                theme.dim(),
            ));
            if columns > 1 {
                title.push(Span::styled(
                    format!(" · col {}/{columns}", (col + 1).min(columns)),
                    theme.dim(),
                ));
            }
            if self.grid.has_selection() {
                let region = self.grid.region(columns);
                title.push(Span::styled(
                    format!(" · {}×{} selected", region.rows.len(), region.cols.len()),
                    theme.key(),
                ));
            }
        }
        let block = panel(title, self.focused, ctx);

        if let Some(error) = &self.error {
            let text = Paragraph::new(Line::from(vec![
                Span::styled(format!("{} ", ctx.icons.error), theme.danger()),
                Span::styled(error.as_str(), theme.danger()),
            ]))
            .wrap(Wrap { trim: false })
            .block(block);
            f.render_widget(text, area);
        } else if rows > 0 {
            f.render_widget(block, area);
            self.grid
                .render(f.buffer_mut(), inner(area), &self.store, self.focused, ctx);
        } else {
            let status = match &self.running {
                Some(query) if query.cancelling => Line::from(vec![
                    spinner(ctx, theme.warning()),
                    Span::styled(" Cancelling…", theme.warning()),
                ]),
                Some(_) => Line::from(vec![
                    spinner(ctx, theme.key()),
                    Span::raw(" Running…  "),
                    Span::styled(
                        format!("{} cancels", ctx.keymap.hint(Action::Copy)),
                        theme.dim(),
                    ),
                ]),
                None => Line::styled(
                    self.message
                        .as_deref()
                        .unwrap_or("Run a query to see results here"),
                    theme.dim(),
                ),
            };
            f.render_widget(Paragraph::new(status).block(block), area);
        }

        // While a query runs, the bottom border sweeps.
        if self.running.is_some() && area.width > 4 && area.height > 1 {
            let mut bar = ProgressBar::new(theme, None, ctx.frame);
            bar.track = theme.border(self.focused).fg.unwrap_or(bar.track);
            f.render_widget(
                bar,
                Rect::new(area.x + 1, area.y + area.height - 1, area.width - 2, 1),
            );
        } else if let (Some(message), true) = (&self.message, rows > 0) {
            let message = format!(" {message} ");
            let width = (message.chars().count() as u16).min(area.width.saturating_sub(4));
            f.render_widget(
                Paragraph::new(Span::styled(message, theme.dim())),
                Rect::new(
                    area.x + area.width - width - 2,
                    area.y + area.height - 1,
                    width,
                    1,
                ),
            );
        }

        if self.picker {
            self.render_picker(f, area, ctx);
        }
    }

    fn is_animating(&self) -> bool {
        self.running.is_some()
    }

    fn wants_text(&self) -> bool {
        self.picker
    }
}

impl ResultsPanel {
    /// The copy-as menu, in the bottom-right corner.
    fn render_picker(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let region = self.export_region();
        let what = if self.grid.has_selection() {
            format!("{} selected row(s)", region.rows.len())
        } else {
            format!("all {} rows", region.rows.len())
        };
        // Inside the panel's border, in the bottom-right corner.
        let room = area.inner(ratatui::layout::Margin::new(1, 1));
        let width = 26.min(room.width);
        let height = (Format::ALL.len() as u16 + 3).min(room.height);
        let popup = Rect::new(room.right() - width, room.bottom() - height, width, height);
        let mut lines = vec![Line::styled(format!(" {what}"), theme.dim())];
        lines.extend(Format::ALL.iter().map(|(key, _, name)| {
            Line::from(vec![
                Span::styled(format!(" {key} "), theme.key()),
                Span::raw(*name),
            ])
        }));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(" Copy as ", theme.title(true)))
            .style(theme.surface());
        f.render_widget(Clear, popup);
        f.render_widget(Paragraph::new(lines).block(block), popup);
    }
}

/// The status shown after a query finishes.
pub fn query_message(stats: &QueryStats, has_columns: bool) -> String {
    let took = format_duration(stats.elapsed);
    if stats.truncated {
        format!("first {} rows · {took}", stats.rows)
    } else if has_columns {
        format!("{} row(s) · {took}", stats.rows)
    } else if stats.rows_affected > 0 {
        format!("{} row(s) affected · {took}", stats.rows_affected)
    } else {
        format!("done · {took}")
    }
}

pub fn format_duration(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{} ms", d.as_millis())
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

#[cfg(test)]
mod tests;
