//! One page, rendered somewhere else.
//!
//! What a tab holds now that rendering happens in a child process (ADR-0012).
//! It owns the [`sandbox::Session`] and answers the handful of questions the
//! window asks of a page: how tall it is, what is under the pointer, where a
//! query matches, and what pixels to blit.
//!
//! Separated from `window.rs` for the reason every other testable piece of the
//! shell is — [`crate::history`], [`crate::tabs`], [`crate::field`]. CI has no
//! display server, so anything left inside the event loop is exercised by hand
//! and by nothing else. Everything here runs against a real child process in
//! `tests/isolation.rs`.

use layout::{Rect, RenderMode};
use sandbox::{Error, Rendered, Renderer};

/// A link, with every rectangle it occupies.
///
/// The grouping comes across the boundary rather than being reconstructed:
/// two different links on a page may lead to the same URL, so grouping by
/// destination here would silently merge them.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// Its rectangles. More than one when it wraps across a line break.
    pub rects: Vec<Rect>,
    /// Where it leads, already absolute.
    pub url: String,
}

impl Link {
    /// The rectangle enclosing all of this link's pieces.
    ///
    /// What scrolling to it uses: bringing the first fragment of a wrapped link
    /// into view can leave the rest of it off screen.
    pub fn bounds(&self) -> Rect {
        let mut bounds = self.rects[0];
        for rect in &self.rects[1..] {
            let right = (bounds.x + bounds.width).max(rect.x + rect.width);
            let bottom = (bounds.y + bounds.height).max(rect.y + rect.height);
            bounds.x = bounds.x.min(rect.x);
            bounds.y = bounds.y.min(rect.y);
            bounds.width = right - bounds.x;
            bounds.height = bottom - bounds.y;
        }
        bounds
    }
}

/// The document a viewport is showing, as the parent knows it.
///
/// Deliberately not much: bytes, a URL, and a viewport. Everything derived from
/// the document — the DOM, the box tree, the text — stays in the child.
#[derive(Debug, Clone)]
pub struct Document {
    /// The undecoded body, as fetched.
    pub body: Vec<u8>,
    /// The `Content-Type` it was served with.
    pub content_type: Option<String>,
    /// Its origin.
    pub origin: net::Origin,
    /// Its path within that origin.
    pub path: String,
}

/// A page held by a live renderer.
pub struct Viewport {
    session: sandbox::Session,
    document: Document,
    page: Rendered,
    force_authored: bool,
}

impl Viewport {
    /// Renders `document` in a fresh child and keeps the child.
    pub fn open(
        renderer: &Renderer,
        document: Document,
        width: u32,
        max_height: u32,
        force_authored: bool,
    ) -> Result<Self, Error> {
        let (session, page) = renderer.open(
            document.body.clone(),
            document.content_type.clone(),
            width,
            max_height,
            Some(document.origin.clone()),
            document.path.clone(),
            force_authored,
        )?;
        Ok(Self {
            session,
            document,
            page,
            force_authored,
        })
    }

    /// The rendered pixels, premultiplied RGBA.
    pub fn pixels(&self) -> &[u8] {
        &self.page.pixels
    }

    /// Canvas width in pixels.
    pub fn width(&self) -> u32 {
        self.page.width
    }

    /// Canvas height in pixels.
    pub fn height(&self) -> u32 {
        self.page.height
    }

    /// Height of the content, which may exceed the canvas.
    pub fn content_height(&self) -> f32 {
        self.page.content_height
    }

    /// How far the page can be scrolled, in pixels of canvas.
    ///
    /// The canvas rather than the content, which are the same number for every
    /// page short enough. Beyond the canvas there is nothing to show — the blit
    /// fills white — so scrolling there is scrolling into a void that looks like
    /// the document ending. Better to stop where the pixels stop and say why.
    pub fn scrollable_height(&self) -> f32 {
        self.page.height as f32
    }

    /// Whether the document is taller than the canvas it was rendered onto.
    ///
    /// The frame bound in [`sandbox::max_canvas_height`] made this reachable:
    /// one canvas covers the whole document so scrolling costs a blit, and one
    /// frame is bounded so a compromised renderer cannot make the parent
    /// allocate without limit. About 20,000 rows at 800 pixels wide.
    ///
    /// Rounded up before comparing, because the canvas is a whole number of
    /// rows and the content height is not — a page 400.5 pixels tall gets a
    /// 401-row canvas and is not truncated.
    pub fn is_truncated(&self) -> bool {
        self.page.content_height.ceil() > self.page.height as f32
    }

    /// The page's title, when it had one.
    pub fn title(&self) -> Option<&str> {
        self.page.title.as_deref()
    }

    /// Whether there is a fallback decision to overrule (ADR-0009).
    pub fn can_toggle_layout(&self) -> bool {
        self.page.can_toggle_layout
    }

    /// Whether the reader is currently overruling the fallback.
    pub fn forcing_authored(&self) -> bool {
        self.force_authored
    }

