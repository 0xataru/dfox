//! What the app does for a [`Command`]: managing saved connections and
//! connecting.

use dfox_core::{engine::Command as DbCommand, models::connections::DbType};
use tokio::time::Instant;

use super::{App, Ask, Connecting, Credentials, Overlay, Screen};
use crate::{
    command::Command,
    components::{
        copy::{target_path, CopyDialog},
        form::ConnectionForm,
        manager::EntryKey,
    },
    connections::{pgpass, secrets, ssh_secret_name, Connection, SshLogin},
    widgets::{confirm::Confirm, toast::Level},
};

impl App {
    pub(super) fn apply_command(&mut self, command: Command) {
        match command {
            Command::Quit => self.should_quit = true,
            Command::ApplyChanges(changes) => self.engine.send(DbCommand::Apply { changes }),
            Command::CopyTo {
                sql,
                source,
                estimate,
            } => match CopyDialog::new(sql, &source, estimate, &self.store.connections) {
                Some(dialog) => self.overlay = Some(Overlay::Copy(dialog)),
                None => self.toasts.push(
                    Level::Info,
                    "Save a connection to a database that takes rows first: Postgres, MySQL, SQLite or MongoDB".into(),
                    Instant::now(),
                ),
            },
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
                    danger: false,
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
                    connection.name = connection
                        .name
                        .trim_start_matches("pgpass: ")
                        .trim_start_matches("my.cnf: ")
                        .to_string();
                    self.open_form(ConnectionForm::edit(None, &connection, found.password));
                }
            }
            Command::SaveConnection {
                original,
                connection,
                password,
                ssh_secret,
            } => self.save_connection(original, *connection, password, ssh_secret),
            Command::ConfirmDelete(name) => {
                self.overlay = Some(Overlay::Confirm(Confirm {
                    title: "Delete".to_string(),
                    message: format!("Delete the connection {name:?}?"),
                    on_yes: Command::DeleteConnection(name),
                    danger: true,
                }));
            }
            Command::DeleteConnection(name) => self.delete_connection(&name),
            Command::CloseForm => {
                self.form = None;
                self.show_manager();
            }
            Command::UseDatabase(database) => {
                if let Some(session) = self.session.clone() {
                    self.start_connect(session.connection, session.credentials, Some(database));
                }
            }
            Command::Connections => self.show_manager(),
            Command::ConfirmRun { sql, reasons } => {
                let mut message = reasons.join("\n");
                message.push_str("\n\nRun it anyway?");
                self.overlay = Some(Overlay::Confirm(Confirm {
                    title: "Destructive query".to_string(),
                    message,
                    on_yes: Command::RunConfirmed(sql),
                    danger: true,
                }));
            }
            Command::RunConfirmed(sql) => self.with_ctx(|app, ctx| {
                if let Some(w) = app.workspace {
                    w.run_checked(sql, ctx);
                }
            }),
            command @ (Command::TableMenu { .. }
            | Command::InsertName(_)
            | Command::Definition { .. }
            | Command::ColumnCard { .. }
            | Command::Reveal(_)
            | Command::ShowSql { .. }) => {
                self.with_ctx(|app, ctx| {
                    if let Some(w) = app.workspace {
                        w.explorer_command(command, ctx);
                    }
                });
            }
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
        ssh_secret: Option<String>,
    ) {
        let name = connection.name.clone();
        // An agent login, or no SSH at all, leaves no secret behind.
        let keeps_ssh_secret = connection
            .ssh
            .as_ref()
            .is_some_and(|ssh| ssh.login != SshLogin::Agent);
        if let Err(err) = self.store.save(original.as_deref(), connection) {
            if let Some(form) = &mut self.form {
                form.error = Some(err);
            }
            return;
        }
        let now = Instant::now();
        let secrets = self.secrets.as_ref();
        let ssh_name = ssh_secret_name(&name);
        let keychain = original
            .as_deref()
            .map_or(Ok(()), |old| {
                secrets::rename(secrets, old, &name)?;
                secrets::rename(secrets, &ssh_secret_name(old), &ssh_name)
            })
            .and_then(|()| match &password {
                Some(password) => secrets.set(&name, password),
                None => Ok(()),
            })
            .and_then(|()| match (&ssh_secret, keeps_ssh_secret) {
                (Some(secret), true) => secrets.set(&ssh_name, secret),
                (_, true) => Ok(()),
                (_, false) => secrets.delete(&ssh_name),
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
                let deleted = self
                    .secrets
                    .delete(name)
                    .and_then(|()| self.secrets.delete(&ssh_secret_name(name)));
                if let Err(err) = deleted {
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
                let password = self.keychain(&name);
                (connection, password)
            }
            EntryKey::Detected(index) => match self.manager.detected.get(index) {
                Some(found) => (found.connection.clone(), found.password.clone()),
                None => return,
            },
        };
        let password =
            password.or_else(|| pgpass::lookup(&self.pgpass, &connection).map(str::to_string));
        let ssh = match connection.ssh {
            Some(_) => self.keychain(&ssh_secret_name(&connection.name)),
            None => None,
        };
        self.connect_with(connection, password, ssh);
    }

    /// Starts the copy `dialog` chose, with the target's saved secrets.
    pub(super) fn start_copy(&mut self, dialog: CopyDialog) {
        let name = dialog.target().to_string();
        let Some(connection) = self.store.get(&name).cloned() else {
            return;
        };
        let password = self
            .keychain(&name)
            .or_else(|| pgpass::lookup(&self.pgpass, &connection).map(str::to_string))
            .or_else(|| (connection.driver == DbType::Sqlite).then(String::new));
        let ssh = match connection.ssh {
            Some(_) => self.keychain(&ssh_secret_name(&name)),
            None => None,
        };
        // shortcut: secrets aren't asked for here; prompt like connecting
        // does when copying to connections without saved ones matters.
        let (Some(password), false) = (password, ssh.is_none() && connection.needs_ssh_password())
        else {
            self.toasts.push(
                Level::Warning,
                format!(
                    "Save {name}'s password first (e on it in the connection list) to copy into it"
                ),
                Instant::now(),
            );
            return;
        };
        let target = dfox_core::engine::CopyTarget {
            db_type: connection.driver,
            params: connection.params(&password, ssh.as_deref()),
            table: target_path(&connection, dialog.table.trim()),
            create: dialog.create,
        };
        self.with_ctx(|app, ctx| {
            if let Some(w) = app.workspace {
                w.start_copy(dialog, name, target, ctx);
            }
        });
    }

    /// Reads a secret from the keychain; a failure is a warning, and the
    /// secret is asked for instead.
    fn keychain(&mut self, name: &str) -> Option<String> {
        match self.secrets.get(name) {
            Ok(secret) => secret,
            Err(err) => {
                self.toasts.push(
                    Level::Warning,
                    format!("Couldn't read the keychain: {err}"),
                    Instant::now(),
                );
                None
            }
        }
    }

    /// Connects once every secret is known, asking for the database
    /// password and then the SSH password when they are missing.
    pub(super) fn connect_with(
        &mut self,
        connection: Connection,
        password: Option<String>,
        ssh: Option<String>,
    ) {
        let password = match password {
            Some(password) => password,
            None if connection.driver == DbType::Sqlite => String::new(),
            None => {
                self.overlay = Some(Overlay::Password {
                    connection,
                    typed: String::new(),
                    ask: Ask::Database { ssh },
                });
                return;
            }
        };
        if ssh.is_none() && connection.needs_ssh_password() {
            self.overlay = Some(Overlay::Password {
                connection,
                typed: String::new(),
                ask: Ask::Ssh { password },
            });
            return;
        }
        self.start_connect(connection, Credentials { password, ssh }, None);
    }

    /// Connects, to the connection's own database or to `switch_to` on the
    /// same server.
    pub(super) fn start_connect(
        &mut self,
        connection: Connection,
        credentials: Credentials,
        switch_to: Option<String>,
    ) {
        let mut params = connection.params(&credentials.password, credentials.ssh.as_deref());
        if let Some(database) = &switch_to {
            params.database = Some(database.clone());
        }
        self.engine.send(DbCommand::Connect {
            db_type: connection.driver,
            params,
        });
        self.connecting = Some(Connecting {
            connection,
            credentials,
            switch_to,
        });
    }
}
