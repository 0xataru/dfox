//! Key handling. Handlers only change state and send engine commands; they
//! never wait on a database.

use std::ops::Range;

use arboard::Clipboard;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dfox_core::result::{ColumnMeta, OwnedCell, ResultSet, RowStore};

use crate::ui::components::{
    connection_input::InputField,
    database_client::ConnectTarget,
    types::{FocusedWidget, ScreenState, MAX_VISIBLE_COLUMNS},
};

use super::DatabaseClientUI;

impl DatabaseClientUI {
    pub fn handle_key(&mut self, key: KeyEvent) {
        match self.current_screen {
            ScreenState::DbTypeSelection => self.handle_db_type_selection_input(key.code),
            ScreenState::MessagePopup => self.current_screen = ScreenState::DbTypeSelection,
            ScreenState::ConnectionInput => self.handle_input_event(key.code),
            ScreenState::DatabaseSelection => self.handle_database_selection_input(key.code),
            ScreenState::TableView => {
                if key.code == KeyCode::Esc {
                    self.should_quit = true;
                } else if key.code == KeyCode::Char('c')
                    && key.modifiers == KeyModifiers::CONTROL
                    && self.running_query.is_some()
                {
                    self.cancel_query();
                } else if self.current_focus == FocusedWidget::SqlEditor {
                    self.handle_sql_editor_input(key.code, key.modifiers);
                } else {
                    self.handle_table_view_input(key.code, key.modifiers);
                }
            }
        }
    }

