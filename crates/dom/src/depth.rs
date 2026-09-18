//! Keeping the tree builder's stack of open elements bounded (#179).
//!
//! # The cost
//!
//! Parsing deeply nested markup is quadratic, and the quadratic step is not in
//! this crate. Measured on a release build, doubling the nesting depth
//! quadruples the time:
//!
//! | depth | parse |
//! | --- | --- |
//! | 2,000 | 13 ms |
//! | 4,000 | 42 ms |
//! | 8,000 | 155 ms |
//! | 16,000 | 604 ms |
//!
//! A profile of the 8,000 case puts **96% of every instruction** inside
//! `html5ever::tree_builder::TreeBuilder::in_scope_named::<button_scope>`.
//!
//! That is the spec doing what the spec says. A `<div>` start tag must first
//! "close a `p` element in button scope", and finding out whether there is one
//! means walking the stack of open elements from the top until a scope boundary
//! is reached. `div` is not a boundary for button scope — nor are most
//! elements — so on a document that is nothing but nested `div`s, every single
//! start tag walks the whole stack. N tags, N deep, N² work.
//!
//! Upgrading does not help: 0.40 measures the same to within a millisecond.
//! It is the algorithm, not a bug.
//!
//! # What this does
//!
//! Stops the stack getting deep. [`super::MAX_DEPTH`] already says that markup
//! nested past 512 is flattened rather than represented faithfully (#176) —
//! that decision was made for the *renderer's* stack, and the parser's stack
//! is the same problem one step earlier. So a start tag that would open the
//! 513th level is dropped before the tree builder sees it, and the end tag that
//! matches it is dropped with it, which leaves the token stream balanced.
//!
//! The work becomes linear, because every tag now walks at most 512.
//!
//! # What it costs
//!
//! Elements past the cap stop existing rather than being flattened onto the
//! 512th. Their *content* is unaffected — text and everything else still
//! arrives and is inserted at the deepest level there is — so what disappears
//! is surplus nesting, which the existing cap had already collapsed into a row
//! of empty boxes at one level.
//!
//! Nothing real is near this. The era fixture in this repository is 14 elements
//! deep; the deepest document in the CSS 2.1 test suite is 13. 512 is the
//! number Blink uses for the same job.

use html5ever::LocalName;
use html5ever::tokenizer::TokenSinkResult;
use html5ever::tokenizer::{Tag, TagKind::EndTag, TagKind::StartTag, Token, TokenSink};
use std::cell::RefCell;

