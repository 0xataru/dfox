//! Autocomplete: suggestions for the word at the cursor from the catalog,
//! and the popup that lists them.

use dfox_core::models::connections::DbType;
use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::{
    catalog::Catalog,
    component::RenderCtx,
    sql::{
        self,
        complete::{context, TableRef, Want},
        Pos,
    },
    widgets::truncate,
};

/// Suggestions listed at once.
const LIMIT: usize = 50;
/// Rows of the popup.
const SHOWN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Schema,
    Table,
    Column,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub label: String,
    /// What replaces the typed word: the label, quoted where needed.
    pub insert: String,
    pub kind: ItemKind,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Where the typed word starts.
    pub start: Pos,
    pub items: Vec<Item>,
    pub selected: usize,
}

impl Completion {
    pub fn select(&mut self, down: bool) {
        let last = self.items.len().saturating_sub(1);
        self.selected = if down {
            if self.selected == last {
                0
            } else {
                self.selected + 1
            }
        } else if self.selected == 0 {
            last
        } else {
            self.selected - 1
        };
    }

    pub fn chosen(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }
}

/// Suggestions for the cursor. Unless `forced` (`Ctrl+Space`), they only
/// show where the context is clear and something has been typed, or after a
/// dot.
pub fn suggest(
    text: &str,
    cursor: Pos,
    driver: DbType,
    catalog: &Catalog,
    forced: bool,
) -> Option<Completion> {
    let ctx = context(text, cursor, driver);
    let candidates = match &ctx.want {
        Want::Nothing => return None,
        Want::Tables => tables(catalog, None, driver),
        Want::Member(qualifier) => members(catalog, qualifier, &ctx.tables, driver),
        Want::Columns => {
            let columns = columns(catalog, &ctx.tables, driver);
            if columns.is_empty() && forced {
                tables(catalog, None, driver)
            } else {
                columns
            }
        }
        Want::Unknown if forced => {
            let mut all = columns(catalog, &ctx.tables, driver);
            all.extend(tables(catalog, None, driver));
            all
        }
        Want::Unknown => return None,
    };
    if !forced && ctx.prefix.is_empty() && !matches!(ctx.want, Want::Member(_)) {
        return None;
    }
    let items = rank(candidates, &ctx.prefix);
    // Nothing to add when the best suggestion is what was typed.
    let typed_already = items
        .first()
        .is_some_and(|i| i.label.eq_ignore_ascii_case(&ctx.prefix));
    if items.is_empty() || (typed_already && !forced) {
        return None;
    }
    Some(Completion {
        start: ctx.start,
        items,
        selected: 0,
    })
}

/// Schemas and tables. Tables outside the default schema come qualified.
fn tables(catalog: &Catalog, schema: Option<&str>, driver: DbType) -> Vec<Item> {
    let default = catalog.schemas().first().map(String::as_str);
    let mut items = Vec::new();
    for (in_schema, table) in catalog.tables(schema) {
        let qualified = schema.is_none() && Some(in_schema) != default;
        let (label, insert) = if qualified {
            (
                format!("{in_schema}.{table}"),
                format!(
                    "{}.{}",
                    sql::quote_ident(in_schema, driver),
                    sql::quote_ident(table, driver)
                ),
            )
        } else {
            (table.to_string(), sql::quote_ident(table, driver))
        };
        items.push(Item {
            label,
            insert,
            kind: ItemKind::Table,
            detail: in_schema.to_string(),
        });
    }
    if schema.is_none() && catalog.schemas().len() > 1 {
        items.extend(catalog.schemas().iter().map(|s| Item {
            label: s.clone(),
            insert: sql::quote_ident(s, driver),
            kind: ItemKind::Schema,
            detail: "schema".to_string(),
        }));
    }
    items
}

