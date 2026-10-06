//! Buffer engine benchmark: `cargo run --release --example bufbench -- <file>`

use foxing::buffer::Buffer;
use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("usage: bufbench <file>");
    let t = Instant::now();
    let mut b = Buffer::open(path.as_ref()).expect("open");
    println!(
        "open: {:?} ({:.2} GB, {} lines)",
        t.elapsed(),
        b.len() as f64 / 1e9,
        b.line_count()
    );

    let lines = b.line_count();
    let t = Instant::now();
    let mut sum = 0;
    for i in 0..100_000 {
        let line = (i * 7919) % lines;
        sum += b.line_range(line).len();
    }
    println!("100k random line_range: {:?} (checksum {sum})", t.elapsed());

    let mid = b.line_start(lines / 2);
    let t = Instant::now();
    for _ in 0..10_000 {
        b.insert(mid, "x");
    }
    println!("10k inserts mid-file: {:?}", t.elapsed());
    let t = Instant::now();
    b.delete(mid..mid + 10_000);
    println!("delete 10k bytes: {:?}", t.elapsed());

    let t = Instant::now();
    let found = b.find("needle-not-present", 0, true, true);
    println!("find miss, match case: {:?} ({found:?})", t.elapsed());
    let t = Instant::now();
    let found = b.find("Needle-Not-Present", 0, true, false);
    println!("find miss, ignore case: {:?} ({found:?})", t.elapsed());

    let out = std::env::temp_dir().join("bufbench-out.txt");
    let t = Instant::now();
    let mut w = std::io::BufWriter::new(std::fs::File::create(&out).unwrap());
    b.write_to(&mut w).unwrap();
    drop(w);
    println!("save: {:?}", t.elapsed());
    std::fs::remove_file(out).ok();
}
