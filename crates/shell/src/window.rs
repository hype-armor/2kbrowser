//! The browser window.
//!
//! Manually verified once, on Linux: the window opens and draws correctly.
//! Automated coverage is limited to the pure logic below — scroll clamping and
//! the title string — because CI has no display server. Event handling and
//! blitting are therefore exercised only by hand, and a regression in them
//! would not be caught by `cargo test`. The pipeline underneath is the same one
//! the reference tests cover on all three platforms.
//!
//! Deliberately thin. Tabs, a URL bar, history, and the mode banner are M3
//! (ADR-0009); this is a viewport onto an already-rendered page.

use std::num::NonZeroU32;
use std::rc::Rc;

use layout::RenderMode;
use text::FontStore;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

/// Pixels scrolled per arrow-key press.
const SCROLL_STEP: f32 = 60.0;
/// Multiplier applied to line-based mouse wheel deltas.
const WHEEL_LINE_HEIGHT: f32 = 40.0;

/// Turns what someone typed into a URL.
///
/// A bare host is the overwhelmingly common case and has to work: typing
/// `example.com` and getting a file-not-found would be absurd. Anything naming
/// a scheme is left alone, and anything that looks like a path is treated as
/// one.
fn entered_url(typed: &str) -> String {
    if net::policy::has_scheme(typed) {
        return typed.to_owned();
    }
    if typed.starts_with('/') || typed.starts_with('.') {
        return format!("file://{typed}");
    }
    // https, not http: the browser should not quietly downgrade what it
    // reaches for, even though it will happily show a page that is only
    // available over http when a link leads there (ADR-0006).
    format!("https://{typed}")
}

/// Packs a rendered pixel for softbuffer.
///
/// softbuffer wants 0RGB in a u32; tiny-skia stores premultiplied RGBA.
/// Demultiplying is unnecessary because everything drawn here is composited
/// over an opaque background already.
fn pack(pixel: &paint::PremultipliedColor) -> u32 {
    (u32::from(pixel.red()) << 16) | (u32::from(pixel.green()) << 8) | u32::from(pixel.blue())
}

/// Clamps a scroll offset to the scrollable range.
///
/// Pure, and therefore testable without a display — which is most of what this
/// module gets wrong when it gets anything wrong.
fn clamp_scroll(offset: f32, content_height: f32, viewport_height: f32) -> f32 {
    let max = (content_height - viewport_height).max(0.0);
    offset.clamp(0.0, max)
}

/// Window title for a page, including its rendering mode.
///
/// ADR-0009 forbids switching rendering mode silently. Until M3 provides a
/// banner, the title bar is where the browser says what it did.
fn title_for(source: &str, mode: &RenderMode, error: Option<&str>) -> String {
    // A failed navigation is the most important thing the title can say, and
    // it outranks the mode: the page on screen is not the page that was asked
    // for, and nothing else would tell the reader that.
    if let Some(error) = error {
        return format!("{source} — {error} — 2kbrowser");
    }
    match mode {
        RenderMode::Authored => format!("{source} — 2kbrowser"),
        RenderMode::Document { .. } => format!("{source} — rendered as document — 2kbrowser"),
        RenderMode::RequiresScripting => format!("{source} — needs JavaScript — 2kbrowser"),
    }
}

/// A document that has been fetched and is ready to render.
struct Loaded {
    html: String,
    origin: net::Origin,
    path: String,
}

/// One tab: a document, where it came from, and how far down it you are.
///
/// Everything here describes a page. What is *not* here — the window, the
/// fonts, the pointer — belongs to the browser rather than to any page in it.
struct Tab {
    loaded: Loaded,
    history: crate::history::History,
    page: Option<crate::render::Page>,
    scroll: f32,
    /// What went wrong with the last navigation in this tab.
    error: Option<String>,
    /// Whether the reader has overruled the document fallback here (ADR-0009).
    /// Reset on navigation: it is a decision about a page, not a setting.
    forcing_authored: bool,
    /// Whether this page had a fallback decision to overrule.
    can_toggle_layout: bool,
    /// The find field when find-in-page is open in this tab.
    finding: Option<crate::field::Field>,
    /// Where the current query matches, in canvas coordinates.
    matches: Vec<layout::Rect>,
    /// Which match the reader is on.
    current_match: usize,
}

impl Tab {
    fn new(loaded: Loaded, url: String) -> Self {
        Self {
            loaded,
            history: crate::history::History::new(url),
            page: None,
            scroll: 0.0,
            error: None,
            forcing_authored: false,
            can_toggle_layout: false,
            finding: None,
            matches: Vec::new(),
            current_match: 0,
        }
    }

    /// What to call this tab: the page's title, or failing that its URL.
    fn label(&self) -> &str {
        match self.page.as_ref().and_then(|page| page.title.as_deref()) {
            Some(title) => title,
            None => self.history.current(),
        }
    }
}

