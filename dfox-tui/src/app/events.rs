//! Engine answers, applied to the screens.

use dfox_core::{engine::EngineEvent, models::connections::DbType};

use super::{App, Screen, Session};
use crate::{components::workspace::Workspace, widgets::toast::Level};

impl App {
    /// Updates the screens with an answer from the engine.
    pub fn apply(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Connected(result) => {
                let Some(connecting) = self.connecting.take() else {
                    return;
                };
                let name = connecting.connection.name.clone();
                match result {
                    Ok(()) => self.connected(connecting),
                    Err(err) => self.with_ctx(|_, ctx| {
                        ctx.toast(Level::Error, format!("Couldn't connect to {name}: {err}"))
                    }),
                }
            }
            EngineEvent::Pong { id, result } => self.manager.on_pong(id, result),
            EngineEvent::Databases(result) => self.with_ctx(|app, ctx| {
                if let Some(w) = app.workspace {
                    w.explorer.on_databases(result, ctx);
                }
            }),
            EngineEvent::Schemas(result) => self.with_ctx(|app, ctx| {
                if let Some(w) = app.workspace {
                    w.explorer.on_schemas(result, ctx);
                }
            }),
            EngineEvent::Objects { schema, result } => {
                if let Some(w) = &mut self.workspace {
                    w.explorer.on_objects(&schema, result);
                }
            }
            EngineEvent::Described {
                schema,
                table,
                result,
            } => {
                if let Some(w) = &mut self.workspace {
                    w.explorer.on_described(&schema, &table, result);
                }
            }
            EngineEvent::Columns { id, columns } => {
                if let Some(w) = &mut self.workspace {
                    w.results.on_columns(id, columns);
                }
            }
            EngineEvent::Rows { id, rows } => {
                if let Some(w) = &mut self.workspace {
                    w.results.on_rows(id, rows);
                }
            }
            EngineEvent::QueryDone { id, outcome } => self.with_ctx(|app, ctx| {
                if let Some(w) = app.workspace {
                    // The query may have created or dropped tables.
                    if w.results.on_done(id, outcome, ctx) {
                        w.explorer.refresh_loaded(ctx);
                    }
                }
            }),
        }
    }

    fn connected(&mut self, connecting: super::Connecting) {
        let connection = connecting.connection;
        let database = match (&connecting.switch_to, connection.driver) {
            (Some(database), _) => database.clone(),
            (None, DbType::Sqlite) => "main".to_string(),
            (None, _) => connection.params("").database.unwrap_or_default(),
        };
        // No username: the event log also goes to the debug log file.
        let switching = connecting.switch_to.is_some();
        self.log
            .push(format!("Connected to {} ({database})", connection.name));
        self.session = Some(Session {
            connection,
            password: connecting.password,
            database: database.clone(),
        });
        if switching {
            self.with_ctx(|app, ctx| {
                if let Some(w) = app.workspace {
                    w.explorer.set_active(&database, ctx);
                }
            });
        } else {
            let mut workspace = Workspace::new(&database);
            self.with_ctx(|_, ctx| workspace.explorer.load(ctx));
            self.workspace = Some(workspace);
        }
        self.screen = Screen::Workspace;
    }
}
