//! The menu the right-hand button opens.
//!
//! A browser without one is a browser where half of what a pointer can do is
//! unreachable (#54): opening a link in a new tab, copying its address,
//! copying what is selected. Every one of those already exists here — the
//! tabs, the clipboard, the selection — and none of them had anywhere to be
//! asked for.
//!
//! Deliberately not a platform menu. A native popup means a second window,
//! a second event loop, and a different implementation per platform, for a
//! list of five words; this is drawn into the same buffer as everything else
//! and hit-tested with the same arithmetic.

use layout::Rect;
use paint::{DisplayItem, DisplayList, Pixmap, rasterise};
use text::FontStore;

use crate::chrome::{PADDING, Theme, draw_text, ui_style};

/// Height of one row.
const ROW: f32 = 26.0;
/// Widest the menu may be, so a long URL does not reach across the window.
const MAX_WIDTH: f32 = 320.0;
/// Narrowest, so a menu of short words is not a sliver.
const MIN_WIDTH: f32 = 160.0;
/// Text size in the menu.
const TEXT: f32 = 13.0;

/// What a menu entry does when it is chosen.
///
/// The URL travels with the entry rather than being looked up again when it is
/// clicked: by then the pointer has moved to the menu and there is no longer a
/// link under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Go back to the previous page in this tab.
    Back,
    /// Go forward again.
    Forward,
    /// Fetch this page again.
    Reload,
    /// Open this link in a new tab.
    OpenInNewTab(String),
    /// Put this link's address on the clipboard.
    CopyLink(String),
    /// Put the selected text on the clipboard.
    CopySelection,
}

impl Item {
    /// What the row says.
    fn label(&self) -> &'static str {
        match self {
            Item::Back => "Back",
            Item::Forward => "Forward",
            Item::Reload => "Reload",
            Item::OpenInNewTab(_) => "Open link in new tab",
            Item::CopyLink(_) => "Copy link address",
            Item::CopySelection => "Copy",
        }
    }
}

/// What a menu should hold, given what the pointer is on and where the tab has
/// been.
///
/// Nothing is greyed out. An entry that cannot do anything is left out
/// instead: the menu is shorter to read, and there is nothing in it to click
/// in hope. "Reload" is the one entry that is always there, because there is
/// always a page to fetch again.
pub fn items_for(
    link: Option<String>,
    selection: bool,
    can_go_back: bool,
    can_go_forward: bool,
) -> Vec<Item> {
    let mut items = Vec::new();
    if let Some(url) = link {
        items.push(Item::OpenInNewTab(url.clone()));
        items.push(Item::CopyLink(url));
    }
    if selection {
        items.push(Item::CopySelection);
    }
    if can_go_back {
        items.push(Item::Back);
    }
    if can_go_forward {
        items.push(Item::Forward);
    }
    items.push(Item::Reload);
    items
}

/// An open menu: what is in it, and where it is.
#[derive(Debug, Clone)]
pub struct Menu {
    /// Top-left corner in window coordinates.
    pub at: (f32, f32),
    /// The entries, top to bottom.
    pub items: Vec<Item>,
    /// Which row the pointer is over.
    pub hovered: Option<usize>,
}

impl Menu {
    /// Opens a menu at `at`, kept inside a window of `size`.
    ///
    /// A menu opened near the right-hand edge is moved left rather than
    /// clipped, and one opened near the bottom opens upwards — which is what
    /// every menu everywhere does, and is the difference between a menu and a
    /// menu with its last two entries off the screen.
    pub fn open(at: (f32, f32), items: Vec<Item>, size: (u32, u32)) -> Option<Self> {
        if items.is_empty() {
            return None;
        }
        let (width, height) = (MAX_WIDTH, items.len() as f32 * ROW);
        let x = at.0.min((size.0 as f32 - width).max(0.0));
        let y = at.1.min((size.1 as f32 - height).max(0.0));
        Some(Self {
            at: (x, y),
            items,
            hovered: None,
        })
    }

    /// Where the menu sits, in window coordinates.
    pub fn rect(&self) -> Rect {
        Rect {
            x: self.at.0,
            y: self.at.1,
            width: MAX_WIDTH,
            height: self.items.len() as f32 * ROW,
        }
    }

    /// Which entry a window point is on, if any.
    pub fn item_at(&self, x: f32, y: f32) -> Option<usize> {
        let rect = self.rect();
        let inside =
            x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
        inside.then(|| ((y - rect.y) / ROW) as usize)
    }

    /// Draws the menu.
    pub fn render(&self, fonts: &mut FontStore, theme: Theme) -> Pixmap {
        let rect = self.rect();
        let mut list = DisplayList {
            canvas: theme.bar,
            ..DisplayList::default()
        };
        let style = ui_style(TEXT);

        // A one-pixel edge, drawn as a filled rectangle with a smaller one
        // over it: the menu floats over the page and needs a line to say where
        // it stops.
        list.items.push(DisplayItem::Rect {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                width: rect.width,
                height: rect.height,
            },
            color: theme.control_edge,
        });
        list.items.push(DisplayItem::Rect {
            rect: Rect {
                x: 1.0,
                y: 1.0,
                width: rect.width - 2.0,
                height: rect.height - 2.0,
            },
            color: theme.bar,
        });

