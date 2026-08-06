//! The browser chrome: the bar above the page.
//!
//! Modern, not period (ADR-0011). It exists because two things in this project
//! are *required* to be said out loud and have had nowhere to say them:
//! ADR-0009 forbids re-rendering a page as a document silently, and §4 of the
//! plan requires plain HTTP to be marked as unauthenticated rather than
//! presented as secure. Until now both were squeezed into the window title.
//!
//! Drawn by building a display list and handing it to the same rasteriser the
//! page goes through, so the chrome is not a second rendering path that can
//! drift from the first — and so it can be tested headlessly, which is most of
//! why the window's own code has so little test coverage.

use layout::{Rect, RenderMode};
use net::Scheme;
use paint::{DisplayItem, DisplayList, Pixmap, rasterise};
use text::FontStore;

use css::style::{ComputedStyle, FontStack, GenericFamily};
use css::value::Color;

/// Height of the bar, in pixels.
pub const HEIGHT: u32 = 34;

/// Width of the back and forward buttons.
const BUTTON: f32 = 30.0;
/// Gap between the buttons and the URL.
const PADDING: f32 = 8.0;

const BAR: Color = Color::rgb(0xf2, 0xf2, 0xf0);
const RULE: Color = Color::rgb(0xcf, 0xcf, 0xcb);
const INK: Color = Color::rgb(0x22, 0x22, 0x22);
/// Used for a control that cannot be used, and for the parts of a URL that are
/// not its host.
const DIM: Color = Color::rgb(0x8a, 0x8a, 0x88);
/// Warnings. Not red: nothing here is an error, and a browser that shouts at
/// people about plain HTTP teaches them to ignore it.
const NOTICE: Color = Color::rgb(0x8a, 0x5a, 0x10);

/// What the bar has to show.
pub struct State<'a> {
    /// The current URL.
    pub url: &'a str,
    /// How the page was rendered (ADR-0009).
    pub mode: &'a RenderMode,
    /// What went wrong with the last navigation, if anything.
    pub error: Option<&'a str>,
    /// Whether back is available.
    pub can_go_back: bool,
    /// Whether forward is available.
    pub can_go_forward: bool,
    /// Whether the reader is currently overruling the fallback.
    pub forcing_authored: bool,
    /// Whether there is a fallback decision to overrule at all.
    pub can_toggle_layout: bool,
}

/// A control in the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Go back.
    Back,
    /// Go forward.
    Forward,
    /// Show the author's layout instead of the document fallback, or return to
    /// the fallback from it.
    ToggleLayout,
}

/// Width of the layout toggle, which carries a word rather than an arrow.
const TOGGLE: f32 = 96.0;

/// Where each control sits, so the window can route a click without knowing
/// how the bar is drawn.
///
/// Fixed positions, deliberately: the toggle sits at the right edge and the
/// status text is fitted around it, rather than the other way round. Geometry
/// that depended on measured text could not be computed without a font store,
/// and a click would have to guess at what a redraw decided.
///
/// The toggle appears only when there is a decision to overrule. On an ordinary
/// page there is nothing for it to do, and a control that does nothing is worse
/// than no control.
pub fn controls(state: &State<'_>) -> Vec<(Control, Rect)> {
    let full = HEIGHT as f32;
    let button = |x: f32, width: f32| Rect {
        x,
        y: 0.0,
        width,
        height: full,
    };
    let mut out = vec![
        (Control::Back, button(PADDING, BUTTON)),
        (Control::Forward, button(PADDING + BUTTON, BUTTON)),
    ];
    if state.can_toggle_layout {
        out.push((Control::ToggleLayout, button(-TOGGLE, TOGGLE)));
    }
    out
}

/// The same, with the toggle placed against the right edge of a bar this wide.
fn placed_controls(state: &State<'_>, width: f32) -> Vec<(Control, Rect)> {
    controls(state)
        .into_iter()
        .map(|(control, mut rect)| {
            // A negative x means "from the right", which keeps `controls`
            // width-independent for callers that only want the set.
            if rect.x < 0.0 {
                rect.x += width - PADDING;
            }
            (control, rect)
        })
        .collect()
}

