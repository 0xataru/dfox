//! The explorer: the session's catalog as a tree — databases, schemas,
//! tables, views and routines, columns — each level loaded the first time it
//! is opened. It knows no particular database: drivers say what each node
//! is. `/` filters loaded objects by fuzzy match.

mod filter;
mod render;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};

use dfox_core::{
    catalog::{ColumnInfo, EntityInfo, Node, NodeKind, NodePath},
    engine::Command as DbCommand,
    errors::DbError,
};
use nucleo_matcher::Matcher;
use tui_tree_widget::TreeState;

use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx},
    widgets::toast::Level,
};

/// Something loaded from the database on demand.
#[derive(Debug, Clone, PartialEq)]
pub enum Load<T> {
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(String),
}

impl<T> Load<T> {
    fn loaded(&self) -> Option<&T> {
        match self {
            Load::Loaded(value) => Some(value),
            _ => None,
        }
    }

    fn from_result(result: Result<T, DbError>) -> Self {
        result.map_or_else(|err| Load::Failed(err.to_string()), Load::Loaded)
    }

    /// True when opening should ask for it.
    fn wanted(&self) -> bool {
        matches!(self, Load::NotLoaded | Load::Failed(_))
    }
}

/// A node of the catalog and what has been loaded under it.
#[derive(Debug)]
struct Entry {
    node: Node,
    children: Load<Vec<Entry>>,
    /// For nodes with rows: their columns, from describing them.
    columns: Load<Vec<ColumnInfo>>,
}

impl Entry {
    fn new(node: Node) -> Self {
        Self {
            node,
            children: Load::NotLoaded,
            columns: Load::NotLoaded,
        }
    }

    /// The tree identifier: what it is and its name.
    fn id(&self) -> String {
        format!("{}/{}", tag(self.node.kind), self.node.name)
    }

    fn is_object(&self) -> bool {
        is_object(self.node.kind)
    }

    /// The one schema a database holds, shown in its place.
    fn only_schema(&self) -> Option<&Entry> {
        match self.children.loaded()?.as_slice() {
            [only] if only.node.kind == NodeKind::Schema => Some(only),
            _ => None,
        }
    }

    /// The entries shown under this one: a lone schema's, or its own.
    fn shown(&self) -> &Load<Vec<Entry>> {
        self.only_schema().map_or(&self.children, |s| &s.children)
    }
}

/// Things the explorer groups by kind under their schema or database.
fn is_object(kind: NodeKind) -> bool {
    kind.has_rows() || matches!(kind, NodeKind::Function | NodeKind::Procedure)
}

fn tag(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Database => "d",
        NodeKind::Schema | NodeKind::Keyspace => "s",
        NodeKind::Column => "c",
        kind if is_object(kind) => "o",
        _ => "n",
    }
}

/// Groups under a schema, in display order.
const GROUPS: [NodeKind; 7] = [
    NodeKind::Table,
    NodeKind::View,
    NodeKind::MaterializedView,
    NodeKind::ForeignTable,
    NodeKind::Collection,
    NodeKind::Function,
    NodeKind::Procedure,
];

fn group_id(kind: NodeKind) -> String {
    format!("g/{kind:?}")
}

/// One filter result: an object and the node it is under.
#[derive(Debug, Clone, PartialEq)]
struct Match {
    parent: NodePath,
    node: Node,
}

pub struct Explorer {
    root: Load<Vec<Entry>>,
    /// The database the session is connected to.
    pub active: String,
    state: RefCell<TreeState<String>>,
    /// The filter text while filtering; `None` shows the tree.
    filter: Option<String>,
    matches: Vec<Match>,
    selected_match: usize,
    matcher: RefCell<Matcher>,
    /// Being opened by itself after connecting: the active database, then
    /// its default schema, whose tables open once loaded.
    auto_open: Option<NodePath>,
    pub focused: bool,
    visible: Cell<usize>,
}

impl std::fmt::Debug for Explorer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Explorer")
            .field("active", &self.active)
            .field("filter", &self.filter)
            .finish_non_exhaustive()
    }
}

impl Explorer {
    pub fn new(active: &str) -> Self {
        Self {
            root: Load::NotLoaded,
            active: active.to_string(),
            state: RefCell::new(TreeState::default()),
            filter: None,
            matches: Vec::new(),
            selected_match: 0,
            matcher: RefCell::new(filter::matcher()),
            auto_open: None,
            focused: false,
            visible: Cell::new(0),
        }
    }

