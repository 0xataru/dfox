//! When to animate: the splash at start, and effects for what changed after
//! each key, click or engine event.

use std::time::Duration;

use dfox_core::{engine::EngineEvent, errors::DbError, result::ResultSet};
use ratatui::layout::Rect;
use tokio::time::Instant;

use super::{App, Screen};
use crate::{
    components::workspace::Panel,
    motion::{self, Slot},
    widgets::toast::Toasts,
};

/// The splash shows at least this long, so the logo can sweep in...
const SPLASH_MIN: Duration = Duration::from_millis(400);
/// ...and never longer than this, even while checks are still running.
const SPLASH_MAX: Duration = Duration::from_millis(600);

/// What effects react to, taken before and after handling something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Seen {
    screen: Screen,
    focus: Option<Panel>,
    tab: Option<usize>,
    toasts: usize,
}

/// A flash on the results border after a query on the tab on screen ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    Done,
    Failed,
    Cancelled,
}

impl App {
    pub(super) fn seen(&self) -> Seen {
        let workspace = self
            .workspace
            .as_ref()
            .filter(|_| self.screen == Screen::Workspace);
        Seen {
            screen: self.screen,
            focus: workspace.map(|w| w.focused()),
            tab: workspace.map(|w| w.active_tab()),
            toasts: self.toasts.len(),
        }
    }

    /// Starts effects for what changed since `before`.
    pub(super) fn react(&mut self, before: Seen) {
        if !self.motion.enabled() {
            return;
        }
        let now = self.seen();
        let main = self.main_area.get();
        if now.screen != before.screen {
            let effect = if before.screen == Screen::Splash {
                motion::splash_out(main)
            } else if (before.screen, now.screen) == (Screen::Manager, Screen::Workspace) {
                motion::connected(main)
            } else {
                motion::screen(&self.theme, main)
            };
            self.motion.start(Slot::Screen, effect);
        } else if let Some(workspace) = &self.workspace {
            if now.tab != before.tab {
                // The editor and results of the new tab fade in.
                let editor = workspace.area(Panel::Editor);
                let results = workspace.area(Panel::Results);
                let effect = motion::screen(&self.theme, editor.union(results));
                self.motion.start(Slot::Tab, effect);
            } else if let (Some(panel), true) = (now.focus, now.focus != before.focus) {
                let effect = motion::focus(&self.theme, workspace.area(panel));
                self.motion.start(Slot::Focus, effect);
            }
        }
        if now.toasts > before.toasts {
            let area = Toasts::newest_area(main);
            self.motion
                .start(Slot::Toast, motion::toast(&self.theme, area));
        }
    }

    /// How a query that ends with `event` ended, when it is on screen.
    pub(super) fn outcome(&self, event: &EngineEvent) -> Option<Outcome> {
        let EngineEvent::QueryDone { id, outcome } = event else {
            return None;
        };
        let shown = self.screen == Screen::Workspace
            && self.workspace.as_ref().is_some_and(|w| w.shows_query(*id));
        if !shown {
            return None;
        }
        Some(match outcome {
            Ok(_) => Outcome::Done,
            Err(DbError::Cancelled) => Outcome::Cancelled,
            Err(_) => Outcome::Failed,
        })
    }

    /// `event` brings the first rows of the result on screen.
    pub(super) fn first_rows(&self, event: &EngineEvent) -> Option<()> {
        let EngineEvent::Batch { id, batch } = event else {
            return None;
        };
        let workspace = self.workspace.as_ref()?;
        let first = self.screen == Screen::Workspace
            && workspace.shows_query(*id)
            && workspace.results().store.row_count() == 0
            && batch.num_rows() > 0;
        first.then_some(())
    }

    /// The first rows sweep in.
    pub(super) fn rows_in(&mut self) {
        if let Some(workspace) = &self.workspace {
            let area = crate::components::inner(workspace.area(Panel::Results));
            self.motion
                .start(Slot::Rows, motion::rows_in(&self.theme, area));
        }
    }

    /// Saved rows: the deleted ones break into cells, changed cells flash.
    pub(super) fn saved_rows(&mut self, saved: crate::components::workspace::Saved) {
        if let Some(effect) = motion::saved(&self.theme, &saved.deleted, &saved.changed) {
            self.motion.start(Slot::Saved, effect);
        }
    }

    /// Flashes the results border green, red or amber.
    pub(super) fn flash_results(&mut self, outcome: Outcome) {
        let Some(workspace) = &self.workspace else {
            return;
        };
        let color = match outcome {
            Outcome::Done => self.theme.success,
            Outcome::Failed => self.theme.danger,
            Outcome::Cancelled => self.theme.warning,
        };
        let area = workspace.area(Panel::Results);
        self.motion.start(Slot::Results, motion::flash(color, area));
        if outcome == Outcome::Failed {
            self.motion.start(Slot::Shake, motion::shake(area));
        }
    }

    /// A failed connect flashes the connection list red.
    pub(super) fn flash_failed_connect(&mut self) {
        let area = self.manager.list_area();
        if area != Rect::default() {
            let effect = motion::flash(self.theme.danger, area);
            self.motion.start(Slot::Manager, effect);
        }
    }

    /// Ends the splash once the checks it waits for are done, or its time
    /// is up.
    pub(super) fn update_splash(&mut self, now: Instant) {
        let Some(started) = self.splash else { return };
        let shown = now.saturating_duration_since(started);
        let (answered, total) = self.manager.ping_progress();
        if shown >= SPLASH_MAX || (answered == total && shown >= SPLASH_MIN) {
            self.end_splash();
        }
    }

    pub(super) fn end_splash(&mut self) {
        if self.splash.take().is_some() {
            self.screen = Screen::Manager;
        }
    }

    /// The splash bar: the share of checks answered, or of the time when
    /// there is nothing to check.
    pub(super) fn splash_progress(&self, now: Instant) -> (f64, String) {
        let (answered, total) = self.manager.ping_progress();
        if total == 0 {
            let shown = self
                .splash
                .map_or(SPLASH_MIN, |s| now.saturating_duration_since(s));
            let ratio = shown.as_secs_f64() / SPLASH_MIN.as_secs_f64();
            return (ratio.min(1.0), "starting".to_string());
        }
        (
            answered as f64 / total as f64,
            format!("checking connections {answered}/{total}"),
        )
    }
}
