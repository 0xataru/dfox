//! The SQL editor: highlighted text, a selection, undo, autocomplete, and a
//! search through the query history.

pub mod buffer;
pub mod complete;
mod history;
mod render;

use std::{
    cell::{Cell, RefCell},
    time::{Duration, Instant},
};

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
use dfox_core::session::{QueryLanguage, SqlDialect};

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
    /// The bracket at the cursor and its match.
    brackets: Option<[Pos; 2]>,
}

/// Lines looked through for a bracket's match.
const MATCH_REACH: usize = 500;

/// The bracket at or just before `cursor` and the one matching it, out of
/// strings and comments.
fn matching(lines: &[String], highlight: &[Vec<Piece>], (line, col): Pos) -> Option<[Pos; 2]> {
    let code = |(line, col): Pos| {
        !highlight.get(line).is_some_and(|pieces| {
            pieces.iter().any(|p| {
                p.start <= col
                    && col < p.end
                    && matches!(p.kind, sql::Kind::String | sql::Kind::Comment)
            })
        })
    };
    let at = |pos: Pos| lines[pos.0].chars().nth(pos.1);
    let bracket = |c: char| {
        [('(', ')'), ('[', ']'), ('{', '}')]
            .into_iter()
            .find(|(o, c2)| *o == c || *c2 == c)
    };
    let from = [Some(col), col.checked_sub(1)]
        .into_iter()
        .flatten()
        .map(|col| (line, col))
        .find(|pos| at(*pos).and_then(bracket).is_some() && code(*pos))?;
    let c = at(from)?;
    let (open, close) = bracket(c)?;
    let rows = from.0.saturating_sub(MATCH_REACH)..lines.len().min(from.0 + MATCH_REACH);
    let chars: Vec<(Pos, char)> = rows
        .flat_map(|row| {
            lines[row]
                .chars()
                .enumerate()
                .map(move |(col, ch)| ((row, col), ch))
        })
        .filter(|(pos, ch)| (*ch == open || *ch == close) && code(*pos))
        .collect();
    let i = chars.iter().position(|(pos, _)| *pos == from)?;
    let mut depth = 0usize;
    let mut step = |&(pos, ch): &(Pos, char)| {
        if ch == c {
            depth += 1;
            return None;
        }
        depth -= 1;
        (depth == 0).then_some([from, pos])
    };
    if c == open {
        chars[i..].iter().find_map(&mut step)
    } else {
        chars[..=i].iter().rev().find_map(&mut step)
    }
}

#[derive(Debug)]
pub struct EditorPanel {
    pub buffer: Buffer,
    dialect: Dialect,
    /// What it holds: SQL in `dialect`, or a database's own commands.
    pub language: QueryLanguage,
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
    /// The function call the cursor is in: its name, arguments, and the
    /// argument being typed.
    signature: Option<Signature>,
    /// The suggestion just picked, for the catalog to count.
    picked: Option<String>,
    /// The last click and how many came quickly on the same spot.
    clicks: Option<(Instant, Pos, u8)>,
}

/// A call's arguments, shown while typing them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub name: String,
    pub parts: Vec<&'static str>,
    /// The part being typed.
    pub current: usize,
    /// `name(a, b)` as a call, or `NAME a b` as a Redis command.
    pub call: bool,
}

impl Signature {
    fn call(name: String, args: &'static str, current: usize) -> Self {
        let parts = if args.is_empty() {
            Vec::new()
        } else {
            args.split(", ").collect()
        };
        Self {
            name,
            parts,
            current,
            call: true,
        }
    }
}

/// Clicks closer than this count as a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(500);

