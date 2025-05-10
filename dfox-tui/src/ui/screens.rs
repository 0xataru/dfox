use dfox_core::models::schema::TableSchema;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, time::Duration};
use tokio::time::timeout;

use crate::db::{DatabaseUI, postgres::PostgresDatabaseUI, mysql::MySqlDatabaseUI};

use super::components::{DatabaseType, FocusedWidget};
use super::{DatabaseClientUI, UIRenderer};

impl UIRenderer for DatabaseClientUI {
    async fn render_message_popup(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        terminal.draw(|f| {
            let size = f.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [
                        Constraint::Percentage(30),
                        Constraint::Percentage(40),
                        Constraint::Percentage(20),
                        Constraint::Percentage(10),
                    ]
                    .as_ref(),
                )
                .split(size);

            let popup_area = centered_rect(50, chunks[1]);

            let block = Block::default()
                .title("Message")
                .borders(Borders::ALL)
                .title_alignment(Alignment::Center);

            let message = Paragraph::new("SQLite is not implemented yet.")
                .block(block)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true });

            f.render_widget(message, popup_area);

            let help_message = vec![Line::from(vec![Span::styled(
                "Press any key to return.",
                Style::default().fg(Color::White),
            )])];

            let help_paragraph = Paragraph::new(help_message)
                .style(Style::default().fg(Color::White))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true });

            f.render_widget(help_paragraph, chunks[2]);
        })?;

        Ok(())
    }

    async fn render_db_type_selection_screen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        let db_types = [
            DatabaseType::Postgres,
            DatabaseType::MySQL,
            DatabaseType::SQLite,
        ];
        let db_type_list: Vec<ListItem> = db_types
            .iter()
            .enumerate()
            .map(|(i, db_type)| {
                let db = db_type.as_str();

                if i == self.selected_db_type {
                    ListItem::new(db).style(
                        Style::default()
                            .bg(Color::Yellow)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    ListItem::new(db).style(Style::default().fg(Color::White))
                }
            })
            .collect();

        terminal.draw(|f| {
            let size = f.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [
                        Constraint::Percentage(30),
                        Constraint::Percentage(40),
                        Constraint::Percentage(20),
                        Constraint::Percentage(10),
                    ]
                    .as_ref(),
                )
                .split(size);

            let horizontal_layout = centered_rect(50, chunks[1]);

            let block = Block::default()
                .title("Select Database Type")
                .borders(Borders::ALL)
                .title_alignment(Alignment::Center);

            let db_type_widget = List::new(db_type_list).block(block).highlight_style(
                Style::default()
                    .bg(Color::Yellow)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            );

            f.render_widget(db_type_widget, horizontal_layout);

            let help_message = vec![Line::from(vec![
                Span::styled(
                    "Up",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("/"),
                Span::styled(
                    "Down",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" to navigate, "),
                Span::styled(
                    "Enter",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" to select, "),
                Span::styled(
                    "q",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" to quit"),
            ])];

            let help_paragraph = Paragraph::new(help_message)
                .style(Style::default().fg(Color::White))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true });

            f.render_widget(help_paragraph, chunks[2]);
        })?;

        Ok(())
    }

    async fn render_connection_input_screen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        terminal.draw(|f| {
            let size = f.area();
            let vertical_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [
                        Constraint::Percentage(30),
                        Constraint::Percentage(40),
                        Constraint::Percentage(20),
                        Constraint::Percentage(10),
                    ]
                    .as_ref(),
                )
                .split(size);

            let horizontal_layout = centered_rect(50, vertical_chunks[1]);

            let block = Block::default()
                .title("Enter Connection Details")
                .borders(Borders::ALL)
                .title_alignment(Alignment::Center);

            let mut content = [
                format!("Username: {}", self.connection_input.username),
                format!(
                    "Password: {}",
                    "*".repeat(self.connection_input.password.len())
                ),
                format!("Hostname: {}", self.connection_input.hostname),
                format!("Port: {}", self.connection_input.port),
            ];

            content[self.current_input_index()].push_str(" <");

            let input_paragraph = Paragraph::new(content.join("\n"))
                .block(block)
                .style(Style::default().fg(Color::White))
                .alignment(Alignment::Left);

            f.render_widget(input_paragraph, horizontal_layout);

            if let Some(error_message) = &self.connection_error_message {
                let error_block = Block::default()
                    .title("Error")
                    .borders(Borders::ALL)
                    .style(Style::default().fg(Color::Red))
                    .title_alignment(Alignment::Center);

                let error_paragraph = Paragraph::new(error_message.clone())
                    .block(error_block)
                    .style(Style::default().fg(Color::White))
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true });

                let error_area = centered_rect(50, vertical_chunks[1]);
                f.render_widget(Clear, error_area);
                f.render_widget(error_paragraph, error_area);
            } else {
                let help_message = vec![Line::from(vec![
                    Span::styled(
                        "Enter",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" to confirm input, "),
                    Span::styled(
                        "Up/Down",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" to navigate fields, "),
                    Span::styled(
                        "Esc",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" to go back"),
                ])];

                let help_paragraph = Paragraph::new(help_message)
                    .style(Style::default().fg(Color::White))
                    .alignment(Alignment::Center)
                    .wrap(Wrap { trim: true });

                f.render_widget(help_paragraph, vertical_chunks[2]);
            }
        })?;

        Ok(())
    }

    async fn render_database_selection_screen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        if self.needs_db_refresh {
            match self.selected_db_type {
                0 => {
                    let db_ui = PostgresDatabaseUI::new(self.clone());
                    match timeout(Duration::from_secs(5), db_ui.fetch_databases()).await {
                        Ok(Ok(databases)) => {
                            self.databases = databases;
                            self.last_db_update = Some(std::time::Instant::now());
                            self.needs_db_refresh = false;
                        }
                        Ok(Err(err)) => {
                            eprintln!("Error fetching databases: {}", err);
                        }
                        Err(_) => {
                            eprintln!("Timeout while fetching databases");
                        }
                    }
                }
                1 => {
                    let db_ui = MySqlDatabaseUI::new(self.clone());
                    match timeout(Duration::from_secs(5), db_ui.fetch_databases()).await {
                        Ok(Ok(databases)) => {
                            self.databases = databases;
                            self.last_db_update = Some(std::time::Instant::now());
                            self.needs_db_refresh = false;
                        }
                        Ok(Err(e)) => {
                            self.databases = vec!["Error fetching databases: {}".to_string(), e.to_string()];
                        }
                        Err(_) => {
                            self.databases = vec!["Timeout while fetching databases".to_string()];
                        }
                    }
                }
                _ => (),
            }
        }

        let visible_databases: Vec<ListItem> = self
            .databases
            .iter()
            .enumerate()
            .skip(self.databases_scroll)
            .take(20) // Примерная высота видимой области
            .map(|(i, db)| {
                if i == self.selected_database {
                    ListItem::new(db.clone()).style(
                        Style::default()
                            .bg(Color::Yellow)
                            .fg(Color::Black)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    ListItem::new(db.clone()).style(Style::default().fg(Color::White))
                }
            })
            .collect();

        terminal.draw(|f| {
            let size = f.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [
                        Constraint::Percentage(30),
                        Constraint::Percentage(40),
                        Constraint::Percentage(30),
                    ]
                    .as_ref(),
                )
                .split(size);

            let horizontal_layout = centered_rect(50, chunks[1]);

            let block = Block::default()
                .title(format!("Select Database ({}/{})", self.selected_database + 1, self.databases.len()))
                .borders(Borders::ALL)
                .title_alignment(Alignment::Center);

            let db_list_widget = List::new(visible_databases).block(block).highlight_style(
                Style::default()
                    .bg(Color::Yellow)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            );

            f.render_widget(db_list_widget, horizontal_layout);

            let help_message = vec![Line::from(vec![
                Span::styled(
                    "Up",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("/"),
                Span::styled(
                    "Down",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" to navigate, "),
                Span::styled(
                    "Enter",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" to select, "),
                Span::styled(
                    "q",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" to quit"),
            ])];

            let help_paragraph = Paragraph::new(help_message)
                .style(Style::default().fg(Color::White))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true });

            f.render_widget(help_paragraph, chunks[2]);
        })?;

        Ok(())
    }

    async fn render_table_view_screen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        if self.needs_tables_refresh {
            let tables = match self.selected_db_type {
                0 => {
                    let db_ui = PostgresDatabaseUI::new(self.clone());
                    timeout(Duration::from_secs(5), db_ui.fetch_tables()).await
                }
                1 => {
                    let db_ui = MySqlDatabaseUI::new(self.clone());
                    timeout(Duration::from_secs(5), db_ui.fetch_tables()).await
                }
                _ => Ok(Ok(Vec::new())),
            };

            match tables {
                Ok(Ok(tables)) => {
                    self.tables = tables;
                    self.last_tables_update = Some(std::time::Instant::now());
                    self.needs_tables_refresh = false;
                }
                Ok(Err(e)) => {
                    eprintln!("Error fetching tables: {}", e);
                    self.tables = Vec::new();
                }
                Err(_) => {
                    eprintln!("Timeout while fetching tables");
                    self.tables = Vec::new();
                }
            }
        }

        terminal.draw(|f| {
            let size = f.area();

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(95), Constraint::Percentage(5)].as_ref())
                .split(size);

            let main_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)].as_ref())
                .split(chunks[0]);

            let right_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
                .split(main_chunks[1]);

            let visible_tables: Vec<ListItem> = self.tables
                .iter()
                .enumerate()
                .skip(self.tables_scroll)
                .take(main_chunks[0].height as usize - 2) // Используем всю доступную высоту
                .map(|(i, table)| {
                    let style = if i == self.selected_table {
                        Style::default().bg(Color::Yellow).fg(Color::Black)
                    } else {
                        Style::default().fg(Color::White)
                    };

                    let mut items = vec![ListItem::new(table.to_string()).style(style)];

                    if let Some(expanded_idx) = self.expanded_table {
                        if expanded_idx == i {
                            if let Some(schema) = self.table_schemas.get(table) {
                                for column in &schema.columns {
                                    let column_info = format!(
                                        "  ├─ {}: {} (Nullable: {}, Default: {:?})",
                                        column.name,
                                        column.data_type,
                                        column.is_nullable,
                                        column.default
                                    );
                                    items.push(
                                        ListItem::new(column_info)
                                            .style(Style::default().fg(Color::Gray)),
                                    );
                                }
                            }
                        }
                    }

                    items
                })
                .flatten()
                .collect();

            let tables_block = Block::default()
                .borders(Borders::ALL)
                .title(format!("Tables ({}/{})", self.selected_table + 1, self.tables.len()))
                .border_style(if let FocusedWidget::TablesList = self.current_focus {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::White)
                });

            let tables_widget = List::new(visible_tables)
                .block(tables_block)
                .highlight_style(Style::default().bg(Color::Yellow).fg(Color::Black));

            let sql_query_block = Block::default()
                .borders(Borders::ALL)
                .title("SQL Query")
                .border_style(if let FocusedWidget::SqlEditor = self.current_focus {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::White)
                });

            let sql_query_widget = Paragraph::new(self.sql_editor_content.clone())
                .block(sql_query_block)
                .style(Style::default().fg(Color::White));

            let sql_result_block = Block::default()
                .borders(Borders::ALL)
                .title("Query Result")
                .border_style(if let FocusedWidget::_QueryResult = self.current_focus {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::White)
                });

            if let Some(error) = &self.sql_query_error {
                let error_widget = Paragraph::new(format!("Error: {}", error))
                    .block(sql_result_block)
                    .style(Style::default().fg(Color::Red));

                f.render_widget(tables_widget, main_chunks[0]);
                f.render_widget(sql_query_widget, right_chunks[0]);
                f.render_widget(error_widget, right_chunks[1]);
            } else if !self.sql_query_result.is_empty() {
                let headers: Vec<String> = self.sql_query_result[0].keys().cloned().collect();
                let rows: Vec<Row> = self
                    .sql_query_result
                    .iter()
                    .skip(self.sql_result_scroll)
                    .take(right_chunks[1].height as usize - 2) // Используем всю доступную высоту
                    .map(|result| {
                        let cells: Vec<String> = headers
                            .iter()
                            .map(|header| {
                                result
                                    .get(header)
                                    .map_or("NULL".to_string(), |v| v.to_string())
                            })
                            .collect();
                        Row::new(cells)
                    })
                    .collect();

                let sql_result_widget =
                    Table::new(rows, headers.iter().map(|_| Constraint::Percentage(25)))
                        .header(Row::new(headers).style(Style::default().fg(Color::Yellow)))
                        .block(sql_result_block);

                f.render_widget(tables_widget, main_chunks[0]);
                f.render_widget(sql_query_widget, right_chunks[0]);
                f.render_widget(sql_result_widget, right_chunks[1]);
            } else {
                let result_message = self
                    .sql_query_success_message
                    .clone()
                    .unwrap_or_else(|| "No results".to_string());
                let result_widget = Paragraph::new(result_message).block(sql_result_block);

                f.render_widget(tables_widget, main_chunks[0]);
                f.render_widget(sql_query_widget, right_chunks[0]);
                f.render_widget(result_widget, right_chunks[1]);
            }

            if let FocusedWidget::SqlEditor = self.current_focus {
                let editor_lines: Vec<&str> = self.sql_editor_content.split('\n').collect();

                let cursor_x = editor_lines.last().map_or(0, |line| line.len()) as u16;
                let cursor_y = editor_lines.len() as u16 - 1;

                let adjusted_cursor_y = right_chunks[0].y + cursor_y + 1;

                f.set_cursor_position((right_chunks[0].x + cursor_x + 1, adjusted_cursor_y));
            }

            let help_message = vec![Line::from(vec![
                Span::styled(
                    "Tab",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - to navigate, "),
                Span::styled(
                    "F5",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" or "),
                Span::styled(
                    "Ctrl+E",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - to execute SQL query, "),
                Span::styled(
                    "F1",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - to return to database selection, "),
                Span::styled(
                    "Esc",
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - to quit"),
            ])];

            let help_paragraph = Paragraph::new(help_message)
                .style(Style::default().fg(Color::White))
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true });

            f.render_widget(help_paragraph, chunks[1]);
        })?;

        Ok(())
    }

    async fn render_table_schema(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        table_schema: &TableSchema,
    ) -> io::Result<()> {
        terminal.draw(|f| {
            let size = f.area();

            let block = Block::default()
                .title(table_schema.table_name.clone())
                .borders(Borders::ALL);

            let column_list: Vec<ListItem> = table_schema
                .columns
                .iter()
                .map(|col| {
                    let col_info = format!(
                        "{}: {} (Nullable: {}, Default: {:?})",
                        col.name, col.data_type, col.is_nullable, col.default
                    );
                    ListItem::new(col_info).style(Style::default().fg(Color::White))
                })
                .collect();

            let columns_widget = List::new(column_list).block(block);

            f.render_widget(columns_widget, size);
        })?;

        Ok(())
    }
}

fn centered_rect(percent_x: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(r);

    popup_layout[1]
}
