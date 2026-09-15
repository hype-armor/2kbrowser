//! The address of the link under the pointer, shown in the corner (#139).
//!
//! Every browser has had this since before tabs did, and it is not decoration.
//! A link's text says what the author wants it to say; only the address says
//! where it goes. Without somewhere to read that, a reader has exactly one way
//! to find out where a link leads, which is to follow it — and a browser whose
//! answer to "where does this go?" is "go there and see" has handed the
//! question back.
//!
//! It matters more here than in a browser with JavaScript, not less. This one
//! has a policy that refuses off-site subresources (ADR-0006) and a chrome that
//! says when a page is not encrypted, both of which are about which host you
//! are dealing with — and the link you are about to click is the one place that
//! question goes unanswered.
//!
//! Drawn into the same buffer as everything else and with the same helpers as
//! the bar, for the reason [`crate::menu`] gives: a native tooltip means a
//! second window and a different implementation per platform, for a strip of
//! text.

use layout::Rect;
use paint::{DisplayItem, DisplayList, Pixmap, rasterise};
use text::FontStore;

use crate::chrome::{PADDING, Theme, draw_text, elided, measure, ui_style};

/// Height of the strip.
const HEIGHT: f32 = 21.0;
/// Text size in it. A step under the bar's, because this is an aside about
/// something the pointer is on rather than a statement about the page.
const TEXT: f32 = 12.0;
/// Most of the window it may take.
///
/// A share rather than a fixed width: the point of the cap is that the page
/// stays readable underneath, and what counts as "most of it" depends on how
/// wide the window is. Long URLs are elided rather than wrapped — this is a
/// glance, not a document.
const MAX_SHARE: f32 = 0.75;

/// Where the strip sits in a window of `size`, and how wide it is for `url`.
///
/// Bottom left, which is where every browser has put it and therefore where a
/// reader's eye already goes. It is *over* the page rather than beside it: a
/// row of chrome reserved for something that is empty almost all the time
/// would cost every page a line of height for a strip nobody is looking at.
pub fn rect(fonts: &mut FontStore, url: &str, size: (u32, u32)) -> Rect {
    let width = (measure(fonts, url, &ui_style(TEXT)) + PADDING * 2.0)
        .min(size.0 as f32 * MAX_SHARE)
        .max(0.0);
    Rect {
        x: 0.0,
        y: (size.1 as f32 - HEIGHT).max(0.0),
        width,
        height: HEIGHT.min(size.1 as f32),
    }
}

/// Draws it.
///
/// A surface with a hairline over its top and right edges, and none along the
/// two that sit on the window's own corner: an edge drawn where the window
/// already ends is a pixel of border nobody can see the far side of.
pub fn render(fonts: &mut FontStore, url: &str, theme: Theme, rect: Rect) -> Pixmap {
    let mut list = DisplayList {
        canvas: theme.control_edge,
        ..DisplayList::default()
    };
    list.items.push(DisplayItem::Rect {
        rect: Rect {
            x: 0.0,
            y: 1.0,
            width: rect.width - 1.0,
            height: rect.height - 1.0,
        },
        color: theme.bar,
    });
    let style = ui_style(TEXT);
    let room = rect.width - PADDING * 2.0;
    let text = elided(fonts, url, &style, room);
    draw_text(
        &mut list,
        fonts,
        &text,
        &style,
        PADDING,
        rect.height / 2.0 - TEXT * 0.72,
        theme.ink,
        room,
    );
    rasterise(
        &list,
        fonts,
        &paint::ImageStore::new(),
        rect.width.max(1.0) as u32,
        rect.height.max(1.0) as u32,
    )
    .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strip_sits_on_the_bottom_left_corner() {
        let mut fonts = FontStore::new();
        let at = rect(&mut fonts, "https://example.com/a.html", (900, 600));
        assert_eq!(at.x, 0.0, "it belongs against the left edge");
        assert_eq!(
            at.y + at.height,
            600.0,
            "and against the bottom one, not floating above it"
        );
        assert!(at.width > 0.0 && at.width < 900.0 * MAX_SHARE + 1.0);
    }

    #[test]
    fn a_long_address_stops_before_it_covers_the_page() {
        // The failure this cap exists for: a URL with a hundred characters of
        // query string is not more worth reading than the page it would be
        // lying across, and a reader cannot move a strip that follows their
        // pointer by definition.
        let mut fonts = FontStore::new();
        let long = format!("https://example.com/{}", "segment/".repeat(60));
        let at = rect(&mut fonts, &long, (900, 600));
        assert!(
            at.width <= 900.0 * MAX_SHARE,
            "{} of a 900px window",
            at.width
        );

        // And what is drawn in it is marked as cut rather than simply stopping
        // — the same rule the address bar follows, for the same reason: text
        // that ends early does not look cut, it looks like a shorter URL.
        let text = elided(&mut fonts, &long, &ui_style(TEXT), at.width - PADDING * 2.0);
        assert!(text.ends_with('\u{2026}'), "{text}");
    }

    #[test]
    fn a_short_address_takes_only_the_room_it_needs() {
        let mut fonts = FontStore::new();
        let at = rect(&mut fonts, "https://a.io/", (900, 600));
        assert!(
            at.width < 900.0 * MAX_SHARE,
            "a short URL should not be padded out to the cap: {}",
            at.width
        );
    }

    #[test]
    fn it_draws_something_in_either_theme() {
        let mut fonts = FontStore::new();
        let url = "https://example.com/a.html";
        let at = rect(&mut fonts, url, (900, 600));
        let light = render(&mut fonts, url, Theme::LIGHT, at);
        let dark = render(&mut fonts, url, Theme::DARK, at);
        assert_eq!(light.width(), at.width as u32);
        assert_eq!(light.height(), at.height as u32);
        assert_ne!(
            light.data(),
            dark.data(),
            "the strip ignores the theme, so it is a light box on a dark page"
        );
    }

    #[test]
    fn every_glyph_a_url_is_made_of_exists_in_the_bundled_fonts() {
        // The same check the bar's own labels get, and for the same reason
        // (ADR-0008): a codepoint no bundled family carries draws as a hollow
        // box, and this strip draws text nobody here chose — an address comes
        // from a stranger's page.
        //
        // Not every codepoint a URL could hold, which is all of them. What is
        // checked is that the ASCII a URL is spelled in is covered at this
        // size, since the strip is drawn at a size no other chrome uses.
        let mut fonts = FontStore::new();
        let ascii: String = (0x21u8..0x7f).map(char::from).collect();
        let layout = fonts.layout(&ascii, &ui_style(TEXT), 10_000.0);
        for line in &layout.lines {
            for glyph in &line.glyphs {
                assert_ne!(
                    glyph.glyph_id, 0,
                    "a printable ASCII character draws as a .notdef box at {TEXT}px"
                );
            }
        }
    }
}