    /// Loads the tree from the top, opening the active database.
    pub fn load(&mut self, ctx: &mut Ctx) {
        self.root = Load::Loading;
        ctx.engine.send(DbCommand::Children {
            path: NodePath::root(),
        });
    }

    /// The session now uses `name`: forget what was loaded for the old one.
    pub fn set_active(&mut self, name: &str, ctx: &mut Ctx) {
        let old = std::mem::replace(&mut self.active, name.to_string());
        self.state
            .borrow_mut()
            .close(&[Entry::new(Node::new(NodeKind::Database, old, true)).id()]);
        if let Load::Loaded(databases) = &mut self.root {
            for db in databases.iter_mut() {
                db.children = Load::NotLoaded;
            }
        }
        let path = NodePath::root().child(name);
        self.auto_open = Some(path.clone());
        self.open_container(&path, ctx);
    }

    /// The entry at `path`.
    fn entry(&self, path: &NodePath) -> Option<&Entry> {
        let mut list: &[Entry] = self.root.loaded()?;
        let mut found = None;
        for name in path.parts() {
            let entry = list.iter().find(|e| &e.node.name == name)?;
            found = Some(entry);
            list = entry.children.loaded().map_or(&[], Vec::as_slice);
        }
        found
    }

    fn entry_mut(&mut self, path: &NodePath) -> Option<&mut Entry> {
        let Load::Loaded(list) = &mut self.root else {
            return None;
        };
        let mut list: &mut [Entry] = list;
        let (last, parents) = path.parts().split_last()?;
        for name in parents {
            let entry = list.iter_mut().find(|e| &e.node.name == name)?;
            list = match &mut entry.children {
                Load::Loaded(children) => children,
                _ => return None,
            };
        }
        list.iter_mut().find(|e| &e.node.name == last)
    }

    /// The tree identifiers of the node at `path`: groups added, a lone
    /// schema left out.
    fn ids(&self, path: &NodePath) -> Option<Vec<String>> {
        let mut ids = Vec::new();
        let mut list = self.root.loaded()?.as_slice();
        let mut parts = path.parts().iter().peekable();
        while let Some(name) = parts.next() {
            let entry = list.iter().find(|e| &e.node.name == name)?;
            if entry.is_object() {
                ids.push(group_id(entry.node.kind));
            }
            ids.push(entry.id());
            if let Some(schema) = entry.only_schema() {
                // The lone schema is shown in the database's place.
                if parts.peek() == Some(&&schema.node.name) {
                    parts.next();
                }
            }
            list = match entry.shown().loaded() {
                Some(children) => children,
                None => &[],
            };
        }
        Some(ids)
    }

    /// The node path and entry of the tree node with identifiers `ids`.
    /// Groups and columns have no entry of their own.
    fn resolve(&self, ids: &[String]) -> Option<(NodePath, &Entry)> {
        let mut path = NodePath::root();
        let mut list = self.root.loaded()?.as_slice();
        let mut found = None;
        for id in ids {
            if id.starts_with("g/") {
                continue;
            }
            let entry = list.iter().find(|e| &e.id() == id)?;
            path = path.child(entry.node.name.clone());
            found = Some(entry);
            if let Some(schema) = entry.only_schema() {
                path = path.child(schema.node.name.clone());
                found = Some(schema);
            }
            list = match found?.children.loaded() {
                Some(children) => children,
                None => &[],
            };
        }
        found.map(|entry| (path, entry))
    }

    /// Selects the tree node with identifiers `ids`.
    #[cfg(test)]
    pub fn select(&mut self, ids: &[String]) {
        self.state.borrow_mut().select(ids.to_vec());
    }

    /// Asks for the children of the container at `path`, unless they are
    /// loaded or loading.
    fn open_container(&mut self, path: &NodePath, ctx: &mut Ctx) {
        let Some(entry) = self.entry_mut(path) else {
            return;
        };
        if entry.children.wanted() {
            entry.children = Load::Loading;
            ctx.engine.send(DbCommand::Children { path: path.clone() });
        }
        if let Some(ids) = self.ids(path) {
            let mut state = self.state.borrow_mut();
            state.open(ids.clone());
            if state.selected().is_empty() {
                state.select(ids);
            }
        }
    }

