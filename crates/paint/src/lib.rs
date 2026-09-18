//! Display list construction and rasterisation.
//!
//! Rasterisation is on the CPU via `tiny-skia` (ADR-0005): no GPU dependency,
//! and combined with bundled fonts it makes output identical on every platform,
//! so one set of reference baselines covers all three.

pub mod images;

use css::style::Visibility;
use css::value::Color;
use layout::{Layout, LayoutBox, Rect, line_offset};
use text::FontStore;

// Re-exported so consumers do not need their own tiny-skia dependency, and so
// the rasteriser choice stays an implementation detail of this crate.
pub use images::{DecodedImage, ImageKey, ImageSlot, ImageStore, decode};
// Re-exported for consumers that composite pixmaps of their own, such as the
// frameset renderer.
// The rasteriser's own types, re-exported rather than depended on twice. The
// shell draws one thing of its own — the application icon — and a second
// `tiny-skia` in another crate's manifest would be a second version to keep in
// step for no gain (ADR-0007).
pub use tiny_skia::{
    Color as RasterColor, FillRule, IntSize, Paint, Path, PathBuilder, Pixmap, PixmapPaint,
    PremultipliedColorU8 as PremultipliedColor, Rect as RasterRect, Stroke, Transform,
};

/// An opaque magenta, for debugging overlays: nothing on a real page is this.
pub fn magenta() -> PremultipliedColor {
    PremultipliedColor::from_rgba(255, 0, 255, 255).expect("opaque magenta is valid")
}

/// A second debugging colour, for when one overlay is not enough.
pub fn cyan() -> PremultipliedColor {
    PremultipliedColor::from_rgba(0, 190, 210, 255).expect("opaque cyan is valid")
}

/// One paint operation.
///
/// Building a display list before rasterising keeps geometry decisions separate
/// from pixel-pushing, which is what makes the same layout paintable to a PNG
/// in tests and to a window at runtime.
#[derive(Debug, Clone)]
pub enum DisplayItem {
    /// A filled rectangle.
    Rect {
        /// Area to fill.
        rect: Rect,
        /// Fill colour.
        color: Color,
    },
    /// A filled ellipse, inscribed in a rectangle.
    ///
    /// CSS 2.1 has no rounded anything, so this exists for one box: a radio
    /// button. A square radio beside a square checkbox is not a cosmetic
    /// shortfall — the shape *is* the meaning, a square saying "any of these"
    /// and a circle "one of these", and drawing both square removes the only
    /// thing telling a reader which question they are answering.
    Ellipse {
        /// The box the ellipse is inscribed in.
        rect: Rect,
        /// Fill colour.
        color: Color,
    },
    /// A decoded image drawn into a rectangle.
    Image {
        /// The element the image belongs to, used to look it up at raster time.
        node: dom::NodeId,
        /// Whether a box with no image in it is a picture that did not arrive.
        ///
        /// True for an `<img>` and false for every other replaced element. An
        /// `<iframe>` has no image by nature, and without this every empty
        /// frame on the page grew a `Load image` button offering to fetch one
        /// (#118).
        placeholder: bool,
        /// Destination rectangle; the image is scaled to fill it.
        rect: Rect,
    },
    /// A background image tiled across a rectangle.
    Tile {
        /// The element whose background this is.
        node: dom::NodeId,
        /// Area the tiling is clipped to.
        rect: Rect,
        /// Which axes the image repeats along.
        repeat: css::style::BackgroundRepeat,
        /// Where within the box the image is anchored.
        position: css::style::BackgroundPosition,
    },
    /// A single positioned glyph.
    Glyph {
        /// Glyph to draw, already positioned by the shaper.
        glyph: text::PositionedGlyph,
        /// Absolute x of the text origin.
        origin_x: f32,
        /// Absolute y of the text origin.
        origin_y: f32,
        /// Ink colour.
        color: Color,
    },
}

/// An ordered list of paint operations, back to front.
#[derive(Debug, Clone)]
pub struct DisplayList {
    /// Colour the whole canvas is cleared to before anything is drawn.
    ///
    /// Held here rather than emitted as the first item because the canvas can
    /// be taller than the content — a frame's cell, or a window showing a short
    /// page — and the background has to reach the bottom of it either way.
    pub canvas: Color,
    /// Image tiled across the whole canvas, for the same reason.
    ///
    /// The fourth field is the *positioning* area, which is not the canvas:
    /// §14.2 places the tile "as if it was painted for the root element
    /// alone", so an offset is measured from that element's padding box while
    /// the tiling covers the window.
    pub canvas_image: Option<(
        dom::NodeId,
        css::style::BackgroundRepeat,
        css::style::BackgroundPosition,
        Rect,
    )>,
    /// The items, in paint order.
    pub items: Vec<DisplayItem>,
    /// Half-open ranges of `items` that came from `position: fixed` subtrees.
    ///
    /// Those items are the one thing on the page whose coordinates are the
    /// *window's* rather than the document's: a page is laid out once and
    /// `rasterise_band` draws a slice of it by shifting every item up by the
    /// band's top, and a fixed box must not move when the reader scrolls
    /// (#108). So it must not be shifted.
    ///
    /// Ranges rather than a list of their own, so the items stay in paint
    /// order where they were emitted. A separate list has to be drawn at some
    /// fixed point — last, in the obvious version — and that is wrong:
    /// `left-offset-position-fixed-001` covers a fixed red square with an
    /// absolutely positioned green one that comes after it in the source, and
    /// a fixed box painted last shows the red.
    ///
    /// The same trick `clip` above uses, and for the same reason: a subtree
    /// emits a contiguous run, so where it starts and stops is all that has to
    /// be remembered.
    pub pinned: Vec<(usize, usize)>,
}

impl DisplayList {
    /// Whether the item at `at` came from a `position: fixed` subtree, and so
    /// is drawn at the window's coordinates rather than the document's.
    ///
    /// A linear scan of the ranges, which is the right shape here: almost
    /// every page has none at all, and a page with one has one.
    pub fn is_pinned(&self, at: usize) -> bool {
        self.pinned.iter().any(|&(from, to)| at >= from && at < to)
    }
}

impl Default for DisplayList {
    fn default() -> Self {
        // White, not transparent: a page that declares no background would
        // otherwise composite against whatever the window happened to contain.
        Self {
            canvas: Color::WHITE,
            canvas_image: None,
            items: Vec::new(),
            pinned: Vec::new(),
        }
    }
}

/// Builds a display list from a layout.
pub fn build_display_list(layout: &Layout) -> DisplayList {
    let mut list = DisplayList {
        // The page's background covers the canvas, not just the root box
        // (CSS 2.1 §14.2). It is composited over white so a translucent one
        // still has something opaque behind it.
        canvas: layout.canvas_background.over(Color::WHITE),
        canvas_image: layout.canvas_image,
        items: Vec::new(),
        pinned: Vec::new(),
    };
    // §14.2 again: an element whose background was propagated to the canvas
    // does not paint it a second time. Drawing it twice is invisible while the
    // colour is opaque and wrong the moment it is not.
    let propagated = layout.canvas_image.map(|(node, ..)| node);
    paint_box(&layout.root, 0.0, 0.0, propagated, &mut list, true);
    list
}

fn paint_box(
    box_: &LayoutBox,
    offset_x: f32,
    offset_y: f32,
    propagated: Option<dom::NodeId>,
    list: &mut DisplayList,
    // Whether this box is a stacking context, and so the one that sorts the
    // context-forming boxes beneath it. The root always is (§9.9.1).
    context: bool,
) {
    let x = offset_x + box_.rect.x;
    let y = offset_y + box_.rect.y;
    let is_canvas_background = box_.node.is_some() && box_.node == propagated;

    // §11.1.2: `clip` applies to an absolutely positioned box, and clips that
    // box together with everything inside it. Applied to the *range of items
    // this subtree emits* rather than threaded through every push below —
    // there are four kinds of item and two more places that emit them, and a
    // parameter on each is four chances to forget one.
    //
    // Nesting falls out of that: an inner clip has already narrowed its own
    // items by the time the outer box's range is clipped, so the two
    // intersect without either knowing about the other.
    let clip = box_
        .style
        .position
        .is_out_of_flow()
        .then_some(box_.style.clip)
        .flatten()
        .map(|clip| resolve_clip(clip, x, y, box_.rect, box_.style.font_size));
    let clip_from = list.items.len();

    // §11.2: a hidden box draws nothing — no background, no border, no text,
    // no image — but its *children are still walked*, because `visibility`
    // inherits and a descendant may set `visible` to come back out of a hidden
    // ancestor. Returning here instead would take that descendant with it.
    //
    // Layout is untouched on purpose: the box keeps every pixel of the space it
    // would have taken, which is the whole difference between this and
    // `display: none` and the reason an author reaches for it.
    let drawn = box_.style.visibility == Visibility::Visible;

    // A round box — a radio button, and nothing else — is a ring rather than
    // four border rects around a filled rectangle: the border colour fills the
    // whole ellipse and the background is drawn inside it, inset by the border
    // width. Two fills rather than a stroke, because a stroked ellipse needs a
    // pen width and joins and this needs neither.
    if drawn && box_.round {
        let border = box_.style.border.top.used_width(box_.style.font_size);
        let outer = Rect {
            x,
            y,
            width: box_.rect.width,
            height: box_.rect.height,
        };
        let frame = box_.style.border.top.color.unwrap_or(box_.style.color);
        if border > 0.0 {
            list.items.push(DisplayItem::Ellipse {
                rect: outer,
                color: frame,
            });
        }
        list.items.push(DisplayItem::Ellipse {
            rect: Rect {
                x: outer.x + border,
                y: outer.y + border,
                width: (outer.width - border * 2.0).max(0.0),
                height: (outer.height - border * 2.0).max(0.0),
            },
            color: box_.style.background_color,
        });
    } else if drawn && !box_.style.background_color.is_transparent() {
        list.items.push(DisplayItem::Rect {
            rect: Rect {
                x,
                y,
                width: box_.rect.width,
                height: box_.rect.height,
            },
            color: box_.style.background_color,
        });
    }

    // The background image goes over the background colour and under
    // everything else, which is the order CSS 2.1 §14.2 specifies and the
    // reason a tile with transparent pixels shows the colour through it.
    if drawn
        && box_.style.background_image.is_some()
        && !is_canvas_background
        && let Some(node) = box_.node
    {
        list.items.push(DisplayItem::Tile {
            node,
            rect: Rect {
                x,
                y,
                width: box_.rect.width,
                height: box_.rect.height,
            },
            repeat: box_.style.background_repeat,
            position: box_.style.background_position,
        });
    }

    // A round box drew its own border above, as the ring.
    if drawn && !box_.round {
        paint_borders(box_, x, y, list);
    }

    // §18.4: outside the border box, the same on all four sides, and taking up
    // no room — so it is drawn after the border it surrounds and over whatever
    // happens to be beside the box. An outline that moved the page could not be
    // used to mark focus, which is what the property is for.
    if drawn {
        let width = box_.style.outline.used_width(box_.style.font_size);
        if width > 0.0 && box_.style.outline.style.is_visible() {
            let rect = Rect {
                x: x - width,
                y: y - width,
                width: box_.rect.width + width * 2.0,
                height: box_.rect.height + width * 2.0,
            };
            let color = box_.style.outline.color.unwrap_or(box_.style.color);
            let style = box_.style.outline.style;
            let sides = [
                (
                    Side::Top,
                    Rect {
                        height: width,
                        ..rect
                    },
                ),
                (
                    Side::Bottom,
                    Rect {
                        y: rect.y + rect.height - width,
                        height: width,
                        ..rect
                    },
                ),
                (
                    Side::Left,
                    Rect {
                        y: rect.y + width,
                        width,
                        height: (rect.height - width * 2.0).max(0.0),
                        ..rect
                    },
                ),
                (
                    Side::Right,
                    Rect {
                        x: rect.x + rect.width - width,
                        y: rect.y + width,
                        width,
                        height: (rect.height - width * 2.0).max(0.0),
                    },
                ),
            ];
            for (side, edge) in sides {
                push_border_side(list, &edge, style, width, side, color);
            }
        }
    }

    if drawn && let Some(node) = box_.replaced {
        list.items.push(DisplayItem::Image {
            node,
            placeholder: box_.replaced_image,
            rect: Rect {
                x: x + box_.content_origin.0,
                y: y + box_.content_origin.1,
                width: box_.content_width,
                height: (box_.rect.height - box_.content_origin.1 * 2.0).max(0.0),
            },
        });
    }

    // Not gated on `drawn`: the glyphs carry their own visibility, because a
    // line merges spans that may disagree about it. A hidden block's own runs
    // inherit `hidden` and drop out here individually, and a span that set
    // `visible` inside one survives — which a gate on the box would take with
    // it.
    if let Some(layout) = &box_.text {
        let content_x = x + box_.content_origin.0;
        let content_y = y + box_.content_origin.1;
        // A list box's chosen rows, under its own text rather than over it —
        // and so before the loop that draws the glyphs rather than after it,
        // which a child box could not be. A bar light enough to read dark text
        // through, because inverting the text would mean shaping the line
        // twice to say the one thing the bar already says.
        for row in &box_.chosen_rows {
            if let Some(line) = layout.lines.get(*row) {
                list.items.push(DisplayItem::Rect {
                    rect: Rect {
                        x,
                        y: content_y + line.y,
                        width: box_.rect.width,
                        height: line.baseline * 1.25,
                    },
                    color: CHOSEN_ROW,
                });
            }
        }
        for line in &layout.lines {
            let dx = line_offset(
                box_.style.text_align.against(box_.style.direction),
                line.width,
                line.available.min(box_.content_width),
            );
            // An inline box's own background and border, under everything the
            // line draws. Outermost first, which is the order the fragments
            // come in, so a nested span's background covers its parent's.
            for fragment in &line.boxes {
                let Some((_, style)) = layout
                    .inline_boxes
                    .iter()
                    .find(|(source, _)| *source == fragment.source)
                else {
                    continue;
                };
                paint_inline_box(fragment, style, content_x + dx, content_y, list);
            }
            // Rules go under the glyphs so an underline sitting close to a
            // descender is crossed by it rather than cutting through it.
            for rule in line.decorations.iter().filter(|rule| !rule.hidden) {
                list.items.push(DisplayItem::Rect {
                    rect: Rect {
                        x: content_x + dx + rule.x,
                        y: content_y + rule.y,
                        width: rule.width,
                        height: rule.thickness,
                    },
                    color: rule
                        .color
                        .map(|(r, g, b, a)| Color { r, g, b, a })
                        .unwrap_or(box_.style.color),
                });
            }
            for glyph in line.glyphs.iter().filter(|glyph| !glyph.hidden) {
                // A glyph's own colour wins: one line can hold spans of
                // different colours, and the block's colour is only the
                // default for text that did not come from a styled span.
                let color = glyph
                    .color
                    .map(|(r, g, b, a)| Color { r, g, b, a })
                    .unwrap_or(box_.style.color);
                list.items.push(DisplayItem::Glyph {
                    glyph: *glyph,
                    origin_x: content_x + dx,
                    origin_y: content_y,
                    color,
                });
            }
        }
    }

    // §9.9's painting order, as far as this engine models stacking. Children
    // are drawn by `(z-index, positioned)` rather than in tree order, which is
    // the difference between two overlapping absolute boxes landing the way
    // their author asked and landing in the order they happen to be written.
    //
    // The pair, and not the number alone, is what gets the middle of §9.9's
    // list right: a negative `z-index` paints *before* the in-flow content, an
    // `auto` or `0` positioned box paints *after* it, and an unpositioned box
    // sits between the two. Comparing only the number would put a positioned
    // `z-index: 0` and its unpositioned sibling in tree order, which is the one
    // pairing §9.9 actually reverses.
    //
    // A stable sort, so boxes that tie keep document order — which is both what
    // §9.9 says and what keeps a rendering reproducible (ADR-0005).
    //
    // §9.9.1 decides *which* boxes sort here. A stacking context sorts its own
    // children together with every context-forming box below them that no
    // nearer context has claimed — a positioned box with `z-index: auto` is
    // not one, so it seals nothing, and a `z-index: -1` descendant of it can
    // finally get behind an ancestor's background (#107). A box that is not a
    // context leaves those to whichever ancestor is, and paints only what is
    // left.
    let mut order: Vec<Stacked<'_>> = Vec::new();
    // What paints in place either way: content that is not positioned at all.
    // Its own positioned descendants are not its to paint — they were taken by
    // whichever ancestor is a stacking context.
    order.extend(
        box_.children
            .iter()
            .filter(|child| {
                !child.style.position.is_positioned()
                    && child.style.float == css::style::Float::None
            })
            .map(|child| Stacked { box_: child, x, y }),
    );
    if context {
        lift_positioned(box_, x, y, &mut order);
    }
    // Floats are lifted by whichever box `lift_floats` would have stopped at:
    // a stacking context, a positioned box, or a float. Each of those paints
    // its own subtree as a unit, so each has to gather the floats inside it or
    // they are gathered by nobody and painted by nobody.
    if context || box_.style.position.is_positioned() || box_.style.float != css::style::Float::None
    {
        lift_floats(box_, x, y, &mut order);
    }
    order.sort_by_key(|stacked| {
        let positioned = stacked.box_.style.position.is_positioned();
        // `z-index` means nothing on an unpositioned box, so it is not read
        // from one: honouring it there would invent a stacking order the spec
        // does not give.
        let z = if positioned {
            stacked.box_.style.z_index.unwrap_or(0)
        } else {
            0
        };
        // Appendix E's layers 3, 4 and 6, in that order: in-flow block boxes,
        // then non-positioned floats, then positioned boxes. A float above the
        // block boxes is what #165 is: a float that overhangs the bottom of its
        // container was painted with that container and then covered by the
        // next sibling's background.
        let layer = match () {
            () if positioned => 2,
            () if stacked.box_.style.float != css::style::Float::None => 1,
            () => 0,
        };
        (z, layer)
    });
    for stacked in order {
        let child = stacked.box_;
        let inside = forms_a_stacking_context(child);
        // A fixed subtree's items are pinned where they are emitted.
        // Everything inside it is positioned against the viewport already —
        // §10.1 made that the containing block — so what is left is to stop
        // the band rasteriser shifting it with the page, and the cleanest
        // place to mark that is where the subtree begins.
        if child.style.position == css::style::Position::Fixed {
            let from = list.items.len();
            paint_box(child, stacked.x, stacked.y, propagated, list, inside);
            list.pinned.push((from, list.items.len()));
            continue;
        }
        paint_box(child, stacked.x, stacked.y, propagated, list, inside);
    }

    if let Some(clip) = clip {
        clip_items(&mut list.items, clip_from, clip);
    }
}

