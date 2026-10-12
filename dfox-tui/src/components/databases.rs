//! The server's databases.

use std::cell::Cell;

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{List, ListItem},
    Frame,
};

use super::{connect::error_popup, inner, panel, scroll_to};
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, RenderCtx},
    widgets::{centered, throbber::spinner},
};

#[derive(Debug, Default)]
pub struct DatabasesScreen {
    pub databases: Vec<String>,
    selected: usize,
    scroll: usize,
    pub loading: bool,
    /// The database being opened.
    pub opening: Option<String>,
    pub error: Option<String>,
    visible: Cell<usize>,
}

impl DatabasesScreen {
    /// An empty list, waiting for the server's answer.
    pub fn loading() -> Self {
        Self {
            loading: true,
            ..Default::default()
        }
    }

    pub fn set_databases(&mut self, databases: Vec<String>) {
        self.databases = databases;
        self.selected = self.selected.min(self.databases.len().saturating_sub(1));
        self.loading = false;
    }

    fn select(&mut self, selected: usize) {
        self.selected = selected.min(self.databases.len().saturating_sub(1));
        self.scroll = scroll_to(self.selected, self.scroll, self.visible.get());
    }
}

impl Component for DatabasesScreen {
    fn handle(&mut self, action: Action, _ctx: &mut Ctx) -> Option<Command> {
        if self.error.is_some() {
            self.error = None;
            return None;
        }
        let page = self.visible.get().max(1);
        match action {
            Action::Up => self.select(self.selected.saturating_sub(1)),
            Action::Down => self.select(self.selected + 1),
            Action::PageUp => self.select(self.selected.saturating_sub(page)),
            Action::PageDown => self.select(self.selected + page),
            Action::Home => self.select(0),
            Action::End => self.select(usize::MAX),
            Action::Confirm if self.opening.is_none() => {
                let name = self.databases.get(self.selected)?.clone();
                return Some(Command::OpenDatabase(name));
            }
            Action::Back => return Some(Command::DbTypes),
            _ => {}
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let rows = self.databases.len().max(1) as u16 + 2;
        let list_area = centered(area, 56, rows.min(area.height.saturating_sub(6)).max(5));
        let visible = inner(list_area).height as usize;
        self.visible.set(visible);

        let items: Vec<ListItem> = self
            .databases
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(visible)
            .map(|(i, name)| {
                let line = if i == self.selected {
                    let mut spans = vec![Span::raw(format!(" {} {name}", ctx.icons.selected))];
                    if self.opening.as_deref() == Some(name) {
                        spans.push(Span::raw(" "));
                        spans.push(spinner(ctx, theme.highlight()));
                    }
                    Line::from(spans).style(theme.highlight())
                } else {
                    Line::from(format!("   {name}"))
                };
                ListItem::new(line)
            })
            .collect();

        let mut title = vec![Span::raw(format!("{} Databases", ctx.icons.database))];
        if self.loading {
            title.push(Span::raw(" "));
            title.push(spinner(ctx, theme.key()));
        } else {
            title.push(Span::styled(
                format!(
                    " {}/{}",
                    (self.selected + 1).min(self.databases.len()),
                    self.databases.len()
                ),
                theme.dim(),
            ));
        }
        f.render_widget(List::new(items).block(panel(title, true, ctx)), list_area);

        if let Some(error) = &self.error {
            error_popup(f, list_area, error, ctx);
        }
    }

    fn is_animating(&self) -> bool {
        self.loading || self.opening.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, TestCtx};

    fn screen() -> DatabasesScreen {
        let mut screen = DatabasesScreen::default();
        screen.set_databases((1..=30).map(|i| format!("db_{i:02}")).collect());
        screen
    }

    #[test]
    fn database_list() {
        let test = TestCtx::new();
        let mut screen = screen();
        screen.opening = Some("db_01".into());
        insta::assert_snapshot!(render(&test, 80, 24, |f, area, ctx| screen.render(f, area, ctx)));
    }

    #[test]
    fn database_list_loading() {
        let test = TestCtx::new();
        let screen = DatabasesScreen {
            loading: true,
            ..Default::default()
        };
        insta::assert_snapshot!(render(&test, 120, 40, |f, area, ctx| screen.render(f, area, ctx)));
    }

    #[test]
    fn selection_scrolls_the_list() {
        let test = TestCtx::new();
        let mut screen = screen();
        render(&test, 80, 24, |f, area, ctx| screen.render(f, area, ctx));
        let visible = screen.visible.get();
        assert!(visible > 0);

        screen.select(25);
        assert_eq!(screen.scroll, 26 - visible);
        screen.select(usize::MAX);
        assert_eq!(screen.selected, 29);
    }
}
