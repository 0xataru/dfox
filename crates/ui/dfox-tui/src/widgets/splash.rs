//! The splash screen: the fox assembling, the name, and a bar that fills as
//! dfox checks the saved connections.

use std::time::Duration;

use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    text::Line,
    widgets::Paragraph,
    Frame,
};
use tui_big_text::{BigText, PixelSize};

use super::{fox::Fox, progress::ProgressBar};
use crate::{component::RenderCtx, theme::ColorDepth};

const TAGLINE: &str = "blazing fast · every database";

/// How long the fox takes to assemble: within the splash's shortest stay.
pub const ASSEMBLE: Duration = Duration::from_millis(380);

/// Draws the splash in `area`, `elapsed` since it started, with the bar at
/// `progress` (0.0 to 1.0) and `label` under it. Returns where the name is,
/// for its sweep-in.
pub fn render(
    f: &mut Frame,
    area: Rect,
    elapsed: Duration,
    progress: f64,
    label: &str,
    ctx: &RenderCtx,
) -> Rect {
    let theme = ctx.theme;
    f.render_widget(Paragraph::new("").style(theme.base()), area);
    // Half-height glyphs are 8 columns wide; quadrants need half that.
    let (pixel_size, logo_width) = if area.width >= 40 {
        (PixelSize::HalfHeight, 32)
    } else {
        (PixelSize::Quadrant, 16)
    };
    // The fox where there is room for it, in Unicode.
    let fox_rows = match area.height {
        _ if ctx.icons.ascii => 0,
        28.. => 14,
        22.. => 10,
        _ => 0,
    };
    let [fox_area, _, logo, _, tagline, _, bar, label_area] = Layout::vertical([
        Constraint::Length(fox_rows),
        Constraint::Length(if fox_rows > 0 { 1 } else { 0 }),
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .flex(Flex::Center)
    .areas(area);
    if fox_rows > 0 {
        let [fox_area] = Layout::horizontal([Constraint::Length(Fox::width(fox_rows))])
            .flex(Flex::Center)
            .areas(fox_area);
        let fox = Fox {
            progress: elapsed.as_secs_f32() / ASSEMBLE.as_secs_f32(),
            depth: ColorDepth::of(theme.bg),
        };
        f.render_widget(fox, fox_area);
    }
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
    use std::time::Duration;

    use crate::testing::{render, TestCtx};

    #[test]
    fn splash() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 60, 14, |f, area, ctx| {
            super::render(
                f,
                area,
                Duration::ZERO,
                0.5,
                "checking connections 1/2",
                ctx,
            );
        }));
    }

    #[test]
    fn splash_narrow() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 30, 10, |f, area, ctx| {
            super::render(f, area, Duration::ZERO, 1.0, "", ctx);
        }));
    }

    #[test]
    fn splash_with_the_fox() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 60, 30, |f, area, ctx| {
            super::render(f, area, super::ASSEMBLE, 1.0, "", ctx);
        }));
    }
}