/// A box waiting to be painted, with the origin of the parent that positions it.
///
/// A box lifted out of a `z-index: auto` subtree is painted a level or more
/// above where it sits, so it carries where it actually is: recomputing the
/// offset at the level that paints it would place it against the wrong parent.
#[derive(Clone, Copy)]
struct Stacked<'a> {
    box_: &'a LayoutBox,
    x: f32,
    y: f32,
}

/// Whether a box forms a stacking context of its own (§9.9.1).
///
/// A positioned box with a numeric `z-index` does. One with `z-index: auto`
/// does **not** — it makes a box in its parent's stacking context and nothing
/// more, so its own positioned descendants belong to that same context and
/// sort against its siblings rather than being sealed inside it.
///
/// `position: fixed` is the exception. CSS 2.1 does not say so in as many
/// words, every browser does it, and the suite tests for both halves at once:
/// `visuren/fixed-pos-stacking-001` has an `#absolute` half whose negative
/// descendant must escape and a `#fixed` half whose must not.
fn forms_a_stacking_context(box_: &LayoutBox) -> bool {
    box_.style.position == css::style::Position::Fixed
        || (box_.style.position.is_positioned() && box_.style.z_index.is_some())
}

/// Collects the positioned boxes inside `box_` that belong to an ancestor's
/// stacking context, in tree order.
///
/// *Every* positioned descendant, not only the ones that form a context of
/// their own: Appendix E sorts child contexts and `z-index: auto` positioned
/// descendants together, at the context, in tree order. Collecting only the
/// first kind leaves the second painted inside whatever parent happens to
/// contain it, which puts it at that parent's place in the order rather than
/// its own — `zindex/z-index-004` is two absolutely positioned siblings, one
/// with `z-index: 0` and one without, and getting this wrong paints them in
/// the wrong order.
///
/// `x` and `y` are the origin `box_`'s children are measured from. The walk
/// stops descending at a box that forms a context, because its own descendants
/// belong to *it* and go no further out.
fn lift_positioned<'a>(box_: &'a LayoutBox, x: f32, y: f32, out: &mut Vec<Stacked<'a>>) {
    for child in &box_.children {
        let positioned = child.style.position.is_positioned();
        if positioned {
            out.push(Stacked { box_: child, x, y });
        }
        if !forms_a_stacking_context(child) {
            lift_positioned(child, x + child.rect.x, y + child.rect.y, out);
        }
    }
}

/// Collects the floats inside `box_` that belong to an ancestor's stacking
/// context, in tree order.
///
/// The twin of [`lift_positioned`], and for the same reason one level down.
/// Appendix E paints floats as their own layer of the stacking context — above
/// in-flow block backgrounds, below positioned boxes — rather than with
/// whichever box happens to contain them. Painting one in place is invisible
/// while it fits inside its container and wrong the moment it does not: since
/// #41 a container is as tall as its in-flow content and no taller, so a float
/// routinely hangs out of the bottom of one, and the next sibling's background
/// then paints over the part that hangs out.
///
/// The walk stops at a box that forms a context, whose floats are its own, and
/// at a positioned box: Appendix E paints a `z-index: auto` positioned box as
/// though it started a context, so what floats inside it stays inside it.
///
/// It does not descend into a float either. A float establishes a block
/// formatting context, so a float within one is contained by it and has nothing
/// to hang out of.
fn lift_floats<'a>(box_: &'a LayoutBox, x: f32, y: f32, out: &mut Vec<Stacked<'a>>) {
    for child in &box_.children {
        if child.style.position.is_positioned() {
            continue;
        }
        if child.style.float != css::style::Float::None {
            out.push(Stacked { box_: child, x, y });
            continue;
        }
        if !forms_a_stacking_context(child) {
            lift_floats(child, x + child.rect.x, y + child.rect.y, out);
        }
    }
}

/// Turns a `clip` into a rectangle on the canvas.
///
/// Every side is measured from the border box's top-left corner, `right` and
/// `bottom` included — they are offsets, not insets from the far edges, which
/// is the one thing about this property that is easy to get backwards. `auto`
/// on a side leaves that border edge alone.
fn resolve_clip(clip: css::style::ClipRect, x: f32, y: f32, rect: Rect, font_size: f32) -> Rect {
    let offset = |length: Option<css::Length>, fallback: f32| {
        length.map_or(fallback, |length| length.to_px(font_size, 0.0))
    };
    let top = y + offset(clip.top, 0.0);
    let left = x + offset(clip.left, 0.0);
    let bottom = y + offset(clip.bottom, rect.height);
    let right = x + offset(clip.right, rect.width);
    Rect {
        x: left,
        y: top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    }
}

/// Narrows every item in `items` to `clip`, dropping what falls outside.
///
/// Rectangles are intersected. Glyphs go whole: one straddling the edge is
/// kept rather than cut through, which is the same trade the form controls
/// make when they clip an over-long value, and which keeps this from needing
/// a clip path in the rasteriser.
fn clip_items(items: &mut Vec<DisplayItem>, from: usize, clip: Rect) {
    let mut tail = items.split_off(from);
    tail.retain_mut(|item| match item {
        // A tile's `rect` is the area it repeats within, so narrowing it
        // clips the tiling without moving where the tiles start — the anchor
        // that decides the phase is carried separately, for exactly this
        // reason.
        // An ellipse is narrowed by narrowing the box it is inscribed in,
        // which squashes it rather than cutting it. That is wrong in general
        // and right for the only thing that draws one: a radio button is small
        // enough that a clip either misses it or removes it.
        DisplayItem::Rect { rect, .. }
        | DisplayItem::Ellipse { rect, .. }
        | DisplayItem::Image { rect, .. }
        | DisplayItem::Tile { rect, .. } => match intersect(*rect, clip) {
            Some(narrowed) => {
                *rect = narrowed;
                true
            }
            None => false,
        },
        DisplayItem::Glyph {
            glyph,
            origin_x,
            origin_y,
            ..
        } => {
            let gx = *origin_x + glyph.x;
            let gy = *origin_y + glyph.y;
            gx >= clip.x && gx < clip.x + clip.width && gy >= clip.y && gy <= clip.y + clip.height
        }
    });
    items.append(&mut tail);
}

/// The overlap of two rectangles, or `None` where they do not overlap.
fn intersect(rect: Rect, clip: Rect) -> Option<Rect> {
    let x = rect.x.max(clip.x);
    let y = rect.y.max(clip.y);
    let right = (rect.x + rect.width).min(clip.x + clip.width);
    let bottom = (rect.y + rect.height).min(clip.y + clip.height);
    (right > x && bottom > y).then_some(Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

/// Emits one line's worth of a non-replaced inline box: the background and
/// border it draws where it crosses that line (§8.4).
///
/// An inline box broken over three lines draws three of these. The horizontal
/// margin, border and padding belong to the whole box rather than to each
/// fragment, so they appear on the first fragment and the last — `opens` and
/// `closes` — and the stretch in between runs edge to edge. The vertical ones
/// are on every fragment, and overflow the line box rather than growing it
/// (§10.6.1): a highlighted phrase with 4px of padding is 8px taller than its
/// text wherever it appears, and the lines around it do not move apart to make
/// room. That is what browsers do and what makes inline padding a thing authors
/// use sparingly.
fn paint_inline_box(
    fragment: &text::InlineBoxFragment,
    style: &css::style::ComputedStyle,
    origin_x: f32,
    origin_y: f32,
    list: &mut DisplayList,
) {
    let font_size = style.font_size;
    // Percentages on an inline box's padding resolve against the containing
    // block's width, which is not known here. They are rare enough on a span
    // that a basis of zero — which reads them as nothing — is better than a
    // basis that is wrong in a way nobody can see the cause of.
    let px = |length: css::value::Length| length.to_px(font_size, 0.0);
    let border = &style.border;
    let (border_top, border_bottom) = (
        border.top.used_width(font_size),
        border.bottom.used_width(font_size),
    );
    // Which *physical* side each of the box's two logical ends is on. §8.4 puts
    // the start side on the first fragment and the end side on the last, and
    // §9.10 decides which is which: in right-to-left text a box opens on the
    // right and closes on the left. Painting `opens` as the left unconditionally
    // drew both sides on the first fragment of an rtl box and neither on the
    // last (#113).
    let rtl = style.direction == css::style::Direction::Rtl;
    let has_left = if rtl { fragment.closes } else { fragment.opens };
    let has_right = if rtl { fragment.opens } else { fragment.closes };
    // The reserved stretch starts at the margin's outer edge, so the border box
    // is inside it by whichever margins are on this fragment.
    let left = origin_x + fragment.x + if has_left { px(style.margin.left) } else { 0.0 };
    let right = origin_x + fragment.x + fragment.width
        - if has_right {
            px(style.margin.right)
        } else {
            0.0
        };
    let top = origin_y + fragment.y - px(style.padding.top) - border_top;
    let bottom = origin_y + fragment.y + fragment.height + px(style.padding.bottom) + border_bottom;
    let rect = Rect {
        x: left,
        y: top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    };
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }

    if !style.background_color.is_transparent() {
        list.items.push(DisplayItem::Rect {
            rect,
            color: style.background_color,
        });
    }

    // Top and bottom run the length of the fragment; the two sides are drawn
    // only where the box actually begins and ends.
    let color_of = |side: &css::style::BorderSide| side.color.unwrap_or(style.color);
    let sides = [
        (
            &border.top,
            Side::Top,
            border_top,
            true,
            Rect {
                height: border_top,
                ..rect
            },
        ),
        (
            &border.bottom,
            Side::Bottom,
            border_bottom,
            true,
            Rect {
                y: rect.y + rect.height - border_bottom,
                height: border_bottom,
                ..rect
            },
        ),
        (
            &border.left,
            Side::Left,
            border.left.used_width(font_size),
            has_left,
            Rect {
                y: rect.y + border_top,
                width: border.left.used_width(font_size),
                height: (rect.height - border_top - border_bottom).max(0.0),
                ..rect
            },
        ),
        (
            &border.right,
            Side::Right,
            border.right.used_width(font_size),
            has_right,
            Rect {
                x: rect.x + rect.width - border.right.used_width(font_size),
                y: rect.y + border_top,
                width: border.right.used_width(font_size),
                height: (rect.height - border_top - border_bottom).max(0.0),
            },
        ),
    ];
    for (side, which, thickness, present, edge) in sides {
        if present && side.style.is_visible() && thickness > 0.0 {
            push_border_side(list, &edge, side.style, thickness, which, color_of(side));
        }
    }
}

/// Emits the four border edges of a box.
///
/// Corners are mitred by letting the top and bottom edges span the full width
/// and insetting the side edges. That is exact for a uniform border and only
/// visibly wrong where two edges of different colours meet, which CSS 2.1
/// resolves with a diagonal join — worth doing when a page needs it, not before.
fn paint_borders(box_: &LayoutBox, x: f32, y: f32, list: &mut DisplayList) {
    let font_size = box_.style.font_size;
    let border = &box_.style.border;
    let (width, height) = (box_.rect.width, box_.rect.height);

    let top = border.top.used_width(font_size);
    let right = border.right.used_width(font_size);
    let bottom = border.bottom.used_width(font_size);
    let left = border.left.used_width(font_size);

    let color_of = |side: &css::style::BorderSide| side.color.unwrap_or(box_.style.color);

    if border.top.style.is_visible() && top > 0.0 {
        // A `<fieldset>`'s rule stops either side of its `<legend>`, so the top
        // side is drawn as the two pieces the gap leaves rather than as one
        // run. Each piece is a border side in its own right: a `groove` still
        // has to light and shade from the same direction on both, which is why
        // this splits the rect and calls the same code twice instead of
        // painting one side and rubbing a hole in it.
        let (from, to) = match box_.top_border_gap {
            Some((from, to)) => (from.clamp(0.0, width), to.clamp(0.0, width)),
            None => (width, width),
        };
        for piece in [(0.0, from), (to, width)] {
            let (start, end) = piece;
            if end <= start {
                continue;
            }
            push_border_side(
                list,
                &Rect {
                    x: x + start,
                    y,
                    width: end - start,
                    height: top,
                },
                border.top.style,
                top,
                Side::Top,
                color_of(&border.top),
            );
        }
    }
    if border.bottom.style.is_visible() && bottom > 0.0 {
        push_border_side(
            list,
            &Rect {
                x,
                y: y + height - bottom,
                width,
                height: bottom,
            },
            border.bottom.style,
            bottom,
            Side::Bottom,
            color_of(&border.bottom),
        );
    }
    let side_height = (height - top - bottom).max(0.0);
    if border.left.style.is_visible() && left > 0.0 {
        push_border_side(
            list,
            &Rect {
                x,
                y: y + top,
                width: left,
                height: side_height,
            },
            border.left.style,
            left,
            Side::Left,
            color_of(&border.left),
        );
    }
    if border.right.style.is_visible() && right > 0.0 {
        push_border_side(
            list,
            &Rect {
                x: x + width - right,
                y: y + top,
                width: right,
                height: side_height,
            },
            border.right.style,
            right,
            Side::Right,
            color_of(&border.right),
        );
    }
}

/// Which edge of the box a border side is.
///
/// More than the axis, because the three-dimensional styles need to know which
/// edge they are on: what makes `outset` look raised is that the top and left
/// catch the light while the bottom and right fall into shadow, and an engine
/// that only knew "horizontal" would light both ends of the box the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    /// Whether the side runs left-to-right.
    fn is_horizontal(self) -> bool {
        matches!(self, Side::Top | Side::Bottom)
    }

    /// Whether this edge is the one a light source above and to the left would
    /// strike. `inset` and `outset` are this predicate and a pair of shades.
    fn faces_the_light(self) -> bool {
        matches!(self, Side::Top | Side::Left)
    }
}

/// A shade of the border's colour, for the styles that fake a light source.
///
/// The specification says only that `inset` "looks as though it were embedded
/// in the canvas" and leaves the rest to the UA, so the two shades are a
/// choice: the declared colour for the lit edges, and half of each channel for
/// the shadowed ones. Half is what makes the era's default grey `outset` button
/// look like a button rather than like a slightly uneven rectangle.
fn shaded(color: Color, factor: f32) -> Color {
    let channel = |value: u8| (value as f32 * factor).clamp(0.0, 255.0) as u8;
    Color {
        r: channel(color.r),
        g: channel(color.g),
        b: channel(color.b),
        a: color.a,
    }
}

/// How much darker a shadowed edge is drawn.
/// The bar behind a chosen row of a list box.
///
/// UA furniture rather than anything the page asked for, like the control's own
/// border — and it cannot come from the stylesheet the way that border does,
/// because a `<select>`'s options are hidden and its rows are lines of one text
/// layout rather than boxes the cascade can reach.
///
/// Light on purpose. A saturated bar would need the text on it inverted to stay
/// legible, which would mean shaping that line a second time in a second colour
/// to say the one thing the bar already says.
const CHOSEN_ROW: Color = Color::rgb(0xcf, 0xdd, 0xee);

const SHADOW: f32 = 0.5;

