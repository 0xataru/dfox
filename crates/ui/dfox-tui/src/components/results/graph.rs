//! The new layout's Graph: the result's numbers as lines, points, bars, a
//! pie or a histogram.

use dfox_core::result::{Cell, ResultSet};
use ratatui::{
    layout::{Constraint, Direction, Flex, Layout, Rect},
    style::{Color, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        canvas::{Canvas, Points},
        Axis, Bar, BarChart, BarGroup, Chart, Dataset, GraphType, Paragraph,
    },
    Frame,
};

use crate::component::RenderCtx;

/// Rows plotted.
// shortcut: only the first rows, add paging or sampling when someone needs more.
const MOST: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    /// A metric over time.
    #[default]
    Line,
    /// Parts of a total over time.
    StackedLine,
    /// How two numbers go together.
    Scatter,
    /// Categories compared.
    Bar,
    /// Categories compared, with long names.
    HorizontalBar,
    /// Parts of each category's total.
    StackedBar,
    /// Shares of a whole.
    Pie,
    /// How a column's values spread.
    Histogram,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::Line,
        Kind::StackedLine,
        Kind::Scatter,
        Kind::Bar,
        Kind::HorizontalBar,
        Kind::StackedBar,
        Kind::Pie,
        Kind::Histogram,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Line => "lines",
            Kind::StackedLine => "stacked lines",
            Kind::Scatter => "points",
            Kind::Bar => "bars",
            Kind::HorizontalBar => "horizontal bars",
            Kind::StackedBar => "stacked bars",
            Kind::Pie => "pie",
            Kind::Histogram => "histogram",
        }
    }

    pub fn next(self) -> Kind {
        let at = Self::ALL.iter().position(|k| *k == self).unwrap_or(0);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }
}

/// What is plotted: X values with their labels, and each numeric column.
#[derive(Debug, PartialEq)]
pub struct Plot {
    pub x: String,
    /// X is the first column's numbers, not the row number.
    pub numeric_x: bool,
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
        numeric_x,
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
    let plot = plot(result).filter(|_| area.width >= 12 && area.height >= 4);
    let Some(plot) = plot else {
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
    let paint = Paint {
        color: &|i| colors[i % colors.len()],
        ctx,
    };
    match kind {
        Kind::Line => chart(f, area, &plot, &plot.series, GraphType::Line, &paint),
        Kind::StackedLine => chart(
            f,
            area,
            &plot,
            &stacked(&plot.series),
            GraphType::Line,
            &paint,
        ),
        Kind::Scatter => chart(f, area, &plot, &plot.series, GraphType::Scatter, &paint),
        Kind::Bar => bars(f, area, &plot, Direction::Vertical, &paint),
        Kind::HorizontalBar => bars(f, area, &plot, Direction::Horizontal, &paint),
        Kind::StackedBar => stacked_bars(f, area, &plot, &paint),
        Kind::Pie => pie(f, area, &plot, &paint),
        Kind::Histogram => histogram(f, area, &plot, &paint),
    }
}

struct Paint<'a> {
    color: &'a dyn Fn(usize) -> Color,
    ctx: &'a RenderCtx<'a>,
}

type Series = Vec<(String, Vec<Option<f64>>)>;

/// Each series on top of the ones before it, gaps as 0.
fn stacked(series: &Series) -> Series {
    let mut sums: Vec<f64> = Vec::new();
    series
        .iter()
        .map(|(name, values)| {
            sums.resize(values.len(), 0.0);
            for (sum, v) in sums.iter_mut().zip(values) {
                *sum += v.unwrap_or(0.0);
            }
            (name.clone(), sums.iter().map(|v| Some(*v)).collect())
        })
        .collect()
}

/// The names of the series, each in its colour, on one line.
fn legend(area: Rect, names: &[&str], paint: &Paint, f: &mut Frame) {
    let spans: Vec<Span> = names
        .iter()
        .enumerate()
        .flat_map(|(i, name)| {
            [
                Span::styled("■ ", Style::default().fg((paint.color)(i))),
                Span::styled(format!("{name}  "), paint.ctx.theme.dim()),
            ]
        })
        .collect();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn chart(f: &mut Frame, area: Rect, plot: &Plot, series: &Series, kind: GraphType, paint: &Paint) {
    let points: Vec<Vec<(f64, f64)>> = series
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
    let marker = match kind {
        GraphType::Scatter => Marker::Dot,
        _ => Marker::Braille,
    };
    let datasets = series
        .iter()
        .zip(&points)
        .enumerate()
        .map(|(i, ((name, _), points))| {
            Dataset::default()
                .name(name.clone())
                .marker(marker)
                .graph_type(kind)
                .style(Style::default().fg((paint.color)(i)))
                .data(points)
        })
        .collect();
    let dim = paint.ctx.theme.dim();
    // Rows by number are labelled with the first column: dates, names.
    let x_labels = match (plot.numeric_x, plot.labels.first(), plot.labels.last()) {
        (false, Some(first), Some(last)) => [first.clone(), last.clone()],
        _ => [short(xb[0]), short(xb[1])],
    };
    let chart = Chart::new(datasets)
        .hidden_legend_constraints((Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)))
        .style(paint.ctx.theme.base())
        .x_axis(
            Axis::default()
                .title(Span::styled(plot.x.clone(), dim))
                .style(dim)
                .bounds(xb)
                .labels(x_labels),
        )
        .y_axis(
            Axis::default()
                .style(dim)
                .bounds(yb)
                .labels([short(yb[0]), short(yb[1])]),
        );
    f.render_widget(chart, area);
}

