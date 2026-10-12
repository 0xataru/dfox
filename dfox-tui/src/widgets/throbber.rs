//! The spinner shown while something loads.

use ratatui::{style::Style, text::Span};

use crate::component::RenderCtx;

/// The spinner's current frame, or a still symbol with animations off.
pub fn spinner(ctx: &RenderCtx, style: Style) -> Span<'static> {
    let set = &ctx.icons.spinner;
    let symbol = if ctx.animations && !set.symbols.is_empty() {
        set.symbols[(ctx.frame % set.symbols.len() as u64) as usize]
    } else {
        set.full
    };
    Span::styled(symbol, style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestCtx;

    #[test]
    fn the_spinner_steps_with_the_frame_and_stops_without_animations() {
        let test = TestCtx::new();
        let mut ctx = test.render_ctx();
        let frames: Vec<String> = (0..7)
            .map(|frame| {
                ctx.frame = frame;
                spinner(&ctx, Style::default()).content.into_owned()
            })
            .collect();
        assert_eq!(frames, ["⠷", "⠯", "⠟", "⠻", "⠽", "⠾", "⠷"]);

        ctx.animations = false;
        assert_eq!(spinner(&ctx, Style::default()).content, "⠿");
    }
}
