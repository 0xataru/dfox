//! The app: owns the screens, routes keys and engine events to them, and runs
//! the event loop.

use std::{io, path::PathBuf, time::Duration};

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use dfox_core::{
    engine::{Command as DbCommand, EngineEvent, EngineHandle},
    models::connections::{ConnectionParams, DbType},
    result::ResultSet,
};
use futures_util::StreamExt;
use ratatui::{
    layout::{Constraint, Layout},
    text::Span,
    widgets::Paragraph,
    DefaultTerminal, Frame,
};
use tokio::{sync::mpsc, time::Instant};

mod events;

use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, EventLog, RenderCtx},
    components::{
        connect::ConnectScreen,
        databases::DatabasesScreen,
        db_type::DbTypeScreen,
        results::format_duration,
        workspace::{Panel, Workspace},
    },
    config::Config,
    icons::Icons,
    keymap::Keymap,
    theme::{self, ColorDepth, Theme},
    widgets::{
        confirm::Confirm,
        help,
        status_bar::StatusBar,
        throbber::spinner,
        toast::{Level, Toasts},
        which_key,
    },
};

/// Redraw interval while something animates. Nothing redraws while idle.
const FRAME: Duration = Duration::from_millis(80);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    DbType,
    Connect,
    Databases,
    Workspace,
}

#[derive(Debug, Clone, PartialEq)]
enum Overlay {
    Help,
    Menu,
    Confirm(Confirm),
}

/// What a pending connect is for.
#[derive(Debug, Clone, PartialEq)]
enum Connecting {
    Server(DbType, ConnectionParams),
    Database(String),
}

pub struct App {
    engine: EngineHandle,
    config: Config,
    keymap: Keymap,
    theme: Theme,
    depth: ColorDepth,
    themes_dir: Option<PathBuf>,
    icons: Icons,
    pub screen: Screen,
    db_type: DbTypeScreen,
    pub connect: ConnectScreen,
    pub databases: DatabasesScreen,
    pub workspace: Workspace,
    connecting: Option<Connecting>,
    /// The server connected to, and the open database.
    server: Option<(DbType, ConnectionParams)>,
    database: Option<String>,
    toasts: Toasts,
    log: EventLog,
    overlay: Option<Overlay>,
    pub should_quit: bool,
    frame: u64,
}

impl App {
    pub fn new(
        engine: EngineHandle,
        config: Config,
        themes_dir: Option<PathBuf>,
        depth: ColorDepth,
        problems: Vec<String>,
    ) -> Self {
        let (keymap, key_problems) = Keymap::new(config.keymap, &config.keys);
        let mut problems: Vec<String> = problems.into_iter().chain(key_problems).collect();
        let theme =
            theme::load(&config.theme, themes_dir.as_deref(), depth).unwrap_or_else(|err| {
                problems.push(err);
                theme::builtin("dfox")
                    .map(|p| p.to_theme("dfox", depth))
                    .unwrap_or_default()
            });
        let mut app = Self {
            engine,
            icons: Icons::new(config.icons),
            config,
            keymap,
            theme,
            depth,
            themes_dir,
            screen: Screen::DbType,
            db_type: DbTypeScreen::default(),
            connect: ConnectScreen::new(DbType::Postgres),
            databases: DatabasesScreen::default(),
            workspace: Workspace::default(),
            connecting: None,
            server: None,
            database: None,
            toasts: Toasts::default(),
            log: EventLog::default(),
            overlay: None,
            should_quit: false,
            frame: 0,
        };
        let now = Instant::now();
        for problem in problems {
            app.log.push(format!("Warning: {problem}"));
            app.toasts.push(Level::Warning, problem, now);
        }
        app
    }

