//! Generates the reference tests' background tile.
//!
//! Checked-in binary assets are otherwise unreviewable: nobody can tell from a
//! PNG whether it changed on purpose. Generating it from code means the diff
//! that changes the tile is a diff anyone can read.
//!
//! Run with `cargo run -p paint --example make-tile -- <path.png>`.

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "tile.png".to_owned());

    // 16x16, the size a real tile of the era would be: small enough that its
    // repetition is the whole visual effect.
    const SIZE: u32 = 16;
    let mut image = image::RgbaImage::new(SIZE, SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            // A diagonal weave with a lighter body, so tiling is obvious and
            // seams would be too if the placement were wrong.
            let on_diagonal = (x + y) % 8 < 2;
            let pixel = if on_diagonal {
                image::Rgba([176, 196, 222, 255])
            } else {
                image::Rgba([240, 244, 250, 255])
            };
            image.put_pixel(x, y, pixel);
        }
    }
    image.save(&path).expect("write tile");
    println!("wrote {path}");
}
