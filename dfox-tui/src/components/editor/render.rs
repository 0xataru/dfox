//! Drawing the editor: line numbers, a marker on the statement that would
//! run, highlighted text and the selection.

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use unicode_width::UnicodeWidthChar;

use super::{EditorPanel, TabLabel};
use crate::{
    action::Action,
    component::RenderCtx,
    components::{inner, panel, scroll_to},
    sql::{self, Piece, Pos},
    widgets::throbber::spinner,
};

impl EditorPanel {
    /// Draws the editor with the query tabs in its title.
    pub fn render_with(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx, tabs: &[TabLabel]) {
        let theme = ctx.theme;
        let mut title = Vec::new();
        if tabs.len() <= 1 {
            title.push(Span::raw("Query"));
        }
        if tabs.len() > 1 {
            for (i, tab) in tabs.iter().enumerate() {
                if i > 0 {
                    title.push(Span::styled(" │ ", theme.dim()));
                }
                if tab.running {
                    title.push(spinner(ctx, theme.key()));
                    title.push(Span::raw(" "));
                }
                let style = if tab.active {
                    theme.title(self.focused)
                } else {
                    theme.dim()
                };
                title.push(Span::styled(tab.title.clone(), style));
            }
        }
        let (line, col) = self.buffer.cursor();
        let mut block = panel(title, self.focused, ctx);
        if self.focused {
            block = block.title_bottom(
                Line::from(Span::styled(
                    format!(" Ln {}, Col {} ", line + 1, col + 1),
                    theme.dim(),
                ))
                .right_aligned(),
            );
        }
        f.render_widget(block, area);

        let text_area = inner(area);
        if text_area.is_empty() {
            return;
        }
        self.refresh_cache();
        let cache = self.cache.borrow();
        let lines = self.buffer.lines();
        let number_width = lines.len().to_string().len() as u16;
        // A statement marker, the line number and a space.
        let gutter = (number_width + 2).min(text_area.width.saturating_sub(1));
        self.gutter.set(gutter);
        self.text_area.set(text_area);
        let width = text_area.width.saturating_sub(gutter) as usize;
        let height = text_area.height as usize;

        let (top, left) = self.scroll.get();
        let top = scroll_to(line, top, height);
        let left = horizontal_scroll(&lines[line], col, left, width);
        self.scroll.set((top, left));

        if lines.len() == 1 && lines[0].is_empty() && !self.focused {
            let hint = format!(
                "Write SQL here · {} runs the statement under the cursor",
                ctx.keymap.hint(Action::Execute)
            );
            f.render_widget(
                Paragraph::new(Span::styled(hint, theme.dim())),
                Rect {
                    x: text_area.x + gutter,
                    width: text_area.width - gutter,
                    ..text_area
                },
            );
        }

        let selection = self.buffer.selection();
        let rows: Vec<Line> = (top..lines.len().min(top + height))
            .map(|n| {
                let in_statement = cache
                    .statement
                    .is_some_and(|s| s.start.0 <= n && n <= s.end.0);
                let marker = if in_statement && self.focused {
                    Span::styled("▎", theme.key())
                } else {
                    Span::raw(" ")
                };
                let number_style = if n == line {
                    Style::default().fg(theme.text)
                } else {
                    theme.dim()
                };
                let mut spans = vec![
                    marker,
                    Span::styled(
                        format!("{:>w$} ", n + 1, w = number_width as usize),
                        number_style,
                    ),
                ];
                spans.extend(styled_line(
                    &lines[n],
                    n,
                    left,
                    &cache.highlight[n],
                    selection,
                    ctx,
                ));
                Line::from(spans)
            })
            .collect();
        f.render_widget(Paragraph::new(rows), text_area);

        if self.focused && self.search.is_none() {
            let x: usize = lines[line]
                .chars()
                .skip(left)
                .take(col.saturating_sub(left))
                .map(width_of)
                .sum();
            let x = text_area.x + gutter + x as u16;
            let y = text_area.y + (line - top) as u16;
            if x < text_area.right() && y < text_area.bottom() {
                f.set_cursor_position((x, y));
            }
        }
    }

    /// Draws the history search over `area`, when it is open.
    pub fn render_overlay(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        if let Some(search) = &self.search {
            search.render(f, area, ctx);
        }
    }
}

/// The first column to show so that the cursor stays in view.
/// Lines whose start fits with the cursor are never scrolled.
pub(super) fn horizontal_scroll(line: &str, col: usize, left: usize, width: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let before: usize = chars.iter().take(col).map(|c| width_of(*c)).sum();
    if before < width {
        return 0;
    }
    if col < left {
        return col;
    }
    let mut used = 0;
    let mut first = col;
    // Walk back from the cursor while the characters still fit.
    while first > left {
        let w = chars.get(first - 1).map_or(1, |c| width_of(*c));
        if used + w >= width {
            break;
        }
        used += w;
        first -= 1;
    }
    if first > left {
        first
    } else {
        left
    }
}

/// One line of text from column `left`, coloured, with the selection shown.
fn styled_line<'a>(
    text: &'a str,
    line: usize,
    left: usize,
    pieces: &[Piece],
    selection: Option<(Pos, Pos)>,
    ctx: &RenderCtx,
) -> Vec<Span<'a>> {
    let theme = ctx.theme;
    let base = theme.syntax(sql::Kind::Plain);
    let selected = |col: usize| {
        selection.is_some_and(|(start, end)| {
            let pos = (line, col);
            start <= pos && pos < end
        })
    };
    let style_at = |col: usize| {
        let style = pieces
            .iter()
            .find(|p| p.start <= col && col < p.end)
            .map_or(base, |p| theme.syntax(p.kind));
        if selected(col) {
            style.bg(theme.selection)
        } else {
            style
        }
    };

    let mut spans = Vec::new();
    let mut start_byte = None;
    let mut current = base;
    let mut indices = text.char_indices().skip(left).enumerate().peekable();
    while let Some((i, (byte, c))) = indices.next() {
        let style = if c.is_control() {
            theme.dim()
        } else {
            style_at(left + i)
        };
        if start_byte.is_none() {
            start_byte = Some(byte);
            current = style;
        } else if style != current {
            if let Some(from) = start_byte {
                spans.push(Span::styled(clean(&text[from..byte]), current));
            }
            start_byte = Some(byte);
            current = style;
        }
        if indices.peek().is_none() {
            if let Some(from) = start_byte {
                spans.push(Span::styled(clean(&text[from..]), current));
            }
        }
    }
    // A selected line break shows as one selected space.
    if selected(text.chars().count()) {
        spans.push(Span::styled(" ", base.bg(theme.selection)));
    }
    spans
}

/// Columns a character takes on screen. Control characters, such as tabs,
/// are drawn as one space.
pub(super) fn width_of(c: char) -> usize {
    if c.is_control() {
        1
    } else {
        c.width().unwrap_or(0)
    }
}

/// Tabs and other control characters take one column each.
pub(super) fn clean(text: &str) -> std::borrow::Cow<'_, str> {
    if text.chars().any(char::is_control) {
        text.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .into()
    } else {
        text.into()
    }
}
