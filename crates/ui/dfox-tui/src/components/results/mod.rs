//! Query results: a grid that fills as rows stream in, or the query's status.

mod edit;
pub mod export;
pub mod grid;
mod inspector;
mod json;

use std::{cell::Cell, sync::Arc, time::Duration};

use arrow_array::RecordBatch;
use arrow_schema::{Schema, SchemaRef};
use dfox_core::{
    arrow::ArrowResultSet,
    catalog::{BrowseQuery, NodePath},
    engine::{Command as DbCommand, QueryId, QueryStats},
    errors::DbError,
    result::{ColumnMeta, OwnedCell, ResultSet},
};
use ratatui::{
    layout::{Constraint, Flex, Layout, Position, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use tokio::time::Instant;

use self::{
    edit::Edits,
    export::{Format, Target},
    grid::Grid,
};
use super::editor::buffer::{Buffer, Motion};
use super::{inner, panel};
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, RenderCtx},
    sql::{self, Dialect},
    theme::ColorDepth,
    widgets::{fox::Fox, progress::ProgressBar, throbber::spinner, toast::Level},
};

/// How long a finished query's row count takes to roll up.
const COUNT_UP: Duration = Duration::from_millis(300);

/// How long the waiting fox takes to assemble, and to fly apart again.
const FOX_LOOP: Duration = Duration::from_millis(1600);

#[derive(Debug)]
pub struct RunningQuery {
    pub id: QueryId,
    pub started: Instant,
    pub cancelling: bool,
}

/// A query that ended, for the history.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    /// The query; `None` for a table's rows.
    pub sql: Option<String>,
    pub ok: bool,
    pub rows: Option<u64>,
    pub elapsed: Duration,
    /// The query may have created or dropped tables.
    pub changed: bool,
}

/// A result with no columns and no rows.
fn empty() -> ArrowResultSet {
    let mut result = ArrowResultSet::new(Arc::new(Schema::empty()));
    result.finish();
    result
}

#[derive(Debug)]
pub struct ResultsPanel {
    pub store: ArrowResultSet,
    pub error: Option<String>,
    pub message: Option<String>,
    pub running: Option<RunningQuery>,
    /// When the last query's rows were counted, to roll the count up.
    counted: Option<(Instant, QueryStats)>,
    pub grid: Grid,
    pub focused: bool,
    dialect: Dialect,
    /// The query the results came from.
    sql: String,
    /// The table the rows came from, as SQL, when it was browsed: for
    /// INSERT statements.
    table: Option<String>,
    /// The copy-as menu is open.
    picker: bool,
    /// The record inspector is open.
    inspecting: bool,
    inspect_scroll: Cell<usize>,
    /// Where the inspector was drawn, for the mouse wheel.
    inspector_area: Cell<Rect>,
    /// Rows show as JSON documents rather than in the grid.
    json: Option<json::JsonView>,
    /// What the rows are, beside the title: a Redis key's type, size and
    /// expiry.
    pub note: Option<String>,
    /// Switch to the JSON view when the result is one JSON value, as a
    /// Redis string of JSON is.
    pub json_if_json: bool,
    /// Lines the JSON view showed, for paging.
    json_height: Cell<usize>,
    /// Changes to the rows, while the grid edits its table.
    pub edits: Option<Edits>,
    /// The cell being changed, while its editor is open.
    cell_edit: Option<CellEdit>,
    /// The edits are listed for a last look before they are saved.
    reviewing: bool,
}

/// A cell being changed: where it is, and what is typed.
#[derive(Debug)]
struct CellEdit {
    row: usize,
    col: usize,
    text: Buffer,
    /// NULL rather than the text.
    null: bool,
}