        for (index, item) in self.items.iter().enumerate() {
            let top = index as f32 * ROW;
            if self.hovered == Some(index) {
                list.items.push(DisplayItem::Rect {
                    rect: Rect {
                        x: 1.0,
                        y: top + 1.0,
                        width: rect.width - 2.0,
                        height: ROW - 2.0,
                    },
                    color: theme.control,
                });
            }
            draw_text(
                &mut list,
                fonts,
                item.label(),
                &style,
                PADDING,
                top + ROW / 2.0 - TEXT * 0.7,
                theme.ink,
                rect.width - PADDING * 2.0,
            );
        }

        rasterise(
            &list,
            fonts,
            &paint::ImageStore::new(),
            rect.width.max(MIN_WIDTH) as u32,
            rect.height as u32,
        )
        .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Item> {
        vec![Item::Back, Item::Forward, Item::Reload]
    }

    #[test]
    fn a_menu_on_a_link_offers_what_a_link_can_do() {
        let items = items_for(Some("https://example.com/".to_owned()), false, true, false);

        assert_eq!(
            items,
            vec![
                Item::OpenInNewTab("https://example.com/".to_owned()),
                Item::CopyLink("https://example.com/".to_owned()),
                Item::Back,
                Item::Reload,
            ]
        );
    }

    #[test]
    fn a_menu_over_a_selection_offers_to_copy_it() {
        // The entry only exists when there is something to copy — the same
        // rule as back and forward, for the same reason.
        assert!(!items_for(None, false, false, false).contains(&Item::CopySelection));
        assert!(items_for(None, true, false, false).contains(&Item::CopySelection));
    }

    #[test]
    fn a_link_comes_before_the_selection_and_both_before_the_page() {
        // Nearest first: what the pointer is on, then what is on the page,
        // then what the tab can do. A menu that put "Reload" above "Copy link
        // address" would make the reader read the whole list every time.
        let items = items_for(Some("https://example.com/".to_owned()), true, true, false);

        assert_eq!(
            items,
            vec![
                Item::OpenInNewTab("https://example.com/".to_owned()),
                Item::CopyLink("https://example.com/".to_owned()),
                Item::CopySelection,
                Item::Back,
                Item::Reload,
            ]
        );
    }

    #[test]
    fn a_menu_on_bare_page_offers_only_what_the_page_can_do() {
        assert_eq!(items_for(None, false, false, false), vec![Item::Reload]);
    }

    #[test]
    fn nothing_is_offered_that_would_do_nothing() {
        // Greyed-out entries are a list of things you cannot have. Leaving
        // them out is shorter to read and cannot be clicked in hope.
        let fresh = items_for(None, false, false, false);
        assert!(!fresh.contains(&Item::Back), "{fresh:?}");
        assert!(!fresh.contains(&Item::Forward), "{fresh:?}");

        let travelled = items_for(None, false, true, true);
        assert!(travelled.contains(&Item::Back));
        assert!(travelled.contains(&Item::Forward));
    }

    #[test]
    fn a_menu_with_nothing_in_it_does_not_open() {
        assert!(Menu::open((10.0, 10.0), Vec::new(), (800, 600)).is_none());
    }

    #[test]
    fn a_menu_opens_where_the_pointer_is() {
        let menu = Menu::open((40.0, 50.0), items(), (800, 600)).expect("opens");
        assert_eq!(menu.at, (40.0, 50.0));
    }

    #[test]
    fn a_menu_near_an_edge_moves_rather_than_hanging_off_it() {
        // Every menu everywhere does this, and it is the difference between a
        // menu and a menu with its last two entries off the screen.
        let menu = Menu::open((790.0, 590.0), items(), (800, 600)).expect("opens");
        let rect = menu.rect();

        assert!(rect.x + rect.width <= 800.0, "{rect:?}");
        assert!(rect.y + rect.height <= 600.0, "{rect:?}");
    }

    #[test]
    fn a_menu_bigger_than_the_window_still_starts_inside_it() {
        // Nothing can produce this today. If a window is ever smaller than a
        // menu, the top-left corner is the part worth keeping.
        let menu = Menu::open((10.0, 10.0), items(), (100, 20)).expect("opens");

        assert_eq!(menu.at, (0.0, 0.0));
    }

    #[test]
    fn each_row_answers_for_its_own_band_and_nothing_else() {
        let menu = Menu::open((0.0, 0.0), items(), (800, 600)).expect("opens");

        assert_eq!(menu.item_at(10.0, 1.0), Some(0));
        assert_eq!(menu.item_at(10.0, ROW + 1.0), Some(1));
        assert_eq!(menu.item_at(10.0, ROW * 2.0 + 1.0), Some(2));
        // Past the last row, and to the left of the first.
        assert_eq!(menu.item_at(10.0, ROW * 3.0 + 1.0), None);
        assert_eq!(menu.item_at(-1.0, 1.0), None);
    }
}
