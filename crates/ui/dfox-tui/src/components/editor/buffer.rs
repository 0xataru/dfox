//! The text being edited: lines, a cursor, an optional selection, and undo
//! history. Knows nothing about drawing or keys.

use crate::sql::{self, Pos};

/// Undo steps kept per buffer.
const MAX_UNDO: usize = 200;

/// Brackets and quotes that close themselves.
pub(super) const PAIRS: [(char, char); 6] = [
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('\'', '\''),
    ('"', '"'),
    ('`', '`'),
];

fn closer(open: char) -> Option<char> {
    PAIRS.iter().find(|(o, _)| *o == open).map(|(_, c)| *c)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// The first non-blank character, or the line start when already there.
    Home,
    End,
    WordLeft,
    WordRight,
    PageUp(usize),
    PageDown(usize),
}

/// Consecutive edits of the same kind undo together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Typing,
    Deleting,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    lines: Vec<String>,
    cursor: Pos,
}

#[derive(Debug, Clone)]
pub struct Buffer {
    lines: Vec<String>,
    cursor: Pos,
    /// Where the selection started; the cursor is its other end.
    anchor: Option<Pos>,
    /// The column Up and Down aim for, kept across short lines.
    goal: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Option<EditKind>,
    /// Changes on every edit, so drawing can cache work per version.
    version: u64,
}

impl Default for Buffer {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: (0, 0),
            anchor: None,
            goal: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            version: 0,
        }
    }
}

fn char_len(line: &str) -> usize {
    line.chars().count()
}

/// Byte offset of character `col` in `line`.
fn byte_at(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
}

#[derive(PartialEq)]
enum Class {
    Space,
    Word,
    Other,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Other
    }
}

impl Buffer {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// Replaces the whole text, as one undoable step, with the cursor at the
    /// end.
    pub fn set_text(&mut self, text: &str) {
        self.checkpoint(EditKind::Other);
        self.lines = sql::lines(text);
        let last = self.lines.len() - 1;
        self.cursor = (last, char_len(&self.lines[last]));
        self.anchor = None;
        self.changed();
    }