/// A band across a side's *thickness*, measured from its outer edge.
///
/// `from` and `to` are fractions, so `(0.0, 0.5)` is the outer half of the
/// side whichever edge it is — the outer half of a bottom border is its lower
/// half, and of a top border its upper one.
fn band(side: &Rect, edge: Side, thickness: f32, from: f32, to: f32) -> Rect {
    let (near, far) = (thickness * from, thickness * to);
    match edge {
        Side::Top => Rect {
            y: side.y + near,
            height: far - near,
            ..*side
        },
        Side::Bottom => Rect {
            y: side.y + side.height - far,
            height: far - near,
            ..*side
        },
        Side::Left => Rect {
            x: side.x + near,
            width: far - near,
            ..*side
        },
        Side::Right => Rect {
            x: side.x + side.width - far,
            width: far - near,
            ..*side
        },
    }
}

/// How long a dash or a dot wants to be, as a multiple of the border's width.
///
/// CSS 2.1 §8.5.3 says a dotted border is "a series of dots" and a dashed one
/// "a series of short line segments", and stops there — the pattern is the UA's
/// to choose, which is why no two browsers match. Square dots the width of the
/// border, and dashes three times that, are what the era's browsers converged
/// on closely enough that a page authored against one does not look broken
/// under this.
const DOT_PERIOD: f32 = 1.0;
const DASH_PERIOD: f32 = 3.0;

/// Pushes one side of a border, as one rectangle or as a row of them.
fn push_border_side(
    list: &mut DisplayList,
    side: &Rect,
    style: css::style::BorderStyle,
    thickness: f32,
    edge: Side,
    color: Color,
) {
    use css::style::BorderStyle;

    let lit = color;
    let dark = shaded(color, SHADOW);

    let wanted = match style {
        BorderStyle::Dotted => DOT_PERIOD * thickness,
        BorderStyle::Dashed => DASH_PERIOD * thickness,
        // Two lines with a gap, each a third of the border. Below three pixels
        // there is no room for that, and the honest answer is the solid line
        // the author would otherwise have got.
        BorderStyle::Double if thickness >= 3.0 => {
            for (from, to) in [(0.0, 1.0 / 3.0), (2.0 / 3.0, 1.0)] {
                list.items.push(DisplayItem::Rect {
                    rect: band(side, edge, thickness, from, to),
                    color,
                });
            }
            return;
        }
        // Embedded or raised: one flat shade per edge, lit from above and left.
        BorderStyle::Inset | BorderStyle::Outset => {
            let raised = style == BorderStyle::Outset;
            let light = edge.faces_the_light() == raised;
            list.items.push(DisplayItem::Rect {
                rect: *side,
                color: if light { lit } else { dark },
            });
            return;
        }
        // Carved or proud: the same idea twice across the thickness, with the
        // halves the other way round. A groove is an inset outer half around an
        // outset inner one, which is what gives it its lip; a ridge is the
        // reverse. Needs two pixels to show at all.
        BorderStyle::Groove | BorderStyle::Ridge if thickness >= 2.0 => {
            let proud = style == BorderStyle::Ridge;
            for (from, to, outer) in [(0.0, 0.5, true), (0.5, 1.0, false)] {
                // The outer half is lit when this edge faces the light and the
                // border stands proud; every other combination flips it, which
                // is the whole of the effect.
                let light = edge.faces_the_light() == (proud == outer);
                list.items.push(DisplayItem::Rect {
                    rect: band(side, edge, thickness, from, to),
                    color: if light { lit } else { dark },
                });
            }
            return;
        }
        // `solid`, and anything too thin to show the pattern it asked for.
        _ => {
            list.items.push(DisplayItem::Rect { rect: *side, color });
            return;
        }
    };

    let length = if edge.is_horizontal() {
        side.width
    } else {
        side.height
    };
    if !(length.is_finite() && length > 0.0 && wanted.is_finite() && wanted > 0.0) {
        return;
    }

    // `n` dashes with `n - 1` equal gaps between them, which is what makes the
    // run start and end flush with the corners. Solving `length = n·d + (n−1)·d`
    // for the dash size gives `d = length / (2n − 1)`, so the pattern is
    // stretched a little rather than leaving a ragged end — the thing that
    // makes a hand-rolled dashed border look wrong is a half dash at one end.
    let count = (((length + wanted) / (2.0 * wanted)).round() as i32).max(1);
    let dash = length / (2 * count - 1) as f32;
    // One dash spanning the whole side is a solid line, which is the honest
    // degradation for a side too short to show a pattern at all.
    for index in 0..count {
        let offset = index as f32 * 2.0 * dash;
        let rect = if edge.is_horizontal() {
            Rect {
                x: side.x + offset,
                width: dash,
                ..*side
            }
        } else {
            Rect {
                y: side.y + offset,
                height: dash,
                ..*side
            }
        };
        list.items.push(DisplayItem::Rect { rect, color });
    }
}

/// Rasterises a display list into a new pixmap.
///
/// Returns `None` only for a zero-sized canvas.
pub fn rasterise(
    list: &DisplayList,
    fonts: &mut FontStore,
    images: &ImageStore,
    width: u32,
    height: u32,
) -> Option<Pixmap> {
    rasterise_band(list, fonts, images, width, 0.0, 0.0, height)
}

/// Rasterises the rows `[top, top + height)` of a document, starting `left`
/// pixels in from its left edge.
///
/// The display list is in document coordinates and does not change between
/// bands — it is built once from the layout, and drawing a band is a matter of
/// where the rows are taken from. That is what makes a band cheap: no parse, no
/// cascade, no layout, just paint.
///
/// Items are shifted by `left` and `top` as they are drawn rather than the list
/// being rewritten, so nothing is allocated per band and the drawable-range
/// check still sees the coordinate that actually reaches the rasteriser.
///
/// `left` is how a page wider than its window is read (#204). The two axes are
/// the same operation and deliberately share one: a band is a rectangle of the
/// document, and there was never a reason beyond habit for its horizontal edge
/// to be fixed at zero.
pub fn rasterise_band(
    list: &DisplayList,
    fonts: &mut FontStore,
    images: &ImageStore,
    width: u32,
    left: f32,
    top: f32,
    height: u32,
) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(width.max(1), height.max(1))?;
    // Clearing to the canvas colour rather than white is what carries the
    // page's background down past the end of its content.
    let canvas = list.canvas;
    pixmap.fill(tiny_skia::Color::from_rgba8(
        canvas.r, canvas.g, canvas.b, canvas.a,
    ));

    // The canvas tile goes over the canvas colour and under everything else.
    if let Some((node, repeat, position, area)) = list.canvas_image
        && let Some(image) = images.get(&ImageKey::background(node))
    {
        // The canvas is the whole document, so the anchor is measured against
        // the document rather than the band — otherwise the tiling phase, and
        // any percentage in the position, would move every time the reader
        // scrolled.
        let full = Rect {
            x: 0.0,
            y: 0.0,
            width: left + pixmap.width() as f32,
            height: top + pixmap.height() as f32,
        };
        let anchor = anchor_of(&area, position, image);
        if let Some(slice) = banded(&full, top, pixmap.height() as f32) {
            let slice = shifted(&slice, left, top);
            if drawable(&slice) {
                tile_image(
                    &mut pixmap,
                    image,
                    &slice,
                    (anchor.0 - left, anchor.1 - top),
                    repeat,
                );
            }
        }
    }

    // In one pass and in order, with the pinned runs drawn at no shift at all:
    // their coordinates are already the window's, which is the whole of what
    // `position: fixed` means once the containing block is the viewport (#108).
    for (at, item) in list.items.iter().enumerate() {
        // A pinned item is not shifted along *either* axis. Its coordinates are
        // the window's, and a fixed sidebar that slid away when the reader
        // scrolled sideways would be no more fixed than one that slid away when
        // they scrolled down (#108, #204).
        let (dx, dy) = if list.is_pinned(at) {
            (0.0, 0.0)
        } else {
            (left, top)
        };
        draw_items(
            &mut pixmap,
            fonts,
            images,
            std::slice::from_ref(item),
            dx,
            dy,
        );
    }
    Some(pixmap)
}

/// Draws a run of display items into a band, shifting them by `left` and `top`.
///
/// Called twice: once for the page's own items, and once for the pinned ones
/// with a shift of zero. That second call is the whole of `position: fixed`'s
/// painting half — a page is laid out once and a band is a slice of it, so an
/// item that must not scroll is simply an item that is not shifted (#108).
fn draw_items(
    pixmap: &mut Pixmap,
    fonts: &mut FontStore,
    images: &ImageStore,
    items: &[DisplayItem],
    left: f32,
    top: f32,
) {
    for item in items {
        // Nothing beyond the drawable range is drawn at all. See `MAX_COORD`:
        // this is the one place every item passes through, so it is the one
        // place the check has to be.
        match item {
            DisplayItem::Rect { rect, color } => {
                let rect = shifted(rect, left, top);
                if drawable(&rect) {
                    fill_rect(pixmap, &rect, *color);
                }
            }
            DisplayItem::Ellipse { rect, color } => {
                let rect = shifted(rect, left, top);
                if drawable(&rect) {
                    fill_ellipse(pixmap, &rect, *color);
                }
            }
            DisplayItem::Image {
                node,
                rect,
                placeholder,
            } => {
                let rect = shifted(rect, left, top);
                if drawable(&rect) {
                    match images.get(&ImageKey::content(*node)) {
                        Some(image) => draw_image(pixmap, image, &rect),
                        None if *placeholder => draw_missing(pixmap, fonts, &rect),
                        None => {}
                    }
                }
            }
            DisplayItem::Tile {
                node,
                rect,
                repeat,
                position,
            } => {
                if let Some(image) = images.get(&ImageKey::background(*node))
                    && let Some(slice) = banded(rect, top, pixmap.height() as f32)
                {
                    // The anchor comes from the element's own box in document
                    // coordinates and is then shifted with everything else, so
                    // a band draws the tiles a whole-page render would have.
                    let anchor = anchor_of(rect, *position, image);
                    let slice = shifted(&slice, left, top);
                    if drawable(&slice) {
                        tile_image(
                            pixmap,
                            image,
                            &slice,
                            (anchor.0 - left, anchor.1 - top),
                            *repeat,
                        );
                    }
                }
            }
            DisplayItem::Glyph {
                glyph,
                origin_x,
                origin_y,
                color,
            } => {
                let origin_x = *origin_x - left;
                let origin_y = *origin_y - top;
                if in_range(origin_x) && in_range(origin_y) {
                    draw_glyph(pixmap, fonts, glyph, origin_x, origin_y, *color);
                }
            }
        }
    }
}

/// The same rectangle, moved into a band's coordinates.
fn shifted(rect: &Rect, left: f32, top: f32) -> Rect {
    Rect {
        x: rect.x - left,
        y: rect.y - top,
        ..*rect
    }
}

/// How far to the right the document's painted content reaches.
///
/// The scrollable width, answered from the display list because that is the one
/// place every drawn thing passes through — a box, a picture, a tile and a
/// glyph all end up here, and anything that does not is by definition not on
/// the screen to be scrolled to.
///
/// Pinned items are left out. Their coordinates are the window's rather than
/// the document's (#108), so a `position: fixed` bar the width of the viewport
/// would otherwise claim to be content sitting at whatever the reader had
/// already scrolled to.
///
/// Clamped to the drawable range: a page can name a coordinate far outside it,
/// and `draw_items` already declines to draw one. A scroll range that ran to
/// 10^9 would be a scrollbar with no thumb and a document that never ends.
pub fn content_width(list: &DisplayList) -> f32 {
    let mut widest: f32 = 0.0;
    for (at, item) in list.items.iter().enumerate() {
        if list.is_pinned(at) {
            continue;
        }
        let right = match item {
            DisplayItem::Rect { rect, .. }
            | DisplayItem::Ellipse { rect, .. }
            | DisplayItem::Image { rect, .. }
            | DisplayItem::Tile { rect, .. } => rect.x + rect.width,
            // A glyph is positioned within its run and then advances, and
            // neither offset is in the run's rectangle: a line of `<pre>` runs
            // past the box holding it, which is exactly the case #204 is about.
            DisplayItem::Glyph {
                glyph, origin_x, ..
            } => origin_x + glyph.x + glyph.advance,
        };
        // `>` rather than `max`: a NaN coordinate fails the comparison and is
        // skipped, where `max` would carry it out of here and into a scroll
        // range nothing could clamp.
        if right > widest {
            widest = right;
        }
    }
    widest.clamp(0.0, MAX_COORD)
}

/// The rows of a rectangle a band can see, still in document coordinates.
///
/// This used to do more, and the more was a workaround. Tiling started at the
/// rectangle's top-left corner, so a band had to be handed a rectangle whose
/// top sat on a tile boundary or the pattern would jump every time the reader
/// scrolled. `tile_image` now takes the anchor separately and works out which
/// tiles overlap the clip, so the phase is carried by the anchor — which is
/// where `background-position` had to put it anyway — and this is left doing
/// the one thing it should: saying which rows are worth drawing.
///
/// Cost still depends on it. `tile_image` steps one image at a time, so a
/// clip as tall as the document is how a one-pixel tile on a long page becomes
/// millions of draws. Clipping to the band bounds that by the band's height
/// however tall the element is.
fn banded(rect: &Rect, top: f32, band_height: f32) -> Option<Rect> {
    let visible_top = rect.y.max(top);
    let visible_bottom = (rect.y + rect.height).min(top + band_height);
    if visible_bottom <= visible_top {
        return None;
    }
    Some(Rect {
        x: rect.x,
        y: visible_top,
        width: rect.width,
        height: visible_bottom - visible_top,
    })
}

/// Where the image's top-left corner lands, in the same space as `rect`.
///
/// The whole of `background-position` at paint time: the property is stored as
/// a length or a percentage, and the percentage cannot be resolved until the
/// image's size is known, which is here and not in the cascade.
fn anchor_of(
    rect: &Rect,
    position: css::style::BackgroundPosition,
    image: &DecodedImage,
) -> (f32, f32) {
    (
        rect.x + css::style::background_offset(position.x, rect.width, image.width()),
        rect.y + css::style::background_offset(position.y, rect.height, image.height()),
    )
}

/// Furthest from the canvas a coordinate may be and still be worth drawing.
///
/// tiny-skia works in `i32` pixel space and builds rectangles that must not
/// overflow it. A coordinate arriving as infinity — which is what
/// `margin: 1e40px` computes to, since `1e40` does not fit in an `f32` —
/// saturates to `i32::MAX` on the cast, and the first addition inside the
/// library after that panics. So does a merely enormous but finite one.
///
/// Ten million pixels is roughly a thousand screens in either direction:
/// nothing this far out is visible, and a page is not entitled to crash the
/// browser by asking for it.
const MAX_COORD: f32 = 1e7;

/// Whether a single coordinate can be drawn at.
fn in_range(value: f32) -> bool {
    value.is_finite() && value.abs() <= MAX_COORD
}

/// Whether a rectangle is worth handing to the rasteriser.
///
/// Also bounds the tile loop, which steps across a rectangle one image at a
/// time: an enormous rectangle is a hang even where it is not a panic.
fn drawable(rect: &Rect) -> bool {
    in_range(rect.x)
        && in_range(rect.y)
        && in_range(rect.width)
        && in_range(rect.height)
        && in_range(rect.x + rect.width)
        && in_range(rect.y + rect.height)
}

/// What the placeholder for an image that did not arrive says (#118).
pub const MISSING_IMAGE_LABEL: &str = "Load image";

/// Text size in that placeholder.
const MISSING_TEXT: f32 = 12.0;

/// Room left around the label, so it does not touch the outline.
const MISSING_PAD: f32 = 4.0;

/// Smallest box that is outlined at all.
const MISSING_OUTLINE_MIN: f32 = 12.0;

