//! The rendering pipeline, from HTML source to pixels.
//!
//! Deliberately headless. Reference tests (ADR-0005) need to render on CI
//! machines with no display server, so the window is a thin consumer of this
//! rather than the only way to produce output.

use std::collections::HashMap;

use css::Stylesheet;
use layout::{IntrinsicSizes, RenderMode};
use net::{Fetcher, Origin, RequestKind};
use paint::{ImageStore, Pixmap, build_display_list};
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
    /// The document row `pixmap` starts at.
    pub band_top: u32,
    /// The colour the canvas was cleared to, opaque (CSS 2.1 §14.2).
    ///
    /// The window needs it for the rows it has no pixels for — below a page
    /// shorter than the window, and ahead of a band still being painted. White
    /// there is fine under a white page and a bright hole under any other,
    /// which the dark document rendering makes impossible to miss.
    pub background: css::Color,
    /// What another band would be painted from, when this page can paint one.
    ///
    /// Kept so that scrolling costs a paint rather than a re-layout: the
    /// display list is in document coordinates and does not change between
    /// bands. `None` for a frameset, whose canvas is composited from its
    /// frames rather than built from one list — and which is never taller than
    /// its own viewport, so no band beyond the one it has is ever asked for.
    source: Option<Box<BandSource>>,
}

/// Everything painting a band of a page needs.
struct BandSource {
    list: paint::DisplayList,
    images: paint::ImageStore,
}

impl Page {
    /// Paints a different band of this page, without laying it out again.
    ///
    /// This is what makes a long page affordable: the parse, the cascade, and
    /// the layout all stay done, and moving down the document costs only the
    /// pixels asked for.
    ///
    /// `None` when this page cannot repaint — a frameset — which is safe
    /// because a frameset's canvas is its viewport and never has rows beyond
    /// the ones it already holds.
    pub fn paint_band(&self, fonts: &mut FontStore, top: u32, height: u32) -> Option<Pixmap> {
        let source = self.source.as_ref()?;
        // Clipped to what the document has below `top`, the same way a first
        // render is clipped to its content. A band running off the bottom
        // otherwise comes back padded with canvas colour, and those rows are
        // not rows of the document — they would scroll past the end.
        let content_rows = self.content_height.ceil().max(1.0) as u32;
        let height = height.min(content_rows.saturating_sub(top)).max(1);
        paint::rasterise_band(
            &source.list,
            fonts,
            &source.images,
            self.pixmap.width(),
            top as f32,
            height,
        )
    }

    /// Whether this page can paint a band other than the one it holds.
    pub fn can_paint_bands(&self) -> bool {
        self.source.is_some()
    }

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

    /// What lies between two points on the canvas, and where it is.
    ///
    /// Only within one frame — the one the drag started in. A selection that
    /// ran from a frameset's sidebar into its article would be two documents'
    /// text with nothing to say where one ended, which is not what anyone
    /// means by dragging across a page.
    pub fn select(&self, from: (f32, f32), to: (f32, f32)) -> layout::Selection {
        for frame in self.frames.iter().rev() {
            let inside = from.0 >= frame.rect.x
                && from.0 < frame.rect.x + frame.rect.width
                && from.1 >= frame.rect.y
                && from.1 < frame.rect.y + frame.rect.height;
            if !inside {
                continue;
            }
            let local = |(x, y): (f32, f32)| (x - frame.rect.x, y - frame.rect.y);
            let mut selection = frame.layout.select(local(from), local(to));
            for rect in &mut selection.rects {
                rect.x += frame.rect.x;
                rect.y += frame.rect.y;
            }
            return selection;
        }
        layout::Selection::default()
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
                if link != node {
                    continue;
                }
                // The whole subtree, not the `<a>` alone. An inline link
                // generates no box of its own and is found through the text
                // spans that name it — and a span names the element the text
                // is *directly* in. Wikipedia writes
                // `<a href="#cite_note-1"><span class="mw-reflink-text">[1]</span></a>`,
                // so asking only about the anchor came back with nothing at
                // all: no rectangle, so no entry in the link list, so no
                // cursor, no keyboard focus, and nothing under the pointer for
                // a click to land on (#52). The window hit-tests against this
                // list, not against the box tree — the box tree is in another
                // process — so a link missing from it is a link that does not
                // work.
                let rects: Vec<layout::Rect> = frame
                    .doc
                    .descendants(node)
                    .into_iter()
                    .flat_map(|inside| frame.layout.rects_for(inside))
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
                    jump_to: jump_to(frame, href),
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
    /// Where on this page it goes, for a link that does not leave it.
    ///
    /// `href="#Etymology"` is not a page to fetch; it is a place on the page
    /// already open. These were being dropped on the floor — left out of the
    /// link list entirely, so they took no cursor, took no keyboard focus, and
    /// did nothing at all when clicked. On Wikipedia that is "Jump to
    /// content", every entry in the contents, and every footnote marker in the
    /// article (#52).
    ///
    /// Carried on the link rather than looked up when it is followed, because
    /// the answer is in the box tree and by then the box tree is in another
    /// process. It costs one optional float per link and saves a round trip
    /// per click.
    ///
    /// `None` for a link that leaves the page, and also for a fragment naming
    /// something this page does not have — a stale anchor is a link to
    /// nowhere, and the honest thing is to do nothing rather than to guess.
    pub jump_to: Option<f32>,
}

/// Where a fragment link lands on the canvas, if it is one and it lands.
fn jump_to(frame: &Frame, href: &str) -> Option<f32> {
    let name = href.strip_prefix('#')?;
    // `href="#"` names the document itself — HTML calls the top of the page
    // the indicated part when the fragment is empty. It is a common enough
    // spelling of "back to the top" that treating it as a link to nowhere
    // would leave a visibly dead link on the page.
    if name.is_empty() {
        return Some(frame.rect.y);
    }
    let target = frame.doc.fragment_target(name)?;
    // The topmost edge of everything under the target, for the same reason the
    // link's own rectangles are gathered that way: an inline element generates
    // no box, and its text belongs to whatever is directly around it. A
    // Wikipedia footnote's back-link points at
    // `<sup id="cite_ref-1"><a>…</a></sup>`, and asking the `<sup>` alone
    // about its geometry comes back with none.
    //
    // The minimum rather than the first: an anchor that wrapped across lines
    // has a rectangle per line, and landing on its last one would put the
    // start of it above the window.
    let top = frame
        .doc
        .descendants(target)
        .into_iter()
        .flat_map(|inside| frame.layout.rects_for(inside))
        .map(|rect| rect.y)
        .reduce(f32::min)?;
    Some(frame.rect.y + top)
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

/// Where a page's subresources come from.
///
/// Rendering used to reach for a [`Fetcher`] wherever it wanted a stylesheet,
/// an image, or a frame. That is fine in a single process and impossible in
/// two: ADR-0012 puts rendering in a child with no sockets, so every one of
/// those has to become a request the parent decides on.
///
/// A parameter rather than two code paths. The reference tests and the
/// command line render in-process with a [`DirectLoader`]; the child renders
/// with one that goes over a pipe. Same rendering code either way, which is
/// the point — a second path would be the one that drifts.
pub trait Loader {
    /// Fetches a subresource, or `None` when it could not be had.
    ///
    /// Deliberately opaque about *why* not. Refused by policy, missing, and
    /// unreachable are one answer here, because rendering behaves identically
    /// for all three and because telling the untrusted side which would leak
    /// the parent's configuration to it.
    fn load(&mut self, url: &str, document: Option<&Origin>, kind: RequestKind) -> Option<Loaded>;

    /// Fetches several subresources at once, answering one per URL in order.
    ///
    /// Worth asking for as a group wherever the caller knows a group: across a
    /// process boundary the parent can fetch them concurrently, which is the
    /// difference between waiting for the sum of a page's latencies and waiting
    /// for the longest of them.
    ///
    /// Defaults to asking one at a time, which is all an in-process loader can
    /// usefully do — there is no boundary to overlap across, and the reference
    /// tests and the command line go through this path.
    fn load_many(
        &mut self,
        urls: &[String],
        document: Option<&Origin>,
        kind: RequestKind,
    ) -> Vec<Option<Loaded>> {
        urls.iter()
            .map(|url| self.load(url, document, kind))
            .collect()
    }
}

/// A fetched subresource.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// The `Content-Type` it was served with, when there was one.
    ///
    /// Carried because a stylesheet's character set can come from the header,
    /// and losing it would silently change how a legacy stylesheet decodes.
    pub content_type: Option<String>,
}

impl Loaded {
    /// The bytes decoded as text, the way a document body is.
    fn text(&self) -> String {
        let (text, ..) = net::encoding::decode_document(&self.bytes, self.content_type.as_deref());
        text
    }
}

/// Loads subresources in this process, subject to the network policy.
///
/// What the command line and the reference tests use. The browser itself does
/// not: its rendering happens in a child that has no network at all.
#[derive(Debug, Default)]
pub struct DirectLoader {
    fetcher: Fetcher,
}

impl Loader for DirectLoader {
    fn load(&mut self, url: &str, document: Option<&Origin>, kind: RequestKind) -> Option<Loaded> {
        let resource = self.fetcher.fetch(url, document, kind).ok()?;
        Some(Loaded {
            bytes: resource.bytes,
            content_type: None,
        })
    }
}

/// How close the page's content may come to the edge of the window.
///
/// `body { margin: 0 }` is in nearly every modern stylesheet, and those pages
/// were written for a window with a scrollbar down one side and browser chrome
/// around the rest — not for a viewport that ends where the glass does. Taken
/// literally, the declaration puts the first letter of every line hard against
/// the window frame, which is unpleasant to read and looks like a bug.
///
/// So the page keeps a gutter whatever it asks for. Eight pixels, matching the
/// UA sheet's own body margin: enough to read against, not enough to be a
/// second opinion about the page's design.
const PAGE_GUTTER: f32 = 8.0;

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
    render_with_base_and_loader(
        html,
        width,
        0,
        max_height,
        fonts,
        &mut DirectLoader::default(),
        base,
    )
}