/// A bar's height out of 1000, the most `top` can be.
fn scale(v: f64, top: f64) -> u64 {
    ((v.max(0.0) / top.max(f64::MIN_POSITIVE)) * 1000.0).round() as u64
}

fn bars(f: &mut Frame, area: Rect, plot: &Plot, direction: Direction, paint: &Paint) {
    let top = plot
        .series
        .iter()
        .flat_map(|(_, v)| v.iter().flatten())
        .fold(0.0f64, |m, v| m.max(*v));
    let [names, area] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let n = plot.series.len() as u16;
    let (width, gap, fit) = match direction {
        Direction::Vertical => (3, 1, area.width / (n * 4 + 1)),
        Direction::Horizontal => (1, 0, area.height / (n + 1)),
    };
    let mut chart = BarChart::default()
        .direction(direction)
        .bar_width(width)
        .bar_gap(gap)
        .group_gap(1)
        .max(1000)
        .style(paint.ctx.theme.base())
        .label_style(paint.ctx.theme.dim());
    for (row, label) in plot.labels.iter().enumerate().take(fit as usize) {
        let bars: Vec<Bar> = plot
            .series
            .iter()
            .enumerate()
            .map(|(i, (_, values))| {
                let v = values[row].unwrap_or(0.0);
                let color = (paint.color)(i);
                Bar::new(scale(v, top))
                    .text_value(short(v))
                    .style(Style::default().fg(color))
                    .value_style(Style::default().fg(paint.ctx.theme.bg).bg(color))
            })
            .collect();
        chart = chart.data(BarGroup::with_label(label.clone(), bars));
    }
    let titles: Vec<&str> = plot.series.iter().map(|(n, _)| n.as_str()).collect();
    legend(names, &titles, paint, f);
    f.render_widget(chart, area);
}

/// One column per row, each series a block on the ones before it.
fn stacked_bars(f: &mut Frame, area: Rect, plot: &Plot, paint: &Paint) {
    let [names, bars, labels] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let titles: Vec<&str> = plot.series.iter().map(|(n, _)| n.as_str()).collect();
    legend(names, &titles, paint, f);
    let sums = stacked(&plot.series);
    let Some((_, totals)) = sums.last() else {
        return;
    };
    let top = totals.iter().flatten().fold(0.0f64, |m, v| m.max(*v));
    let height = bars.height as f64;
    let buf = f.buffer_mut();
    for (row, label) in plot.labels.iter().enumerate().take(bars.width as usize / 4) {
        let x = bars.x + row as u16 * 4;
        let mut below = 0;
        for (i, (_, cumulative)) in sums.iter().enumerate() {
            let v = cumulative[row].unwrap_or(0.0).max(0.0);
            let up = ((v / top.max(f64::MIN_POSITIVE)) * height).round() as u16;
            for level in below..up.min(bars.height) {
                let y = bars.bottom() - 1 - level;
                buf.set_string(x, y, "███", Style::default().fg((paint.color)(i)));
            }
            below = below.max(up);
        }
        let label: String = label.chars().take(3).collect();
        buf.set_string(x, labels.y, label, paint.ctx.theme.dim());
    }
}

/// Slices shown, the rest go into one.
const SLICES: usize = 7;

/// The first series' share of each row, biggest first.
fn shares(plot: &Plot) -> Vec<(String, f64)> {
    let Some((_, values)) = plot.series.first() else {
        return Vec::new();
    };
    let mut slices: Vec<(String, f64)> = plot
        .labels
        .iter()
        .zip(values)
        .filter_map(|(label, v)| v.filter(|v| *v > 0.0).map(|v| (label.clone(), v)))
        .collect();
    slices.sort_by(|a, b| b.1.total_cmp(&a.1));
    if slices.len() > SLICES {
        let rest: f64 = slices.drain(SLICES - 1..).map(|(_, v)| v).sum();
        slices.push(("other".into(), rest));
    }
    slices
}