impl ResultsPanel {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            store: empty(),
            error: None,
            message: None,
            running: None,
            counted: None,
            grid: Grid::default(),
            focused: false,
            dialect,
            sql: String::new(),
            table: None,
            picker: false,
            inspecting: false,
            inspect_scroll: Cell::new(0),
            inspector_area: Cell::new(Rect::default()),
            json: None,
            note: None,
            json_if_json: false,
            json_height: Cell::new(0),
            edits: None,
            cell_edit: None,
            reviewing: false,
        }
    }

    /// Starts editing the rows, which are of the table at `table` keyed by
    /// the columns `key`. Fails when the rows don't hold the key.
    pub fn start_editing(&mut self, table: NodePath, key: &[String]) -> Result<(), String> {
        if self.edits.is_none() {
            self.edits = Some(Edits::new(table, key, &self.store)?);
        }
        Ok(())
    }

    /// How many changes wait to be saved.
    pub fn pending(&self) -> usize {
        self.edits.as_ref().map_or(0, Edits::count)
    }

    /// The query the rows came from.
    pub fn query(&self) -> &str {
        &self.sql
    }

    /// Keys while editing rows. `None` when `action` isn't one of them.
    fn handle_edit(&mut self, action: Action, ctx: &mut Ctx) -> Option<Option<Command>> {
        if let Some(cell) = &mut self.cell_edit {
            match action {
                Action::Input(c) => {
                    cell.text.insert_char(c);
                    cell.null = false;
                }
                Action::Backspace => cell.text.backspace(),
                Action::DeleteWord => cell.text.delete_word(),
                Action::Left => cell.text.move_cursor(Motion::Left, false),
                Action::Right => cell.text.move_cursor(Motion::Right, false),
                Action::WordLeft => cell.text.move_cursor(Motion::WordLeft, false),
                Action::WordRight => cell.text.move_cursor(Motion::WordRight, false),
                Action::Home => cell.text.move_cursor(Motion::Home, false),
                Action::End => cell.text.move_cursor(Motion::End, false),
                // Delete makes it NULL.
                Action::Delete => {
                    cell.text.set_text("");
                    cell.null = true;
                }
                Action::Confirm => {
                    let value = (!cell.null).then(|| cell.text.text());
                    let (row, col) = (cell.row, cell.col);
                    self.cell_edit = None;
                    if let Some(edits) = &mut self.edits {
                        edits.set(&self.store, row, col, value);
                    }
                }
                Action::Back => self.cell_edit = None,
                _ => {}
            }
            return Some(None);
        }
        if self.reviewing {
            match action {
                Action::Confirm => {
                    self.reviewing = false;
                    let edits = self.edits.as_mut()?;
                    edits.saving = true;
                    return Some(Some(Command::ApplyChanges(edits.change_set(&self.store))));
                }
                Action::Input('x') => {
                    self.reviewing = false;
                    let count = self.pending();
                    self.edits = None;
                    ctx.toast(Level::Info, format!("Discarded {count} change(s)"));
                }
                Action::Back | Action::Save => self.reviewing = false,
                _ => {}
            }
            return Some(None);
        }
        // The JSON view has no cells to edit.
        if self.json.is_some() {
            return None;
        }
        let edits = self.edits.as_mut()?;
        let (row, col) = self.grid.cursor();
        match action {
            _ if edits.saving => ctx.toast(Level::Info, "Saving…"),
            Action::AddRow => {
                edits.add_row();
                let size = (
                    self.store.row_count() + edits.added,
                    self.store.columns().len(),
                );
                self.grid.handle(Action::End, size);
            }
            Action::Edit if self.store.row_count() + edits.added == 0 => {}
            Action::Edit if edits.marks.deleted.contains(&row) => {
                ctx.toast(Level::Warning, "The row is marked to delete")
            }
            Action::Edit if edits.is_key(row, col) => ctx.toast(
                Level::Warning,
                "The key finds the row: it can't be changed here",
            ),
            Action::Edit => {
                let current = match edits.marks.cells.get(&(row, col)) {
                    Some(value) => value.clone(),
                    None => match self.store.cell(row, col) {
                        None | Some(dfox_core::result::Cell::Null) => None,
                        Some(cell) => Some(cell.to_string()),
                    },
                };
                let mut text = Buffer::default();
                text.set_text(current.as_deref().unwrap_or_default());
                text.move_cursor(Motion::End, false);
                self.cell_edit = Some(CellEdit {
                    row,
                    col,
                    text,
                    null: current.is_none(),
                });
            }
            Action::Delete if self.store.row_count() + edits.added > 0 => edits.toggle_delete(row),
            Action::Undo => {
                if !edits.undo() {
                    ctx.toast(Level::Info, "Nothing to undo");
                }
            }
            Action::Save if edits.count() == 0 => ctx.toast(Level::Info, "No changes to save"),
            Action::Save => self.reviewing = true,
            _ => return None,
        }
        Some(None)
    }

    /// Starts `sql` as query `id`.
    pub fn run(&mut self, id: QueryId, sql: &str, ctx: &mut Ctx) {
        ctx.engine.send(DbCommand::Run {
            id,
            sql: sql.to_string(),
            row_limit: ctx.config.row_limit,
        });
        self.start(id, ctx);
        self.sql = sql.to_string();
    }

    /// Starts showing the rows of the table at `path` that `query` picks,
    /// as query `id`; `table` is its name as SQL.
    pub fn browse(
        &mut self,
        id: QueryId,
        path: NodePath,
        query: BrowseQuery,
        table: String,
        ctx: &mut Ctx,
    ) {
        ctx.engine.send(DbCommand::Browse { id, path, query });
        self.start(id, ctx);
        self.table = Some(table);
    }

    fn start(&mut self, id: QueryId, ctx: &Ctx) {
        self.running = Some(RunningQuery {
            id,
            started: ctx.now,
            cancelling: false,
        });
        self.counted = None;
        self.sql.clear();
        self.table = None;
        self.note = None;
        self.edits = None;
        self.cell_edit = None;
        self.reviewing = false;
        self.json_if_json = false;
        self.show(empty());
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

    pub(crate) fn show(&mut self, store: ArrowResultSet) {
        self.store = store;
        self.error = None;
        self.message = None;
        self.grid = Grid::default();
        // New rows, the same view.
        if self.json.is_some() {
            self.json = Some(json::JsonView::default());
        }
        self.picker = false;
        self.inspect_scroll.set(0);
    }

    /// A result set of query `id` starts, replacing any earlier one.
    pub fn on_schema(&mut self, id: QueryId, schema: SchemaRef) {
        if self.is_current(id) {
            self.show(ArrowResultSet::new(schema));
        }
    }

    pub fn on_batch(&mut self, id: QueryId, batch: RecordBatch) {
        if self.is_current(id) {
            self.store.push(batch);
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
        let browsed = self.table.is_some();
        let mut finished = Finished {
            sql: (!browsed).then(|| self.sql.clone()),
            ok: false,
            rows: None,
            elapsed: started.map_or(Duration::ZERO, |s| ctx.now.saturating_duration_since(s)),
            changed: false,
        };
        match outcome {
            Ok(stats) => {
                if self.json_if_json && self.json.is_none() && json::holds_json(&self.store) {
                    self.json = Some(json::JsonView::default());
                }
                let has_columns = !self.store.columns().is_empty();
                self.message = Some(query_message(&stats, has_columns));
                // A few rows show as they are; many roll up.
                if has_columns && stats.rows >= 100 {
                    self.counted = Some((ctx.now, stats));
                }
                finished.ok = true;
                finished.changed = !browsed;
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
                self.show(empty());
                self.error = Some(err.to_string());
            }
        }
        Some(finished)
    }

    /// Redraws at an animation's pace are wanted: the fox runs while a query
    /// waits for its first rows, and the row count rolls up after.
    pub fn smooth(&self, now: Instant) -> bool {
        (self.running.is_some() && self.size().0 == 0)
            || self
                .counted
                .as_ref()
                .is_some_and(|(at, _)| now.saturating_duration_since(*at) < COUNT_UP)
    }

    /// The message under the rows: while the count rolls up, a share of it.
    fn shown_message(&self, ctx: &RenderCtx) -> Option<String> {
        if let (Some((at, stats)), true) = (&self.counted, ctx.animations) {
            let gone = ctx.now.saturating_duration_since(*at);
            if gone < COUNT_UP {
                let t = gone.as_secs_f64() / COUNT_UP.as_secs_f64();
                let eased = 1. - (1. - t).powi(3);
                let mut stats = *stats;
                stats.rows = (stats.rows as f64 * eased).round() as u64;
                return Some(query_message(&stats, true));
            }
        }
        self.message.clone()
    }

    /// Shows the event log in the grid.
    pub fn show_log(&mut self, entries: &[String]) {
        let rows = entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                vec![
                    OwnedCell::Int(i as i64 + 1),
                    OwnedCell::Text(entry.as_str().into()),
                ]
            })
            .collect();
        let columns = vec![ColumnMeta::new("#", ""), ColumnMeta::new("Event", "")];
        self.show(ArrowResultSet::from_cells(columns, rows));
        self.message = Some(format!("Event log ({} entries)", entries.len()));
    }

    /// True when the rows show as JSON documents.
    pub fn shows_json(&self) -> bool {
        self.json.is_some()
    }

    /// True while the copy-as menu has the keys.
    pub fn is_modal(&self) -> bool {
        self.picker || self.cell_edit.is_some() || self.reviewing
    }

    /// True when Back has something to undo here: an open menu or a
    /// selection.
    pub fn takes_back(&self) -> bool {
        self.picker
            || self.inspecting
            || self.grid.has_selection()
            || self.cell_edit.is_some()
            || self.reviewing
    }

    /// Scrolls the inspector when the point is on it. Returns false
    /// otherwise.
    pub fn scroll_inspector(&mut self, column: u16, row: u16, lines: isize) -> bool {
        if !self.inspecting
            || !self
                .inspector_area
                .get()
                .contains(Position::new(column, row))
        {
            return false;
        }
        let scroll = self.inspect_scroll.get().saturating_add_signed(lines);
        self.inspect_scroll.set(scroll);
        true
    }

    /// Rows, new ones included, and columns.
    fn size(&self) -> (usize, usize) {
        let added = self.edits.as_ref().map_or(0, |e| e.added);
        (self.store.row_count() + added, self.store.columns().len())
    }

    /// Copies the selected cells, or the current one.
    pub fn copy(&mut self, ctx: &mut Ctx) {
        if self.store.row_count() == 0 {
            ctx.toast(Level::Warning, "No results to copy");
            return;
        }
        if let Some(text) = self
            .json
            .as_ref()
            .and_then(|view| view.selected(&self.store))
        {
            ctx.copy(text, "the JSON");
            return;
        }
        let region = self.grid.region(self.store.columns().len());
        let what = describe(region.rows.len(), region.cells());
        ctx.copy(export::plain(&self.store, &region), &what);
    }

    fn copy_as(&mut self, format: Format, name: &str, ctx: &mut Ctx) {
        let region = self.export_region();
        let table = self
            .table
            .clone()
            .or_else(|| sql::single_table(&self.sql, self.dialect))
            .unwrap_or_else(|| "table_name".into());
        let target = Target {
            table: &table,
            dialect: self.dialect,
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
        if let Some(command) = self.handle_edit(action, ctx) {
            return command;
        }
        if action == Action::JsonView {
            self.json = match self.json {
                Some(_) => None,
                None => Some(json::JsonView::default()),
            };
            self.inspecting = false;
            return None;
        }
        if let Some(view) = &mut self.json {
            match action {
                Action::Copy => self.copy(ctx),
                Action::CopyAs if self.store.row_count() > 0 => self.picker = true,
                action => {
                    view.handle(action, &self.store, self.json_height.get());
                }
            }
            return None;
        }
        match action {
            Action::Inspect | Action::Confirm if self.store.row_count() > 0 => {
                self.inspecting = !self.inspecting;
                self.inspect_scroll.set(0);
            }
            Action::Back if self.inspecting && !self.grid.has_selection() => {
                self.inspecting = false
            }
            Action::Copy => self.copy(ctx),
            Action::CopyAs if self.store.row_count() == 0 => {
                ctx.toast(Level::Warning, "No results to copy")
            }
            Action::CopyAs => self.picker = true,
            action => {
                let size = self.size();
                let row = self.grid.cursor().0;
                self.grid.handle(action, size);
                if self.grid.cursor().0 != row {
                    self.inspect_scroll.set(0);
                }
            }
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let mut title = vec![Span::raw("Results")];
        if let Some(note) = &self.note {
            title.push(Span::styled(format!(" · {note}"), theme.dim()));
        }
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
        if rows > 0 && self.json.is_some() {
            title.push(Span::styled(format!(" {rows} rows"), theme.dim()));
            title.push(Span::styled(" · JSON", theme.key()));
        } else if rows > 0 {
            let (row, col) = self.grid.cursor();
            // New rows count too.
            let total = self.size().0;
            title.push(Span::styled(
                format!(" {}/{total}", (row + 1).min(total)),
                theme.dim(),
            ));
            if columns > 1 {
                title.push(Span::styled(
                    format!(" · col {}/{columns}", (col + 1).min(columns)),
                    theme.dim(),
                ));
            }
            if let Some(edits) = self.edits.as_ref().filter(|e| e.count() > 0) {
                title.push(Span::styled(
                    format!(" · Δ {} unsaved", edits.count()),
                    theme.warning(),
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
        } else if let (Some(view), true) = (&self.json, rows > 0) {
            f.render_widget(block, area);
            let room = inner(area);
            self.json_height.set(room.height as usize);
            view.render(f.buffer_mut(), room, &self.store, self.focused, ctx);
        } else if self.size().0 > 0 {
            f.render_widget(block, area);
            let marks = self
                .edits
                .as_ref()
                .map(|e| e.marks.clone())
                .unwrap_or_default();
            // New rows show after the result's own, with what is set in them.
            let shown = grid::WithNew {
                result: &self.store,
                added: self.edits.as_ref().map_or(0, |e| e.added),
            };
            let mut grid_area = inner(area);
            if self.inspecting && rows > 0 {
                // Beside the grid when there is room, over it otherwise.
                let room = area.inner(ratatui::layout::Margin::new(1, 1));
                let wide = room.width >= 80;
                let width = if wide { room.width * 2 / 5 } else { room.width };
                let panel = Rect {
                    x: room.right() - width,
                    width,
                    ..room
                };
                if wide {
                    grid_area.width = grid_area.width.saturating_sub(width + 1);
                }
                self.grid.render_marked(
                    f.buffer_mut(),
                    grid_area,
                    &shown,
                    self.focused,
                    ctx,
                    &marks,
                );
                let row = self.grid.cursor().0.min(rows - 1);
                let used =
                    inspector::render(f, panel, &self.store, row, self.inspect_scroll.get(), ctx);
                self.inspect_scroll.set(used);
                self.inspector_area.set(panel);
            } else {
                self.grid.render_marked(
                    f.buffer_mut(),
                    grid_area,
                    &shown,
                    self.focused,
                    ctx,
                    &marks,
                );
            }
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
            let room = inner(area);
            // Where there is room, the fox assembles and flies apart while
            // the first rows are on their way.
            let fox_rows = match room.height {
                _ if !ctx.animations || ctx.icons.ascii => 0,
                h if self.running.is_none() || h < 10 => 0,
                14.. => 8,
                _ => 6,
            };
            if fox_rows > 0 {
                f.render_widget(block, area);
                let [fox_area, _, line] = Layout::vertical([
                    Constraint::Length(fox_rows),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .flex(Flex::Center)
                .areas(room);
                let [fox_area] = Layout::horizontal([Constraint::Length(Fox::width(fox_rows))])
                    .flex(Flex::Center)
                    .areas(fox_area);
                let waited = self.running.as_ref().map_or(Duration::ZERO, |q| {
                    ctx.now.saturating_duration_since(q.started)
                });
                let fox = Fox {
                    progress: fox_progress(waited),
                    depth: ColorDepth::of(theme.bg),
                };
                f.render_widget(fox, fox_area);
                f.render_widget(Paragraph::new(status).centered(), line);
            } else {
                f.render_widget(Paragraph::new(status).block(block), area);
            }
        }

        // While a query runs, the bottom border sweeps; amber once it is
        // being cancelled.
        if let (Some(query), true) = (&self.running, area.width > 4 && area.height > 1) {
            let mut bar = ProgressBar::new(theme, None, ctx.frame);
            bar.track = theme.border(self.focused).fg.unwrap_or(bar.track);
            if query.cancelling {
                (bar.from, bar.to) = (theme.warning, theme.warning);
            }
            f.render_widget(
                bar,
                Rect::new(area.x + 1, area.y + area.height - 1, area.width - 2, 1),
            );
        } else if let (Some(message), true) = (self.shown_message(ctx), rows > 0) {
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
        if let Some(cell) = &self.cell_edit {
            self.render_cell_edit(cell, f, area, ctx);
        }
        if self.reviewing {
            self.render_review(f, area, ctx);
        }
    }

    fn is_animating(&self) -> bool {
        self.running.is_some()
    }

    fn wants_text(&self) -> bool {
        self.picker || self.cell_edit.is_some() || self.reviewing
    }
}

impl ResultsPanel {
    /// The cell editor, over the bottom of the panel.
    fn render_cell_edit(&self, cell: &CellEdit, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let room = area.inner(ratatui::layout::Margin::new(1, 1));
        let height = 4.min(room.height);
        let popup = Rect::new(room.x, room.bottom() - height, room.width, height);
        let column = self
            .store
            .columns()
            .get(cell.col)
            .map_or("", |c| c.name.as_str());
        let text = cell.text.text();
        let value = if cell.null {
            Span::styled(ctx.icons.null, theme.cell(&dfox_core::result::Cell::Null))
        } else {
            Span::raw(text.as_str())
        };
        let lines = vec![
            Line::from(vec![Span::raw(" "), value]),
            Line::from(vec![
                Span::styled(" Enter", theme.key()),
                Span::styled(" keep  ", theme.dim()),
                Span::styled("Esc", theme.key()),
                Span::styled(" cancel  ", theme.dim()),
                Span::styled("Del", theme.key()),
                Span::styled(" NULL", theme.dim()),
            ]),
        ];
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(
                format!(" {column}, row {} ", cell.row + 1),
                theme.title(true),
            ))
            .style(theme.surface());
        f.render_widget(Clear, popup);
        f.render_widget(Paragraph::new(lines).block(block), popup);
        if !cell.null {
            let typed: usize = text
                .chars()
                .take(cell.text.cursor().1)
                .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
                .sum();
            let x = popup.x + 2 + (typed as u16).min(popup.width.saturating_sub(4));
            f.set_cursor_position((x, popup.y + 1));
        }
    }

    /// The edits listed for a last look before saving.
    fn render_review(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let Some(edits) = &self.edits else {
            return;
        };
        let theme = ctx.theme;
        let room = area.inner(ratatui::layout::Margin::new(1, 1));
        let review = edits.review(&self.store);
        let shown = review.len().min(room.height.saturating_sub(4) as usize);
        let height = (shown as u16 + 4).min(room.height);
        let width = room.width.min(72);
        let popup = Rect::new(
            room.x + (room.width - width) / 2,
            room.y + (room.height - height) / 2,
            width,
            height,
        );
        let mut lines: Vec<Line> = review
            .iter()
            .take(shown)
            .map(|line| Line::raw(format!(" {line}")))
            .collect();
        if review.len() > shown {
            lines.pop();
            lines.push(Line::styled(
                format!(" … and {} more", review.len() - shown + 1),
                theme.dim(),
            ));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled(" Enter", theme.key()),
            Span::styled(" save  ", theme.dim()),
            Span::styled("x", theme.key()),
            Span::styled(" discard all  ", theme.dim()),
            Span::styled("Esc", theme.key()),
            Span::styled(" back", theme.dim()),
        ]));
        let name = edits.table.name().unwrap_or_default();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(
                format!(" Save {} change(s) to {name}? ", edits.count()),
                theme.title(true),
            ))
            .style(theme.surface());
        f.render_widget(Clear, popup);
        f.render_widget(Paragraph::new(lines).block(block), popup);
    }

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

/// Where the waiting fox is `waited` into its loop: it assembles, holds,
/// flies apart to the left and holds empty.
fn fox_progress(waited: Duration) -> f32 {
    let t = (waited.as_millis() % FOX_LOOP.as_millis()) as f32 / FOX_LOOP.as_millis() as f32;
    match t {
        t if t < 0.4 => t / 0.4,
        t if t < 0.65 => 1.,
        t if t < 0.9 => 1. - (t - 0.65) / 0.25,
        _ => 0.,
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
