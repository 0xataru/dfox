//! Motion: short effects drawn over the finished frame with `tachyonfx`.
//!
//! The app starts an effect when something changes (a screen, the focused
//! panel, a finished query), and [`Motion::apply`] advances every running
//! effect by the time since the last frame. Effects never hold up input: a
//! key press finishes them on the next frame. With animations off, nothing is
//! started at all.

use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    layout::{Margin, Rect},
};
use tachyonfx::{fx, CellFilter, Effect, EffectManager, Interpolation, Motion as Direction};
use tokio::time::Instant;

use crate::theme::Theme;

/// What an effect is for. A new effect replaces a running one in the same
/// slot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    #[default]
    Splash,
    Screen,
    Tab,
    Focus,
    Results,
    /// The first rows of a result sweeping in.
    Rows,
    /// A failed query's results shaking.
    Shake,
    /// A new theme washing over the screen.
    Theme,
    /// Saved rows: deleted ones breaking up, changed cells flashing.
    Saved,
    Manager,
    Toast,
}

pub struct Motion {
    manager: EffectManager<Slot>,
    enabled: bool,
    /// When effects were last advanced.
    last: Option<Instant>,
    /// Finish every effect on the next frame.
    finish: bool,
}

impl std::fmt::Debug for Motion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Motion")
            .field("enabled", &self.enabled)
            .field("running", &self.is_running())
            .finish()
    }
}

impl Motion {
    pub fn new(enabled: bool) -> Self {
        Self {
            manager: EffectManager::default(),
            enabled,
            last: None,
            finish: false,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Starts `effect` in `slot`, replacing what runs there.
    pub fn start(&mut self, slot: Slot, effect: Effect) {
        if self.enabled {
            self.manager.add_unique_effect(slot, effect);
        }
    }

    pub fn is_running(&self) -> bool {
        self.manager.is_running()
    }

    /// Makes every running effect jump to its end on the next frame, so a
    /// key press never waits for one.
    pub fn finish(&mut self) {
        if self.is_running() {
            self.finish = true;
        }
    }

    /// Draws the running effects over `buf`, advanced to `now`.
    pub fn apply(&mut self, buf: &mut Buffer, area: Rect, now: Instant) {
        if !self.is_running() {
            self.last = None;
            self.finish = false;
            return;
        }
        let elapsed = if self.finish {
            Duration::from_secs(60)
        } else {
            self.last
                .map_or(Duration::ZERO, |last| now.saturating_duration_since(last))
        };
        let millis = u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX);
        self.manager
            .process_effects(tachyonfx::Duration::from_millis(millis), buf, area);
        self.last = Some(now);
        self.finish = false;
    }
}

/// The border of an effect's area.
fn border() -> CellFilter {
    CellFilter::Outer(Margin::new(1, 1))
}

/// A newly focused panel's border lights up and settles into its colour.
pub fn focus(theme: &Theme, area: Rect) -> Effect {
    fx::fade_from_fg(theme.text, (180, Interpolation::QuadOut))
        .with_filter(border())
        .with_area(area)
}

/// A new screen fades in from the background.
pub fn screen(theme: &Theme, area: Rect) -> Effect {
    fx::fade_from(theme.bg, theme.bg, (150, Interpolation::QuadOut)).with_area(area)
}

/// A panel's border flashes `color` and fades back: green for a finished
/// query, red for an error, amber for a cancel.
pub fn flash(color: ratatui::style::Color, area: Rect) -> Effect {
    fx::fade_from_fg(color, (250, Interpolation::QuadOut))
        .with_filter(border())
        .with_area(area)
}

/// The first rows of a result sweep in from the left, as they arrive.
pub fn rows_in(theme: &Theme, area: Rect) -> Effect {
    fx::sweep_in(
        Direction::LeftToRight,
        12,
        0,
        theme.bg,
        (160, Interpolation::QuadOut),
    )
    .with_area(area)
}

/// The area shakes sideways a few times, less each time: a query failed.
pub fn shake(area: Rect) -> Effect {
    fx::effect_fn_buf((), (220, Interpolation::Linear), |_, context, buf| {
        let alpha = context.timer.alpha();
        let offset = ((alpha * std::f32::consts::PI * 6.).sin() * 2. * (1. - alpha)).round() as i32;
        let area = context.area.intersection(buf.area);
        if offset == 0 || area.width == 0 {
            return;
        }
        for y in area.top()..area.bottom() {
            let row: Vec<_> = (area.left()..area.right())
                .map(|x| buf[(x, y)].clone())
                .collect();
            for (i, x) in (area.left()..area.right()).enumerate() {
                let from = i as i32 - offset;
                if let Some(cell) = usize::try_from(from).ok().and_then(|from| row.get(from)) {
                    buf[(x, y)] = cell.clone();
                } else {
                    buf[(x, y)].set_symbol(" ");
                }
            }
        }
    })
    .with_area(area)
}

/// A toast slides in from the right.
pub fn toast(theme: &Theme, area: Rect) -> Effect {
    fx::slide_in(
        Direction::RightToLeft,
        8,
        0,
        theme.bg,
        (200, Interpolation::QuadOut),
    )
    .with_area(area)
}

/// A new theme washes over the screen from the top-left corner, the old
/// background giving way.
pub fn new_theme(old_bg: ratatui::style::Color, area: Rect) -> Effect {
    fx::fade_from(old_bg, old_bg, (250, Interpolation::QuadOut))
        .with_pattern(
            tachyonfx::pattern::DiagonalPattern::top_left_to_bottom_right()
                .with_transition_width(12.),
        )
        .with_area(area)
}

/// Saved edits: each deleted row breaks into cells that drop away, each
/// changed cell flashes the accent colour. `None` when none is on screen.
pub fn saved(theme: &Theme, deleted: &[Rect], changed: &[Rect]) -> Option<Effect> {
    let effects: Vec<Effect> = deleted
        .iter()
        .map(|row| fx::dissolve((250, Interpolation::QuadOut)).with_area(*row))
        .chain(changed.iter().map(|cell| {
            fx::fade_from(theme.accent, theme.accent, (250, Interpolation::QuadOut))
                .with_area(*cell)
        }))
        .collect();
    (!effects.is_empty()).then(|| fx::parallel(&effects))
}

/// The workspace, once connected, forms out of scattered cells.
pub fn connected(area: Rect) -> Effect {
    fx::coalesce((250, Interpolation::QuadOut)).with_area(area)
}

/// The splash logo sweeps in from the left.
pub fn splash_in(theme: &Theme, area: Rect) -> Effect {
    fx::sweep_in(
        Direction::LeftToRight,
        20,
        0,
        theme.bg,
        (400, Interpolation::QuadOut),
    )
    .with_area(area)
}

/// The first screen after the splash forms out of scattered cells.
pub fn splash_out(area: Rect) -> Effect {
    fx::coalesce((250, Interpolation::QuadOut)).with_area(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Effects take at most this long; longer is a loader, not motion.
    const MAX: Duration = Duration::from_millis(250);

    fn frame() -> (Buffer, Rect) {
        let area = Rect::new(0, 0, 20, 5);
        (Buffer::empty(area), area)
    }

    #[test]
    fn effects_run_out_on_their_own() {
        let theme = Theme::default();
        let (mut buf, area) = frame();
        let mut motion = Motion::new(true);
        motion.start(Slot::Focus, focus(&theme, area));
        assert!(motion.is_running());

        let start = Instant::now();
        motion.apply(&mut buf, area, start);
        assert!(motion.is_running());
        motion.apply(&mut buf, area, start + Duration::from_millis(100));
        assert!(motion.is_running());
        motion.apply(&mut buf, area, start + MAX);
        assert!(!motion.is_running());
        // Nothing left to do.
        motion.apply(&mut buf, area, start + MAX * 2);
    }

    #[test]
    fn transitions_take_at_most_250_ms() {
        let theme = Theme::default();
        let area = Rect::new(0, 0, 20, 5);
        for effect in [
            focus(&theme, area),
            screen(&theme, area),
            flash(theme.danger, area),
            toast(&theme, area),
            splash_out(area),
            rows_in(&theme, area),
            shake(area),
            new_theme(theme.bg, area),
            connected(area),
        ] {
            let mut buf = Buffer::empty(area);
            let mut motion = Motion::new(true);
            motion.start(Slot::Screen, effect);
            let now = Instant::now();
            motion.apply(&mut buf, area, now);
            motion.apply(&mut buf, area, now + MAX);
            assert!(!motion.is_running());
        }
    }

    #[test]
    fn a_shake_moves_the_content_sideways_and_back() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "ab", ratatui::style::Style::default());
        let mut motion = Motion::new(true);
        motion.start(Slot::Shake, shake(area));
        let now = Instant::now();
        motion.apply(&mut buf.clone(), area, now);
        // A twelfth of the way in, the content is pushed right.
        let mut moved = buf.clone();
        motion.apply(&mut moved, area, now + Duration::from_millis(18));
        assert_ne!(moved[(0, 0)].symbol(), "a");
        motion.apply(&mut buf, area, now + MAX);
        assert_eq!(buf[(0, 0)].symbol(), "a");
    }

