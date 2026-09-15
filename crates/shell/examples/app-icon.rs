//! Writes the application icon, and a sheet of it at the sizes it is used at.
//!
//! The icon is drawn rather than shipped ([`shell::icon`]), so this is how it
//! gets *looked* at — the same arrangement the chrome bar has, and for the same
//! reason: a headless test can compare pixels and a person cannot compare
//! descriptions.
//!
//! Run with `cargo run -p shell --example app-icon -- [out-dir]`.

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "docs/images".to_owned());

    let full = 512u32;
    shell::icon::render(full)
        .expect("renders")
        .save_png(format!("{out}/icon.png"))
        .expect("write");

    // The sizes a platform actually asks for, at actual size and in a row. Not
    // magnified: the question this sheet answers is whether the drawing holds
    // up small, and a blown-up sixteen-pixel icon answers a different one.
    let sizes = [16u32, 24, 32, 48, 64, 128];
    let gap = 12u32;
    let width: u32 = sizes.iter().map(|size| size + gap).sum::<u32>() + gap;
    let height = sizes.iter().copied().max().unwrap_or(0) + gap * 2;
    let mut sheet = paint::Pixmap::new(width, height).expect("sheet");
    sheet.fill(paint::RasterColor::from_rgba8(0xee, 0xee, 0xec, 0xff));

    let mut x = gap as i32;
    for size in sizes {
        let icon = shell::icon::render(size).expect("renders");
        // Bottom-aligned, so the row reads as one drawing getting smaller
        // rather than as six drawings at six heights.
        let y = (height - gap - size) as i32;
        sheet.draw_pixmap(
            x,
            y,
            icon.as_ref(),
            &paint::PixmapPaint::default(),
            paint::Transform::identity(),
            None,
        );
        x += (size + gap) as i32;
    }
    sheet
        .save_png(format!("{out}/icon-sizes.png"))
        .expect("write");

    println!("wrote {out}/icon.png at {full}px and {out}/icon-sizes.png");
}
