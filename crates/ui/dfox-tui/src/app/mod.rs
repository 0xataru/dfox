//! The app: owns the screens, routes keys and engine events to them, and runs
//! the event loop.

mod commands;
mod events;
mod motion;
mod status;
#[cfg(test)]
mod tests;

use std::{cell::Cell, io, path::PathBuf, time::Duration};

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use dfox_core::engine::{EngineEvent, EngineHandle};
use futures_util::StreamExt;
use ratatui::{
    layout::{Constraint, Layout, Rect},
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
    keymap::{Key, Keymap},
    theme::{self, ColorDepth, Theme},
    widgets::{
        confirm::Confirm,
        help,
        palette::Palette,
        prompt, splash,
        toast::{Level, Toasts},
        which_key,
    },
};

pub use crate::components::manager::Detected;

/// Redraw interval while a spinner or a clock is shown. Nothing redraws
/// while idle.
const FRAME: Duration = Duration::from_millis(80);
/// With animations off, clocks and toasts still update this often.
const STILL_FRAME: Duration = Duration::from_secs(1);
/// Redraw interval while an effect runs: about 60 frames a second...
const FX_FRAME: Duration = Duration::from_millis(16);
/// ...or 20 when drawing is slow, such as over SSH.
const SLOW_FX_FRAME: Duration = Duration::from_millis(50);
/// A frame that takes longer than this to draw counts as slow.
const SLOW_DRAW: Duration = Duration::from_millis(12);
/// How often the connection manager re-checks every connection.
const PING_EVERY: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// The logo while the first connection checks run.
    Splash,
    Manager,
    Form,
    Workspace,
}

#[derive(Debug, Clone, PartialEq)]
enum Overlay {
    /// The key list, where keys are changed.
    Help(help::Help),
    /// Where to copy a table's rows.
    Copy(crate::components::copy::CopyDialog),
    Palette(Palette),
    Menu,
    Confirm(Confirm),
    /// Typing a password for `connection`: the database's, or the SSH
    /// jump host's.
    Password {
        connection: Connection,
        typed: String,
        ask: Ask,
    },
}

/// Which secret the password prompt asks for, with what is known already.
#[derive(Debug, Clone, PartialEq)]
enum Ask {
    Database { ssh: Option<String> },
    Ssh { password: String },
}

/// The secrets a connection was made with, kept in memory to switch
/// databases on the same server.
#[derive(Debug, Clone, Default, PartialEq)]
struct Credentials {
    password: String,
    /// The SSH password or key passphrase.
    ssh: Option<String>,
}

/// A connection being made: to a server, or to another database on the
/// server of the current session.
#[derive(Debug, Clone, PartialEq)]
struct Connecting {
    connection: Connection,
    credentials: Credentials,
    /// Set when switching the session to this database.
    switch_to: Option<String>,
}

/// The connection in use.
#[derive(Debug, Clone, PartialEq)]
struct Session {
    connection: Connection,
    credentials: Credentials,
    database: String,
}

/// Everything the app starts with.
pub struct Setup {
    pub config: Config,
    /// Where `config.toml` and `themes/` are, if anywhere.
    pub config_dir: Option<PathBuf>,
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
    /// Show the splash first, when animations are on.
    pub splash: bool,
    /// A newer dfox is out: `0.3.6 is out: brew upgrade dfox`.
    pub update: Option<String>,
}

pub struct App {
    engine: EngineHandle,
    config: Config,
    keymap: Keymap,
    theme: Theme,
    depth: ColorDepth,
    config_dir: Option<PathBuf>,
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
    /// Shown in the manager's status bar when a newer dfox is out.
    update: Option<String>,
    pub should_quit: bool,
    frame: u64,
    motion: crate::motion::Motion,
    /// When the splash started, while it shows.
    splash: Option<Instant>,
    /// The splash logo needs its sweep-in, once drawn here.
    splash_logo: Cell<Option<Rect>>,
    /// The area above the status bar, as last drawn.
    main_area: Cell<Rect>,
    /// How long drawing takes, smoothed, to slow effects down over SSH.
    draw_cost: Duration,
}

