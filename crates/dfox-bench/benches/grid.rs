//! Drawing a 200×60 result grid scrolled to the end of a million rows held
//! as Arrow batches. The budget is 2 ms per frame.

use std::collections::BTreeMap;

use criterion::{criterion_group, criterion_main, Criterion};
use dfox_bench::arrow_result;
use dfox_core::result::ResultSet;
use dfox_tui::{
    action::Action,
    component::RenderCtx,
    components::results::grid::Grid,
    icons::Icons,
    keymap::{Keymap, Preset},
    theme::Theme,
};
use ratatui::{buffer::Buffer, layout::Rect};

fn draw(c: &mut Criterion, name: &str, result: &impl ResultSet) {
    let theme = Theme::default();
    let icons = Icons::unicode();
    let keymap = Keymap::new(Preset::Standard, &BTreeMap::new()).0;
    let ctx = RenderCtx {
        theme: &theme,
        icons: &icons,
        keymap: &keymap,
        now: tokio::time::Instant::now(),
        frame: 0,
        animations: true,
    };
    let mut grid = Grid::default();
    grid.handle(Action::End, (result.row_count(), result.columns().len()));
    let area = Rect::new(0, 0, 200, 60);
    let mut buf = Buffer::empty(area);
    c.bench_function(name, |b| {
        b.iter(|| grid.render(&mut buf, area, result, true, &ctx))
    });
}

fn grid(c: &mut Criterion) {
    draw(c, "grid/arrow 1M×20", &arrow_result(1_000_000, 20));
}

criterion_group!(benches, grid);
criterion_main!(benches);