/// Everything the event loop needs between frames.
struct App {
    tabs: crate::tabs::Tabs<Tab>,
    fetcher: net::Fetcher,
    fonts: FontStore,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    size: (u32, u32),
    /// Last known pointer position, in window coordinates.
    pointer: (f32, f32),
    /// Whether the pointer is over a link, so the cursor can say so.
    over_link: bool,
    /// Held because a key event does not carry the modifier state with it.
    modifiers: winit::event::Modifiers,
    /// The chrome bar, redrawn whenever what it says changes.
    chrome: paint::Pixmap,
    /// The tab strip. Empty when there is only one tab.
    strip: paint::Pixmap,
    /// The URL bar when it has focus. `None` means it is showing where you
    /// are rather than accepting where you want to go.
    editing: Option<crate::field::Field>,
    /// The saved list, shared by every tab: a bookmark is a property of the
    /// browser, not of the window you happened to press Ctrl+D in.
    bookmarks: crate::bookmarks::Bookmarks,
    /// Where that list is written back to.
    bookmarks_path: std::path::PathBuf,
}

impl App {
    /// The tab being shown.
    fn tab(&self) -> &Tab {
        self.tabs.active()
    }

    /// The tab being shown, mutably.
    fn tab_mut(&mut self) -> &mut Tab {
        self.tabs.active_mut()
    }

    /// Re-renders at the current width. Called on open and on resize, because
    /// layout depends on viewport width and nothing else here does.
    fn rerender(&mut self) {
        let (width, height) = self.size;
        if width == 0 || height == 0 {
            return;
        }
        let viewport = self.viewport_height();

        // Destructured so the borrows are disjoint: rendering needs the font
        // store mutably while reading the tab, and going through a method on
        // `self` would borrow the whole of it.
        let App { tabs, fonts, .. } = self;
        let tab = tabs.active_mut();

        // The canvas is the full document height, not the viewport height:
        // scrolling then costs a blit offset rather than a re-layout.
        let base = Some((&tab.loaded.origin, tab.loaded.path.as_str()));
        let page = if tab.forcing_authored {
            crate::render::render_as_authored(&tab.loaded.html, width, u32::MAX, fonts, base)
        } else {
            crate::render::render_with_base(&tab.loaded.html, width, u32::MAX, fonts, base)
        };

        // Whether there is anything to overrule. Once overruling, the answer is
        // yes by construction — the reader has to be able to get back.
        tab.can_toggle_layout =
            tab.forcing_authored || !matches!(page.mode, layout::RenderMode::Authored);
        tab.scroll = clamp_scroll(tab.scroll, page.content_height, viewport);
        tab.page = Some(page);

        // The old matches pointed at the old layout.
        if let Some(query) = tab.finding.as_ref().map(|field| field.text().to_owned()) {
            tab.matches = match (&tab.page, query.trim().is_empty()) {
                (Some(page), false) => page.find(&query),
                _ => Vec::new(),
            };
            tab.current_match = tab.current_match.min(tab.matches.len().saturating_sub(1));
        }

        if let Some(window) = &self.window {
            // The page's own title, falling back to the URL: a titled page is
            // named by its author, and an untitled one has only its address.
            let tab = self.tabs.active();
            let mode = tab.page.as_ref().map(|page| page.mode.clone());
            window.set_title(&title_for(
                tab.label(),
                &mode.unwrap_or(layout::RenderMode::Authored),
                tab.error.as_deref(),
            ));
        }
        self.refresh_chrome();
    }

