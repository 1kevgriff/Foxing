//! Per-keystroke view costs: `cargo run --release --example viewbench -- <file>`

use foxing::buffer::Buffer;
use foxing::editor::{Editor, Motion};
use std::time::Instant;

fn time<R>(label: &str, n: u32, mut f: impl FnMut() -> R) {
    let t = Instant::now();
    for _ in 0..n {
        std::hint::black_box(f());
    }
    println!(
        "{label:<28} {:>8.1} µs",
        t.elapsed().as_secs_f64() * 1e6 / n as f64
    );
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: viewbench <file>");
    let mut ed = Editor::new(Buffer::open(path.as_ref()).expect("open"), "\n");
    // A 3840x1023 window at 96 DPI with Consolas 11pt: about 470 x 60 cells.
    ed.set_view(60, 470);
    for _ in 0..1000 {
        ed.move_caret(Motion::PageDown, false);
    }
    time("visible_rows", 200, || ed.visible_rows());
    time("visible_width", 200, || ed.visible_width());
    time("caret_cell", 200, || ed.caret_cell());
    let rows = ed.visible_rows();
    time("glyphs (all rows)", 200, || {
        rows.iter().map(|r| ed.glyphs(r).len()).sum::<usize>()
    });
    time("line_range (60 lines)", 200, || {
        rows.iter()
            .map(|r| ed.buffer().line_range(r.line).len())
            .sum::<usize>()
    });
    time("insert char", 200, || ed.insert("x"));
}
