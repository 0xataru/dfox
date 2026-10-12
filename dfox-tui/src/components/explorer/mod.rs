//! The explorer: the server's databases, then schemas, then tables, views and
//! routines, each loaded the first time it is opened. `/` filters loaded
//! objects by fuzzy match.

mod filter;
mod render;
#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};

use dfox_core::{
    engine::Command as DbCommand,
    errors::DbError,
    models::schema::{ColumnSchema, DbObject, ObjectKind, TableSchema},
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
}

#[derive(Debug)]
struct Database {
    name: String,
    schemas: Load<Vec<Schema>>,
}

#[derive(Debug)]
struct Schema {
    name: String,
    objects: Load<Vec<Object>>,
}

#[derive(Debug)]
struct Object {
    object: DbObject,
    columns: Load<Vec<ColumnSchema>>,
}

/// A node of the tree, found from its path of identifiers.
#[derive(Debug, Clone, PartialEq)]
enum Node {
    Database(String),
    Schema(String),
    Group,
    Object {
        schema: String,
        name: String,
        kind: ObjectKind,
    },
    Column,
}

/// One filter result.
#[derive(Debug, Clone, PartialEq)]
struct Match {
    schema: String,
    object: DbObject,
}

/// Groups under a schema, in display order.
const GROUPS: [ObjectKind; 6] = [
    ObjectKind::Table,
    ObjectKind::View,
    ObjectKind::MaterializedView,
    ObjectKind::ForeignTable,
    ObjectKind::Function,
    ObjectKind::Procedure,
];

