//! The new layout's Graph: the result's numbers as lines or bars.

use dfox_core::result::{Cell, ResultSet};
use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Color, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Bar, BarChart, BarGroup, Chart, Dataset, GraphType, Paragraph},
    Frame,
};

use crate::component::RenderCtx;

/// Rows plotted.
// shortcut: only the first rows, add paging or sampling when someone needs more.
const MOST: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Line,
    Bar,
}

/// What is plotted: X values with their labels, and each numeric column.
#[derive(Debug, PartialEq)]
pub struct Plot {
    pub x: String,
    pub labels: Vec<String>,
    pub xs: Vec<f64>,
    pub series: Vec<(String, Vec<Option<f64>>)>,
}

fn number(cell: Cell) -> Option<f64> {
    match cell {
        Cell::Int(v) => Some(v as f64),
        Cell::UInt(v) => Some(v as f64),
        Cell::Float(v) => Some(v),
        Cell::Decimal(v) => v.parse().ok(),
        _ => None,
    }
}

/// The X axis is the first column when it and another are numbers, the row
/// number otherwise, labelled with the first column. Every other numeric
/// column is a series.
pub fn plot(result: &impl ResultSet) -> Option<Plot> {
    let rows = result.row_count().min(MOST);
    let columns = result.columns();
    let numeric = |col: usize| {
        let cells = (0..rows).filter_map(|r| result.cell(r, col));
        let mut any = false;
        for cell in cells {
            match cell {
                Cell::Null => {}
                cell if cell.is_numeric() => any = true,
                _ => return false,
            }
        }
        any
    };
    let numbers: Vec<usize> = (0..columns.len()).filter(|&c| numeric(c)).collect();
    let first_is_number = numbers.first() == Some(&0);
    let numeric_x = first_is_number && numbers.len() >= 2;
    let ys = if numeric_x {
        &numbers[1..]
    } else {
        &numbers[..]
    };
    if ys.is_empty() {
        return None;
    }
    let by_row = first_is_number && !numeric_x;
    let labels = (0..rows)
        .map(|r| match by_row {
            true => (r + 1).to_string(),
            false => result.cell(r, 0).map(|v| v.to_string()).unwrap_or_default(),
        })
        .collect();
    let xs = (0..rows)
        .map(|r| match numeric_x {
            true => result.cell(r, 0).and_then(number).unwrap_or(r as f64),
            false => (r + 1) as f64,
        })
        .collect();
    let x = match by_row {
        true => "row".to_string(),
        false => columns[0].name.clone(),
    };
    let series = ys
        .iter()
        .map(|&c| {
            let values = (0..rows)
                .map(|r| result.cell(r, c).and_then(number))
                .collect();
            (columns[c].name.clone(), values)
        })
        .collect();
    Some(Plot {
        x,
        labels,
        xs,
        series,
    })
}

fn short(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        format!("{v:.2}")
    }
}

pub fn render(f: &mut Frame, area: Rect, result: &impl ResultSet, kind: Kind, ctx: &RenderCtx) {
    let theme = ctx.theme;
    let Some(plot) = plot(result) else {
        let [line] = Layout::vertical([Constraint::Length(1)])
            .flex(Flex::Center)
            .areas(area);
        let text = Line::from(vec![
            Span::styled(format!("{} ", ctx.icons.info), theme.key()),
            Span::styled(
                "No numbers to plot. Run a query with a numeric column.",
                theme.dim(),
            ),
        ]);
        f.render_widget(Paragraph::new(text).centered(), line);
        return;
    };
    let colors = [
        theme.accent,
        theme.success,
        theme.warning,
        theme.accent_alt,
        theme.keyword,
        theme.danger,
    ];
    let color = |i: usize| colors[i % colors.len()];
    match kind {
        Kind::Line => line(f, area, &plot, &color, ctx),
        Kind::Bar => bars(f, area, &plot, &color, ctx),
    }
}

