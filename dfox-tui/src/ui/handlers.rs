use std::{
    io::{self, stdout},
    process,
};

use crossterm::{
    event::{KeyCode, KeyModifiers, DisableMouseCapture},
    execute,
    terminal::{disable_raw_mode, LeaveAlternateScreen, Clear, ClearType},
};
use ratatui::{prelude::CrosstermBackend, Terminal};

use crate::db::{DatabaseUI, postgres::PostgresDatabaseUI, mysql::MySqlDatabaseUI};
use dfox_core::errors::DbError;
use dfox_core::models::schema::TableSchema;

use super::{
    components::{FocusedWidget, InputField, ScreenState},
    DatabaseClientUI, UIHandler, UIRenderer,
};

use std::collections::HashMap;

impl UIHandler for DatabaseClientUI {
    async fn handle_message_popup_input(&mut self) {
        self.current_screen = ScreenState::DbTypeSelection
    }

    async fn handle_db_type_selection_input(&mut self, key: KeyCode) {
        match key {
            KeyCode::Up => {
                if self.selected_db_type > 0 {
                    self.selected_db_type -= 1;
                }
            }
            KeyCode::Down => {
                if self.selected_db_type < 2 {
                    self.selected_db_type += 1;
                }
            }
            KeyCode::Enter => {
                if self.selected_db_type == 2 {
                    self.current_screen = ScreenState::MessagePopup;
                } else {
                    self.current_screen = ScreenState::ConnectionInput;
                }
            }
            KeyCode::Char('q') => {
                disable_raw_mode().unwrap();
                execute!(
                    stdout(),
                    LeaveAlternateScreen,
                    DisableMouseCapture,
                    Clear(ClearType::All)
                ).unwrap();
                process::exit(0);
            }
            _ => {}
        }
    }

    async fn handle_input_event(&mut self, key: KeyCode) -> io::Result<()> {
        if let Some(_error_message) = &self.connection_error_message {
            match key {
                KeyCode::Enter | KeyCode::Esc => {
                    self.connection_error_message = None;
                }
                _ => {}
            }
        } else {
            match key {
                KeyCode::Esc => {
                    self.current_screen = ScreenState::DbTypeSelection;
                }
                KeyCode::Up => {
                    self.connection_input.current_field = match self.connection_input.current_field
                    {
                        InputField::Port => InputField::Hostname,
                        InputField::Hostname => InputField::Password,
                        InputField::Password => InputField::Username,
                        InputField::Username => InputField::Username,
                    };
                }
                KeyCode::Down => {
                    self.connection_input.current_field = match self.connection_input.current_field
                    {
                        InputField::Username => InputField::Password,
                        InputField::Password => InputField::Hostname,
                        InputField::Hostname => InputField::Port,
                        InputField::Port => InputField::Port,
                    };
                }
                _ => match self.connection_input.current_field {
                    InputField::Username => match key {
                        KeyCode::Char(c) => self.connection_input.username.push(c),
                        KeyCode::Backspace => {
                            self.connection_input.username.pop();
                        }
                        KeyCode::Enter => {
                            self.connection_input.current_field = InputField::Password;
                        }
                        _ => {}
                    },
                    InputField::Password => match key {
                        KeyCode::Char(c) => self.connection_input.password.push(c),
                        KeyCode::Backspace => {
                            self.connection_input.password.pop();
                        }
                        KeyCode::Enter => {
                            self.connection_input.current_field = InputField::Hostname;
                        }
                        _ => {}
                    },
                    InputField::Hostname => match key {
                        KeyCode::Char(c) => self.connection_input.hostname.push(c),
                        KeyCode::Backspace => {
                            self.connection_input.hostname.pop();
                        }
                        KeyCode::Enter => {
                            self.connection_input.current_field = InputField::Port;
                        }
                        _ => {}
                    },
                    InputField::Port => match key {
                        KeyCode::Char(c) => self.connection_input.port.push(c),
                        KeyCode::Backspace => {
                            self.connection_input.port.pop();
                        }
                        KeyCode::Enter => {
                            let result = match self.selected_db_type {
                                0 => PostgresDatabaseUI::new(self.clone()).connect_to_default_db().await,
                                1 => MySqlDatabaseUI::new(self.clone()).connect_to_default_db().await,
                                _ => Ok(()),
                            };
                            if result.is_ok() {
                                self.current_screen = ScreenState::DatabaseSelection;
                            }
                        }
                        _ => {}
                    },
                },
            }
        }
        Ok(())
    }

