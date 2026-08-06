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

/// One document occupying a rectangle of the canvas.
///
/// An ordinary page has exactly one, covering the whole canvas. A frameset has
/// one per frame, and they are genuinely separate documents — a link in a frame
/// resolves against *that* frame's URL, not the frameset's, which is why the
/// origin and path travel with it rather than sitting on the page.
pub struct Frame {
    /// Where this document sits on the canvas.
    pub rect: layout::Rect,
    /// The parsed document.
    pub doc: dom::Document,
    /// Its layout, in the frame's own coordinates.
    pub layout: layout::Layout,
    /// Origin it was fetched from, for resolving links inside it.
    pub origin: Origin,
    /// Path it was fetched from.
    pub path: String,
}

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
    /// The documents on this canvas, in paint order.
    pub frames: Vec<Frame>,
    /// The page's `<title>`, collapsed and trimmed.
    ///
    /// `None` when the document has none, which is common on the era's pages
    /// and on anything hand-written — the caller falls back to the URL rather
    /// than showing an empty tab.
    pub title: Option<String>,
}

impl Page {
    /// The absolute URL of the link at a point, in canvas coordinates.
    ///
    /// Resolved here rather than handed back raw because the answer depends on
    /// which frame was hit, and the caller has no way to know that.
    pub fn link_at(&self, x: f32, y: f32) -> Option<String> {
        // Reverse order: later frames are painted over earlier ones.
        for frame in self.frames.iter().rev() {
            if x < frame.rect.x
                || x >= frame.rect.x + frame.rect.width
                || y < frame.rect.y
                || y >= frame.rect.y + frame.rect.height
            {
                continue;
            }
            let hit = frame.layout.hit_test(x - frame.rect.x, y - frame.rect.y)?;
            let (_, href) = frame.doc.enclosing_link(hit)?;
            // A fragment alone is a destination within this document, not a
            // navigation; there is nothing to fetch.
            if href.starts_with('#') {
                return None;
            }
            return Some(net::resolve(&frame.origin, &frame.path, href));
        }
        None
    }

    /// Every rectangle where `query` appears, in canvas coordinates.
    ///
    /// Across every frame, because a frameset's content is as much the page as
    /// an ordinary document's is — a reader searching a framed site does not
    /// care which cell the words are in.
    pub fn find(&self, query: &str) -> Vec<layout::Rect> {
        let mut out = Vec::new();
        for frame in &self.frames {
            out.extend(frame.layout.find(query).into_iter().map(|mut rect| {
                rect.x += frame.rect.x;
                rect.y += frame.rect.y;
                rect
            }));
        }
        out
    }

    /// Every link rectangle on the canvas, with the URL it leads to.
    ///
    /// What a keyboard-first browser needs: something to number, highlight, and
    /// jump between without a pointer ever being involved.
    pub fn links(&self) -> Vec<(layout::Rect, String)> {
        self.link_groups()
            .into_iter()
            .flat_map(|link| {
                link.rects
                    .into_iter()
                    .map(move |rect| (rect, link.url.clone()))
            })
            .collect()
    }

    /// The same links, with each one's rectangles kept together.
    ///
    /// A link that wraps across a line break is several rectangles and one
    /// destination. Keyboard focus has to move link by link — stepping through
    /// rectangles would stop twice inside one link and look like nothing
    /// happened — so the grouping cannot be recovered afterwards and is kept.
    ///
    /// Document order, which is the order a reader would meet them in.
    pub fn link_groups(&self) -> Vec<Link> {
        let mut out = Vec::new();
        for frame in &self.frames {
            for node in frame.doc.descendants(frame.doc.root()) {
                let Some((link, href)) = frame.doc.enclosing_link(node) else {
                    continue;
                };
                if link != node || href.starts_with('#') {
                    continue;
                }
                let rects: Vec<layout::Rect> = frame
                    .layout
                    .rects_for(node)
                    .into_iter()
                    .map(|mut rect| {
                        rect.x += frame.rect.x;
                        rect.y += frame.rect.y;
                        rect
                    })
                    .collect();
                if rects.is_empty() {
                    continue;
                }
                out.push(Link {
                    rects,
                    url: net::resolve(&frame.origin, &frame.path, href),
                });
            }
        }
        out
    }
}

