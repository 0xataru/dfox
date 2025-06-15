use dfox_core::models::schema::TableSchema;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{io, time::Duration};
use tokio::time::timeout;

use crate::db::{DatabaseUI, postgres::PostgresDatabaseUI, mysql::MySqlDatabaseUI};

use super::components::{DatabaseType, FocusedWidget, MAX_VISIBLE_COLUMNS};
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

            // Safely add cursor indicator to current field
            let current_index = self.current_input_index();
            if current_index < content.len() {
                content[current_index].push_str(" <");
            }

            let input_paragraph = Paragraph::new(content.join("\n"))
                .block(block)
                .style(Style::default().fg(Color::White))
                .alignment(Alignment::Left)
                .wrap(Wrap { trim: true });

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
            .take(20) 
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
                .take(main_chunks[0].height as usize - 2) 
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
                .style(Style::default().fg(Color::White))
                .wrap(Wrap { trim: false })
                .scroll((self.sql_editor_scroll as u16, 0));

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
                // Get headers from IndexMap which preserves insertion order
                let headers = if let Some(first_result) = self.sql_query_result.first() {
                    let keys: Vec<String> = first_result.keys().cloned().collect();
                    keys
                } else {
                    Vec::new()
                };
                
                // Apply horizontal scroll to headers
                let max_visible_columns = MAX_VISIBLE_COLUMNS; // Limit visible columns to prevent overcrowding
                let total_columns = headers.len();
                
                let visible_headers: Vec<String> = if self.sql_result_horizontal_scroll > 0 && headers.len() > self.sql_result_horizontal_scroll {
                    headers.iter()
                        .skip(self.sql_result_horizontal_scroll)
                        .take(max_visible_columns)
                        .cloned()
                        .collect()
                } else {
                    headers.iter()
                        .take(max_visible_columns)
                        .cloned()
                        .collect()
                };
                
                // Calculate column widths for visible headers only with minimum widths
                let mut column_widths = vec![6u16]; // Row number column (wider for better readability)
                for header in &visible_headers {
                    let header_width = header.len() as u16;
                    let max_content_width = self
                        .sql_query_result
                        .iter()
                        .take(std::cmp::min(50, self.sql_query_result.len())) // Sample fewer rows for performance
                        .map(|row| {
                            row.get(header)
                                .map_or(4, |v| std::cmp::min(v.len(), 50)) as u16 // Limit sample width to 50 chars
                        })
                        .max()
                        .unwrap_or(header_width) as u16;
                    
                    // Use reasonable width limits to prevent extreme stretching
                    let optimal_width = std::cmp::max(header_width + 2, max_content_width + 2);
                    let final_width = std::cmp::min(optimal_width, 40); // Max 40 chars per column
                    column_widths.push(std::cmp::max(final_width, 8)); // Min 8 chars per column
                }
                
                let visible_rows = (right_chunks[1].height as usize).saturating_sub(3); // Account for borders and headers
                let total_rows = self.sql_query_result.len();
                
                // Ensure scroll position is within bounds
                let safe_scroll = std::cmp::min(self.sql_result_scroll, total_rows.saturating_sub(1));
                
                // Limit the number of rows to prevent performance issues
                let max_displayable_rows = std::cmp::min(visible_rows, 1000); // Max 1000 rows at once
                let end_index = std::cmp::min(
                    safe_scroll + max_displayable_rows,
                    total_rows
                );
                
                let rows: Vec<Row> = if total_rows > 0 && end_index > safe_scroll {
                    self.sql_query_result
                        .iter()
                        .skip(safe_scroll)
                        .take(end_index - safe_scroll)
                        .enumerate()
                        .map(|(idx, result)| {
                            let row_num = safe_scroll + idx + 1;
                            let mut cells = vec![format!("{}", row_num)];
                            
                            // Apply horizontal scroll to data columns
                            for header in &visible_headers {
                                let value = result
                                    .get(header)
                                    .map_or("NULL".to_string(), |v| {
                                        // More aggressive data cleaning but preserve full length
                                        let cleaned = v
                                            .chars()
                                            .filter(|c| {
                                                // Keep all printable characters, including Unicode (Cyrillic, emojis, etc.)
                                                !c.is_control() || *c == '\t' || *c == '\n'
                                            })
                                            .collect::<String>()
                                            .trim()
                                            .to_string();
                                        
                                        // Smart truncation for display - keep reasonable cell sizes
                                        if cleaned.len() > 100 {
                                            let mut chars: Vec<char> = cleaned.chars().collect();
                                            if chars.len() >= 97 {
                                                chars.truncate(97);
                                                let truncated: String = chars.into_iter().collect();
                                                format!("{}...", truncated)
                                            } else {
                                                cleaned
                                            }
                                        } else if cleaned.is_empty() {
                                            "NULL".to_string()
                                        } else {
                                            cleaned
                                        }
                                    });
                                cells.push(value);
                            }
                            
                            let row = Row::new(cells);
                            
                            // Highlight selected row (with bounds checking)
                            if total_rows > 0 && 
                               self.selected_result_row < total_rows &&
                               safe_scroll + idx == self.selected_result_row && 
                               matches!(self.current_focus, FocusedWidget::_QueryResult) {
                                row.style(Style::default().bg(Color::Yellow).fg(Color::Black))
                            } else {
                                row.style(Style::default().fg(Color::White))
                            }
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                // Create constraints based on calculated widths - no compression needed with horizontal scroll
                let constraints: Vec<Constraint> = column_widths.into_iter().map(Constraint::Length).collect();

                let mut header_cells = vec!["#".to_string()];
                header_cells.extend(visible_headers.clone());

                // Create title with scroll indicators
                let title = if total_rows > visible_rows || self.sql_result_horizontal_scroll > 0 || total_columns > max_visible_columns {
                    let h_scroll_info = if total_columns > max_visible_columns {
                        let start_col = self.sql_result_horizontal_scroll + 1;
                        let end_col = std::cmp::min(
                            self.sql_result_horizontal_scroll + visible_headers.len(), 
                            total_columns
                        );
                        format!(" | Cols {}-{}/{}", start_col, end_col, total_columns)
                    } else {
                        String::new()
                    };
                    
                    let row_info = if total_rows > visible_rows {
                        format!("Rows {}/{} - ", 
                            std::cmp::min(safe_scroll + visible_rows, total_rows),
                            total_rows)
                    } else {
                        format!("{} rows - ", total_rows)
                    };
                    
                    format!(
                        "Query Result ({} Row {}/{}{})",
                        row_info,
                        std::cmp::min(self.selected_result_row + 1, total_rows),
                        total_rows,
                        h_scroll_info
                    )
                } else {
                    format!("Query Result ({} rows)", total_rows)
                };

                let sql_result_widget = Table::new(rows, constraints.clone())
                    .header(
                        Row::new(header_cells)
                            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                            .bottom_margin(1)
                    )
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_type(ratatui::widgets::BorderType::Double)
                            .border_style(if let FocusedWidget::_QueryResult = self.current_focus {
                                Style::default().fg(Color::Yellow)
                            } else {
                                Style::default().fg(Color::White)
                            })
                            .title(title)
                    )
                    .column_spacing(1)
                    .widths(&constraints)
                    .style(Style::default().fg(Color::White));

                f.render_widget(tables_widget, main_chunks[0]);
                
                // Add scrollbar for tables list if needed
                let total_tables = self.tables.len();
                let visible_tables_height = (main_chunks[0].height as usize).saturating_sub(2);
                if total_tables > visible_tables_height {
                    let scrollbar_area = Rect {
                        x: main_chunks[0].x + main_chunks[0].width - 1,
                        y: main_chunks[0].y + 1,
                        width: 1,
                        height: main_chunks[0].height - 2,
                    };
                    
                    let scrollbar_height = scrollbar_area.height as usize;
                    if scrollbar_height > 0 && total_tables > 0 {
                        let thumb_size = std::cmp::max(1, (scrollbar_height * visible_tables_height) / total_tables);
                        let scroll_progress = if total_tables > visible_tables_height {
                            (self.tables_scroll as f64) / ((total_tables - visible_tables_height) as f64)
                        } else {
                            0.0
                        };
                        let thumb_position = (scroll_progress * (scrollbar_height - thumb_size) as f64) as usize;
                        
                        // Draw scrollbar track
                        for y in 0..scrollbar_height {
                            let style = if y >= thumb_position && y < thumb_position + thumb_size {
                                Style::default().bg(Color::Black).fg(Color::White) // Thumb
                            } else {
                                Style::default().bg(Color::White).fg(Color::Black) // Track
                            };
                            
                            f.render_widget(
                                ratatui::widgets::Paragraph::new("█").style(style),
                                Rect {
                                    x: scrollbar_area.x,
                                    y: scrollbar_area.y + y as u16,
                                    width: 1,
                                    height: 1,
                                }
                            );
                        }
                    }
                }
                
                f.render_widget(sql_query_widget, right_chunks[0]);
                f.render_widget(sql_result_widget, right_chunks[1]);
                
                // Add scrollbar for query results
                if total_rows > visible_rows {
                    let scrollbar_area = Rect {
                        x: right_chunks[1].x + right_chunks[1].width - 1,
                        y: right_chunks[1].y + 1,
                        width: 1,
                        height: right_chunks[1].height - 2,
                    };
                    
                    let scrollbar_height = scrollbar_area.height as usize;
                    if scrollbar_height > 0 && total_rows > 0 {
                        let thumb_size = std::cmp::max(1, (scrollbar_height * visible_rows) / total_rows);
                        let scroll_progress = if total_rows > visible_rows {
                            (safe_scroll as f64) / ((total_rows - visible_rows) as f64)
                        } else {
                            0.0
                        };
                        let thumb_position = (scroll_progress * (scrollbar_height - thumb_size) as f64) as usize;
                        
                        // Draw scrollbar track
                        for y in 0..scrollbar_height {
                            let style = if y >= thumb_position && y < thumb_position + thumb_size {
                                Style::default().bg(Color::Black).fg(Color::White) // Thumb
                            } else {
                                Style::default().bg(Color::White).fg(Color::Black) // Track
                            };
                            
                            f.render_widget(
                                ratatui::widgets::Paragraph::new("█").style(style),
                                Rect {
                                    x: scrollbar_area.x,
                                    y: scrollbar_area.y + y as u16,
                                    width: 1,
                                    height: 1,
                                }
                            );
                        }
                    }
                }
            } else {
                let result_message = self
                    .sql_query_success_message
                    .clone()
                    .unwrap_or_else(|| "No results".to_string());
                let result_widget = Paragraph::new(result_message).block(sql_result_block);

                f.render_widget(tables_widget, main_chunks[0]);
                
                // Add scrollbar for tables list if needed
                let total_tables = self.tables.len();
                let visible_tables_height = (main_chunks[0].height as usize).saturating_sub(2);
                if total_tables > visible_tables_height {
                    let scrollbar_area = Rect {
                        x: main_chunks[0].x + main_chunks[0].width - 1,
                        y: main_chunks[0].y + 1,
                        width: 1,
                        height: main_chunks[0].height - 2,
                    };
                    
                    let scrollbar_height = scrollbar_area.height as usize;
                    if scrollbar_height > 0 && total_tables > 0 {
                        let thumb_size = std::cmp::max(1, (scrollbar_height * visible_tables_height) / total_tables);
                        let scroll_progress = if total_tables > visible_tables_height {
                            (self.tables_scroll as f64) / ((total_tables - visible_tables_height) as f64)
                        } else {
                            0.0
                        };
                        let thumb_position = (scroll_progress * (scrollbar_height - thumb_size) as f64) as usize;
                        
                        // Draw scrollbar track
                        for y in 0..scrollbar_height {
                            let style = if y >= thumb_position && y < thumb_position + thumb_size {
                                Style::default().bg(Color::Black).fg(Color::White) // Thumb
                            } else {
                                Style::default().bg(Color::White).fg(Color::Black) // Track
                            };
                            
                            f.render_widget(
                                ratatui::widgets::Paragraph::new("█").style(style),
                                Rect {
                                    x: scrollbar_area.x,
                                    y: scrollbar_area.y + y as u16,
                                    width: 1,
                                    height: 1,
                                }
                            );
                        }
                    }
                }
                
                f.render_widget(sql_query_widget, right_chunks[0]);
                f.render_widget(result_widget, right_chunks[1]);
            }

            if let FocusedWidget::SqlEditor = self.current_focus {
                let cursor_x = self.sql_editor_cursor_x as u16;
                let cursor_y = self.sql_editor_cursor_y as u16;

                let adjusted_cursor_x = right_chunks[0].x + cursor_x + 1;
                let adjusted_cursor_y = right_chunks[0].y + cursor_y + 1 - (self.sql_editor_scroll as u16);

                if adjusted_cursor_y >= right_chunks[0].y && 
                   adjusted_cursor_y < right_chunks[0].y + right_chunks[0].height - 1 &&
                   adjusted_cursor_x < right_chunks[0].x + right_chunks[0].width - 1 {
                    f.set_cursor_position((adjusted_cursor_x, adjusted_cursor_y));
                }
            }

            let help_message = vec![Line::from(vec![
                Span::styled(
                    "Tab",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - navigate, "),
                Span::styled(
                    "F5",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("/"),
                Span::styled(
                    "Ctrl+E",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - execute, "),
                Span::styled(
                    "Ctrl+C",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - copy row, "),
                Span::styled(
                    "Ctrl+A",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - copy all, "),
                Span::styled(
                    "F12",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - debug, "),
                Span::styled(
                    "F1",
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - databases, "),
                Span::styled(
                    "Esc",
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" - quit"),
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