    /// Children of `path` arrived.
    pub fn on_children(
        &mut self,
        path: &NodePath,
        result: Result<Vec<Node>, DbError>,
        ctx: &mut Ctx,
    ) {
        if path.parts().is_empty() {
            self.on_databases(result, ctx);
        } else if let Some(entry) = self.entry_mut(path) {
            // Keep what was loaded under nodes that are still there.
            let mut old = match std::mem::replace(&mut entry.children, Load::NotLoaded) {
                Load::Loaded(entries) => entries,
                _ => Vec::new(),
            };
            entry.children = Load::from_result(result.map(|nodes| {
                nodes
                    .into_iter()
                    .map(|node| {
                        match old
                            .iter()
                            .position(|e| e.node.name == node.name && e.node.kind == node.kind)
                        {
                            Some(i) => Entry {
                                node,
                                ..old.swap_remove(i)
                            },
                            None => Entry::new(node),
                        }
                    })
                    .collect()
            }));
            if self.auto_open.as_ref() == Some(path) {
                self.continue_auto_open(path, ctx);
            }
        }
        if let Some(filter) = self.filter.clone() {
            self.update_matches(&filter);
        }
    }

    fn on_databases(&mut self, result: Result<Vec<Node>, DbError>, ctx: &mut Ctx) {
        let mut nodes = match result {
            Ok(nodes) => nodes,
            Err(err) => {
                ctx.toast(Level::Error, format!("Couldn't list databases: {err}"));
                Vec::new()
            }
        };
        // A user who can't list databases still has the one they're using.
        if !nodes.iter().any(|n| n.name == self.active) {
            nodes.insert(0, Node::new(NodeKind::Database, self.active.clone(), true));
        }
        self.root = Load::Loaded(nodes.into_iter().map(Entry::new).collect());
        let active = NodePath::root().child(self.active.clone());
        self.auto_open = Some(active.clone());
        self.open_container(&active, ctx);
    }

    /// The default schema's tables open by themselves after connecting.
    fn continue_auto_open(&mut self, path: &NodePath, ctx: &mut Ctx) {
        let Some(entry) = self.entry(path) else {
            return;
        };
        let Some(children) = entry.children.loaded() else {
            self.auto_open = None;
            return;
        };
        let schemas: Vec<&str> = children
            .iter()
            .filter(|e| matches!(e.node.kind, NodeKind::Schema | NodeKind::Keyspace))
            .map(|e| e.node.name.as_str())
            .collect();
        if !schemas.is_empty() && schemas.len() == children.len() {
            // Open `public`, or the first schema.
            let name = schemas
                .iter()
                .find(|s| **s == "public")
                .unwrap_or(&schemas[0])
                .to_string();
            let schema = path.child(name);
            self.auto_open = Some(schema.clone());
            self.open_container(&schema, ctx);
            return;
        }
        self.auto_open = None;
        if let Some(mut ids) = self.ids(path) {
            ids.push(group_id(NodeKind::Table));
            self.state.borrow_mut().open(ids);
        }
    }

    pub fn on_described(&mut self, path: &NodePath, result: Result<EntityInfo, DbError>) {
        if let Some(entry) = self.entry_mut(path) {
            entry.columns = Load::from_result(result.map(|info| info.columns));
        }
    }

    /// Reloads every loaded list of objects, after a query that may have
    /// changed them.
    pub fn refresh_loaded(&mut self, ctx: &mut Ctx) {
        fn walk(list: &[Entry], path: &NodePath, out: &mut Vec<NodePath>) {
            for entry in list {
                let Some(children) = entry.children.loaded() else {
                    continue;
                };
                let here = path.child(entry.node.name.clone());
                if children.iter().any(Entry::is_object) {
                    out.push(here.clone());
                }
                walk(children, &here, out);
            }
        }
        let mut paths = Vec::new();
        if let Some(list) = self.root.loaded() {
            walk(list, &NodePath::root(), &mut paths);
        }
        for path in paths {
            ctx.engine.send(DbCommand::Children { path });
        }
    }

    /// Selects the node drawn at a point on screen; a second click on it opens
    /// or closes it.
    pub fn click(&mut self, column: u16, row: u16, ctx: &mut Ctx) -> Option<Command> {
        if self.filter.is_some() {
            return None;
        }
        let before = self.state.borrow().selected().to_vec();
        let clicked = self
            .state
            .borrow_mut()
            .click_at(ratatui::layout::Position::new(column, row));
        let selected = self.state.borrow().selected().to_vec();
        // `click_at` toggled the node already; load what it needs.
        if clicked && selected == before && self.state.borrow().opened().contains(&selected) {
            self.state.borrow_mut().close(&selected);
            return self.open_selected(false, ctx);
        }
        None
    }