/// Draws the box where an image was going to be, and did not arrive.
///
/// Not a broken-image icon and not nothing. Nothing is what this used to draw,
/// and on a page whose pictures are all on a CDN the reader got a screenful of
/// holes with no way to tell a refused image from a dead server — the report
/// that became issue #109 was exactly that, and it was a browser working as
/// designed being indistinguishable from a broken one.
///
/// It says `Load image` because that is what pressing it does. It deliberately
/// does *not* say "blocked": this side of the renderer boundary has no business
/// knowing whether a resource was refused by policy or was merely missing, and
/// the wire makes the two identical on purpose so a compromised renderer cannot
/// probe the user's configuration (ADR-0012). The parent knows which it was and
/// says so in the chrome.
///
/// Fixed colours rather than the page's. There is no theme on this side, and a
/// placeholder tinted to blend into the document would be a hole again — this
/// is a control sitting on someone else's page and reads better for saying so.
fn draw_missing(pixmap: &mut Pixmap, fonts: &mut FontStore, rect: &Rect) {
    if rect.width < MISSING_OUTLINE_MIN || rect.height < MISSING_OUTLINE_MIN {
        return;
    }
    const PLATE: Color = Color::rgb(0xf4, 0xf4, 0xf2);
    const EDGE: Color = Color::rgb(0x9a, 0x9a, 0x96);
    const INK: Color = Color::rgb(0x44, 0x44, 0x44);

    fill_rect(pixmap, rect, EDGE);
    fill_rect(
        pixmap,
        &Rect {
            x: rect.x + 1.0,
            y: rect.y + 1.0,
            width: rect.width - 2.0,
            height: rect.height - 2.0,
        },
        PLATE,
    );
    // Sans, at a fixed size, whatever the page set. This is the browser
    // speaking rather than the document, and a placeholder that inherited the
    // author's face would read as content — which is the one thing it is not.
    let style = css::style::ComputedStyle {
        font_size: MISSING_TEXT,
        line_height: css::style::LineHeight::Px(MISSING_TEXT * 1.2),
        font_family: css::style::FontStack {
            families: Vec::new(),
            generic: css::style::GenericFamily::SansSerif,
        },
        ..css::style::ComputedStyle::default()
    };
    let height = MISSING_TEXT * 1.2;
    let layout = fonts.layout(MISSING_IMAGE_LABEL, &style, f32::MAX);
    let Some(line) = layout.lines.first() else {
        return;
    };
    // Measured, not guessed at with a minimum size. A great deal of the era's
    // markup is 1x1 spacers and 10px bullets, and two words written across
    // those would turn a page of invisible scaffolding into a page of smudges
    // — which is the failure mode of every broken-image icon that ever
    // shipped. A fixed threshold got this nearly right and then clipped the
    // label at both ends in an 80x30 box, which is the size half the era's
    // thumbnails are.
    if line.width + MISSING_PAD * 2.0 > rect.width || height + MISSING_PAD > rect.height {
        return;
    }

    // Centred in the box rather than pinned to a corner: the box is whatever
    // size the author's `width` and `height` asked for, and a label in the
    // middle of it is the one position that reads the same at every size.
    //
    // `origin_y` is the *line's* top, not its baseline: a glyph carries its own
    // offset within the run, the way the ordinary text path passes the line
    // box's y and lets `draw_glyph` add the rest. Passing a baseline here put
    // the words a whole ascent too low, hard against the bottom edge.
    let x = rect.x + (rect.width - line.width) / 2.0;
    let y = rect.y + (rect.height - height) / 2.0;
    for glyph in &line.glyphs {
        draw_glyph(pixmap, fonts, glyph, x, y, INK);
    }
}

