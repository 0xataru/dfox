use std::{collections::HashMap, io, time::Duration};

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyEventKind},
    execute,
    terminal::{enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{prelude::CrosstermBackend, Terminal};
use tokio::{sync::mpsc, time::Instant};

use crate::ui::components::{
    connection_input::{ConnectionInput, InputField},
    types::{FocusedWidget, ScreenState, MAX_RESULT_ROWS},
};
use crate::ui::utils::terminal_guard::TerminalGuard;
use dfox_core::{
    engine::{Command, EngineEvent, EngineHandle, QueryId, QueryStats},
    errors::DbError,
    models::{
        connections::{ConnectionParams, DbType},
        schema::TableSchema,
    },
    result::{ResultSet, RowStore},
};

/// How often the screen redraws while something is loading, to move the
/// spinner and the elapsed time. Nothing redraws while idle.
const BUSY_TICK: Duration = Duration::from_millis(100);

/// What a pending connect leads to once it succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectTarget {
    /// The server's default database, to list the others.
    Server,
    /// The database picked from the list.
    Database,
}

/// Requests sent to the engine that haven't been answered yet.
#[derive(Debug, Default)]
pub struct Pending {
    pub connect: Option<ConnectTarget>,
    pub databases: bool,
    pub tables: bool,
    pub describe: Option<String>,
}

#[derive(Debug)]
pub struct RunningQuery {
    pub id: QueryId,
    pub started: Instant,
    pub cancelling: bool,
}

pub struct DatabaseClientUI {
    pub engine: EngineHandle,
    pub connection_input: ConnectionInput,
    pub current_screen: ScreenState,
    pub selected_db_type: usize,
    pub databases: Vec<String>,
    pub selected_database: usize,
    pub tables: Vec<String>,
    pub selected_table: usize,
    pub expanded_table: Option<usize>,
    pub table_schemas: HashMap<String, TableSchema>,
    pub sql_editor_content: String,
    pub sql_query_result: RowStore,
    pub sql_query_error: Option<String>,
    pub sql_query_success_message: Option<String>,
    pub current_focus: FocusedWidget,
    pub connection_error_message: Option<String>,
    pub tables_scroll: usize,
    pub sql_result_scroll: usize,
    pub sql_result_horizontal_scroll: usize,
    pub databases_scroll: usize,
    pub selected_result_row: usize,
    pub sql_editor_scroll: usize,
    pub sql_editor_cursor_x: usize,
    pub sql_editor_cursor_y: usize,
    pub debug_info: Vec<String>,
    pub pending: Pending,
    pub running_query: Option<RunningQuery>,
    pub should_quit: bool,
    next_query_id: QueryId,
    /// Spinner frames are counted from here.
    pub started_at: Instant,
}

impl DatabaseClientUI {
    pub fn new(engine: EngineHandle) -> Self {
        Self {
            engine,
            connection_input: ConnectionInput::new(),
            current_screen: ScreenState::DbTypeSelection,
            selected_db_type: 0,
            databases: Vec::new(),
            selected_database: 0,
            tables: Vec::new(),
            selected_table: 0,
            expanded_table: None,
            table_schemas: HashMap::new(),
            sql_editor_content: String::new(),
            sql_query_result: RowStore::default(),
            sql_query_error: None,
            sql_query_success_message: None,
            current_focus: FocusedWidget::TablesList,
            connection_error_message: None,
            tables_scroll: 0,
            sql_result_scroll: 0,
            sql_result_horizontal_scroll: 0,
            databases_scroll: 0,
            selected_result_row: 0,
            sql_editor_scroll: 0,
            sql_editor_cursor_x: 0,
            sql_editor_cursor_y: 0,
            debug_info: Vec::new(),
            pending: Pending::default(),
            running_query: None,
            should_quit: false,
            next_query_id: 0,
            started_at: Instant::now(),
        }
    }

    pub fn add_debug_info(&mut self, info: String) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let debug_msg = format!("[{}] {}", timestamp, info);