    /// Enter: a table's rows, or else opening or closing the node.
    fn confirm(&mut self, ctx: &mut Ctx) -> Option<Command> {
        let ids = self.state.borrow().selected().to_vec();
        let on_object = ids.last().is_some_and(|id| id.starts_with("o/"));
        if let Some((path, entry)) = self.resolve(&ids).filter(|_| on_object) {
            if entry.node.kind.has_rows() {
                let name = self.sql_name(&path);
                return Some(Command::Browse { path, name });
            }
        }
        self.open_selected(true, ctx)
    }

    /// How SQL names the selected node, in parts to quote: a column by its
    /// own name, an object without the database and schema it is in when
    /// those are the ones in use.
    fn selected_name(&self) -> Option<Vec<String>> {
        let ids = self.state.borrow().selected().to_vec();
        let last = ids.last()?;
        if let Some(column) = last.strip_prefix("c/") {
            return Some(vec![column.to_string()]);
        }
        if last.starts_with("g/") {
            return None;
        }
        let (path, entry) = self.resolve(&ids)?;
        Some(match entry.node.kind {
            kind if is_object(kind) => self.sql_name(&path),
            _ => vec![entry.node.name.clone()],
        })
    }

    /// The SQL name of the object at `path`.
    fn sql_name(&self, path: &NodePath) -> Vec<String> {
        match path.parts() {
            [database, object] if *database == self.active => vec![object.clone()],
            [database, schema, object] => {
                let only = self
                    .entry(&NodePath::root().child(database.clone()))
                    .and_then(Entry::only_schema)
                    .is_some();
                if only || schema == "public" {
                    vec![object.clone()]
                } else {
                    vec![schema.clone(), object.clone()]
                }
            }
            parts => parts.to_vec(),
        }
    }

    /// Opens or closes the selected node, loading what it needs.
    fn open_selected(&mut self, toggle: bool, ctx: &mut Ctx) -> Option<Command> {
        let ids = self.state.borrow().selected().to_vec();
        let resolved = self
            .resolve(&ids)
            .map(|(path, entry)| (path, entry.node.clone()));
        if let Some((_, node)) = &resolved {
            // A database the session can't browse: switch to it.
            if node.kind == NodeKind::Database && node.name != self.active && !node.has_children {
                return Some(Command::UseDatabase(node.name.clone()));
            }
        }
        let opened = {
            let mut state = self.state.borrow_mut();
            if toggle {
                state.toggle(ids.clone());
            } else {
                state.open(ids.clone());
            }
            state.opened().contains(&ids)
        };
        // Groups and columns load nothing; neither does closing.
        let (true, Some((path, node))) = (opened, resolved) else {
            return None;
        };
        if ids.last().is_some_and(|id| id.starts_with("g/")) {
            return None;
        }
        if node.kind.has_rows() {
            if let Some(entry) = self.entry_mut(&path) {
                if entry.columns.wanted() {
                    entry.columns = Load::Loading;
                    ctx.engine.send(DbCommand::Describe { path });
                }
            }
        } else if node.has_children && !is_object(node.kind) {
            self.open_container(&path, ctx);
        }
        None
    }
}

impl Component for Explorer {
    fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        if self.filter.is_some() {
            self.handle_filter(action);
            return None;
        }
        let page = self.visible.get().max(1);
        let mut state = self.state.borrow_mut();
        match action {
            Action::Up => {
                state.key_up();
            }
            Action::Down => {
                state.key_down();
            }
            Action::PageUp => {
                state.select_relative(|i| i.map_or(0, |i| i.saturating_sub(page)));
            }
            Action::PageDown => {
                state.select_relative(|i| i.map_or(0, |i| i + page));
            }
            Action::Home => {
                state.select_first();
            }
            Action::End => {
                state.select_last();
            }
            Action::Left => {
                state.key_left();
            }
            Action::Right => {
                drop(state);
                return self.open_selected(false, ctx);
            }
            Action::Confirm => {
                drop(state);
                return self.confirm(ctx);
            }
            Action::Inspect => {
                drop(state);
                return self.selected_name().map(Command::InsertName);
            }
            Action::Filter => {
                drop(state);
                self.filter = Some(String::new());
                self.update_matches("");
            }
            Action::Refresh => {
                drop(state);
                self.load(ctx);
            }
            _ => {}
        }
        None
    }

    fn render(
        &self,
        f: &mut ratatui::Frame,
        area: ratatui::layout::Rect,
        ctx: &crate::component::RenderCtx,
    ) {
        render::render(self, f, area, ctx);
    }

    fn is_animating(&self) -> bool {
        render::is_loading(self)
    }

    fn wants_text(&self) -> bool {
        self.filter.is_some()
    }
}
