//! Engine answers, applied to the screens.

use dfox_core::{
    engine::{EngineEvent, SessionInfo},
    models::connections::DbType,
};

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
                    Ok(info) => self.connected(connecting, info),
                    Err(err) => self.with_ctx(|_, ctx| {
                        ctx.toast(Level::Error, format!("Couldn't connect to {name}: {err}"))
                    }),
                }
            }
            EngineEvent::Pong { id, result } => self.manager.on_pong(id, result),
            EngineEvent::AllColumns { path, result } => {
                if let (Some(w), Some(name)) = (&mut self.workspace, path.name()) {
                    w.catalog.on_columns(name, result);
                }
            }
            EngineEvent::Children { path, result } => self.with_ctx(|app, ctx| {
                if let Some(w) = app.workspace {
                    w.on_children(&path, result, ctx);
                }
            }),
            EngineEvent::Definition { path, kind, result } => self.with_ctx(|app, ctx| {
                if let Some(w) = app.workspace {
                    w.on_definition(path, kind, result, ctx);
                }
            }),
            EngineEvent::ColumnStats {
                path,
                column,
                profile,
                result,
            } => {
                if let Some(w) = &mut self.workspace {
                    w.on_column_stats(&path, &column, profile, result);
                }
            }
            EngineEvent::Described { path, result } => {
                let command = self.with_ctx(|app, ctx| {
                    app.workspace
                        .as_mut()
                        .and_then(|w| w.on_described(&path, result, ctx))
                });
                if let Some(command) = command {
                    self.apply_command(command);
                }
            }
            EngineEvent::Counted { path, result } => self.with_ctx(|app, ctx| {
                if let Some(w) = app.workspace {
                    w.on_counted(&path, result, ctx);
                }
            }),
            event @ (EngineEvent::Schema { .. }
            | EngineEvent::Batch { .. }
            | EngineEvent::Notice { .. }
            | EngineEvent::QueryDone { .. }) => self.with_ctx(|app, ctx| {
                if let Some(w) = app.workspace {
                    if w.on_query_event(event, ctx) {
                        w.explorer.refresh_loaded(ctx);
                    }
                }
            }),
        }
    }

    fn connected(&mut self, connecting: super::Connecting, info: SessionInfo) {
        let connection = connecting.connection;
        let (name, driver, read_only) = (
            connection.name.clone(),
            connection.driver,
            connection.read_only,
        );
        let database = match (&connecting.switch_to, connection.driver) {
            (Some(database), _) => database.clone(),
            (None, DbType::Sqlite) => "main".to_string(),
            (None, _) => connection.target().database.unwrap_or_default(),
        };
        // No username: the event log also goes to the debug log file.
        let switching = connecting.switch_to.is_some();
        self.log
            .push(format!("Connected to {} ({database})", connection.name));
        self.session = Some(Session {
            connection,
            credentials: connecting.credentials,
            database: database.clone(),
        });
        if switching {
            self.with_ctx(|app, ctx| {
                if let Some(w) = app.workspace {
                    w.catalog = Default::default();
                    w.explorer.set_active(&database, ctx);
                }
            });
        } else {
            let mut workspace = Workspace::new(&name, driver, &database).with_read_only(read_only);
            if let Some(dialect) = crate::sql::session_dialect(info.language) {
                workspace = workspace.with_dialect(dialect);
            }
            self.with_ctx(|_, ctx| workspace.explorer.load(ctx));
            self.workspace = Some(workspace);
        }
        self.screen = Screen::Workspace;
    }
}
