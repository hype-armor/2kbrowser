//! The rendering pipeline, from HTML source to pixels.
//!
//! Deliberately headless. Reference tests (ADR-0005) need to render on CI
//! machines with no display server, so the window is a thin consumer of this
//! rather than the only way to produce output.

use css::Stylesheet;
use layout::{IntrinsicSizes, RenderMode};
use net::{Fetcher, Origin, RequestKind};
use paint::{ImageStore, Pixmap, build_display_list, rasterise};
use text::FontStore;

/// A rendered page.
pub struct Page {
    /// The rasterised canvas.
    pub pixmap: Pixmap,
    /// Which mode the document was rendered in (ADR-0009).
    pub mode: RenderMode,
    /// Full content height in CSS pixels, which may exceed the canvas.
    pub content_height: f32,
    /// How many images were fetched and decoded.
    pub images_loaded: usize,
}

/// Renders HTML at a given viewport width.
///
/// `max_height` bounds the canvas so that a pathological page cannot allocate
/// an unbounded pixmap.
pub fn render(html: &str, width: u32, max_height: u32, fonts: &mut FontStore) -> Page {
    render_with_base(html, width, max_height, fonts, None)
}

/// Renders HTML, resolving subresources against the document's own URL.
///
/// Without a base there is nothing to resolve relative URLs against, so images
/// are simply not loaded — which is the right outcome for a bare HTML string.
pub fn render_with_base(
    html: &str,
    width: u32,
    max_height: u32,
    fonts: &mut FontStore,
    base: Option<(&Origin, &str)>,
) -> Page {
    let doc = dom::parse(html);

    // A frameset document has no body to lay out: each frame is a separate
    // page, fetched and rendered in its own right, then composited into the
    // window. Handled before styling because there is nothing here to style.
    if let Some(frameset) = doc.find_element("frameset")
        && let Some((origin, path)) = base
    {
        return render_frameset(&doc, frameset, width, max_height, fonts, origin, path, 0);
    }

    let author_sheets = collect_stylesheets(&doc);
    let styles = css::cascade::cascade(&doc, &author_sheets);

    // Classify before laying out: if the page needs layout we do not implement,
    // producing the wrong layout first and discarding it would be wasted work.
    let mode = layout::classify(&doc, &styles);

    let styles = match mode {
        RenderMode::Authored => styles,
        // Re-render as a document. The author's sheets are dropped entirely —
        // keeping them would reintroduce exactly the layout that failed — and
        // the reader sheet is applied over the UA defaults instead.
        RenderMode::Document { .. } | RenderMode::RequiresScripting => {
            let reader = Stylesheet::parse(css::ua::READER_STYLESHEET);
            css::cascade::cascade(&doc, &[reader])
        }
    };

    // Images are only loaded for the authored path: the document fallback
    // discards the author's layout, and pulling in its images with it would
    // spend requests on decoration nobody is going to see.
    let images = match (&mode, base) {
        (RenderMode::Authored, Some((origin, path))) => load_images(&doc, origin, path),
        _ => ImageStore::new(),
    };
    let intrinsic: IntrinsicSizes = images
        .iter()
        .map(|(node, image)| (*node, (image.width(), image.height())))
        .collect();

    let laid_out = layout::layout(&doc, &styles, fonts, &intrinsic, width as f32);
    let list = build_display_list(&laid_out);
    let height = (laid_out.height.ceil().max(1.0) as u32).min(max_height);
    let pixmap = rasterise(&list, fonts, &images, width, height)
        .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"));

    Page {
        pixmap,
        mode,
        content_height: laid_out.height,
        images_loaded: images.len(),
    }
}

/// How deeply framesets may nest before we stop following them.
///
/// A frameset can name itself, directly or through a cycle, and a browser that
/// followed that would fetch forever.
const MAX_FRAME_DEPTH: usize = 4;

/// Viewport height assumed for a frameset when none is supplied.
///
/// An ordinary page has an intrinsic height — its content — and `max_height` is
/// only a cap. A frameset has none: it *is* the viewport, and its rows are
/// shares of a height that has to come from somewhere. Headless rendering has
/// no window to ask, so it picks one rather than stretching frames down a
/// content-sized canvas.
const DEFAULT_FRAMESET_HEIGHT: u32 = 600;

