//! Cards over the workspace: a column's, with everything known about it —
//! its type, keys, indexes and comment — and what its values look like, from
//! the planner's statistics or a quick profile of the first rows; and a
//! table's properties at a glance.

use dfox_core::{
    catalog::{
        ColumnInfo, ColumnStats, Distinct, EntityInfo, ForeignKey, NodeKind, NodePath, PROFILE_ROWS,
    },
    engine::Command as DbCommand,
    errors::DbError,
};
use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use super::explorer::Load;
use crate::{
    action::Action,
    component::{Ctx, RenderCtx},
    widgets::{centered, truncate},
};

/// Columns of the card, borders included.
const WIDTH: u16 = 64;
/// Columns for a property's name.
const LABEL: usize = 11;
/// Most common values shown.
const COMMON: usize = 5;
/// Columns for a common value before its bar.
const VALUE: usize = 16;
/// Columns of the longest bar.
const BAR: usize = 20;

/// What the card asks of the workspace.
#[derive(Debug, Clone, PartialEq)]
pub enum CardEvent {
    Close,
    /// Go to the table a foreign key points at.
    Reveal(NodePath),
}

#[derive(Debug)]
pub struct ColumnCard {
    table: NodePath,
    column: ColumnInfo,
    info: EntityInfo,
    stats: Load<Option<ColumnStats>>,
    profile: Load<ColumnStats>,
}

impl ColumnCard {
    /// A card for `column` of the table at `table`, described by `info`; it
    /// asks for the planner's statistics. `None` when `info` has no such
    /// column.
    pub fn open(table: NodePath, column: &str, info: &EntityInfo, ctx: &mut Ctx) -> Option<Self> {
        let column = info.columns.iter().find(|c| c.name == column)?.clone();
        ctx.engine.send(DbCommand::ColumnStats {
            path: table.clone(),
            column: column.name.clone(),
            profile: false,
        });
        Some(Self {
            table,
            column,
            info: info.clone(),
            stats: Load::Loading,
            profile: Load::NotLoaded,
        })
    }

    /// Statistics arrived, for this card or an earlier one.
    pub fn on_stats(
        &mut self,
        table: &NodePath,
        column: &str,
        profile: bool,
        result: Result<Option<ColumnStats>, DbError>,
    ) {
        if *table != self.table || column != self.column.name {
            return;
        }
        if profile {
            self.profile = match result {
                Ok(Some(stats)) => Load::Loaded(stats),
                Ok(None) => Load::Failed("the database can't profile it".into()),
                Err(err) => Load::Failed(err.to_string()),
            };
        } else {
            self.stats = result.map_or_else(|err| Load::Failed(err.to_string()), Load::Loaded);
        }
    }

    pub fn handle(&mut self, action: Action, ctx: &mut Ctx) -> Option<CardEvent> {
        match action {
            Action::Back => return Some(CardEvent::Close),
            Action::Confirm => {
                return self
                    .reference()
                    .map(|(key, _)| CardEvent::Reveal(key.target.clone()))
            }
            Action::Input('p') | Action::Execute if !matches!(self.profile, Load::Loading) => {
                self.profile = Load::Loading;
                ctx.engine.send(DbCommand::ColumnStats {
                    path: self.table.clone(),
                    column: self.column.name.clone(),
                    profile: true,
                });
            }
            _ => {}
        }
        None
    }

    pub fn is_loading(&self) -> bool {
        matches!(self.stats, Load::Loading) || matches!(self.profile, Load::Loading)
    }

    /// The foreign key the column is part of, and the column it matches.
    fn reference(&self) -> Option<(&ForeignKey, &str)> {
        self.info.foreign_keys.iter().find_map(|key| {
            let at = key.columns.iter().position(|c| *c == self.column.name)?;
            Some((key, key.target_columns.get(at)?.as_str()))
        })
    }

