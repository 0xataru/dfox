//! The SQL editor: highlighted text, a selection, undo, autocomplete, and a
//! search through the query history.

pub mod buffer;
pub mod complete;
mod history;
mod render;

use std::cell::{Cell, RefCell};

use ratatui::{
    layout::{Position, Rect},
    Frame,
};

use self::{
    buffer::{Buffer, Motion},
    complete::Completion,
    history::HistorySearch,
    render::width_of,
};
use crate::{
    action::Action,
    catalog::Catalog,
    command::Command,
    component::{Component, Ctx, RenderCtx},
    sql::{self, Dialect, Piece, Pos, Stmt},
};

/// A query tab, as its label in the editor's title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabLabel {
    /// The tab holds a definition, named for its object.
    pub named: bool,
    pub title: String,
    pub active: bool,
    pub running: bool,
}

/// What drawing works out from the text, kept until the text changes.
#[derive(Debug, Default)]
struct Cache {
    version: Option<u64>,
    highlight: Vec<Vec<Piece>>,
    statements: usize,
    /// The statement under `cursor`.
    cursor: Pos,
    statement: Option<Stmt>,
}

#[derive(Debug)]
pub struct EditorPanel {
    pub buffer: Buffer,
    dialect: Dialect,
    /// Holds mongo shell commands rather than SQL.
    pub shell: bool,
    pub focused: bool,
    /// First line and first column shown.
    scroll: Cell<(usize, usize)>,
    cache: RefCell<Cache>,
    /// Where the text was last drawn, for the mouse.
    text_area: Cell<Rect>,
    gutter: Cell<u16>,
    /// Where the cursor was drawn, for the autocomplete popup.
    screen_cursor: Cell<Option<(u16, u16)>>,
    search: Option<HistorySearch>,
    completion: Option<Completion>,
}

impl EditorPanel {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            buffer: Buffer::default(),
            dialect,
            shell: false,
            focused: false,
            scroll: Cell::new((0, 0)),
            cache: RefCell::new(Cache::default()),
            text_area: Cell::new(Rect::default()),
            gutter: Cell::new(0),
            screen_cursor: Cell::new(None),
            search: None,
            completion: None,
        }
    }

    /// The editor, for mongo shell commands when `shell`.
    pub fn with_shell(mut self, shell: bool) -> Self {
        self.shell = shell;
        self
    }

    /// What is written here, for the hint in an empty editor.
    pub fn holds(&self) -> &'static str {
        if self.shell {
            "commands"
        } else {
            "SQL"
        }
    }

    fn statement_at(&self, text: &str, cursor: sql::Pos) -> Option<sql::Stmt> {
        if self.shell {
            sql::mongo::statement_at(text, cursor)
        } else {
            sql::statement_at(text, cursor, self.dialect)
        }
    }

    pub fn text(&self) -> String {
        self.buffer.text()
    }

    #[cfg(test)]
    pub fn set_text(&mut self, text: &str) {
        self.buffer.set_text(text);
    }

    /// Replaces the text, with the cursor at the start.
    pub fn open(&mut self, text: &str) {
        self.buffer.set_text(text);
        self.buffer.set_cursor((0, 0), false);
    }

    /// What to run: the selection, or else the statement under the cursor.
    pub fn statement(&self) -> Option<String> {
        if let Some(selected) = self.buffer.selected_text() {
            return Some(selected).filter(|s| !s.trim().is_empty());
        }
        let text = self.buffer.text();
        let statement = self.statement_at(&text, self.buffer.cursor())?;
        Some(sql::slice(
            self.buffer.lines(),
            statement.start,
            statement.end,
        ))
    }

    /// True while the history search has the keys.
    pub fn is_modal(&self) -> bool {
        self.search.is_some()
    }

    /// What the autocomplete popup lists, best first.
    #[cfg(test)]
    pub fn completion_labels(&self) -> Vec<String> {
        self.completion
            .iter()
            .flat_map(|c| c.items.iter().map(|i| i.label.clone()))
            .collect()
    }

    /// True while the autocomplete popup is open.
    pub fn completing(&self) -> bool {
        self.completion.is_some()
    }

    /// Updates autocomplete after `action` changed the text: suggestions
    /// while typing a word or after a dot, or on request (`forced`).
    pub fn complete(&mut self, catalog: &Catalog, action: Action) {
        let refresh = match action {
            Action::Complete => true,
            Action::Input(c) => c.is_alphanumeric() || c == '_' || c == '.',
            Action::Backspace => self.completion.is_some(),
            _ => false,
        };
        self.completion = if refresh && self.buffer.selection().is_none() && self.shell {
            complete::suggest_shell(
                &self.buffer.text(),
                self.buffer.cursor(),
                catalog,
                action == Action::Complete,
            )
        } else if refresh && self.buffer.selection().is_none() {
            complete::suggest(
                &self.buffer.text(),
                self.buffer.cursor(),
                self.dialect,
                catalog,
                action == Action::Complete,
            )
        } else {
            None
        };
    }

    pub fn close_completion(&mut self) {
        self.completion = None;
    }

    /// Keys for the open popup. Returns false for keys it doesn't use,
    /// which close it.
    fn handle_completion(&mut self, action: Action) -> bool {
        let Some(completion) = &mut self.completion else {
            return false;
        };
        match action {
            Action::Up => completion.select(false),
            Action::Down => completion.select(true),
            Action::NextFocus | Action::Confirm => {
                if let Some(item) = completion.chosen() {
                    let (start, insert) = (completion.start, item.insert.clone());
                    let cursor = self.buffer.cursor();
                    self.buffer.set_cursor(start, false);
                    self.buffer.set_cursor(cursor, true);
                    self.buffer.insert_str(&insert);
                }
                self.completion = None;
            }
            Action::Back => self.completion = None,
            _ => {
                self.completion = None;
                return false;
            }
        }
        true
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(search) = &mut self.search {
            search
                .query
                .push_str(text.lines().next().unwrap_or_default());
        } else {
            self.buffer.insert_str(text);
        }
    }

    fn page(&self) -> usize {
        (self.text_area.get().height as usize).max(1)
    }

    /// Moves the cursor to a point on screen, extending the selection when
    /// dragging. Returns false when the point isn't on the text.
    pub fn click(&mut self, column: u16, row: u16, drag: bool) -> bool {
        let area = self.text_area.get();
        if !area.contains(Position::new(column, row)) && !drag {
            return false;
        }
        let (top, left) = self.scroll.get();
        let line = top + row.saturating_sub(area.y) as usize;
        let x = column.saturating_sub(area.x + self.gutter.get()) as usize;
        let Some(text) = self
            .buffer
            .lines()
            .get(line.min(self.buffer.lines().len() - 1))
        else {
            return false;
        };
        // Walk the characters to find the one under the pointer.
        let mut width = 0;
        let mut col = left;
        for c in text.chars().skip(left) {
            let w = width_of(c);
            if width + w > x {
                break;
            }
            width += w;
            col += 1;
        }
        self.buffer.set_cursor((line, col), drag);
        self.buffer.break_undo();
        true
    }

    /// Scrolls the view by `lines`, moving the cursor along.
    pub fn scroll_by(&mut self, lines: isize) {
        let motion = if lines < 0 {
            Motion::PageUp(lines.unsigned_abs())
        } else {
            Motion::PageDown(lines.unsigned_abs())
        };
        self.buffer.move_cursor(motion, false);
    }

    fn copy(&mut self, ctx: &mut Ctx, cut: bool) {
        let selected = if cut {
            self.buffer.cut()
        } else {
            self.buffer.selected_text()
        };
        match selected {
            Some(text) => ctx.copy(text, "the selection"),
            None => {
                let line = self.buffer.lines()[self.buffer.cursor().0].clone();
                ctx.copy(line, "the line");
            }
        }
    }

    fn refresh_cache(&self) {
        let mut cache = self.cache.borrow_mut();
        let version = self.buffer.version();
        let cursor = self.buffer.cursor();
        if cache.version != Some(version) {
            if self.shell {
                cache.highlight = sql::mongo::highlight(self.buffer.lines());
                cache.statements = sql::mongo::statements(&self.buffer.text()).len();
            } else {
                cache.highlight = sql::highlight(self.buffer.lines(), self.dialect);
                cache.statements = sql::statements(&self.buffer.text(), self.dialect).len();
            }
            cache.version = Some(version);
            cache.statement = None;
            cache.cursor = (usize::MAX, 0);
        }
        if cache.cursor != cursor {
            cache.statement = if cache.statements > 1 {
                self.statement_at(&self.buffer.text(), cursor)
            } else {
                None
            };
            cache.cursor = cursor;
        }
    }
}