impl App {
    pub fn new(engine: EngineHandle, setup: Setup) -> Self {
        let Setup {
            config,
            config_dir,
            depth,
            store,
            secrets,
            pgpass,
            history,
            clipboard,
            detected,
            connect,
            problems,
            splash,
            update,
        } = setup;
        let (keymap, key_problems) = Keymap::new(config.keymap, &config.keys);
        let themes_dir = config_dir.as_ref().map(|dir| dir.join("themes"));
        let mut problems: Vec<String> = problems.into_iter().chain(key_problems).collect();
        let theme =
            theme::load(&config.theme, themes_dir.as_deref(), depth).unwrap_or_else(|err| {
                problems.push(err);
                theme::builtin("dfox")
                    .map(|p| p.to_theme("dfox", depth))
                    .unwrap_or_default()
            });
        let manager = Manager::new(store.connections.clone(), detected);
        let splash = (splash && config.animations && connect.is_none()).then(Instant::now);
        let mut app = Self {
            motion: crate::motion::Motion::new(config.animations),
            splash,
            splash_logo: Cell::new(None),
            main_area: Cell::new(Rect::default()),
            draw_cost: Duration::ZERO,
            engine,
            icons: Icons::new(config.icons),
            config,
            keymap,
            theme,
            depth,
            config_dir,
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
            update,
            should_quit: false,
            frame: 0,
        };
        let now = Instant::now();
        for problem in problems {
            app.log.push(format!("Warning: {problem}"));
            app.toasts.push(Level::Warning, problem, now);
        }
        app.show_manager();
        if app.splash.is_some() {
            app.screen = Screen::Splash;
        }
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
        let period = if self.config.animations {
            FRAME
        } else {
            STILL_FRAME
        };
        let mut frames = tokio::time::interval(period);
        frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut pings = tokio::time::interval_at(Instant::now() + PING_EVERY, PING_EVERY);
        pings.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut dirty = true;
        let mut drawn = Instant::now();
        // The splash counts from the first frame, after the terminal is set
        // up.
        if self.splash.is_some() {
            self.splash = Some(drawn);
        }

        while !self.should_quit {
            if dirty {
                let start = std::time::Instant::now();
                terminal.draw(|f| self.render(f))?;
                self.draw_cost = (self.draw_cost * 3 + start.elapsed()) / 4;
                drawn = Instant::now();
                dirty = false;
            }
            let fx_frame = if self.draw_cost > SLOW_DRAW {
                SLOW_FX_FRAME
            } else {
                FX_FRAME
            };
            let before = self.seen();
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
                    self.on_engine_event(event);
                    dirty = true;
                }
                _ = frames.tick(), if self.is_animating() => {
                    self.frame += 1;
                    let now = Instant::now();
                    self.toasts.expire(now);
                    self.update_splash(now);
                    dirty = true;
                }
                _ = tokio::time::sleep_until(drawn + fx_frame), if self.motion.is_running() => {
                    dirty = true;
                }
                _ = pings.tick(), if self.screen == Screen::Manager => {
                    self.with_ctx(|app, ctx| app.manager.ping_all(ctx));
                }
            }
            self.react(before);
        }
        Ok(())
    }

    /// Applies an engine event, with an effect for how a query or a
    /// connection attempt ended.
    pub fn on_engine_event(&mut self, event: EngineEvent) {
        let outcome = self.outcome(&event);
        let failed_connect =
            self.connecting.is_some() && matches!(event, EngineEvent::Connected(Err(_)));
        self.apply(event);
        if let Some(outcome) = outcome {
            self.flash_results(outcome);
        }
        if failed_connect {
            self.flash_failed_connect();
        }
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
                Screen::Splash => true,
                Screen::Manager => self.manager.is_animating(),
                Screen::Form => false,
                Screen::Workspace => self.workspace.as_ref().is_some_and(|w| w.is_animating()),
            }
    }

    fn wants_text(&self) -> bool {
        match self.screen {
            Screen::Splash | Screen::Manager => false,
            Screen::Form => self.form.as_ref().is_some_and(|f| f.wants_text()),
            Screen::Workspace => self.workspace.as_ref().is_some_and(|w| w.wants_text()),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        // A key never waits for an effect.
        self.motion.finish();
        if self.splash.is_some() {
            self.end_splash();
            return;
        }
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
            Overlay::Help(mut help) if help.capturing => {
                if key.code == KeyCode::Esc {
                    help.capturing = false;
                    help.taken = None;
                } else {
                    self.rebind(&mut help, key);
                }
                self.overlay = Some(Overlay::Help(help));
            }
            Overlay::Help(mut help) => {
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => help.select(1),
                    KeyCode::Up | KeyCode::Char('k') => help.select(-1),
                    KeyCode::PageDown => help.select(10),
                    KeyCode::PageUp => help.select(-10),
                    KeyCode::Enter => help.capturing = true,
                    KeyCode::Delete | KeyCode::Backspace => {
                        let action = help.action();
                        let saved = self.set_keys(action, None);
                        self.keys_changed(action, None, saved);
                    }
                    _ => return,
                }
                self.overlay = Some(Overlay::Help(help));
            }
            Overlay::Menu => {
                if let KeyCode::Char(c) = key.code {
                    if let Some(action) = which_key::action_for(c) {
                        self.dispatch(action);
                    }
                }
            }
            Overlay::Copy(mut dialog) => match dialog.handle(key) {
                Some(crate::components::copy::CopyEvent::Copy) => self.start_copy(dialog),
                Some(crate::components::copy::CopyEvent::Close) => {}
                None => self.overlay = Some(Overlay::Copy(dialog)),
            },
            Overlay::Confirm(confirm) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.apply_command(confirm.on_yes),
                KeyCode::Enter if !confirm.danger => self.apply_command(confirm.on_yes),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.overlay = Some(Overlay::Confirm(confirm)),
            },
            Overlay::Password {
                connection,
                mut typed,
                ask,
            } => match key.code {
                KeyCode::Enter => match ask {
                    Ask::Database { ssh } => self.connect_with(connection, Some(typed), ssh),
                    Ask::Ssh { password } => {
                        self.connect_with(connection, Some(password), Some(typed))
                    }
                },
                KeyCode::Esc => {}
                KeyCode::Backspace => {
                    typed.pop();
                    self.overlay = Some(Overlay::Password {
                        connection,
                        typed,
                        ask,
                    });
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    typed.push(c);
                    self.overlay = Some(Overlay::Password {
                        connection,
                        typed,
                        ask,
                    });
                }
                _ => {
                    self.overlay = Some(Overlay::Password {
                        connection,
                        typed,
                        ask,
                    })
                }
            },
        }
    }

    pub fn dispatch(&mut self, action: Action) {
        match action {
            Action::Help => self.overlay = Some(Overlay::Help(help::Help::default())),
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
                    Screen::Splash => None,
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

    /// `key`, pressed while the key list waits for the selected action's new
    /// key, becomes its only key. A key another action has moves only when
    /// pressed twice.
    fn rebind(&mut self, help: &mut help::Help, event: KeyEvent) {
        let action = help.action();
        let key = Key::new(event.code, event.modifiers);
        let Some(name) = key.name() else {
            self.toasts.push(
                Level::Warning,
                format!("{} can't be bound", key.label()),
                Instant::now(),
            );
            return;
        };
        let owner = self.keymap.owner(key).filter(|owner| *owner != action);
        if let Some(owner) = owner {
            if help.taken != Some(key) {
                help.taken = Some(key);
                return;
            }
            let rest: Vec<String> = self
                .keymap
                .keys(owner)
                .iter()
                .filter(|k| **k != key)
                .filter_map(Key::name)
                .collect();
            // Saving the second one reports for both.
            let _ = self.set_keys(owner, Some(rest));
        }
        let saved = self.set_keys(action, Some(vec![name]));
        self.keys_changed(action, owner, saved);
        help.capturing = false;
        help.taken = None;
    }

    /// Gives `action` `keys`, or its defaults with `None`, here and in
    /// `config.toml`.
    fn set_keys(&mut self, action: Action, keys: Option<Vec<String>>) -> Result<(), String> {
        let name = action.name();
        match &keys {
            Some(keys) => self.config.keys.insert(name.to_string(), keys.clone()),
            None => self.config.keys.remove(name),
        };
        (self.keymap, _) = Keymap::new(self.config.keymap, &self.config.keys);
        match &self.config_dir {
            Some(dir) => Config::save_keys(dir, name, keys.as_deref()),
            None => Err("no config directory, so only until dfox quits".to_string()),
        }
    }

    /// Says what `action`'s keys are now, and whether they were saved.
    fn keys_changed(&mut self, action: Action, from: Option<Action>, saved: Result<(), String>) {
        let keys = self.keymap.keys_for(action).join(" ");
        let mut text = format!("{}: {keys}", action.description());
        if let Some(from) = from {
            text.push_str(&format!(", taken from {}", from.description()));
        }
        let (level, text) = match saved {
            Ok(()) => (Level::Info, format!("{text} · saved")),
            Err(err) => (Level::Warning, format!("{text} · not saved: {err}")),
        };
        self.toasts.push(level, text, Instant::now());
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

    /// Draws the frame, then the running effects over it.
    pub fn render(&mut self, f: &mut Frame) {
        let now = Instant::now();
        let ctx = RenderCtx {
            theme: &self.theme,
            icons: &self.icons,
            keymap: &self.keymap,
            now,
            frame: self.frame,
            animations: self.config.animations,
        };
        self.draw(f, &ctx);
        if let Some(logo) = self.splash_logo.take() {
            let effect = crate::motion::splash_in(&self.theme, logo);
            self.motion.start(crate::motion::Slot::Splash, effect);
        }
        let area = f.area();
        self.motion.apply(f.buffer_mut(), area, now);
    }

    fn draw(&self, f: &mut Frame, ctx: &RenderCtx) {
        let area = f.area();
        f.render_widget(Paragraph::new("").style(ctx.theme.base()), area);
        if self.screen == Screen::Splash {
            let (progress, label) = self.splash_progress(ctx.now);
            let logo = splash::render(f, area, progress, &label, ctx);
            // The first splash frame starts the logo's sweep.
            if self.frame == 0 && !self.motion.is_running() {
                self.splash_logo.set(Some(logo));
            }
            return;
        }
        let [main, status] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        self.main_area.set(main);

        match self.screen {
            Screen::Splash => {}
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
            Some(Overlay::Help(state)) => help::render(f, main, state, ctx),
            Some(Overlay::Palette(palette)) => palette.render(f, main, ctx),
            Some(Overlay::Menu) => which_key::render(f, main, ctx),
            Some(Overlay::Confirm(confirm)) => confirm.render(f, main, ctx),
            Some(Overlay::Copy(dialog)) => dialog.render(f, main, ctx),
            Some(Overlay::Password {
                connection,
                typed,
                ask,
            }) => {
                let title = match ask {
                    Ask::Database { .. } => format!("Password for {}", connection.name),
                    Ask::Ssh { .. } => format!("SSH password for {}", connection.name),
                };
                prompt::render(f, main, &title, typed.chars().count(), ctx)
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
