//! Drawing the explorer: the tree, or the filter's matches.

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{List, ListItem},
    Frame,
};
use tui_tree_widget::{Tree, TreeItem};

use super::{Database, Explorer, Load, Object, Schema, GROUPS};
use crate::{
    component::RenderCtx,
    components::{inner, panel},
    widgets::{throbber::spinner, truncate},
};
use dfox_core::models::schema::ObjectKind;

pub fn render(explorer: &Explorer, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let visible = inner(area).height as usize;
    explorer.visible.set(visible);
    match &explorer.filter {
        Some(query) => render_matches(explorer, query, f, area, ctx),
        None => render_tree(explorer, f, area, ctx),
    }
}

pub fn is_loading(explorer: &Explorer) -> bool {
    let Load::Loaded(databases) = &explorer.databases else {
        return matches!(explorer.databases, Load::Loading);
    };
    databases.iter().any(|db| match &db.schemas {
        Load::Loading => true,
        Load::Loaded(schemas) => schemas.iter().any(|s| match &s.objects {
            Load::Loading => true,
            Load::Loaded(objects) => objects.iter().any(|o| matches!(o.columns, Load::Loading)),
            _ => false,
        }),
        _ => false,
    })
}

fn render_tree(explorer: &Explorer, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let theme = ctx.theme;
    let mut title = vec![Span::raw("Explorer")];
    if is_loading(explorer) {
        title.push(Span::raw(" "));
        title.push(spinner(ctx, theme.key()));
    }
    let block = panel(title, explorer.focused, ctx);

    let items: Vec<TreeItem<String>> = match &explorer.databases {
        Load::Loaded(databases) => databases
            .iter()
            .map(|db| database_item(explorer, db, ctx))
            .collect(),
        Load::Failed(err) => vec![TreeItem::new_leaf(
            "error".into(),
            Line::styled(err.clone(), theme.danger()),
        )],
        _ => Vec::new(),
    };
    let highlight = if explorer.focused {
        theme.highlight()
    } else {
        theme.selection()
    };
    match Tree::new(&items) {
        Ok(tree) => {
            let tree = tree
                .block(block)
                .highlight_style(highlight)
                .node_closed_symbol("▸ ")
                .node_open_symbol("▾ ")
                .node_no_children_symbol("  ");
            f.render_stateful_widget(tree, area, &mut explorer.state.borrow_mut());
        }
        Err(err) => f.render_widget(
            ratatui::widgets::Paragraph::new(err.to_string()).block(block),
            area,
        ),
    }
}

fn database_item<'a>(
    explorer: &Explorer,
    db: &'a Database,
    ctx: &RenderCtx,
) -> TreeItem<'a, String> {
    let theme = ctx.theme;
    let active = db.name == explorer.active;
    let mut text = vec![Span::raw(format!("{} {}", ctx.icons.database, db.name))];
    if active {
        text.push(Span::styled(" ●", theme.success()));
    }
    let children = match &db.schemas {
        Load::Loaded(schemas) if schemas.len() == 1 => group_items(&schemas[0], ctx),
        Load::Loaded(schemas) => schemas.iter().map(|s| schema_item(s, ctx)).collect(),
        Load::Loading => vec![loading_item(ctx)],
        Load::Failed(err) => vec![error_item(err, ctx)],
        Load::NotLoaded => Vec::new(),
    };
    item(Explorer::db_id(&db.name), Line::from(text), children)
}

fn schema_item<'a>(schema: &'a Schema, ctx: &RenderCtx) -> TreeItem<'a, String> {
    let theme = ctx.theme;
    let mut text = vec![Span::raw(format!("◇ {}", schema.name))];
    if let Load::Loaded(objects) = &schema.objects {
        text.push(Span::styled(format!(" {}", objects.len()), theme.dim()));
    }
    item(
        format!("s/{}", schema.name),
        Line::from(text),
        group_items(schema, ctx),
    )
}

fn group_items<'a>(schema: &'a Schema, ctx: &RenderCtx) -> Vec<TreeItem<'a, String>> {
    let objects = match &schema.objects {
        Load::Loaded(objects) => objects,
        Load::Loading => return vec![loading_item(ctx)],
        Load::Failed(err) => return vec![error_item(err, ctx)],
        Load::NotLoaded => return Vec::new(),
    };
    GROUPS
        .into_iter()
        .filter_map(|kind| {
            let members: Vec<&Object> = objects.iter().filter(|o| o.object.kind == kind).collect();
            if members.is_empty() {
                return None;
            }
            let text = Line::from(vec![
                Span::styled(group_name(kind), ctx.theme.header()),
                Span::styled(format!(" {}", members.len()), ctx.theme.dim()),
            ]);
            let children = members.into_iter().map(|o| object_item(o, ctx)).collect();
            Some(item(format!("g/{kind:?}"), text, children))
        })
        .collect()
}

