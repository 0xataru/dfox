//! Screens and the panels inside them.

pub mod editor;
pub mod explorer;
pub mod form;
pub mod manager;
pub mod results;
pub mod workspace;

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Padding},
};

/// Space between a panel's border and its content.
const PADDING: Padding = Padding::horizontal(1);

use crate::component::RenderCtx;

/// A panel's frame: rounded, highlighted when focused, with a title.
pub fn panel<'a>(title: Vec<Span<'a>>, focused: bool, ctx: &RenderCtx) -> Block<'a> {
    let theme = ctx.theme;
    let mut spans = vec![Span::raw(" ")];
    spans.extend(title);
    spans.push(Span::raw(" "));
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border(focused))
        .title(Line::from(spans).style(theme.title(focused)))
        .padding(PADDING)
        .style(theme.base())
}

/// Keeps `selected` within `visible` rows of `scroll`, and returns the new
/// scroll offset.
pub fn scroll_to(selected: usize, scroll: usize, visible: usize) -> usize {
    let visible = visible.max(1);
    if selected < scroll {
        selected
    } else if selected >= scroll + visible {
        selected + 1 - visible
    } else {
        scroll
    }
}

/// The area inside a panel's border and padding.
pub fn inner(area: Rect) -> Rect {
    Block::default()
        .borders(Borders::ALL)
        .padding(PADDING)
        .inner(area)
}

#[cfg(test)]
mod tests {
    use super::scroll_to;

    #[test]
    fn scrolling_follows_the_selection() {
        assert_eq!(scroll_to(5, 0, 10), 0);
        assert_eq!(scroll_to(12, 0, 10), 3);
        assert_eq!(scroll_to(2, 5, 10), 2);
        assert_eq!(scroll_to(0, 0, 0), 0);
    }
}
