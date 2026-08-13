//! Box tree construction and block layout.
//!
//! Block boxes stack vertically, each laying its inline content out as an
//! inline formatting context: differently-styled spans share line boxes and
//! break as one paragraph. Floats, tables, and positioning are the rest of M2
//! (ADR-0004).

pub mod classify;
pub mod floats;
pub mod frameset;
pub mod table;

use css::cascade::StyleMap;
use css::style::{
    ComputedStyle, Display, Float, Overflow, Position, TextAlign, VerticalAlign, WhiteSpace,
};
use css::value::Length;
use dom::{Document, NodeId};
use floats::FloatContext;
use text::{FontStore, InlineRun, TextLayout};

pub use classify::{RenderMode, classify};

/// Intrinsic sizes of replaced elements, keyed by node.
///
/// Layout needs an image's natural size but must not depend on the decoder, so
/// the sizes are handed in rather than looked up.
pub type IntrinsicSizes = std::collections::HashMap<NodeId, (f32, f32)>;

/// Default box for an image that has not loaded, matching what browsers show
/// for a broken image with no dimensions given.
const BROKEN_IMAGE_SIZE: (f32, f32) = (20.0, 20.0);

/// The containing block that absolutely positioned descendants resolve against.
///
/// Boxes are stored parent-relative, but an absolutely positioned element is
/// placed against a possibly distant ancestor. Carrying that ancestor's size,
/// plus where the current box sits inside it, is what lets the two coordinate
/// systems be reconciled without a second tree walk.
#[derive(Debug, Clone, Copy)]
pub struct ContainingBlock {
    /// Position of the current box's content origin within the containing
    /// block's coordinate system.
    offset: (f32, f32),
    /// The containing block's content size.
    size: (f32, f32),
}

impl ContainingBlock {
    /// The initial containing block: the viewport.
    fn viewport(width: f32, height: f32) -> Self {
        Self {
            offset: (0.0, 0.0),
            size: (width, height),
        }
    }

    /// This containing block seen from a child at `(dx, dy)` in local content
    /// coordinates.
    fn descend(self, dx: f32, dy: f32) -> Self {
        Self {
            offset: (self.offset.0 + dx, self.offset.1 + dy),
            size: self.size,
        }
    }

    /// A box establishing itself as the containing block for its descendants.
    fn establish(size: (f32, f32)) -> Self {
        Self {
            offset: (0.0, 0.0),
            size,
        }
    }
}

/// Resolves an absolutely positioned box's offset within its containing block.
///
/// `left` wins over `right` when both are given, which is the correct
/// behaviour for left-to-right text; a box with neither stays where normal flow
/// would have put it, which is what makes `position: absolute` with no offsets
/// behave like a hoisted static box.
fn absolute_offset(
    style: &ComputedStyle,
    containing: (f32, f32),
    size: (f32, f32),
    static_position: (f32, f32),
) -> (f32, f32) {
    let font_size = style.font_size;
    let offsets = style.offsets;

    let x = match (offsets.left, offsets.right) {
        (Length::Auto, Length::Auto) => static_position.0,
        (Length::Auto, right) => containing.0 - right.to_px(font_size, containing.0) - size.0,
        (left, _) => left.to_px(font_size, containing.0),
    };
    let y = match (offsets.top, offsets.bottom) {
        (Length::Auto, Length::Auto) => static_position.1,
        (Length::Auto, bottom) => containing.1 - bottom.to_px(font_size, containing.1) - size.1,
        (top, _) => top.to_px(font_size, containing.1),
    };
    (x, y)
}

/// Shift applied by `position: relative`, which moves the box without
/// disturbing anything around it.
fn relative_shift(style: &ComputedStyle, containing: (f32, f32)) -> (f32, f32) {
    let font_size = style.font_size;
    let offsets = style.offsets;
    let x = match (offsets.left, offsets.right) {
        (Length::Auto, Length::Auto) => 0.0,
        (Length::Auto, right) => -right.to_px(font_size, containing.0),
        (left, _) => left.to_px(font_size, containing.0),
    };
    let y = match (offsets.top, offsets.bottom) {
        (Length::Auto, Length::Auto) => 0.0,
        (Length::Auto, bottom) => -bottom.to_px(font_size, containing.1),
        (top, _) => top.to_px(font_size, containing.1),
    };
    (x, y)
}

/// Resolves a replaced element's used size.
///
/// When only one dimension is given the other follows from the intrinsic
/// aspect ratio, which is what keeps `<img width="200">` from squashing.
pub fn replaced_size(
    style: &ComputedStyle,
    intrinsic: Option<(f32, f32)>,
    attr_width: Option<f32>,
    attr_height: Option<f32>,
    available_width: f32,
) -> (f32, f32) {
    let font_size = style.font_size;
    // CSS wins over the presentational attribute, which is only a fallback.
    let width = match style.width {
        Length::Auto => attr_width,
        length => Some(length.to_px(font_size, available_width)),
    };
    let height = match style.height {
        Length::Auto => attr_height,
        length => Some(length.to_px(font_size, available_width)),
    };

    match (width, height, intrinsic) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some((iw, ih))) if iw > 0.0 => (w, w * ih / iw),
        (None, Some(h), Some((iw, ih))) if ih > 0.0 => (h * iw / ih, h),
        (Some(w), None, None) => (w, w),
        (None, Some(h), None) => (h, h),
        (None, None, Some(size)) => size,
        (None, None, None) => BROKEN_IMAGE_SIZE,
        (Some(w), None, Some(_)) => (w, w),
        (None, Some(h), Some(_)) => (h, h),
    }
}

/// Whether an element is a replaced element this engine lays out as a box of
/// intrinsic size rather than from its children.
fn is_replaced(doc: &Document, node: NodeId) -> bool {
    doc.element(node).is_some_and(|e| e.local_name() == "img")
}

/// Reads a presentational width/height attribute, which the era's markup used
/// far more than CSS.
fn size_attr(doc: &Document, node: NodeId, name: &str) -> Option<f32> {
    let value = doc.element(node)?.attr(name)?.trim();
    // Percentages in these attributes are not supported; they were rare and
    // ambiguous, and treating them as pixels would be worse than ignoring them.
    if value.ends_with('%') {
        return None;
    }
    value.parse::<f32>().ok().filter(|v| *v >= 0.0)
}

/// A rectangle in CSS pixels, with the origin at the top left of the canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

/// A laid-out box.
#[derive(Debug, Clone)]
pub struct LayoutBox {
    /// Border-box geometry.
    pub rect: Rect,
    /// The style this box was laid out with.
    pub style: ComputedStyle,
    /// Inline content, already wrapped, positioned relative to the content box.
    pub text: Option<TextLayout>,
    /// Offset of the content box within `rect`.
    pub content_origin: (f32, f32),
    /// Width of the content box.
    ///
    /// Stored rather than derived: with asymmetric borders and padding the
    /// content width cannot be recovered from `rect` and `content_origin`.
    pub content_width: f32,
    /// Child boxes.
    pub children: Vec<LayoutBox>,
    /// Set when this box is a replaced element, naming the node so paint can
    /// find its decoded image.
    pub replaced: Option<NodeId>,
    /// The element this box was generated from, where there is one.
    ///
    /// Anonymous boxes — the canvas root, a list marker — have none. Paint uses
    /// it to find the element's background image, and hit testing will want it.
    pub node: Option<NodeId>,
}

/// The single margin that two adjoining ones collapse into (CSS 2.1 §8.3.1).
///
/// Not `max`, which is the rule everyone remembers and is only the rule while
/// both are positive. The spec is stated over the whole set: take the largest
/// positive, take the most negative, and add them. For two margins that comes
/// out as the maximum when both are positive, the *minimum* when both are
/// negative — two -20px margins pull by 20, not 40 — and the sum when they
/// disagree, which is how a negative margin cancels a positive one.
fn collapse(first: f32, second: f32) -> f32 {
    first.max(second).max(0.0) + first.min(second).min(0.0)
}

/// Whether a box's children's margins stay inside it.
///
/// A margin collapses *through* a box's top edge only when nothing separates
/// them — no top border, no top padding — and only when the box is part of its
/// parent's flow. A float, an absolutely positioned box, a table cell, and
/// anything with `overflow` other than `visible` each establish a formatting
/// context of their own, and a margin cannot escape one: letting a float's
/// first child pull the float upwards would move it out from under the text
/// flowing beside it.
///
/// `overflow` is in that list for a reason found rather than remembered. The
/// CSS 2.1 suite has a container with `overflow: hidden` whose last child
/// carries `margin-bottom: -200px`, sized so that the negative margin exactly
/// cancels the child if it stays inside and reveals a red block if it escapes.
/// It escaped. The property was not modelled at all until then, so the box did
/// not know it was a formatting context.
fn keeps_its_childrens_margins(style: &ComputedStyle) -> bool {
    style.float != Float::None
        || style.position.is_out_of_flow()
        || style.display == Display::TableCell
        || style.overflow != Overflow::Visible
}

/// What laying out one block cost its parent, with its margins kept apart.
///
/// A single "outer height" was enough while margins only ever added up. It is
/// not enough to collapse them: a parent has to know its child's *top* margin
/// separately, because a first child's top margin can escape the parent
/// entirely and become the parent's own (§8.3.1), and a caller placing the next
/// sibling has to collapse against the previous one's bottom margin rather than
/// against a total it can no longer take apart.
#[derive(Debug, Clone, Copy)]
struct Consumed {
    /// Border-box height, margins excluded.
    height: f32,
    /// Top margin, after collapsing with anything inside the box.
    margin_top: f32,
    /// Bottom margin, after collapsing with anything inside the box.
    margin_bottom: f32,
    /// Whether the box's own top and bottom margins are adjoining, so that a
    /// margin collapses straight *through* it (§8.3.1).
    ///
    /// True of a box with nothing in it and nothing separating its two edges —
    /// no content, no border, no padding, no height of its own. Such a box
    /// contributes one margin to the run it sits in rather than two, and takes
    /// up no height at all.
    ///
    /// This is the gap the README named as "margin collapsing does not handle
    /// an empty block collapsing through itself". It is worse on the modern web
    /// than that sounds: those pages are built out of empty wrappers, and every
    /// one of them was adding a margin nobody wrote. It showed worst in the
    /// document fallback, where the author's own `margin: 0` resets go out with
    /// the rest of their stylesheet and the UA sheet's margins then apply to
    /// everything.
    collapses_through: bool,
}

impl Consumed {
    /// Everything the box occupies, which is what a caller that does not
    /// collapse still wants.
    fn outer(self) -> f32 {
        self.margin_top + self.height + self.margin_bottom
    }
}

/// The result of laying out a document.
#[derive(Debug, Clone)]
pub struct Layout {
    /// Root box, covering the whole canvas.
    pub root: LayoutBox,
    /// Total content height, which may exceed the viewport.
    pub height: f32,
    /// Background image the whole canvas takes, with the element it came from
    /// and how it repeats. Propagated from the root or the body by the same
    /// §14.2 rule as the colour, and for the same reason: a tile that stopped
    /// at the content height would leave a band of blank canvas below it.
    pub canvas_image: Option<(
        NodeId,
        css::style::BackgroundRepeat,
        css::style::BackgroundPosition,
    )>,
    /// Colour the whole canvas takes, per CSS 2.1 §14.2.
    ///
    /// The root element's background covers the canvas however tall that
    /// canvas is, and when the root has none the body's is used instead. This
    /// is not a detail: a page is usually shorter than the window it is shown
    /// in, so without it a `<body bgcolor>` page ends in a band of white.
    pub canvas_background: css::Color,
}

impl Layout {
    /// The element at a point, in canvas coordinates.
    ///
    /// Later boxes win, which is paint order: whatever was drawn last is what
    /// a person sees at that point and therefore what they mean to click.
    ///
    /// A point over text resolves to the element that *wrapped* the text, not
    /// to the block containing it. That is the whole difficulty — an inline
    /// element has no box, so a link only has a rectangle once the line breaker
    /// has said where its glyphs went.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        hit_test_box(&self.root, x, y, 0.0, 0.0)
    }

    /// Every rectangle where `query` appears, in canvas coordinates and in
    /// reading order.
    ///
    /// Matched case-insensitively, which is what a reader means by "find".
    /// Matching is per line: a phrase broken across a line break is not found,
    /// because the two halves are not one run of text on the screen and there
    /// would be no single rectangle to show for it.
    pub fn find(&self, query: &str) -> Vec<Rect> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let needle = query.to_lowercase();
        let mut out = Vec::new();
        collect_matches(&self.root, &needle, 0.0, 0.0, &mut out);
        // Reading order: down the page, then across. Tree order is nearly this
        // but not exactly — a float is laid out before the text beside it.
        out.sort_by(|a, b| {
            a.y.partial_cmp(&b.y)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
        });
        out
    }

    /// Every rectangle belonging to `node`, in canvas coordinates.
    ///
    /// A link wrapping onto three lines has three, which is what drawing a
    /// focus ring around it needs — one bounding box would swallow the text
    /// either side of it on the first and last lines.
    pub fn rects_for(&self, node: NodeId) -> Vec<Rect> {
        let mut out = Vec::new();
        collect_rects(&self.root, node, 0.0, 0.0, &mut out);
        out
    }
}

/// Depth-first, last match wins.
fn hit_test_box(box_: &LayoutBox, x: f32, y: f32, offset_x: f32, offset_y: f32) -> Option<NodeId> {
    let left = offset_x + box_.rect.x;
    let top = offset_y + box_.rect.y;

    // Children first and in reverse, so the last-painted match is found first.
    for child in box_.children.iter().rev() {
        if let Some(hit) = hit_test_box(child, x, y, left, top) {
            return Some(hit);
        }
    }

    if let Some(text) = &box_.text {
        let content_x = left + box_.content_origin.0;
        let content_y = top + box_.content_origin.1;
        for line in &text.lines {
            let dx = line_offset(box_.style.text_align, line.width, box_.content_width);
            for span in &line.spans {
                let span_x = content_x + dx + span.x;
                let span_y = content_y + span.y;
                if x >= span_x && x < span_x + span.width && y >= span_y && y < span_y + span.height
                {
                    return Some(NodeId(span.source));
                }
            }
        }
    }

    // The box's own area, last: a child or a span inside it is the more
    // specific answer and has already had its chance.
    let inside = x >= left && x < left + box_.rect.width && y >= top && y < top + box_.rect.height;
    if inside { box_.node } else { None }
}

fn collect_matches(
    box_: &LayoutBox,
    needle: &str,
    offset_x: f32,
    offset_y: f32,
    out: &mut Vec<Rect>,
) {
    let left = offset_x + box_.rect.x;
    let top = offset_y + box_.rect.y;

    if let Some(text) = &box_.text {
        let content_x = left + box_.content_origin.0;
        let content_y = top + box_.content_origin.1;
        for line in &text.lines {
            if line.glyphs.is_empty() {
                continue;
            }
            let dx = line_offset(box_.style.text_align, line.width, box_.content_width);
            let lowered = line.text.to_lowercase();
            // Lowercasing can change a string's length — `İ` becomes two chars
            // — so an offset into the lowered text is not an offset into the
            // original, and the glyph offsets index the original. Only trust
            // them when the two agree.
            if lowered.len() != line.text.len() {
                continue;
            }
            let mut from = 0usize;
            while let Some(found) = lowered[from..].find(needle) {
                let start = from + found;
                let end = start + needle.len();
                if let Some(rect) = span_rect(line, start, end, content_x + dx, content_y) {
                    out.push(rect);
                }
                // Overlapping matches are not what anyone means by "next".
                from = end.max(start + 1);
            }
        }
    }

    for child in &box_.children {
        collect_matches(child, needle, left, top, out);
    }
}

/// The rectangle covering a byte range of a line's text.
fn span_rect(
    line: &text::Line,
    start: usize,
    end: usize,
    origin_x: f32,
    origin_y: f32,
) -> Option<Rect> {
    // A glyph covers a byte range; the match covers one too, and the glyphs
    // that matter are the ones that overlap it.
    let mut left = f32::MAX;
    let mut right = f32::MIN;
    for (index, glyph) in line.glyphs.iter().enumerate() {
        if glyph.end <= start || glyph.start >= end {
            continue;
        }
        left = left.min(glyph.x);
        // A glyph carries no advance; the next one's origin is where it ends,
        // and for the last, the line's width is.
        let glyph_right = line
            .glyphs
            .get(index + 1)
            .map(|next| next.x)
            .unwrap_or(line.width);
        right = right.max(glyph_right);
    }
    if left > right {
        return None;
    }
    Some(Rect {
        x: origin_x + left,
        y: origin_y + line.glyphs.first()?.y - line.baseline,
        width: right - left,
        height: line.baseline * 1.25,
    })
}

