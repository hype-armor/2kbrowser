//! Display list construction and rasterisation.
//!
//! Rasterisation is on the CPU via `tiny-skia` (ADR-0005): no GPU dependency,
//! and combined with bundled fonts it makes output identical on every platform,
//! so one set of reference baselines covers all three.

pub mod images;

use css::value::Color;
use layout::{Layout, LayoutBox, Rect, line_offset};
use text::FontStore;
use tiny_skia::{FillRule, Paint, PathBuilder};

// Re-exported so consumers do not need their own tiny-skia dependency, and so
// the rasteriser choice stays an implementation detail of this crate.
pub use images::{DecodedImage, ImageKey, ImageSlot, ImageStore, decode};
// Re-exported for consumers that composite pixmaps of their own, such as the
// frameset renderer.
pub use tiny_skia::{
    Color as RasterColor, IntSize, Pixmap, PixmapPaint, PremultipliedColorU8 as PremultipliedColor,
    Transform,
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
    /// A decoded image drawn into a rectangle.
    Image {
        /// The element the image belongs to, used to look it up at raster time.
        node: dom::NodeId,
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
    pub canvas_image: Option<(
        dom::NodeId,
        css::style::BackgroundRepeat,
        css::style::BackgroundPosition,
    )>,
    /// The items, in paint order.
    pub items: Vec<DisplayItem>,
}

impl Default for DisplayList {
    fn default() -> Self {
        // White, not transparent: a page that declares no background would
        // otherwise composite against whatever the window happened to contain.
        Self {
            canvas: Color::WHITE,
            canvas_image: None,
            items: Vec::new(),
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
    };
    // §14.2 again: an element whose background was propagated to the canvas
    // does not paint it a second time. Drawing it twice is invisible while the
    // colour is opaque and wrong the moment it is not.
    let propagated = layout.canvas_image.map(|(node, ..)| node);
    paint_box(&layout.root, 0.0, 0.0, propagated, &mut list);
    list
}

fn paint_box(
    box_: &LayoutBox,
    offset_x: f32,
    offset_y: f32,
    propagated: Option<dom::NodeId>,
    list: &mut DisplayList,
) {
    let x = offset_x + box_.rect.x;
    let y = offset_y + box_.rect.y;
    let is_canvas_background = box_.node.is_some() && box_.node == propagated;

    if !box_.style.background_color.is_transparent() {
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
    if box_.style.background_image.is_some()
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

    paint_borders(box_, x, y, list);

    if let Some(node) = box_.replaced {
        list.items.push(DisplayItem::Image {
            node,
            rect: Rect {
                x: x + box_.content_origin.0,
                y: y + box_.content_origin.1,
                width: box_.content_width,
                height: (box_.rect.height - box_.content_origin.1 * 2.0).max(0.0),
            },
        });
    }

    if let Some(layout) = &box_.text {
        let content_x = x + box_.content_origin.0;
        let content_y = y + box_.content_origin.1;
        let content_width = box_.content_width;
        for line in &layout.lines {
            let dx = line_offset(box_.style.text_align, line.width, content_width);
            // Rules go under the glyphs so an underline sitting close to a
            // descender is crossed by it rather than cutting through it.
            for rule in &line.decorations {
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
            for glyph in &line.glyphs {
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

    for child in &box_.children {
        paint_box(child, x, y, propagated, list);
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
        push_border_side(
            list,
            &Rect {
                x,
                y,
                width,
                height: top,
            },
            border.top.style,
            top,
            Side::Top,
            color_of(&border.top),
        );
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
    rasterise_band(list, fonts, images, width, 0.0, height)
}

/// Rasterises the rows `[top, top + height)` of a document.
///
/// The display list is in document coordinates and does not change between
/// bands — it is built once from the layout, and drawing a band is a matter of
/// where the rows are taken from. That is what makes a band cheap: no parse, no
/// cascade, no layout, just paint.
///
/// Items are shifted by `top` as they are drawn rather than the list being
/// rewritten, so nothing is allocated per band and the drawable-range check
/// still sees the coordinate that actually reaches the rasteriser.
pub fn rasterise_band(
    list: &DisplayList,
    fonts: &mut FontStore,
    images: &ImageStore,
    width: u32,
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
    if let Some((node, repeat, position)) = list.canvas_image
        && let Some(image) = images.get(&ImageKey::background(node))
    {
        // The canvas is the whole document, so the anchor is measured against
        // the document rather than the band — otherwise the tiling phase, and
        // any percentage in the position, would move every time the reader
        // scrolled.
        let full = Rect {
            x: 0.0,
            y: 0.0,
            width: pixmap.width() as f32,
            height: top + pixmap.height() as f32,
        };
        let anchor = anchor_of(&full, position, image);
        if let Some(slice) = banded(&full, top, pixmap.height() as f32) {
            let slice = shifted(&slice, top);
            if drawable(&slice) {
                tile_image(
                    &mut pixmap,
                    image,
                    &slice,
                    (anchor.0, anchor.1 - top),
                    repeat,
                );
            }
        }
    }

    for item in &list.items {
        // Nothing beyond the drawable range is drawn at all. See `MAX_COORD`:
        // this is the one place every item passes through, so it is the one
        // place the check has to be.
        match item {
            DisplayItem::Rect { rect, color } => {
                let rect = shifted(rect, top);
                if drawable(&rect) {
                    fill_rect(&mut pixmap, &rect, *color);
                }
            }
            DisplayItem::Image { node, rect } => {
                let rect = shifted(rect, top);
                if drawable(&rect)
                    && let Some(image) = images.get(&ImageKey::content(*node))
                {
                    draw_image(&mut pixmap, image, &rect);
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
                    let slice = shifted(&slice, top);
                    if drawable(&slice) {
                        tile_image(
                            &mut pixmap,
                            image,
                            &slice,
                            (anchor.0, anchor.1 - top),
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
                let origin_y = *origin_y - top;
                if in_range(*origin_x) && in_range(origin_y) {
                    draw_glyph(&mut pixmap, fonts, glyph, *origin_x, origin_y, *color);
                }
            }
        }
    }
    Some(pixmap)
}

/// The same rectangle, moved into a band's coordinates.
fn shifted(rect: &Rect, top: f32) -> Rect {
    Rect {
        y: rect.y - top,
        ..*rect
    }
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
mod tests {
    use super::*;
    use css::Stylesheet;

    fn render(html: &str, css_text: &str, width: u32) -> Pixmap {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css_text)];
        let styles = css::cascade::cascade(&doc, &sheets);
        let mut fonts = FontStore::new();
        let sizes = layout::IntrinsicSizes::new();
        let layout = layout::layout(&doc, &styles, &mut fonts, &sizes, width as f32);
        let list = build_display_list(&layout);
        let height = layout.height.ceil().max(1.0) as u32;
        let images = ImageStore::new();
        rasterise(&list, &mut fonts, &images, width, height).expect("pixmap")
    }

    /// The display list, images, and height for a page, so a test can rasterise
    /// it whole and in pieces and compare.
    fn scene(html: &str, css_text: &str, width: u32) -> (DisplayList, FontStore, u32) {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css_text)];
        let styles = css::cascade::cascade(&doc, &sheets);
        let mut fonts = FontStore::new();
        let sizes = layout::IntrinsicSizes::new();
        let layout = layout::layout(&doc, &styles, &mut fonts, &sizes, width as f32);
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
                let band =
                    rasterise_band(&list, &mut fonts, &images, width, top as f32, band_height)
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
                &shifted(&slice, top),
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
        let layout = layout::layout(&doc, &styles, &mut fonts, &sizes, 200.0);
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
}
