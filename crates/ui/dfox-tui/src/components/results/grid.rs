//! The result grid: a cell cursor, a selection, and drawing only the cells
//! on screen, so a million rows draw as fast as ten.

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
};

use dfox_core::result::{Cell as Value, ResultSet};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::export::Region;
use crate::{action::Action, component::RenderCtx, components::scroll_to, icons::Icons};

/// Rows sampled to size a column.
const SAMPLE_ROWS: usize = 200;
/// Widest a column starts out; `>` widens it.
const MAX_AUTO_WIDTH: u16 = 40;
const MIN_WIDTH: u16 = 3;
const MAX_WIDTH: u16 = 200;
/// Space between columns.
const GAP: u16 = 2;
/// Characters of a long value looked at when drawing it.
const MAX_SHOWN: usize = 512;

#[derive(Debug, Default)]
struct Widths {
    sampled: usize,
    widths: Vec<u16>,
}

/// Where things were drawn, for the mouse.
#[derive(Debug, Default)]
struct Drawn {
    first_row: Rect,
    top: usize,
    /// Column index, x, and width.
    columns: Vec<(usize, u16, u16)>,
}

#[derive(Debug, Default)]
pub struct Grid {
    /// Row and column of the current cell.
    cursor: (usize, usize),
    /// Where the selection started; the cursor is its other corner.
    anchor: Option<(usize, usize)>,
    /// The selection covers whole rows.
    whole_rows: bool,
    top: Cell<usize>,
    left: Cell<usize>,
    widths: RefCell<Widths>,
    /// Width changes made with `<` and `>`, per column.
    adjust: Vec<i16>,
    page: Cell<usize>,
    drawn: RefCell<Drawn>,
    /// The column the rows are sorted by, and whether downwards.
    pub sorted: Option<(usize, bool)>,
}

/// A result with `added` empty rows after its own, for new rows being
/// edited: the grid draws their values from its [`Marks`].
pub struct WithNew<'a, R> {
    pub result: &'a R,
    pub added: usize,
}

impl<R: ResultSet> ResultSet for WithNew<'_, R> {
    fn columns(&self) -> &[dfox_core::result::ColumnMeta] {
        self.result.columns()
    }

    fn row_count(&self) -> usize {
        self.result.row_count() + self.added
    }

    fn cell(&self, row: usize, col: usize) -> Option<Value<'_>> {
        if row < self.result.row_count() {
            self.result.cell(row, col)
        } else {
            // Blank, not NULL: what isn't set gets the column's default.
            (row < self.row_count() && col < self.columns().len()).then_some(Value::Text(""))
        }
    }

    fn is_complete(&self) -> bool {
        self.result.is_complete()
    }
}

/// Edits waiting to be saved, as the grid shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    /// New values by (row, column); `None` is NULL.
    pub cells: BTreeMap<(usize, usize), Option<String>>,
    /// Rows to delete.
    pub deleted: BTreeSet<usize>,
}

impl Grid {
    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    pub fn has_selection(&self) -> bool {
        self.anchor.is_some()
    }

    fn clear_selection(&mut self) {
        self.anchor = None;
        self.whole_rows = false;
    }

    /// The selected cells, or the current one.
    pub fn region(&self, columns: usize) -> Region {
        let (row, col) = self.cursor;
        let (anchor_row, anchor_col) = self.anchor.unwrap_or(self.cursor);
        let cols = if self.whole_rows {
            0..columns
        } else {
            anchor_col.min(col)..anchor_col.max(col) + 1
        };
        Region {
            rows: anchor_row.min(row)..anchor_row.max(row) + 1,
            cols,
        }
    }

    fn in_selection(&self, row: usize, col: usize, columns: usize) -> bool {
        if self.anchor.is_none() && !self.whole_rows {
            return false;
        }
        let region = self.region(columns);
        region.rows.contains(&row) && region.cols.contains(&col)
    }

