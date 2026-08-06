//! Renders the saved-pages list, so it can be looked at.
//!
//! The list is a real page shown by the real engine, so this is the same thing
//! Ctrl+B puts in a tab — a debugging aid for the one screen the browser
//! generates itself.
//!
//! Run with `cargo run -p shell --example bookmark-page -- [out.png]`.
fn main() {
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "bookmarks.png".to_owned());
    let marks = shell::bookmarks::Bookmarks::load(&shell::bookmarks::default_path());
    let html = shell::bookmarks::page(&marks);

    let path = shell::bookmarks::page_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("config directory");
    }
    std::fs::write(&path, &html).expect("write the page");
    let url = format!("file://{}", path.display());
    let (origin, at) = net::parse_url(&url).expect("a file URL");

    let mut fonts = text::FontStore::new();
    let page = shell::render::render_with_base(&html, 800, 2000, &mut fonts, Some((&origin, &at)));
    page.pixmap.save_png(&output).expect("write");
    println!(
        "wrote {output} — {} saved page(s), {} link(s)",
        marks.len(),
        page.links().len()
    );
}