    /// Waits on the terminal, the engine and, while something animates, a
    /// frame timer, and redraws only after something changed.
    pub async fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        mut events: mpsc::Receiver<EngineEvent>,
    ) -> io::Result<()> {
        let mut input = EventStream::new();
        let mut frames = tokio::time::interval(FRAME);
        frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut dirty = true;

        while !self.should_quit {
            if dirty {
                terminal.draw(|f| self.render(f))?;
                dirty = false;
            }
            tokio::select! {
                event = input.next() => match event {
                    // Windows also reports key releases; act on presses only.
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        self.handle_key(key);
                        dirty = true;
                    }
                    Some(Ok(Event::Resize(..))) => dirty = true,
                    Some(Ok(_)) => {}
                    Some(Err(err)) => return Err(err),
                    None => return Ok(()),
                },
                Some(event) = events.recv() => {
                    self.apply(event);
                    dirty = true;
                }
                _ = frames.tick(), if self.is_animating() => {
                    self.frame += 1;
                    self.toasts.expire(Instant::now());
                    dirty = true;
                }
            }
        }
        Ok(())
    }

    pub fn is_animating(&self) -> bool {
        !self.toasts.is_empty()
            || self.connecting.is_some()
            || match self.screen {
                Screen::DbType => false,
                Screen::Connect => self.connect.is_animating(),
                Screen::Databases => self.databases.is_animating(),
                Screen::Workspace => self.workspace.is_animating(),
            }
    }

    fn wants_text(&self) -> bool {
        match self.screen {
            Screen::Connect => self.connect.wants_text(),
            Screen::Workspace => self.workspace.wants_text(),
            _ => false,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if let Some(overlay) = self.overlay.take() {
            self.handle_overlay_key(overlay, key);
            return;
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if let (KeyCode::Char(c), true, true) = (key.code, plain, self.wants_text()) {
            self.dispatch(Action::Input(c));
        } else if let Some(action) = self.keymap.action(key) {
            self.dispatch(action);
        }
    }

    fn handle_overlay_key(&mut self, overlay: Overlay, key: KeyEvent) {
        match overlay {
            Overlay::Help => {}
            Overlay::Menu => {
                if let KeyCode::Char(c) = key.code {
                    if let Some(action) = which_key::action_for(c) {
                        self.dispatch(action);
                    }
                }
            }
            Overlay::Confirm(confirm) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.apply_command(confirm.on_yes)
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.overlay = Some(Overlay::Confirm(confirm)),
            },
        }
    }

    pub fn dispatch(&mut self, action: Action) {
        match action {
            Action::Help => self.overlay = Some(Overlay::Help),
            Action::Menu if !self.wants_text() => self.overlay = Some(Overlay::Menu),
            Action::NextTheme => self.next_theme(),
            Action::Quit if self.server.is_some() => self.apply_command(Command::ConfirmQuit),
            Action::Quit => self.should_quit = true,
            action => {
                let mut ctx = Ctx {
                    engine: &self.engine,
                    config: &self.config,
                    toasts: &mut self.toasts,
                    log: &mut self.log,
                    now: Instant::now(),
                };
                let command = match self.screen {
                    Screen::DbType => self.db_type.handle(action, &mut ctx),
                    Screen::Connect => self.connect.handle(action, &mut ctx),
                    Screen::Databases => self.databases.handle(action, &mut ctx),
                    Screen::Workspace => self.workspace.handle(action, &mut ctx),
                };
                if let Some(command) = command {
                    self.apply_command(command);
                }
            }
        }
    }

    fn apply_command(&mut self, command: Command) {
        match command {
            Command::Quit => self.should_quit = true,
            Command::ConfirmQuit => {
                let message = if self.workspace.results.running.is_some() {
                    "Quit dfox? A query is still running."
                } else {
                    "Quit dfox?"
                };
                self.overlay = Some(Overlay::Confirm(Confirm {
                    title: "Quit".to_string(),
                    message: message.to_string(),
                    on_yes: Command::Quit,
                }));
            }
            Command::ConnectForm(db_type) => {
                if self.connect.db_type != db_type {
                    self.connect = ConnectScreen::new(db_type);
                }
                self.screen = Screen::Connect;
            }
            Command::ConnectServer { db_type, params } => {
                self.connect.connecting = true;
                self.connecting = Some(Connecting::Server(db_type, params.clone()));
                self.engine.send(DbCommand::Connect { db_type, params });
            }
            Command::OpenDatabase(name) => {
                let Some((db_type, server)) = &self.server else {
                    return;
                };
                let params = ConnectionParams {
                    database: Some(name.clone()),
                    ..server.clone()
                };
                self.databases.opening = Some(name.clone());
                self.connecting = Some(Connecting::Database(name));
                self.engine.send(DbCommand::Connect {
                    db_type: *db_type,
                    params,
                });
            }
            Command::Databases => self.screen = Screen::Databases,
            Command::DbTypes => self.screen = Screen::DbType,
        }
    }

    fn next_theme(&mut self) {
        let names: Vec<&str> = theme::BUILTIN.iter().map(|(name, _)| *name).collect();
        let next = names
            .iter()
            .position(|name| *name == self.theme.name)
            .map_or(0, |i| (i + 1) % names.len());
        let name = names[next];
        if let Ok(theme) = theme::load(name, self.themes_dir.as_deref(), self.depth) {
            self.theme = theme;
            self.toasts
                .push(Level::Info, format!("Theme: {name}"), Instant::now());
        }
    }

    pub fn render(&self, f: &mut Frame) {
        let ctx = RenderCtx {
            theme: &self.theme,
            icons: &self.icons,
            keymap: &self.keymap,
            now: Instant::now(),
            frame: self.frame,
            animations: self.config.animations,
        };
        self.draw(f, &ctx);
    }

    fn draw(&self, f: &mut Frame, ctx: &RenderCtx) {
        let area = f.area();
        f.render_widget(Paragraph::new("").style(ctx.theme.base()), area);
        let [main, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);

        match self.screen {
            Screen::DbType => self.db_type.render(f, main, ctx),
            Screen::Connect => self.connect.render(f, main, ctx),
            Screen::Databases => self.databases.render(f, main, ctx),
            Screen::Workspace => self.workspace.render(f, main, ctx),
        }
        self.status_bar(ctx).render(f, status, ctx);

        match &self.overlay {
            Some(Overlay::Help) => help::render(f, main, ctx),
            Some(Overlay::Menu) => which_key::render(f, main, ctx),
            Some(Overlay::Confirm(confirm)) => confirm.render(f, main, ctx),
            None => {}
        }
        self.toasts.render(f, main, ctx);
    }

    fn status_bar(&self, ctx: &RenderCtx) -> StatusBar<'_> {
        let theme = ctx.theme;
        let mut location = vec![Span::styled("dfox", theme.key())];
        if let Some((_, params)) = &self.server {
            location.push(Span::styled(
                format!("  {} {}", ctx.icons.database, describe(params)),
                theme.dim(),
            ));
            if let (Screen::Workspace, Some(database)) = (self.screen, &self.database) {
                location.push(Span::styled(
                    format!("/{database}"),
                    theme.base().bg(theme.surface),
                ));
            }
        }

        let activity = self
            .workspace
            .results
            .running
            .as_ref()
            .filter(|_| self.screen == Screen::Workspace)
            .map(|query| {
                vec![
                    spinner(ctx, theme.key()),
                    Span::raw(format!(
                        " {} · {} rows ",
                        format_duration(ctx.now.saturating_duration_since(query.started)),
                        self.workspace.results.store.row_count()
                    )),
                    Span::styled(
                        format!(" {} ", ctx.keymap.hint(Action::CopyRow)),
                        theme.key(),
                    ),
                    Span::styled("cancel ", theme.dim()),
                ]
            });

        let hints: &[(Action, &str)] = match self.screen {
            Screen::DbType => &[
                (Action::Confirm, "select"),
                (Action::Help, "keys"),
                (Action::Quit, "quit"),
            ],
            Screen::Connect => &[(Action::Confirm, "next"), (Action::Back, "back")],
            Screen::Databases => &[
                (Action::Confirm, "open"),
                (Action::Menu, "menu"),
                (Action::Help, "keys"),
            ],
            Screen::Workspace => match self.workspace.focused() {
                Panel::Editor => &[
                    (Action::Execute, "run"),
                    (Action::NextFocus, "focus"),
                    (Action::Help, "keys"),
                ],
                _ => &[
                    (Action::Execute, "run"),
                    (Action::NextFocus, "focus"),
                    (Action::Menu, "menu"),
                    (Action::Help, "keys"),
                ],
            },
        };
        StatusBar {
            location,
            activity,
            hints,
        }
    }
}

/// `user@host:port`.
fn describe(params: &ConnectionParams) -> String {
    format!("{}@{}:{}", params.username, params.host, params.port)
}

#[cfg(test)]
mod tests;
