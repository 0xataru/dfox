//! The command menu: one key per command, shown after the menu key.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::{action::Action, component::RenderCtx};

pub const ENTRIES: &[(char, Action, &str)] = &[
    ('r', Action::Execute, "Run the query"),
    ('c', Action::CopyRow, "Copy the row"),
    ('a', Action::CopyAll, "Copy all rows"),
    ('d', Action::Databases, "Databases"),
    ('t', Action::NextTheme, "Next theme"),
    ('l', Action::Debug, "Event log"),
    ('?', Action::Help, "Keys"),
    ('q', Action::Quit, "Quit"),
];

/// The action for a key pressed while the menu is open.
pub fn action_for(c: char) -> Option<Action> {
    ENTRIES
        .iter()
        .find(|(key, _, _)| *key == c)
        .map(|(_, action, _)| *action)
}

/// Draws the menu in the bottom-right corner of `area`.
pub fn render(f: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let theme = ctx.theme;
    let width = 28.min(area.width);
    let height = (ENTRIES.len() as u16 + 2).min(area.height);
    let popup = Rect::new(
        area.x + area.width - width,
        area.y + area.height - height,
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border(true))
        .title(Span::styled(" Menu ", theme.title(true)))
        .style(theme.surface());
    let lines: Vec<Line> = ENTRIES
        .iter()
        .map(|(key, _, label)| {
            Line::from(vec![
                Span::styled(format!(" {key}  "), theme.key()),
                Span::raw(*label),
            ])
        })
        .collect();
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(block), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, TestCtx};

    #[test]
    fn menu_keys_map_to_actions() {
        assert_eq!(action_for('r'), Some(Action::Execute));
        assert_eq!(action_for('z'), None);
    }

    #[test]
    fn which_key_menu() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 50, 14, super::render));
    }
}
