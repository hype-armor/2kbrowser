//! The application's icon, drawn rather than shipped as a picture.
//!
//! A beige-box computer with a filled screen: what the browser is *for*, drawn
//! the way the era drew everything. It is the one piece of this program's
//! interface that appears outside its own window — in a dock, a task bar, an
//! alt-tab list — so it has to hold up at sixteen pixels as well as at five
//! hundred and twelve.
//!
//! **Drawn, not shipped.** The same argument the padlock in the bar settled
//! (ADR-0008): a picture would need one file per size, each one a separate
//! thing to keep in step, and every one of them would be a binary blob nobody
//! reviews. This is a page of geometry that renders at whatever size it is
//! asked for, through the rasteriser the rest of the browser already links —
//! so the icon costs no new dependency, no new asset pipeline, and about two
//! kilobytes of code instead of a quarter of a megabyte of PNGs.
//!
//! The shapes are laid out on a 1024-unit grid and scaled, so every size is the
//! same drawing rather than the same drawing plus rounding luck.

use paint::{FillRule, Paint, Path, PathBuilder, Pixmap, RasterRect, Stroke, Transform};

/// The grid the geometry below is written against.
const GRID: f32 = 1024.0;

/// The ink: outlines, the neck, and the drive slot.
const INK: (u8, u8, u8) = (0x2f, 0x74, 0x82);

/// The screen's glow.
const SCREEN: (u8, u8, u8) = (0x8f, 0xc0, 0xcb);

/// The case, which is a colour rather than a hole.
///
/// The artwork is line work on white and its white is load-bearing: it is the
/// computer's case, not the page behind it. Leaving it transparent would give a
/// dark task bar a teal outline with a screen floating inside it, which is a
/// different drawing.
const CASE: (u8, u8, u8) = (0xff, 0xff, 0xff);

/// How thick the outlines are, on the grid.
const LINE: f32 = 26.0;

/// Thinnest an outline may be once it reaches pixels.
///
/// At the grid width a sixteen-pixel icon's outline is four tenths of a pixel,
/// which antialiases to a grey wash — the drawing is still there and nobody can
/// see it. Every icon set in existence solves this by drawing the small sizes
/// heavier, and this is that, as one number rather than a second drawing.
const MIN_LINE: f32 = 1.4;

/// Smallest icon that gets the drive slot.
///
/// Below it the slot is under three pixels wide and half a pixel tall, which
/// draws as a smudge beside the screen rather than as a detail. A detail nobody
/// can resolve is noise, and at this size every pixel is doing work.
const SLOT_FROM: u32 = 24;

/// Renders the icon at `size` by `size` pixels.
///
/// `None` only when `size` is zero, which is not a thing to ask for.
pub fn render(size: u32) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(size.max(1), size.max(1))?;
    let scale = size as f32 / GRID;
    let at = Transform::from_scale(scale, scale);

    let fill = |pixmap: &mut Pixmap, path: &Path, colour: (u8, u8, u8)| {
        let mut paint = Paint::default();
        paint.set_color_rgba8(colour.0, colour.1, colour.2, 0xff);
        paint.anti_alias = true;
        pixmap.fill_path(path, &paint, FillRule::Winding, at, None);
    };
    let stroke = |pixmap: &mut Pixmap, path: &Path| {
        let mut paint = Paint::default();
        paint.set_color_rgba8(INK.0, INK.1, INK.2, 0xff);
        paint.anti_alias = true;
        let stroke = Stroke {
            width: LINE.max(MIN_LINE / scale),
            ..Stroke::default()
        };
        pixmap.stroke_path(path, &paint, &stroke, at, None);
    };

    // The base first, so the neck and the monitor sit in front of it.
    let base = rounded(234.0, 692.0, 556.0, 86.0, 22.0)?;
    fill(&mut pixmap, &base, CASE);

    // The neck: two short posts from the underside of the monitor down into the
    // base. Drawn before the base's outline so that outline closes across them.
    for x in [310.0, 690.0] {
        let post = rounded(x, 640.0, 24.0, 60.0, 0.0)?;
        fill(&mut pixmap, &post, INK);
    }
    stroke(&mut pixmap, &base);

    // The monitor: a filled case with an outline, so the screen has something
    // to sit on other than whatever is behind the icon.
    let body = rounded(267.0, 249.0, 490.0, 388.0, 38.0)?;
    fill(&mut pixmap, &body, CASE);
    stroke(&mut pixmap, &body);

    // The screen, lit.
    let screen = rounded(329.0, 303.0, 366.0, 224.0, 26.0)?;
    fill(&mut pixmap, &screen, SCREEN);
    stroke(&mut pixmap, &screen);

    // The drive slot, under the screen and to the right, which is the one
    // detail that says this is a computer rather than a television.
    if size >= SLOT_FROM {
        let slot = rounded(536.0, 570.0, 176.0, 32.0, 16.0)?;
        fill(&mut pixmap, &slot, INK);
    }

    Some(pixmap)
}

