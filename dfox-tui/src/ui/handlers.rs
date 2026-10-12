use std::{
    io::{self, stdout},
    process,
};

use crossterm::{
    event::{DisableMouseCapture, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, Clear, ClearType, LeaveAlternateScreen},
};
use ratatui::{prelude::CrosstermBackend, Terminal};

use crate::{
    db::ServerDatabaseUI,
    ui::components::types::{FocusedWidget, ScreenState, MAX_VISIBLE_COLUMNS},
};
use dfox_core::db::QueryOutput;
use dfox_core::errors::DbError;
use dfox_core::models::schema::TableSchema;

use super::{DatabaseClientUI, UIHandler, UIRenderer};

use crate::ui::components::connection_input::InputField;
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
                            let result = match self.server_db_type() {
                                Some(db_type) => {
                                    ServerDatabaseUI::new(self.clone(), db_type)
                                        .connect_to_default_db()
                                        .await
                                }
                                None => Ok(()),
                            };
                            match result {
                                Ok(()) => self.current_screen = ScreenState::DatabaseSelection,
                                Err(err) => {
                                    log::error!("Error connecting: {}", err);
                                    self.connection_error_message = Some(err.to_string());
                                }
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
                    let result = match self.server_db_type() {
                        Some(db_type) => {
                            ServerDatabaseUI::new(self.clone(), db_type)
                                .connect_to_selected_db(db_name)
                                .await
                        }
                        None => Ok(()),
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
        modifiers: KeyModifiers,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) {
        // Handle clipboard operations first for any focus widget
        match (key, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.add_debug_info(format!(
                    "Ctrl+C pressed, current focus: {:?}",
                    self.current_focus
                ));

                if let FocusedWidget::QueryResult = self.current_focus {
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
                                        .map_or("NULL".to_string(), |v| v.to_string()),
                                );
                            }
                            clipboard_content.push_str(&row_values.join("\t"));
                            clipboard_content.push('\n');
                        }

                        // Add debug info about what we're copying
                        self.add_debug_info(format!(
                            "Attempting to copy {} chars to clipboard",
                            clipboard_content.len()
                        ));
                        self.add_debug_info(format!(
                            "Selected row: {}/{}",
                            self.selected_result_row + 1,
                            self.sql_query_result.len()
                        ));
                        self.add_debug_info(format!(
                            "Content preview: {:?}",
                            &clipboard_content[..std::cmp::min(50, clipboard_content.len())]
                        ));

                        match Clipboard::new() {
                            Ok(mut clipboard) => {
                                // First try to clear any existing content
                                let _ = clipboard.set_text("");

                                match clipboard.set_text(clipboard_content.clone()) {
                                    Ok(()) => {
                                        self.add_debug_info(
                                            "Successfully copied to clipboard!".to_string(),
                                        );

                                        // Verify the copy worked by reading it back
                                        match clipboard.get_text() {
                                            Ok(read_back) => {
                                                if read_back == clipboard_content {
                                                    self.add_debug_info(
                                                        "✓ Clipboard verification successful"
                                                            .to_string(),
                                                    );
                                                    self.sql_query_success_message = Some(format!(
                                                        "Copied row {} to clipboard",
                                                        self.selected_result_row + 1
                                                    ));
                                                } else {
                                                    self.add_debug_info(format!("✗ Clipboard verification failed. Expected {} chars, got {} chars", clipboard_content.len(), read_back.len()));
                                                    self.sql_query_error = Some(
                                                        "Clipboard copy verification failed"
                                                            .to_string(),
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                self.add_debug_info(format!(
                                                    "Could not verify clipboard: {}",
                                                    e
                                                ));
                                                self.sql_query_success_message = Some(format!(
                                                    "Copied row {} to clipboard (unverified)",
                                                    self.selected_result_row + 1
                                                ));
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        self.add_debug_info(format!(
                                            "Failed to set clipboard text: {}",
                                            e
                                        ));
                                        log::error!("Error setting clipboard text: {}", e);
                                        // Show error message to user
                                        self.sql_query_error =
                                            Some(format!("Clipboard error: {}", e));
                                    }
                                }
                            }
                            Err(e) => {
                                self.add_debug_info(format!("Failed to create clipboard: {}", e));
                                log::error!("Error creating clipboard: {}", e);
                                // Show error message to user
                                self.sql_query_error =
                                    Some(format!("Cannot access clipboard: {}", e));
                            }
                        }
                    } else {
                        self.add_debug_info("No query results to copy".to_string());
                        self.sql_query_error = Some("No query results to copy".to_string());
                    }
                } else {
                    self.add_debug_info(format!(
                        "Wrong focus for copying. Current focus: {:?}",
                        self.current_focus
                    ));
                    self.sql_query_error =
                        Some("Switch focus to query results (Tab) before copying".to_string());
                }
                return;
            }
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.add_debug_info(format!(
                    "Ctrl+A pressed, current focus: {:?}",
                    self.current_focus
                ));

                if let FocusedWidget::QueryResult = self.current_focus {
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
                                        .map_or("NULL".to_string(), |v| v.to_string()),
                                );
                            }
                            clipboard_content.push_str(&row_values.join("\t"));
                            clipboard_content.push('\n');
                        }

                        // Add debug info about what we're copying
                        self.add_debug_info(format!(
                            "Attempting to copy {} chars ({} rows) to clipboard",
                            clipboard_content.len(),
                            self.sql_query_result.len()
                        ));
                        self.add_debug_info(format!(
                            "Content preview: {:?}",
                            &clipboard_content[..std::cmp::min(100, clipboard_content.len())]
                        ));

                        match Clipboard::new() {
                            Ok(mut clipboard) => {
                                // First try to clear any existing content
                                let _ = clipboard.set_text("");

                                match clipboard.set_text(clipboard_content.clone()) {
                                    Ok(()) => {
                                        self.add_debug_info(
                                            "Successfully copied all results to clipboard!"
                                                .to_string(),
                                        );

                                        // Verify the copy worked by reading it back
                                        match clipboard.get_text() {
                                            Ok(read_back) => {
                                                if read_back == clipboard_content {
                                                    self.add_debug_info(
                                                        "✓ Clipboard verification successful"
                                                            .to_string(),
                                                    );
                                                    self.sql_query_success_message = Some(format!(
                                                        "Copied {} rows to clipboard",
                                                        self.sql_query_result.len()
                                                    ));
                                                } else {
                                                    self.add_debug_info(format!("✗ Clipboard verification failed. Expected {} chars, got {} chars", clipboard_content.len(), read_back.len()));
                                                    self.sql_query_error = Some(
                                                        "Clipboard copy verification failed"
                                                            .to_string(),
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                self.add_debug_info(format!(
                                                    "Could not verify clipboard: {}",
                                                    e
                                                ));
                                                self.sql_query_success_message = Some(format!(
                                                    "Copied {} rows to clipboard (unverified)",
                                                    self.sql_query_result.len()
                                                ));
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        self.add_debug_info(format!(
                                            "Failed to set clipboard text: {}",
                                            e
                                        ));
                                        log::error!("Error setting clipboard text: {}", e);
                                        // Show error message to user
                                        self.sql_query_error =
                                            Some(format!("Clipboard error: {}", e));
                                    }
                                }
                            }
                            Err(e) => {
                                self.add_debug_info(format!("Failed to create clipboard: {}", e));
                                log::error!("Error creating clipboard: {}", e);
                                // Show error message to user
                                self.sql_query_error =
                                    Some(format!("Cannot access clipboard: {}", e));
                            }
                        }
                    } else {
                        self.add_debug_info("No query results to copy".to_string());
                        self.sql_query_error = Some("No query results to copy".to_string());
                    }
                } else {
                    self.add_debug_info(format!(
                        "Wrong focus for copying. Current focus: {:?}",
                        self.current_focus
                    ));
                    self.sql_query_error =
                        Some("Switch focus to query results (Tab) before copying".to_string());
                }
                return;
            }
            _ => {}
        }

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
                } else if let FocusedWidget::QueryResult = self.current_focus {
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
                } else if let FocusedWidget::QueryResult = self.current_focus {
                    if !self.sql_query_result.is_empty()
                        && self.selected_result_row < self.sql_query_result.len().saturating_sub(1)
                    {
                        self.selected_result_row += 1;
                        let visible_height = 20;
                        if self.selected_result_row >= self.sql_result_scroll + visible_height {
                            self.sql_result_scroll =
                                self.selected_result_row.saturating_sub(visible_height - 1);
                        }
                    }
                    self.sync_cursor_position();
                }
            }
            KeyCode::Left => {
                if self.current_focus == FocusedWidget::QueryResult
                    && !self.sql_query_result.is_empty()
                {
                    if self.sql_result_horizontal_scroll > 0 {
                        self.sql_result_horizontal_scroll =
                            self.sql_result_horizontal_scroll.saturating_sub(1);
                    }
                } else if self.current_focus == FocusedWidget::SqlEditor {
                    if self.sql_editor_cursor_x > 0 {
                        self.sql_editor_cursor_x -= 1;
                    }
                    self.sync_cursor_position();
                }
            }
            KeyCode::Right => {
                if self.current_focus == FocusedWidget::QueryResult
                    && !self.sql_query_result.is_empty()
                {
                    // Get the maximum number of columns and account for limited visible columns
                    let total_columns = if let Some(first_row) = self.sql_query_result.first() {
                        first_row.len()
                    } else {
                        0
                    };

                    let max_visible_columns = MAX_VISIBLE_COLUMNS; // Should match the value in screens.rs
                    let max_scroll = total_columns.saturating_sub(max_visible_columns);

                    if self.sql_result_horizontal_scroll < max_scroll {
                        self.sql_result_horizontal_scroll += 1;
                        self.add_debug_info(format!(
                            "Horizontal scroll right to: {} (max: {})",
                            self.sql_result_horizontal_scroll, max_scroll
                        ));
                    } else {
                        self.add_debug_info(format!(
                            "Already at rightmost position: {} columns, {} visible",
                            total_columns, max_visible_columns
                        ));
                    }
                } else if self.current_focus == FocusedWidget::SqlEditor {
                    let current_line = self
                        .sql_editor_content
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
                if matches!(self.current_focus, FocusedWidget::QueryResult)
                    && !self.sql_query_result.is_empty()
                {
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
                if matches!(self.current_focus, FocusedWidget::QueryResult)
                    && !self.sql_query_result.is_empty()
                {
                    let page_size = 10;
                    let max_row = self.sql_query_result.len().saturating_sub(1);
                    if self.selected_result_row + page_size <= max_row {
                        self.selected_result_row += page_size;
                    } else {
                        self.selected_result_row = max_row;
                    }
                    let visible_height = 20;
                    if self.selected_result_row >= self.sql_result_scroll + visible_height {
                        self.sql_result_scroll =
                            self.selected_result_row.saturating_sub(visible_height - 1);
                    }
                    self.sync_cursor_position();
                }
            }
            KeyCode::Home => {
                if matches!(self.current_focus, FocusedWidget::QueryResult)
                    && !self.sql_query_result.is_empty()
                {
                    self.selected_result_row = 0;
                    self.sql_result_scroll = 0;
                    self.sql_result_horizontal_scroll = 0;
                    self.sync_cursor_position();
                }
            }
            KeyCode::End => {
                if matches!(self.current_focus, FocusedWidget::QueryResult)
                    && !self.sql_query_result.is_empty()
                {
                    self.selected_result_row = self.sql_query_result.len().saturating_sub(1);
                    let visible_height = 20;
                    self.sql_result_scroll =
                        self.selected_result_row.saturating_sub(visible_height - 1);
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
                            let result = match self.server_db_type() {
                                Some(db_type) => {
                                    ServerDatabaseUI::new(self.clone(), db_type)
                                        .describe_table(&selected_table)
                                        .await
                                }
                                None => Err(DbError::Connection(
                                    "Unsupported database type".to_string(),
                                )),
                            };

                            match result {
                                Ok(columns) => {
                                    let table_schema = TableSchema {
                                        table_name: selected_table.clone(),
                                        columns: columns
                                            .into_iter()
                                            .map(|name| dfox_core::models::schema::ColumnSchema {
                                                name,
                                                data_type: String::new(),
                                                is_nullable: true,
                                                default: None,
                                            })
                                            .collect(),
                                        indexes: Vec::new(),
                                    };
                                    let table_schema_clone = table_schema.clone();
                                    self.table_schemas
                                        .insert(selected_table.clone(), table_schema_clone);
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
        if let Err(err) = UIRenderer::render_table_view_screen(self, terminal).await {
            log::error!("Error rendering UI: {}", err);
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

                    let result = match self.server_db_type() {
                        Some(db_type) => {
                            ServerDatabaseUI::new(self.clone(), db_type)
                                .execute_sql_query(&sql_content)
                                .await
                        }
                        None => Err(DbError::Connection("Unsupported database type".to_string())),
                    };

                    match result {
                        Ok(output) => self.show_query_output(output),
                        Err(err) => {
                            self.sql_query_error = Some(format!("SQL Error: {}", err));
                            self.sql_query_success_message = None;
                            self.sql_query_result.clear();
                        }
                    }
                    // Reset result navigation state
                    self.selected_result_row = 0;
                    self.sql_result_scroll = 0;
                    self.sql_result_horizontal_scroll = 0;
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
                if matches!(self.current_focus, FocusedWidget::SqlEditor)
                    && self.sql_editor_cursor_x > 0
                {
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
                if matches!(self.current_focus, FocusedWidget::SqlEditor)
                    && self.sql_editor_cursor_y > 0
                {
                    self.sql_editor_cursor_y -= 1;
                    let lines: Vec<&str> = self.sql_editor_content.split('\n').collect();
                    if let Some(line) = lines.get(self.sql_editor_cursor_y) {
                        self.sql_editor_cursor_x =
                            std::cmp::min(self.sql_editor_cursor_x, line.len());
                    }
                }
            }
            (KeyCode::Down, _) => {
                if let FocusedWidget::SqlEditor = self.current_focus {
                    let lines: Vec<&str> = self.sql_editor_content.split('\n').collect();
                    if self.sql_editor_cursor_y < lines.len().saturating_sub(1) {
                        self.sql_editor_cursor_y += 1;
                        if let Some(line) = lines.get(self.sql_editor_cursor_y) {
                            self.sql_editor_cursor_x =
                                std::cmp::min(self.sql_editor_cursor_x, line.len());
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
                    self.sql_query_success_message = Some(format!(
                        "Debug info ({} messages) - Press F12 again to clear",
                        self.debug_info.len()
                    ));
                    self.selected_result_row = 0;
                    self.sql_result_scroll = 0;
                } else {
                    // Clear debug display and restore normal result
                    self.sql_query_result.clear();
                    self.sql_query_success_message =
                        Some("No debug information available".to_string());
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
    /// Puts query output into the result grid, or a status message when the
    /// statements returned no rows.
    pub fn show_query_output(&mut self, output: QueryOutput) {
        let headers = unique_headers(&output.columns);
        self.sql_query_result = output
            .rows
            .into_iter()
            .map(|row| {
                headers
                    .iter()
                    .cloned()
                    .zip(
                        row.into_iter()
                            .map(|cell| cell.unwrap_or_else(|| "NULL".to_string())),
                    )
                    .collect()
            })
            .collect();

        let rows = self.sql_query_result.len();
        let message = if output.truncated {
            format!("Showing the first {rows} rows")
        } else if !output.columns.is_empty() {
            format!("{rows} row(s)")
        } else if output.rows_affected > 0 {
            format!("{} row(s) affected", output.rows_affected)
        } else {
            "Query executed successfully.".to_string()
        };
        self.sql_query_success_message = Some(message);
        self.sql_query_error = None;
        self.needs_tables_refresh = true;
    }

    pub fn cycle_focus(&mut self) {
        self.current_focus = match self.current_focus {
            FocusedWidget::TablesList => FocusedWidget::SqlEditor,
            FocusedWidget::SqlEditor => FocusedWidget::QueryResult,
            FocusedWidget::QueryResult => FocusedWidget::TablesList,
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
        if self.selected_result_row >= self.sql_query_result.len()
            && !self.sql_query_result.is_empty()
        {
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

/// The result grid keys cells by column name, so repeated names (for example
/// `id` from two joined tables) get a numbered suffix instead of overwriting.
fn unique_headers(columns: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashMap::new();
    columns
        .iter()
        .map(|name| {
            let count = seen.entry(name.as_str()).or_insert(0);
            *count += 1;
            if *count == 1 {
                name.clone()
            } else {
                format!("{name} ({count})")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::unique_headers;

    #[test]
    fn duplicate_headers_are_numbered() {
        let columns = ["id", "name", "id", "id"].map(String::from);
        assert_eq!(unique_headers(&columns), ["id", "name", "id (2)", "id (3)"]);
    }
}