    /// Fetches `url` and shows it, without touching history.
    ///
    /// Used by back and forward as well as by following a link, so the history
    /// bookkeeping stays in one place rather than being repeated per caller.
    fn show(&mut self, url: &str) {
        match self.fetcher.fetch(url, None, net::RequestKind::Navigation) {
            Ok(resource) => {
                self.tab_mut().loaded = Loaded {
                    html: resource.body,
                    origin: resource.origin,
                    path: resource.path,
                };
                self.tab_mut().error = None;
                self.tab_mut().scroll = 0.0;
                // A decision about the previous page, not a setting.
                self.tab_mut().forcing_authored = false;
            }
            // The page that failed stays on screen rather than being replaced
            // with a blank one: what was there is more useful than nothing, and
            // the title says what happened.
            Err(error) => self.tab_mut().error = Some(error.to_string()),
        }
        self.rerender();
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Follows a link.
    fn navigate(&mut self, url: String) {
        self.tab_mut().history.visit(url);
        let target = self.tab().history.current().to_owned();
        self.show(&target);
    }

    fn go_back(&mut self) {
        if let Some(url) = self.tab_mut().history.back().map(str::to_owned) {
            self.show(&url);
        }
    }

    fn go_forward(&mut self) {
        if let Some(url) = self.tab_mut().history.forward().map(str::to_owned) {
            self.show(&url);
        }
    }

    /// Redraws only the bar, which is all that changed.
    ///
    /// A keystroke in the URL bar must not re-lay-out the page: that is the
    /// difference between a field that feels instant and one that stutters on
    /// every character of a long document.
    fn refresh_chrome(&mut self) {
        // Destructured for disjoint borrows: the bar is drawn with the font
        // store while reading the tab it describes.
        let App {
            tabs,
            fonts,
            chrome,
            editing,
            size,
            bookmarks,
            ..
        } = self;
        let tab = tabs.active();
        let mode = tab
            .page
            .as_ref()
            .map(|page| page.mode.clone())
            .unwrap_or(layout::RenderMode::Authored);
        *chrome = crate::chrome::render(
            &crate::chrome::State {
                url: tab.history.current(),
                mode: &mode,
                error: tab.error.as_deref(),
                can_go_back: tab.history.can_go_back(),
                can_go_forward: tab.history.can_go_forward(),
                forcing_authored: tab.forcing_authored,
                can_toggle_layout: tab.can_toggle_layout,
                editing: editing.as_ref(),
                finding: tab
                    .finding
                    .as_ref()
                    .map(|field| (field, tab.current_match, tab.matches.len())),
                saved: bookmarks.contains(tab.history.current()),
            },
            size.0,
            fonts,
        );
        // The strip only exists with more than one tab, and its labels change
        // as pages load, so it is rebuilt with the bar.
        let App {
            tabs,
            fonts,
            strip,
            size,
            ..
        } = self;
        let labels: Vec<&str> = tabs.iter().map(Tab::label).collect();
        *strip = crate::chrome::render_tabs(&labels, tabs.active_index(), size.0, fonts);

        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// Gives the URL bar focus, with the current URL selected.
    fn focus_url(&mut self) {
        self.editing = Some(crate::field::Field::with_all_selected(
            self.tab().history.current(),
        ));
        self.refresh_chrome();
    }

    /// Gives up editing without navigating.
    fn cancel_editing(&mut self) {
        if self.editing.take().is_some() {
            self.refresh_chrome();
        }
    }

    /// Opens a new tab showing `url`, beside the current one.
    fn open_tab(&mut self, url: &str) {
        match self.fetcher.fetch(url, None, net::RequestKind::Navigation) {
            Ok(resource) => {
                let loaded = Loaded {
                    html: resource.body,
                    origin: resource.origin,
                    path: resource.path,
                };
                self.tabs.open(Tab::new(loaded, url.to_owned()));
            }
            Err(error) => {
                // A tab that failed still opens, showing nothing and saying
                // why. Silently not opening one looks like a broken click.
                let mut tab = Tab::new(
                    Loaded {
                        html: String::new(),
                        origin: self.tab().loaded.origin.clone(),
                        path: self.tab().loaded.path.clone(),
                    },
                    url.to_owned(),
                );
                tab.error = Some(error.to_string());
                self.tabs.open(tab);
            }
        }
        self.editing = None;
        self.rerender();
    }

    /// Closes a tab. The last one cannot be closed.
    fn close_tab(&mut self, index: usize) {
        if self.tabs.close(index).is_some() {
            self.editing = None;
            self.rerender();
        }
    }

    /// Switches tabs, which is a change of page and so of everything the
    /// chrome describes.
    fn select_tab(&mut self, index: usize) {
        if index == self.tabs.active_index() {
            return;
        }
        self.tabs.select(index);
        self.editing = None;
        // The new tab may never have been rendered, or was rendered at another
        // width, so it is laid out rather than merely redrawn.
        self.rerender();
    }

    /// Saves the current page, or forgets it if it is already saved.
    ///
    /// Written through immediately rather than on exit: a browser that lost
    /// your bookmarks because it was closed the wrong way would be worse than
    /// one with no bookmarks at all, and the file is a few kilobytes.
    fn toggle_bookmark(&mut self) {
        let url = self.tab().history.current().to_owned();
        let title = self
            .tab()
            .page
            .as_ref()
            .and_then(|page| page.title.clone())
            .unwrap_or_default();
        self.bookmarks.toggle(&url, &title);
        // A failed write is reported where every other navigation failure is,
        // because silently not saving looks exactly like saving.
        if let Err(error) = self.bookmarks.save(&self.bookmarks_path) {
            self.tab_mut().error = Some(format!("could not save bookmarks: {error}"));
        }
        self.refresh_chrome();
    }

    /// Opens the saved list, as a page, in a new tab.
    ///
    /// Written out and loaded like any other file so that back, forward, find,
    /// and the links on it all work without a second code path.
    fn open_bookmarks(&mut self) {
        let path = crate::bookmarks::page_path();
        let html = crate::bookmarks::page(&self.bookmarks);
        let written = path
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| std::fs::write(&path, html));
        match written {
            Ok(()) => self.open_tab(&format!("file://{}", path.display())),
            Err(error) => {
                self.tab_mut().error = Some(format!("could not write the saved list: {error}"));
                self.refresh_chrome();
            }
        }
    }

    /// Opens find-in-page, or refocuses it if it is already open.
    fn open_find(&mut self) {
        let existing = self
            .tab()
            .finding
            .as_ref()
            .map(|field| field.text().to_owned())
            .unwrap_or_default();
        self.tab_mut().finding = Some(crate::field::Field::with_all_selected(existing));
        self.refresh_matches();
    }

    /// Closes find, clearing the highlights.
    fn close_find(&mut self) {
        if self.tab_mut().finding.take().is_some() {
            self.tab_mut().matches.clear();
            self.tab_mut().current_match = 0;
            self.refresh_chrome();
        }
    }

    /// Re-runs the search after the query or the page changed.
    fn refresh_matches(&mut self) {
        let query = self
            .tab()
            .finding
            .as_ref()
            .map(|field| field.text().to_owned())
            .unwrap_or_default();
        self.tab_mut().matches = match (&self.tab().page, query.is_empty()) {
            (Some(page), false) => page.find(&query),
            _ => Vec::new(),
        };
        self.tab_mut().current_match = 0;
        self.scroll_to_current_match();
        self.refresh_chrome();
    }

    /// Steps to the next or previous match, wrapping around.
    fn step_match(&mut self, forward: bool) {
        if self.tab().matches.is_empty() {
            return;
        }
        let count = self.tab().matches.len();
        self.tab_mut().current_match = if forward {
            (self.tab().current_match + 1) % count
        } else {
            (self.tab().current_match + count - 1) % count
        };
        self.scroll_to_current_match();
        self.refresh_chrome();
    }

    /// Brings the current match into view, if it is not already.
    ///
    /// Only when it is off screen: a match already visible should stay where
    /// it is, because scrolling the page under someone who can see what they
    /// were looking for is disorienting.
    fn scroll_to_current_match(&mut self) {
        let Some(rect) = self.tab().matches.get(self.tab().current_match).copied() else {
            return;
        };
        let Some(page) = &self.tab().page else { return };
        let viewport = self.viewport_height();
        let (top, bottom) = (self.tab().scroll, self.tab().scroll + viewport);
        if rect.y >= top && rect.y + rect.height <= bottom {
            return;
        }
        // A third of the way down, which leaves the match in context rather
        // than jammed against the top edge.
        let target = rect.y - viewport / 3.0;
        self.tab_mut().scroll = clamp_scroll(target, page.content_height, viewport);
    }

    /// Handles a key while find is open.
    ///
    /// Returns whether the key was consumed.
    fn find_key(&mut self, key: &Key, alt: bool, ctrl: bool, shift: bool) -> bool {
        let Some(field) = &mut self.tab_mut().finding else {
            return false;
        };
        let by_word = ctrl || alt;
        match key {
            Key::Named(NamedKey::Escape) => {
                self.close_find();
                return true;
            }
            Key::Named(NamedKey::Enter) => {
                self.step_match(!shift);
                return true;
            }
            Key::Named(NamedKey::Backspace) => field.backspace(),
            Key::Named(NamedKey::Delete) => field.delete(),
            Key::Named(NamedKey::ArrowLeft) if by_word => field.word_left(shift),
            Key::Named(NamedKey::ArrowRight) if by_word => field.word_right(shift),
            Key::Named(NamedKey::ArrowLeft) => field.left(shift),
            Key::Named(NamedKey::ArrowRight) => field.right(shift),
            Key::Named(NamedKey::Home) => field.home(shift),
            Key::Named(NamedKey::End) => field.end(shift),
            Key::Named(NamedKey::Space) => field.insert(" "),
            Key::Character(text) if ctrl => {
                if text.as_str() == "a" {
                    field.select_all();
                    self.refresh_chrome();
                }
                return true;
            }
            Key::Character(text) => field.insert(text.as_str()),
            _ => return true,
        }
        // Anything that changed the query re-runs the search.
        self.refresh_matches();
        true
    }

    /// Handles a key while the URL bar has focus.
    ///
    /// Returns whether the key was consumed, so a keystroke meant for the field
    /// never also scrolls the page underneath it.
    fn edit_key(&mut self, key: &Key, alt: bool, ctrl: bool, shift: bool) -> bool {
        let Some(field) = &mut self.editing else {
            return false;
        };
        // Word motion is Ctrl+arrow everywhere except macOS, where it is
        // Alt+arrow. Accepting both costs nothing and spares the question.
        let by_word = ctrl || alt;
        match key {
            Key::Named(NamedKey::Escape) => {
                self.cancel_editing();
                return true;
            }
            Key::Named(NamedKey::Enter) => {
                let typed = field.text().trim().to_owned();
                self.editing = None;
                if typed.is_empty() {
                    self.refresh_chrome();
                } else {
                    self.navigate(entered_url(&typed));
                }
                return true;
            }
            Key::Named(NamedKey::Backspace) => field.backspace(),
            Key::Named(NamedKey::Delete) => field.delete(),
            Key::Named(NamedKey::ArrowLeft) if by_word => field.word_left(shift),
            Key::Named(NamedKey::ArrowRight) if by_word => field.word_right(shift),
            Key::Named(NamedKey::ArrowLeft) => field.left(shift),
            Key::Named(NamedKey::ArrowRight) => field.right(shift),
            Key::Named(NamedKey::Home) => field.home(shift),
            Key::Named(NamedKey::End) => field.end(shift),
            Key::Named(NamedKey::Space) => field.insert(" "),
            Key::Character(text) if ctrl => {
                if text.as_str() == "a" {
                    field.select_all();
                } else {
                    return true;
                }
            }
            Key::Character(text) => field.insert(text.as_str()),
            // Anything else is swallowed rather than falling through to the
            // page: while the field has focus, it has focus.
            _ => return true,
        }
        self.refresh_chrome();
        true
    }

    /// The chrome control under the pointer, if any.
    fn control_under_pointer(&self) -> Option<crate::chrome::Control> {
        let mode = self.tab().page.as_ref().map(|page| page.mode.clone());
        let mode = mode.unwrap_or(layout::RenderMode::Authored);
        crate::chrome::control_at(
            &crate::chrome::State {
                url: self.tab().history.current(),
                mode: &mode,
                error: self.tab().error.as_deref(),
                can_go_back: self.tab().history.can_go_back(),
                can_go_forward: self.tab().history.can_go_forward(),
                forcing_authored: self.tab().forcing_authored,
                can_toggle_layout: self.tab().can_toggle_layout,
                editing: self.editing.as_ref(),
                finding: self
                    .tab()
                    .finding
                    .as_ref()
                    .map(|field| (field, self.tab().current_match, self.tab().matches.len())),
                saved: self.bookmarks.contains(self.tab().history.current()),
            },
            self.size.0 as f32,
            self.pointer.0,
            // The bar's own coordinates: with a strip above it, the window's
            // y is not the bar's y, and a click would land on the row above
            // whatever it looked like it was on.
            self.pointer.1 - (self.chrome_height() - crate::chrome::HEIGHT) as f32,
        )
    }

    /// The link under the pointer, if any.
    ///
    /// The pointer is in window coordinates; the page starts below the bar and
    /// is scrolled, so both have to come off before the page can be asked.
    fn link_under_pointer(&self) -> Option<String> {
        let page = self.tab().page.as_ref()?;
        let y = self.pointer.1 - self.chrome_height() as f32;
        if y < 0.0 {
            return None;
        }
        page.link_at(self.pointer.0, y + self.tab().scroll)
    }

    /// Total chrome height: the URL bar, plus the tab strip when there is one.
    fn chrome_height(&self) -> u32 {
        crate::chrome::total_height(self.tabs.len())
    }

    /// Height of the page area, which is the window less the chrome.
    fn viewport_height(&self) -> f32 {
        (self.size.1.saturating_sub(self.chrome_height())) as f32
    }

    fn scroll_by(&mut self, delta: f32) {
        let Some(page) = &self.tab().page else { return };
        let before = self.tab().scroll;
        self.tab_mut().scroll = clamp_scroll(
            self.tab().scroll + delta,
            page.content_height,
            self.viewport_height(),
        );
        if self.tab().scroll != before
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    /// Copies the rendered page into the window surface at the current scroll.
    fn draw(&mut self) {
        // Destructured for disjoint borrows: the surface is written while the
        // page and the chrome are read.
        let App {
            tabs,
            surface,
            chrome,
            strip,
            size,
            ..
        } = self;
        let tab = tabs.active();
        let (Some(surface), Some(page)) = (surface.as_mut(), tab.page.as_ref()) else {
            return;
        };
        let (Some(width), Some(height)) = (NonZeroU32::new(size.0), NonZeroU32::new(size.1)) else {
            return;
        };
        if surface.resize(width, height).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };

        let pixmap = &page.pixmap;
        let offset = tab.scroll as u32;
        let viewport_width = width.get() as usize;
        let strip_height = if tabs.len() > 1 {
            crate::chrome::TAB_HEIGHT.min(height.get())
        } else {
            0
        };
        let bar_height = (strip_height + crate::chrome::HEIGHT).min(height.get());

        // The strip, then the bar, across the top.
        let blit = |buffer: &mut [u32], source: &paint::Pixmap, from: u32, rows: u32| {
            for row in 0..rows {
                let start = (from + row) as usize * viewport_width;
                let source_start = row as usize * source.width() as usize;
                for column in 0..viewport_width {
                    buffer[start + column] = match source.pixels().get(source_start + column) {
                        Some(pixel) => pack(pixel),
                        None => 0x00ff_ffff,
                    };
                }
            }
        };
        blit(&mut buffer, strip, 0, strip_height);
        blit(&mut buffer, chrome, strip_height, bar_height - strip_height);

        for row in bar_height..height.get() {
            let source_row = row - bar_height + offset;
            let start = row as usize * viewport_width;
            if source_row >= pixmap.height() {
                // Past the end of the document: white, not stale pixels.
                buffer[start..start + viewport_width].fill(0x00ff_ffff);
                continue;
            }
            let pixels = pixmap.pixels();
            let source_start = source_row as usize * pixmap.width() as usize;
            for column in 0..viewport_width {
                buffer[start + column] = match pixels.get(source_start + column) {
                    Some(pixel) => pack(pixel),
                    None => 0x00ff_ffff,
                };
            }
        }

        highlight_matches(
            &mut buffer,
            &tab.matches,
            tab.current_match,
            tab.scroll,
            (width.get(), height.get()),
            bar_height,
        );
        let _ = buffer.present();
    }
}

/// Tints the find matches in the window buffer.
///
/// Drawn here rather than into the page so that searching does not re-render
/// anything: the page is expensive and unchanged, and the highlights move every
/// time the reader steps to the next match.
fn highlight_matches(
    buffer: &mut [u32],
    matches: &[layout::Rect],
    current: usize,
    scroll: f32,
    size: (u32, u32),
    bar_height: u32,
) {
    let (width, height) = size;
    for (index, rect) in matches.iter().enumerate() {
        // The current match is stronger, because "which one am I on" is the
        // question the reader is actually asking.
        let tint = if index == current {
            (255u32, 185u32, 50u32)
        } else {
            (255, 240, 150)
        };
        let top = rect.y - scroll + bar_height as f32;
        let (x0, x1) = (
            rect.x.max(0.0) as u32,
            (rect.x + rect.width).max(0.0) as u32,
        );
        let (y0, y1) = (
            top.max(bar_height as f32) as u32,
            (top + rect.height).max(0.0) as u32,
        );

        for y in y0..y1.min(height) {
            for x in x0..x1.min(width) {
                let Some(pixel) = buffer.get_mut((y * width + x) as usize) else {
                    continue;
                };
                // Multiplied rather than replaced, so the text stays legible
                // through the highlight instead of being painted over.
                let blend = |shift: u32, tint: u32| {
                    let channel = (*pixel >> shift) & 0xff;
                    ((channel * tint) / 255) << shift
                };
                *pixel = blend(16, tint.0) | blend(8, tint.1) | blend(0, tint.2);
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attributes = Window::default_attributes()
            .with_title(self.tab().history.current())
            .with_inner_size(winit::dpi::LogicalSize::new(self.size.0, self.size.1));
        let Ok(window) = event_loop.create_window(attributes) else {
            event_loop.exit();
            return;
        };
        let window = Rc::new(window);

        let context = match softbuffer::Context::new(window.clone()) {
            Ok(context) => context,
            Err(_) => {
                event_loop.exit();
                return;
            }
        };
        match softbuffer::Surface::new(&context, window.clone()) {
            Ok(surface) => self.surface = Some(surface),
            Err(_) => {
                event_loop.exit();
                return;
            }
        }

        let size = window.inner_size();
        self.size = (size.width.max(1), size.height.max(1));
        self.window = Some(window);
        self.rerender();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.size = (size.width.max(1), size.height.max(1));
                self.rerender();
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers,
            WindowEvent::MouseWheel { delta, .. } => {
                let pixels = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => -lines * WHEEL_LINE_HEIGHT,
                    MouseScrollDelta::PixelDelta(position) => -position.y as f32,
                };
                self.scroll_by(pixels);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = (position.x as f32, position.y as f32);
                // The cursor says whether there is a link here, which is how a
                // pointer-driven browser has always answered that question.
                let over = self.link_under_pointer().is_some();
                if over != self.over_link
                    && let Some(window) = &self.window
                {
                    self.over_link = over;
                    window.set_cursor(if over {
                        winit::window::CursorIcon::Pointer
                    } else {
                        winit::window::CursorIcon::Default
                    });
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button,
                ..
            } => match button {
                MouseButton::Left => {
                    // The bar owns the top of the window, so it gets first
                    // refusal on a click there.
                    let strip_height = if self.tabs.len() > 1 {
                        crate::chrome::TAB_HEIGHT as f32
                    } else {
                        0.0
                    };
                    if self.pointer.1 < strip_height {
                        if let Some((index, on_close)) = crate::chrome::tab_at(
                            self.tabs.len(),
                            self.size.0 as f32,
                            self.pointer.0,
                            self.pointer.1,
                        ) {
                            if on_close {
                                self.close_tab(index);
                            } else {
                                self.select_tab(index);
                            }
                        }
                    } else if self.pointer.1 < self.chrome_height() as f32 {
                        match self.control_under_pointer() {
                            Some(crate::chrome::Control::Back) => self.go_back(),
                            Some(crate::chrome::Control::Forward) => self.go_forward(),
                            Some(crate::chrome::Control::Bookmark) => self.toggle_bookmark(),
                            Some(crate::chrome::Control::ToggleLayout) => {
                                self.tab_mut().forcing_authored = !self.tab().forcing_authored;
                                self.rerender();
                                if let Some(window) = &self.window {
                                    window.request_redraw();
                                }
                            }
                            // Anywhere else in the bar is the URL, and clicking
                            // a URL bar is how most people focus one.
                            None => self.focus_url(),
                        }
                    } else {
                        // A click on the page is a click on the page, even if
                        // it is not on a link: the URL bar loses focus.
                        self.cancel_editing();
                        if let Some(url) = self.link_under_pointer() {
                            self.navigate(url);
                        }
                    }
                }
                // Middle-click opens a link in a new tab, which is how most
                // people open most tabs.
                MouseButton::Middle => {
                    if let Some(url) = self.link_under_pointer() {
                        self.open_tab(&url);
                    }
                }
                // The mouse's own back and forward buttons, which people who
                // have them use constantly.
                MouseButton::Back => self.go_back(),
                MouseButton::Forward => self.go_forward(),
                _ => {}
            },
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let viewport = self.viewport_height();
                // Alt+Left and Alt+Right are the platform convention;
                // Backspace is what the era's browsers used and many hands
                // still reach for.
                let state = self.modifiers.state();
                let (alt, ctrl, shift) = (state.alt_key(), state.control_key(), state.shift_key());

                // A focused field takes every key, so a keystroke meant for it
                // never also scrolls the page underneath.
                if self.editing.is_some() {
                    self.edit_key(&event.logical_key, alt, ctrl, shift);
                    return;
                }
                if self.tab().finding.is_some() {
                    self.find_key(&event.logical_key, alt, ctrl, shift);
                    return;
                }
                if ctrl && matches!(&event.logical_key, Key::Character(c) if c == "f") {
                    self.open_find();
                    return;
                }
                if ctrl {
                    match &event.logical_key {
                        // A new tab shows the page you are on, which is the
                        // only thing this browser could put there: there is no
                        // home page and no new-tab page to fill with tiles.
                        Key::Character(c) if c == "t" => {
                            let url = self.tab().history.current().to_owned();
                            self.open_tab(&url);
                            return;
                        }
                        Key::Character(c) if c == "w" => {
                            self.close_tab(self.tabs.active_index());
                            return;
                        }
                        // Ctrl+D saves and Ctrl+B shows the list, which is
                        // where every browser has put them for twenty years.
                        Key::Character(c) if c == "d" => {
                            self.toggle_bookmark();
                            return;
                        }
                        Key::Character(c) if c == "b" => {
                            self.open_bookmarks();
                            return;
                        }
                        Key::Named(NamedKey::Tab) => {
                            if shift {
                                self.tabs.previous();
                            } else {
                                self.tabs.next();
                            }
                            self.editing = None;
                            self.rerender();
                            return;
                        }
                        // Ctrl+1..9 jumps to a tab, and Ctrl+9 is the last one
                        // however many there are — which is the convention
                        // everywhere and is more useful than a ninth tab.
                        Key::Character(c) if c.len() == 1 => {
                            if let Some(digit) = c.chars().next().and_then(|c| c.to_digit(10))
                                && digit > 0
                            {
                                let index = if digit == 9 {
                                    self.tabs.len() - 1
                                } else {
                                    digit as usize - 1
                                };
                                self.select_tab(index);
                                return;
                            }
                        }
                        _ => {}
                    }
                }
                // F3 steps through matches without reopening the field, which
                // is how someone reads a page while searching it.
                if matches!(event.logical_key, Key::Named(NamedKey::F3)) {
                    self.step_match(!shift);
                    return;
                }
                // Ctrl+L is the URL bar everywhere; F6 and Alt+D are the other
                // two spellings people's hands know.
                if (ctrl && matches!(&event.logical_key, Key::Character(c) if c == "l"))
                    || matches!(event.logical_key, Key::Named(NamedKey::F6))
                    || (alt && matches!(&event.logical_key, Key::Character(c) if c == "d"))
                {
                    self.focus_url();
                    return;
                }
                match event.logical_key {
                    Key::Named(NamedKey::ArrowLeft) if alt => {
                        self.go_back();
                        return;
                    }
                    Key::Named(NamedKey::ArrowRight) if alt => {
                        self.go_forward();
                        return;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        self.go_back();
                        return;
                    }
                    _ => {}
                }
                match event.logical_key {
                    Key::Named(NamedKey::Escape) => event_loop.exit(),
                    Key::Named(NamedKey::ArrowDown) => self.scroll_by(SCROLL_STEP),
                    Key::Named(NamedKey::ArrowUp) => self.scroll_by(-SCROLL_STEP),
                    Key::Named(NamedKey::PageDown) | Key::Named(NamedKey::Space) => {
                        self.scroll_by(viewport * 0.9);
                    }
                    Key::Named(NamedKey::PageUp) => self.scroll_by(-viewport * 0.9),
                    Key::Named(NamedKey::Home) => self.scroll_by(f32::NEG_INFINITY),
                    Key::Named(NamedKey::End) => self.scroll_by(f32::INFINITY),
                    Key::Character(ref c) if c == "q" => event_loop.exit(),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Opens a window showing the document already fetched from `url`, and runs
/// until the user closes it.
///
/// The fetched body is passed in rather than re-fetched so the caller can
/// report a failure before a window ever appears. Blocks for the lifetime of
/// the window.
pub fn open(
    html: String,
    url: String,
    origin: net::Origin,
    path: String,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|error| {
        format!("could not start the event loop ({error}); is a display available?")
    })?;
    // Wait rather than poll: a document browser has nothing to animate, and
    // polling would burn CPU against the resource-weight goal for no benefit.
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App {
        tabs: crate::tabs::Tabs::new(Tab::new(Loaded { html, origin, path }, url)),
        fetcher: net::Fetcher::default(),
        fonts: FontStore::new(),
        window: None,
        surface: None,
        size: (width.max(1), height.max(1)),
        pointer: (0.0, 0.0),
        over_link: false,
        modifiers: winit::event::Modifiers::default(),
        chrome: paint::Pixmap::new(1, 1).expect("1x1 pixmap"),
        strip: paint::Pixmap::new(1, 1).expect("1x1 pixmap"),
        editing: None,
        bookmarks: crate::bookmarks::Bookmarks::load(&crate::bookmarks::default_path()),
        bookmarks_path: crate::bookmarks::default_path(),
    };
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_is_clamped_to_the_document() {
        // Cannot scroll above the top.
        assert_eq!(clamp_scroll(-50.0, 1000.0, 600.0), 0.0);
        // Nor past the last screenful.
        assert_eq!(clamp_scroll(9999.0, 1000.0, 600.0), 400.0);
        assert_eq!(clamp_scroll(150.0, 1000.0, 600.0), 150.0);
    }

    #[test]
    fn a_document_shorter_than_the_window_does_not_scroll() {
        assert_eq!(clamp_scroll(100.0, 300.0, 600.0), 0.0);
        assert_eq!(clamp_scroll(f32::INFINITY, 300.0, 600.0), 0.0);
    }

    #[test]
    fn the_title_states_the_rendering_mode() {
        // ADR-0009: never switch rendering mode silently. Until M3 has a
        // banner, the title bar carries it.
        assert_eq!(
            title_for("a.html", &RenderMode::Authored, None),
            "a.html — 2kbrowser"
        );
        assert!(
            title_for(
                "a.html",
                &RenderMode::Document {
                    unsupported_share: 0.9
                },
                None
            )
            .contains("rendered as document")
        );
        assert!(
            title_for("a.html", &RenderMode::RequiresScripting, None).contains("needs JavaScript")
        );
    }

    #[test]
    fn a_failed_navigation_outranks_the_mode_in_the_title() {
        // The page on screen is not the page that was asked for, and with no
        // chrome yet the title is the only place that can say so.
        let title = title_for("b.html", &RenderMode::Authored, Some("404"));
        assert!(title.contains("404"), "got {title}");
    }
}

#[cfg(test)]
mod entered_url_tests {
    use super::entered_url;

    #[test]
    fn a_bare_host_becomes_https() {
        // The overwhelmingly common thing typed into a URL bar. Getting a
        // file-not-found for `example.com` would be absurd.
        assert_eq!(entered_url("example.com"), "https://example.com");
        assert_eq!(
            entered_url("example.com/a/b.html"),
            "https://example.com/a/b.html"
        );
    }

    #[test]
    fn a_scheme_is_left_alone() {
        // Including http: the browser will show a page that is only available
        // over http when a link leads there, and typing one is the same thing.
        for typed in [
            "http://example.org/old.html",
            "https://example.com/",
            "file:///home/user/a.html",
        ] {
            assert_eq!(entered_url(typed), typed);
        }
    }

    #[test]
    fn a_path_becomes_a_file_url() {
        assert_eq!(entered_url("/home/user/a.html"), "file:///home/user/a.html");
        assert_eq!(entered_url("./a.html"), "file://./a.html");
    }
}

#[cfg(test)]
mod highlight_tests {
    use super::highlight_matches;
    use layout::Rect;

    /// A 4x4 white buffer with no chrome, for arithmetic that is easier to
    /// read than to reason about.
    fn buffer() -> Vec<u32> {
        vec![0x00ff_ffff; 16]
    }

    fn tinted(buffer: &[u32]) -> usize {
        buffer.iter().filter(|p| **p != 0x00ff_ffff).count()
    }

    #[test]
    fn a_match_tints_exactly_its_rectangle() {
        let mut buffer = buffer();
        highlight_matches(
            &mut buffer,
            &[Rect {
                x: 1.0,
                y: 1.0,
                width: 2.0,
                height: 2.0,
            }],
            0,
            0.0,
            (4, 4),
            0,
        );
        assert_eq!(tinted(&buffer), 4);
        assert_eq!(buffer[0], 0x00ff_ffff, "the corner is untouched");
    }

    #[test]
    fn the_current_match_is_tinted_differently_from_the_rest() {
        // "Which one am I on" is the question the reader is actually asking.
        let rects = [
            Rect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
            Rect {
                x: 2.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        ];
        let mut buffer = buffer();
        highlight_matches(&mut buffer, &rects, 1, 0.0, (4, 4), 0);
        assert_ne!(buffer[0], buffer[2]);
    }

    #[test]
    fn scrolling_moves_the_highlight_with_the_page() {
        let rect = Rect {
            x: 0.0,
            y: 2.0,
            width: 4.0,
            height: 1.0,
        };
        let mut at_top = buffer();
        highlight_matches(&mut at_top, &[rect], 0, 0.0, (4, 4), 0);
        let mut scrolled = buffer();
        highlight_matches(&mut scrolled, &[rect], 0, 2.0, (4, 4), 0);
        assert_ne!(at_top, scrolled);
        assert_eq!(tinted(&scrolled), 4, "still one row, higher up");
    }

    #[test]
    fn a_match_scrolled_off_the_top_does_not_bleed_into_the_chrome() {
        // The bar is not part of the page and must never be tinted by it.
        let mut buffer = buffer();
        highlight_matches(
            &mut buffer,
            &[Rect {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 4.0,
            }],
            0,
            3.0,
            (4, 4),
            2,
        );
        assert_eq!(buffer[0], 0x00ff_ffff, "chrome row untouched");
        assert_eq!(buffer[4], 0x00ff_ffff, "chrome row untouched");
    }

    #[test]
    fn a_match_past_the_edges_is_clipped_rather_than_panicking() {
        let mut buffer = buffer();
        highlight_matches(
            &mut buffer,
            &[Rect {
                x: -10.0,
                y: -10.0,
                width: 100.0,
                height: 100.0,
            }],
            0,
            0.0,
            (4, 4),
            0,
        );
        assert_eq!(tinted(&buffer), 16);
    }

    #[test]
    fn no_matches_leaves_the_buffer_alone() {
        let mut buffer = buffer();
        highlight_matches(&mut buffer, &[], 0, 0.0, (4, 4), 0);
        assert_eq!(tinted(&buffer), 0);
    }
}
