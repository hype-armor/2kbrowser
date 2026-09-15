//! The list a `<select>` opens.
//!
//! A closed dropdown is a control whose whole purpose is the list nobody can
//! see. Until now this browser drew the box, drew the option the markup had
//! chosen, and had no way to show the others — so a form with a country picker
//! could only ever be submitted with whatever country the author put first.
//!
//! The list has to float over the page: there is no room for it in the box, and
//! pushing the page down to make room would move everything the reader was
//! looking at. Floating over the page means it cannot be drawn by the renderer,
//! whose canvas *is* the page, so it is drawn here in the chrome's buffer
//! beside the menu and the site panel — which is where everything that floats
//! already lives, and why this looks so much like [`crate::menu`].
//!
//! What it does not do is hold the answer. The child says what is in the list
//! and this draws it; when a row is pressed, the child is told which row and
//! decides what that means. The window never learns which option is selected,
//! because that is a fact about a document it does not have (ADR-0012).

use layout::Rect;
use paint::{DisplayItem, DisplayList, Pixmap, rasterise};
use text::FontStore;

use crate::chrome::{PADDING, Theme, draw_text, elided, measure, ui_style};

/// Height of one row.
const ROW: f32 = 22.0;
/// Text size in the list.
const TEXT: f32 = 13.0;
/// Narrowest the list may be, so a list of one-letter options is not a sliver.
const MIN_WIDTH: f32 = 80.0;
/// Widest, so a list of long options does not reach across the window.
const MAX_WIDTH: f32 = 420.0;
/// How many rows are shown before the list stops growing.
///
/// A `<select>` of every country in the world is a real thing on the era's web,
/// and a list two hundred rows tall is a list taller than the window. Scrolling
/// one is a control of its own and is not offered yet, so the list is capped
/// and the rows past the cap are unreachable — which is worse than scrolling
/// and much better than a list that draws off the bottom of the screen.
const MAX_ROWS: usize = 12;

/// An open dropdown: what is in it, where it is, and which row is on.
#[derive(Debug, Clone, PartialEq)]
pub struct Dropdown {
    /// Top-left corner, in window coordinates.
    pub at: (f32, f32),
    /// How wide the list is drawn.
    pub width: f32,
    /// What each row reads as, in document order.
    pub options: Vec<String>,
    /// Which row the `<select>` is currently on.
    pub on: usize,
    /// Which row the pointer is over.
    pub hovered: Option<usize>,
    /// The child's own name for the `<select>`, carried so it can be handed
    /// back when a row is chosen. Never read here.
    pub node: u32,
}

impl Dropdown {
    /// Opens a list under `box_`, kept inside a window of `size`.
    ///
    /// Under the control rather than at the pointer, which is what a dropdown
    /// does everywhere and is the difference between a list that belongs to
    /// the box above it and a menu that happens to be nearby. Near the bottom
    /// of the window it opens upwards instead, for the reason every menu
    /// everywhere does: a list with its last rows off the screen is a list
    /// with rows nobody can reach.
    pub fn open(
        box_: Rect,
        options: Vec<String>,
        on: usize,
        node: u32,
        fonts: &mut FontStore,
        size: (u32, u32),
    ) -> Option<Self> {
        if options.is_empty() {
            return None;
        }
        let style = ui_style(TEXT);
        // At least as wide as the control it belongs to, so the list does not
        // look like it came from somewhere else, and wide enough for its own
        // longest row.
        let widest = options
            .iter()
            .map(|option| measure(fonts, option, &style))
            .fold(box_.width, f32::max);
        let width = (widest + PADDING * 2.0).clamp(MIN_WIDTH, MAX_WIDTH);

        let rows = options.len().min(MAX_ROWS);
        let height = rows as f32 * ROW;
        let x = box_.x.min((size.0 as f32 - width).max(0.0)).max(0.0);
        // Below the box if it fits, above it if it does not, and pinned to the
        // top if neither does.
        let below = box_.y + box_.height;
        let y = if below + height <= size.1 as f32 {
            below
        } else {
            (box_.y - height).max(0.0)
        };
        Some(Self {
            at: (x, y),
            width,
            options,
            on,
            hovered: None,
            node,
        })
    }

    /// How many rows are drawn, which is not always how many options there are.
    fn rows(&self) -> usize {
        self.options.len().min(MAX_ROWS)
    }

    /// Where the list sits, in window coordinates.
    pub fn rect(&self) -> Rect {
        Rect {
            x: self.at.0,
            y: self.at.1,
            width: self.width,
            height: self.rows() as f32 * ROW,
        }
    }