    async fn handle_database_selection_input(&mut self, key: KeyCode) -> io::Result<()> {
        match key {
            KeyCode::Up => {
                if self.selected_database > 0 {
                    self.selected_database -= 1;
                    if self.selected_database < self.databases_scroll {
                        self.databases_scroll = self.selected_database;
                    }
                }
            }
            KeyCode::Down => {
                if !self.databases.is_empty() && self.selected_database < self.databases.len() - 1 {
                    self.selected_database += 1;
                    let visible_height = 20; // Примерная высота видимой области
                    if self.selected_database >= self.databases_scroll + visible_height {
                        self.databases_scroll = self.selected_database - visible_height + 1;
                    }
                }
            }
            KeyCode::Enter => {
                let cloned = self.databases.clone();
                if let Some(db_name) = cloned.get(self.selected_database) {
                    let result = match self.selected_db_type {
                        0 => PostgresDatabaseUI::new(self.clone()).connect_to_selected_db(db_name).await,
                        1 => MySqlDatabaseUI::new(self.clone()).connect_to_selected_db(db_name).await,
                        _ => Ok(()),
                    };
                    if let Err(err) = result {
                        eprintln!("Error connecting to database: {}", err);
                    } else {
                        self.needs_tables_refresh = true;
                        self.current_screen = ScreenState::TableView;
                    }
                }
            }
            KeyCode::Char('q') => {
                disable_raw_mode()?;
                execute!(
                    stdout(),
                    LeaveAlternateScreen,
                    DisableMouseCapture,
                    Clear(ClearType::All)
                )?;
                process::exit(0);
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_table_view_input(
        &mut self,
        key: KeyCode,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) {
        match key {
            KeyCode::F(1) => {
                self.current_screen = ScreenState::DatabaseSelection;
                self.sql_editor_content.clear();
                self.sql_query_result.clear();
                if let Err(err) = UIRenderer::render_database_selection_screen(self, terminal).await
                {
                    eprintln!("Error rendering database selection screen: {}", err);
                }
            }
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::Up => {
                if let FocusedWidget::TablesList = self.current_focus {
                    self.move_selection_up();
                } else if let FocusedWidget::_QueryResult = self.current_focus {
                    self.scroll_sql_result_up();
                }
            }
            KeyCode::Down => {
                if let FocusedWidget::TablesList = self.current_focus {
                    self.move_selection_down();
                } else if let FocusedWidget::_QueryResult = self.current_focus {
                    self.scroll_sql_result_down();
                }
            }
            KeyCode::Left => {
                if let FocusedWidget::_QueryResult = self.current_focus {
                    self.scroll_sql_result_left();
                }
            }
            KeyCode::Right => {
                if let FocusedWidget::_QueryResult = self.current_focus {
                    self.scroll_sql_result_right();
                }
            }
            KeyCode::Enter => {
                if let FocusedWidget::TablesList = self.current_focus {
                    if self.tables.is_empty() {
                        println!("No tables available.");
                        return;
                    }

                    if self.selected_table < self.tables.len() {
                        let selected_table = self.tables[self.selected_table].clone();

                        if Some(self.selected_table) == self.expanded_table {
                            self.expanded_table = None;
                        } else {
                            let result = match self.selected_db_type {
                                0 => PostgresDatabaseUI::new(self.clone()).describe_table(&selected_table).await,
                                1 => MySqlDatabaseUI::new(self.clone()).describe_table(&selected_table).await,
                                _ => Err(DbError::Connection("Unsupported database type".to_string())),
                            };

                            match result {
                                Ok(columns) => {
                                    let table_schema = TableSchema {
                                        table_name: selected_table.clone(),
                                        columns: columns.into_iter().map(|name| dfox_core::models::schema::ColumnSchema {
                                            name,
                                            data_type: String::new(),
                                            is_nullable: true,
                                            default: None,
                                        }).collect(),
                                        indexes: Vec::new(),
                                    };
                                    let table_schema_clone = table_schema.clone();
                                    self.table_schemas.insert(
                                        selected_table.clone(),
                                        table_schema_clone,
                                    );
                                    self.expanded_table = Some(self.selected_table);

                                    if let Err(err) = UIRenderer::render_table_schema(
                                        self,
                                        terminal,
                                        &table_schema,
                                    )
                                    .await
                                    {
                                        eprintln!("Error rendering table schema: {}", err);
                                    }
                                }
                                Err(err) => {
                                    eprintln!("Error describing table: {}", err);
                                }
                            }
                        }
                    } else {
                        eprintln!("Selected table index out of bounds.");
                    }
                }
            }
            KeyCode::Esc => {
                terminal.clear().unwrap();
                terminal.show_cursor().unwrap();
                execute!(
                    terminal.backend_mut(),
                    LeaveAlternateScreen,
                    DisableMouseCapture,
                    Clear(ClearType::All)
                ).unwrap();
                process::exit(0);
            }
            _ => {}
        }
    }

    async fn handle_sql_editor_input(
        &mut self,
        key: KeyCode,
        modifiers: KeyModifiers,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) {
        match (key, modifiers) {
            (KeyCode::Tab, _) => self.cycle_focus(),
            (KeyCode::F(5), _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                if !self.sql_editor_content.is_empty() {
                    self.sql_query_error = None;
                    let sql_content = self.sql_editor_content.clone();
                    let result = match self.selected_db_type {
                        0 => PostgresDatabaseUI::new(self.clone()).execute_sql_query(&sql_content).await,
                        1 => MySqlDatabaseUI::new(self.clone()).execute_sql_query(&sql_content).await,
                        _ => Err(DbError::Connection("Unsupported database type".to_string())),
                    };
                    match result {
                        Ok((result, success_message)) => {
                            self.sql_query_result = result.into_iter()
                                .map(|row| {
                                    let mut map = HashMap::new();
                                    map.insert("value".to_string(), row);
                                    map
                                })
                                .collect();
                            self.sql_query_success_message = Some(success_message);
                            self.sql_query_error = None;
                            self.needs_tables_refresh = true;
                        }
                        Err(err) => {
                            self.sql_query_error = Some(err.to_string());
                            self.sql_query_result.clear();
                        }
                    }
                    self.sql_editor_content.clear();
                }

                let _ = match self.selected_db_type {
                    0 => PostgresDatabaseUI::new(self.clone()).update_tables().await,
                    1 => MySqlDatabaseUI::new(self.clone()).update_tables().await,
                    _ => Ok(()),
                };
            }
            (KeyCode::Enter, _) => {
                self.sql_editor_content.push('\n');
            }
            (KeyCode::Char(c), _) => {
                self.sql_editor_content.push(c);
            }
            (KeyCode::Backspace, _) => {
                self.sql_editor_content.pop();
            }
            (KeyCode::F(1), _) => {
                self.current_screen = ScreenState::DatabaseSelection;
                self.sql_editor_content.clear();
                self.sql_query_result.clear();
                if let Err(err) = UIRenderer::render_database_selection_screen(self, terminal).await
                {
                    eprintln!("Error rendering database selection screen: {}", err);
                }
                return;
            }
            _ => {}
        }
        if let Err(err) = UIRenderer::render_table_view_screen(self, terminal).await {
            eprintln!("Error rendering UI: {}", err);
        }
    }
}

impl DatabaseClientUI {
    pub fn cycle_focus(&mut self) {
        self.current_focus = match self.current_focus {
            FocusedWidget::TablesList => FocusedWidget::SqlEditor,
            FocusedWidget::SqlEditor => FocusedWidget::_QueryResult,
            FocusedWidget::_QueryResult => FocusedWidget::TablesList,
        };
    }

    pub fn move_selection_up(&mut self) {
        if self.selected_table > 0 {
            self.selected_table -= 1;
            if self.selected_table < self.tables_scroll {
                self.tables_scroll = self.selected_table;
            }
        }
    }

    pub fn move_selection_down(&mut self) {
        if self.selected_table < self.tables.len().saturating_sub(1) {
            self.selected_table += 1;
            let visible_height = 50; // Примерная высота видимой области
            if self.selected_table >= self.tables_scroll + visible_height {
                self.tables_scroll = self.selected_table - visible_height + 1;
            }
        }
    }

    pub fn scroll_sql_result_up(&mut self) {
        if self.sql_result_scroll > 0 {
            self.sql_result_scroll -= 1;
        }
    }

    pub fn scroll_sql_result_down(&mut self) {
        let visible_height = 20; // Примерная высота видимой области
        if self.sql_result_scroll + visible_height < self.sql_query_result.len() {
            self.sql_result_scroll += 1;
        }
    }

    pub fn scroll_sql_result_left(&mut self) {
        if self.sql_result_horizontal_scroll > 0 {
            self.sql_result_horizontal_scroll -= 1;
        }
    }

    pub fn scroll_sql_result_right(&mut self) {
        let visible_width = 80; // Примерная ширина видимой области
        if self.sql_result_horizontal_scroll + visible_width < 200 { // Максимальная ширина
            self.sql_result_horizontal_scroll += 1;
        }
    }
} 