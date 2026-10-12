//! The first screen: which kind of database to connect to.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{List, ListItem},
    Frame,
};

use super::panel;
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, RenderCtx},
    widgets::{centered, toast::Level},
};
use dfox_core::models::connections::DbType;

const CHOICES: &[(&str, Option<DbType>)] = &[
    ("PostgreSQL", Some(DbType::Postgres)),
    ("MySQL", Some(DbType::MySql)),
    ("SQLite", None),
];

#[derive(Debug, Default)]
pub struct DbTypeScreen {
    selected: usize,
}

impl Component for DbTypeScreen {
    fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        match action {
            Action::Up => self.selected = self.selected.saturating_sub(1),
            Action::Down => self.selected = (self.selected + 1).min(CHOICES.len() - 1),
            Action::Confirm => match CHOICES[self.selected] {
                (_, Some(db_type)) => return Some(Command::ConnectForm(db_type)),
                (name, None) => ctx.toast(Level::Info, format!("{name} isn't supported yet")),
            },
            Action::Back => return Some(Command::Quit),
            _ => {}
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let items: Vec<ListItem> = CHOICES
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                let line = if i == self.selected {
                    Line::from(format!(" {} {name}", ctx.icons.selected)).style(theme.highlight())
                } else {
                    Line::from(format!("   {name}"))
                };
                ListItem::new(line)
            })
            .collect();
        let block = panel(vec![Span::raw("Connect to")], true, ctx);
        f.render_widget(
            List::new(items).block(block),
            centered(area, 40, CHOICES.len() as u16 + 2),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, TestCtx};

    #[test]
    fn db_type_screen() {
        let test = TestCtx::new();
        let screen = DbTypeScreen { selected: 1 };
        insta::assert_snapshot!(render(&test, 80, 24, |f, area, ctx| screen.render(f, area, ctx)));
    }
}