/// The icon as straight (non-premultiplied) RGBA, for a window manager.
///
/// `Pixmap` holds premultiplied pixels and every icon API in sight wants
/// straight ones. The difference is invisible where the alpha is 0 or 255 and
/// is the whole of the antialiased edge, which on a sixteen-pixel icon is most
/// of the drawing.
pub fn rgba(size: u32) -> Option<(Vec<u8>, u32, u32)> {
    let pixmap = render(size)?;
    let (width, height) = (pixmap.width(), pixmap.height());
    let mut out = Vec::with_capacity(pixmap.data().len());
    for pixel in pixmap.pixels() {
        let alpha = pixel.alpha();
        let straight = |channel: u8| match alpha {
            0 => 0,
            _ => ((u32::from(channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha)).min(255)
                as u8,
        };
        out.extend_from_slice(&[
            straight(pixel.red()),
            straight(pixel.green()),
            straight(pixel.blue()),
            alpha,
        ]);
    }
    Some((out, width, height))
}

/// A rectangle with rounded corners, on the grid.
///
/// A radius of zero is an ordinary rectangle, which is what the neck posts are.
fn rounded(x: f32, y: f32, width: f32, height: f32, radius: f32) -> Option<Path> {
    let r = radius.min(width / 2.0).min(height / 2.0).max(0.0);
    let (right, bottom) = (x + width, y + height);
    let mut path = PathBuilder::new();
    if r == 0.0 {
        path.push_rect(RasterRect::from_xywh(x, y, width, height)?);
        return path.finish();
    }
    // A quarter circle at each corner, as a quadratic through the corner point.
    // Near enough to a circle at every size this is drawn, and one control
    // point rather than the two a cubic needs.
    path.move_to(x + r, y);
    path.line_to(right - r, y);
    path.quad_to(right, y, right, y + r);
    path.line_to(right, bottom - r);
    path.quad_to(right, bottom, right - r, bottom);
    path.line_to(x + r, bottom);
    path.quad_to(x, bottom, x, bottom - r);
    path.line_to(x, y + r);
    path.quad_to(x, y, x + r, y);
    path.close();
    path.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether a pixel is close to a colour, allowing for antialiasing.
    fn near(pixel: paint::PremultipliedColor, colour: (u8, u8, u8)) -> bool {
        let demultiplied = pixel.demultiply();
        i32::from(demultiplied.red()).abs_diff(i32::from(colour.0)) < 24
            && i32::from(demultiplied.green()).abs_diff(i32::from(colour.1)) < 24
            && i32::from(demultiplied.blue()).abs_diff(i32::from(colour.2)) < 24
            && demultiplied.alpha() > 0xf0
    }

    fn at(pixmap: &Pixmap, x: u32, y: u32) -> paint::PremultipliedColor {
        pixmap.pixels()[(y * pixmap.width() + x) as usize]
    }

    #[test]
    fn it_draws_at_every_size_a_platform_asks_for() {
        // The sizes that actually get asked for: a task bar, a dock, a window
        // list, and the largest any of them use. A drawing that only works at
        // one of them is a drawing that will be seen wrong somewhere.
        for size in [16u32, 32, 48, 64, 128, 256, 512] {
            let pixmap = render(size).expect("renders");
            assert_eq!((pixmap.width(), pixmap.height()), (size, size));
            let inked = pixmap
                .pixels()
                .iter()
                .filter(|pixel| pixel.alpha() > 0)
                .count();
            assert!(
                inked > (size * size / 8) as usize,
                "a {size}px icon is nearly empty: {inked} of {} pixels",
                size * size
            );
        }
    }

    #[test]
    fn the_corners_are_transparent_and_the_screen_is_not() {
        // The drawing has to be a computer sitting on nothing rather than a
        // teal square: an icon with an opaque background is one that shows its
        // own edges against every dock that rounds them.
        let pixmap = render(512).expect("renders");
        for (x, y) in [(2, 2), (509, 2), (2, 509), (509, 509)] {
            assert_eq!(
                at(&pixmap, x, y).alpha(),
                0,
                "the corner at {x},{y} is not transparent"
            );
        }

        // The middle of the screen, which is the one place the light teal goes.
        assert!(
            near(at(&pixmap, 256, 208), SCREEN),
            "the screen is not lit: {:?}",
            at(&pixmap, 256, 208).demultiply()
        );
        // The case, which is a colour rather than a hole — a transparent one
        // would put a dark task bar behind the computer's own shell.
        assert!(
            near(at(&pixmap, 256, 300), CASE),
            "the case is not opaque: {:?}",
            at(&pixmap, 256, 300).demultiply()
        );
    }

    #[test]
    fn the_window_icon_is_straight_rather_than_premultiplied() {
        // Every icon API in sight wants straight alpha, and a premultiplied
        // buffer handed to one draws a drawing that fades to black at the edges
        // instead of to nothing.
        let (rgba, width, height) = rgba(64).expect("renders");
        assert_eq!(rgba.len(), (width * height * 4) as usize);

        // A pixel on the screen's fill, which is opaque: premultiplying an
        // opaque pixel changes nothing, so this is the control.
        let pixel = |x: u32, y: u32| {
            let at = ((y * width + x) * 4) as usize;
            (rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3])
        };
        let (r, g, b, a) = pixel(32, 26);
        assert_eq!(a, 0xff);
        assert!(
            i32::from(r).abs_diff(i32::from(SCREEN.0)) < 24
                && i32::from(g).abs_diff(i32::from(SCREEN.1)) < 24
                && i32::from(b).abs_diff(i32::from(SCREEN.2)) < 24,
            "the screen came out {r},{g},{b}"
        );

        // And nothing anywhere is brighter than it should be for its alpha,
        // which is what a premultiplied buffer read as straight would be.
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] > 0 || pixel[..3] == [0, 0, 0]),
            "a fully transparent pixel carries colour"
        );
    }

    #[test]
    fn a_zero_sized_icon_is_a_pixel_rather_than_a_panic() {
        // Asked for by nobody on purpose and by a window manager on a bad day.
        assert_eq!(render(0).expect("renders").width(), 1);
    }
}
