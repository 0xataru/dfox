//! The SQL editor: multi-line text with a cursor.

use std::cell::Cell;

use ratatui::{layout::Rect, text::Span, widgets::Paragraph, Frame};

use super::{inner, panel};
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, RenderCtx},
};

#[derive(Debug)]
pub struct EditorPanel {
    lines: Vec<String>,
    /// Cursor line, and column in characters.
    row: usize,
    col: usize,
    scroll: Cell<usize>,
    pub focused: bool,
}

impl Default for EditorPanel {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            scroll: Cell::new(0),
            focused: false,
        }
    }
}

impl EditorPanel {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    #[cfg(test)]
    pub fn set_text(&mut self, text: &str) {
        self.lines = text.split('\n').map(str::to_string).collect();
        self.row = self.lines.len() - 1;
        self.col = self.line_len();
    }

    fn line_len(&self) -> usize {
        self.lines[self.row].chars().count()
    }

    /// Byte offset of the cursor in its line.
    fn byte_col(&self) -> usize {
        let line = &self.lines[self.row];
        line.char_indices()
            .nth(self.col)
            .map_or(line.len(), |(i, _)| i)
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_col();
        self.lines[self.row].insert(at, c);
        self.col += 1;
    }

    fn newline(&mut self) {
        let at = self.byte_col();
        let rest = self.lines[self.row].split_off(at);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let at = self.byte_col();
            self.lines[self.row].remove(at);
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len();
            self.lines[self.row].push_str(&line);
        }
    }
}

impl Component for EditorPanel {
    fn handle(&mut self, action: Action, _ctx: &mut Ctx) -> Option<Command> {
        match action {
            Action::Input(c) => self.insert(c),
            Action::Confirm => self.newline(),
            Action::Backspace => self.backspace(),
            Action::Left if self.col > 0 => self.col -= 1,
            Action::Left if self.row > 0 => {
                self.row -= 1;
                self.col = self.line_len();
            }
            Action::Right if self.col < self.line_len() => self.col += 1,
            Action::Right if self.row + 1 < self.lines.len() => {
                self.row += 1;
                self.col = 0;
            }
            Action::Up if self.row > 0 => {
                self.row -= 1;
                self.col = self.col.min(self.line_len());
            }
            Action::Down if self.row + 1 < self.lines.len() => {
                self.row += 1;
                self.col = self.col.min(self.line_len());
            }
            Action::Home => self.col = 0,
            Action::End => self.col = self.line_len(),
            _ => {}
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let text_area = inner(area);
        let height = text_area.height as usize;
        let scroll = super::scroll_to(self.row, self.scroll.get(), height);
        self.scroll.set(scroll);

        let mut title = vec![Span::raw("Query")];
        if self.focused {
            title.push(Span::styled(
                format!(" {}:{}", self.row + 1, self.col + 1),
                theme.dim(),
            ));
        }
        let editor = Paragraph::new(self.text())
            .block(panel(title, self.focused, ctx))
            .scroll((scroll as u16, 0));
        f.render_widget(editor, area);

        if self.focused {
            let x = text_area.x + self.col as u16;
            let y = text_area.y + (self.row - scroll) as u16;
            if x < text_area.x + text_area.width && y < text_area.y + text_area.height {
                f.set_cursor_position((x, y));
            }
        }
    }

    fn wants_text(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, Harness, TestCtx};

    fn type_into(editor: &mut EditorPanel, actions: &[Action]) {
        let mut harness = Harness::new();
        for action in actions {
            editor.handle(*action, &mut harness.ctx());
        }
    }

    fn typed(text: &str) -> Vec<Action> {
        text.chars()
            .map(|c| {
                if c == '\n' {
                    Action::Confirm
                } else {
                    Action::Input(c)
                }
            })
            .collect()
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut editor = EditorPanel::default();
        type_into(&mut editor, &typed("SELECT 1\nFROM t"));
        type_into(&mut editor, &[Action::Up, Action::End, Action::Left]);
        type_into(&mut editor, &typed("0"));
        assert_eq!(editor.text(), "SELECT 01\nFROM t");
    }

    #[test]
    fn backspace_joins_lines_and_handles_wide_characters() {
        let mut editor = EditorPanel::default();
        type_into(&mut editor, &typed("é😀\nx"));
        type_into(
            &mut editor,
            &[Action::Home, Action::Backspace, Action::Backspace],
        );
        assert_eq!(editor.text(), "éx");
        assert_eq!((editor.row, editor.col), (0, 1));
    }

    #[test]
    fn enter_splits_the_line() {
        let mut editor = EditorPanel::default();
        editor.set_text("SELECT * FROM t");
        type_into(&mut editor, &[Action::Home]);
        for _ in 0..8 {
            type_into(&mut editor, &[Action::Right]);
        }
        type_into(&mut editor, &[Action::Confirm]);
        assert_eq!(editor.text(), "SELECT *\n FROM t");
    }

    #[test]
    fn editor_with_text() {
        let test = TestCtx::new();
        let mut editor = EditorPanel {
            focused: true,
            ..Default::default()
        };
        editor.set_text("SELECT id, name\nFROM customers\nWHERE id > 10");
        insta::assert_snapshot!(render(&test, 50, 6, |f, area, ctx| editor.render(f, area, ctx)));
    }
}