    /// How the page was rendered.
    ///
    /// Translated back from the wire type rather than shared with it: the
    /// protocol deliberately mirrors `RenderMode` instead of being it, so that
    /// a new variant on one side is a compile error rather than a wire format
    /// that quietly changed shape.
    pub fn mode(&self) -> RenderMode {
        match &self.page.mode {
            sandbox::Mode::Authored => RenderMode::Authored,
            sandbox::Mode::Document { unsupported_share } => RenderMode::Document {
                unsupported_share: *unsupported_share,
            },
            sandbox::Mode::RequiresScripting => RenderMode::RequiresScripting,
        }
    }

    /// Every link, with each one's rectangles kept together, in document order.
    pub fn links(&self) -> Vec<Link> {
        let mut out: Vec<Link> = Vec::new();
        let mut groups: Vec<u32> = Vec::new();
        for link in &self.page.links {
            match groups.iter().position(|group| *group == link.group) {
                Some(at) => out[at].rects.push(link.rect),
                None => {
                    groups.push(link.group);
                    out.push(Link {
                        rects: vec![link.rect],
                        url: link.url.clone(),
                    });
                }
            }
        }
        out
    }

    /// The URL of the link at a point, in canvas coordinates.
    ///
    /// Answered from the rectangles the child sent rather than by asking it.
    /// This runs on every pointer move, and a round trip per mouse motion would
    /// be absurd — but it is also all the parent *can* do, since the box tree it
    /// would hit-test against is on the other side.
    pub fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        // Reverse order: a link drawn later sits on top of one drawn earlier.
        self.page
            .links
            .iter()
            .rev()
            .find(|link| {
                let rect = link.rect;
                x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
            })
            .map(|link| link.url.as_str())
    }

    /// Where `query` appears, asked of the child holding the page.
    pub fn find(&mut self, query: &str) -> Vec<Rect> {
        self.session.find(query).unwrap_or_default()
    }

    /// Re-renders at a new width, in the same child.
    ///
    /// Cheaper than a fresh page — the document is already parsed — and it is
    /// what stops a resize re-fetching every image.
    pub fn resize(&mut self, width: u32, max_height: u32) -> Result<(), Error> {
        self.page = self.session.render(
            self.document.body.clone(),
            self.document.content_type.clone(),
            width,
            max_height,
            Some(self.document.origin.clone()),
            self.document.path.clone(),
            self.force_authored,
        )?;
        Ok(())
    }

    /// Overrules the document fallback, or returns to it (ADR-0009).
    pub fn set_forcing_authored(
        &mut self,
        forcing: bool,
        width: u32,
        max_height: u32,
    ) -> Result<(), Error> {
        self.force_authored = forcing;
        self.resize(width, max_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandbox::message::Link as WireLink;

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// The grouping logic alone, without a child process. The rest of this type
    /// is covered against a real one in `tests/isolation.rs`.
    fn group(links: &[WireLink]) -> Vec<Link> {
        let mut out: Vec<Link> = Vec::new();
        let mut groups: Vec<u32> = Vec::new();
        for link in links {
            match groups.iter().position(|group| *group == link.group) {
                Some(at) => out[at].rects.push(link.rect),
                None => {
                    groups.push(link.group);
                    out.push(Link {
                        rects: vec![link.rect],
                        url: link.url.clone(),
                    });
                }
            }
        }
        out
    }

    fn wire(group: u32, url: &str, at: f32) -> WireLink {
        WireLink {
            rect: rect(at, at, 10.0, 5.0),
            url: url.to_owned(),
            group,
        }
    }

    #[test]
    fn a_wrapped_links_rectangles_stay_together() {
        let grouped = group(&[
            wire(0, "https://example.com/a", 0.0),
            wire(0, "https://example.com/a", 20.0),
            wire(1, "https://example.com/b", 40.0),
        ]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].rects.len(), 2, "one link, two pieces");
        assert_eq!(grouped[1].rects.len(), 1);
    }

    #[test]
    fn two_links_to_the_same_place_stay_separate() {
        // The reason the grouping crosses the boundary instead of being
        // reconstructed here: grouping by URL would merge these, and Tab would
        // skip one of them.
        let grouped = group(&[
            wire(0, "https://example.com/same", 0.0),
            wire(1, "https://example.com/same", 30.0),
        ]);
        assert_eq!(grouped.len(), 2, "{grouped:?}");
    }

    #[test]
    fn grouping_preserves_document_order() {
        let grouped = group(&[
            wire(2, "https://example.com/third", 0.0),
            wire(0, "https://example.com/first", 10.0),
            wire(2, "https://example.com/third", 20.0),
        ]);
        // Order of first appearance, which is the order the child sent them in
        // and therefore document order.
        assert_eq!(grouped[0].url, "https://example.com/third");
        assert_eq!(grouped[1].url, "https://example.com/first");
    }

    #[test]
    fn bounds_enclose_every_piece() {
        let link = Link {
            rects: vec![rect(10.0, 0.0, 20.0, 5.0), rect(0.0, 10.0, 15.0, 5.0)],
            url: "https://example.com/".to_owned(),
        };
        let bounds = link.bounds();
        assert_eq!((bounds.x, bounds.y), (0.0, 0.0));
        assert_eq!((bounds.width, bounds.height), (30.0, 15.0));
    }
}
