//! The app: owns the screens, routes keys and engine events to them, and runs
//! the event loop.

mod commands;
mod events;
mod status;
#[cfg(test)]
mod tests;

use std::{io, path::PathBuf, time::Duration};

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use dfox_core::engine::{EngineEvent, EngineHandle};
use futures_util::StreamExt;
use ratatui::{
    layout::{Constraint, Layout},
    widgets::Paragraph,
    DefaultTerminal, Frame,
};
use tokio::{sync::mpsc, time::Instant};

use crate::{
    action::Action,
    clipboard::Clipboard,
    command::Command,
    component::{Component, Ctx, EventLog, RenderCtx},
    components::{
        form::ConnectionForm,
        manager::{EntryKey, Manager},
        workspace::Workspace,
    },
    config::Config,
    connections::{pgpass, secrets::Secrets, Connection, Store},
    history::History,
    icons::Icons,
    keymap::Keymap,
    theme::{self, ColorDepth, Theme},
    widgets::{
        confirm::Confirm,
        help,
        palette::Palette,
        prompt,
        toast::{Level, Toasts},
        which_key,
    },
};

pub use crate::components::manager::Detected;

/// Redraw interval while something animates. Nothing redraws while idle.
const FRAME: Duration = Duration::from_millis(80);
/// How often the connection manager re-checks every connection.
const PING_EVERY: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Manager,
    Form,
    Workspace,
}

#[derive(Debug, Clone, PartialEq)]
enum Overlay {
    /// The key list, scrolled down this many lines.
    Help(usize),
    Palette(Palette),
    Menu,
    Confirm(Confirm),
    /// Typing the password for `connection`.
    Password {
        connection: Connection,
        typed: String,
    },
}

/// A connection being made: to a server, or to another database on the
/// server of the current session.
#[derive(Debug, Clone, PartialEq)]
struct Connecting {
    connection: Connection,
    password: String,
    /// Set when switching the session to this database.
    switch_to: Option<String>,
}

/// The connection in use.
#[derive(Debug, Clone, PartialEq)]
struct Session {
    connection: Connection,
    /// Kept in memory to switch databases on the same server.
    password: String,
    database: String,
}

/// Everything the app starts with.
pub struct Setup {
    pub config: Config,
    pub themes_dir: Option<PathBuf>,
    pub depth: ColorDepth,
    pub store: Store,
    pub secrets: Box<dyn Secrets>,
    pub pgpass: Vec<pgpass::Entry>,
    pub history: History,
    pub clipboard: Box<dyn Clipboard>,
    pub detected: Vec<Detected>,
    /// Connect to this right away (`dfox <url>`).
    pub connect: Option<EntryKey>,
    /// Shown as warnings once the UI is up.
    pub problems: Vec<String>,
}

pub struct App {
    engine: EngineHandle,
    config: Config,
    keymap: Keymap,
    theme: Theme,
    depth: ColorDepth,
    themes_dir: Option<PathBuf>,
    icons: Icons,
    store: Store,
    secrets: Box<dyn Secrets>,
    pgpass: Vec<pgpass::Entry>,
    pub screen: Screen,
    pub manager: Manager,
    pub form: Option<ConnectionForm>,
    pub workspace: Option<Workspace>,
    connecting: Option<Connecting>,
    session: Option<Session>,
    toasts: Toasts,
    log: EventLog,
    clipboard: Box<dyn Clipboard>,
    history: History,
    overlay: Option<Overlay>,
    pub should_quit: bool,
    frame: u64,
}

