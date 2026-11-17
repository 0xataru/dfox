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

/// Rows or categories plotted.
// shortcut: only the first ones, add sampling when someone needs more.
const MOST: usize = 100_000;

/// Rows a column's kind is judged by.
const SAMPLE: usize = 1000;

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

/// The columns plotted: X (the row number when `None`), and the Y columns,
/// with the row count as one more.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pick {
    pub x: Option<usize>,
    pub ys: Vec<usize>,
    pub count: bool,
}

/// What is plotted: X values with their labels, and each Y.
#[derive(Debug, PartialEq)]
pub struct Plot {
    pub x: String,
    /// X is a column's numbers, not the row number or categories.
    pub numeric_x: bool,
    /// X is categories: rows with the same one were added up.
    pub grouped: bool,
    pub labels: Vec<String>,
    pub xs: Vec<f64>,
    pub series: Vec<(String, Vec<Option<f64>>)>,
    /// Rows or categories there were, of which [`MOST`] at most are here.
    pub total: usize,
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

/// The column holds numbers, judged by its first rows.
pub fn is_numeric(result: &impl ResultSet, col: usize) -> bool {
    let mut any = false;
    for r in 0..result.row_count().min(SAMPLE) {
        match result.cell(r, col) {
            Some(Cell::Null) | None => {}
            Some(cell) if cell.is_numeric() => any = true,
            Some(_) => return false,
        }
    }
    any
}

/// A key or a reference to one: numbers, but nothing to plot.
fn is_id(name: &str) -> bool {
    let name = name.to_lowercase();
    name == "id" || name.ends_with("_id") || name.ends_with("_ids")
}

/// Categories a column may have to be X: a status has a few, a name has
/// one per row.
const CATEGORIES: usize = 50;

/// Starts like `2026-11-17`.
fn is_date(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
}

/// X is a column of categories (a status, a type: few values, not an id),
/// else a date, else the row number. Y is the first number that isn't an
/// id, else the row count.
pub fn default_pick(result: &impl ResultSet) -> Pick {
    let columns = result.columns();
    let rows = result.row_count().min(SAMPLE);
    let texts: Vec<usize> = (0..columns.len())
        .filter(|&c| !is_id(&columns[c].name) && !is_numeric(result, c))
        .collect();
    let values = |c: usize| {
        (0..rows)
            .filter_map(move |r| result.cell(r, c))
            .filter(|v| !v.is_null())
    };
    let categories = texts.iter().copied().find(|&c| {
        let distinct: std::collections::HashSet<String> =
            values(c).map(|v| v.to_string()).collect();
        (2..=CATEGORIES).contains(&distinct.len()) && distinct.len() < rows
    });
    let date = || {
        texts
            .iter()
            .copied()
            .find(|&c| values(c).next().is_some_and(|v| is_date(&v.to_string())))
    };
    let x = categories.or_else(date);
    let y = (0..columns.len()).find(|&c| !is_id(&columns[c].name) && is_numeric(result, c));
    Pick {
        x,
        ys: y.into_iter().collect(),
        count: y.is_none(),
    }
}

/// The rows as `pick` asks. A category X adds up each Y over the rows
/// that share it, in the order they first come.
pub fn plot(result: &impl ResultSet, pick: &Pick) -> Option<Plot> {
    let columns = result.columns();
    let rows = result.row_count();
    if pick.ys.is_empty() && !pick.count {
        return None;
    }
    let value = |r: usize, c: usize| result.cell(r, c).and_then(number);
    let names = pick
        .ys
        .iter()
        .map(|&c| columns[c].name.clone())
        .chain(pick.count.then(|| "rows".to_string()));
    let x = pick.x.filter(|&c| c < columns.len());
    let numeric_x = x.is_some_and(|c| is_numeric(result, c));
    if let (Some(c), false) = (x, numeric_x) {
        let mut at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut labels: Vec<String> = Vec::new();
        let mut sums: Vec<Vec<Option<f64>>> = Vec::new();
        for r in 0..rows {
            let label = result.cell(r, c).map(|v| v.to_string()).unwrap_or_default();
            let i = *at.entry(label.clone()).or_insert_with(|| {
                labels.push(label);
                sums.push(vec![None; pick.ys.len() + pick.count as usize]);
                labels.len() - 1
            });
            for (k, &y) in pick.ys.iter().enumerate() {
                if let Some(v) = value(r, y) {
                    *sums[i][k].get_or_insert(0.0) += v;
                }
            }
            if pick.count {
                *sums[i][pick.ys.len()].get_or_insert(0.0) += 1.0;
            }
        }
        let total = labels.len();
        labels.truncate(MOST);
        let series = names
            .enumerate()
            .map(|(k, name)| (name, sums.iter().take(MOST).map(|s| s[k]).collect()))
            .collect();
        return Some(Plot {
            x: columns[c].name.clone(),
            numeric_x: false,
            grouped: true,
            xs: (1..=labels.len()).map(|i| i as f64).collect(),
            labels,
            series,
            total,
        });
    }
    let shown = rows.min(MOST);
    let series = names
        .enumerate()
        .map(|(k, name)| {
            let values = (0..shown)
                .map(|r| match pick.ys.get(k) {
                    Some(&y) => value(r, y),
                    None => Some(1.0),
                })
                .collect();
            (name, values)
        })
        .collect();
    Some(Plot {
        x: x.map_or("row".to_string(), |c| columns[c].name.clone()),
        numeric_x,
        grouped: false,
        labels: (0..shown)
            .map(|r| match x {
                Some(c) => result.cell(r, c).map(|v| v.to_string()).unwrap_or_default(),
                None => (r + 1).to_string(),
            })
            .collect(),
        xs: (0..shown)
            .map(|r| match x {
                Some(c) => value(r, c).unwrap_or(r as f64),
                None => (r + 1) as f64,
            })
            .collect(),
        series,
        total: rows,
    })
}

fn short(v: f64) -> String {
    let (n, unit) = match v.abs() {
        a if a >= 1e9 => (v / 1e9, "B"),
        a if a >= 1e6 => (v / 1e6, "M"),
        a if a >= 1e4 => (v / 1e3, "k"),
        _ => (v, ""),
    };
    let text = if n.fract() == 0.0 {
        format!("{n:.0}")
    } else if unit.is_empty() {
        format!("{n:.2}")
    } else {
        format!("{n:.1}")
    };
    let text = match text.contains('.') {
        true => text.trim_end_matches('0').trim_end_matches('.').to_string(),
        false => text,
    };
    format!("{text}{unit}")
}

/// A line in the middle of `area`.
fn centered(f: &mut Frame, area: Rect, text: &str, ctx: &RenderCtx) {
    let [line] = Layout::vertical([Constraint::Length(1)])
        .flex(Flex::Center)
        .areas(area);
    let text = Line::from(vec![
        Span::styled(format!("{} ", ctx.icons.info), ctx.theme.key()),
        Span::styled(text.to_string(), ctx.theme.dim()),
    ]);
    f.render_widget(Paragraph::new(text).centered(), line);
}

/// Draws the graph. Gives back where its X and Y are named, to click on.
pub fn render(
    f: &mut Frame,
    area: Rect,
    plot: Option<&Plot>,
    kind: Kind,
    ctx: &RenderCtx,
) -> [Rect; 2] {
    let theme = ctx.theme;
    let Some(plot) = plot.filter(|_| area.width >= 12 && area.height >= 5) else {
        centered(f, area, "Nothing to plot. Enter picks the columns.", ctx);
        return [Rect::default(); 2];
    };
    if kind == Kind::Pie && !plot.grouped {
        centered(
            f,
            area,
            "A pie shows categories: Enter, then a text column as X.",
            ctx,
        );
        return [Rect::default(); 2];
    }
    let [note, area] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
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
    let all = plot.labels.len();
    let shown = match kind {
        Kind::Line => chart(f, area, plot, &plot.series, GraphType::Line, &paint),
        Kind::StackedLine => chart(
            f,
            area,
            plot,
            &stacked(&plot.series),
            GraphType::Line,
            &paint,
        ),
        Kind::Scatter => chart(f, area, plot, &plot.series, GraphType::Scatter, &paint),
        Kind::Bar => bars(f, area, plot, Direction::Vertical, &paint),
        Kind::HorizontalBar => bars(f, area, plot, Direction::Horizontal, &paint),
        Kind::StackedBar => stacked_bars(f, area, plot, &paint),
        Kind::Pie => {
            pie(f, area, plot, &paint);
            all
        }
        Kind::Histogram => {
            histogram(f, area, plot, &paint);
            all
        }
    };
    let ys: Vec<&str> = plot.series.iter().map(|(n, _)| n.as_str()).collect();
    let by = if plot.grouped { " (sums)" } else { "" };
    let x = format!("X {} ▾", plot.x);
    let y = match kind {
        Kind::Histogram => format!("How {} spreads ▾", ys.first().unwrap_or(&"")),
        _ => format!("Y {}{by} ▾", ys.join(", ")),
    };
    let width = |s: &str| s.chars().count() as u16;
    let mut axes = [
        Rect::new(note.x, note.y, width(&x), 1),
        Rect::new(note.x + width(&x) + 3, note.y, width(&y), 1),
    ];
    let mut text = format!("{x} · {y}");
    if kind == Kind::Histogram {
        axes = [Rect::default(), Rect::new(note.x, note.y, width(&y), 1)];
        text = y;
    }
    if shown < plot.total {
        let what = if plot.grouped { "categories" } else { "rows" };
        text.push_str(&format!(" · first {shown} of {} {what}", plot.total));
    }
    text.push_str(" · Enter: X, then ←→");
    f.render_widget(Paragraph::new(Span::styled(text, theme.dim())), note);
    axes.map(|axis| axis.intersection(note))
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

fn chart(
    f: &mut Frame,
    area: Rect,
    plot: &Plot,
    series: &Series,
    kind: GraphType,
    paint: &Paint,
) -> usize {
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
    plot.labels.len()
}

/// A bar's height out of 1000, the most `top` can be.
fn scale(v: f64, top: f64) -> u64 {
    ((v.max(0.0) / top.max(f64::MIN_POSITIVE)) * 1000.0).round() as u64
}

fn bars(f: &mut Frame, area: Rect, plot: &Plot, direction: Direction, paint: &Paint) -> usize {
    let top = plot
        .series
        .iter()
        .flat_map(|(_, v)| v.iter().flatten())
        .fold(0.0f64, |m, v| m.max(*v));
    let [names, area] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let n = plot.series.len().max(1) as u16;
    let groups = plot.labels.len().max(1) as u16;
    let (width, gap, fit) = match direction {
        Direction::Vertical => {
            // Wide enough for the labels when there are few.
            let width = (area.width / groups / n).saturating_sub(1).clamp(3, 12);
            (width, 1, area.width / (n * (width + 1) + 1))
        }
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
    plot.labels.len().min(fit as usize)
}

/// One column per row, each series a block on the ones before it.
fn stacked_bars(f: &mut Frame, area: Rect, plot: &Plot, paint: &Paint) -> usize {
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
        return 0;
    };
    let top = totals.iter().flatten().fold(0.0f64, |m, v| m.max(*v));
    let height = bars.height as f64;
    let groups = plot.labels.len().max(1) as u16;
    let width = (bars.width / groups).saturating_sub(1).clamp(3, 12);
    let fit = (bars.width / (width + 1)) as usize;
    let block = "█".repeat(width as usize);
    let buf = f.buffer_mut();
    for (row, label) in plot.labels.iter().enumerate().take(fit) {
        let x = bars.x + row as u16 * (width + 1);
        let mut below = 0;
        for (i, (_, cumulative)) in sums.iter().enumerate() {
            let v = cumulative[row].unwrap_or(0.0).max(0.0);
            let up = ((v / top.max(f64::MIN_POSITIVE)) * height).round() as u16;
            for level in below..up.min(bars.height) {
                let y = bars.bottom() - 1 - level;
                buf.set_string(x, y, &block, Style::default().fg((paint.color)(i)));
            }
            below = below.max(up);
        }
        let label: String = label.chars().take(width as usize).collect();
        buf.set_string(x, labels.y, label, paint.ctx.theme.dim());
    }
    plot.labels.len().min(fit)
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
    let mut values: Vec<f64> = values.iter().flatten().copied().collect();
    let all = values.len();
    // A few huge values would squeeze the rest into one bin.
    if all >= 100 {
        let mut sorted = values.clone();
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted[((all - 1) as f64 * q).round() as usize];
        let (lo, hi) = (at(0.01), at(0.99));
        values.retain(|v| (lo..=hi).contains(v));
    }
    let [names, area] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let mut title = format!("{name} · {} values", values.len());
    if values.len() < all {
        title.push_str(&format!(" · {} outliers left out", all - values.len()));
    }
    legend(names, &[title.as_str()], paint, f);
    // Sturges' rule, as many as fit.
    let wanted = (values.len().max(1) as f64).log2().ceil() as usize + 1;
    let counted = bins(&values, wanted.min(area.width as usize / 7).clamp(1, 20));
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
        .bar_width(6)
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
    fn ids_are_not_plotted_and_categories_add_up() {
        use OwnedCell::{Decimal, Int, Null, Text};
        let row = |id, status: &str, sum: &str, manager| {
            vec![Int(id), Text(status.into()), Decimal(sum.into()), manager]
        };
        let r = result(
            &["id", "status", "order_sum", "manager_id"],
            vec![
                row(1, "new", "10.5", Int(7)),
                row(2, "paid", "4", Null),
                row(3, "new", "1.5", Int(7)),
            ],
        );
        let pick = default_pick(&r);
        assert_eq!(
            pick,
            Pick {
                x: Some(1),
                ys: vec![2],
                count: false
            }
        );
        let p = plot(&r, &pick).expect("plot");
        assert!(p.grouped);
        assert_eq!(p.labels, ["new", "paid"]);
        assert_eq!(
            p.series,
            [("order_sum".to_string(), vec![Some(12.0), Some(4.0)])]
        );

        let counted = Pick {
            x: Some(1),
            ys: vec![],
            count: true,
        };
        let p = plot(&r, &counted).expect("plot");
        assert_eq!(p.series, [("rows".to_string(), vec![Some(2.0), Some(1.0)])]);

        let by_id = Pick {
            x: Some(0),
            ys: vec![2],
            count: false,
        };
        let p = plot(&r, &by_id).expect("plot");
        assert!(p.numeric_x && !p.grouped);
        assert_eq!(p.xs, [1.0, 2.0, 3.0]);

        let people = result(
            &["fio", "created_at", "sum"],
            (0..5)
                .map(|i| {
                    vec![
                        Text(format!("person {i}").into()),
                        Text(format!("2026-11-0{i} 10:00").into()),
                        Int(i),
                    ]
                })
                .collect(),
        );
        assert_eq!(
            default_pick(&people).x,
            Some(1),
            "a date, not a name per row"
        );

        let ids = result(&["id"], vec![vec![Int(1)]]);
        assert_eq!(
            default_pick(&ids),
            Pick {
                x: None,
                ys: vec![],
                count: true
            }
        );
        assert_eq!(plot(&ids, &Pick::default()), None);
    }

    #[test]
    fn numbers_are_short() {
        assert_eq!(short(-998454.85), "-998.5k");
        assert_eq!(short(1_260_000.0), "1.3M");
        assert_eq!(short(3.5), "3.5");
        assert_eq!(short(12000.0), "12k");
        assert_eq!(short(7.0), "7");
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
