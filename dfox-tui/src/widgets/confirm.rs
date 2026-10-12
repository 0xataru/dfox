//! A yes/no question over the screen.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use super::centered;
use crate::{command::Command, component::RenderCtx};

#[derive(Debug, Clone, PartialEq)]
pub struct Confirm {
    pub title: String,
    pub message: String,
    /// Done when the answer is yes.
    pub on_yes: Command,
}

impl Confirm {
    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let popup = centered(area, 44, 7);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.warning())
            .title(Span::styled(format!(" {} ", self.title), theme.title(true)))
            .style(theme.surface());
        let text = vec![
            Line::from(self.message.as_str()),
            Line::from(""),
            Line::from(vec![
                Span::styled(" y ", theme.key()),
                Span::styled("yes   ", theme.dim()),
                Span::styled(" n ", theme.key()),
                Span::styled("no", theme.dim()),
            ]),
        ];
        f.render_widget(Clear, popup);
        f.render_widget(
            Paragraph::new(text)
                .block(block)
                .centered()
                .wrap(Wrap { trim: true }),
            popup,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, TestCtx};

    #[test]
    fn confirm_dialog() {
        let test = TestCtx::new();
        let confirm = Confirm {
            title: "Quit".into(),
            message: "Quit dfox? A query is still running.".into(),
            on_yes: Command::Quit,
        };
        insta::assert_snapshot!(render(&test, 60, 11, |f, area, ctx| confirm.render(f, area, ctx)));
    }
}
