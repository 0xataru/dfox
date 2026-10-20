use super::*;
use dfox_core::result::{ColumnMeta, OwnedCell};
use ratatui::{backend::TestBackend, Terminal};

use crate::testing::{Cells, TestCtx};

fn store(rows: usize, columns: usize) -> Cells {
    Cells {
        columns: (0..columns)
            .map(|c| ColumnMeta::new(format!("column_{c}"), "text"))
            .collect(),
        rows: (0..rows)
            .map(|r| {
                (0..columns)
                    .map(|c| OwnedCell::Text(format!("r{r}c{c}").into()))
                    .collect()
            })
            .collect(),
    }
}

fn draw(grid: &Grid, result: &Cells, width: u16, height: u16) -> String {
    let test = TestCtx::new();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    let ctx = test.render_ctx();
    terminal
        .draw(|f| {
            let area = f.area();
            grid.render(f.buffer_mut(), area, result, true, &ctx)
        })
        .expect("draw");
    terminal
        .backend()
        .buffer()
        .content
        .chunks(width as usize)
        .map(|row| {
            row.iter()
                .map(|c| c.symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn keys_move_the_cursor_within_the_result() {
    let mut grid = Grid::default();
    let size = (10, 3);
    grid.handle(Action::End, size);
    assert_eq!(grid.cursor(), (9, 0));
    grid.handle(Action::Down, size);
    grid.handle(Action::WordRight, size);
    assert_eq!(grid.cursor(), (9, 2));
    grid.handle(Action::Right, size);
    assert_eq!(grid.cursor(), (9, 2));
    grid.handle(Action::Home, size);
    assert_eq!(grid.cursor(), (0, 0));
    grid.handle(Action::PageDown, size);
    grid.handle(Action::Up, size);
    grid.handle(Action::PageUp, size);
    grid.handle(Action::Left, size);
    assert_eq!(grid.cursor(), (0, 0));
    assert!(!grid.handle(Action::Back, size));
    assert!(!grid.handle(Action::Confirm, size));
}

#[test]
fn shift_keys_select_a_range_and_back_clears_it() {
    let mut grid = Grid::default();
    let size = (10, 4);
    grid.handle(Action::Right, size);
    grid.handle(Action::SelectDown, size);
    grid.handle(Action::SelectRight, size);
    assert_eq!(
        grid.region(4),
        Region {
            rows: 0..2,
            cols: 1..3
        }
    );
    grid.handle(Action::SelectUp, size);
    grid.handle(Action::SelectLeft, size);
    grid.handle(Action::SelectLeft, size);
    assert_eq!(
        grid.region(4),
        Region {
            rows: 0..1,
            cols: 0..2
        }
    );
    grid.handle(Action::SelectEnd, size);
    assert_eq!(
        grid.region(4),
        Region {
            rows: 0..1,
            cols: 1..4
        }
    );
    grid.handle(Action::SelectHome, size);
    assert!(grid.has_selection());
    assert!(grid.handle(Action::Back, size));
    assert!(!grid.has_selection());
    assert_eq!(
        grid.region(4),
        Region {
            rows: 0..1,
            cols: 0..1
        }
    );
}

#[test]
fn rows_and_everything_can_be_selected() {
    let mut grid = Grid::default();
    let size = (5, 3);
    grid.handle(Action::Down, size);
    grid.handle(Action::SelectRow, size);
    grid.handle(Action::SelectDown, size);
    assert_eq!(
        grid.region(3),
        Region {
            rows: 1..3,
            cols: 0..3
        }
    );
    grid.handle(Action::SelectAll, size);
    assert_eq!(
        grid.region(3),
        Region {
            rows: 0..5,
            cols: 0..3
        }
    );
}

#[test]
fn columns_widen_and_narrow_within_limits() {
    let result = store(3, 2);
    let mut grid = Grid::default();
    let icons = Icons::unicode();
    assert_eq!(grid.widths(&result, &icons), [8, 8]);
    grid.handle(Action::Wider, (3, 2));
    assert_eq!(grid.widths(&result, &icons), [12, 8]);
    for _ in 0..10 {
        grid.handle(Action::Narrower, (3, 2));
    }
    assert_eq!(grid.widths(&result, &icons), [MIN_WIDTH, 8]);
}

#[test]
fn widths_follow_the_widest_sampled_value() {
    let mut result = Cells::new(vec![ColumnMeta::new("id", ""), ColumnMeta::new("note", "")]);
    result.push(vec![
        OwnedCell::Int(1),
        OwnedCell::Text("x".repeat(100).into()),
    ]);
    result.push(vec![OwnedCell::Int(123_456), OwnedCell::Null]);
    let grid = Grid::default();
    assert_eq!(grid.widths(&result, &Icons::unicode()), [7, MAX_AUTO_WIDTH]);
}

#[test]
fn only_the_rows_on_screen_are_drawn() {
    let result = store(1_000_000, 3);
    let mut grid = Grid::default();
    grid.handle(Action::End, (1_000_000, 3));
    let screen = draw(&grid, &result, 50, 6);
    insta::assert_snapshot!(screen);
}

#[test]
fn the_cursor_column_scrolls_into_view() {
    let result = store(3, 12);
    let mut grid = Grid::default();
    for _ in 0..9 {
        grid.handle(Action::Right, (3, 12));
    }
    grid.handle(Action::SelectDown, (3, 12));
    grid.handle(Action::SelectLeft, (3, 12));
    insta::assert_snapshot!(draw(&grid, &result, 40, 6));
}

#[test]
fn clicks_find_cells_once_drawn() {
    let result = store(20, 3);
    let mut grid = Grid::default();
    draw(&grid, &result, 60, 8);
    // Row numbers take 2 columns plus " │ "; each column 9 wide plus a gap.
    assert_eq!(grid.cell_at(5, 2), Some((0, 0)));
    assert_eq!(grid.cell_at(5 + 11, 4), Some((2, 1)));
    assert_eq!(grid.cell_at(5, 0), None);
    assert!(grid.contains(5, 2));
    grid.click((2, 1), false, (20, 3));
    grid.click((3, 2), true, (20, 3));
    assert_eq!(
        grid.region(3),
        Region {
            rows: 2..4,
            cols: 1..3
        }
    );
    grid.scroll_by(3, (20, 3));
    assert_eq!(grid.cursor(), (6, 2));
    grid.scroll_columns(-5, (20, 3));
    assert_eq!(grid.cursor(), (6, 0));
}

#[test]
fn values_are_shown_for_reading() {
    let icons = Icons::unicode();
    let show = |value| display(value, &icons).into_owned();
    assert_eq!(show(Value::Null), "∅");
    assert_eq!(show(Value::Bool(true)), "✓");
    assert_eq!(show(Value::Bool(false)), "✗");
    assert_eq!(show(Value::Int(2024)), "2024");
    assert_eq!(show(Value::Int(-1234567)), "-1,234,567");
    assert_eq!(show(Value::UInt(10_000)), "10,000");
    assert_eq!(show(Value::Float(12345.25)), "12,345.25");
    assert_eq!(show(Value::Decimal("123456789.000")), "123,456,789.000");
    assert_eq!(show(Value::Text("a\nb\tc")), "a↵b c");
    assert_eq!(show(Value::Json("{\"a\":1}")), "{\"a\":1}");
    assert_eq!(show(Value::Bytes(&[0xab])), "0xab");
    assert_eq!(show(Value::Undecodable("bad")), "⟨bad⟩");
    let long = "x".repeat(MAX_SHOWN + 10);
    assert_eq!(show(Value::Text(&long)).chars().count(), MAX_SHOWN + 1);
    let bytes = vec![0u8; MAX_SHOWN];
    assert!(show(Value::Bytes(&bytes)).ends_with('…'));
    assert_eq!(group_digits("1e10"), "1e10");
    assert_eq!(group_digits("NaN"), "NaN");
    assert_eq!(group_digits("12345abc"), "12345abc");
}

/// `cargo test -p dfox --release -- --ignored grid_render_time`
#[test]
#[ignore = "timing; run in release"]
fn grid_render_time() {
    let result = store(1_000_000, 20);
    let mut grid = Grid::default();
    grid.handle(Action::End, (1_000_000, 20));
    let test = TestCtx::new();
    let ctx = test.render_ctx();
    let area = Rect::new(0, 0, 200, 60);
    let mut buf = Buffer::empty(area);
    let mut times: Vec<_> = (0..500)
        .map(|_| {
            let start = std::time::Instant::now();
            grid.render(&mut buf, area, &result, true, &ctx);
            start.elapsed()
        })
        .collect();
    times.sort();
    let p99 = times[times.len() * 99 / 100];
    println!("grid render p99: {p99:?}");
    assert!(p99 < std::time::Duration::from_millis(2), "{p99:?}");
}

/// Arrow results draw through the same `ResultSet` interface.
fn arrow_result(rows: usize, columns: usize) -> dfox_core::arrow::ArrowResultSet {
    use std::sync::Arc;

    use arrow_array::{
        ArrayRef, Date32Array, Decimal128Array, Int64Array, RecordBatch, StringArray,
    };
    use arrow_schema::DataType;
    use dfox_core::arrow::{field, schema, ArrowResultSet};

    let mut fields = vec![
        field("id", DataType::Int64, "int8"),
        field("price", DataType::Decimal128(12, 2), "numeric"),
        field("day", DataType::Date32, "date"),
    ];
    fields.extend((3..columns).map(|c| field(&format!("text_{c}"), DataType::Utf8, "text")));
    let schema = schema(fields);
    let mut result = ArrowResultSet::new(schema.clone());
    for start in (0..rows).step_by(8192) {
        let end = rows.min(start + 8192);
        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from_iter_values(start as i64..end as i64)),
            Arc::new(
                Decimal128Array::from_iter_values((start..end).map(|i| i as i128 * 1234))
                    .with_precision_and_scale(12, 2)
                    .expect("decimal"),
            ),
            Arc::new(Date32Array::from_iter(
                (start..end).map(|i| (i % 7 != 0).then_some(19_000 + i as i32 % 365)),
            )),
        ];
        arrays.extend((3..columns).map(|c| {
            Arc::new(StringArray::from_iter_values(
                (start..end).map(|i| format!("r{i}c{c}")),
            )) as ArrayRef
        }));
        result.push(RecordBatch::try_new(schema.clone(), arrays).expect("batch"));
    }
    result.finish();
    result
}

#[test]
fn arrow_results_draw_like_any_other() {
    let result = arrow_result(30, 4);
    let mut grid = Grid::default();
    grid.handle(Action::Down, (30, 4));
    let test = TestCtx::new();
    let mut terminal = Terminal::new(TestBackend::new(60, 8)).expect("terminal");
    let ctx = test.render_ctx();
    terminal
        .draw(|f| {
            let area = f.area();
            grid.render(f.buffer_mut(), area, &result, true, &ctx)
        })
        .expect("draw");
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .chunks(60)
        .map(|row| {
            row.iter()
                .map(|c| c.symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(screen);
}

/// `cargo test -p dfox --release -- --ignored arrow_grid_render_time`
#[test]
#[ignore = "timing; run in release"]
fn arrow_grid_render_time() {
    let result = arrow_result(1_000_000, 20);
    let mut grid = Grid::default();
    grid.handle(Action::End, (1_000_000, 20));
    let test = TestCtx::new();
    let ctx = test.render_ctx();
    let area = Rect::new(0, 0, 200, 60);
    let mut buf = Buffer::empty(area);
    let mut times: Vec<_> = (0..500)
        .map(|_| {
            let start = std::time::Instant::now();
            grid.render(&mut buf, area, &result, true, &ctx);
            start.elapsed()
        })
        .collect();
    times.sort();
    let p99 = times[times.len() * 99 / 100];
    println!("arrow grid render p99: {p99:?}");
    assert!(p99 < std::time::Duration::from_millis(2), "{p99:?}");
}