/// Draws an image scaled into `rect`.
fn draw_image(pixmap: &mut Pixmap, image: &DecodedImage, rect: &Rect) {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let scale_x = rect.width / image.width().max(1.0);
    let scale_y = rect.height / image.height().max(1.0);
    // Bilinear rather than nearest: the era's pages routinely scaled images
    // with width/height attributes, and nearest-neighbour makes that look
    // broken rather than merely resized.
    let paint = PixmapPaint {
        quality: tiny_skia::FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(
        0,
        0,
        image.pixmap.as_ref(),
        &paint,
        Transform::from_translate(rect.x / scale_x, rect.y / scale_y).post_scale(scale_x, scale_y),
        None,
    );
}

/// Where one tile begins, and how many are needed to cover `clip` along an axis.
///
/// The anchor is where `background-position` put the image, which is not
/// necessarily inside the clip and not necessarily inside the box: a repeating
/// background tiles outwards from it in both directions, so the first tile that
/// shows is usually one at a negative index.
///
/// `None` where the arithmetic leaves the range worth drawing in — a tile count
/// that is infinite or negative is a pathological input, not a background.
fn tile_span(anchor: f32, size: f32, start: f32, length: f32, tiles: bool) -> Option<(f32, u32)> {
    if !tiles {
        // One tile, wherever the anchor is. It may miss the clip entirely, and
        // the mask takes care of that.
        return Some((anchor, 1));
    }
    let index = ((start - anchor) / size).floor();
    if !index.is_finite() {
        return None;
    }
    let first = anchor + index * size;
    let count = ((start + length - first) / size).ceil();
    if !count.is_finite() || count < 0.0 {
        return None;
    }
    Some((first, count as u32))
}

/// Tiles a background image over `clip`, with one tile's corner at `anchor`.
///
/// A background image is never scaled — that is what distinguishes it from a
/// content image, and it is why a 20-pixel tile fills a page rather than being
/// stretched across it.
///
/// The anchor and the clip are separate arguments because
/// `background-position` separated them. They used to be the same rectangle,
/// on the assumption that tiling starts at the box's top-left corner; a
/// position of `50% 20px` breaks that in both directions at once, since the
/// phase moves and the first visible tile can begin above and to the left of
/// the box.
///
/// Keeping them apart also bounds the work better than the old arrangement did.
/// Tiles are counted from the *clip*, which for a banded render is the handful
/// of rows on screen rather than the whole document, so a one-pixel tile on a
/// very long page costs a band's worth of draws instead of a page's.
fn tile_image(
    pixmap: &mut Pixmap,
    image: &DecodedImage,
    clip: &Rect,
    anchor: (f32, f32),
    repeat: css::style::BackgroundRepeat,
) {
    let (width, height) = (image.width(), image.height());
    if clip.width <= 0.0 || clip.height <= 0.0 || width < 1.0 || height < 1.0 {
        return;
    }

    let (tile_x, tile_y) = repeat.axes();
    let Some((first_x, columns)) = tile_span(anchor.0, width, clip.x, clip.width, tile_x) else {
        return;
    };
    let Some((first_y, rows)) = tile_span(anchor.1, height, clip.y, clip.height, tile_y) else {
        return;
    };

    let bounds = tiny_skia::IntRect::from_xywh(
        clip.x.floor() as i32,
        clip.y.floor() as i32,
        clip.width.ceil() as u32,
        clip.height.ceil() as u32,
    );
    let Some(mask) = bounds.and_then(|bounds| {
        let mut mask = tiny_skia::Mask::new(pixmap.width(), pixmap.height())?;
        let mut builder = PathBuilder::new();
        builder.push_rect(bounds.to_rect());
        let path = builder.finish()?;
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
        Some(mask)
    }) else {
        return;
    };

    let paint = PixmapPaint::default();
    for row in 0..rows {
        for column in 0..columns {
            pixmap.draw_pixmap(
                (first_x + column as f32 * width).round() as i32,
                (first_y + row as f32 * height).round() as i32,
                image.pixmap.as_ref(),
                &paint,
                Transform::identity(),
                Some(&mask),
            );
        }
    }
}

/// Fills the ellipse inscribed in `rect`.
///
/// Anti-aliased, where a rectangle is not: a circle drawn without it is a
/// staircase, and the whole point of this primitive is that the shape reads.
/// Determinism across platforms is unaffected — the rasteriser is `tiny-skia`
/// and the anti-aliasing is its own arithmetic, not the host's (ADR-0005).
fn fill_ellipse(pixmap: &mut Pixmap, rect: &Rect, color: Color) {
    if rect.width <= 0.0 || rect.height <= 0.0 || color.is_transparent() {
        return;
    }
    let Some(oval) = tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height) else {
        return;
    };
    let mut builder = PathBuilder::new();
    builder.push_oval(oval);
    let Some(path) = builder.finish() else { return };

    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn fill_rect(pixmap: &mut Pixmap, rect: &Rect, color: Color) {
    if rect.width <= 0.0 || rect.height <= 0.0 || color.is_transparent() {
        return;
    }
    let mut builder = PathBuilder::new();
    builder.push_rect(
        tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)
            .unwrap_or_else(|| tiny_skia::Rect::from_xywh(0.0, 0.0, 1.0, 1.0).expect("unit rect")),
    );
    let Some(path) = builder.finish() else { return };

    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = false;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// The glyph every font reserves for "no glyph for this character".
const NOTDEF: u16 = 0;

/// How much of the font size a tofu box stands above the baseline.
///
/// Cap height rather than the full ascent: a box drawn to the ascender sits
/// noticeably higher than the letters beside it, and a line mixing covered and
/// uncovered script should read as one line.
const TOFU_HEIGHT: f32 = 0.66;

/// How much of the advance is left clear either side, so a run of them reads as
/// separate boxes rather than as a bar.
const TOFU_SIDE: f32 = 0.12;

/// How thick the box's edge is drawn, as a share of the font size, and the
/// floor it never goes below.
const TOFU_EDGE: f32 = 0.06;
const TOFU_EDGE_MIN: f32 = 1.0;

/// Below this many pixels of advance a tofu is not drawn at all.
///
/// A box smaller than this is a smudge rather than a character, and a page set
/// in 4px text would gain nothing from a row of them. The same judgement the
/// image placeholder makes at its own size.
const TOFU_MIN: f32 = 4.0;

/// Draws a hollow box where a character has no glyph.
///
/// `x` is the pen position and `y` the baseline, which is what the glyph path
/// around this already holds.
fn draw_tofu(pixmap: &mut Pixmap, glyph: &text::PositionedGlyph, x: f32, y: f32, color: Color) {
    let advance = glyph.advance;
    let height = glyph.font_size * TOFU_HEIGHT;
    if advance < TOFU_MIN || height < TOFU_MIN {
        return;
    }
    let inset = advance * TOFU_SIDE;
    // Snapped to whole pixels, unlike a glyph. A glyph is antialiased and reads
    // correctly at a fractional position; a one-pixel edge drawn at one is
    // spread across two rows at half intensity, so a box of them comes out as a
    // grey smudge rather than as a box.
    //
    // It also makes the box a function of the rounded baseline rather than the
    // exact one, which matters more than it sounds: two layouts whose baselines
    // differ by a fraction of a pixel — an anonymous block beside a `<br>`, say
    // — would otherwise draw boxes of different heights, and a hard edge turns
    // a sub-pixel difference into a visible one.
    let rect = Rect {
        x: (x + inset).round(),
        y: (y - height).round(),
        width: (advance - inset * 2.0).round().max(1.0),
        height: height.round().max(1.0),
    };
    let edge = (glyph.font_size * TOFU_EDGE).max(TOFU_EDGE_MIN).round();
    // Hollow, so it reads as a container for a character that is missing rather
    // than as a solid block, which at small sizes is indistinguishable from
    // censored text.
    if rect.width <= edge * 2.0 || rect.height <= edge * 2.0 {
        fill_rect(pixmap, &rect, color);
        return;
    }
    for side in [
        Rect {
            height: edge,
            ..rect
        },
        Rect {
            y: rect.y + rect.height - edge,
            height: edge,
            ..rect
        },
        Rect {
            width: edge,
            ..rect
        },
        Rect {
            x: rect.x + rect.width - edge,
            width: edge,
            ..rect
        },
    ] {
        fill_rect(pixmap, &side, color);
    }
}

fn draw_glyph(
    pixmap: &mut Pixmap,
    fonts: &mut FontStore,
    glyph: &text::PositionedGlyph,
    origin_x: f32,
    origin_y: f32,
    color: Color,
) {
    // The caller checks the text origin, which is not the same as this glyph's
    // position: `glyph.x` and `glyph.y` are offsets within the run, and a run
    // laid out from a stylesheet with `left: 1e30em` in it puts an in-range
    // origin arbitrarily far from where the glyph lands. Found by the fuzzer,
    // as a panic inside tiny-skia's `IntRect::from_xywh(...).unwrap()` — the
    // same family as the `margin: 1e40px` bug, one layer further in.
    let (x, y) = (origin_x + glyph.x, origin_y + glyph.y);
    if !in_range(x) || !in_range(y) {
        return;
    }

    // A character the bundled fonts do not cover (#97). The shaper resolves it
    // to `.notdef` and gives it a real advance, so the line is the right length
    // and the layout is right — and then nothing is drawn, because `.notdef`
    // has no outline in these faces. A page in Chinese or Arabic came out
    // *blank*, which is the worse of the two failures: a reader cannot tell
    // "this page is empty" from "this browser has no font for it".
    //
    // ADR-0008 and PLAN.md both already say pages in uncovered scripts render
    // as tofu. This is that sentence becoming true. Covering the scripts is the
    // other half and a separate decision, since it is tens of megabytes against
    // a font budget currently using four.
    if glyph.glyph_id == NOTDEF {
        draw_tofu(pixmap, glyph, x, y, color);
        return;
    }

    let Some((coverage, left, top, width, height)) = fonts.rasterise(glyph) else {
        return;
    };

    // `floor`, not a cast. A cast truncates toward zero, so -0.5 becomes 0
    // while -1.5 becomes -1 — the rounding changes direction at zero. Nothing
    // noticed while every canvas began at document row 0; a band's coordinates
    // go negative above it.
    //
    // Checked rather than trusted: `left` and `top` come from the rasteriser
    // rather than from us.
    let (Some(x), Some(y)) = (
        (x.floor() as i32).checked_add(left),
        (y.floor() as i32).checked_sub(top),
    ) else {
        return;
    };

    // The part of the glyph that lands on this canvas, cropped out of the
    // coverage buffer rather than drawn at a negative offset and left to the
    // rasteriser to clip.
    //
    // That distinction is not pedantry. `draw_pixmap` fills a rectangle with
    // the bitmap as a *pattern*, and a pattern sampled outside its bounds pads
    // with its edge — so a glyph straddling the top of a band came out with a
    // duplicated row, and a band was not the rows it claimed. Cropping here
    // means every draw lands at a non-negative offset and no sampling happens
    // outside the bitmap at all.
    let (glyph_width, glyph_height) = (width as i32, height as i32);
    let (canvas_width, canvas_height) = (pixmap.width() as i32, pixmap.height() as i32);
    let from_x = (-x).max(0);
    let from_y = (-y).max(0);
    let to_x = (canvas_width - x).min(glyph_width);
    let to_y = (canvas_height - y).min(glyph_height);
    if to_x <= from_x || to_y <= from_y {
        return;
    }
    let (visible_width, visible_height) = ((to_x - from_x) as u32, (to_y - from_y) as u32);

    // The shaper gives 8-bit coverage; colour is applied here. Premultiplied,
    // because that is what tiny-skia composites in.
    let mut glyph_pixmap = match Pixmap::new(visible_width, visible_height) {
        Some(pixmap) => pixmap,
        None => return,
    };
    let transparent =
        tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 0).expect("transparent is valid");
    for row in 0..visible_height {
        for column in 0..visible_width {
            let source =
                (from_y as usize + row as usize) * width + from_x as usize + column as usize;
            let alpha =
                u32::from(coverage.get(source).copied().unwrap_or(0)) * u32::from(color.a) / 255;
            let scale = |channel: u8| (u32::from(channel) * alpha / 255) as u8;
            glyph_pixmap.pixels_mut()[(row * visible_width + column) as usize] =
                tiny_skia::PremultipliedColorU8::from_rgba(
                    scale(color.r),
                    scale(color.g),
                    scale(color.b),
                    alpha as u8,
                )
                .unwrap_or(transparent);
        }
    }

    pixmap.draw_pixmap(
        x + from_x,
        y + from_y,
        glyph_pixmap.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

#[cfg(test)]
mod clip_tests {
    use super::*;
    use css::style::ClipRect;
    use css::value::Length;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn every_side_is_measured_from_the_top_left() {
        // The trap in this property: `right` and `bottom` are offsets from
        // the top-left, not insets from the far edges. Read as insets, a
        // `rect(0, 60px, 30px, 0)` on a 120x60 box clips to the wrong half.
        let clip = ClipRect {
            top: Some(Length::Px(0.0)),
            right: Some(Length::Px(60.0)),
            bottom: Some(Length::Px(30.0)),
            left: Some(Length::Px(0.0)),
        };
        assert_eq!(
            resolve_clip(clip, 10.0, 20.0, rect(0.0, 0.0, 120.0, 60.0), 16.0),
            rect(10.0, 20.0, 60.0, 30.0)
        );
    }

    #[test]
    fn an_auto_side_is_the_border_edge() {
        let clip = ClipRect {
            top: Some(Length::Px(10.0)),
            right: None,
            bottom: None,
            left: Some(Length::Px(10.0)),
        };
        assert_eq!(
            resolve_clip(clip, 0.0, 0.0, rect(0.0, 0.0, 120.0, 60.0), 16.0),
            rect(10.0, 10.0, 110.0, 50.0)
        );
    }

    #[test]
    fn a_backwards_clip_is_empty_rather_than_negative() {
        let clip = ClipRect {
            top: Some(Length::Px(40.0)),
            right: Some(Length::Px(10.0)),
            bottom: Some(Length::Px(10.0)),
            left: Some(Length::Px(40.0)),
        };
        let resolved = resolve_clip(clip, 0.0, 0.0, rect(0.0, 0.0, 120.0, 60.0), 16.0);
        assert_eq!((resolved.width, resolved.height), (0.0, 0.0));
    }

    #[test]
    fn clipping_narrows_a_rectangle_and_drops_one_outside() {
        let mut items = vec![
            DisplayItem::Rect {
                rect: rect(0.0, 0.0, 100.0, 100.0),
                color: Color::rgb(1, 2, 3),
            },
            DisplayItem::Rect {
                rect: rect(200.0, 200.0, 10.0, 10.0),
                color: Color::rgb(1, 2, 3),
            },
        ];
        clip_items(&mut items, 0, rect(0.0, 0.0, 50.0, 50.0));
        assert_eq!(items.len(), 1, "the outside rectangle survived");
        let DisplayItem::Rect { rect: narrowed, .. } = items[0] else {
            panic!("wrong item kind");
        };
        assert_eq!(narrowed, rect(0.0, 0.0, 50.0, 50.0));
    }

    #[test]
    fn clipping_leaves_items_emitted_before_the_box_alone() {
        // The clip applies to the range this subtree emitted, and an earlier
        // sibling's items sit in front of it in the same list.
        let mut items = vec![
            DisplayItem::Rect {
                rect: rect(200.0, 200.0, 10.0, 10.0),
                color: Color::rgb(1, 2, 3),
            },
            DisplayItem::Rect {
                rect: rect(200.0, 200.0, 10.0, 10.0),
                color: Color::rgb(1, 2, 3),
            },
        ];
        clip_items(&mut items, 1, rect(0.0, 0.0, 50.0, 50.0));
        assert_eq!(items.len(), 1, "an earlier item was clipped too");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use css::Stylesheet;

    fn render(html: &str, css_text: &str, width: u32) -> Pixmap {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css_text)];
        let styles = css::cascade::cascade(&doc, &sheets);
        let mut fonts = FontStore::new();
        let sizes = layout::IntrinsicSizes::new();
        let layout = layout::layout(
            &doc,
            &styles,
            &mut fonts,
            &sizes,
            width as f32,
            width as f32,
        );
        let list = build_display_list(&layout);
        let height = layout.height.ceil().max(1.0) as u32;
        let images = ImageStore::new();
        rasterise(&list, &mut fonts, &images, width, height).expect("pixmap")
    }

    /// How many pixels of a render are not the blank canvas.
    fn ink(pixmap: &Pixmap) -> usize {
        pixmap
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|px| px != &&[255, 255, 255, 255])
            .count()
    }

    /// An in-flow box that draws nothing, so a page of out-of-flow content
    /// still has a height to rasterise.
    const SPACER: &str = "<div style=\"height: 50px\"></div>";

    /// The colour at a point, as `(r, g, b)`.
    fn at(pixmap: &Pixmap, x: u32, y: u32) -> (u8, u8, u8) {
        // Checked rather than indexed blind: a page of only out-of-flow boxes
        // lays out to no height, and the one-row canvas that results is far
        // easier to recognise from this than from a bounds panic.
        assert!(
            x < pixmap.width() && y < pixmap.height(),
            "({x}, {y}) lies outside a {}x{} canvas - the page laid out to \
             nothing, so no colour assertion below could mean anything",
            pixmap.width(),
            pixmap.height()
        );
        let i = ((y * pixmap.width() + x) * 4) as usize;
        let d = pixmap.data();
        (d[i], d[i + 1], d[i + 2])
    }

    #[test]
    fn z_index_decides_which_of_two_positioned_boxes_is_on_top() {
        // Written blue-last, so tree order would put blue on top. `z-index`
        // says otherwise and must win, in both directions — the second half is
        // what catches a sort that runs the wrong way and still "does
        // something".
        // `#r, #b` rather than `div`, so the spacer that gives the page a
        // height is not itself positioned — two out-of-flow boxes alone lay out
        // to no height at all and rasterise to a single row.
        let sheet = "body { margin: 0 } #r, #b { position: absolute; top: 0; left: 0; \
                     width: 40px; height: 40px } #r { background: #ff0000 } \
                     #b { background: #0000ff }";
        let red_on_top = render(
            &format!(
                "<body>{SPACER}<div id=r style=\"z-index: 2\"></div>\
                 <div id=b style=\"z-index: 1\"></div></body>"
            ),
            sheet,
            60,
        );
        assert_eq!(at(&red_on_top, 20, 20), (255, 0, 0), "z-index was ignored");

        let blue_on_top = render(
            &format!(
                "<body>{SPACER}<div id=r style=\"z-index: 1\"></div>\
                 <div id=b style=\"z-index: 2\"></div></body>"
            ),
            sheet,
            60,
        );
        assert_eq!(
            at(&blue_on_top, 20, 20),
            (0, 0, 255),
            "the order did not follow z-index, it only changed"
        );
    }

    #[test]
    fn a_negative_z_index_goes_behind_the_in_flow_content() {
        // §9.9's middle: a negative `z-index` paints before unpositioned
        // content, an `auto` or `0` positioned box after it. Comparing the
        // numbers alone would put a positioned `0` and its unpositioned sibling
        // in tree order, which is the one pairing §9.9 reverses.
        let sheet = "body { margin: 0 } #flow { background: #00ff00; height: 40px }                      #pos { position: absolute; top: 0; left: 0; width: 40px;                      height: 40px; background: #ff0000 }";
        let behind = render(
            "<body><div id=pos style=\"z-index: -1\"></div><div id=flow></div></body>",
            sheet,
            60,
        );
        assert_eq!(
            at(&behind, 20, 20),
            (0, 255, 0),
            "a negative z-index did not go behind the in-flow box"
        );

        // A *relatively* positioned box for the second half, and that is the
        // whole point of it. An absolutely positioned one is laid out after the
        // in-flow children and appended to the box tree last, so tree order
        // alone already puts it in front and the test would pass without the
        // rule it claims to check — it did, until this was noticed. A relative
        // box keeps its place among its siblings, so only §9.9 can move it.
        let overlapping = "body { margin: 0 } div { height: 40px }                            #rel { position: relative; background: #ff0000 }                            #flow { background: #00ff00; margin-top: -40px }";
        let front = render(
            "<body><div id=rel></div><div id=flow></div></body>",
            overlapping,
            60,
        );
        assert_eq!(
            at(&front, 20, 20),
            (255, 0, 0),
            "a positioned box with auto z-index did not come out in front of \
             the in-flow box written after it"
        );
    }

    #[test]
    fn z_index_is_not_read_from_an_unpositioned_box() {
        // §9.9 gives it meaning only on a positioned box. Honouring it
        // elsewhere would invent a stacking order the spec does not describe,
        // and pages do set it on static elements by accident.
        let sheet = "body { margin: 0 } div { height: 40px }                      #a { background: #ff0000 } #b { background: #0000ff; margin-top: -40px }";
        let plain = render("<body><div id=a></div><div id=b></div></body>", sheet, 60);
        let with_z = render(
            "<body><div id=a style=\"z-index: 9\"></div><div id=b></div></body>",
            sheet,
            60,
        );
        assert_eq!(
            plain.data(),
            with_z.data(),
            "z-index moved an unpositioned box"
        );
    }

    #[test]
    fn a_hidden_box_draws_nothing_and_keeps_its_room() {
        // Both halves matter and they pull in opposite directions. Drawing
        // nothing is what separates this from being ignored; keeping the room
        // is what separates it from `display: none`, and is the reason an
        // author reaches for it.
        let sheet =
            "body { margin: 0 } div { height: 40px; background: #000; border: 2px solid #000 }";
        let shown = render("<body><div></div></body>", sheet, 60);
        let hidden = render(
            "<body><div style=\"visibility: hidden\"></div></body>",
            sheet,
            60,
        );
        let gone = render(
            "<body><div style=\"display: none\"></div></body>",
            sheet,
            60,
        );

        assert!(ink(&shown) > 0, "the control drew nothing");
        assert_eq!(ink(&hidden), 0, "a hidden box was drawn");
        assert_eq!(
            hidden.height(),
            shown.height(),
            "a hidden box gave up its space, which is `display: none`"
        );
        assert!(
            gone.height() < hidden.height(),
            "`display: none` kept its space, so this proves nothing"
        );
    }

    #[test]
    fn hidden_inline_text_is_not_drawn_but_still_holds_its_line_open() {
        // The case a box-level check cannot reach: spans are merged into one
        // text layout, so hiding one has to travel with its glyphs. Before
        // this, `visibility: hidden` on a span did nothing at all.
        let sheet = "body { margin: 0 }";
        let plain = render("<body><p>AAA BBB</p></body>", sheet, 200);
        let hidden = render(
            "<body><p>AAA <span style=\"visibility: hidden\">BBB</span></p></body>",
            sheet,
            200,
        );
        let dropped = render("<body><p>AAA </p></body>", sheet, 200);

        assert!(
            ink(&hidden) < ink(&plain),
            "the hidden span was still drawn"
        );
        assert!(ink(&hidden) > 0, "it took the whole paragraph with it");
        assert_eq!(
            ink(&hidden),
            ink(&dropped),
            "what is left is not exactly the visible text"
        );
    }

    #[test]
    fn a_visible_span_inside_a_hidden_block_is_drawn() {
        // §11.2's surprise, and the reason the glyphs carry their own
        // visibility rather than the box carrying it for them: gating the whole
        // text layout on the block would take this span with it.
        let sheet = "body { margin: 0 }";
        let hidden = render(
            "<body><p style=\"visibility: hidden\">AAA</p></body>",
            sheet,
            200,
        );
        let with_child = render(
            "<body><p style=\"visibility: hidden\">AAA              <span style=\"visibility: visible\">BBB</span></p></body>",
            sheet,
            200,
        );
        assert_eq!(ink(&hidden), 0, "the hidden paragraph drew something");
        assert!(
            ink(&with_child) > 0,
            "a span that asked to be visible inside it was hidden anyway"
        );
    }

    /// The display list, images, and height for a page, so a test can rasterise
    /// it whole and in pieces and compare.
    fn scene(html: &str, css_text: &str, width: u32) -> (DisplayList, FontStore, u32) {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css_text)];
        let styles = css::cascade::cascade(&doc, &sheets);
        let mut fonts = FontStore::new();
        let sizes = layout::IntrinsicSizes::new();
        let layout = layout::layout(
            &doc,
            &styles,
            &mut fonts,
            &sizes,
            width as f32,
            width as f32,
        );
        let height = layout.height.ceil().max(1.0) as u32;
        (build_display_list(&layout), fonts, height)
    }

    #[test]
    fn a_band_is_exactly_the_rows_it_names_from_the_whole_page() {
        // The property banded rendering rests on, and the reason it is safe to
        // stop rendering whole documents: a reader scrolling through bands must
        // see the same pixels they would have seen from one canvas. Anything
        // less and the fix for long pages is a rendering change in disguise.
        //
        // The fixture is chosen for the things that could go wrong rather than
        // for looking like a page: a tiled background whose phase must not jump
        // between bands, borders that straddle band edges, and enough text that
        // glyphs land in every band.
        let html = "<body><div class=tiled><p>one</p><p>two</p><p>three</p>\
             <p>four</p><p>five</p><p>six</p><p>seven</p><p>eight</p></div></body>";
        let css_text = "body { background: #eef; margin: 0 }
             .tiled { border: 3px solid #333; padding: 7px }
             p { margin: 9px 0; border-bottom: 1px solid #999 }";
        let width = 120;

        let (list, mut fonts, height) = scene(html, css_text, width);
        assert!(height > 60, "the fixture is too short to band: {height}");
        let images = ImageStore::new();
        let whole = rasterise(&list, &mut fonts, &images, width, height).expect("whole page");

        // Band heights that do and do not divide the page, and a band running
        // off the bottom, because the last band of a real page always does.
        for band_height in [7u32, 16, 23] {
            let mut top = 0u32;
            while top < height {
                let band = rasterise_band(
                    &list,
                    &mut fonts,
                    &images,
                    width,
                    0.0,
                    top as f32,
                    band_height,
                )
                .expect("band");
                for row in 0..band_height {
                    let document_row = top + row;
                    if document_row >= height {
                        break;
                    }
                    let from_band =
                        &band.pixels()[(row * width) as usize..((row + 1) * width) as usize];
                    let from_whole = &whole.pixels()
                        [(document_row * width) as usize..((document_row + 1) * width) as usize];
                    assert_eq!(
                        from_band, from_whole,
                        "band of {band_height} at {top}: row {row} is not document row \
                         {document_row}"
                    );
                }
                top += band_height;
            }
        }
    }

    #[test]
    fn a_band_is_exactly_the_columns_it_names_from_the_whole_page() {
        // The same property, sideways (#204). It is the one horizontal
        // scrolling rests on: the reader moving right must see the columns they
        // would have seen from a canvas wide enough to hold the whole page, or
        // scrolling is a rendering change in disguise.
        //
        // Checked against a *wide* canvas rather than the window-sized one,
        // because the whole point is that the page is wider than the window —
        // there is no other way to have the right answer to compare with.
        let html = "<body><div class=wide><p>one two three four five six seven</p>\
             <pre>a line of fixed width output that runs well past any window</pre>\
             <p>eight nine ten</p></div></body>";
        let css_text = "body { background: #eef; margin: 0 }
             .wide { width: 700px; border: 3px solid #333; padding: 7px }
             pre { margin: 0 }";
        let window = 120;
        let whole_width = 760;

        // Laid out once, at the window's width — which is what actually
        // happens: the page overflows the viewport rather than being laid out
        // for a wider one.
        let (list, mut fonts, height) = scene(html, css_text, window);
        assert!(
            content_width(&list) > window as f32 * 2.0,
            "the fixture does not overflow its window: {}",
            content_width(&list)
        );
        let images = ImageStore::new();
        let whole =
            rasterise_band(&list, &mut fonts, &images, whole_width, 0.0, 0.0, height).expect("all");

        for left in [0u32, 1, 60, 119, 300, 640] {
            let band = rasterise_band(&list, &mut fonts, &images, window, left as f32, 0.0, height)
                .expect("band");
            for row in 0..height {
                for column in 0..window {
                    let document_column = left + column;
                    if document_column >= whole_width {
                        break;
                    }
                    let from_band = band.pixels()[(row * window + column) as usize];
                    let from_whole = whole.pixels()[(row * whole_width + document_column) as usize];
                    assert_eq!(
                        from_band, from_whole,
                        "band at {left}: row {row} column {column} is not document column \
                         {document_column}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_content_width_reaches_the_furthest_thing_drawn() {
        let list = DisplayList {
            canvas: Color::rgb(0xff, 0xff, 0xff),
            canvas_image: None,
            items: vec![
                DisplayItem::Rect {
                    rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 100.0,
                        height: 10.0,
                    },
                    color: Color::rgb(0, 0, 0),
                },
                DisplayItem::Rect {
                    rect: Rect {
                        x: 400.0,
                        y: 0.0,
                        width: 250.0,
                        height: 10.0,
                    },
                    color: Color::rgb(0, 0, 0),
                },
            ],
            pinned: Vec::new(),
        };
        assert_eq!(content_width(&list), 650.0);
    }

    #[test]
    fn a_fixed_box_is_not_something_to_scroll_to() {
        // A `position: fixed` item's coordinates are the window's, not the
        // document's (#108). Counting one as content would give every page with
        // a fixed bar a scroll range it does not have — and the range would
        // move as the reader scrolled, which is not a thing a document does.
        let bar = DisplayItem::Rect {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: 900.0,
                height: 10.0,
            },
            color: Color::rgb(0, 0, 0),
        };
        let text = DisplayItem::Rect {
            rect: Rect {
                x: 0.0,
                y: 20.0,
                width: 300.0,
                height: 10.0,
            },
            color: Color::rgb(0, 0, 0),
        };
        let list = DisplayList {
            canvas: Color::rgb(0xff, 0xff, 0xff),
            canvas_image: None,
            items: vec![bar, text],
            pinned: vec![(0, 1)],
        };
        assert_eq!(content_width(&list), 300.0, "the fixed bar was counted");
    }

    fn count_non_white(pixmap: &Pixmap) -> usize {
        pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() != 255 || p.green() != 255 || p.blue() != 255)
            .count()
    }

    #[test]
    fn a_glyph_pushed_out_of_range_by_its_own_offset_is_skipped() {
        // The text *origin* is checked before `draw_glyph` is called, and that
        // is not the same thing as where the glyph lands: `glyph.x` and
        // `glyph.y` are offsets within the run. A stylesheet that shifts a run
        // by an enormous amount puts an in-range origin arbitrarily far from an
        // out-of-range glyph, and tiny-skia panics on the `i32` rectangle it
        // builds from it rather than refusing.
        //
        // Found by the fuzzer as a mutation of the fixture written for the
        // `margin: 1e40px` family. Same family, one layer in.
        let mut list = DisplayList::default();
        let mut fonts = FontStore::new();
        let laid_out = fonts.layout("H", &crate::tests::glyph_style(), 1000.0);
        let glyph = laid_out.lines[0].glyphs[0];

        for (x, y) in [
            (f32::INFINITY, 0.0),
            (0.0, f32::INFINITY),
            (1e30, 0.0),
            (0.0, -1e30),
            (f32::NAN, 0.0),
        ] {
            list.items.push(DisplayItem::Glyph {
                glyph: text::PositionedGlyph { x, y, ..glyph },
                origin_x: 1.0,
                origin_y: 1.0,
                color: Color::BLACK,
            });
        }

        // The point is that this returns at all.
        let images = ImageStore::new();
        let pixmap = rasterise(&list, &mut fonts, &images, 50, 50).expect("pixmap");
        assert_eq!(
            count_non_white(&pixmap),
            0,
            "nothing should have been drawn"
        );
    }

    /// A style for building a glyph to position by hand.
    fn glyph_style() -> css::style::ComputedStyle {
        css::style::ComputedStyle::default()
    }

    #[test]
    fn an_empty_page_paints_nothing_but_white() {
        assert_eq!(count_non_white(&render("<body></body>", "", 200)), 0);
    }

    #[test]
    fn text_puts_ink_on_the_canvas() {
        let pixmap = render("<body><p>Hello world</p></body>", "", 400);
        assert!(count_non_white(&pixmap) > 50, "expected glyph coverage");
    }

    #[test]
    fn a_background_colour_fills_its_box() {
        let pixmap = render(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { background-color: #ff0000; height: 20px }",
            50,
        );
        let red = pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() > 200 && p.green() < 60 && p.blue() < 60)
            .count();
        assert!(
            red >= 50 * 20 - 50,
            "expected a filled red band, got {red} pixels"
        );
    }

    #[test]
    fn colour_reaches_the_glyphs() {
        // Blue text must produce blue ink, not merely some ink: this is the
        // step where coverage bitmaps get their colour.
        let pixmap = render("<body><p>iiiiiiii</p></body>", "p { color: #0000ff }", 300);
        let blue = pixmap
            .pixels()
            .iter()
            .filter(|p| p.blue() > 100 && p.red() < 100)
            .count();
        assert!(blue > 10, "expected blue glyph pixels, got {blue}");
    }

    #[test]
    fn an_inline_span_carries_its_own_colour_into_the_pixels() {
        // End to end: cascade gives <b> a colour, the shaper carries it per
        // glyph, and paint honours it rather than the block's colour.
        let pixmap = render(
            "<body><p>plain <b>red</b></p></body>",
            "p { color: #000000 } b { color: #ff0000 }",
            300,
        );
        let red = pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() > 150 && p.green() < 80 && p.blue() < 80)
            .count();
        assert!(
            red > 5,
            "expected red glyph pixels from the <b> span, got {red}"
        );
    }

    #[test]
    fn an_inline_span_can_change_the_font() {
        // <code> should render monospace even inside a proportional paragraph.
        let proportional = render("<body><p>iiiiiiiiii</p></body>", "", 400);
        let monospaced = render("<body><p><code>iiiiiiiiii</code></p></body>", "", 400);
        let ink_width = |pixmap: &Pixmap| -> u32 {
            let width = pixmap.width();
            let columns: Vec<u32> = pixmap
                .pixels()
                .iter()
                .enumerate()
                .filter(|(_, p)| p.red() != 255 || p.green() != 255 || p.blue() != 255)
                .map(|(i, _)| i as u32 % width)
                .collect();
            match (columns.iter().min(), columns.iter().max()) {
                (Some(min), Some(max)) => max - min,
                _ => 0,
            }
        };
        // Monospace 'i' is much wider than proportional 'i'.
        assert!(
            ink_width(&monospaced) > ink_width(&proportional),
            "code span did not switch to monospace: {} vs {}",
            ink_width(&monospaced),
            ink_width(&proportional)
        );
    }

    #[test]
    fn borders_paint_on_all_four_edges() {
        let pixmap = render(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { border: 4px solid #ff0000; height: 40px }",
            60,
        );
        let width = pixmap.width();
        let is_red = |x: u32, y: u32| {
            let p = pixmap.pixels()[(y * width + x) as usize];
            p.red() > 200 && p.green() < 60 && p.blue() < 60
        };
        assert!(is_red(30, 1), "top edge");
        assert!(is_red(30, 46), "bottom edge");
        assert!(is_red(1, 20), "left edge");
        assert!(is_red(58, 20), "right edge");
        assert!(!is_red(30, 20), "interior must not be filled");
    }

    #[test]
    fn each_edge_can_have_its_own_colour() {
        let pixmap = render(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { border: 4px solid; border-top-color: #ff0000; \
             border-bottom-color: #0000ff; border-left-color: #00ff00; \
             border-right-color: #000000; height: 40px }",
            60,
        );
        let width = pixmap.width();
        let at = |x: u32, y: u32| pixmap.pixels()[(y * width + x) as usize];
        assert!(at(30, 1).red() > 200, "top is red");
        assert!(at(30, 46).blue() > 200, "bottom is blue");
        assert!(at(1, 20).green() > 200, "left is green");
        let right = at(58, 20);
        assert!(
            right.red() < 60 && right.green() < 60 && right.blue() < 60,
            "right is black"
        );
    }

    #[test]
    fn a_hidden_border_reserves_space_but_paints_nothing() {
        let hidden = render(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { border: 6px hidden #ff0000; height: 20px }",
            40,
        );
        let red = hidden
            .pixels()
            .iter()
            .filter(|p| p.red() > 200 && p.green() < 60 && p.blue() < 60)
            .count();
        assert_eq!(red, 0, "hidden must not paint");
        // But it still occupies space, so the box is taller than its content.
        assert!(hidden.height() >= 32, "6px top + 20px content + 6px bottom");
    }

    #[test]
    fn an_inline_box_paints_its_background_and_border() {
        // The bug this pins: a `<span>` with a background drew its text and
        // nothing else. A highlighted phrase, a tinted `<code>`, a pill — all
        // ordinary markup, all plain text until now.
        let count = |pixmap: &Pixmap, want: (u8, u8, u8)| {
            pixmap
                .pixels()
                .iter()
                .filter(|p| (p.red(), p.green(), p.blue()) == want)
                .count()
        };
        let yellow = (255, 204, 0);

        let plain = render(
            "<body><p>a <span>b</span> c</p></body>",
            "body{margin:0}",
            200,
        );
        assert_eq!(
            count(&plain, yellow),
            0,
            "a plain span painted a background"
        );

        let filled = render(
            "<body><p>a <span>b</span> c</p></body>",
            "body{margin:0} span{background:#ffcc00;padding:0 6px}",
            200,
        );
        assert!(
            count(&filled, yellow) > 100,
            "the span's background came out {} pixels",
            count(&filled, yellow)
        );

        // And the border is on the outside of the padding, not instead of it.
        let bordered = render(
            "<body><p>a <span>b</span> c</p></body>",
            "body{margin:0} span{background:#ffcc00;padding:0 6px;border:2px solid #cc0000}",
            200,
        );
        assert!(count(&bordered, (204, 0, 0)) > 40, "no border drawn");
        assert!(
            count(&bordered, yellow) >= count(&filled, yellow),
            "the border ate the background it should surround"
        );
    }

    #[test]
    fn an_inline_boxs_horizontal_border_is_only_on_the_ends() {
        // §8.4: the left and right sides belong to the whole box, so a box
        // broken over two lines draws them once each and not once per line.
        // Counted as red pixels: two vertical 2px sides at ~14px of content
        // area is a few dozen, four would be twice that.
        let sides = |css: &str| {
            let pixmap = render(
                "<body><p><span>one two three four five six seven eight</span></p></body>",
                css,
                120,
            );
            pixmap
                .pixels()
                .iter()
                .filter(|p| (p.red(), p.green(), p.blue()) == (204, 0, 0))
                .count()
        };
        let one_line = render(
            "<body><p><span>short</span></p></body>",
            "body{margin:0} span{border-left:2px solid #cc0000;border-right:2px solid #cc0000}",
            400,
        );
        let per_side = one_line
            .pixels()
            .iter()
            .filter(|p| (p.red(), p.green(), p.blue()) == (204, 0, 0))
            .count()
            / 2;
        assert!(per_side > 8, "a 2px side came out {per_side} pixels");
        let broken = sides(
            "body{margin:0} span{border-left:2px solid #cc0000;border-right:2px solid #cc0000}",
        );
        assert!(
            broken < per_side * 4,
            "a box broken over lines drew {broken} pixels of side, which is more than the two \
             it has"
        );
    }

    #[test]
    fn an_outline_is_drawn_outside_the_border_and_moves_nothing() {
        // §18.4: outside the border box and taking up no room, which is the
        // whole point — an outline that moved the page could not be used to
        // mark focus.
        let red = |pixmap: &Pixmap| {
            pixmap
                .pixels()
                .iter()
                .filter(|p| (p.red(), p.green(), p.blue()) == (255, 0, 0))
                .count()
        };
        const CSS: &str = "body { margin: 20px } p { margin: 0; width: 100px; height: 20px; \
                           border: 2px solid #000000 }";
        let bare = render("<body><p>x</p></body>", CSS, 200);
        let outlined = render(
            "<body><p>x</p></body>",
            "body { margin: 20px } p { margin: 0; width: 100px; height: 20px; \
             border: 2px solid #000000; outline: 3px solid #ff0000 }",
            200,
        );
        assert_eq!(red(&bare), 0);
        assert!(red(&outlined) > 100, "no outline drawn");
        assert_eq!(
            bare.height(),
            outlined.height(),
            "the outline changed the page's height"
        );

        // It is *outside* the border: the pixel three rows above the box's top
        // edge is outline, and the border is still black underneath.
        let (r, g, b) = at(&outlined, 40, 19);
        assert_eq!(
            (r, g, b),
            (255, 0, 0),
            "the row above the border is not red"
        );
        let (r, g, b) = at(&outlined, 40, 21);
        assert_eq!((r, g, b), (0, 0, 0), "the border itself was overwritten");
    }

    #[test]
    fn rendering_is_deterministic() {
        // The property ADR-0005 buys: identical input, identical bytes. If this
        // ever fails, the single shared baseline set is invalid.
        let once = render("<body><h1>Title</h1><p>Body text here.</p></body>", "", 300);
        let twice = render("<body><h1>Title</h1><p>Body text here.</p></body>", "", 300);
        assert_eq!(once.data(), twice.data());
    }

    #[test]
    fn centred_text_sits_further_right_than_left_aligned() {
        let leftmost_ink = |css: &str| -> u32 {
            let pixmap = render("<body><p>xx</p></body>", css, 400);
            let width = pixmap.width();
            pixmap
                .pixels()
                .iter()
                .enumerate()
                .filter(|(_, p)| p.red() != 255 || p.green() != 255 || p.blue() != 255)
                .map(|(i, _)| i as u32 % width)
                .min()
                .unwrap_or(width)
        };
        assert!(leftmost_ink("p { text-align: center }") > leftmost_ink("p { text-align: left }"));
    }
    #[test]
    fn a_legend_leaves_a_hole_in_the_rule_and_the_rest_of_it_standing() {
        // The point of the hole is that a reader sees the legend *in* the rule
        // rather than floating above an unbroken line. So the rule has to stop
        // where the legend starts and pick up again where it ends — both halves
        // of that, since a rule that vanished entirely would pass a test that
        // only looked for the gap.
        const CSS: &str = "body { margin: 0 } \
                           fieldset { margin: 0; border: 2px solid black; padding: 6px }";
        fn dark(pixmap: &Pixmap, x: u32, y: u32) -> bool {
            let (r, g, b) = at(pixmap, x, y);
            r < 128 && g < 128 && b < 128
        }
        /// The top rule: the first row that is mostly drawn.
        fn rule_row(pixmap: &Pixmap) -> u32 {
            (0..pixmap.height())
                .find(|&y| {
                    (0..pixmap.width()).filter(|&x| dark(pixmap, x, y)).count() as u32
                        > pixmap.width() / 2
                })
                .expect("a rule across the page")
        }

        let with = render(
            "<body><fieldset><legend>L</legend><div style=\"height: 20px\"></div></fieldset></body>",
            CSS,
            200,
        );
        let without = render(
            "<body><fieldset><div style=\"height: 20px\"></div></fieldset></body>",
            CSS,
            200,
        );

        // Inside the legend's own left padding, which is the near end of the
        // gap and the one place in it no glyph can reach.
        let y = rule_row(&with);
        assert!(!dark(&with, 9, y), "the rule ran on behind the legend");
        assert!(dark(&with, 1, y), "the rule lost its left end");
        assert!(
            dark(&with, 198, y),
            "the rule did not pick up again after the legend"
        );

        let y = rule_row(&without);
        assert!(
            dark(&without, 9, y),
            "a fieldset with no legend drew a gap anyway"
        );
    }
}

#[cfg(test)]
mod border_tests {
    use super::*;
    use css::style::BorderStyle;

    fn side(style: BorderStyle, length: f32, thickness: f32) -> Vec<Rect> {
        let mut list = DisplayList::default();
        push_border_side(
            &mut list,
            &Rect {
                x: 10.0,
                y: 20.0,
                width: length,
                height: thickness,
            },
            style,
            thickness,
            Side::Top,
            Color::BLACK,
        );
        list.items
            .into_iter()
            .map(|item| match item {
                DisplayItem::Rect { rect, .. } => rect,
                other => panic!("a border drew {other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_solid_border_is_still_one_rectangle() {
        let segments = side(BorderStyle::Solid, 100.0, 2.0);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].width, 100.0);
    }

    #[test]
    fn a_dotted_border_is_a_row_of_squares_the_border_wide() {
        let segments = side(BorderStyle::Dotted, 100.0, 2.0);
        assert!(segments.len() > 10, "got {} dots", segments.len());
        // Square, or as near as stretching to fit allows.
        for dot in &segments {
            assert!(
                (dot.width - 2.0).abs() < 0.5,
                "a dot is {} wide against a 2px border",
                dot.width
            );
        }
    }

    #[test]
    fn a_dashed_border_uses_longer_segments_than_a_dotted_one() {
        let dashes = side(BorderStyle::Dashed, 100.0, 2.0);
        let dots = side(BorderStyle::Dotted, 100.0, 2.0);
        assert!(
            dashes.len() * 2 < dots.len(),
            "{} dashes against {} dots is not a visible difference",
            dashes.len(),
            dots.len()
        );
    }

    #[test]
    fn a_pattern_starts_and_ends_flush_with_the_corners() {
        // The thing that makes a hand-rolled dashed border look wrong is half a
        // dash at one end, so the run is stretched to fit a whole number.
        for style in [BorderStyle::Dotted, BorderStyle::Dashed] {
            for length in [7.0, 31.0, 100.0, 253.0] {
                let segments = side(style, length, 3.0);
                let first = segments.first().expect("at least one segment");
                let last = segments.last().expect("at least one segment");
                assert_eq!(first.x, 10.0, "{style:?} at {length} starts short");
                assert!(
                    ((last.x + last.width) - (10.0 + length)).abs() < 0.01,
                    "{style:?} at {length} ends at {} rather than {}",
                    last.x + last.width,
                    10.0 + length,
                );
            }
        }
    }

    fn colours(style: BorderStyle, edge: Side, thickness: f32) -> Vec<Color> {
        let mut list = DisplayList::default();
        push_border_side(
            &mut list,
            &Rect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: thickness,
            },
            style,
            thickness,
            edge,
            Color::rgb(200, 200, 200),
        );
        list.items
            .into_iter()
            .map(|item| match item {
                DisplayItem::Rect { color, .. } => color,
                other => panic!("a border drew {other:?}"),
            })
            .collect()
    }

    #[test]
    fn outset_lights_the_top_and_shadows_the_bottom() {
        // The whole of the effect: an edge that a light source above and to the
        // left would strike keeps the declared colour, and the opposite edge is
        // darkened. Reversed for `inset`, which is what makes one look raised
        // and the other pressed in.
        let top = colours(BorderStyle::Outset, Side::Top, 4.0);
        let bottom = colours(BorderStyle::Outset, Side::Bottom, 4.0);
        assert_eq!(top.len(), 1);
        assert!(top[0].r > bottom[0].r, "{:?} vs {:?}", top[0], bottom[0]);

        let top = colours(BorderStyle::Inset, Side::Top, 4.0);
        let bottom = colours(BorderStyle::Inset, Side::Bottom, 4.0);
        assert!(
            top[0].r < bottom[0].r,
            "inset is outset the other way up: {:?} vs {:?}",
            top[0],
            bottom[0],
        );
    }

    #[test]
    fn groove_and_ridge_split_the_thickness_into_two_shades() {
        // Two bands across the border rather than one, and a ridge is a groove
        // with the halves exchanged — which is the whole difference between
        // carved and proud.
        let groove = colours(BorderStyle::Groove, Side::Top, 4.0);
        let ridge = colours(BorderStyle::Ridge, Side::Top, 4.0);
        assert_eq!(groove.len(), 2, "an outer half and an inner one");
        assert_eq!(ridge.len(), 2);
        assert_ne!(groove[0], groove[1], "the two halves must differ");
        assert_eq!(groove[0], ridge[1], "a ridge is a groove reversed");
        assert_eq!(groove[1], ridge[0]);
    }

    #[test]
    fn double_draws_two_lines_and_falls_back_when_there_is_no_room() {
        let wide = colours(BorderStyle::Double, Side::Top, 6.0);
        assert_eq!(wide.len(), 2, "two lines with a gap between them");

        // Under three pixels there is nowhere to put a gap, so the author gets
        // the solid line they would have got anyway rather than two lines
        // rounded into one.
        let thin = colours(BorderStyle::Double, Side::Top, 2.0);
        assert_eq!(thin.len(), 1);
    }

    #[test]
    fn a_side_too_short_for_a_pattern_degrades_to_a_solid_run() {
        // Two dots on a six-pixel side is not a pattern, it is noise. One run
        // is the honest answer, and it is what the arithmetic already gives.
        let segments = side(BorderStyle::Dashed, 4.0, 3.0);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].width, 4.0);
    }

    #[test]
    fn a_zero_length_side_draws_nothing_rather_than_dividing_by_it() {
        assert!(side(BorderStyle::Dotted, 0.0, 2.0).is_empty());
        assert!(side(BorderStyle::Dotted, 100.0, 0.0).is_empty());
        assert!(side(BorderStyle::Dashed, f32::INFINITY, 2.0).is_empty());
    }
}

#[cfg(test)]
mod tile_tests {
    use super::*;
    use css::style::BackgroundRepeat;

    /// A 4x4 image, entirely opaque red.
    fn red_tile() -> DecodedImage {
        let mut pixmap = Pixmap::new(4, 4).expect("pixmap");
        pixmap.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        DecodedImage { pixmap }
    }

    /// A 4x4 image with a red top row and left column, white elsewhere.
    ///
    /// A *patterned* tile, because a uniform one cannot show where the tile
    /// boundaries fall. That is not hypothetical: the band test below was
    /// written with `red_tile` and passed with the phase deliberately broken,
    /// since tiling a solid colour covers everything whatever the offset. Any
    /// test about position or phase needs a tile that looks different in
    /// different places.
    fn corner_tile() -> DecodedImage {
        let mut pixmap = Pixmap::new(4, 4).expect("pixmap");
        pixmap.fill(tiny_skia::Color::WHITE);
        let red = PremultipliedColor::from_rgba(255, 0, 0, 255).expect("opaque red");
        let pixels = pixmap.pixels_mut();
        for pixel in pixels.iter_mut().take(4) {
            *pixel = red;
        }
        for y in 0..4usize {
            pixels[y * 4] = red;
        }
        DecodedImage { pixmap }
    }

    fn canvas() -> Pixmap {
        let mut pixmap = Pixmap::new(20, 20).expect("pixmap");
        pixmap.fill(tiny_skia::Color::WHITE);
        pixmap
    }

    /// White is also high in red, so the green channel is what tells them
    /// apart — checking red alone counts the whole blank canvas.
    fn red_pixels(pixmap: &Pixmap) -> usize {
        pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() > 200 && p.green() < 100)
            .count()
    }

    /// Whether the pixel at `(x, y)` is red.
    fn is_red(pixmap: &Pixmap, x: u32, y: u32) -> bool {
        let pixel = pixmap.pixels()[(y * pixmap.width() + x) as usize];
        pixel.red() > 200 && pixel.green() < 100
    }

    #[test]
    fn a_tile_repeats_across_the_whole_box() {
        let mut pixmap = canvas();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
        };
        tile_image(
            &mut pixmap,
            &red_tile(),
            &rect,
            (rect.x, rect.y),
            BackgroundRepeat::Repeat,
        );
        assert_eq!(red_pixels(&pixmap), 400, "every pixel covered");
    }

    #[test]
    fn a_tile_is_never_scaled_to_its_box() {
        // This is what separates a background from a content image: a 4px tile
        // in a 20px box repeats five times, it does not stretch to 20px.
        let mut pixmap = canvas();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 4.0,
        };
        tile_image(
            &mut pixmap,
            &red_tile(),
            &rect,
            (rect.x, rect.y),
            BackgroundRepeat::RepeatX,
        );
        assert!(is_red(&pixmap, 19, 3), "the last tile is drawn");
        assert!(!is_red(&pixmap, 0, 4), "and nothing below the box");
    }

    #[test]
    fn repeat_x_and_repeat_y_tile_one_axis_only() {
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
        };
        let mut horizontal = canvas();
        tile_image(
            &mut horizontal,
            &red_tile(),
            &rect,
            (rect.x, rect.y),
            BackgroundRepeat::RepeatX,
        );
        assert_eq!(red_pixels(&horizontal), 80, "one row of tiles");

        let mut vertical = canvas();
        tile_image(
            &mut vertical,
            &red_tile(),
            &rect,
            (rect.x, rect.y),
            BackgroundRepeat::RepeatY,
        );
        assert_eq!(red_pixels(&vertical), 80, "one column of tiles");
    }

    #[test]
    fn no_repeat_draws_exactly_one_tile() {
        let mut pixmap = canvas();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
        };
        tile_image(
            &mut pixmap,
            &red_tile(),
            &rect,
            (rect.x, rect.y),
            BackgroundRepeat::NoRepeat,
        );
        assert_eq!(red_pixels(&pixmap), 16);
    }

    #[test]
    fn a_positioned_tile_starts_where_it_was_put() {
        // `background-position: 5px 3px`, no repeat: exactly one 4x4 tile, and
        // its corner is where the offset says rather than the box's.
        let mut pixmap = canvas();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
        };
        tile_image(
            &mut pixmap,
            &red_tile(),
            &rect,
            (rect.x + 5.0, rect.y + 3.0),
            BackgroundRepeat::NoRepeat,
        );
        assert_eq!(red_pixels(&pixmap), 16, "still one tile");
        assert!(is_red(&pixmap, 5, 3), "its corner");
        assert!(is_red(&pixmap, 8, 6), "its far corner");
        assert!(!is_red(&pixmap, 4, 3), "nothing to the left of it");
        assert!(!is_red(&pixmap, 5, 2), "nothing above it");
    }

    #[test]
    fn a_percentage_lines_the_image_up_with_the_box_rather_than_offsetting_it() {
        // The half of §14.2.1 that is easy to get wrong. `50%` on a 20px box
        // holding a 4px tile is *not* 10px across — it is 50% of (20 − 4) = 8,
        // which is what puts the middle of the image at the middle of the box.
        let position = css::style::BackgroundPosition {
            x: css::value::Length::Percent(50.0),
            y: css::value::Length::Percent(100.0),
        };
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
        };
        let anchor = anchor_of(&rect, position, &red_tile());
        assert_eq!(anchor, (8.0, 16.0), "centred across, flush to the bottom");

        // And it goes negative when the image is bigger than the box, which is
        // the case that reads as wrong and is correct: the middle of a large
        // image still lands at the middle of a small box.
        let narrow = Rect { width: 2.0, ..rect };
        assert_eq!(anchor_of(&narrow, position, &red_tile()).0, -1.0);
    }

    #[test]
    fn a_repeating_tile_extends_backwards_from_its_position() {
        // A positioned repeat tiles in both directions, so the box's own corner
        // is covered by a tile at a negative index. Getting this wrong leaves a
        // gap along the top and left that only appears once a position is set.
        let mut pixmap = canvas();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 20.0,
        };
        tile_image(
            &mut pixmap,
            &red_tile(),
            &rect,
            (rect.x + 5.0, rect.y + 3.0),
            BackgroundRepeat::Repeat,
        );
        assert_eq!(red_pixels(&pixmap), 400, "no gap anywhere");
    }

    #[test]
    fn a_band_tiles_a_positioned_background_where_the_whole_page_would() {
        // What the anchor/clip split was for. A band renders rows from part-way
        // down the document, and the tiling phase has to come from the
        // element's box rather than from wherever the band starts — with a
        // position on top of that, which shifts the phase again.
        //
        // Drawn twice: once as one tall canvas, once as bands, and compared.
        // The tile is patterned, which is load-bearing — see `corner_tile`.
        let tile = corner_tile();
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 20.0,
            height: 30.0,
        };
        let anchor = (rect.x + 1.0, rect.y + 3.0);

        let mut whole = Pixmap::new(20, 30).expect("pixmap");
        whole.fill(tiny_skia::Color::WHITE);
        tile_image(&mut whole, &tile, &rect, anchor, BackgroundRepeat::Repeat);

        for top in [0.0, 7.0, 11.0, 22.0] {
            let height = 8.0_f32.min(30.0 - top);
            let mut band = Pixmap::new(20, height as u32).expect("pixmap");
            band.fill(tiny_skia::Color::WHITE);
            let slice = banded(&rect, top, height).expect("the band overlaps");
            tile_image(
                &mut band,
                &tile,
                &shifted(&slice, 0.0, top),
                (anchor.0, anchor.1 - top),
                BackgroundRepeat::Repeat,
            );

            for y in 0..height as u32 {
                for x in 0..20 {
                    assert_eq!(
                        is_red(&band, x, y),
                        is_red(&whole, x, y + top as u32),
                        "row {} of the band from {top} differs at x={x}",
                        y,
                    );
                }
            }
        }
    }

    #[test]
    fn a_tile_is_clipped_to_its_box() {
        // The last tile in a row usually overhangs. Without clipping it paints
        // over whatever sits beside the box.
        let mut pixmap = canvas();
        let rect = Rect {
            x: 2.0,
            y: 2.0,
            width: 6.0,
            height: 6.0,
        };
        tile_image(
            &mut pixmap,
            &red_tile(),
            &rect,
            (rect.x, rect.y),
            BackgroundRepeat::Repeat,
        );
        assert!(is_red(&pixmap, 7, 7), "inside the box");
        assert!(!is_red(&pixmap, 8, 7), "past its right edge");
        assert!(!is_red(&pixmap, 1, 1), "before its top-left corner");
    }
}