impl App {
    pub fn new(engine: EngineHandle, setup: Setup) -> Self {
        let Setup {
            config,
            themes_dir,
            depth,
            store,
            secrets,
            pgpass,
            history,
            clipboard,
            detected,
            connect,
            problems,
        } = setup;
        let (keymap, key_problems) = Keymap::new(config.keymap, &config.keys);
        let mut problems: Vec<String> = problems.into_iter().chain(key_problems).collect();
        let theme =
            theme::load(&config.theme, themes_dir.as_deref(), depth).unwrap_or_else(|err| {
                problems.push(err);
                theme::builtin("dfox")
                    .map(|p| p.to_theme("dfox", depth))
                    .unwrap_or_default()
            });
        let manager = Manager::new(store.connections.clone(), detected);
        let mut app = Self {
            engine,
            icons: Icons::new(config.icons),
            config,
            keymap,
            theme,
            depth,
            themes_dir,
            store,
            secrets,
            pgpass,
            screen: Screen::Manager,
            manager,
            form: None,
            workspace: None,
            connecting: None,
            session: None,
            toasts: Toasts::default(),
            log: EventLog::default(),
            clipboard,
            history,
            overlay: None,
            should_quit: false,
            frame: 0,
        };
        let now = Instant::now();
        for problem in problems {
            app.log.push(format!("Warning: {problem}"));
            app.toasts.push(Level::Warning, problem, now);
        }
        app.show_manager();
        if let Some(key) = connect {
            app.apply_command(Command::Connect(key));
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
        let mut pings = tokio::time::interval_at(Instant::now() + PING_EVERY, PING_EVERY);
        pings.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
                    Some(Ok(Event::Mouse(mouse))) => {
                        self.handle_mouse(mouse);
                        dirty = true;
                    }
                    Some(Ok(Event::Paste(text))) => {
                        self.paste(&text);
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
                _ = pings.tick(), if self.screen == Screen::Manager => {
                    self.with_ctx(|app, ctx| app.manager.ping_all(ctx));
                }
            }
        }
        Ok(())
    }

    /// Runs `f` with a handler context made from the app's own fields.
    fn with_ctx<T>(&mut self, f: impl FnOnce(&mut AppParts, &mut Ctx) -> T) -> T {
        let mut ctx = Ctx {
            engine: &self.engine,
            config: &self.config,
            toasts: &mut self.toasts,
            log: &mut self.log,
            clipboard: self.clipboard.as_mut(),
            history: &mut self.history,
            now: Instant::now(),
        };
        let mut parts = AppParts {
            manager: &mut self.manager,
            form: &mut self.form,
            workspace: &mut self.workspace,
        };
        f(&mut parts, &mut ctx)
    }

    pub fn is_animating(&self) -> bool {
        !self.toasts.is_empty()
            || self.connecting.is_some()
            || match self.screen {
                Screen::Manager => self.manager.is_animating(),
                Screen::Form => false,
                Screen::Workspace => self.workspace.as_ref().is_some_and(|w| w.is_animating()),
            }
    }