/// One link: everywhere it is on the canvas, and where it leads.
#[derive(Debug, Clone)]
pub struct Link {
    /// Its rectangles. More than one when it wraps across a line break.
    pub rects: Vec<layout::Rect>,
    /// The absolute URL it leads to.
    pub url: String,
}

impl Link {
    /// The rectangle enclosing all of this link's pieces.
    ///
    /// What scrolling to it uses: bringing the first fragment of a wrapped link
    /// into view can leave the rest of it off screen.
    pub fn bounds(&self) -> layout::Rect {
        let mut bounds = self.rects[0];
        for rect in &self.rects[1..] {
            let right = (bounds.x + bounds.width).max(rect.x + rect.width);
            let bottom = (bounds.y + bounds.height).max(rect.y + rect.height);
            bounds.x = bounds.x.min(rect.x);
            bounds.y = bounds.y.min(rect.y);
            bounds.width = right - bounds.x;
            bounds.height = bottom - bounds.y;
        }
        bounds
    }
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
    render_sized(html, width, max_height, Settings::default(), fonts, base)
}

/// Renders HTML into a canvas of exactly `height`, whatever the content needs.
///
/// This is what a frame gets: a frame is a viewport in its own right, so its
/// document's canvas is the cell it was given rather than however tall the
/// document happened to be. Shrinking to the content instead would leave the
/// rest of the cell showing whatever was underneath — which, for a page with a
/// background of its own, means a band of the wrong colour.
pub fn render_in_viewport(
    html: &str,
    width: u32,
    height: u32,
    fonts: &mut FontStore,
    base: Option<(&Origin, &str)>,
) -> Page {
    render_sized(
        html,
        width,
        height,
        Settings {
            fill_height: true,
            ..Settings::default()
        },
        fonts,
        base,
    )
}

/// Renders with the author's layout even when classification says not to.
///
/// The override ADR-0009 requires: the fallback is automatic, and the reader
/// can always overrule it and see what the author actually wrote. A browser
/// that decides for you and gives you no way to look is worse than one that
/// gets the decision wrong.
pub fn render_as_authored(
    html: &str,
    width: u32,
    max_height: u32,
    fonts: &mut FontStore,
    base: Option<(&Origin, &str)>,
) -> Page {
    render_sized(
        html,
        width,
        max_height,
        Settings {
            force_authored: true,
            ..Settings::default()
        },
        fonts,
        base,
    )
}

/// How to render, beyond the document itself.
#[derive(Debug, Clone, Copy, Default)]
struct Settings {
    /// Fill the canvas to the height given rather than shrinking to content.
    fill_height: bool,
    /// Use the author's layout whatever classification decided.
    force_authored: bool,
}

fn render_sized(
    html: &str,
    width: u32,
    max_height: u32,
    settings: Settings,
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

    let author_sheets = collect_stylesheets(&doc, base);
    let styles = css::cascade::cascade(&doc, &author_sheets);

    // Classify before laying out: if the page needs layout we do not implement,
    // producing the wrong layout first and discarding it would be wasted work.
    let mode = if settings.force_authored {
        // The reader asked to see what the author wrote. Classification still
        // ran — the answer is just not being acted on.
        RenderMode::Authored
    } else {
        layout::classify(&doc, &styles)
    };

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
        (RenderMode::Authored, Some((origin, path))) => load_images(&doc, &styles, origin, path),
        _ => ImageStore::new(),
    };
    // Only content images have an intrinsic size layout cares about: a
    // background tile is drawn at its natural size and never sizes its box.
    let intrinsic: IntrinsicSizes = images
        .iter()
        .filter(|(key, _)| key.slot == paint::ImageSlot::Content)
        .map(|(key, image)| (key.node, (image.width(), image.height())))
        .collect();

    let laid_out = layout::layout(&doc, &styles, fonts, &intrinsic, width as f32);
    let list = build_display_list(&laid_out);
    let height = if settings.fill_height {
        max_height.max(1)
    } else {
        (laid_out.height.ceil().max(1.0) as u32).min(max_height)
    };
    let pixmap = rasterise(&list, fonts, &images, width, height)
        .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"));

    // The whole canvas is one document. `base` is what a link inside it
    // resolves against; without one there is nothing to resolve against and
    // nothing to navigate to, so the frame carries no link geometry.
    let title = document_title(&doc);
    let content_height = laid_out.height;
    let frames = match base {
        Some((origin, path)) => vec![Frame {
            rect: layout::Rect {
                x: 0.0,
                y: 0.0,
                width: width as f32,
                // The whole canvas, and the whole content if that is taller:
                // a frame shorter than its cell must still be clickable to the
                // bottom of the cell, and a page taller than its canvas must
                // still be clickable once scrolled.
                height: content_height.max(height as f32),
            },
            doc,
            layout: laid_out,
            origin: origin.clone(),
            path: path.to_owned(),
        }],
        None => Vec::new(),
    };

    Page {
        pixmap,
        mode,
        content_height,
        images_loaded: images.len(),
        title,
        frames,
    }
}