#[cfg(test)]
mod canvas_background_tests {
    use super::*;
    use css::Stylesheet;

    fn list_for(html: &str) -> (DisplayList, dom::Document) {
        let doc = dom::parse(html);
        let styles = css::cascade::cascade(&doc, &[Stylesheet::default()]);
        let mut fonts = FontStore::new();
        let sizes = layout::IntrinsicSizes::new();
        let layout = layout::layout(&doc, &styles, &mut fonts, &sizes, 200.0, 200.0);
        (build_display_list(&layout), doc)
    }

    fn tiles(list: &DisplayList) -> usize {
        list.items
            .iter()
            .filter(|item| matches!(item, DisplayItem::Tile { .. }))
            .count()
    }

    #[test]
    fn a_body_tile_goes_to_the_canvas_and_is_not_drawn_again() {
        // §14.2: the propagated background belongs to the canvas, and the body
        // box must not paint it a second time. With an opaque tile that is
        // merely wasteful; with a translucent one it doubles the alpha.
        let (list, doc) = list_for(r#"<body background="tile.gif"><p>x</p></body>"#);
        let body = doc.find_element("body").expect("body");
        assert_eq!(list.canvas_image.map(|(node, ..)| node), Some(body));
        assert_eq!(tiles(&list), 0, "the body must not tile itself as well");
    }

    #[test]
    fn a_tile_on_an_ordinary_element_is_drawn_where_it_sits() {
        let (list, _) =
            list_for(r#"<body><table background="tile.gif"><tr><td>x</td></tr></table></body>"#);
        assert_eq!(list.canvas_image, None, "a table is not the canvas");
        assert_eq!(tiles(&list), 1);
    }

    #[test]
    fn the_root_tile_wins_over_the_body_one() {
        let (list, doc) = list_for(
            "<html style=\"background-image: url(root.gif)\">\
             <body background=\"body.gif\"><p>x</p></body></html>",
        );
        let html = doc.find_element("html").expect("html");
        assert_eq!(list.canvas_image.map(|(node, ..)| node), Some(html));
        // The body's own tile is not propagated, so it still paints normally.
        assert_eq!(tiles(&list), 1);
    }

    #[test]
    fn a_root_colour_keeps_the_body_tile_on_the_body() {
        // §14.2 propagates the body's background only when the root has *none*
        // — no image and no colour. A root with a colour alone used to leave
        // the body's tile going to the canvas anyway, which is a different
        // rectangle and so a different part of the tile.
        let (list, _) = list_for(
            "<html style=\"background-color: navy\">\
             <body background=\"body.gif\"><p>x</p></body></html>",
        );
        assert_eq!(
            list.canvas_image, None,
            "the root has a background of its own"
        );
        assert_eq!(tiles(&list), 1, "so the body paints its own tile");
    }

    #[test]
    fn the_canvas_tile_is_positioned_against_the_root_and_not_the_window() {
        // §14.2 paints it over the whole canvas but places it "as if it was
        // painted for the root element alone", so the positioning area is that
        // element's padding box. Measured from the window instead, a root with
        // a margin puts the tile in the wrong place — and a negative offset
        // puts it off the canvas entirely.
        let (list, _) = list_for(
            "<html style=\"margin: 20px; border: 5px solid red; \
             background-image: url(root.gif)\"><body><p>x</p></body></html>",
        );
        let (.., area) = list.canvas_image.expect("a canvas tile");
        assert_eq!((area.x, area.y), (25.0, 25.0), "margin then border");
    }

    #[test]
    fn a_propagated_background_colour_is_not_painted_over_the_canvas_again() {
        // §14.2's other half, and the one that was wrong. The colour reached
        // the canvas *and* stayed on the anonymous root box, which is as tall
        // as the content rather than as tall as the window — so it laid an
        // opaque rectangle over the top of whatever the canvas held. Invisible
        // while the canvas held only the same colour; it cost the whole lower
        // part of a `no-repeat` background image taller than the page's text.
        let (list, _) = list_for(
            r#"<body style="background: #00ff00 url(tile.gif) no-repeat"><p>x</p></body>"#,
        );
        assert_eq!(
            list.canvas,
            Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255
            },
            "the colour did not reach the canvas"
        );
        let fills = list
            .items
            .iter()
            .filter(|item| {
                matches!(item, DisplayItem::Rect { color, .. }
                    if !color.is_transparent())
            })
            .count();
        assert_eq!(fills, 0, "the canvas colour was painted a second time");
    }

    #[test]
    fn an_ordinary_background_colour_is_still_painted() {
        // The guard above is on the canvas box alone, so it must not have
        // taken every background with it.
        let (list, _) = list_for(r#"<body><p style="background: #00ff00">x</p></body>"#);
        assert!(
            list.items
                .iter()
                .any(|item| matches!(item, DisplayItem::Rect { color, .. }
                    if !color.is_transparent())),
            "a paragraph's background went missing"
        );
    }
}

#[cfg(test)]
mod missing_image_tests {
    use super::*;

    /// A canvas with one `<img>`-shaped hole in it, and no image to fill it.
    fn placeholder(width: f32, height: f32) -> Pixmap {
        let mut pixmap = Pixmap::new(width as u32 + 20, height as u32 + 20).expect("a pixmap");
        pixmap.fill(tiny_skia::Color::from_rgba8(0xff, 0xff, 0xff, 0xff));
        let mut fonts = FontStore::new();
        draw_missing(
            &mut pixmap,
            &mut fonts,
            &Rect {
                x: 10.0,
                y: 10.0,
                width,
                height,
            },
        );
        pixmap
    }

    /// How many pixels are not the white the canvas started as.
    fn marked(pixmap: &Pixmap) -> usize {
        pixmap
            .pixels()
            .iter()
            .filter(|pixel| pixel.red() != 0xff || pixel.green() != 0xff || pixel.blue() != 0xff)
            .count()
    }

    #[test]
    fn a_spacer_gif_stays_invisible() {
        // The failure every broken-image icon has shipped with. A great deal of
        // the era's markup is 1x1 spacers and 10px bullets holding a table
        // layout open, and drawing anything at all in those turns a page of
        // invisible scaffolding into a page of smudges.
        for size in [1.0, 4.0, 10.0] {
            assert_eq!(
                marked(&placeholder(size, size)),
                0,
                "a {size}x{size} image drew something"
            );
        }
    }

    #[test]
    fn a_thumbnail_gets_an_outline_and_no_words() {
        // Big enough to be a picture somebody would miss, too small to say two
        // words in. The outline alone is the honest answer: there is a box
        // here, it is empty, and there is nowhere to explain why.
        let small = placeholder(40.0, 40.0);
        assert!(marked(&small) > 0, "a 40x40 image drew nothing at all");

        // The label would need more than 40px. If this ever stops being true
        // the assertion below is what says so, rather than the words quietly
        // being clipped at both ends — which is how this was first wrong.
        let mut fonts = FontStore::new();
        let width = fonts
            .layout(
                MISSING_IMAGE_LABEL,
                &css::style::ComputedStyle {
                    font_size: MISSING_TEXT,
                    ..css::style::ComputedStyle::default()
                },
                f32::MAX,
            )
            .width;
        assert!(width + MISSING_PAD * 2.0 > 40.0, "{width}");
    }

    #[test]
    fn a_picture_sized_box_says_what_pressing_it_does() {
        let big = placeholder(240.0, 160.0);
        let small = placeholder(40.0, 40.0);
        // Per pixel of box, because the big one is thirty times the area: what
        // says the words are there is ink in the middle, not ink at all.
        let density = |pixmap: &Pixmap, area: f32| marked(pixmap) as f32 / area;
        assert!(
            density(&big, 240.0 * 160.0) > density(&small, 40.0 * 40.0) / 4.0,
            "the large box has no more in it than its outline"
        );

        // And there is ink across the middle of it, which is where the label is
        // and nowhere else. A band rather than one pixel: the centre of
        // "Load image" is the space between the two words, so the single
        // sample this started as landed on the plate and called it empty.
        let row = (160 / 2 + 10) * big.width() as usize;
        let inked = big.pixels()[row + 20..row + 240]
            .iter()
            .filter(|pixel| pixel.red() < 0xc0)
            .count();
        assert!(
            inked > 10,
            "only {inked} inked pixels across the middle of a 240x160 \
             placeholder, so the label is not there"
        );
    }
}

#[cfg(test)]
mod tofu_tests {
    use super::*;

    /// One line of `text` at 20px, rasterised onto white.
    fn draw(text: &str) -> Pixmap {
        let mut fonts = FontStore::new();
        let style = css::style::ComputedStyle {
            font_size: 20.0,
            ..css::style::ComputedStyle::default()
        };
        let runs = [text::InlineRun::text(text, style.clone())];
        let layout = fonts.layout_runs(&runs, &style, 400.0);
        let mut pixmap = Pixmap::new(400, 60).expect("a pixmap");
        pixmap.fill(tiny_skia::Color::from_rgba8(0xff, 0xff, 0xff, 0xff));
        for line in &layout.lines {
            for glyph in &line.glyphs {
                draw_glyph(
                    &mut pixmap,
                    &mut fonts,
                    glyph,
                    0.0,
                    30.0,
                    Color::rgb(0, 0, 0),
                );
            }
        }
        pixmap
    }

    /// How many pixels are not the white the canvas started as.
    fn inked(pixmap: &Pixmap) -> usize {
        pixmap
            .pixels()
            .iter()
            .filter(|pixel| pixel.red() != 0xff || pixel.green() != 0xff || pixel.blue() != 0xff)
            .count()
    }

    #[test]
    fn a_script_the_bundled_fonts_do_not_cover_draws_something() {
        // #97. The shaper resolves these to `.notdef` and gives them real
        // advances, so the line was always the right length — and then nothing
        // was drawn, so a page in Chinese came out blank. A reader cannot tell
        // "this page is empty" from "this browser has no font for it".
        for (script, text) in [
            ("CJK", "你好世界"),
            ("Arabic", "مرحبا"),
            ("Devanagari", "नमस्ते"),
            ("Thai", "สวัสดี"),
        ] {
            let ink = inked(&draw(text));
            assert!(ink > 0, "{script} drew nothing at all");
        }
    }

    #[test]
    fn a_script_the_fonts_do_cover_still_draws_its_own_glyphs() {
        // The tofu must not be reaching text that has glyphs. Hebrew is the
        // interesting one: Liberation is metric-compatible with Arial and
        // inherits its coverage, which is why it renders where Arabic does not.
        for (script, text) in [
            ("Latin", "Hello"),
            ("Greek", "Καλημέρα"),
            ("Hebrew", "שלום"),
        ] {
            let pixmap = draw(text);
            let ink = inked(&pixmap);
            assert!(ink > 0, "{script} drew nothing");
            // A tofu is a hollow rectangle: its rows are either empty or have
            // ink at both ends and none between. Real text is not that regular,
            // so a row with ink somewhere strictly inside it is proof of a
            // glyph rather than a box.
            let inside = (0..pixmap.height()).any(|y| {
                let row: Vec<bool> = (0..pixmap.width())
                    .map(|x| {
                        let p = pixmap.pixels()[(y * pixmap.width() + x) as usize];
                        p.red() != 0xff || p.green() != 0xff || p.blue() != 0xff
                    })
                    .collect();
                match (
                    row.iter().position(|on| *on),
                    row.iter().rposition(|on| *on),
                ) {
                    (Some(first), Some(last)) if last > first + 1 => {
                        row[first + 1..last].iter().any(|on| *on)
                    }
                    _ => false,
                }
            });
            assert!(inside, "{script} drew hollow boxes rather than glyphs");
        }
    }

    #[test]
    fn a_space_is_not_drawn_as_a_box() {
        // A space has a glyph and an advance; only a character with *no* glyph
        // gets a box. Getting this wrong would put a box between every word.
        assert_eq!(inked(&draw(" ")), 0);
        assert_eq!(inked(&draw("   ")), 0);
    }

    #[test]
    fn a_tofu_is_no_wider_than_the_advance_it_stands_in() {
        // Otherwise a run of them overlaps and reads as a bar rather than as
        // one box per character.
        let mut fonts = FontStore::new();
        let style = css::style::ComputedStyle {
            font_size: 20.0,
            ..css::style::ComputedStyle::default()
        };
        let runs = [text::InlineRun::text("你好", style.clone())];
        let layout = fonts.layout_runs(&runs, &style, 400.0);
        let line = layout.lines.first().expect("a line");
        let glyphs = &line.glyphs;
        assert_eq!(glyphs.len(), 2, "two characters, two glyphs");
        assert!(
            glyphs[0].advance > 0.0,
            "an uncovered character still reserves its width",
        );
        assert!(
            glyphs[0].x + glyphs[0].advance <= glyphs[1].x + 0.01,
            "the first box ends before the second begins",
        );
    }

    #[test]
    fn text_too_small_to_read_draws_no_boxes() {
        // A page of 3px scaffolding text would otherwise become a page of
        // smudges — the same judgement the image placeholder makes at its own
        // size.
        let mut fonts = FontStore::new();
        let style = css::style::ComputedStyle {
            font_size: 3.0,
            ..css::style::ComputedStyle::default()
        };
        let runs = [text::InlineRun::text("你好世界", style.clone())];
        let layout = fonts.layout_runs(&runs, &style, 400.0);
        let mut pixmap = Pixmap::new(400, 60).expect("a pixmap");
        pixmap.fill(tiny_skia::Color::from_rgba8(0xff, 0xff, 0xff, 0xff));
        for line in &layout.lines {
            for glyph in &line.glyphs {
                draw_glyph(
                    &mut pixmap,
                    &mut fonts,
                    glyph,
                    0.0,
                    30.0,
                    Color::rgb(0, 0, 0),
                );
            }
        }
        assert_eq!(inked(&pixmap), 0);
    }
}

#[cfg(test)]
mod rtl_inline_side_tests {
    use super::*;
    use css::style::{BorderSide, BorderStyle, Direction};

    /// A span's style with a border only on the side named, so which side got
    /// drawn can be read off the display list by colour.
    fn bordered(direction: Direction) -> css::style::ComputedStyle {
        let side = |color: Color| BorderSide {
            width: css::value::Length::Px(4.0),
            style: BorderStyle::Solid,
            color: Some(color),
        };
        let mut style = css::style::ComputedStyle {
            direction,
            ..css::style::ComputedStyle::default()
        };
        style.border.left = side(Color::rgb(0xff, 0x00, 0x00));
        style.border.right = side(Color::rgb(0x00, 0x00, 0xff));
        style
    }

    /// Which border colours a fragment drew.
    fn sides(fragment: &text::InlineBoxFragment, style: &css::style::ComputedStyle) -> Vec<Color> {
        let mut list = DisplayList::default();
        paint_inline_box(fragment, style, 0.0, 0.0, &mut list);
        list.items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Rect { color, .. } => Some(*color),
                _ => None,
            })
            .collect()
    }

