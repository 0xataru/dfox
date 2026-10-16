//! Drawing the explorer: the tree, or the filter's matches.

use dfox_core::catalog::NodeKind;
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{List, ListItem},
    Frame,
};
use tui_tree_widget::{Tree, TreeItem};

use super::{group_id, Entry, Explorer, Load, GROUPS};
use crate::{
    component::RenderCtx,
    components::{inner, panel},
    widgets::{throbber::spinner, truncate},
};

pub fn render(explorer: &Explorer, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let visible = inner(area).height as usize;
    explorer.visible.set(visible);
    match &explorer.filter {
        Some(query) => render_matches(explorer, query, f, area, ctx),
        None => render_tree(explorer, f, area, ctx),
    }
}

pub fn is_loading(explorer: &Explorer) -> bool {
    fn loading(list: &[Entry]) -> bool {
        list.iter().any(|e| {
            matches!(e.columns, Load::Loading)
                || match &e.children {
                    Load::Loading => true,
                    Load::Loaded(children) => loading(children),
                    _ => false,
                }
        })
    }
    match &explorer.root {
        Load::Loading => true,
        Load::Loaded(list) => loading(list),
        _ => false,
    }
}

fn render_tree(explorer: &Explorer, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
    let theme = ctx.theme;
    let mut title = vec![Span::raw("Explorer")];
    if is_loading(explorer) {
        title.push(Span::raw(" "));
        title.push(spinner(ctx, theme.key()));
    }
    let block = panel(title, explorer.focused, ctx);

    let items: Vec<TreeItem<String>> = match &explorer.root {
        Load::Loaded(entries) => entries
            .iter()
            .map(|e| entry_item(explorer, e, ctx))
            .collect(),
        Load::Failed(err) => vec![error_item(err, ctx)],
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

/// A database, schema or other container, with what is loaded under it.
fn entry_item<'a>(explorer: &Explorer, entry: &'a Entry, ctx: &RenderCtx) -> TreeItem<'a, String> {
    if entry.is_object() {
        return object_item(entry, ctx);
    }
    let theme = ctx.theme;
    let mut text = match entry.node.kind {
        NodeKind::Database => vec![Span::raw(format!(
            "{} {}",
            ctx.icons.database, entry.node.name
        ))],
        _ => vec![Span::raw(format!("◇ {}", entry.node.name))],
    };
    if entry.node.kind == NodeKind::Database && entry.node.name == explorer.active {
        text.push(Span::styled(" ●", theme.success()));
    } else if let Load::Loaded(children) = &entry.children {
        text.push(Span::styled(format!(" {}", children.len()), theme.dim()));
    }
    let children = match entry.shown() {
        Load::Loaded(children) => child_items(explorer, children, ctx),
        Load::Loading => vec![loading_item(ctx)],
        Load::Failed(err) => vec![error_item(err, ctx)],
        Load::NotLoaded => Vec::new(),
    };
    item(entry.id(), Line::from(text), children)
}

/// Children of a container: other containers as they are, objects grouped
/// by kind.
fn child_items<'a>(
    explorer: &Explorer,
    children: &'a [Entry],
    ctx: &RenderCtx,
) -> Vec<TreeItem<'a, String>> {
    let mut items: Vec<TreeItem<String>> = children
        .iter()
        .filter(|e| !e.is_object())
        .map(|e| entry_item(explorer, e, ctx))
        .collect();
    items.extend(GROUPS.into_iter().filter_map(|kind| {
        let members: Vec<&Entry> = children.iter().filter(|e| e.node.kind == kind).collect();
        if members.is_empty() {
            return None;
        }
        let text = Line::from(vec![
            Span::styled(group_name(kind), ctx.theme.header()),
            Span::styled(format!(" {}", members.len()), ctx.theme.dim()),
        ]);
        let items = members.into_iter().map(|e| object_item(e, ctx)).collect();
        Some(item(group_id(kind), text, items))
    }));
    items
}

fn object_item<'a>(entry: &'a Entry, ctx: &RenderCtx) -> TreeItem<'a, String> {
    let theme = ctx.theme;
    let icon = if entry.node.kind.has_rows() {
        ctx.icons.table
    } else {
        "ƒ"
    };
    let mut text = vec![Span::raw(format!("{icon} {}", entry.node.name))];
    if let Some(detail) = &entry.node.detail {
        text.push(Span::styled(format!(" {detail}"), theme.dim()));
    }
    let children = match &entry.columns {
        Load::Loaded(columns) => columns
            .iter()
            .map(|column| {
                let null = if column.nullable { "" } else { " not null" };
                TreeItem::new_leaf(
                    format!("c/{}", column.name),
                    Line::from(vec![
                        Span::styled(format!("{} ", ctx.icons.column), theme.dim()),
                        Span::raw(column.name.clone()),
                        Span::styled(format!(" {}{null}", column.type_name), theme.dim()),
                    ]),
                )
            })
            .collect(),
        Load::Loading => vec![loading_item(ctx)],
        Load::Failed(err) => vec![error_item(err, ctx)],
        Load::NotLoaded => Vec::new(),
    };
    item(entry.id(), Line::from(text), children)
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

fn group_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Table => "Tables",
        NodeKind::View => "Views",
        NodeKind::MaterializedView => "Materialized views",
        NodeKind::ForeignTable => "Foreign tables",
        NodeKind::Collection => "Collections",
        NodeKind::Function => "Functions",
        NodeKind::Procedure => "Procedures",
        _ => "Other",
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
    let several_parents = explorer
        .matches
        .iter()
        .any(|m| m.parent != explorer.matches[0].parent);
    let items: Vec<ListItem> = explorer
        .matches
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .map(|(i, m)| {
            let name = match (several_parents, m.parent.name()) {
                (true, Some(parent)) => format!("{parent}.{}", m.node.name),
                _ => m.node.name.clone(),
            };
            let icon = if m.node.kind.has_rows() {
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