fn line(f: &mut Frame, area: Rect, plot: &Plot, color: &dyn Fn(usize) -> Color, ctx: &RenderCtx) {
    let points: Vec<Vec<(f64, f64)>> = plot
        .series
        .iter()
        .map(|(_, values)| {
            plot.xs
                .iter()
                .zip(values)
                .filter_map(|(x, y)| y.map(|y| (*x, y)))
                .collect()
        })
        .collect();
    let all = || points.iter().flatten();
    let bounds = |pick: fn(&(f64, f64)) -> f64| {
        let (lo, hi) = all()
            .map(pick)
            .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
        if lo > hi {
            [0.0, 1.0]
        } else if lo == hi {
            [lo - 1.0, hi + 1.0]
        } else {
            [lo, hi]
        }
    };
    let (xb, yb) = (bounds(|p| p.0), bounds(|p| p.1));
    let datasets = plot
        .series
        .iter()
        .zip(&points)
        .enumerate()
        .map(|(i, ((name, _), points))| {
            Dataset::default()
                .name(name.clone())
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(color(i)))
                .data(points)
        })
        .collect();
    let dim = ctx.theme.dim();
    let axis = |title: &str, [lo, hi]: [f64; 2]| {
        Axis::default()
            .title(Span::styled(title.to_string(), dim))
            .style(dim)
            .bounds([lo, hi])
            .labels([short(lo), short(hi)])
    };
    let chart = Chart::new(datasets)
        .style(ctx.theme.base())
        .x_axis(axis(&plot.x, xb))
        .y_axis(axis("", yb));
    f.render_widget(chart, area);
}

fn bars(f: &mut Frame, area: Rect, plot: &Plot, color: &dyn Fn(usize) -> Color, ctx: &RenderCtx) {
    let top = plot
        .series
        .iter()
        .flat_map(|(_, v)| v.iter().flatten())
        .fold(0.0f64, |m, v| m.max(*v));
    let scale = |v: f64| ((v.max(0.0) / top.max(f64::MIN_POSITIVE)) * 1000.0).round() as u64;
    let per_group = plot.series.len() as u16 * 4 + 1;
    let fit = (area.width / per_group.max(1)) as usize;
    let mut chart = BarChart::default()
        .bar_width(3)
        .bar_gap(1)
        .group_gap(1)
        .max(1000)
        .style(ctx.theme.base())
        .label_style(ctx.theme.dim());
    for (row, label) in plot.labels.iter().enumerate().take(fit) {
        let bars: Vec<Bar> = plot
            .series
            .iter()
            .enumerate()
            .map(|(i, (_, values))| {
                let v = values[row].unwrap_or(0.0);
                Bar::new(scale(v))
                    .text_value(short(v))
                    .style(Style::default().fg(color(i)))
                    .value_style(Style::default().fg(ctx.theme.bg).bg(color(i)))
            })
            .collect();
        chart = chart.data(BarGroup::with_label(label.clone(), bars));
    }
    let [legend, chart_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let names: Vec<Span> = plot
        .series
        .iter()
        .enumerate()
        .flat_map(|(i, (name, _))| {
            [
                Span::styled("■ ", Style::default().fg(color(i))),
                Span::styled(format!("{name}  "), ctx.theme.dim()),
            ]
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(names)), legend);
    f.render_widget(chart, chart_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use dfox_core::{
        arrow::ArrowResultSet,
        result::{ColumnMeta, OwnedCell},
    };

    fn result(columns: &[&str], rows: Vec<Vec<OwnedCell>>) -> ArrowResultSet {
        ArrowResultSet::from_cells(
            columns.iter().map(|c| ColumnMeta::new(*c, "")).collect(),
            rows,
        )
    }

    #[test]
    fn the_x_is_the_first_column_when_it_is_a_number_and_the_rest_are_series() {
        use OwnedCell::{Int, Null, Text};
        let r = result(
            &["day", "sales", "note"],
            vec![
                vec![Int(1), Int(10), Text("a".into())],
                vec![Int(2), Null, Text("b".into())],
            ],
        );
        let p = plot(&r).expect("plot");
        assert_eq!((p.x.as_str(), p.xs.clone()), ("day", vec![1.0, 2.0]));
        assert_eq!(p.series, [("sales".to_string(), vec![Some(10.0), None])]);

        let r = result(&["name", "n"], vec![vec![Text("x".into()), Int(3)]]);
        let p = plot(&r).expect("plot");
        assert_eq!(
            (p.x.as_str(), p.labels.clone()),
            ("name", vec!["x".to_string()])
        );

        let r = result(&["name"], vec![vec![Text("x".into())]]);
        assert_eq!(plot(&r), None);
    }
}
