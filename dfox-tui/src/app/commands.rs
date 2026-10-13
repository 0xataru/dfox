//! What the app does for a [`Command`]: managing saved connections and
//! connecting.

use dfox_core::{engine::Command as DbCommand, models::connections::DbType};
use tokio::time::Instant;

use super::{App, Connecting, Overlay, Screen};
use crate::{
    command::Command,
    components::{form::ConnectionForm, manager::EntryKey},
    connections::{pgpass, secrets, Connection},
    widgets::{confirm::Confirm, toast::Level},
};

impl App {
    pub(super) fn apply_command(&mut self, command: Command) {
        match command {
            Command::Quit => self.should_quit = true,
            Command::ConfirmQuit => {
                let running = self
                    .workspace
                    .as_ref()
                    .is_some_and(|w| w.running().is_some());
                let message = if running {
                    "Quit dfox? A query is still running."
                } else {
                    "Quit dfox?"
                };
                self.overlay = Some(Overlay::Confirm(Confirm {
                    title: "Quit".to_string(),
                    message: message.to_string(),
                    on_yes: Command::Quit,
                }));
            }
            Command::Connect(key) => self.connect(key),
            Command::NewConnection => self.open_form(ConnectionForm::new()),
            Command::EditConnection(name) => {
                if let Some(connection) = self.store.get(&name).cloned() {
                    self.open_form(ConnectionForm::edit(Some(name), &connection, None));
                }
            }
            Command::SaveDetected(index) => {
                if let Some(found) = self.manager.detected.get(index).cloned() {
                    let mut connection = found.connection;
                    connection.name = connection.name.trim_start_matches("pgpass: ").to_string();
                    self.open_form(ConnectionForm::edit(None, &connection, found.password));
                }
            }
            Command::SaveConnection {
                original,
                connection,
                password,
            } => self.save_connection(original, connection, password),
            Command::ConfirmDelete(name) => {
                self.overlay = Some(Overlay::Confirm(Confirm {
                    title: "Delete".to_string(),
                    message: format!("Delete the connection {name:?}?"),
                    on_yes: Command::DeleteConnection(name),
                }));
            }
            Command::DeleteConnection(name) => self.delete_connection(&name),
            Command::CloseForm => {
                self.form = None;
                self.show_manager();
            }
            Command::UseDatabase(database) => {
                if let Some(session) = self.session.clone() {
                    self.start_connect(session.connection, session.password, Some(database));
                }
            }
            Command::Connections => self.show_manager(),
        }
    }

    /// Shows the connection manager and checks every connection.
    pub(super) fn show_manager(&mut self) {
        self.screen = Screen::Manager;
        self.with_ctx(|app, ctx| app.manager.ping_all(ctx));
    }

    fn open_form(&mut self, form: ConnectionForm) {
        self.form = Some(form);
        self.screen = Screen::Form;
    }

    fn save_connection(
        &mut self,
        original: Option<String>,
        connection: Connection,
        password: Option<String>,
    ) {
        let name = connection.name.clone();
        if let Err(err) = self.store.save(original.as_deref(), connection) {
            if let Some(form) = &mut self.form {
                form.error = Some(err);
            }
            return;
        }
        let now = Instant::now();
        let keychain = original
            .as_deref()
            .map_or(Ok(()), |old| {
                secrets::rename(self.secrets.as_ref(), old, &name)
            })
            .and_then(|()| match &password {
                Some(password) => self.secrets.set(&name, password),
                None => Ok(()),
            });
        if let Err(err) = keychain {
            self.toasts.push(
                Level::Warning,
                format!("Couldn't keep the password in the keychain ({err}); dfox will ask for it"),
                now,
            );
        }
        self.log.push(format!("Saved connection {name}"));
        self.toasts
            .push(Level::Success, format!("Saved {name}"), now);
        self.manager.saved = self.store.connections.clone();
        self.manager.select_name(&name);
        self.form = None;
        self.show_manager();
    }

    fn delete_connection(&mut self, name: &str) {
        let now = Instant::now();
        match self.store.remove(name) {
            Ok(()) => {
                if let Err(err) = self.secrets.delete(name) {
                    self.toasts.push(
                        Level::Warning,
                        format!("Couldn't remove the password from the keychain: {err}"),
                        now,
                    );
                }
                self.toasts
                    .push(Level::Success, format!("Deleted {name}"), now);
                self.manager.saved = self.store.connections.clone();
            }
            Err(err) => self.toasts.push(Level::Error, err, now),
        }
    }

    /// Connects to an entry of the manager, with its password from the URL,
    /// the keychain or `.pgpass`, or asks for one.
    fn connect(&mut self, key: EntryKey) {
        let (connection, password) = match key {
            EntryKey::Saved(name) => {
                let Some(connection) = self.store.get(&name).cloned() else {
                    return;
                };
                let password = match self.secrets.get(&name) {
                    Ok(password) => password,
                    Err(err) => {
                        self.toasts.push(
                            Level::Warning,
                            format!("Couldn't read the keychain: {err}"),
                            Instant::now(),
                        );
                        None
                    }
                };
                (connection, password)
            }
            EntryKey::Detected(index) => match self.manager.detected.get(index) {
                Some(found) => (found.connection.clone(), found.password.clone()),
                None => return,
            },
        };
        let password =
            password.or_else(|| pgpass::lookup(&self.pgpass, &connection).map(str::to_string));
        match password {
            Some(password) => self.start_connect(connection, password, None),
            None if connection.driver == DbType::Sqlite => {
                self.start_connect(connection, String::new(), None)
            }
            None => {
                self.overlay = Some(Overlay::Password {
                    connection,
                    typed: String::new(),
                })
            }
        }
    }

    /// Connects, to the connection's own database or to `switch_to` on the
    /// same server.
    pub(super) fn start_connect(
        &mut self,
        connection: Connection,
        password: String,
        switch_to: Option<String>,
    ) {
        let mut params = connection.params(&password);
        if let Some(database) = &switch_to {
            params.database = Some(database.clone());
        }
        self.engine.send(DbCommand::Connect {
            db_type: connection.driver,
            params,
        });
        self.connecting = Some(Connecting {
            connection,
            password,
            switch_to,
        });
    }
}
