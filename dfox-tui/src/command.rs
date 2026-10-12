//! What a component asks the app to do in response to an action.

use dfox_core::models::connections::{ConnectionParams, DbType};

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Quit,
    /// Asks before quitting.
    ConfirmQuit,
    /// Shows the connection form for `DbType`.
    ConnectForm(DbType),
    /// Connects to a server, to list its databases.
    ConnectServer {
        db_type: DbType,
        params: ConnectionParams,
    },
    /// Connects to one of the server's databases.
    OpenDatabase(String),
    /// Back to the database list.
    Databases,
    /// Back to the database type list.
    DbTypes,
}
