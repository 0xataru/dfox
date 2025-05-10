use std::{collections::HashMap, sync::Arc};

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dfox_core::{models::schema::TableSchema, DbManager};
use ratatui::{backend::CrosstermBackend, Terminal};
use serde_json::Value;
use std::io;

use super::{UIHandler, UIRenderer};

pub struct DatabaseClientUI {
    pub db_manager: Arc<DbManager>,
    pub connection_input: ConnectionInput,
    pub current_screen: ScreenState,
    pub selected_db_type: usize,
    pub selected_database: usize,
    pub databases: Vec<String>,
    pub current_focus: FocusedWidget,
    pub selected_table: usize,
    pub tables: Vec<String>,
    pub sql_editor_content: String,
    pub sql_query_result: Vec<HashMap<String, Value>>,
    pub expanded_table: Option<usize>,
    pub table_schemas: HashMap<String, TableSchema>,
    pub sql_query_error: Option<String>,
    pub sql_query_success_message: Option<String>,
    pub connection_error_message: Option<String>,
}

pub enum InputField {
    Username,
    Password,
    Hostname,
    Port,
}

pub struct ConnectionInput {
    pub username: String,
    pub password: String,
    pub hostname: String,
    pub port: String,
    pub current_field: InputField,
}

impl ConnectionInput {
    pub fn new() -> Self {
        Self {
            username: String::new(),
            password: String::new(),
            hostname: String::new(),
            port: String::new(),
            current_field: InputField::Username,
        }
    }
}

pub enum ScreenState {
    DbTypeSelection,
    DatabaseSelection,
    ConnectionInput,
    TableView,
    MessagePopup,
}

#[derive(Clone, PartialEq)]
pub enum FocusedWidget {
    TablesList,
    SqlEditor,
    _QueryResult,
}

#[derive(Debug, Clone)]
pub enum DatabaseType {
    Postgres,
    MySQL,
    SQLite,
}

impl DatabaseType {
    pub fn as_str(&self) -> &str {
        match self {
            DatabaseType::Postgres => "Postgres",
            DatabaseType::MySQL => "MySQL",
            DatabaseType::SQLite => "SQLite",
        }
    }
}

impl DatabaseClientUI {
    pub fn new(db_manager: Arc<DbManager>) -> Self {
        Self {
            db_manager,
            connection_input: ConnectionInput::new(),
            current_screen: ScreenState::DbTypeSelection,
            selected_db_type: 0,
            selected_database: 0,
            databases: Vec::new(),
            current_focus: FocusedWidget::TablesList,
            selected_table: 0,
            tables: Vec::new(),
            sql_editor_content: String::new(),
            sql_query_result: Vec::new(),
            expanded_table: None,
            table_schemas: HashMap::new(),
            sql_query_error: None,
            sql_query_success_message: None,
            connection_error_message: None,
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

    pub async fn run_ui(&mut self) -> Result<(), io::Error> {
        let _guard = TerminalGuard;
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.ui_loop(&mut terminal).await;

        terminal.show_cursor()?;

        result
    }

    async fn ui_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        loop {
            match self.current_screen {
                ScreenState::DbTypeSelection => {
                    UIRenderer::render_db_type_selection_screen(self, terminal).await?
                }
                ScreenState::MessagePopup => self.render_message_popup(terminal).await?,
                ScreenState::ConnectionInput => {
                    UIRenderer::render_connection_input_screen(self, terminal).await?
                }
                ScreenState::DatabaseSelection => {
                    UIRenderer::render_database_selection_screen(self, terminal).await?
                }
                ScreenState::TableView => {
                    UIRenderer::render_table_view_screen(self, terminal).await?
                }
            }

            if let Event::Key(key) = event::read()? {
                match self.current_screen {
                    ScreenState::DbTypeSelection => {
                        UIHandler::handle_db_type_selection_input(self, key.code).await;
                    }
                    ScreenState::MessagePopup => {
                        UIHandler::handle_message_popup_input(self).await;
                    }

                    ScreenState::ConnectionInput => {
                        UIHandler::handle_input_event(self, key.code).await?;
                    }
                    ScreenState::DatabaseSelection => {
                        UIHandler::handle_database_selection_input(self, key.code).await?;
                    }
                    ScreenState::TableView => {
                        if key.code == KeyCode::Esc {
                            return Ok(());
                        }

                        if let FocusedWidget::SqlEditor = self.current_focus {
                            UIHandler::handle_sql_editor_input(
                                self,
                                key.code,
                                key.modifiers,
                                terminal,
                            )
                            .await;
                        } else {
                            UIHandler::handle_table_view_input(self, key.code, terminal).await;
                        }
                    }
                }
            }
        }
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
    }
}
