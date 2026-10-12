//! The list of tables, with a table's columns shown under it on Enter.

use std::{cell::Cell, collections::HashMap};

use dfox_core::{engine::Command as DbCommand, errors::DbError, models::schema::TableSchema};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{List, ListItem},
    Frame,
};

use super::{inner, panel, scroll_to};
use crate::{
    action::Action,
    command::Command,
    component::{Component, Ctx, RenderCtx},
    widgets::{throbber::spinner, toast::Level, truncate},
};

#[derive(Debug, Default)]
pub struct TablesPanel {
    pub tables: Vec<String>,
    selected: usize,
    scroll: usize,
    expanded: Option<String>,
    schemas: HashMap<String, TableSchema>,
    pub loading: bool,
    pub describing: Option<String>,
    pub focused: bool,
    visible: Cell<usize>,
}

impl TablesPanel {
    pub fn refresh(&mut self, ctx: &mut Ctx) {
        self.loading = true;
        ctx.engine.send(DbCommand::ListTables);
    }

    pub fn on_tables(&mut self, result: Result<Vec<String>, DbError>, ctx: &mut Ctx) {
        self.loading = false;
        match result {
            Ok(tables) => {
                self.tables = tables;
                self.select(self.selected);
            }
            Err(err) => ctx.toast(Level::Error, format!("Couldn't list tables: {err}")),
        }
    }

    pub fn on_schema(&mut self, result: Result<TableSchema, DbError>, ctx: &mut Ctx) {
        self.describing = None;
        match result {
            Ok(schema) => {
                self.expanded = Some(schema.table_name.clone());
                self.schemas.insert(schema.table_name.clone(), schema);
            }
            Err(err) => ctx.toast(Level::Error, format!("Couldn't describe the table: {err}")),
        }
    }

    fn select(&mut self, selected: usize) {
        self.selected = selected.min(self.tables.len().saturating_sub(1));
        self.scroll = scroll_to(self.selected, self.scroll, self.visible.get());
    }

    fn toggle(&mut self, ctx: &mut Ctx) {
        let Some(table) = self.tables.get(self.selected).cloned() else {
            return;
        };
        if self.expanded.as_ref() == Some(&table) {
            self.expanded = None;
        } else if self.describing.is_none() {
            self.describing = Some(table.clone());
            ctx.engine.send(DbCommand::Describe { table });
        }
    }
}

impl Component for TablesPanel {
    fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<Command> {
        let page = self.visible.get().max(1);
        match action {
            Action::Up => self.select(self.selected.saturating_sub(1)),
            Action::Down => self.select(self.selected + 1),
            Action::PageUp => self.select(self.selected.saturating_sub(page)),
            Action::PageDown => self.select(self.selected + page),
            Action::Home => self.select(0),
            Action::End => self.select(usize::MAX),
            Action::Confirm => self.toggle(ctx),
            _ => {}
        }
        None
    }

    fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let visible = inner(area).height as usize;
        let width = inner(area).width as usize;
        self.visible.set(visible);

        let mut items = Vec::new();
        for (i, table) in self.tables.iter().enumerate().skip(self.scroll) {
            if items.len() >= visible {
                break;
            }
            let selected = i == self.selected;
            let mut spans = vec![Span::raw(format!(
                " {} {}",
                ctx.icons.table,
                truncate(table, width.saturating_sub(4))
            ))];
            if self.describing.as_ref() == Some(table) {
                spans.push(Span::raw(" "));
                spans.push(spinner(ctx, theme.key()));
            }
            let style = match (selected, self.focused) {
                (true, true) => theme.highlight(),
                (true, false) => theme.selection(),
                _ => theme.base(),
            };
            items.push(ListItem::new(Line::from(spans).style(style)));

            if self.expanded.as_ref() == Some(table) {
                if let Some(schema) = self.schemas.get(table) {
                    for column in &schema.columns {
                        let null = if column.is_nullable { "" } else { " not null" };
                        let default = column
                            .default
                            .as_ref()
                            .map(|d| format!(" = {d}"))
                            .unwrap_or_default();
                        items.push(ListItem::new(Line::from(vec![
                            Span::styled(format!("   {} ", ctx.icons.column), theme.dim()),
                            Span::raw(column.name.clone()),
                            Span::styled(
                                format!(" {}{null}{default}", column.data_type),
                                theme.dim(),
                            ),
                        ])));
                    }
                }
            }
        }

        let mut title = vec![Span::raw("Tables")];
        if self.loading {
            title.push(Span::raw(" "));
            title.push(spinner(ctx, theme.key()));
        } else {
            title.push(Span::styled(format!(" {}", self.tables.len()), theme.dim()));
        }
        f.render_widget(
            List::new(items).block(panel(title, self.focused, ctx)),
            area,
        );
    }

    fn is_animating(&self) -> bool {
        self.loading || self.describing.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{render, TestCtx};
    use dfox_core::models::schema::ColumnSchema;

    #[test]
    fn tables_with_a_described_table() {
        let test = TestCtx::new();
        let mut panel = TablesPanel {
            tables: vec!["customers".into(), "orders".into(), "products".into()],
            focused: true,
            describing: Some("products".into()),
            ..Default::default()
        };
        panel.selected = 1;
        panel.expanded = Some("orders".into());
        panel.schemas.insert(
            "orders".into(),
            TableSchema {
                table_name: "orders".into(),
                columns: vec![
                    ColumnSchema {
                        name: "id".into(),
                        data_type: "integer".into(),
                        is_nullable: false,
                        default: Some("nextval('orders_id_seq')".into()),
                    },
                    ColumnSchema {
                        name: "note".into(),
                        data_type: "text".into(),
                        is_nullable: true,
                        default: None,
                    },
                ],
                indexes: Vec::new(),
            },
        );
        insta::assert_snapshot!(render(&test, 50, 10, |f, area, ctx| panel.render(f, area, ctx)));
    }
}
