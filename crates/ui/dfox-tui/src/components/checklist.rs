//! Options to tick, dropped down under the button that opened them.

use std::cell::Cell;

use ratatui::{
    layout::{Position, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::{action::Action, component::RenderCtx};

#[derive(Debug)]
pub struct Checklist {
    pub items: Vec<(&'static str, bool)>,
    selected: usize,
    /// The button it hangs from.
    anchor: Rect,
    drawn: Cell<Rect>,
}

impl Checklist {
    pub fn new(items: Vec<(&'static str, bool)>, anchor: Rect) -> Self {
        Self {
            items,
            selected: 0,
            anchor,
            drawn: Cell::new(Rect::default()),
        }
    }

    /// Takes a key. False when it closes.
    pub fn handle(&mut self, action: Action) -> bool {
        match action {
            Action::Up => self.selected = self.selected.saturating_sub(1),
            Action::Down => self.selected = (self.selected + 1).min(self.items.len() - 1),
            Action::Confirm | Action::Input(' ') => self.items[self.selected].1 ^= true,
            Action::Back => return false,
            _ => {}
        }
        true
    }

    /// A click ticks the option under it. False when it was outside.
    pub fn click(&mut self, point: Position) -> bool {
        let drawn = self.drawn.get();
        if !drawn.contains(point) {
            return false;
        }
        let row = (point.y - drawn.y) as usize;
        if (1..=self.items.len()).contains(&row) {
            self.selected = row - 1;
            self.items[self.selected].1 ^= true;
        }
        true
    }

    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let longest = self.items.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
        let width = (longest as u16 + 9).min(area.width);
        let height = (self.items.len() as u16 + 2).min(area.height);
        let x = self
            .anchor
            .right()
            .saturating_sub(width)
            .clamp(area.x, area.right().saturating_sub(width));
        let y = (self.anchor.y + 1).min(area.bottom().saturating_sub(height));
        let popup = Rect::new(x, y, width, height);
        self.drawn.set(popup);
        let lines: Vec<Line> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, (label, on))| {
                let marker = if i == self.selected {
                    ctx.icons.selected
                } else {
                    " "
                };
                let tick = if *on { "[x]" } else { "[ ]" };
                Line::from(vec![
                    Span::styled(format!("{marker} {tick} "), theme.key()),
                    Span::raw(*label),
                ])
            })
            .collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .style(theme.surface());
        f.render_widget(Clear, popup);
        f.render_widget(Paragraph::new(lines).block(block), popup);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_clicks_tick_and_close() {
        let mut list = Checklist::new(vec![("A", false), ("B", true)], Rect::new(10, 0, 3, 1));
        assert!(list.handle(Action::Down));
        assert!(list.handle(Action::Input(' ')));
        assert_eq!(list.items, [("A", false), ("B", false)]);
        list.drawn.set(Rect::new(0, 1, 13, 4));
        assert!(list.click(Position::new(3, 2)));
        assert_eq!(list.items, [("A", true), ("B", false)]);
        assert!(!list.click(Position::new(30, 2)));
        assert!(!list.handle(Action::Back));
    }
}