/// Reads a document's `<title>`.
///
/// Collapsed and trimmed because the markup's line breaks and indentation are
/// not part of the title, and a title with a newline in it makes a mess of
/// every place one is shown.
fn document_title(doc: &dom::Document) -> Option<String> {
    let node = doc.find_element("title")?;
    let text = layout::collapse_whitespace(&doc.text_content(node));
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
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
    let mut frames: Vec<Frame> = Vec::new();

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
            render_in_viewport(
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

        // The sub-page's frames move into this one's coordinates. A nested
        // frameset arrives with several of its own, already flattened, so the
        // depth of the nesting does not reach the caller.
        frames.extend(sub.frames.into_iter().map(|mut frame| {
            frame.rect.x += x;
            frame.rect.y += y;
            frame
        }));
    }

    Page {
        pixmap,
        mode: RenderMode::Authored,
        content_height: height as f32,
        images_loaded: loaded,
        // The frameset document's own title, not any frame's: a frame is a
        // part of the page, and its title is not the page's.
        title: document_title(doc),
        frames,
    }
}

/// Fetches and decodes every `<img>` the policy allows.
///
/// Failures are silent by design: a missing or corrupt image is an ordinary
/// thing to find on the web, and the element simply lays out at its declared
/// size with nothing drawn in it.
fn load_images(
    doc: &dom::Document,
    styles: &css::cascade::StyleMap,
    origin: &Origin,
    path: &str,
) -> ImageStore {
    let fetcher = Fetcher::default();
    let mut store = ImageStore::new();
    let mut cache: std::collections::HashMap<String, Option<paint::DecodedImage>> =
        std::collections::HashMap::new();

    // The same image often appears many times on a page — a tile appears on
    // every cell of a table — so fetching each URL once matters more here than
    // usual, since every fetch is synchronous.
    let load = |url: &str, cache: &mut std::collections::HashMap<_, _>| {
        cache
            .entry(url.to_owned())
            .or_insert_with(|| {
                // Subresource, so ADR-0006's third-party rule applies.
                let bytes = fetcher
                    .fetch_bytes(url, Some(origin), RequestKind::Subresource)
                    .ok()?;
                paint::decode(&bytes)
            })
            .clone()
    };

    for node in doc.descendants(doc.root()) {
        let Some(element) = doc.element(node) else {
            continue;
        };
        if element.local_name() == "img"
            && let Some(src) = element.attr("src")
        {
            let url = net::resolve(origin, path, src);
            if let Some(image) = load(&url, &mut cache) {
                store.insert(paint::ImageKey::content(node), image);
            }
        }
        if let Some(source) = styles
            .get(node)
            .and_then(|style| style.background_image.as_deref())
        {
            let url = net::resolve(origin, path, source);
            if let Some(image) = load(&url, &mut cache) {
                store.insert(paint::ImageKey::background(node), image);
            }
        }
    }
    store
}

/// Extracts the contents of every `<style>` element, in document order.
///
/// `<link rel=stylesheet>` is not followed here: fetching is the net crate's
/// job, and same-origin policy (ADR-0006) applies to it.
/// How deeply `@import` may nest before we stop following it.
///
/// A stylesheet can import itself, directly or through a cycle, and a browser
/// that followed that would fetch forever.
const MAX_IMPORT_DEPTH: usize = 4;

