//! What this site is allowed to load from, and the two words that change it.
//!
//! ADR-0006's third-party rule is a default rather than a prohibition, and the
//! ADR names the per-site override as the reason it is allowed to be as
//! absolute as it is. This is the override (#118): the padlock in the bar opens
//! it, it lists what this page asked for and did not get, and each line can be
//! allowed. Lines already allowed can be taken back from the same place.
//!
//! **Granting and revoking are the same list.** An allow-list you can only add
//! to is one a reader stops being able to reason about, because "I let this
//! through once to see the images" quietly becomes permanent. Both directions
//! being one keypress from the padlock is what makes the exception a decision
//! rather than a ratchet.
//!
//! Scoped to the site, not to the host. Allowing `fonts.example.net` because
//! one page needs it must not hand every other page on the web a host already
//! in the browser's good books — that is a cross-site identifier reassembled
//! by consent, which is the exact thing the rule exists to remove.
//!
//! Drawn into the same buffer as everything else and hit-tested with the same
//! arithmetic, for the reason [`crate::menu`] gives at greater length.

use layout::Rect;
use paint::{DisplayItem, DisplayList, Pixmap, rasterise};
use text::FontStore;

use crate::chrome::{PADDING, Theme, draw_text, elided, measure, ui_style};

/// Height of one row.
const ROW: f32 = 26.0;
/// Height of the heading, which carries no control and needs less room.
const HEAD: f32 = 24.0;
/// How wide the panel is.
const WIDTH: f32 = 300.0;
/// Text size in it.
const TEXT: f32 = 13.0;
/// Text size of the heading and the two group labels.
const SMALL: f32 = 11.0;

/// One line of the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The site all of this is about. Not a control.
    Site(String),
    /// A label over a group. Not a control.
    Group(&'static str),
    /// A host this page asked for and did not get. Pressing it allows this
    /// site to load from that host.
    Allow(String),
    /// A host this site is allowed to load from. Pressing it takes that back.
    Revoke(String),
    /// Nothing was refused and nothing has been allowed. Not a control.
    Nothing,
}

impl Row {
    /// The height this row takes.
    fn height(&self) -> f32 {
        match self {
            Row::Site(_) | Row::Group(_) => HEAD,
            Row::Allow(_) | Row::Revoke(_) | Row::Nothing => ROW,
        }
    }

    /// The word on the right, for a row that does something.
    fn verb(&self) -> Option<&'static str> {
        match self {
            Row::Allow(_) => Some("allow"),
            Row::Revoke(_) => Some("revoke"),
            _ => None,
        }
    }

    /// What the row reads as on the left.
    fn text(&self) -> &str {
        match self {
            Row::Site(site) => site,
            Row::Group(label) => label,
            Row::Allow(host) | Row::Revoke(host) => host,
            Row::Nothing => "nothing was blocked on this page",
        }
    }
}

/// What the panel should hold for a site.
///
/// Refused first, allowed second. The refused group is why the panel was
/// opened — something is missing from the page on screen — and the allowed
/// group is a standing decision the reader made earlier. Putting the standing
/// list first would bury the thing that prompted the visit under it.
pub fn rows_for(site: &str, withheld: &[String], allowed: &[String]) -> Vec<Row> {
    let mut rows = vec![Row::Site(site.to_owned())];
    if !withheld.is_empty() {
        rows.push(Row::Group("blocked on this page"));
        rows.extend(withheld.iter().cloned().map(Row::Allow));
    }
    if !allowed.is_empty() {
        rows.push(Row::Group("allowed on this site"));
        rows.extend(allowed.iter().cloned().map(Row::Revoke));
    }
    if withheld.is_empty() && allowed.is_empty() {
        rows.push(Row::Nothing);
    }
    rows
}

/// The open panel.
#[derive(Debug, Clone)]
pub struct Panel {
    at: (f32, f32),
    /// The lines, top to bottom.
    pub rows: Vec<Row>,
    /// Which row the pointer is over, among those that do something.
    pub hovered: Option<usize>,
}

impl Panel {
    /// Opens one under `at`, kept inside a window of `size`.
    pub fn open(at: (f32, f32), rows: Vec<Row>, size: (u32, u32)) -> Option<Self> {
        if rows.is_empty() {
            return None;
        }
        let height: f32 = rows.iter().map(Row::height).sum();
        let x = at.0.min((size.0 as f32 - WIDTH).max(0.0));
        let y = at.1.min((size.1 as f32 - height).max(0.0));
        Some(Self {
            at: (x, y),
            rows,
            hovered: None,
        })
    }

