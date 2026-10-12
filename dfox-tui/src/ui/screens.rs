//! Rendering. Every function here draws the current state and returns; none
//! waits on a database.

use dfox_core::result::{Cell, ResultSet};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;

use super::components::database_client::{format_duration, ConnectTarget};
use super::components::types::{DatabaseType, FocusedWidget, ScreenState, MAX_VISIBLE_COLUMNS};
use super::DatabaseClientUI;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Longest cell text shown in the result grid, in characters.
const MAX_CELL_CHARS: usize = 100;

impl DatabaseClientUI {
    pub fn render(&self, f: &mut Frame) {
        match self.current_screen {
            ScreenState::DbTypeSelection => self.render_db_type_selection_screen(f),
            ScreenState::MessagePopup => self.render_message_popup(f),
            ScreenState::ConnectionInput => self.render_connection_input_screen(f),
            ScreenState::DatabaseSelection => self.render_database_selection_screen(f),
            ScreenState::TableView => self.render_table_view_screen(f),
        }
    }

    fn spinner(&self) -> &'static str {
        let frame = self.started_at.elapsed().as_millis() / 100;
        SPINNER[frame as usize % SPINNER.len()]
    }

    fn render_message_popup(&self, f: &mut Frame) {
        let chunks = popup_layout(f.area());

        let block = Block::default()
            .title("Message")
            .borders(Borders::ALL)
            .title_alignment(Alignment::Center);

        let message = Paragraph::new("SQLite is not implemented yet.")
            .block(block)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });

        f.render_widget(message, centered_rect(50, chunks[1]));
        render_help(
            f,
            chunks[2],
            vec![Span::styled(
                "Press any key to return.",
                Style::default().fg(Color::White),
            )],
        );
    }

    fn render_db_type_selection_screen(&self, f: &mut Frame) {
        let db_types = [
            DatabaseType::Postgres,
            DatabaseType::MySQL,
            DatabaseType::SQLite,
        ];
        let db_type_list: Vec<ListItem> = db_types
            .iter()
            .enumerate()
            .map(|(i, db_type)| {
                ListItem::new(db_type.as_str()).style(item_style(i == self.selected_db_type))
            })
            .collect();

        let chunks = popup_layout(f.area());
        let block = Block::default()
            .title("Select Database Type")
            .borders(Borders::ALL)
            .title_alignment(Alignment::Center);

        f.render_widget(
            List::new(db_type_list).block(block),
            centered_rect(50, chunks[1]),
        );
        render_help(
            f,
            chunks[2],
            [
                hint("Up", Color::Yellow),
                Span::raw("/"),
                hint("Down", Color::Yellow),
                Span::raw(" to navigate, "),
                hint("Enter", Color::Green),
                Span::raw(" to select, "),
                hint("q", Color::Red),
                Span::raw(" to quit"),
            ]
            .to_vec(),
        );
    }

    fn render_connection_input_screen(&self, f: &mut Frame) {
        let chunks = popup_layout(f.area());
        let area = centered_rect(50, chunks[1]);

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
        if let Some(field) = content.get_mut(self.current_input_index()) {
            field.push_str(" <");
        }

        let input_paragraph = Paragraph::new(content.join("\n"))
            .block(block)
            .style(Style::default().fg(Color::White))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true });
        f.render_widget(input_paragraph, area);

        if let Some(error_message) = &self.connection_error_message {
            render_error_popup(f, area, error_message);
        } else if self.pending.connect.is_some() {
            render_help(
                f,
                chunks[2],
                vec![Span::styled(
                    format!("{} Connecting…", self.spinner()),
                    Style::default().fg(Color::Yellow),
                )],
            );
        } else {
            render_help(
                f,
                chunks[2],
                [
                    hint("Enter", Color::Green),
                    Span::raw(" to confirm input, "),
                    hint("Up/Down", Color::Yellow),
                    Span::raw(" to navigate fields, "),
                    hint("Esc", Color::Red),
                    Span::raw(" to go back"),
                ]
                .to_vec(),
            );
        }
    }

    fn render_database_selection_screen(&self, f: &mut Frame) {
        let visible_databases: Vec<ListItem> = self
            .databases
            .iter()
            .enumerate()
            .skip(self.databases_scroll)
            .take(20)
            .map(|(i, db)| {
                ListItem::new(db.as_str()).style(item_style(i == self.selected_database))
            })
            .collect();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(30),
                Constraint::Percentage(40),
                Constraint::Percentage(30),
            ])
            .split(f.area());
        let area = centered_rect(50, chunks[1]);

        let title = if self.pending.databases {
            format!("Select Database ({} loading…)", self.spinner())
        } else {
            format!(
                "Select Database ({}/{})",
                self.selected_database + 1,
                self.databases.len()
            )
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .title_alignment(Alignment::Center);
        f.render_widget(List::new(visible_databases).block(block), area);

        if let Some(error_message) = &self.connection_error_message {
            render_error_popup(f, area, error_message);
        } else if self.pending.connect == Some(ConnectTarget::Database) {
            let name = self
                .databases
                .get(self.selected_database)
                .map_or("", String::as_str);
            render_help(
                f,
                chunks[2],
                vec![Span::styled(
                    format!("{} Connecting to {name}…", self.spinner()),
                    Style::default().fg(Color::Yellow),
                )],
            );
        } else {
            render_help(
                f,
                chunks[2],
                [
                    hint("Up", Color::Yellow),
                    Span::raw("/"),
                    hint("Down", Color::Yellow),
                    Span::raw(" to navigate, "),
                    hint("Enter", Color::Green),
                    Span::raw(" to select, "),
                    hint("q", Color::Red),
                    Span::raw(" to quit"),
                ]
                .to_vec(),
            );
        }
    }

    fn render_table_view_screen(&self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(95), Constraint::Percentage(5)])
            .split(f.area());
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(chunks[0]);
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(main_chunks[1]);

        self.render_tables(f, main_chunks[0]);
        self.render_sql_editor(f, right_chunks[0]);
        self.render_result(f, right_chunks[1]);

        let mut help = vec![
            hint("Tab", Color::Yellow),
            Span::raw(" - navigate, "),
            hint("F5", Color::Green),
            Span::raw("/"),
            hint("Ctrl+E", Color::Green),
            Span::raw(" - execute, "),
        ];
        if self.running_query.is_some() {
            help.extend([hint("Ctrl+C", Color::Red), Span::raw(" - cancel, ")]);
        } else {
            help.extend([
                hint("Ctrl+C", Color::Cyan),
                Span::raw(" - copy row, "),
                hint("Ctrl+A", Color::Cyan),
                Span::raw(" - copy all, "),
            ]);
        }
        help.extend([
            hint("F12", Color::Magenta),
            Span::raw(" - debug, "),
            hint("F1", Color::Blue),
            Span::raw(" - databases, "),
            hint("Esc", Color::Red),
            Span::raw(" - quit"),
        ]);
        render_help(f, chunks[1], help);
    }

    fn render_tables(&self, f: &mut Frame, area: Rect) {
        let visible_tables: Vec<ListItem> = self
            .tables
            .iter()
            .enumerate()
            .skip(self.tables_scroll)
            .take((area.height as usize).saturating_sub(2))
            .flat_map(|(i, table)| {
                let style = if i == self.selected_table {
                    Style::default().bg(Color::Yellow).fg(Color::Black)
                } else {
                    Style::default().fg(Color::White)
                };
                let mut items = vec![ListItem::new(table.as_str()).style(style)];

                if self.expanded_table == Some(i) {
                    if let Some(schema) = self.table_schemas.get(table) {
                        items.extend(schema.columns.iter().map(|column| {
                            ListItem::new(format!(
                                "  ├─ {}: {} (Nullable: {}, Default: {:?})",
                                column.name, column.data_type, column.is_nullable, column.default
                            ))
                            .style(Style::default().fg(Color::Gray))
                        }));
                    }
                }
                items
            })
            .collect();

        let title = if self.pending.tables {
            format!("Tables ({} loading…)", self.spinner())
        } else if self.pending.describe.is_some() {
            format!(
                "Tables ({}/{}) {}",
                self.selected_table + 1,
                self.tables.len(),
                self.spinner()
            )
        } else {
            format!("Tables ({}/{})", self.selected_table + 1, self.tables.len())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(focus_style(self.current_focus == FocusedWidget::TablesList));

        f.render_widget(List::new(visible_tables).block(block), area);
        render_scrollbar(
            f,
            area,
            self.tables.len(),
            (area.height as usize).saturating_sub(2),
            self.tables_scroll,
        );
    }

    fn render_sql_editor(&self, f: &mut Frame, area: Rect) {
        let focused = self.current_focus == FocusedWidget::SqlEditor;
        let block = Block::default()
            .borders(Borders::ALL)
            .title("SQL Query")
            .border_style(focus_style(focused));

        let editor = Paragraph::new(self.sql_editor_content.as_str())
            .block(block)
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false })
            .scroll((self.sql_editor_scroll as u16, 0));
        f.render_widget(editor, area);

        if focused {
            let x = area.x + self.sql_editor_cursor_x as u16 + 1;
            let y = (area.y + self.sql_editor_cursor_y as u16 + 1)
                .saturating_sub(self.sql_editor_scroll as u16);
            if y > area.y && y < area.y + area.height - 1 && x < area.x + area.width - 1 {
                f.set_cursor_position((x, y));
            }
        }
    }

    fn render_result(&self, f: &mut Frame, area: Rect) {
        let focused = self.current_focus == FocusedWidget::QueryResult;
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Query Result")
            .border_style(focus_style(focused));

        if let Some(error) = &self.sql_query_error {
            let error_widget = Paragraph::new(format!("Error: {}", error))
                .block(block)
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: false });
            f.render_widget(error_widget, area);
        } else if self.sql_query_result.row_count() > 0 {
            self.render_result_grid(f, area);
        } else if let Some(query) = &self.running_query {
            let status = if query.cancelling {
                format!("{} Cancelling…", self.spinner())
            } else {
                format!(
                    "{} Running… {}   Ctrl+C to cancel",
                    self.spinner(),
                    format_duration(query.started.elapsed())
                )
            };
            let widget = Paragraph::new(status)
                .block(block)
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(widget, area);
        } else {
            let message = self
                .sql_query_success_message
                .as_deref()
                .unwrap_or("No results");
            f.render_widget(Paragraph::new(message).block(block), area);
        }
    }

    fn render_result_grid(&self, f: &mut Frame, area: Rect) {
        let result = &self.sql_query_result;
        let focused = self.current_focus == FocusedWidget::QueryResult;
        let total_rows = result.row_count();
        let total_columns = result.columns().len();

        let first_column = self
            .sql_result_horizontal_scroll
            .min(total_columns.saturating_sub(1));
        let visible_columns: Vec<usize> = (first_column..total_columns)
            .take(MAX_VISIBLE_COLUMNS)
            .collect();

        // Width from the header and a sample of rows, within 8..=40.
        let mut widths = vec![Constraint::Length(6)];
        for &col in &visible_columns {
            let header = result.columns()[col].name.chars().count();
            let content = (0..total_rows.min(50))
                .filter_map(|row| result.cell(row, col))
                .map(|cell| display_cell(cell).chars().count().min(50))
                .max()
                .unwrap_or(0);
            let width = (header.max(content) + 2).clamp(8, 40);
            widths.push(Constraint::Length(width as u16));
        }

        // Borders, the header and its margin.
        let visible_rows = (area.height as usize).saturating_sub(4).max(1);
        let scroll = self.sql_result_scroll.min(total_rows.saturating_sub(1));
        let rows: Vec<Row> = (scroll..total_rows.min(scroll + visible_rows))
            .map(|row| {
                let mut cells = vec![(row + 1).to_string()];
                cells.extend(
                    visible_columns
                        .iter()
                        .map(|&col| result.cell(row, col).map(display_cell).unwrap_or_default()),
                );
                let style = if focused && row == self.selected_result_row {
                    Style::default().bg(Color::Yellow).fg(Color::Black)
                } else {
                    Style::default().fg(Color::White)
                };
                Row::new(cells).style(style)
            })
            .collect();

        let mut header = vec!["#".to_string()];
        header.extend(
            visible_columns
                .iter()
                .map(|&col| result.columns()[col].name.clone()),
        );

        let mut title = format!(
            "Query Result ({total_rows} rows - Row {}/{total_rows}",
            (self.selected_result_row + 1).min(total_rows)
        );
        if total_columns > MAX_VISIBLE_COLUMNS {
            title.push_str(&format!(
                " | Cols {}-{}/{total_columns}",
                first_column + 1,
                first_column + visible_columns.len()
            ));
        }
        title.push(')');
        if let Some(query) = &self.running_query {
            title.push_str(&format!(
                " {} {}",
                self.spinner(),
                format_duration(query.started.elapsed())
            ));
        } else if let Some(message) = &self.sql_query_success_message {
            title.push_str(&format!(" {message}"));
        }

        let table = Table::new(rows, widths)
            .header(
                Row::new(header)
                    .style(
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    )
                    .bottom_margin(1),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Double)
                    .border_style(focus_style(focused))
                    .title(title),
            )
            .column_spacing(1)
            .style(Style::default().fg(Color::White));

        f.render_widget(table, area);
        render_scrollbar(f, area, total_rows, visible_rows, scroll);
    }
}

