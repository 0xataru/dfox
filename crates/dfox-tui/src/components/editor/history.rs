//! Searching the query history from the editor (`Ctrl+R`).

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::{
    action::Action,
    component::{Ctx, RenderCtx},
    history::Entry,
    widgets::{centered, truncate},
};

/// Matches listed at once.
const LIMIT: usize = 50;

#[derive(Debug)]
pub struct HistorySearch {
    pub query: String,
    matches: Vec<Entry>,
    selected: usize,
}

impl HistorySearch {
    pub fn open(ctx: &Ctx) -> Self {
        let mut search = Self {
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
        };
        search.refresh(ctx);
        search
    }

    fn refresh(&mut self, ctx: &Ctx) {
        self.matches = ctx
            .history
            .search(&self.query, LIMIT)
            .into_iter()
            .cloned()
            .collect();
        self.selected = 0;
    }

    /// Handles a key. Returns `Some` when the search closes: with the chosen
    /// query, or `None` when cancelled.
    pub fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Option<String>> {
        match action {
            Action::Input(c) => {
                self.query.push(c);
                self.refresh(ctx);
            }
            Action::Backspace => {
                self.query.pop();
                self.refresh(ctx);
            }
            Action::Up => self.selected = self.selected.saturating_sub(1),
            Action::Down | Action::History => {
                self.selected = (self.selected + 1).min(self.matches.len().saturating_sub(1))
            }
            Action::Confirm => {
                return Some(self.matches.get(self.selected).map(|e| e.sql.clone()));
            }
            Action::Back => return Some(None),
            _ => {}
        }
        None
    }

    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let width = area.width.saturating_sub(8).clamp(20, 100);
        let height = (self.matches.len() as u16 + 4).clamp(6, area.height.saturating_sub(2).max(6));
        let popup = centered(area, width, height);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(" History ", theme.title(true)))
            .title_bottom(Line::from(Span::styled(" ↵ use · esc close ", theme.dim())).centered())
            .style(theme.surface());
        let inner = block.inner(popup);
        f.render_widget(Clear, popup);
        f.render_widget(block, popup);
        if inner.height < 2 {
            return;
        }

        let prompt = Line::from(vec![
            Span::styled(" ❯ ", theme.key()),
            Span::raw(self.query.as_str()),
        ]);
        f.render_widget(Paragraph::new(prompt), Rect { height: 1, ..inner });
        f.set_cursor_position((inner.x + 3 + self.query.chars().count() as u16, inner.y));

        let list = Rect {
            y: inner.y + 1,
            height: inner.height - 1,
            ..inner
        };
        if self.matches.is_empty() {
            let empty = if self.query.is_empty() {
                "No queries run yet"
            } else {
                "No matches"
            };
            f.render_widget(
                Paragraph::new(Span::styled(format!("   {empty}"), theme.dim())),
                list,
            );
            return;
        }
        let visible = list.height as usize;
        let first = self.selected.saturating_sub(visible.saturating_sub(1));
        let text_width = (list.width as usize).saturating_sub(4 + 16);
        let rows: Vec<Line> = self
            .matches
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .map(|(i, entry)| {
                let one_line = entry.sql.split_whitespace().collect::<Vec<_>>().join(" ");
                let marker = if entry.ok {
                    Span::styled(" ✓ ", theme.success())
                } else {
                    Span::styled(" ✗ ", theme.danger())
                };
                let style = if i == self.selected {
                    theme.highlight()
                } else {
                    theme.base().bg(theme.surface)
                };
                Line::from(vec![
                    marker,
                    Span::styled(
                        format!("{:<text_width$}", truncate(&one_line, text_width)),
                        style,
                    ),
                    Span::styled(
                        format!(" {:>15}", truncate(&entry.database, 15)),
                        theme.dim(),
                    ),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(rows), list);
    }
}
