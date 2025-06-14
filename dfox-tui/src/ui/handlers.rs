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
    components::{FocusedWidget, InputField, ScreenState, MAX_VISIBLE_COLUMNS},
    DatabaseClientUI, UIHandler, UIRenderer,
};

use arboard::Clipboard;
use indexmap::IndexMap;

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
                if let Err(e) = disable_raw_mode() {
                    log::error!("Error disabling raw mode: {}", e);
                }
                if let Err(e) = execute!(
                    stdout(),
                    LeaveAlternateScreen,
                    DisableMouseCapture,
                    Clear(ClearType::All)
                ) {
                    log::error!("Error cleaning up terminal: {}", e);
                }
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
                    let visible_height = 20; 
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
                        log::error!("Error connecting to database: {}", err);
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
                    log::error!("Error rendering database selection screen: {}", err);
                }
            }
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::Up => {
                if let FocusedWidget::TablesList = self.current_focus {
                    self.move_selection_up();
                } else if let FocusedWidget::_QueryResult = self.current_focus {
                    if !self.sql_query_result.is_empty() && self.selected_result_row > 0 {
                        self.selected_result_row -= 1;
                        if self.selected_result_row < self.sql_result_scroll {
                            self.sql_result_scroll = self.selected_result_row;
                        }
                    }
                    self.sync_cursor_position();
                }
            }
            KeyCode::Down => {
                if let FocusedWidget::TablesList = self.current_focus {
                    self.move_selection_down();
                } else if let FocusedWidget::_QueryResult = self.current_focus {
                    if !self.sql_query_result.is_empty() && self.selected_result_row < self.sql_query_result.len().saturating_sub(1) {
                        self.selected_result_row += 1;
                        let visible_height = 20; // Adjust based on terminal size
                        if self.selected_result_row >= self.sql_result_scroll + visible_height {
                            self.sql_result_scroll = self.selected_result_row.saturating_sub(visible_height - 1);
                        }
                    }
                    self.sync_cursor_position();
                }
            }
            KeyCode::Left => {
                if self.current_focus == FocusedWidget::_QueryResult && !self.sql_query_result.is_empty() {
                    if self.sql_result_horizontal_scroll > 0 {
                        self.sql_result_horizontal_scroll = self.sql_result_horizontal_scroll.saturating_sub(1);
                        self.add_debug_info(format!("Horizontal scroll left to: {}", self.sql_result_horizontal_scroll));
                    }
                } else if self.current_focus == FocusedWidget::SqlEditor {
                    if self.sql_editor_cursor_x > 0 {
                        self.sql_editor_cursor_x -= 1;
                    }
                    self.sync_cursor_position();
                }
            }
            KeyCode::Right => {
                if self.current_focus == FocusedWidget::_QueryResult && !self.sql_query_result.is_empty() {
                    // Get the maximum number of columns and account for limited visible columns
                    let total_columns = if let Some(first_row) = self.sql_query_result.first() {
                        first_row.len()
                    } else {
                        0
                    };
                    
                    let max_visible_columns = MAX_VISIBLE_COLUMNS; // Should match the value in screens.rs
                    let max_scroll = if total_columns > max_visible_columns {
                        total_columns - max_visible_columns
                    } else {
                        0
                    };
                    
                    if self.sql_result_horizontal_scroll < max_scroll {
                        self.sql_result_horizontal_scroll += 1;
                        self.add_debug_info(format!("Horizontal scroll right to: {} (max: {})", 
                            self.sql_result_horizontal_scroll, max_scroll));
                    } else {
                        self.add_debug_info(format!("Already at rightmost position: {} columns, {} visible", 
                            total_columns, max_visible_columns));
                    }
                } else if self.current_focus == FocusedWidget::SqlEditor {
                    let current_line = self.sql_editor_content
                        .lines()
                        .nth(self.sql_editor_cursor_y)
                        .unwrap_or("");
                    if self.sql_editor_cursor_x < current_line.len() {
                        self.sql_editor_cursor_x += 1;
                    }
                    self.sync_cursor_position();
                }
            }
            KeyCode::PageUp => {
                if matches!(self.current_focus, FocusedWidget::_QueryResult) && !self.sql_query_result.is_empty() {
                    let page_size = 10;
                    if self.selected_result_row >= page_size {
                        self.selected_result_row -= page_size;
                    } else {
                        self.selected_result_row = 0;
                    }
                    self.sql_result_scroll = self.selected_result_row;
                    self.sync_cursor_position();
                }
            }
            KeyCode::PageDown => {
                if matches!(self.current_focus, FocusedWidget::_QueryResult) && !self.sql_query_result.is_empty() {
                    let page_size = 10;
                    let max_row = self.sql_query_result.len().saturating_sub(1);
                    if self.selected_result_row + page_size <= max_row {
                        self.selected_result_row += page_size;
                    } else {
                        self.selected_result_row = max_row;
                    }
                    let visible_height = 20;
                    if self.selected_result_row >= self.sql_result_scroll + visible_height {
                        self.sql_result_scroll = self.selected_result_row.saturating_sub(visible_height - 1);
                    }
                    self.sync_cursor_position();
                }
            }
            KeyCode::Home => {
                if matches!(self.current_focus, FocusedWidget::_QueryResult) && !self.sql_query_result.is_empty() {
                    self.selected_result_row = 0;
                    self.sql_result_scroll = 0;
                    self.sql_result_horizontal_scroll = 0;
                    self.sync_cursor_position();
                }
            }
            KeyCode::End => {
                if matches!(self.current_focus, FocusedWidget::_QueryResult) && !self.sql_query_result.is_empty() {
                    self.selected_result_row = self.sql_query_result.len().saturating_sub(1);
                    let visible_height = 20;
                    self.sql_result_scroll = self.selected_result_row.saturating_sub(visible_height - 1);
                    self.sync_cursor_position();
                }
            }
            KeyCode::Enter => {
                if let FocusedWidget::TablesList = self.current_focus {
                    if self.tables.is_empty() {
                        log::error!("No tables available.");
                        return;
                    }

                    if let Some(selected_table) = self.tables.get(self.selected_table) {
                        let selected_table = selected_table.clone();

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
                                        log::error!("Error rendering table schema: {}", err);
                                    }
                                }
                                Err(err) => {
                                    log::error!("Error describing table: {}", err);
                                }
                            }
                        }
                    } else {
                        log::error!("Selected table index out of bounds.");
                    }
                }
            }
            KeyCode::Esc => {
                if let Err(e) = terminal.clear() {
                    log::error!("Error clearing terminal: {}", e);
                }
                if let Err(e) = terminal.show_cursor() {
                    log::error!("Error showing cursor: {}", e);
                }
                if let Err(e) = execute!(
                    terminal.backend_mut(),
                    LeaveAlternateScreen,
                    DisableMouseCapture,
                    Clear(ClearType::All)
                ) {
                    log::error!("Error cleaning up terminal: {}", e);
                }
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
                            if !result.is_empty() {
                                if let Some(first_row) = result.first() {
                                    // Debug: print first few characters to understand the format
                                    let debug_len = std::cmp::min(100, first_row.len());
                                    let debug_slice = if first_row.len() >= debug_len {
                                        &first_row[..debug_len]
                                    } else {
                                        first_row
                                    };
                                    self.add_debug_info(format!("First row sample: {:?}", debug_slice));
                                    
                                    let headers: Vec<String> = first_row
                                        .split('\t')
                                        .map(|s| s.trim().to_string())
                                        .filter(|s| !s.is_empty())
                                        .collect();
                                    
                                    // Debug: print headers
                                    self.add_debug_info(format!("Headers found: {:?}", headers));
                                    
                                    // Debug: check if first result preserves order
                                    if !result.is_empty() && result.len() > 1 {
                                        let first_data_row = &result[1];
                                        let first_values: Vec<&str> = first_data_row.split('\t').collect();
                                        self.add_debug_info(format!("First data values (first 5): {:?}", 
                                            first_values.iter().take(5).collect::<Vec<_>>()));
                                    }
                                    
                                    // Debug: show sample of raw data
                                    if result.len() > 1 {
                                        let sample_row = &result[1]; // First data row
                                        let sample_len = std::cmp::min(200, sample_row.len());
                                        let sample_slice = if sample_row.len() >= sample_len {
                                            &sample_row[..sample_len]
                                        } else {
                                            sample_row
                                        };
                                        self.add_debug_info(format!("Sample data row: {:?}", sample_slice));
                                        
                                        // Debug: show character codes for first few characters
                                        let char_codes: Vec<u32> = sample_row.chars().take(20).map(|c| c as u32).collect();
                                        self.add_debug_info(format!("First 20 char codes: {:?}", char_codes));
                                    }
                                    
                                    // Limit the number of rows to prevent memory issues
                                    let max_rows = 1000; // Limit to 1k rows for better performance
                                    let (limited_result, success_msg) = if result.len() > max_rows + 1 {
                                        let limited = result.into_iter().take(max_rows + 1).collect::<Vec<_>>();
                                        (limited, format!("Results limited to {} rows for performance", max_rows))
                                    } else {
                                        (result, success_message)
                                    };
                                    
                                    self.sql_query_result = limited_result
                                        .into_iter()
                                        .skip(1)
                                        .enumerate()
                                        .filter_map(|(row_idx, row)| {
                                            let values: Vec<&str> = row.split('\t').collect();
                                            if values.len() >= headers.len() {
                                                let mut map = IndexMap::new();
                                                // Insert in the same order as headers appear in SQL result
                                                for (i, header) in headers.iter().enumerate() {
                                                    if let Some(value) = values.get(i) {
                                                        // Try multiple cleaning strategies
                                                        let cleaned_value = if value.chars().any(|c| (c as u32) < 32 && c != '\t' && c != '\n') {
                                                            // Strategy 1: Remove only control characters (except tab/newline)
                                                            value
                                                                .chars()
                                                                .filter(|c| (*c as u32) >= 32 || *c == '\t' || *c == '\n')
                                                                .collect::<String>()
                                                                .trim()
                                                                .to_string()
                                                        } else {
                                                            // Strategy 2: Keep all printable characters including Unicode
                                                            value
                                                                .chars()
                                                                .filter(|c| {
                                                                    // Keep all printable characters, including Unicode (Cyrillic, emojis, etc.)
                                                                    !c.is_control() || *c == '\t' || *c == '\n'
                                                                })
                                                                .collect::<String>()
                                                                .trim()
                                                                .to_string()
                                                        };
                                                        
                                                        // Replace empty values with NULL
                                                        let final_value = if cleaned_value.is_empty() {
                                                            "NULL".to_string()
                                                        } else {
                                                            cleaned_value
                                                        };
                                                        
                                                        // Insert in order - this preserves the SQL column order
                                                        map.insert(header.clone(), final_value);
                                                    }
                                                }
                                                Some(map)
                                            } else {
                                                log::warn!("Row {} has {} values but {} headers expected", row_idx, values.len(), headers.len());
                                                None
                                            }
                                        })
                                        .collect();
                                        
                                    self.add_debug_info(format!("Processed {} rows successfully", self.sql_query_result.len()));
                                    
                                    // Debug: check order in first IndexMap
                                    if let Some(first_map) = self.sql_query_result.first() {
                                        let map_keys: Vec<String> = first_map.keys().cloned().collect();
                                        self.add_debug_info(format!("IndexMap keys order: {:?}", map_keys.iter().take(5).collect::<Vec<_>>()));
                                    }
                                    
                                    self.sql_query_success_message = Some(success_msg);
                                } else {
                                    self.sql_query_result = Vec::new();
                                    self.sql_query_success_message = Some("Empty result set".to_string());
                                }
                            } else {
                                self.sql_query_result = Vec::new();
                                self.sql_query_success_message = Some(success_message);
                            }
                            self.sql_query_error = None;
                            self.needs_tables_refresh = true;
                            // Reset result navigation state
                            self.selected_result_row = 0;
                            self.sql_result_scroll = 0;
                            self.sql_result_horizontal_scroll = 0;
                        }
                        Err(err) => {
                            self.sql_query_error = Some(format!("SQL Error: {}", err));
                            self.sql_query_result.clear();
                            // Reset result navigation state
                            self.selected_result_row = 0;
                            self.sql_result_scroll = 0;
                            self.sql_result_horizontal_scroll = 0;
                        }
                    }
                    
                    // Don't clear the SQL content after execution
                    // self.sql_editor_content.clear(); // Commented out
                }

                // Safely update tables without crashing
                let _: Result<(), ()> = match self.selected_db_type {
                    0 => {
                        if let Ok(db_ui) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            PostgresDatabaseUI::new(self.clone())
                        })) {
                            let _ = db_ui.update_tables().await;
                        }
                        Ok(())
                    },
                    1 => {
                        if let Ok(db_ui) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            MySqlDatabaseUI::new(self.clone())
                        })) {
                            let _ = db_ui.update_tables().await;
                        }
                        Ok(())
                    },
                    _ => Ok(()),
                };
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if let FocusedWidget::_QueryResult = self.current_focus {
                    if !self.sql_query_result.is_empty() {
                        // Get headers from IndexMap which preserves insertion order
                        let headers = if let Some(first_result) = self.sql_query_result.first() {
                            first_result.keys().cloned().collect::<Vec<String>>()
                        } else {
                            Vec::new()
                        };

                        let mut clipboard_content = String::new();
                        
                        // Add headers
                        clipboard_content.push_str(&headers.join("\t"));
                        clipboard_content.push('\n');

                        // Add selected row if it exists
                        if let Some(row) = self.sql_query_result.get(self.selected_result_row) {
                            let mut row_values = Vec::new();
                            for header in &headers {
                                row_values.push(
                                    row.get(header)
                                        .map_or("NULL".to_string(), |v| v.to_string())
                                );
                            }
                            clipboard_content.push_str(&row_values.join("\t"));
                            clipboard_content.push('\n');
                        }

                        if let Err(e) = Clipboard::new().and_then(|mut ctx| ctx.set_text(clipboard_content)) {
                            log::error!("Error copying to clipboard: {}", e);
                        }
                    }
                }
            }
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                if let FocusedWidget::_QueryResult = self.current_focus {
                    if !self.sql_query_result.is_empty() {
                        // Get headers from IndexMap which preserves insertion order
                        let headers = if let Some(first_result) = self.sql_query_result.first() {
                            first_result.keys().cloned().collect::<Vec<String>>()
                        } else {
                            Vec::new()
                        };

                        let mut clipboard_content = String::new();
                        
                        // Add headers
                        clipboard_content.push_str(&headers.join("\t"));
                        clipboard_content.push('\n');

                        // Add all rows
                        for row in &self.sql_query_result {
                            let mut row_values = Vec::new();
                            for header in &headers {
                                row_values.push(
                                    row.get(header)
                                        .map_or("NULL".to_string(), |v| v.to_string())
                                );
                            }
                            clipboard_content.push_str(&row_values.join("\t"));
                            clipboard_content.push('\n');
                        }

                        if let Err(e) = Clipboard::new().and_then(|mut ctx| ctx.set_text(clipboard_content)) {
                            log::error!("Error copying to clipboard: {}", e);
                        }
                    }
                }
            }
            (KeyCode::Enter, _) => {
                if let FocusedWidget::SqlEditor = self.current_focus {
                    self.sql_editor_content.push('\n');
                    self.sql_editor_cursor_y += 1;
                    self.sql_editor_cursor_x = 0;
                    self.sync_cursor_position();
                }
            }
            (KeyCode::Char(c), _) => {
                if let FocusedWidget::SqlEditor = self.current_focus {
                    self.sql_editor_content.push(c);
                    self.sql_editor_cursor_x += 1;
                    self.sync_cursor_position();
                }
            }
            (KeyCode::Backspace, _) => {
                if let FocusedWidget::SqlEditor = self.current_focus {
                    if !self.sql_editor_content.is_empty() {
                        self.sql_editor_content.pop();
                        if self.sql_editor_cursor_x > 0 {
                            self.sql_editor_cursor_x -= 1;
                        } else if self.sql_editor_cursor_y > 0 {
                            self.sql_editor_cursor_y -= 1;
                            // Calculate cursor position for previous line
                            let lines: Vec<&str> = self.sql_editor_content.split('\n').collect();
                            if let Some(prev_line) = lines.get(self.sql_editor_cursor_y) {
                                self.sql_editor_cursor_x = prev_line.len();
                            } else {
                                self.sql_editor_cursor_x = 0;
                            }
                        } else {
                            // At the beginning of text, just stay at position 0,0
                            self.sql_editor_cursor_x = 0;
                            self.sql_editor_cursor_y = 0;
                        }
                        self.sync_cursor_position();
                    }
                }
            }
            (KeyCode::Left, _) => {
                if matches!(self.current_focus, FocusedWidget::SqlEditor) && self.sql_editor_cursor_x > 0 {
                    self.sql_editor_cursor_x -= 1;
                }
            }
            (KeyCode::Right, _) => {
                if let FocusedWidget::SqlEditor = self.current_focus {
                    let lines: Vec<&str> = self.sql_editor_content.split('\n').collect();
                    if let Some(current_line) = lines.get(self.sql_editor_cursor_y) {
                        if self.sql_editor_cursor_x < current_line.len() {
                            self.sql_editor_cursor_x += 1;
                        }
                    }
                }
            }
            (KeyCode::Up, _) => {
                if matches!(self.current_focus, FocusedWidget::SqlEditor) && self.sql_editor_cursor_y > 0 {
                    self.sql_editor_cursor_y -= 1;
                    let lines: Vec<&str> = self.sql_editor_content.split('\n').collect();
                    if let Some(line) = lines.get(self.sql_editor_cursor_y) {
                        self.sql_editor_cursor_x = std::cmp::min(self.sql_editor_cursor_x, line.len());
                    }
                }
            }
            (KeyCode::Down, _) => {
                if let FocusedWidget::SqlEditor = self.current_focus {
                    let lines: Vec<&str> = self.sql_editor_content.split('\n').collect();
                    if self.sql_editor_cursor_y < lines.len().saturating_sub(1) {
                        self.sql_editor_cursor_y += 1;
                        if let Some(line) = lines.get(self.sql_editor_cursor_y) {
                            self.sql_editor_cursor_x = std::cmp::min(self.sql_editor_cursor_x, line.len());
                        }
                    }
                }
            }
            (KeyCode::F(1), _) => {
                self.current_screen = ScreenState::DatabaseSelection;
                self.sql_editor_content.clear();
                self.sql_query_result.clear();
                if let Err(err) = UIRenderer::render_database_selection_screen(self, terminal).await
                {
                    log::error!("Error rendering database selection screen: {}", err);
                }
                return;
            }
            (KeyCode::F(12), _) => {
                // Show debug information in SQL query result area
                if !self.debug_info.is_empty() {
                    self.sql_query_result.clear();
                    for (i, debug_msg) in self.debug_info.iter().enumerate() {
                        let mut debug_row = IndexMap::new();
                        debug_row.insert("Debug Info".to_string(), debug_msg.clone());
                        debug_row.insert("#".to_string(), (i + 1).to_string());
                        self.sql_query_result.push(debug_row);
                    }
                    self.sql_query_success_message = Some(format!("Debug info ({} messages) - Press F12 again to clear", self.debug_info.len()));
                    self.selected_result_row = 0;
                    self.sql_result_scroll = 0;
                } else {
                    // Clear debug display and restore normal result
                    self.sql_query_result.clear();
                    self.sql_query_success_message = Some("No debug information available".to_string());
                }
            }
            _ => {}
        }
        if let Err(err) = UIRenderer::render_table_view_screen(self, terminal).await {
            log::error!("Error rendering UI: {}", err);
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
            let visible_height = 50; 
            if self.selected_table >= self.tables_scroll + visible_height {
                self.tables_scroll = self.selected_table - visible_height + 1;
            }
        }
    }

    pub fn sync_cursor_position(&mut self) {
        let lines: Vec<&str> = self.sql_editor_content.split('\n').collect();
        
        // Ensure cursor Y is within bounds
        if self.sql_editor_cursor_y >= lines.len() {
            self.sql_editor_cursor_y = lines.len().saturating_sub(1);
        }
        
        // Ensure cursor X is within bounds for current line
        if let Some(current_line) = lines.get(self.sql_editor_cursor_y) {
            if self.sql_editor_cursor_x > current_line.len() {
                self.sql_editor_cursor_x = current_line.len();
            }
        } else {
            // Safety fallback
            self.sql_editor_cursor_x = 0;
            self.sql_editor_cursor_y = 0;
        }
        
        // Additional safety checks for result navigation
        if self.selected_result_row >= self.sql_query_result.len() && !self.sql_query_result.is_empty() {
            self.selected_result_row = self.sql_query_result.len() - 1;
        }
        
        if self.sql_result_scroll > self.selected_result_row {
            self.sql_result_scroll = self.selected_result_row;
        }
        
        // Ensure scroll doesn't go beyond available data
        let max_scroll = self.sql_query_result.len().saturating_sub(1);
        if self.sql_result_scroll > max_scroll {
            self.sql_result_scroll = max_scroll;
        }
    }
} 