/// A cell as one line of the grid: control characters removed, long values
/// cut short.
fn display_cell(cell: Cell<'_>) -> String {
    let text: String = cell
        .to_string()
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let text = text.trim();
    if text.chars().count() > MAX_CELL_CHARS {
        let cut: String = text.chars().take(MAX_CELL_CHARS - 3).collect();
        format!("{cut}...")
    } else {
        text.to_string()
    }
}

fn hint(key: &str, color: Color) -> Span<'_> {
    Span::styled(key, Style::default().fg(color).add_modifier(Modifier::BOLD))
}

fn item_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    }
}

fn focus_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::White)
    }
}

fn render_help(f: &mut Frame, area: Rect, spans: Vec<Span<'_>>) {
    let help = Paragraph::new(Line::from(spans))
        .style(Style::default().fg(Color::White))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    f.render_widget(help, area);
}

fn render_error_popup(f: &mut Frame, area: Rect, message: &str) {
    let block = Block::default()
        .title("Error")
        .borders(Borders::ALL)
        .style(Style::default().fg(Color::Red))
        .title_alignment(Alignment::Center);
    let paragraph = Paragraph::new(message)
        .block(block)
        .style(Style::default().fg(Color::White))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    f.render_widget(Clear, area);
    f.render_widget(paragraph, area);
}

