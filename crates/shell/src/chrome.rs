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
pub const HEIGHT: u32 = 46;

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
const BUTTON: f32 = 40.0;
/// Gap between the buttons and the URL.
const PADDING: f32 = 8.0;

/// Every colour the chrome draws with, so that the two schemes are one
/// substitution rather than a branch at each use site.
///
/// Only the chrome is themed. The page below it is the author's, and repainting
/// their colours is a decision about someone else's document — the same class
/// of decision ADR-0009 requires the browser to say out loud before making. A
/// dark bar above a white page is honest about that; an inverted page would not
/// be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// The bar's own surface.
    pub bar: Color,
    /// Hairlines, outlines, and the gap between tabs.
    pub rule: Color,
    /// Text and enabled controls.
    pub ink: Color,
    /// A control that cannot be used, and the parts of a URL that are not its
    /// host.
    pub dim: Color,
    /// Warnings. Not red: nothing here is an error, and a browser that shouts
    /// at people about plain HTTP teaches them to ignore it.
    pub notice: Color,
    /// Behind selected text in the URL bar.
    pub selection: Color,
    /// The focused field's surround, so it is obvious where the typing goes.
    pub focus: Color,
    /// A tab that is not the one being shown.
    pub inactive_tab: Color,
}

impl Theme {
    /// The default scheme.
    pub const LIGHT: Self = Self {
        bar: Color::rgb(0xf2, 0xf2, 0xf0),
        rule: Color::rgb(0xcf, 0xcf, 0xcb),
        ink: Color::rgb(0x22, 0x22, 0x22),
        dim: Color::rgb(0x8a, 0x8a, 0x88),
        notice: Color::rgb(0x8a, 0x5a, 0x10),
        selection: Color::rgb(0xb4, 0xd0, 0xf0),
        focus: Color::rgb(0x3a, 0x6e, 0xa5),
        inactive_tab: Color::rgb(0xdf, 0xdf, 0xdb),
    };

    /// The dark scheme.
    ///
    /// Not an inversion of the light one: the warning colour has to stay
    /// legible against a dark bar and stay distinct from ordinary ink, and
    /// inverting `theme.notice` would have given a pale blue that reads as a link.
    pub const DARK: Self = Self {
        bar: Color::rgb(0x24, 0x24, 0x26),
        rule: Color::rgb(0x3d, 0x3d, 0x41),
        ink: Color::rgb(0xe9, 0xe9, 0xe7),
        dim: Color::rgb(0x92, 0x92, 0x96),
        notice: Color::rgb(0xe3, 0xb0, 0x4b),
        selection: Color::rgb(0x2d, 0x4f, 0x74),
        focus: Color::rgb(0x6f, 0xa8, 0xdc),
        inactive_tab: Color::rgb(0x1b, 0x1b, 0x1d),
    };
}