/// Renders a frameset by rendering each frame and compositing the results.
#[expect(
    clippy::too_many_arguments,
    reason = "render context, threaded explicitly for clarity"
)]
fn render_frameset(
    doc: &dom::Document,
    frameset: dom::NodeId,
    width: u32,
    max_height: u32,
    fonts: &mut FontStore,
    origin: &Origin,
    path: &str,
    depth: usize,
) -> Page {
    let height = max_height.clamp(1, DEFAULT_FRAMESET_HEIGHT);
    let mut pixmap = Pixmap::new(width.max(1), height).expect("frameset canvas");
    pixmap.fill(paint::RasterColor::WHITE);

    let element = doc.element(frameset);
    let rows =
        layout::frameset::parse_spec(element.and_then(|e| e.attr("rows")).unwrap_or_default());
    let columns =
        layout::frameset::parse_spec(element.and_then(|e| e.attr("cols")).unwrap_or_default());
    let row_sizes = layout::frameset::distribute(&rows, height as f32);
    let column_sizes = layout::frameset::distribute(&columns, width as f32);
    let cells = layout::frameset::cells(&row_sizes, &column_sizes);

    // Only `frame` and nested `frameset` children occupy cells, in order.
    let children: Vec<dom::NodeId> = doc
        .children(frameset)
        .iter()
        .copied()
        .filter(|&child| {
            doc.element(child)
                .is_some_and(|e| matches!(e.local_name(), "frame" | "frameset"))
        })
        .collect();

    let fetcher = Fetcher::default();
    let mut loaded = 0usize;

    for (child, cell) in children.iter().zip(cells) {
        let (x, y, cell_width, cell_height) = cell;
        if cell_width < 1.0 || cell_height < 1.0 {
            continue;
        }
        let Some(element) = doc.element(*child) else {
            continue;
        };

        let sub = if element.local_name() == "frameset" {
            if depth + 1 > MAX_FRAME_DEPTH {
                continue;
            }
            render_frameset(
                doc,
                *child,
                cell_width as u32,
                cell_height as u32,
                fonts,
                origin,
                path,
                depth + 1,
            )
        } else {
            let Some(src) = element.attr("src") else {
                continue;
            };
            let url = net::resolve(origin, path, src);
            // A frame is a navigation to another document, not a subresource,
            // so it is not subject to the third-party rule (ADR-0006).
            let Ok(resource) = fetcher.fetch(&url, None, RequestKind::Navigation) else {
                continue;
            };
            if depth + 1 > MAX_FRAME_DEPTH {
                continue;
            }
            loaded += 1;
            render_with_base(
                &resource.body,
                cell_width as u32,
                cell_height as u32,
                fonts,
                Some((&resource.origin, &resource.path)),
            )
        };

        pixmap.draw_pixmap(
            x as i32,
            y as i32,
            sub.pixmap.as_ref(),
            &paint::PixmapPaint::default(),
            paint::Transform::identity(),
            None,
        );
    }

    Page {
        pixmap,
        mode: RenderMode::Authored,
        content_height: height as f32,
        images_loaded: loaded,
    }
}

/// Fetches and decodes every `<img>` the policy allows.
///
/// Failures are silent by design: a missing or corrupt image is an ordinary
/// thing to find on the web, and the element simply lays out at its declared
/// size with nothing drawn in it.
fn load_images(doc: &dom::Document, origin: &Origin, path: &str) -> ImageStore {
    let fetcher = Fetcher::default();
    let mut store = ImageStore::new();
    let mut cache: std::collections::HashMap<String, Option<paint::DecodedImage>> =
        std::collections::HashMap::new();

    for node in doc.descendants(doc.root()) {
        let Some(element) = doc.element(node) else {
            continue;
        };
        if element.local_name() != "img" {
            continue;
        }
        let Some(src) = element.attr("src") else {
            continue;
        };
        let url = net::resolve(origin, path, src);

        // The same image often appears many times on a page; fetching it once
        // matters more here than usual, since every fetch is synchronous.
        let decoded = cache.entry(url.clone()).or_insert_with(|| {
            // Subresource, so ADR-0006's third-party rule applies.
            let bytes = fetcher
                .fetch_bytes(&url, Some(origin), RequestKind::Subresource)
                .ok()?;
            paint::decode(&bytes)
        });
        if let Some(image) = decoded {
            store.insert(node, image.clone());
        }
    }
    store
}

/// Extracts the contents of every `<style>` element, in document order.
///
/// `<link rel=stylesheet>` is not followed here: fetching is the net crate's
/// job, and same-origin policy (ADR-0006) applies to it.
fn collect_stylesheets(doc: &dom::Document) -> Vec<Stylesheet> {
    doc.descendants(doc.root())
        .into_iter()
        .filter(|&node| doc.element(node).is_some_and(|e| e.local_name() == "style"))
        .map(|node| Stylesheet::parse(&doc.text_content(node)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_style_elements_are_applied() {
        let mut fonts = FontStore::new();
        let page = render(
            "<style>body { background-color: #00ff00 }</style><body>x</body>",
            20,
            100,
            &mut fonts,
        );
        let green = page
            .pixmap
            .pixels()
            .iter()
            .filter(|p| p.green() > 200 && p.red() < 60)
            .count();
        assert!(green > 0, "author stylesheet had no effect");
    }

    #[test]
    fn a_modern_page_is_re_rendered_as_a_document() {
        let body: String = (0..12)
            .map(|i| format!("<p>Paragraph {i} with a reasonable amount of text in it.</p>"))
            .collect();
        let html = format!(
            "<style>#app {{ display: flex }}</style><body><div id=\"app\">{body}</div></body>"
        );
        let mut fonts = FontStore::new();
        let page = render(&html, 600, 2000, &mut fonts);
        assert!(
            matches!(page.mode, RenderMode::Document { .. }),
            "got {:?}",
            page.mode
        );
        // The fallback must still produce a readable page, not an empty one.
        let ink = page
            .pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() != 255 || p.green() != 255 || p.blue() != 255)
            .count();
        assert!(ink > 100, "document fallback rendered nothing");
    }

    #[test]
    fn an_ordinary_page_keeps_its_authored_layout() {
        let mut fonts = FontStore::new();
        let page = render(
            "<body><p>Just an ordinary page.</p></body>",
            400,
            500,
            &mut fonts,
        );
        assert_eq!(page.mode, RenderMode::Authored);
    }

    #[test]
    fn the_canvas_is_bounded_by_max_height() {
        let body: String = (0..500).map(|i| format!("<p>Line {i}</p>")).collect();
        let mut fonts = FontStore::new();
        let page = render(&format!("<body>{body}</body>"), 300, 400, &mut fonts);
        assert_eq!(page.pixmap.height(), 400);
        assert!(
            page.content_height > 400.0,
            "content should exceed the canvas"
        );
    }
}