fn pie(f: &mut Frame, area: Rect, plot: &Plot, paint: &Paint) {
    let slices = shares(plot);
    let total: f64 = slices.iter().map(|(_, v)| v).sum();
    let [circle, key] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Length(28)]).areas(area);
    // Braille dots are about square: a cell is 2 dots wide and 4 high.
    let (cols, rows) = (circle.width.max(1) as f64, circle.height.max(1) as f64);
    let ratio = cols / (2.0 * rows);
    let (xr, yr) = if ratio >= 1.0 {
        (ratio, 1.0)
    } else {
        (1.0, 1.0 / ratio)
    };
    let step = 2.0 * xr / (2.0 * cols);
    let mut points: Vec<Vec<(f64, f64)>> = vec![Vec::new(); slices.len()];
    let mut y = -yr;
    while y <= yr {
        let mut x = -xr;
        while x <= xr {
            if x * x + y * y <= 0.9 {
                // Clockwise from twelve o'clock.
                let turn = (x.atan2(y) / std::f64::consts::TAU).rem_euclid(1.0);
                let mut sum = 0.0;
                if let Some(i) = slices.iter().position(|(_, v)| {
                    sum += v / total;
                    turn < sum
                }) {
                    points[i].push((x, y));
                }
            }
            x += step;
        }
        y += step / 2.0;
    }
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([-xr, xr])
        .y_bounds([-yr, yr])
        .background_color(paint.ctx.theme.bg)
        .paint(|c| {
            for (i, coords) in points.iter().enumerate() {
                c.draw(&Points {
                    coords,
                    color: (paint.color)(i),
                });
            }
        });
    f.render_widget(canvas, circle);
    let lines: Vec<Line> = slices
        .iter()
        .enumerate()
        .map(|(i, (label, v))| {
            Line::from(vec![
                Span::styled("■ ", Style::default().fg((paint.color)(i))),
                Span::styled(
                    format!("{:>5.1}% ", v / total * 100.0),
                    paint.ctx.theme.key(),
                ),
                Span::styled(label.clone(), paint.ctx.theme.dim()),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), key);
}

/// How many of the first series' values fall in each of `bins` equal
/// ranges, by where each range starts.
fn bins(values: &[f64], bins: usize) -> Vec<(f64, u64)> {
    let (lo, hi) = values
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    if values.is_empty() {
        return Vec::new();
    }
    let bins = if lo == hi { 1 } else { bins.max(1) };
    let width = (hi - lo) / bins as f64;
    let mut counts = vec![0u64; bins];
    for v in values {
        let at = if width == 0.0 {
            0
        } else {
            (((v - lo) / width) as usize).min(bins - 1)
        };
        counts[at] += 1;
    }
    counts
        .into_iter()
        .enumerate()
        .map(|(i, n)| (lo + width * i as f64, n))
        .collect()
}

fn histogram(f: &mut Frame, area: Rect, plot: &Plot, paint: &Paint) {
    let Some((name, values)) = plot.series.first() else {
        return;
    };
    let values: Vec<f64> = values.iter().flatten().copied().collect();
    let [names, area] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let title = format!("{name} · {} values", values.len());
    legend(names, &[title.as_str()], paint, f);
    // Sturges' rule, as many as fit.
    let wanted = (values.len().max(1) as f64).log2().ceil() as usize + 1;
    let counted = bins(&values, wanted.min(area.width as usize / 6).clamp(1, 20));
    let color = (paint.color)(0);
    let bars: Vec<Bar> = counted
        .iter()
        .map(|(from, n)| {
            Bar::with_label(short(*from), *n)
                .style(Style::default().fg(color))
                .value_style(Style::default().fg(paint.ctx.theme.bg).bg(color))
        })
        .collect();
    let chart = BarChart::new(bars)
        .bar_width(5)
        .bar_gap(1)
        .style(paint.ctx.theme.base())
        .label_style(paint.ctx.theme.dim());
    f.render_widget(chart, area);
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

    #[test]
    fn stacks_sum_and_bins_count() {
        let series = vec![
            ("a".to_string(), vec![Some(1.0), None]),
            ("b".to_string(), vec![Some(2.0), Some(3.0)]),
        ];
        assert_eq!(stacked(&series)[1].1, [Some(3.0), Some(3.0)]);
        assert_eq!(bins(&[1.0, 2.0, 2.0, 5.0], 2), [(1.0, 3), (3.0, 1)]);
        assert_eq!(bins(&[4.0, 4.0], 5), [(4.0, 2)]);
        assert_eq!(Kind::Histogram.next(), Kind::Line);
    }
}
