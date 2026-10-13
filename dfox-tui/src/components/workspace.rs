//! The main screen once a database is open: the explorer, and query tabs of
//! an editor with its results. Tab moves focus between the panels.

use std::cell::Cell;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use dfox_core::{
    engine::{Command as DbCommand, EngineEvent, QueryId},
    errors::DbError,
    models::{connections::DbType, schema::DbObject},
};
use ratatui::{
    layout::{Constraint, Layout, Position, Rect},
    Frame,
};

use super::{
    editor::{EditorPanel, TabLabel},
    explorer::Explorer,
    results::ResultsPanel,
};
use crate::{
    action::Action,
    catalog::Catalog,
    command::Command,
    component::{Component, Ctx, Focus, RenderCtx},
    history::Entry,
    sql::guard,
    widgets::toast::Level,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Explorer,
    Editor,
    Results,
}

const ORDER: &[Panel] = &[Panel::Explorer, Panel::Editor, Panel::Results];

/// Lines the mouse wheel scrolls.
const WHEEL: isize = 3;

/// A query tab: its text and its last results.
#[derive(Debug)]
pub struct Tab {
    pub editor: EditorPanel,
    pub results: ResultsPanel,
    number: usize,
}

#[derive(Debug)]
pub struct Workspace {
    focus: Focus<Panel>,
    pub explorer: Explorer,
    tabs: Vec<Tab>,
    active: usize,
    driver: DbType,
    connection: String,
    next_query: QueryId,
    next_tab: usize,
    /// The history file couldn't be written; said once.
    history_failed: bool,
    /// The connection is read-only: scripts that write aren't sent.
    read_only: bool,
    /// Tables and columns, for autocomplete.
    pub catalog: Catalog,
    /// Where the explorer, editor and results were drawn, for the mouse.
    areas: Cell<[Rect; 3]>,
}

impl Workspace {
    /// A workspace on `database`, the one the session is connected to.
    pub fn new(connection: &str, driver: DbType, database: &str) -> Self {
        let mut workspace = Self {
            focus: Focus::new(ORDER),
            explorer: Explorer::new(database),
            tabs: Vec::new(),
            active: 0,
            driver,
            connection: connection.to_string(),
            next_query: 0,
            next_tab: 0,
            history_failed: false,
            read_only: false,
            catalog: Catalog::default(),
            areas: Cell::new([Rect::default(); 3]),
        };
        workspace.new_tab();
        workspace.set_focus(Panel::Editor);
        workspace
    }

    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Records a schema's tables, and asks for their columns.
    pub fn on_objects(
        &mut self,
        schema: &str,
        result: Result<Vec<DbObject>, DbError>,
        ctx: &mut Ctx,
    ) {
        if let Ok(objects) = &result {
            self.catalog.on_objects(schema, objects);
            ctx.engine.send(DbCommand::ListColumns {
                schema: schema.to_string(),
            });
        }
        self.explorer.on_objects(schema, result);
    }

    pub fn focused(&self) -> Panel {
        self.focus.get()
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn editor(&self) -> &EditorPanel {
        &self.tab().editor
    }

    pub fn editor_mut(&mut self) -> &mut EditorPanel {
        &mut self.tab_mut().editor
    }

    pub fn results(&self) -> &ResultsPanel {
        &self.tab().results
    }

    /// The results of whichever tab is running a query.
    pub fn running(&self) -> Option<&ResultsPanel> {
        self.tabs
            .iter()
            .map(|t| &t.results)
            .find(|r| r.running.is_some())
    }

    fn running_tab(&self) -> Option<usize> {
        self.tabs.iter().position(|t| t.results.running.is_some())
    }

    #[cfg(test)]
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    fn set_focus(&mut self, panel: Panel) {
        self.focus.set(panel);
        self.sync_focus();
    }

    fn sync_focus(&mut self) {
        let (editor, results) = (self.focus.is(Panel::Editor), self.focus.is(Panel::Results));
        self.explorer.focused = self.focus.is(Panel::Explorer);
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let active = i == self.active;
            tab.editor.focused = active && editor;
            tab.results.focused = active && results;
        }
    }

    fn new_tab(&mut self) {
        self.next_tab += 1;
        self.tabs.push(Tab {
            editor: EditorPanel::new(self.driver),
            results: ResultsPanel::new(self.driver),
            number: self.next_tab,
        });
        self.active = self.tabs.len() - 1;
        self.sync_focus();
    }

    fn close_tab(&mut self, ctx: &mut Ctx) {
        self.tab_mut().results.abandon(ctx);
        if self.tabs.len() == 1 {
            self.tabs.clear();
            self.next_tab = 0;
            self.new_tab();
        } else {
            self.tabs.remove(self.active);
            self.active = self.active.min(self.tabs.len() - 1);
        }
        self.sync_focus();
    }

