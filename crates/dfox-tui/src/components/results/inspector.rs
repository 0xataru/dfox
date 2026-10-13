//! The record inspector: every field of the current row in full, JSON
//! pretty-printed and binary as a hex dump.

use dfox_core::result::{Cell as Value, ResultSet};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthChar;

use crate::component::RenderCtx;

/// Lines of a single value shown before it is cut short.
const MAX_VALUE_LINES: usize = 2000;

/// The lines for `row`, wrapped to `width` columns.
pub fn lines<'a>(
    result: &'a impl ResultSet,
    row: usize,
    width: usize,
    ctx: &RenderCtx,
) -> Vec<Line<'a>> {
    let theme = ctx.theme;
    let width = width.max(8);
    let mut out = Vec::new();
    for (col, meta) in result.columns().iter().enumerate() {
        if col > 0 {
            out.push(Line::raw(""));
        }
        let mut header = vec![Span::styled(meta.name.as_str(), theme.header())];
        if !meta.type_name.is_empty() {
            header.push(Span::styled(format!("  {}", meta.type_name), theme.dim()));
        }
        out.push(Line::from(header));
        let value = result.cell(row, col).unwrap_or(Value::Null);
        let style = theme.cell(&value);
        let text = match value {
            Value::Null => vec![ctx.icons.null.to_string()],
            Value::Bytes(bytes) => hex_dump(bytes, width - 2),
            Value::Json(json) => pretty_json(json),
            Value::Text(text) => text.lines().map(str::to_string).collect(),
            other => vec![other.to_string()],
        };
        let mut shown = 0;
        for line in text {
            for piece in wrap(&line, width - 2) {
                if shown == MAX_VALUE_LINES {
                    out.push(Line::styled("  …", theme.dim()));
                    break;
                }
                out.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(piece, style),
                ]));
                shown += 1;
            }
        }
    }
    out
}

fn pretty_json(json: &str) -> Vec<String> {
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(value) => serde_json::to_string_pretty(&value)
            .unwrap_or_else(|_| json.to_string())
            .lines()
            .map(str::to_string)
            .collect(),
        Err(_) => json.lines().map(str::to_string).collect(),
    }
}

/// `00000000  de ad be ef …  |....|`, with 16, 8 or 4 bytes a line,
/// whichever fits in `width`.
fn hex_dump(bytes: &[u8], width: usize) -> Vec<String> {
    if bytes.is_empty() {
        return vec!["(empty)".to_string()];
    }
    // Offset, gaps and bars take 14 columns; each byte 4.
    let per_line = [16, 8, 4]
        .into_iter()
        .find(|n| 14 + 4 * n <= width)
        .unwrap_or(4);
    bytes
        .chunks(per_line)
        .enumerate()
        .map(|(i, chunk)| {
            let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
            let ascii: String = chunk
                .iter()
                .map(|&b| {
                    if b.is_ascii_graphic() || b == b' ' {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();
            format!(
                "{:08x}  {:<w$}  |{ascii}|",
                i * per_line,
                hex.join(" "),
                w = per_line * 3 - 1
            )
        })
        .collect()
}

/// Splits `line` into pieces of at most `width` columns. Tabs and other
/// control characters show as spaces.
fn wrap(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut pieces = vec![String::new()];
    let mut used = 0;
    for c in line.chars() {
        let c = if c.is_control() { ' ' } else { c };
        let w = c.width().unwrap_or(0);
        if used + w > width {
            pieces.push(String::new());
            used = 0;
        }
        if let Some(last) = pieces.last_mut() {
            last.push(c);
        }
        used += w;
    }
    pieces
}

/// Draws the inspector for `row` in `area`, scrolled down `scroll` lines.
/// Returns the scroll actually used, which stops at the end.
pub fn render(
    f: &mut Frame,
    area: Rect,
    result: &impl ResultSet,
    row: usize,
    scroll: usize,
    ctx: &RenderCtx,
) -> usize {
    let theme = ctx.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border(true))
        .title(Line::from(vec![
            Span::styled(" Row ", theme.title(true)),
            Span::styled(format!("{} ", row + 1), theme.dim()),
        ]))
        .title_bottom(Line::from(Span::styled(" i / esc close ", theme.dim())).right_aligned())
        .style(theme.surface());
    let inner = block.inner(area).inner(ratatui::layout::Margin::new(1, 0));
    let lines = lines(result, row, inner.width as usize, ctx);
    let scroll = scroll.min(lines.len().saturating_sub(inner.height as usize));
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), inner);
    scroll
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_dumps_show_offsets_and_ascii() {
        assert_eq!(
            hex_dump(b"hello, world!\x00\x01\x02xyz", 80),
            [
                "00000000  68 65 6c 6c 6f 2c 20 77 6f 72 6c 64 21 00 01 02  |hello, world!...|",
                "00000010  78 79 7a                                         |xyz|",
            ]
        );
        assert_eq!(hex_dump(&[], 80), ["(empty)"]);
        assert_eq!(
            hex_dump(b"0123456789", 50),
            [
                "00000000  30 31 32 33 34 35 36 37  |01234567|",
                "00000008  38 39                    |89|"
            ]
        );
    }

    #[test]
    fn json_is_pretty_printed_and_bad_json_kept() {
        assert_eq!(
            pretty_json(r#"{"a":[1,2]}"#),
            ["{", "  \"a\": [", "    1,", "    2", "  ]", "}"]
        );
        assert_eq!(pretty_json("{nope"), ["{nope"]);
    }

    #[test]
    fn long_lines_wrap_by_display_width() {
        assert_eq!(wrap("abcdef", 4), ["abcd", "ef"]);
        assert_eq!(wrap("界界界", 4), ["界界", "界"]);
        assert_eq!(wrap("a\tb", 10), ["a b"]);
        assert_eq!(wrap("", 4), [""]);
    }
}
