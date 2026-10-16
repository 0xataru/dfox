//! What Enter on a table or view offers: its rows in a few ways, a count,
//! SQL scripts for it, its properties, and dropping it. Each item has a
//! letter; Enter picks the highlighted one.

use dfox_core::catalog::{EntityInfo, NodeKind, NodePath};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::{
    action::Action,
    component::RenderCtx,
    sql::{self, Dialect},
    widgets::{centered, truncate},
};

/// Rows the first and last views show.
pub const FEW_ROWS: u64 = 100;

/// A SQL script for a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Select,
    Insert,
    Update,
    Delete,
    Create,
}

/// What was picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    FirstRows,
    LastRows,
    AllRows,
    /// Rows meeting a SQL condition.
    Filtered(String),
    Count,
    Script(Script),
    Definition,
    InsertName,
    Properties,
    Refresh,
    Truncate,
    Drop {
        cascade: bool,
    },
}

impl Choice {
    /// True when it needs the table's columns and keys first.
    pub fn needs_info(&self) -> bool {
        matches!(
            self,
            Choice::LastRows
                | Choice::Properties
                | Choice::Script(Script::Select | Script::Insert | Script::Update | Script::Delete)
        )
    }
}

/// One line of the menu.
#[derive(Debug, Clone)]
struct Item {
    key: char,
    label: &'static str,
    /// Picking it opens the scripts or asks for a condition.
    then: Then,
}

#[derive(Debug, Clone)]
enum Then {
    Pick(Choice),
    Scripts,
    AskCondition,
}

#[derive(Debug, Clone, PartialEq)]
enum Level {
    Main,
    Scripts,
    /// Typing the condition for filtered rows.
    Condition(String),
}

/// What the workspace does with a key the menu got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEvent {
    Close,
    Pick(Choice),
}

#[derive(Debug)]
pub struct TableMenu {
    pub table: NodePath,
    pub kind: NodeKind,
    /// How SQL names it, in parts to quote.
    pub name: Vec<String>,
    level: Level,
    selected: usize,
    items: Vec<Item>,
}

impl TableMenu {
    /// The menu for the table or view at `table`, a `kind`. A read-only
    /// connection offers nothing that writes.
    pub fn new(
        table: NodePath,
        kind: NodeKind,
        name: Vec<String>,
        dialect: Dialect,
        read_only: bool,
    ) -> Self {
        let is_table = kind == NodeKind::Table;
        let pick = |key, label, choice| Item {
            key,
            label,
            then: Then::Pick(choice),
        };
        let mut items = vec![
            pick('f', "First 100 rows", Choice::FirstRows),
            pick('a', "All rows", Choice::AllRows),
            Item {
                key: 'w',
                label: "Filtered rows…",
                then: Then::AskCondition,
            },
            pick('c', "Count rows", Choice::Count),
            Item {
                key: 's',
                label: "Scripts",
                then: Then::Scripts,
            },
            pick('i', "Insert name", Choice::InsertName),
            pick('p', "Properties", Choice::Properties),
            pick('r', "Refresh", Choice::Refresh),
        ];
        if is_table {
            items.insert(1, pick('l', "Last 100 rows", Choice::LastRows));
        } else {
            items.insert(6, pick('e', "Definition", Choice::Definition));
        }
        if !read_only {
            if is_table {
                items.push(pick('t', "Truncate…", Choice::Truncate));
            }
            items.push(pick('d', "Drop…", Choice::Drop { cascade: false }));
            if dialect == Dialect::Postgres {
                items.push(pick('D', "Drop cascade…", Choice::Drop { cascade: true }));
            }
        }
        Self {
            table,
            kind,
            name,
            level: Level::Main,
            selected: 0,
            items,
        }
    }

    fn scripts(&self) -> Vec<Item> {
        let script = |key, label, script| Item {
            key,
            label,
            then: Then::Pick(Choice::Script(script)),
        };
        let mut items = vec![script('s', "SELECT", Script::Select)];
        if self.kind == NodeKind::Table {
            items.extend([
                script('i', "INSERT", Script::Insert),
                script('u', "UPDATE", Script::Update),
                script('d', "DELETE", Script::Delete),
            ]);
        }
        items.push(script('c', "CREATE", Script::Create));
        items
    }

    fn shown(&self) -> Vec<Item> {
        match self.level {
            Level::Scripts => self.scripts(),
            _ => self.items.clone(),
        }
    }

    pub fn handle(&mut self, action: Action) -> Option<MenuEvent> {
        if let Level::Condition(typed) = &mut self.level {
            match action {
                Action::Input(c) => typed.push(c),
                Action::Backspace => {
                    typed.pop();
                }
                Action::Confirm if !typed.trim().is_empty() => {
                    let condition = typed.trim().to_string();
                    return Some(MenuEvent::Pick(Choice::Filtered(condition)));
                }
                Action::Back => self.back(),
                _ => {}
            }
            return None;
        }
        let items = self.shown();
        match action {
            Action::Up => self.selected = (self.selected + items.len() - 1) % items.len(),
            Action::Down => self.selected = (self.selected + 1) % items.len(),
            Action::Home => self.selected = 0,
            Action::End => self.selected = items.len() - 1,
            Action::Confirm | Action::Right => {
                return self.choose(items[self.selected].clone());
            }
            Action::Left if self.level != Level::Main => self.back(),
            Action::Back if self.level != Level::Main => self.back(),
            Action::Back => return Some(MenuEvent::Close),
            Action::Input(c) => {
                if let Some(item) = items.into_iter().find(|i| i.key == c) {
                    return self.choose(item);
                }
            }
            _ => {}
        }
        None
    }

