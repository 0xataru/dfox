//! What a component asks the app to do in response to an action.

use dfox_core::catalog::NodePath;

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
        connection: Box<Connection>,
        /// A new password to keep in the keychain.
        password: Option<String>,
        /// A new SSH password or key passphrase to keep in the keychain.
        ssh_secret: Option<String>,
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
    /// Shows the rows of the table at `path`; `name` is how SQL names it.
    Browse {
        path: NodePath,
        name: Vec<String>,
    },
    /// Puts a name, quoted for the dialect, at the editor's cursor.
    InsertName(Vec<String>),
}