    fn fragment(opens: bool, closes: bool) -> text::InlineBoxFragment {
        text::InlineBoxFragment {
            source: 0,
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 16.0,
            opens,
            closes,
        }
    }

    const RED: Color = Color::rgb(0xff, 0x00, 0x00);
    const BLUE: Color = Color::rgb(0x00, 0x00, 0xff);

    #[test]
    fn a_left_to_right_box_opens_on_the_left() {
        let style = bordered(Direction::Ltr);
        let first = sides(&fragment(true, false), &style);
        assert!(first.contains(&RED), "the first fragment drew no left side");
        assert!(
            !first.contains(&BLUE),
            "the first fragment drew the closing side too",
        );
        let last = sides(&fragment(false, true), &style);
        assert!(last.contains(&BLUE));
        assert!(!last.contains(&RED));
    }

    #[test]
    fn a_right_to_left_box_opens_on_the_right() {
        // #113. §9.10: a right-to-left box begins on the right, so its opening
        // side is the physical *right* border. Painting `opens` as the left
        // unconditionally put both sides on the first fragment of an rtl box
        // and neither on the last.
        let style = bordered(Direction::Rtl);
        let first = sides(&fragment(true, false), &style);
        assert!(
            first.contains(&BLUE),
            "the first fragment of an rtl box drew no right side",
        );
        assert!(
            !first.contains(&RED),
            "it drew the left side, which belongs to the last fragment",
        );
        let last = sides(&fragment(false, true), &style);
        assert!(last.contains(&RED));
        assert!(!last.contains(&BLUE));
    }

