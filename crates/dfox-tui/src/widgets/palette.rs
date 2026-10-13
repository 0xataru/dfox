//! The command palette: every command, found by typing part of its name.

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use super::truncate;
use crate::{action::Action, component::RenderCtx};

/// Rows of matches shown.
const SHOWN: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
    matches: Vec<Action>,
}

impl Default for Palette {
    fn default() -> Self {
        let mut palette = Self {
            query: String::new(),
            selected: 0,
            matches: Vec::new(),
        };
        palette.refresh();
        palette
    }
}

/// Commands worth finding by name: not moving the cursor or typing.
fn commands() -> impl Iterator<Item = Action> {
    Action::BINDABLE.iter().copied().filter(|a| {
        !matches!(
            a,
            Action::Up
                | Action::Down
                | Action::Left
                | Action::Right
                | Action::PageUp
                | Action::PageDown
                | Action::Home
                | Action::End
                | Action::Confirm
                | Action::Back
                | Action::Backspace
                | Action::DeleteWord
                | Action::WordLeft
                | Action::WordRight
                | Action::SelectLeft
                | Action::SelectRight
                | Action::SelectUp
                | Action::SelectDown
                | Action::SelectHome
                | Action::SelectEnd
                | Action::Palette
        )
    })
}

impl Palette {
    fn refresh(&mut self) {
        let all: Vec<Action> = commands().collect();
        self.selected = 0;
        if self.query.trim().is_empty() {
            self.matches = all;
            return;
        }
        let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize)> = all
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                // The config name helps too: `next_tab` finds "Next query tab".
                let text = format!("{} {}", a.description(), a.name());
                pattern
                    .score(Utf32Str::new(&text, &mut buf), &mut matcher)
                    .map(|score| (score, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.matches = scored.into_iter().map(|(_, i)| all[i]).collect();
    }

    pub fn input(&mut self, c: char) {
        self.query.push(c);
        self.refresh();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.refresh();
    }

    pub fn select(&mut self, down: bool) {
        let last = self.matches.len().saturating_sub(1);
        self.selected = if down {
            (self.selected + 1).min(last)
        } else {
            self.selected.saturating_sub(1)
        };
    }

    pub fn chosen(&self) -> Option<Action> {
        self.matches.get(self.selected).copied()
    }

    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let width = 60.min(area.width);
        let rows = self.matches.len().clamp(1, SHOWN);
        let height = (rows as u16 + 4).min(area.height);
        // Near the top, like most palettes.
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + area.height.saturating_sub(height).min(3),
            width,
            height,
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(" Commands ", theme.title(true)))
            .style(theme.surface());
        let inner = block.inner(popup);
        f.render_widget(Clear, popup);
        f.render_widget(block, popup);
        if inner.height < 2 {
            return;
        }
        let prompt = Line::from(vec![
            Span::styled(" ❯ ", theme.key()),
            Span::raw(self.query.as_str()),
        ]);
        f.render_widget(Paragraph::new(prompt), Rect { height: 1, ..inner });
        f.set_cursor_position((inner.x + 3 + self.query.chars().count() as u16, inner.y));

        let list = Rect {
            y: inner.y + 2,
            height: inner.height.saturating_sub(2),
            ..inner
        };
        if self.matches.is_empty() {
            f.render_widget(
                Paragraph::new(Span::styled("   No matching command", theme.dim())),
                list,
            );
            return;
        }
        let first = self.selected.saturating_sub(SHOWN - 1);
        let key_width = 14;
        let text_width = (list.width as usize).saturating_sub(key_width + 3);
        let lines: Vec<Line> = self
            .matches
            .iter()
            .enumerate()
            .skip(first)
            .take(list.height as usize)
            .map(|(i, action)| {
                let style = if i == self.selected {
                    theme.highlight()
                } else {
                    theme.base().bg(theme.surface)
                };
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        format!(
                            "{:<text_width$}",
                            truncate(action.description(), text_width)
                        ),
                        style,
                    ),
                    Span::styled(
                        format!(
                            " {:>key_width$}",
                            truncate(&ctx.keymap.hint(*action), key_width)
                        ),
                        theme.key(),
                    ),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(lines), list);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, TestCtx};

    #[test]
    fn typing_narrows_the_commands() {
        let mut palette = Palette::default();
        assert!(palette.matches.len() > 20);
        assert!(!palette.matches.contains(&Action::Up));
        for c in "theme".chars() {
            palette.input(c);
        }
        assert_eq!(palette.chosen(), Some(Action::NextTheme));
        palette.backspace();
        palette.query.clear();
        palette.input('z');
        palette.input('q');
        palette.input('x');
        assert_eq!(palette.chosen(), None);
    }

    #[test]
    fn config_names_match_too() {
        let mut palette = Palette::default();
        for c in "copy_as".chars() {
            palette.input(c);
        }
        assert_eq!(palette.chosen(), Some(Action::CopyAs));
    }

    #[test]
    fn selection_stays_in_range() {
        let mut palette = Palette::default();
        palette.select(false);
        assert_eq!(palette.selected, 0);
        for _ in 0..100 {
            palette.select(true);
        }
        assert_eq!(palette.selected, palette.matches.len() - 1);
    }

    #[test]
    fn palette() {
        let test = TestCtx::new();
        let mut palette = Palette::default();
        palette.input('r');
        palette.input('u');
        palette.input('n');
        insta::assert_snapshot!(render(&test, 80, 16, |f, area, ctx| palette.render(f, area, ctx)));
        let empty = Palette {
            query: "zzz".into(),
            selected: 0,
            matches: Vec::new(),
        };
        insta::assert_snapshot!(
            "palette_no_match",
            render(&test, 80, 10, |f, area, ctx| empty.render(f, area, ctx))
        );
    }
}
