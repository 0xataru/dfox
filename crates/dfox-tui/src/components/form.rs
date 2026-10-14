//! The form for a new connection, or for editing a saved one.

use dfox_core::models::connections::{DbType, TlsMode};
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::{
    manager::{driver_name, tag_color},
    panel,
};
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, RenderCtx},
    connections::{Connection, Tag},
    widgets::centered,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Driver,
    Host,
    Port,
    Database,
    User,
    Password,
    Tls,
    ReadOnly,
    Tag,
}

impl Field {
    fn is_text(self) -> bool {
        matches!(
            self,
            Field::Name
                | Field::Host
                | Field::Port
                | Field::Database
                | Field::User
                | Field::Password
        )
    }
}

const DRIVERS: [DbType; 3] = [DbType::Postgres, DbType::MySql, DbType::Sqlite];
const TLS: [TlsMode; 4] = [
    TlsMode::Prefer,
    TlsMode::Require,
    TlsMode::VerifyFull,
    TlsMode::Disable,
];

#[derive(Debug)]
pub struct ConnectionForm {
    /// The saved connection being edited, by name.
    pub original: Option<String>,
    name: String,
    driver: DbType,
    host: String,
    port: String,
    database: String,
    user: String,
    password: String,
    tls: TlsMode,
    read_only: bool,
    tag: Tag,
    field: usize,
    pub error: Option<String>,
}

impl Default for ConnectionForm {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionForm {
    pub fn new() -> Self {
        Self::edit(None, &Connection::new("", DbType::Postgres), None)
    }

    /// A form showing `connection`. `original` is its saved name, if any;
    /// `password` prefills the password field.
    pub fn edit(
        original: Option<String>,
        connection: &Connection,
        password: Option<String>,
    ) -> Self {
        Self {
            original,
            name: connection.name.clone(),
            driver: connection.driver,
            host: connection.host.clone(),
            port: connection.port.map(|p| p.to_string()).unwrap_or_default(),
            database: connection.database.clone().unwrap_or_default(),
            user: connection.user.clone(),
            password: password.unwrap_or_default(),
            tls: connection.tls,
            read_only: connection.read_only,
            tag: connection.tag,
            field: 0,
            error: None,
        }
    }

    fn fields(&self) -> Vec<Field> {
        if self.driver == DbType::Sqlite {
            vec![
                Field::Name,
                Field::Driver,
                Field::Database,
                Field::ReadOnly,
                Field::Tag,
            ]
        } else {
            vec![
                Field::Name,
                Field::Driver,
                Field::Host,
                Field::Port,
                Field::Database,
                Field::User,
                Field::Password,
                Field::Tls,
                Field::ReadOnly,
                Field::Tag,
            ]
        }
    }

    fn current(&self) -> Field {
        let fields = self.fields();
        fields[self.field.min(fields.len() - 1)]
    }

    fn text_mut(&mut self, field: Field) -> Option<&mut String> {
        match field {
            Field::Name => Some(&mut self.name),
            Field::Host => Some(&mut self.host),
            Field::Port => Some(&mut self.port),
            Field::Database => Some(&mut self.database),
            Field::User => Some(&mut self.user),
            Field::Password => Some(&mut self.password),
            _ => None,
        }
    }

    /// Steps a choice field forwards or backwards.
    fn cycle(&mut self, forward: bool) {
        fn step<T: Copy + PartialEq>(all: &[T], current: T, forward: bool) -> T {
            let i = all.iter().position(|v| *v == current).unwrap_or(0);
            let next = if forward { i + 1 } else { i + all.len() - 1 };
            all[next % all.len()]
        }
        match self.current() {
            Field::Driver => self.driver = step(&DRIVERS, self.driver, forward),
            Field::Tls => self.tls = step(&TLS, self.tls, forward),
            Field::ReadOnly => self.read_only = !self.read_only,
            Field::Tag => self.tag = step(&Tag::ALL, self.tag, forward),
            _ => {}
        }
    }

