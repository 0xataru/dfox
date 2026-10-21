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
    /// Only `y` says yes, not Enter, and the frame is red.
    pub danger: bool,
}

/// Columns of message text in the dialog.
const TEXT_WIDTH: usize = 50;

impl Confirm {
    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let lines: usize = self
            .message
            .lines()
            .map(|l| l.chars().count().div_ceil(TEXT_WIDTH).max(1))
            .sum();
        let popup = centered(area, TEXT_WIDTH as u16 + 4, lines as u16 + 6);
        let border = if self.danger {
            theme.danger()
        } else {
            theme.warning()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(Span::styled(format!(" {} ", self.title), theme.title(true)))
            .style(theme.surface());
        let mut text: Vec<Line> = vec![Line::from("")];
        text.extend(self.message.lines().map(Line::from));
        text.extend([
            Line::from(""),
            Line::from(vec![
                Span::styled(" y ", theme.key()),
                Span::styled("yes   ", theme.dim()),
                Span::styled(" n ", theme.key()),
                Span::styled("no", theme.dim()),
            ]),
        ]);
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
            danger: false,
        };
        insta::assert_snapshot!(render(&test, 60, 11, |f, area, ctx| confirm.render(f, area, ctx)));
    }

    #[test]
    fn confirm_dialog_with_a_long_message() {
        let test = TestCtx::new();
        let confirm = Confirm {
            title: "Destructive query".into(),
            message: "DELETE without WHERE on orders\nDROP TABLE customers_with_a_rather_long_name_that_wraps_around\n\nRun it anyway?".into(),
            on_yes: Command::Quit,
            danger: true,
        };
        insta::assert_snapshot!(render(&test, 70, 14, |f, area, ctx| confirm.render(f, area, ctx)));
    }
}