/// The same, with the caller supplying where subresources come from.
///
/// What the renderer child uses: its loader goes over a pipe to the parent
/// rather than to a socket (ADR-0012).
pub fn render_with_base_and_loader(
    html: &str,
    width: u32,
    band_top: u32,
    band_height: u32,
    fonts: &mut FontStore,
    loader: &mut dyn Loader,
    base: Option<(&Origin, &str)>,
) -> Page {
    render_sized(
        html,
        width,
        band_top,
        band_height,
        Settings::default(),
        fonts,
        loader,
        base,
    )
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
    render_in_viewport_with(
        html,
        width,
        height,
        fonts,
        &mut DirectLoader::default(),
        base,
    )
}

/// The same, with the caller supplying where subresources come from.
fn render_in_viewport_with(
    html: &str,
    width: u32,
    height: u32,
    fonts: &mut FontStore,
    loader: &mut dyn Loader,
    base: Option<(&Origin, &str)>,
) -> Page {
    render_sized(
        html,
        width,
        0,
        height,
        Settings {
            fill_height: true,
            ..Settings::default()
        },
        fonts,
        loader,
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
    render_as_authored_with(
        html,
        width,
        0,
        max_height,
        fonts,
        &mut DirectLoader::default(),
        base,
    )
}

/// Renders as the document fallback whatever classification decided.
///
/// The counterpart to `render_as_authored_with`, for a reader who wants the
/// simplified layout on a page that renders perfectly well without it.
pub fn render_as_document_with(
    html: &str,
    width: u32,
    band_top: u32,
    band_height: u32,
    fonts: &mut FontStore,
    loader: &mut dyn Loader,
    base: Option<(&Origin, &str)>,
) -> Page {
    render_sized(
        html,
        width,
        band_top,
        band_height,
        Settings {
            force_document: true,
            ..Settings::default()
        },
        fonts,
        loader,
        base,
    )
}

/// The same, with the caller supplying where subresources come from.
pub fn render_as_authored_with(
    html: &str,
    width: u32,
    band_top: u32,
    band_height: u32,
    fonts: &mut FontStore,
    loader: &mut dyn Loader,
    base: Option<(&Origin, &str)>,
) -> Page {
    render_sized(
        html,
        width,
        band_top,
        band_height,
        Settings {
            force_authored: true,
            ..Settings::default()
        },
        fonts,
        loader,
        base,
    )
}

/// How to render, beyond the document itself.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Settings {
    /// Fill the canvas to the height given rather than shrinking to content.
    pub(crate) fill_height: bool,
    /// Use the author's layout whatever classification decided.
    pub(crate) force_authored: bool,
    /// Use the document fallback whatever classification decided.
    ///
    /// The other direction of `force_authored`, and not reachable by inverting
    /// it: a page that classifies as `Authored` has no fallback to return to,
    /// so asking for one is a different request rather than the absence of
    /// this one.
    pub(crate) force_document: bool,
    /// How much bigger than its own pixels the page is drawn.
    ///
    /// 1.0 is the page as written. It is applied in the cascade, where every
    /// pixel length is computed — so the text is *shaped* at the zoomed size
    /// rather than a finished rendering being blown up, and the viewport keeps
    /// its real width so the text reflows to the window (#55).
    pub(crate) zoom: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fill_height: false,
            force_authored: false,
            force_document: false,
            zoom: 1.0,
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "a render's inputs, threaded explicitly rather than bundled into a struct \
              nothing else would use"
)]
pub(crate) fn render_sized(
    html: &str,
    width: u32,
    band_top: u32,
    band_height: u32,
    settings: Settings,
    fonts: &mut FontStore,
    loader: &mut dyn Loader,
    base: Option<(&Origin, &str)>,
) -> Page {
    let doc = dom::parse(html);

    // A frameset document has no body to lay out: each frame is a separate
    // page, fetched and rendered in its own right, then composited into the
    // window. Handled before styling because there is nothing here to style.
    if let Some(frameset) = doc.find_element("frameset")
        && let Some((origin, path)) = base
    {
        return render_frameset(
            &doc,
            frameset,
            width,
            band_height,
            fonts,
            loader,
            origin,
            path,
            0,
        );
    }

    let author_sheets = collect_stylesheets(&doc, loader, base, width as f32);
    let styles = css::cascade::cascade_at(&doc, &author_sheets, settings.zoom);

    // Classify before laying out: if the page needs layout we do not implement,
    // producing the wrong layout first and discarding it would be wasted work.
    let mode = if settings.force_authored {
        // The reader asked to see what the author wrote. Classification still
        // ran — the answer is just not being acted on.
        RenderMode::Authored
    } else if settings.force_document {
        // The reader asked for the fallback on a page that did not need one.
        // Classification still runs and its measurement is kept, so the bar can
        // go on saying how much of this page actually wanted newer layout —
        // which on a page in this branch is usually none of it, and saying so
        // is the honest answer rather than an embarrassing one.
        match layout::classify(&doc, &styles) {
            RenderMode::Authored => RenderMode::Document {
                unsupported_share: 0.0,
            },
            already_a_fallback => already_a_fallback,
        }
    } else {
        layout::classify(&doc, &styles)
    };

    let mut styles = match mode {
        RenderMode::Authored => styles,
        // Re-render as a document. The author's sheets are dropped entirely —
        // keeping them would reintroduce exactly the layout that failed — and
        // the reader sheet is applied over the UA defaults instead.
        //
        // Their colours go too, and that needs saying separately: a sheet is
        // not the only place an author writes one. Wikipedia's taxobox carries
        // its pale bands in a `style` attribute on each row, which survived
        // the sheet being dropped and left near-white text on near-white
        // backgrounds — a reading view less legible than the page it was
        // rescuing.
        RenderMode::Document { .. }
        | RenderMode::DocumentFrame { .. }
        | RenderMode::RequiresScripting => {
            let reader = Stylesheet::parse(css::ua::READER_STYLESHEET);
            let mut styles = css::cascade::cascade_as(
                &doc,
                &[reader],
                settings.zoom,
                css::cascade::Colours::Readers,
            );
            // The author's furniture goes with the author's layout. Without
            // this the navigation, the sidebar and the footer no longer sit
            // beside the article — they stack above and below it, so a reading
            // view opens on every link the site has (ADR-0009).
            //
            // Hidden rather than removed, so the document the child is holding
            // is still the whole document and pressing "as authored" does not
            // cost a re-parse.
            for node in slop::extract(&doc).dropped {
                styles.hide(node);
            }
            collapse_blank_lines(&doc, &mut styles);
            styles
        }
    };

    // Whatever the page asked for, it does not get to put its text against the
    // glass. The body's own margin counts towards the gutter, so a page that
    // left the UA default alone is unchanged and only `margin: 0` is topped up.
    if let Some(body) = doc.find_element("body") {
        styles.keep_off_the_edges(body, PAGE_GUTTER, width as f32);
    }
    // Images are loaded whichever way the page is being rendered. They used to
    // be dropped on the document fallback, on the grounds that a rendering
    // which has discarded the author's layout should not spend requests on
    // their decoration. That reasoning does not survive contact with what those
    // images actually are: a diagram in an article, a photograph a caption
    // refers to, the panel of a comic. Discarding the layout is a judgement
    // about how a page is arranged, and it was quietly being used as a
    // judgement about what the page is *made of*.
    //
    // What made them decoration was the author's stylesheet, and that is
    // already gone: a background image cannot survive the sheet that named it,
    // so what is left here is the images the markup itself points at.
    let images = match base {
        Some((origin, path)) => load_images(&doc, &styles, loader, origin, path),
        None => ImageStore::new(),
    };
    // Only content images have an intrinsic size layout cares about: a
    // background tile is drawn at its natural size and never sizes its box.
    let intrinsic: IntrinsicSizes = images
        .iter()
        .filter(|(key, _)| key.slot == paint::ImageSlot::Content)
        .map(|(key, image)| (key.node, (image.width(), image.height())))
        .collect();

    if !matches!(mode, RenderMode::Authored) {
        hide_missing_images(&doc, &intrinsic, &mut styles);
        // After the images and not before: the boxes this finds are mostly
        // the ones the line above just emptied.
        hide_empty_boxes(&doc, &mut styles);
    }
    // Settled: everything that patches a computed style after the cascade has
    // had its turn, and the rest of the pipeline reads.
    let styles = styles;

    let laid_out = layout::layout(&doc, &styles, fonts, &intrinsic, width as f32);
    let list = build_display_list(&laid_out);
    // The band asked for, clipped to what the document actually has below it.
    // A page shorter than the band gets a canvas its own height, which is what
    // every page did before bands existed and is why a short page still paints
    // exactly as it used to.
    let content_rows = laid_out.height.ceil().max(1.0) as u32;
    let height = if settings.fill_height {
        band_height.max(1)
    } else {
        band_height
            .min(content_rows.saturating_sub(band_top))
            .max(1)
    };
    let pixmap = paint::rasterise_band(&list, fonts, &images, width, band_top as f32, height)
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
        band_top,
        background: list.canvas,
        source: Some(Box::new(BandSource { list, images })),
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
    reason = "a frame's rendering context, threaded explicitly for clarity"
)]
fn render_frameset(
    doc: &dom::Document,
    frameset: dom::NodeId,
    width: u32,
    max_height: u32,
    fonts: &mut FontStore,
    loader: &mut dyn Loader,
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
                loader,
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
            let Some(resource) = loader.load(&url, None, RequestKind::Navigation) else {
                continue;
            };
            if depth + 1 > MAX_FRAME_DEPTH {
                continue;
            }
            // The frame's own origin, resolved from the URL rather than
            // reported by the loader: a loader on the far side of a process
            // boundary is not a thing to take an origin from.
            let Ok((frame_origin, frame_path)) = net::parse_url(&url) else {
                continue;
            };
            loaded += 1;
            render_in_viewport_with(
                &resource.text(),
                cell_width as u32,
                cell_height as u32,
                fonts,
                loader,
                Some((&frame_origin, &frame_path)),
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
        band_top: 0,
        // A frameset's canvas is composited from its frames rather than built
        // from one display list, so there is nothing to repaint a band from —
        // and nothing needs one, because a frameset is its viewport and never
        // has rows below the ones it holds.
        source: None,
        mode: RenderMode::Authored,
        // A frameset fills its own viewport exactly, so the window never has a
        // row of it to fill in. White is the colour of the gaps between the
        // frames, which is what `render_frameset` clears its canvas to.
        background: css::Color::rgb(0xff, 0xff, 0xff),
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
    loader: &mut dyn Loader,
    origin: &Origin,
    path: &str,
) -> ImageStore {
    // Two passes: every image the page refers to is found first, and only then
    // asked for. Interleaving the two meant each image was requested where it
    // was discovered, so they went out one at a time and the page waited for
    // the sum of their latencies. Nothing here needs the first image to know
    // about the second, so nothing needs to.
    let mut wanted: Vec<(paint::ImageKey, String)> = Vec::new();
    for node in doc.descendants(doc.root()) {
        let Some(element) = doc.element(node) else {
            continue;
        };
        if element.local_name() == "img"
            && let Some(src) = element.attr("src")
        {
            wanted.push((
                paint::ImageKey::content(node),
                net::resolve(origin, path, src),
            ));
        }
        if let Some(source) = styles
            .get(node)
            .and_then(|style| style.background_image.as_deref())
        {
            wanted.push((
                paint::ImageKey::background(node),
                net::resolve(origin, path, source),
            ));
        }
    }

    // The same image often appears many times on a page — a tile on every cell
    // of a table — so each distinct URL is asked for once. Order is kept so
    // that what arrives can be matched back positionally.
    let mut distinct: Vec<String> = Vec::new();
    for (_, url) in &wanted {
        if !distinct.contains(url) {
            distinct.push(url.clone());
        }
    }

    // Subresources, so ADR-0006's third-party rule applies — wherever the
    // loader chooses to apply it.
    let fetched = loader.load_many(&distinct, Some(origin), RequestKind::Subresource);
    let decoded: std::collections::HashMap<&str, Option<paint::DecodedImage>> = distinct
        .iter()
        .map(String::as_str)
        .zip(fetched)
        // Decoded here rather than by the parent: an image format is a parser
        // like any other, and parsers live on this side of the boundary.
        .map(|(url, resource)| (url, resource.and_then(|got| paint::decode(&got.bytes))))
        .collect();

    let mut store = ImageStore::new();
    for (key, url) in &wanted {
        if let Some(Some(image)) = decoded.get(url.as_str()) {
            store.insert(*key, image.clone());
        }
    }
    store
}

/// Extracts the contents of every `<style>` element, in document order.
///
/// `<link rel=stylesheet>` is not followed here: fetching is the net crate's
/// job, and same-origin policy (ADR-0006) applies to it.
/// Keeps the first of every run of consecutive line breaks and drops the rest.
///
/// Only on the document fallback. A page that writes `<br><br><br><br>` between
/// two paragraphs is using line breaks as a margin, which was how a great deal
/// of the era's markup — and every WYSIWYG editor since — did its spacing. The
/// author's layout is the place to honour that exactly; a document rendering
/// has already thrown their stylesheet away on the grounds that it was not
/// working, and reproducing their vertical spacing to the pixel while doing so
/// only turns a page into a column of gaps with the occasional sentence in it.
///
/// One is kept rather than none, because a single break between two lines is
/// almost always meant — an address, a verse, a signature — and a reader mode
/// that ran those together would be destroying content rather than spacing.
///
/// Whitespace between the breaks does not interrupt a run: `<br>\n  <br>` is
/// two breaks with a newline between them, and that newline collapses to
/// nothing of its own.
fn collapse_blank_lines(doc: &dom::Document, styles: &mut css::cascade::StyleMap) {
    for node in doc.descendants(doc.root()) {
        let mut breaks = 0usize;
        for &child in doc.children(node) {
            // Whitespace between two breaks is not content between them: it
            // collapses away, so the breaks are still consecutive on the page
            // even though they are not consecutive in the markup.
            if let Some(text) = doc.text(child) {
                if text.trim().is_empty() {
                    continue;
                }
                breaks = 0;
                continue;
            }
            let is_break = doc
                .element(child)
                .is_some_and(|element| element.local_name() == "br");
            if !is_break {
                breaks = 0;
                continue;
            }
            breaks += 1;
            if breaks > 1 {
                styles.hide(child);
            }
        }
    }
}

/// Hides boxes that have nothing left to draw, in a document rendering.
///
/// The counterpart to [`hide_missing_images`], and it runs second because it
/// depends on it. A picture on a Wikipedia article arrives wrapped in three
/// or four sized `<div>`s, and the taxobox at the top of the page stacks six
/// of those inside a table: hide the images and 850 pixels of empty scaffold
/// are still standing, each box holding the height its inline `style` asked
/// for around nothing at all. Hiding the pictures without this only moves the
/// hole.
///
/// The rule is about content rather than cause, which is what makes it safe
/// to apply to a whole document: an element is hidden when its subtree holds
/// no text and nothing that draws by itself. Already-hidden descendants do
/// not count — that is the point, since what makes these boxes empty is
/// precisely what [`slop::extract`] and `hide_missing_images` hid.
///
/// Only on the fallback, and for the same reason as the images: a rendering
/// that has kept the author's stylesheet has to keep the boxes it sizes, and
/// a rendering that threw the sheet away has already decided that the
/// author's arrangement was not serving the reader.
///
/// Bottom-up in reverse document order, so each element reads an answer its
/// children have already written instead of walking its own subtree. On an
/// article whose tables nest five deep that is the difference between a pass
/// and a page-load.
fn hide_empty_boxes(doc: &dom::Document, styles: &mut css::cascade::StyleMap) {
    let mut draws: HashMap<dom::NodeId, bool> = HashMap::new();
    for node in doc.descendants(doc.root()).into_iter().rev() {
        let drawn = draws_something(doc, styles, node, &draws);
        draws.insert(node, drawn);
        // The root and the body are the page itself. Hiding them turns an
        // article that happens to be all pictures into a blank window, with
        // nothing left underneath to explain where it went.
        if !drawn
            && doc
                .element(node)
                .is_some_and(|element| may_go(element.local_name()))
        {
            styles.hide(node);
        }
    }
}

/// Whether an empty element of this name may be taken out of the flow.
///
/// Emptiness is not on its own a licence to remove something. An element can
/// hold a place as well as hold content, and these hold places:
///
/// * A cell keeps a column in line. `<td></td>` is ordinary in a data table,
///   and dropping it slides every later cell in that row one column left —
///   which puts the wrong numbers under the headings, silently.
/// * A list item keeps the count. Dropping an empty one renumbers every item
///   after it.
///
/// The root and the body are not on the list, and not because they are safe
/// to drop: layout reads the body's style and then lays out its children
/// whatever its `display` says, so hiding it does nothing at all. An entry
/// here would be a comment claiming a danger the code cannot have.
fn may_go(local_name: &str) -> bool {
    !matches!(
        local_name,
        "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "col"
            | "colgroup"
            | "caption"
            | "li"
            | "dt"
            | "dd"
    )
}

/// Whether this node puts ink on the page, given what its children have
/// already answered.
///
/// Text, or an element that draws on its own account. The list is short and
/// errs towards keeping things: a false "yes" costs an empty box, a false
/// "no" deletes something the reader came for.
fn draws_something(
    doc: &dom::Document,
    styles: &css::cascade::StyleMap,
    node: dom::NodeId,
    draws: &HashMap<dom::NodeId, bool>,
) -> bool {
    /// Elements that draw with no text of their own.
    const SELF_DRAWING: &[&str] = &[
        "img", "hr", "canvas", "svg", "video", "audio", "iframe", "object", "embed", "input",
        "textarea", "select", "button",
    ];
    if styles
        .get(node)
        .is_some_and(|style| style.display == css::style::Display::None)
    {
        return false;
    }
    if let Some(text) = doc.text(node) {
        return !text.trim().is_empty();
    }
    let Some(element) = doc.element(node) else {
        return false;
    };
    // A `<br>` puts no ink anywhere and is still the whole of what its author
    // wrote — an address, a verse, a signature. It counts.
    if element.local_name() == "br" {
        return true;
    }
    if SELF_DRAWING.contains(&element.local_name()) {
        return true;
    }
    doc.children(node)
        .iter()
        .any(|child| draws.get(child).copied().unwrap_or(false))
}

/// Hides images that are not going to be drawn, in a document rendering.
///
/// This is where the reading view's empty gaps came from. Every large blank
/// band on a Wikipedia article — 312 pixels, 294, 282, on down — was an
/// `<img>` holding open the box its `width` and `height` attributes asked
/// for, with no picture in it. Wikipedia serves its images from a different
/// host, third-party requests are refused (ADR-0006), and an article is
/// mostly photographs: the reader was scrolling past holes.
///
/// A browser rendering the page as authored has to keep the hole. The
/// author's layout is built around a box of that size, and Chromium reserves
/// it too — a broken image with dimensions is 250x290 of nothing there as
/// well. A document rendering has already given that up: it threw away the
/// author's sheet precisely because their layout was not serving the reader,
/// and a gap reserved for a picture that does not exist is the clearest case
/// of it. So this only runs on the fallback, and `Authored` keeps Chromium's
/// behaviour, gaps and all.
///
/// "Not going to be drawn" is decided after the fetch, so it means failed or
/// refused rather than still coming: `intrinsic` holds every image that
/// loaded and decoded, and layout is about to size the rest from thin air.
///
/// The caption stays. It is text, the reader can still learn what the picture
/// showed, and losing it as well would be a second, quieter kind of hole.
fn hide_missing_images(
    doc: &dom::Document,
    intrinsic: &IntrinsicSizes,
    styles: &mut css::cascade::StyleMap,
) {
    for node in doc.descendants(doc.root()) {
        let is_image = doc
            .element(node)
            .is_some_and(|element| element.local_name() == "img");
        if is_image && !intrinsic.contains_key(&node) {
            styles.hide(node);
        }
    }
}

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
    loader: &mut dyn Loader,
    base: Option<(&Origin, &str)>,
    depth: usize,
    viewport_width: f32,
) {
    if depth < MAX_IMPORT_DEPTH
        && let Some((origin, path)) = base
    {
        for href in &sheet.imports {
            let url = net::resolve(origin, path, href);
            let Some(resource) = loader.load(&url, Some(origin), RequestKind::Subresource) else {
                continue;
            };
            // The imported sheet's own imports resolve against *it*, not
            // against whatever imported it — and its origin comes from the URL
            // we asked for rather than from whatever answered.
            let Ok((sheet_origin, sheet_path)) = net::parse_url(&url) else {
                continue;
            };
            push_with_imports(
                sheets,
                Stylesheet::parse_at(&resource.text(), viewport_width),
                loader,
                Some((&sheet_origin, &sheet_path)),
                depth + 1,
                viewport_width,
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

fn collect_stylesheets(
    doc: &dom::Document,
    loader: &mut dyn Loader,
    base: Option<(&Origin, &str)>,
    // The width the page is being rendered at, which decides which `@media`
    // blocks contribute any rules at all.
    viewport_width: f32,
) -> Vec<Stylesheet> {
    let mut sheets = Vec::new();

    for node in doc.descendants(doc.root()) {
        let Some(element) = doc.element(node) else {
            continue;
        };
        match element.local_name() {
            "style" => {
                let sheet = Stylesheet::parse_at(&doc.text_content(node), viewport_width);
                // A `<style>` block's imports resolve against the document.
                push_with_imports(&mut sheets, sheet, loader, base, 0, viewport_width);
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
                if let Some(resource) = loader.load(&url, Some(origin), RequestKind::Subresource)
                    && let Ok((sheet_origin, sheet_path)) = net::parse_url(&url)
                {
                    let sheet = Stylesheet::parse_at(&resource.text(), viewport_width);
                    // An imported sheet's URLs resolve against the sheet that
                    // imported it, not against the document.
                    push_with_imports(
                        &mut sheets,
                        sheet,
                        loader,
                        Some((&sheet_origin, &sheet_path)),
                        0,
                        viewport_width,
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
    /// The document fallback's height for a fragment, and the author's.
    fn both_ways(html: &str) -> (f32, f32) {
        let mut fonts = FontStore::new();
        let document = render_as_document_with(
            html,
            600,
            0,
            9000,
            &mut fonts,
            &mut DirectLoader::default(),
            None,
        );
        let authored = render_as_authored_with(
            html,
            600,
            0,
            9000,
            &mut fonts,
            &mut DirectLoader::default(),
            None,
        );
        (document.content_height, authored.content_height)
    }

    /// A loader that answers any `.png` with one solid green image.
    struct GreenImages {
        png: Vec<u8>,
    }

    impl Loader for GreenImages {
        fn load(
            &mut self,
            url: &str,
            _document: Option<&Origin>,
            _kind: RequestKind,
        ) -> Option<Loaded> {
            url.ends_with(".png").then(|| Loaded {
                bytes: self.png.clone(),
                content_type: Some("image/png".to_owned()),
            })
        }
    }

    fn green_png(width: u32, height: u32) -> Vec<u8> {
        let mut pixmap = Pixmap::new(width, height).expect("a pixmap");
        pixmap.fill(paint::RasterColor::from_rgba8(0, 0xff, 0, 0xff));
        let file = std::env::temp_dir().join(format!("2kbrowser-green-{width}x{height}.png"));
        pixmap.save_png(&file).expect("writes a png");
        std::fs::read(&file).expect("reads it back")
    }

    fn green_pixels(page: &Page) -> usize {
        page.pixmap
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] < 80 && pixel[1] > 150 && pixel[2] < 80)
            .count()
    }

    #[test]
    fn the_document_fallback_paints_none_of_the_colours_in_the_markup() {
        // Dropping the author's sheet does not drop the colours in their
        // markup. Wikipedia's taxobox carries its bands inline, on every row,
        // and they came through onto the dark reader page as pale strips with
        // near-white text on them — a reading view less legible than the page
        // it was rescuing.
        let html = "<body><p style=\"background-color: rgb(0,255,0)\">A paragraph.</p></body>";
        let mut fonts = FontStore::new();
        let as_document = render_as_document_with(
            html,
            900,
            0,
            4000,
            &mut fonts,
            &mut DirectLoader::default(),
            None,
        );
        let as_authored = render_as_authored(html, 900, 4000, &mut fonts, None);

        assert_eq!(
            green_pixels(&as_document),
            0,
            "the reading view painted the author's background"
        );
        assert!(
            green_pixels(&as_authored) > 0,
            "the authored rendering lost it"
        );
    }

    #[test]
    fn the_document_fallback_still_shows_the_pages_images() {
        // They used to be dropped, on the grounds that a rendering which has
        // discarded the author's layout should not spend requests on their
        // decoration. That was using a judgement about how a page is arranged
        // as a judgement about what it is made of: the diagram in an article
        // and the photograph a caption refers to are the content.
        let html = "<body><p>before</p><img src=\"picture.png\"><p>after</p></body>";
        let (origin, at) = net::parse_url("https://example.com/p.html").expect("parses");
        let mut fonts = FontStore::new();
        let page = render_as_document_with(
            html,
            900,
            0,
            4000,
            &mut fonts,
            &mut GreenImages {
                png: green_png(100, 60),
            },
            Some((&origin, &at)),
        );

        assert_eq!(page.images_loaded, 1, "the image was never fetched");
        assert_eq!(
            green_pixels(&page),
            100 * 60,
            "the image was fetched and then not painted at its own size"
        );
    }

    #[test]
    fn an_image_too_wide_for_the_reader_column_is_brought_down_to_it() {
        // An article's photograph is routinely wider than the 42em the reader
        // sheet asks for, and nothing clips an overflow — so without a bound it
        // runs off the side of the page it is supposed to be illustrating.
        let html = "<body><p>before</p><img src=\"wide.png\"><p>after</p></body>";
        let (origin, at) = net::parse_url("https://example.com/p.html").expect("parses");
        let mut fonts = FontStore::new();
        let page = render_as_document_with(
            html,
            900,
            0,
            4000,
            &mut fonts,
            &mut GreenImages {
                png: green_png(3000, 400),
            },
            Some((&origin, &at)),
        );

        let green = green_pixels(&page);
        assert!(green > 0, "the image was not painted at all");
        assert!(
            green < 3000 * 400,
            "the image was painted at its full size and overflowed"
        );
        let rows_with_green = (0..page.pixmap.height())
            .filter(|row| {
                let start = (*row * page.pixmap.width() * 4) as usize;
                let end = start + (page.pixmap.width() * 4) as usize;
                page.pixmap.data()[start..end]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[0] < 80 && pixel[1] > 150 && pixel[2] < 80)
            })
            .count();
        let widest = green / rows_with_green.max(1);
        assert!(
            widest <= 900,
            "the image is {widest}px wide on a 900px canvas"
        );
        // 3000x400 is 15:2, so a width of `widest` should come with about
        // `widest * 2 / 15` rows. Anything else means it was squashed.
        let expected_rows = (widest * 2).div_ceil(15);
        assert!(
            rows_with_green.abs_diff(expected_rows) <= 2,
            "{widest}x{rows_with_green} is not the 15:2 image scaled down"
        );
    }

    /// A loader that fetches nothing, like a page whose images are all on
    /// another host and refused (ADR-0006).
    struct NoImages;

    impl Loader for NoImages {
        fn load(
            &mut self,
            _url: &str,
            _document: Option<&Origin>,
            _kind: RequestKind,
        ) -> Option<Loaded> {
            None
        }
    }

    /// Height of a page rendered as a document, with `loader` for its images.
    fn document_height(html: &str, loader: &mut dyn Loader) -> f32 {
        let (origin, at) = net::parse_url("https://example.com/p.html").expect("parses");
        let mut fonts = FontStore::new();
        render_as_document_with(html, 900, 0, 4000, &mut fonts, loader, Some((&origin, &at)))
            .content_height
    }

    #[test]
    fn a_document_rendering_does_not_hold_a_gap_open_for_an_image_that_never_arrived() {
        // Every large blank band in the reading view of a Wikipedia article
        // was one of these: an `<img>` whose `width` and `height` attributes
        // reserved a box, with nothing in it because the picture is on
        // another host and third-party requests are refused. The article was
        // holes.
        let html = "<body><p>before</p><img src=\"gone.png\" width=\"250\" height=\"290\"><p>after</p></body>";
        let gap = document_height(html, &mut NoImages);
        let no_image = document_height("<body><p>before</p><p>after</p></body>", &mut NoImages);

        assert!(
            (gap - no_image).abs() < 1.0,
            "the missing image still holds {}px open",
            gap - no_image
        );
    }

    #[test]
    fn a_document_rendering_keeps_the_gap_for_an_image_that_did_arrive() {
        // The other half, and the one that matters: the rule is "there is no
        // picture", not "pictures are noise". An image that loaded is the
        // content of the article and takes exactly the room it always did.
        let html = "<body><p>before</p><img src=\"here.png\" width=\"250\" height=\"290\"><p>after</p></body>";
        let mut green = GreenImages {
            png: green_png(250, 290),
        };
        let shown = document_height(html, &mut green);
        let no_image = document_height("<body><p>before</p><p>after</p></body>", &mut NoImages);

        assert!(
            shown - no_image > 280.0,
            "the image only added {}px",
            shown - no_image
        );
    }

    #[test]
    fn a_document_rendering_drops_the_scaffolding_left_around_a_missing_image() {
        // Hiding the picture alone only moves the hole. A Wikipedia photograph
        // arrives inside three or four `<div>`s carrying its size in an inline
        // style, and the taxobox stacks six of those in a table: 850 pixels of
        // empty frame around nothing.
        let html = "<body><p>before</p>\
            <div style=\"height: 300px\"><div style=\"height: 290px\">\
            <img src=\"gone.png\" width=\"250\" height=\"290\"></div></div>\
            <p>after</p></body>";
        let scaffold = document_height(html, &mut NoImages);
        let no_image = document_height("<body><p>before</p><p>after</p></body>", &mut NoImages);

        assert!(
            (scaffold - no_image).abs() < 1.0,
            "the empty frame still holds {}px open",
            scaffold - no_image
        );
    }

    #[test]
    fn an_empty_box_next_to_a_caption_goes_without_taking_the_caption_with_it() {
        // The caption is text and the reader can still learn what the picture
        // showed. Losing it as well would be a second, quieter hole — and it
        // is the case that tells "hide what draws nothing" apart from "hide
        // the figure".
        let html = "<body><figure><div style=\"height: 290px\">\
            <img src=\"gone.png\" width=\"250\" height=\"290\"></div>\
            <figcaption>A cat, asleep.</figcaption></figure></body>";
        let (origin, at) = net::parse_url("https://example.com/p.html").expect("parses");
        let mut fonts = FontStore::new();
        let page = render_as_document_with(
            html,
            900,
            0,
            4000,
            &mut fonts,
            &mut NoImages,
            Some((&origin, &at)),
        );

        assert!(
            page.content_height < 120.0,
            "the empty frame is still {}px tall",
            page.content_height
        );
        assert!(
            page.content_height > 20.0,
            "the caption went with the picture"
        );
    }

    #[test]
    fn a_page_rendered_as_authored_keeps_the_box_a_missing_image_asked_for() {
        // Deliberately Chromium's behaviour, which reserves the full box for a
        // broken image that gave its dimensions. The author's layout is built
        // around it, and a rendering that kept their stylesheet has to keep
        // their boxes; it is giving the sheet up that earns the right to
        // close the gap.
        let html = "<body><p>before</p><img src=\"gone.png\" width=\"250\" height=\"290\"><p>after</p></body>";
        let (origin, at) = net::parse_url("https://example.com/p.html").expect("parses");
        let mut fonts = FontStore::new();
        let page = render_as_authored_with(
            html,
            900,
            0,
            4000,
            &mut fonts,
            &mut NoImages,
            Some((&origin, &at)),
        );

        assert!(
            page.content_height > 280.0,
            "the box is only {}px tall",
            page.content_height
        );
    }

    /// Where the words of a page ended up, left to right and top to bottom.
    fn document_text(html: &str) -> Vec<String> {
        document_words(html)
            .into_iter()
            .map(|(_, _, word)| word)
            .collect()
    }

    /// The same, keeping each line's baseline and left edge.
    fn document_words(html: &str) -> Vec<(i32, i32, String)> {
        let (origin, at) = net::parse_url("https://example.com/p.html").expect("parses");
        let mut fonts = FontStore::new();
        let page = render_as_document_with(
            html,
            900,
            0,
            4000,
            &mut fonts,
            &mut NoImages,
            Some((&origin, &at)),
        );
        let mut placed: Vec<(i32, i32, String)> = Vec::new();
        collect_text(&page.frames[0].layout.root, 0.0, 0.0, &mut placed);
        placed.sort();
        placed
    }

    fn collect_text(box_: &layout::LayoutBox, x: f32, y: f32, out: &mut Vec<(i32, i32, String)>) {
        let (x, y) = (x + box_.rect.x, y + box_.rect.y);
        if let Some(text) = &box_.text {
            for line in &text.lines {
                if line.text.trim().is_empty() {
                    continue;
                }
                let left = line.glyphs.first().map_or(0.0, |glyph| glyph.x);
                out.push((
                    (y + line.baseline) as i32,
                    (x + left) as i32,
                    line.text.trim().to_owned(),
                ));
            }
        }
        for child in &box_.children {
            collect_text(child, x, y, out);
        }
    }

    #[test]
    fn a_wrapper_holding_only_a_newline_is_holding_nothing() {
        // What separates a box with nothing in it from a box with a line of
        // text in it is a `trim`, and almost every wrapper on a real page is
        // this case: markup is indented, so an "empty" div holds a newline
        // and two spaces. Without the trim the pass finds nearly nothing.
        let bare = document_height("<body><p>before</p><p>after</p></body>", &mut NoImages);
        let indented = document_height(
            "<body><p>before</p><div style=\"height: 290px\">\n  \n</div><p>after</p></body>",
            &mut NoImages,
        );

        assert!(
            (indented - bare).abs() < 1.0,
            "the whitespace held {}px open",
            indented - bare
        );
    }

    #[test]
    fn an_empty_cell_keeps_its_column_in_line() {
        // Emptiness is not on its own a licence to remove something. `<td></td>`
        // is ordinary in a data table, and taking it out slides every later
        // cell in that row one column left — which files the wrong numbers
        // under the headings and says nothing about having done it.
        let html = "<body><table>\
            <tr><td>top left</td><td>top right</td></tr>\
            <tr><td></td><td>bottom right</td></tr></table></body>";
        let words = document_words(html);
        let column = |wanted: &str| {
            words
                .iter()
                .find(|(_, _, word)| word.contains(wanted))
                .unwrap_or_else(|| panic!("{wanted} is not on the page: {words:?}"))
                .1
        };

        assert!(
            column("top left") < column("top right"),
            "the two columns are in one place: {words:?}"
        );
        assert_eq!(
            column("bottom right"),
            column("top right"),
            "the empty cell was dropped and the row slid a column left: {words:?}"
        );
    }

    #[test]
    fn an_empty_list_item_keeps_the_count() {
        // Same reason as the cell, counted differently: drop an empty item and
        // every item after it is renumbered, so a list of six references
        // silently becomes a list of five with the wrong numbers on them.
        let gapped = document_text("<body><ol><li>one</li><li></li><li>three</li></ol></body>");
        let marker = |words: &[String], word: &str| {
            let at = words
                .iter()
                .position(|found| found.contains(word))
                .unwrap_or_else(|| panic!("{word} is not on the page: {words:?}"));
            words[..at]
                .iter()
                .rev()
                .find(|found| found.ends_with('.'))
                .cloned()
                .unwrap_or_default()
        };

        assert_eq!(marker(&gapped, "one"), "1.", "{gapped:?}");
        assert_eq!(
            marker(&gapped, "three"),
            "3.",
            "the empty item was dropped and the third was renumbered: {gapped:?}"
        );
    }

    #[test]
    fn the_document_fallback_paints_a_dark_page_and_says_so() {
        // Two halves of the same thing. The canvas has to *be* dark, and the
        // page has to *report* what colour it is: the window fills the rows it
        // has no pixels for — below a short page, ahead of a band still being
        // painted — and white there would be a lit strip beside the text.
        let html = "<body><h1>Title</h1><p>An ordinary paragraph.</p></body>";
        let mut fonts = FontStore::new();
        let page = render_as_document_with(
            html,
            800,
            0,
            2000,
            &mut fonts,
            &mut DirectLoader::default(),
            None,
        );

        let corner = &page.pixmap.data()[..4];
        let brightness = |r: u8, g: u8, b: u8| {
            (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0
        };
        assert!(
            brightness(corner[0], corner[1], corner[2]) < 0.2,
            "the fallback canvas came out {corner:?}"
        );
        assert_eq!(
            (
                page.background.r,
                page.background.g,
                page.background.b,
                page.background.a
            ),
            (corner[0], corner[1], corner[2], corner[3]),
            "the page reported a background it did not paint"
        );

        // And the author's own layout is left in the light, since the reader
        // sheet is not applied to it.
        let authored = render(html, 800, 2000, &mut fonts);
        let lit = &authored.pixmap.data()[..4];
        assert!(
            brightness(lit[0], lit[1], lit[2]) > 0.8,
            "the authored rendering went dark too: {lit:?}"
        );
    }

    /// The leftmost and rightmost columns of the canvas carrying any ink.
    fn ink_columns(page: &Page) -> (u32, u32) {
        let width = page.pixmap.width();
        let mut span: Option<(u32, u32)> = None;
        for (index, pixel) in page.pixmap.data().as_chunks::<4>().0.iter().enumerate() {
            // Anything that is not the blank canvas. Every fixture below is
            // black text on white, so "not white" is exactly "content".
            if pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200 {
                continue;
            }
            let column = index as u32 % width;
            span = Some(match span {
                None => (column, column),
                Some((lo, hi)) => (lo.min(column), hi.max(column)),
            });
        }
        span.expect("the fixture rendered nothing at all")
    }

    /// Enough words to fill several lines at any sensible width.
    const PARAGRAPH: &str = "The quick brown fox jumps over the lazy dog, and then does \
                             it again because the first time nobody was watching.";

    #[test]
    fn a_page_that_zeroes_its_body_margin_still_keeps_a_gutter() {
        // `body { margin: 0 }` is in nearly every modern stylesheet, and taken
        // literally it sets the first letter of every line against the window
        // frame.
        let mut fonts = FontStore::new();
        let page = render(
            &format!("<style>body {{ margin: 0 }}</style><body><p>{PARAGRAPH}</p></body>"),
            400,
            2000,
            &mut fonts,
        );
        let (left, right) = ink_columns(&page);
        assert!(left >= 8, "text starts at column {left}");
        assert!(right <= 400 - 8, "text runs to column {right} of 400");
    }

    #[test]
    fn the_gutter_is_a_floor_and_not_an_extra_margin() {
        // The whole point of a floor: a page that already asked for room gets
        // exactly the room it asked for. Adding the gutter on top would push
        // every ordinary page inwards for no reason, and would keep pushing a
        // generous one further in.
        let mut fonts = FontStore::new();
        let render_at = |css: &str, fonts: &mut FontStore| {
            ink_columns(&render(
                &format!("<style>{css}</style><body><p>{PARAGRAPH}</p></body>"),
                400,
                2000,
                fonts,
            ))
        };

        // The UA sheet's own 8px, which is already the floor.
        let default = render_at("", &mut fonts);
        let zeroed = render_at("body { margin: 0 }", &mut fonts);
        assert_eq!(
            default, zeroed,
            "the floor moved a page that had not asked for less than it"
        );

        // And a page asking for more keeps all of it, unchanged.
        let generous = render_at("body { margin: 40px }", &mut fonts);
        assert_eq!(generous.0, 40, "a 40px margin became {}", generous.0);
    }

    #[test]
    fn the_pages_background_still_reaches_the_window_edge() {
        // The gutter holds the page's *content* back; it is not a frame drawn
        // around the page. A body background that stopped 8px short would put a
        // pale border around every coloured page.
        //
        // The root is given a background of its own so that §14.2 propagation
        // cannot answer this by accident: the canvas is white here, and the
        // blue at the window edge can only have come from the body's own box
        // still spanning the window.
        let mut fonts = FontStore::new();
        let page = render(
            "<style>html { background: #ffffff } \
             body { margin: 0; background: #3366cc }</style><body><p>hi</p></body>",
            400,
            2000,
            &mut fonts,
        );
        let row = &page.pixmap.data()[..page.pixmap.width() as usize * 4];
        let colour = |pixel: &[u8]| (pixel[0], pixel[1], pixel[2]);
        assert_eq!(
            colour(&row[..4]),
            (0x33, 0x66, 0xcc),
            "the top-left pixel is not the page's background"
        );
        assert_eq!(
            colour(&row[row.len() - 4..]),
            (0x33, 0x66, 0xcc),
            "the top-right pixel is not the page's background"
        );
    }

    #[test]
    fn a_run_of_line_breaks_becomes_one_in_the_document_fallback() {
        // Using `<br><br><br><br>` as a margin is how a great deal of the era's
        // markup did its spacing, and how every WYSIWYG editor has done it
        // since. Reproduced faithfully in a document rendering, it turns a page
        // into a column of gaps with the occasional sentence in it — which is
        // the complaint this exists for.
        let one = both_ways("<body><p>one<br>two</p></body>");
        let five = both_ways("<body><p>one<br><br><br><br><br>two</p></body>");
        let twenty = both_ways(&format!("<body><p>one{}two</p></body>", "<br>".repeat(20)));

        assert_eq!(
            five.0, one.0,
            "five breaks should read as one in the document fallback"
        );
        assert_eq!(twenty.0, one.0, "and so should twenty");

        // The author's layout is left exactly alone. They asked for those
        // breaks, and the authored rendering is where that is honoured.
        assert!(
            five.1 > one.1 && twenty.1 > five.1,
            "the author's own layout lost breaks it asked for: {one:?} {five:?} {twenty:?}"
        );
    }

    #[test]
    fn a_single_break_survives_and_so_does_one_with_words_around_it() {
        // One break between two lines is almost always meant — an address, a
        // verse, a signature — so collapsing to none would be destroying
        // content rather than spacing. And two breaks with text between them
        // are not a run at all.
        let plain = both_ways("<body><p>one two</p></body>");
        let broken = both_ways("<body><p>one<br>two</p></body>");
        assert!(
            broken.0 > plain.0,
            "the one break a reader meant was dropped"
        );

        let apart = both_ways("<body><p>one<br>mid<br>two</p></body>");
        assert!(
            apart.0 > broken.0,
            "two breaks with words between them are not a run"
        );
    }

    #[test]
    fn whitespace_between_two_breaks_does_not_keep_them_apart() {
        // `<br>\n  <br>` is two breaks with a newline between them, and that
        // newline collapses to nothing — so on the page they are consecutive
        // even though they are not consecutive in the markup. Markup written by
        // hand is full of this.
        let tight = both_ways("<body><p>one<br><br><br>two</p></body>");
        let spaced = both_ways("<body><p>one<br>\n  <br>\n\t<br>two</p></body>");
        assert_eq!(
            spaced.0, tight.0,
            "newlines between the breaks stopped them being seen as a run"
        );
    }

    #[test]
    fn a_page_that_needs_no_fallback_can_still_be_given_one() {
        // The capability `force_authored` had no counterpart. Inverting it does
        // not produce this: an ordinary page classifies as `Authored`, so there
        // was no way to ask for the document fallback on one, which is exactly
        // what a reader wanting a simplified view of a working page asks for.
        let html = "<body><h1>Title</h1><p>An ordinary paragraph.</p></body>";
        let mut fonts = FontStore::new();

        let ordinary = render(html, 800, 2000, &mut fonts);
        assert!(
            matches!(ordinary.mode, RenderMode::Authored),
            "this fixture is only useful while it needs no fallback: {:?}",
            ordinary.mode
        );

        let forced = render_as_document_with(
            html,
            800,
            0,
            2000,
            &mut fonts,
            &mut DirectLoader::default(),
            None,
        );
        assert!(
            matches!(forced.mode, RenderMode::Document { .. }),
            "asking for the fallback did not produce one: {:?}",
            forced.mode
        );
        // And it is not the same rendering wearing a different label: the
        // reader sheet replaces the author's, so the pixels have to differ.
        assert_ne!(
            ordinary.pixmap.data(),
            forced.pixmap.data(),
            "the forced fallback rendered identically to the author's layout"
        );
    }

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
    fn a_link_whose_text_is_wrapped_in_a_span_is_still_somewhere_to_click() {
        // The window hit-tests against this list and not against the box tree,
        // because the box tree is in another process — so a link with no
        // rectangle in it is a link that does not work: no cursor, no keyboard
        // focus, nothing under the pointer for a click to land on (#52).
        //
        // An inline link generates no box of its own and is found through the
        // text spans naming it, and a span names the element its text is
        // *directly* in. Wikipedia writes every footnote marker as
        // `<a href="…"><span>[1]</span></a>`, and there are 951 of them on one
        // article.
        let (page, _) = page_at(
            "nested.html",
            r#"<body><p><a href="b.html"><span><b>there</b></span></a></p></body>"#,
        );
        let link = page.link_groups().pop().expect("the link is on the page");

        assert!(!link.rects.is_empty(), "the link has no geometry");
        let bounds = link.bounds();
        assert_eq!(
            page.link_at(
                bounds.x + bounds.width / 2.0,
                bounds.y + bounds.height / 2.0
            ),
            Some(link.url.clone()),
            "the rectangle is not where the link actually is"
        );
    }

    #[test]
    fn a_fragment_can_name_something_whose_text_is_wrapped_up_too() {
        // The other end of the same problem. A Wikipedia footnote's back-link
        // points at `<sup id="cite_ref-1"><a>…</a></sup>`, and asking the
        // `<sup>` alone where it is comes back with nothing.
        let (page, _) = page_at(
            "wrapped-target.html",
            r##"<body><p><a href="#marker">back</a></p>
                <p style="height: 300px">spacer</p>
                <sup id="marker"><span>[1]</span></sup></body>"##,
        );

        assert!(
            page.link_groups()[0].jump_to.is_some_and(|top| top > 100.0),
            "the wrapped target was not found: {:?}",
            page.link_groups()[0].jump_to
        );
    }

    #[test]
    fn a_fragment_link_goes_to_a_place_on_the_page_rather_than_nowhere() {
        // It names a destination inside this document, so there is nothing to
        // fetch — and for want of anywhere else to put that, these were being
        // left out of the link list altogether. They took no cursor, took no
        // keyboard focus, and did nothing at all when clicked. The Wikipedia
        // article I was testing against has 690 of them: "Jump to content",
        // every line of the contents, and every footnote marker (#52).
        let (page, _) = page_at(
            "frag.html",
            r##"<body><p><a href="#section">jump</a></p>
                <p style="height: 400px">spacer</p>
                <h2 id="section">Section</h2></body>"##,
        );
        let link = page.link_groups().pop().expect("the fragment is a link");
        let heading = page.frames[0]
            .doc
            .find_element("h2")
            .expect("the heading exists");
        let top = page.frames[0].layout.rects_for(heading)[0].y;

        assert_eq!(link.jump_to, Some(top));
        assert!(top > 100.0, "the spacer did not push the heading down");
    }

    #[test]
    fn a_bare_hash_goes_back_to_the_top() {
        // HTML calls the top of the page the indicated part when the fragment
        // is empty, and `href="#"` is a common enough spelling of "back to the
        // top" that treating it as a link to nowhere leaves a visibly dead
        // link on the page.
        let (page, _) = page_at(
            "top.html",
            r##"<body><p style="height: 400px">spacer</p>
                <p><a href="#">back to the top</a></p></body>"##,
        );

        assert_eq!(page.link_groups()[0].jump_to, Some(0.0));
    }

    #[test]
    fn a_target_that_wraps_is_scrolled_to_its_first_line_and_not_its_last() {
        // Landing on the last line of the destination puts the start of it
        // above the window, which is the one place the reader was told to
        // look.
        let (page, _) = page_at(
            "wrapping-target.html",
            &format!(
                r##"<body><p><a href="#long">down</a></p>
                    <p style="height: 200px">spacer</p>
                    <p><span id="long">{}</span></p></body>"##,
                "a destination long enough to take several lines ".repeat(8)
            ),
        );
        let target = page.frames[0]
            .doc
            .find_element("span")
            .expect("the target exists");
        let rects = page.frames[0].layout.rects_for(target);
        let (first, last) = (rects[0].y, rects[rects.len() - 1].y);

        assert!(last > first, "the target did not wrap: {rects:?}");
        assert_eq!(page.link_groups()[0].jump_to, Some(first));
    }

    #[test]
    fn a_fragment_naming_nothing_on_the_page_goes_nowhere() {
        // A stale anchor is a link to nowhere. Doing nothing is the honest
        // answer; scrolling to the top instead would look like the page had
        // jumped for no reason.
        let (page, _) = page_at(
            "stale.html",
            r##"<body><p><a href="#gone">jump</a></p></body>"##,
        );

        assert_eq!(page.link_groups()[0].jump_to, None);
    }

    #[test]
    fn an_old_pages_named_anchor_is_a_destination_too() {
        // `<a name="top">` is how a page written before `id` was universal
        // marks its own sections, and the era this browser is for is full of
        // them.
        let (page, _) = page_at(
            "named.html",
            r##"<body><p><a href="#top">up</a></p>
                <p style="height: 300px">spacer</p>
                <a name="top">here</a></body>"##,
        );

        assert!(
            page.link_groups()[0].jump_to.is_some_and(|top| top > 100.0),
            "the named anchor was not found: {:?}",
            page.link_groups()[0].jump_to
        );
    }

    #[test]
    fn a_link_that_leaves_the_page_has_nowhere_on_it_to_go() {
        let (page, _) = page_at(
            "away.html",
            r#"<body><p><a href="b.html">there</a></p></body>"#,
        );

        assert_eq!(page.link_groups()[0].jump_to, None);
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
