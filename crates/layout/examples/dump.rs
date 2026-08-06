//! Prints the box tree for an HTML file. A debugging aid.
//!
//! Run with `cargo run -p layout --example dump -- <file.html> [width]`.

fn walk(box_: &layout::LayoutBox, depth: usize, x: f32, y: f32) {
    let indent = "  ".repeat(depth);
    let tag = box_
        .node
        .map(|n| format!("#{}", n.0))
        .unwrap_or_else(|| "(anon)".to_owned());
    let text = box_
        .text
        .as_ref()
        .map(|t| format!(" text={} lines", t.lines.len()))
        .unwrap_or_default();
    println!(
        "{indent}{tag} x={:.0} y={:.0} w={:.0} h={:.0} content_w={:.0}{text}",
        x + box_.rect.x,
        y + box_.rect.y,
        box_.rect.width,
        box_.rect.height,
        box_.content_width,
    );
    for child in &box_.children {
        walk(child, depth + 1, x + box_.rect.x, y + box_.rect.y);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a file");
    let width: f32 = args.next().and_then(|w| w.parse().ok()).unwrap_or(800.0);

    let bytes = std::fs::read(&path).expect("readable");
    let (html, _, _) = net::encoding::decode_document(&bytes, None);
    let doc = dom::parse(&html);
    for node in doc.descendants(doc.root()) {
        if let Some(element) = doc.element(node) {
            println!("node #{} = <{}>", node.0, element.local_name());
        }
    }
    let styles = css::cascade::cascade(&doc, &[]);
    let mut fonts = text::FontStore::new();
    let laid_out = layout::layout(
        &doc,
        &styles,
        &mut fonts,
        &layout::IntrinsicSizes::new(),
        width,
    );
    println!("--- boxes");
    walk(&laid_out.root, 0, 0.0, 0.0);
}