fn collect_rects(
    box_: &LayoutBox,
    node: NodeId,
    offset_x: f32,
    offset_y: f32,
    out: &mut Vec<Rect>,
) {
    let left = offset_x + box_.rect.x;
    let top = offset_y + box_.rect.y;

    if box_.node == Some(node) {
        out.push(Rect {
            x: left,
            y: top,
            width: box_.rect.width,
            height: box_.rect.height,
        });
    }
    if let Some(text) = &box_.text {
        let content_x = left + box_.content_origin.0;
        let content_y = top + box_.content_origin.1;
        for line in &text.lines {
            let dx = line_offset(box_.style.text_align, line.width, box_.content_width);
            for span in line.spans.iter().filter(|span| span.source == node.0) {
                out.push(Rect {
                    x: content_x + dx + span.x,
                    y: content_y + span.y,
                    width: span.width,
                    height: span.height,
                });
            }
        }
    }
    for child in &box_.children {
        collect_rects(child, node, left, top, out);
    }
}

/// Lays out a styled document at a given viewport width.
pub fn layout(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    intrinsic: &IntrinsicSizes,
    viewport_width: f32,
) -> Layout {
    let body = doc.find_element("body").unwrap_or_else(|| doc.root());
    let body_style = styles.get(body).cloned().unwrap_or_default();

    let mut root = LayoutBox {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: viewport_width,
            height: 0.0,
        },
        style: body_style.clone(),
        text: None,
        content_origin: (0.0, 0.0),
        content_width: viewport_width,
        children: Vec::new(),
        replaced: None,
        node: None,
    };

    let height = layout_block(
        doc,
        styles,
        fonts,
        body,
        &body_style,
        intrinsic,
        0.0,
        0.0,
        viewport_width,
        FloatContext::new(viewport_width),
        ContainingBlock::viewport(viewport_width, viewport_width),
        &mut root,
    );
    root.rect.height = height.outer();

    // §14.2: the root element's background paints the canvas, and only when it
    // has none does the body's get used in its place.
    let html = doc.find_element("html");
    let html_style = html.and_then(|node| styles.get(node));
    let html_background = html_style
        .map(|style| style.background_color)
        .unwrap_or(css::Color::TRANSPARENT);
    let canvas_background = if html_background.is_transparent() {
        body_style.background_color
    } else {
        html_background
    };

    // The image propagates independently of the colour: a root with a colour
    // and a body with a tile is ordinary markup, and both belong on the canvas.
    let canvas_image = match (html, html_style) {
        (Some(node), Some(style)) if style.background_image.is_some() => {
            Some((node, style.background_repeat, style.background_position))
        }
        _ if body_style.background_image.is_some() => Some((
            body,
            body_style.background_repeat,
            body_style.background_position,
        )),
        _ => None,
    };

    Layout {
        root,
        height: height.outer(),
        canvas_background,
        canvas_image,
    }
}

/// Gap between a list marker and the item's content edge.
const MARKER_GAP: f32 = 0.4;

/// The number this list item counts as.
///
/// `<ol start>` moves where a list begins and `<li value>` restarts it
/// mid-way; both were ordinary markup, used for lists split across pages and
/// for numbered steps resumed after an aside.
fn list_ordinal(doc: &Document, styles: &StyleMap, node: NodeId) -> usize {
    let Some(parent) = doc.node(node).parent else {
        return 1;
    };
    let mut ordinal: i64 = doc
        .element(parent)
        .and_then(|element| element.attr("start"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(1);

    for &sibling in doc.children(parent) {
        if !styles
            .get(sibling)
            .is_some_and(|style| style.display == Display::ListItem)
        {
            continue;
        }
        if let Some(value) = doc
            .element(sibling)
            .and_then(|element| element.attr("value"))
            .and_then(|value| value.trim().parse().ok())
        {
            ordinal = value;
        }
        if sibling == node {
            break;
        }
        ordinal += 1;
    }
    ordinal.max(0) as usize
}

/// Builds the marker box for a list item, to sit in the list's left padding.
///
/// Returned as an ordinary box with text rather than drawn specially, so the
/// marker picks up the item's font and colour the way it should — a red `<li>`
/// has a red bullet.
fn marker_box(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    node: NodeId,
    style: &ComputedStyle,
    content_origin: (f32, f32),
) -> Option<LayoutBox> {
    let ordinal = if style.list_style_type.is_ordered() {
        list_ordinal(doc, styles, node)
    } else {
        1
    };
    let text = style.list_style_type.marker(ordinal);
    if text.is_empty() {
        return None;
    }

    // No wrapping: a marker is a few characters and must stay on one line.
    let layout = fonts.layout(&text, style, f32::MAX);
    let width = layout.width;
    Some(LayoutBox {
        rect: Rect {
            // To the left of the content edge, which puts it in the list's own
            // padding — where `ul { padding-left: 40px }` leaves room for it.
            x: content_origin.0 - width - style.font_size * MARKER_GAP,
            y: content_origin.1,
            width,
            height: layout.height,
        },
        style: style.clone(),
        text: Some(layout),
        content_origin: (0.0, 0.0),
        content_width: width,
        children: Vec::new(),
        replaced: None,
        node: None,
    })
}

/// Turns the line breaker's placements into real child boxes.
///
/// An inline image is still a box: it can carry a border, padding, and a
/// background, and a broken one has to show its frame. Emitting boxes rather
/// than painting the placements directly means all of that goes through the
/// ordinary box-painting path instead of being special-cased.
fn emit_replaced_boxes(
    styles: &StyleMap,
    layout: &TextLayout,
    style: &ComputedStyle,
    origin: (f32, f32),
    content_width: f32,
    parent: &mut LayoutBox,
) {
    for line in &layout.lines {
        // The same shift paint applies to the line's glyphs, so a centred line
        // carries its images along with its text.
        let dx = line_offset(style.text_align, line.width, content_width);
        for placed in &line.replaced {
            let node = NodeId(placed.id);
            let Some(child_style) = styles.get(node) else {
                continue;
            };
            let font_size = child_style.font_size;
            let left = child_style.border.left.used_width(font_size)
                + child_style.padding.left.to_px(font_size, content_width);
            let right = child_style.border.right.used_width(font_size)
                + child_style.padding.right.to_px(font_size, content_width);
            let top = child_style.border.top.used_width(font_size)
                + child_style.padding.top.to_px(font_size, content_width);

            parent.children.push(LayoutBox {
                rect: Rect {
                    x: origin.0 + dx + placed.x,
                    y: origin.1 + placed.y,
                    width: placed.width,
                    height: placed.height,
                },
                style: child_style.clone(),
                text: None,
                content_origin: (left, top),
                content_width: (placed.width - left - right).max(0.0),
                children: Vec::new(),
                replaced: Some(node),
                node: Some(node),
            });
        }
    }
}

/// Whether an inline element has a block-level element inside it.
///
/// `<font>…<hr>…</font>` is ordinary in the era's markup, and an inline box
/// cannot contain a block one: CSS 2.1 §9.2.1.1 says the inline box is split
/// around it. Splitting properly is a larger piece of machinery than this
/// engine has; treating the offending inline element as a block instead
/// produces the same visual result for the shapes that actually occur — the
/// content before the block, the block, the content after — because a block
/// container with mixed children already handles exactly that.
///
/// Without this the block child is never laid out at all: it is skipped when
/// gathering inline runs and never reached by the block walk, so it vanishes.
fn contains_block(doc: &Document, styles: &StyleMap, node: NodeId, depth: usize) -> bool {
    if depth >= MAX_INTRINSIC_DEPTH {
        return false;
    }
    doc.children(node).iter().any(|&child| {
        styles.get(child).is_some_and(|style| {
            if style.display == Display::None || style.position.is_out_of_flow() {
                return false;
            }
            if !style.display.is_inline() {
                return true;
            }
            contains_block(doc, styles, child, depth + 1)
        })
    })
}

/// Whether a child participates in its parent's inline formatting context.
///
/// An inline element wrapping a block one does not: see [`contains_block`].
fn is_inline_child(doc: &Document, styles: &StyleMap, node: NodeId, style: &ComputedStyle) -> bool {
    style.display.is_inline() && !contains_block(doc, styles, node, 0)
}

/// How deep the intrinsic-width walk goes before giving up.
///
/// Era pages nest tables several deep on purpose; a hostile one can nest them
/// arbitrarily. The cap bounds the work without affecting anything real.
const MAX_INTRINSIC_DEPTH: usize = 24;

/// Intrinsic widths of a whole subtree, as `(minimum, preferred)`.
///
/// Measuring a box's inline runs alone reports zero for anything whose content
/// is not text — a cell holding a nested table, an image, or a `<div>` — and a
/// table column sized from that collapses to nothing. Since the era's pages are
/// built out of tables inside tables, that is not an edge case: it is the
/// common shape.
#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn subtree_widths(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    node: NodeId,
    style: &ComputedStyle,
    intrinsic: &IntrinsicSizes,
    available: f32,
    depth: usize,
) -> (f32, f32) {
    let font_size = style.font_size;
    let surround = style.padding.left.to_px(font_size, available)
        + style.padding.right.to_px(font_size, available)
        + style.border.left.used_width(font_size)
        + style.border.right.used_width(font_size);

    // A declared width settles it: the box wants exactly that much, whatever
    // is inside. This is how `<td width="150">` sizes its column, which is how
    // the era's layout tables were built.
    if let Length::Px(width) = style.width {
        return (width + surround, width + surround);
    }
    if depth >= MAX_INTRINSIC_DEPTH {
        return (0.0, 0.0);
    }

    if is_replaced(doc, node) {
        let (width, _) = replaced_size(
            style,
            intrinsic.get(&node).copied(),
            size_attr(doc, node, "width"),
            size_attr(doc, node, "height"),
            available,
        );
        return (width + surround, width + surround);
    }

    if style.display == Display::Table {
        let (min, max) = table_widths(doc, styles, fonts, node, style, intrinsic, available, depth);
        return (min + surround, max + surround);
    }

    // The box's own inline content, then every block child, whichever is
    // widest — they stack, so the container must fit the widest of them.
    let runs = collect_inline_runs(doc, styles, node, style, intrinsic, available);
    let (mut min, mut max) = fonts.intrinsic_widths(&runs, style);

    for &child in doc.children(node) {
        let Some(child_style) = styles.get(child) else {
            continue;
        };
        if child_style.display == Display::None
            || is_inline_child(doc, styles, child, child_style)
            || child_style.display.is_table_internal()
            || child_style.position.is_out_of_flow()
        {
            continue;
        }
        let (child_min, child_max) = subtree_widths(
            doc,
            styles,
            fonts,
            child,
            child_style,
            intrinsic,
            available,
            depth + 1,
        );
        let margins = child_style
            .margin
            .left
            .to_px(child_style.font_size, available)
            + child_style
                .margin
                .right
                .to_px(child_style.font_size, available);
        min = min.max(child_min + margins);
        max = max.max(child_max + margins);
    }
    (min + surround, max.max(min) + surround)
}

/// Intrinsic widths of a table's content box, summed across its columns.
#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn table_widths(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    node: NodeId,
    style: &ComputedStyle,
    intrinsic: &IntrinsicSizes,
    available: f32,
    depth: usize,
) -> (f32, f32) {
    let grid = table::build_grid(doc, styles, node);
    if grid.columns == 0 {
        return (0.0, 0.0);
    }
    let spacing = style
        .border_spacing
        .to_px(style.font_size, available)
        .max(0.0);

    let mut mins = vec![0.0f32; grid.columns];
    let mut maxes = vec![0.0f32; grid.columns];
    let mut spans: Vec<(usize, usize, f32, f32)> = Vec::new();

    for row in &grid.rows {
        for cell in &row.cells {
            let (min, max) = subtree_widths(
                doc,
                styles,
                fonts,
                cell.node,
                &cell.style,
                intrinsic,
                available,
                depth + 1,
            );
            if cell.colspan == 1 {
                if let (Some(column_min), Some(column_max)) =
                    (mins.get_mut(cell.column), maxes.get_mut(cell.column))
                {
                    *column_min = column_min.max(min);
                    *column_max = column_max.max(max);
                }
            } else {
                spans.push((cell.column, cell.colspan, min, max));
            }
        }
    }
    for (column, colspan, min, max) in spans {
        table::apply_span(&mut mins, column, colspan, min, spacing);
        table::apply_span(&mut maxes, column, colspan, max, spacing);
    }

    let gaps = spacing * (grid.columns + 1) as f32;
    (
        mins.iter().sum::<f32>() + gaps,
        maxes.iter().sum::<f32>() + gaps,
    )
}

/// Resolves `margin-left: auto` and `margin-right: auto` against the space a
/// box leaves over.
///
/// Two auto margins split it, which centres the box. One takes all of it,
/// which pushes the box to the other side. Neither, and the leftover simply
/// sits to the right, as an over-constrained box does in left-to-right text.
fn distribute_auto_margins(style: &ComputedStyle, leftover: f32, left: &mut f32, right: &mut f32) {
    let leftover = leftover.max(0.0);
    match (style.margin.left, style.margin.right) {
        (Length::Auto, Length::Auto) => {
            *left = leftover / 2.0;
            *right = leftover / 2.0;
        }
        (Length::Auto, _) => *left = (leftover - *right).max(0.0),
        (_, Length::Auto) => *right = (leftover - *left).max(0.0),
        _ => {}
    }
}

/// Lays out a stretch of inline children as an anonymous block box.
///
/// CSS 2.1 §9.2.1.1: when a block container holds both inline and block
/// children, each run of inline content is wrapped in an anonymous block. It
/// matters because without it every scrap of inline content in the container
/// is hoisted above every block one — `<img><p>caption</p><img>` puts both
/// images at the top rather than one on each side of the caption.
///
/// Returns the height consumed. `pending` is emptied.
#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn flush_inline(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    pending: &mut Vec<NodeId>,
    holder: NodeId,
    style: &ComputedStyle,
    intrinsic: &IntrinsicSizes,
    at: (f32, f32),
    content_width: f32,
    context: &FloatContext,
    content_top: f32,
    parent: &mut LayoutBox,
) -> f32 {
    if pending.is_empty() {
        return 0.0;
    }
    let children = std::mem::take(pending);
    let runs = inline_runs_for(
        doc,
        styles,
        &children,
        style,
        holder,
        intrinsic,
        content_width,
    );
    if !runs
        .iter()
        .any(|run| !run.text.trim().is_empty() || run.replaced.is_some())
    {
        return 0.0;
    }

    // Floats are seen from where this stretch actually starts, so text after a
    // block child still flows around a float that reaches down to it.
    let local = context.translated(0.0, at.1 - content_top, content_width);
    let layout = if local.is_empty() {
        fonts.layout_runs(&runs, style, content_width)
    } else {
        fonts.layout_runs_constrained(&runs, style, |y, height| local.line_box(y, height))
    };
    let height = layout.height;

    let mut anonymous = LayoutBox {
        rect: Rect {
            x: at.0,
            y: at.1,
            width: content_width,
            height,
        },
        // The parent's style, less anything that would paint: an anonymous box
        // is not an element and must not draw a second background or border.
        style: ComputedStyle {
            background_color: css::Color::TRANSPARENT,
            background_image: None,
            border: css::style::Borders::default(),
            margin: css::style::Edges::ZERO,
            padding: css::style::Edges::ZERO,
            ..style.clone()
        },
        text: Some(layout),
        content_origin: (0.0, 0.0),
        content_width,
        children: Vec::new(),
        replaced: None,
        node: None,
    };
    if let Some(laid_out) = &anonymous.text {
        let laid_out = laid_out.clone();
        emit_replaced_boxes(
            styles,
            &laid_out,
            style,
            (0.0, 0.0),
            content_width,
            &mut anonymous,
        );
    }
    parent.children.push(anonymous);
    height
}