    fn switch_tab(&mut self, forward: bool) {
        let n = self.tabs.len();
        self.active = if forward {
            (self.active + 1) % n
        } else {
            (self.active + n - 1) % n
        };
        self.sync_focus();
    }

    /// Runs `sql` in the current tab, after checking it. Destructive
    /// statements come back as a command to ask first.
    fn run(&mut self, sql: Option<String>, ctx: &mut Ctx) -> Option<Command> {
        let Some(sql) = sql.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
            ctx.toast(Level::Info, "Nothing to run");
            return None;
        };
        let findings = guard::check(&sql, self.driver);
        if self.read_only && findings.writes {
            ctx.toast(
                Level::Error,
                "This connection is read-only: the query would write",
            );
            return None;
        }
        if !findings.destructive.is_empty() && ctx.config.confirm_destructive {
            return Some(Command::ConfirmRun {
                sql,
                reasons: findings.destructive,
            });
        }
        self.run_checked(sql, ctx);
        None
    }

    /// Runs `sql` in the current tab without checking it again.
    pub fn run_checked(&mut self, sql: String, ctx: &mut Ctx) {
        if let Some(i) = self.running_tab() {
            let message = if i == self.active {
                "A query is already running".to_string()
            } else {
                format!("A query is already running in {}", self.title(i))
            };
            ctx.toast(Level::Warning, message);
            return;
        }
        self.next_query += 1;
        let id = self.next_query;
        self.tab_mut().results.run(id, &sql, ctx);
    }

    fn title(&self, i: usize) -> String {
        format!("Query {}", self.tabs[i].number)
    }

    fn results_for(&mut self, id: QueryId) -> Option<&mut ResultsPanel> {
        self.tabs
            .iter_mut()
            .map(|t| &mut t.results)
            .find(|r| r.is_current(id))
    }

    /// Applies a query's events to the tab running it. Returns true when the
    /// explorer should reload, as the query may have changed tables.
    pub fn on_query_event(&mut self, event: EngineEvent, ctx: &mut Ctx) -> bool {
        match event {
            EngineEvent::Columns { id, columns } => {
                if let Some(results) = self.results_for(id) {
                    results.on_columns(id, columns);
                }
            }
            EngineEvent::Rows { id, rows } => {
                if let Some(results) = self.results_for(id) {
                    results.on_rows(id, rows);
                }
            }
            EngineEvent::QueryDone { id, outcome } => {
                let Some(finished) = self
                    .results_for(id)
                    .and_then(|results| results.on_done(id, outcome, ctx))
                else {
                    return false;
                };
                let entry = Entry {
                    ok: finished.ok,
                    rows: finished.rows,
                    ms: finished.elapsed.as_millis() as u64,
                    ..Entry::now(&self.connection, &self.explorer.active, &finished.sql)
                };
                if let Err(err) = ctx.history.push(entry) {
                    if !self.history_failed {
                        self.history_failed = true;
                        ctx.toast(Level::Warning, format!("History not saved: {err}"));
                    }
                }
                return finished.changed;
            }
            _ => {}
        }
        false
    }

    fn copy(&mut self, ctx: &mut Ctx) -> Option<Command> {
        if let Some(i) = self.running_tab() {
            self.tabs[i].results.cancel(ctx);
            return None;
        }
        self.delegate(Action::Copy, ctx)
    }

    fn delegate(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        match self.focus.get() {
            Panel::Explorer => self.explorer.handle(action, ctx),
            Panel::Editor => self.tab_mut().editor.handle(action, ctx),
            Panel::Results => self.tab_mut().results.handle(action, ctx),
        }
    }

    /// The focused panel has a menu or search open that takes every key.
    fn modal(&self) -> bool {
        match self.focus.get() {
            Panel::Explorer => false,
            Panel::Editor => self.editor().is_modal(),
            Panel::Results => self.results().is_modal(),
        }
    }

    /// Pasted text goes to the editor. Returns false when the editor isn't
    /// focused.
    pub fn paste(&mut self, text: &str) -> bool {
        if self.focus.is(Panel::Editor) {
            self.editor_mut().paste(text);
            true
        } else {
            false
        }
    }

    /// Clicks focus a panel and pick what is under the pointer; the wheel
    /// scrolls the panel under it.
    pub fn mouse(&mut self, event: MouseEvent, ctx: &mut Ctx) -> Option<Command> {
        let point = Position::new(event.column, event.row);
        let [explorer, editor, results] = self.areas.get();
        let under = if explorer.contains(point) {
            Some(Panel::Explorer)
        } else if editor.contains(point) {
            Some(Panel::Editor)
        } else if results.contains(point) {
            Some(Panel::Results)
        } else {
            None
        };
        let sideways = event.modifiers.contains(KeyModifiers::SHIFT);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let panel = under?;
                if self.modal() {
                    return None;
                }
                self.set_focus(panel);
                match panel {
                    Panel::Explorer => return self.explorer.click(event.column, event.row, ctx),
                    Panel::Editor => {
                        self.editor_mut().click(event.column, event.row, false);
                    }
                    Panel::Results => {
                        self.tab_mut().results.click(event.column, event.row, false);
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.focus.get() {
                Panel::Editor => {
                    self.editor_mut().click(event.column, event.row, true);
                }
                Panel::Results => {
                    self.tab_mut().results.click(event.column, event.row, true);
                }
                Panel::Explorer => {}
            },
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let down = event.kind == MouseEventKind::ScrollDown;
                let lines = if down { WHEEL } else { -WHEEL };
                match under? {
                    Panel::Explorer => {
                        let action = if down { Action::Down } else { Action::Up };
                        for _ in 0..WHEEL {
                            self.explorer.handle(action, ctx);
                        }
                    }
                    Panel::Editor => self.editor_mut().scroll_by(lines),
                    Panel::Results if sideways => {
                        self.tab_mut().results.scroll_columns(lines.signum())
                    }
                    Panel::Results => {
                        let results = &mut self.tab_mut().results;
                        if !results.scroll_inspector(event.column, event.row, lines) {
                            results.scroll_by(lines);
                        }
                    }
                }
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                if under == Some(Panel::Results) =>
            {
                let cols = if event.kind == MouseEventKind::ScrollRight {
                    1
                } else {
                    -1
                };
                self.tab_mut().results.scroll_columns(cols);
            }
            _ => {}
        }
        None
    }
}

