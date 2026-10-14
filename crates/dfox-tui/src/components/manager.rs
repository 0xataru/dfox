//! The connection manager: saved connections and ones found on this machine,
//! with a detail panel. Each connection is pinged in the background, so a
//! dot shows whether it is reachable and a sparkline shows its latency.

use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    time::Duration,
};

use dfox_core::{engine::Command as DbCommand, errors::DbError, models::connections::DbType};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph, Sparkline},
    Frame,
};

use super::{inner, panel, results::format_duration, scroll_to};
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, RenderCtx},
    connections::{Connection, Tag},
    theme::Theme,
    widgets::{centered, throbber::spinner, truncate},
};
use dfox_core::models::connections::TlsMode;

/// Which entry of the list: a saved connection by name, or a found one by
/// position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKey {
    Saved(String),
    Detected(usize),
}

/// A connection found on this machine rather than saved.
#[derive(Debug, Clone, PartialEq)]
pub struct Detected {
    pub connection: Connection,
    /// From the URL or `.pgpass`, kept in memory only.
    pub password: Option<String>,
    /// Where it was found: `DATABASE_URL`, `.pgpass`, `command line`.
    pub source: String,
}

/// Latencies kept for the sparkline.
const HISTORY: usize = 30;

#[derive(Debug, Default)]
struct Health {
    latencies: VecDeque<u64>,
    last: Option<Result<Duration, String>>,
    pinging: bool,
}

#[derive(Debug, Default)]
pub struct Manager {
    pub saved: Vec<Connection>,
    pub detected: Vec<Detected>,
    selected: usize,
    scroll: usize,
    visible: Cell<usize>,
    /// Where the list was last drawn, for effects.
    list_area: Cell<Rect>,
    health: HashMap<String, Health>,
    pings: HashMap<u64, String>,
    next_ping: u64,
}

impl Manager {
    pub fn new(saved: Vec<Connection>, detected: Vec<Detected>) -> Self {
        Self {
            saved,
            detected,
            ..Default::default()
        }
    }

    fn len(&self) -> usize {
        self.saved.len() + self.detected.len()
    }

    fn entry(&self, index: usize) -> Option<(EntryKey, &Connection)> {
        match self.saved.get(index) {
            Some(c) => Some((EntryKey::Saved(c.name.clone()), c)),
            None => {
                let i = index - self.saved.len();
                self.detected
                    .get(i)
                    .map(|d| (EntryKey::Detected(i), &d.connection))
            }
        }
    }

    pub fn selected(&self) -> Option<EntryKey> {
        self.entry(self.selected).map(|(key, _)| key)
    }

    pub fn select_name(&mut self, name: &str) {
        if let Some(i) = self.saved.iter().position(|c| c.name == name) {
            self.select(i);
        }
    }

    fn select(&mut self, index: usize) {
        self.selected = index.min(self.len().saturating_sub(1));
        self.scroll = scroll_to(self.selected, self.scroll, self.visible.get());
    }

    /// Pings every connection that isn't being pinged already.
    pub fn ping_all(&mut self, ctx: &mut Ctx) {
        let connections: Vec<Connection> = self
            .saved
            .iter()
            .chain(self.detected.iter().map(|d| &d.connection))
            .cloned()
            .collect();
        for connection in connections {
            let health = self.health.entry(connection.name.clone()).or_default();
            if health.pinging {
                continue;
            }
            health.pinging = true;
            self.next_ping += 1;
            self.pings.insert(self.next_ping, connection.name.clone());
            ctx.engine.send(DbCommand::Ping {
                id: self.next_ping,
                db_type: connection.driver,
                params: connection.target(),
            });
        }
    }

    /// Checks answered and checks started, for the splash's progress bar.
    pub fn ping_progress(&self) -> (usize, usize) {
        let total = self.health.len();
        let waiting = self.health.values().filter(|h| h.pinging).count();
        (total - waiting, total)
    }

    pub fn list_area(&self) -> Rect {
        self.list_area.get()
    }

