//! Query results: a grid that fills as rows stream in, or the query's status.

use std::{cell::Cell, ops::Range, time::Duration};

use arboard::Clipboard;
use dfox_core::{
    engine::{Command as DbCommand, QueryId, QueryStats},
    errors::DbError,
    result::{Cell as Value, ColumnMeta, OwnedCell, ResultSet, RowStore},
};
use ratatui::{
    layout::{Constraint, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Row, Table, Wrap},
    Frame,
};
use tokio::time::Instant;

use super::{inner, panel, scroll_to};
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, RenderCtx},
    widgets::{progress::ProgressBar, throbber::spinner, toast::Level, truncate},
};

/// Columns shown side by side; Left and Right scroll through the rest.
const VISIBLE_COLUMNS: usize = 8;
/// Longest cell text shown, in characters.
const MAX_CELL_CHARS: usize = 100;

#[derive(Debug)]
pub struct RunningQuery {
    pub id: QueryId,
    pub started: Instant,
    pub cancelling: bool,
}

#[derive(Debug, Default)]
pub struct ResultsPanel {
    pub store: RowStore,
    pub error: Option<String>,
    pub message: Option<String>,
    pub running: Option<RunningQuery>,
    selected: usize,
    scroll: usize,
    first_column: usize,
    next_id: QueryId,
    pub focused: bool,
    visible: Cell<usize>,
}

