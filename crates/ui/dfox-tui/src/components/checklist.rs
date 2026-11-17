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
    pub items: Vec<(String, bool)>,
    /// The first `radio` items are one choice: ticking one unticks the rest.
    radio: usize,
    selected: usize,
    /// The first item on screen, when they don't all fit.
    top: Cell<usize>,
    /// The button it hangs from.
    anchor: Rect,
    drawn: Cell<Rect>,
}

impl Checklist {
    pub fn new(items: Vec<(String, bool)>, anchor: Rect) -> Self {
        Self {
            items,
            radio: 0,
            selected: 0,
            top: Cell::new(0),
            anchor,
            drawn: Cell::new(Rect::default()),
        }
    }

    /// The first `count` items are one choice.
    pub fn radio(mut self, count: usize) -> Self {
        self.radio = count.min(self.items.len());
        self
    }

    fn tick(&mut self, at: usize) {
        if at < self.radio {
            for (i, item) in self.items[..self.radio].iter_mut().enumerate() {
                item.1 = i == at;
            }
        } else {
            self.items[at].1 ^= true;
        }
    }

    /// Takes a key. False when it closes.
    pub fn handle(&mut self, action: Action) -> bool {
        match action {
            Action::Up => self.selected = self.selected.saturating_sub(1),
            Action::Down => self.selected = (self.selected + 1).min(self.items.len() - 1),
            Action::Confirm | Action::Input(' ') => self.tick(self.selected),
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
        let at = (row + self.top.get()).wrapping_sub(1);
        if row >= 1 && row + 1 < drawn.height as usize && at < self.items.len() {
            self.selected = at;
            self.tick(self.selected);
        }
        true
    }

    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let longest = self
            .items
            .iter()
            .map(|(l, _)| l.chars().count())
            .max()
            .unwrap_or(0);
        let width = (longest as u16 + 9).min(area.width);
        let height = (self.items.len() as u16 + 2).min(area.height);
        let shown = (height as usize).saturating_sub(2).max(1);
        let top = self
            .top
            .get()
            .min(self.selected)
            .max((self.selected + 1).saturating_sub(shown));
        self.top.set(top);
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
            .skip(top)
            .take(shown)
            .map(|(i, (label, on))| {
                let marker = if i == self.selected {
                    ctx.icons.selected
                } else {
                    " "
                };
                let tick = match (i < self.radio, *on) {
                    (true, true) => "(•)",
                    (true, false) => "( )",
                    (false, true) => "[x]",
                    (false, false) => "[ ]",
                };
                Line::from(vec![
                    Span::styled(format!("{marker} {tick} "), theme.key()),
                    Span::raw(label.as_str()),
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
        let item = |l: &str, on| (l.to_string(), on);
        let mut list = Checklist::new(
            vec![item("A", false), item("B", true)],
            Rect::new(10, 0, 3, 1),
        );
        assert!(list.handle(Action::Down));
        assert!(list.handle(Action::Input(' ')));
        assert_eq!(list.items, [item("A", false), item("B", false)]);
        list.drawn.set(Rect::new(0, 1, 13, 4));
        assert!(list.click(Position::new(3, 2)));
        assert_eq!(list.items, [item("A", true), item("B", false)]);
        assert!(!list.click(Position::new(30, 2)));
        assert!(!list.handle(Action::Back));

        let mut one = Checklist::new(
            vec![item("X", true), item("Y", false), item("Z", false)],
            Rect::default(),
        )
        .radio(2);
        one.handle(Action::Down);
        one.handle(Action::Confirm);
        one.handle(Action::Confirm);
        assert_eq!(
            one.items,
            [item("X", false), item("Y", true), item("Z", false)]
        );
    }
}