        self.debug_info.push(debug_msg.clone());

        if self.debug_info.len() > 100 {
            self.debug_info.remove(0);
        }

        log::debug!("{}", info);
    }

    /// The server type picked on the first screen. SQLite has no server
    /// connection flow yet.
    pub fn server_db_type(&self) -> Option<DbType> {
        match self.selected_db_type {
            0 => Some(DbType::Postgres),
            1 => Some(DbType::MySql),
            _ => None,
        }
    }

    pub fn current_input_index(&self) -> usize {
        match self.connection_input.current_field {
            InputField::Username => 0,
            InputField::Password => 1,
            InputField::Hostname => 2,
            InputField::Port => 3,
        }
    }

    /// True while waiting on the engine, so the screen keeps redrawing.
    pub fn is_busy(&self) -> bool {
        self.pending.connect.is_some()
            || self.pending.databases
            || self.pending.tables
            || self.pending.describe.is_some()
            || self.running_query.is_some()
    }

    pub async fn run_ui(&mut self, events: mpsc::Receiver<EngineEvent>) -> io::Result<()> {
        let _guard = TerminalGuard;
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

        let result = self.ui_loop(&mut terminal, events).await;

        terminal.clear()?;
        terminal.show_cursor()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Clear(ClearType::All)
        )?;

        result
    }

    /// Waits on the terminal, the engine and, while busy, a redraw tick, and
    /// redraws only after something changed. Nothing here waits on a database.
    async fn ui_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        mut events: mpsc::Receiver<EngineEvent>,
    ) -> io::Result<()> {
        let mut input = EventStream::new();
        let mut tick = tokio::time::interval(BUSY_TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut dirty = true;

        while !self.should_quit {
            if dirty {
                terminal.draw(|f| self.render(f))?;
                dirty = false;
            }

            tokio::select! {
                event = input.next() => match event {
                    // Windows also reports key releases; act on presses only.
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        self.handle_key(key);
                        dirty = true;
                    }
                    Some(Ok(Event::Resize(..))) => dirty = true,
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(err),
                    None => return Ok(()),
                },
                Some(event) = events.recv() => {
                    self.apply(event);
                    dirty = true;
                }
                _ = tick.tick(), if self.is_busy() => dirty = true,
            }
        }
        Ok(())
    }

    /// Connection details from the connection screen, for `database`.
    fn connection_params(&self, database: &str) -> Result<ConnectionParams, DbError> {
        let input = &self.connection_input;
        let port = match input.port.trim() {
            "" => match self.server_db_type() {
                Some(DbType::MySql) => 3306,
                _ => 5432,
            },
            port => port
                .parse()
                .map_err(|_| DbError::Config(format!("Invalid port: {port}")))?,
        };
        Ok(ConnectionParams {
            host: input.hostname.trim().to_string(),
            port,
            username: input.username.clone(),
            password: input.password.clone(),
            database: Some(database.to_string()),
        })
    }

    pub fn connect(&mut self, target: ConnectTarget, database: &str) {
        if self.pending.connect.is_some() {
            return;
        }
        let Some(db_type) = self.server_db_type() else {
            return;
        };
        match self.connection_params(database) {
            Ok(params) => {
                self.pending.connect = Some(target);
                self.engine.send(Command::Connect { db_type, params });
            }
            Err(err) => self.connection_error_message = Some(err.to_string()),
        }
    }

    pub fn default_database(&self) -> &'static str {
        match self.server_db_type() {
            Some(DbType::MySql) => "mysql",
            _ => "postgres",
        }
    }

    fn request_databases(&mut self) {
        self.pending.databases = true;
        self.engine.send(Command::ListDatabases);
    }

    fn request_tables(&mut self) {
        self.pending.tables = true;
        self.engine.send(Command::ListTables);
    }

    pub fn describe(&mut self, table: String) {
        self.pending.describe = Some(table.clone());
        self.engine.send(Command::Describe { table });
    }

    pub fn run_query(&mut self) {
        let sql = self.sql_editor_content.trim();
        if sql.is_empty() || self.running_query.is_some() {
            return;
        }
        self.next_query_id += 1;
        let id = self.next_query_id;
        self.engine.send(Command::Run {
            id,
            sql: sql.to_string(),
            row_limit: MAX_RESULT_ROWS,
        });
        self.running_query = Some(RunningQuery {
            id,
            started: Instant::now(),
            cancelling: false,
        });
        self.sql_query_result = RowStore::default();
        self.sql_query_error = None;
        self.sql_query_success_message = None;
        self.reset_result_navigation();
    }

    pub fn cancel_query(&mut self) {
        if let Some(query) = &mut self.running_query {
            if !query.cancelling {
                query.cancelling = true;
                self.engine.send(Command::Cancel { id: query.id });
            }
        }
    }

    /// Stops following the running query: it is cancelled, and its remaining
    /// events are ignored.
    pub fn abandon_query(&mut self) {
        self.cancel_query();
        self.running_query = None;
    }

    pub fn reset_result_navigation(&mut self) {
        self.selected_result_row = 0;
        self.sql_result_scroll = 0;
        self.sql_result_horizontal_scroll = 0;
    }

    fn is_current_query(&self, id: QueryId) -> bool {
        self.running_query.as_ref().is_some_and(|q| q.id == id)
    }

    /// Updates the state with an answer from the engine.
    pub fn apply(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Connected(result) => {
                let Some(target) = self.pending.connect.take() else {
                    return;
                };
                match (result, target) {
                    (Ok(()), ConnectTarget::Server) => {
                        self.current_screen = ScreenState::DatabaseSelection;
                        self.request_databases();
                    }
                    (Ok(()), ConnectTarget::Database) => {
                        self.current_screen = ScreenState::TableView;
                        self.tables.clear();
                        self.selected_table = 0;
                        self.tables_scroll = 0;
                        self.expanded_table = None;
                        self.table_schemas.clear();
                        self.request_tables();
                    }
                    (Err(err), _) => {
                        log::error!("Error connecting: {}", err);
                        self.connection_error_message = Some(err.to_string());
                    }
                }
            }
            EngineEvent::Databases(result) => {
                self.pending.databases = false;
                match result {
                    Ok(databases) => self.databases = databases,
                    Err(err) => {
                        log::error!("Error fetching databases: {}", err);
                        self.databases = vec![format!("Error fetching databases: {}", err)];
                    }
                }
                self.selected_database = self
                    .selected_database
                    .min(self.databases.len().saturating_sub(1));
            }
            EngineEvent::Tables(result) => {
                self.pending.tables = false;
                match result {
                    Ok(tables) => self.tables = tables,
                    Err(err) => {
                        log::error!("Error fetching tables: {}", err);
                        self.sql_query_error = Some(format!("Error fetching tables: {}", err));
                        self.tables.clear();
                    }
                }
                self.selected_table = self.selected_table.min(self.tables.len().saturating_sub(1));
            }
            EngineEvent::Schema(result) => {
                self.pending.describe = None;
                match result {
                    Ok(schema) => {
                        self.expanded_table =
                            self.tables.iter().position(|t| *t == schema.table_name);
                        self.table_schemas.insert(schema.table_name.clone(), schema);
                    }
                    Err(err) => {
                        log::error!("Error describing table: {}", err);
                        self.sql_query_error = Some(format!("Error describing table: {}", err));
                    }
                }
            }
            EngineEvent::Columns { id, columns } if self.is_current_query(id) => {
                self.sql_query_result = RowStore::new(columns);
                self.reset_result_navigation();
            }
            EngineEvent::Rows { id, rows } if self.is_current_query(id) => {
                self.sql_query_result.extend(rows);
            }
            EngineEvent::QueryDone { id, outcome } if self.is_current_query(id) => {
                self.running_query = None;
                self.sql_query_result.finish();
                match outcome {
                    Ok(stats) => {
                        self.sql_query_success_message = Some(query_message(
                            &stats,
                            !self.sql_query_result.columns().is_empty(),
                        ));
                        // The query may have created or dropped tables.
                        self.request_tables();
                    }
                    Err(DbError::Cancelled) => {
                        self.sql_query_success_message = Some("Query cancelled".to_string());
                    }
                    Err(err) => {
                        self.sql_query_error = Some(format!("SQL Error: {}", err));
                        self.sql_query_result = RowStore::default();
                    }
                }
            }
            // Rows of a query that was replaced or abandoned.
            EngineEvent::Columns { .. }
            | EngineEvent::Rows { .. }
            | EngineEvent::QueryDone { .. } => {}
        }
    }
}

