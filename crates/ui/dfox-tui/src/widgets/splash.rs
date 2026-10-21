//! The splash screen: the logo, and a bar that fills as dfox checks the
//! saved connections.

use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    text::Line,
    widgets::Paragraph,
    Frame,
};
use tui_big_text::{BigText, PixelSize};

use super::progress::ProgressBar;
use crate::component::RenderCtx;

const TAGLINE: &str = "blazing fast · every database";

/// Draws the splash in `area` with the bar at `progress` (0.0 to 1.0) and
/// `label` under it. Returns where the logo is, for its sweep-in.
pub fn render(f: &mut Frame, area: Rect, progress: f64, label: &str, ctx: &RenderCtx) -> Rect {
    let theme = ctx.theme;
    f.render_widget(Paragraph::new("").style(theme.base()), area);
    // Half-height glyphs are 8 columns wide; quadrants need half that.
    let (pixel_size, logo_width) = if area.width >= 40 {
        (PixelSize::HalfHeight, 32)
    } else {
        (PixelSize::Quadrant, 16)
    };
    let [logo, _, tagline, _, bar, label_area] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .flex(Flex::Center)
    .areas(area);
    let [logo] = Layout::horizontal([Constraint::Length(logo_width)])
        .flex(Flex::Center)
        .areas(logo);

    if ctx.icons.ascii {
        f.render_widget(Paragraph::new("dfox").style(theme.key()).centered(), logo);
    } else {
        let big = BigText::builder()
            .pixel_size(pixel_size)
            .style(theme.key())
            .lines(vec![Line::from("dfox")])
            .build();
        f.render_widget(big, logo);
    }
    f.render_widget(
        Paragraph::new(TAGLINE).style(theme.dim()).centered(),
        tagline,
    );
    let [bar] = Layout::horizontal([Constraint::Length(logo_width.max(24))])
        .flex(Flex::Center)
        .areas(bar);
    f.render_widget(ProgressBar::new(theme, Some(progress), ctx.frame), bar);
    f.render_widget(
        Paragraph::new(label).style(theme.dim()).centered(),
        label_area,
    );
    logo
}

#[cfg(test)]
mod tests {
    use crate::testing::{render, TestCtx};

    #[test]
    fn splash() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 60, 14, |f, area, ctx| {
            super::render(f, area, 0.5, "checking connections 1/2", ctx);
        }));
    }

    #[test]
    fn splash_narrow() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 30, 10, |f, area, ctx| {
            super::render(f, area, 1.0, "", ctx);
        }));
    }
}
