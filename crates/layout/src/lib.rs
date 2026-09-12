//! Box tree construction and block layout.
//!
//! Block boxes stack vertically, each laying its inline content out as an
//! inline formatting context: differently-styled spans share line boxes and
//! break as one paragraph. Floats, tables, and positioning are the rest of M2
//! (ADR-0004).

pub mod classify;
pub mod floats;
pub mod forms;
pub mod frameset;
pub mod table;

use css::cascade::StyleMap;
use css::selector::PseudoElement;
use css::style::{
    BorderCollapse, CaptionSide, ComputedStyle, Display, Float, Overflow, Position, TextAlign,
    VerticalAlign, WhiteSpace,
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
    /// The containing block's height, where it has a definite one.
    ///
    /// §10.5 turns on this and nothing else: a percentage `height` resolves
    /// against the containing block's height only when that height does not
    /// itself depend on the content. Otherwise the percentage computes to
    /// `auto`. `size.1` cannot answer it — that is a number whatever its
    /// provenance, and for a normal-flow block it is a width standing in for a
    /// height it does not know.
    definite_height: Option<f32>,
}

impl ContainingBlock {
    /// The initial containing block: the viewport.
    fn viewport(width: f32, height: f32) -> Self {
        Self {
            offset: (0.0, 0.0),
            size: (width, height),
            // The initial containing block is the viewport, whose height is
            // as definite as a height gets: `height: 100%` on the root fills
            // the window.
            definite_height: Some(height),
        }
    }

    /// This containing block seen from a child at `(dx, dy)` in local content
    /// coordinates.
    fn descend(self, dx: f32, dy: f32) -> Self {
        Self {
            offset: (self.offset.0 + dx, self.offset.1 + dy),
            size: self.size,
            definite_height: self.definite_height,
        }
    }

    /// This containing block with a definite height of `height`.
    fn with_definite_height(self, height: Option<f32>) -> Self {
        Self {
            definite_height: height,
            ..self
        }
    }

    /// A box establishing itself as the containing block for its descendants.
    ///
    /// Its height is definite: a positioned box has been sized by the time it
    /// becomes a containing block, so a percentage height inside it has a
    /// real number to resolve against.
    fn establish(size: (f32, f32)) -> Self {
        Self {
            offset: (0.0, 0.0),
            size,
            definite_height: Some(size.1),
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
    attr_width: Option<Length>,
    attr_height: Option<Length>,
    available_width: f32,
) -> (f32, f32) {
    let font_size = style.font_size;
    // CSS wins over the presentational attribute, which is only a fallback.
    let width = match style.width {
        Length::Auto => attr_width.map(|length| length.to_px(font_size, available_width)),
        length => Some(length.to_px(font_size, available_width)),
    };
    let height = match style.height {
        // A percentage *height* is a different matter from a percentage width.
        // It resolves against the containing block's height, which is `auto`
        // for nearly every box on a page of this era, and CSS 2.1 §10.5 says
        // such a percentage computes to `auto` in turn. Reading it against the
        // width instead — the only basis to hand — would stretch an image to a
        // fraction of the page's *width*, which is not a small error.
        Length::Auto => attr_height.and_then(|length| match length {
            Length::Percent(_) => None,
            length => Some(length.to_px(font_size, available_width)),
        }),
        length => Some(length.to_px(font_size, available_width)),
    };

    let (used_width, used_height) = match (width, height, intrinsic) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some((iw, ih))) if iw > 0.0 => (w, w * ih / iw),
        (None, Some(h), Some((iw, ih))) if ih > 0.0 => (h * iw / ih, h),
        (Some(w), None, None) => (w, w),
        (None, Some(h), None) => (h, h),
        (None, None, Some(size)) => size,
        (None, None, None) => BROKEN_IMAGE_SIZE,
        (Some(w), None, Some(_)) => (w, w),
        (None, Some(h), Some(_)) => (h, h),
    };

    // §10.4 again, and for a replaced box the height has to come with it: an
    // image narrowed to fit and left at its original height is a stretched
    // image, which is worse than one that overflows. The ratio used is the
    // one that was actually resolved above, so an explicit width and height
    // are honoured in proportion to each other rather than to the file's.
    //
    // This is what keeps a document rendering readable now that it shows
    // pictures: an article's photograph is routinely wider than the 42em the
    // reader sheet asks for, and nothing clips it.
    match style.max_width {
        Length::Auto => (used_width, used_height),
        bound => {
            let bound = bound.to_px(font_size, available_width);
            if used_width <= bound || used_width <= 0.0 {
                (used_width, used_height)
            } else {
                (bound, used_height * bound / used_width)
            }
        }
    }
}

/// Whether an element is a replaced element this engine lays out as a box of
/// intrinsic size rather than from its children.
fn is_replaced(doc: &Document, node: NodeId) -> bool {
    doc.element(node).is_some_and(|e| e.local_name() == "img")
}

/// Reads a presentational width/height attribute, which the era's markup used
/// far more than CSS.
/// A `width` or `height` presentational attribute, as a length.
///
/// A percentage is not a curiosity here. `<img width="100%">` is how the era
/// drew a rule across a column, how a spacer GIF held a layout open, and how
/// a banner filled the page — and this used to return `None` for one, on the
/// grounds that percentages "were rare and ambiguous". They were neither.
/// HTML's rendering rules map `width="N%"` onto `width: N%`, and the CSS 2.1
/// suite's own references lean on it: `<img src="1x1-green.png" width="100%"
/// height="50">` is how several of them draw the green bar a test must match,
/// so the *reference* came out a 50px square and the test failed for having
/// been right.
fn size_attr(doc: &Document, node: NodeId, name: &str) -> Option<Length> {
    let value = doc.element(node)?.attr(name)?.trim();
    if let Some(number) = value.strip_suffix('%') {
        let percent = number.trim().parse::<f32>().ok().filter(|v| *v >= 0.0)?;
        return Some(Length::Percent(percent));
    }
    // A bare number is pixels, and a trailing `px` is not legal in the
    // attribute — `width="100px"` is what an author writes when they mean the
    // CSS property, and browsers read it as no width at all.
    value
        .parse::<f32>()
        .ok()
        .filter(|v| *v >= 0.0)
        .map(Length::Px)
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
        || style.display == Display::InlineBlock
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

    /// What lies between two points on the page.
    ///
    /// The points are where a drag started and where it is now, in either
    /// order — a reader selecting upwards is selecting, not doing nothing. The
    /// range runs in reading order between them and covers whole lines in
    /// between, which is what selection means everywhere and is not what a
    /// rectangle between the two points would give.
    pub fn select(&self, from: (f32, f32), to: (f32, f32)) -> Selection {
        let mut lines = Vec::new();
        placed_lines(&self.root, 0.0, 0.0, &mut lines);
        // Reading order: down the page, then across. Tree order is nearly this
        // and not exactly — a float is laid out before the text beside it.
        lines.sort_by(|a, b| {
            a.top
                .partial_cmp(&b.top)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    a.origin_x
                        .partial_cmp(&b.origin_x)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        if lines.is_empty() {
            return Selection::default();
        }

        let (start, end) = {
            let (a, b) = (caret_at(&lines, from), caret_at(&lines, to));
            if a <= b { (a, b) } else { (b, a) }
        };

        let mut out = Selection::default();
        for (index, placed) in lines.iter().enumerate().take(end.line + 1).skip(start.line) {
            let text = &placed.line.text;
            let from = if index == start.line { start.offset } else { 0 };
            let to = if index == end.line {
                end.offset
            } else {
                text.len()
            };
            if from >= to {
                continue;
            }
            if let Some(rect) = span_rect(placed.line, from, to, placed.origin_x, placed.origin_y) {
                out.rects.push(rect);
            }
            if !out.text.is_empty() {
                // Where the page broke the line. The source's own newlines are
                // already gone — whitespace collapsed on the way in — so this
                // is the only place one can come from, and text copied out of
                // a paragraph reads as it looked.
                out.text.push('\n');
            }
            out.text.push_str(&text[from..to]);
        }
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

/// Text on the page between two points, and where it is.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    /// One rectangle per line the selection touches, to draw the highlight.
    pub rects: Vec<Rect>,
    /// What is selected, with a newline where the page broke the line.
    ///
    /// Copied as it reads rather than as it was written: the whitespace is
    /// already collapsed, so what comes out is what is on the screen.
    pub text: String,
}

/// One line of text on the page, with where it sits.
struct PlacedLine<'a> {
    line: &'a text::Line,
    /// Left edge of the line's own text, alignment already applied.
    origin_x: f32,
    origin_y: f32,
    top: f32,
    bottom: f32,
}

/// Where a point falls in the page's text: which line, and how far into it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Caret {
    line: usize,
    offset: usize,
}

/// Every line of text on the page, in reading order.
fn placed_lines<'a>(box_: &'a LayoutBox, x: f32, y: f32, out: &mut Vec<PlacedLine<'a>>) {
    let left = x + box_.rect.x;
    let top = y + box_.rect.y;
    if let Some(text) = &box_.text {
        let content_x = left + box_.content_origin.0;
        let content_y = top + box_.content_origin.1;
        for line in &text.lines {
            let Some(first) = line.glyphs.first() else {
                continue;
            };
            let line_top = content_y + first.y - line.baseline;
            out.push(PlacedLine {
                line,
                origin_x: content_x
                    + line_offset(box_.style.text_align, line.width, box_.content_width),
                origin_y: content_y,
                top: line_top,
                bottom: line_top + line.baseline * 1.25,
            });
        }
    }
    for child in &box_.children {
        placed_lines(child, left, top, out);
    }
}

/// Which byte of a line a point is nearest, measured to the middle of each
/// glyph so that clicking the left half of a letter puts the caret before it.
fn offset_at(placed: &PlacedLine, x: f32) -> usize {
    let line = placed.line;
    for (index, glyph) in line.glyphs.iter().enumerate() {
        let right = line.glyphs.get(index + 1).map_or(line.width, |next| next.x);
        if x < placed.origin_x + (glyph.x + right) / 2.0 {
            return glyph.start;
        }
    }
    line.text.len()
}

/// Where a point falls among `lines`, which are in reading order.
///
/// A point off the end of a line belongs to that line, and a point in the gap
/// between two lines belongs to the one above — which is what makes dragging
/// down the left-hand margin select whole lines rather than nothing.
fn caret_at(lines: &[PlacedLine], (x, y): (f32, f32)) -> Caret {
    let mut last = Caret { line: 0, offset: 0 };
    for (index, placed) in lines.iter().enumerate() {
        if y < placed.top {
            return last;
        }
        if y < placed.bottom {
            return Caret {
                line: index,
                offset: offset_at(placed, x),
            };
        }
        last = Caret {
            line: index,
            offset: placed.line.text.len(),
        };
    }
    last
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

    // Not `height.outer()` alone. That is the root box's own height, and
    // content is allowed to be taller than the box holding it — `overflow`
    // defaults to `visible` and CSS 2.1 §11.1.1 says such content is still
    // painted. Sizing the canvas by the root box cuts it off, and being cut
    // off is indistinguishable from never having been drawn: a page whose last
    // element overflowed simply lost it, silently.
    let height = height.outer().max(ink_bottom(&root, 0.0));

    Layout {
        root,
        height,
        canvas_background,
        canvas_image,
    }
}

/// The lowest point on the page anything actually reaches.
///
/// A box's own height is not that point. A block with `height: 0` and text in
/// it, a float taller than its container, a positioned box placed past the
/// bottom — each draws below the edge of what holds it, and `overflow:
/// visible` says each must still be drawn. Rectangles here are relative to the
/// parent, so the offset is carried down rather than read off the box.
///
/// Text is measured separately from the box, and that is the case that matters:
/// a box's rectangle can be shorter than the lines inside it, which is exactly
/// what `height: 0` produces.
fn ink_bottom(box_: &LayoutBox, offset_y: f32) -> f32 {
    let top = offset_y + box_.rect.y;
    let mut bottom = top + box_.rect.height;
    // `overflow: hidden` all but ends the question here (CSS 2.1 §11.1.1).
    // What hangs out of this box is clipped, and clipped content is not part
    // of the page: it cannot be scrolled to, because there is nothing to
    // scroll to.
    //
    // This is how a page gets a mile of blank space under it. A dropdown
    // written without scripting is `height: 0; overflow: hidden` until it is
    // opened, and the menu inside it is as tall as its list — so the page
    // carried hundreds of pixels of menu it never drew and could never show.
    //
    // "All but", because of what escapes: an absolutely positioned box is
    // clipped by this one only if this one is its containing block, and a
    // static box is nobody's containing block. Chromium agrees, and so does
    // the reader — a tooltip anchored to the page, inside a clipped wrapper,
    // is on the page.
    if box_.style.overflow == Overflow::Clipped {
        return bottom.max(escaping_bottom(box_, top));
    }
    if let Some(text) = &box_.text {
        bottom = bottom.max(top + box_.content_origin.1 + text.height);
    }
    for child in &box_.children {
        bottom = bottom.max(ink_bottom(child, top));
    }
    bottom
}

/// How far down content reaches that a clipping box does not clip.
///
/// An absolutely positioned box is clipped by an ancestor only when that
/// ancestor is its containing block — the nearest positioned one (§10.1). So
/// the walk goes down through *static* descendants looking for absolutely
/// positioned boxes, and stops at the first positioned one it meets: that box
/// is the containing block for everything below it, and it is inside the clip.
///
/// A positioned clipper clips everything under it and never calls this.
fn escaping_bottom(clipper: &LayoutBox, top: f32) -> f32 {
    if clipper.style.position != Position::Static {
        return f32::MIN;
    }
    let mut bottom = f32::MIN;
    for child in &clipper.children {
        bottom = bottom.max(match child.style.position {
            // Anchored outside the clip, so it is drawn and scrolled to in
            // full — and so is everything under it.
            Position::Absolute | Position::Fixed => ink_bottom(child, top),
            // Still static, so still nobody's containing block: keep looking.
            Position::Static => escaping_bottom(child, top + child.rect.y),
            // A positioned box is the containing block for anything below it,
            // and it is inside the clip.
            Position::Relative => f32::MIN,
        });
    }
    bottom
}

/// Gap between a list marker and the item's content edge.
const MARKER_GAP: f32 = 0.4;

/// A shaping width wide enough that no line ever breaks at it.
///
/// Used where the caller wants one line whatever its length, and will decide
/// for itself what to do with the part that does not fit.
const UNWRAPPED: f32 = 1.0e6;

/// Cuts a form control's label off at the edge of its box.
///
/// A control clips, and nothing else in this engine does: an over-long value
/// stops at the field's border instead of being drawn through it and across
/// whatever sits beside it. Doing it here, on the shaped label, keeps paint
/// free of a clip path it would need for this one case.
///
/// Glyphs go whole, so one straddling the edge is kept rather than cut through
/// — at a text field's size that is a fraction of a character of overhang. The
/// line's text is left intact so a search still matches what the field holds.
fn clip_label(label: &mut text::TextLayout, width: f32) {
    for line in &mut label.lines {
        line.glyphs.retain(|glyph| glyph.x < width);
        line.decorations.retain(|run| run.x < width);
        line.width = line.width.min(width);
    }
    label.width = label.width.min(width);
}

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

/// Where a line's atomic boxes go.
///
/// The box being filled, together with the two numbers needed to place
/// anything on a line inside it: where its content starts, and the width lines
/// were broken at — which is what an aligned line measures its shift against.
struct LineBoxes<'a> {
    origin: (f32, f32),
    content_width: f32,
    parent: &'a mut LayoutBox,
}

