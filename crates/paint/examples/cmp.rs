//! Compares two PNGs pixel by pixel. A debugging aid for reference baselines.

fn main() {
    let mut args = std::env::args().skip(1);
    let a = image::open(args.next().expect("first image"))
        .expect("decode")
        .to_rgba8();
    let b = image::open(args.next().expect("second image"))
        .expect("decode")
        .to_rgba8();
    println!("{:?} vs {:?}", a.dimensions(), b.dimensions());
    let mut rows: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
    for ((x, y, p), q) in a.enumerate_pixels().zip(b.pixels()) {
        let _ = x;
        if p != q {
            *rows.entry(y).or_default() += 1;
        }
    }
    println!("differing rows: {rows:?}");
}
