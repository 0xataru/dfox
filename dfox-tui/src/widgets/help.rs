//! Every key binding, generated from the keymap so it is never out of date.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use super::{centered, truncate};
use crate::{action::Action, component::RenderCtx};

pub fn render(f: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let theme = ctx.theme;
    let entries: Vec<(String, &str)> = Action::BINDABLE
        .iter()
        .map(|action| (ctx.keymap.keys_for(*action).join(" "), action.description()))
        .filter(|(keys, _)| !keys.is_empty())
        .collect();

    const KEY_WIDTH: usize = 12;
    // Key, description (at most 24) and a gap, per column, plus the border.
    let two_columns = area.width >= 2 * (KEY_WIDTH as u16 + 25) + 4;
    let rows = if two_columns {
        entries.len().div_ceil(2)
    } else {
        entries.len()
    };
    let width = if two_columns {
        2 * (KEY_WIDTH as u16 + 25) + 4
    } else {
        44
    };
    let popup = centered(area, width, rows as u16 + 4);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border(true))
        .title(Span::styled(" Keys ", theme.title(true)))
        .title_bottom(Line::from(Span::styled(" any key to close ", theme.dim())).centered())
        .style(theme.surface());
    let inner = block.inner(popup).inner(ratatui::layout::Margin::new(1, 1));
    let rows = rows.max(1);
    f.render_widget(Clear, popup);
    f.render_widget(block, popup);

    let columns: Vec<Rect> = if two_columns {
        Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)])
            .split(inner)
            .to_vec()
    } else {
        vec![inner]
    };
    for (column, chunk) in columns.iter().zip(entries.chunks(rows)) {
        let key_width = KEY_WIDTH;
        let lines: Vec<Line> = chunk
            .iter()
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
        insta::assert_snapshot!(render(&test, 120, 40, super::render));
    }

    #[test]
    fn help_narrow() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 80, 24, super::render));
    }
}