    pub fn lines(&self, width: usize, ctx: &RenderCtx) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let column = &self.column;
        let room = width.saturating_sub(LABEL + 1).max(8);
        let mut out = Vec::new();
        let mut row = |label: &str, value: Vec<Span<'static>>| {
            let mut spans = vec![Span::styled(format!("{label:<LABEL$} "), theme.dim())];
            spans.extend(value);
            out.push(Line::from(spans));
        };
        let text = |value: &str| vec![Span::raw(truncate(value, room))];

        row("Type", text(&column.type_name));
        row(
            "Null",
            text(if column.nullable {
                "nullable"
            } else {
                "not null"
            }),
        );
        if let Some(default) = &column.default {
            row("Default", text(default));
        }
        if let Some(generated) = &column.generated {
            row("Generated", text(generated));
        }
        let mut keys = Vec::new();
        if column.primary_key {
            keys.push("primary key");
        }
        let alone = |columns: &[String]| columns.len() == 1 && columns[0] == column.name;
        if !column.primary_key
            && self
                .info
                .indexes
                .iter()
                .any(|i| i.unique && alone(&i.columns))
        {
            keys.push("unique");
        }
        if !keys.is_empty() {
            row("Key", text(&keys.join(" · ")));
        }
        if let Some((key, target)) = self.reference() {
            let table = key.target.name().unwrap_or_default();
            row("References", text(&format!("{table}.{target}")));
        }
        let indexes: Vec<&str> = self
            .info
            .indexes
            .iter()
            .filter(|i| i.columns.contains(&column.name))
            .map(|i| i.name.as_str())
            .collect();
        if !indexes.is_empty() {
            row("Indexes", text(&indexes.join(", ")));
        }
        if let Some(collation) = &column.collation {
            row("Collation", text(collation));
        }
        if let Some(comment) = &column.comment {
            row("Comment", text(comment));
        }

        out.push(Line::raw(""));
        match (&self.profile, &self.stats) {
            (Load::Loaded(stats), _) | (_, Load::Loaded(Some(stats))) => {
                out.extend(self.stats_lines(stats, width, ctx));
            }
            (Load::Loading, _) | (_, Load::Loading) => {
                out.push(Line::styled("Counting…", theme.dim()));
            }
            (Load::Failed(err), _) | (_, Load::Failed(err)) => {
                out.push(Line::styled(truncate(err, width), theme.danger()));
            }
            (Load::NotLoaded, _) => {
                let rows = short(PROFILE_ROWS as f64);
                out.push(Line::styled(
                    format!("No planner statistics yet: p counts the first {rows} rows."),
                    theme.dim(),
                ));
            }
        }

        out.push(Line::raw(""));
        let mut keys = Vec::new();
        let mut key = |key: &str, what: String| {
            keys.push(Span::styled(format!(" {key} "), theme.key()));
            keys.push(Span::styled(format!(" {what}   "), theme.dim()));
        };
        if let Some((target, _)) = self.reference() {
            let table = target.target.name().unwrap_or_default();
            key("Enter", format!("go to {table}"));
        }
        key("p", "profile".into());
        key("Esc", "close".into());
        out.push(Line::from(keys));
        out
    }

    fn stats_lines(
        &self,
        stats: &ColumnStats,
        width: usize,
        ctx: &RenderCtx,
    ) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let mut out = Vec::new();
        let source = match stats.rows {
            Some(rows) => format!("  counted in {rows} rows"),
            None => "  from the planner".to_string(),
        };
        out.push(Line::from(vec![
            Span::styled("Values", theme.header()),
            Span::styled(source, theme.dim()),
        ]));
        let label = |name: &str| Span::styled(format!("{name:<LABEL$} "), theme.dim());
        if let Some(nulls) = stats.null_fraction {
            out.push(Line::from(vec![label("Nulls"), Span::raw(percent(nulls))]));
        }
        if let Some(distinct) = stats.distinct {
            let profiled = stats.rows.is_some();
            let text = match distinct {
                Distinct::Count(n) if profiled => format!("{n}"),
                Distinct::Count(n) => format!("≈ {}", short(n)),
                Distinct::Fraction(f) => match self.info.row_estimate {
                    Some(rows) => format!("{} of rows, ≈ {}", percent(f), short(f * rows as f64)),
                    None => format!("{} of rows", percent(f)),
                },
            };
            out.push(Line::from(vec![label("Distinct"), Span::raw(text)]));
        }
        if let (Some(min), Some(max)) = (&stats.min, &stats.max) {
            let room = width.saturating_sub(LABEL + 4) / 2;
            let range = format!("{} … {}", truncate(min, room), truncate(max, room));
            out.push(Line::from(vec![label("Range"), Span::raw(range)]));
        }
        let most = stats.common.first().map_or(0.0, |(_, f)| *f);
        for (i, (value, fraction)) in stats.common.iter().take(COMMON).enumerate() {
            let name = if i == 0 { "Common" } else { "" };
            out.push(Line::from(vec![
                label(name),
                Span::raw(format!("{:<VALUE$} ", truncate(value, VALUE))),
                Span::styled(bar(*fraction / most, BAR), theme.key()),
                Span::styled(format!(" {}", percent(*fraction)), theme.dim()),
            ]));
        }
        if let (Some(first), Some(last)) = (stats.histogram.first(), stats.histogram.last()) {
            let room = width.saturating_sub(LABEL + 1);
            let spark = sparkline(&stats.histogram, room.min(32));
            let ends = format!(" {} … {}", first.low, last.high);
            out.push(Line::from(vec![
                label("Spread"),
                Span::styled(spark.clone(), theme.key()),
                Span::styled(
                    truncate(&ends, room.saturating_sub(spark.chars().count())),
                    theme.dim(),
                ),
            ]));
        }
        out
    }

    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let width = WIDTH.min(area.width);
        let lines = self.lines(width.saturating_sub(4) as usize, ctx);
        let popup = centered(area, width, lines.len() as u16 + 2);
        let table = self.table.name().unwrap_or_default();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(
                format!(" {table}.{} ", self.column.name),
                theme.title(true),
            ))
            .padding(ratatui::widgets::Padding::horizontal(1))
            .style(theme.surface());
        f.render_widget(Clear, popup);
        f.render_widget(Paragraph::new(lines).block(block), popup);
    }
}