    pub fn on_pong(&mut self, id: u64, result: Result<Duration, DbError>) {
        let Some(name) = self.pings.remove(&id) else {
            return;
        };
        let health = self.health.entry(name).or_default();
        health.pinging = false;
        if let Ok(latency) = &result {
            health.latencies.push_back(latency.as_millis() as u64);
            if health.latencies.len() > HISTORY {
                health.latencies.pop_front();
            }
        }
        health.last = Some(result.map_err(|err| match err {
            DbError::Connection(message) => message,
            other => other.to_string(),
        }));
    }
}

impl Component for Manager {
    fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        let page = self.visible.get().max(1);
        match action {
            Action::Up => self.select(self.selected.saturating_sub(1)),
            Action::Down => self.select(self.selected + 1),
            Action::PageUp => self.select(self.selected.saturating_sub(page)),
            Action::PageDown => self.select(self.selected + page),
            Action::Home => self.select(0),
            Action::End => self.select(usize::MAX),
            Action::Confirm => return self.selected().map(Command::Connect),
            Action::New => return Some(Command::NewConnection),
            Action::Edit => {
                return match self.selected()? {
                    EntryKey::Saved(name) => Some(Command::EditConnection(name)),
                    EntryKey::Detected(i) => Some(Command::SaveDetected(i)),
                }
            }
            Action::Delete => {
                if let Some(EntryKey::Saved(name)) = self.selected() {
                    return Some(Command::ConfirmDelete(name));
                }
            }
            Action::Refresh => self.ping_all(ctx),
            Action::Back => return Some(Command::Quit),
            _ => {}
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        if self.len() == 0 {
            let message = Paragraph::new(vec![
                Line::from("No saved connections yet."),
                Line::from(""),
                Line::from(vec![
                    Span::raw("Press "),
                    Span::styled(ctx.keymap.hint(Action::New), ctx.theme.key()),
                    Span::raw(" to add one, or start dfox with a URL:"),
                ]),
                Line::styled("dfox postgres://user@localhost/db", ctx.theme.dim()),
            ])
            .centered()
            .block(panel(vec![Span::raw("Connections")], true, ctx));
            f.render_widget(message, centered(area, 60, 8));
            return;
        }
        let [list, detail] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Fill(1)]).areas(area);
        self.list_area.set(list);
        self.render_list(f, list, ctx);
        self.render_detail(f, detail, ctx);
    }

    fn is_animating(&self) -> bool {
        self.health.values().any(|h| h.pinging)
    }
}

impl Manager {
    fn render_list(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        // Leave room for the section headers.
        let visible = (inner(area).height as usize).saturating_sub(2).max(1);
        self.visible.set(visible);
        let width = inner(area).width as usize;

        let mut items = Vec::new();
        for index in self.scroll..self.len().min(self.scroll + visible) {
            if index == 0 && !self.saved.is_empty() {
                items.push(ListItem::new(Line::styled("Saved", theme.header())));
            }
            if index == self.saved.len() {
                items.push(ListItem::new(Line::styled(
                    "Found on this machine",
                    theme.header(),
                )));
            }
            let Some((_, connection)) = self.entry(index) else {
                continue;
            };
            let health = self.health.get(&connection.name);
            let dot = match health {
                Some(h) if h.pinging && h.last.is_none() => spinner(ctx, theme.dim()),
                Some(Health {
                    last: Some(Ok(_)), ..
                }) => Span::styled("●", theme.success()),
                Some(Health {
                    last: Some(Err(_)), ..
                }) => Span::styled("●", theme.danger()),
                _ => Span::styled("○", theme.dim()),
            };
            let mut spans = vec![
                Span::styled("▌", Style::default().fg(tag_color(theme, connection.tag))),
                dot,
                Span::raw(format!(
                    " {}",
                    truncate(&connection.name, width.saturating_sub(14))
                )),
                Span::styled(format!(" {}", driver_short(connection.driver)), theme.dim()),
            ];
            if connection.read_only {
                spans.push(Span::styled(" ro", theme.warning()));
            }
            let line = Line::from(spans);
            items.push(ListItem::new(if index == self.selected {
                line.style(theme.selection())
            } else {
                line
            }));
        }
        let title = vec![
            Span::raw("Connections"),
            Span::styled(format!(" {}", self.len()), theme.dim()),
        ];
        f.render_widget(List::new(items).block(panel(title, true, ctx)), area);
    }