    /// The selection, start first, when it isn't empty.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            return None;
        }
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection()
            .map(|(start, end)| sql::slice(&self.lines, start, end))
    }

    pub fn select_all(&mut self) {
        self.anchor = Some((0, 0));
        let last = self.lines.len() - 1;
        self.cursor = (last, char_len(&self.lines[last]));
    }

    /// Selects the word, number or run of spaces or punctuation under the
    /// cursor, or just before it at the line's end.
    pub fn select_word(&mut self) {
        let (line, col) = self.cursor;
        let chars: Vec<char> = self.lines[line].chars().collect();
        let Some(at) = (col < chars.len()).then_some(col).or(col.checked_sub(1)) else {
            return;
        };
        let kind = class(chars[at]);
        let start = (0..at)
            .rev()
            .take_while(|&i| class(chars[i]) == kind)
            .last();
        let end = (at..chars.len())
            .take_while(|&i| class(chars[i]) == kind)
            .last();
        self.anchor = Some((line, start.unwrap_or(at)));
        self.cursor = (line, end.unwrap_or(at) + 1);
        self.goal = None;
    }

    /// Selects the whole lines the selection or cursor is on, with the line
    /// break; again, the next line too.
    pub fn select_line(&mut self) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        self.anchor = Some((start.0, 0));
        let last = self.lines.len() - 1;
        self.cursor = if end.0 < last {
            (end.0 + 1, 0)
        } else {
            (last, char_len(&self.lines[last]))
        };
        self.goal = None;
    }

    /// Sets the cursor, clamped to the text, optionally extending the
    /// selection.
    pub fn set_cursor(&mut self, (line, col): Pos, select: bool) {
        let line = line.min(self.lines.len() - 1);
        let col = col.min(char_len(&self.lines[line]));
        self.place((line, col), select);
        self.goal = None;
    }

    fn place(&mut self, pos: Pos, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
    }

    fn line_len(&self, line: usize) -> usize {
        char_len(&self.lines[line])
    }

    pub fn move_cursor(&mut self, motion: Motion, select: bool) {
        // Without shift, Left and Right first collapse a selection.
        if !select {
            if let Some((start, end)) = self.selection() {
                match motion {
                    Motion::Left => return self.set_cursor(start, false),
                    Motion::Right => return self.set_cursor(end, false),
                    _ => {}
                }
            }
        }
        let (line, col) = self.cursor;
        let last = self.lines.len() - 1;
        let vertical = |this: &mut Self, target: usize| {
            let goal = *this.goal.get_or_insert(col);
            let pos = (target, goal.min(this.line_len(target)));
            this.place(pos, select);
        };
        match motion {
            Motion::Up if line > 0 => return vertical(self, line - 1),
            Motion::Down if line < last => return vertical(self, line + 1),
            Motion::PageUp(n) => return vertical(self, line.saturating_sub(n)),
            Motion::PageDown(n) => return vertical(self, (line + n).min(last)),
            _ => {}
        }
        let pos = match motion {
            Motion::Left if col > 0 => (line, col - 1),
            Motion::Left if line > 0 => (line - 1, self.line_len(line - 1)),
            Motion::Right if col < self.line_len(line) => (line, col + 1),
            Motion::Right if line < last => (line + 1, 0),
            Motion::Up => (line, 0),
            Motion::Down => (line, self.line_len(line)),
            Motion::Home => {
                let indent = self.lines[line]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .count();
                (line, if col == indent { 0 } else { indent })
            }
            Motion::End => (line, self.line_len(line)),
            Motion::WordLeft => self.word_left(),
            Motion::WordRight => self.word_right(),
            _ => self.cursor,
        };
        self.set_cursor(pos, select);
    }

    /// The start of the word before the cursor, crossing line starts.
    fn word_left(&self) -> Pos {
        let (line, mut col) = self.cursor;
        if col == 0 {
            return if line > 0 {
                (line - 1, self.line_len(line - 1))
            } else {
                (0, 0)
            };
        }
        let chars: Vec<char> = self.lines[line].chars().collect();
        while col > 0 && class(chars[col - 1]) == Class::Space {
            col -= 1;
        }
        if col > 0 {
            let kind = class(chars[col - 1]);
            while col > 0 && class(chars[col - 1]) == kind {
                col -= 1;
            }
        }
        (line, col)
    }

    /// The end of the word after the cursor, crossing line ends.
    fn word_right(&self) -> Pos {
        let (line, mut col) = self.cursor;
        let chars: Vec<char> = self.lines[line].chars().collect();
        if col == chars.len() {
            return if line + 1 < self.lines.len() {
                (line + 1, 0)
            } else {
                self.cursor
            };
        }
        while col < chars.len() && class(chars[col]) == Class::Space {
            col += 1;
        }
        if col < chars.len() {
            let kind = class(chars[col]);
            while col < chars.len() && class(chars[col]) == kind {
                col += 1;
            }
        }
        (line, col)
    }

    /// Saves the text for undo, unless this edit continues the last one.
    fn checkpoint(&mut self, kind: EditKind) {
        let continues = kind != EditKind::Other && self.last_edit == Some(kind);
        if !continues {
            self.undo.push(Snapshot {
                lines: self.lines.clone(),
                cursor: self.cursor,
            });
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = Some(kind);
    }

    fn changed(&mut self) {
        self.version += 1;
        self.goal = None;
    }

    /// Ends the current undo group, so the next edit starts a new one.
    pub fn break_undo(&mut self) {
        self.last_edit = None;
    }

    /// Removes the selection's text. Returns whether there was one. An
    /// empty selection is dropped too, so the edit that follows doesn't
    /// start a selection.
    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let tail = {
            let line = &self.lines[end.0];
            line[byte_at(line, end.1)..].to_string()
        };
        let head = &mut self.lines[start.0];
        head.truncate(byte_at(head, start.1));
        head.push_str(&tail);
        self.lines.drain(start.0 + 1..=end.0);
        self.cursor = start;
        self.anchor = None;
        true
    }

    pub fn insert_char(&mut self, c: char) {
        // A word boundary starts a new undo step.
        let kind = if c.is_whitespace() {
            EditKind::Other
        } else {
            EditKind::Typing
        };
        self.checkpoint(kind);
        self.delete_selection();
        let (line, col) = self.cursor;
        let at = byte_at(&self.lines[line], col);
        self.lines[line].insert(at, c);
        self.cursor.1 += 1;
        self.changed();
    }

    fn char_at(&self, (line, col): Pos) -> Option<char> {
        self.lines[line].chars().nth(col)
    }

    fn char_before(&self) -> Option<char> {
        let (line, col) = self.cursor;
        col.checked_sub(1).and_then(|col| self.char_at((line, col)))
    }

    /// A typed character: an opening bracket or quote brings its closing
    /// one, or wraps the selection, and a closing one already there is
    /// stepped over.
    pub fn type_char(&mut self, c: char) {
        let next = self.char_at(self.cursor);
        if let Some((start, end)) = self.selection() {
            if let Some(close) = closer(c) {
                self.checkpoint(EditKind::Other);
                let at = byte_at(&self.lines[end.0], end.1);
                self.lines[end.0].insert(at, close);
                let at = byte_at(&self.lines[start.0], start.1);
                self.lines[start.0].insert(at, c);
                self.shifted(|pos| (pos.0, pos.1 + usize::from(pos.0 == start.0)));
                return;
            }
        } else if next == Some(c) && PAIRS.iter().any(|(_, close)| *close == c) {
            self.cursor.1 += 1;
            self.break_undo();
            return;
        }
        let close = closer(c).filter(|close| {
            let free_after = next.is_none_or(|n| n.is_whitespace() || ")]},;".contains(n));
            let quote = *close == c;
            let free_before = !quote
                || self
                    .char_before()
                    .is_none_or(|p| !(p.is_alphanumeric() || p == '_' || p == c));
            free_after && free_before
        });
        self.insert_char(c);
        if let Some(close) = close {
            let (line, col) = self.cursor;
            let at = byte_at(&self.lines[line], col);
            self.lines[line].insert(at, close);
        }
    }

    /// Comments the lines of the selection, or the cursor's, out with
    /// `prefix`, or back in when every one of them is commented.
    pub fn toggle_comment(&mut self, prefix: &str) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        // A selection ending at a line's start doesn't take that line.
        let last = if end.0 > start.0 && end.1 == 0 {
            end.0 - 1
        } else {
            end.0
        };
        let filled: Vec<usize> = (start.0..=last)
            .filter(|n| !self.lines[*n].trim().is_empty())
            .collect();
        if filled.is_empty() {
            return;
        }
        self.checkpoint(EditKind::Other);
        let indent = |line: &str| char_len(line) - char_len(line.trim_start());
        let commented = filled
            .iter()
            .all(|n| self.lines[*n].trim_start().starts_with(prefix));
        let column = filled
            .iter()
            .map(|n| indent(&self.lines[*n]))
            .min()
            .unwrap_or(0);
        let mut moved = Vec::new();
        for n in filled {
            let line = &mut self.lines[n];
            if commented {
                let at = indent(line);
                let mut gone = char_len(prefix);
                if line.chars().nth(at + gone) == Some(' ') {
                    gone += 1;
                }
                let from = byte_at(line, at);
                line.replace_range(from..byte_at(line, at + gone), "");
                moved.push((n, at, -(gone as isize)));
            } else {
                line.insert_str(byte_at(line, column), &format!("{prefix} "));
                moved.push((n, column, char_len(prefix) as isize + 1));
            }
        }
        let shift = |(line, col): Pos| match moved.iter().find(|(n, ..)| *n == line) {
            Some((_, at, by)) if col >= *at => (line, col.saturating_add_signed(*by).max(*at)),
            _ => (line, col),
        };
        self.shifted(shift);
    }

    /// After an edit that moved text along lines: the cursor and the
    /// selection follow it, in an undo step of its own.
    fn shifted(&mut self, shift: impl Fn(Pos) -> Pos) {
        self.anchor = self.anchor.map(&shift);
        self.cursor = shift(self.cursor);
        self.changed();
        self.break_undo();
    }

    /// Inserts pasted text, replacing the selection.
    pub fn insert_str(&mut self, text: &str) {
        self.checkpoint(EditKind::Other);
        self.delete_selection();
        let mut pasted = sql::lines(&text.replace("\r\n", "\n").replace('\r', "\n"));
        let (line, col) = self.cursor;
        let at = byte_at(&self.lines[line], col);
        let tail = self.lines[line].split_off(at);
        let first = pasted.remove(0);
        self.lines[line].push_str(&first);
        let end = if pasted.is_empty() {
            (line, col + char_len(&first))
        } else {
            let count = pasted.len();
            let last_len = char_len(&pasted[count - 1]);
            for (i, text) in pasted.into_iter().enumerate() {
                self.lines.insert(line + 1 + i, text);
            }
            (line + count, last_len)
        };
        self.lines[end.0].push_str(&tail);
        self.cursor = end;
        self.changed();
        self.break_undo();
    }

    /// Splits the line, keeping the indentation of the current one.
    pub fn newline(&mut self) {
        self.checkpoint(EditKind::Other);
        self.delete_selection();
        let (line, col) = self.cursor;
        let indent: String = self.lines[line]
            .chars()
            .take(col)
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let at = byte_at(&self.lines[line], col);
        let rest = self.lines[line].split_off(at);
        let indent_len = char_len(&indent);
        self.lines.insert(line + 1, indent + &rest);
        self.cursor = (line + 1, indent_len);
        self.changed();
    }

    pub fn backspace(&mut self) {
        self.checkpoint(EditKind::Deleting);
        if !self.delete_selection() {
            let (line, col) = self.cursor;
            if col > 0 {
                // An empty pair goes as one.
                let next = self.char_at(self.cursor);
                if next.is_some() && self.char_before().and_then(closer) == next {
                    let at = byte_at(&self.lines[line], col);
                    self.lines[line].remove(at);
                }
                let at = byte_at(&self.lines[line], col - 1);
                self.lines[line].remove(at);
                self.cursor.1 -= 1;
            } else if line > 0 {
                let text = self.lines.remove(line);
                let len = self.line_len(line - 1);
                self.lines[line - 1].push_str(&text);
                self.cursor = (line - 1, len);
            }
        }
        self.changed();
    }

    pub fn delete(&mut self) {
        self.checkpoint(EditKind::Deleting);
        if !self.delete_selection() {
            let (line, col) = self.cursor;
            if col < self.line_len(line) {
                let at = byte_at(&self.lines[line], col);
                self.lines[line].remove(at);
            } else if line + 1 < self.lines.len() {
                let next = self.lines.remove(line + 1);
                self.lines[line].push_str(&next);
            }
        }
        self.changed();
    }

    /// Deletes back to the start of the word.
    pub fn delete_word(&mut self) {
        if self.selection().is_none() {
            let start = self.word_left();
            self.anchor = Some(self.cursor);
            self.cursor = start;
        }
        self.checkpoint(EditKind::Other);
        self.delete_selection();
        self.changed();
    }

    /// Removes and returns the selected text.
    pub fn cut(&mut self) -> Option<String> {
        let text = self.selected_text()?;
        self.checkpoint(EditKind::Other);
        self.delete_selection();
        self.changed();
        Some(text)
    }

    /// Removes the cursor's line and returns it with a line break, or None
    /// when the text is a single empty line.
    pub fn cut_line(&mut self) -> Option<String> {
        let (line, _) = self.cursor;
        let last = self.lines.len() - 1;
        if last == 0 && self.lines[0].is_empty() {
            return None;
        }
        let text = format!("{}\n", self.lines[line]);
        // The last line goes with the break before it.
        let (start, end) = if line < last {
            ((line, 0), (line + 1, 0))
        } else if line > 0 {
            (
                (line - 1, self.line_len(line - 1)),
                (line, self.line_len(line)),
            )
        } else {
            ((0, 0), (0, self.line_len(0)))
        };
        self.anchor = Some(start);
        self.cursor = end;
        self.cut();
        Some(text)
    }

    pub fn undo(&mut self) -> bool {
        self.swap_history(true)
    }

    pub fn redo(&mut self) -> bool {
        self.swap_history(false)
    }

    fn swap_history(&mut self, undo: bool) -> bool {
        let (from, to) = if undo {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        let Some(snapshot) = from.pop() else {
            return false;
        };
        to.push(Snapshot {
            lines: std::mem::replace(&mut self.lines, snapshot.lines),
            cursor: self.cursor,
        });
        self.cursor = snapshot.cursor;
        self.anchor = None;
        self.last_edit = None;
        self.changed();
        true
    }

    /// The character under the cursor, for tests.
    #[cfg(test)]
    pub fn current_char(&self) -> Option<char> {
        self.lines[self.cursor.0].chars().nth(self.cursor.1)
    }
}

#[cfg(test)]
#[path = "buffer_tests.rs"]
mod tests;