    fn move_to(&mut self, row: usize, col: usize, select: bool, size: (usize, usize)) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.clear_selection();
        }
        self.cursor = (
            row.min(size.0.saturating_sub(1)),
            col.min(size.1.saturating_sub(1)),
        );
    }

    /// Handles a key. `size` is rows by columns. Returns false when the key
    /// isn't the grid's.
    pub fn handle(&mut self, action: Action, size: (usize, usize)) -> bool {
        let (row, col) = self.cursor;
        let page = self.page.get().max(1);
        let last_col = size.1.saturating_sub(1);
        match action {
            Action::Up => self.move_to(row.saturating_sub(1), col, false, size),
            Action::Down => self.move_to(row + 1, col, false, size),
            Action::Left => self.move_to(row, col.saturating_sub(1), false, size),
            Action::Right => self.move_to(row, col + 1, false, size),
            Action::PageUp => self.move_to(row.saturating_sub(page), col, false, size),
            Action::PageDown => self.move_to(row + page, col, false, size),
            Action::Home => self.move_to(0, 0, false, size),
            Action::End => self.move_to(usize::MAX, col, false, size),
            Action::WordLeft => self.move_to(row, 0, false, size),
            Action::WordRight => self.move_to(row, last_col, false, size),
            Action::SelectUp => self.move_to(row.saturating_sub(1), col, true, size),
            Action::SelectDown => self.move_to(row + 1, col, true, size),
            Action::SelectLeft => self.move_to(row, col.saturating_sub(1), true, size),
            Action::SelectRight => self.move_to(row, col + 1, true, size),
            Action::SelectHome => self.move_to(row, 0, true, size),
            Action::SelectEnd => self.move_to(row, last_col, true, size),
            Action::SelectAll => {
                self.anchor = Some((0, 0));
                self.whole_rows = true;
                self.cursor = (size.0.saturating_sub(1), col);
            }
            Action::SelectRow => {
                self.anchor.get_or_insert(self.cursor);
                self.whole_rows = !self.whole_rows;
            }
            Action::Wider | Action::Narrower => {
                if self.adjust.len() < size.1 {
                    self.adjust.resize(size.1, 0);
                }
                let step = if action == Action::Wider { 4 } else { -4 };
                if let Some(adjust) = self.adjust.get_mut(col) {
                    *adjust = adjust.saturating_add(step);
                }
            }
            Action::Back if self.anchor.is_some() => self.clear_selection(),
            _ => return false,
        }
        true
    }

    /// The cell at a point on screen.
    pub fn cell_at(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        let drawn = self.drawn.borrow();
        let area = drawn.first_row;
        if row < area.y || row >= area.y + area.height {
            return None;
        }
        let (col, _, _) = drawn
            .columns
            .iter()
            .find(|(_, x, w)| column >= *x && column < x + w + GAP)?;
        Some((drawn.top + (row - area.y) as usize, *col))
    }

    /// Where `row` was last drawn: the cell of column `col`, or the whole
    /// row with `None`. `None` when it is off screen.
    pub fn area_of(&self, row: usize, col: Option<usize>) -> Option<Rect> {
        let drawn = self.drawn.borrow();
        let rows = drawn.first_row;
        let offset = row.checked_sub(drawn.top)?;
        if offset >= rows.height as usize {
            return None;
        }
        let y = rows.y + offset as u16;
        match col {
            None => Some(Rect::new(rows.x, y, rows.width, 1)),
            Some(col) => drawn
                .columns
                .iter()
                .find(|(c, _, _)| *c == col)
                .map(|(_, x, w)| Rect::new(*x, y, *w, 1)),
        }
    }

    /// Moves to a clicked cell, or extends the selection to it.
    pub fn click(&mut self, cell: (usize, usize), drag: bool, size: (usize, usize)) {
        self.move_to(cell.0, cell.1, drag, size);
    }

    /// Scrolls by `rows`, moving the cursor along.
    pub fn scroll_by(&mut self, rows: isize, size: (usize, usize)) {
        let (row, col) = self.cursor;
        let row = row.saturating_add_signed(rows);
        self.move_to(row, col, false, size);
    }

    pub fn scroll_columns(&mut self, cols: isize, size: (usize, usize)) {
        let (row, col) = self.cursor;
        self.move_to(row, col.saturating_add_signed(cols), false, size);
    }

    /// Column widths from the header and the first rows, plus what the user
    /// changed.
    fn widths(&self, result: &impl ResultSet, icons: &Icons) -> Vec<u16> {
        let mut cache = self.widths.borrow_mut();
        let columns = result.columns();
        let sampled = result.row_count().min(SAMPLE_ROWS);
        if cache.sampled != sampled || cache.widths.len() != columns.len() || sampled == 0 {
            cache.widths = columns
                .iter()
                .enumerate()
                .map(|(col, meta)| {
                    let content = (0..sampled)
                        .filter_map(|row| result.cell(row, col))
                        .map(|value| display(value, icons).width())
                        .max()
                        .unwrap_or(0);
                    (content.max(meta.name.width()) as u16).clamp(MIN_WIDTH, MAX_AUTO_WIDTH)
                })
                .collect();
            cache.sampled = sampled;
        }
        cache
            .widths
            .iter()
            .enumerate()
            .map(|(col, width)| {
                let adjust = self.adjust.get(col).copied().unwrap_or(0);
                (*width as i32 + adjust as i32).clamp(MIN_WIDTH as i32, MAX_WIDTH as i32) as u16
            })
            .collect()
    }

    /// Draws `result` into `area`: a header, a rule, and the rows on screen.
    pub fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        result: &impl ResultSet,
        focused: bool,
        ctx: &RenderCtx,
    ) {
        self.render_marked(buf, area, result, focused, ctx, &Marks::default());
    }

    /// Draws as [`Grid::render`] does, with the edits in `marks` over the
    /// rows: new values in place of the old, deleted rows struck through.
    pub fn render_marked(
        &self,
        buf: &mut Buffer,
        area: Rect,
        result: &impl ResultSet,
        focused: bool,
        ctx: &RenderCtx,
        marks: &Marks,
    ) {
        let theme = ctx.theme;
        let rows = result.row_count();
        let columns = result.columns().len();
        if area.height < 3 || area.width < 8 || columns == 0 {
            return;
        }
        let widths = self.widths(result, ctx.icons);
        let (cursor_row, cursor_col) = (
            self.cursor.0.min(rows.saturating_sub(1)),
            self.cursor.1.min(columns - 1),
        );

        let number_width = rows.max(1).to_string().len() as u16;
        let prefix = number_width + 3;
        let data_x = area.x + prefix;
        let data_width = area.width.saturating_sub(prefix);
        let visible = (area.height - 2) as usize;
        self.page.set(visible);
        let top = scroll_to(cursor_row, self.top.get(), visible);
        self.top.set(top);
        let left = first_column(&widths, self.left.get(), cursor_col, data_width);
        self.left.set(left);

        // Columns that fit, the last one cut short if at least a little shows.
        let mut placed = Vec::new();
        let mut x = data_x;
        for (col, width) in widths.iter().enumerate().skip(left) {
            let room = (data_x + data_width).saturating_sub(x);
            if room < MIN_WIDTH {
                break;
            }
            let width = (*width).min(room);
            placed.push((col, x, width));
            x += width + GAP;
        }

        // Header and rule.
        let dim = theme.dim();
        let rule_style = theme.border(false);
        let y = area.y;
        buf.set_stringn(
            area.x,
            y,
            format!("{:>w$}", "#", w = number_width as usize),
            number_width as usize,
            dim,
        );
        buf.set_string(area.x + number_width, y, " │ ", rule_style);
        let rule: String = "─".repeat(area.width as usize);
        buf.set_stringn(area.x, y + 1, &rule, area.width as usize, rule_style);
        buf.set_string(area.x + number_width + 1, y + 1, "┼", rule_style);
        for &(col, x, width) in &placed {
            let name = &result.columns()[col].name;
            let name = match self.sorted {
                Some((c, false)) if c == col => format!("{name} ▲"),
                Some((c, true)) if c == col => format!("{name} ▼"),
                _ => name.clone(),
            };
            let name = &name;
            let style = if col == cursor_col && focused {
                theme.key()
            } else {
                theme.header()
            };
            put(buf, x, y, name, width, false, style);
        }

        // Rows.
        for (i, row) in (top..rows.min(top + visible)).enumerate() {
            let y = area.y + 2 + i as u16;
            let number_style = if row == cursor_row { theme.key() } else { dim };
            buf.set_stringn(
                area.x,
                y,
                format!("{:>w$}", row + 1, w = number_width as usize),
                number_width as usize,
                number_style,
            );
            buf.set_string(area.x + number_width, y, " │ ", rule_style);
            let deleted = marks.deleted.contains(&row);
            for &(col, x, width) in &placed {
                let mut value = result.cell(row, col).unwrap_or(Value::Null);
                let mut style = theme.cell(&value);
                let edited = marks.cells.get(&(row, col));
                if let Some(new) = edited {
                    value = new.as_deref().map_or(Value::Null, Value::Text);
                    style = theme.warning().add_modifier(Modifier::BOLD);
                }
                if deleted {
                    style = theme.danger().add_modifier(Modifier::CROSSED_OUT);
                }
                let selected = self.in_selection(row, col, columns);
                if row == cursor_row && col == cursor_col {
                    style = if focused {
                        theme.highlight()
                    } else {
                        theme.selection()
                    };
                } else if selected {
                    style = style.bg(theme.selection);
                }
                // A selection runs through the gaps between its cells.
                let gap_selected = selected && self.in_selection(row, col + 1, columns);
                if gap_selected {
                    buf.set_string(
                        x + width,
                        y,
                        " ".repeat(GAP as usize),
                        Style::default().bg(theme.selection),
                    );
                }
                put(
                    buf,
                    x,
                    y,
                    &display(value, ctx.icons),
                    width,
                    value.is_numeric(),
                    style,
                );
            }
        }

        *self.drawn.borrow_mut() = Drawn {
            first_row: Rect::new(area.x, area.y + 2, area.width, visible as u16),
            top,
            columns: placed,
        };
    }

    #[cfg(test)]
    pub fn contains(&self, column: u16, row: u16) -> bool {
        self.drawn
            .borrow()
            .first_row
            .contains(ratatui::layout::Position::new(column, row))
    }
}

