//! The main screen once a database is open: the explorer, and query tabs of
//! an editor with its results. Tab moves focus between the panels.

use std::cell::{Cell, RefCell};

use unicode_width::UnicodeWidthStr;

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use dfox_core::{
    catalog::{EntityInfo, Node, NodeKind, NodePath},
    engine::{Command as DbCommand, EngineEvent, QueryId},
    errors::DbError,
    models::connections::DbType,
    session::QueryLanguage,
};
use ratatui::{
    layout::{Constraint, Layout, Position, Rect},
    Frame,
};

use super::{
    card::{CardEvent, ColumnCard, TableCard},
    editor::{EditorPanel, TabLabel},
    explorer::Explorer,
    results::ResultsPanel,
    table_menu::{self, mongo_handle, mongo_key, Choice, MenuEvent, Script, TableMenu, FEW_ROWS},
};
use crate::{
    action::Action,
    catalog::Catalog,
    command::Command,
    component::{Component, Ctx, Focus, RenderCtx},
    history::Entry,
    sql::{self, guard, Dialect},
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
    /// The view or routine whose definition it was opened with.
    source: Option<(NodePath, NodeKind)>,
    /// Its name, when it was opened with a definition, script or a
    /// table's rows.
    title: Option<String>,
    /// The table whose rows it was opened to show.
    rows_of: Option<NodePath>,
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
    /// The columns each tab's name was drawn across, on the editor's top
    /// border, for clicks.
    tab_spots: RefCell<Vec<(u16, u16)>>,
    /// The column card, over everything while it is open.
    card: Option<ColumnCard>,
    /// What Enter on a table offers, while it is open.
    menu: Option<TableMenu>,
    /// A table's properties, while they are shown.
    properties: Option<TableCard>,
    pending: Option<Pending>,
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
            connection: connection.to_string(),
            next_query: 0,
            next_tab: 0,
            history_failed: false,
            read_only: false,
            catalog: Catalog::default(),
            areas: Cell::new([Rect::default(); 3]),
            tab_spots: RefCell::new(Vec::new()),
            card: None,
            menu: None,
            properties: None,
            pending: None,
        };
        workspace.new_tab();
        workspace.set_focus(Panel::Editor);
        workspace
    }

    /// The language the session reported. SQL starts the first tab over in
    /// its dialect.
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
            if !tables.is_empty() {
                self.catalog.on_tables(name, tables);
                ctx.engine
                    .send(DbCommand::AllColumns { path: path.clone() });
            }
        }
        self.explorer.on_children(path, result, ctx);
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
            editor: EditorPanel::new(self.dialect).with_language(self.language),
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
        let findings = match self.language {
            QueryLanguage::Sql(_) | QueryLanguage::Cql => guard::check(&sql, self.dialect),
            QueryLanguage::MongoShell => sql::mongo::check(&sql),
            // Only flags what wipes a database: the driver itself refuses
            // writes on a read-only connection.
            QueryLanguage::RedisCmd => sql::redis::check(&sql),
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
        if self.tabs.len() < 2 {
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
        if row != editor.y {
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
                if self.results_for(id).is_some() {
                    ctx.toast(Level::Info, message);
                }
            }
            EngineEvent::QueryDone { id, outcome } => {
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
                    let key_type = self.explorer.detail(&table).unwrap_or_default();
                    let command = sql::redis::open_command(key_type, &key, first);
                    return self.show_rows(table, command, ctx);
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
            MenuEvent::Pick(choice) => self.choose(menu.table, menu.kind, menu.name, choice, ctx),
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

    /// The focused panel has a menu or search open that takes every key.
    fn modal(&self) -> bool {
        if self.card.is_some() || self.menu.is_some() || self.properties.is_some() {
            return true;
        }
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
        if self.card.is_some() {
            return self.card_handle(action, ctx);
        }
        if self.menu.is_some() {
            return self.menu_handle(action, ctx);
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
        self.tab_spots.replace(self.tab_spots_in(editor));
        let labels: Vec<TabLabel> = (0..self.tabs.len()).map(|i| self.label(i)).collect();
        self.explorer.render(f, left, ctx);
        self.editor().render_with(f, editor, ctx, &labels);
        self.results().render(f, results, ctx);
        self.editor().render_overlay(f, area, ctx);
        if let Some(menu) = &self.menu {
            menu.render(f, left, ctx);
        }
        if let Some(properties) = &self.properties {
            properties.render(f, area, ctx);
        }
        if let Some(card) = &self.card {
            card.render(f, area, ctx);
        }
    }

    fn is_animating(&self) -> bool {
        self.explorer.is_animating()
            || self.running().is_some()
            || self.card.as_ref().is_some_and(ColumnCard::is_loading)
    }

    fn wants_text(&self) -> bool {
        // Cards and menus take letters as keys.
        if self.card.is_some() || self.menu.is_some() || self.properties.is_some() {
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