/// Lays out `node` as a block box at `(x, y)` within `available_width`,
/// appending it to `parent`. Returns what it consumed, with its margins kept
/// separate so a caller can collapse against them.
#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn layout_block(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    node: NodeId,
    style: &ComputedStyle,
    intrinsic: &IntrinsicSizes,
    x: f32,
    y: f32,
    available_width: f32,
    inherited: FloatContext,
    containing: ContainingBlock,
    parent: &mut LayoutBox,
) -> Consumed {
    let font_size = style.font_size;
    let mut margin_left = style.margin.left.to_px(font_size, available_width);
    let mut margin_right = style.margin.right.to_px(font_size, available_width);
    let margin_top = style.margin.top.to_px(font_size, available_width);
    let padding_left = style.padding.left.to_px(font_size, available_width);
    let padding_right = style.padding.right.to_px(font_size, available_width);
    let padding_top = style.padding.top.to_px(font_size, available_width);
    let padding_bottom = style.padding.bottom.to_px(font_size, available_width);
    let border_left = style.border.left.used_width(font_size);
    let border_right = style.border.right.used_width(font_size);
    let border_top = style.border.top.used_width(font_size);
    let border_bottom = style.border.bottom.used_width(font_size);

    // CSS 2.1 `width` is the *content* width, so borders and padding grow the
    // box outwards rather than being absorbed by it.
    let surround = padding_left + padding_right + border_left + border_right;
    let outer_width = match style.width {
        Length::Auto => (available_width - margin_left - margin_right).max(0.0),
        length => length.to_px(font_size, available_width) + surround,
    };
    let content_width = (outer_width - surround).max(0.0);

    // CSS 2.1 §10.3.3. An auto margin beside a definite width takes the space
    // left over; two of them split it, which is how a block is centred and so
    // how both `margin: 0 auto` and `<table align="center">` work. With an auto
    // *width* there is nothing left over, and auto margins are zero.
    if style.width != Length::Auto {
        distribute_auto_margins(
            style,
            available_width - outer_width,
            &mut margin_left,
            &mut margin_right,
        );
        // `<center>` and `align="center"` centre their block children as well
        // as their text. `text-align` inherits, so a box can tell by looking at
        // its own — which is exactly why this needs its own value rather than
        // reusing plain `center`.
        if style.text_align == TextAlign::CenterBlocks
            && style.margin.left == Length::Px(0.0)
            && style.margin.right == Length::Px(0.0)
        {
            margin_left = ((available_width - outer_width) / 2.0).max(0.0);
        }
    }

    if is_replaced(doc, node) {
        let (image_width, image_height) = replaced_size(
            style,
            intrinsic.get(&node).copied(),
            size_attr(doc, node, "width"),
            size_attr(doc, node, "height"),
            available_width,
        );
        let box_ = LayoutBox {
            rect: Rect {
                x: x + margin_left,
                y: y + margin_top,
                width: image_width + surround,
                height: image_height + padding_top + padding_bottom + border_top + border_bottom,
            },
            style: style.clone(),
            text: None,
            content_origin: (padding_left + border_left, padding_top + border_top),
            content_width: image_width,
            children: Vec::new(),
            replaced: Some(node),
            node: Some(node),
        };
        let consumed = Consumed {
            height: box_.rect.height,
            margin_top,
            margin_bottom: style.margin.bottom.to_px(font_size, available_width),
            // A replaced box has content by definition, and a table is a
            // formatting context of its own; neither can have a margin pass
            // through it.
            collapses_through: false,
        };
        parent.children.push(box_);
        return consumed;
    }

    let mut box_ = LayoutBox {
        rect: Rect {
            x: x + margin_left,
            y: y + margin_top,
            width: outer_width,
            height: 0.0,
        },
        style: style.clone(),
        text: None,
        content_origin: (padding_left + border_left, padding_top + border_top),
        content_width,
        children: Vec::new(),
        replaced: None,
        node: Some(node),
    };

    // Inline children become styled runs shaped as one paragraph, so a <b> or
    // <code> inside this block keeps its own style while still breaking lines
    // with the text around it.
    //
    // A container whose children are *all* inline — every paragraph, every
    // heading, most cells — lays its text out directly on this box. One with a
    // block child anywhere among them cannot: its inline stretches have to be
    // laid out where they sit, which is what the anonymous boxes below do.
    let all_inline = !doc.children(node).iter().any(|&child| {
        styles.get(child).is_some_and(|child_style| {
            child_style.display != Display::None
                && !is_inline_child(doc, styles, child, child_style)
                && !child_style.display.is_table_internal()
                && child_style.float == Float::None
                && !child_style.position.is_out_of_flow()
        })
    });
    let runs = if all_inline {
        collect_inline_runs(doc, styles, node, style, intrinsic, content_width)
    } else {
        Vec::new()
    };
    let mut content_height = 0.0;
    // Floats declared by ancestors still apply here, shifted into this block's
    // coordinates; this block's own floats are added on top.
    let mut context = inherited.translated(
        padding_left + border_left,
        padding_top + border_top,
        content_width,
    );

    // Floats declared before any in-flow block are placed first, so this
    // block's own text knows to flow around them. Floats declared later are
    // placed during the walk below, at the height they actually appear —
    // placing everything up front would lift a float above content that
    // precedes it in the source.
    let mut early: Vec<(NodeId, ComputedStyle)> = Vec::new();
    let mut late: Vec<(NodeId, ComputedStyle)> = Vec::new();
    let mut seen_in_flow = false;
    for &child in doc.children(node) {
        let Some(child_style) = styles.get(child) else {
            continue;
        };
        if child_style.display == Display::None {
            continue;
        }
        if child_style.float != Float::None {
            if seen_in_flow {
                late.push((child, child_style.clone()));
            } else {
                early.push((child, child_style.clone()));
            }
        } else if !is_inline_child(doc, styles, child, child_style)
            && !child_style.display.is_table_internal()
        {
            seen_in_flow = true;
        }
    }
    for (child, child_style) in &early {
        place_float(
            doc,
            styles,
            fonts,
            *child,
            child_style,
            intrinsic,
            content_width,
            0.0,
            (padding_left + border_left, padding_top + border_top),
            &mut context,
            &mut box_,
        );
    }

    if style.display == Display::ListItem
        && let Some(marker) = marker_box(
            doc,
            styles,
            fonts,
            node,
            style,
            (padding_left + border_left, padding_top + border_top),
        )
    {
        box_.children.push(marker);
    }

    // A line's worth of content is text *or* an atomic box: a paragraph
    // holding nothing but an image still needs laying out.
    if runs
        .iter()
        .any(|run| !run.text.trim().is_empty() || run.replaced.is_some())
    {
        let layout = if context.is_empty() {
            fonts.layout_runs(&runs, style, content_width)
        } else {
            fonts.layout_runs_constrained(&runs, style, |y, height| context.line_box(y, height))
        };
        content_height = layout.height;
        emit_replaced_boxes(
            styles,
            &layout,
            style,
            (padding_left + border_left, padding_top + border_top),
            content_width,
            &mut box_,
        );
        box_.text = Some(layout);
    }

    if style.display == Display::Table {
        let (table_width, table_height) = layout_table(
            doc,
            styles,
            fonts,
            node,
            style,
            intrinsic,
            padding_left + border_left,
            padding_top + border_top,
            content_width,
            &mut box_,
        );
        box_.rect.height = padding_top + border_top + table_height + padding_bottom + border_bottom;
        // A table with no declared width shrinks to fit its columns, and so
        // must its box: left at the container's width, its border and
        // background stretch across the page while the cells huddle at one end.
        if style.width == Length::Auto {
            box_.rect.width = (table_width + surround).min(outer_width);
            // Now that the real width is known, auto margins have something to
            // work with. A shrink-to-fit table with `align="center"` has no
            // leftover space until this point, so centring it has to wait.
            let mut left = margin_left;
            let mut right = margin_right;
            distribute_auto_margins(
                style,
                available_width - box_.rect.width,
                &mut left,
                &mut right,
            );
            // A shrink-to-fit table inside `<center>` is the era's commonest
            // way of centring one, and its real width is only known now.
            if style.text_align == TextAlign::CenterBlocks
                && style.margin.left == Length::Px(0.0)
                && style.margin.right == Length::Px(0.0)
            {
                left = ((available_width - box_.rect.width) / 2.0).max(0.0);
            }
            box_.rect.x = x + left;
        }
        let consumed = Consumed {
            height: box_.rect.height,
            margin_top,
            margin_bottom: style.margin.bottom.to_px(font_size, available_width),
            // A replaced box has content by definition, and a table is a
            // formatting context of its own; neither can have a margin pass
            // through it.
            collapses_through: false,
        };
        parent.children.push(box_);
        return consumed;
    }

    let mut absolutes: Vec<(NodeId, ComputedStyle, f32)> = Vec::new();
    let mut cursor_y = padding_top + border_top + content_height;
    // Inline children seen since the last block child. Flushed as an anonymous
    // box when a block child arrives, and again at the end.
    let mut pending: Vec<NodeId> = Vec::new();
    // The bottom margin of the last in-flow block placed, kept so the next
    // one's top margin can collapse into it (§8.3.1). `None` means there is
    // nothing to collapse with — either nothing has been placed yet, or
    // something in between separated them.
    let mut previous_bottom: Option<f32> = None;
    // The first in-flow child's top margin, once it has escaped this box —
    // `None` until that happens, and it can happen at most once.
    let mut escaped_top: Option<f32> = None;
    // The bottom margin of the last in-flow block placed, as that child
    // collapsed it. Cleared by anything laid out after it, since that is
    // something between the margin and this box's bottom edge.
    let mut trailing_bottom: Option<f32> = None;

    for &child in doc.children(node) {
        let Some(child_style) = styles.get(child) else {
            // A text node has no style but is very much inline content.
            if doc.text(child).is_some() && !all_inline {
                pending.push(child);
            }
            continue;
        };
        // Table-internal boxes are positioned by their table, not by block
        // flow. A stray one outside a table falls through to block layout so
        // its content is still shown.
        //
        // An inline replaced element is *not* a block child: it was already
        // placed on a line by `collect_inline_runs`. Only one given a
        // block-level `display` reaches block flow.
        let replaced = is_replaced(doc, child) && !child_style.display.is_inline();
        let inline = is_inline_child(doc, styles, child, child_style);
        if child_style.position.is_out_of_flow() {
            // Laid out after the in-flow content, when this block's height —
            // and so its containing-block size — is finally known.
            absolutes.push((child, child_style.clone(), cursor_y));
            continue;
        }
        if child_style.display == Display::None
            || child_style.display.is_table_internal()
            // Floated children were placed above, out of the normal flow.
            || child_style.float != Float::None
        {
            continue;
        }
        if inline && !replaced {
            if !all_inline {
                pending.push(child);
            }
            continue;
        }

        // A block child ends the run of inline content before it. Content
        // between two blocks stops their margins touching, so it also ends the
        // run of collapsing.
        let flushed = flush_inline(
            doc,
            styles,
            fonts,
            &mut pending,
            node,
            style,
            intrinsic,
            (padding_left + border_left, cursor_y),
            content_width,
            &context,
            padding_top + border_top,
            &mut box_,
        );
        cursor_y += flushed;
        if flushed > 0.0 {
            previous_bottom = None;
        }
        // Place any float declared before this child, at the height reached so
        // far rather than at the top of the container.
        while let Some((float_node, float_style)) = late.first().cloned() {
            if !precedes(doc, node, float_node, child) {
                break;
            }
            late.remove(0);
            place_float(
                doc,
                styles,
                fonts,
                float_node,
                &float_style,
                intrinsic,
                content_width,
                cursor_y - padding_top - border_top,
                (padding_left + border_left, padding_top + border_top),
                &mut context,
                &mut box_,
            );
        }

        // `clear` pushes this box below the floats it names.
        //
        // Asked in the *context's* coordinates and converted back, which is the
        // conversion every other use of `cursor_y` here already does — the two
        // lines below it, and the float placement above. `context` was
        // translated into this block's content box, and `cursor_y` counts from
        // its border box, so asking directly cleared to a point too high by
        // exactly this block's top padding and border. The symptom was a
        // paragraph whose *box* sat below the floats correctly while its first
        // line dodged sideways as though one were still in the way, on any
        // container with padding — which is most of them.
        // Collapse this child's top margin into the previous sibling's bottom
        // one. `layout_block` places a box at `y + margin_top` and returns
        // `margin_top + height + margin_bottom`, so left alone the two margins
        // simply add — which is what the CSS 2.1 suite caught: two paragraphs
        // 40px apart were getting 80.
        //
        // Taken off the cursor rather than passed down, so the child still
        // computes its own position from its own margin and nothing else needs
        // to know this happened.
        let child_margins = (
            child_style
                .margin
                .top
                .to_px(child_style.font_size, content_width),
            child_style
                .margin
                .bottom
                .to_px(child_style.font_size, content_width),
        );
        // Where the cursor sat before this child's margin was collapsed into
        // the run, kept because a box a margin collapses *through* has to leave
        // it exactly as it found it.
        let before_child = cursor_y;
        if let Some(previous) = previous_bottom {
            cursor_y -= previous + child_margins.0 - collapse(previous, child_margins.0);
        }

        let into_context = padding_top + border_top;
        // Clearance is applied *after* collapsing, and pushes down from
        // wherever collapsing left the cursor. A box that clears is separated
        // from the floats above it by construction, so it cannot end up higher
        // than it would have without the collapse.
        cursor_y = context.clearance(child_style.clear, cursor_y - into_context) + into_context;
        let child_context = context.translated(0.0, cursor_y - into_context, content_width);
        let child_containing = containing.descend(padding_left + border_left, cursor_y);
        let consumed = layout_block(
            doc,
            styles,
            fonts,
            child,
            child_style,
            intrinsic,
            padding_left + border_left,
            cursor_y,
            content_width,
            child_context,
            child_containing,
            &mut box_,
        );
        // §8.3.1's second rule: with nothing between them — no top border, no
        // top padding, and nothing already laid out above — a first child's top
        // margin is adjoining its parent's, and the two collapse into one
        // *outside* the parent. So the child does not get that margin inside
        // the box; the box gets it, and moves down by it.
        //
        // Recognised by the cursor still sitting exactly where it started: any
        // inline content, any earlier block, or any float would have moved it,
        // and each of those is something between the two margins.
        let adjoining = escaped_top.is_none()
            && padding_top == 0.0
            && border_top == 0.0
            && cursor_y == padding_top + border_top
            && !keeps_its_childrens_margins(style);

        // §8.3.1 again, and the case that was missing: a box with nothing in it
        // and nothing separating its edges does not hold its two margins apart.
        // They are adjoining, so they collapse with each other *and* with the
        // run this box sits in, and the box itself occupies nothing. Without
        // this, every empty wrapper on a page added a margin — which is what
        // made the document fallback mostly blank space.
        //
        // Only where something precedes it. A leading one is the parent's own
        // top margin escaping, which `adjoining` above already handles, and
        // reaching into that from here would be two rules fighting over the
        // same box.
        if consumed.collapses_through && previous_bottom.is_some() {
            let previous = previous_bottom.unwrap_or_default();
            let through = collapse(consumed.margin_top, consumed.margin_bottom);
            let run = collapse(previous, through);
            // The cursor is put back exactly where it was and then moved by the
            // difference the run makes, so that it still ends with the pending
            // margin included — which is the invariant the next sibling's
            // collapsing depends on.
            cursor_y = before_child - previous + run;
            previous_bottom = Some(run);
            trailing_bottom = Some(run);
            continue;
        }

        if adjoining && consumed.margin_top != 0.0 {
            // Pull the child, and everything laid out within it, back up by the
            // margin it is giving away.
            if let Some(placed) = box_.children.last_mut() {
                placed.rect.y -= consumed.margin_top;
            }
            escaped_top = Some(consumed.margin_top);
            cursor_y += consumed.height + consumed.margin_bottom;
        } else {
            cursor_y += consumed.outer();
        }
        previous_bottom = Some(child_margins.1);
        trailing_bottom = Some(consumed.margin_bottom);
    }

    // Trailing inline content, after the last block child.
    let trailing = flush_inline(
        doc,
        styles,
        fonts,
        &mut pending,
        node,
        style,
        intrinsic,
        (padding_left + border_left, cursor_y),
        content_width,
        &context,
        padding_top + border_top,
        &mut box_,
    );
    cursor_y += trailing;
    if trailing > 0.0 {
        // Text after the last block child stands between that child's bottom
        // margin and this box's bottom edge, so they are no longer adjoining
        // and nothing escapes.
        trailing_bottom = None;
    }

    // A block must be tall enough to contain its own floats, or the next
    // block would start beside one and overlap it.
    // §8.3.1's third rule, the mirror of the first: with no bottom padding, no
    // bottom border and no height of its own, a box's bottom edge adjoins its
    // last child's bottom margin, and the two collapse into one outside the
    // box. A declared height separates them — the box ends where it was told
    // to, whatever its last child wanted.
    //
    // Also skipped where floats reach lower than the content, since then the
    // bottom edge is decided by a float rather than by that margin.
    let escaped_bottom = match trailing_bottom {
        Some(child_bottom)
            if padding_bottom == 0.0
                && border_bottom == 0.0
                && style.height == Length::Auto
                && !keeps_its_childrens_margins(style)
                && cursor_y >= padding_top + border_top + context.lowest_edge() =>
        {
            cursor_y -= child_bottom;
            Some(child_bottom)
        }
        _ => None,
    };

    let content_end = cursor_y.max(padding_top + border_top + context.lowest_edge())
        + padding_bottom
        + border_bottom;
    box_.rect.height = match style.height {
        Length::Auto => content_end,
        length => {
            length.to_px(font_size, available_width)
                + padding_top
                + padding_bottom
                + border_top
                + border_bottom
        }
    };

    // Absolutely positioned children, now that this block's size is known.
    // A positioned box becomes the containing block for its own descendants;
    // otherwise the one inherited from an ancestor still applies.
    let own_size = (
        content_width,
        (box_.rect.height - padding_top - padding_bottom - border_top - border_bottom).max(0.0),
    );
    for (child, child_style, static_y) in absolutes {
        let child_containing = if style.position.is_positioned() {
            ContainingBlock::establish(own_size)
        } else {
            containing.descend(padding_left + border_left, padding_top + border_top)
        };

        let mut probe = LayoutBox {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            style: child_style.clone(),
            text: None,
            content_origin: (0.0, 0.0),
            content_width: 0.0,
            children: Vec::new(),
            replaced: None,
            node: None,
        };
        // An absolutely positioned box with `width: auto` shrinks to fit its
        // content rather than filling its containing block — the difference
        // between a tooltip-sized box and a full-width band. Measured from the
        // box's own inline content; nested block children are not accounted
        // for, which would need a full intrinsic-width pass over the subtree.
        let available = child_containing.size.0;
        let width_basis = match child_style.width {
            Length::Auto => {
                let runs =
                    collect_inline_runs(doc, styles, child, &child_style, intrinsic, content_width);
                let (min, max) = fonts.intrinsic_widths(&runs, &child_style);
                let surround = child_style
                    .padding
                    .left
                    .to_px(child_style.font_size, available)
                    + child_style
                        .padding
                        .right
                        .to_px(child_style.font_size, available)
                    + child_style.border.left.used_width(child_style.font_size)
                    + child_style.border.right.used_width(child_style.font_size);
                if max <= 0.0 {
                    available
                } else {
                    (max + surround)
                        .min(available)
                        .max((min + surround).min(available))
                }
            }
            _ => available,
        };
        layout_block(
            doc,
            styles,
            fonts,
            child,
            &child_style,
            intrinsic,
            0.0,
            0.0,
            width_basis,
            FloatContext::new(width_basis),
            ContainingBlock::viewport(width_basis, child_containing.size.1),
            &mut probe,
        );
        let Some(mut child_box) = probe.children.pop() else {
            continue;
        };

        let size = (child_box.rect.width, child_box.rect.height);
        let (cb_x, cb_y) = absolute_offset(
            &child_style,
            child_containing.size,
            size,
            // With no offsets given the box stays where flow would have put it.
            (
                child_containing.offset.0 + padding_left + border_left,
                child_containing.offset.1 + static_y,
            ),
        );
        // Convert from containing-block coordinates to this box's own.
        child_box.rect.x = cb_x - child_containing.offset.0;
        child_box.rect.y = cb_y - child_containing.offset.1;
        box_.children.push(child_box);
    }

    // `position: relative` shifts the box after everything around it has been
    // placed, so siblings keep the space it would have occupied.
    if style.position == Position::Relative {
        let (dx, dy) = relative_shift(style, (available_width, available_width));
        box_.rect.x += dx;
        box_.rect.y += dy;
    }

    // Computed here rather than taken from `outer_height`, which resolves
    // percentages against a basis of zero — harmless while the answer was only
    // ever summed, wrong the moment the two ends are told apart.
    // Whatever escaped from the first child is this box's margin now. The box
    // was positioned with its own margin long before that was known, so it
    // moves by the difference rather than being placed again.
    let collapsed_top = match escaped_top {
        Some(escaped) => collapse(margin_top, escaped),
        None => margin_top,
    };
    box_.rect.y += collapsed_top - margin_top;

    let own_bottom = style.margin.bottom.to_px(font_size, available_width);
    let consumed = Consumed {
        height: box_.rect.height,
        margin_top: collapsed_top,
        margin_bottom: match escaped_bottom {
            Some(escaped) => collapse(own_bottom, escaped),
            None => own_bottom,
        },
        // Nothing stands between this box's two edges: a zero border-box
        // height already means no content, no border and no padding, since any
        // of those would have given it height. What is left to rule out is a
        // height it was told to have, and a formatting context of its own —
        // a float or an `overflow` container keeps its margins to itself.
        collapses_through: box_.rect.height == 0.0
            && matches!(style.height, Length::Auto | Length::Px(0.0))
            && !keeps_its_childrens_margins(style),
    };
    parent.children.push(box_);
    consumed
}

