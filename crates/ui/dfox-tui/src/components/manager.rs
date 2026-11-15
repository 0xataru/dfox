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

use super::{inner, panel, scroll_to};
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
/// The sparkline's top is at least this many microseconds, so local
/// databases' tenths of a millisecond stay a flat line near the bottom
/// and only a real slowdown stands out.
const SCALE_FLOOR: u64 = 50_000;

/// A latency in microseconds as people read it: `0.3 ms` below 10 ms.
fn millis(micros: u64) -> String {
    if micros < 10_000 {
        format!("{:.1} ms", micros as f64 / 1000.0)
    } else {
        format!("{} ms", micros / 1000)
    }
}

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
    /// The found connections that came from scanning the machine, by name.
    scanned: Vec<String>,
}

/// One database found by another way too: its source joins the listed
/// one's, and what the listed one lacks comes from it. A password comes
/// only with its own user, never onto another one's.
fn merge(listed: &mut Detected, other: Detected) {
    if !listed.source.split(" · ").any(|s| s == other.source) {
        listed.source = format!("{} · {}", listed.source, other.source);
    }
    let (into, from) = (&mut listed.connection, other.connection);
    let same_user = into.user.is_empty() || into.user == from.user;
    if listed.password.is_none() && other.password.is_some() && same_user {
        into.user = from.user;
        listed.password = other.password;
    } else if into.user.is_empty() {
        into.user = from.user;
    }
    if into.database.is_none() {
        into.database = from.database;
    }
}

/// The section of the list a found connection shows in, by the way it was
/// found first: the order, then the heading.
fn group(found: &Detected) -> (u8, String) {
    let source = found.source.split(" · ").next().unwrap_or_default();
    if source.starts_with("docker") || source.starts_with("podman") {
        (2, "Docker".to_string())
    } else if let Some(context) = source.strip_prefix("kubernetes: ") {
        let cluster = crate::connections::kubernetes::short_context(context);
        (3, format!("Kubernetes · {cluster}"))
    } else if source == "port-forward from k9s"
        || source.starts_with("kubectl port-forward")
        || source == "an SSH tunnel"
        || source.starts_with("port ")
    {
        (4, "Local ports".to_string())
    } else if source.starts_with("a SQLite file") {
        (5, "SQLite files".to_string())
    } else {
        (1, "Environment and config files".to_string())
    }
}

/// Every place a found connection leads to: its own, and for a Kubernetes
/// one, the other services it stands for.
fn places(found: &Detected) -> Vec<(DbType, String, u16)> {
    let mut all = vec![place(&found.connection)];
    if let Some(kube) = &found.connection.kube {
        for (namespace, service) in crate::connections::kubernetes::aliases(&found.source) {
            let mut alias = found.connection.clone();
            alias.kube = Some(dfox_core::models::connections::KubeParams {
                context: kube.context.clone(),
                namespace,
                service,
            });
            all.push(place(&alias));
        }
    }
    all
}

/// Where a connection leads: two at the same place are one database.
fn place(c: &Connection) -> (DbType, String, u16) {
    // A SQLite database is its file.
    if c.driver == DbType::Sqlite {
        return (c.driver, c.database.clone().unwrap_or_default(), 0);
    }
    let params = c.target();
    // Through Kubernetes, the service is the place, not this machine.
    if let Some(kube) = &c.kube {
        let context = kube.context.as_deref().unwrap_or_default();
        let at = format!("k8s:{context}/{}/{}", kube.namespace, kube.service);
        return (c.driver, at, params.port);
    }
    let host = match params.host.as_str() {
        "localhost" | "::1" => "127.0.0.1".to_string(),
        host => host.to_string(),
    };
    (c.driver, host, params.port)
}