impl EditorPanel {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            buffer: Buffer::default(),
            dialect,
            language: QueryLanguage::Sql(SqlDialect::Generic),
            focused: false,
            scroll: Cell::new((0, 0)),
            cache: RefCell::new(Cache::default()),
            text_area: Cell::new(Rect::default()),
            gutter: Cell::new(0),
            screen_cursor: Cell::new(None),
            search: None,
            completion: None,
            signature: None,
            picked: None,
            clicks: None,
        }
    }

    /// The editor, for `language`.
    pub fn with_language(mut self, language: QueryLanguage) -> Self {
        self.language = language;
        self
    }

    /// What is written here, for the hint in an empty editor.
    pub fn holds(&self) -> &'static str {
        match self.language {
            QueryLanguage::MongoShell | QueryLanguage::RedisCmd => "commands",
            _ => "SQL",
        }
    }

    fn statement_at(&self, text: &str, cursor: sql::Pos) -> Option<sql::Stmt> {
        match self.language {
            QueryLanguage::MongoShell => sql::mongo::statement_at(text, cursor),
            QueryLanguage::RedisCmd => sql::redis::statement_at(text, cursor),
            _ => sql::statement_at(text, cursor, self.dialect),
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
            Action::Input(c) => c.is_alphanumeric() || c == '_' || c == '.' || c == ':',
            Action::Backspace => self.completion.is_some(),
            _ => false,
        };
        let refresh = refresh && self.buffer.selection().is_none();
        self.signature = match self.language {
            QueryLanguage::Sql(_) => {
                let ctx =
                    sql::complete::context(&self.buffer.text(), self.buffer.cursor(), self.dialect);
                ctx.call.and_then(|(name, arg)| {
                    let args = sql::builtins::arguments(&name, self.dialect)?;
                    Some(Signature::call(name, args, arg))
                })
            }
            QueryLanguage::MongoShell => {
                sql::mongo::call(&self.buffer.text(), self.buffer.cursor()).and_then(
                    |(method, arg)| {
                        let (_, args) = sql::mongo::METHODS.iter().find(|(m, _)| *m == method)?;
                        Some(Signature::call(method, args, arg))
                    },
                )
            }
            QueryLanguage::RedisCmd => {
                sql::redis::argument_at(&self.buffer.text(), self.buffer.cursor()).and_then(|arg| {
                    let syntax = sql::redis::Syntax::of(&arg.command)?;
                    Some(Signature {
                        name: syntax.command.to_string(),
                        parts: syntax.parts(),
                        current: arg.index,
                        call: false,
                    })
                })
            }
            _ => None,
        };
        self.completion = if refresh && self.language == QueryLanguage::RedisCmd {
            complete::suggest_redis(
                &self.buffer.text(),
                self.buffer.cursor(),
                catalog,
                action == Action::Complete,
            )
        } else if refresh && self.language == QueryLanguage::MongoShell {
            complete::suggest_shell(
                &self.buffer.text(),
                self.buffer.cursor(),
                catalog,
                action == Action::Complete,
            )
        } else if refresh {
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

    /// The Redis key being typed, when the cursor is on a command's key.
    pub fn typed_key(&self) -> Option<String> {
        let arg = sql::redis::argument_at(&self.buffer.text(), self.buffer.cursor())?;
        let syntax = sql::redis::Syntax::of(&arg.command)?;
        syntax.takes_key(arg.index).then_some(arg.prefix)
    }

    /// The suggestion picked since the last call.
    pub fn take_picked(&mut self) -> Option<String> {
        self.picked.take()
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
                    self.picked = Some(item.label.clone());
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
    /// dragging or Shift-clicking. A double click selects the word, a triple
    /// click the line. Returns false when the point isn't on the text.
    pub fn click(&mut self, column: u16, row: u16, drag: bool) -> bool {
        let area = self.text_area.get();
        if !area.contains(Position::new(column, row)) && !drag {
            return false;
        }
        let (top, left) = self.scroll.get();
        // A drag above the text scrolls up; below it, drawing follows the cursor.
        let line = if row < area.y {
            top.saturating_sub((area.y - row) as usize)
        } else {
            top + (row - area.y) as usize
        };
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
        self.signature = None;
        if drag {
            return true;
        }
        let now = Instant::now();
        let pos = self.buffer.cursor();
        let count = match self.clicks {
            Some((at, last, n)) if last == pos && now - at < MULTI_CLICK => n % 3 + 1,
            _ => 1,
        };
        self.clicks = Some((now, pos, count));
        match count {
            2 => self.buffer.select_word(),
            3 => self.buffer.select_line(),
            _ => {}
        }
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

    /// Copies or cuts the selection, or the line when nothing is selected.
    fn copy(&mut self, ctx: &mut Ctx, cut: bool) {
        let b = &mut self.buffer;
        let (text, done) = match (cut, b.selection().is_some()) {
            (false, true) => (b.selected_text(), "Copied the selection"),
            (true, true) => (b.cut(), "Cut the selection"),
            (false, false) => (Some(b.lines()[b.cursor().0].clone()), "Copied the line"),
            (true, false) => (b.cut_line(), "Cut the line"),
        };
        if let Some(text) = text {
            ctx.copy(text, done);
        }
    }

    fn refresh_cache(&self) {
        let mut cache = self.cache.borrow_mut();
        let version = self.buffer.version();
        let cursor = self.buffer.cursor();
        if cache.version != Some(version) {
            if self.language == QueryLanguage::MongoShell {
                cache.highlight = sql::mongo::highlight(self.buffer.lines());
                cache.statements = sql::mongo::statements(&self.buffer.text()).len();
            } else if self.language == QueryLanguage::RedisCmd {
                cache.highlight = sql::redis::highlight(self.buffer.lines());
                cache.statements = sql::redis::statements(&self.buffer.text()).len();
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
            cache.brackets = matching(self.buffer.lines(), &cache.highlight, cursor);
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
            Action::SelectWordLeft => Some((Motion::WordLeft, true)),
            Action::SelectWordRight => Some((Motion::WordRight, true)),
            _ => None,
        };
        if let Some((motion, select)) = motion {
            b.move_cursor(motion, select);
            b.break_undo();
            return None;
        }
        match action {
            Action::Input(c) => b.type_char(c),
            Action::Comment => {
                let prefix = match self.language {
                    QueryLanguage::Sql(_) | QueryLanguage::Cql => "--",
                    QueryLanguage::MongoShell => "//",
                    QueryLanguage::RedisCmd => "#",
                };
                b.toggle_comment(prefix);
            }
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
            Action::SelectLine => b.select_line(),
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