/// Draws a scrollbar on the right border of `area` when `total` items don't
/// fit in `visible` lines.
fn render_scrollbar(f: &mut Frame, area: Rect, total: usize, visible: usize, offset: usize) {
    let height = area.height.saturating_sub(2) as usize;
    if total <= visible || height == 0 || area.width == 0 {
        return;
    }
    let thumb_size = (height * visible / total).max(1);
    let progress = offset as f64 / (total - visible) as f64;
    let thumb_start = (progress * (height - thumb_size) as f64) as usize;

    for y in 0..height {
        let style = if (thumb_start..thumb_start + thumb_size).contains(&y) {
            Style::default().bg(Color::Black).fg(Color::White)
        } else {
            Style::default().bg(Color::White).fg(Color::Black)
        };
        f.render_widget(
            Paragraph::new("█").style(style),
            Rect {
                x: area.x + area.width - 1,
                y: area.y + 1 + y as u16,
                width: 1,
                height: 1,
            },
        );
    }
}

/// Rows for a centred popup: the popup in `[1]`, help in `[2]`.
fn popup_layout(area: Rect) -> std::rc::Rc<[Rect]> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(40),
            Constraint::Percentage(20),
            Constraint::Percentage(10),
        ])
        .split(area)
}

fn centered_rect(percent_x: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(r);

    popup_layout[1]
}