/// After `x.`: the tables of schema `x`, or the columns of table or alias
/// `x`.
fn members(catalog: &Catalog, qualifier: &str, refs: &[TableRef], driver: DbType) -> Vec<Item> {
    let named = refs.iter().find(|r| {
        r.alias
            .as_deref()
            .is_some_and(|a| a.eq_ignore_ascii_case(qualifier))
    });
    let named = named.or_else(|| {
        refs.iter()
            .find(|r| r.alias.is_none() && r.name.eq_ignore_ascii_case(qualifier))
    });
    if let Some(table) = named {
        return columns(catalog, std::slice::from_ref(table), driver);
    }
    if let Some(schema) = catalog.schema(qualifier) {
        return tables(catalog, Some(schema), driver);
    }
    let table = TableRef {
        schema: None,
        name: qualifier.to_string(),
        alias: None,
    };
    columns(catalog, &[table], driver)
}

/// The columns of the statement's tables, each name once.
fn columns(catalog: &Catalog, refs: &[TableRef], driver: DbType) -> Vec<Item> {
    let mut items: Vec<Item> = Vec::new();
    for table in refs {
        for column in catalog.columns(table.schema.as_deref(), &table.name) {
            if items.iter().any(|i| i.label == column.name) {
                continue;
            }
            items.push(Item {
                label: column.name.clone(),
                insert: sql::quote_ident(&column.name, driver),
                kind: ItemKind::Column,
                detail: format!("{} · {}", column.table, column.data_type),
            });
        }
    }
    items
}

/// Matches for `prefix`, best first; ties keep the catalog's order. An
/// empty prefix keeps everything in order.
fn rank(candidates: Vec<Item>, prefix: &str) -> Vec<Item> {
    if prefix.is_empty() {
        return candidates.into_iter().take(LIMIT).collect();
    }
    let pattern = Pattern::parse(prefix, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, bool, usize)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            let score = pattern.score(Utf32Str::new(&item.label, &mut buf), &mut matcher)?;
            let starts = item
                .label
                .to_lowercase()
                .starts_with(&prefix.to_lowercase());
            Some((score, starts, i))
        })
        .collect();
    // Names that start with what was typed come first.
    scored.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)).then(a.2.cmp(&b.2)));
    let mut candidates: Vec<Option<Item>> = candidates.into_iter().map(Some).collect();
    scored
        .into_iter()
        .take(LIMIT)
        .filter_map(|(_, _, i)| candidates[i].take())
        .collect()
}

