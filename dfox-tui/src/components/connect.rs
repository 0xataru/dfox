//! The connection form.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use super::panel;
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, RenderCtx},
    widgets::{centered, throbber::spinner},
};
use dfox_core::{
    errors::DbError,
    models::connections::{ConnectionParams, DbType},
};

const FIELDS: [&str; 4] = ["Username", "Password", "Host", "Port"];

#[derive(Debug)]
pub struct ConnectScreen {
    pub db_type: DbType,
    /// Username, password, host, port.
    values: [String; 4],
    field: usize,
    pub connecting: bool,
    pub error: Option<String>,
}

impl ConnectScreen {
    pub fn new(db_type: DbType) -> Self {
        Self {
            db_type,
            values: Default::default(),
            field: 0,
            connecting: false,
            error: None,
        }
    }

    #[cfg(test)]
    pub fn set(&mut self, field: usize, value: &str) {
        self.values[field] = value.to_string();
    }

    fn default_port(&self) -> u16 {
        match self.db_type {
            DbType::MySql => 3306,
            _ => 5432,
        }
    }

    /// Connection details for `database`, from the form.
    pub fn params(&self, database: &str) -> Result<ConnectionParams, DbError> {
        let [username, password, host, port] = &self.values;
        let port = match port.trim() {
            "" => self.default_port(),
            port => port
                .parse()
                .map_err(|_| DbError::Config(format!("Invalid port: {port}")))?,
        };
        let host = match host.trim() {
            "" => "localhost",
            host => host,
        };
        Ok(ConnectionParams {
            host: host.to_string(),
            port,
            username: username.clone(),
            password: password.clone(),
            database: Some(database.to_string()),
        })
    }

    fn default_database(&self) -> &'static str {
        match self.db_type {
            DbType::MySql => "mysql",
            _ => "postgres",
        }
    }

    fn submit(&mut self) -> Option<Command> {
        if self.connecting {
            return None;
        }
        match self.params(self.default_database()) {
            Ok(params) => Some(Command::ConnectServer {
                db_type: self.db_type,
                params,
            }),
            Err(err) => {
                self.error = Some(err.to_string());
                None
            }
        }
    }
}

impl Component for ConnectScreen {
    fn handle(&mut self, action: Action, _ctx: &mut Ctx) -> Option<Command> {
        if self.error.is_some() {
            if matches!(action, Action::Confirm | Action::Back) {
                self.error = None;
            }
            return None;
        }
        match action {
            Action::Back => return Some(Command::DbTypes),
            Action::Up | Action::PrevFocus => self.field = self.field.saturating_sub(1),
            Action::Down | Action::NextFocus => self.field = (self.field + 1).min(FIELDS.len() - 1),
            Action::Confirm if self.field + 1 < FIELDS.len() => self.field += 1,
            Action::Confirm => return self.submit(),
            Action::Input(c) => self.values[self.field].push(c),
            Action::Backspace => {
                self.values[self.field].pop();
            }
            _ => {}
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let form = centered(area, 56, FIELDS.len() as u16 + 4);
        let name = match self.db_type {
            DbType::Postgres => "PostgreSQL",
            DbType::MySql => "MySQL",
            DbType::Sqlite => "SQLite",
        };
        let mut title = vec![Span::raw(format!("{} {name}", ctx.icons.database))];
        if self.connecting {
            title.push(Span::raw("  "));
            title.push(spinner(ctx, theme.key()));
            title.push(Span::styled(" connecting…", theme.dim()));
        }

        let placeholders = ["", "", "localhost", &self.default_port().to_string()];
        let mut lines = vec![Line::from("")];
        for (i, label) in FIELDS.iter().enumerate() {
            let focused = i == self.field;
            let value = if i == 1 {
                "•".repeat(self.values[i].chars().count())
            } else {
                self.values[i].clone()
            };
            let value = if value.is_empty() && !focused {
                Span::styled(placeholders[i].to_string(), theme.dim())
            } else {
                Span::raw(value)
            };
            let marker = if focused { ctx.icons.selected } else { " " };
            lines.push(Line::from(vec![
                Span::styled(format!(" {marker} {label:<10}"), theme.title(focused)),
                value,
            ]));
        }
        f.render_widget(Paragraph::new(lines).block(panel(title, true, ctx)), form);

        if !self.connecting && self.error.is_none() {
            let [x, y] = [
                form.x + 15 + self.values[self.field].chars().count() as u16,
                form.y + 2 + self.field as u16,
            ];
            if x < form.x + form.width - 1 {
                f.set_cursor_position((x, y));
            }
        }

        if let Some(error) = &self.error {
            error_popup(f, form, error, ctx);
        }
    }

    fn is_animating(&self) -> bool {
        self.connecting
    }

    fn wants_text(&self) -> bool {
        self.error.is_none()
    }
}

/// An error over `area` that the next key dismisses.
pub fn error_popup(f: &mut Frame, area: Rect, message: &str, ctx: &RenderCtx) {
    let theme = ctx.theme;
    let popup = centered(area, area.width.saturating_sub(4), 6);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.danger())
        .title(Span::styled(
            format!(" {} Error ", ctx.icons.error),
            theme.danger(),
        ))
        .title_bottom(Line::from(Span::styled(" Enter to close ", theme.dim())).centered())
        .style(theme.surface());
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(message)
            .block(block)
            .centered()
            .wrap(Wrap { trim: true }),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, TestCtx};

    #[test]
    fn empty_fields_take_defaults() {
        let screen = ConnectScreen::new(DbType::MySql);
        let params = screen.params("mysql").expect("params");
        assert_eq!((params.host.as_str(), params.port), ("localhost", 3306));
    }

    #[test]
    fn a_bad_port_is_an_error() {
        let mut screen = ConnectScreen::new(DbType::Postgres);
        screen.set(3, "abc");
        assert_eq!(screen.submit(), None);
        assert_eq!(
            screen.error.as_deref(),
            Some("Configuration error: Invalid port: abc")
        );
    }

    #[test]
    fn connect_form() {
        let test = TestCtx::new();
        let mut screen = ConnectScreen::new(DbType::Postgres);
        screen.set(0, "postgres");
        screen.set(1, "secret");
        screen.field = 2;
        insta::assert_snapshot!(render(&test, 80, 24, |f, area, ctx| screen.render(f, area, ctx)));
    }

    #[test]
    fn connect_form_connecting() {
        let test = TestCtx::new();
        let mut screen = ConnectScreen::new(DbType::MySql);
        screen.set(0, "root");
        screen.connecting = true;
        insta::assert_snapshot!(render(&test, 120, 40, |f, area, ctx| screen.render(f, area, ctx)));
    }

    #[test]
    fn connect_form_error() {
        let test = TestCtx::new();
        let mut screen = ConnectScreen::new(DbType::Postgres);
        screen.error = Some(
            "Connection error: error communicating with database: Connection refused (os error 61)"
                .into(),
        );
        insta::assert_snapshot!(render(&test, 80, 24, |f, area, ctx| screen.render(f, area, ctx)));
    }
}