/// A table's properties: what it is made of, at a glance.
#[derive(Debug)]
pub struct TableCard {
    table: NodePath,
    kind: NodeKind,
    info: EntityInfo,
}

impl TableCard {
    pub fn new(table: NodePath, kind: NodeKind, info: EntityInfo) -> Self {
        Self { table, kind, info }
    }

    /// Any key that means "close" closes it.
    pub fn closes(action: Action) -> bool {
        matches!(action, Action::Back | Action::Confirm)
    }

    pub fn lines(&self, width: usize, ctx: &RenderCtx) -> Vec<Line<'static>> {
        let theme = ctx.theme;
        let info = &self.info;
        let room = width.saturating_sub(LABEL + 1).max(8);
        let mut out = Vec::new();
        let mut row = |label: &str, value: String| {
            out.push(Line::from(vec![
                Span::styled(format!("{label:<LABEL$} "), theme.dim()),
                Span::raw(truncate(&value, room)),
            ]));
        };
        row("Kind", self.kind.label().to_string());
        if let Some(rows) = info.row_estimate {
            row("Rows", format!("≈ {}", short(rows as f64)));
        }
        row("Columns", info.columns.len().to_string());
        let key: Vec<&str> = info
            .columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.as_str())
            .collect();
        if !key.is_empty() {
            row("Key", key.join(", "));
        }
        for (i, fk) in info.foreign_keys.iter().enumerate() {
            let target = fk.target.name().unwrap_or_default();
            let text = format!(
                "{} → {target}.{}",
                fk.columns.join(", "),
                fk.target_columns.join(", ")
            );
            row(if i == 0 { "References" } else { "" }, text);
        }
        let count = |n: usize| (n > 0).then(|| n.to_string());
        for (label, n) in [
            ("Constraints", count(info.constraints.len())),
            ("Indexes", count(info.indexes.len())),
            ("Triggers", count(info.triggers.len())),
            ("Policies", count(info.policies.len())),
        ] {
            if let Some(n) = n {
                row(label, n);
            }
        }
        if let Some(comment) = &info.comment {
            row("Comment", comment.clone());
        }
        out.push(Line::raw(""));
        out.push(Line::from(vec![
            Span::styled(" Esc ", theme.key()),
            Span::styled(" close", theme.dim()),
        ]));
        out
    }

    pub fn render(&self, f: &mut Frame, area: Rect, ctx: &RenderCtx) {
        let theme = ctx.theme;
        let width = WIDTH.min(area.width);
        let lines = self.lines(width.saturating_sub(4) as usize, ctx);
        let popup = centered(area, width, lines.len() as u16 + 2);
        let name = self.table.name().unwrap_or_default();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border(true))
            .title(Span::styled(format!(" {name} "), theme.title(true)))
            .padding(ratatui::widgets::Padding::horizontal(1))
            .style(theme.surface());
        f.render_widget(Clear, popup);
        f.render_widget(Paragraph::new(lines).block(block), popup);
    }
}