    fn render_detail(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let Some((key, connection)) = self.entry(self.selected) else {
            return;
        };
        let field = |label: &str, value: String| {
            Line::from(vec![
                Span::styled(format!("{label:<10}"), theme.dim()),
                Span::raw(value),
            ])
        };
        let mut lines = vec![field("Driver", driver_name(connection.driver).to_string())];
        if connection.driver == DbType::Sqlite {
            lines.push(field("File", connection.address()));
        } else {
            let params = connection.target();
            lines.push(field("Host", format!("{}:{}", params.host, params.port)));
            lines.push(field(
                "Database",
                connection
                    .database
                    .clone()
                    .unwrap_or_else(|| format!("{} (default)", connection.default_database())),
            ));
            lines.push(field("User", connection.user.clone()));
            lines.push(field(
                "TLS",
                match connection.tls {
                    TlsMode::Prefer => "prefer",
                    TlsMode::Require => "require",
                    TlsMode::VerifyFull => "verify",
                    TlsMode::Disable => "off",
                }
                .to_string(),
            ));
            let password = match &key {
                EntryKey::Saved(_) => "OS keychain".to_string(),
                EntryKey::Detected(i) => match &self.detected[*i] {
                    d if d.password.is_some() => format!("from {}", d.source),
                    _ => "asked when connecting".to_string(),
                },
            };
            lines.push(field("Password", password));
        }
        if connection.read_only {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<10}", "Access"), theme.dim()),
                Span::styled("read-only", theme.warning()),
            ]));
        }
        if let EntryKey::Detected(i) = &key {
            lines.push(field("Found in", self.detected[*i].source.clone()));
        }

        let health = self.health.get(&connection.name);
        lines.push(Line::from(""));
        lines.push(match health.and_then(|h| h.last.as_ref()) {
            Some(Ok(latency)) => Line::from(vec![
                Span::styled("● ", theme.success()),
                Span::raw(format!("reachable · {}", format_duration(*latency))),
            ]),
            Some(Err(err)) => Line::from(vec![
                Span::styled("● ", theme.danger()),
                Span::styled(err.clone(), theme.danger()),
            ]),
            None => Line::styled("checking…", theme.dim()),
        });

        let mut title = vec![Span::raw(connection.name.clone())];
        if connection.tag != Tag::None {
            title.push(Span::styled(
                format!(" ▌{}", connection.tag.name()),
                Style::default().fg(tag_color(theme, connection.tag)),
            ));
        }
        let block = panel(title, false, ctx);
        let body = inner(area);
        f.render_widget(block, area);

        let text_height = lines.len() as u16;
        let [text_area, spark_area] =
            Layout::vertical([Constraint::Length(text_height), Constraint::Max(4)]).areas(body);
        f.render_widget(Paragraph::new(lines), text_area);
        if let Some(health) = health.filter(|h| !h.latencies.is_empty()) {
            let data: Vec<u64> = health.latencies.iter().copied().collect();
            f.render_widget(
                Sparkline::default()
                    .data(&data)
                    .style(Style::default().fg(theme.accent_alt)),
                spark_area,
            );
        }
    }
}

pub fn tag_color(theme: &Theme, tag: Tag) -> Color {
    match tag {
        Tag::None => theme.bg,
        Tag::Red => theme.danger,
        Tag::Orange => theme.accent,
        Tag::Yellow => theme.warning,
        Tag::Green => theme.success,
        Tag::Blue => theme.accent_alt,
        Tag::Purple => theme.keyword,
    }
}

pub fn driver_name(driver: DbType) -> &'static str {
    match driver {
        DbType::Postgres => "PostgreSQL",
        DbType::MySql => "MySQL",
        DbType::Sqlite => "SQLite",
    }
}

fn driver_short(driver: DbType) -> &'static str {
    match driver {
        DbType::Postgres => "pg",
        DbType::MySql => "mysql",
        DbType::Sqlite => "sqlite",
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