/// Adds a sheet, with everything it imports placed *before* it.
///
/// Order is the point: CSS says an imported sheet's rules come before the
/// importing sheet's own, so a rule in the importing sheet overrides the one it
/// imported. Appending them afterwards inverts every such override.
fn push_with_imports(
    sheets: &mut Vec<Stylesheet>,
    sheet: Stylesheet,
    fetcher: &Fetcher,
    base: Option<(&Origin, &str)>,
    depth: usize,
) {
    if depth < MAX_IMPORT_DEPTH
        && let Some((origin, path)) = base
    {
        for href in &sheet.imports {
            let url = net::resolve(origin, path, href);
            let Ok(resource) = fetcher.fetch(&url, Some(origin), RequestKind::Subresource) else {
                continue;
            };
            // The imported sheet's own imports resolve against *it*, not
            // against whatever imported it.
            push_with_imports(
                sheets,
                Stylesheet::parse(&resource.body),
                fetcher,
                Some((&resource.origin, &resource.path)),
                depth + 1,
            );
        }
    }
    sheets.push(sheet);
}

/// Whether a `<link rel>` names a stylesheet this browser should apply.
///
/// `rel` is a space-separated list and the era's markup puts other tokens
/// beside `stylesheet` freely. An `alternate` sheet is one the reader may
/// choose rather than one to apply, and there is no UI to choose with — so it
/// is skipped rather than applied on top of the real one.
fn is_applied_stylesheet(rel: Option<&str>) -> bool {
    let Some(rel) = rel else { return false };
    let mut stylesheet = false;
    for token in rel.split_ascii_whitespace() {
        if token.eq_ignore_ascii_case("alternate") {
            return false;
        }
        stylesheet |= token.eq_ignore_ascii_case("stylesheet");
    }
    stylesheet
}