/// Lays out a table's rows and cells, appending them to `parent`.
///
/// Returns the height consumed. Column widths come from cell content
/// (`table::distribute_widths`); each cell is then laid out as an ordinary
/// block at its column's width, and a row is as tall as its tallest cell.
#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn layout_table(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    node: NodeId,
    style: &ComputedStyle,
    intrinsic: &IntrinsicSizes,
    x: f32,
    y: f32,
    available_width: f32,
    parent: &mut LayoutBox,
) -> (f32, f32) {
    let grid = table::build_grid(doc, styles, node);
    if grid.columns == 0 {
        return (0.0, 0.0);
    }
    // `border-spacing` is the table's own, not a constant: `cellspacing="0"`
    // is how a table used for page layout closed the seams between its cells.
    let spacing = style
        .border_spacing
        .to_px(style.font_size, available_width)
        .max(0.0);

    // Intrinsic widths per column, from the cells that span exactly one.
    let mut mins = vec![0.0f32; grid.columns];
    let mut maxes = vec![0.0f32; grid.columns];
    let mut declared = vec![false; grid.columns];
    let mut spans: Vec<(usize, usize, f32, f32)> = Vec::new();

    for row in &grid.rows {
        for cell in &row.cells {
            // The whole subtree, not just the cell's text: a cell holding a
            // nested table measures as nothing otherwise, and its column
            // collapses to zero width.
            let (min, max) = subtree_widths(
                doc,
                styles,
                fonts,
                cell.node,
                &cell.style,
                intrinsic,
                available_width,
                0,
            );

            if cell.colspan == 1 {
                if let (Some(column_min), Some(column_max)) =
                    (mins.get_mut(cell.column), maxes.get_mut(cell.column))
                {
                    *column_min = column_min.max(min);
                    *column_max = column_max.max(max);
                }
                // A column whose cell declared a width has asked for exactly
                // that, and must not be stretched when the table is widened.
                if matches!(cell.style.width, Length::Px(_) | Length::Percent(_))
                    && let Some(fixed) = declared.get_mut(cell.column)
                {
                    *fixed = true;
                }
            } else {
                // Spanning cells are applied after the single-column cells have
                // set a baseline, so they only ever widen columns.
                spans.push((cell.column, cell.colspan, min, max));
            }
        }
    }
    for (column, colspan, min, max) in spans {
        table::apply_span(&mut mins, column, colspan, min, spacing);
        table::apply_span(&mut maxes, column, colspan, max, spacing);
    }

    let spacing_total = spacing * (grid.columns + 1) as f32;
    let usable = (available_width - spacing_total).max(0.0);
    let mut widths = table::distribute_widths(&mins, &maxes, Some(usable));

    // A table with no declared width shrinks to fit its content. One with a
    // declared width fills it, which is exactly what `<table width="100%">`
    // meant on the era's pages and why so many of them used it.
    if style.width != Length::Auto {
        let total: f32 = widths.iter().sum();
        if total > 0.0 && usable > total {
            let surplus = usable - total;
            // The surplus goes to the columns that did not ask for a width. A
            // `<td width="150">` beside a flexible column means a 150-pixel
            // sidebar and a content column that takes the rest — stretching
            // both in proportion gives a sidebar that grows with the window,
            // which is the opposite of what the markup asked for.
            let flexible: f32 = widths
                .iter()
                .zip(&declared)
                .filter(|(_, fixed)| !**fixed)
                .map(|(width, _)| *width)
                .sum();
            let flexible_count = declared.iter().filter(|fixed| !**fixed).count();

            if flexible_count > 0 {
                for (width, fixed) in widths.iter_mut().zip(&declared) {
                    if *fixed {
                        continue;
                    }
                    *width += if flexible > 0.0 {
                        surplus * *width / flexible
                    } else {
                        // Every flexible column is empty; share evenly rather
                        // than leaving them all at zero.
                        surplus / flexible_count as f32
                    };
                }
            } else {
                // Every column declared a width and they do not add up. Scale
                // them together rather than leaving the table short.
                let scale = usable / total;
                for width in &mut widths {
                    *width *= scale;
                }
            }
        }
    }

    // Lay every cell out first, then decide row heights, then place them.
    // A cell spanning rows cannot be positioned until the heights of all the
    // rows it covers are known, and those heights depend on the other cells.
    struct Placed {
        box_: LayoutBox,
        row: usize,
        rowspan: usize,
        height: f32,
    }

    let mut placed: Vec<Placed> = Vec::new();
    for (index, row) in grid.rows.iter().enumerate() {
        for cell in &row.cells {
            let end = (cell.column + cell.colspan).min(widths.len());
            if cell.column >= end {
                continue;
            }
            let width: f32 = widths[cell.column..end].iter().sum::<f32>()
                + spacing * (end - cell.column - 1) as f32;
            let cell_x = x
                + spacing
                + widths[..cell.column].iter().sum::<f32>()
                + spacing * cell.column as f32;

            // Each cell is an ordinary block in a box of its column's width.
            let mut holder = LayoutBox {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width,
                    height: 0.0,
                },
                style: cell.style.clone(),
                text: None,
                content_origin: (0.0, 0.0),
                content_width: width,
                children: Vec::new(),
                replaced: None,
                node: None,
            };
            // A cell establishes its own formatting context, so floats outside
            // the table do not reach into it.
            let consumed = layout_block(
                doc,
                styles,
                fonts,
                cell.node,
                &cell.style,
                intrinsic,
                cell_x,
                // Placed on the second pass; only the height matters here.
                0.0,
                width,
                FloatContext::new(width),
                ContainingBlock::viewport(width, width),
                &mut holder,
            );
            if let Some(box_) = holder.children.pop() {
                placed.push(Placed {
                    box_,
                    row: index,
                    rowspan: cell.rowspan,
                    height: consumed.outer(),
                });
            }
        }
    }

    // A row is as tall as the tallest cell that ends in it. Cells spanning
    // several rows are applied afterwards, so they can only grow a row.
    let mut heights = vec![0.0f32; grid.rows.len()];
    for cell in &placed {
        if cell.rowspan == 1
            && let Some(height) = heights.get_mut(cell.row)
        {
            *height = height.max(cell.height);
        }
    }
    for cell in &placed {
        if cell.rowspan == 1 {
            continue;
        }
        let end = (cell.row + cell.rowspan).min(heights.len());
        if cell.row >= end {
            continue;
        }
        let covered: f32 =
            heights[cell.row..end].iter().sum::<f32>() + spacing * (end - cell.row - 1) as f32;
        if covered < cell.height {
            // The shortfall goes on the last row it covers. Spreading it evenly
            // would push apart rows whose own content already fits, which reads
            // as the table having gaps in it.
            heights[end - 1] += cell.height - covered;
        }
    }

    let mut tops = Vec::with_capacity(heights.len());
    let mut cursor_y = y + spacing;
    for height in &heights {
        tops.push(cursor_y);
        cursor_y += height + spacing;
    }

    // Row boxes first, so their backgrounds paint behind the cells.
    let row_width: f32 =
        widths.iter().sum::<f32>() + spacing * (widths.len().saturating_sub(1)) as f32;
    for (index, row) in grid.rows.iter().enumerate() {
        parent.children.push(LayoutBox {
            rect: Rect {
                x: x + spacing,
                y: tops[index],
                width: row_width,
                height: heights[index],
            },
            style: row.style.clone(),
            text: None,
            content_origin: (0.0, 0.0),
            content_width: row_width,
            children: Vec::new(),
            replaced: None,
            node: Some(row.node),
        });
    }

    // Cells stretch to fill every row they cover, so backgrounds and borders
    // line up and a spanning cell reaches the bottom of its last row.
    for mut cell in placed {
        let end = (cell.row + cell.rowspan).min(heights.len());
        let spanned: f32 =
            heights[cell.row..end].iter().sum::<f32>() + spacing * (end - cell.row - 1) as f32;
        cell.box_.rect.y = tops[cell.row];
        let stretched = cell.box_.rect.height.max(spanned);

        // The cell's content is aligned within the space the row gave it.
        // `middle` is the default, which is why a short column looks centred
        // against a long one until `valign="top"` says otherwise.
        let slack = (stretched - cell.height).max(0.0);
        let shift = match cell.box_.style.vertical_align {
            VerticalAlign::Top | VerticalAlign::Baseline => 0.0,
            VerticalAlign::Middle => slack / 2.0,
            VerticalAlign::Bottom => slack,
        };
        if shift > 0.0 {
            cell.box_.content_origin.1 += shift;
            for child in &mut cell.box_.children {
                child.rect.y += shift;
            }
        }

        cell.box_.rect.height = stretched;
        parent.children.push(cell.box_);
    }

    // The table's own width: its columns, the gaps between them, and the gap
    // outside the first and last.
    let width = widths.iter().sum::<f32>() + spacing * (grid.columns + 1) as f32;
    (width, cursor_y - y)
}

/// Lays out a floated child and places it in `context`.
///
/// `y` is where the float may first sit, in the container's content
/// coordinates; `origin` is the container's content-box offset within its own
/// border box, needed to position the resulting box.
#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn place_float(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    child: NodeId,
    child_style: &ComputedStyle,
    intrinsic: &IntrinsicSizes,
    content_width: f32,
    y: f32,
    origin: (f32, f32),
    context: &mut FloatContext,
    parent: &mut LayoutBox,
) {
    let surround = child_style
        .padding
        .left
        .to_px(child_style.font_size, content_width)
        + child_style
            .padding
            .right
            .to_px(child_style.font_size, content_width)
        + child_style.border.left.used_width(child_style.font_size)
        + child_style.border.right.used_width(child_style.font_size);

    // A float shrinks to fit its content unless a width is declared. Measured
    // over the whole subtree: a float whose content is an image or a nested
    // table has no text to measure, and a zero-width float reserves no room —
    // the text beside it then runs straight over it.
    let float_width = match child_style.width {
        Length::Auto => {
            let (_, natural) = subtree_widths(
                doc,
                styles,
                fonts,
                child,
                child_style,
                intrinsic,
                content_width,
                0,
            );
            natural.min(content_width)
        }
        length => length.to_px(child_style.font_size, content_width) + surround,
    };

    let mut probe = LayoutBox {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: float_width,
            height: 0.0,
        },
        style: child_style.clone(),
        text: None,
        content_origin: (0.0, 0.0),
        content_width: float_width,
        children: Vec::new(),
        replaced: None,
        node: None,
    };
    let float_height = layout_block(
        doc,
        styles,
        fonts,
        child,
        child_style,
        intrinsic,
        0.0,
        0.0,
        float_width,
        FloatContext::new(float_width),
        ContainingBlock::viewport(float_width, float_width),
        &mut probe,
    );
    // The space a float reserves is its *margin* box, not its border box: an
    // image with `hspace="8"` is asking for text to keep eight pixels away,
    // and reserving only the border box lets the text touch it.
    let font_size = child_style.font_size;
    let margin_x = child_style.margin.left.to_px(font_size, content_width)
        + child_style.margin.right.to_px(font_size, content_width);
    let margin_y = child_style.margin.top.to_px(font_size, content_width)
        + child_style.margin.bottom.to_px(font_size, content_width);
    let (left, top) = context.place(
        child_style.float,
        float_width + margin_x,
        float_height.outer() + margin_y,
        y,
    );

    if let Some(mut float_box) = probe.children.pop() {
        // Offsetting rather than assigning keeps the float's own margins,
        // which `layout_block` already folded into its rect.
        float_box.rect.x += origin.0 + left;
        float_box.rect.y += origin.1 + top;
        parent.children.push(float_box);
    }
}

