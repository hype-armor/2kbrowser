//! Renders a page with its link rectangles outlined. A debugging aid.
//!
//! Link geometry is easy to get subtly wrong and impossible to check by
//! reading numbers: drawing it over the page is how you see that a link's
//! clickable area is where the link looks like it is.
//!
//! Run with `cargo run -p shell --example link-map -- <file.html> [out.png]`.

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("an input file");
    let output = args.next().unwrap_or_else(|| "link-map.png".to_owned());

    let path = std::path::Path::new(&input)
        .canonicalize()
        .expect("input exists");
    let url = net::file_url(&path);
    let fetcher = net::Fetcher::default();
    let resource = fetcher
        .fetch(&url, None, net::RequestKind::Navigation)
        .expect("readable");

    let mut fonts = text::FontStore::new();
    let page = shell::render::render_with_base(
        &resource.body,
        800,
        3000,
        &mut fonts,
        Some((&resource.origin, &resource.path)),
    );

    let mut pixmap = page.pixmap.clone();
    let links = page.links();
    for (rect, _) in &links {
        outline(&mut pixmap, rect, paint::magenta());
    }

    // A third argument searches the page and outlines the matches, so find and
    // link geometry can be eyeballed the same way.
    let query = args.next();
    let matches = query
        .as_deref()
        .map(|query| page.find(query))
        .unwrap_or_default();
    for rect in &matches {
        outline(&mut pixmap, rect, paint::cyan());
    }

    pixmap.save_png(&output).expect("write");
    println!(
        "wrote {output} with {} link rectangle(s) and {} match(es)",
        links.len(),
        matches.len()
    );
}

/// Draws a one-pixel outline in a colour nothing on a real page will be.
fn outline(pixmap: &mut paint::Pixmap, rect: &layout::Rect, color: paint::PremultipliedColor) {
    let width = pixmap.width() as i32;
    let height = pixmap.height() as i32;
    let set = |pixels: &mut [paint::PremultipliedColor], x: i32, y: i32| {
        if x < 0 || y < 0 || x >= width || y >= height {
            return;
        }
        pixels[(y * width + x) as usize] = color;
    };
    let (x0, y0) = (rect.x.round() as i32, rect.y.round() as i32);
    let (x1, y1) = (
        (rect.x + rect.width).round() as i32 - 1,
        (rect.y + rect.height).round() as i32 - 1,
    );
    let pixels = pixmap.pixels_mut();
    for x in x0..=x1 {
        set(pixels, x, y0);
        set(pixels, x, y1);
    }
    for y in y0..=y1 {
        set(pixels, x0, y);
        set(pixels, x1, y);
    }
}