impl Component for EditorPanel {
    fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        if let Some(search) = &mut self.search {
            if let Some(chosen) = search.handle(action, ctx) {
                if let Some(sql) = chosen {
                    self.buffer.set_text(&sql);
                }
                self.search = None;
            }
            return None;
        }
        if self.handle_completion(action) {
            return None;
        }
        let page = self.page();
        let b = &mut self.buffer;
        let motion = match action {
            Action::Left => Some((Motion::Left, false)),
            Action::Right => Some((Motion::Right, false)),
            Action::Up => Some((Motion::Up, false)),
            Action::Down => Some((Motion::Down, false)),
            Action::Home => Some((Motion::Home, false)),
            Action::End => Some((Motion::End, false)),
            Action::PageUp => Some((Motion::PageUp(page), false)),
            Action::PageDown => Some((Motion::PageDown(page), false)),
            Action::WordLeft => Some((Motion::WordLeft, false)),
            Action::WordRight => Some((Motion::WordRight, false)),
            Action::SelectLeft => Some((Motion::Left, true)),
            Action::SelectRight => Some((Motion::Right, true)),
            Action::SelectUp => Some((Motion::Up, true)),
            Action::SelectDown => Some((Motion::Down, true)),
            Action::SelectHome => Some((Motion::Home, true)),
            Action::SelectEnd => Some((Motion::End, true)),
            _ => None,
        };
        if let Some((motion, select)) = motion {
            b.move_cursor(motion, select);
            b.break_undo();
            return None;
        }
        match action {
            Action::Input(c) => b.insert_char(c),
            Action::Confirm => b.newline(),
            Action::Backspace => b.backspace(),
            Action::Delete => b.delete(),
            Action::DeleteWord => b.delete_word(),
            Action::Undo => {
                b.undo();
            }
            Action::Redo => {
                b.redo();
            }
            Action::SelectAll => b.select_all(),
            Action::Copy => self.copy(ctx, false),
            Action::Cut => self.copy(ctx, true),
            Action::Paste => match ctx.clipboard.get() {
                Some(text) => self.buffer.insert_str(&text),
                None => ctx.toast(
                    crate::widgets::toast::Level::Warning,
                    "Nothing to paste: the clipboard can't be read",
                ),
            },
            Action::History => self.search = Some(HistorySearch::open(ctx)),
            _ => {}
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        self.render_with(f, area, ctx, &[]);
    }

    fn wants_text(&self) -> bool {
        true
    }
}

#[cfg(test)]
use render::{clean, horizontal_scroll};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
