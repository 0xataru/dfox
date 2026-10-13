//! Asks for a password before connecting, when none is saved.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use super::centered;
use crate::component::RenderCtx;

pub fn render(f: &mut Frame, area: Rect, connection: &str, typed: usize, ctx: &RenderCtx) {
    let theme = ctx.theme;
    let popup = centered(area, 52, 7);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border(true))
        .title(Span::styled(
            format!(" Password for {connection} "),
            theme.title(true),
        ))
        .style(theme.surface());
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(format!("{} ", ctx.icons.selected), theme.key()),
            Span::raw("•".repeat(typed)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Enter", theme.key()),
            Span::styled(" connect   ", theme.dim()),
            Span::styled("Esc", theme.key()),
            Span::styled(" cancel", theme.dim()),
        ]),
    ];
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(block), popup);
    f.set_cursor_position((popup.x + 4 + typed.min(44) as u16, popup.y + 2));
}

#[cfg(test)]
mod tests {
    use crate::testing::{render, TestCtx};

    #[test]
    fn password_prompt() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 60, 9, |f, area, ctx| super::render(
            f, area, "prod", 5, ctx
        )));
    }
}