    #[test]
    fn a_middle_fragment_draws_neither_side_in_either_direction() {
        for direction in [Direction::Ltr, Direction::Rtl] {
            let drawn = sides(&fragment(false, false), &bordered(direction));
            assert!(!drawn.contains(&RED), "{direction:?} drew a left side");
            assert!(!drawn.contains(&BLUE), "{direction:?} drew a right side");
        }
    }

    #[test]
    fn a_box_on_one_line_draws_both_sides_in_either_direction() {
        for direction in [Direction::Ltr, Direction::Rtl] {
            let drawn = sides(&fragment(true, true), &bordered(direction));
            assert!(drawn.contains(&RED), "{direction:?} lost its left side");
            assert!(drawn.contains(&BLUE), "{direction:?} lost its right side");
        }
    }
}

#[cfg(test)]
mod stacking_context_tests {
    use super::*;
    use css::Stylesheet;

    /// The colours of the filled rectangles a page paints, in paint order.
    fn painted(html: &str, css_text: &str) -> Vec<Color> {
        let doc = dom::parse(html);
        let styles = css::cascade::cascade(
            &doc,
            &[
                Stylesheet::parse(css::ua::UA_STYLESHEET),
                Stylesheet::parse(css_text),
            ],
        );
        let mut fonts = FontStore::new();
        let out = layout::layout(
            &doc,
            &styles,
            &mut fonts,
            &layout::IntrinsicSizes::new(),
            300.0,
            300.0,
        );
        build_display_list(&out)
            .items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Rect { color, .. } if !color.is_transparent() => Some(*color),
                _ => None,
            })
            .collect()
    }

    const RED: Color = Color::rgb(0xff, 0x00, 0x00);
    const GREEN: Color = Color::rgb(0x00, 0x80, 0x00);

    /// Whether `first` is painted before `last`, so `last` covers it.
    fn before(order: &[Color], first: Color, last: Color) -> bool {
        match (
            order.iter().position(|c| *c == first),
            order.iter().rposition(|c| *c == last),
        ) {
            (Some(a), Some(b)) => a < b,
            _ => false,
        }
    }

    #[test]
    fn a_float_paints_over_a_later_siblings_background() {
        // #165, and Appendix E: floats are layer 4 and in-flow block
        // backgrounds are layer 3. Since #41 a container is as tall as its
        // in-flow content and no taller, so a float routinely hangs out of the
        // bottom of one — and painting it with its container put it under
        // whatever came next.
        let order = painted(
            "<body><div class=a><span class=f></span></div><div class=b></div></body>",
            "body { margin: 0 }
             .a { height: 10px }
             .f { float: left; width: 50px; height: 60px; background: green }
             .b { height: 60px; background: red }",
        );
        assert!(
            before(&order, RED, GREEN),
            "the float went under the next block's background: {order:?}",
        );
    }

    #[test]
    fn a_float_inside_a_float_is_still_painted() {
        // The float lift stops at a float, because a float establishes a block
        // formatting context and contains its own. Stopping there without
        // having that float gather them is how they stop being painted at all
        // — 27 of the `margin-padding-clear` tests, which are built out of
        // exactly this shape.
        let order = painted(
            "<body><div class=outer><div class=inner></div></div></body>",
            "body { margin: 0 }
             .outer { float: left; width: 80px; height: 80px; background: red }
             .inner { float: left; width: 40px; height: 40px; background: green }",
        );
        assert!(
            order.contains(&GREEN),
            "the nested float vanished: {order:?}"
        );
        assert!(
            before(&order, RED, GREEN),
            "and it belongs over its container: {order:?}",
        );
    }

    #[test]
    fn a_float_still_paints_under_a_positioned_box() {
        // Layer 4 is below layer 6. Lifting floats must not lift them past the
        // positioned boxes that were already being lifted.
        let order = painted(
            "<body><span class=f></span><div class=p></div></body>",
            "body { margin: 0 }
             .f { float: left; width: 50px; height: 50px; background: red }
             .p { position: absolute; top: 0; left: 0;
                  width: 50px; height: 50px; background: green }",
        );
        assert!(
            before(&order, RED, GREEN),
            "a float painted over a positioned box: {order:?}",
        );
    }

    #[test]
    fn a_z_index_auto_box_does_not_seal_a_negative_descendant() {
        // #107, and the `#absolute` half of `visuren/fixed-pos-stacking-001`.
        // §9.9.1: a positioned box with `z-index: auto` makes a box in its
        // parent's stacking context and not a context of its own, so a
        // `z-index: -1` descendant escapes to the nearest real context and
        // paints behind that box's own background.
        let order = painted(
            "<body><div id=\"a\"><div><div id=\"b\"></div></div></div></body>",
            "#a { position: absolute; background: #008000; width: 50px; height: 50px } \
             #a div { position: absolute } \
             #b { position: absolute; z-index: -1; background: #ff0000; \
                  width: 50px; height: 50px }",
        );
        assert!(
            before(&order, RED, GREEN),
            "the red descendant did not get behind the green ancestor: {order:?}",
        );
    }

    #[test]
    fn a_fixed_box_does_seal_one() {
        // The `#fixed` half of the same test. CSS 2.1 does not say so in as
        // many words; every browser does it, and the suite tests for it.
        let order = painted(
            "<body><div id=\"a\"><div><div id=\"b\"></div></div></div></body>",
            "#a { position: fixed; background: #ff0000; width: 50px; height: 50px } \
             #a div { position: absolute } \
             #b { position: absolute; z-index: -1; background: #008000; \
                  width: 50px; height: 50px }",
        );
        assert!(
            before(&order, RED, GREEN),
            "the descendant escaped a fixed box, which is a context: {order:?}",
        );
    }

    #[test]
    fn positioned_siblings_keep_tree_order_when_their_z_ties() {
        // `zindex/z-index-004`: one absolutely positioned box with `z-index: 0`
        // and one with `z-index: auto`, siblings. They tie, so the later one
        // wins — which only works if the lifted boxes keep their document
        // order rather than being appended after the ones that were not
        // lifted.
        let order = painted(
            "<body><div id=\"w\"><div id=\"a\"></div><div id=\"b\"></div></div></body>",
            "#w { position: relative } \
             #a { position: absolute; z-index: 0; background: #ff0000; \
                  width: 50px; height: 50px } \
             #b { position: absolute; background: #008000; \
                  width: 50px; height: 50px }",
        );
        assert!(
            before(&order, RED, GREEN),
            "the later sibling did not paint last: {order:?}",
        );
    }

    #[test]
    fn a_numeric_z_index_still_sorts_against_its_siblings() {
        // The half that must not change: a negative one goes behind.
        let order = painted(
            "<body><div id=\"a\"></div><div id=\"b\"></div></body>",
            "#a { position: absolute; background: #008000; width: 50px; height: 50px } \
             #b { position: absolute; z-index: -1; background: #ff0000; \
                  width: 50px; height: 50px }",
        );
        assert!(before(&order, RED, GREEN), "{order:?}");
    }

    #[test]
    fn what_forms_a_context_and_what_does_not() {
        let context = |position, z| {
            let box_ = LayoutBox {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                },
                style: css::style::ComputedStyle {
                    position,
                    z_index: z,
                    ..css::style::ComputedStyle::default()
                },
                text: None,
                content_origin: (0.0, 0.0),
                content_width: 0.0,
                children: Vec::new(),
                replaced: None,
                replaced_image: false,
                node: None,
                round: false,
                chosen_rows: Vec::new(),
                top_border_gap: None,
                absolute_at: None,
            };
            forms_a_stacking_context(&box_)
        };
        use css::style::Position;
        assert!(!context(Position::Static, None));
        assert!(!context(Position::Static, Some(3)), "z means nothing here");
        assert!(
            !context(Position::Absolute, None),
            "`auto` is not a context"
        );
        assert!(context(Position::Absolute, Some(0)));
        assert!(context(Position::Relative, Some(-1)));
        assert!(context(Position::Fixed, None), "fixed always is");
    }
}