/// Draws the popup under the cursor at `(x, y)`, or above it when there is
/// no room below.
pub fn render(completion: &Completion, f: &mut Frame, x: u16, y: u16, ctx: &RenderCtx) {
    let theme = ctx.theme;
    let screen = f.area();
    let label_width = completion
        .items
        .iter()
        .map(|i| i.label.chars().count())
        .max()
        .unwrap_or(0)
        .min(32);
    let detail_width = completion
        .items
        .iter()
        .map(|i| i.detail.chars().count())
        .max()
        .unwrap_or(0)
        .min(24);
    let width = ((label_width + detail_width + 6) as u16).min(screen.width);
    let rows = completion.items.len().min(SHOWN);
    let height = rows as u16 + 2;
    let below = y + 1 + height <= screen.bottom();
    let top = if below {
        y + 1
    } else {
        y.saturating_sub(height)
    };
    let left = x
        .saturating_sub(3)
        .min(screen.right().saturating_sub(width));
    let area = Rect::new(left, top, width, height.min(screen.height));

    let first = completion.selected.saturating_sub(SHOWN - 1);
    let lines: Vec<Line> = completion
        .items
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, item)| {
            let icon = match item.kind {
                ItemKind::Schema => "◇",
                ItemKind::Table => ctx.icons.table,
                ItemKind::Column => ctx.icons.column,
            };
            let style = if i == completion.selected {
                theme.highlight()
            } else {
                theme.base().bg(theme.surface)
            };
            Line::from(vec![
                Span::styled(format!("{icon} "), theme.dim()),
                Span::styled(
                    format!("{:<label_width$}", truncate(&item.label, label_width)),
                    style,
                ),
                Span::styled(
                    format!(" {}", truncate(&item.detail, detail_width)),
                    theme.dim(),
                ),
            ])
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::sample_catalog;

    /// Labels suggested at `|`.
    fn labels(text: &str, forced: bool) -> Vec<String> {
        let (line, col) = text
            .split('\n')
            .enumerate()
            .find_map(|(i, l)| l.find('|').map(|c| (i, l[..c].chars().count())))
            .expect("a cursor");
        suggest(
            &text.replace('|', ""),
            (line, col),
            DbType::Postgres,
            &sample_catalog(),
            forced,
        )
        .map(|c| c.items.into_iter().map(|i| i.label).collect())
        .unwrap_or_default()
    }

    #[test]
    fn tables_after_from() {
        // Names starting with what was typed come before other matches.
        assert_eq!(
            labels("select * from or|", false),
            ["orders", "audit.orders", "customers"]
        );
        assert_eq!(labels("select * from cus|", false), ["customers"]);
        // Nothing typed yet: only on request.
        assert!(labels("select * from |", false).is_empty());
        assert_eq!(
            labels("select * from |", true),
            [
                "customers",
                "orders",
                "audit.log",
                "audit.orders",
                "public",
                "audit"
            ]
        );
    }

    #[test]
    fn columns_of_the_statements_tables() {
        assert_eq!(
            labels("select | from orders o join customers c on true", true),
            ["id", "customer_id", "name"]
        );
        assert_eq!(labels("select * from customers where na|", false), ["name"]);
        // Without tables there is nothing to suggest, unless asked.
        assert!(labels("select na|", false).is_empty());
        assert_eq!(labels("select |", true).len(), 6);
    }

    #[test]
    fn members_after_a_dot() {
        assert_eq!(labels("select c.| from customers c", false), ["id", "name"]);
        assert_eq!(
            labels("select orders.| from orders", false),
            ["id", "customer_id"]
        );
        assert_eq!(labels("select * from audit.|", false), ["log", "orders"]);
        assert_eq!(labels("select customers.n|", false), ["name"]);
        assert!(labels("select x.|", false).is_empty());
    }

    #[test]
    fn exact_and_impossible_matches_show_nothing() {
        assert!(labels("select * from customers|", false).is_empty());
        assert!(labels("select * from zzz|", false).is_empty());
        assert!(labels("select 'from |", true).is_empty());
        assert!(labels("select * from customers |", false).is_empty());
        assert_eq!(labels("select * from customers |", true).len(), 8);
    }

    #[test]
    fn names_are_quoted_when_needed() {
        let mut catalog = Catalog::default();
        catalog.on_schemas(&["public".into()]);
        catalog.on_objects(
            "public",
            &[dfox_core::models::schema::DbObject {
                name: "Order Items".into(),
                kind: dfox_core::models::schema::ObjectKind::Table,
                rows: None,
            }],
        );
        let c = suggest(
            "select * from ord",
            (0, 17),
            DbType::Postgres,
            &catalog,
            false,
        )
        .expect("a suggestion");
        assert_eq!(c.items[0].insert, "\"Order Items\"");
        assert_eq!(c.start, (0, 14));
    }

    #[test]
    fn selection_wraps_around() {
        let mut c = Completion {
            start: (0, 0),
            items: rank(tables(&sample_catalog(), None, DbType::Postgres), ""),
            selected: 0,
        };
        c.select(false);
        assert_eq!(c.chosen().map(|i| i.label.as_str()), Some("audit"));
        c.select(true);
        assert_eq!(c.chosen().map(|i| i.label.as_str()), Some("customers"));
    }

    #[test]
    fn suggestions_are_quick_on_a_big_schema() {
        let mut catalog = Catalog::default();
        catalog.on_schemas(&["public".into()]);
        let objects: Vec<_> = (0..2000)
            .map(|i| dfox_core::models::schema::DbObject {
                name: format!("table_{i:04}"),
                kind: dfox_core::models::schema::ObjectKind::Table,
                rows: None,
            })
            .collect();
        catalog.on_objects("public", &objects);
        let start = std::time::Instant::now();
        let c = suggest(
            "select * from tab",
            (0, 17),
            DbType::Postgres,
            &catalog,
            false,
        )
        .expect("suggestions");
        let took = start.elapsed();
        assert_eq!(c.items.len(), LIMIT);
        // 10 ms in a release build; debug builds are several times slower.
        assert!(took < std::time::Duration::from_millis(100), "{took:?}");
    }
}