    fn handle_db_type_selection_input(&mut self, key: KeyCode) {
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
            KeyCode::Char('q') => self.should_quit = true,
            _ => {}
        }
    }

    fn handle_input_event(&mut self, key: KeyCode) {
        if self.connection_error_message.is_some() {
            if matches!(key, KeyCode::Enter | KeyCode::Esc) {
                self.connection_error_message = None;
            }
            return;
        }
        let input = &mut self.connection_input;
        match key {
            KeyCode::Esc => self.current_screen = ScreenState::DbTypeSelection,
            KeyCode::Up => {
                input.current_field = match input.current_field {
                    InputField::Port => InputField::Hostname,
                    InputField::Hostname => InputField::Password,
                    InputField::Password => InputField::Username,
                    InputField::Username => InputField::Username,
                };
            }
            KeyCode::Down => {
                input.current_field = match input.current_field {
                    InputField::Username => InputField::Password,
                    InputField::Password => InputField::Hostname,
                    InputField::Hostname => InputField::Port,
                    InputField::Port => InputField::Port,
                };
            }
            KeyCode::Enter => match input.current_field {
                InputField::Username => input.current_field = InputField::Password,
                InputField::Password => input.current_field = InputField::Hostname,
                InputField::Hostname => input.current_field = InputField::Port,
                InputField::Port => self.connect(ConnectTarget::Server, self.default_database()),
            },
            KeyCode::Char(c) => {
                let field = match input.current_field {
                    InputField::Username => &mut input.username,
                    InputField::Password => &mut input.password,
                    InputField::Hostname => &mut input.hostname,
                    InputField::Port => &mut input.port,
                };
                field.push(c);
            }
            KeyCode::Backspace => {
                let field = match input.current_field {
                    InputField::Username => &mut input.username,
                    InputField::Password => &mut input.password,
                    InputField::Hostname => &mut input.hostname,
                    InputField::Port => &mut input.port,
                };
                field.pop();
            }
            _ => {}
        }
    }

    fn handle_database_selection_input(&mut self, key: KeyCode) {
        if self.connection_error_message.is_some() {
            self.connection_error_message = None;
            return;
        }
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
                if self.selected_database + 1 < self.databases.len() {
                    self.selected_database += 1;
                    let visible_height = 20;
                    if self.selected_database >= self.databases_scroll + visible_height {
                        self.databases_scroll = self.selected_database - visible_height + 1;
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(db_name) = self.databases.get(self.selected_database).cloned() {
                    self.connect(ConnectTarget::Database, &db_name);
                }
            }
            KeyCode::Char('q') => self.should_quit = true,
            _ => {}
        }
    }

    fn handle_table_view_input(&mut self, key: KeyCode, modifiers: KeyModifiers) {
        match (key, modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                let row = self.selected_result_row;
                self.copy_results(row..row + 1, format!("row {}", row + 1));
                return;
            }
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                let rows = self.sql_query_result.row_count();
                self.copy_results(0..rows, format!("{rows} rows"));
                return;
            }
            _ => {}
        }

        let result_rows = self.sql_query_result.row_count();
        let in_results = self.current_focus == FocusedWidget::QueryResult && result_rows > 0;
        match key {
            KeyCode::F(1) => self.back_to_databases(),
            KeyCode::Tab => self.cycle_focus(),
            KeyCode::Up => {
                if self.current_focus == FocusedWidget::TablesList {
                    self.move_selection_up();
                } else if in_results {
                    if self.selected_result_row > 0 {
                        self.selected_result_row -= 1;
                        if self.selected_result_row < self.sql_result_scroll {
                            self.sql_result_scroll = self.selected_result_row;
                        }
                    }
                    self.sync_cursor_position();
                }
            }
            KeyCode::Down => {
                if self.current_focus == FocusedWidget::TablesList {
                    self.move_selection_down();
                } else if in_results {
                    if self.selected_result_row + 1 < result_rows {
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
                if in_results {
                    self.sql_result_horizontal_scroll =
                        self.sql_result_horizontal_scroll.saturating_sub(1);
                }
            }
            KeyCode::Right => {
                if in_results {
                    let total_columns = self.sql_query_result.columns().len();
                    let max_scroll = total_columns.saturating_sub(MAX_VISIBLE_COLUMNS);
                    if self.sql_result_horizontal_scroll < max_scroll {
                        self.sql_result_horizontal_scroll += 1;
                    }
                }
            }
            KeyCode::PageUp => {
                if in_results {
                    self.selected_result_row = self.selected_result_row.saturating_sub(10);
                    self.sql_result_scroll = self.selected_result_row;
                    self.sync_cursor_position();
                }
            }
            KeyCode::PageDown => {
                if in_results {
                    self.selected_result_row = (self.selected_result_row + 10).min(result_rows - 1);
                    let visible_height = 20;
                    if self.selected_result_row >= self.sql_result_scroll + visible_height {
                        self.sql_result_scroll =
                            self.selected_result_row.saturating_sub(visible_height - 1);
                    }
                    self.sync_cursor_position();
                }
            }
            KeyCode::Home => {
                if in_results {
                    self.reset_result_navigation();
                }
            }
            KeyCode::End => {
                if in_results {
                    self.selected_result_row = result_rows - 1;
                    self.sql_result_scroll = self.selected_result_row.saturating_sub(19);
                }
            }
            KeyCode::Enter if self.current_focus == FocusedWidget::TablesList => {
                self.toggle_table_schema();
            }
            _ => {}
        }
    }

    fn handle_sql_editor_input(&mut self, key: KeyCode, modifiers: KeyModifiers) {
        match (key, modifiers) {
            (KeyCode::Tab, _) => self.cycle_focus(),
            (KeyCode::F(5), _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => self.run_query(),
            (KeyCode::Enter, _) => {
                self.sql_editor_content.push('\n');
                self.sql_editor_cursor_y += 1;
                self.sql_editor_cursor_x = 0;
                self.sync_cursor_position();
            }
            (KeyCode::Char(c), _) => {
                self.sql_editor_content.push(c);
                self.sql_editor_cursor_x += 1;
                self.sync_cursor_position();
            }
            (KeyCode::Backspace, _) => {
                if self.sql_editor_content.pop().is_some() {
                    if self.sql_editor_cursor_x > 0 {
                        self.sql_editor_cursor_x -= 1;
                    } else if self.sql_editor_cursor_y > 0 {
                        self.sql_editor_cursor_y -= 1;
                        self.sql_editor_cursor_x = self
                            .sql_editor_content
                            .split('\n')
                            .nth(self.sql_editor_cursor_y)
                            .map_or(0, str::len);
                    }
                    self.sync_cursor_position();
                }
            }
            (KeyCode::Left, _) => {
                self.sql_editor_cursor_x = self.sql_editor_cursor_x.saturating_sub(1);
            }
            (KeyCode::Right, _) => {
                let line_len = self
                    .sql_editor_content
                    .split('\n')
                    .nth(self.sql_editor_cursor_y)
                    .map_or(0, str::len);
                if self.sql_editor_cursor_x < line_len {
                    self.sql_editor_cursor_x += 1;
                }
            }
            (KeyCode::Up, _) => {
                if self.sql_editor_cursor_y > 0 {
                    self.sql_editor_cursor_y -= 1;
                    self.clamp_editor_cursor_x();
                }
            }
            (KeyCode::Down, _) => {
                let lines = self.sql_editor_content.split('\n').count();
                if self.sql_editor_cursor_y + 1 < lines {
                    self.sql_editor_cursor_y += 1;
                    self.clamp_editor_cursor_x();
                }
            }
            (KeyCode::F(1), _) => self.back_to_databases(),
            (KeyCode::F(12), _) => self.show_debug_info(),
            _ => {}
        }
    }

    fn back_to_databases(&mut self) {
        self.abandon_query();
        self.current_screen = ScreenState::DatabaseSelection;
        self.sql_editor_content.clear();
        self.sql_editor_cursor_x = 0;
        self.sql_editor_cursor_y = 0;
        self.sql_query_result = RowStore::default();
        self.sql_query_error = None;
        self.sql_query_success_message = None;
    }

    fn toggle_table_schema(&mut self) {
        let Some(table) = self.tables.get(self.selected_table).cloned() else {
            return;
        };
        if self.expanded_table == Some(self.selected_table) {
            self.expanded_table = None;
        } else {
            self.describe(table);
        }
    }

    fn show_debug_info(&mut self) {
        if self.debug_info.is_empty() {
            self.sql_query_result = RowStore::default();
            self.sql_query_success_message = Some("No debug information available".to_string());
            return;
        }
        let mut store = RowStore::new(vec![
            ColumnMeta::new("#", ""),
            ColumnMeta::new("Debug Info", ""),
        ]);
        store.extend(self.debug_info.iter().enumerate().map(|(i, msg)| {
            vec![
                OwnedCell::Int(i as i64 + 1),
                OwnedCell::Text(msg.as_str().into()),
            ]
        }));
        store.finish();
        self.sql_query_result = store;
        self.sql_query_error = None;
        self.sql_query_success_message =
            Some(format!("Debug info ({} messages)", self.debug_info.len()));
        self.reset_result_navigation();
    }

    /// Copies the header and `rows` of the result, tab-separated.
    fn copy_results(&mut self, rows: Range<usize>, what: String) {
        if self.current_focus != FocusedWidget::QueryResult {
            self.sql_query_error =
                Some("Switch focus to query results (Tab) before copying".to_string());
            return;
        }
        if self.sql_query_result.row_count() == 0 {
            self.sql_query_error = Some("No query results to copy".to_string());
            return;
        }
        let text = results_as_tsv(&self.sql_query_result, rows);
        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text)) {
            Ok(()) => {
                self.add_debug_info(format!("Copied {what} to clipboard"));
                self.sql_query_success_message = Some(format!("Copied {what} to clipboard"));
            }
            Err(err) => {
                log::error!("Error copying to the clipboard: {}", err);
                self.sql_query_error = Some(format!("Clipboard error: {}", err));
            }
        }
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
        if self.selected_table + 1 < self.tables.len() {
            self.selected_table += 1;
            let visible_height = 50;
            if self.selected_table >= self.tables_scroll + visible_height {
                self.tables_scroll = self.selected_table - visible_height + 1;
            }
        }
    }

    fn clamp_editor_cursor_x(&mut self) {
        if let Some(line) = self
            .sql_editor_content
            .split('\n')
            .nth(self.sql_editor_cursor_y)
        {
            self.sql_editor_cursor_x = self.sql_editor_cursor_x.min(line.len());
        }
    }

    pub fn sync_cursor_position(&mut self) {
        let lines: Vec<&str> = self.sql_editor_content.split('\n').collect();

        if self.sql_editor_cursor_y >= lines.len() {
            self.sql_editor_cursor_y = lines.len().saturating_sub(1);
        }
        match lines.get(self.sql_editor_cursor_y) {
            Some(line) => self.sql_editor_cursor_x = self.sql_editor_cursor_x.min(line.len()),
            None => {
                self.sql_editor_cursor_x = 0;
                self.sql_editor_cursor_y = 0;
            }
        }

        let rows = self.sql_query_result.row_count();
        self.selected_result_row = self.selected_result_row.min(rows.saturating_sub(1));
        self.sql_result_scroll = self
            .sql_result_scroll
            .min(self.selected_result_row)
            .min(rows.saturating_sub(1));
    }
}

/// The column names and `rows` of `result`, tab-separated, one line each.
fn results_as_tsv(result: &impl ResultSet, rows: Range<usize>) -> String {
    let columns = result.columns().len();
    let mut lines = vec![result
        .columns()
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join("\t")];
    lines.extend(rows.map(|row| {
        (0..columns)
            .map(|col| {
                result
                    .cell(row, col)
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("\t")
    }));
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_copy_as_tab_separated_lines() {
        let mut result = RowStore::new(vec![ColumnMeta::new("id", ""), ColumnMeta::new("id", "")]);
        result.push(vec![OwnedCell::Int(1), OwnedCell::Null]);
        result.push(vec![OwnedCell::Int(2), OwnedCell::Text("b".into())]);

        assert_eq!(results_as_tsv(&result, 0..2), "id\tid\n1\tNULL\n2\tb\n");
        assert_eq!(results_as_tsv(&result, 1..2), "id\tid\n2\tb\n");
        // Out of range rows come out empty instead of panicking.
        assert_eq!(results_as_tsv(&result, 5..6), "id\tid\n\t\n");
    }
}
