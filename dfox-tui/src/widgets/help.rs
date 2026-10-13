//! Every key binding, generated from the keymap so it is never out of date.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use super::{centered, truncate};
use crate::{action::Action, component::RenderCtx};

/// Draws the key list, `scroll` lines down. Scrolling stops at the end.
pub fn render(f: &mut Frame, area: Rect, scroll: usize, ctx: &RenderCtx) {
    let theme = ctx.theme;
    const KEY_WIDTH: usize = 18;
    let entries: Vec<(String, &str)> = Action::BINDABLE
        .iter()
        .map(|action| {
            // As many keys as fit; the rest are alternatives.
            let mut keys = String::new();
            for key in ctx.keymap.keys_for(*action) {
                if keys.is_empty() {
                    keys = key;
                } else if keys.chars().count() + 1 + key.chars().count() < KEY_WIDTH {
                    keys = format!("{keys} {key}");
                }
            }
            (keys, action.description())
        })
        .filter(|(keys, _)| !keys.is_empty())
        .collect();

    // Key, description (at most 24) and a gap, per column, plus the border.
    let two_columns = area.width >= 2 * (KEY_WIDTH as u16 + 25) + 4;
    let three_columns = area.width >= 3 * (KEY_WIDTH as u16 + 25) + 4;
    let columns = if three_columns {
        3
    } else if two_columns {
        2
    } else {
        1
    };
    let rows = entries.len().div_ceil(columns);
    let width = columns as u16 * (KEY_WIDTH as u16 + 25) + 4;
    let popup = centered(area, width, rows as u16 + 4);
    let shown = (popup.height as usize).saturating_sub(4).max(1);
    let scroll = scroll.min(rows.saturating_sub(shown));
    let hint = if rows > shown {
        " ↑↓ scroll · any other key closes "
    } else {
        " any key to close "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border(true))
        .title(Span::styled(" Keys ", theme.title(true)))
        .title_bottom(Line::from(Span::styled(hint, theme.dim())).centered())
        .style(theme.surface());
    let inner = block.inner(popup).inner(ratatui::layout::Margin::new(1, 1));
    let rows = rows.max(1);
    f.render_widget(Clear, popup);
    f.render_widget(block, popup);

    let areas = Layout::horizontal(vec![Constraint::Fill(1); columns]).split(inner);
    for (column, chunk) in areas.iter().zip(entries.chunks(rows)) {
        let key_width = KEY_WIDTH;
        let lines: Vec<Line> = chunk
            .iter()
            .skip(scroll)
            .map(|(keys, description)| {
                Line::from(vec![
                    Span::styled(
                        format!("{:<key_width$}", truncate(keys, key_width - 1)),
                        theme.key(),
                    ),
                    Span::styled(*description, theme.base().bg(theme.surface)),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(lines), *column);
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::{render, TestCtx};

    #[test]
    fn help_wide() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 120, 40, |f, area, ctx| super::render(
            f, area, 0, ctx
        )));
    }

    #[test]
    fn help_narrow() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 80, 24, |f, area, ctx| super::render(
            f, area, 0, ctx
        )));
    }

    #[test]
    fn help_scrolled_to_the_end() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 80, 24, |f, area, ctx| super::render(
            f, area, 1000, ctx
        )));
    }
}