    /// Which row a window point is on, if any.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        let rect = self.rect();
        let inside =
            x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
        inside
            .then(|| ((y - rect.y) / ROW) as usize)
            .filter(|row| *row < self.rows())
    }

    /// Draws the list.
    pub fn render(&self, fonts: &mut FontStore, theme: Theme) -> Pixmap {
        let rect = self.rect();
        let mut list = DisplayList {
            canvas: theme.bar,
            ..DisplayList::default()
        };
        let style = ui_style(TEXT);

        // A one-pixel edge, drawn as a filled rectangle with a smaller one over
        // it: the list floats over the page and needs a line to say where it
        // stops.
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

        for (index, option) in self.options.iter().take(self.rows()).enumerate() {
            let top = index as f32 * ROW;
            // The row under the pointer, and failing that the row the control
            // is on. Marking the current one matters more here than in a menu:
            // a dropdown is a question that already has an answer, and a list
            // that did not say which was the answer would make the reader
            // choose again to find out.
            if self.hovered == Some(index) || (self.hovered.is_none() && self.on == index) {
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
            let room = rect.width - PADDING * 2.0;
            let label = elided(fonts, option, &style, room);
            draw_text(
                &mut list,
                fonts,
                &label,
                &style,
                PADDING,
                top + ROW / 2.0 - TEXT * 0.7,
                theme.ink,
                room,
            );
        }

        rasterise(
            &list,
            fonts,
            &paint::ImageStore::new(),
            rect.width as u32,
            rect.height as u32,
        )
        .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_(y: f32) -> Rect {
        Rect {
            x: 20.0,
            y,
            width: 100.0,
            height: 20.0,
        }
    }

    fn options() -> Vec<String> {
        ["United Kingdom", "France", "Japan"]
            .map(str::to_owned)
            .to_vec()
    }

    fn open(y: f32, size: (u32, u32)) -> Dropdown {
        let mut fonts = FontStore::new();
        Dropdown::open(box_(y), options(), 1, 7, &mut fonts, size).expect("opens")
    }

    #[test]
    fn a_list_opens_under_the_control_it_belongs_to() {
        let list = open(50.0, (800, 600));
        assert_eq!(list.at.0, 20.0, "lined up with the box");
        assert_eq!(list.at.1, 70.0, "directly below it");
    }

    #[test]
    fn a_list_near_the_bottom_opens_upwards() {
        // Otherwise its last rows are off the screen, which is a list with
        // options nobody can choose.
        let list = open(560.0, (800, 600));
        let rect = list.rect();
        assert!(rect.y + rect.height <= 580.0, "{rect:?}");
        assert!(rect.y >= 0.0, "{rect:?}");
    }

    #[test]
    fn a_list_at_the_right_edge_moves_rather_than_hanging_off_it() {
        let mut fonts = FontStore::new();
        let at_edge = Rect {
            x: 780.0,
            ..box_(50.0)
        };
        let list = Dropdown::open(at_edge, options(), 0, 7, &mut fonts, (800, 600)).expect("opens");
        let rect = list.rect();
        assert!(rect.x + rect.width <= 800.0, "{rect:?}");
    }

    #[test]
    fn a_list_is_at_least_as_wide_as_its_control() {
        let mut fonts = FontStore::new();
        let wide = Rect {
            width: 300.0,
            ..box_(50.0)
        };
        let list = Dropdown::open(wide, vec!["x".to_owned()], 0, 7, &mut fonts, (800, 600))
            .expect("opens");
        assert!(
            list.width >= 300.0,
            "a list narrower than its box looks like it came from somewhere \
             else, got {}",
            list.width,
        );
    }

    #[test]
    fn each_row_answers_for_its_own_band_and_nothing_else() {
        let list = open(50.0, (800, 600));
        let rect = list.rect();
        assert_eq!(list.row_at(rect.x + 5.0, rect.y + 1.0), Some(0));
        assert_eq!(list.row_at(rect.x + 5.0, rect.y + ROW + 1.0), Some(1));
        assert_eq!(list.row_at(rect.x + 5.0, rect.y + ROW * 3.0 + 1.0), None);
        assert_eq!(list.row_at(rect.x - 1.0, rect.y + 1.0), None);
    }

    #[test]
    fn a_long_list_stops_growing_rather_than_leaving_the_window() {
        let mut fonts = FontStore::new();
        let many: Vec<String> = (0..200).map(|n| format!("option {n}")).collect();
        let list = Dropdown::open(box_(50.0), many, 0, 7, &mut fonts, (800, 600)).expect("opens");
        assert_eq!(list.rect().height, MAX_ROWS as f32 * ROW);
        assert_eq!(
            list.row_at(30.0, list.rect().y + MAX_ROWS as f32 * ROW - 1.0),
            Some(MAX_ROWS - 1),
            "the last drawn row is still choosable",
        );
    }

    #[test]
    fn an_empty_select_opens_nothing() {
        let mut fonts = FontStore::new();
        assert!(Dropdown::open(box_(50.0), Vec::new(), 0, 7, &mut fonts, (800, 600)).is_none());
    }
}
