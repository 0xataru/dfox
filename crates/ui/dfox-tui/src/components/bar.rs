//! The new layout's connection bar, and the list of databases it opens.

use std::cell::Cell;

use dfox_core::models::connections::DbType;
use ratatui::{
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use super::{manager::tag_color, scroll_to};
use crate::{action::Action, component::RenderCtx, connections::Tag};

/// Rows of databases shown at once.
const SHOWN: usize = 10;

/// What a toolbar button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    Action(Action),
    Sql(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Button {
    pub click: Click,
    pub label: &'static str,
    /// Nerd Font, Unicode, ASCII.
    glyphs: [&'static str; 3],
}

const fn button(click: Click, label: &'static str, glyphs: [&'static str; 3]) -> Button {
    Button {
        click,
        label,
        glyphs,
    }
}

pub const RUN: Button = button(
    Click::Action(Action::Execute),
    "Run the statement",
    ["\u{f04b}", "▶", ">"],
);
pub const STOP: Button = button(
    Click::Action(Action::Copy),
    "Stop the query",
    ["\u{f04d}", "■", "#"],
);
pub const COMMIT: Button = button(Click::Sql("COMMIT"), "Commit", ["\u{f00c}", "✔", "C"]);
pub const ROLLBACK: Button = button(Click::Sql("ROLLBACK"), "Roll back", ["\u{f0e2}", "↶", "R"]);
pub const SCRIPT: Button = button(
    Click::Action(Action::ExecuteAll),
    "Run the whole script",
    ["\u{f04e}", "»", ">>"],
);
pub const EXPLAIN: Button = button(
    Click::Action(Action::Explain),
    "Explain the statement",
    ["\u{f0e8}", "Ex", "Ex"],
);
pub const ANALYZE: Button = button(
    Click::Action(Action::ExplainAnalyze),
    "Explain analyze: runs it",
    ["\u{f0e4}", "Ex+", "Ex+"],
);
pub const FORMAT: Button = button(
    Click::Action(Action::Format),
    "Format the statement",
    ["\u{f036}", "≡", "F"],
);
pub const HISTORY: Button = button(
    Click::Action(Action::History),
    "Query history",
    ["\u{f1da}", "⟲", "H"],
);
pub const HELP: Button = button(Click::Action(Action::Help), "Keys", ["\u{f059}", "?", "?"]);

impl Button {
    fn glyph(&self, ctx: &RenderCtx) -> &'static str {
        match (ctx.icons.nerd, ctx.icons.ascii) {
            (true, _) => self.glyphs[0],
            (_, true) => self.glyphs[2],
            _ => self.glyphs[1],
        }
    }

    /// What the status bar says while the mouse is over it.
    pub fn tooltip(&self, keymap: &crate::keymap::Keymap) -> String {
        match self.click {
            Click::Action(action) => match keymap.hint(action) {
                key if key.is_empty() => self.label.to_string(),
                key => format!("{} · {key}", self.label),
            },
            Click::Sql(sql) => format!("{} · runs {sql}", self.label),
        }
    }
}

pub struct Bar<'a> {
    pub tag: Tag,
    pub driver: Option<DbType>,
    pub connection: &'a str,
    pub database: &'a str,
    pub read_only: bool,
    pub in_transaction: bool,
    /// The toolbar, most wanted first: the last ones go when it doesn't fit.
    pub buttons: &'a [Button],
}

impl Bar<'_> {
    /// Draws the bar on `area`'s row and returns where the database is, and
    /// where each button that fit is.
    pub fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        ctx: &RenderCtx,
    ) -> (Rect, Vec<(Rect, Button)>) {
        let theme = ctx.theme;
        let mut spans = vec![Span::raw(" ")];
        if self.tag != Tag::None {
            spans.push(Span::styled(
                "▌",
                Style::default().fg(tag_color(theme, self.tag)),
            ));
        }
        if let Some(driver) = self.driver {
            spans.push(Span::styled(
                format!("{} ", ctx.icons.engine(driver)),
                theme.engine(driver),
            ));
        }
        spans.push(Span::raw(self.connection.to_string()));
        spans.push(Span::styled(" · ", theme.dim()));
        let before: usize = spans.iter().map(|s| s.content.width()).sum();
        let database = format!("{} ▾", self.database);
        let width = database.width();
        spans.push(Span::styled(database, theme.key()));
        if self.read_only {
            spans.push(Span::styled(" · read-only", theme.warning()));
        }
        if self.in_transaction {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                " TXN ",
                Style::default()
                    .fg(theme.bg)
                    .bg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let used: usize = spans.iter().map(|s| s.content.width()).sum();
        f.render_widget(Paragraph::new(Line::from(spans)).style(theme.base()), area);
        let database =
            Rect::new(area.x + before as u16, area.y, width as u16, 1).intersection(area);

        let mut room = (area.width as usize).saturating_sub(used + 2);
        let fit: Vec<(Button, String)> = self
            .buttons
            .iter()
            .map(|b| (*b, format!(" {} ", b.glyph(ctx))))
            .take_while(|(_, text)| {
                let fits = text.width() <= room;
                room = room.saturating_sub(text.width());
                fits
            })
            .collect();
        let total: usize = fit.iter().map(|(_, text)| text.width()).sum();
        let mut x = area.right().saturating_sub(total as u16 + 1);
        let mut spots = Vec::new();
        for (button, text) in fit {
            let spot = Rect::new(x, area.y, text.width() as u16, 1);
            f.render_widget(Span::styled(text, theme.key()), spot);
            spots.push((spot, button));
            x += spot.width;
        }
        (database, spots)
    }
}

