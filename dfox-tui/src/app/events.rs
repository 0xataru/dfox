//! Engine answers, applied to the screens.

use dfox_core::engine::{Command as DbCommand, EngineEvent};
use tokio::time::Instant;

use super::{describe, App, Connecting, Screen};
use crate::{
    component::Ctx,
    components::{databases::DatabasesScreen, workspace::Workspace},
};

impl App {
    /// Updates the screens with an answer from the engine.
    pub fn apply(&mut self, event: EngineEvent) {
        let mut ctx = Ctx {
            engine: &self.engine,
            config: &self.config,
            toasts: &mut self.toasts,
            log: &mut self.log,
            now: Instant::now(),
        };
        match event {
            EngineEvent::Connected(result) => match (self.connecting.take(), result) {
                (Some(Connecting::Server(db_type, params)), Ok(())) => {
                    ctx.log.push(format!("Connected to {}", describe(&params)));
                    self.connect.connecting = false;
                    self.server = Some((db_type, params));
                    self.databases = DatabasesScreen::loading();
                    self.screen = Screen::Databases;
                    self.engine.send(DbCommand::ListDatabases);
                }
                (Some(Connecting::Database(name)), Ok(())) => {
                    ctx.log.push(format!("Opened {name}"));
                    self.databases.opening = None;
                    self.database = Some(name);
                    self.workspace = Workspace::default();
                    self.workspace.tables.refresh(&mut ctx);
                    self.screen = Screen::Workspace;
                }
                (Some(Connecting::Server(..)), Err(err)) => {
                    ctx.log.push(format!("Connect failed: {err}"));
                    self.connect.connecting = false;
                    self.connect.error = Some(err.to_string());
                }
                (Some(Connecting::Database(_)), Err(err)) => {
                    ctx.log.push(format!("Open failed: {err}"));
                    self.databases.opening = None;
                    self.databases.error = Some(err.to_string());
                }
                (None, _) => {}
            },
            EngineEvent::Databases(result) => match result {
                Ok(databases) => self.databases.set_databases(databases),
                Err(err) => {
                    self.databases.loading = false;
                    self.databases.error = Some(format!("Couldn't list databases: {err}"));
                }
            },
            EngineEvent::Tables(result) => self.workspace.tables.on_tables(result, &mut ctx),
            EngineEvent::Schema(result) => self.workspace.tables.on_schema(result, &mut ctx),
            EngineEvent::Columns { id, columns } => self.workspace.results.on_columns(id, columns),
            EngineEvent::Rows { id, rows } => self.workspace.results.on_rows(id, rows),
            EngineEvent::QueryDone { id, outcome } => {
                // The query may have created or dropped tables.
                if self.workspace.results.on_done(id, outcome, &mut ctx) {
                    self.workspace.tables.refresh(&mut ctx);
                }
            }
        }
    }
}