fn collect_stylesheets(doc: &dom::Document, base: Option<(&Origin, &str)>) -> Vec<Stylesheet> {
    let fetcher = Fetcher::default();
    let mut sheets = Vec::new();

    for node in doc.descendants(doc.root()) {
        let Some(element) = doc.element(node) else {
            continue;
        };
        match element.local_name() {
            "style" => {
                let sheet = Stylesheet::parse(&doc.text_content(node));
                // A `<style>` block's imports resolve against the document.
                push_with_imports(&mut sheets, sheet, &fetcher, base, 0);
            }
            // An external stylesheet is how a site of this era shared one look
            // across every page; skipping them leaves those pages unstyled.
            "link" => {
                if !is_applied_stylesheet(element.attr("rel")) {
                    continue;
                }
                let Some((origin, path)) = base else { continue };
                let Some(href) = element.attr("href") else {
                    continue;
                };
                let url = net::resolve(origin, path, href);
                // Subresource, so ADR-0006's third-party rule applies: a sheet
                // from another origin is refused like any other.
                if let Ok(resource) = fetcher.fetch(&url, Some(origin), RequestKind::Subresource) {
                    let sheet = Stylesheet::parse(&resource.body);
                    // An imported sheet's URLs resolve against the sheet that
                    // imported it, not against the document.
                    push_with_imports(
                        &mut sheets,
                        sheet,
                        &fetcher,
                        Some((&resource.origin, &resource.path)),
                        0,
                    );
                }
            }
            _ => {}
        }
    }
    sheets
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

    /// Colour of the canvas's bottom-left pixel, past any content.
    fn bottom_left(page: &Page) -> (u8, u8, u8) {
        let y = page.pixmap.height() - 1;
        let pixel = page.pixmap.pixels()[(y * page.pixmap.width()) as usize];
        (pixel.red(), pixel.green(), pixel.blue())
    }

    #[test]
    fn a_page_background_reaches_the_bottom_of_the_viewport() {
        // CSS 2.1 §14.2: the background covers the canvas, not just the box.
        // A page is nearly always shorter than the window showing it, so
        // getting this wrong ends every such page in a band of white.
        let mut fonts = FontStore::new();
        let page = render_in_viewport(
            r##"<body bgcolor="#ff0000">short</body>"##,
            50,
            400,
            &mut fonts,
            None,
        );
        assert_eq!(
            page.pixmap.height(),
            400,
            "a viewport is filled, not shrunk"
        );
        assert!(
            page.content_height < 400.0,
            "the content must be shorter than the canvas for this to test anything"
        );
        assert_eq!(bottom_left(&page), (255, 0, 0));
    }

    #[test]
    fn the_root_background_wins_over_the_body_one() {
        // Only when the root has none does the body's get propagated.
        let mut fonts = FontStore::new();
        let page = render_in_viewport(
            "<style>html { background: #0000ff } body { background: #ff0000 }</style>\
             <body>short</body>",
            50,
            300,
            &mut fonts,
            None,
        );
        assert_eq!(bottom_left(&page), (0, 0, 255));
    }

    #[test]
    fn a_page_with_no_background_still_gets_an_opaque_canvas() {
        let mut fonts = FontStore::new();
        let page = render_in_viewport("<body>short</body>", 50, 300, &mut fonts, None);
        assert_eq!(bottom_left(&page), (255, 255, 255));
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

#[cfg(test)]
mod link_tests {
    use super::is_applied_stylesheet;

    #[test]
    fn a_stylesheet_link_is_applied() {
        assert!(is_applied_stylesheet(Some("stylesheet")));
        // Case-insensitive, and other tokens beside it are ordinary.
        assert!(is_applied_stylesheet(Some("StyleSheet")));
        assert!(is_applied_stylesheet(Some("preload stylesheet")));
    }

    #[test]
    fn other_link_relations_are_not_stylesheets() {
        for rel in ["icon", "shortcut icon", "next", "canonical", ""] {
            assert!(!is_applied_stylesheet(Some(rel)), "applied {rel:?}");
        }
        assert!(!is_applied_stylesheet(None), "a link with no rel");
    }

    #[test]
    fn an_alternate_stylesheet_is_skipped() {
        // It is one the reader may choose, not one to apply. Applying it as
        // well as the real sheet gives a page both looks at once.
        assert!(!is_applied_stylesheet(Some("alternate stylesheet")));
        assert!(!is_applied_stylesheet(Some("stylesheet alternate")));
    }
}

#[cfg(test)]
mod link_geometry_tests {
    use super::*;

    /// Renders `html` as if it were a file in a real directory, so relative
    /// links have something to resolve against.
    fn page_at(name: &str, html: &str) -> (Page, String) {
        let dir = std::env::temp_dir().join("2kbrowser-link-tests");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(name);
        std::fs::write(&path, html).expect("write");
        // Through `net::file_url` rather than pasting the OS path after
        // `file://`. On Windows a path is `C:\dir\a.html`, and splitting that
        // on `/` to get the document's directory finds the second slash of
        // `file://` and yields `file:/` — so the value this test compares
        // against was nonsense on one platform and right on the other two.
        let url = net::file_url(&path);
        let (origin, path) = net::parse_url(&url).expect("parses");

        let mut fonts = FontStore::new();
        let page = render_with_base(html, 600, 2000, &mut fonts, Some((&origin, &path)));
        let base = url.rsplit_once('/').expect("a directory").0.to_owned();
        (page, base)
    }

    /// The URL reported at the centre of the first link rectangle.
    fn follow_first_link(page: &Page) -> Option<String> {
        let rect = page.links().first().map(|(rect, _)| *rect)?;
        page.link_at(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
    }

    #[test]
    fn clicking_a_relative_link_resolves_it_against_the_page() {
        let (page, base) = page_at(
            "a.html",
            r#"<body><p>go <a href="b.html">there</a></p></body>"#,
        );
        assert_eq!(follow_first_link(&page), Some(format!("{base}/b.html")));
    }

    #[test]
    fn a_fragment_link_is_not_a_navigation() {
        // It names a destination inside this document. There is nothing to
        // fetch, and treating it as a fetch reloads the page for no reason.
        let (page, _) = page_at(
            "frag.html",
            r##"<body><p><a href="#section">jump</a></p></body>"##,
        );
        assert!(
            page.links().is_empty(),
            "a fragment is not a link to follow"
        );

        let rects = page.frames[0].layout.rects_for(
            page.frames[0]
                .doc
                .find_element("a")
                .expect("the anchor exists"),
        );
        let rect = rects.first().expect("it still has geometry");
        assert_eq!(
            page.link_at(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0),
            None
        );
    }

    #[test]
    fn a_wrapped_link_is_one_link_with_several_rectangles() {
        // Keyboard focus moves link by link. Stepping through rectangles would
        // stop twice inside one link and look like the key did nothing.
        let (page, _) = page_at(
            "wrapped.html",
            &format!(
                "<body><p><a href=\"b.html\">{}</a></p></body>",
                "a long link that has to wrap ".repeat(6)
            ),
        );
        let groups = page.link_groups();
        assert_eq!(groups.len(), 1, "one link");
        assert!(
            groups[0].rects.len() > 1,
            "it should have wrapped: {:?}",
            groups[0].rects
        );
        assert_eq!(
            page.links().len(),
            groups[0].rects.len(),
            "and the flat list still has every rectangle"
        );
    }

    #[test]
    fn a_links_bounds_enclose_all_of_its_pieces() {
        // Scrolling to the first fragment of a wrapped link can leave the rest
        // of it off screen.
        let (page, _) = page_at(
            "bounds.html",
            &format!(
                "<body><p><a href=\"b.html\">{}</a></p></body>",
                "wrap me around several lines please ".repeat(6)
            ),
        );
        let link = page.link_groups().pop().expect("a link");
        let bounds = link.bounds();
        for rect in &link.rects {
            assert!(rect.x >= bounds.x, "{rect:?} vs {bounds:?}");
            assert!(rect.y >= bounds.y, "{rect:?} vs {bounds:?}");
            assert!(rect.x + rect.width <= bounds.x + bounds.width, "{rect:?}");
            assert!(rect.y + rect.height <= bounds.y + bounds.height, "{rect:?}");
        }
        assert!(bounds.height > link.rects[0].height, "more than one line");
    }

    #[test]
    fn links_come_back_in_document_order() {
        // The order a reader would meet them in, which is what Tab has to
        // follow.
        let (page, base) = page_at(
            "order.html",
            r#"<body><p><a href="one.html">one</a> <a href="two.html">two</a></p>
               <p><a href="three.html">three</a></p></body>"#,
        );
        let urls: Vec<String> = page.link_groups().into_iter().map(|l| l.url).collect();
        assert_eq!(
            urls,
            vec![
                format!("{base}/one.html"),
                format!("{base}/two.html"),
                format!("{base}/three.html"),
            ]
        );
    }

    #[test]
    fn a_point_on_ordinary_text_is_not_a_link() {
        let (page, _) = page_at(
            "plain.html",
            r#"<body><p>just words here, and <a href="b.html">one link</a></p></body>"#,
        );
        // Far to the right of the text, on the same line.
        assert_eq!(page.link_at(580.0, 12.0), None);
    }

    #[test]
    fn a_page_with_no_base_has_no_link_geometry() {
        // Nothing to resolve against, so there is nowhere a click could lead.
        let mut fonts = FontStore::new();
        let page = render(
            r#"<body><a href="b.html">x</a></body>"#,
            300,
            300,
            &mut fonts,
        );
        assert!(page.frames.is_empty());
        assert!(page.links().is_empty());
        assert_eq!(page.link_at(5.0, 5.0), None);
    }
}

#[cfg(test)]
mod title_tests {
    use super::*;

    fn title_of(html: &str) -> Option<String> {
        let mut fonts = FontStore::new();
        render(html, 300, 300, &mut fonts).title
    }

    #[test]
    fn a_title_is_read() {
        assert_eq!(
            title_of("<html><head><title>A Page</title></head><body>x</body></html>"),
            Some("A Page".to_owned())
        );
    }

    #[test]
    fn a_title_is_collapsed_and_trimmed() {
        // Markup indentation is not part of the title, and a title with a
        // newline in it makes a mess of every place one is shown.
        assert_eq!(
            title_of("<html><head><title>\n   The Node\n   & Nib\n  </title></head></html>"),
            Some("The Node & Nib".to_owned())
        );
    }

    #[test]
    fn a_page_with_no_title_has_none() {
        // Common on the era's pages, and on anything hand-written.
        assert_eq!(title_of("<html><body>x</body></html>"), None);
        assert_eq!(
            title_of("<html><head><title>   </title></head></html>"),
            None,
            "a title of only whitespace is no title"
        );
    }

    #[test]
    fn entities_in_a_title_are_decoded() {
        assert_eq!(
            title_of("<html><head><title>Node &amp; Nib &#8212; 1998</title></head></html>"),
            Some("Node & Nib — 1998".to_owned())
        );
    }
}