/// `12.5%`, or `<0.1%` for a sliver.
fn percent(fraction: f64) -> String {
    match fraction * 100.0 {
        p if p > 0.0 && p < 0.1 => "<0.1%".to_string(),
        p => format!("{p:.1}%"),
    }
}

/// `950`, `12.4k`, `100k`, `3.1M`.
fn short(n: f64) -> String {
    let (n, unit) = match n {
        n if n < 1000.0 => return format!("{}", n.round()),
        n if n < 1_000_000.0 => (n / 1000.0, "k"),
        n => (n / 1_000_000.0, "M"),
    };
    let text = format!("{n:.1}");
    format!("{}{unit}", text.trim_end_matches(".0"))
}

/// A bar `share` of `width` long, in eighths.
fn bar(share: f64, width: usize) -> String {
    const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let eighths = (share.clamp(0.0, 1.0) * width as f64 * 8.0).round() as usize;
    let mut out = "█".repeat(eighths / 8);
    out.push_str(EIGHTHS[eighths % 8]);
    if out.is_empty() {
        out.push('▏');
    }
    out
}

/// How densely values fill each range, in at most `width` columns: rows
/// per unit of value when the bounds are numbers, rows per range when not.
fn sparkline(buckets: &[dfox_core::catalog::Bucket], width: usize) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if buckets.is_empty() || width == 0 {
        return String::new();
    }
    // Neighbouring ranges merge until they fit.
    let per = buckets.len().div_ceil(width);
    let densities: Vec<f64> = buckets
        .chunks(per)
        .map(|chunk| {
            let fraction: f64 = chunk.iter().map(|b| b.fraction).sum();
            let low = chunk[0].low.parse::<f64>();
            let high = chunk[chunk.len() - 1].high.parse::<f64>();
            match (low, high) {
                (Ok(low), Ok(high)) if high > low => fraction / (high - low),
                _ => fraction,
            }
        })
        .collect();
    let top = densities.iter().copied().fold(0.0, f64::max);
    densities
        .iter()
        .map(|d| {
            let level = if top > 0.0 { d / top * 7.0 } else { 0.0 };
            LEVELS[level.round() as usize]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use dfox_core::catalog::{Bucket, IndexInfo};

    use super::*;
    use crate::testing::{render, Harness, TestCtx};

    fn orders() -> EntityInfo {
        let column = |name: &str, type_name: &str| ColumnInfo {
            name: name.into(),
            type_name: type_name.into(),
            nullable: true,
            ..ColumnInfo::default()
        };
        EntityInfo {
            columns: vec![
                ColumnInfo {
                    nullable: false,
                    primary_key: true,
                    default: Some("nextval('orders_id_seq'::regclass)".into()),
                    ..column("id", "integer")
                },
                ColumnInfo {
                    comment: Some("who placed it".into()),
                    ..column("customer", "integer")
                },
                ColumnInfo {
                    collation: Some("C".into()),
                    ..column("code", "text")
                },
            ],
            indexes: vec![
                IndexInfo {
                    name: "orders_pkey".into(),
                    columns: vec!["id".into()],
                    unique: true,
                    ..IndexInfo::default()
                },
                IndexInfo {
                    name: "orders_code_key".into(),
                    columns: vec!["code".into()],
                    unique: true,
                    ..IndexInfo::default()
                },
                IndexInfo {
                    name: "orders_customer_idx".into(),
                    columns: vec!["customer".into(), "code".into()],
                    unique: false,
                    ..IndexInfo::default()
                },
            ],
            row_estimate: Some(12_400),
            foreign_keys: vec![ForeignKey {
                name: "orders_customer_fkey".into(),
                columns: vec!["customer".into()],
                target: ["shop", "public", "customers"].into_iter().collect(),
                target_columns: vec!["id".into()],
            }],
            comment: None,
            ..EntityInfo::default()
        }
    }

    fn table() -> NodePath {
        ["shop", "public", "orders"].into_iter().collect()
    }

    fn snapshot(card: &ColumnCard) -> String {
        let test = TestCtx::new();
        render(&test, 70, 24, |f, area, ctx| card.render(f, area, ctx))
    }

    #[test]
    fn a_column_with_a_foreign_key_and_planner_statistics() {
        let mut harness = Harness::new();
        let mut card =
            ColumnCard::open(table(), "customer", &orders(), &mut harness.ctx()).expect("card");
        assert_eq!(
            harness.sent(),
            [DbCommand::ColumnStats {
                path: table(),
                column: "customer".into(),
                profile: false,
            }]
        );
        let histogram = (0..10)
            .map(|i| Bucket {
                low: (i * 10).to_string(),
                high: (i * 10 + if i < 5 { 10 } else { 40 }).to_string(),
                fraction: 0.08,
            })
            .collect();
        card.on_stats(
            &table(),
            "customer",
            false,
            Ok(Some(ColumnStats {
                null_fraction: Some(0.02),
                distinct: Some(Distinct::Fraction(0.1)),
                common: vec![("7".into(), 0.12), ("42".into(), 0.06)],
                histogram,
                ..ColumnStats::default()
            })),
        );
        insta::assert_snapshot!(snapshot(&card));

        assert_eq!(
            card.handle(Action::Confirm, &mut harness.ctx()),
            Some(CardEvent::Reveal(
                ["shop", "public", "customers"].into_iter().collect()
            ))
        );
        assert_eq!(
            card.handle(Action::Back, &mut harness.ctx()),
            Some(CardEvent::Close)
        );
    }

    #[test]
    fn a_column_without_statistics_can_be_profiled() {
        let mut harness = Harness::new();
        let mut card =
            ColumnCard::open(table(), "code", &orders(), &mut harness.ctx()).expect("card");
        harness.sent();
        card.on_stats(&table(), "code", false, Ok(None));
        insta::assert_snapshot!("no_statistics", snapshot(&card));
        assert_eq!(card.handle(Action::Confirm, &mut harness.ctx()), None);

        card.handle(Action::Input('p'), &mut harness.ctx());
        assert_eq!(
            harness.sent(),
            [DbCommand::ColumnStats {
                path: table(),
                column: "code".into(),
                profile: true,
            }]
        );
        assert!(card.is_loading());
        // Another column's answer is not for this card.
        card.on_stats(&table(), "id", true, Ok(None));
        assert!(card.is_loading());
        card.on_stats(
            &table(),
            "code",
            true,
            Ok(Some(ColumnStats {
                source: dfox_core::catalog::StatsSource::Profile,
                rows: Some(800),
                null_fraction: Some(0.0),
                distinct: Some(Distinct::Count(800.0)),
                min: Some("A-0001".into()),
                max: Some("Z-0999".into()),
                ..ColumnStats::default()
            })),
        );
        insta::assert_snapshot!("profiled", snapshot(&card));
    }

    #[test]
    fn unknown_columns_have_no_card() {
        let mut harness = Harness::new();
        assert!(ColumnCard::open(table(), "nope", &orders(), &mut harness.ctx()).is_none());
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!(percent(0.125), "12.5%");
        assert_eq!(percent(0.0001), "<0.1%");
        assert_eq!(percent(0.0), "0.0%");
        assert_eq!(short(950.0), "950");
        assert_eq!(short(12_400.0), "12.4k");
        assert_eq!(short(100_000.0), "100k");
        assert_eq!(bar(1.0, 2), "██");
        assert_eq!(bar(0.5, 3), "█▌");
        assert_eq!(bar(0.0, 3), "▏");
        let bucket = |low: &str, high: &str| Bucket {
            low: low.into(),
            high: high.into(),
            fraction: 0.25,
        };
        // Narrow ranges hold the same rows more densely.
        let spark = sparkline(
            &[
                bucket("0", "1"),
                bucket("1", "2"),
                bucket("2", "10"),
                bucket("10", "50"),
            ],
            4,
        );
        assert_eq!(spark, "██▂▁");
        assert_eq!(sparkline(&[bucket("a", "b"), bucket("b", "c")], 1), "█");
    }
}
