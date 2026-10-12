
mod handlers;
mod screens;
mod components;
mod utils;

use std::io;

pub use components::database_client::DatabaseClientUI;
use crossterm::event::{KeyCode, KeyModifiers};
use dfox_core::models::schema::TableSchema;
use ratatui::{prelude::CrosstermBackend, Terminal};

pub trait UIHandler {
    async fn handle_message_popup_input(&mut self);
    async fn handle_db_type_selection_input(&mut self, key: KeyCode);
    async fn handle_input_event(&mut self, key: KeyCode) -> io::Result<()>;
    async fn handle_database_selection_input(&mut self, key: KeyCode) -> io::Result<()>;
    async fn handle_table_view_input(
        &mut self,
        key: KeyCode,
        modifiers: KeyModifiers,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    );
    async fn handle_sql_editor_input(
        &mut self,
        key: KeyCode,
        modifiers: KeyModifiers,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    );
}

pub trait UIRenderer {
    async fn render_message_popup(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()>;
    async fn render_db_type_selection_screen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()>;
    async fn render_connection_input_screen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()>;
    async fn render_database_selection_screen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()>;
    async fn render_table_view_screen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()>;
    async fn render_table_schema(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        table_schema: &TableSchema,
    ) -> io::Result<()>;
}