/// The control at a point in the bar's own coordinates.
pub fn control_at(state: &State<'_>, width: f32, x: f32, y: f32) -> Option<Control> {
    placed_controls(state, width)
        .into_iter()
        .find_map(|(control, rect)| {
            let inside =
                x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
            inside.then_some(control)
        })
}

/// The word on the layout toggle.
pub fn toggle_label(state: &State<'_>) -> &'static str {
    if state.forcing_authored {
        "as document"
    } else {
        "as authored"
    }
}

/// What to say about the URL's scheme, if anything.
///
/// `https` gets nothing at all. A browser that decorates the secure case
/// teaches people to look for a positive signal, and the absence of one is
/// easy to miss; marking only the exception is the way round that works.
pub fn scheme_notice(url: &str) -> Option<&'static str> {
    match net::parse_url(url).ok()?.0.scheme {
        Scheme::Http => Some("not encrypted"),
        Scheme::File => Some("local file"),
        Scheme::Https => None,
    }
}

/// The message the bar shows on the right, if any.
///
/// A navigation error outranks the rendering mode: the page on screen is not
/// the page that was asked for, which the reader needs to know first.
pub fn status(state: &State<'_>) -> Option<String> {
    if let Some(error) = state.error {
        return Some(error.to_owned());
    }
    match state.mode {
        RenderMode::Authored => scheme_notice(state.url).map(str::to_owned),
        // Short enough to fit beside a URL. The words that matter are at the
        // front, so what truncation there is costs the least.
        RenderMode::Document { unsupported_share } => Some(format!(
            "rendered as a document — {}% needs newer layout",
            (unsupported_share * 100.0).round() as u32
        )),
        RenderMode::RequiresScripting => {
            Some("rendered as a document — needs JavaScript".to_owned())
        }
    }
}

/// Draws the bar.
pub fn render(state: &State<'_>, width: u32, fonts: &mut FontStore) -> Pixmap {
    let mut list = DisplayList {
        canvas: BAR,
        ..DisplayList::default()
    };
    let width_f = width as f32;

    // A hairline under the bar, so the page below it reads as a separate
    // surface rather than as more chrome.
    list.items.push(DisplayItem::Rect {
        rect: Rect {
            x: 0.0,
            y: HEIGHT as f32 - 1.0,
            width: width_f,
            height: 1.0,
        },
        color: RULE,
    });

    let mut toggle_left = width_f - PADDING;
    for (control, rect) in placed_controls(state, width_f) {
        match control {
            Control::Back | Control::Forward => {
                let enabled = if control == Control::Back {
                    state.can_go_back
                } else {
                    state.can_go_forward
                };
                // Arrows rather than words: the one piece of browser
                // iconography nobody has to learn.
                let glyph = if control == Control::Back {
                    "\u{2190}"
                } else {
                    "\u{2192}"
                };
                draw_text(
                    &mut list,
                    fonts,
                    glyph,
                    &ui_style(15.0),
                    rect.x + BUTTON / 2.0 - 5.0,
                    baseline(),
                    if enabled { INK } else { DIM },
                    BUTTON,
                );
            }
            Control::ToggleLayout => {
                toggle_left = rect.x;
                // Outlined rather than filled: it is an escape hatch, not the
                // thing the reader came here to press.
                outline(&mut list, &rect);
                let label = toggle_label(state);
                let label_style = ui_style(12.0);
                let text_width = measure(fonts, label, &label_style);
                draw_text(
                    &mut list,
                    fonts,
                    label,
                    &label_style,
                    rect.x + (rect.width - text_width) / 2.0,
                    baseline() + 1.0,
                    INK,
                    rect.width,
                );
            }
        }
    }

    let url_x = PADDING * 2.0 + BUTTON * 2.0;
    let status = status(state);
    // The status takes what it needs from the right; the URL gets the rest,
    // because a truncated URL is survivable and a truncated warning is not.
    let status_width = status
        .as_ref()
        .map(|text| measure(fonts, text, &ui_style(13.0)).min(width_f * 0.5))
        .unwrap_or(0.0);
    let status_x = toggle_left - PADDING - status_width;
    let url_width = (status_x - PADDING - url_x).max(0.0);

    draw_text(
        &mut list,
        fonts,
        state.url,
        &ui_style(14.0),
        url_x,
        baseline(),
        INK,
        url_width,
    );

    if let Some(text) = &status {
        let color = if state.error.is_some() || !matches!(state.mode, RenderMode::Authored) {
            NOTICE
        } else {
            DIM
        };
        draw_text(
            &mut list,
            fonts,
            text,
            &ui_style(13.0),
            status_x,
            baseline(),
            color,
            status_width,
        );
    }

    rasterise(
        &list,
        fonts,
        &paint::ImageStore::new(),
        width.max(1),
        HEIGHT,
    )
    .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"))
}