/// Elements that never have content and so never open a level.
///
/// Counting one of these as nesting would make the cap arrive early on a page
/// of images, which is a page the era is made of.
fn is_void(name: &LocalName) -> bool {
    matches!(
        &**name,
        "area"
            | "base"
            | "basefont"
            | "bgsound"
            | "br"
            | "col"
            | "embed"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Sits between the tokenizer and the tree builder and refuses to open a level
/// past the cap.
pub(crate) struct CapDepth<Sink> {
    pub(crate) inner: Sink,
    /// How many levels are open, as the tokens have described them.
    ///
    /// The tokenizer's view rather than the tree builder's, which is not the
    /// same thing: the tree builder closes elements the markup never closed,
    /// and this cannot see that. It does not need to. The number only has to be
    /// wrong in the safe direction — too high, never too low — for the cap to
    /// hold, and every way this differs from the builder is a way it counts
    /// more levels than are really open.
    open: RefCell<usize>,
    /// The names of the start tags that were dropped, innermost last.
    ///
    /// So the end tag that matches one is dropped too. By name rather than by
    /// count, because an end tag that matches nothing is ordinary in real
    /// markup and must be passed through to the tree builder to be ignored the
    /// way the spec ignores it — not silently eaten as if it closed something.
    dropped: RefCell<Vec<LocalName>>,
}

impl<Sink> CapDepth<Sink> {
    pub(crate) fn new(inner: Sink) -> Self {
        Self {
            inner,
            open: RefCell::new(0),
            dropped: RefCell::new(Vec::new()),
        }
    }

    /// Whether this start tag opens a level past the cap, recording it if so.
    fn refuse_start(&self, tag: &Tag) -> bool {
        if tag.self_closing || is_void(&tag.name) {
            return false;
        }
        let mut open = self.open.borrow_mut();
        if *open >= super::MAX_DEPTH {
            self.dropped.borrow_mut().push(tag.name.clone());
            return true;
        }
        *open += 1;
        false
    }

    /// Whether this end tag closes one that was dropped.
    fn refuse_end(&self, tag: &Tag) -> bool {
        let mut dropped = self.dropped.borrow_mut();
        if dropped.last() == Some(&tag.name) {
            dropped.pop();
            return true;
        }
        drop(dropped);
        let mut open = self.open.borrow_mut();
        *open = open.saturating_sub(1);
        false
    }
}

impl<Sink: TokenSink> TokenSink for CapDepth<Sink> {
    type Handle = Sink::Handle;

    fn process_token(&self, token: Token, line_number: u64) -> TokenSinkResult<Self::Handle> {
        if let Token::TagToken(tag) = &token {
            let refused = match tag.kind {
                StartTag => self.refuse_start(tag),
                EndTag => self.refuse_end(tag),
            };
            if refused {
                // Swallowed here rather than handed on. `Continue` is what the
                // tree builder answers for a token it has dealt with, and from
                // the tokenizer's point of view this one has been.
                return TokenSinkResult::Continue;
            }
        }
        self.inner.process_token(token, line_number)
    }

    fn end(&self) {
        self.inner.end();
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.inner
            .adjusted_current_node_present_but_not_in_html_namespace()
    }
}

#[cfg(test)]
mod tests {
    use crate::{MAX_DEPTH, parse};

    #[test]
    fn an_ordinary_page_is_untouched() {
        // The first thing this must not do. Nothing real is anywhere near the
        // cap, so a page of ordinary markup has to come out exactly as it did.
        let doc = parse(
            "<!doctype html><html><body><div><p>one <b>two</b> three</p>\
             <ul><li>a</li><li>b</li></ul><img src=x><br></div></body></html>",
        );
        let body = doc.find_element("body").expect("body");
        // No space between the paragraph and the list: `text_content`
        // concatenates what the elements hold rather than inventing
        // separators between them.
        assert_eq!(doc.text_content(body), "one two threeab");
        assert!(doc.find_element("img").is_some(), "a void element vanished");
        assert!(doc.find_element("br").is_some(), "a void element vanished");
    }

    #[test]
    fn a_page_of_images_does_not_count_them_as_nesting() {
        // Void elements never open a level. Counting them would bring the cap
        // down on a page of thumbnails, which is what the era's web is made of.
        let images = "<img src=x>".repeat(MAX_DEPTH * 2);
        let doc = parse(&format!(
            "<!doctype html><body><p>after</p>{images}<p>end</p>"
        ));
        assert!(
            doc.text_content(doc.root()).contains("end"),
            "the text after two thousand images was dropped"
        );
    }

    #[test]
    fn nesting_stops_at_the_cap() {
        let deep = MAX_DEPTH + 200;
        let doc = parse(&format!(
            "<!doctype html><body>{}deep{}</body>",
            "<div>".repeat(deep),
            "</div>".repeat(deep)
        ));
        let divs = doc
            .descendants(doc.root())
            .into_iter()
            .filter(|node| {
                doc.element(*node)
                    .is_some_and(|it| it.local_name() == "div")
            })
            .count();
        assert!(
            divs <= MAX_DEPTH,
            "{divs} divs survived a cap of {MAX_DEPTH}"
        );
    }

    #[test]
    fn what_is_inside_deep_nesting_is_still_shown() {
        // The cap drops surplus *nesting*, not content. A reader whose page
        // happens to be badly nested must still see its words — losing them
        // would be a far worse answer than a slow parse.
        let deep = MAX_DEPTH * 4;
        let doc = parse(&format!(
            "<!doctype html><body>{}the words{}</body>",
            "<div>".repeat(deep),
            "</div>".repeat(deep)
        ));
        assert!(
            doc.text_content(doc.root()).contains("the words"),
            "the text inside deep nesting was lost with the nesting"
        );
    }

    #[test]
    fn markup_after_deep_nesting_still_parses() {
        // The balance check: drop a start tag and its end tag must go too, or
        // everything after the deep part is nested inside something that never
        // closes.
        let deep = MAX_DEPTH * 2;
        let doc = parse(&format!(
            "<!doctype html><body>{}inside{}<p id=after>after</p></body>",
            "<div>".repeat(deep),
            "</div>".repeat(deep)
        ));
        let after = doc
            .descendants(doc.root())
            .into_iter()
            .find(|node| {
                doc.element(*node).and_then(super::super::ElementData::id) == Some("after")
            })
            .expect("the paragraph after the deep nesting");
        assert_eq!(doc.text_content(after), "after");
    }

    #[test]
    fn an_end_tag_that_closes_nothing_is_left_to_the_tree_builder() {
        // Stray end tags are ordinary in real markup and the spec says to
        // ignore them. Eating one here as though it closed a dropped element
        // would put the count out by one for the rest of the document.
        let doc = parse("<!doctype html><body><p>one</p></div></span><p>two</p></body>");
        let text = doc.text_content(doc.root());
        assert!(text.contains("one"), "{text}");
        assert!(text.contains("two"), "{text}");
    }

    #[test]
    fn deep_nesting_costs_time_in_proportion_to_its_size() {
        // The whole of #179, as a property rather than a number. Before the
        // cap, doubling the depth quadrupled the time — 155ms at 8,000 and
        // 604ms at 16,000 — because every start tag walked the whole stack of
        // open elements looking for a `p` that was not there.
        //
        // A ratio rather than a threshold, because a threshold on a shared
        // machine is a flaky test. Quadratic growth shows up as a ratio near
        // four; linear shows up near two. Six is far enough above two to leave
        // room for a loaded CI box and far enough below the real quadratic
        // ratio to catch it coming back.
        use std::time::Instant;

        let build = |depth: usize| {
            format!(
                "<!doctype html><body>{}text{}</body>",
                "<div>".repeat(depth),
                "</div>".repeat(depth)
            )
        };
        // Warmed, so the first allocation of the run is not counted against
        // the smaller of the two.
        let _ = parse(&build(1_000));

        let small = build(8_000);
        let large = build(32_000);
        let at = |html: &str| {
            let started = Instant::now();
            let doc = parse(html);
            std::hint::black_box(&doc);
            started.elapsed().as_secs_f64().max(1e-6)
        };
        let ratio = at(&large) / at(&small);
        assert!(
            ratio < 6.0,
            "four times the depth cost {ratio:.1} times the time, which is the \
             quadratic growth #179 is about coming back"
        );
    }

    #[test]
    fn self_closing_tags_do_not_open_a_level() {
        let tags = "<div/>".repeat(MAX_DEPTH * 2);
        let doc = parse(&format!("<!doctype html><body>{tags}<p>end</p></body>"));
        assert!(doc.text_content(doc.root()).contains("end"));
    }
}
