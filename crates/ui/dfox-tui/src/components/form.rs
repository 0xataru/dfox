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
    connections::{Connection, Ssh, SshLogin, Tag},
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
    CaFile,
    Ssh,
    SshHost,
    SshPort,
    SshUser,
    SshKey,
    SshSecret,
    KnownHosts,
    ReadOnly,
    Tag,
}

impl Field {
    fn is_text(self) -> bool {
        !matches!(
            self,
            Field::Driver | Field::Tls | Field::Ssh | Field::ReadOnly | Field::Tag
        )
    }
}

const DRIVERS: [DbType; 6] = [
    DbType::Postgres,
    DbType::MySql,
    DbType::Mongo,
    DbType::Cassandra,
    DbType::Redis,
    DbType::Sqlite,
];
const TLS: [TlsMode; 4] = [
    TlsMode::Prefer,
    TlsMode::Require,
    TlsMode::VerifyFull,
    TlsMode::Disable,
];
/// No SSH, then each way of logging in.
const SSH: [Option<SshLogin>; 4] = [
    None,
    Some(SshLogin::Agent),
    Some(SshLogin::Key),
    Some(SshLogin::Password),
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
    ca_file: String,
    /// How to log in to the jump host; `None` connects directly.
    ssh: Option<SshLogin>,
    ssh_host: String,
    ssh_port: String,
    ssh_user: String,
    ssh_key: String,
    ssh_secret: String,
    known_hosts: String,
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

/// `text` trimmed, or `None` when empty.
fn optional(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn port(text: &str) -> Result<Option<u16>, String> {
    match text.trim() {
        "" => Ok(None),
        port => port
            .parse()
            .map(Some)
            .map_err(|_| format!("Invalid port: {port}")),
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
        let ssh = connection.ssh.as_ref();
        let ssh_text = |get: fn(&Ssh) -> Option<String>| ssh.and_then(get).unwrap_or_default();
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
            ca_file: connection.ca_file.clone().unwrap_or_default(),
            ssh: ssh.map(|s| s.login),
            ssh_host: ssh_text(|s| Some(s.host.clone())),
            ssh_port: ssh_text(|s| s.port.map(|p| p.to_string())),
            ssh_user: ssh_text(|s| Some(s.user.clone())),
            ssh_key: ssh_text(|s| s.key.clone()),
            ssh_secret: String::new(),
            known_hosts: ssh_text(|s| s.known_hosts.clone()),
            read_only: connection.read_only,
            tag: connection.tag,
            field: 0,
            error: None,
        }
    }

    fn fields(&self) -> Vec<Field> {
        if self.driver == DbType::Sqlite {
            return vec![
                Field::Name,
                Field::Driver,
                Field::Database,
                Field::ReadOnly,
                Field::Tag,
            ];
        }
        let mut fields = vec![
            Field::Name,
            Field::Driver,
            Field::Host,
            Field::Port,
            Field::Database,
            Field::User,
            Field::Password,
            Field::Tls,
        ];
        if self.tls == TlsMode::VerifyFull {
            fields.push(Field::CaFile);
        }
        fields.push(Field::Ssh);
        if let Some(login) = self.ssh {
            fields.extend([Field::SshHost, Field::SshPort, Field::SshUser]);
            if login == SshLogin::Key {
                fields.push(Field::SshKey);
            }
            if login != SshLogin::Agent {
                fields.push(Field::SshSecret);
            }
            fields.push(Field::KnownHosts);
        }
        fields.extend([Field::ReadOnly, Field::Tag]);
        fields
    }

    fn current(&self) -> Field {
        let fields = self.fields();
        fields[self.field.min(fields.len() - 1)]
    }

    fn text(&self, field: Field) -> Option<&String> {
        Some(match field {
            Field::Name => &self.name,
            Field::Host => &self.host,
            Field::Port => &self.port,
            Field::Database => &self.database,
            Field::User => &self.user,
            Field::Password => &self.password,
            Field::CaFile => &self.ca_file,
            Field::SshHost => &self.ssh_host,
            Field::SshPort => &self.ssh_port,
            Field::SshUser => &self.ssh_user,
            Field::SshKey => &self.ssh_key,
            Field::SshSecret => &self.ssh_secret,
            Field::KnownHosts => &self.known_hosts,
            _ => return None,
        })
    }

    fn text_mut(&mut self, field: Field) -> Option<&mut String> {
        Some(match field {
            Field::Name => &mut self.name,
            Field::Host => &mut self.host,
            Field::Port => &mut self.port,
            Field::Database => &mut self.database,
            Field::User => &mut self.user,
            Field::Password => &mut self.password,
            Field::CaFile => &mut self.ca_file,
            Field::SshHost => &mut self.ssh_host,
            Field::SshPort => &mut self.ssh_port,
            Field::SshUser => &mut self.ssh_user,
            Field::SshKey => &mut self.ssh_key,
            Field::SshSecret => &mut self.ssh_secret,
            Field::KnownHosts => &mut self.known_hosts,
            _ => return None,
        })
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
            Field::Ssh => self.ssh = step(&SSH, self.ssh, forward),
            Field::ReadOnly => self.read_only = !self.read_only,
            Field::Tag => self.tag = step(&Tag::ALL, self.tag, forward),
            _ => {}
        }
    }

    /// The jump host described by the form, or what is wrong with it.
    fn ssh(&self) -> Result<Option<Ssh>, String> {
        let Some(login) = self.ssh else {
            return Ok(None);
        };
        let host = self.ssh_host.trim();
        if host.is_empty() {
            return Err("Enter the SSH host".to_string());
        }
        let key = optional(&self.ssh_key);
        if login == SshLogin::Key && key.is_none() {
            return Err("Enter the SSH key file".to_string());
        }
        Ok(Some(Ssh {
            host: host.to_string(),
            port: port(&self.ssh_port).map_err(|err| format!("SSH: {err}"))?,
            user: self.ssh_user.trim().to_string(),
            login,
            key: key.filter(|_| login == SshLogin::Key),
            known_hosts: optional(&self.known_hosts),
        }))
    }

    /// The connection described by the form, or what is wrong with it.
    pub fn connection(&self) -> Result<Connection, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Give the connection a name".to_string());
        }
        let port = port(&self.port)?;
        let database = self.database.trim();
        if self.driver == DbType::Sqlite && database.is_empty() {
            return Err("Enter the path of the SQLite file".to_string());
        }
        let sqlite = self.driver == DbType::Sqlite;
        let server = |text: &str| {
            if sqlite {
                String::new()
            } else {
                text.trim().to_string()
            }
        };
        Ok(Connection {
            name: name.to_string(),
            driver: self.driver,
            host: server(&self.host),
            port: if sqlite { None } else { port },
            database: (!database.is_empty()).then(|| database.to_string()),
            user: server(&self.user),
            tls: self.tls,
            ca_file: optional(&self.ca_file).filter(|_| !sqlite && self.tls == TlsMode::VerifyFull),
            read_only: self.read_only,
            tag: self.tag,
            ssh: if sqlite { None } else { self.ssh()? },
        })
    }

    fn save(&mut self) -> Option<Command> {
        match self.connection() {
            Ok(connection) => {
                let server = connection.driver != DbType::Sqlite;
                let ssh_secret = connection
                    .ssh
                    .as_ref()
                    .filter(|ssh| ssh.login != SshLogin::Agent)
                    .and(optional(&self.ssh_secret));
                Some(Command::SaveConnection {
                    original: self.original.clone(),
                    password: (!self.password.is_empty() && server).then(|| self.password.clone()),
                    ssh_secret,
                    connection: Box::new(connection),
                })
            }
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
        let form = centered(area, 66, fields.len() as u16 + 6);
        let current = self.current();
        let editing_saved = self.original.is_some();

        let mut lines = vec![Line::from("")];
        for field in &fields {
            let focused = *field == current;
            let (label, value, placeholder) = self.row(*field, editing_saved);
            let marker = if focused { ctx.icons.selected } else { " " };
            let value = if value.is_empty() {
                Span::styled(placeholder, theme.dim())
            } else if *field == Field::Tag && self.tag != Tag::None {
                Span::styled(value, Style::default().fg(tag_color(theme, self.tag)))
            } else {
                Span::raw(value)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{marker} {label:<13} "), theme.title(focused)),
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
            let value_len = self.text(current).map_or(0, |t| t.chars().count());
            let x = form.x + 2 + 16 + value_len as u16;
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

impl ConnectionForm {
    /// A field's label, its value as shown, and what shows when it is empty.
    fn row(
        &self,
        field: Field,
        editing_saved: bool,
    ) -> (&'static str, String, std::borrow::Cow<'static, str>) {
        let choice = |value: &str| format!("‹ {value} ›");
        let hidden = |text: &str| "•".repeat(text.chars().count());
        let unchanged = if editing_saved {
            "unchanged"
        } else {
            "asked when connecting"
        };
        // What dfox connects to when they are left empty.
        match field {
            Field::Port => {
                let port = Connection::default_port(self.driver).to_string();
                return ("Port", self.port.clone(), port.into());
            }
            Field::Database if self.driver != DbType::Sqlite => {
                let label = if self.driver == DbType::Cassandra {
                    "Keyspace"
                } else {
                    "Database"
                };
                let database = Connection::default_database_of(self.driver);
                return (label, self.database.clone(), database.into());
            }
            _ => {}
        }
        let (label, value, hint) = match field {
            Field::Name => ("Name", self.name.clone(), "my database"),
            Field::Driver => ("Driver", choice(driver_name(self.driver)), ""),
            Field::Host => ("Host", self.host.clone(), "localhost"),
            // SQLite's file: the port and other databases are answered above.
            Field::Port | Field::Database => ("File", self.database.clone(), "/path/to/file.db"),
            Field::User => ("User", self.user.clone(), ""),
            Field::Password => ("Password", hidden(&self.password), unchanged),
            Field::Tls => (
                "TLS",
                choice(match self.tls {
                    TlsMode::Prefer => "prefer",
                    TlsMode::Require => "require",
                    TlsMode::VerifyFull => "verify",
                    TlsMode::Disable => "off",
                }),
                "",
            ),
            Field::CaFile => ("CA file", self.ca_file.clone(), "system roots only"),
            Field::Ssh => ("SSH", choice(self.ssh.map_or("off", SshLogin::name)), ""),
            Field::SshHost => ("  Host", self.ssh_host.clone(), "bastion.example.com"),
            Field::SshPort => ("  Port", self.ssh_port.clone(), "22"),
            Field::SshUser => ("  User", self.ssh_user.clone(), ""),
            Field::SshKey => ("  Key file", self.ssh_key.clone(), "~/.ssh/id_ed25519"),
            Field::SshSecret if self.ssh == Some(SshLogin::Key) => (
                "  Passphrase",
                hidden(&self.ssh_secret),
                if editing_saved { "unchanged" } else { "none" },
            ),
            Field::SshSecret => ("  Password", hidden(&self.ssh_secret), unchanged),
            Field::KnownHosts => (
                "  Known hosts",
                self.known_hosts.clone(),
                "~/.ssh/known_hosts",
            ),
            Field::ReadOnly => (
                "Read-only",
                choice(if self.read_only { "yes" } else { "no" }),
                "",
            ),
            Field::Tag => ("Tag", choice(self.tag.name()), ""),
        };
        (label, value, hint.into())
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
        // Past TLS and SSH to read-only.
        act(
            &mut form,
            &[Action::Down, Action::Down, Action::Down, Action::Right],
        );

        match act(&mut form, &[Action::Save]) {
            Some(Command::SaveConnection {
                original: None,
                connection,
                password,
                ssh_secret: None,
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
    fn hints_are_what_each_database_connects_to() {
        let mut form = ConnectionForm::new();
        let hints = |form: &ConnectionForm| {
            let (_, _, port) = form.row(Field::Port, false);
            let (label, _, database) = form.row(Field::Database, false);
            (port.into_owned(), label, database.into_owned())
        };
        let expected = [
            (DbType::Postgres, "5432", "Database", "postgres"),
            (DbType::MySql, "3306", "Database", "mysql"),
            (DbType::Mongo, "27017", "Database", "test"),
            (DbType::Cassandra, "9042", "Keyspace", "system"),
            (DbType::Redis, "6379", "Database", "0"),
        ];
        for (driver, port, label, database) in expected {
            form.driver = driver;
            assert_eq!(
                hints(&form),
                (port.to_string(), label, database.to_string()),
                "{driver:?}"
            );
        }
        form.driver = DbType::Sqlite;
        assert_eq!(form.row(Field::Database, false).0, "File");
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
    fn a_connection_through_ssh_with_a_key() {
        let mut form = ConnectionForm::new();
        type_text(&mut form, "prod");
        // Down to TLS, verify-full: a CA file field appears.
        act(&mut form, &[Action::Down; 7]);
        act(&mut form, &[Action::Right, Action::Right]);
        act(&mut form, &[Action::Down]);
        assert_eq!(form.current(), Field::CaFile);
        type_text(&mut form, "~/ca.pem");
        // SSH with a key: its fields appear.
        act(&mut form, &[Action::Down, Action::Right, Action::Right]);
        assert_eq!(form.ssh, Some(SshLogin::Key));
        act(&mut form, &[Action::Save]);
        assert_eq!(form.error.as_deref(), Some("Enter the SSH host"));
        act(&mut form, &[Action::Down]);
        type_text(&mut form, "bastion");
        act(&mut form, &[Action::Save]);
        assert_eq!(form.error.as_deref(), Some("Enter the SSH key file"));
        act(&mut form, &[Action::Down, Action::Down, Action::Down]);
        assert_eq!(form.current(), Field::SshKey);
        type_text(&mut form, "~/.ssh/id_ed25519");
        act(&mut form, &[Action::Down]);
        type_text(&mut form, "phrase");

        match act(&mut form, &[Action::Save]) {
            Some(Command::SaveConnection {
                connection,
                ssh_secret,
                ..
            }) => {
                assert_eq!(connection.ca_file.as_deref(), Some("~/ca.pem"));
                let ssh = connection.ssh.expect("ssh");
                assert_eq!(ssh.host, "bastion");
                assert_eq!(ssh.key.as_deref(), Some("~/.ssh/id_ed25519"));
                assert_eq!(ssh_secret.as_deref(), Some("phrase"));
            }
            other => panic!("expected a save, got {other:?}"),
        }
    }

    #[test]
    fn agent_logins_keep_no_secret_and_editing_shows_the_ssh_fields() {
        let connection = Connection {
            ssh: Some(Ssh {
                host: "bastion".into(),
                port: Some(2222),
                user: "me".into(),
                login: SshLogin::Agent,
                key: None,
                known_hosts: None,
            }),
            ..Connection::new("prod", DbType::Postgres)
        };
        let mut form = ConnectionForm::edit(Some("prod".into()), &connection, None);
        assert!(form.fields().contains(&Field::SshHost));
        assert!(!form.fields().contains(&Field::SshSecret));
        assert_eq!(form.ssh_port, "2222");
        match act(&mut form, &[Action::Save]) {
            Some(Command::SaveConnection {
                connection: saved,
                ssh_secret,
                ..
            }) => {
                assert_eq!(*saved, connection);
                assert_eq!(ssh_secret, None);
            }
            other => panic!("expected a save, got {other:?}"),
        }
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 80, 30, |f, area, ctx| form.render(f, area, ctx)));
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