/// Draws a one-pixel outline around a control.
fn outline(list: &mut DisplayList, rect: &Rect) {
    let inset = 5.0;
    let (x, y) = (rect.x, rect.y + inset);
    let (w, h) = (rect.width, rect.height - inset * 2.0);
    for edge in [
        Rect {
            x,
            y,
            width: w,
            height: 1.0,
        },
        Rect {
            x,
            y: y + h - 1.0,
            width: w,
            height: 1.0,
        },
        Rect {
            x,
            y,
            width: 1.0,
            height: h,
        },
        Rect {
            x: x + w - 1.0,
            y,
            width: 1.0,
            height: h,
        },
    ] {
        list.items.push(DisplayItem::Rect {
            rect: edge,
            color: RULE,
        });
    }
}

/// Vertical position of the bar's single line of text.
fn baseline() -> f32 {
    HEIGHT as f32 / 2.0 - 9.0
}

/// The chrome's own font: sans-serif, because this is not the page.
fn ui_style(size: f32) -> ComputedStyle {
    ComputedStyle {
        font_size: size,
        line_height: size * 1.2,
        font_family: FontStack {
            families: Vec::new(),
            generic: GenericFamily::SansSerif,
        },
        ..ComputedStyle::default()
    }
}

fn measure(fonts: &mut FontStore, text: &str, style: &ComputedStyle) -> f32 {
    fonts.layout(text, style, f32::MAX).width
}