impl Manager {
    /// The manager over `saved` and `detected`, each detected database
    /// once: the first found of those at the same place stays, so one given
    /// on the command line keeps its index.
    pub fn new(saved: Vec<Connection>, detected: Vec<Detected>) -> Self {
        let mut once: Vec<Detected> = Vec::new();
        for found in detected {
            let at = place(&found.connection);
            match once.iter_mut().find(|d| place(&d.connection) == at) {
                Some(listed) => merge(listed, found),
                None => once.push(found),
            }
        }
        let mut manager = Self {
            saved,
            detected: once,
            ..Default::default()
        };
        manager.regroup();
        manager
    }

    /// Keeps the found connections in their sections' order, the selection
    /// on the same connection. Within a section, as found.
    fn regroup(&mut self) {
        let selected = self.entry(self.selected).map(|(_, c)| c.name.clone());
        // By section and then by its heading, a cluster's together.
        self.detected.sort_by_key(group);
        if let Some(name) = selected {
            let at = self
                .saved
                .iter()
                .map(|c| &c.name)
                .chain(self.detected.iter().map(|d| &d.connection.name))
                .position(|n| *n == name);
            if let Some(at) = at {
                self.selected = at;
            }
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

    /// Takes what a scan of the machine found now: the ones no longer there
    /// go, new ones join unless already listed (the same engine at the same
    /// host and port).
    pub fn rescanned(&mut self, now: Vec<Detected>) {
        let gone: Vec<String> = self
            .scanned
            .iter()
            .filter(|name| !now.iter().any(|d| &d.connection.name == *name))
            .cloned()
            .collect();
        // Gone, or saved since it was found: either way not found any more.
        let saved: Vec<_> = self.saved.iter().map(place).collect();
        let gone: Vec<String> = gone
            .into_iter()
            .chain(
                self.detected
                    .iter()
                    .filter(|d| self.scanned.contains(&d.connection.name))
                    .filter(|d| places(d).iter().any(|p| saved.contains(p)))
                    .map(|d| d.connection.name.clone()),
            )
            .collect();
        self.detected.retain(|d| !gone.contains(&d.connection.name));
        self.scanned.retain(|name| !gone.contains(name));
        let fresh: Vec<Detected> = now
            .into_iter()
            .filter(|d| !self.scanned.contains(&d.connection.name))
            .collect();
        let before = self.detected.len();
        self.found(fresh);
        let added: Vec<String> = self.detected[before..]
            .iter()
            .map(|d| d.connection.name.clone())
            .collect();
        self.scanned.extend(added);
        self.regroup();
        self.select(self.selected);
    }

    /// Adds connections found after the start, leaving out those already
    /// listed: the same engine at the same host and port.
    fn found(&mut self, more: Vec<Detected>) {
        for found in more {
            let at = place(&found.connection);
            // Saved under any of the names it goes by: shown as saved.
            let leads = places(&found);
            if self.saved.iter().any(|c| leads.contains(&place(c))) {
                continue;
            }
            match self
                .detected
                .iter_mut()
                .find(|d| place(&d.connection) == at)
            {
                Some(listed) => merge(listed, found),
                None => self.detected.push(found),
            }
        }
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
        self.ping(ctx, false);
    }

    /// Pings the connections never pinged yet, as ones just found.
    pub fn ping_new(&mut self, ctx: &mut Ctx) {
        self.ping(ctx, true);
    }

    fn ping(&mut self, ctx: &mut Ctx, only_new: bool) {
        let connections: Vec<Connection> = self
            .saved
            .iter()
            .chain(self.detected.iter().map(|d| &d.connection))
            .filter(|c| !only_new || !self.health.contains_key(&c.name))
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
            health.latencies.push_back(latency.as_micros() as u64);
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
            Action::Clusters => return Some(Command::Clusters),
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
        let headers = 1 + self
            .detected
            .iter()
            .map(|d| group(d).0)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let visible = (inner(area).height as usize).saturating_sub(headers).max(1);
        self.visible.set(visible);
        let width = inner(area).width as usize;

        let mut items = Vec::new();
        for index in self.scroll..self.len().min(self.scroll + visible) {
            if index == 0 && !self.saved.is_empty() {
                items.push(ListItem::new(Line::styled("Saved", theme.header())));
            }
            // A heading where a section of found connections starts.
            if let Some(found) = index
                .checked_sub(self.saved.len())
                .and_then(|i| self.detected.get(i))
            {
                let starts = index == self.saved.len()
                    || group(&self.detected[index - self.saved.len() - 1]) != group(found);
                if starts {
                    items.push(ListItem::new(Line::styled(group(found).1, theme.header())));
                }
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
                Span::styled(
                    format!(
                        " {} {}",
                        ctx.icons.engine(connection.driver),
                        driver_short(connection.driver)
                    ),
                    theme.engine(connection.driver),
                ),
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
            match &connection.kube {
                Some(kube) => {
                    let service = format!(
                        "svc/{}:{} in ns {}",
                        kube.service, params.port, kube.namespace
                    );
                    lines.push(field("Service", service));
                    let cluster = kube.context.as_deref().map_or_else(
                        || "the current context".to_string(),
                        crate::connections::kubernetes::short_context,
                    );
                    lines.push(field("Cluster", cluster));
                    lines.push(field("Through", "kubectl port-forward".to_string()));
                }
                None => lines.push(field("Host", format!("{}:{}", params.host, params.port))),
            }
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
            if let Some(ssh) = &connection.ssh {
                let user = match ssh.user.as_str() {
                    "" => String::new(),
                    user => format!("{user}@"),
                };
                lines.push(field(
                    "SSH",
                    format!(
                        "{user}{}:{} · {}",
                        ssh.host,
                        ssh.port.unwrap_or(22),
                        ssh.login.name()
                    ),
                ));
            }
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
            // Through Kubernetes the check is kubectl finding the service,
            // not a trip to the database.
            Some(Ok(latency)) if connection.kube.is_some() => Line::from(vec![
                Span::styled("● ", theme.success()),
                Span::raw(format!(
                    "service found · kubectl took {}",
                    millis(latency.as_micros() as u64)
                )),
            ]),
            Some(Ok(latency)) => Line::from(vec![
                Span::styled("● ", theme.success()),
                Span::raw(format!(
                    "reachable · {}",
                    millis(latency.as_micros() as u64)
                )),
            ]),
            Some(Err(err)) => Line::from(vec![
                Span::styled("● ", theme.danger()),
                Span::styled(err.clone(), theme.danger()),
            ]),
            None => Line::styled("checking…", theme.dim()),
        });
        let latencies = health.map(|h| &h.latencies).filter(|l| !l.is_empty());
        if let Some(latencies) = latencies {
            let (low, high) = (
                latencies.iter().min().copied().unwrap_or_default(),
                latencies.iter().max().copied().unwrap_or_default(),
            );
            lines.push(Line::styled(
                format!(
                    "{}, last {} checks: {} – {}",
                    if connection.kube.is_some() {
                        "kubectl"
                    } else {
                        "latency"
                    },
                    latencies.len(),
                    millis(low),
                    millis(high)
                ),
                theme.dim(),
            ));
        }

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
        if let Some(latencies) = latencies {
            let data: Vec<u64> = latencies.iter().copied().collect();
            let top = data
                .iter()
                .copied()
                .max()
                .unwrap_or_default()
                .max(SCALE_FLOOR);
            f.render_widget(
                Sparkline::default()
                    .data(&data)
                    .max(top)
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
        DbType::Mongo => "MongoDB",
        DbType::Cassandra => "Cassandra / ScyllaDB",
        DbType::Redis => "Redis / Valkey",
    }
}

fn driver_short(driver: DbType) -> &'static str {
    match driver {
        DbType::Postgres => "pg",
        DbType::MySql => "mysql",
        DbType::Sqlite => "sqlite",
        DbType::Mongo => "mongo",
        DbType::Cassandra => "cql",
        DbType::Redis => "redis",
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
