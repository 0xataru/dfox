//! The pieces every screen and panel share.

use dfox_core::engine::EngineHandle;
use ratatui::{layout::Rect, Frame};
use tokio::time::Instant;

use crate::{
    action::Action,
    command::Command,
    config::Config,
    icons::Icons,
    keymap::Keymap,
    theme::Theme,
    widgets::toast::{Level, Toasts},
};

/// A part of the UI with its own state.
pub trait Component {
    /// Reacts to an action. Database work goes through `ctx.engine`; anything
    /// the app itself must do comes back as a [`Command`].
    fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command>;

    /// Draws the current state. Never waits on anything.
    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx);

    /// True while the component needs redrawing without input, for a spinner
    /// or a clock.
    fn is_animating(&self) -> bool {
        false
    }

    /// True while typed characters should go into the component as text
    /// instead of being read as shortcuts.
    fn wants_text(&self) -> bool {
        false
    }
}

/// What handlers can reach besides their own state.
pub struct Ctx<'a> {
    pub engine: &'a EngineHandle,
    pub config: &'a Config,
    pub toasts: &'a mut Toasts,
    pub log: &'a mut EventLog,
    pub now: Instant,
}

impl Ctx<'_> {
    /// Shows a toast and records it in the event log.
    pub fn toast(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        self.log.push(format!("{level:?}: {text}"));
        self.toasts.push(level, text, self.now);
    }
}

/// What rendering can read besides the component's own state.
pub struct RenderCtx<'a> {
    pub theme: &'a Theme,
    pub icons: &'a Icons,
    pub keymap: &'a Keymap,
    pub now: Instant,
    /// Counts redraws while animating, to step spinners.
    pub frame: u64,
    pub animations: bool,
}

/// Notable events, shown by the debug action.
#[derive(Debug, Default)]
pub struct EventLog {
    started: Option<Instant>,
    entries: Vec<String>,
}

impl EventLog {
    const MAX: usize = 200;

    pub fn push(&mut self, entry: String) {
        let started = *self.started.get_or_insert_with(Instant::now);
        log::info!("{entry}");
        self.entries.push(format!(
            "[{:>7.1}s] {entry}",
            started.elapsed().as_secs_f64()
        ));
        if self.entries.len() > Self::MAX {
            self.entries.remove(0);
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }
}

/// Which of a fixed set of panels has focus.
#[derive(Debug, Clone)]
pub struct Focus<T: Copy + PartialEq + 'static> {
    order: &'static [T],
    current: usize,
}

impl<T: Copy + PartialEq + 'static> Focus<T> {
    pub fn new(order: &'static [T]) -> Self {
        Self { order, current: 0 }
    }

    pub fn get(&self) -> T {
        self.order[self.current]
    }

    pub fn is(&self, panel: T) -> bool {
        self.get() == panel
    }

    pub fn set(&mut self, panel: T) {
        if let Some(index) = self.order.iter().position(|p| *p == panel) {
            self.current = index;
        }
    }

    pub fn next(&mut self) {
        self.current = (self.current + 1) % self.order.len();
    }

    pub fn prev(&mut self) {
        self.current = (self.current + self.order.len() - 1) % self.order.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_cycles_both_ways() {
        let mut focus = Focus::new(&[1, 2, 3]);
        focus.prev();
        assert_eq!(focus.get(), 3);
        focus.next();
        focus.next();
        assert_eq!(focus.get(), 2);
        focus.set(1);
        assert!(focus.is(1));
    }

    #[test]
    fn the_event_log_keeps_the_latest_entries() {
        let mut log = EventLog::default();
        for i in 0..250 {
            log.push(format!("event {i}"));
        }
        assert_eq!(log.entries().len(), EventLog::MAX);
        assert!(log.entries()[0].ends_with("event 50"));
    }
}
