//! Every key binding, generated from the keymap so it is never out of date,
//! and where they are changed: Enter on an action takes its new key.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use super::{centered, truncate};
use crate::{action::Action, component::RenderCtx, keymap::Key};

/// The key list's state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Help {
    /// The action the cursor is on, in [`Action::BINDABLE`].
    pub selected: usize,
    /// Waiting for the selected action's new key.
    pub capturing: bool,
    /// A change waiting for Enter; any other key drops it.
    pub confirm: Option<Change>,
}

/// A change to the selected action's keys, asked about before it is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The key becomes its only key, taken from another action if needed.
    Bind(Key),
    /// Back to its default keys.
    Reset,
}

impl Help {
    pub fn action(&self) -> Action {
        Action::BINDABLE[self.selected]
    }

    /// Moves the cursor `by` actions, stopping at the ends.
    pub fn select(&mut self, by: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(by)
            .min(Action::BINDABLE.len() - 1);
    }
}

/// Draws the key list, scrolled so the selected action shows.
pub fn render(f: &mut Frame, area: Rect, help: &Help, ctx: &RenderCtx) {
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
            if keys.is_empty() {
                keys = "–".to_string();
            }
            (keys, action.description())
        })
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
    let scroll = (help.selected % rows.max(1) + 1).saturating_sub(shown);
    let action = help.action().description();
    let hint = match (help.confirm, help.capturing) {
        (Some(Change::Bind(key)), _) => {
            let from = ctx
                .keymap
                .owner(key)
                .filter(|owner| *owner != help.action())
                .map_or(String::new(), |owner| {
                    format!(", from {}", owner.description())
                });
            format!(" {} for {action}{from}? Enter yes · Esc no ", key.label())
        }
        (Some(Change::Reset), _) => format!(" {action} back to default? Enter yes · Esc no "),
        (None, true) => format!(" key for {action}? Esc cancels "),
        (None, false) => " Enter change · Del default · Esc close ".to_string(),
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
    for (column, (area, chunk)) in areas.iter().zip(entries.chunks(rows)).enumerate() {
        let key_width = KEY_WIDTH;
        let lines: Vec<Line> = chunk
            .iter()
            .enumerate()
            .skip(scroll)
            .map(|(row, (keys, description))| {
                let selected = column * rows + row == help.selected;
                let (key_style, text_style) = if selected {
                    (theme.selection(), theme.selection())
                } else {
                    (theme.key(), theme.base().bg(theme.surface))
                };
                let pending;
                let keys = match (selected, help.confirm) {
                    (true, Some(Change::Bind(key))) => {
                        pending = key.label();
                        pending.as_str()
                    }
                    (true, None) if help.capturing => "…",
                    _ => keys,
                };
                Line::from(vec![
                    Span::styled(
                        format!("{:<key_width$}", truncate(keys, key_width - 1)),
                        key_style,
                    ),
                    Span::styled(*description, text_style),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(lines), *area);
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::{render, TestCtx};

    #[test]
    fn help_wide() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 120, 40, |f, area, ctx| super::render(
            f,
            area,
            &super::Help::default(),
            ctx
        )));
    }

    #[test]
    fn help_narrow() {
        let test = TestCtx::new();
        insta::assert_snapshot!(render(&test, 80, 24, |f, area, ctx| super::render(
            f,
            area,
            &super::Help::default(),
            ctx
        )));
    }

    #[test]
    fn help_scrolled_to_the_end() {
        let test = TestCtx::new();
        let help = super::Help {
            selected: crate::action::Action::BINDABLE.len() - 1,
            capturing: true,
            ..Default::default()
        };
        insta::assert_snapshot!(render(&test, 80, 24, |f, area, ctx| super::render(
            f, area, &help, ctx
        )));
    }
}