    /// Where it sits, in window coordinates.
    pub fn rect(&self) -> Rect {
        Rect {
            x: self.at.0,
            y: self.at.1,
            width: WIDTH,
            height: self.rows.iter().map(Row::height).sum(),
        }
    }

    /// Which row a window point is on, whether or not it does anything.
    ///
    /// Rows are not all the same height, so this walks them rather than
    /// dividing — the arithmetic that works for a menu of equal rows is the
    /// arithmetic that puts a click on the wrong line here.
    pub fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        let rect = self.rect();
        if x < rect.x || x >= rect.x + rect.width || y < rect.y || y >= rect.y + rect.height {
            return None;
        }
        let mut top = rect.y;
        for (index, row) in self.rows.iter().enumerate() {
            let bottom = top + row.height();
            if y < bottom {
                return Some(index);
            }
            top = bottom;
        }
        None
    }

    /// The row a click chose, if it chose one that does something.
    pub fn choice_at(&self, x: f32, y: f32) -> Option<&Row> {
        let row = self.rows.get(self.row_at(x, y)?)?;
        row.verb().is_some().then_some(row)
    }

    /// Draws it.
    pub fn render(&self, fonts: &mut FontStore, theme: Theme) -> Pixmap {
        let rect = self.rect();
        let mut list = DisplayList {
            canvas: theme.control_edge,
            ..DisplayList::default()
        };
        // A one-pixel edge: the panel floats over the page and needs a line to
        // say where it stops.
        list.items.push(DisplayItem::Rect {
            rect: Rect {
                x: 1.0,
                y: 1.0,
                width: rect.width - 2.0,
                height: rect.height - 2.0,
            },
            color: theme.bar,
        });

        let mut top = 0.0;
        for (index, row) in self.rows.iter().enumerate() {
            let height = row.height();
            if self.hovered == Some(index) && row.verb().is_some() {
                list.items.push(DisplayItem::Rect {
                    rect: Rect {
                        x: 1.0,
                        y: top + 1.0,
                        width: rect.width - 2.0,
                        height: height - 2.0,
                    },
                    color: theme.control,
                });
            }
            let (size, ink) = match row {
                // Dimmer and smaller: these name what the lines below them are
                // rather than saying anything a reader has to act on.
                Row::Site(_) | Row::Group(_) => (SMALL, theme.dim),
                Row::Nothing => (TEXT, theme.dim),
                Row::Allow(_) | Row::Revoke(_) => (TEXT, theme.ink),
            };
            let style = ui_style(size);
            let verb = row.verb();
            // The verb is reserved its room before the host is measured, so a
            // long hostname is cut rather than pushing the word off the panel.
            // The word is the part that has to survive: a line of the panel
            // that does not say what pressing it does is not a control.
            let verb_width = verb
                .map(|verb| measure(fonts, verb, &ui_style(SMALL)) + PADDING)
                .unwrap_or(0.0);
            let room = rect.width - PADDING * 2.0 - verb_width;
            let text = elided(fonts, row.text(), &style, room);
            draw_text(
                &mut list,
                fonts,
                &text,
                &style,
                PADDING,
                top + height / 2.0 - size * 0.7,
                ink,
                room,
            );
            if let Some(verb) = verb {
                let style = ui_style(SMALL);
                let width = measure(fonts, verb, &style);
                draw_text(
                    &mut list,
                    fonts,
                    verb,
                    &style,
                    rect.width - PADDING - width,
                    top + height / 2.0 - SMALL * 0.7,
                    theme.notice,
                    width,
                );
            }
            top += height;
        }

        rasterise(
            &list,
            fonts,
            &paint::ImageStore::new(),
            rect.width as u32,
            rect.height.max(1.0) as u32,
        )
        .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn what_was_refused_comes_before_what_was_allowed() {
        let rows = rows_for(
            "example.com",
            &hosts(&["ads.example.org"]),
            &hosts(&["cdn.example.net"]),
        );
        assert_eq!(
            rows,
            vec![
                Row::Site("example.com".to_owned()),
                Row::Group("blocked on this page"),
                Row::Allow("ads.example.org".to_owned()),
                Row::Group("allowed on this site"),
                Row::Revoke("cdn.example.net".to_owned()),
            ]
        );
    }

    #[test]
    fn a_page_with_nothing_to_decide_says_so() {
        // Rather than opening an empty box, which reads as a broken panel. The
        // padlock is on every page, so most of the time this is what it opens.
        let rows = rows_for("example.com", &[], &[]);
        assert_eq!(
            rows,
            vec![Row::Site("example.com".to_owned()), Row::Nothing]
        );
        let panel = Panel::open((10.0, 40.0), rows, (900, 600)).expect("opens");
        assert!(
            panel.choice_at(20.0, 60.0).is_none(),
            "a line that explains something is not a line to press"
        );
    }

    #[test]
    fn a_click_lands_on_the_row_it_looks_like() {
        // Rows are not all the same height — a heading is shorter than a
        // control — so dividing by a row height puts a click on the wrong
        // line. This is the arithmetic that caught it.
        let rows = rows_for(
            "example.com",
            &hosts(&["ads.example.org", "beacon.example.com"]),
            &[],
        );
        let panel = Panel::open((0.0, 0.0), rows, (900, 600)).expect("opens");

        // Heading, group label, then the two hosts.
        assert_eq!(panel.row_at(10.0, 1.0), Some(0));
        assert_eq!(panel.row_at(10.0, HEAD + 1.0), Some(1));
        assert_eq!(
            panel.choice_at(10.0, HEAD * 2.0 + 1.0),
            Some(&Row::Allow("ads.example.org".to_owned()))
        );
        assert_eq!(
            panel.choice_at(10.0, HEAD * 2.0 + ROW + 1.0),
            Some(&Row::Allow("beacon.example.com".to_owned())),
            "the second host"
        );
        assert_eq!(
            panel.row_at(10.0, HEAD * 2.0 + ROW * 2.0 + 1.0),
            None,
            "below the panel"
        );
    }

    #[test]
    fn the_panel_stays_inside_the_window() {
        let rows = rows_for("example.com", &hosts(&["a.example", "b.example"]), &[]);
        let panel = Panel::open((880.0, 590.0), rows, (900, 600)).expect("opens");
        let rect = panel.rect();
        assert!(rect.x + rect.width <= 900.0, "{rect:?}");
        assert!(rect.y + rect.height <= 600.0, "{rect:?}");
    }

    #[test]
    fn it_draws_at_the_size_it_claims_in_either_theme() {
        let mut fonts = FontStore::new();
        let rows = rows_for(
            "example.com",
            &hosts(&["ads.example.org"]),
            &hosts(&["cdn.example.net"]),
        );
        let panel = Panel::open((10.0, 40.0), rows, (900, 600)).expect("opens");
        let rect = panel.rect();
        let light = panel.render(&mut fonts, Theme::LIGHT);
        assert_eq!(
            (light.width(), light.height()),
            (WIDTH as u32, rect.height as u32)
        );
        let dark = panel.render(&mut fonts, Theme::DARK);
        assert_ne!(light.data(), dark.data(), "the panel ignores the theme");
    }

    #[test]
    fn every_word_the_panel_draws_exists_in_the_bundled_fonts() {
        // ADR-0008, the same check the bar's own labels get. Hostnames come
        // from a stranger's page and cannot be covered here; the words this
        // module chooses can be, and they are drawn at two sizes no other
        // chrome uses.
        let mut fonts = FontStore::new();
        let rows = rows_for(
            "example.com",
            &hosts(&["a.example"]),
            &hosts(&["b.example"]),
        );
        let mut words: Vec<String> = Vec::new();
        for row in &rows_for("example.com", &[], &[]) {
            words.push(row.text().to_owned());
        }
        for row in &rows {
            words.push(row.text().to_owned());
            if let Some(verb) = row.verb() {
                words.push(verb.to_owned());
            }
        }
        for word in words {
            for size in [TEXT, SMALL] {
                let layout = fonts.layout(&word, &ui_style(size), 10_000.0);
                for line in &layout.lines {
                    for glyph in &line.glyphs {
                        assert_ne!(
                            glyph.glyph_id, 0,
                            "{word:?} at {size}px draws a .notdef box"
                        );
                    }
                }
            }
        }
    }
}
