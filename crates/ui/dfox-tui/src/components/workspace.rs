//! The main screen once a database is open: the explorer, and query tabs of
//! an editor with its results. Tab moves focus between the panels.

use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

use unicode_width::UnicodeWidthStr;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use dfox_core::{
    catalog::{EntityInfo, Node, NodeKind, NodePath},
    driver::Driver,
    engine::{Command as DbCommand, EngineEvent, QueryId},
    errors::DbError,
    models::connections::DbType,
    session::{ApplyReport, Capabilities, QueryLanguage},
};
use ratatui::{
    layout::{Constraint, Layout, Position, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use super::{
    bar::{self, Bar, Button, Click},
    card::{CardEvent, ColumnCard, TableCard},
    checklist::Checklist,
    copy::CopyDialog,
    editor::{Checker, EditorPanel, TabLabel},
    explorer::Explorer,
    export::{DialogEvent, ExportDialog},
    manager::EntryKey,
    panel,
    results::{self, ResultsPanel},
    table_menu::{self, mongo_handle, mongo_key, Choice, MenuEvent, Script, TableMenu, FEW_ROWS},
};
use crate::{
    action::Action,
    catalog::Catalog,
    command::Command,
    component::{Component, Ctx, Focus, RenderCtx},
    config::LayoutStyle,
    connections::Tag,
    history::Entry,
    sql::{self, guard, Dialect},
    widgets::{throbber::spinner, toast::Level},
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

/// Postgres's explain options: the keyword, its label, whether it is on by
/// default, and whether it needs `ANALYZE`.
const EXPLAIN_OPTIONS: [(&str, &str, bool, bool); 7] = [
    ("VERBOSE", "Verbose", false, false),
    ("COSTS", "Costs", true, false),
    ("BUFFERS", "Buffers", false, false),
    ("TIMING", "Timing (analyze)", true, true),
    ("SUMMARY", "Summary", false, false),
    ("SETTINGS", "Settings", false, false),
    ("WAL", "Wal (analyze)", false, true),
];

/// Postgres's `EXPLAIN` with the options that differ from its defaults.
fn postgres_explain(analyze: bool, options: &[bool; 7]) -> String {
    let mut parts: Vec<String> = analyze.then(|| "ANALYZE".to_string()).into_iter().collect();
    for ((keyword, _, default, needs_analyze), on) in EXPLAIN_OPTIONS.iter().zip(options) {
        if on != default && (analyze || !needs_analyze) {
            parts.push(if *on {
                keyword.to_string()
            } else {
                format!("{keyword} OFF")
            });
        }
    }
    match parts.len() {
        0 => "EXPLAIN ".into(),
        1 if analyze => "EXPLAIN ANALYZE ".into(),
        _ => format!("EXPLAIN ({}) ", parts.join(", ")),
    }
}

/// A query tab: its text and its last results.
#[derive(Debug)]
pub struct Tab {
    pub editor: EditorPanel,
    pub results: ResultsPanel,
    number: usize,
    /// The view or routine whose definition it was opened with.
    source: Option<(NodePath, NodeKind)>,
    /// Its name, when it was opened with a definition, script or a
    /// table's rows.
    title: Option<String>,
    /// The table whose rows it was opened to show.
    rows_of: Option<NodePath>,
}

/// Where the rows just saved are on screen, for their effect.
#[derive(Debug, Default, PartialEq)]
pub struct Saved {
    pub deleted: Vec<Rect>,
    pub changed: Vec<Rect>,
}

/// How long the saved rows' effect plays before the rows reload.
const SAVED_EFFECT: std::time::Duration = std::time::Duration::from_millis(250);

/// An export being written: where, and how far it has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exporting {
    pub id: QueryId,
    pub file: std::path::PathBuf,
    /// The table it imports into; `None` for an export.
    pub import: Option<NodePath>,
    /// The connection it copies into; `None` for an export or import.
    pub copy_to: Option<String>,
    pub rows: u64,
    /// About how many rows there are, when the table says.
    pub estimate: Option<u64>,
    pub started: tokio::time::Instant,
}

/// What `CREATE` makes for an object of `kind` with rows.
fn kind_or_table(kind: NodeKind) -> NodeKind {
    match kind {
        NodeKind::View | NodeKind::MaterializedView => kind,
        _ => NodeKind::Table,
    }
}

/// Which rows of a table to show.
enum Rows {
    First,
    All,
    /// The last, by these key columns.
    Last(Vec<String>),
    /// Those meeting a condition: SQL after WHERE, or a find filter.
    Matching(String),
}

/// A menu choice waiting for its table to be described.
#[derive(Debug)]
struct Pending {
    table: NodePath,
    kind: NodeKind,
    name: Vec<String>,
    choice: Choice,
}

#[derive(Debug)]
pub struct Workspace {
    focus: Focus<Panel>,
    pub explorer: Explorer,
    tabs: Vec<Tab>,
    active: usize,
    dialect: Dialect,
    /// What the session speaks: SQL, or mongo shell commands and the like.
    language: QueryLanguage,
    checker: Checker,
    connection: String,
    next_query: QueryId,
    next_tab: usize,
    /// The history file couldn't be written; said once.
    history_failed: bool,
    /// The connection is read-only: scripts that write aren't sent.
    read_only: bool,
    /// The session can write edits made in the grid.
    can_edit: bool,
    /// Tables and columns, for autocomplete.
    pub catalog: Catalog,
    /// Where the explorer, editor and results were drawn, for the mouse.
    areas: Cell<[Rect; 3]>,
    /// The columns each tab's name was drawn across, on the editor's top
    /// border, for clicks.
    tab_spots: RefCell<Vec<(u16, u16)>>,
    /// The new layout's row of query tabs, and the column of each tab's ×.
    strip: Cell<Rect>,
    closes: RefCell<Vec<u16>>,
    /// The connection's colour, for the new layout's connection bar.
    tag: Tag,
    /// Where the bar shows the database, for clicks.
    database_spot: Cell<Rect>,
    /// The new layout's rail of the explorer, the query and the connections.
    rail: Cell<[Rect; 3]>,
    /// The new layout's `+` after the query tabs.
    plus: Cell<Rect>,
    /// The toolbar's buttons where they were drawn, and the one the mouse
    /// is over.
    toolbar: RefCell<Vec<(Rect, Button)>>,
    hovered: Option<Button>,
    /// The focused editor or results fill the workspace.
    maximized: bool,
    /// Where the maximize buttons of the editor and the results were drawn.
    buttons: Cell<[Rect; 2]>,
    /// The column card, over everything while it is open.
    card: Option<ColumnCard>,
    /// What Enter on a table offers, while it is open.
    menu: Option<TableMenu>,
    /// The explain options, dropped down.
    checklist: Option<Checklist>,
    /// Postgres's explain options, as [`EXPLAIN_OPTIONS`] lists them.
    explain_options: [bool; 7],
    /// A table's properties, while they are shown.
    properties: Option<TableCard>,
    pending: Option<Pending>,
    /// Where to export rows, while it is asked.
    export_dialog: Option<ExportDialog>,
    /// The export being written.
    exporting: Option<Exporting>,
    /// When to reload the rows of a tab whose edits were just saved: after
    /// the saved rows' effect has played.
    refresh_at: Option<(tokio::time::Instant, usize)>,
    /// A transaction was opened by a query run here and not closed yet.
    pub in_transaction: bool,
    /// The saved connections, offered under the explorer's drop-down.
    pub saved: Vec<String>,
    /// The connection chosen there, while the keys are on them.
    switching: Option<usize>,
    /// Where those connections were drawn, one per row.
    switches: Cell<Rect>,
}

impl Workspace {
    /// A workspace on `database`, the one the session is connected to.
    pub fn new(connection: &str, driver: DbType, database: &str) -> Self {
        let mut workspace = Self {
            focus: Focus::new(ORDER),
            explorer: Explorer::new(database),
            tabs: Vec::new(),
            active: 0,
            dialect: sql::dialect(driver),
            language: sql::language(driver),
            checker: Checker::default(),
            connection: connection.to_string(),
            next_query: 0,
            next_tab: 0,
            history_failed: false,
            read_only: false,
            can_edit: false,
            catalog: Catalog::default(),
            areas: Cell::new([Rect::default(); 3]),
            tab_spots: RefCell::new(Vec::new()),
            strip: Cell::new(Rect::default()),
            closes: RefCell::new(Vec::new()),
            tag: Tag::None,
            database_spot: Cell::new(Rect::default()),
            toolbar: RefCell::new(Vec::new()),
            rail: Cell::new([Rect::default(); 3]),
            plus: Cell::new(Rect::default()),
            hovered: None,
            maximized: false,
            buttons: Cell::new([Rect::default(); 2]),
            card: None,
            menu: None,
            saved: Vec::new(),
            switching: None,
            switches: Cell::new(Rect::default()),
            checklist: None,
            explain_options: EXPLAIN_OPTIONS.map(|(_, _, default, _)| default),
            properties: None,
            pending: None,
            export_dialog: None,
            exporting: None,
            refresh_at: None,
            in_transaction: false,
        };
        workspace.explorer.driver = Some(driver);
        workspace.new_tab();
        workspace.set_focus(Panel::Editor);
        workspace
    }

    /// The language the session reported. SQL starts the first tab over in
    /// its dialect.
    /// What the session can do: whether rows can be edited in the grid.
    pub fn with_caps(mut self, caps: Capabilities) -> Self {
        self.can_edit = caps.contains(Capabilities::EDIT_BY_PK);
        self
    }

    pub fn with_language(mut self, language: QueryLanguage) -> Self {
        self.language = language;
        match sql::session_dialect(language) {
            Some(dialect) => self.with_dialect(dialect),
            None => {
                for tab in &mut self.tabs {
                    tab.editor.language = language;
                }
                self
            }
        }
    }

    /// The dialect the session reported, for the editor, autocomplete and
    /// the guard. The first tab starts over in it.
    pub fn with_dialect(mut self, dialect: Dialect) -> Self {
        self.dialect = dialect;
        self.tabs.clear();
        self.next_tab = 0;
        self.new_tab();
        self.set_focus(Panel::Editor);
        self
    }

    /// The session's driver, which checks the editors' statements.
    pub fn with_checker(mut self, driver: Option<Arc<dyn Driver>>) -> Self {
        self.checker = Checker(driver);
        for tab in &mut self.tabs {
            tab.editor.checker = self.checker.clone();
        }
        self
    }

    pub fn with_tag(mut self, tag: Tag) -> Self {
        self.tag = tag;
        self
    }

    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Children of a catalog node arrived. Tables under it go to
    /// autocomplete, which then asks for their columns.
    pub fn on_children(
        &mut self,
        path: &NodePath,
        result: Result<Vec<Node>, DbError>,
        ctx: &mut Ctx,
    ) {
        if let (Ok(nodes), QueryLanguage::RedisCmd) = (&result, self.language) {
            if !path.parts().is_empty() {
                self.catalog.on_keys(path, nodes);
                // Keys asked for while typing one: show them.
                let tab = &mut self.tabs[self.active];
                if self.focus.is(Panel::Editor) && tab.editor.typed_key().is_some() {
                    tab.editor.complete(&self.catalog, Action::Complete);
                }
            }
        }
        if let (Ok(nodes), true) = (&result, path.parts().is_empty()) {
            let databases = nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Database)
                .map(|n| n.name.clone());
            self.catalog.on_databases(databases.collect());
        }
        if let (Ok(nodes), Some(name)) = (&result, path.name()) {
            let tables: Vec<String> = nodes
                .iter()
                .filter(|n| n.kind.has_rows())
                .map(|n| n.name.clone())
                .collect();
            let schemas: Vec<String> = nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Schema)
                .map(|n| n.name.clone())
                .collect();
            let active = path.parts() == [self.explorer.active.as_str()];
            // The active database's schemas, the default first; a database
            // without schemas is its own.
            if active && !schemas.is_empty() {
                self.catalog.on_schemas(&schemas);
            } else if active {
                self.catalog.on_schemas(&[name.to_string()]);
            }
            let functions: Vec<(String, Option<String>)> = nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Function)
                .map(|n| (n.name.clone(), n.detail.clone()))
                .collect();
            if !functions.is_empty() {
                self.catalog.on_functions(name, functions);
            }
            if !tables.is_empty() {
                self.catalog.on_tables(name, tables);
                ctx.engine
                    .send(DbCommand::AllColumns { path: path.clone() });
            }
        }
        self.explorer.on_children(path, result, ctx);
    }

    /// Lists the keys under the Redis prefix being typed, with the
    /// explorer's `SCAN`, when they haven't been listed yet.
    fn list_typed_keys(&mut self, ctx: &mut Ctx) {
        if self.language != QueryLanguage::RedisCmd {
            return;
        }
        let Some(typed) = self.editor().typed_key() else {
            return;
        };
        if let Some(path) = self.catalog.ask_prefix(&typed) {
            ctx.engine.send(DbCommand::Children { path });
        }
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

    /// The results on screen want redraws at an animation's pace.
    pub fn smooth(&self, now: tokio::time::Instant) -> bool {
        self.results().smooth(now)
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

    pub fn active_tab(&self) -> usize {
        self.active
    }

    /// Where `panel` was last drawn.
    pub fn area(&self, panel: Panel) -> Rect {
        let [explorer, editor, results] = self.areas.get();
        match panel {
            Panel::Explorer => explorer,
            Panel::Editor => editor,
            Panel::Results => results,
        }
    }

    /// True when query `id` runs in the tab on screen.
    pub fn shows_query(&self, id: QueryId) -> bool {
        self.results().is_current(id)
    }

    #[cfg(test)]
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    fn set_focus(&mut self, panel: Panel) {
        self.focus.set(panel);
        self.switching = None;
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
        let mut editor = EditorPanel::new(self.dialect).with_language(self.language);
        editor.checker = self.checker.clone();
        self.tabs.push(Tab {
            editor,
            results: ResultsPanel::new(self.dialect),
            number: self.next_tab,
            source: None,
            title: None,
            rows_of: None,
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

    /// Closes tab `i`, the active one staying active.
    pub fn close_tab_at(&mut self, i: usize, ctx: &mut Ctx) {
        let was = self.active;
        self.active = i;
        self.close_tab(ctx);
        if i < was {
            self.active = was - 1;
        } else if i > was {
            self.active = was;
        }
        self.sync_focus();
    }

    /// A click on tab `i`'s ×: asks first when the tab holds a query.
    fn close_clicked(&mut self, i: usize, ctx: &mut Ctx) -> Option<Command> {
        if self.tabs[i].editor.text().trim().is_empty() {
            self.close_tab_at(i, ctx);
            return None;
        }
        Some(Command::ConfirmCloseTab {
            tab: i,
            title: self.title(i),
        })
    }

    /// Draws the new layout's rail: the explorer, the query, the
    /// connections, the one in use marked.
    fn render_rail(&self, f: &mut Frame, rail: Rect, ctx: &RenderCtx) {
        if rail.is_empty() {
            self.rail.set([Rect::default(); 3]);
            return;
        }
        let theme = ctx.theme;
        f.render_widget(Paragraph::new("").style(theme.surface()), rail);
        let glyphs = match (ctx.icons.nerd, ctx.icons.ascii) {
            (true, _) => ["\u{f1bb}", "\u{f121}", "\u{f1c0}"],
            (_, true) => ["T", "Q", "C"],
            _ => ["▤", "✎", "◆"],
        };
        let explorer = self.focus.is(Panel::Explorer);
        let mut spots = [Rect::default(); 3];
        for (i, glyph) in glyphs.into_iter().enumerate() {
            let spot =
                Rect::new(rail.x, rail.y + 1 + 2 * i as u16, rail.width, 1).intersection(rail);
            let active = (i == 0 && explorer) || (i == 1 && !explorer);
            let style = if active {
                theme.selection()
            } else {
                theme.dim().bg(theme.surface)
            };
            f.render_widget(Paragraph::new(glyph).centered().style(style), spot);
            spots[i] = spot;
        }
        self.rail.set(spots);
    }

    /// Draws the new layout's row of query tabs, keeping where each landed.
    fn render_strip(&self, f: &mut Frame, strip: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let close = if ctx.icons.ascii { "x" } else { "×" };
        let (mut spans, mut spots, mut closes) = (Vec::new(), Vec::new(), Vec::new());
        let mut x = strip.x;
        for i in 0..self.tabs.len() {
            if self.tabs[i].results.running.is_some() {
                spans.push(Span::raw(" "));
                spans.push(spinner(ctx, theme.key()));
                x += 2;
            }
            let title = self.title(i);
            let width = UnicodeWidthStr::width(title.as_str()) as u16;
            let style = if i == self.active {
                theme.key().add_modifier(Modifier::UNDERLINED)
            } else {
                theme.dim()
            };
            spans.push(Span::raw(" "));
            spots.push((x + 1, x + 1 + width));
            spans.push(Span::styled(title, style));
            spans.push(Span::styled(format!(" {close} "), theme.dim()));
            closes.push(x + 2 + width);
            spans.push(Span::styled("│", theme.dim()));
            x += width + 5;
        }
        spans.push(Span::raw(" "));
        let hovered = self.hovered == Some(bar::NEW_TAB);
        spans.push(Span::styled(" + ", bar::button_style(theme, hovered)));
        self.plus
            .set(Rect::new(x + 1, strip.y, 3, 1).intersection(strip));
        f.render_widget(Paragraph::new(Line::from(spans)).style(theme.base()), strip);
        self.tab_spots.replace(spots);
        self.closes.replace(closes);
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
        let checked = sql.clone();
        self.run_guarded(sql, &checked, ctx)
    }

    /// What `EXPLAIN` is in this database, `None` where there is none.
    fn explain_prefix(&self, analyze: bool) -> Option<String> {
        if !matches!(self.language, QueryLanguage::Sql(_)) {
            return None;
        }
        match (self.dialect, analyze) {
            (Dialect::Postgres, _) => Some(postgres_explain(analyze, &self.explain_options)),
            (Dialect::MySql, false) => Some("EXPLAIN ".into()),
            (Dialect::MySql, true) => Some("EXPLAIN ANALYZE ".into()),
            (Dialect::Sqlite, false) => Some("EXPLAIN QUERY PLAN ".into()),
            _ => None,
        }
    }

    /// Drops the explain options down under their button.
    fn open_explain_options(&mut self) {
        let anchor = self
            .toolbar
            .borrow()
            .iter()
            .find(|(_, b)| *b == bar::EXPLAIN_OPTIONS)
            .map(|(spot, _)| *spot)
            .unwrap_or_default();
        let items = EXPLAIN_OPTIONS
            .iter()
            .zip(self.explain_options)
            .map(|((_, label, _, _), on)| (label.to_string(), on))
            .collect();
        self.checklist = Some(Checklist::new(items, anchor));
    }

    /// The checklist has the keys.
    fn checklist_handle(&mut self, action: Action) {
        let Some(list) = self.checklist.as_mut() else {
            return;
        };
        if !list.handle(action) {
            self.checklist = None;
            return;
        }
        self.keep_explain_options();
    }

    fn keep_explain_options(&mut self) {
        if let Some(list) = &self.checklist {
            for (on, (_, ticked)) in self.explain_options.iter_mut().zip(&list.items) {
                *on = *ticked;
            }
        }
    }

    /// Runs the statement under the cursor explained. `ANALYZE` runs it, so
    /// the statement itself is what the guard checks.
    fn explain(&mut self, analyze: bool, ctx: &mut Ctx) -> Option<Command> {
        let Some(prefix) = self.explain_prefix(analyze) else {
            let what = if analyze {
                "explain analyze"
            } else {
                "explain"
            };
            ctx.toast(Level::Info, format!("This database has no {what}"));
            return None;
        };
        let statement = self.editor().statement();
        let Some(statement) = statement
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        else {
            ctx.toast(Level::Info, "Nothing to run");
            return None;
        };
        self.run_guarded(format!("{prefix}{statement}"), &statement, ctx)
    }

    /// The toolbar for what can be done now, most wanted first.
    fn buttons(&self) -> Vec<Button> {
        let mut buttons = vec![bar::RUN];
        if self.running().is_some() {
            buttons.push(bar::STOP);
        }
        if self.in_transaction {
            buttons.extend([bar::COMMIT, bar::ROLLBACK]);
        }
        buttons.push(bar::SCRIPT);
        if self.explain_prefix(false).is_some() {
            buttons.push(bar::EXPLAIN);
        }
        if self.explain_prefix(true).is_some() {
            buttons.push(bar::ANALYZE);
        }
        if self.dialect == Dialect::Postgres && matches!(self.language, QueryLanguage::Sql(_)) {
            buttons.push(bar::EXPLAIN_OPTIONS);
        }
        buttons.push(bar::LIMIT);
        if dfox_core::result::ResultSet::row_count(&self.results().store) > 0 {
            buttons.push(bar::FILTER);
        }
        if self.results().rearranged() {
            buttons.push(bar::UNFILTER);
        }
        buttons.extend([bar::FORMAT, bar::HISTORY, bar::HELP]);
        buttons
    }

    /// What the status bar says about the button under the mouse.
    pub fn tooltip(&self, keymap: &crate::keymap::Keymap) -> Option<String> {
        self.hovered.map(|b| b.tooltip(keymap))
    }

    /// Runs `sql`, checked as if it were `checked`.
    fn run_guarded(&mut self, sql: String, checked: &str, ctx: &mut Ctx) -> Option<Command> {
        // New rows would replace the edited ones.
        let pending = self.results().pending();
        if pending > 0 {
            ctx.toast(
                Level::Warning,
                format!("Save or undo the {pending} change(s) to these rows first"),
            );
            return None;
        }
        let findings = match self.language {
            QueryLanguage::Sql(_) | QueryLanguage::Cql => guard::check(checked, self.dialect),
            QueryLanguage::MongoShell => sql::mongo::check(checked),
            // Only flags what wipes a database: the driver itself refuses
            // writes on a read-only connection.
            QueryLanguage::RedisCmd => sql::redis::check(checked),
        };
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
        if self.busy(ctx) {
            return;
        }
        self.next_query += 1;
        let id = self.next_query;
        self.tab_mut().results.run(id, &sql, ctx);
    }

    /// True, after saying so, while a query runs: one runs at a time.
    fn busy(&self, ctx: &mut Ctx) -> bool {
        let Some(i) = self.running_tab() else {
            return false;
        };
        let message = if i == self.active {
            "A query is already running".to_string()
        } else {
            format!("A query is already running in {}", self.title(i))
        };
        ctx.toast(Level::Warning, message);
        true
    }

    fn title(&self, i: usize) -> String {
        let tab = &self.tabs[i];
        tab.title
            .clone()
            .unwrap_or_else(|| format!("Query {}", tab.number))
    }

    pub(crate) fn label(&self, i: usize) -> TabLabel {
        let tab = &self.tabs[i];
        TabLabel {
            named: tab.title.is_some(),
            title: self.title(i),
            active: i == self.active,
            running: tab.results.running.is_some(),
        }
    }

    /// Where the tabs' names land on the editor's top border: after its
    /// corner and a space, ` │ ` between them, a spinner before a running
    /// one.
    fn tab_spots_in(&self, editor: Rect) -> Vec<(u16, u16)> {
        if self.tabs.len() < 2 || editor.is_empty() {
            return Vec::new();
        }
        let mut x = editor.x + 2;
        let mut spots = Vec::new();
        for i in 0..self.tabs.len() {
            if i > 0 {
                x += 3;
            }
            if self.tabs[i].results.running.is_some() {
                x += 2;
            }
            let width = UnicodeWidthStr::width(self.title(i).as_str()) as u16;
            spots.push((x, x + width));
            x += width;
        }
        spots
    }

    /// The tab whose name is at a point on screen.
    fn tab_at(&self, column: u16, row: u16) -> Option<usize> {
        let [_, editor, _] = self.areas.get();
        let strip = self.strip.get();
        if row != if strip.is_empty() { editor.y } else { strip.y } {
            return None;
        }
        self.tab_spots
            .borrow()
            .iter()
            .position(|(start, end)| (*start..*end).contains(&column))
    }

    /// The tab opened with the definition of `path`, a `kind`.
    fn definition_tab(&self, path: &NodePath, kind: NodeKind) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| t.source.as_ref() == Some(&(path.clone(), kind)))
    }

    /// The definition of `path` arrived: it opens in a tab of its own, or
    /// in the current one while that is empty.
    pub fn on_definition(
        &mut self,
        path: NodePath,
        kind: NodeKind,
        result: Result<String, DbError>,
        ctx: &mut Ctx,
    ) {
        let text = match result {
            Ok(text) => text,
            Err(err) => {
                let name = path.name().unwrap_or_default();
                ctx.toast(Level::Error, format!("Couldn't open {name}: {err}"));
                return;
            }
        };
        if let Some(i) = self.definition_tab(&path, kind) {
            self.active = i;
            self.set_focus(Panel::Editor);
        } else {
            let title = path.name().unwrap_or_default().to_string();
            self.open_text(title, Some((path, kind)), &text);
        }
    }

    /// Opens `text` in a tab named `title`: the current one while it is
    /// empty, or a new one.
    fn open_text(&mut self, title: String, source: Option<(NodePath, NodeKind)>, text: &str) {
        let tab = self.tab();
        let blank = tab.title.is_none()
            && tab.editor.text().trim().is_empty()
            && tab.results.running.is_none();
        if !blank {
            self.new_tab();
        }
        let tab = self.tab_mut();
        tab.source = source;
        tab.rows_of = None;
        tab.title = Some(title);
        tab.editor.open(text);
        self.set_focus(Panel::Editor);
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
        if let Some(export) = &mut self.exporting {
            match event {
                EngineEvent::Progress { id, rows } if id == export.id => {
                    export.rows = rows;
                    return false;
                }
                EngineEvent::QueryDone { id, outcome } if id == export.id => {
                    let Some(export) = self.exporting.take() else {
                        return false;
                    };
                    let file = export.file.display().to_string();
                    let rows = |n: u64| results::grid::group_digits(&n.to_string());
                    if let Some(into) = &export.copy_to {
                        match outcome {
                            Ok(stats) => ctx.toast(
                                Level::Success,
                                format!(
                                    "Copied {} rows of {file} into {into} in {}",
                                    rows(stats.rows),
                                    results::format_duration(stats.elapsed)
                                ),
                            ),
                            Err(DbError::Cancelled) => ctx.toast(
                                Level::Info,
                                format!("Copy cancelled after {} rows went in", rows(export.rows)),
                            ),
                            Err(err) => ctx.toast(Level::Error, format!("Couldn't copy: {err}")),
                        }
                        return false;
                    }
                    match (outcome, &export.import) {
                        (Ok(stats), None) => ctx.toast(
                            Level::Success,
                            format!(
                                "Exported {} rows to {file} in {}",
                                rows(stats.rows),
                                results::format_duration(stats.elapsed)
                            ),
                        ),
                        (Ok(stats), Some(table)) => {
                            ctx.toast(
                                Level::Success,
                                format!(
                                    "Imported {} rows into {} in {}",
                                    rows(stats.rows),
                                    table.name().unwrap_or_default(),
                                    results::format_duration(stats.elapsed)
                                ),
                            );
                            self.explorer.refresh_table(table, ctx);
                        }
                        (Err(DbError::Cancelled), None) => {
                            ctx.toast(Level::Info, "Export cancelled")
                        }
                        (Err(DbError::Cancelled), Some(_)) => ctx.toast(
                            Level::Info,
                            format!("Import cancelled after {} rows went in", rows(export.rows)),
                        ),
                        (Err(err), None) => {
                            ctx.toast(Level::Error, format!("Couldn't export: {err}"))
                        }
                        (Err(err), Some(_)) => {
                            ctx.toast(Level::Error, format!("Couldn't import: {err}"))
                        }
                    }
                    return false;
                }
                EngineEvent::Notice { id, message } if id == export.id => {
                    ctx.toast(Level::Info, message);
                    return false;
                }
                _ => {}
            }
        }
        match event {
            EngineEvent::Schema { id, schema } => {
                if let Some(results) = self.results_for(id) {
                    results.on_schema(id, schema);
                }
            }
            EngineEvent::Batch { id, batch } => {
                if let Some(results) = self.results_for(id) {
                    results.on_batch(id, batch);
                }
            }
            EngineEvent::Notice { id, message } => {
                let workspace = ctx.config.layout == LayoutStyle::Workspace;
                match self.results_for(id) {
                    Some(results) if workspace => results.messages.notice(message),
                    Some(_) => ctx.toast(Level::Info, message),
                    None => {}
                }
            }
            EngineEvent::QueryDone { id, outcome } => {
                let place = match &outcome {
                    Err(DbError::At { at, message }) => Some((*at, message.clone())),
                    _ => None,
                };
                let tab = self.tabs.iter().position(|t| t.results.is_current(id));
                let Some(finished) = self
                    .results_for(id)
                    .and_then(|results| results.on_done(id, outcome, ctx))
                else {
                    return false;
                };
                // A table's rows aren't a query to keep.
                let Some(sql) = &finished.sql else {
                    return false;
                };
                if let (Some((at, message)), Some(tab)) = (place, tab) {
                    self.tabs[tab].editor.mark_failure(sql, at, message);
                }
                // A failed run may still have opened one (BEGIN; bad; …), but
                // whether it closed one is unknown.
                if matches!(self.language, QueryLanguage::Sql(_)) {
                    match guard::transaction(sql, self.dialect) {
                        Some(true) => self.in_transaction = true,
                        Some(false) if finished.ok => self.in_transaction = false,
                        _ => {}
                    }
                }
                let entry = Entry {
                    ok: finished.ok,
                    rows: finished.rows,
                    ms: finished.elapsed.as_millis() as u64,
                    ..Entry::now(&self.connection, &self.explorer.active, sql)
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
        // Then the export, so Ctrl+C stops what is running.
        if let Some(export) = &self.exporting {
            ctx.engine.send(DbCommand::Cancel { id: export.id });
            ctx.toast(Level::Info, "Cancelling…");
            return None;
        }
        self.delegate(Action::Copy, ctx)
    }

    /// Puts `sql`, a query for the rows of the table at `table`, in the
    /// table's own tab and runs it there: the query can be read, changed and
    /// run again.
    fn show_rows(&mut self, table: NodePath, sql: String, ctx: &mut Ctx) -> Option<Command> {
        if self.busy(ctx) {
            return None;
        }
        match self
            .tabs
            .iter()
            .position(|t| t.rows_of.as_ref() == Some(&table))
        {
            Some(i) => {
                self.active = i;
                self.editor_mut().open(&sql);
            }
            None => {
                // A Redis key is named by every part after its database.
                let title = if self.language == QueryLanguage::RedisCmd {
                    table.parts()[1..].concat()
                } else {
                    table.name().unwrap_or_default().to_string()
                };
                self.open_text(title, None, &sql);
                self.tab_mut().rows_of = Some(table);
            }
        }
        // Back to the explorer, to look at the next table.
        self.set_focus(Panel::Explorer);
        self.run(Some(sql), ctx)
    }

    /// Does what was picked in a table's menu. Choices that need the
    /// table's columns wait for them.
    fn choose(
        &mut self,
        table: NodePath,
        kind: NodeKind,
        name: Vec<String>,
        choice: Choice,
        ctx: &mut Ctx,
    ) -> Option<Command> {
        let info = self.explorer.described(&table).cloned();
        if choice.needs_info() && info.is_none() {
            self.explorer.describe(&table, ctx);
            self.pending = Some(Pending {
                table,
                kind,
                name,
                choice,
            });
            return None;
        }
        let quoted = self.quote(&name);
        if self.language == QueryLanguage::RedisCmd {
            let key = table.parts()[1..].concat();
            match choice {
                Choice::FirstRows | Choice::AllRows => {
                    let first = (choice == Choice::FirstRows).then_some(FEW_ROWS as usize);
                    let key_type = self.explorer.detail(&table).unwrap_or_default().to_string();
                    let note = info.and_then(|info| info.comment);
                    let command = sql::redis::open_command(&key_type, &key, first);
                    let shown = self.show_rows(table.clone(), command, ctx);
                    // Unless busy, the key's tab is the active one now.
                    if self.tab().rows_of.as_ref() == Some(&table) {
                        let results = &mut self.tab_mut().results;
                        results.note = note;
                        results.json_if_json = matches!(key_type.as_str(), "string" | "ReJSON-RL");
                    }
                    return shown;
                }
                Choice::Drop { .. } => {
                    let command = format!("DEL {quoted}");
                    if !ctx.config.confirm_destructive {
                        return self.run(Some(command), ctx);
                    }
                    return Some(Command::ConfirmRun {
                        reasons: vec![format!("{command} deletes the key")],
                        sql: command,
                    });
                }
                _ => {}
            }
        }
        match choice {
            Choice::FirstRows => {
                let query = self.rows_query(&name, Rows::First);
                return self.show_rows(table, query, ctx);
            }
            Choice::AllRows => {
                let query = self.rows_query(&name, Rows::All);
                return self.show_rows(table, query, ctx);
            }
            Choice::LastRows => {
                let key: Vec<String> = info
                    .iter()
                    .flat_map(|info| &info.columns)
                    .filter(|c| c.primary_key)
                    .map(|c| c.name.clone())
                    .collect();
                if key.is_empty() {
                    ctx.toast(
                        Level::Warning,
                        format!("{quoted} has no primary key to find its last rows by"),
                    );
                    return None;
                }
                let query = self.rows_query(&name, Rows::Last(key));
                return self.show_rows(table, query, ctx);
            }
            Choice::PartitionRows(condition) => {
                // The highest keys first, as the last rows of a table are.
                // CQL orders a partition only as its clustering columns are
                // declared, or exactly the reverse: whichever puts the first
                // one descending.
                let clustering: Vec<(String, bool)> = info
                    .iter()
                    .flat_map(|info| &info.columns)
                    .filter_map(|c| {
                        let declared = c.comment.as_deref()?.strip_prefix("clustering key, ")?;
                        Some((sql::quote_ident(&c.name, self.dialect), declared == "DESC"))
                    })
                    .collect();
                let flip = clustering
                    .first()
                    .is_some_and(|(_, descending)| !descending);
                let order: Vec<String> = clustering
                    .iter()
                    .map(|(name, descending)| {
                        let way = if descending ^ flip { "DESC" } else { "ASC" };
                        format!("{name} {way}")
                    })
                    .collect();
                let mut query = format!("SELECT * FROM {quoted} WHERE {condition}");
                if !order.is_empty() {
                    query.push_str(&format!(" ORDER BY {}", order.join(", ")));
                }
                query.push_str(&format!(" LIMIT {FEW_ROWS}"));
                return self.show_rows(table, query, ctx);
            }
            Choice::Filtered(condition) => {
                let query = self.rows_query(&name, Rows::Matching(condition));
                return self.show_rows(table, query, ctx);
            }
            Choice::Count => ctx.engine.send(DbCommand::Count { path: table }),
            Choice::CopyTo => {
                return Some(Command::CopyTo {
                    sql: self.rows_query(&name, Rows::All),
                    source: table.name().unwrap_or_default().to_string(),
                    estimate: info.as_ref().and_then(|info| info.row_estimate),
                });
            }
            Choice::Import => {
                let columns = info
                    .iter()
                    .flat_map(|info| &info.columns)
                    .map(|c| c.name.clone())
                    .collect();
                self.export_dialog = Some(ExportDialog::import(table, columns));
            }
            Choice::Export => {
                let sql = if self.language == QueryLanguage::RedisCmd {
                    let key_type = self.explorer.detail(&table).unwrap_or_default();
                    sql::redis::open_command(key_type, &table.parts()[1..].concat(), None)
                } else {
                    self.rows_query(&name, Rows::All)
                };
                let estimate = info.as_ref().and_then(|info| info.row_estimate);
                let file_name = table
                    .name()
                    .unwrap_or("rows")
                    .replace(['/', '\\', ':'], "_");
                self.export_dialog = Some(ExportDialog::new(&file_name, sql, estimate));
            }
            Choice::Script(Script::Create) | Choice::Definition => {
                let kind = if choice == Choice::Definition {
                    kind
                } else {
                    kind_or_table(kind)
                };
                return self.explorer_command(Command::Definition { path: table, kind }, ctx);
            }
            Choice::Script(script) => {
                let info = info.unwrap_or_default();
                let text = if self.speaks_sql() {
                    table_menu::script(script, &quoted, &info, self.dialect)
                } else {
                    table_menu::shell_script(script, &quoted, &info)
                };
                let what = match (self.speaks_sql(), script) {
                    (true, script) => format!("{script:?}").to_uppercase(),
                    (false, Script::Select) => "find".into(),
                    (false, Script::Insert) => "insertOne".into(),
                    (false, Script::Update) => "updateOne".into(),
                    (false, _) => "deleteOne".into(),
                };
                let title = format!("{what} {}", table.name().unwrap_or_default());
                self.open_text(title, None, &text);
            }
            Choice::InsertName => {
                self.editor_mut().paste(&quoted);
            }
            Choice::Properties => {
                self.properties = Some(TableCard::new(table, kind, info.unwrap_or_default()));
            }
            Choice::Refresh => self.explorer.refresh_table(&table, ctx),
            Choice::Truncate if !self.speaks_sql() => {
                let command = format!("{}.deleteMany({{}})", mongo_handle(&quoted));
                return self.run(Some(command), ctx);
            }
            Choice::Drop { .. } if !self.speaks_sql() => {
                let command = format!("{}.drop()", mongo_handle(&quoted));
                return self.run(Some(command), ctx);
            }
            Choice::Truncate => {
                let sql = if self.dialect == Dialect::Sqlite {
                    format!("DELETE FROM {quoted}")
                } else {
                    format!("TRUNCATE TABLE {quoted}")
                };
                return self.run(Some(sql), ctx);
            }
            Choice::Drop { cascade } => {
                let what = match kind {
                    NodeKind::View => "VIEW",
                    NodeKind::MaterializedView => "MATERIALIZED VIEW",
                    NodeKind::ForeignTable => "FOREIGN TABLE",
                    _ => "TABLE",
                };
                let cascade = if cascade { " CASCADE" } else { "" };
                return self.run(Some(format!("DROP {what} {quoted}{cascade}")), ctx);
            }
        }
        None
    }

    /// The table's columns and keys arrived: a choice waiting for them goes
    /// on.
    pub fn on_described(
        &mut self,
        path: &NodePath,
        result: Result<EntityInfo, DbError>,
        ctx: &mut Ctx,
    ) -> Option<Command> {
        let failed = result.as_ref().err().map(ToString::to_string);
        self.explorer.on_described(path, result);
        let pending = self.pending.take_if(|p| p.table == *path)?;
        if let Some(err) = failed {
            ctx.toast(
                Level::Error,
                format!("Couldn't read {}: {err}", self.quote(&pending.name)),
            );
            return None;
        }
        self.choose(
            pending.table,
            pending.kind,
            pending.name,
            pending.choice,
            ctx,
        )
    }

    /// A table's rows were counted.
    /// Asks where to export the current tab's rows: the query is run again,
    /// whole, so only one that reads.
    fn export_results(&mut self, ctx: &mut Ctx) {
        let sql = self.results().query().trim().to_string();
        if sql.is_empty() {
            ctx.toast(Level::Info, "Run a query first, then export its rows");
            return;
        }
        let writes = match self.language {
            QueryLanguage::Sql(_) | QueryLanguage::Cql => guard::check(&sql, self.dialect).writes,
            QueryLanguage::MongoShell => sql::mongo::check(&sql).writes,
            QueryLanguage::RedisCmd => false,
        };
        if writes {
            ctx.toast(
                Level::Warning,
                "Exporting runs the query again: only a query that reads can be exported",
            );
            return;
        }
        let name = self
            .tab()
            .rows_of
            .as_ref()
            .and_then(|t| t.name())
            .unwrap_or("rows")
            .replace(['/', '\\', ':'], "_");
        self.export_dialog = Some(ExportDialog::new(&name, sql, None));
    }

    /// Starts writing `dialog`'s query to `file`.
    fn start_export(&mut self, dialog: ExportDialog, file: std::path::PathBuf, ctx: &mut Ctx) {
        if self.exporting.is_some() {
            ctx.toast(
                Level::Warning,
                "An export or import is running: wait for it, or cancel it",
            );
            return;
        }
        self.next_query += 1;
        let id = self.next_query;
        ctx.engine.send(match &dialog.import {
            Some(table) => DbCommand::Import {
                id,
                table: table.clone(),
                file: file.clone(),
                columns: dialog.columns(),
            },
            None => DbCommand::Export {
                id,
                sql: dialog.sql,
                file: file.clone(),
            },
        });
        self.exporting = Some(Exporting {
            id,
            file,
            import: dialog.import,
            copy_to: None,
            rows: 0,
            estimate: dialog.estimate,
            started: ctx.now,
        });
    }

    /// Starts copying `dialog`'s rows into `target`, a table of the saved
    /// connection `into`.
    pub fn start_copy(
        &mut self,
        dialog: CopyDialog,
        into: String,
        target: dfox_core::engine::CopyTarget,
        ctx: &mut Ctx,
    ) {
        if self.exporting.is_some() {
            ctx.toast(
                Level::Warning,
                "An export or import is running: wait for it, or cancel it",
            );
            return;
        }
        self.next_query += 1;
        let id = self.next_query;
        ctx.engine.send(DbCommand::Copy {
            id,
            sql: dialog.sql,
            target: Box::new(target),
        });
        self.exporting = Some(Exporting {
            id,
            file: std::path::PathBuf::from(&dialog.source),
            import: None,
            copy_to: Some(into),
            rows: 0,
            estimate: dialog.estimate,
            started: ctx.now,
        });
    }

    /// The export being written, for the status bar.
    pub fn exporting(&self) -> Option<&Exporting> {
        self.exporting.as_ref()
    }

    /// Edits of the table at `path` were saved, or not.
    /// Reloads saved rows once their effect has played.
    pub fn tick(&mut self, ctx: &mut Ctx) {
        let Some((at, tab)) = self.refresh_at else {
            return;
        };
        if ctx.now < at {
            return;
        }
        self.refresh_at = None;
        if tab == self.active {
            let sql = self.results().query().to_string();
            if !sql.is_empty() {
                self.run_checked(sql, ctx);
            }
        }
    }

    /// The saved edits answered. Returns where the saved rows are on screen,
    /// when they are, for their effect.
    pub fn on_applied(
        &mut self,
        path: &NodePath,
        result: Result<ApplyReport, DbError>,
        ctx: &mut Ctx,
    ) -> Option<Saved> {
        let i = self.tabs.iter().position(|t| {
            t.results
                .edits
                .as_ref()
                .is_some_and(|e| e.saving && e.table == *path)
        })?;
        let name = path.name().unwrap_or_default();
        match result {
            Ok(report) => {
                let mut done = Vec::new();
                for (n, what) in [(report.updated, "updated"), (report.deleted, "deleted")] {
                    if n > 0 {
                        done.push(format!("{n} {what}"));
                    }
                }
                let done = if done.is_empty() {
                    "no rows matched".to_string()
                } else {
                    done.join(", ")
                };
                ctx.toast(Level::Success, format!("Saved {name}: {done}"));
                let results = &mut self.tabs[i].results;
                let saved = results
                    .edits
                    .take()
                    .filter(|_| i == self.active)
                    .map(|edits| {
                        let grid = &results.grid;
                        Saved {
                            deleted: (edits.marks.deleted.iter())
                                .filter_map(|row| grid.area_of(*row, None))
                                .collect(),
                            changed: (edits.marks.cells.keys())
                                .filter(|(row, _)| !edits.marks.deleted.contains(row))
                                .filter_map(|(row, col)| grid.area_of(*row, Some(*col)))
                                .collect(),
                        }
                    });
                // Show the rows as they are now, after their effect.
                if i == self.active {
                    let wait = if ctx.config.animations {
                        SAVED_EFFECT
                    } else {
                        std::time::Duration::ZERO
                    };
                    self.refresh_at = Some((ctx.now + wait, i));
                    self.tick(ctx);
                }
                return saved;
            }
            Err(err) => {
                if let Some(edits) = &mut self.tabs[i].results.edits {
                    edits.saving = false;
                }
                ctx.toast(Level::Error, format!("Couldn't save {name}: {err}"));
            }
        }
        None
    }

    /// Keys that edit rows, with the results focused: the rows become
    /// editable first, when they are a table's rows with its key.
    fn edit_rows(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        if self.results().edits.is_none() && action != Action::Undo {
            if let Err(why) = self.editable() {
                ctx.toast(Level::Warning, why);
                return None;
            }
        }
        self.delegate(action, ctx)
    }

    /// Makes the current rows editable, or says why they can't be.
    fn editable(&mut self) -> Result<(), String> {
        if !self.can_edit {
            return Err("Rows of this database can't be edited here yet".into());
        }
        if self.read_only {
            return Err("This connection is read-only".into());
        }
        let Some(table) = self.tab().rows_of.clone() else {
            return Err(
                "Only a table's rows can be edited: open them from the table's menu".into(),
            );
        };
        let name = table.name().unwrap_or_default().to_string();
        let Some(info) = self.explorer.described(&table) else {
            return Err(format!("{name}'s columns are still loading"));
        };
        let key: Vec<String> = info
            .columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.clone())
            .collect();
        if key.is_empty() {
            return Err(format!("{name} has no primary key to find its rows by"));
        }
        self.tab_mut()
            .results
            .start_editing(table, &key)
            .map_err(|why| format!("Can't edit these rows: {why}"))
    }

    pub fn on_counted(&mut self, path: &NodePath, result: Result<u64, DbError>, ctx: &mut Ctx) {
        let name = path.name().unwrap_or_default();
        match result {
            Ok(rows) => {
                self.explorer.set_count(path, rows);
                ctx.toast(Level::Info, format!("{name} holds {rows} rows"));
            }
            Err(err) => ctx.toast(Level::Error, format!("Couldn't count {name}: {err}")),
        }
    }

    /// The table menu has the keys.
    fn menu_handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        let menu = self.menu.as_mut()?;
        let event = menu.handle(action)?;
        let menu = self.menu.take()?;
        match event {
            MenuEvent::Close => None,
            MenuEvent::Pick(choice) => {
                let command = self.choose(menu.table, menu.kind, menu.name, choice, ctx);
                // The new layout's explorer closes once a table is chosen.
                if ctx.config.layout == LayoutStyle::Workspace && self.focus.is(Panel::Explorer) {
                    self.set_focus(Panel::Editor);
                }
                command
            }
        }
    }

    /// A name in parts, quoted for the dialect; as it is where no SQL is
    /// spoken.
    fn quote(&self, name: &[String]) -> String {
        if self.language == QueryLanguage::RedisCmd {
            return sql::redis::quote(&name.concat());
        }
        if !self.speaks_sql() {
            return name.join(".");
        }
        name.iter()
            .map(|part| sql::quote_ident(part, self.dialect))
            .collect::<Vec<_>>()
            .join(".")
    }

    /// True unless the session speaks something other than SQL.
    fn speaks_sql(&self) -> bool {
        matches!(self.language, QueryLanguage::Sql(_) | QueryLanguage::Cql)
    }

    /// The query for some rows of the table named `name`, in the session's
    /// language.
    fn rows_query(&self, name: &[String], rows: Rows) -> String {
        if self.language == QueryLanguage::MongoShell {
            let collection = name.last().map_or("", String::as_str);
            let handle = mongo_handle(collection);
            return match rows {
                Rows::First => format!("{handle}.find().limit({FEW_ROWS})"),
                Rows::All => format!("{handle}.find()"),
                Rows::Last(key) => {
                    let sort: Vec<String> = key
                        .iter()
                        .map(|k| format!("{}: -1", mongo_key(k)))
                        .collect();
                    format!(
                        "{handle}.find().sort({{ {} }}).limit({FEW_ROWS})",
                        sort.join(", ")
                    )
                }
                Rows::Matching(filter) => format!("{handle}.find({filter})"),
            };
        }
        let quoted = self.quote(name);
        match rows {
            Rows::First => format!("SELECT * FROM {quoted} LIMIT {FEW_ROWS}"),
            Rows::All => format!("SELECT * FROM {quoted}"),
            Rows::Last(key) => {
                let order: Vec<String> = key
                    .iter()
                    .map(|k| format!("{} DESC", sql::quote_ident(k, self.dialect)))
                    .collect();
                format!(
                    "SELECT * FROM {quoted} ORDER BY {} LIMIT {FEW_ROWS}",
                    order.join(", ")
                )
            }
            // CQL filters outside the key only when asked to.
            Rows::Matching(condition) if self.language == QueryLanguage::Cql => {
                format!("SELECT * FROM {quoted} WHERE {condition} ALLOW FILTERING")
            }
            Rows::Matching(condition) => format!("SELECT * FROM {quoted} WHERE {condition}"),
        }
    }

    /// What the explorer asked for that the workspace does itself.
    pub fn explorer_command(&mut self, command: Command, ctx: &mut Ctx) -> Option<Command> {
        match command {
            Command::TableMenu { path, kind, name } => {
                self.explorer.describe(&path, ctx);
                self.menu = Some(TableMenu::new(
                    path,
                    kind,
                    name,
                    self.language,
                    self.read_only,
                ));
            }
            Command::InsertName(name) => {
                let text = self.quote(&name);
                self.editor_mut().paste(&text);
            }
            Command::ColumnCard { table, column } => {
                let card = self
                    .explorer
                    .described(&table)
                    .cloned()
                    .and_then(|info| ColumnCard::open(table, &column, &info, ctx));
                if card.is_none() {
                    ctx.toast(Level::Warning, format!("No column {column} to show"));
                }
                self.card = card;
            }
            Command::ShowSql { path, kind, text } => self.on_definition(path, kind, Ok(text), ctx),
            Command::Reveal(path) => {
                self.explorer.reveal(&path, ctx);
                self.set_focus(Panel::Explorer);
            }
            Command::Definition { path, kind } => match self.definition_tab(&path, kind) {
                Some(i) => {
                    self.active = i;
                    self.set_focus(Panel::Editor);
                }
                None => ctx.engine.send(DbCommand::Definition { path, kind }),
            },
            command => return Some(command),
        }
        None
    }

    fn delegate(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        match self.focus.get() {
            Panel::Explorer => {
                let command = self.explorer.handle(action, ctx)?;
                self.explorer_command(command, ctx)
            }
            Panel::Editor => self.tab_mut().editor.handle(action, ctx),
            Panel::Results => self.tab_mut().results.handle(action, ctx),
        }
    }

    /// Statistics for a column card arrived.
    pub fn on_column_stats(
        &mut self,
        table: &NodePath,
        column: &str,
        profile: bool,
        result: Result<Option<dfox_core::catalog::ColumnStats>, DbError>,
    ) {
        if let Some(card) = &mut self.card {
            card.on_stats(table, column, profile, result);
        }
    }

    /// The column card has the keys.
    fn card_handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        let card = self.card.as_mut()?;
        match card.handle(action, ctx)? {
            CardEvent::Close => self.card = None,
            CardEvent::Reveal(path) => {
                self.card = None;
                return self.explorer_command(Command::Reveal(path), ctx);
            }
        }
        None
    }

    /// The other connections, and a new one, last.
    fn others(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .saved
            .iter()
            .map(String::as_str)
            .filter(|name| *name != self.connection)
            .collect();
        names.push("New connection…");
        names
    }

    fn switch_to(&self, at: usize) -> Option<Command> {
        let others = self.others();
        match others.get(at) {
            Some(_) if at + 1 == others.len() => Some(Command::NewConnection),
            Some(name) => Some(Command::Connect(EntryKey::Saved(name.to_string()))),
            None => None,
        }
    }

    /// The keys while the drop-down's connections have them, or Down off
    /// the tree's last row onto them.
    fn switches_handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Option<Command>> {
        if let Some(at) = self.switching {
            match action {
                Action::Up => self.switching = at.checked_sub(1),
                Action::Down => self.switching = Some((at + 1).min(self.others().len() - 1)),
                Action::Confirm => return Some(self.switch_to(at)),
                Action::Back => self.switching = None,
                _ => {
                    self.switching = None;
                    return None;
                }
            }
            return Some(None);
        }
        if action != Action::Down {
            return None;
        }
        let before = self.explorer.selected();
        let command = self.delegate(action, ctx);
        if self.explorer.selected() == before {
            self.switching = Some(0);
        }
        Some(command)
    }

    fn render_switches(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let lines: Vec<Line> = self
            .others()
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                if self.switching == Some(i) {
                    Line::styled(format!("{} {name}", ctx.icons.selected), theme.selection())
                } else {
                    Line::raw(format!("  {name}"))
                }
            })
            .collect();
        let block = panel(
            vec![Span::raw("Connections")],
            self.switching.is_some(),
            ctx,
        );
        self.switches.set(block.inner(area));
        f.render_widget(Clear, area);
        f.render_widget(Paragraph::new(lines).block(block), area);
    }

    /// The focused panel has a menu or search open that takes every key.
    fn modal(&self) -> bool {
        if self.card.is_some()
            || self.menu.is_some()
            || self.checklist.is_some()
            || self.properties.is_some()
            || self.export_dialog.is_some()
        {
            return true;
        }
        match self.focus.get() {
            // Its filter takes Esc and Enter.
            Panel::Explorer => self.explorer.wants_text(),
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
        if let Some(list) = self.checklist.as_mut() {
            if event.kind == MouseEventKind::Down(MouseButton::Left) {
                if list.click(point) {
                    self.keep_explain_options();
                } else {
                    self.checklist = None;
                }
            }
            return None;
        }
        let switches = self.switches.get();
        if event.kind == MouseEventKind::Down(MouseButton::Left) && switches.contains(point) {
            return self.switch_to((event.row - switches.y) as usize);
        }
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
        let [editor_button, results_button] = self.buttons.get();
        let strip = self.strip.get();
        if event.kind == MouseEventKind::Down(MouseButton::Left) && !self.modal() {
            match self.rail.get().iter().position(|r| r.contains(point)) {
                Some(0) => return Component::handle(self, Action::Explorer, ctx),
                Some(1) => {
                    self.set_focus(Panel::Editor);
                    return None;
                }
                Some(_) => return Some(Command::Dispatch(Action::Connections)),
                None => {}
            }
        }
        let button = self
            .toolbar
            .borrow()
            .iter()
            .find(|(spot, _)| spot.contains(point))
            .map(|(_, button)| *button)
            .or_else(|| {
                self.results()
                    .data_toolbar
                    .borrow()
                    .iter()
                    .find(|(spot, _)| spot.contains(point))
                    .map(|(_, button)| *button)
            })
            .or_else(|| self.plus.get().contains(point).then_some(bar::NEW_TAB));
        if event.kind == MouseEventKind::Moved {
            self.hovered = button;
            self.tab_mut().results.hovered = button;
            return None;
        }
        if let (MouseEventKind::Down(MouseButton::Left), Some(button), false) =
            (event.kind, button, self.modal())
        {
            return match button.click {
                Click::Action(action) => {
                    match action {
                        _ if bar::DATA.contains(&button) => self.set_focus(Panel::Results),
                        Action::Format => self.set_focus(Panel::Editor),
                        Action::Filter | Action::Unfilter => self.set_focus(Panel::Results),
                        _ => {}
                    }
                    Some(Command::Dispatch(action))
                }
                Click::Sql(sql) => self.run(Some(sql.to_string()), ctx),
                Click::Limit => {
                    let config = ctx.config.row_limit;
                    let results = &mut self.tab_mut().results;
                    let now = results.row_limit.unwrap_or(config);
                    results.row_limit = Some(bar::next_limit(now, config));
                    None
                }
            };
        }
        // The database opens the explorer under it, and closes it.
        if event.kind == MouseEventKind::Down(MouseButton::Left)
            && self.database_spot.get().contains(point)
        {
            let to = if self.focus.is(Panel::Explorer) {
                Panel::Editor
            } else {
                Panel::Explorer
            };
            self.set_focus(to);
            return None;
        }
        let closing = (strip.contains(point) && !self.modal())
            .then(|| self.closes.borrow().iter().position(|x| *x == event.column))
            .flatten();
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if closing.is_some() => {
                return self.close_clicked(closing?, ctx);
            }
            MouseEventKind::Down(MouseButton::Left)
                if !self.modal()
                    && (editor_button.contains(point) || results_button.contains(point)) =>
            {
                let panel = if editor_button.contains(point) {
                    Panel::Editor
                } else {
                    Panel::Results
                };
                self.maximized = !(self.maximized && self.focus.is(panel));
                self.set_focus(panel);
            }
            MouseEventKind::Down(MouseButton::Left)
                if !self.modal() && self.tab_at(event.column, event.row).is_some() =>
            {
                if let Some(i) = self.tab_at(event.column, event.row) {
                    self.active = i;
                    self.set_focus(Panel::Editor);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let panel = under?;
                // The history has the keys, but `Query` still leads back.
                if panel == Panel::Editor
                    && self.editor().view_at(event.column, event.row) == Some(false)
                {
                    self.editor_mut().close_history();
                    self.set_focus(Panel::Editor);
                    return None;
                }
                if self.modal() {
                    return None;
                }
                self.set_focus(panel);
                match panel {
                    Panel::Explorer => return self.explorer.click(event.column, event.row, ctx),
                    Panel::Editor => match self.editor().view_at(event.column, event.row) {
                        Some(true) if !self.editor().is_modal() => {
                            return self.delegate(Action::History, ctx);
                        }
                        Some(false) => self.editor_mut().close_history(),
                        _ => {
                            self.editor_mut().click(event.column, event.row, sideways);
                        }
                    },
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
        if let Some(dialog) = &mut self.export_dialog {
            match dialog.handle(action) {
                Some(DialogEvent::Export(file)) => {
                    let dialog = self.export_dialog.take()?;
                    self.start_export(dialog, file, ctx);
                }
                Some(DialogEvent::Close) => self.export_dialog = None,
                None => {}
            }
            return None;
        }
        if self.card.is_some() {
            return self.card_handle(action, ctx);
        }
        if self.menu.is_some() {
            return self.menu_handle(action, ctx);
        }
        if self.checklist.is_some() {
            self.checklist_handle(action);
            return None;
        }
        if self.properties.is_some() {
            if TableCard::closes(action) {
                self.properties = None;
            }
            return None;
        }
        if self.modal() {
            return self.delegate(action, ctx);
        }
        if self.focus.is(Panel::Explorer) && ctx.config.layout == LayoutStyle::Workspace {
            if let Some(command) = self.switches_handle(action, ctx) {
                return command;
            }
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
            Action::NextFocus | Action::PrevFocus => {
                // The new layout's explorer opens from the bar, not by Tab.
                let skip = ctx.config.layout == LayoutStyle::Workspace;
                loop {
                    if action == Action::NextFocus {
                        self.focus.next();
                    } else {
                        self.focus.prev();
                    }
                    if !(skip && self.focus.is(Panel::Explorer)) {
                        break;
                    }
                }
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
            Action::Explain => return self.explain(false, ctx),
            Action::ExplainAnalyze => return self.explain(true, ctx),
            Action::ExplainOptions if self.buttons().contains(&bar::EXPLAIN_OPTIONS) => {
                self.open_explain_options();
            }
            Action::Complete if self.focus.is(Panel::Editor) => {
                let tab = &mut self.tabs[self.active];
                tab.editor.complete(&self.catalog, action);
                self.list_typed_keys(ctx);
            }
            Action::Export if self.focus.is(Panel::Results) && !self.results().is_modal() => {
                self.export_results(ctx);
            }
            Action::Edit | Action::AddRow | Action::Delete | Action::Save | Action::Undo
                if self.focus.is(Panel::Results) && !self.results().is_modal() =>
            {
                return self.edit_rows(action, ctx);
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
            Action::Explorer if self.focus.is(Panel::Explorer) => self.set_focus(Panel::Editor),
            Action::Explorer => self.set_focus(Panel::Explorer),
            // The new layout's explorer slides back.
            Action::Back
                if self.focus.is(Panel::Explorer)
                    && ctx.config.layout == LayoutStyle::Workspace =>
            {
                self.set_focus(Panel::Editor)
            }
            Action::Maximize => {
                if self.focus.is(Panel::Explorer) {
                    self.set_focus(Panel::Editor);
                    self.maximized = true;
                } else {
                    self.maximized = !self.maximized;
                }
            }
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
                    if let Some(label) = tab.editor.take_picked() {
                        self.catalog.picked(&label);
                    }
                    tab.editor.complete(&self.catalog, action);
                    self.list_typed_keys(ctx);
                }
                return command;
            }
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let none = Rect::default();
        let workspace = ctx.layout == LayoutStyle::Workspace;
        let [rail, area] = if workspace {
            Layout::horizontal([Constraint::Length(3), Constraint::Fill(1)]).areas(area)
        } else {
            [none, area]
        };
        let shown = Some(self.focus.get()).filter(|p| self.maximized && *p != Panel::Explorer);
        let [left, right] = if workspace || shown.is_some() {
            [none, area]
        } else {
            Layout::horizontal([Constraint::Percentage(28), Constraint::Fill(1)]).areas(area)
        };
        let [strip, bar, right] = if workspace {
            Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Fill(1),
            ])
            .areas(right)
        } else {
            [none, none, right]
        };
        let buttons = self.buttons();
        let limit = bar::limit_label(self.results().row_limit.unwrap_or(ctx.row_limit));
        let (spot, toolbar) = if workspace {
            Bar {
                tag: self.tag,
                driver: self.explorer.driver,
                connection: &self.connection,
                database: &self.explorer.active,
                read_only: self.read_only,
                in_transaction: self.in_transaction,
                buttons: &buttons,
                hovered: self.hovered,
                limit: &limit,
            }
            .render(f, bar, ctx)
        } else {
            (none, Vec::new())
        };
        self.toolbar.replace(toolbar);
        self.database_spot.set(spot);
        let [editor, results] = match shown {
            Some(Panel::Editor) => [right, none],
            Some(_) => [none, right],
            None => {
                Layout::vertical([Constraint::Percentage(35), Constraint::Fill(1)]).areas(right)
            }
        };
        // The new layout's explorer drops down from the database while
        // focused.
        let left = if workspace && self.focus.is(Panel::Explorer) {
            let width = (right.width * 2 / 5).max(40).min(right.width);
            let height = (right.height * 2 / 3).max(12).min(right.height);
            let x = spot
                .x
                .saturating_sub(1)
                .clamp(right.x, right.right().saturating_sub(width));
            Rect::new(x, right.y, width, height)
        } else {
            left
        };
        self.areas.set([left, editor, results]);
        self.strip.set(strip);
        let labels: Vec<TabLabel> = if workspace {
            self.render_strip(f, strip, ctx);
            vec![self.label(self.active)]
        } else {
            self.tab_spots.replace(self.tab_spots_in(editor));
            self.closes.replace(Vec::new());
            self.plus.set(Rect::default());
            (0..self.tabs.len()).map(|i| self.label(i)).collect()
        };
        if !editor.is_empty() {
            self.editor().render_with(f, editor, ctx, &labels);
        }
        if !results.is_empty() {
            self.results().render(f, results, ctx);
        }
        let mut button = |panel: Rect| {
            if ctx.layout != LayoutStyle::Workspace || panel.width < 12 {
                return none;
            }
            let spot = Rect::new(panel.right() - 4, panel.y, 3, 1);
            let text = format!(" {} ", ctx.icons.maximize);
            f.render_widget(Span::styled(text, ctx.theme.dim()), spot);
            spot
        };
        self.buttons.set([button(editor), button(results)]);
        self.switches.set(none);
        if !left.is_empty() {
            let tree = if workspace {
                let height = (self.others().len() as u16 + 2).min(left.height / 2);
                let [tree, switches] =
                    Layout::vertical([Constraint::Fill(1), Constraint::Length(height)]).areas(left);
                self.render_switches(f, switches, ctx);
                tree
            } else {
                left
            };
            f.render_widget(Clear, tree);
            self.explorer.render(f, tree, ctx);
        }
        self.render_rail(f, rail, ctx);
        self.editor().render_overlay(f, area, ctx);
        if let Some(menu) = &self.menu {
            menu.render(f, left, ctx);
        }
        if let Some(list) = &self.checklist {
            list.render(f, area, ctx);
        }
        if let Some(properties) = &self.properties {
            properties.render(f, area, ctx);
        }
        if let Some(card) = &self.card {
            card.render(f, area, ctx);
        }
        if let Some(dialog) = &self.export_dialog {
            dialog.render(f, area, ctx);
        }
    }

    fn is_animating(&self) -> bool {
        self.explorer.is_animating()
            || self.running().is_some()
            || self.exporting.is_some()
            || self.refresh_at.is_some()
            || self.card.as_ref().is_some_and(ColumnCard::is_loading)
    }

    fn wants_text(&self) -> bool {
        // Cards and menus take letters as keys.
        if self.card.is_some()
            || self.menu.is_some()
            || self.checklist.is_some()
            || self.properties.is_some()
            || self.export_dialog.is_some()
        {
            return true;
        }
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