#[cfg(test)]
mod tests {
    use dfox_core::{
        engine::EngineHandle,
        result::{ColumnMeta, OwnedCell, RowStore},
    };
    use ratatui::{backend::TestBackend, Terminal};
    use tokio::sync::mpsc;

    use super::*;

    fn draw(ui: &DatabaseClientUI) -> String {
        let mut terminal = Terminal::new(TestBackend::new(140, 30)).expect("terminal");
        terminal.draw(|f| ui.render(f)).expect("draw");
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .map(|line| line.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn table_view() -> DatabaseClientUI {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut ui = DatabaseClientUI::new(EngineHandle::new(tx));
        ui.current_screen = ScreenState::TableView;
        ui.tables = vec!["people".to_string()];
        ui
    }

    #[test]
    fn a_running_query_shows_progress_and_how_to_cancel() {
        let mut ui = table_view();
        ui.sql_editor_content = "SELECT pg_sleep(5)".to_string();
        ui.run_query();

        let screen = draw(&ui);
        assert!(screen.contains("Running…"), "{screen}");
        assert!(screen.contains("Ctrl+C to cancel"), "{screen}");
        assert!(screen.contains("Ctrl+C - cancel"), "{screen}");
    }

    #[test]
    fn results_show_in_the_grid() {
        let mut ui = table_view();
        let mut result = RowStore::new(vec![
            ColumnMeta::new("id", "int4"),
            ColumnMeta::new("name", "text"),
        ]);
        result.push(vec![OwnedCell::Int(7), OwnedCell::Null]);
        result.push(vec![OwnedCell::Int(8), OwnedCell::Text("tab\there".into())]);
        ui.sql_query_result = result;
        ui.sql_query_success_message = Some("2 row(s) in 3 ms".to_string());

        let screen = draw(&ui);
        assert!(
            screen.contains("Query Result (2 rows - Row 1/2) 2 row(s) in 3 ms"),
            "{screen}"
        );
        assert!(screen.contains("NULL"), "{screen}");
        assert!(screen.contains("tabhere"), "{screen}");
        assert!(screen.contains("people"), "{screen}");
    }

    #[test]
    fn long_values_are_cut_short() {
        let long = "x".repeat(150);
        let shown = display_cell(Cell::Text(&long));
        assert_eq!(shown.chars().count(), MAX_CELL_CHARS);
        assert!(shown.ends_with("..."));
    }
}