/// The first column to draw so that the cursor's column fits.
fn first_column(widths: &[u16], left: usize, cursor: usize, room: u16) -> usize {
    let mut left = left.min(cursor);
    loop {
        let used: u16 = widths[left..=cursor]
            .iter()
            .map(|w| w + GAP)
            .sum::<u16>()
            .saturating_sub(GAP);
        if used <= room || left == cursor {
            return left;
        }
        left += 1;
    }
}

/// Writes `text` into `width` columns, cut with `…` when longer, right-aligned
/// when asked. The whole width takes `style`.
fn put(buf: &mut Buffer, x: u16, y: u16, text: &str, width: u16, right: bool, style: Style) {
    let width = width as usize;
    let text_width = text.width();
    let fitted: Cow<str> = if text_width <= width {
        text.into()
    } else {
        let mut out = String::new();
        let mut used = 0;
        for c in text.chars() {
            let w = c.width().unwrap_or(0);
            if used + w + 1 > width {
                break;
            }
            used += w;
            out.push(c);
        }
        out.push('…');
        out.into()
    };
    let shown = fitted.width();
    let pad = width.saturating_sub(shown);
    let line = if right {
        format!("{}{fitted}", " ".repeat(pad))
    } else {
        format!("{fitted}{}", " ".repeat(pad))
    };
    buf.set_stringn(x, y, line, width, style);
}