impl Default for Theme {
    fn default() -> Self {
        Self::LIGHT
    }
}

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
pub fn render_tabs(
    labels: &[&str],
    active: usize,
    width: u32,
    fonts: &mut FontStore,
    theme: Theme,
) -> Pixmap {
    let mut list = DisplayList {
        canvas: theme.rule,
        ..DisplayList::default()
    };
    let style = ui_style(12.0);

    for (index, rect) in tab_rects(labels.len(), width as f32)
        .into_iter()
        .enumerate()
    {
        // The active tab is the colour of the bar below it, so the two read as
        // one surface and the tab looks attached to what it shows.
        let background = if index == active {
            theme.bar
        } else {
            theme.inactive_tab
        };
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
            if index == active {
                theme.ink
            } else {
                theme.dim
            },
            rect.width - PADDING * 2.0 - CLOSE_WIDTH,
        );
        draw_text(
            &mut list,
            fonts,
            "\u{00d7}",
            &ui_style(14.0),
            rect.x + rect.width - CLOSE_WIDTH + 5.0,
            TAB_HEIGHT as f32 / 2.0 - 9.0,
            theme.dim,
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
    /// Whether the reader has asked for the fallback on a page that
    /// classification did not give one to.
    pub forcing_document: bool,
    /// Whether this page is in a layout decision rather than the plain answer —
    /// either one classification made, or one the reader asked for.
    pub can_toggle_layout: bool,
    /// The URL bar's editing state, when it has focus. `None` means the bar is
    /// showing where you are rather than where you are going.
    pub editing: Option<&'a crate::field::Field>,
    /// The find field, with which match is current and how many there are.
    pub finding: Option<(&'a crate::field::Field, usize, usize)>,
    /// Whether this page is in the saved list.
    pub saved: bool,
    /// Whether the certificate chain verified only against a local root.
    pub local_root: bool,
    /// Which colour scheme to draw in.
    pub theme: Theme,
}

/// A control in the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Go back.
    Back,
    /// Go forward.
    Forward,
    /// Fetch this page again.
    Reload,
    /// Switch between the author's layout and the document fallback, whichever
    /// way round this page currently is.
    ///
    /// On a page classification sent to the fallback this offers the author's
    /// layout, and offers the fallback back afterwards. On an ordinary page it
    /// offers the fallback, which is a different request rather than the same
    /// one inverted: a page that classified as `Authored` has no fallback to
    /// return to (ADR-0009).
    ToggleLayout,
    /// Save this page, or forget it if it is already saved.
    Bookmark,
}

/// Width of the reload control.
///
/// A word, for the same reason the bookmark control is one: U+21BB is not in
/// any of the four bundled families (ADR-0008), so it drew as a hollow box —
/// which is how it reached a screenshot before anyone noticed. Back and forward
/// keep their arrows because U+2190 and U+2192 are actually there.
const RELOAD: f32 = 58.0;
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
/// The toggle is on every page. It used to appear only where classification had
/// made a decision, because on an ordinary page it had nothing to do; now it
/// does — a page that renders perfectly well can still be handed the document
/// fallback, which is what a reader wanting a plain view of a busy page is
/// asking for. So it no longer comes and goes with the page, and the reader who
/// wants it does not have to learn which pages have it.
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
        (Control::Reload, button(PADDING + BUTTON * 2.0, RELOAD)),
    ];
    // Not while editing: the bar gives its right-hand side over to the field,
    // so these are not drawn — and a control that is not drawn must not still
    // be catching clicks.
    if state.editing.is_some() || state.finding.is_some() {
        return out;
    }
    // Both on every page, and always in this order, so neither is somewhere
    // different depending on what the page turned out to be.
    out.push((Control::Bookmark, button(-BOOKMARK, BOOKMARK)));
    out.push((Control::ToggleLayout, button(-(BOOKMARK + TOGGLE), TOGGLE)));
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

