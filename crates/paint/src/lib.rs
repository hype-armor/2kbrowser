//! Display list construction and rasterisation.
//!
//! Rasterisation is on the CPU via `tiny-skia` (ADR-0005): no GPU dependency,
//! and combined with bundled fonts it makes output identical on every platform,
//! so one set of reference baselines covers all three.

use css::value::Color;
use layout::{Layout, LayoutBox, Rect, line_offset};
use text::FontStore;
use tiny_skia::{FillRule, Paint, PathBuilder, PixmapPaint, Transform};

// Re-exported so consumers do not need their own tiny-skia dependency, and so
// the rasteriser choice stays an implementation detail of this crate.
pub use tiny_skia::Pixmap;

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
#[derive(Debug, Clone, Default)]
pub struct DisplayList {
    /// The items, in paint order.
    pub items: Vec<DisplayItem>,
}

/// Builds a display list from a layout.
pub fn build_display_list(layout: &Layout) -> DisplayList {
    let mut list = DisplayList::default();
    // The canvas starts white; a transparent page background would otherwise
    // composite against whatever the window happened to contain.
    list.items.push(DisplayItem::Rect {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: layout.root.rect.width,
            height: layout.height,
        },
        color: Color::WHITE,
    });
    paint_box(&layout.root, 0.0, 0.0, &mut list);
    list
}

fn paint_box(box_: &LayoutBox, offset_x: f32, offset_y: f32, list: &mut DisplayList) {
    let x = offset_x + box_.rect.x;
    let y = offset_y + box_.rect.y;

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

    if let Some(layout) = &box_.text {
        let content_x = x + box_.content_origin.0;
        let content_y = y + box_.content_origin.1;
        let content_width = box_.rect.width - box_.content_origin.0 * 2.0;
        for line in &layout.lines {
            let dx = line_offset(box_.style.text_align, line.width, content_width);
            for glyph in &line.glyphs {
                list.items.push(DisplayItem::Glyph {
                    glyph: *glyph,
                    origin_x: content_x + dx,
                    origin_y: content_y,
                    color: box_.style.color,
                });
            }
        }
    }

    for child in &box_.children {
        paint_box(child, x, y, list);
    }
}

/// Rasterises a display list into a new pixmap.
///
/// Returns `None` only for a zero-sized canvas.
pub fn rasterise(
    list: &DisplayList,
    fonts: &mut FontStore,
    width: u32,
    height: u32,
) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(width.max(1), height.max(1))?;
    pixmap.fill(tiny_skia::Color::WHITE);

    for item in &list.items {
        match item {
            DisplayItem::Rect { rect, color } => fill_rect(&mut pixmap, rect, *color),
            DisplayItem::Glyph {
                glyph,
                origin_x,
                origin_y,
                color,
            } => {
                draw_glyph(&mut pixmap, fonts, glyph, *origin_x, *origin_y, *color);
            }
        }
    }
    Some(pixmap)
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

    let x = (origin_x + glyph.x) as i32 + left;
    let y = (origin_y + glyph.y) as i32 - top;
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
        let layout = layout::layout(&doc, &styles, &mut fonts, width as f32);
        let list = build_display_list(&layout);
        let height = layout.height.ceil().max(1.0) as u32;
        rasterise(&list, &mut fonts, width, height).expect("pixmap")
    }

    fn count_non_white(pixmap: &Pixmap) -> usize {
        pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() != 255 || p.green() != 255 || p.blue() != 255)
            .count()
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