/// The status line shown after a query finishes.
fn query_message(stats: &QueryStats, has_columns: bool) -> String {
    let took = format_duration(stats.elapsed);
    if stats.truncated {
        format!("Showing the first {} rows ({took})", stats.rows)
    } else if has_columns {
        format!("{} row(s) in {took}", stats.rows)
    } else if stats.rows_affected > 0 {
        format!("{} row(s) affected in {took}", stats.rows_affected)
    } else {
        format!("Query executed successfully in {took}.")
    }
}

pub fn format_duration(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{} ms", d.as_millis())
    } else {
        format!("{:.1} s", d.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dfox_core::result::{ColumnMeta, OwnedCell};

    fn ui() -> (DatabaseClientUI, mpsc::UnboundedReceiver<Command>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (DatabaseClientUI::new(EngineHandle::new(tx)), rx)
    }

    fn running(ui: &mut DatabaseClientUI) -> QueryId {
        ui.current_screen = ScreenState::TableView;
        ui.sql_editor_content = "  SELECT 1  ".to_string();
        ui.run_query();
        ui.running_query.as_ref().expect("a running query").id
    }

    #[test]
    fn connecting_to_the_server_lists_its_databases() {
        let (mut ui, mut commands) = ui();
        ui.current_screen = ScreenState::ConnectionInput;
        ui.connection_input.username = "me".to_string();
        ui.connection_input.hostname = " localhost ".to_string();

        ui.connect(ConnectTarget::Server, "postgres");
        match commands.try_recv() {
            Ok(Command::Connect { db_type, params }) => {
                assert_eq!(db_type, DbType::Postgres);
                assert_eq!(params.host, "localhost");
                assert_eq!(params.port, 5432);
                assert_eq!(params.database.as_deref(), Some("postgres"));
            }
            other => panic!("expected Connect, got {other:?}"),
        }
        assert!(ui.is_busy());

        ui.apply(EngineEvent::Connected(Ok(())));
        assert!(matches!(ui.current_screen, ScreenState::DatabaseSelection));
        assert!(matches!(commands.try_recv(), Ok(Command::ListDatabases)));

        ui.apply(EngineEvent::Databases(Ok(vec!["a".into(), "b".into()])));
        assert_eq!(ui.databases, ["a", "b"]);
        assert!(!ui.is_busy());
    }

    #[test]
    fn connect_errors_stay_on_the_connection_screen() {
        let (mut ui, mut commands) = ui();
        ui.current_screen = ScreenState::ConnectionInput;

        ui.connection_input.port = "abc".to_string();
        ui.connect(ConnectTarget::Server, "postgres");
        assert!(commands.try_recv().is_err());
        assert_eq!(
            ui.connection_error_message.as_deref(),
            Some("Configuration error: Invalid port: abc")
        );

        ui.connection_input.port = "5432".to_string();
        ui.connect(ConnectTarget::Server, "postgres");
        ui.apply(EngineEvent::Connected(Err(DbError::Connection(
            "refused".into(),
        ))));
        assert!(matches!(ui.current_screen, ScreenState::ConnectionInput));
        assert_eq!(
            ui.connection_error_message.as_deref(),
            Some("Connection error: refused")
        );
    }

    #[test]
    fn query_rows_stream_into_the_result() {
        let (mut ui, mut commands) = ui();
        let id = running(&mut ui);
        match commands.try_recv() {
            Ok(Command::Run { sql, row_limit, .. }) => {
                assert_eq!(sql, "SELECT 1");
                assert_eq!(row_limit, MAX_RESULT_ROWS);
            }
            other => panic!("expected Run, got {other:?}"),
        }

        ui.apply(EngineEvent::Columns {
            id,
            columns: vec![ColumnMeta::new("n", "int4")],
        });
        ui.apply(EngineEvent::Rows {
            id,
            rows: vec![vec![OwnedCell::Int(1)], vec![OwnedCell::Int(2)]],
        });
        assert_eq!(ui.sql_query_result.row_count(), 2);
        assert!(!ui.sql_query_result.is_complete());

        ui.apply(EngineEvent::Rows {
            id,
            rows: vec![vec![OwnedCell::Int(3)]],
        });
        ui.apply(EngineEvent::QueryDone {
            id,
            outcome: Ok(QueryStats {
                elapsed: Duration::from_millis(5),
                rows: 3,
                ..Default::default()
            }),
        });

        assert_eq!(ui.sql_query_result.row_count(), 3);
        assert!(ui.sql_query_result.is_complete());
        assert!(ui.running_query.is_none());
        assert_eq!(
            ui.sql_query_success_message.as_deref(),
            Some("3 row(s) in 5 ms")
        );
        assert!(matches!(commands.try_recv(), Ok(Command::ListTables)));
    }

    #[test]
    fn query_errors_replace_the_result() {
        let (mut ui, _commands) = ui();
        let id = running(&mut ui);

        ui.apply(EngineEvent::QueryDone {
            id,
            outcome: Err(DbError::General("boom".into())),
        });

        assert_eq!(
            ui.sql_query_error.as_deref(),
            Some("SQL Error: Error: boom")
        );
        assert_eq!(ui.sql_query_result.row_count(), 0);
    }

    #[test]
    fn cancel_is_sent_once() {
        let (mut ui, mut commands) = ui();
        let id = running(&mut ui);
        let _ = commands.try_recv();

        ui.cancel_query();
        ui.cancel_query();
        assert!(matches!(commands.try_recv(), Ok(Command::Cancel { id: got }) if got == id));
        assert!(commands.try_recv().is_err());

        ui.apply(EngineEvent::QueryDone {
            id,
            outcome: Err(DbError::Cancelled),
        });
        assert_eq!(
            ui.sql_query_success_message.as_deref(),
            Some("Query cancelled")
        );
        assert!(ui.sql_query_error.is_none());
    }

    #[test]
    fn events_of_an_abandoned_query_are_ignored() {
        let (mut ui, mut commands) = ui();
        let old = running(&mut ui);
        ui.abandon_query();
        let _ = commands.try_recv();
        assert!(matches!(commands.try_recv(), Ok(Command::Cancel { .. })));

        let new = running(&mut ui);
        ui.apply(EngineEvent::Columns {
            id: old,
            columns: vec![ColumnMeta::new("stale", "")],
        });
        ui.apply(EngineEvent::QueryDone {
            id: old,
            outcome: Ok(QueryStats::default()),
        });

        assert!(ui.sql_query_result.columns().is_empty());
        assert_eq!(ui.running_query.as_ref().map(|q| q.id), Some(new));
    }

    #[test]
    fn only_one_query_runs_at_a_time() {
        let (mut ui, mut commands) = ui();
        running(&mut ui);
        ui.run_query();

        assert!(matches!(commands.try_recv(), Ok(Command::Run { .. })));
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(format_duration(Duration::from_millis(42)), "42 ms");
        assert_eq!(format_duration(Duration::from_millis(1250)), "1.2 s");
    }
}