/// The word on the layout toggle: where pressing it leads, not where you are.
///
/// Three wordings for what looks like two states, because "show me the
/// fallback" is two different offers depending on the page. On a page
/// classification sent to the fallback, the way back is a return to a decision
/// the browser already made and stated. On an ordinary page there is no such
/// decision, and the button is asking whether to impose one — so it says what
/// pressing it *does* rather than naming a state the page was never in.
pub fn toggle_label(state: &State<'_>) -> &'static str {
    if state.forcing_authored {
        // Overruling a fallback: the way back is the fallback.
        "as document"
    } else if state.can_toggle_layout {
        // Showing a document, whether classification chose it or the reader
        // asked for it. Either way out is the author's layout.
        "as authored"
    } else {
        // An ordinary page, in no decision at all. Imperative, like `save`,
        // because there is nothing here to go back to.
        "simplify"
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
    // Above the rendering mode. Who can read this connection outranks how the
    // page was laid out, and unlike the scheme notice it applies whichever mode
    // the page ended up in — an intercepted page rendered as a document is
    // still intercepted.
    if state.local_root {
        return Some("local certificate — readable in transit".to_owned());
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
    let theme = state.theme;
    let mut list = DisplayList {
        canvas: theme.bar,
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
        color: theme.rule,
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
            Control::Reload => {
                // A word, and never dimmed: reloading is how you recover from
                // a failed navigation, which is exactly when back and forward
                // are least use.
                let label_style = ui_style(12.0);
                let text_width = measure(fonts, "reload", &label_style);
                draw_text(
                    &mut list,
                    fonts,
                    "reload",
                    &label_style,
                    rect.x + (rect.width - text_width) / 2.0,
                    baseline() + 1.0,
                    theme.ink,
                    rect.width,
                );
            }
            Control::Back | Control::Forward => {
                let enabled = if control == Control::Back {
                    state.can_go_back
                } else {
                    state.can_go_forward
                };
                // Arrows rather than words: the one piece of browser
                // iconography nobody has to learn, and both are in the bundled
                // families.
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
                    if enabled { theme.ink } else { theme.dim },
                    BUTTON,
                );
            }
            Control::ToggleLayout | Control::Bookmark => {
                right_edge = right_edge.min(rect.x);
                // Outlined rather than filled: these are escape hatches, not
                // the thing the reader came here to press.
                outline(&mut list, &rect, theme);
                let (label, ink) = if control == Control::ToggleLayout {
                    (toggle_label(state), theme.ink)
                } else {
                    // Dimmed until the page is saved, so the two states differ
                    // at a glance and not only by reading the word.
                    (
                        bookmark_label(state),
                        if state.saved { theme.ink } else { theme.dim },
                    )
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

    let url_x = PADDING * 2.0 + BUTTON * 2.0 + RELOAD;

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
            theme.dim,
            label_width,
        );
        draw_field(theme, &mut list, fonts, field, &style, &box_);
        draw_text(
            &mut list,
            fonts,
            &count,
            &ui_style(13.0),
            width_f - count_width - PADDING,
            baseline(),
            if total == 0 && !field.text().is_empty() {
                theme.notice
            } else {
                theme.dim
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
        draw_field(theme, &mut list, fonts, field, &style, &box_);
        return rasterise(
            &list,
            fonts,
            &paint::ImageStore::new(),
            width.max(1),
            HEIGHT,
        )
        .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"));
    }

    // Everything between the reload button and the right-hand controls, which
    // the URL and the status divide between them.
    let shared = (right_edge - PADDING * 2.0 - url_x).max(0.0);
    let fit = fitted(fonts, state.url, status(state).as_deref(), shared);
    let status_x = right_edge - PADDING - fit.status_width;

    draw_text(
        &mut list,
        fonts,
        &fit.url,
        &ui_style(14.0),
        url_x,
        baseline(),
        theme.ink,
        fit.url_width,
    );

    if let Some(text) = &fit.status {
        // Grey for the quiet facts, amber for the ones that contradict what the
        // rest of the screen implies.
        //
        // Plain HTTP is grey on purpose: it is already visible in the URL, and
        // a browser that shouts about it teaches people to ignore the shouting.
        // An intercepted connection is the opposite case — the URL says
        // `https://`, which actively suggests privacy, so a grey note would be
        // competing with the padlock-shaped intuition rather than correcting
        // it. Same reasoning ADR-0013 used to refuse marked TLS downgrades,
        // applied to the one place a marking can still work: here the reader
        // learns something the address bar was implying the opposite of.
        let color = if state.error.is_some()
            || state.local_root
            || !matches!(state.mode, RenderMode::Authored)
        {
            theme.notice
        } else {
            theme.dim
        };
        draw_text(
            &mut list,
            fonts,
            text,
            &ui_style(13.0),
            status_x,
            baseline(),
            color,
            fit.status_width,
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
    theme: Theme,
    list: &mut DisplayList,
    fonts: &mut FontStore,
    field: &crate::field::Field,
    style: &ComputedStyle,
    box_: &Rect,
) {
    outline_in(list, box_, theme.focus);

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
            color: theme.selection,
        });
    }

    draw_text(
        list,
        fonts,
        field.text(),
        style,
        text_x,
        baseline(),
        theme.ink,
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
        color: theme.ink,
    });
}

/// Draws a one-pixel outline around a control.
fn outline(list: &mut DisplayList, rect: &Rect, theme: Theme) {
    let inset = 5.0;
    outline_in(
        list,
        &Rect {
            x: rect.x,
            y: rect.y + inset,
            width: rect.width,
            height: rect.height - inset * 2.0,
        },
        theme.rule,
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

/// The part of a URL that is not its path: scheme, host, and any port.
///
/// What the bar protects when there is not room for everything. A reader
/// deciding whether to trust a page is deciding whether to trust its host, and
/// the scheme is what the rest of the bar's marking is about; the path is the
/// part they can read the page itself to learn.
fn origin_prefix(url: &str) -> &str {
    let Some(mark) = url.find("://") else {
        return url;
    };
    let host_at = mark + 3;
    match url[host_at..].find('/') {
        // Including the slash, so it is visibly the end of a host rather than a
        // host that might have had more to it.
        Some(slash) => &url[..host_at + slash + 1],
        None => url,
    }
}

/// How the URL and the status divide the space between the buttons.
///
/// Returns their widths in that order, always summing to `shared`.
///
/// The status outranks the URL — a truncated warning is worse than a truncated
/// address, and its wording is front-loaded so that what goes is the least of
/// it — but no longer to the point of crowding the URL out. It used to be
/// capped at half the *bar*, which is a share of the wrong thing: half of a
/// 600px bar is 300px, and the buttons either side had already spent 314 of it,
/// so the URL was handed a negative width and drew nothing whatsoever. The one
/// field a reader is being asked to make a judgement about vanished on exactly
/// the pages that gave them something to judge.
///
/// So the cap is now what is really there to share, less what the URL is
/// guaranteed: its origin, plus the marker saying a path was cut off it. A host
/// cut short is the one truncation with nothing to recommend it — it is the
/// part being vouched for. That guarantee stops at half of what there is, so a
/// page with an enormous host cannot annihilate the warning the way the warning
/// used to annihilate it.
fn share(fonts: &mut FontStore, url: &str, status: Option<&str>, shared: f32) -> (f32, f32) {
    let url_style = ui_style(14.0);
    let origin = origin_prefix(url);
    let floor = if origin.len() == url.len() {
        // Nothing to elide, so nothing to leave room for.
        measure(fonts, url, &url_style)
    } else {
        measure(fonts, origin, &url_style) + measure(fonts, ELLIPSIS, &url_style)
    }
    .min(shared / 2.0)
    .max(0.0);
    let status_width = status
        .map(|text| measure(fonts, text, &ui_style(13.0)).min(shared - floor))
        .unwrap_or(0.0)
        .max(0.0);
    (shared - status_width, status_width)
}

/// What the bar puts between its buttons, once the space has been divided.
struct Fitted {
    /// The URL as it will be drawn, marked if it had to be cut.
    url: String,
    url_width: f32,
    /// The status as it will be drawn, marked if it had to be cut.
    status: Option<String>,
    status_width: f32,
}

/// Divides the space and cuts both fields to what they got.
///
/// One function rather than two steps at the call site, because the division
/// and the cutting have to agree: a width worked out to guarantee the host is
/// worth nothing if what is drawn in it was cut by some other rule.
fn fitted(fonts: &mut FontStore, url: &str, status: Option<&str>, shared: f32) -> Fitted {
    let (url_width, status_width) = share(fonts, url, status, shared);
    Fitted {
        url: elided(fonts, url, &ui_style(14.0), url_width),
        url_width,
        status: status.map(|text| elided(fonts, text, &ui_style(13.0), status_width)),
        status_width,
    }
}

/// The marker for text that did not fit.
///
/// U+2026, which is in the bundled families — checked by the same test that
/// caught the reload control drawing as a hollow box (ADR-0008).
const ELLIPSIS: &str = "\u{2026}";

/// `text`, cut to fit `max_width` and marked where it was cut.
///
/// The bar had no such marking, and silence here is the failure this project
/// spends most of its restraint avoiding: `https://example.com/behind-a-proxy`
/// clipped to `https://example.com/behi` does not look clipped, it looks like a
/// URL whose path is `behi`. A reader cannot tell they are missing something
/// unless they are told, and the address bar is the last place to be quietly
/// approximate.
fn elided(fonts: &mut FontStore, text: &str, style: &ComputedStyle, max_width: f32) -> String {
    if measure(fonts, text, style) <= max_width {
        return text.to_owned();
    }
    let marker = measure(fonts, ELLIPSIS, style);
    // Not even room for the marker, so there is nothing honest to draw. Better
    // an empty space than one character standing in for an address. This is
    // also the zero-and-below case, which is the caller saying there is no room
    // at all rather than a little — answering that with the untouched text
    // would hand `draw_text` a string to clip silently, which is the whole
    // thing being fixed.
    if marker > max_width {
        return String::new();
    }
    // Character boundaries, not bytes: this runs over URLs, which can be any
    // encoding a host is willing to serve.
    let mut kept = text;
    for (at, _) in text.char_indices().rev() {
        kept = &text[..at];
        if measure(fonts, kept, style) + marker <= max_width {
            break;
        }
    }
    format!("{kept}{ELLIPSIS}")
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
            forcing_document: false,
            can_toggle_layout: false,
            editing: None,
            finding: None,
            saved: false,
            local_root: false,
            theme: Theme::LIGHT,
        }
    }

    #[test]
    fn a_connection_verified_only_by_a_local_root_says_so() {
        // The marking ADR-0015 exists for. Trusting this computer's own roots
        // is what makes the browser usable behind an intercepting proxy;
        // trusting them *silently* would make an intercepted page and an
        // ordinary one look identical, which is the thing this project marks
        // everywhere else.
        let mut plain = state("https://example.com/a.html", &RenderMode::Authored);
        assert_eq!(status(&plain), None, "an ordinary page says nothing");
        plain.local_root = true;
        let said = status(&plain).expect("says something");
        assert!(said.contains("local certificate"), "{said}");

        // Whichever mode the page ended up in: an intercepted page rendered as
        // a document is still intercepted.
        let document = RenderMode::Document {
            unsupported_share: 1.0,
        };
        let mut fallen_back = state("https://example.com/a.html", &document);
        fallen_back.local_root = true;
        assert!(
            status(&fallen_back)
                .expect("says something")
                .contains("local certificate")
        );

        // Below an error, which is about whether the page loaded at all.
        fallen_back.error = Some("server returned 500");
        assert_eq!(status(&fallen_back).as_deref(), Some("server returned 500"));
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
        assert_eq!(
            placed.len(),
            5,
            "back, forward, reload, toggle, save — the toggle is on every page now"
        );

        assert_eq!(
            control_at(&state, 600.0, placed[0].1.x + 1.0, 5.0),
            Some(Control::Back)
        );
        assert_eq!(
            control_at(&state, 600.0, placed[1].1.x + 1.0, 5.0),
            Some(Control::Forward)
        );
        assert_eq!(
            control_at(&state, 600.0, placed[2].1.x + 1.0, 5.0),
            Some(Control::Reload)
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
    fn the_layout_toggle_is_on_every_page_and_never_moves() {
        // It used to be there only where classification had made a decision,
        // because on an ordinary page it had nothing to do. It has something to
        // do now: an ordinary page can be handed the document fallback, which
        // is what a reader wanting a plain view of a busy page asks for, and
        // which is not the absence of overruling a fallback (ADR-0009).
        //
        // "Never moves" is the half worth pinning. A control that appeared and
        // disappeared with the page would be one you had to look for each time,
        // and the reader who wants a plain view is looking for it *because* the
        // page in front of them is not plain.
        let authored = RenderMode::Authored;
        let ordinary = state("https://example.com/", &authored);
        assert_eq!(
            control_at(&ordinary, 600.0, 460.0, 17.0),
            Some(Control::ToggleLayout),
            "an ordinary page can be asked for the fallback too"
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

        // The same rectangle in both, which is what "never moves" means.
        let toggle_of = |state: &State<'_>| {
            controls(state)
                .into_iter()
                .find(|(control, _)| *control == Control::ToggleLayout)
                .expect("a toggle")
                .1
        };
        let (plain, decided) = (toggle_of(&ordinary), toggle_of(&fallback));
        assert_eq!((plain.x, plain.width), (decided.x, decided.width));

        // And it says different things in the two, because pressing it means
        // different things: one imposes a decision, the other overrules one.
        assert_ne!(toggle_label(&ordinary), toggle_label(&fallback));
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
    fn every_glyph_the_bar_draws_exists_in_the_bundled_fonts() {
        // Glyph 0 is `.notdef` by definition, and `.notdef` in these families
        // is the hollow box that shipped in a screenshot: the reload control
        // was given U+21BB, which is in none of the four ADR-0008 bundles, and
        // 124 tests passed over it because a box has a position like any other
        // glyph and differs from what was there before like any other change.
        //
        // Asking the font store directly is the check none of them were. It
        // costs nothing and it is the only one that would have failed.
        let mut fonts = FontStore::new();
        let mode = RenderMode::Authored;
        let mut state = state("http://example.com/", &mode);

        let mut wanted = vec![
            // The arrows, which are real.
            "\u{2190}".to_owned(),
            "\u{2192}".to_owned(),
            "reload".to_owned(),
            "Find:".to_owned(),
            // The cut marker. Drawn over a URL, which is the last place in the
            // browser that can afford a hollow box.
            ELLIPSIS.to_owned(),
        ];
        // Taken from the label functions rather than copied out of them, so a
        // control whose word changes is covered without anyone remembering to
        // come back here.
        for saved in [false, true] {
            state.saved = saved;
            wanted.push(bookmark_label(&state).to_owned());
        }
        // All three of the toggle's wordings, which needs both flags walked
        // rather than one: `simplify` only appears on a page in no decision,
        // and it is the one a bundled family has never had to draw before.
        let mut toggle_words: Vec<String> = Vec::new();
        for (forcing_authored, can_toggle_layout) in [(false, false), (false, true), (true, true)] {
            state.forcing_authored = forcing_authored;
            state.can_toggle_layout = can_toggle_layout;
            toggle_words.push(toggle_label(&state).to_owned());
        }
        state.forcing_authored = false;
        state.can_toggle_layout = false;
        toggle_words.sort();
        toggle_words.dedup();
        assert_eq!(
            toggle_words.len(),
            3,
            "these flags no longer reach all three wordings, so one goes unchecked: {toggle_words:?}"
        );
        wanted.extend(toggle_words);
        for url in ["http://example.com/", "file:///tmp/a.html"] {
            if let Some(notice) = scheme_notice(url) {
                wanted.push(notice.to_owned());
            }
        }
        state.local_root = true;
        if let Some(text) = status(&state) {
            wanted.push(text);
        }

        for text in wanted {
            for size in [12.0, 13.0, 15.0] {
                let layout = fonts.layout(&text, &ui_style(size), 10_000.0);
                for line in &layout.lines {
                    for glyph in &line.glyphs {
                        assert_ne!(
                            glyph.glyph_id, 0,
                            "{text:?} at {size}px draws a .notdef box — no bundled family has it"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_dark_theme_actually_reaches_the_pixels() {
        let mut fonts = FontStore::new();
        let mode = RenderMode::Authored;
        let mut state = state("https://example.com/", &mode);
        let light = render(&state, 600, &mut fonts);
        state.theme = Theme::DARK;
        let dark = render(&state, 600, &mut fonts);
        assert_ne!(light.data(), dark.data(), "the theme changed nothing");

        // Not just different — actually dark. A theme that swapped two pale
        // colours would pass the comparison above and fail every reader.
        let corner = &dark.data()[..4];
        assert!(
            corner[0] < 0x60 && corner[1] < 0x60 && corner[2] < 0x60,
            "the dark bar is not dark: {corner:?}"
        );
    }

    #[test]
    fn reload_is_offered_on_every_page_and_sits_beside_forward() {
        let mode = RenderMode::Authored;
        let state = state("https://example.com/", &mode);
        let placed = controls(&state);
        let (_, forward) = placed
            .iter()
            .find(|(control, _)| *control == Control::Forward)
            .expect("a forward button");
        let (_, reload) = placed
            .iter()
            .find(|(control, _)| *control == Control::Reload)
            .expect("a reload button");
        assert_eq!(reload.x, forward.x + BUTTON, "reload is not beside forward");
        assert_eq!(reload.width, RELOAD, "reload carries a word, not a glyph");
        // Unlike back and forward it is never greyed out, so it must always be
        // present to click — including on a page that failed to load, which is
        // exactly when it is wanted.
        assert_eq!(
            control_at(&state, 600.0, reload.x + 1.0, 5.0),
            Some(Control::Reload)
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
    fn a_long_status_never_leaves_the_url_with_nothing() {
        // The regression this rule exists for. The status used to be capped at
        // half the *bar* rather than half of what was left of it, so on a
        // narrow window the buttons and a long warning between them spent more
        // than the whole width and the URL was handed a negative one. It drew
        // nothing at all — no truncation, no marker, no address — on precisely
        // the pages that had given the reader something to judge.
        let mut fonts = FontStore::new();
        let mode = RenderMode::Document {
            unsupported_share: 0.87,
        };
        let mut busiest = state("https://example.com/something-modern", &mode);
        busiest.can_toggle_layout = true;
        let longest = status(&busiest).expect("a status");

        for width in [480.0f32, 600.0, 700.0, 900.0, 1400.0] {
            let url = "https://example.com/something-modern";
            let shared = (width
                - PADDING
                - BOOKMARK
                - TOGGLE
                - PADDING * 2.0
                - (PADDING * 2.0 + BUTTON * 2.0 + RELOAD))
                .max(0.0);
            let (url_width, status_width) = share(&mut fonts, url, Some(&longest), shared);

            assert!(
                url_width > 0.0,
                "the URL was crowded out entirely at {width}px"
            );
            assert!(status_width >= 0.0, "negative status at {width}px");
            assert!(
                (url_width + status_width - shared).abs() < 0.01,
                "the two do not add up at {width}px: {url_width} + {status_width} != {shared}"
            );
            // And what is drawn in that width actually says something: at least
            // the scheme and the beginning of the host, never a bare marker.
            let drawn = elided(&mut fonts, url, &ui_style(14.0), url_width);
            assert!(
                drawn.trim_end_matches(ELLIPSIS).len() > "https://".len(),
                "nothing of the host survived at {width}px: {drawn:?}"
            );
        }
    }

    /// What the bar has between its buttons at a given overall width.
    fn shared_at(width: f32) -> f32 {
        (width
            - PADDING
            - BOOKMARK
            - TOGGLE
            - PADDING * 2.0
            - (PADDING * 2.0 + BUTTON * 2.0 + RELOAD))
            .max(0.0)
    }

    #[test]
    fn where_the_url_is_squeezed_to_its_floor_the_whole_host_survives() {
        // The floor is the point of the rule, and it only bites in a band of
        // widths: wide enough that the URL is not simply taking half, narrow
        // enough that the status wants more than what is left. Inside that band
        // the URL gets exactly what it was guaranteed, so this is where a floor
        // measured a few pixels short shows up — as a host with its last
        // characters shaved off, which is the one cut with nothing to be said
        // for it.
        let mut fonts = FontStore::new();
        let mode = RenderMode::Document {
            unsupported_share: 0.87,
        };
        let mut busy = state("https://example.com/a/deep/path/page.html", &mode);
        busy.can_toggle_layout = true;
        let long = status(&busy).expect("a status");

        for width in [640.0f32, 680.0, 700.0, 740.0, 770.0] {
            let fit = fitted(&mut fonts, busy.url, Some(&long), shared_at(width));
            assert_eq!(
                fit.url, "https://example.com/\u{2026}",
                "the host did not survive intact at {width}px"
            );
            assert!(
                fit.status.as_deref().is_some_and(|s| s.ends_with(ELLIPSIS)),
                "this band is only interesting while the status is the one being cut: {:?}",
                fit.status
            );
        }
    }

    #[test]
    fn a_status_cut_for_space_says_so_too() {
        // The status is front-loaded so that a cut costs the least, which is a
        // reason to cut it and not a reason to hide that it was. Without this
        // the reader is told the page needs newer layout and never learns that
        // the sentence had a number on the end of it.
        let mut fonts = FontStore::new();
        let mode = RenderMode::Document {
            unsupported_share: 0.87,
        };
        let mut busy = state("https://example.com/something-modern", &mode);
        busy.can_toggle_layout = true;
        let long = status(&busy).expect("a status");

        let cramped = fitted(&mut fonts, busy.url, Some(&long), shared_at(600.0));
        let status_text = cramped.status.expect("a status");
        assert!(
            status_text.ends_with(ELLIPSIS),
            "a cut status has to say it was cut: {status_text:?}"
        );
        assert!(
            long.starts_with(status_text.trim_end_matches(ELLIPSIS)),
            "what survived is not the front of the message: {status_text:?}"
        );

        // And where it fits, it is left alone — a marker on every status would
        // stop meaning anything.
        let roomy = fitted(&mut fonts, busy.url, Some(&long), shared_at(1400.0));
        assert_eq!(roomy.status.as_deref(), Some(long.as_str()));
    }

    #[test]
    fn a_url_with_room_keeps_its_host_and_says_a_path_was_cut() {
        // Where the toggle's 96px actually bites: a page with a long warning on
        // an ordinary-width window. The host is what a reader is being asked to
        // trust, so it is what survives — and the cut is marked, because
        // `https://example.com/behi` does not look cut, it looks like a page
        // whose path is `behi`.
        let mut fonts = FontStore::new();
        let url = "https://example.com/behind-a-proxy.html";
        let authored = RenderMode::Authored;
        let mut proxied = state(url, &authored);
        proxied.local_root = true;
        let notice = status(&proxied).expect("a status");

        let shared = (700.0
            - PADDING
            - BOOKMARK
            - TOGGLE
            - PADDING * 2.0
            - (PADDING * 2.0 + BUTTON * 2.0 + RELOAD))
            .max(0.0);
        let (url_width, _) = share(&mut fonts, url, Some(&notice), shared);
        let drawn = elided(&mut fonts, url, &ui_style(14.0), url_width);

        assert!(
            drawn.starts_with("https://example.com/"),
            "the host did not survive: {drawn:?}"
        );
        assert!(
            drawn.ends_with(ELLIPSIS),
            "a cut URL has to say it was cut: {drawn:?}"
        );
        assert!(
            !drawn.contains("proxy.html"),
            "this case is only interesting while the URL does not fit: {drawn:?}"
        );
    }

    #[test]
    fn a_url_that_fits_is_left_exactly_alone() {
        // The marker must not appear where nothing was lost, or it stops
        // meaning anything and every URL looks approximate.
        let mut fonts = FontStore::new();
        let url = "https://example.com/";
        let drawn = elided(&mut fonts, url, &ui_style(14.0), 600.0);
        assert_eq!(drawn, url);
        assert!(!drawn.contains(ELLIPSIS));
    }

    #[test]
    fn eliding_cuts_on_characters_and_gives_up_rather_than_lying() {
        let mut fonts = FontStore::new();
        let style = ui_style(14.0);

        // Multi-byte text, cut at every width there is. A byte-indexed cut
        // would panic here rather than merely look wrong, and a URL can carry
        // whatever encoding a host is willing to serve.
        let wide = "https://пример.рф/путь/страница.html";
        for width in 0..240 {
            let drawn = elided(&mut fonts, wide, &style, width as f32);
            assert!(
                wide.starts_with(drawn.trim_end_matches(ELLIPSIS)),
                "the cut text is not a prefix of the original: {drawn:?}"
            );
        }

        // Narrower than the marker itself. There is nothing honest to draw, and
        // a lone marker would be a URL bar claiming a URL it cannot show.
        assert_eq!(elided(&mut fonts, wide, &style, 1.0), "");
        assert_eq!(elided(&mut fonts, wide, &style, 0.0), "");
    }

    #[test]
    fn the_origin_is_what_the_url_is_guaranteed() {
        assert_eq!(
            origin_prefix("https://example.com/behind-a-proxy.html"),
            "https://example.com/"
        );
        assert_eq!(
            origin_prefix("http://example.com:8080/a/b?c=d"),
            "http://example.com:8080/"
        );
        // No path at all, so the whole thing is the origin.
        assert_eq!(origin_prefix("https://example.com"), "https://example.com");
        assert_eq!(origin_prefix("file:///home/user/a.html"), "file:///");
        // Not a URL this browser would ever be showing, but the bar draws what
        // it is given and must not index into the middle of a character.
        assert_eq!(origin_prefix("nonsense"), "nonsense");
        assert_eq!(origin_prefix(""), "");
    }

    #[test]
    fn the_status_stops_short_of_the_controls() {
        // A status that ran under the buttons would be unreadable exactly when
        // it matters, which is when there is something to warn about.
        //
        // Checked on an ordinary page as well as a fallback one, because the
        // toggle is now drawn on both: the right-hand controls used to give
        // 96px back on a page with no decision to overrule, and every status
        // that fitted did so with that margin in hand.
        let fallback = RenderMode::Document {
            unsupported_share: 0.9,
        };
        let authored = RenderMode::Authored;

        let mut decided = state("http://example.com/", &fallback);
        decided.can_toggle_layout = true;
        // The longest thing the bar ever says about an ordinary page.
        let mut intercepted = state("http://example.com/", &authored);
        intercepted.local_root = true;

        for state in [&decided, &intercepted] {
            let placed = placed_controls(state, 600.0);
            let leftmost = placed
                .iter()
                // The right-hand controls only. The nav buttons sit at the left
                // edge and are not what the status has to stop short of.
                .filter(|(control, _)| {
                    !matches!(control, Control::Back | Control::Forward | Control::Reload)
                })
                .map(|(_, rect)| rect.x)
                .fold(f32::MAX, f32::min);
            assert!(leftmost < 600.0 - BOOKMARK, "{leftmost}");

            let text = status(state).expect("a status");
            let width = measure(&mut FontStore::new(), &text, &ui_style(13.0)).min(300.0);
            assert!(
                leftmost - PADDING - width > 0.0,
                "no room left for the URL beside {text:?}"
            );
        }
    }

    #[test]
    fn the_toggle_says_where_it_leads() {
        let authored = RenderMode::Authored;
        let document = RenderMode::Document {
            unsupported_share: 0.9,
        };

        // An ordinary page, in no decision at all. There is nothing here to
        // overrule and nothing to return to, so the word says what pressing it
        // does rather than naming a state the page was never in.
        let mut ordinary = state("https://example.com/", &authored);
        assert_eq!(toggle_label(&ordinary), "simplify");

        // Once pressed, it is in a decision like any other, and the way out is
        // the author's layout — the same word the automatic fallback offers,
        // because it is the same offer.
        ordinary.forcing_document = true;
        ordinary.can_toggle_layout = true;
        assert_eq!(
            toggle_label(&ordinary),
            "as authored",
            "a reader who asked for this needs the way back"
        );

        let mut fallback = state("https://example.com/", &document);
        fallback.can_toggle_layout = true;
        assert_eq!(toggle_label(&fallback), "as authored");
        fallback.forcing_authored = true;
        assert_eq!(
            toggle_label(&fallback),
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
        let first = render_tabs(&labels, 0, 600, &mut fonts, Theme::LIGHT);
        let second = render_tabs(&labels, 1, 600, &mut fonts, Theme::LIGHT);
        assert_ne!(first.data(), second.data());
        assert_eq!((first.width(), first.height()), (600, TAB_HEIGHT));
    }
}