    fn wants_text(&self) -> bool {
        match self.screen {
            Screen::Manager => false,
            Screen::Form => self.form.as_ref().is_some_and(|f| f.wants_text()),
            Screen::Workspace => self.workspace.as_ref().is_some_and(|w| w.wants_text()),
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

    /// The workspace takes clicks and the wheel; other screens scroll with
    /// the wheel.
    pub fn handle_mouse(&mut self, event: MouseEvent) {
        if self.overlay.is_some() {
            return;
        }
        if self.screen == Screen::Workspace {
            let command =
                self.with_ctx(|app, ctx| app.workspace.as_mut().and_then(|w| w.mouse(event, ctx)));
            if let Some(command) = command {
                self.apply_command(command);
            }
            return;
        }
        match event.kind {
            MouseEventKind::ScrollDown => self.dispatch(Action::Down),
            MouseEventKind::ScrollUp => self.dispatch(Action::Up),
            _ => {}
        }
    }

    /// Pasted text: into the editor as it is, into other fields as typed
    /// characters.
    pub fn paste(&mut self, text: &str) {
        let typed: Vec<char> = text.chars().filter(|c| !c.is_control()).collect();
        match &mut self.overlay {
            Some(Overlay::Password {
                typed: password, ..
            }) => {
                password.extend(typed);
                return;
            }
            Some(_) => return,
            None => {}
        }
        if self.screen == Screen::Workspace
            && self.workspace.as_mut().is_some_and(|w| w.paste(text))
        {
            return;
        }
        if self.wants_text() {
            for c in typed {
                self.dispatch(Action::Input(c));
            }
        }
    }

    fn handle_overlay_key(&mut self, overlay: Overlay, key: KeyEvent) {
        match overlay {
            Overlay::Palette(mut palette) => match key.code {
                KeyCode::Enter => {
                    if let Some(action) = palette.chosen() {
                        self.dispatch(action);
                    }
                }
                KeyCode::Esc => {}
                KeyCode::Up => {
                    palette.select(false);
                    self.overlay = Some(Overlay::Palette(palette));
                }
                KeyCode::Down | KeyCode::Tab => {
                    palette.select(true);
                    self.overlay = Some(Overlay::Palette(palette));
                }
                KeyCode::Backspace => {
                    palette.backspace();
                    self.overlay = Some(Overlay::Palette(palette));
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    palette.input(c);
                    self.overlay = Some(Overlay::Palette(palette));
                }
                _ => self.overlay = Some(Overlay::Palette(palette)),
            },
            Overlay::Help(scroll) => {
                let scroll = match key.code {
                    KeyCode::Down | KeyCode::Char('j') => Some(scroll + 1),
                    KeyCode::Up | KeyCode::Char('k') => Some(scroll.saturating_sub(1)),
                    KeyCode::PageDown => Some(scroll + 10),
                    KeyCode::PageUp => Some(scroll.saturating_sub(10)),
                    _ => None,
                };
                self.overlay = scroll.map(Overlay::Help);
            }
            Overlay::Menu => {
                if let KeyCode::Char(c) = key.code {
                    if let Some(action) = which_key::action_for(c) {
                        self.dispatch(action);
                    }
                }
            }
            Overlay::Confirm(confirm) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.apply_command(confirm.on_yes),
                KeyCode::Enter if !confirm.danger => self.apply_command(confirm.on_yes),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.overlay = Some(Overlay::Confirm(confirm)),
            },
            Overlay::Password {
                connection,
                mut typed,
            } => match key.code {
                KeyCode::Enter => self.start_connect(connection, typed, None),
                KeyCode::Esc => {}
                KeyCode::Backspace => {
                    typed.pop();
                    self.overlay = Some(Overlay::Password { connection, typed });
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    typed.push(c);
                    self.overlay = Some(Overlay::Password { connection, typed });
                }
                _ => self.overlay = Some(Overlay::Password { connection, typed }),
            },
        }
    }

    pub fn dispatch(&mut self, action: Action) {
        match action {
            Action::Help => self.overlay = Some(Overlay::Help(0)),
            Action::Palette => self.overlay = Some(Overlay::Palette(Palette::default())),
            Action::Menu if self.screen == Screen::Workspace && !self.wants_text() => {
                self.overlay = Some(Overlay::Menu)
            }
            Action::NextTheme => self.next_theme(),
            Action::Quit if self.session.is_some() => self.apply_command(Command::ConfirmQuit),
            Action::Quit => self.should_quit = true,
            action => {
                let screen = self.screen;
                let command = self.with_ctx(|app, ctx| match screen {
                    Screen::Manager => app.manager.handle(action, ctx),
                    Screen::Form => app.form.as_mut().and_then(|f| f.handle(action, ctx)),
                    Screen::Workspace => app.workspace.as_mut().and_then(|w| w.handle(action, ctx)),
                });
                if let Some(command) = command {
                    self.apply_command(command);
                }
            }
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
            Screen::Manager => self.manager.render(f, main, ctx),
            Screen::Form => {
                if let Some(form) = &self.form {
                    form.render(f, main, ctx);
                }
            }
            Screen::Workspace => {
                if let Some(workspace) = &self.workspace {
                    workspace.render(f, main, ctx);
                }
            }
        }
        self.status_bar(ctx).render(f, status, ctx);

        match &self.overlay {
            Some(Overlay::Help(scroll)) => help::render(f, main, *scroll, ctx),
            Some(Overlay::Palette(palette)) => palette.render(f, main, ctx),
            Some(Overlay::Menu) => which_key::render(f, main, ctx),
            Some(Overlay::Confirm(confirm)) => confirm.render(f, main, ctx),
            Some(Overlay::Password { connection, typed }) => {
                prompt::render(f, main, &connection.name, typed.chars().count(), ctx)
            }
            None => {}
        }
        self.toasts.render(f, main, ctx);
    }
}

/// The screens, borrowed apart from the rest of the app so handlers can get
/// a [`Ctx`] at the same time.
struct AppParts<'a> {
    manager: &'a mut Manager,
    form: &'a mut Option<ConnectionForm>,
    workspace: &'a mut Option<Workspace>,
}