pub struct Explorer {
    databases: Load<Vec<Database>>,
    /// The database the session is connected to. Only it can be opened.
    pub active: String,
    state: RefCell<TreeState<String>>,
    /// The filter text while filtering; `None` shows the tree.
    filter: Option<String>,
    matches: Vec<Match>,
    selected_match: usize,
    matcher: RefCell<Matcher>,
    /// The schema whose tables open by themselves once loaded.
    open_tables_of: Option<String>,
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
            databases: Load::NotLoaded,
            active: active.to_string(),
            state: RefCell::new(TreeState::default()),
            filter: None,
            matches: Vec::new(),
            selected_match: 0,
            matcher: RefCell::new(filter::matcher()),
            open_tables_of: None,
            focused: false,
            visible: Cell::new(0),
        }
    }

    fn db_id(name: &str) -> String {
        format!("d/{name}")
    }

    fn active_db(&mut self) -> Option<&mut Database> {
        let active = self.active.clone();
        match &mut self.databases {
            Load::Loaded(databases) => databases.iter_mut().find(|db| db.name == active),
            _ => None,
        }
    }

    /// Loads the database list and the active database's schemas.
    pub fn load(&mut self, ctx: &mut Ctx) {
        self.databases = Load::Loading;
        ctx.engine.send(DbCommand::ListDatabases);
    }

    /// The session now uses `name`: forget what was loaded for the old one.
    pub fn set_active(&mut self, name: &str, ctx: &mut Ctx) {
        let old = std::mem::replace(&mut self.active, name.to_string());
        self.state.borrow_mut().close(&[Self::db_id(&old)]);
        if let Load::Loaded(databases) = &mut self.databases {
            for db in databases.iter_mut() {
                db.schemas = Load::NotLoaded;
            }
        }
        self.load_schemas(ctx);
    }

    fn load_schemas(&mut self, ctx: &mut Ctx) {
        let id = Self::db_id(&self.active);
        if let Some(db) = self.active_db() {
            db.schemas = Load::Loading;
            ctx.engine.send(DbCommand::ListSchemas);
            let mut state = self.state.borrow_mut();
            state.open(vec![id.clone()]);
            if state.selected().is_empty() {
                state.select(vec![id]);
            }
        }
    }

    /// Reloads the objects of every schema that was loaded, after a query
    /// that may have changed them.
    pub fn refresh_loaded(&mut self, ctx: &mut Ctx) {
        if let Some(Load::Loaded(schemas)) = self.active_db().map(|db| &mut db.schemas) {
            for schema in schemas.iter_mut() {
                if matches!(schema.objects, Load::Loaded(_)) {
                    ctx.engine.send(DbCommand::ListObjects {
                        schema: schema.name.clone(),
                    });
                }
            }
        }
    }

    pub fn on_databases(&mut self, result: Result<Vec<String>, DbError>, ctx: &mut Ctx) {
        let mut names = match result {
            Ok(names) => names,
            Err(err) => {
                ctx.toast(Level::Error, format!("Couldn't list databases: {err}"));
                Vec::new()
            }
        };
        // A user who can't list databases still has the one they're using.
        if !names.contains(&self.active) {
            names.insert(0, self.active.clone());
        }
        self.databases = Load::Loaded(
            names
                .into_iter()
                .map(|name| Database {
                    name,
                    schemas: Load::NotLoaded,
                })
                .collect(),
        );
        self.load_schemas(ctx);
    }

    pub fn on_schemas(&mut self, result: Result<Vec<String>, DbError>, ctx: &mut Ctx) {
        let db_id = Self::db_id(&self.active);
        let Some(db) = self.active_db() else {
            return;
        };
        db.schemas = Load::from_result(result.map(|names| {
            names
                .into_iter()
                .map(|name| Schema {
                    name,
                    objects: Load::NotLoaded,
                })
                .collect()
        }));
        // Open the only schema, or `public`, or the first.
        let Some(schemas) = db.schemas.loaded() else {
            return;
        };
        let Some(first) = schemas
            .iter()
            .find(|s| schemas.len() == 1 || s.name == "public")
            .or(schemas.first())
            .map(|s| s.name.clone())
        else {
            return;
        };
        let flattened = schemas.len() == 1;
        self.open_tables_of = Some(first.clone());
        self.open_schema(&first, ctx);
        if !flattened {
            self.state
                .borrow_mut()
                .open(vec![db_id, format!("s/{first}")]);
        }
    }

    fn open_schema(&mut self, name: &str, ctx: &mut Ctx) {
        if let Some(Load::Loaded(schemas)) = self.active_db().map(|db| &mut db.schemas) {
            if let Some(schema) = schemas.iter_mut().find(|s| s.name == name) {
                if matches!(schema.objects, Load::NotLoaded | Load::Failed(_)) {
                    schema.objects = Load::Loading;
                    ctx.engine.send(DbCommand::ListObjects {
                        schema: name.to_string(),
                    });
                }
            }
        }
    }

    pub fn on_objects(&mut self, schema: &str, result: Result<Vec<DbObject>, DbError>) {
        let Some(Load::Loaded(schemas)) = self.active_db().map(|db| &mut db.schemas) else {
            return;
        };
        let Some(node) = schemas.iter_mut().find(|s| s.name == schema) else {
            return;
        };
        // Keep columns already loaded for objects that are still there.
        let mut old = match std::mem::replace(&mut node.objects, Load::NotLoaded) {
            Load::Loaded(objects) => objects,
            _ => Vec::new(),
        };
        node.objects = Load::from_result(result.map(|objects| {
            objects
                .into_iter()
                .map(|object| {
                    let columns = old
                        .iter()
                        .position(|o| o.object.name == object.name && o.object.kind == object.kind)
                        .map(|i| old.swap_remove(i).columns)
                        .unwrap_or(Load::NotLoaded);
                    Object { object, columns }
                })
                .collect()
        }));
        if self.open_tables_of.as_deref() == Some(schema) {
            self.open_tables_of = None;
            let mut path = vec![Self::db_id(&self.active)];
            if self.only_schema().is_none() {
                path.push(format!("s/{schema}"));
            }
            path.push(format!("g/{:?}", ObjectKind::Table));
            self.state.borrow_mut().open(path);
        }
        if let Some(filter) = self.filter.clone() {
            self.update_matches(&filter);
        }
    }

    pub fn on_described(
        &mut self,
        schema: &str,
        table: &str,
        result: Result<TableSchema, DbError>,
    ) {
        if let Some(object) = self.object_mut(schema, table) {
            object.columns = Load::from_result(result.map(|t| t.columns));
        }
    }

    fn object_mut(&mut self, schema: &str, name: &str) -> Option<&mut Object> {
        let Some(Load::Loaded(schemas)) = self.active_db().map(|db| &mut db.schemas) else {
            return None;
        };
        let schema = schemas.iter_mut().find(|s| s.name == schema)?;
        match &mut schema.objects {
            Load::Loaded(objects) => objects
                .iter_mut()
                .find(|o| o.object.name == name && o.object.kind.has_rows()),
            _ => None,
        }
    }

    /// The node at `path`, decoded from its identifiers.
    fn node(&self, path: &[String]) -> Option<Node> {
        let last = path.last()?;
        let (kind, name) = last.split_once('/')?;
        Some(match kind {
            "d" => Node::Database(name.to_string()),
            "s" => Node::Schema(name.to_string()),
            "g" => Node::Group,
            "c" => Node::Column,
            "o" => {
                let schema = match path.iter().find_map(|id| id.strip_prefix("s/")) {
                    Some(schema) => schema.to_string(),
                    // A flattened tree has one schema and no schema level.
                    None => self.only_schema()?,
                };
                let group = path.iter().rev().find_map(|id| id.strip_prefix("g/"))?;
                let kind = GROUPS.into_iter().find(|k| format!("{k:?}") == group)?;
                Node::Object {
                    schema,
                    name: name.to_string(),
                    kind,
                }
            }
            _ => return None,
        })
    }

    fn only_schema(&self) -> Option<String> {
        let db = self
            .databases
            .loaded()?
            .iter()
            .find(|db| db.name == self.active)?;
        match db.schemas.loaded()?.as_slice() {
            [only] => Some(only.name.clone()),
            _ => None,
        }
    }

    /// Opens or closes the selected node, loading what it needs.
    fn open_selected(&mut self, toggle: bool, ctx: &mut Ctx) -> Option<Command> {
        let path = self.state.borrow().selected().to_vec();
        let node = self.node(&path)?;
        if let Node::Database(name) = &node {
            if *name != self.active {
                return Some(Command::UseDatabase(name.clone()));
            }
        }
        let opened = {
            let mut state = self.state.borrow_mut();
            if toggle {
                state.toggle(path.clone());
            } else {
                state.open(path.clone());
            }
            state.opened().contains(&path)
        };
        if !opened {
            return None;
        }
        match node {
            Node::Database(_) => {
                if self
                    .active_db()
                    .is_some_and(|db| matches!(db.schemas, Load::NotLoaded | Load::Failed(_)))
                {
                    self.load_schemas(ctx);
                }
            }
            Node::Schema(name) => self.open_schema(&name, ctx),
            Node::Object { schema, name, kind } if kind.has_rows() => {
                if let Some(object) = self.object_mut(&schema, &name) {
                    if matches!(object.columns, Load::NotLoaded | Load::Failed(_)) {
                        object.columns = Load::Loading;
                        ctx.engine.send(DbCommand::Describe {
                            schema,
                            table: name,
                        });
                    }
                }
            }
            _ => {}
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
                return self.open_selected(true, ctx);
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
