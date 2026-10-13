//! What a component asks the app to do in response to an action.

use crate::{components::manager::EntryKey, connections::Connection};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Quit,
    /// Asks before quitting.
    ConfirmQuit,
    Connect(EntryKey),
    NewConnection,
    EditConnection(String),
    /// Opens the form to save a connection found on this machine.
    SaveDetected(usize),
    SaveConnection {
        /// The saved connection being replaced, by name.
        original: Option<String>,
        connection: Connection,
        /// A new password to keep in the keychain.
        password: Option<String>,
    },
    ConfirmDelete(String),
    DeleteConnection(String),
    CloseForm,
    /// Switches the session to another database on the same server.
    UseDatabase(String),
    /// Back to the connection manager.
    Connections,
    /// Asks before running SQL the guard flagged, saying why.
    ConfirmRun {
        sql: String,
        reasons: Vec<String>,
    },
    /// Runs SQL that was confirmed.
    RunConfirmed(String),
}
