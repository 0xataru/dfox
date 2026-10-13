//! Bytes taken by a million rows of an integer, a float and two texts, kept
//! as cells and as Arrow batches. Counts every allocation, so the numbers are
//! what the app would hold. Not timed: prints a table.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicIsize, Ordering},
};

use dfox_bench::{arrow_result, row_store, ROWS};

/// Counts the bytes in use.
struct Counting;

static IN_USE: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        IN_USE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        IN_USE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Bytes still held by what `make` returns.
fn held<T>(make: impl FnOnce() -> T) -> (usize, T) {
    let before = IN_USE.load(Ordering::Relaxed);
    let value = make();
    let after = IN_USE.load(Ordering::Relaxed);
    ((after - before).max(0) as usize, value)
}

fn main() {
    // `cargo bench` passes `--bench`; anything else, such as `cargo test`
    // running benches as tests, just checks this builds.
    if !std::env::args().any(|a| a == "--bench") {
        return;
    }
    let mb = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    let (cells, store) = held(|| row_store(ROWS, 4));
    drop(store);
    let (arrow, result) = held(|| arrow_result(ROWS, 4));
    println!("memory/1M rows of (int8, float8, text, text)");
    println!("  cells  {:>8.1} MB", mb(cells));
    println!(
        "  arrow  {:>8.1} MB  ({:.1} MB in batches)",
        mb(arrow),
        mb(result.memory_bytes())
    );
    println!(
        "  arrow uses {:.1}× less",
        cells as f64 / arrow.max(1) as f64
    );
}