/// Appends one line of text, clipped to `max_width` by dropping what overflows.
#[expect(
    clippy::too_many_arguments,
    reason = "a drawing call; every argument is a distinct visual property"
)]
fn draw_text(
    list: &mut DisplayList,
    fonts: &mut FontStore,
    text: &str,
    style: &ComputedStyle,
    x: f32,
    y: f32,
    color: Color,
    max_width: f32,
) {
    if text.is_empty() || max_width <= 0.0 {
        return;
    }
    // Laid out unwrapped and then cut: a URL that does not fit should run off
    // the end, not wrap into a second line the bar has no room for.
    let layout = fonts.layout(text, style, f32::MAX);
    let Some(line) = layout.lines.first() else {
        return;
    };
    for (index, glyph) in line.glyphs.iter().enumerate() {
        // A glyph that *starts* inside the limit can still finish outside it,
        // and the one that overhangs is the one that touches whatever sits
        // alongside. A glyph carries no advance, but the next one's origin is
        // exactly where this one ends — and for the last, the line's width is.
        let right = line
            .glyphs
            .get(index + 1)
            .map(|next| next.x)
            .unwrap_or(line.width);
        if right > max_width {
            break;
        }
        list.items.push(DisplayItem::Glyph {
            glyph: *glyph,
            origin_x: x,
            origin_y: y,
            color,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state<'a>(url: &'a str, mode: &'a RenderMode) -> State<'a> {
        State {
            url,
            mode,
            error: None,
            can_go_back: false,
            can_go_forward: false,
            forcing_authored: false,
            can_toggle_layout: false,
        }
    }

    #[test]
    fn plain_http_is_marked_and_https_is_not() {
        // §4: HTTP is allowed because much of the old web needs it, and must
        // never be presented as secure. Marking the secure case instead would
        // teach people to look for a signal whose absence is easy to miss.
        assert_eq!(
            scheme_notice("http://example.com/a.html"),
            Some("not encrypted")
        );
        assert_eq!(scheme_notice("https://example.com/a.html"), None);
        assert_eq!(scheme_notice("file:///a.html"), Some("local file"));
    }

    #[test]
    fn the_document_fallback_is_stated() {
        // ADR-0009 forbids switching rendering mode silently.
        let mode = RenderMode::Document {
            unsupported_share: 0.87,
        };
        let text = status(&state("https://example.com/", &mode)).expect("a status");
        assert!(text.contains("87%"), "got {text}");
        assert!(text.contains("document"), "got {text}");

        let scripting = status(&state(
            "https://example.com/",
            &RenderMode::RequiresScripting,
        ))
        .expect("a status");
        assert!(scripting.contains("JavaScript"), "got {scripting}");
    }

    #[test]
    fn an_ordinary_https_page_says_nothing() {
        // Restraint shows as absence (ADR-0011). There is nothing to report.
        assert_eq!(
            status(&state("https://example.com/", &RenderMode::Authored)),
            None
        );
    }

    #[test]
    fn an_error_outranks_everything_else() {
        let mode = RenderMode::Document {
            unsupported_share: 0.9,
        };
        let mut state = state("http://example.com/", &mode);
        state.error = Some("server returned 404");
        let text = status(&state).expect("a status");
        assert_eq!(text, "server returned 404");
    }

    #[test]
    fn the_buttons_are_where_clicks_look_for_them() {
        let mode = RenderMode::Authored;
        let state = state("https://example.com/", &mode);
        let placed = controls(&state);
        assert_eq!(placed.len(), 2, "no toggle on an ordinary page");

        assert_eq!(
            control_at(&state, 600.0, placed[0].1.x + 1.0, 5.0),
            Some(Control::Back)
        );
        assert_eq!(
            control_at(&state, 600.0, placed[1].1.x + 1.0, 5.0),
            Some(Control::Forward)
        );
        // Past the buttons is the URL, which is not a control.
        assert_eq!(control_at(&state, 600.0, 300.0, 5.0), None);
        assert_eq!(
            control_at(&state, 600.0, 0.0, 5.0),
            None,
            "left of the first button"
        );
    }

    #[test]
    fn the_layout_toggle_appears_only_when_there_is_a_decision_to_overrule() {
        // ADR-0009 requires the override. A control that does nothing on every
        // ordinary page is worse than no control, so it is not there.
        let authored = RenderMode::Authored;
        assert!(
            control_at(
                &state("https://example.com/", &authored),
                600.0,
                560.0,
                17.0
            )
            .is_none()
        );

        let mode = RenderMode::Document {
            unsupported_share: 0.9,
        };
        let mut fallback = state("https://example.com/", &mode);
        fallback.can_toggle_layout = true;
        assert_eq!(
            control_at(&fallback, 600.0, 560.0, 17.0),
            Some(Control::ToggleLayout),
            "the toggle sits at the right edge"
        );
    }

    #[test]
    fn the_toggle_says_where_it_leads() {
        let mode = RenderMode::Document {
            unsupported_share: 0.9,
        };
        let mut state = state("https://example.com/", &mode);
        state.can_toggle_layout = true;
        assert_eq!(toggle_label(&state), "as authored");
        state.forcing_authored = true;
        assert_eq!(
            toggle_label(&state),
            "as document",
            "once overruling, it has to offer the way back"
        );
    }

    #[test]
    fn the_bar_is_drawn_at_the_requested_width() {
        let mut fonts = FontStore::new();
        let pixmap = render(
            &state("https://example.com/page.html", &RenderMode::Authored),
            600,
            &mut fonts,
        );
        assert_eq!((pixmap.width(), pixmap.height()), (600, HEIGHT));
    }

    #[test]
    fn a_notice_actually_reaches_the_pixels() {
        // The status is composed correctly above; this is the check that it is
        // also *drawn*, which is a different failure.
        let mut fonts = FontStore::new();
        let plain = render(
            &state("https://example.com/", &RenderMode::Authored),
            600,
            &mut fonts,
        );
        let warned = render(
            &state("http://example.com/", &RenderMode::Authored),
            600,
            &mut fonts,
        );
        assert_ne!(
            plain.data(),
            warned.data(),
            "the http warning was composed but never drawn"
        );
    }
}
