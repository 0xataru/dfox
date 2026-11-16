//! The new layout's Messages: how each query of the tab went, and what the
//! server said while it ran.

use std::collections::VecDeque;

use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::component::RenderCtx;

/// Queries remembered per tab.
const KEPT: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    sql: String,
    outcome: Result<String, String>,
    notices: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Messages {
    entries: VecDeque<Entry>,
    /// What the server said about the query still running.
    pending: Vec<String>,
}

impl Messages {
    pub fn notice(&mut self, message: String) {
        self.pending.push(message);
    }

    /// Query `sql` ended with `outcome`, which takes the notices it got.
    pub fn done(&mut self, sql: &str, outcome: Result<String, String>) {
        if self.entries.len() == KEPT {
            self.entries.pop_front();
        }
        let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        self.entries.push_back(Entry {
            sql,
            outcome,
            notices: std::mem::take(&mut self.pending),
        });
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The latest messages that fit, newest at the bottom.
    // shortcut: no scrolling back past what fits, add it when someone asks.
    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        if self.entries.is_empty() {
            let [line] = Layout::vertical([Constraint::Length(1)])
                .flex(Flex::Center)
                .areas(area);
            let text = Line::from(vec![
                Span::styled(format!("{} ", ctx.icons.info), theme.key()),
                Span::styled("No messages. Run a query to see how it went.", theme.dim()),
            ]);
            f.render_widget(Paragraph::new(text).centered(), line);
            return;
        }
        let mut lines: Vec<Line> = Vec::new();
        for entry in &self.entries {
            if !lines.is_empty() {
                lines.push(Line::raw(""));
            }
            let (icon, style) = match entry.outcome {
                Ok(_) => (ctx.icons.success, theme.key()),
                Err(_) => (ctx.icons.error, theme.danger()),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{icon} "), style),
                Span::raw(entry.sql.clone()),
            ]));
            for notice in &entry.notices {
                lines.push(Line::styled(format!("  {notice}"), theme.warning()));
            }
            lines.push(match &entry.outcome {
                Ok(text) => Line::styled(format!("  {text}"), theme.dim()),
                Err(text) => Line::styled(format!("  {text}"), theme.danger()),
            });
        }
        let skip = lines.len().saturating_sub(area.height as usize);
        f.render_widget(Paragraph::new(lines.split_off(skip)), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_go_with_their_query_and_old_ones_drop() {
        let mut messages = Messages::default();
        messages.notice("NOTICE: table \"t\" does not exist, skipping".into());
        messages.done("drop table\n  if exists t", Ok("done · 1 ms".into()));
        messages.done("select 1", Err("boom".into()));
        assert_eq!(messages.entries[0].sql, "drop table if exists t");
        assert_eq!(messages.entries[0].notices.len(), 1);
        assert!(messages.entries[1].notices.is_empty());
        for _ in 0..KEPT {
            messages.done("select 1", Ok("1 row(s)".into()));
        }
        assert_eq!(messages.entries.len(), KEPT);
        assert!(messages.entries.iter().all(|e| e.outcome.is_ok()));
    }
}