/// Whether `first` comes before `second` among `parent`'s children.
fn precedes(doc: &Document, parent: NodeId, first: NodeId, second: NodeId) -> bool {
    let children = doc.children(parent);
    let index = |target: NodeId| children.iter().position(|&c| c == target);
    match (index(first), index(second)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

/// Collects a block's inline content as styled runs, stopping at block children
/// so their text is not counted twice.
///
/// Each inline element contributes its own run carrying its own computed style,
/// which is what lets `<b>` and `<code>` inside a paragraph render differently
/// from the text around them.
fn collect_inline_runs(
    doc: &Document,
    styles: &StyleMap,
    node: NodeId,
    inherited: &ComputedStyle,
    intrinsic: &IntrinsicSizes,
    available_width: f32,
) -> Vec<InlineRun> {
    inline_runs_for(
        doc,
        styles,
        doc.children(node),
        inherited,
        node,
        intrinsic,
        available_width,
    )
}

/// Collects inline runs from a specific list of siblings.
///
/// Taking a slice rather than a parent is what lets a block container with
/// mixed content lay out each stretch of inline children where it actually
/// sits, instead of hoisting every one of them above the block children.
fn inline_runs_for(
    doc: &Document,
    styles: &StyleMap,
    children: &[NodeId],
    inherited: &ComputedStyle,
    holder: NodeId,
    intrinsic: &IntrinsicSizes,
    available_width: f32,
) -> Vec<InlineRun> {
    let mut runs = Vec::new();
    for &child in children {
        gather_one(
            doc,
            styles,
            child,
            inherited,
            holder,
            intrinsic,
            available_width,
            &mut runs,
        );
    }

    // Whitespace collapsing spans run boundaries: `<b>bold</b> <i>italic</i>`
    // must not lose the space between the runs, and `a <b> b</b>` must not keep
    // two. Collapsing each run in isolation would get both wrong, so the runs
    // are collapsed as one stream with the boundary state carried across.
    let mut previous_ended_in_space = true;
    for run in &mut runs {
        if run.style.white_space == WhiteSpace::Pre {
            previous_ended_in_space = run.text.ends_with(char::is_whitespace);
            continue;
        }
        let collapsed = collapse_whitespace_from(&run.text, previous_ended_in_space);
        previous_ended_in_space = collapsed.ends_with(' ');
        run.text = collapsed;
    }
    // Leading and trailing whitespace of the whole block is dropped.
    if let Some(first) = runs.first_mut() {
        first.text = first.text.trim_start().to_owned();
    }
    if let Some(last) = runs.last_mut() {
        last.text = last.text.trim_end().to_owned();
    }
    runs
}

#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn gather_one(
    doc: &Document,
    styles: &StyleMap,
    child: NodeId,
    inherited: &ComputedStyle,
    holder: NodeId,
    intrinsic: &IntrinsicSizes,
    available_width: f32,
    out: &mut Vec<InlineRun>,
) {
    if let Some(text) = doc.text(child) {
        // Text belongs to the nearest element that wraps it, not to the text
        // node: `<a>go</a>` must hit the anchor, which is what has the href.
        out.push(InlineRun::text(text, inherited.clone()).from_element(holder.0));
        return;
    }
    let Some(style) = styles.get(child) else {
        return;
    };
    if style.display == Display::None || !is_inline_child(doc, styles, child, style) {
        return;
    }

    // `<br>` is a forced break, not an element with content. Emitted as a
    // newline the segmenter must honour, which means marking the run
    // preformatted so collapsing does not turn it back into a space.
    if doc
        .element(child)
        .is_some_and(|element| element.local_name() == "br")
    {
        out.push(
            InlineRun::text(
                "\n",
                ComputedStyle {
                    white_space: WhiteSpace::Pre,
                    ..style.clone()
                },
            )
            .from_element(child.0),
        );
        return;
    }

    // An inline replaced element sits *on* the line rather than interrupting
    // it: an icon beside a link, a spacer between words. It becomes an atomic
    // box the line breaker can place, keyed by node so paint can find the
    // decoded image again.
    if is_replaced(doc, child) {
        let (width, height) = replaced_size(
            style,
            intrinsic.get(&child).copied(),
            size_attr(doc, child, "width"),
            size_attr(doc, child, "height"),
            available_width,
        );
        // CSS `width` is the content width, so the box the line has to make
        // room for is that plus the border and padding around it. Leaving them
        // out lets a bordered image overlap the text beside it.
        let font_size = style.font_size;
        let horizontal = style.border.left.used_width(font_size)
            + style.border.right.used_width(font_size)
            + style.padding.left.to_px(font_size, available_width)
            + style.padding.right.to_px(font_size, available_width);
        let vertical = style.border.top.used_width(font_size)
            + style.border.bottom.used_width(font_size)
            + style.padding.top.to_px(font_size, available_width)
            + style.padding.bottom.to_px(font_size, available_width);
        out.push(InlineRun::replaced(
            text::ReplacedInline {
                id: child.0,
                width: width + horizontal,
                height: height + vertical,
            },
            style.clone(),
        ));
        return;
    }

    for &grandchild in doc.children(child) {
        gather_one(
            doc,
            styles,
            grandchild,
            style,
            child,
            intrinsic,
            available_width,
            out,
        );
    }
}

/// Collapses whitespace per `white-space: normal`.
///
/// Every run of spaces, tabs, and newlines becomes a single space. Without
/// this, source indentation and line breaks reach the shaper verbatim and every
/// wrapped line inherits the author's leading whitespace — visible as a ragged
/// indent on continuation lines.
pub fn collapse_whitespace(text: &str) -> String {
    collapse_whitespace_from(text, true)
}

/// Collapses whitespace, treating the preceding text as already ending in a
/// space when `after_space` is set.
///
/// The carried state is what makes collapsing correct across inline run
/// boundaries: without it, the space between `</b>` and `<i>` either vanishes
/// or doubles depending on which run it landed in.
fn collapse_whitespace_from(text: &str, after_space: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_whitespace = after_space;
    for c in text.chars() {
        if c.is_whitespace() {
            if !in_whitespace {
                out.push(' ');
            }
            in_whitespace = true;
        } else {
            out.push(c);
            in_whitespace = false;
        }
    }
    out
}

/// Horizontal offset for a line, given the alignment of its block.
pub fn line_offset(align: TextAlign, line_width: f32, content_width: f32) -> f32 {
    match align {
        TextAlign::Left | TextAlign::Justify => 0.0,
        TextAlign::Center | TextAlign::CenterBlocks => {
            ((content_width - line_width) / 2.0).max(0.0)
        }
        TextAlign::Right => (content_width - line_width).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use css::Stylesheet;

    struct Rendered {
        layout: Layout,
    }

    fn run(html: &str, css_text: &str, width: f32) -> Rendered {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css_text)];
        let styles = css::cascade::cascade(&doc, &sheets);
        let mut fonts = FontStore::new();
        Rendered {
            layout: layout(&doc, &styles, &mut fonts, &IntrinsicSizes::new(), width),
        }
    }

    /// Depth-first list of every box below `root`.
    fn boxes(root: &LayoutBox) -> Vec<&LayoutBox> {
        let mut out = Vec::new();
        let mut stack: Vec<&LayoutBox> = root.children.iter().rev().collect();
        while let Some(b) = stack.pop() {
            out.push(b);
            stack.extend(b.children.iter().rev());
        }
        out
    }

    /// Boxes inside `<body>`, which is what the tests actually care about.
    ///
    /// The layout root is the canvas and its sole child is the body box, so
    /// indexing the root directly returns the body and silently shifts every
    /// expectation by one level.
    fn content_boxes(rendered: &Rendered) -> Vec<&LayoutBox> {
        let body = rendered.layout.root.children.first().expect("body box");
        boxes(body)
    }

    #[test]
    fn blocks_stack_vertically_without_overlapping() {
        let rendered = run("<body><p>one</p><p>two</p><p>three</p></body>", "", 800.0);
        let all = content_boxes(&rendered);
        let paragraphs: Vec<_> = all.iter().filter(|b| b.text.is_some()).collect();
        assert_eq!(paragraphs.len(), 3);
        for pair in paragraphs.windows(2) {
            assert!(
                pair[1].rect.y >= pair[0].rect.y + pair[0].rect.height,
                "boxes overlap: {:?} then {:?}",
                pair[0].rect,
                pair[1].rect
            );
        }
    }

    #[test]
    fn a_block_fills_the_width_it_is_given() {
        let rendered = run("<body><div>x</div></body>", "body { margin: 0 }", 500.0);
        assert_eq!(content_boxes(&rendered)[0].rect.width, 500.0);
    }

    #[test]
    fn margins_offset_a_block() {
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { margin: 10px 20px }",
            500.0,
        );
        let div = content_boxes(&rendered)[0];
        assert_eq!(div.rect.x, 20.0);
        assert_eq!(
            div.rect.width, 460.0,
            "width shrinks by both horizontal margins"
        );

        // The vertical margin is no longer *inside* the body. `body` has no top
        // padding or border, so its first child's top margin collapses out of
        // it (§8.3.1) — the div sits at the body's content edge and the body
        // moved down instead. This assertion used to read `div.rect.y == 10`
        // and was measuring which box holds the margin rather than where the
        // div ends up.
        assert_eq!(div.rect.y, 0.0, "the top margin escaped to the body");

        // What a reader sees is unchanged, which is the part worth asserting:
        // the margin still separates, it is just owned by the other box now.
        let without = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { margin: 0 20px }",
            500.0,
        );
        assert_eq!(
            rendered.layout.height - without.layout.height,
            20.0,
            "10px above and below still occupy the page",
        );
    }

    #[test]
    fn a_first_childs_top_margin_escapes_a_parent_with_no_border_or_padding() {
        // §8.3.1's second rule. Visible in the *parent's* box rather than the
        // child's position: the child does not move, the container shrinks to
        // fit it exactly and moves down by the margin it took on.
        let bare = run(
            "<body><div class=\"box\"><p></p></div></body>",
            "body { margin: 0 } .box { background: #eee } \
             p { margin: 30px 0; height: 20px }",
            200.0,
        );
        let separated = run(
            "<body><div class=\"box\"><p></p></div></body>",
            "body { margin: 0 } .box { background: #eee; border-top: 1px solid #000 } \
             p { margin: 30px 0; height: 20px }",
            200.0,
        );
        let container = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .find(|b| !b.style.background_color.is_transparent())
                .map(|b| b.rect.height)
                .expect("the container box")
        };
        assert_eq!(
            container(&bare),
            20.0,
            "with nothing between them the margins escape and the box wraps the child",
        );
        // A single pixel of border is enough to stop it, which is the rule
        // being tested rather than a coincidence of this markup.
        assert!(
            container(&separated) > 20.0,
            "a top border separates the margins: {}",
            container(&separated),
        );
    }

    #[test]
    fn padding_grows_a_box_and_insets_its_content() {
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { padding: 12px }",
            400.0,
        );
        let div = content_boxes(&rendered)[0];
        assert_eq!(div.content_origin, (12.0, 12.0));
        assert!(div.rect.height >= 24.0, "height includes both paddings");
    }

    #[test]
    fn narrow_columns_wrap_text_and_grow_taller() {
        let html = "<body><p>the quick brown fox jumps over the lazy dog repeatedly</p></body>";
        let wide = run(html, "", 900.0);
        let narrow = run(html, "", 150.0);
        assert!(
            narrow.layout.height > wide.layout.height,
            "narrow {} should exceed wide {}",
            narrow.layout.height,
            wide.layout.height
        );
    }

    #[test]
    fn display_none_removes_a_box_entirely() {
        let both = run("<body><p>one</p><p>two</p></body>", "", 800.0);
        let one_hidden = run(
            r#"<body><p>one</p><p class="h">two</p></body>"#,
            ".h { display: none }",
            800.0,
        );
        assert_eq!(
            content_boxes(&one_hidden).len(),
            content_boxes(&both).len() - 1
        );
    }

    #[test]
    fn headings_are_taller_than_paragraphs() {
        let heading = run("<body><h1>Title</h1></body>", "", 800.0);
        let paragraph = run("<body><p>Title</p></body>", "", 800.0);
        assert!(
            heading.layout.height > paragraph.layout.height,
            "h1 at 2em should exceed a paragraph"
        );
    }

    #[test]
    fn borders_grow_the_box_and_inset_its_content() {
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { border: 5px solid black; padding: 10px }",
            400.0,
        );
        let div = content_boxes(&rendered)[0];
        assert_eq!(
            div.content_origin,
            (15.0, 15.0),
            "content sits inside border then padding"
        );
        assert_eq!(
            div.rect.width, 400.0,
            "an auto-width box still fills its container"
        );
        assert_eq!(
            div.content_width, 370.0,
            "content shrinks by both borders and paddings"
        );
    }

    #[test]
    fn an_explicit_width_is_the_content_width() {
        // CSS 2.1 is content-box: borders and padding grow the box outwards.
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { width: 100px; border: 2px solid black; padding: 8px }",
            400.0,
        );
        let div = content_boxes(&rendered)[0];
        assert_eq!(div.content_width, 100.0);
        assert_eq!(div.rect.width, 120.0, "100 + 2*8 padding + 2*2 border");
    }

    #[test]
    fn a_border_without_a_style_occupies_no_space() {
        let styled = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { border-width: 20px; border-style: solid }",
            400.0,
        );
        let unstyled = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { border-width: 20px }",
            400.0,
        );
        assert_eq!(content_boxes(&styled)[0].content_width, 360.0);
        assert_eq!(content_boxes(&unstyled)[0].content_width, 400.0);
    }

    #[test]
    fn nested_blocks_indent_by_their_parents_padding() {
        let rendered = run(
            "<body><div class=\"outer\"><div class=\"inner\">x</div></div></body>",
            "body { margin: 0 } .outer { padding-left: 30px }",
            600.0,
        );
        let inner = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.padding.left == Length::Px(0.0) && b.text.is_some())
            .expect("inner box");
        assert_eq!(inner.rect.x, 30.0);
        assert_eq!(inner.rect.width, 570.0);
    }

    #[test]
    fn source_whitespace_is_collapsed() {
        assert_eq!(collapse_whitespace("a\n    b\tc  d"), "a b c d");
        assert_eq!(collapse_whitespace("\n   leading"), "leading");
    }

    #[test]
    fn indented_source_does_not_indent_wrapped_lines() {
        // Pretty-printed HTML is the norm, so this is the common case, not an
        // edge case: without collapsing, every continuation line is indented by
        // the author's source indentation.
        let pretty = "<body><p>the quick brown fox\n        jumps over the lazy dog\n        and \
                      keeps running onward</p></body>";
        let flat = "<body><p>the quick brown fox jumps over the lazy dog and keeps running \
                    onward</p></body>";
        let a = run(pretty, "", 200.0);
        let b = run(flat, "", 200.0);
        assert_eq!(
            a.layout.height, b.layout.height,
            "indentation changed the layout"
        );
    }

    #[test]
    fn table_cells_are_laid_out_side_by_side() {
        let rendered = run(
            "<body><table><tr><td>one</td><td>two</td></tr></table></body>",
            "body { margin: 0 }",
            600.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert_eq!(cells.len(), 2);
        assert!(
            cells[1].rect.x > cells[0].rect.x,
            "second cell must sit to the right"
        );
        // Same row, so the same top edge.
        assert!((cells[0].rect.y - cells[1].rect.y).abs() < 0.01);
    }

    #[test]
    fn table_rows_stack_downwards() {
        let rendered = run(
            "<body><table><tr><td>one</td></tr><tr><td>two</td></tr></table></body>",
            "body { margin: 0 }",
            600.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert_eq!(cells.len(), 2);
        assert!(
            cells[1].rect.y > cells[0].rect.y,
            "second row must sit below"
        );
        assert!(
            (cells[0].rect.x - cells[1].rect.x).abs() < 0.01,
            "same column, same x"
        );
    }

    /// Boxes standing in for replaced elements, in document order.
    fn replaced_boxes(rendered: &Rendered) -> Vec<&LayoutBox> {
        content_boxes(rendered)
            .into_iter()
            .filter(|b| b.replaced.is_some())
            .collect()
    }

    #[test]
    fn an_inline_image_sits_on_the_line_rather_than_breaking_it() {
        let mut sizes = IntrinsicSizes::new();
        let html = r#"<body><p>before <img src="x.png"> after</p></body>"#;
        let doc = dom::parse(html);
        let image = doc.find_element("img").expect("img");
        sizes.insert(image, (20.0, 20.0));

        let styles = css::cascade::cascade(&doc, &[Stylesheet::parse("body { margin: 0 }")]);
        let mut fonts = FontStore::new();
        let rendered = Rendered {
            layout: layout(&doc, &styles, &mut fonts, &sizes, 600.0),
        };

        let all = content_boxes(&rendered);
        let paragraph = all
            .iter()
            .find(|b| b.text.is_some())
            .expect("the paragraph's text");
        assert_eq!(
            paragraph.text.as_ref().expect("text").lines.len(),
            1,
            "text and image share one line"
        );

        let placed = &paragraph.text.as_ref().expect("text").lines[0].replaced;
        assert_eq!(placed.len(), 1);
        assert_eq!(placed[0].id, image.0);
        assert!(placed[0].x > 0.0, "the image follows the text before it");
    }

    #[test]
    fn a_bordered_inline_image_reserves_room_for_its_border() {
        // The line has to make room for the border box, not the content box,
        // or a framed image overlaps the text beside it.
        let mut sizes = IntrinsicSizes::new();
        let html = r#"<body><p>x <img src="a.png"> y</p></body>"#;
        let doc = dom::parse(html);
        let image = doc.find_element("img").expect("img");
        sizes.insert(image, (20.0, 20.0));

        let mut fonts = FontStore::new();
        let widths: Vec<f32> = ["img { border: 0 }", "img { border: 5px solid red }"]
            .into_iter()
            .map(|css| {
                let styles = css::cascade::cascade(&doc, &[Stylesheet::parse(css)]);
                let rendered = Rendered {
                    layout: layout(&doc, &styles, &mut fonts, &sizes, 600.0),
                };
                content_boxes(&rendered)
                    .into_iter()
                    .find(|b| b.text.is_some())
                    .and_then(|b| b.text.as_ref())
                    .map(|text| text.lines[0].replaced[0].width)
                    .expect("a placed image")
            })
            .collect();
        assert_eq!(widths[1] - widths[0], 10.0, "5px of border on each side");
    }

    #[test]
    fn inline_content_stays_between_the_blocks_it_sits_between() {
        // CSS 2.1 §9.2.1.1. Without anonymous block boxes every scrap of
        // inline content is hoisted above every block one, so this renders as
        // "one two" followed by the paragraph.
        let rendered = run(
            "<body>one<p>middle</p>two</body>",
            "body { margin: 0 }",
            600.0,
        );
        let texts: Vec<&LayoutBox> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert_eq!(texts.len(), 3, "two anonymous boxes and the paragraph");
        for pair in texts.windows(2) {
            assert!(
                pair[1].rect.y >= pair[0].rect.y,
                "content must stay in source order: {:?} then {:?}",
                pair[0].rect,
                pair[1].rect
            );
        }
    }

    #[test]
    fn a_container_of_only_inline_content_needs_no_anonymous_box() {
        // The common case — every paragraph, every heading. Wrapping it would
        // add a box per block for nothing.
        let rendered = run("<body><p>just text</p></body>", "body { margin: 0 }", 600.0);
        let paragraph = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.text.is_some())
            .expect("the paragraph");
        assert!(
            paragraph.children.is_empty(),
            "text laid out on the paragraph itself"
        );
    }

    #[test]
    fn a_block_level_image_still_flows_as_a_block() {
        let mut sizes = IntrinsicSizes::new();
        let doc = dom::parse(r#"<body><img src="x.png"><p>after</p></body>"#);
        let image = doc.find_element("img").expect("img");
        sizes.insert(image, (40.0, 40.0));
        let styles = css::cascade::cascade(
            &doc,
            &[Stylesheet::parse(
                "body { margin: 0 } img { display: block }",
            )],
        );
        let mut fonts = FontStore::new();
        let rendered = Rendered {
            layout: layout(&doc, &styles, &mut fonts, &sizes, 600.0),
        };
        let boxes = replaced_boxes(&rendered);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].rect.height, 40.0);
    }

    #[test]
    fn every_list_item_gets_a_marker_box() {
        let rendered = run("<body><ul><li>one</li><li>two</li></ul></body>", "", 400.0);
        let markers = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.style.display == Display::ListItem)
            .filter(|item| item.children.iter().any(|child| child.text.is_some()))
            .count();
        assert_eq!(markers, 2);
    }

    #[test]
    fn a_marker_sits_left_of_its_item_and_inside_the_list() {
        let rendered = run("<body><ul><li>one</li></ul></body>", "", 400.0);
        let all = content_boxes(&rendered);
        let item = all
            .iter()
            .find(|b| b.style.display == Display::ListItem)
            .expect("a list item");
        let list = all
            .iter()
            .find(|b| b.style.padding.left != css::Length::Px(0.0))
            .expect("the list, which carries the indent");

        let marker = item.children.first().expect("marker box");
        assert!(
            marker.rect.x < item.content_origin.0,
            "marker at {} must sit left of the content edge at {}",
            marker.rect.x,
            item.content_origin.0
        );
        // The marker lives in the list's padding, not outside the list.
        let marker_x = item.rect.x + marker.rect.x;
        assert!(
            marker_x > list.rect.x,
            "marker at {marker_x} escaped the list starting at {}",
            list.rect.x
        );
    }

    #[test]
    fn a_list_with_no_marker_type_still_indents_but_draws_nothing() {
        let rendered = run(
            "<body><ul><li>one</li></ul></body>",
            "ul { list-style-type: none }",
            400.0,
        );
        let item = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.display == Display::ListItem)
            .expect("a list item");
        assert!(item.children.is_empty(), "no marker box for `none`");
    }

    #[test]
    fn item_values_and_list_starts_move_the_count() {
        let doc = dom::parse(
            r#"<body><ol start="5"><li>a</li><li value="9">b</li><li>c</li></ol></body>"#,
        );
        let styles = css::cascade::cascade(&doc, &[]);
        let items: Vec<NodeId> = doc
            .descendants(doc.root())
            .into_iter()
            .filter(|&node| {
                doc.element(node)
                    .is_some_and(|element| element.local_name() == "li")
            })
            .collect();
        let ordinals: Vec<usize> = items
            .iter()
            .map(|&node| list_ordinal(&doc, &styles, node))
            .collect();
        // `start` sets the first, `value` restarts mid-list, and the count
        // carries on from wherever it was last set.
        assert_eq!(ordinals, vec![5, 9, 10]);
    }

    #[test]
    fn a_row_gets_a_box_spanning_its_cells() {
        // Striped tables put the colour on `<tr>`, so the row needs a box of
        // its own: without one there is nothing for that background to paint
        // on and the stripes vanish.
        let rendered = run(
            "<body><table><tr><td>one</td><td>two</td></tr></table></body>",
            "body { margin: 0 } tr { background: #ff0000 }",
            600.0,
        );
        let all = content_boxes(&rendered);
        let cells: Vec<_> = all.iter().filter(|b| b.text.is_some()).collect();
        let row = all
            .iter()
            .find(|b| b.style.background_color == css::Color::rgb(255, 0, 0))
            .expect("a box carries the row background");

        let left = cells.iter().map(|c| c.rect.x).fold(f32::MAX, f32::min);
        let right = cells
            .iter()
            .map(|c| c.rect.x + c.rect.width)
            .fold(f32::MIN, f32::max);
        assert!(
            row.rect.x <= left && row.rect.x + row.rect.width >= right,
            "row {:?} must span its cells {left}..{right}",
            row.rect
        );
        assert!(row.rect.height > 0.0, "a row with cells has height");
    }

    #[test]
    fn auto_margins_centre_a_block_of_definite_width() {
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { width: 200px; margin-left: auto; margin-right: auto }",
            600.0,
        );
        assert_eq!(content_boxes(&rendered)[0].rect.x, 200.0);
    }

    #[test]
    fn one_auto_margin_pushes_a_block_to_the_other_side() {
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { width: 200px; margin-left: auto }",
            600.0,
        );
        assert_eq!(content_boxes(&rendered)[0].rect.x, 400.0);
    }

    #[test]
    fn auto_margins_do_nothing_without_a_width() {
        // There is no leftover space to share, so the box still fills.
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { margin-left: auto; margin-right: auto }",
            600.0,
        );
        let box_ = content_boxes(&rendered)[0];
        assert_eq!((box_.rect.x, box_.rect.width), (0.0, 600.0));
    }

    #[test]
    fn center_moves_its_block_children_and_text_align_center_does_not() {
        // `<center><table></center>` was the commonest way to centre a table,
        // and plain `text-align: center` does not move a table at all. Sharing
        // one value between them either stops `<center>` working or starts
        // moving boxes for stylesheets that only asked for centred text.
        let table_x = |html: &str, css: &str| {
            let rendered = run(html, css, 600.0);
            content_boxes(&rendered)
                .into_iter()
                .find(|b| b.style.display == Display::Table)
                .expect("a table")
                .rect
                .x
        };

        let centred = table_x(
            r#"<body><center><table width="300"><tr><td>x</td></tr></table></center></body>"#,
            "body { margin: 0 }",
        );
        assert!(centred > 100.0, "the table sits at {centred}");

        let not_centred = table_x(
            r#"<body><div><table width="300"><tr><td>x</td></tr></table></div></body>"#,
            "body { margin: 0 } div { text-align: center }",
        );
        assert_eq!(not_centred, 0.0, "a stylesheet must not move the table");
    }

    #[test]
    fn a_shrink_to_fit_table_is_centred_too() {
        // Its real width is only known after its columns are sized.
        let rendered = run(
            "<body><center><table><tr><td>narrow</td></tr></table></center></body>",
            "body { margin: 0 }",
            600.0,
        );
        let table = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.display == Display::Table)
            .expect("a table");
        assert!(
            table.rect.x > 100.0,
            "table at {} is {} wide",
            table.rect.x,
            table.rect.width
        );
    }

    #[test]
    fn a_centred_table_is_centred_without_centring_its_text() {
        // `<table align="center">` mapped to `text-align` centres every line on
        // the page, because `text-align` inherits and a table of this era wraps
        // the whole document.
        let rendered = run(
            r#"<body><table align="center" width="200"><tr><td>cell</td></tr></table></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        let all = content_boxes(&rendered);
        let table = all
            .iter()
            .find(|b| b.style.display == Display::Table)
            .expect("a table");
        assert!(
            table.rect.x > 100.0,
            "the table should be centred, not at {}",
            table.rect.x
        );
        let cell = all
            .iter()
            .find(|b| b.text.is_some())
            .expect("the cell's text");
        assert_eq!(
            cell.style.text_align,
            TextAlign::Left,
            "the contents must not be centred"
        );
    }

    #[test]
    fn a_cell_holding_a_nested_table_does_not_collapse() {
        // Measuring only a cell's text reports zero for one whose content is a
        // nested table, and its column collapses to nothing — which is the
        // shape nearly every page of this era is built out of.
        let rendered = run(
            "<body><table><tr>\
               <td>nav</td>\
               <td><table><tr><td>the content column</td></tr></table></td>\
             </tr></table></body>",
            "body { margin: 0 }",
            600.0,
        );
        // The widest text box is the inner column; if its cell collapsed it
        // would be narrower than the word "nav" beside it.
        let widest = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .map(|b| b.content_width)
            .fold(0.0f32, f32::max);
        assert!(widest > 100.0, "the inner column is {widest} wide");
    }

    #[test]
    fn a_declared_column_width_is_not_stretched_to_fill_the_table() {
        // A 150px sidebar beside a flexible column means a fixed sidebar and a
        // content column that takes the rest. Scaling both in proportion gives
        // a sidebar that grows with the window, which is the opposite of what
        // the markup asked for.
        let rendered = run(
            r#"<body><table width="600"><tr>
                 <td width="150">nav</td><td>content</td>
               </tr></table></body>"#,
            "body { margin: 0 } td { padding: 0 } table { border-spacing: 0 }",
            600.0,
        );
        let cells: Vec<&LayoutBox> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].rect.width, 150.0);
        assert!(
            (cells[1].rect.width - 450.0).abs() < 0.01,
            "content column is {}",
            cells[1].rect.width
        );
    }

    #[test]
    fn a_float_reserves_its_margins_too() {
        // `hspace="8"` on an image is asking for text to keep eight pixels
        // away; reserving only the border box lets the text touch it.
        let start = |css: &str| {
            let rendered = run(
                "<body><div class=\"f\">float</div><p>Text beside it.</p></body>",
                css,
                600.0,
            );
            content_boxes(&rendered)
                .into_iter()
                .filter(|b| b.text.is_some())
                .filter(|b| b.style.float == Float::None)
                .map(|b| b.rect.x)
                .next()
                .expect("the paragraph")
        };
        let bare = start("body { margin: 0 } .f { float: left; width: 100px }");
        let spaced =
            start("body { margin: 0 } .f { float: left; width: 100px; margin-right: 20px }");
        // The paragraph's box still starts at 0; what moves is where its text
        // may begin, so compare the first line's offset instead.
        assert_eq!(bare, spaced, "the block itself is not moved by a float");

        let line_start = |css: &str| {
            let rendered = run(
                "<body><div class=\"f\">float</div><p>Text beside it.</p></body>",
                css,
                600.0,
            );
            let rendered_boxes = content_boxes(&rendered);
            let paragraph = rendered_boxes
                .into_iter()
                .rfind(|b| b.text.is_some() && b.style.float == Float::None)
                .expect("the paragraph");
            paragraph.text.as_ref().expect("text").lines[0].glyphs[0].x
        };
        assert_eq!(
            line_start("body { margin: 0 } .f { float: left; width: 100px; margin-right: 20px }")
                - line_start("body { margin: 0 } .f { float: left; width: 100px }"),
            20.0
        );
    }

    #[test]
    fn an_inline_element_wrapping_a_block_still_lays_the_block_out() {
        // `<font>…<hr>…</font>` is ordinary markup. Skipped when gathering
        // inline runs and never reached by the block walk, the `<hr>`
        // disappears from the page entirely.
        let rendered = run(
            "<body><font>before<hr>after</font></body>",
            "body { margin: 0 } hr { height: 4px; background: #ff0000 }",
            600.0,
        );
        let rule = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.background_color == css::Color::rgb(255, 0, 0));
        assert!(rule.is_some(), "the rule inside the font element vanished");
    }

    #[test]
    fn a_cell_is_middle_aligned_unless_told_otherwise() {
        let offset = |markup: &str| {
            let rendered = run(
                &format!("<body><table><tr>{markup}<td>a<br>b<br>c<br>d</td></tr></table></body>"),
                "body { margin: 0 } td { padding: 0 }",
                600.0,
            );
            let cells: Vec<&LayoutBox> = content_boxes(&rendered)
                .into_iter()
                .filter(|b| b.text.is_some())
                .collect();
            cells[0].content_origin.1
        };
        assert!(
            offset("<td>short</td>") > 0.0,
            "a short cell is centred against a tall one"
        );
        assert_eq!(
            offset(r#"<td valign="top">short</td>"#),
            0.0,
            "valign=\"top\" is what stops it"
        );
        assert!(
            offset(r#"<td valign="bottom">short</td>"#) > offset("<td>short</td>"),
            "bottom sits lower than middle"
        );
    }

    #[test]
    fn cellspacing_sets_the_gap_between_cells() {
        // `cellspacing="0"` is how a table used for page layout closed the
        // seams between its cells. Leaving the 2px initial value in place puts
        // a visible line through the layout.
        let gap = |css: &str| {
            let rendered = run(
                "<body><table><tr><td>a</td><td>b</td></tr></table></body>",
                css,
                600.0,
            );
            let cells: Vec<Rect> = content_boxes(&rendered)
                .into_iter()
                .filter(|b| b.text.is_some())
                .map(|b| b.rect)
                .collect();
            assert_eq!(cells.len(), 2);
            cells[1].x - (cells[0].x + cells[0].width)
        };

        assert_eq!(gap("body { margin: 0 }"), 2.0, "the CSS 2.1 initial value");
        assert_eq!(gap("body { margin: 0 } table { border-spacing: 0 }"), 0.0);
        assert_eq!(
            gap("body { margin: 0 } table { border-spacing: 12px }"),
            12.0
        );
    }

    #[test]
    fn the_cellspacing_attribute_is_border_spacing() {
        let rendered = run(
            r#"<body><table cellspacing="0"><tr><td>a</td><td>b</td></tr></table></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        let cells: Vec<Rect> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .map(|b| b.rect)
            .collect();
        assert_eq!(cells[1].x - (cells[0].x + cells[0].width), 0.0);
    }

    #[test]
    fn a_table_box_shrinks_to_fit_its_columns() {
        // Otherwise its border and background stretch across the container
        // while the cells huddle at one end.
        let rendered = run(
            "<body><table><tr><td>a</td></tr></table></body>",
            "body { margin: 0 } table { border: 1px solid red }",
            600.0,
        );
        let table = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.display == Display::Table)
            .expect("a table box");
        assert!(
            table.rect.width < 200.0,
            "table box is {} wide in a 600px container",
            table.rect.width
        );
    }

    #[test]
    fn a_declared_width_still_fills_it() {
        let rendered = run(
            "<body><table><tr><td>a</td></tr></table></body>",
            "body { margin: 0 } table { width: 100% }",
            600.0,
        );
        let table = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.display == Display::Table)
            .expect("a table box");
        assert_eq!(table.rect.width, 600.0);
    }

    #[test]
    fn a_rowspan_cell_holds_its_column_in_the_rows_below() {
        // Without occupancy tracking the second row's only cell takes column 0
        // — the one the spanning cell is still in — and every row below the
        // span shifts left by a column.
        let doc = dom::parse(
            r#"<body><table>
                 <tr><td rowspan="2">span</td><td>a</td></tr>
                 <tr><td>b</td></tr>
               </table></body>"#,
        );
        let styles = css::cascade::cascade(&doc, &[Stylesheet::parse("body { margin: 0 }")]);
        let grid = table::build_grid(&doc, &styles, doc.find_element("table").expect("table"));

        assert_eq!(grid.columns, 2);
        assert_eq!(grid.rows[0].cells[0].rowspan, 2);
        assert_eq!(grid.rows[0].cells[1].column, 1);
        assert_eq!(
            grid.rows[1].cells[0].column, 1,
            "the second row's cell sits beside the span, not under it"
        );
    }

    #[test]
    fn a_rowspan_cell_reaches_the_bottom_of_its_last_row() {
        let rendered = run(
            r#"<body><table>
                 <tr><td rowspan="2">tall</td><td>a</td></tr>
                 <tr><td>b</td></tr>
               </table></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        let cells: Vec<&LayoutBox> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert_eq!(cells.len(), 3);

        let spanning = &cells[0];
        let last = cells
            .iter()
            .max_by(|a, b| {
                (a.rect.y + a.rect.height)
                    .partial_cmp(&(b.rect.y + b.rect.height))
                    .expect("finite")
            })
            .expect("a cell");
        assert!(
            spanning.rect.height >= last.rect.y + last.rect.height - spanning.rect.y - 0.01,
            "spanning cell {:?} stops short of the last row {:?}",
            spanning.rect,
            last.rect
        );
    }

    #[test]
    fn a_tall_rowspan_cell_grows_the_rows_it_covers() {
        // Its content has to fit somewhere: if the rows it spans are shorter
        // than it is, one of them has to give.
        let short = run(
            r#"<body><table><tr><td>a</td><td>b</td></tr>
               <tr><td>c</td><td>d</td></tr></table></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        let tall = run(
            r#"<body><table>
                 <tr><td rowspan="2" style="height: 200px">tall</td><td>b</td></tr>
                 <tr><td>d</td></tr>
               </table></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        let height = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .map(|b| b.rect.y + b.rect.height)
                .fold(0.0f32, f32::max)
        };
        assert!(
            height(&tall) > height(&short) + 100.0,
            "the table did not grow for its spanning cell"
        );
    }

    #[test]
    fn an_absurd_span_does_not_size_an_allocation() {
        // The attribute is unbounded in the markup, and this one is a plausible
        // typo as well as a plausible attack.
        let rendered = run(
            r#"<body><table><tr><td rowspan="99999999" colspan="99999999">x</td></tr></table></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        assert!(
            content_boxes(&rendered).iter().any(|b| b.text.is_some()),
            "the cell still renders"
        );
    }

    #[test]
    fn a_narrow_table_shrinks_to_fit_rather_than_stretching() {
        let rendered = run(
            "<body><table><tr><td>a</td><td>b</td></tr></table></body>",
            "body { margin: 0 }",
            800.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        let total: f32 = cells.iter().map(|c| c.rect.width).sum();
        assert!(
            total < 200.0,
            "two one-letter cells should not fill 800px, got {total}"
        );
    }

    #[test]
    fn a_wide_column_gets_more_room_than_a_narrow_one() {
        let rendered = run(
            "<body><table><tr><td>x</td>\
             <td>a considerably longer piece of cell content here</td></tr></table></body>",
            "body { margin: 0 }",
            400.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert!(
            cells[1].rect.width > cells[0].rect.width * 3.0,
            "column sizing ignored content: {} vs {}",
            cells[0].rect.width,
            cells[1].rect.width
        );
    }

    #[test]
    fn a_full_width_table_fills_its_container() {
        // `<table width="100%">` was ubiquitous on the era's pages.
        let rendered = run(
            "<body><table><tr><td>a</td><td>b</td></tr></table></body>",
            "body { margin: 0 } table { width: 100% }",
            500.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        let total: f32 = cells.iter().map(|c| c.rect.width).sum();
        assert!(
            total > 450.0,
            "declared width should fill the container, got {total}"
        );
    }

    #[test]
    fn a_spanning_cell_covers_the_columns_below_it() {
        let rendered = run(
            r#"<body><table><tr><td colspan="2">wide header</td></tr>
               <tr><td>a</td><td>b</td></tr></table></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert_eq!(cells.len(), 3);
        let spanning = cells[0].rect.width;
        let below = cells[1].rect.width + cells[2].rect.width;
        assert!(
            spanning >= below - 5.0,
            "span {spanning} should cover both columns {below}"
        );
    }

    #[test]
    fn text_flows_beside_a_left_float() {
        let html = "<body><div class=\"f\">side</div><p>the quick brown fox jumps over the \
                    lazy dog and keeps on running for a good while longer than one line \
                    so that several lines sit beside the float</p></body>";
        // Tall enough to narrow several lines; a one-line float would narrow
        // only the first and the test could not tell the cases apart.
        let floated = run(
            html,
            "body { margin: 0 } .f { float: left; width: 150px; height: 120px }",
            500.0,
        );
        let plain = run(
            html,
            "body { margin: 0 } .f { width: 150px; height: 120px }",
            500.0,
        );

        let float_box = content_boxes(&floated)
            .into_iter()
            .find(|b| b.style.float == Float::Left)
            .expect("float box");
        assert_eq!(
            float_box.rect.y, 0.0,
            "the float sits at the top, not below the text"
        );

        // Beside a 150px float the paragraph has less room, so it needs more
        // lines than the same paragraph laid out at full width.
        let lines = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .filter_map(|b| b.text.as_ref().map(|t| t.lines.len()))
                .max()
                .unwrap_or(0)
        };
        assert!(
            lines(&floated) > lines(&plain),
            "text did not narrow beside the float: {} vs {}",
            lines(&floated),
            lines(&plain)
        );
    }

    #[test]
    fn a_right_float_sits_against_the_right_edge() {
        let rendered = run(
            "<body><div class=\"f\">side</div><p>text</p></body>",
            "body { margin: 0 } .f { float: right; width: 100px }",
            600.0,
        );
        let float_box = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.float == Float::Right)
            .expect("float box");
        assert!(
            float_box.rect.x > 400.0,
            "expected a right-hand position, got {}",
            float_box.rect.x
        );
    }

    #[test]
    fn clear_pushes_a_block_below_the_float() {
        let cleared = run(
            "<body><div class=\"f\">side</div><p class=\"c\">after</p></body>",
            "body { margin: 0 } .f { float: left; width: 100px; height: 80px } .c { clear: left }",
            500.0,
        );
        let uncleared = run(
            "<body><div class=\"f\">side</div><p class=\"c\">after</p></body>",
            "body { margin: 0 } .f { float: left; width: 100px; height: 80px }",
            500.0,
        );
        // Select the paragraph specifically: the float also has text, and it is
        // pushed into the box list first.
        let paragraph_y = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .find(|b| b.style.float == Float::None && b.text.is_some())
                .map(|b| b.rect.y)
                .expect("paragraph box")
        };
        assert!(
            paragraph_y(&cleared) >= 80.0,
            "clear:left must move below the 80px float, got {}",
            paragraph_y(&cleared)
        );
        assert!(
            paragraph_y(&uncleared) < 80.0,
            "without clear it stays alongside"
        );
    }

    #[test]
    fn adjacent_sibling_margins_collapse_into_one() {
        // §8.3.1, and the single largest thing the CSS 2.1 suite found missing:
        // two blocks 40px apart were getting 80px between them.
        let rendered = run(
            "<body><div class=\"a\"></div><div class=\"b\"></div></body>",
            "body { margin: 0 } \
             .a { height: 20px; margin-bottom: 40px } \
             .b { height: 20px; margin-top: 40px }",
            200.0,
        );
        let boxes: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.rect.height == 20.0)
            .collect();
        assert_eq!(boxes.len(), 2, "both blocks are laid out");
        assert_eq!(
            boxes[1].rect.y - (boxes[0].rect.y + boxes[0].rect.height),
            40.0,
            "the gap is one 40px margin, not two",
        );
    }

    #[test]
    fn a_margin_collapses_straight_through_an_empty_block() {
        // §8.3.1's fourth rule, and the one the README named as missing. A box
        // with nothing in it and nothing separating its edges does not hold its
        // two margins apart: they are adjoining, so they collapse with each
        // other and with the run the box sits in, and the box itself takes up
        // nothing at all.
        //
        // Without it every empty wrapper on a page added a margin nobody wrote,
        // which is worst in the document fallback — the author's own `margin: 0`
        // resets leave with the rest of their stylesheet.
        let plain = run(
            "<body><div class=\"a\"></div><div class=\"b\"></div></body>",
            "body { margin: 0 } \
             .a { height: 20px; margin-bottom: 40px } \
             .b { height: 20px; margin-top: 40px }",
            200.0,
        );
        let padded = run(
            "<body><div class=\"a\"></div><div class=\"gap\"></div>\
             <div class=\"gap\"></div><div class=\"gap\"></div>\
             <div class=\"b\"></div></body>",
            "body { margin: 0 } \
             .a { height: 20px; margin-bottom: 40px } \
             .gap { margin: 40px 0 } \
             .b { height: 20px; margin-top: 40px }",
            200.0,
        );
        let gap_of = |rendered: &Rendered| {
            let boxes: Vec<_> = content_boxes(rendered)
                .into_iter()
                .filter(|b| b.rect.height == 20.0)
                .collect();
            assert_eq!(boxes.len(), 2, "both real blocks are laid out");
            boxes[1].rect.y - (boxes[0].rect.y + boxes[0].rect.height)
        };
        assert_eq!(gap_of(&plain), 40.0, "the pair on their own");
        assert_eq!(
            gap_of(&padded),
            40.0,
            "three empty blocks between them added margins of their own"
        );
    }

    #[test]
    fn a_block_with_something_in_it_still_holds_its_margins_apart() {
        // The other side of the rule, and what stops it eating real spacing:
        // anything at all between the two edges — content, a border, padding, a
        // height — means the margins are not adjoining and both apply.
        let gap_of = |extra: &str, rule: &str| {
            let rendered = run(
                &format!(
                    "<body><div class=\"a\"></div><div class=\"gap\">{extra}</div>\
                     <div class=\"b\"></div></body>"
                ),
                &format!(
                    "body {{ margin: 0 }} \
                     .a {{ height: 20px; margin-bottom: 40px }} \
                     .gap {{ margin: 40px 0; {rule} }} \
                     .b {{ height: 20px; margin-top: 40px }}"
                ),
                200.0,
            );
            let boxes: Vec<_> = content_boxes(&rendered)
                .into_iter()
                .filter(|b| b.rect.height == 20.0)
                .collect();
            boxes[1].rect.y - (boxes[0].rect.y + boxes[0].rect.height)
        };
        assert!(
            gap_of("text", "") > 40.0,
            "text between them separates them"
        );
        assert!(
            gap_of("", "height: 10px") > 40.0,
            "a height of its own separates them"
        );
        assert!(
            gap_of("", "border-top: 1px solid #000") > 40.0,
            "a border separates them"
        );
        assert!(
            gap_of("", "padding-top: 1px") > 40.0,
            "padding separates them"
        );
    }

    #[test]
    fn collapsing_takes_the_largest_positive_and_the_most_negative() {
        // The rule people remember is `max`, and that is only right while both
        // margins are positive. Two negatives pull by the larger of the two
        // rather than by their sum, and a mixed pair cancels.
        assert_eq!(collapse(40.0, 10.0), 40.0);
        assert_eq!(collapse(-20.0, -20.0), -20.0);
        assert_eq!(collapse(-10.0, -30.0), -30.0);
        assert_eq!(collapse(40.0, -15.0), 25.0);
        assert_eq!(collapse(0.0, 0.0), 0.0);
    }

    #[test]
    fn overflow_stops_a_margin_escaping_its_container() {
        // Anything but `overflow: visible` establishes a block formatting
        // context, and a margin cannot escape one. The CSS 2.1 suite tests it
        // with a negative margin sized to cancel the child exactly if it stays
        // inside, which is what makes the difference visible at all.
        let clipped = run(
            "<body><div class=\"box\"><div class=\"tall\"></div></div></body>",
            "body { margin: 0 } \
             .box { overflow: hidden; width: 200px } \
             .tall { height: 200px; margin-bottom: -100px }",
            400.0,
        );
        let visible = run(
            "<body><div class=\"box\"><div class=\"tall\"></div></div></body>",
            "body { margin: 0 } \
             .box { width: 200px } \
             .tall { height: 200px; margin-bottom: -100px }",
            400.0,
        );
        let container = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .find(|b| b.rect.width == 200.0)
                .map(|b| b.rect.height)
                .expect("the container")
        };
        assert_eq!(
            container(&clipped),
            100.0,
            "the negative margin stays inside and shortens the container",
        );
        assert_eq!(
            container(&visible),
            200.0,
            "with `overflow: visible` it escapes and the container keeps its height",
        );
    }

    #[test]
    fn content_between_two_blocks_stops_their_margins_collapsing() {
        // The margins have to be *adjoining*. A line of text between them is
        // not nothing, and treating it as nothing pulls the blocks together
        // through their own content.
        let with_text = run(
            "<body><div class=\"a\"></div>between<div class=\"b\"></div></body>",
            "body { margin: 0 } \
             .a { height: 20px; margin-bottom: 40px } \
             .b { height: 20px; margin-top: 40px }",
            200.0,
        );
        let without = run(
            "<body><div class=\"a\"></div><div class=\"b\"></div></body>",
            "body { margin: 0 } \
             .a { height: 20px; margin-bottom: 40px } \
             .b { height: 20px; margin-top: 40px }",
            200.0,
        );
        let last_y = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .filter(|b| b.rect.height == 20.0)
                .map(|b| b.rect.y)
                .fold(0.0f32, f32::max)
        };
        assert!(
            last_y(&with_text) > last_y(&without),
            "text between the blocks must keep both margins: {} vs {}",
            last_y(&with_text),
            last_y(&without),
        );
    }

    #[test]
    fn a_cleared_line_starts_at_the_edge_inside_a_padded_container() {
        // The existing `clear` test puts the floats directly in `body` with no
        // padding, and passed throughout this bug. `clear` was asked in the
        // wrong coordinate space — `cursor_y` counts from the block's border
        // box and the float context had been translated into its content box —
        // so clearance fell short by exactly the container's top padding and
        // border.
        //
        // What that looks like is not a box in the wrong place: the box lands
        // correctly, because it is moved again by the enclosing flow. It is the
        // box's *first line* dodging sideways as though a float were still
        // beside it. So the assertion is about where the text starts, not where
        // the block does — the version of this test that checks `rect.y`
        // passes with the bug in place.
        let rendered = run(
            "<body><div class=\"pad\">\
             <div class=\"f\"></div><p class=\"c\">after</p>\
             </div></body>",
            "body { margin: 0 } \
             .pad { padding: 12px; border: 1px solid #888 } \
             .f { float: left; width: 100px; height: 80px } \
             .c { clear: both; margin: 0 }",
            500.0,
        );

        let line_x = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.float == Float::None && b.text.is_some())
            .and_then(|b| {
                b.text
                    .as_ref()
                    .and_then(|layout| layout.lines.first())
                    .map(|line| line.glyphs.first().map(|g| g.x).unwrap_or(0.0))
            })
            .expect("the cleared paragraph has a line");

        assert!(
            line_x < 1.0,
            "the cleared paragraph's first line starts at {line_x}, so it is \
             still avoiding a float it was cleared past",
        );
    }

    #[test]
    fn a_container_encloses_a_float_taller_than_its_text() {
        // Otherwise the next block starts beside the float and overlaps it.
        let rendered = run(
            "<body><div class=\"box\"><div class=\"f\">side</div>short</div></body>",
            "body { margin: 0 } .f { float: left; width: 60px; height: 120px }",
            400.0,
        );
        let container = content_boxes(&rendered)[0];
        assert!(
            container.rect.height >= 120.0,
            "container must enclose its float, got {}",
            container.rect.height
        );
    }

    #[test]
    fn an_image_uses_its_intrinsic_size_when_nothing_is_declared() {
        let style = ComputedStyle::default();
        assert_eq!(
            replaced_size(&style, Some((80.0, 40.0)), None, None, 500.0),
            (80.0, 40.0)
        );
    }

    #[test]
    fn one_declared_dimension_preserves_the_aspect_ratio() {
        // `<img width="200">` on a 2:1 image must not squash it.
        let style = ComputedStyle::default();
        assert_eq!(
            replaced_size(&style, Some((100.0, 50.0)), Some(200.0), None, 500.0),
            (200.0, 100.0)
        );
        assert_eq!(
            replaced_size(&style, Some((100.0, 50.0)), None, Some(25.0), 500.0),
            (50.0, 25.0)
        );
    }

    #[test]
    fn both_declared_dimensions_win_over_the_ratio() {
        let style = ComputedStyle::default();
        assert_eq!(
            replaced_size(&style, Some((100.0, 50.0)), Some(30.0), Some(300.0), 500.0),
            (30.0, 300.0)
        );
    }

    #[test]
    fn css_overrides_the_presentational_attribute() {
        let style = ComputedStyle {
            width: Length::Px(64.0),
            ..ComputedStyle::default()
        };
        let (width, _) = replaced_size(&style, Some((100.0, 100.0)), Some(999.0), None, 500.0);
        assert_eq!(width, 64.0);
    }

    #[test]
    fn an_image_that_never_loaded_still_occupies_its_declared_box() {
        // A broken image must not collapse the layout around it.
        let style = ComputedStyle::default();
        assert_eq!(
            replaced_size(&style, None, Some(120.0), Some(60.0), 500.0),
            (120.0, 60.0)
        );
        assert_eq!(
            replaced_size(&style, None, None, None, 500.0),
            BROKEN_IMAGE_SIZE
        );
    }

    #[test]
    fn an_image_element_becomes_a_replaced_box() {
        let doc = dom::parse(r#"<body><img src="x.png" width="90" height="45"></body>"#);
        let styles = css::cascade::cascade(&doc, &[]);
        let mut fonts = FontStore::new();
        let sizes = IntrinsicSizes::new();
        let laid_out = layout(&doc, &styles, &mut fonts, &sizes, 500.0);
        let image = laid_out
            .root
            .children
            .first()
            .and_then(|body| body.children.first())
            .expect("image box");
        assert!(image.replaced.is_some(), "img must be marked replaced");
        assert_eq!(image.rect.width, 90.0);
        assert_eq!(image.rect.height, 45.0);
    }

    #[test]
    fn a_percentage_size_attribute_is_ignored_rather_than_read_as_pixels() {
        let doc = dom::parse(r#"<body><img src="x.png" width="50%"></body>"#);
        assert_eq!(
            size_attr(&doc, doc.find_element("img").expect("img"), "width"),
            None,
            "50% must not be read as 50px"
        );
    }

    #[test]
    fn text_flows_beside_a_floated_image() {
        // A floated image has no text to measure, so its width has to come from
        // its intrinsic or declared size. Measuring it as text registered a
        // zero-width float and let the paragraph run straight over the image.
        let doc = dom::parse(
            r#"<body><p>before</p><img src="x.png" width="90" height="60">
               <p>the quick brown fox jumps over the lazy dog and runs on and on</p></body>"#,
        );
        let sheets = [css::Stylesheet::parse(
            "body { margin: 0 } img { float: left }",
        )];
        let styles = css::cascade::cascade(&doc, &sheets);
        let mut fonts = FontStore::new();
        let laid = layout(&doc, &styles, &mut fonts, &IntrinsicSizes::new(), 400.0);

        let rendered = Rendered { layout: laid };
        let boxes = content_boxes(&rendered);
        let image = boxes
            .iter()
            .find(|b| b.replaced.is_some())
            .expect("image box");
        assert_eq!(image.rect.width, 90.0);

        let after = boxes
            .iter()
            .rfind(|b| b.text.is_some())
            .expect("trailing paragraph");
        let first_glyph_x = after
            .text
            .as_ref()
            .and_then(|t| t.lines.first())
            .and_then(|l| l.glyphs.first())
            .map(|g| g.x)
            .expect("glyphs");
        assert!(
            first_glyph_x >= 90.0,
            "text should start past the 90px float, got {first_glyph_x}"
        );
    }

    #[test]
    fn relative_positioning_shifts_a_box_without_moving_its_siblings() {
        let html = "<body><p>one</p><p class=\"r\">two</p><p>three</p></body>";
        let shifted = run(
            html,
            "body { margin: 0 } .r { position: relative; left: 40px; top: 5px }",
            400.0,
        );
        let plain = run(html, "body { margin: 0 }", 400.0);

        let shifted_boxes = content_boxes(&shifted);
        let plain_boxes = content_boxes(&plain);
        assert_eq!(shifted_boxes[1].rect.x, plain_boxes[1].rect.x + 40.0);
        assert_eq!(shifted_boxes[1].rect.y, plain_boxes[1].rect.y + 5.0);

        // The space it would have taken is kept, so the third paragraph does
        // not move — that is the whole difference from absolute positioning.
        assert_eq!(shifted_boxes[2].rect.y, plain_boxes[2].rect.y);
        assert_eq!(shifted.layout.height, plain.layout.height);
    }

    #[test]
    fn a_negative_relative_offset_moves_the_other_way() {
        let html = "<body><p>one</p><p class=\"r\">two</p></body>";
        let shifted = run(
            html,
            "body { margin: 0 } .r { position: relative; right: 30px }",
            400.0,
        );
        let plain = run(html, "body { margin: 0 }", 400.0);
        assert_eq!(
            content_boxes(&shifted)[1].rect.x,
            content_boxes(&plain)[1].rect.x - 30.0,
            "`right` shifts leftwards"
        );
    }

    #[test]
    fn an_absolute_box_leaves_the_flow() {
        let html = "<body><p>one</p><p class=\"a\">floating free</p><p>three</p></body>";
        let positioned = run(
            html,
            "body { margin: 0 } .a { position: absolute; top: 200px }",
            400.0,
        );
        let plain = run(html, "body { margin: 0 }", 400.0);

        // The paragraph after it moves up into the space it vacated.
        let after = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .filter(|b| b.style.position != Position::Absolute)
                .filter(|b| b.text.is_some())
                .map(|b| b.rect.y)
                .next_back()
                .expect("last in-flow paragraph")
        };
        assert!(
            after(&positioned) < after(&plain),
            "in-flow content should close the gap: {} vs {}",
            after(&positioned),
            after(&plain)
        );
    }

    #[test]
    fn absolute_offsets_resolve_against_the_nearest_positioned_ancestor() {
        let rendered = run(
            "<body><div class=\"outer\"><div class=\"inner\">x</div></div></body>",
            "body { margin: 0 } \
             .outer { position: relative; margin-top: 50px; padding: 10px } \
             .inner { position: absolute; left: 20px; top: 30px }",
            400.0,
        );
        let inner = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.position == Position::Absolute)
            .expect("absolute box");
        // Coordinates are parent-relative, and the positioned parent is the
        // containing block, so the offsets land unchanged.
        assert_eq!(inner.rect.x, 20.0);
        assert_eq!(inner.rect.y, 30.0);
    }

    #[test]
    fn right_and_bottom_offsets_measure_from_the_far_edges() {
        let rendered = run(
            "<body><div class=\"outer\"><div class=\"inner\">x</div></div></body>",
            "body { margin: 0 } \
             .outer { position: relative; height: 200px } \
             .inner { position: absolute; right: 0; bottom: 0; width: 50px; height: 20px }",
            400.0,
        );
        let inner = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.position == Position::Absolute)
            .expect("absolute box");
        assert_eq!(
            inner.rect.x, 350.0,
            "400 wide container, 50 wide box, right: 0"
        );
        assert_eq!(
            inner.rect.y, 180.0,
            "200 tall container, 20 tall box, bottom: 0"
        );
    }

    #[test]
    fn an_absolute_box_with_no_offsets_stays_where_flow_would_have_put_it() {
        let rendered = run(
            "<body><p>one</p><p class=\"a\">two</p></body>",
            "body { margin: 0 } .a { position: absolute }",
            400.0,
        );
        let absolute = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.position == Position::Absolute)
            .expect("absolute box");
        assert!(
            absolute.rect.y > 0.0,
            "should sit below the first paragraph, got {}",
            absolute.rect.y
        );
    }

    #[test]
    fn whitespace_collapses_across_inline_run_boundaries() {
        // The space between two inline elements lives in neither of them.
        // Collapsing runs in isolation either loses it or doubles it, and both
        // are visible on any page that emphasises a word.
        let spaced = run("<body><p><b>one</b> <i>two</i></p></body>", "", 800.0);
        let joined = run("<body><p><b>one</b><i>two</i></p></body>", "", 800.0);
        let width = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .find_map(|b| b.text.as_ref().map(|t| t.width))
                .expect("text")
        };
        assert!(
            width(&spaced) > width(&joined),
            "the inter-element space vanished: {} vs {}",
            width(&spaced),
            width(&joined)
        );
    }

    #[test]
    fn runs_of_whitespace_around_a_span_collapse_to_one() {
        let single = run("<body><p>a <b>b</b></p></body>", "", 800.0);
        let many = run("<body><p>a   <b>  b</b></p></body>", "", 800.0);
        let width = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .find_map(|b| b.text.as_ref().map(|t| t.width))
                .expect("text")
        };
        assert!(
            (width(&single) - width(&many)).abs() < 0.01,
            "extra whitespace was not collapsed: {} vs {}",
            width(&single),
            width(&many)
        );
    }

    #[test]
    fn pre_preserves_whitespace() {
        let html = "<body><pre>one\ntwo\nthree</pre></body>";
        let rendered = run(html, "", 800.0);
        let pre = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.text.is_some())
            .expect("pre box");
        assert_eq!(
            pre.text.as_ref().unwrap().lines.len(),
            3,
            "newlines must survive in pre"
        );
    }

    #[test]
    fn text_align_offsets_lines_within_the_content_box() {
        assert_eq!(line_offset(TextAlign::Left, 100.0, 500.0), 0.0);
        assert_eq!(line_offset(TextAlign::Center, 100.0, 500.0), 200.0);
        assert_eq!(line_offset(TextAlign::Right, 100.0, 500.0), 400.0);
        // A line wider than its box never produces a negative offset.
        assert_eq!(line_offset(TextAlign::Center, 700.0, 500.0), 0.0);
    }
}