impl Component for Workspace {
    fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        if self.modal() {
            return self.delegate(action, ctx);
        }
        // The autocomplete popup takes the keys that pick from it.
        let picking = matches!(
            action,
            Action::NextFocus | Action::Confirm | Action::Up | Action::Down | Action::Back
        );
        if self.editor().completing() {
            if picking && self.focus.is(Panel::Editor) {
                return self.delegate(action, ctx);
            }
            self.editor_mut().close_completion();
        }
        match action {
            Action::NextFocus => {
                self.focus.next();
                self.sync_focus();
            }
            Action::PrevFocus => {
                self.focus.prev();
                self.sync_focus();
            }
            Action::Execute => {
                let sql = self.editor().statement();
                return self.run(sql, ctx);
            }
            Action::ExecuteAll => {
                let sql = Some(self.editor().text());
                return self.run(sql, ctx);
            }
            Action::Complete if self.focus.is(Panel::Editor) => {
                let tab = &mut self.tabs[self.active];
                tab.editor.complete(&self.catalog, action);
            }
            Action::Inspect | Action::Confirm if self.focus.is(Panel::Results) => {
                return self.delegate(action, ctx);
            }
            Action::Copy => return self.copy(ctx),
            Action::NewTab => {
                self.new_tab();
                self.set_focus(Panel::Editor);
            }
            Action::CloseTab => self.close_tab(ctx),
            Action::NextTab => self.switch_tab(true),
            Action::PrevTab => self.switch_tab(false),
            Action::History => {
                self.set_focus(Panel::Editor);
                return self.delegate(action, ctx);
            }
            Action::Debug => {
                self.tab_mut().results.show_log(ctx.log.entries());
                self.set_focus(Panel::Results);
            }
            Action::Connections => {
                for tab in &mut self.tabs {
                    tab.results.abandon(ctx);
                }
                return Some(Command::Connections);
            }
            Action::Back if self.focus.is(Panel::Results) && self.results().takes_back() => {
                return self.delegate(action, ctx);
            }
            Action::Back => return Some(Command::ConfirmQuit),
            action => {
                let command = self.delegate(action, ctx);
                if self.focus.is(Panel::Editor) {
                    let tab = &mut self.tabs[self.active];
                    tab.editor.complete(&self.catalog, action);
                }
                return command;
            }
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(28), Constraint::Fill(1)]).areas(area);
        let [editor, results] =
            Layout::vertical([Constraint::Percentage(35), Constraint::Fill(1)]).areas(right);
        self.areas.set([left, editor, results]);
        let labels: Vec<TabLabel> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| TabLabel {
                title: self.title(i),
                active: i == self.active,
                running: tab.results.running.is_some(),
            })
            .collect();
        self.explorer.render(f, left, ctx);
        self.editor().render_with(f, editor, ctx, &labels);
        self.results().render(f, results, ctx);
        self.editor().render_overlay(f, area, ctx);
    }

    fn is_animating(&self) -> bool {
        self.explorer.is_animating() || self.running().is_some()
    }

    fn wants_text(&self) -> bool {
        match self.focus.get() {
            Panel::Editor => true,
            Panel::Explorer => self.explorer.wants_text(),
            Panel::Results => self.results().wants_text(),
        }
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