/// Inline-block boxes, already laid out, keyed by the node each belongs to.
///
/// An inline-block is a block container sized by its own content that sits on a
/// line. The line breaker cannot leave room for one without knowing how big it
/// is, and the only way to know is to lay it out — so it is laid out first, and
/// the whole box is kept rather than measured now and built again later.
type InlineBlocks = std::collections::HashMap<NodeId, LayoutBox>;

/// Lays out every inline-block among `children`, and inside any inline element
/// among them, so the line breaker has sizes to place.
///
/// Walks the same shapes [`gather_one`] does, and stops at the same places: a
/// replaced element and a form control are atomic already, and an inline-block
/// lays its own descendants out, including any inline-blocks among them.
fn layout_inline_blocks(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    children: &[NodeId],
    intrinsic: &IntrinsicSizes,
    available_width: f32,
    out: &mut InlineBlocks,
) {
    for &child in children {
        let Some(style) = styles.get(child) else {
            continue;
        };
        if style.display == Display::None || !is_inline_child(doc, styles, child, style) {
            continue;
        }
        if is_replaced(doc, child) || forms::control_of(doc, child).is_some() {
            continue;
        }
        if style.display == Display::InlineBlock {
            let box_ =
                layout_inline_block(doc, styles, fonts, child, style, intrinsic, available_width);
            out.insert(child, box_);
            continue;
        }
        layout_inline_blocks(
            doc,
            styles,
            fonts,
            doc.children(child),
            intrinsic,
            available_width,
            out,
        );
    }
}

/// Lays one inline-block out on its own, at the width §10.3.9 gives it.
///
/// The box comes back positioned within its own *margin* box — its rect already
/// offset by its left and top margins — because that is the box the line makes
/// room for, and moving it onto the line is then a single translation.
fn layout_inline_block(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    node: NodeId,
    style: &ComputedStyle,
    intrinsic: &IntrinsicSizes,
    available_width: f32,
) -> LayoutBox {
    let font_size = style.font_size;
    let margin = style.margin.left.to_px(font_size, available_width)
        + style.margin.right.to_px(font_size, available_width);

    // §10.3.9: an `auto` width shrinks to fit. As wide as the content would
    // like, no wider than the room left on the line, and never narrower than
    // the widest thing in it that cannot be broken — which is what keeps a
    // one-word label from being squeezed to nothing in a narrow column.
    //
    // `layout_block` derives an auto width from what it is given, so the room
    // it is handed *is* the answer. A declared width it works out itself, and
    // is given the real available width so percentages inside resolve against
    // the containing block rather than against the box.
    let effective = match style.width {
        Length::Auto => {
            let room = (available_width - margin).max(0.0);
            let (min, max) = subtree_widths(doc, styles, fonts, node, style, intrinsic, room, 0);
            min.max(room).min(max.max(min)) + margin
        }
        _ => available_width,
    };

    let mut holder = LayoutBox {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: effective,
            height: 0.0,
        },
        style: ComputedStyle::default(),
        text: None,
        content_origin: (0.0, 0.0),
        content_width: effective,
        children: Vec::new(),
        replaced: None,
        node: None,
    };
    let consumed = layout_block(
        doc,
        styles,
        fonts,
        node,
        style,
        intrinsic,
        0.0,
        0.0,
        effective,
        // An inline-block establishes a formatting context of its own, so no
        // float declared outside it reaches in.
        FloatContext::new(effective),
        ContainingBlock::establish((effective, 0.0)),
        &mut holder,
    );
    let mut box_ = match holder.children.pop() {
        Some(box_) => box_,
        // Unreachable: `layout_block` always pushes exactly one box. Returning
        // an empty box rather than panicking keeps a layout bug from taking the
        // whole page down with it.
        None => holder,
    };

    // The margin box is what goes on the line, so the height the line is told
    // about includes margins that do not collapse with anything — an
    // inline-block keeps its own (§8.3.1, and `keeps_its_childrens_margins`).
    box_.rect.y = consumed.margin_top;
    box_
}

/// Distance from the top of `box_` to the baseline of its last line box.
///
/// §10.8.1: an inline-block lines up with the text beside it on the baseline of
/// its own last line, not on its bottom edge — so a caption under a thumbnail
/// sits level with the sentence it is part of. `None` when there is no line box
/// to align on, in which case the caller falls back to the bottom margin edge,
/// which is the same rule and the reason an empty spacer sits *on* the line.
fn last_baseline(box_: &LayoutBox) -> Option<f32> {
    let mut found = box_.text.as_ref().and_then(|text| {
        let line = text.lines.last()?;
        Some(box_.content_origin.1 + line.y + line.baseline)
    });
    for child in &box_.children {
        // Out-of-flow boxes are not in the line-box run: an absolutely
        // positioned footnote at the bottom of one of these must not drag the
        // whole box down relative to the text beside it.
        if child.style.position.is_out_of_flow() || child.style.float != Float::None {
            continue;
        }
        if let Some(inner) = last_baseline(child) {
            found = Some(child.rect.y + inner);
        }
    }
    found
}

/// Turns the line breaker's placements into real child boxes.
///
/// An inline image is still a box: it can carry a border, padding, and a
/// background, and a broken one has to show its frame. Emitting boxes rather
/// than painting the placements directly means all of that goes through the
/// ordinary box-painting path instead of being special-cased.
fn emit_replaced_boxes(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    layout: &TextLayout,
    style: &ComputedStyle,
    blocks: &InlineBlocks,
    into: LineBoxes<'_>,
) {
    let LineBoxes {
        origin,
        content_width,
        parent,
    } = into;
    for line in &layout.lines {
        // The same shift paint applies to the line's glyphs, so a centred line
        // carries its images along with its text.
        let dx = line_offset(style.text_align, line.width, content_width);
        for placed in &line.replaced {
            let node = NodeId(placed.id);
            // An inline-block was laid out whole before the line was
            // broken; placing it is moving it, not building it again.
            if let Some(box_) = blocks.get(&node) {
                let mut box_ = box_.clone();
                box_.rect.x += origin.0 + dx + placed.x;
                box_.rect.y += origin.1 + placed.y;
                parent.children.push(box_);
                continue;
            }
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

            // A control is painted from its own border and background rather
            // than from decoded bytes, so it carries no image — and its label,
            // where it has one, is shaped into the box now that the box's
            // width is finally known.
            let control = forms::control_of(doc, node);
            let inner = (placed.width - left - right).max(0.0);
            let label = control
                .and_then(|control| Some((control, forms::label_of(doc, node, control)?)))
                .map(|(control, text)| {
                    let single_line = forms::is_single_line(doc, node, control);
                    // A list box and a textarea hold their lines apart with
                    // newlines, so the label is shaped preformatted or they
                    // would collapse into one run-on line — which is the bug
                    // that made a `<select>` read as `United KingdomFrance`
                    // before any of this existed.
                    let mut label_style = child_style.clone();
                    if !single_line {
                        label_style.white_space = WhiteSpace::Pre;
                    }
                    let runs = [InlineRun::text(text, label_style.clone())];
                    // A field that clips is shaped with nothing to break at,
                    // so the value stays on one line and is cut at the border
                    // rather than wrapping out of the box.
                    let shaping_width = if single_line { UNWRAPPED } else { inner };
                    let mut label = fonts.layout_runs(&runs, &label_style, shaping_width);
                    clip_label(&mut label, inner);
                    label
                });

            // A checked box needs a mark, and the rasteriser draws rectangles:
            // a filled inner square reads as "checked" and is what a small
            // checkbox mostly comes down to at this size anyway. The radio gets
            // the same square, which is the divergence named in `forms`.
            let checked = matches!(
                control,
                Some(forms::Control::Checkbox | forms::Control::Radio)
            ) && doc
                .element(node)
                .is_some_and(|element| element.attr("checked").is_some());
            let mut children = Vec::new();
            if checked {
                let inset = (placed.width * 0.25).max(1.0);
                let mark = ComputedStyle {
                    background_color: child_style.color,
                    ..ComputedStyle::default()
                };
                children.push(LayoutBox {
                    rect: Rect {
                        x: inset,
                        y: inset,
                        width: (placed.width - inset * 2.0).max(1.0),
                        height: (placed.height - inset * 2.0).max(1.0),
                    },
                    style: mark,
                    text: None,
                    content_origin: (0.0, 0.0),
                    content_width: 0.0,
                    children: Vec::new(),
                    replaced: None,
                    node: None,
                });
            }

            parent.children.push(LayoutBox {
                rect: Rect {
                    x: origin.0 + dx + placed.x,
                    y: origin.1 + placed.y,
                    width: placed.width,
                    height: placed.height,
                },
                style: child_style.clone(),
                text: label,
                content_origin: (left, top),
                content_width: inner,
                children,
                replaced: control.is_none().then_some(node),
                node: Some(node),
            });
        }
    }
}