#[cfg(test)]
mod hit_tests {
    use super::*;
    use css::Stylesheet;

    struct Page {
        doc: Document,
        layout: Layout,
    }

    fn page(html: &str, css_text: &str) -> Page {
        let doc = dom::parse(html);
        let styles = css::cascade::cascade(&doc, &[Stylesheet::parse(css_text)]);
        let mut fonts = FontStore::new();
        let layout = layout(&doc, &styles, &mut fonts, &IntrinsicSizes::new(), 600.0);
        Page { doc, layout }
    }

    /// The first rectangle belonging to the element with this tag.
    fn rect_of(page: &Page, tag: &str) -> Rect {
        let node = page.doc.find_element(tag).expect("element present");
        *page
            .layout
            .rects_for(node)
            .first()
            .unwrap_or_else(|| panic!("<{tag}> has no rectangle"))
    }

    #[test]
    fn a_point_over_a_link_finds_the_link() {
        // The point lands on text, and that text belongs to the anchor — not
        // to the paragraph containing it. Without spans there is nothing to
        // hit at all: an inline element has no box.
        let page = page(
            r#"<body><p>before <a href="x.html">the link</a> after</p></body>"#,
            "body { margin: 0 }",
        );
        let link = page.doc.find_element("a").expect("an anchor");
        let rect = rect_of(&page, "a");
        let hit = page
            .layout
            .hit_test(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
        assert_eq!(hit, Some(link));
    }

    #[test]
    fn a_point_beside_a_link_does_not() {
        let page = page(
            r#"<body><p>before <a href="x.html">link</a> after</p></body>"#,
            "body { margin: 0 }",
        );
        let link = page.doc.find_element("a").expect("an anchor");
        let rect = rect_of(&page, "a");
        // Just past its right edge, still on the same line.
        let hit = page
            .layout
            .hit_test(rect.x + rect.width + 6.0, rect.y + rect.height / 2.0);
        assert_ne!(hit, Some(link));
    }

    #[test]
    fn a_hit_inside_a_link_resolves_to_the_link_itself() {
        // The text belongs to the `<b>`, and the href is on the `<a>` above it.
        let page = page(
            r#"<body><p><a href="x.html"><b>bold link</b></a></p></body>"#,
            "body { margin: 0 }",
        );
        let rect = rect_of(&page, "b");
        let hit = page
            .layout
            .hit_test(rect.x + 2.0, rect.y + rect.height / 2.0)
            .expect("something under the point");
        let (link, href) = page
            .doc
            .enclosing_link(hit)
            .expect("a link encloses the hit");
        assert_eq!(link, page.doc.find_element("a").expect("an anchor"));
        assert_eq!(href, "x.html");
    }

    #[test]
    fn a_named_anchor_is_not_a_link() {
        // It is a destination. Reporting it as clickable invites a click that
        // does nothing.
        let page = page(
            r#"<body><p><a name="here">destination</a></p></body>"#,
            "body { margin: 0 }",
        );
        let anchor = page.doc.find_element("a").expect("an anchor");
        assert!(page.doc.enclosing_link(anchor).is_none());
    }

    #[test]
    fn a_wrapped_link_has_a_rectangle_per_line() {
        // One bounding box would swallow the text either side of it on the
        // first and last lines, which is wrong to click and wrong to draw.
        let page = page(
            r#"<body><p>lead in <a href="x.html">a link long enough that it has to
               wrap across more than one line of this paragraph</a> and out</p></body>"#,
            "body { margin: 0 } p { width: 200px }",
        );
        let link = page.doc.find_element("a").expect("an anchor");
        let rects = page.layout.rects_for(link);
        assert!(rects.len() > 1, "got {} rectangles", rects.len());
        // Every one of them is a live target.
        for rect in &rects {
            let hit = page
                .layout
                .hit_test(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
            assert_eq!(hit, Some(link), "missed the fragment at {rect:?}");
        }
    }

    #[test]
    fn an_image_is_hit_where_it_is_drawn() {
        let mut sizes = IntrinsicSizes::new();
        let doc = dom::parse(r#"<body><p>text <a href="x.html"><img src="i.png"></a></p></body>"#);
        let image = doc.find_element("img").expect("img");
        sizes.insert(image, (40.0, 40.0));
        let styles = css::cascade::cascade(&doc, &[Stylesheet::parse("body { margin: 0 }")]);
        let mut fonts = FontStore::new();
        let laid_out = layout(&doc, &styles, &mut fonts, &sizes, 600.0);

        let rect = *laid_out
            .rects_for(image)
            .first()
            .expect("the image has a rectangle");
        let hit = laid_out
            .hit_test(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
            .expect("something under the image");
        assert_eq!(
            doc.enclosing_link(hit).map(|(node, _)| node),
            doc.find_element("a"),
            "an image inside a link is part of the link"
        );
    }

    #[test]
    fn a_point_past_the_content_hits_nothing_clickable() {
        let page = page(
            r#"<body><p><a href="x.html">link</a></p></body>"#,
            "body { margin: 0 }",
        );
        let hit = page.layout.hit_test(590.0, 2000.0);
        assert!(hit.is_none() || page.doc.enclosing_link(hit.expect("hit")).is_none());
    }
}

#[cfg(test)]
mod find_tests {
    use super::*;
    use css::Stylesheet;

    fn find_in(html: &str, css_text: &str, query: &str) -> Vec<Rect> {
        let doc = dom::parse(html);
        let styles = css::cascade::cascade(&doc, &[Stylesheet::parse(css_text)]);
        let mut fonts = FontStore::new();
        layout(&doc, &styles, &mut fonts, &IntrinsicSizes::new(), 600.0).find(query)
    }

    #[test]
    fn a_word_is_found_where_it_is_drawn() {
        let rects = find_in(
            "<body><p>the quick brown fox</p></body>",
            "body { margin: 0 }",
            "brown",
        );
        assert_eq!(rects.len(), 1);
        // Past "the quick " and narrower than the whole line.
        assert!(rects[0].x > 20.0, "at {:?}", rects[0]);
        assert!(
            rects[0].width > 5.0 && rects[0].width < 120.0,
            "{:?}",
            rects[0]
        );
    }

    #[test]
    fn matching_ignores_case() {
        let rects = find_in(
            "<body><p>The Quick Brown Fox</p></body>",
            "body { margin: 0 }",
            "brown",
        );
        assert_eq!(rects.len(), 1);
    }

    #[test]
    fn every_occurrence_is_found_in_reading_order() {
        let rects = find_in(
            "<body><p>one</p><p>two one three</p><p>one</p></body>",
            "body { margin: 0 }",
            "one",
        );
        assert_eq!(rects.len(), 3);
        for pair in rects.windows(2) {
            assert!(
                pair[0].y < pair[1].y || (pair[0].y == pair[1].y && pair[0].x <= pair[1].x),
                "out of order: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn a_phrase_spanning_a_space_between_spans_is_found() {
        // The space between two inline elements has no glyphs, so it only
        // exists in the line's text if the line assembles it deliberately.
        let rects = find_in(
            "<body><p><b>hello</b> <i>world</i></p></body>",
            "body { margin: 0 }",
            "hello world",
        );
        assert_eq!(rects.len(), 1, "the space between the spans was lost");
    }

    #[test]
    fn a_match_is_found_across_a_style_change_without_a_space() {
        let rects = find_in(
            "<body><p>un<b>break</b>able</p></body>",
            "body { margin: 0 }",
            "unbreakable",
        );
        assert_eq!(rects.len(), 1);
    }

    #[test]
    fn a_phrase_broken_by_a_line_break_is_not_found() {
        // The two halves are not one run of text on the screen, and there is no
        // single rectangle that would show the match. Reporting it would mean
        // scrolling somewhere and highlighting nothing.
        let rects = find_in(
            "<body><p>alpha<br>beta</p></body>",
            "body { margin: 0 }",
            "alpha beta",
        );
        assert!(rects.is_empty());
    }

    #[test]
    fn an_empty_query_finds_nothing() {
        // Otherwise every position in the document matches.
        assert!(find_in("<body><p>text</p></body>", "", "").is_empty());
        assert!(find_in("<body><p>text</p></body>", "", "   ").is_empty());
    }

    #[test]
    fn a_query_that_is_not_there_finds_nothing() {
        assert!(find_in("<body><p>text</p></body>", "", "absent").is_empty());
    }

    #[test]
    fn overlapping_occurrences_are_counted_once_each() {
        // "aa" in "aaaa" is two matches, not three: the reader stepping
        // through them expects to move past what was just highlighted.
        let rects = find_in("<body><p>aaaa</p></body>", "body { margin: 0 }", "aa");
        assert_eq!(rects.len(), 2);
    }

    #[test]
    fn text_inside_a_table_is_searchable() {
        // Cells are laid out through a different path from ordinary blocks, so
        // it is worth checking they are reached at all.
        let rects = find_in(
            "<body><table><tr><td>needle</td><td>other</td></tr></table></body>",
            "body { margin: 0 }",
            "needle",
        );
        assert_eq!(rects.len(), 1);
    }
}
