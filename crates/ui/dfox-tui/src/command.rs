//! What a component asks the app to do in response to an action.

use dfox_core::catalog::{NodeKind, NodePath};

use crate::{components::manager::EntryKey, connections::Connection};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Quit,
    /// Writes the grid's edits to their table.
    ApplyChanges(dfox_core::session::ChangeSet),
    /// Asks where to copy `sql`'s rows, the rows of the table `source`.
    CopyTo {
        sql: String,
        source: String,
        estimate: Option<u64>,
    },
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
    /// Offers what can be done with the table or view at `path`, a `kind`;
    /// `name` is how SQL names it.
    TableMenu {
        path: NodePath,
        kind: NodeKind,
        name: Vec<String>,
    },
    /// Puts a name, quoted for the dialect, at the editor's cursor.
    InsertName(Vec<String>),
    /// Opens the SQL that creates the view or routine at `path` in a tab.
    Definition {
        path: NodePath,
        kind: NodeKind,
    },
    /// Shows everything known about `column` of the table at `table`.
    ColumnCard {
        table: NodePath,
        column: String,
    },
    /// Selects the object at `path` in the explorer.
    Reveal(NodePath),
    /// Opens SQL already at hand, the definition of `path`, in a tab.
    ShowSql {
        path: NodePath,
        kind: NodeKind,
        text: String,
    },
}