/// Sorts the floats a block has to place into those declared before any in-flow
/// block child and those declared after, so each is placed at the height it
/// actually appears.
///
/// Descends through inline elements, because a float inside one belongs to this
/// block all the same — `<span><div style="float: left">…</div></span>` is
/// ordinary markup, and §9.7 makes that div block-level wherever it sits. Not
/// descending is not merely inexact: the float is reached by nothing, neither
/// gathered as an inline run nor walked as a block child, and disappears.
fn collect_floats(
    doc: &Document,
    styles: &StyleMap,
    children: &[NodeId],
    early: &mut Vec<(NodeId, ComputedStyle)>,
    late: &mut Vec<(NodeId, ComputedStyle)>,
    seen_in_flow: &mut bool,
) {
    for &child in children {
        let Some(child_style) = styles.get(child) else {
            continue;
        };
        if child_style.display == Display::None {
            continue;
        }
        if child_style.float != Float::None {
            let into = if *seen_in_flow {
                &mut *late
            } else {
                &mut *early
            };
            into.push((child, child_style.clone()));
        } else if is_inline_child(doc, styles, child, child_style) {
            // An inline-block places its own floats, in its own formatting
            // context; a form control has no children to look inside.
            if child_style.display != Display::InlineBlock
                && forms::control_of(doc, child).is_none()
            {
                collect_floats(doc, styles, doc.children(child), early, late, seen_in_flow);
            }
        } else if !child_style.display.is_table_internal() {
            *seen_in_flow = true;
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
            // A float does not split the inline box around it either: it is
            // taken out of the flow and placed against an edge, and the text
            // beside it carries on across the line it came from. §9.7 gives it
            // `display: block`, so without this every floated `<span>` inside a
            // paragraph would read as a block child and break the line.
            if style.float != Float::None {
                return false;
            }
            // An inline-block is a block container, but it is *atomic*: it goes
            // on the line whole, so a block inside one never reaches the inline
            // box around it and never splits it.
            if style.display == Display::InlineBlock {
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
    // A form control is atomic: its children are never laid out, so what they
    // are cannot decide how it is laid out. Asking `contains_block` about them
    // is not merely pointless but wrong — a `<select multiple>` gives its
    // options `display: block` so they stack inside the control, and reading
    // that as "this element contains a block" turned the whole control into a
    // block box that filled the line.
    if forms::control_of(doc, node).is_some() {
        return style.display.is_inline();
    }
    // An inline-block is atomic for the same reason a control is: what is
    // inside it is laid out by the box itself and cannot decide where the box
    // goes. It is the case that makes the difference visible, because unlike a
    // control it usually *does* hold blocks.
    if style.display == Display::InlineBlock {
        return true;
    }
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

    // Already an outer width: a collapsing table's surround is half of its
    // outermost grid lines rather than the border it declared, and only
    // `table_widths` has resolved them.
    if style.display == Display::Table {
        return table_widths(doc, styles, fonts, node, style, intrinsic, available, depth);
    }

    // The box's own inline content, then every block child, whichever is
    // widest — they stack, so the container must fit the widest of them.
    let runs = collect_inline_runs(
        doc,
        styles,
        node,
        style,
        intrinsic,
        &InlineBlocks::new(),
        available,
    );
    let (mut min, mut max) = fonts.intrinsic_widths(&runs, style);

    for &child in doc.children(node) {
        let Some(child_style) = styles.get(child) else {
            continue;
        };
        // An inline-block is measured here rather than among the runs: it is
        // atomic, so its own content decides how wide it wants to be. Taking
        // the widest rather than the sum understates a row of them, which
        // costs a column too little width and never too much.
        if child_style.display == Display::None
            || (is_inline_child(doc, styles, child, child_style)
                && child_style.display != Display::InlineBlock)
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

/// Intrinsic widths of a table's *border box*, summed across its columns.
///
/// Outer rather than content widths because the two border models surround a
/// table differently — `border-spacing` and a declared border in one, half of
/// the outermost grid lines and no padding at all in the other — and only this
/// function has resolved which.
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
    // Measured the same way it will be laid out: in the collapsing model a
    // cell's used borders are halves of the grid lines rather than what it
    // declared, and measuring it against the declared ones sizes the columns
    // for a table that is never drawn.
    let collapsed = (style.border_collapse == BorderCollapse::Collapse)
        .then(|| table::collapse_borders(doc, styles, node, style));
    let owned_grid;
    let grid = match &collapsed {
        Some(collapsed) => &collapsed.grid,
        None => {
            owned_grid = table::build_grid(doc, styles, node);
            &owned_grid
        }
    };
    if grid.columns == 0 {
        return (0.0, 0.0);
    }
    // No gap between cells in the collapsing model: they share their borders.
    let spacing = if collapsed.is_some() {
        0.0
    } else {
        style
            .border_spacing
            .to_px(style.font_size, available)
            .max(0.0)
    };

    let mut mins = vec![0.0f32; grid.columns];
    let mut maxes = vec![0.0f32; grid.columns];
    let mut spans: Vec<(usize, usize, f32, f32)> = Vec::new();

    let rows = grid.rows.len();
    for (index, row) in grid.rows.iter().enumerate() {
        for cell in &row.cells {
            let cell_style = match &collapsed {
                Some(collapsed) => table::with_reserved_borders(
                    &cell.style,
                    collapsed.reserved(
                        (index, (index + cell.rowspan).min(rows)),
                        (cell.column, (cell.column + cell.colspan).min(grid.columns)),
                    ),
                    false,
                ),
                None => cell.style.clone(),
            };
            let (min, max) = subtree_widths(
                doc,
                styles,
                fonts,
                cell.node,
                &cell_style,
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

    // What the table costs beyond its columns. In the separated model that is
    // `border-spacing` once per column boundary and once outside each end, plus
    // the table's own padding and border. In the collapsing model it is half of
    // each outermost grid line and nothing else — the table has no padding
    // there (§17.6.2), and its declared border was already spent on the
    // conflict the grid lines resolved.
    let surround = match &collapsed {
        Some(collapsed) => collapsed.half_vertical(0) + collapsed.half_vertical(grid.columns),
        None => {
            spacing * (grid.columns + 1) as f32
                + style.padding.left.to_px(style.font_size, available)
                + style.padding.right.to_px(style.font_size, available)
                + style.border.left.used_width(style.font_size)
                + style.border.right.used_width(style.font_size)
        }
    };
    (
        mins.iter().sum::<f32>() + surround,
        maxes.iter().sum::<f32>() + surround,
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
    // Generated content bracketing the holder's own. Only ever passed on the
    // first and last stretch respectively, which is what keeps a `::before`
    // from reappearing above every block child.
    generated: (Option<InlineRun>, Option<InlineRun>),
) -> f32 {
    let (lead, tail) = generated;
    // Not `pending.is_empty()` alone: an element whose content is entirely
    // block-level still gets its generated boxes, and they arrive here with
    // nothing pending beside them.
    if pending.is_empty() && lead.is_none() && tail.is_none() {
        return 0.0;
    }
    let children = std::mem::take(pending);
    let mut blocks = InlineBlocks::new();
    layout_inline_blocks(
        doc,
        styles,
        fonts,
        &children,
        intrinsic,
        content_width,
        &mut blocks,
    );
    let mut runs = Vec::new();
    runs.extend(lead);
    runs.extend(inline_runs_for(
        doc,
        styles,
        &children,
        style,
        holder,
        intrinsic,
        &blocks,
        content_width,
    ));
    runs.extend(tail);
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
            doc,
            styles,
            fonts,
            &laid_out,
            style,
            &blocks,
            LineBoxes {
                origin: (0.0, 0.0),
                content_width,
                parent: &mut anonymous,
            },
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
    // A collapsing table is not a box with a border around a grid; it *is* the
    // grid, and its own border is the outer half of the outermost grid lines
    // (§17.6.2). So the borders have to be resolved before anything measures
    // this box, and the style everything below reads is the rewritten one: half
    // widths that reserve space and paint nothing, and no padding, which the
    // collapsing model does not give a table at all.
    let collapsed = (style.display == Display::Table
        && style.border_collapse == BorderCollapse::Collapse)
        .then(|| table::collapse_borders(doc, styles, node, style));
    let collapsed_style;
    let style = match &collapsed {
        Some(collapsed) => {
            let rows = collapsed.grid.rows.len();
            let columns = collapsed.grid.columns;
            collapsed_style = table::with_reserved_borders(
                style,
                collapsed.reserved((0, rows), (0, columns)),
                true,
            );
            &collapsed_style
        }
        None => style,
    };

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
    // §10.4: the used width is clamped to `max-width`, whether it came from a
    // declared width or from filling the space available. Like `width` it is a
    // bound on the *content* box, so the surround is added back on.
    //
    // The reader sheet has asked for `max-width: 42em` since it was written and
    // has been getting the whole window: an unimplemented property is not an
    // ignored declaration, it is a measure nobody chose. A 42em column is most
    // of what makes a document rendering readable rather than merely plain.
    let outer_width = match style.max_width {
        Length::Auto => outer_width,
        bound => outer_width.min(bound.to_px(font_size, available_width) + surround),
    };
    // §10.4 again, and after the maximum rather than before it: where the two
    // bounds cross, the minimum wins. A box told to be at most 100px and at
    // least 200px is 200px wide — which looks like a mistake in the stylesheet
    // and is what the specification asks for, because the author who wrote a
    // floor meant the content to fit.
    let outer_width = match style.min_width {
        Length::Auto => outer_width,
        bound => outer_width.max(bound.to_px(font_size, available_width) + surround),
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
    let mut blocks = InlineBlocks::new();
    let runs = if all_inline {
        layout_inline_blocks(
            doc,
            styles,
            fonts,
            doc.children(node),
            intrinsic,
            content_width,
            &mut blocks,
        );
        collect_inline_runs(doc, styles, node, style, intrinsic, &blocks, content_width)
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
    collect_floats(
        doc,
        styles,
        doc.children(node),
        &mut early,
        &mut late,
        &mut seen_in_flow,
    );
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
            doc,
            styles,
            fonts,
            &layout,
            style,
            &blocks,
            LineBoxes {
                origin: (padding_left + border_left, padding_top + border_top),
                content_width,
                parent: &mut box_,
            },
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
            collapsed.as_ref(),
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

        // §17.4: a caption is *not* inside the table's border box. It is a
        // sibling of it — as wide as the table, above or below it — which is
        // why a bordered table does not draw its border around its own
        // heading. Laid out here rather than by the child walk below, because
        // this branch returns before that walk ever runs: that is exactly how
        // captions came to be dropped altogether.
        //
        // Placed only once the table's width and x are settled, since both are
        // what the caption is measured and positioned against, and a
        // shrink-to-fit table does not know either until now.
        let mut captions: Vec<LayoutBox> = Vec::new();
        let mut above = 0.0;
        let mut below = 0.0;
        for top in [true, false] {
            for (caption, caption_style) in table::captions(doc, styles, node) {
                if (caption_style.caption_side == CaptionSide::Top) != top {
                    continue;
                }
                // As wide as the table, but never narrower than the caption's
                // longest word: a one-column table would otherwise wrap its
                // heading to a letter a line. Browsers widen the wrapper box
                // for this; with no wrapper here the caption simply overhangs,
                // which is the same picture for everything but the table's own
                // horizontal placement.
                let (minimum, _) = subtree_widths(
                    doc,
                    styles,
                    fonts,
                    caption,
                    &caption_style,
                    intrinsic,
                    box_.rect.width,
                    0,
                );
                let width = box_.rect.width.max(minimum);
                // Top captions stack down from the table's top edge; bottom
                // ones from below it, by which point the table has already been
                // moved down past the top ones.
                let at = if top {
                    box_.rect.y + above
                } else {
                    box_.rect.y + box_.rect.height + below
                };
                let mut holder = LayoutBox {
                    rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        width,
                        height: 0.0,
                    },
                    style: caption_style.clone(),
                    text: None,
                    content_origin: (0.0, 0.0),
                    content_width: width,
                    children: Vec::new(),
                    replaced: None,
                    node: None,
                };
                let taken = layout_block(
                    doc,
                    styles,
                    fonts,
                    caption,
                    &caption_style,
                    intrinsic,
                    box_.rect.x,
                    at,
                    width,
                    FloatContext::new(width),
                    ContainingBlock::viewport(width, width),
                    &mut holder,
                );
                if let Some(caption_box) = holder.children.pop() {
                    if top {
                        above += taken.outer();
                    } else {
                        below += taken.outer();
                    }
                    captions.push(caption_box);
                }
            }
            // The table gives up the space its top captions took. Its children
            // are positioned relative to it, so they come along.
            if top {
                box_.rect.y += above;
            }
        }

        let consumed = Consumed {
            // The captions are part of what this box occupies even though they
            // sit outside its border box, or the content after the table would
            // be laid out on top of a bottom caption.
            height: above + box_.rect.height + below,
            margin_top,
            margin_bottom: style.margin.bottom.to_px(font_size, available_width),
            // A replaced box has content by definition, and a table is a
            // formatting context of its own; neither can have a margin pass
            // through it.
            collapses_through: false,
        };
        parent.children.push(box_);
        parent.children.extend(captions);
        return consumed;
    }

    let mut absolutes: Vec<(NodeId, ComputedStyle, f32)> = Vec::new();
    let mut cursor_y = padding_top + border_top + content_height;
    // Inline children seen since the last block child. Flushed as an anonymous
    // box when a block child arrives, and again at the end.
    // This box's own height, where it has a definite one — what a child's
    // percentage height resolves against (§10.5). A percentage here is itself
    // definite only if the chain above it was, which is what stops a
    // percentage resolving against nothing at all.
    let own_definite_height = match style.height {
        Length::Auto => None,
        Length::Percent(percent) => containing
            .definite_height
            .map(|basis| basis * percent / 100.0),
        length => Some(length.to_px(style.font_size, available_width)),
    };
    // Clamped, because what a child resolves its percentage against is the
    // *used* height and not the declared one. A box with `height: 4em;
    // max-height: 2em` is two ems tall, and a child asking for 50% of it wants
    // one em rather than two — `absolute-non-replaced-max-001` in the suite is
    // exactly this, with a black square that comes out four times too big.
    //
    // §10.7's order, the maximum and then the minimum, so a box given both
    // takes the minimum. Both bound the content box, which is what this is.
    // A percentage bound resolves against a height that is not known here, so
    // those are no bound rather than a guess — the same rule the used height
    // itself follows a few hundred lines below.
    let own_definite_height = own_definite_height.map(|height| {
        let mut height = height;
        if let Length::Px(_) | Length::Em(_) = style.max_height {
            height = height.min(style.max_height.to_px(style.font_size, 0.0));
        }
        if let Length::Px(_) | Length::Em(_) = style.min_height {
            height = height.max(style.min_height.to_px(style.font_size, 0.0));
        }
        height
    });

    let mut pending: Vec<NodeId> = Vec::new();
    // §12.1: the generated boxes bracket the element's content, so they go on
    // the first and last stretch of it and nowhere else. `all_inline` has its
    // own path through `collect_inline_runs`; these are for the mixed case,
    // where content is collected in stretches between the block children.
    let mut lead = (!all_inline)
        .then(|| generated_run(styles, node, PseudoElement::Before))
        .flatten();
    let tail = (!all_inline)
        .then(|| generated_run(styles, node, PseudoElement::After))
        .flatten();
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
            (lead.take(), None),
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
        // A normal-flow child's containing block is *this* box, so the
        // definite height it may resolve a percentage against is this box's,
        // not an ancestor's. Carrying the ancestor's down instead would let
        // `height: 50%` find a basis through a chain of auto-height parents
        // that CSS 2.1 says stops at the first one.
        let child_containing = containing
            .descend(padding_left + border_left, cursor_y)
            .with_definite_height(own_definite_height);
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

    // Floats declared after the last block child. Nothing further comes along
    // to trigger the drain inside the loop, so without this they are laid out
    // nowhere and never appear — a float at the end of a container simply
    // vanished, which is most of what `float: left` is used for on a page that
    // ends with one.
    //
    // Placed before the trailing inline flush so that text after them wraps
    // around them, which is the ordinary case. A float written *after* some
    // trailing text is placed a little too high by this, since the drain
    // inside the loop only ever compares against block children; that is the
    // same granularity the rest of this function works at, and it is a far
    // smaller error than not drawing the float at all.
    for (float_node, float_style) in std::mem::take(&mut late) {
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
        // `lead` is still here when every child was block-level and the loop
        // never flushed anything, which is the case that would otherwise drop
        // a `::before` entirely.
        (lead.take(), tail),
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
    let surround = padding_top + padding_bottom + border_top + border_bottom;
    box_.rect.height = match style.height {
        Length::Auto => content_end,
        // §10.5: a percentage height resolves against the containing block's
        // height, and where that height is not itself definite the percentage
        // computes to `auto`. Resolving it against the *width* instead — which
        // is what `to_px` does when handed `available_width`, and what this
        // used to do — is how `height: 100%` on Wikipedia's logo became a
        // 1000-pixel-tall empty box that pushed the entire article off the
        // screen. A percentage of a width is not a height.
        Length::Percent(percent) => match containing.definite_height {
            Some(basis) => basis * percent / 100.0 + surround,
            None => content_end,
        },
        length => length.to_px(font_size, available_width) + surround,
    };
    // §10.7: the used height is capped at `max-height` first, and raised to
    // `min-height` after — that order is the spec's, and it is what makes
    // `min-height` win when an author asks for both at once. Content taller
    // than the cap overflows rather than being clipped, which is the same
    // thing this engine does everywhere else and is why the canvas is sized
    // from what is drawn rather than from the boxes.
    //
    // A percentage is treated as no bound, for the reason given below.
    if let Length::Px(_) | Length::Em(_) = style.max_height {
        let ceiling = style.max_height.to_px(font_size, 0.0)
            + padding_top
            + padding_bottom
            + border_top
            + border_bottom;
        box_.rect.height = box_.rect.height.min(ceiling);
    }

    // The used height is then raised to `min-height`, whether it came
    // from a declared height or from the content. Like `height` it bounds the
    // *content* box, so the surround is added back on — the same shape as
    // `max-width` above, and wrong in the same visible way if it is not.
    //
    // A percentage resolves against the containing block's *height*, which is
    // not known here and is `auto` for most of the era's markup anyway; those
    // are treated as no bound rather than guessed at.
    if let Length::Px(_) | Length::Em(_) = style.min_height {
        let floor = style.min_height.to_px(font_size, 0.0)
            + padding_top
            + padding_bottom
            + border_top
            + border_bottom;
        box_.rect.height = box_.rect.height.max(floor);
    }

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
                let runs = collect_inline_runs(
                    doc,
                    styles,
                    child,
                    &child_style,
                    intrinsic,
                    &InlineBlocks::new(),
                    content_width,
                );
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
    collapsed: Option<&table::Collapsed>,
    intrinsic: &IntrinsicSizes,
    x: f32,
    y: f32,
    available_width: f32,
    parent: &mut LayoutBox,
) -> (f32, f32) {
    // The grid was already built to resolve the borders; building it a second
    // time would read the same DOM to the same answer.
    let owned_grid;
    let grid = match collapsed {
        Some(collapsed) => &collapsed.grid,
        None => {
            owned_grid = table::build_grid(doc, styles, node);
            &owned_grid
        }
    };
    if grid.columns == 0 {
        return (0.0, 0.0);
    }
    // `border-spacing` is the table's own, not a constant: `cellspacing="0"`
    // is how a table used for page layout closed the seams between its cells.
    // It does not apply in the collapsing model, where there is no gap for it
    // to describe — the cells share their borders rather than being separated.
    let spacing = if collapsed.is_some() {
        0.0
    } else {
        style
            .border_spacing
            .to_px(style.font_size, available_width)
            .max(0.0)
    };

    // In the collapsing model a cell's used border is half of the grid line it
    // sits against, whatever it declared — the declared value was spent on
    // winning (or losing) the conflict. Everything downstream measures and lays
    // out against this style rather than the cascaded one.
    let rows = grid.rows.len();
    let effective = |cell: &table::Cell, row: usize| -> ComputedStyle {
        match collapsed {
            Some(collapsed) => table::with_reserved_borders(
                &cell.style,
                // Clamped, because a `rowspan` may run off the bottom of the
                // table and the grid line it would name does not exist.
                collapsed.reserved(
                    (row, (row + cell.rowspan).min(rows)),
                    (cell.column, (cell.column + cell.colspan).min(grid.columns)),
                ),
                false,
            ),
            None => cell.style.clone(),
        }
    };

    // Intrinsic widths per column, from the cells that span exactly one.
    let mut mins = vec![0.0f32; grid.columns];
    let mut maxes = vec![0.0f32; grid.columns];
    let mut declared = vec![false; grid.columns];
    let mut spans: Vec<(usize, usize, f32, f32)> = Vec::new();

    for (index, row) in grid.rows.iter().enumerate() {
        for cell in &row.cells {
            // The whole subtree, not just the cell's text: a cell holding a
            // nested table measures as nothing otherwise, and its column
            // collapses to zero width.
            let cell_style = effective(cell, index);
            let (min, max) = subtree_widths(
                doc,
                styles,
                fonts,
                cell.node,
                &cell_style,
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
            let cell_style = effective(cell, index);
            let mut holder = LayoutBox {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width,
                    height: 0.0,
                },
                style: cell_style.clone(),
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
                &cell_style,
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
        // A row's edges are grid lines in the collapsing model, and whatever
        // border it declared has already been offered to them. Drawing it here
        // as well would put a second line beside the one it helped decide.
        let mut row_style = row.style.clone();
        if collapsed.is_some() {
            row_style.border = css::style::Borders::default();
        }
        parent.children.push(LayoutBox {
            rect: Rect {
                x: x + spacing,
                y: tops[index],
                width: row_width,
                height: heights[index],
            },
            style: row_style,
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

    if let Some(collapsed) = collapsed {
        // Last, so the borders draw over the cells and rows whose halves of
        // them were only ever reserved space.
        emit_collapsed_borders(collapsed, x, y, &widths, &heights, parent);
    }

    // The table's own width: its columns, the gaps between them, and the gap
    // outside the first and last.
    let width = widths.iter().sum::<f32>() + spacing * (grid.columns + 1) as f32;
    (width, cursor_y - y)
}

/// Emits one box per resolved grid line segment, drawn centred on the line.
///
/// The cells reserved half of each border and painted none of it, so this is
/// where a collapsed border actually becomes visible. Each segment is a box
/// whose rect *is* the border: one side set to the resolved style at the full
/// resolved width, the other three left at `none`. Which side is not a detail —
/// `inset`, `outset`, `groove` and `ridge` are lit from above and to the left,
/// so the winning border is drawn as the edge it was declared on, and a cell's
/// `border-bottom: inset` looks like a bottom border rather than like the top
/// border of the cell underneath.
///
/// `widths` and `heights` are the distances between grid line centres, so
/// `x` — the table's content origin — is the centre of vertical grid line 0.
fn emit_collapsed_borders(
    collapsed: &table::Collapsed,
    x: f32,
    y: f32,
    widths: &[f32],
    heights: &[f32],
    parent: &mut LayoutBox,
) {
    // Centres of the grid lines, which is what every segment is measured from.
    let mut line_x = Vec::with_capacity(widths.len() + 1);
    let mut cursor = x;
    line_x.push(cursor);
    for width in widths {
        cursor += width;
        line_x.push(cursor);
    }
    let mut line_y = Vec::with_capacity(heights.len() + 1);
    let mut cursor = y;
    line_y.push(cursor);
    for height in heights {
        cursor += height;
        line_y.push(cursor);
    }

    let segment = |rect: Rect, border: &table::Candidate| {
        let side = css::style::BorderSide {
            width: Length::Px(border.width),
            style: border.style,
            color: Some(border.color),
        };
        let mut style = ComputedStyle::default();
        match border.edge {
            table::BorderEdge::Top => style.border.top = side,
            table::BorderEdge::Right => style.border.right = side,
            table::BorderEdge::Bottom => style.border.bottom = side,
            table::BorderEdge::Left => style.border.left = side,
        }
        // `used_width` resolves `em` against the font size, and these widths
        // are already pixels — but the default is 16px and a stray `em` here
        // would scale them, so it is pinned rather than left to chance.
        style.font_size = 1.0;
        LayoutBox {
            rect,
            style,
            text: None,
            content_origin: (0.0, 0.0),
            content_width: rect.width,
            children: Vec::new(),
            replaced: None,
            node: None,
        }
    };

    // Every segment runs the full way over the crossings at both of its ends,
    // so a corner is always covered by both of the borders that meet there and
    // never by neither — a gap at every crossing is what "undefined" looks
    // like if nothing decides. CSS 2.1 does leave the corner undefined, so what
    // decides it here is width: the segments are drawn narrowest first, and the
    // wider border takes the corner. That is the rule that matches what a
    // reader expects, because the thick border is the one they are looking at:
    // a 12px rule crossed by a 1px one should not come away notched.
    let mut segments: Vec<(f32, LayoutBox)> = Vec::new();
    for (line, borders) in collapsed.horizontal.iter().enumerate() {
        let Some(&centre) = line_y.get(line) else {
            continue;
        };
        for (column, border) in borders.iter().enumerate() {
            let (Some(border), Some(&left), Some(&right)) =
                (border.as_ref(), line_x.get(column), line_x.get(column + 1))
            else {
                continue;
            };
            let left = left - collapsed.half_vertical(column);
            let right = right + collapsed.half_vertical(column + 1);
            segments.push((
                border.width,
                segment(
                    Rect {
                        x: left,
                        y: centre - border.width / 2.0,
                        width: right - left,
                        height: border.width,
                    },
                    border,
                ),
            ));
        }
    }
    for (line, borders) in collapsed.vertical.iter().enumerate() {
        let Some(&centre) = line_x.get(line) else {
            continue;
        };
        for (row, border) in borders.iter().enumerate() {
            let (Some(border), Some(&top), Some(&bottom)) =
                (border.as_ref(), line_y.get(row), line_y.get(row + 1))
            else {
                continue;
            };
            let top = top - collapsed.half_horizontal(row);
            let bottom = bottom + collapsed.half_horizontal(row + 1);
            segments.push((
                border.width,
                segment(
                    Rect {
                        x: centre - border.width / 2.0,
                        y: top,
                        width: border.width,
                        height: bottom - top,
                    },
                    border,
                ),
            ));
        }
    }
    // A stable sort, so two borders of the same width settle it the same way
    // every time: the vertical wins, because it was collected second. Nothing
    // about a rendering this engine produces may depend on which run it is.
    segments.sort_by(|a, b| a.0.total_cmp(&b.0));
    parent
        .children
        .extend(segments.into_iter().map(|(_, box_)| box_));
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
    blocks: &InlineBlocks,
    available_width: f32,
) -> Vec<InlineRun> {
    let mut runs = Vec::new();
    // §12.1: `::before` and `::after` are boxes at the very start and very end
    // of the element's content, so they bracket the children rather than
    // joining them. Done here rather than in `inline_runs_for` because that
    // one also collects a *stretch* of inline children between two blocks,
    // and a pseudo-element attached to every stretch would appear several
    // times over.
    if let Some(before) = generated_run(styles, node, PseudoElement::Before) {
        runs.push(before);
    }
    runs.extend(inline_runs_for(
        doc,
        styles,
        doc.children(node),
        inherited,
        node,
        intrinsic,
        blocks,
        available_width,
    ));
    if let Some(after) = generated_run(styles, node, PseudoElement::After) {
        runs.push(after);
    }
    runs
}

/// The inline run for one of an element's generated boxes, if it has one.
///
/// A style in the pseudo table means the cascade gave it `content`, which is
/// what decides whether the box exists at all — so there is nothing to check
/// here beyond whether the style is present.
fn generated_run(styles: &StyleMap, node: NodeId, which: PseudoElement) -> Option<InlineRun> {
    let style = styles.pseudo(node, which)?;
    let content = style.content.clone()?;
    // A column box renders no content — §17.2 gives `table-column` and
    // `table-column-group` a box that sizes a column and draws its own
    // background, and nothing inside it. So a pseudo-element given one of
    // those displays has its content dropped rather than shown somewhere
    // else, which is what the suite checks by putting the word FAIL in it.
    let boxless = matches!(style.display, Display::None | Display::TableColumn);
    (!boxless).then(|| InlineRun::text(content, style.clone()))
}

/// Collects inline runs from a specific list of siblings.
///
/// Taking a slice rather than a parent is what lets a block container with
/// mixed content lay out each stretch of inline children where it actually
/// sits, instead of hoisting every one of them above the block children.
#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn inline_runs_for(
    doc: &Document,
    styles: &StyleMap,
    children: &[NodeId],
    inherited: &ComputedStyle,
    holder: NodeId,
    intrinsic: &IntrinsicSizes,
    blocks: &InlineBlocks,
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
            blocks,
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
    blocks: &InlineBlocks,
    available_width: f32,
    out: &mut Vec<InlineRun>,
) {
    if let Some(text) = doc.text(child) {
        // Text belongs to the nearest element that wraps it, not to the text
        // node: `<a>go</a>` must hit the anchor, which is what has the href.
        //
        // §16.5's transform is applied here, before shaping, because it changes
        // how wide the text is — uppercase is wider than what it replaces in
        // every face bundled here, and a line measured lowercase would wrap in
        // the wrong place and then be drawn in capitals over the top.
        let transformed = inherited.text_transform.apply(text);
        out.push(InlineRun::text(transformed, inherited.clone()).from_element(holder.0));
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
    // A form control is the same shape of thing: a box on the line with no
    // inline content to look inside. It differs only in where its picture comes
    // from — its own border and background rather than decoded bytes — which is
    // settled when the box is emitted, not here.
    let control = forms::control_of(doc, child);
    if is_replaced(doc, child) || control.is_some() {
        let (width, height) = match control {
            Some(control) => {
                let label = forms::label_of(doc, child, control);
                let (w, h) = forms::intrinsic_size(doc, child, style, control, label.as_deref());
                // A declared width or height still wins, as it does for an
                // image: `<input style="width: 300px">` is a wide field.
                match (style.width, style.height) {
                    (Length::Auto, Length::Auto) => (w, h),
                    (Length::Auto, height) => (w, height.to_px(style.font_size, h)),
                    (width, Length::Auto) => (width.to_px(style.font_size, available_width), h),
                    (width, height) => (
                        width.to_px(style.font_size, available_width),
                        height.to_px(style.font_size, h),
                    ),
                }
            }
            None => replaced_size(
                style,
                intrinsic.get(&child).copied(),
                size_attr(doc, child, "width"),
                size_attr(doc, child, "height"),
                available_width,
            ),
        };
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
                baseline: height + vertical,
            },
            style.clone(),
        ));
        return;
    }

    // An inline-block was laid out before the line was built, so what is left
    // is to reserve its margin box and record where it aligns. It is asked
    // *after* the replaced branch above, because `display: inline-block` on an
    // image does not make it a block container: it is still sized from its own
    // pixels, and routing it here instead made it disappear.
    //
    // Absent from the map means this pass is measuring rather than laying out —
    // `subtree_widths` has no boxes to hand out and measures inline-blocks
    // itself.
    if style.display == Display::InlineBlock {
        if let Some(box_) = blocks.get(&child) {
            let font_size = style.font_size;
            let margin_left = style.margin.left.to_px(font_size, available_width);
            let margin_right = style.margin.right.to_px(font_size, available_width);
            let margin_bottom = style.margin.bottom.to_px(font_size, available_width);
            let height = box_.rect.y + box_.rect.height + margin_bottom;
            out.push(InlineRun::replaced(
                text::ReplacedInline {
                    id: child.0,
                    width: box_.rect.width + margin_left + margin_right,
                    height,
                    // §10.8.1: on the baseline of its own last line, or on its
                    // bottom margin edge when it has no line to offer.
                    baseline: last_baseline(box_).map_or(height, |baseline| box_.rect.y + baseline),
                },
                style.clone(),
            ));
        }
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
            blocks,
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

    /// A page, its layout, and a way to select across it.
    fn page_for(html: &str, width: f32) -> (Document, StyleMap, Layout) {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css::ua::UA_STYLESHEET)];
        let styles = css::cascade::cascade(&doc, &sheets);
        let mut fonts = FontStore::new();
        let layout = layout(&doc, &styles, &mut fonts, &Default::default(), width);
        (doc, styles, layout)
    }

    #[test]
    fn a_drag_across_a_line_selects_what_it_crossed() {
        let (_, _, out) = page_for("<body><p>hello there world</p></body>", 800.0);
        let all = out.select((0.0, 0.0), (800.0, 10_000.0));
        assert_eq!(all.text, "hello there world");

        // Halfway across the line it actually drew, which must be some of it
        // and not all of it.
        let line = all.rects[0];
        let half = out.select(
            (0.0, 0.0),
            (line.x + line.width / 2.0, line.y + line.height / 2.0),
        );
        assert!(
            all.text.starts_with(&half.text) && half.text.len() < all.text.len(),
            "{:?} is not a prefix of {:?}",
            half.text,
            all.text
        );
        assert!(!half.text.is_empty(), "nothing was selected");
    }

    #[test]
    fn dragging_backwards_selects_the_same_thing() {
        // A reader selecting upwards is selecting, not doing nothing.
        let (_, _, out) = page_for("<body><p>hello there world</p></body>", 800.0);
        let forwards = out.select((0.0, 0.0), (800.0, 10_000.0));
        let backwards = out.select((800.0, 10_000.0), (0.0, 0.0));

        assert_eq!(forwards.text, backwards.text);
        assert_eq!(forwards.rects.len(), backwards.rects.len());
    }

    #[test]
    fn a_selection_across_lines_covers_the_ones_in_between_whole() {
        // Not the rectangle between the two points: selection means everything
        // in reading order, which is what makes dragging down the left margin
        // take whole lines.
        let (_, _, out) = page_for(
            "<body><p>first line here</p><p>second line here</p><p>third line here</p></body>",
            800.0,
        );
        let all = out.select((0.0, 0.0), (800.0, 10_000.0));
        assert_eq!(
            all.text,
            "first line here\nsecond line here\nthird line here"
        );
        assert_eq!(all.rects.len(), 3, "one rectangle per line");
    }

    #[test]
    fn a_line_break_on_the_page_is_a_line_break_in_what_is_copied() {
        // The source's own newlines are gone by now — whitespace collapsed on
        // the way in — so a wrapped paragraph copied out reads as it looked.
        let (_, _, out) = page_for(
            &format!("<body><p>{}</p></body>", "a word ".repeat(40)),
            200.0,
        );
        let all = out.select((0.0, 0.0), (800.0, 10_000.0));

        assert!(all.text.contains('\n'), "a wrapped paragraph came out flat");
        assert!(all.rects.len() > 1, "one rectangle for several lines");
    }

    #[test]
    fn a_click_past_the_middle_of_a_letter_takes_the_letter() {
        // Where the caret goes decides which letter a drag includes, and
        // measuring to the *start* of each glyph rather than its middle loses
        // the one the pointer is plainly on top of.
        let (_, _, out) = page_for("<body><p>abcdef</p></body>", 800.0);
        let line = out.select((0.0, 0.0), (800.0, 10_000.0)).rects[0];
        let letter = line.width / 6.0;
        let middle = line.y + line.height / 2.0;

        // Just past the middle of the first letter: that letter is in.
        let over = out.select((line.x, middle), (line.x + letter * 0.6, middle));
        assert_eq!(over.text, "a");
        // Just short of it: nothing yet.
        let under = out.select((line.x, middle), (line.x + letter * 0.4, middle));
        assert_eq!(under.text, "");
    }

    #[test]
    fn dragging_down_the_margin_takes_whole_lines() {
        // A point in the gap between two lines belongs to the one above, which
        // is what makes a drag down the left-hand margin select the lines it
        // passes rather than nothing at all.
        let (_, _, out) = page_for(
            "<body><p>first line here</p><p>second line here</p><p>third line here</p></body>",
            800.0,
        );
        let rects = out.select((0.0, 0.0), (800.0, 10_000.0)).rects;
        // Down the far-left edge, stopping between the second line and the
        // third.
        let gap = rects[1].y + rects[1].height + 1.0;
        let down = out.select((0.0, 0.0), (0.0, gap));

        assert_eq!(down.text, "first line here\nsecond line here");
    }

    #[test]
    fn selecting_nothing_selects_nothing() {
        let (_, _, out) = page_for("<body><p>hello</p></body>", 800.0);
        let empty = out.select((0.0, 0.0), (0.0, 0.0));

        assert_eq!(empty.text, "");
        assert!(empty.rects.is_empty());
    }

    #[test]
    fn a_page_with_no_text_has_nothing_to_select() {
        let (_, _, out) = page_for("<body><hr></body>", 800.0);

        assert_eq!(out.select((0.0, 0.0), (800.0, 800.0)), Selection::default());
    }
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
    fn generated_content_brackets_the_elements_own_content() {
        let rendered = run(
            "<body><p>middle</p></body>",
            "body { margin: 0 } p::before { content: \"A\" } p::after { content: \"Z\" }",
            600.0,
        );
        let text = content_boxes(&rendered)
            .into_iter()
            .filter_map(|b| b.text.as_ref())
            .flat_map(|t| t.lines.iter().map(|line| line.text.clone()))
            .collect::<String>();
        assert_eq!(text.trim(), "AmiddleZ");
    }

    #[test]
    fn generated_content_appears_once_around_mixed_content() {
        // The element's inline children are collected in stretches between its
        // block children. Attaching the generated boxes to each stretch rather
        // than to the element would repeat them.
        let rendered = run(
            "<body><div>one<p>block</p>two</div></body>",
            "body { margin: 0 } div::before { content: \"A\" }",
            600.0,
        );
        let count = content_boxes(&rendered)
            .into_iter()
            .filter_map(|b| b.text.as_ref())
            .flat_map(|t| t.lines.iter())
            .filter(|line| line.text.contains('A'))
            .count();
        assert_eq!(count, 1, "the generated box was emitted {count} times");
    }

    #[test]
    fn a_column_display_generates_no_content_box() {
        // §17.2: a column box sizes a column and draws its own background,
        // and renders nothing inside it.
        for display in ["table-column", "table-column-group", "none"] {
            let css = format!(
                "body {{ margin: 0 }} p::before {{ content: \"FAIL\"; display: {display} }}"
            );
            let rendered = run("<body><p>ok</p></body>", &css, 600.0);
            let text = content_boxes(&rendered)
                .into_iter()
                .filter_map(|b| b.text.as_ref())
                .flat_map(|t| t.lines.iter().map(|line| line.text.clone()))
                .collect::<String>();
            assert!(!text.contains("FAIL"), "{display} drew its content");
        }
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

    /// The body's own children, which is the one coordinate space in which a
    /// table and its caption can be compared: every other box in the tree is
    /// positioned relative to its parent.
    fn siblings(rendered: &Rendered) -> &[LayoutBox] {
        &rendered
            .layout
            .root
            .children
            .first()
            .expect("body box")
            .children
    }

    /// The table box among the body's children.
    fn table_of(rendered: &Rendered) -> &LayoutBox {
        siblings(rendered)
            .iter()
            .find(|b| b.style.display == Display::Table)
            .expect("a table box beside the caption")
    }

    /// The table box of a rendered page.
    fn table_box(rendered: &Rendered) -> &LayoutBox {
        content_boxes(rendered)
            .into_iter()
            .find(|b| b.style.display == Display::Table)
            .expect("a table box")
    }

    #[test]
    fn a_percentage_height_against_an_auto_parent_is_auto() {
        // §10.5. The parent's height depends on its content, so there is
        // nothing for the percentage to be a percentage *of*, and it computes
        // to `auto`. Resolving it against the width instead is how
        // `height: 100%` on Wikipedia's logo became a 1000-pixel empty box
        // that pushed the article off the screen.
        let rendered = run(
            "<body><div id=outer><div id=inner>x</div></div></body>",
            "body { margin: 0 } #inner { height: 100% }",
            1000.0,
        );
        let inner = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.text.is_some())
            .expect("the inner box");
        // Not merely "small": `auto` means the content's height, so a box
        // with one line in it is one line tall. Collapsing it to zero would
        // satisfy a looser assertion and lose the text.
        let line = run("<body><div>x</div></body>", "body { margin: 0 }", 1000.0);
        assert_eq!(
            inner.rect.height,
            content_boxes(&line)[0].rect.height,
            "a percentage against an auto parent should be the content height"
        );
    }

    #[test]
    fn a_percentage_height_chains_through_a_parent_that_has_one() {
        // The middle box's own height is a percentage, and a definite one,
        // so the inner box resolves against *it* rather than finding nothing.
        let rendered = run(
            "<body><div id=outer><div id=mid><div id=inner>x</div></div></div></body>",
            "body { margin: 0 } #outer { height: 400px } #mid { height: 50% } \
             #inner { height: 50% }",
            1000.0,
        );
        let inner = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.text.is_some())
            .expect("the inner box");
        assert_eq!(inner.rect.height, 100.0, "half of half of 400");
    }

    #[test]
    fn a_percentage_height_resolves_against_a_parent_that_has_one() {
        let rendered = run(
            "<body><div id=outer><div id=inner>x</div></div></body>",
            "body { margin: 0 } #outer { height: 400px } #inner { height: 50% }",
            1000.0,
        );
        let inner = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.text.is_some())
            .expect("the inner box");
        assert_eq!(inner.rect.height, 200.0);
    }

    #[test]
    fn a_definite_height_does_not_reach_past_an_auto_parent() {
        // The chain stops at the first auto ancestor: the inner box's
        // containing block is the middle one, which has no definite height of
        // its own, so the outer 400px is not what 50% means here.
        let rendered = run(
            "<body><div id=outer><div id=mid><div id=inner>x</div></div></div></body>",
            "body { margin: 0 } #outer { height: 400px } #inner { height: 50% }",
            1000.0,
        );
        let inner = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.text.is_some())
            .expect("the inner box");
        assert!(
            inner.rect.height < 100.0,
            "the percentage found a basis through an auto parent: {}",
            inner.rect.height
        );
    }

    #[test]
    fn a_percentage_resolves_against_the_height_a_bound_left_behind() {
        // What a child resolves against is the *used* height, not the declared
        // one. This only became reachable when `max-height` arrived: a box
        // told `height: 400px; max-height: 200px` is 200 tall, and a child
        // asking for half of it wants 100 rather than 200.
        //
        // `absolute-non-replaced-max-001` in the suite is exactly this, and
        // caught it — the black square came out four times its area.
        let rendered = run(
            "<body><div id=outer><div id=inner>x</div></div></body>",
            "body { margin: 0 } #outer { height: 400px; max-height: 200px } \
             #inner { height: 50% }",
            1000.0,
        );
        let inner = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.text.is_some())
            .expect("the inner box");

        assert_eq!(inner.rect.height, 100.0);
    }

    #[test]
    fn a_percentage_resolves_against_a_floor_the_same_way() {
        // §10.7's other half, and the order matters: the maximum is applied
        // first and the minimum second, so a box given both takes the minimum
        // — and a child measures itself against that.
        let rendered = run(
            "<body><div id=outer><div id=inner>x</div></div></body>",
            "body { margin: 0 } #outer { height: 50px; max-height: 100px; min-height: 300px } \
             #inner { height: 50% }",
            1000.0,
        );
        let inner = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.text.is_some())
            .expect("the inner box");

        assert_eq!(inner.rect.height, 150.0);
    }

    #[test]
    fn a_negative_height_is_invalid_and_leaves_the_earlier_one_standing() {
        // The earlier declaration has to be one the invalid value would
        // visibly replace: with `height: 0` first, dropping and accepting
        // both land on zero and the test cannot tell them apart.
        for bad in ["-1%", "-1px", "-1em"] {
            let css = format!("body {{ margin: 0 }} div {{ height: 50px; height: {bad} }}");
            let rendered = run("<body><div></div></body>", &css, 1000.0);
            assert_eq!(
                content_boxes(&rendered)[0].rect.height,
                50.0,
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn min_height_raises_a_box_that_would_be_shorter() {
        // §10.7, and it bounds the content box like `height` does — so the
        // padding and border are added back on rather than absorbed, which is
        // the same shape as `max-width` and visibly wrong if it is not.
        let rendered = run(
            "<body><div>short</div></body>",
            "body { margin: 0 } div { min-height: 120px; padding: 10px; border: 5px solid }",
            600.0,
        );
        let box_ = siblings(&rendered).first().expect("the div");
        assert!(
            (box_.rect.height - (120.0 + 20.0 + 10.0)).abs() < 0.01,
            "got {:?}",
            box_.rect
        );
    }

    #[test]
    fn max_height_caps_a_box_that_would_be_taller() {
        let rendered = run(
            "<body><div>one<br>two<br>three<br>four</div></body>",
            "body { margin: 0 } div { max-height: 30px }",
            600.0,
        );
        assert_eq!(content_boxes(&rendered)[0].rect.height, 30.0);
    }

    #[test]
    fn max_height_does_not_stretch_a_shorter_box() {
        let rendered = run(
            "<body><div>one line</div></body>",
            "body { margin: 0 } div { max-height: 300px }",
            600.0,
        );
        let height = content_boxes(&rendered)[0].rect.height;
        assert!(
            height < 300.0,
            "a cap is not a height: the box grew to {height}"
        );
    }

    #[test]
    fn max_height_none_takes_an_earlier_cap_back_off() {
        // How a page overrides a cap set by a rule above it. `none` is a
        // keyword, not a length, so parsing one is not enough to see it.
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { height: 96px; max-height: 0; max-height: none }",
            600.0,
        );
        assert_eq!(content_boxes(&rendered)[0].rect.height, 96.0);
    }

    #[test]
    fn a_negative_bound_is_invalid_and_leaves_the_box_alone() {
        // CSS 2.1 forbids a negative value here, and an invalid declaration is
        // dropped rather than clamped to zero — the box keeps the height it
        // had. Clamping instead collapses a one-inch square to nothing, which
        // is what four of the suite's `max-height` tests check for directly.
        for bound in [
            "max-height: -1px",
            "min-height: -1px",
            "width: -1px",
            "height: -1px",
        ] {
            let css = format!("body {{ margin: 0 }} div {{ height: 96px; width: 96px; {bound} }}");
            let rendered = run("<body><div></div></body>", &css, 600.0);
            let rect = content_boxes(&rendered)[0].rect;
            assert_eq!((rect.width, rect.height), (96.0, 96.0), "{bound}");
        }
    }

    #[test]
    fn a_negative_padding_is_invalid_but_a_negative_margin_is_not() {
        // The one place the two edges differ. A negative margin is legal and
        // was how the era pulled a box back over its neighbour; a negative
        // padding is invalid and the declaration goes.
        let padded = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { padding: 10px; padding-top: -5px }",
            600.0,
        );
        assert_eq!(
            content_boxes(&padded)[0].style.padding.top,
            css::Length::Px(10.0),
            "a negative padding was accepted"
        );
        let pulled = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { margin-top: -5px }",
            600.0,
        );
        assert_eq!(
            content_boxes(&pulled)[0].style.margin.top,
            css::Length::Px(-5.0),
            "a negative margin was rejected"
        );
    }

    #[test]
    fn min_height_wins_when_both_bounds_apply() {
        // §10.7 applies the maximum first and the minimum second, so a box
        // asked to be at most 10px and at least 60px is 60px. Applying them
        // the other way round gives 10px and looks just as deliberate.
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { max-height: 10px; min-height: 60px }",
            600.0,
        );
        assert_eq!(content_boxes(&rendered)[0].rect.height, 60.0);
    }

    #[test]
    fn max_height_bounds_the_content_box_and_not_the_border_box() {
        // The same shape as `max-width` and `min-height`: the bound is on the
        // content box, so padding and border are added back on. Getting this
        // wrong is a few pixels and is plainly visible on a bordered box.
        let rendered = run(
            "<body><div>one<br>two<br>three</div></body>",
            "body { margin: 0 } div { max-height: 20px; padding: 5px; border: 2px solid black }",
            600.0,
        );
        assert_eq!(
            content_boxes(&rendered)[0].rect.height,
            20.0 + 10.0 + 4.0,
            "the cap swallowed the padding and border"
        );
    }

    #[test]
    fn a_percentage_max_height_is_no_bound() {
        // It resolves against the containing block's height, which is `auto`
        // for nearly every box here. Chromium leaves such a box unbounded too.
        let rendered = run(
            "<body><div>one<br>two<br>three<br>four</div></body>",
            "body { margin: 0 } div { max-height: 25% }",
            600.0,
        );
        assert!(
            content_boxes(&rendered)[0].rect.height > 40.0,
            "a percentage was treated as a bound"
        );
    }

    #[test]
    fn min_height_does_not_shrink_a_taller_box() {
        // It is a floor, not a height. A box whose content already exceeds it
        // must be left alone, or the property becomes `height` under a
        // different name.
        let sheet = "body { margin: 0 } div { min-height: 10px }";
        let tall = run(
            "<body><div>one<br>two<br>three<br>four<br>five</div></body>",
            sheet,
            600.0,
        );
        let plain = run(
            "<body><div>one<br>two<br>three<br>four<br>five</div></body>",
            "body { margin: 0 }",
            600.0,
        );
        assert_eq!(
            siblings(&tall).first().expect("div").rect.height,
            siblings(&plain).first().expect("div").rect.height
        );
    }

    #[test]
    fn text_indent_moves_the_first_line_and_only_the_first() {
        // §16.4. The indent comes out of the first line's own width rather than
        // the block's, which is why it cannot be an offset applied at paint
        // time: it changes where the text wraps.
        let rendered = run(
            "<body><p>the first line of this paragraph is indented and the rest              of it is not, which takes several lines to show</p></body>",
            "body { margin: 0 } p { text-indent: 40px; width: 200px; margin: 0 }",
            600.0,
        );
        let text = siblings(&rendered)
            .iter()
            .find_map(|b| b.text.as_ref())
            .expect("the paragraph's text");
        assert!(text.lines.len() > 1, "needs to wrap to prove anything");
        // A line carries no offset of its own — `push_line` folds it into the
        // glyph positions — so the leftmost glyph is where the line starts.
        let starts_at = |line: &text::Line| {
            line.glyphs
                .iter()
                .map(|glyph| glyph.x)
                .fold(f32::MAX, f32::min)
        };
        let first = starts_at(&text.lines[0]);
        assert!(
            (first - 40.0).abs() < 1.0,
            "first line starts at {first}, not at the 40px indent"
        );
        for line in &text.lines[1..] {
            let at = starts_at(line);
            assert!(
                at < 1.0,
                "a later line starts at {at}, so the indent was not the first line's alone"
            );
        }
    }

    #[test]
    fn text_transform_changes_what_is_measured_not_only_what_is_drawn() {
        // Applied before shaping, because uppercase is wider in every bundled
        // face. A transform done at paint time would wrap the line at the
        // lowercase width and then draw capitals past the edge.
        let plain = run(
            "<body><p>make me shout</p></body>",
            "body { margin: 0 } p { margin: 0 }",
            600.0,
        );
        let shouted = run(
            "<body><p>make me shout</p></body>",
            "body { margin: 0 } p { margin: 0; text-transform: uppercase }",
            600.0,
        );
        let width = |r: &Rendered| {
            siblings(r)
                .iter()
                .find_map(|b| b.text.as_ref())
                .expect("text")
                .width
        };
        assert!(
            width(&shouted) > width(&plain),
            "uppercase measured {} against {}",
            width(&shouted),
            width(&plain)
        );
    }

    #[test]
    fn a_caption_is_laid_out_at_all() {
        // It was not, for the whole life of the table code: the table branch of
        // `layout_block` returns before the child walk that would have reached
        // it, so a `<caption>` was parsed, cascaded, and then silently dropped.
        // Wikitables and infoboxes use them constantly.
        let rendered = run(
            "<body><table><caption>The heading</caption>\
             <tr><td>a</td><td>b</td></tr></table></body>",
            "body { margin: 0 }",
            600.0,
        );
        // Three text-bearing boxes: the caption and the two cells.
        let with_text = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .count();
        assert_eq!(with_text, 3, "the caption was dropped again");
    }

    #[test]
    fn a_caption_sits_outside_the_table_box_on_the_side_it_asks_for() {
        // §17.4: the caption is a sibling of the table box, not a child of it —
        // which is why a bordered table does not draw its border around its own
        // heading. Asserted as "outside", not merely "above": a caption laid
        // out inside the table would still be above its rows.
        let markup = "<body><table><caption>Heading</caption>\
                      <tr><td>a</td></tr></table></body>";
        let top = run(markup, "body { margin: 0 }", 600.0);
        let bottom = run(
            markup,
            "body { margin: 0 } table { caption-side: bottom }",
            600.0,
        );

        // Among the *body's own children*, because that is the only place the
        // question means anything: a box's rect is relative to its parent, so a
        // cell's rect and the table's are not in the same space at all.
        let caption_of = |rendered: &Rendered| -> Rect {
            siblings(rendered)
                .iter()
                .find(|b| b.style.display != Display::Table)
                .map(|b| b.rect)
                .expect("a caption box beside the table")
        };

        let table = table_of(&top).rect;
        let caption = caption_of(&top);
        assert!(
            caption.y + caption.height <= table.y + 0.01,
            "top caption at {caption:?} is not clear of the table at {table:?}"
        );

        let table = table_of(&bottom).rect;
        let caption = caption_of(&bottom);
        assert!(
            caption.y >= table.y + table.height - 0.01,
            "bottom caption at {caption:?} is not below the table at {table:?}"
        );
    }

    #[test]
    fn a_caption_never_wraps_narrower_than_its_longest_word() {
        // A one-column table of a single character would otherwise wrap its
        // heading to a letter a line. Browsers widen the table's wrapper box to
        // the caption's minimum; with no wrapper here the caption overhangs
        // instead, which comes to the same picture except for where the table
        // sits across it.
        let rendered = run(
            "<body><table><caption>Extraordinarily</caption>\
             <tr><td>x</td></tr></table></body>",
            "body { margin: 0 } td { padding: 0 }",
            600.0,
        );
        let table = table_of(&rendered).rect;
        let caption = siblings(&rendered)
            .iter()
            .find(|b| b.style.display != Display::Table)
            .expect("a caption")
            .rect;
        assert!(
            caption.width > table.width,
            "caption {caption:?} was squeezed to the table's {table:?}"
        );
    }

    #[test]
    fn content_after_a_table_clears_its_captions() {
        // The caption sits outside the table's border box, so the height the
        // table reports has to include it or the next paragraph is laid out on
        // top of a bottom caption.
        let without = run(
            "<body><table><tr><td>a</td></tr></table><p>after</p></body>",
            "body { margin: 0 } p { margin: 0 }",
            600.0,
        );
        let with = run(
            "<body><table><caption>Heading</caption><tr><td>a</td></tr></table>\
             <p>after</p></body>",
            "body { margin: 0 } p { margin: 0 }",
            600.0,
        );
        // The paragraph is a child of the body, as the table and its caption
        // are, so all three are measured in one coordinate space.
        let paragraph_y = |rendered: &Rendered| {
            siblings(rendered)
                .iter()
                .filter(|b| b.text.is_some())
                .map(|b| b.rect.y)
                .fold(0.0f32, f32::max)
        };
        assert!(
            paragraph_y(&with) > paragraph_y(&without),
            "the caption took no room: {} vs {}",
            paragraph_y(&with),
            paragraph_y(&without)
        );
    }

    #[test]
    fn a_caption_belongs_to_its_own_table_and_not_a_nested_one() {
        // `captions` reads direct children only. Walking the subtree would let
        // an outer table steal the heading of a table inside one of its cells,
        // and the era's pages nest tables several deep.
        let doc = dom::parse(
            "<table><tr><td><table><caption>inner</caption>\
             <tr><td>a</td></tr></table></td></tr></table>",
        );
        let styles = css::cascade::cascade(&doc, &[]);
        let outer = doc.find_element("table").expect("a table");
        assert!(
            table::captions(&doc, &styles, outer).is_empty(),
            "the outer table claimed the inner table's caption"
        );
    }

    #[test]
    fn collapsing_borders_are_shared_rather_than_doubled() {
        // Two 10px borders meeting between two cells become one 10px border,
        // not twenty pixels of them. The whole point of the model, and the
        // width of the table is the cleanest place to see it: three grid lines
        // of 10px rather than six borders plus `border-spacing`.
        let markup = "<body><table><tr><td>a</td><td>b</td></tr></table></body>";
        let sheet = "body { margin: 0 } table { width: auto } \
                     td { border: 10px solid black; padding: 0; width: 40px }";
        let separate = run(markup, sheet, 600.0);
        let collapsed = run(
            markup,
            &format!("{sheet} table {{ border-collapse: collapse }}"),
            600.0,
        );

        // Separated: two 40px columns, four 10px borders, and `border-spacing`
        // outside each cell and between them.
        let gaps = css::style::DEFAULT_BORDER_SPACING * 3.0;
        assert!(
            (table_box(&separate).rect.width - (40.0 * 2.0 + 10.0 * 4.0 + gaps)).abs() < 0.01,
            "separated table is {:?}",
            table_box(&separate).rect
        );
        // Collapsed: two 40px columns and *three* grid lines, with no spacing.
        assert!(
            (table_box(&collapsed).rect.width - (40.0 * 2.0 + 10.0 * 3.0)).abs() < 0.01,
            "collapsed table is {:?}",
            table_box(&collapsed).rect
        );
    }

    #[test]
    fn a_collapsed_cell_reserves_half_of_each_grid_line() {
        // Half in, half out: the cell's own used border is 5px on each side of
        // a 10px grid line, which is what puts its content where the reader
        // sees it. Reserved and not painted — the border is drawn once, by the
        // table, rather than twice in halves.
        let rendered = run(
            "<body><table><tr><td>a</td><td>b</td></tr></table></body>",
            "body { margin: 0 } table { border-collapse: collapse } \
             td { border: 10px solid black; padding: 0 }",
            600.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.style.display == Display::TableCell)
            .collect();
        assert_eq!(cells.len(), 2);
        for cell in &cells {
            assert!(
                (cell.style.border.left.used_width(cell.style.font_size) - 5.0).abs() < 0.01,
                "cell reserved {:?}",
                cell.style.border.left
            );
            assert_eq!(
                cell.style.border.left.style,
                css::style::BorderStyle::Hidden,
                "a cell must not paint its half of a collapsed border"
            );
        }
        // The cells meet: the first ends exactly where the second begins,
        // because they share the grid line their halves sit on.
        let first = &cells[0].rect;
        assert!(
            (first.x + first.width - cells[1].rect.x).abs() < 0.01,
            "cells at {:?} and {:?} do not share a grid line",
            first,
            cells[1].rect
        );
    }

    #[test]
    fn the_collapsing_model_ignores_border_spacing_and_table_padding() {
        // §17.6.2: neither applies. `cellspacing` is the same property under
        // its era name, and the era's markup sets it constantly — a table that
        // honoured it here would be pushed apart at every seam it just closed.
        let base = "body { margin: 0 } table { border-collapse: collapse } \
                    td { border: 2px solid black; padding: 0; width: 30px }";
        let plain = run(
            "<body><table><tr><td>a</td><td>b</td></tr></table></body>",
            base,
            600.0,
        );
        let spaced = run(
            r#"<body><table cellspacing="20"><tr><td>a</td><td>b</td></tr></table></body>"#,
            &format!("{base} table {{ border-spacing: 20px; padding: 15px }}"),
            600.0,
        );
        assert_eq!(
            table_box(&plain).rect.width,
            table_box(&spaced).rect.width,
            "border-spacing or padding moved a collapsing table"
        );
        assert_eq!(
            table_box(&plain).rect.height,
            table_box(&spaced).rect.height
        );
    }

    #[test]
    fn a_collapsing_table_draws_each_grid_line_once() {
        // The borders are boxes of their own, emitted after the cells so they
        // paint over the halves nobody drew. A 2x2 grid has three vertical and
        // three horizontal lines, and each is one box per segment: 6 + 6.
        let rendered = run(
            "<body><table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table></body>",
            "body { margin: 0 } table { border-collapse: collapse } \
             td { border: 2px solid black; padding: 0 }",
            600.0,
        );
        let painted = content_boxes(&rendered)
            .into_iter()
            .filter(|b| {
                b.node.is_none()
                    && [
                        b.style.border.top.style,
                        b.style.border.right.style,
                        b.style.border.bottom.style,
                        b.style.border.left.style,
                    ]
                    .iter()
                    .any(|style| style.is_visible())
            })
            .count();
        assert_eq!(painted, 12, "one box per grid line segment");
    }

    #[test]
    fn a_hidden_border_removes_a_grid_line_segment_and_nothing_else() {
        // `hidden` beats everything, so the segment it touches is not drawn —
        // and only that segment. The rest of the same grid line survives.
        let rendered = run(
            "<body><table><tr><td>a</td><td>b</td></tr>\
             <tr><td class=\"gone\">c</td><td>d</td></tr></table></body>",
            "body { margin: 0 } table { border-collapse: collapse } \
             td { border: 2px solid black; padding: 0 } \
             .gone { border-top-style: hidden }",
            600.0,
        );
        let doc = dom::parse(
            "<table><tr><td>a</td><td>b</td></tr>\
             <tr><td class=\"gone\">c</td><td>d</td></tr></table>",
        );
        let sheets = [Stylesheet::parse(
            "table { border-collapse: collapse } td { border: 2px solid black } \
             .gone { border-top-style: hidden }",
        )];
        let styles = css::cascade::cascade(&doc, &sheets);
        let table = doc.find_element("table").expect("table");
        let collapsed = table::collapse_borders(
            &doc,
            &styles,
            table,
            styles.get(table).expect("a styled table"),
        );
        assert!(collapsed.horizontal[1][0].is_none(), "the hidden segment");
        assert!(
            collapsed.horizontal[1][1].is_some(),
            "the rest of the line went with it"
        );
        // The line still reserves its width, so the hole is a hole rather than
        // a place where the table closes up.
        assert_eq!(collapsed.horizontal_widths[1], 2.0);
        assert!(rendered.layout.height > 0.0);
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

    /// Every inline-block box on the page, in document order.
    fn inline_blocks(rendered: &Rendered) -> Vec<&LayoutBox> {
        content_boxes(rendered)
            .into_iter()
            .filter(|b| b.style.display == Display::InlineBlock)
            .collect()
    }

    #[test]
    fn an_inline_block_is_as_wide_as_its_content_and_no_wider() {
        // §10.3.9. The whole difference between an inline-block and a block:
        // one fills the line, the other takes what it needs.
        let rendered = run(
            "<body><div><span class=\"ib\">hi</span></div></body>",
            "body { margin: 0 } .ib { display: inline-block }",
            600.0,
        );
        let ib = inline_blocks(&rendered);
        assert_eq!(ib.len(), 1, "the inline-block is not a box at all");
        assert!(
            ib[0].rect.width > 0.0 && ib[0].rect.width < 200.0,
            "shrink-to-fit gave {:?}",
            ib[0].rect
        );
    }

    #[test]
    fn a_narrow_line_squeezes_an_inline_block_but_not_below_its_longest_word() {
        // The other half of §10.3.5: available width caps it, the widest
        // unbreakable thing in it floors it.
        let rendered = run(
            "<body><div><span class=\"ib\">antidisestablishmentarianism</span></div></body>",
            "body { margin: 0 } .ib { display: inline-block }",
            20.0,
        );
        let ib = inline_blocks(&rendered);
        assert!(
            ib[0].rect.width > 20.0,
            "squeezed below its longest word: {:?}",
            ib[0].rect
        );
    }

    #[test]
    fn an_empty_inline_block_still_takes_up_the_room_it_was_given() {
        // The spacer that used to vanish. Laid out as a plain inline, an empty
        // inline-block has no text and so contributed nothing at all — which is
        // the silent failure that kept it out of `is_supported_layout`.
        let rendered = run(
            "<body><div><span class=\"ib\"></span></div></body>",
            "body { margin: 0 } .ib { display: inline-block; width: 40px; height: 12px }",
            600.0,
        );
        let ib = inline_blocks(&rendered);
        assert_eq!(ib.len(), 1, "the empty inline-block vanished");
        assert_eq!((ib[0].rect.width, ib[0].rect.height), (40.0, 12.0));
    }

    #[test]
    fn two_inline_blocks_sit_side_by_side_until_the_line_runs_out() {
        let row = |width: f32| {
            let rendered = run(
                "<body><div><span class=\"ib\">a</span><span class=\"ib\">b</span></div></body>",
                "body { margin: 0 } .ib { display: inline-block; width: 60px; height: 10px }",
                width,
            );
            let ib = inline_blocks(&rendered);
            assert_eq!(ib.len(), 2);
            (ib[0].rect, ib[1].rect)
        };

        let (first, second) = row(600.0);
        assert_eq!(first.y, second.y, "both belong on the same line");
        assert!(
            second.x >= first.x + first.width,
            "they overlap: {first:?} then {second:?}"
        );

        let (first, second) = row(80.0);
        assert!(
            second.y >= first.y + first.height,
            "a line with room for one kept both: {first:?} then {second:?}"
        );
    }

    #[test]
    fn an_inline_block_lays_its_own_blocks_out_inside_itself() {
        // The thing that makes it a block *container*: children stack, and the
        // block inside does not split the line around it the way a block inside
        // a plain inline element does.
        let rendered = run(
            "<body><p>before <span class=\"ib\"><b>one</b><b>two</b></span> after</p></body>",
            "body { margin: 0 } .ib { display: inline-block } b { display: block }",
            600.0,
        );
        let stacked: Vec<&LayoutBox> = inline_blocks(&rendered)
            .into_iter()
            .flat_map(|ib| boxes(ib))
            .filter(|b| b.style.display == Display::Block && b.text.is_some())
            .collect();
        assert_eq!(stacked.len(), 2, "the blocks inside were not laid out");
        assert!(
            stacked[1].rect.y >= stacked[0].rect.y + stacked[0].rect.height,
            "they did not stack: {:?} then {:?}",
            stacked[0].rect,
            stacked[1].rect
        );
    }

    #[test]
    fn an_inline_block_lines_up_on_the_baseline_of_its_last_line() {
        // §10.8.1. Two inline-blocks of different heights, each holding one
        // line: their text must sit level with each other and with the text
        // beside them, rather than their bottom edges lining up.
        let rendered = run(
            "<body><p>x<span class=\"tall\">y</span><span class=\"short\">z</span></p></body>",
            "body { margin: 0 } p { margin: 0 } \
             .tall, .short { display: inline-block } \
             .tall { padding-top: 30px } .short { padding-top: 0 }",
            600.0,
        );
        let ib = inline_blocks(&rendered);
        assert_eq!(ib.len(), 2);
        let baseline = |b: &LayoutBox| b.rect.y + last_baseline(b).expect("a line to align on");
        assert!(
            (baseline(ib[0]) - baseline(ib[1])).abs() < 0.5,
            "their text is not level: {:?} against {:?}",
            ib[0].rect,
            ib[1].rect
        );
    }

    #[test]
    fn an_inline_block_with_no_line_in_it_sits_on_the_baseline() {
        // The other half of §10.8.1: with no line box of its own, an
        // inline-block aligns on its bottom margin edge, like an image. An
        // empty spacer that hung below the text instead would drag every line
        // holding one out of position.
        let rendered = run(
            "<body><p>text<span class=\"ib\"></span></p></body>",
            "body { margin: 0 } p { margin: 0 } \
             .ib { display: inline-block; width: 10px; height: 40px }",
            600.0,
        );
        let ib = inline_blocks(&rendered);
        let line = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.text.as_ref().is_some_and(|t| !t.lines.is_empty()))
            .expect("a line of text");
        let text_baseline =
            line.rect.y + line.content_origin.1 + line.text.as_ref().unwrap().lines[0].baseline;
        assert!(
            (ib[0].rect.y + ib[0].rect.height - text_baseline).abs() < 0.5,
            "the spacer does not sit on the baseline: {:?} against {text_baseline}",
            ib[0].rect
        );
    }

    #[test]
    fn an_inline_block_with_room_takes_its_preferred_width_not_its_smallest() {
        // The other end of §10.3.5. Shrinking to the *minimum* would be
        // shrink-to-fit too, by a reading that puts every inline-block one word
        // wide and several lines tall.
        let rendered = run(
            "<body><div><span class=\"ib\">one two three four</span></div></body>",
            "body { margin: 0 } .ib { display: inline-block }",
            600.0,
        );
        let ib = inline_blocks(&rendered);
        let lines = ib[0]
            .children
            .iter()
            .chain(std::iter::once(ib[0]))
            .filter_map(|b| b.text.as_ref())
            .map(|t| t.lines.len())
            .sum::<usize>();
        assert_eq!(lines, 1, "it wrapped where there was room not to");
    }

    #[test]
    fn an_inline_block_holding_a_block_still_sits_on_the_line() {
        // An inline element containing a block is laid out as a block here,
        // because an inline box cannot contain one. An inline-block can: it is
        // atomic, and the text before it belongs on the same line.
        let rendered = run(
            "<body><p>before <span class=\"ib\"><b>x</b></span></p></body>",
            "body { margin: 0 } .ib { display: inline-block } b { display: block }",
            600.0,
        );
        let ib = inline_blocks(&rendered);
        assert_eq!(ib.len(), 1);
        assert!(
            ib[0].rect.x > 0.0,
            "it was pushed onto a line of its own: {:?}",
            ib[0].rect
        );
    }

    #[test]
    fn an_inline_block_keeps_its_childs_top_margin_inside_it() {
        // §8.3.1: an inline-block establishes a formatting context, so a first
        // child's margin does not escape through its top edge the way it does
        // through an ordinary block's.
        let rendered = run(
            "<body><div><span class=\"ib\"><p>x</p></span></div></body>",
            "body { margin: 0 } .ib { display: inline-block } \
             p { margin: 0; margin-top: 20px }",
            600.0,
        );
        let ib = inline_blocks(&rendered);
        let inner = ib[0].children.first().expect("the paragraph inside");
        assert!(
            inner.rect.y >= 20.0,
            "the margin escaped the inline-block: {:?}",
            inner.rect
        );
    }

    #[test]
    fn a_line_grows_to_fit_a_box_aligned_to_its_top_or_bottom() {
        // A `top`- or `bottom`-aligned box is hung from the line box rather
        // than from the baseline, so it cannot be accounted for as ascent — but
        // the line still has to be tall enough to hold it.
        let height = |align: &str| {
            let rendered = run(
                "<body><p>text<span class=\"ib\"></span></p><hr></body>",
                &format!(
                    "body {{ margin: 0 }} p {{ margin: 0 }} hr {{ margin: 0 }} \
                     .ib {{ display: inline-block; width: 10px; height: 60px; \
                            vertical-align: {align} }}"
                ),
                600.0,
            );
            content_boxes(&rendered)
                .into_iter()
                .find(|b| b.style.display == Display::Block && b.node.is_some())
                .map(|b| b.rect.height)
                .expect("the paragraph")
        };
        for align in ["top", "bottom"] {
            assert!(
                height(align) >= 60.0,
                "a {align}-aligned box overflowed its line: {}",
                height(align)
            );
        }
    }

    #[test]
    fn a_line_makes_room_for_what_hangs_below_its_baseline() {
        // A tall box fixes where the baseline is; a second box whose own
        // baseline is near its top then hangs a long way below it. Counting
        // only the tallest thing on the line leaves that overhang outside the
        // line box, and the next line is drawn through it.
        let rendered = run(
            "<body><p><span class=\"tall\"></span><span class=\"hang\">x</span></p></body>",
            "body { margin: 0 } p { margin: 0 } \
             .tall, .hang { display: inline-block } \
             .tall { width: 10px; height: 60px } \
             .hang { padding-bottom: 30px }",
            600.0,
        );
        let paragraph = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.display == Display::Block && b.node.is_some())
            .expect("the paragraph");
        assert!(
            paragraph.rect.height >= 80.0,
            "the overhang fell outside the line: {:?}",
            paragraph.rect
        );
    }

    #[test]
    fn a_floated_inline_block_is_a_float_and_not_a_box_on_the_line() {
        // §9.7: `float` makes a box block-level whatever `display` said.
        // `float: left; display: inline-block` is how a shrink-to-fit float
        // gets written, and reading the `display` literally put the box on a
        // line instead of against the containing block's edge.
        let rendered = run(
            "<body><div id=\"outer\"><span class=\"ib\">a</span><span class=\"f\">b</span></div></body>",
            "body { margin: 0 } #outer { width: 300px } \
             .ib, .f { display: inline-block; width: 100px; height: 20px } \
             .f { float: left }",
            600.0,
        );
        assert_eq!(
            inline_blocks(&rendered).len(),
            1,
            "the floated one is still on the line"
        );
    }

    #[test]
    fn a_float_inside_an_inline_element_is_still_placed() {
        // `<span><div style="float: left">…</div></span>`. The float is out of
        // flow, so it must not break the line around it — and it must not be
        // lost either, which is what happens if nothing descends to find it.
        let rendered = run(
            "<body><p>before <span><i class=\"f\"></i></span> after</p></body>",
            "body { margin: 0 } .f { float: left; width: 30px; height: 30px; background: #ff0000 }",
            600.0,
        );
        let float = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.background_color == css::Color::rgb(255, 0, 0));
        assert!(float.is_some(), "the float inside the span vanished");
        let lines: Vec<&LayoutBox> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.as_ref().is_some_and(|t| !t.lines.is_empty()))
            .collect();
        assert_eq!(lines.len(), 1, "the float broke the paragraph into pieces");
    }

    #[test]
    fn vertical_align_decides_where_an_inline_block_hangs_on_the_line() {
        // §10.8.1. One short box beside a tall one whose own baseline sits well
        // above its bottom edge, so the four placements are four different
        // places: at the line's top, on the baseline, straddling it, and at the
        // line's bottom.
        let placed = |align: &str| {
            let rendered = run(
                "<body><p><span class=\"tall\">x</span><span class=\"ib\"></span></p></body>",
                &format!(
                    "body {{ margin: 0 }} p {{ margin: 0 }} \
                     .tall, .ib {{ display: inline-block; width: 10px }} \
                     .tall {{ padding-top: 40px }} \
                     .ib {{ height: 10px; vertical-align: {align} }}"
                ),
                600.0,
            );
            let ib = inline_blocks(&rendered);
            assert_eq!(ib.len(), 2);
            ib[1].rect.y
        };
        let top = placed("top");
        let baseline = placed("baseline");
        let middle = placed("middle");
        let bottom = placed("bottom");
        assert_eq!(top, 0.0, "top did not reach the top of the line");
        assert!(
            baseline < middle && middle < bottom,
            "baseline {baseline}, middle {middle}, bottom {bottom} are not in order"
        );
        assert!(
            top < baseline,
            "top {top} is not above the baseline placement {baseline}"
        );
    }

    #[test]
    fn a_cell_still_centres_its_content_without_being_told_to() {
        // The initial value of `vertical-align` is `baseline`, which an
        // inline-block needs and a cell must not have. A cell's `middle` comes
        // from the UA sheet instead — and this is the assertion that the two
        // did not get swapped.
        let rendered = run(
            "<body><table><tr><td>short</td><td>a<br>b<br>c<br>d</td></tr></table></body>",
            "body { margin: 0 } td { padding: 0 }",
            600.0,
        );
        let cells: Vec<&LayoutBox> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert!(
            cells[0].content_origin.1 > 0.0,
            "the short cell was not centred"
        );
    }

    #[test]
    fn margins_on_an_inline_block_hold_it_away_from_what_is_beside_it() {
        // An inline-block's margins are its own: they neither collapse with a
        // child's nor vanish the way an inline element's vertical ones do.
        let rendered = run(
            "<body><div><span class=\"ib\">a</span><span class=\"ib\">b</span></div></body>",
            "body { margin: 0 } \
             .ib { display: inline-block; width: 20px; height: 10px; margin: 5px }",
            600.0,
        );
        let ib = inline_blocks(&rendered);
        assert!(
            ib[1].rect.x - (ib[0].rect.x + ib[0].rect.width) >= 10.0,
            "the two margins between them were lost: {:?} then {:?}",
            ib[0].rect,
            ib[1].rect
        );
        assert!(
            ib[0].rect.y >= 5.0,
            "the top margin was lost: {:?}",
            ib[0].rect
        );
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
    fn a_float_after_the_last_block_is_still_placed() {
        // Floats declared after the first in-flow block are held back and
        // placed when the next block arrives. A float with no block after it
        // was never placed at all — it vanished, which is most of what
        // `float: left` is for on a page that ends with one.
        let rendered = run(
            r#"<body><p>text</p><div id="f"></div></body>"#,
            "body { margin: 0 } div { float: left; width: 40px; height: 60px; background: #ccc }",
            600.0,
        );
        let floated: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.style.float != Float::None)
            .collect();
        assert_eq!(floated.len(), 1, "the trailing float was dropped");
        assert!(
            floated[0].rect.y > 0.0,
            "it must sit below the paragraph, not at the top: {:?}",
            floated[0].rect
        );
        assert!(
            rendered.layout.height >= 60.0,
            "the page is too short to hold it: {}",
            rendered.layout.height
        );
    }

    #[test]
    fn a_trailing_float_is_placed_once_and_not_twice() {
        // The drain inside the loop and the drain after it must not both fire
        // for the same float.
        let rendered = run(
            r#"<body><p>one</p><div id="f"></div><p>two</p></body>"#,
            "body { margin: 0 } div { float: left; width: 40px; height: 60px; background: #ccc }",
            600.0,
        );
        let floated = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.style.float != Float::None)
            .count();
        assert_eq!(floated, 1, "the float was placed twice");
    }

    #[test]
    fn an_absolutely_positioned_box_is_not_also_a_float() {
        // §9.7: `position: absolute` makes `float` compute to `none`. Both
        // take the box out of flow and each has its own way of placing it, so
        // a box claiming both was placed twice and drawn twice.
        let rendered = run(
            r#"<body><p>one</p><div id="f"></div></body>"#,
            "body { margin: 0 } div { float: right; position: absolute; top: 0; \
             width: 40px; height: 60px; background: #ccc }",
            600.0,
        );
        let boxes: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.rect.width == 40.0 && b.rect.height == 60.0)
            .collect();
        assert_eq!(boxes.len(), 1, "the box was drawn twice: {boxes:?}");
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
    fn max_width_narrows_an_image_without_stretching_it() {
        // A replaced box has to bring its height down with its width. Narrowed
        // and left at its original height, an image is not bounded, it is
        // squashed — which is worse than one that overflows, because it looks
        // like the picture rather than like the browser.
        let style = ComputedStyle {
            max_width: Length::Px(300.0),
            ..ComputedStyle::default()
        };
        let (width, height) = replaced_size(&style, Some((1200.0, 400.0)), None, None, 1000.0);
        assert_eq!(width, 300.0);
        assert_eq!(height, 100.0, "the 3:1 ratio should have been kept");

        // One already inside the bound is left exactly alone.
        let (width, height) = replaced_size(&style, Some((120.0, 40.0)), None, None, 1000.0);
        assert_eq!((width, height), (120.0, 40.0));
    }

    #[test]
    fn max_width_bounds_a_box_that_would_otherwise_fill_its_container() {
        // §10.4. Unimplemented until now, which mattered more than an unused
        // property usually does: the reader sheet has asked for `max-width:
        // 42em` since it was written and had been getting the whole window.
        let rendered = run(
            "<body><div class=\"bound\"></div></body>",
            "body { margin: 0 } .bound { height: 10px; max-width: 300px }",
            1000.0,
        );
        let boxes: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.rect.height == 10.0)
            .collect();
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].rect.width, 300.0, "the bound was not applied");
    }

    #[test]
    fn max_width_clamps_a_declared_width_rather_than_replacing_it() {
        // A width below the bound is left exactly alone; one above it is cut
        // down to the bound. Getting this backwards would make every bounded
        // box the same size.
        let width_of = |css: &str, available: f32| {
            let rendered = run(
                "<body><div class=\"b\"></div></body>",
                &format!("body {{ margin: 0 }} .b {{ height: 10px; {css} }}"),
                available,
            );
            content_boxes(&rendered)
                .into_iter()
                .find(|b| b.rect.height == 10.0)
                .expect("the box")
                .rect
                .width
        };
        assert_eq!(width_of("width: 100px; max-width: 300px", 1000.0), 100.0);
        assert_eq!(width_of("width: 800px; max-width: 300px", 1000.0), 300.0);
        // And a bound wider than the space available does not widen anything.
        assert_eq!(width_of("max-width: 900px", 400.0), 400.0);
    }

    /// Width of the one 10px-tall box on a page, which is how these ask.
    fn bounded_width(css: &str, available: f32) -> f32 {
        let rendered = run(
            "<body><div class=\"b\"></div></body>",
            &format!("body {{ margin: 0 }} .b {{ height: 10px; {css} }}"),
            available,
        );
        content_boxes(&rendered)
            .into_iter()
            .find(|b| b.rect.height == 10.0)
            .expect("the box")
            .rect
            .width
    }

    #[test]
    fn min_width_raises_a_box_that_would_be_narrower() {
        // §10.4, and absent until now on the grounds that nothing needed it.
        // Wikipedia's stylesheet asks for it 33 times, and 27 WPT tests went
        // from failing to passing when it arrived.
        assert_eq!(bounded_width("width: 10px; min-width: 300px", 800.0), 300.0);
        // One already above the floor is left exactly alone. Getting this
        // backwards would make every floored box the same size.
        assert_eq!(
            bounded_width("width: 600px; min-width: 100px", 800.0),
            600.0
        );
    }

    #[test]
    fn a_negative_floor_is_dropped_rather_than_obeyed() {
        // §10.4 forbids a negative value here, and an invalid declaration is
        // dropped rather than clamped: the box keeps the width it already had.
        // Free with `parse_size`, which arrived with `max-height`, and worth
        // pinning because using the plain length parser would look right until
        // a stylesheet said `min-width: -1px`.
        assert_eq!(bounded_width("width: 60px; min-width: -1px", 800.0), 60.0);
    }

    #[test]
    fn min_width_wins_where_it_crosses_max_width() {
        // §10.4 applies the maximum and *then* the minimum, so where the two
        // cross the floor is what survives. It looks like a mistake in the
        // stylesheet and is what the specification asks for: the author who
        // wrote a floor meant the content to fit. Chromium gives 200 here too.
        assert_eq!(
            bounded_width("width: 10px; max-width: 50px; min-width: 200px", 800.0),
            200.0
        );
    }

    #[test]
    fn min_width_measures_the_content_box_like_width_does() {
        // The same shape as `max-width`: CSS 2.1 sizes the *content* box, so
        // padding and border grow the box outwards past the floor rather than
        // eating into it. 200 of content and 50 of padding is 250 of box, which
        // is what Chromium lays out.
        assert_eq!(
            bounded_width("width: 100px; min-width: 200px; padding: 0 25px", 800.0),
            250.0
        );
    }

    #[test]
    fn a_floor_below_the_width_already_taken_changes_nothing() {
        // A percentage floor resolves against the containing block's width, so
        // a box filling its container is already past a floor of less than all
        // of it.
        assert_eq!(bounded_width("min-width: 40%", 100.0), 100.0);
    }

    #[test]
    fn max_width_measures_the_content_box_like_width_does() {
        // CSS 2.1 sizes the *content* box, so padding and border grow the box
        // outwards past the bound rather than eating into it. A bound that
        // included them would make a padded box narrower than an unpadded one
        // asking for the same measure.
        let rendered = run(
            "<body><div class=\"b\"></div></body>",
            "body { margin: 0 } \
             .b { height: 10px; max-width: 300px; padding: 0 20px; border: 0 }",
            1000.0,
        );
        let box_ = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.rect.height == 10.0)
            .expect("the box");
        assert_eq!(box_.rect.width, 340.0, "300 of content plus 20 either side");
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
            replaced_size(
                &style,
                Some((100.0, 50.0)),
                Some(Length::Px(200.0)),
                None,
                500.0
            ),
            (200.0, 100.0)
        );
        assert_eq!(
            replaced_size(
                &style,
                Some((100.0, 50.0)),
                None,
                Some(Length::Px(25.0)),
                500.0
            ),
            (50.0, 25.0)
        );
    }

    #[test]
    fn both_declared_dimensions_win_over_the_ratio() {
        let style = ComputedStyle::default();
        assert_eq!(
            replaced_size(
                &style,
                Some((100.0, 50.0)),
                Some(Length::Px(30.0)),
                Some(Length::Px(300.0)),
                500.0
            ),
            (30.0, 300.0)
        );
    }

    #[test]
    fn css_overrides_the_presentational_attribute() {
        let style = ComputedStyle {
            width: Length::Px(64.0),
            ..ComputedStyle::default()
        };
        let (width, _) = replaced_size(
            &style,
            Some((100.0, 100.0)),
            Some(Length::Px(999.0)),
            None,
            500.0,
        );
        assert_eq!(width, 64.0);
    }

    #[test]
    fn an_image_that_never_loaded_still_occupies_its_declared_box() {
        // A broken image must not collapse the layout around it.
        let style = ComputedStyle::default();
        assert_eq!(
            replaced_size(
                &style,
                None,
                Some(Length::Px(120.0)),
                Some(Length::Px(60.0)),
                500.0
            ),
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
    fn a_percentage_size_attribute_is_a_percentage_and_not_pixels() {
        let doc = dom::parse(r#"<body><img src="x.png" width="50%"></body>"#);
        assert_eq!(
            size_attr(&doc, doc.find_element("img").expect("img"), "width"),
            Some(Length::Percent(50.0)),
            "50% must be half the containing block, and never 50px"
        );
    }

    #[test]
    fn a_percentage_width_attribute_fills_that_share_of_the_line() {
        // `<img width="100%">` is how the era drew a rule across a column and
        // how the CSS 2.1 suite's own references draw a green bar. Ignoring it
        // left a 1x1 image at its intrinsic size — a single pixel where the
        // page wanted a band.
        let style = ComputedStyle::default();
        assert_eq!(
            replaced_size(
                &style,
                Some((1.0, 1.0)),
                Some(Length::Percent(100.0)),
                Some(Length::Px(50.0)),
                500.0
            ),
            (500.0, 50.0)
        );
        assert_eq!(
            replaced_size(
                &style,
                Some((1.0, 1.0)),
                Some(Length::Percent(50.0)),
                None,
                500.0
            ),
            (250.0, 250.0),
            "with no height, a square image keeps its ratio"
        );
    }

    #[test]
    fn a_percentage_height_attribute_is_auto() {
        // It resolves against the containing block's *height*, which is `auto`
        // for nearly every box on a page of this era — §10.5 makes such a
        // percentage `auto` in turn. The only basis to hand is the width, and
        // using it would size an image to a fraction of the page's width in
        // the vertical direction, which is not a small error.
        let style = ComputedStyle::default();
        assert_eq!(
            replaced_size(
                &style,
                Some((100.0, 50.0)),
                None,
                Some(Length::Percent(50.0)),
                500.0
            ),
            (100.0, 50.0),
            "the image keeps its intrinsic size"
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

    /// The box drawn for the first control on the page.
    fn control_box(rendered: &Rendered) -> &LayoutBox {
        content_boxes(rendered)
            .into_iter()
            .find(|b| b.style.border.left.used_width(b.style.font_size) > 0.0)
            .expect("a control box")
    }

    #[test]
    fn a_control_is_a_box_on_the_line_and_not_a_block() {
        // The bug this pins: a `<select multiple>` gives its options
        // `display: block`, and reading that as "contains a block" laid the
        // control out as a block box filling the whole line.
        let rendered = run(
            r#"<body><p>Pick: <select multiple size="3"><option>Alpha</option>
               <option>Beta</option></select></p></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        let control = control_box(&rendered);
        assert!(
            control.rect.width < 200.0,
            "a list box filled the line: {:?}",
            control.rect
        );
        assert!(
            control.rect.x > 0.0,
            "a list box was not placed after the text beside it: {:?}",
            control.rect
        );
    }

    #[test]
    fn a_value_too_long_for_its_field_is_cut_at_the_border() {
        let rendered = run(
            r#"<body><input type="text" size="8" value="far more text than eight characters"></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        let control = control_box(&rendered);
        let label = control.text.as_ref().expect("a shaped value");
        assert_eq!(label.lines.len(), 1, "the value wrapped out of the field");
        assert!(
            label.width <= control.content_width,
            "the value ran past the field: {} in {}",
            label.width,
            control.content_width
        );
        // Cut, not merely measured short: the glyphs beyond the edge are gone.
        let last = label.lines[0]
            .glyphs
            .last()
            .expect("some of the value is shown");
        assert!(
            last.x < control.content_width,
            "a glyph was left outside the field at x={}",
            last.x
        );
    }

    #[test]
    fn a_textarea_keeps_the_lines_it_was_given() {
        let rendered = run(
            "<body><textarea rows=\"3\" cols=\"20\">one\ntwo\nthree</textarea></body>",
            "body { margin: 0 }",
            600.0,
        );
        let control = control_box(&rendered);
        let label = control.text.as_ref().expect("a shaped value");
        assert_eq!(
            label.lines.len(),
            3,
            "a textarea collapsed its lines into one"
        );
    }

    #[test]
    fn a_textarea_shows_the_spacing_it_was_typed_with() {
        // A textarea is preformatted: the spaces between two words are as many
        // as were typed. Shaping its value like ordinary flow text collapses
        // them to one, and columns lined up with spaces — which is how the
        // era's forms held their shape — fall apart.
        let width = |value: &str| {
            let html = format!("<body><textarea cols=\"40\">{value}</textarea></body>");
            let rendered = run(&html, "body { margin: 0 }", 600.0);
            control_box(&rendered)
                .text
                .as_ref()
                .expect("a shaped value")
                .lines[0]
                .width
        };
        assert!(
            width("a    b") > width("a b") + 2.0,
            "a textarea collapsed the spacing inside its value: {} vs {}",
            width("a    b"),
            width("a b")
        );
    }

    #[test]
    fn a_checked_box_draws_a_mark_and_an_unchecked_one_does_not() {
        let checked = run(
            r#"<body><input type="checkbox" checked></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        let empty = run(
            r#"<body><input type="checkbox"></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        assert_eq!(control_box(&checked).children.len(), 1, "no tick was drawn");
        assert!(
            control_box(&empty).children.is_empty(),
            "an unchecked box was ticked"
        );
    }

    #[test]
    fn a_hidden_input_takes_no_room() {
        let rendered = run(
            r#"<body><p>before<input type="hidden" value="x">after</p></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        assert!(
            content_boxes(&rendered).into_iter().all(|b| b
                .style
                .border
                .left
                .used_width(b.style.font_size)
                == 0.0),
            "a hidden input was drawn"
        );
    }

    #[test]
    fn a_page_is_tall_enough_for_content_that_overflows_its_box() {
        // `height: 0` with text in it is not an empty box: `overflow`
        // defaults to `visible`, so the text is still drawn — below the box.
        // Sizing the canvas by the box alone cuts it off, and being cut off
        // looks exactly like never having been drawn.
        let rendered = run(
            "<body><div style=\"height: 0\">text below the box</div></body>",
            "body { margin: 0 }",
            600.0,
        );
        assert!(
            rendered.layout.height > 5.0,
            "the page ended at the box's own edge: {}",
            rendered.layout.height
        );
    }

    #[test]
    fn a_page_with_nothing_overflowing_is_not_made_taller() {
        // The floor is the root box. Growing past it without cause would pad
        // every page with blank canvas and move every committed baseline.
        let rendered = run("<body><p>one line</p></body>", "body { margin: 0 }", 600.0);
        assert_eq!(
            rendered.layout.height, rendered.layout.root.rect.height,
            "the page grew past the root box with nothing overflowing it"
        );
    }

    #[test]
    fn a_child_reaching_below_its_parent_extends_the_page() {
        // The spacer above matters: rectangles are relative to the parent, so
        // an overflow measured without carrying the offset down reports 60
        // here rather than 160, and is right only when the overflow happens
        // to begin at the very top of the page.
        let rendered = run(
            "<body><div style=\"height: 100px\"></div>\
             <div style=\"height: 4px\"><div style=\"height: 60px\"></div></div></body>",
            "body { margin: 0 }",
            600.0,
        );
        assert!(
            rendered.layout.height >= 160.0,
            "an overflowing child was cut off: {}",
            rendered.layout.height
        );
    }

    #[test]
    fn clipping_a_label_leaves_what_fits() {
        let mut label = text::TextLayout {
            lines: vec![text::Line {
                glyphs: Vec::new(),
                replaced: Vec::new(),
                spans: Vec::new(),
                decorations: Vec::new(),
                text: String::new(),
                width: 200.0,
                y: 0.0,
                baseline: 10.0,
            }],
            height: 12.0,
            width: 200.0,
        };
        clip_label(&mut label, 50.0);
        assert_eq!(label.width, 50.0);
        assert_eq!(label.lines[0].width, 50.0);

        // A label already inside its box is left exactly as it was.
        let mut narrow = label.clone();
        clip_label(&mut narrow, 500.0);
        assert_eq!(narrow.width, 50.0, "clipping widened a label");
    }
}

#[cfg(test)]
mod clipped_overflow_tests {
    use super::*;
    use css::Stylesheet;

    /// How tall the page is, which is how far it can be scrolled.
    fn height(html: &str) -> f32 {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css::ua::UA_STYLESHEET)];
        let styles = css::cascade::cascade(&doc, &sheets);
        let mut fonts = FontStore::new();
        layout(&doc, &styles, &mut fonts, &IntrinsicSizes::new(), 800.0).height
    }

    #[test]
    fn content_clipped_out_of_sight_is_not_somewhere_to_scroll() {
        // CSS 2.1 §11.1.1: `overflow: hidden` clips, and offers no way to
        // reach what it clipped. A page that can be scrolled to it is a page
        // with a mile of blank space under it — which is what a dropdown
        // written without scripting leaves behind, since it is `height: 0;
        // overflow: hidden` with its whole menu inside.
        let clipped = height(
            "<body style=\"margin: 0\"><div style=\"height: 2000px\">spacer</div>\
             <div style=\"position: absolute; height: 0; overflow: hidden\">\
             <div style=\"height: 3000px\">menu</div></div></body>",
        );
        let visible = height(
            "<body style=\"margin: 0\"><div style=\"height: 2000px\">spacer</div>\
             <div style=\"position: absolute; height: 0\">\
             <div style=\"height: 3000px\">menu</div></div></body>",
        );

        assert_eq!(clipped, 2000.0);
        // The same page without the clip keeps every pixel of the overflow —
        // the menu starts where the spacer ends and runs 3000px on from there
        // — which is what makes the first number mean something.
        assert_eq!(visible, 5000.0);
    }

    #[test]
    fn a_box_anchored_outside_the_clip_is_not_clipped_by_it() {
        // §10.1: an absolutely positioned box is clipped by an ancestor only
        // when that ancestor is its containing block, and a static box is
        // nobody's containing block. Chromium reaches 2000 here too, and the
        // reader is right to expect it: a tooltip anchored to the page, inside
        // a clipped wrapper, is on the page.
        let escaped = height(
            "<body style=\"margin: 0\"><div style=\"height: 1000px\">spacer</div>\
             <div style=\"height: 10px; overflow: hidden\">\
             <div style=\"position: absolute; top: 1500px; height: 500px\">out</div>\
             </div></body>",
        );

        assert_eq!(escaped, 2000.0);
    }

    #[test]
    fn a_box_escapes_from_however_deep_inside_the_clip_it_sits() {
        // The walk has to go *through* static boxes, not only look at the
        // clipper's own children: a static wrapper is nobody's containing
        // block either, so a box below one is still anchored to the page.
        // Chromium reaches 2000 here too.
        let escaped = height(
            "<body style=\"margin: 0\"><div style=\"height: 1000px\">spacer</div>\
             <div style=\"height: 10px; overflow: hidden\"><div><div>\
             <div style=\"position: absolute; top: 1500px; height: 500px\">out</div>\
             </div></div></div></body>",
        );

        assert_eq!(escaped, 2000.0);
    }

    #[test]
    fn a_relative_box_inside_the_clip_anchors_what_is_under_it() {
        // And the walk has to stop there. The relative wrapper is the
        // containing block for the box below it, and the wrapper is inside the
        // clip — so the box is measured from it and clipped with it. Chromium
        // stops at 1010 here as well.
        let anchored = height(
            "<body style=\"margin: 0\"><div style=\"height: 1000px\">spacer</div>\
             <div style=\"height: 10px; overflow: hidden\">\
             <div style=\"position: relative\">\
             <div style=\"position: absolute; top: 1500px; height: 500px\">in</div>\
             </div></div></body>",
        );

        assert_eq!(anchored, 1010.0);
    }

    #[test]
    fn a_positioned_clipper_clips_what_it_anchors() {
        // The other half of the same rule. Make the wrapper `relative` and it
        // becomes the containing block, so the box inside is measured from it
        // and clipped by it — which is Wikipedia's dropdown exactly.
        let anchored = height(
            "<body style=\"margin: 0\"><div style=\"height: 1000px\">spacer</div>\
             <div style=\"position: relative; height: 10px; overflow: hidden\">\
             <div style=\"position: absolute; top: 1500px; height: 500px\">in</div>\
             </div></body>",
        );

        assert_eq!(anchored, 1010.0);
    }

    #[test]
    fn a_clipping_box_still_takes_its_own_room() {
        // Only what hangs *out* of it is gone. The box is in its parent's flow
        // and as tall as it says it is.
        let sized = height(
            "<body style=\"margin: 0\"><div style=\"height: 300px; overflow: hidden\">\
             <div style=\"height: 3000px\">tall</div></div></body>",
        );

        assert_eq!(sized, 300.0);
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