    /// The connection described by the form, or what is wrong with it.
    pub fn connection(&self) -> Result<Connection, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Give the connection a name".to_string());
        }
        let port = match self.port.trim() {
            "" => None,
            port => Some(port.parse().map_err(|_| format!("Invalid port: {port}"))?),
        };
        let database = self.database.trim();
        if self.driver == DbType::Sqlite && database.is_empty() {
            return Err("Enter the path of the SQLite file".to_string());
        }
        let sqlite = self.driver == DbType::Sqlite;
        Ok(Connection {
            name: name.to_string(),
            driver: self.driver,
            host: if sqlite {
                String::new()
            } else {
                self.host.trim().to_string()
            },
            port: if sqlite { None } else { port },
            database: (!database.is_empty()).then(|| database.to_string()),
            user: if sqlite {
                String::new()
            } else {
                self.user.trim().to_string()
            },
            tls: self.tls,
            read_only: self.read_only,
            tag: self.tag,
        })
    }

    fn save(&mut self) -> Option<Command> {
        match self.connection() {
            Ok(connection) => Some(Command::SaveConnection {
                original: self.original.clone(),
                password: (!self.password.is_empty() && connection.driver != DbType::Sqlite)
                    .then(|| self.password.clone()),
                connection,
            }),
            Err(err) => {
                self.error = Some(err);
                None
            }
        }
    }
}