fn object_item<'a>(object: &'a Object, ctx: &RenderCtx) -> TreeItem<'a, String> {
    let theme = ctx.theme;
    let icon = if object.object.kind.has_rows() {
        ctx.icons.table
    } else {
        "ƒ"
    };
    let mut text = vec![Span::raw(format!("{icon} {}", object.object.name))];
    if let Some(rows) = object.object.rows {
        text.push(Span::styled(format!(" ~{}", compact(rows)), theme.dim()));
    }
    let children = match &object.columns {
        Load::Loaded(columns) => columns
            .iter()
            .map(|column| {
                let null = if column.is_nullable { "" } else { " not null" };
                TreeItem::new_leaf(
                    format!("c/{}", column.name),
                    Line::from(vec![
                        Span::styled(format!("{} ", ctx.icons.column), theme.dim()),
                        Span::raw(column.name.clone()),
                        Span::styled(format!(" {}{null}", column.data_type), theme.dim()),
                    ]),
                )
            })
            .collect(),
        Load::Loading => vec![loading_item(ctx)],
        Load::Failed(err) => vec![error_item(err, ctx)],
        Load::NotLoaded => Vec::new(),
    };
    item(
        format!("o/{}", object.object.name),
        Line::from(text),
        children,
    )
}

/// A node, or a leaf if its children clash (two with the same identifier).
fn item<'a>(
    id: String,
    text: Line<'a>,
    children: Vec<TreeItem<'a, String>>,
) -> TreeItem<'a, String> {
    if children.is_empty() {
        return TreeItem::new_leaf(id, text);
    }
    match TreeItem::new(id.clone(), text.clone(), children) {
        Ok(item) => item,
        Err(_) => TreeItem::new_leaf(id, text),
    }
}

fn loading_item<'a>(ctx: &RenderCtx) -> TreeItem<'a, String> {
    TreeItem::new_leaf(
        "loading".to_string(),
        Line::styled("loading…", ctx.theme.dim()),
    )
}

fn error_item<'a>(err: &str, ctx: &RenderCtx) -> TreeItem<'a, String> {
    TreeItem::new_leaf(
        "error".to_string(),
        Line::styled(err.to_string(), ctx.theme.danger()),
    )
}

fn group_name(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Table => "Tables",
        ObjectKind::View => "Views",
        ObjectKind::MaterializedView => "Materialized views",
        ObjectKind::ForeignTable => "Foreign tables",
        ObjectKind::Function => "Functions",
        ObjectKind::Procedure => "Procedures",
    }
}

/// `950`, `1.2k`, `3.4M`.
pub fn compact(n: i64) -> String {
    match n {
        n if n < 1_000 => n.to_string(),
        n if n < 1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        n if n < 1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n => format!("{:.1}B", n as f64 / 1e9),
    }
}

fn render_matches(explorer: &Explorer, query: &str, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let theme = ctx.theme;
    let title = vec![
        Span::raw("Filter "),
        Span::styled(format!("/{query}"), theme.key()),
        Span::styled(format!(" {}", explorer.matches.len()), theme.dim()),
    ];
    let width = inner(area).width as usize;
    let visible = inner(area).height as usize;
    let first = explorer
        .selected_match
        .saturating_sub(visible.saturating_sub(1));
    let several_schemas = explorer
        .matches
        .iter()
        .any(|m| m.schema != explorer.matches[0].schema);
    let items: Vec<ListItem> = explorer
        .matches
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .map(|(i, m)| {
            let name = if several_schemas {
                format!("{}.{}", m.schema, m.object.name)
            } else {
                m.object.name.clone()
            };
            let icon = if m.object.kind.has_rows() {
                ctx.icons.table
            } else {
                "ƒ"
            };
            let line = Line::from(format!(
                "{icon} {}",
                truncate(&name, width.saturating_sub(2))
            ));
            ListItem::new(if i == explorer.selected_match {
                line.style(theme.highlight())
            } else {
                line
            })
        })
        .collect();
    f.render_widget(List::new(items).block(panel(title, true, ctx)), area);

    let cursor_x = area.x + 2 + 8 + query.chars().count() as u16;
    if cursor_x < area.x + area.width - 1 {
        f.set_cursor_position((cursor_x, area.y));
    }
}
