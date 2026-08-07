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
    pub canvas_image: Option<(dom::NodeId, css::style::BackgroundRepeat)>,
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
    let propagated = layout.canvas_image.map(|(node, _)| node);
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
        list.items.push(DisplayItem::Rect {
            rect: Rect {
                x,
                y,
                width,
                height: top,
            },
            color: color_of(&border.top),
        });
    }
    if border.bottom.style.is_visible() && bottom > 0.0 {
        list.items.push(DisplayItem::Rect {
            rect: Rect {
                x,
                y: y + height - bottom,
                width,
                height: bottom,
            },
            color: color_of(&border.bottom),
        });
    }
    let side_height = (height - top - bottom).max(0.0);
    if border.left.style.is_visible() && left > 0.0 {
        list.items.push(DisplayItem::Rect {
            rect: Rect {
                x,
                y: y + top,
                width: left,
                height: side_height,
            },
            color: color_of(&border.left),
        });
    }
    if border.right.style.is_visible() && right > 0.0 {
        list.items.push(DisplayItem::Rect {
            rect: Rect {
                x: x + width - right,
                y: y + top,
                width: right,
                height: side_height,
            },
            color: color_of(&border.right),
        });
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
    let mut pixmap = Pixmap::new(width.max(1), height.max(1))?;
    // Clearing to the canvas colour rather than white is what carries the
    // page's background down past the end of its content.
    let canvas = list.canvas;
    pixmap.fill(tiny_skia::Color::from_rgba8(
        canvas.r, canvas.g, canvas.b, canvas.a,
    ));

    // The canvas tile goes over the canvas colour and under everything else.
    if let Some((node, repeat)) = list.canvas_image
        && let Some(image) = images.get(&ImageKey::background(node))
    {
        let full = Rect {
            x: 0.0,
            y: 0.0,
            width: pixmap.width() as f32,
            height: pixmap.height() as f32,
        };
        tile_image(&mut pixmap, image, &full, repeat);
    }

    for item in &list.items {
        // Nothing beyond the drawable range is drawn at all. See `MAX_COORD`:
        // this is the one place every item passes through, so it is the one
        // place the check has to be.
        match item {
            DisplayItem::Rect { rect, color } => {
                if drawable(rect) {
                    fill_rect(&mut pixmap, rect, *color);
                }
            }
            DisplayItem::Image { node, rect } => {
                if drawable(rect)
                    && let Some(image) = images.get(&ImageKey::content(*node))
                {
                    draw_image(&mut pixmap, image, rect);
                }
            }
            DisplayItem::Tile { node, rect, repeat } => {
                if drawable(rect)
                    && let Some(image) = images.get(&ImageKey::background(*node))
                {
                    tile_image(&mut pixmap, image, rect, *repeat);
                }
            }
            DisplayItem::Glyph {
                glyph,
                origin_x,
                origin_y,
                color,
            } => {
                if in_range(*origin_x) && in_range(*origin_y) {
                    draw_glyph(&mut pixmap, fonts, glyph, *origin_x, *origin_y, *color);
                }
            }
        }
    }
    Some(pixmap)
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

/// Tiles a background image across `rect`, at its natural size.
///
/// A background image is never scaled — that is what distinguishes it from a
/// content image, and it is why a 20-pixel tile fills a page rather than being
/// stretched across it. Tiles are drawn from the box's top-left corner, which
/// is `background-position: 0 0`.
fn tile_image(
    pixmap: &mut Pixmap,
    image: &DecodedImage,
    rect: &Rect,
    repeat: css::style::BackgroundRepeat,
) {
    let (width, height) = (image.width(), image.height());
    if rect.width <= 0.0 || rect.height <= 0.0 || width < 1.0 || height < 1.0 {
        return;
    }

    let (tile_x, tile_y) = repeat.axes();
    // A tile count rather than a while-loop on coordinates: with a 1px tile and
    // a tall page the loop is long, and bounding it here keeps a pathological
    // image from turning into an unbounded amount of work.
    let columns = if tile_x {
        (rect.width / width).ceil() as u32
    } else {
        1
    };
    let rows = if tile_y {
        (rect.height / height).ceil() as u32
    } else {
        1
    };

    let clip = tiny_skia::IntRect::from_xywh(
        rect.x.floor() as i32,
        rect.y.floor() as i32,
        rect.width.ceil() as u32,
        rect.height.ceil() as u32,
    );
    let Some(mask) = clip.and_then(|clip| {
        let mut mask = tiny_skia::Mask::new(pixmap.width(), pixmap.height())?;
        let mut builder = PathBuilder::new();
        builder.push_rect(clip.to_rect());
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
                (rect.x + column as f32 * width).round() as i32,
                (rect.y + row as f32 * height).round() as i32,
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

    // The shaper gives 8-bit coverage; colour is applied here. Premultiplied,
    // because that is what tiny-skia composites in.
    let mut glyph_pixmap = match Pixmap::new(width as u32, height as u32) {
        Some(pixmap) => pixmap,
        None => return,
    };
    for (index, pixel) in glyph_pixmap.pixels_mut().iter_mut().enumerate() {
        let alpha = u32::from(coverage[index]) * u32::from(color.a) / 255;
        let scale = |channel: u8| (u32::from(channel) * alpha / 255) as u8;
        *pixel = tiny_skia::PremultipliedColorU8::from_rgba(
            scale(color.r),
            scale(color.g),
            scale(color.b),
            alpha as u8,
        )
        .unwrap_or_else(|| {
            tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 0).expect("transparent")
        });
    }

    // Checked rather than trusted even after the range guard above: `left`,
    // `top`, and the bitmap's own size come from the rasteriser rather than
    // from us, and what tiny-skia actually panics on is `x + width` leaving
    // `i32` — so that is the sum to prove cannot.
    let (Some(x), Some(y)) = ((x as i32).checked_add(left), (y as i32).checked_sub(top)) else {
        return;
    };
    let (Some(_), Some(_)) = (
        i32::try_from(width).ok().and_then(|w| x.checked_add(w)),
        i32::try_from(height).ok().and_then(|h| y.checked_add(h)),
    ) else {
        return;
    };
    pixmap.draw_pixmap(
        x,
        y,
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
mod tile_tests {
    use super::*;
    use css::style::BackgroundRepeat;

    /// A 4x4 image, entirely opaque red.
    fn red_tile() -> DecodedImage {
        let mut pixmap = Pixmap::new(4, 4).expect("pixmap");
        pixmap.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
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
        tile_image(&mut pixmap, &red_tile(), &rect, BackgroundRepeat::Repeat);
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
        tile_image(&mut pixmap, &red_tile(), &rect, BackgroundRepeat::RepeatX);
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
            BackgroundRepeat::RepeatX,
        );
        assert_eq!(red_pixels(&horizontal), 80, "one row of tiles");

        let mut vertical = canvas();
        tile_image(&mut vertical, &red_tile(), &rect, BackgroundRepeat::RepeatY);
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
        tile_image(&mut pixmap, &red_tile(), &rect, BackgroundRepeat::NoRepeat);
        assert_eq!(red_pixels(&pixmap), 16);
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
        tile_image(&mut pixmap, &red_tile(), &rect, BackgroundRepeat::Repeat);
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
        assert_eq!(list.canvas_image.map(|(node, _)| node), Some(body));
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
        assert_eq!(list.canvas_image.map(|(node, _)| node), Some(html));
        // The body's own tile is not propagated, so it still paints normally.
        assert_eq!(tiles(&list), 1);
    }
}