/// What a choice in the list of databases does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    Database(String),
    Close,
}

/// The databases on the server, one to switch to.
#[derive(Debug)]
pub struct DatabasePicker {
    names: Vec<String>,
    selected: usize,
    scroll: Cell<usize>,
    area: Cell<Rect>,
}

impl DatabasePicker {
    pub fn new(names: Vec<String>, active: &str) -> Self {
        let selected = names.iter().position(|n| n == active).unwrap_or(0);
        Self {
            names,
            selected,
            scroll: Cell::new(0),
            area: Cell::new(Rect::default()),
        }
    }

    pub fn handle(&mut self, action: Action) -> Option<Picked> {
        let last = self.names.len().saturating_sub(1);
        match action {
            Action::Up => self.selected = self.selected.saturating_sub(1),
            Action::Down => self.selected = (self.selected + 1).min(last),
            Action::Home => self.selected = 0,
            Action::End => self.selected = last,
            Action::Confirm => return self.names.get(self.selected).cloned().map(Picked::Database),
            Action::Back | Action::Quit => return Some(Picked::Close),
            _ => {}
        }
        None
    }

    /// A click picks the database under it; anywhere else closes the list.
    pub fn click(&self, column: u16, row: u16) -> Picked {
        let area = self.area.get();
        let inner = Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height.saturating_sub(2),
        );
        if !inner.contains(Position::new(column, row)) {
            return Picked::Close;
        }
        let i = self.scroll.get() + (row - inner.y) as usize;
        self.names
            .get(i)
            .cloned()
            .map_or(Picked::Close, Picked::Database)
    }

    /// Draws the list under `at`, kept within `bounds`.
    pub fn render(&self, f: &mut Frame, at: Rect, bounds: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let rows = self.names.len().clamp(1, SHOWN);
        let widest = self.names.iter().map(|n| n.width()).max().unwrap_or(0);
        let width = (widest as u16 + 4).max(at.width + 2).min(bounds.width);
        let height = (rows as u16 + 2).min(bounds.bottom().saturating_sub(at.y + 1));
        let x =
            at.x.saturating_sub(1)
                .min(bounds.right().saturating_sub(width));
        let area = Rect::new(x, at.y + 1, width, height);
        self.area.set(area);
        let visible = height.saturating_sub(2) as usize;
        let scroll = scroll_to(self.selected, self.scroll.get(), visible);
        self.scroll.set(scroll);
        let lines: Vec<Line> = self
            .names
            .iter()
            .enumerate()
            .skip(scroll)
            .take(visible)
            .map(|(i, name)| {
                let style = if i == self.selected {
                    theme.selection()
                } else {
                    theme.base()
                };
                Line::styled(format!(" {name} "), style)
            })
            .collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .style(theme.surface());
        f.render_widget(Clear, area);
        f.render_widget(Paragraph::new(lines).block(block), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_clicks_pick_a_database() {
        let names = vec!["postgres".to_string(), "shop".into(), "stats".into()];
        let mut picker = DatabasePicker::new(names, "shop");
        assert_eq!(picker.handle(Action::Down), None);
        assert_eq!(picker.handle(Action::Down), None);
        assert_eq!(
            picker.handle(Action::Confirm),
            Some(Picked::Database("stats".into()))
        );
        assert_eq!(picker.handle(Action::Back), Some(Picked::Close));

        picker.area.set(Rect::new(10, 5, 12, 5));
        assert_eq!(picker.click(12, 6), Picked::Database("postgres".into()));
        assert_eq!(picker.click(12, 8), Picked::Database("stats".into()));
        assert_eq!(picker.click(30, 6), Picked::Close);
    }
}
