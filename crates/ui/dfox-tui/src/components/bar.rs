//! The new layout's connection bar and its toolbar.

use dfox_core::models::connections::DbType;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use unicode_width::UnicodeWidthStr;

use super::manager::tag_color;
use crate::{action::Action, component::RenderCtx, connections::Tag};

/// What a toolbar button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    Action(Action),
    Sql(&'static str),
    /// The next row limit.
    Limit,
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
pub const NEW_TAB: Button = button(
    Click::Action(Action::NewTab),
    "New query tab",
    ["+", "+", "+"],
);
pub const FILTER: Button = button(
    Click::Action(Action::Filter),
    "Keep the rows with the cell's value",
    ["\u{f0b0}", "⊜", "="],
);
pub const UNFILTER: Button = button(
    Click::Action(Action::Unfilter),
    "Remove sort and filter",
    ["\u{f12d}", "⊘", "x"],
);
pub const LIMIT: Button = button(
    Click::Limit,
    "Rows kept per result, for this tab · click for the next",
    ["", "", ""],
);
pub const ADD_ROW: Button = button(
    Click::Action(Action::AddRow),
    "Add a row",
    ["\u{f067}", "+", "+"],
);
pub const DELETE_ROW: Button = button(
    Click::Action(Action::Delete),
    "Delete the row, or keep it",
    ["\u{f1f8}", "✗", "d"],
);
pub const SAVE: Button = button(
    Click::Action(Action::Save),
    "Save the edits",
    ["\u{f0c7}", "✔", "s"],
);
pub const COPY: Button = button(
    Click::Action(Action::Copy),
    "Copy the cell or the selection",
    ["\u{f0c5}", "⧉", "c"],
);
pub const EXPORT: Button = button(
    Click::Action(Action::Export),
    "Export the rows to a file",
    ["\u{f019}", "⤓", "x"],
);
/// The buttons over the rows: they act on the results.
pub const DATA: [Button; 5] = [ADD_ROW, DELETE_ROW, SAVE, COPY, EXPORT];
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
            Click::Limit => self.label.to_string(),
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
    /// The button under the mouse, lit up.
    pub hovered: Option<Button>,
    /// What the row limit chip says.
    pub limit: &'a str,
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

        // Each button a block of its own, a column apart.
        let mut room = (area.width as usize).saturating_sub(used + 2);
        let fit: Vec<(Button, String)> = self
            .buttons
            .iter()
            .map(|b| match b.click {
                Click::Limit => (*b, format!(" {} ", self.limit)),
                _ => (*b, format!(" {} ", b.glyph(ctx))),
            })
            .take_while(|(_, text)| {
                let fits = text.width() < room;
                room = room.saturating_sub(text.width() + 1);
                fits
            })
            .collect();
        let total: usize = fit.iter().map(|(_, text)| text.width() + 1).sum();
        let mut x = area.right().saturating_sub(total as u16);
        let mut spots = Vec::new();
        for (button, text) in fit {
            let spot = Rect::new(x, area.y, text.width() as u16, 1);
            f.render_widget(
                Span::styled(text, button_style(theme, self.hovered == Some(button))),
                spot,
            );
            spots.push((spot, button));
            x += spot.width + 1;
        }
        (database, spots)
    }
}

/// What the row limit chip says for `limit`.
pub fn limit_label(limit: usize) -> String {
    match limit {
        usize::MAX => "No limit".to_string(),
        n if n >= 10_000 && n % 1000 == 0 => format!("{}k rows", n / 1000),
        n => format!("{n} rows"),
    }
}

/// The limit after `current`: the presets and the config's, smallest first,
/// then back round.
pub fn next_limit(current: usize, config: usize) -> usize {
    let mut all = vec![100, 500, 1000, 100_000, usize::MAX, config];
    all.sort_unstable();
    all.dedup();
    all.into_iter().find(|&n| n > current).unwrap_or(100)
}

/// Draws `buttons` right to left from `x` on `y`, while they stay right of
/// `left`, and returns where each one went.
pub fn chips(
    f: &mut Frame,
    buttons: &[Button],
    (left, x, y): (u16, u16, u16),
    hovered: Option<Button>,
    ctx: &RenderCtx,
) -> Vec<(Rect, Button)> {
    let mut x = x;
    let mut spots = Vec::new();
    for button in buttons.iter().rev() {
        let text = format!(" {} ", button.glyph(ctx));
        let width = text.width() as u16;
        if x < left + width + 1 {
            break;
        }
        x -= width + 1;
        let spot = Rect::new(x, y, width, 1);
        f.render_widget(
            Span::styled(text, button_style(ctx.theme, hovered == Some(*button))),
            spot,
        );
        spots.push((spot, *button));
    }
    spots
}

/// A button: accent on a raised block, the other way round under the mouse.
pub fn button_style(theme: &crate::theme::Theme, hovered: bool) -> Style {
    if hovered {
        Style::default().fg(theme.bg).bg(theme.accent)
    } else {
        Style::default().fg(theme.accent).bg(theme.surface_alt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_go_round_through_the_presets_and_the_config() {
        let mut seen = vec![limit_label(100_000)];
        let mut limit = 100_000;
        for _ in 0..6 {
            limit = next_limit(limit, 2000);
            seen.push(limit_label(limit));
        }
        assert_eq!(
            seen,
            [
                "100k rows",
                "No limit",
                "100 rows",
                "500 rows",
                "1000 rows",
                "2000 rows",
                "100k rows"
            ]
        );
    }
}