impl Component for ConnectionForm {
    fn handle(&mut self, action: Action, _ctx: &mut Ctx) -> Option<Command> {
        let last = self.fields().len() - 1;
        match action {
            Action::Back => return Some(Command::CloseForm),
            Action::Save => return self.save(),
            Action::Up | Action::PrevFocus => self.field = self.field.saturating_sub(1),
            Action::Down | Action::NextFocus => self.field = (self.field + 1).min(last),
            Action::Confirm if self.field >= last => return self.save(),
            Action::Confirm => self.field += 1,
            Action::Left => self.cycle(false),
            Action::Right => self.cycle(true),
            Action::Input(c) => {
                if let Some(text) = self.text_mut(self.current()) {
                    text.push(c);
                }
            }
            Action::Backspace => {
                if let Some(text) = self.text_mut(self.current()) {
                    text.pop();
                }
            }
            _ => {}
        }
        self.error = None;
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let fields = self.fields();
        let form = centered(area, 64, fields.len() as u16 + 6);
        let current = self.current();
        let editing_saved = self.original.is_some();

        let mut lines = vec![Line::from("")];
        for field in &fields {
            let focused = *field == current;
            let (label, value, placeholder) = match field {
                Field::Name => ("Name", self.name.clone(), "my database"),
                Field::Driver => ("Driver", format!("‹ {} ›", driver_name(self.driver)), ""),
                Field::Host => ("Host", self.host.clone(), "localhost"),
                Field::Port => (
                    "Port",
                    self.port.clone(),
                    if self.driver == DbType::MySql {
                        "3306"
                    } else {
                        "5432"
                    },
                ),
                Field::Database if self.driver == DbType::Sqlite => {
                    ("File", self.database.clone(), "/path/to/file.db")
                }
                Field::Database => (
                    "Database",
                    self.database.clone(),
                    if self.driver == DbType::MySql {
                        "mysql"
                    } else {
                        "postgres"
                    },
                ),
                Field::User => ("User", self.user.clone(), ""),
                Field::Password => (
                    "Password",
                    "•".repeat(self.password.chars().count()),
                    if editing_saved {
                        "unchanged"
                    } else {
                        "asked when connecting"
                    },
                ),
                Field::Tls => (
                    "TLS",
                    format!(
                        "‹ {} ›",
                        match self.tls {
                            TlsMode::Prefer => "prefer",
                            TlsMode::Require => "require",
                            TlsMode::VerifyFull => "verify",
                            TlsMode::Disable => "off",
                        }
                    ),
                    "",
                ),
                Field::ReadOnly => (
                    "Read-only",
                    format!("‹ {} ›", if self.read_only { "yes" } else { "no" }),
                    "",
                ),
                Field::Tag => ("Tag", format!("‹ {} ›", self.tag.name()), ""),
            };
            let marker = if focused { ctx.icons.selected } else { " " };
            let value = if value.is_empty() {
                Span::styled(placeholder, theme.dim())
            } else if *field == Field::Tag && self.tag != Tag::None {
                Span::styled(value, Style::default().fg(tag_color(theme, self.tag)))
            } else {
                Span::raw(value)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{marker} {label:<10} "), theme.title(focused)),
                value,
            ]));
        }
        lines.push(Line::from(""));
        lines.push(match &self.error {
            Some(error) => Line::styled(error.clone(), theme.danger()),
            None if current.is_text() => Line::styled("Type to edit · ↑↓ move", theme.dim()),
            None => Line::styled("←→ change · ↑↓ move", theme.dim()),
        });

        let title = if editing_saved {
            "Edit connection"
        } else {
            "New connection"
        };
        f.render_widget(
            Paragraph::new(lines).block(panel(vec![Span::raw(title)], true, ctx)),
            form,
        );

        if current.is_text() {
            let index = fields.iter().position(|f| *f == current).unwrap_or(0);
            let value_len = match current {
                Field::Password => self.password.chars().count(),
                Field::Name => self.name.chars().count(),
                Field::Host => self.host.chars().count(),
                Field::Port => self.port.chars().count(),
                Field::Database => self.database.chars().count(),
                Field::User => self.user.chars().count(),
                _ => 0,
            };
            let x = form.x + 2 + 13 + value_len as u16;
            let y = form.y + 2 + index as u16;
            if x < form.x + form.width - 1 {
                f.set_cursor_position((x, y));
            }
        }
    }

    fn wants_text(&self) -> bool {
        self.current().is_text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, Harness, TestCtx};

    fn type_text(form: &mut ConnectionForm, text: &str) {
        let mut harness = Harness::new();
        for c in text.chars() {
            form.handle(Action::Input(c), &mut harness.ctx());
        }
    }

    fn act(form: &mut ConnectionForm, actions: &[Action]) -> Option<Command> {
        let mut harness = Harness::new();
        actions
            .iter()
            .filter_map(|a| form.handle(*a, &mut harness.ctx()))
            .last()
    }

    #[test]
    fn filling_in_a_server_connection() {
        let mut form = ConnectionForm::new();
        type_text(&mut form, "shop");
        act(&mut form, &[Action::Down, Action::Right, Action::Down]);
        type_text(&mut form, "db.example.com");
        act(&mut form, &[Action::Down, Action::Down, Action::Down]);
        type_text(&mut form, "app");
        act(&mut form, &[Action::Down]);
        type_text(&mut form, "secret");
        act(&mut form, &[Action::Down, Action::Down, Action::Right]);

        match act(&mut form, &[Action::Save]) {
            Some(Command::SaveConnection {
                original: None,
                connection,
                password,
            }) => {
                assert_eq!(connection.driver, DbType::MySql);
                assert_eq!(connection.address(), "app@db.example.com:3306");
                assert!(connection.read_only);
                assert_eq!(password.as_deref(), Some("secret"));
            }
            other => panic!("expected a save, got {other:?}"),
        }
    }

    #[test]
    fn sqlite_asks_for_a_file_only() {
        let mut form = ConnectionForm::new();
        type_text(&mut form, "local");
        act(&mut form, &[Action::Down, Action::Left]);
        assert_eq!(form.fields().len(), 5);
        assert_eq!(act(&mut form, &[Action::Save]), None);
        assert_eq!(
            form.error.as_deref(),
            Some("Enter the path of the SQLite file")
        );
    }

    #[test]
    fn mistakes_are_explained() {
        let mut form = ConnectionForm::new();
        assert_eq!(act(&mut form, &[Action::Save]), None);
        assert_eq!(form.error.as_deref(), Some("Give the connection a name"));
        type_text(&mut form, "x");
        act(&mut form, &[Action::Down, Action::Down, Action::Down]);
        type_text(&mut form, "12ab");
        act(&mut form, &[Action::Save]);
        assert_eq!(form.error.as_deref(), Some("Invalid port: 12ab"));
    }

    #[test]
    fn new_connection_form() {
        let test = TestCtx::new();
        let mut form = ConnectionForm::new();
        type_text(&mut form, "shop");
        act(&mut form, &[Action::Down, Action::Down]);
        insta::assert_snapshot!(render(&test, 80, 24, |f, area, ctx| form.render(f, area, ctx)));
    }

    #[test]
    fn edit_sqlite_form() {
        let test = TestCtx::new();
        let connection = Connection {
            database: Some("/home/me/app.db".into()),
            read_only: true,
            tag: Tag::Red,
            ..Connection::new("app", DbType::Sqlite)
        };
        let form = ConnectionForm::edit(Some("app".into()), &connection, None);
        insta::assert_snapshot!(render(&test, 120, 40, |f, area, ctx| form.render(f, area, ctx)));
    }
}
