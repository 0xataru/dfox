//! Small drawing pieces shared by the components.

pub mod confirm;
pub mod help;
pub mod palette;
pub mod progress;
pub mod prompt;
pub mod splash;
pub mod status_bar;
pub mod throbber;
pub mod toast;
pub mod which_key;

use ratatui::layout::{Constraint, Flex, Layout, Rect};

/// A `width`×`height` rectangle in the middle of `area`, clipped to it.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [rect] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    rect
}

/// Cuts `text` to `max` characters, ending in `…` when it was longer.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_rects_fit() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(centered(area, 40, 10), Rect::new(20, 7, 40, 10));
        assert_eq!(centered(area, 100, 30), area);
    }

    #[test]
    fn truncation_marks_the_cut() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
    }
}
