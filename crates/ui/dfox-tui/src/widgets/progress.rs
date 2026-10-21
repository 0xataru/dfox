//! A thin progress bar: filled to a ratio with a colour gradient, or, with no
//! ratio, a segment sweeping back and forth.

use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::Widget};

use crate::theme::Theme;

pub struct ProgressBar {
    /// 0.0 to 1.0, or `None` while the total is unknown.
    pub ratio: Option<f64>,
    pub from: Color,
    pub to: Color,
    pub track: Color,
    /// Animation step for the sweeping segment.
    pub frame: u64,
}

impl ProgressBar {
    pub fn new(theme: &Theme, ratio: Option<f64>, frame: u64) -> Self {
        Self {
            ratio,
            from: theme.accent,
            to: theme.accent_alt,
            track: theme.border,
            frame,
        }
    }
}

impl Widget for ProgressBar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let width = area.width as usize;
        if width == 0 || area.height == 0 {
            return;
        }
        let (start, end) = match self.ratio {
            Some(ratio) => (0, (ratio.clamp(0.0, 1.0) * width as f64).round() as usize),
            None => {
                let len = (width / 5).max(3).min(width);
                let travel = (width - len).max(1) as u64;
                let step = self.frame % (2 * travel);
                let start = if step < travel {
                    step
                } else {
                    2 * travel - step
                } as usize;
                (start, start + len)
            }
        };
        for x in 0..width {
            let cell = &mut buf[(area.x + x as u16, area.y)];
            if (start..end).contains(&x) {
                let at = if width > 1 {
                    x as f64 / (width - 1) as f64
                } else {
                    0.0
                };
                cell.set_symbol("━")
                    .set_fg(Theme::blend(self.from, self.to, at));
            } else {
                cell.set_symbol("─").set_fg(self.track);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    fn line(ratio: Option<f64>, frame: u64) -> String {
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        ProgressBar::new(&Theme::default(), ratio, frame).render(area, &mut buf);
        (0..20).map(|x| buf[(x, 0)].symbol().to_string()).collect()
    }

    #[test]
    fn determinate_fills_to_the_ratio() {
        insta::assert_snapshot!(
            [line(Some(0.0), 0), line(Some(0.25), 0), line(Some(1.0), 0)].join("\n"),
            @r"
        ────────────────────
        ━━━━━───────────────
        ━━━━━━━━━━━━━━━━━━━━
        "
        );
    }

    #[test]
    fn indeterminate_sweeps_back_and_forth() {
        insta::assert_snapshot!(
            [line(None, 0), line(None, 8), line(None, 16), line(None, 24)].join("\n"),
            @"
        ━━━━────────────────
        ────────━━━━────────
        ────────────────━━━━
        ────────━━━━────────
        "
        );
    }

    #[test]
    fn the_gradient_runs_from_one_colour_to_the_other() {
        let area = Rect::new(0, 0, 3, 1);
        let mut buf = Buffer::empty(area);
        let theme = Theme::default();
        ProgressBar::new(&theme, Some(1.0), 0).render(area, &mut buf);
        assert_eq!(buf[(0, 0)].fg, theme.accent);
        assert_eq!(buf[(2, 0)].fg, theme.accent_alt);
    }
}
