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

/// Height of the URL bar, in pixels.
pub const HEIGHT: u32 = 34;

/// Height of the tab strip, shown only when there is more than one tab.
///
/// A strip above a single tab is a row of chrome that says nothing — the URL
/// bar already names the page — so it is not drawn until it has something to
/// distinguish.
pub const TAB_HEIGHT: u32 = 28;

/// Widest a tab may be. Beyond this they stop growing and the strip has room
/// for more of them.
const TAB_MAX_WIDTH: f32 = 200.0;
/// Narrowest a tab may be before its label is pointless.
const TAB_MIN_WIDTH: f32 = 70.0;

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
/// Behind selected text in the URL bar.
const SELECTION: Color = Color::rgb(0xb4, 0xd0, 0xf0);
/// The focused field's surround, so it is obvious where the typing goes.
const FOCUS: Color = Color::rgb(0x3a, 0x6e, 0xa5);
/// A tab that is not the one being shown.
const INACTIVE_TAB: Color = Color::rgb(0xdf, 0xdf, 0xdb);

/// Total height of the chrome, given how many tabs there are.
pub fn total_height(tab_count: usize) -> u32 {
    HEIGHT + if tab_count > 1 { TAB_HEIGHT } else { 0 }
}

/// Where each tab sits in the strip.
pub fn tab_rects(tab_count: usize, width: f32) -> Vec<Rect> {
    if tab_count <= 1 {
        return Vec::new();
    }
    let each = (width / tab_count as f32).clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH);
    (0..tab_count)
        .map(|index| Rect {
            x: index as f32 * each,
            y: 0.0,
            width: each,
            height: TAB_HEIGHT as f32,
        })
        .collect()
}

/// The tab at a point in the strip's own coordinates, and whether the point is
/// on its close button.
pub fn tab_at(tab_count: usize, width: f32, x: f32, y: f32) -> Option<(usize, bool)> {
    tab_rects(tab_count, width)
        .into_iter()
        .enumerate()
        .find_map(|(index, rect)| {
            let inside =
                x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
            // The close button is the right-hand end of the tab.
            inside.then_some((index, x > rect.x + rect.width - CLOSE_WIDTH))
        })
}

/// Width of a tab's close button.
const CLOSE_WIDTH: f32 = 22.0;

/// Draws the tab strip. Empty when there is only one tab.
pub fn render_tabs(labels: &[&str], active: usize, width: u32, fonts: &mut FontStore) -> Pixmap {
    let mut list = DisplayList {
        canvas: RULE,
        ..DisplayList::default()
    };
    let style = ui_style(12.0);

    for (index, rect) in tab_rects(labels.len(), width as f32)
        .into_iter()
        .enumerate()
    {
        // The active tab is the colour of the bar below it, so the two read as
        // one surface and the tab looks attached to what it shows.
        let background = if index == active { BAR } else { INACTIVE_TAB };
        list.items.push(DisplayItem::Rect {
            rect: Rect {
                x: rect.x,
                y: rect.y,
                width: rect.width - 1.0,
                height: rect.height,
            },
            color: background,
        });

        let label = labels.get(index).copied().unwrap_or_default();
        draw_text(
            &mut list,
            fonts,
            label,
            &style,
            rect.x + PADDING,
            TAB_HEIGHT as f32 / 2.0 - 8.0,
            if index == active { INK } else { DIM },
            rect.width - PADDING * 2.0 - CLOSE_WIDTH,
        );
        draw_text(
            &mut list,
            fonts,
            "\u{00d7}",
            &ui_style(14.0),
            rect.x + rect.width - CLOSE_WIDTH + 5.0,
            TAB_HEIGHT as f32 / 2.0 - 9.0,
            DIM,
            CLOSE_WIDTH,
        );
    }

    rasterise(
        &list,
        fonts,
        &paint::ImageStore::new(),
        width.max(1),
        TAB_HEIGHT,
    )
    .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"))
}

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
    /// The URL bar's editing state, when it has focus. `None` means the bar is
    /// showing where you are rather than where you are going.
    pub editing: Option<&'a crate::field::Field>,
    /// The find field, with which match is current and how many there are.
    pub finding: Option<(&'a crate::field::Field, usize, usize)>,
    /// Whether this page is in the saved list.
    pub saved: bool,
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
    /// Save this page, or forget it if it is already saved.
    Bookmark,
}