    #[test]
    fn a_key_finishes_every_effect() {
        let theme = Theme::default();
        let (mut buf, area) = frame();
        let mut motion = Motion::new(true);
        motion.start(Slot::Screen, screen(&theme, area));
        motion.start(Slot::Toast, toast(&theme, area));
        motion.start(Slot::Splash, splash_in(&theme, area));
        motion.start(Slot::Results, flash(theme.success, area));
        motion.start(Slot::Manager, splash_out(area));
        let now = Instant::now();
        motion.apply(&mut buf, area, now);
        motion.finish();
        motion.apply(&mut buf, area, now);
        assert!(!motion.is_running());
    }

    #[test]
    fn a_slot_holds_one_effect() {
        let theme = Theme::default();
        let (mut buf, area) = frame();
        let mut motion = Motion::new(true);
        let now = Instant::now();
        motion.start(Slot::Focus, focus(&theme, area));
        motion.apply(&mut buf, area, now);
        motion.apply(&mut buf, area, now + Duration::from_millis(100));
        // Restarting the slot starts its timer again: 200 ms in, the first
        // effect would be over, the second is only half way.
        motion.start(Slot::Focus, focus(&theme, area));
        motion.apply(&mut buf, area, now + Duration::from_millis(100));
        motion.apply(&mut buf, area, now + Duration::from_millis(200));
        assert!(motion.is_running());
    }

    #[test]
    fn nothing_starts_with_animations_off() {
        let theme = Theme::default();
        let mut motion = Motion::new(false);
        motion.start(Slot::Focus, focus(&theme, Rect::new(0, 0, 4, 4)));
        assert!(!motion.is_running());
        assert!(!motion.enabled());
        motion.finish();
        assert!(format!("{motion:?}").contains("enabled: false"));
    }
}