impl ResultsPanel {
    pub fn run(&mut self, sql: &str, ctx: &mut Ctx) {
        let sql = sql.trim();
        if sql.is_empty() {
            return;
        }
        if self.running.is_some() {
            ctx.toast(Level::Warning, "A query is already running");
            return;
        }
        self.next_id += 1;
        ctx.engine.send(DbCommand::Run {
            id: self.next_id,
            sql: sql.to_string(),
            row_limit: ctx.config.row_limit,
        });
        self.running = Some(RunningQuery {
            id: self.next_id,
            started: ctx.now,
            cancelling: false,
        });
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

    fn is_current(&self, id: QueryId) -> bool {
        self.running.as_ref().is_some_and(|q| q.id == id)
    }

    fn show(&mut self, store: RowStore) {
        self.store = store;
        self.error = None;
        self.message = None;
        self.selected = 0;
        self.scroll = 0;
        self.first_column = 0;
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

    /// Returns `true` when the query finished and may have changed tables.
    pub fn on_done(
        &mut self,
        id: QueryId,
        outcome: Result<QueryStats, DbError>,
        ctx: &mut Ctx,
    ) -> bool {
        if !self.is_current(id) {
            return false;
        }
        self.running = None;
        self.store.finish();
        match outcome {
            Ok(stats) => {
                self.message = Some(query_message(&stats, !self.store.columns().is_empty()));
                true
            }
            Err(DbError::Cancelled) => {
                self.message = Some("Query cancelled".to_string());
                ctx.toast(Level::Info, "Query cancelled");
                false
            }
            Err(err) => {
                self.show(RowStore::default());
                self.error = Some(err.to_string());
                false
            }
        }
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

    pub fn copy_row(&mut self, ctx: &mut Ctx) {
        let row = self.selected;
        self.copy(row..row + 1, format!("row {}", row + 1), ctx);
    }

    pub fn copy_all(&mut self, ctx: &mut Ctx) {
        let rows = self.store.row_count();
        self.copy(0..rows, format!("{rows} rows"), ctx);
    }

    fn copy(&mut self, rows: Range<usize>, what: String, ctx: &mut Ctx) {
        if self.store.row_count() == 0 {
            ctx.toast(Level::Warning, "No results to copy");
            return;
        }
        let text = results_as_tsv(&self.store, rows);
        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text)) {
            Ok(()) => ctx.toast(Level::Success, format!("Copied {what} to the clipboard")),
            Err(err) => ctx.toast(Level::Error, format!("Clipboard error: {err}")),
        }
    }

    fn select(&mut self, selected: usize) {
        self.selected = selected.min(self.store.row_count().saturating_sub(1));
        self.scroll = scroll_to(self.selected, self.scroll, self.visible.get());
    }

    fn elapsed(&self, now: Instant) -> Option<Duration> {
        self.running
            .as_ref()
            .map(|q| now.saturating_duration_since(q.started))
    }
}

impl Component for ResultsPanel {
    fn handle(&mut self, action: Action, _ctx: &mut Ctx) -> Option<Command> {
        let page = self.visible.get().max(1);
        let columns = self.store.columns().len();
        match action {
            Action::Up => self.select(self.selected.saturating_sub(1)),
            Action::Down => self.select(self.selected + 1),
            Action::PageUp => self.select(self.selected.saturating_sub(page)),
            Action::PageDown => self.select(self.selected + page),
            Action::Home => {
                self.select(0);
                self.first_column = 0;
            }
            Action::End => self.select(usize::MAX),
            Action::Left => self.first_column = self.first_column.saturating_sub(1),
            Action::Right => {
                self.first_column =
                    (self.first_column + 1).min(columns.saturating_sub(VISIBLE_COLUMNS));
            }
            _ => {}
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
        if self.store.row_count() > 0 {
            title.push(Span::styled(
                format!(
                    " {}/{}",
                    (self.selected + 1).min(self.store.row_count()),
                    self.store.row_count()
                ),
                theme.dim(),
            ));
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
        } else if self.store.row_count() > 0 {
            self.render_grid(f, area, block, ctx);
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
                        format!("{} cancels", ctx.keymap.hint(Action::CopyRow)),
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
        } else if let (Some(message), true) = (&self.message, self.store.row_count() > 0) {
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
    }

    fn is_animating(&self) -> bool {
        self.running.is_some()
    }
}

impl ResultsPanel {
    fn render_grid(
        &self,
        f: &mut Frame,
        area: Rect,
        block: ratatui::widgets::Block,
        ctx: &RenderCtx,
    ) {
        let theme = ctx.theme;
        let store = &self.store;
        let total_rows = store.row_count();
        let total_columns = store.columns().len();
        let first = self.first_column.min(total_columns.saturating_sub(1));
        let columns: Vec<usize> = (first..total_columns).take(VISIBLE_COLUMNS).collect();

        // The header takes two lines.
        let visible = (inner(area).height as usize).saturating_sub(2).max(1);
        self.visible.set(visible);
        let scroll = scroll_to(self.selected, self.scroll, visible).min(total_rows - 1);

        let number_width = total_rows.to_string().len().max(1) as u16;
        let mut widths = vec![Constraint::Length(number_width)];
        for &col in &columns {
            let header = store.columns()[col].name.chars().count();
            let content = (0..total_rows.min(50))
                .filter_map(|row| store.cell(row, col))
                .map(|cell| display_cell(cell).chars().count().min(40))
                .max()
                .unwrap_or(0);
            widths.push(Constraint::Length(header.max(content).clamp(4, 40) as u16));
        }

        let rows: Vec<Row> = (scroll..total_rows.min(scroll + visible))
            .map(|row| {
                let mut cells = vec![ratatui::widgets::Cell::from(
                    Line::styled((row + 1).to_string(), theme.dim()).right_aligned(),
                )];
                cells.extend(columns.iter().map(|&col| {
                    let value = store.cell(row, col).unwrap_or(Value::Null);
                    let line = Line::styled(display_cell(value), theme.cell(&value));
                    ratatui::widgets::Cell::from(if value.is_numeric() {
                        line.right_aligned()
                    } else {
                        line
                    })
                }));
                let row_widget = Row::new(cells);
                if row == self.selected {
                    row_widget.style(if self.focused {
                        theme.selection()
                    } else {
                        theme.base()
                    })
                } else {
                    row_widget
                }
            })
            .collect();

        let mut header = vec![Span::raw("#")];
        header.extend(
            columns
                .iter()
                .map(|&col| Span::raw(store.columns()[col].name.clone())),
        );
        let mut title_extra = Vec::new();
        if total_columns > VISIBLE_COLUMNS {
            title_extra.push(Span::styled(
                format!(
                    " cols {}-{}/{total_columns}",
                    first + 1,
                    first + columns.len()
                ),
                theme.dim(),
            ));
        }
        let block = block.title(Line::from(title_extra).right_aligned());

        let table = Table::new(rows, widths)
            .header(Row::new(header).style(theme.header()).bottom_margin(1))
            .column_spacing(2)
            .block(block);
        f.render_widget(table, area);
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

/// A value as one line of the grid: control characters removed, long values
/// cut short.
fn display_cell(value: Value<'_>) -> String {
    let text: String = value
        .to_string()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    truncate(text.trim(), MAX_CELL_CHARS)
}

/// The column names and `rows` of `result`, tab-separated, one line each.
fn results_as_tsv(result: &impl ResultSet, rows: Range<usize>) -> String {
    let columns = result.columns().len();
    let mut lines = vec![result
        .columns()
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join("\t")];
    lines.extend(rows.map(|row| {
        (0..columns)
            .map(|col| {
                result
                    .cell(row, col)
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("\t")
    }));
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
#[path = "results_tests.rs"]
mod tests;