/// Width of the layout toggle, which carries a word rather than an arrow.
const TOGGLE: f32 = 96.0;
/// Width of the bookmark control, which also carries a word.
///
/// A word rather than a star because the bundled fonts have no star in them
/// (ADR-0008 bundles four families and nothing else), and a control that draws
/// as a hollow box is worse than one that says what it does.
const BOOKMARK: f32 = 56.0;

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
    // Not while editing: the bar gives its right-hand side over to the field,
    // so these are not drawn — and a control that is not drawn must not still
    // be catching clicks.
    if state.editing.is_some() || state.finding.is_some() {
        return out;
    }
    // Outermost, because it is there on every page; the toggle appears beside
    // it only when there is a decision to overrule, and a control that moved
    // depending on the page would be one you had to look for every time.
    out.push((Control::Bookmark, button(-BOOKMARK, BOOKMARK)));
    if state.can_toggle_layout {
        out.push((Control::ToggleLayout, button(-(BOOKMARK + TOGGLE), TOGGLE)));
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

/// The word on the bookmark control.
///
/// Past tense for the saved state, imperative for the other: the control has to
/// say both what pressing it does and what is true now, and a single word can
/// only do that if the two readings differ.
pub fn bookmark_label(state: &State<'_>) -> &'static str {
    if state.saved { "saved" } else { "save" }
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

    // Where the right-hand controls start, which is where the status has to
    // stop.
    let mut right_edge = width_f - PADDING;
    // Find takes the whole bar: its label sits where the back button would be,
    // and drawing both puts one on top of the other.
    let nav_controls = if state.finding.is_some() {
        Vec::new()
    } else {
        placed_controls(state, width_f)
    };
    for (control, rect) in nav_controls {
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
            Control::ToggleLayout | Control::Bookmark => {
                right_edge = right_edge.min(rect.x);
                // Outlined rather than filled: these are escape hatches, not
                // the thing the reader came here to press.
                outline(&mut list, &rect);
                let (label, ink) = if control == Control::ToggleLayout {
                    (toggle_label(state), INK)
                } else {
                    // Dimmed until the page is saved, so the two states differ
                    // at a glance and not only by reading the word.
                    (bookmark_label(state), if state.saved { INK } else { DIM })
                };
                let label_style = ui_style(12.0);
                let text_width = measure(fonts, label, &label_style);
                draw_text(
                    &mut list,
                    fonts,
                    label,
                    &label_style,
                    rect.x + (rect.width - text_width) / 2.0,
                    baseline() + 1.0,
                    ink,
                    rect.width,
                );
            }
        }
    }

    let url_x = PADDING * 2.0 + BUTTON * 2.0;

    // Find takes the bar over while it is open, the same way editing does, and
    // for the same reason: what you are doing is more important than where you
    // are.
    if let Some((field, current, total)) = state.finding {
        let style = ui_style(14.0);
        let count = if total == 0 {
            if field.text().is_empty() {
                String::new()
            } else {
                "no matches".to_owned()
            }
        } else {
            format!("{} of {total}", current + 1)
        };
        let count_width = measure(fonts, &count, &ui_style(13.0));
        let label_style = ui_style(13.0);
        let label_width = measure(fonts, "Find:", &label_style) + PADDING;
        let field_x = PADDING + label_width;
        let box_ = Rect {
            x: field_x,
            y: 4.0,
            width: (width_f - field_x - count_width - PADDING * 2.0).max(0.0),
            height: HEIGHT as f32 - 9.0,
        };
        draw_text(
            &mut list,
            fonts,
            "Find:",
            &label_style,
            PADDING,
            baseline(),
            DIM,
            label_width,
        );
        draw_field(&mut list, fonts, field, &style, &box_);
        draw_text(
            &mut list,
            fonts,
            &count,
            &ui_style(13.0),
            width_f - count_width - PADDING,
            baseline(),
            if total == 0 && !field.text().is_empty() {
                NOTICE
            } else {
                DIM
            },
            count_width,
        );
        return rasterise(
            &list,
            fonts,
            &paint::ImageStore::new(),
            width.max(1),
            HEIGHT,
        )
        .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"));
    }

    // While editing, the bar is a field and nothing else: the status describes
    // the page you are on, and you are in the middle of leaving it.
    if let Some(field) = state.editing {
        let style = ui_style(14.0);
        let box_ = Rect {
            x: url_x - PADDING / 2.0,
            y: 4.0,
            width: (width_f - url_x - PADDING / 2.0).max(0.0),
            height: HEIGHT as f32 - 9.0,
        };
        draw_field(&mut list, fonts, field, &style, &box_);
        return rasterise(
            &list,
            fonts,
            &paint::ImageStore::new(),
            width.max(1),
            HEIGHT,
        )
        .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"));
    }

    let status = status(state);
    // The status takes what it needs from the right; the URL gets the rest,
    // because a truncated URL is survivable and a truncated warning is not.
    let status_width = status
        .as_ref()
        .map(|text| measure(fonts, text, &ui_style(13.0)).min(width_f * 0.5))
        .unwrap_or(0.0);
    let status_x = right_edge - PADDING - status_width;
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

/// Draws the URL bar in its editing state.
fn draw_field(
    list: &mut DisplayList,
    fonts: &mut FontStore,
    field: &crate::field::Field,
    style: &ComputedStyle,
    box_: &Rect,
) {
    outline_in(list, box_, FOCUS);

    let text_x = box_.x + PADDING / 2.0;
    let max_width = (box_.width - PADDING).max(0.0);

    // Measuring a prefix is how a byte index becomes an x: the field knows
    // where the cursor is in the text, and only the shaper knows where that is
    // on screen.
    let offset_of = |fonts: &mut FontStore, at: usize| {
        measure(fonts, &field.text()[..at], style).min(max_width)
    };

    if let Some((start, end)) = field.selection() {
        let (from, to) = (offset_of(fonts, start), offset_of(fonts, end));
        list.items.push(DisplayItem::Rect {
            rect: Rect {
                x: text_x + from,
                y: box_.y + 3.0,
                width: (to - from).max(0.0),
                height: box_.height - 6.0,
            },
            color: SELECTION,
        });
    }

    draw_text(
        list,
        fonts,
        field.text(),
        style,
        text_x,
        baseline(),
        INK,
        max_width,
    );

    // The caret goes over the text, so it is visible inside a selection.
    let caret = offset_of(fonts, field.cursor());
    list.items.push(DisplayItem::Rect {
        rect: Rect {
            x: text_x + caret,
            y: box_.y + 3.0,
            width: 1.0,
            height: box_.height - 6.0,
        },
        color: INK,
    });
}

/// Draws a one-pixel outline around a control.
fn outline(list: &mut DisplayList, rect: &Rect) {
    let inset = 5.0;
    outline_in(
        list,
        &Rect {
            x: rect.x,
            y: rect.y + inset,
            width: rect.width,
            height: rect.height - inset * 2.0,
        },
        RULE,
    );
}

/// Draws a one-pixel outline in a given colour.
fn outline_in(list: &mut DisplayList, rect: &Rect, color: Color) {
    let (x, y) = (rect.x, rect.y);
    let (w, h) = (rect.width, rect.height);
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
        list.items.push(DisplayItem::Rect { rect: edge, color });
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
            editing: None,
            finding: None,
            saved: false,
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
        assert_eq!(placed.len(), 3, "back, forward, save — no toggle");

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
        assert_eq!(
            control_at(
                &state("https://example.com/", &authored),
                600.0,
                460.0,
                17.0
            ),
            None,
            "nothing there on an ordinary page"
        );

        let mode = RenderMode::Document {
            unsupported_share: 0.9,
        };
        let mut fallback = state("https://example.com/", &mode);
        fallback.can_toggle_layout = true;
        assert_eq!(
            control_at(&fallback, 600.0, 460.0, 17.0),
            Some(Control::ToggleLayout),
            "the toggle sits beside the save control"
        );
    }

    #[test]
    fn the_toggle_stops_catching_clicks_while_the_url_bar_is_focused() {
        // It is not drawn then, and an invisible control that still works is
        // how a stray click becomes an inexplicable change.
        let mode = RenderMode::Document {
            unsupported_share: 0.9,
        };
        let mut state = state("https://example.com/", &mode);
        state.can_toggle_layout = true;
        assert_eq!(
            control_at(&state, 600.0, 460.0, 17.0),
            Some(Control::ToggleLayout)
        );
        assert_eq!(
            control_at(&state, 600.0, 560.0, 17.0),
            Some(Control::Bookmark)
        );

        let field = crate::field::Field::with_all_selected("https://example.com/");
        state.editing = Some(&field);
        assert_eq!(control_at(&state, 600.0, 460.0, 17.0), None);
        assert_eq!(control_at(&state, 600.0, 560.0, 17.0), None);
        // Back and forward are still drawn, so they still work.
        assert_eq!(control_at(&state, 600.0, 12.0, 17.0), Some(Control::Back));
    }

    #[test]
    fn saving_is_offered_on_every_page_and_says_which_state_it_is_in() {
        let mode = RenderMode::Authored;
        let mut state = state("https://example.com/", &mode);
        assert_eq!(
            control_at(&state, 600.0, 560.0, 17.0),
            Some(Control::Bookmark),
            "there is always a page to save"
        );
        assert_eq!(bookmark_label(&state), "save");
        state.saved = true;
        assert_eq!(
            bookmark_label(&state),
            "saved",
            "it has to say what is true now, not only what pressing it does"
        );
    }

    #[test]
    fn the_saved_state_actually_reaches_the_pixels() {
        let mut fonts = FontStore::new();
        let mode = RenderMode::Authored;
        let mut state = state("https://example.com/", &mode);
        let unsaved = render(&state, 600, &mut fonts);
        state.saved = true;
        let saved = render(&state, 600, &mut fonts);
        assert_ne!(
            unsaved.data(),
            saved.data(),
            "saving a page changed nothing on screen"
        );
    }

    #[test]
    fn the_status_stops_short_of_the_controls() {
        // A status that ran under the buttons would be unreadable exactly when
        // it matters, which is when there is something to warn about.
        let mode = RenderMode::Document {
            unsupported_share: 0.9,
        };
        let mut state = state("http://example.com/", &mode);
        state.can_toggle_layout = true;
        let placed = placed_controls(&state, 600.0);
        let leftmost = placed
            .iter()
            .filter(|(control, _)| *control != Control::Back && *control != Control::Forward)
            .map(|(_, rect)| rect.x)
            .fold(f32::MAX, f32::min);
        assert!(leftmost < 600.0 - BOOKMARK, "{leftmost}");

        let text = status(&state).expect("a status");
        let width = measure(&mut FontStore::new(), &text, &ui_style(13.0)).min(300.0);
        assert!(leftmost - PADDING - width > 0.0, "no room left for the URL");
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
    fn the_editing_bar_shows_the_field_and_not_the_status() {
        // Mid-edit the bar is a field and nothing else: the status describes
        // the page you are on, and you are in the middle of leaving it.
        let mut fonts = FontStore::new();
        let mode = RenderMode::Authored;
        let field = crate::field::Field::with_all_selected("http://example.com/");

        let mut resting = state("http://example.com/", &mode);
        let resting_bar = render(&resting, 600, &mut fonts);

        resting.editing = Some(&field);
        let editing_bar = render(&resting, 600, &mut fonts);

        assert_ne!(
            resting_bar.data(),
            editing_bar.data(),
            "focusing the URL bar changed nothing on screen"
        );
    }

    #[test]
    fn the_caret_moves_when_the_cursor_does() {
        // The field knows where the cursor is in the text; only the shaper
        // knows where that is on screen, and this is the join between them.
        let mut fonts = FontStore::new();
        let mode = RenderMode::Authored;

        let mut at_start = crate::field::Field::with_all_selected("example.com");
        at_start.home(false);
        let mut at_end = crate::field::Field::with_all_selected("example.com");
        at_end.end(false);

        let mut state = state("https://example.com/", &mode);
        state.editing = Some(&at_start);
        let start_bar = render(&state, 600, &mut fonts);
        state.editing = Some(&at_end);
        let end_bar = render(&state, 600, &mut fonts);

        assert_ne!(start_bar.data(), end_bar.data());
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

#[cfg(test)]
mod tab_strip_tests {
    use super::*;

    #[test]
    fn a_single_tab_gets_no_strip() {
        // A strip above one tab is a row of chrome that says nothing: the URL
        // bar already names the page.
        assert!(tab_rects(1, 800.0).is_empty());
        assert_eq!(total_height(1), HEIGHT);
        assert_eq!(tab_at(1, 800.0, 10.0, 5.0), None);
    }

    #[test]
    fn the_strip_appears_with_a_second_tab() {
        assert_eq!(tab_rects(2, 800.0).len(), 2);
        assert_eq!(total_height(2), HEIGHT + TAB_HEIGHT);
    }

    #[test]
    fn tabs_stop_growing_rather_than_filling_the_window() {
        // Two tabs across a wide window should not be half a screen each.
        let wide = tab_rects(2, 2000.0);
        assert!(wide[0].width <= TAB_MAX_WIDTH);
        assert_eq!(wide[1].x, wide[0].width, "and they still sit side by side");
    }

    #[test]
    fn tabs_stop_shrinking_rather_than_vanishing() {
        let many = tab_rects(40, 800.0);
        assert!(many[0].width >= TAB_MIN_WIDTH);
    }

    #[test]
    fn a_click_finds_the_tab_under_it() {
        let rects = tab_rects(3, 800.0);
        for (index, rect) in rects.iter().enumerate() {
            assert_eq!(
                tab_at(3, 800.0, rect.x + 4.0, 5.0),
                Some((index, false)),
                "tab {index}"
            );
        }
    }

    #[test]
    fn the_right_hand_end_of_a_tab_closes_it() {
        let rects = tab_rects(3, 800.0);
        let rect = rects[1];
        assert_eq!(
            tab_at(3, 800.0, rect.x + rect.width - 4.0, 5.0),
            Some((1, true))
        );
        assert_eq!(
            tab_at(3, 800.0, rect.x + 4.0, 5.0),
            Some((1, false)),
            "and the rest of it selects"
        );
    }

    #[test]
    fn a_click_past_the_last_tab_hits_nothing() {
        let rects = tab_rects(2, 800.0);
        let past = rects[1].x + rects[1].width + 10.0;
        assert_eq!(tab_at(2, 800.0, past, 5.0), None);
    }

    #[test]
    fn the_active_tab_is_drawn_differently() {
        let mut fonts = FontStore::new();
        let labels = ["First page", "Second page"];
        let first = render_tabs(&labels, 0, 600, &mut fonts);
        let second = render_tabs(&labels, 1, 600, &mut fonts);
        assert_ne!(first.data(), second.data());
        assert_eq!((first.width(), first.height()), (600, TAB_HEIGHT));
    }
}