    fn choose(&mut self, item: Item) -> Option<MenuEvent> {
        match item.then {
            Then::Pick(choice) => return Some(MenuEvent::Pick(choice)),
            Then::Scripts => self.level = Level::Scripts,
            Then::AskCondition => self.level = Level::Condition(String::new()),
        }
        self.selected = 0;
        None
    }

    /// Back to the main list, on the item that led away from it.
    fn back(&mut self) {
        let key = match self.level {
            Level::Scripts => 's',
            _ => 'w',
        };
        self.level = Level::Main;
        self.selected = self.items.iter().position(|i| i.key == key).unwrap_or(0);
    }

    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let name = self.table.name().unwrap_or_default();
        let (title, lines) = match &self.level {
            Level::Condition(typed) => {
                let lines = vec![
                    Line::styled(format!("SELECT * FROM {name} WHERE"), theme.dim()),
                    Line::from(vec![
                        Span::styled(format!("{} ", ctx.icons.selected), theme.key()),
                        Span::raw(typed.clone()),
                    ]),
                    Line::raw(""),
                    Line::from(vec![
                        Span::styled("Enter", theme.key()),
                        Span::styled(" run   ", theme.dim()),
                        Span::styled("Esc", theme.key()),
                        Span::styled(" back", theme.dim()),
                    ]),
                ];
                (format!("{name} · filter"), lines)
            }
            level => {
                let lines = self
                    .shown()
                    .iter()
                    .enumerate()
                    .map(|(i, item)| {
                        let style = if i == self.selected {
                            theme.highlight()
                        } else {
                            theme.base()
                        };
                        let more = matches!(item.then, Then::Scripts);
                        let label = format!("{:<18}{}", item.label, if more { "▸" } else { " " });
                        Line::from(vec![
                            Span::styled(format!(" {} ", item.key), theme.key()),
                            Span::styled(format!(" {label} "), style),
                        ])
                    })
                    .collect();
                let title = match level {
                    Level::Scripts => format!("{name} · scripts"),
                    _ => name.to_string(),
                };
                (title, lines)
            }
        };
        let (width, padding) = match self.level {
            Level::Condition(_) => (48, 1),
            _ => (28, 0),
        };
        let popup = centered(area, width, lines.len() as u16 + 2);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(
                format!(" {} ", truncate(&title, width as usize - 4)),
                theme.title(true),
            ))
            .padding(ratatui::widgets::Padding::horizontal(padding))
            .style(theme.surface());
        f.render_widget(Clear, popup);
        f.render_widget(Paragraph::new(lines).block(block), popup);
        if let Level::Condition(typed) = &self.level {
            let room = width.saturating_sub(7);
            let x = popup.x + 4 + (typed.chars().count() as u16).min(room);
            f.set_cursor_position((x, popup.y + 2));
        }
    }
}

