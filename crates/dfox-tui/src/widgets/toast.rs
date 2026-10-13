//! Short messages that stack in the top-right corner and fade out on their
//! own. The line under each shows how long it has left.

use std::time::Duration;

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};
use tokio::time::Instant;

use super::{progress::ProgressBar, truncate};
use crate::component::RenderCtx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

impl Level {
    fn lifetime(self) -> Duration {
        match self {
            Level::Error | Level::Warning => Duration::from_secs(6),
            Level::Info | Level::Success => Duration::from_secs(3),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub level: Level,
    pub text: String,
    pub shown: Instant,
}

#[derive(Debug, Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    /// At most this many show at once; older ones make way.
    const MAX: usize = 4;
    const WIDTH: u16 = 48;

    pub fn push(&mut self, level: Level, text: String, now: Instant) {
        self.items.push(Toast {
            level,
            text,
            shown: now,
        });
        if self.items.len() > Self::MAX {
            self.items.remove(0);
        }
    }

    /// Drops toasts whose time is up. Returns `true` if any were dropped.
    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.items.len();
        self.items
            .retain(|toast| now.saturating_duration_since(toast.shown) < toast.level.lifetime());
        self.items.len() != before
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Where the newest toast is drawn in `area`: at the top.
    pub fn newest_area(area: Rect) -> Rect {
        let width = Self::WIDTH.min(area.width);
        Rect::new(
            area.x + area.width - width,
            area.y,
            width,
            3.min(area.height),
        )
    }

    #[cfg(test)]
    pub fn items(&self) -> &[Toast] {
        &self.items
    }

    /// Draws the stack, newest at the top, in the top-right of `area`.
    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let width = Self::WIDTH.min(area.width);
        let x = area.x + area.width - width;
        for (i, toast) in self.items.iter().rev().enumerate() {
            let y = area.y + 3 * i as u16;
            if y + 3 > area.y + area.height {
                break;
            }
            render_toast(f, Rect::new(x, y, width, 3), toast, ctx);
        }
    }
}

fn render_toast(f: &mut Frame, area: Rect, toast: &Toast, ctx: &RenderCtx) {
    let theme = ctx.theme;
    let (icon, color) = match toast.level {
        Level::Info => (ctx.icons.info, theme.accent_alt),
        Level::Success => (ctx.icons.success, theme.success),
        Level::Warning => (ctx.icons.warning, theme.warning),
        Level::Error => (ctx.icons.error, theme.danger),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .style(theme.surface());
    let text_width = area.width.saturating_sub(4) as usize;
    let line = Line::from(vec![
        Span::styled(format!("{icon} "), Style::default().fg(color)),
        Span::raw(truncate(&toast.text, text_width.saturating_sub(2))),
    ]);

    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(line).block(block), area);

    // Time left, along the bottom border.
    let lifetime = toast.level.lifetime().as_secs_f64();
    let elapsed = ctx.now.saturating_duration_since(toast.shown).as_secs_f64();
    let left = 1.0 - (elapsed / lifetime).clamp(0.0, 1.0);
    let mut bar = ProgressBar::new(theme, Some(left), ctx.frame);
    bar.from = color;
    bar.to = color;
    bar.track = color;
    if area.width > 2 {
        f.render_widget(
            bar,
            Rect::new(area.x + 1, area.y + area.height - 1, area.width - 2, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, TestCtx};

    #[test]
    fn toasts_expire_after_their_lifetime() {
        let now = Instant::now();
        let mut toasts = Toasts::default();
        toasts.push(Level::Success, "copied".into(), now);
        toasts.push(Level::Error, "failed".into(), now);

        assert!(!toasts.expire(now + Duration::from_secs(1)));
        assert!(toasts.expire(now + Duration::from_secs(4)));
        assert_eq!(toasts.items().len(), 1);
        assert!(toasts.expire(now + Duration::from_secs(7)));
        assert!(toasts.is_empty());
    }

    #[test]
    fn only_the_latest_toasts_are_kept() {
        let now = Instant::now();
        let mut toasts = Toasts::default();
        for i in 0..6 {
            toasts.push(Level::Info, format!("toast {i}"), now);
        }
        assert_eq!(toasts.items().len(), Toasts::MAX);
        assert_eq!(toasts.items()[0].text, "toast 2");
    }

    #[test]
    fn toast_stack() {
        let test = TestCtx::new();
        let mut toasts = Toasts::default();
        toasts.push(Level::Info, "Theme: nord".into(), test.now);
        toasts.push(
            Level::Success,
            "Copied 3 rows to the clipboard".into(),
            test.now,
        );
        toasts.push(
            Level::Error,
            "Connection error: connection refused (os error 61) while connecting".into(),
            test.now - Duration::from_secs(3),
        );
        insta::assert_snapshot!(render(&test, 60, 10, |f, area, ctx| toasts.render(f, area, ctx)));
    }
}