/// A value as the grid shows it: one line, NULL and booleans as symbols,
/// long numbers grouped in thousands, long text cut short.
pub fn display<'a>(value: Value<'a>, icons: &Icons) -> Cow<'a, str> {
    match value {
        Value::Null => icons.null.into(),
        Value::Bool(true) => icons.yes.into(),
        Value::Bool(false) => icons.no.into(),
        Value::Int(_) | Value::UInt(_) | Value::Float(_) => group_digits(&value.to_string()).into(),
        Value::Decimal(text) => group_digits(text).into(),
        Value::Text(text) | Value::Json(text) => one_line(text),
        Value::Bytes(bytes) if bytes.len() > MAX_SHOWN / 2 => {
            let shown: String = bytes[..MAX_SHOWN / 2]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("0x{shown}…").into()
        }
        other => one_line(&other.to_string()).into_owned().into(),
    }
}

/// Text on one line: line breaks as `↵`, other control characters as spaces.
fn one_line(text: &str) -> Cow<'_, str> {
    let long = text.len() > MAX_SHOWN;
    if !long && !text.chars().any(char::is_control) {
        return text.into();
    }
    let mut out: String = text
        .chars()
        .take(MAX_SHOWN)
        .map(|c| match c {
            '\n' => '↵',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    if long {
        out.push('…');
    }
    out.into()
}

/// `1234567.5` → `1,234,567.5`, for numbers of five digits or more before the
/// point. Anything else comes back as it was.
pub fn group_digits(text: &str) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text),
    };
    let split = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (int, tail) = rest.split_at(split);
    if int.len() < 5 || tail.contains(['e', 'E']) || !(tail.is_empty() || tail.starts_with('.')) {
        return text.to_string();
    }
    let mut grouped = String::with_capacity(text.len() + int.len() / 3);
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{sign}{grouped}{tail}")
}

#[cfg(test)]
#[path = "grid_tests.rs"]
mod tests;