/// A script for the table named `table` (already SQL) with `info`'s
/// columns. `?` marks where values go.
pub fn script(script: Script, table: &str, info: &EntityInfo, dialect: Dialect) -> String {
    let quote = |name: &str| sql::quote_ident(name, dialect);
    let columns: Vec<String> = info.columns.iter().map(|c| quote(&c.name)).collect();
    // Generated columns take no values.
    let writable: Vec<String> = info
        .columns
        .iter()
        .filter(|c| c.generated.is_none())
        .map(|c| quote(&c.name))
        .collect();
    let key: Vec<String> = info
        .columns
        .iter()
        .filter(|c| c.primary_key)
        .map(|c| quote(&c.name))
        .collect();
    let condition = if key.is_empty() {
        "<condition>".to_string()
    } else {
        key.iter()
            .map(|k| format!("{k} = ?"))
            .collect::<Vec<_>>()
            .join("\n  AND ")
    };
    let list = |names: &[String]| names.join(",\n    ");
    match script {
        Script::Select => format!("SELECT\n    {}\nFROM {table};\n", list(&columns)),
        Script::Insert => format!(
            "INSERT INTO {table} (\n    {}\n) VALUES (\n    {}\n);\n",
            list(&writable),
            vec!["?"; writable.len()].join(",\n    ")
        ),
        Script::Update => {
            let set: Vec<String> = writable
                .iter()
                .filter(|c| !key.contains(c))
                .map(|c| format!("{c} = ?"))
                .collect();
            format!(
                "UPDATE {table}\nSET {}\nWHERE {condition};\n",
                set.join(",\n    ")
            )
        }
        Script::Delete => format!("DELETE FROM {table}\nWHERE {condition};\n"),
        // The database writes that one.
        Script::Create => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use dfox_core::catalog::ColumnInfo;

    use super::*;
    use crate::testing::{render, TestCtx};

    fn menu(kind: NodeKind, read_only: bool) -> TableMenu {
        TableMenu::new(
            ["shop", "public", "orders"].into_iter().collect(),
            kind,
            vec!["orders".into()],
            Dialect::Postgres,
            read_only,
        )
    }

    fn snapshot(menu: &TableMenu) -> String {
        let test = TestCtx::new();
        render(&test, 50, 18, |f, area, ctx| menu.render(f, area, ctx))
    }

    #[test]
    fn a_table_offers_everything() {
        let mut m = menu(NodeKind::Table, false);
        insta::assert_snapshot!(snapshot(&m));
        assert_eq!(
            m.handle(Action::Confirm),
            Some(MenuEvent::Pick(Choice::FirstRows))
        );
        m.handle(Action::Down);
        assert_eq!(
            m.handle(Action::Confirm),
            Some(MenuEvent::Pick(Choice::LastRows))
        );
        assert_eq!(
            m.handle(Action::Input('c')),
            Some(MenuEvent::Pick(Choice::Count))
        );
        assert_eq!(
            m.handle(Action::Input('D')),
            Some(MenuEvent::Pick(Choice::Drop { cascade: true }))
        );
        // Wraps around.
        m.handle(Action::Home);
        m.handle(Action::Up);
        assert_eq!(
            m.handle(Action::Confirm),
            Some(MenuEvent::Pick(Choice::Drop { cascade: true }))
        );
        assert_eq!(m.handle(Action::Back), Some(MenuEvent::Close));
    }

    #[test]
    fn scripts_open_a_second_list() {
        let mut m = menu(NodeKind::Table, false);
        assert_eq!(m.handle(Action::Input('s')), None);
        insta::assert_snapshot!("scripts", snapshot(&m));
        assert_eq!(
            m.handle(Action::Input('u')),
            Some(MenuEvent::Pick(Choice::Script(Script::Update)))
        );
        m.handle(Action::Back);
        assert_eq!(
            m.handle(Action::Confirm),
            None,
            "back on Scripts, which opens again"
        );
        m.handle(Action::Left);
        assert_eq!(m.handle(Action::Back), Some(MenuEvent::Close));
    }

    #[test]
    fn filtered_rows_ask_for_a_condition() {
        let mut m = menu(NodeKind::Table, false);
        m.handle(Action::Input('w'));
        for c in "total > 10x".chars() {
            m.handle(Action::Input(c));
        }
        m.handle(Action::Backspace);
        insta::assert_snapshot!("condition", snapshot(&m));
        assert_eq!(
            m.handle(Action::Confirm),
            Some(MenuEvent::Pick(Choice::Filtered("total > 10".into())))
        );
        m.handle(Action::Back);
        assert_eq!(m.handle(Action::Back), Some(MenuEvent::Close));
    }

    #[test]
    fn views_and_read_only_connections_offer_less() {
        let m = menu(NodeKind::View, true);
        insta::assert_snapshot!("view_read_only", snapshot(&m));
        let mut m = menu(NodeKind::View, false);
        assert_eq!(m.handle(Action::Input('t')), None);
        assert_eq!(m.handle(Action::Input('l')), None);
        assert_eq!(
            m.handle(Action::Input('e')),
            Some(MenuEvent::Pick(Choice::Definition))
        );
        m.handle(Action::Input('s'));
        assert_eq!(m.handle(Action::Input('i')), None, "no INSERT into a view");
    }

    #[test]
    fn scripts_use_the_columns_and_key() {
        let info = EntityInfo {
            columns: vec![
                ColumnInfo {
                    name: "id".into(),
                    primary_key: true,
                    ..ColumnInfo::default()
                },
                ColumnInfo {
                    name: "order".into(),
                    ..ColumnInfo::default()
                },
                ColumnInfo {
                    name: "doubled".into(),
                    generated: Some("total * 2".into()),
                    ..ColumnInfo::default()
                },
            ],
            ..EntityInfo::default()
        };
        let pg = Dialect::Postgres;
        assert_eq!(
            script(Script::Select, "orders", &info, pg),
            "SELECT\n    id,\n    \"order\",\n    doubled\nFROM orders;\n"
        );
        assert_eq!(
            script(Script::Insert, "orders", &info, pg),
            "INSERT INTO orders (\n    id,\n    \"order\"\n) VALUES (\n    ?,\n    ?\n);\n"
        );
        assert_eq!(
            script(Script::Update, "orders", &info, Dialect::MySql),
            "UPDATE orders\nSET `order` = ?\nWHERE id = ?;\n"
        );
        assert_eq!(
            script(Script::Delete, "orders", &info, pg),
            "DELETE FROM orders\nWHERE id = ?;\n"
        );
        let keyless = EntityInfo {
            columns: vec![ColumnInfo {
                name: "x".into(),
                ..ColumnInfo::default()
            }],
            ..EntityInfo::default()
        };
        assert_eq!(
            script(Script::Delete, "t", &keyless, pg),
            "DELETE FROM t\nWHERE <condition>;\n"
        );
    }
}
