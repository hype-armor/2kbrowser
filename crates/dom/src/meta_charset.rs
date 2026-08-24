//! Working around an out-of-bounds index in `html5ever`'s meta-charset scan.
//!
//! # The bug
//!
//! `html5ever` 0.39.0's implementation of the spec's [algorithm for extracting
//! a character encoding from a meta element][spec] runs a cursor forward
//! through the value of a `<meta http-equiv=content-type>` element's `content`
//! attribute, looking for `charset` followed by `=`. Step 4 reads the byte
//! after the word and any whitespace:
//!
//! ```text
//! if input.as_bytes()[position] == b'=' {
//! ```
//!
//! Nothing has established that there *is* a byte there. When the value ends
//! with `charset`, optionally followed by ASCII whitespace, the cursor is
//! sitting exactly one past the end and that line panics. Reduced all the way
//! down, this is the whole document:
//!
//! ```text
//! <meta http-equiv=content-type content=charset>
//! ```
//!
//! That is forty-five bytes any web site can serve, and it takes the renderer
//! down. The fuzzer found it as a mutation of the reference fixtures' own
//! `<meta http-equiv="Content-Type" content="text/html; charset=iso-8859-1">`
//! with the `=` turned into a `"`, which ends the attribute value early and
//! leaves it terminating in `charset` — a one-byte typo away from a real page.
//!
//! # Why the fix is here rather than upstream
//!
//! It *is* upstream. `servo/html5ever`'s `main` reads
//! `if *input.as_bytes().get(position)? == b'=' {`, which returns "no encoding
//! found" instead of indexing. It has not been released: 0.39.0 is the newest
//! version on crates.io and it is the one with the bug. So there is nothing to
//! upgrade to, and this stands in until there is.
//!
//! Two things that would look like easier answers are not answers.
//!
//! Catching the panic does not work. `[profile.release]` sets
//! `panic = "abort"`, so in the browser people actually run there is no unwind
//! to catch — a `catch_unwind` here would pass the tests, pass the fuzzer, and
//! do nothing whatsoever in the shipped binary. That is a worse state than the
//! bug, because it looks fixed.
//!
//! Pointing `[patch.crates.io]` at upstream's `main` does work, and brings
//! every other unreleased change to the parser with it. The reference tests
//! compare rendered output byte for byte against a shared baseline (ADR-0005);
//! taking an unpinned snapshot of somebody's development branch to fix a
//! one-line bounds check is a large, untargeted change to what this browser
//! draws.
//!
//! # What this does instead
//!
//! `html5ever` lets the tokenizer and the tree builder be assembled by hand,
//! and they are two separate objects with a `TokenSink` between them. The bug
//! is in the tree builder; the tokenizer is fine. So this sits in the join and
//! looks at `<meta>` start tags on the way past.
//!
//! When a `content` value would send that cursor off the end, one `=` is
//! appended to it. The scan then stops on the `=` exactly as step 4 intends,
//! finds nothing after it, and returns "no encoding found" — which is, byte for
//! byte, what the fixed upstream returns for the same input. Every other token
//! is forwarded untouched.
//!
//! The cost is one attribute value, on a malformed `<meta>` element, gaining a
//! character that no part of this browser reads: the encoding was already
//! decided from the response bytes before the parser ran
//! (`net::encoding::decode_document`), `<meta>` is not rendered, and there is
//! no script engine to observe the difference (ADR-0003).
//!
//! # Removing it
//!
//! When `html5ever` releases a version containing the fix, delete this file,
//! put [`crate::parse`] back to `parse_document(sink, opts).one(html)`, and
//! keep `defuses_a_meta_charset_that_would_index_past_the_end` — the bug is
//! gone but the input that found it is still worth parsing in a test.
//!
//! [spec]: https://html.spec.whatwg.org/multipage/#algorithm-for-extracting-a-character-encoding-from-a-meta-element

use html5ever::tokenizer::{StartTag, Tag, Token, TokenSink, TokenSinkResult};
use html5ever::{LocalName, local_name, ns};

/// Sits between the tokenizer and the tree builder and defuses the one token
/// shape that makes the tree builder index past the end of a string.
pub(crate) struct DefuseMetaCharset<Sink>(pub(crate) Sink);

impl<Sink: TokenSink> TokenSink for DefuseMetaCharset<Sink> {
    type Handle = Sink::Handle;

    fn process_token(&self, mut token: Token, line_number: u64) -> TokenSinkResult<Self::Handle> {
        // Only `<meta>` start tags can reach the scan, so everything else costs
        // one discriminant check. A document's tokens are mostly characters.
        // `local_name!` interns at compile time, so these are pointer
        // comparisons rather than string ones — the same way the tree builder
        // matches its own tags.
        if let Token::TagToken(tag) = &mut token
            && tag.kind == StartTag
            && tag.name == local_name!("meta")
        {
            // Every condition the tree builder applies before it reaches the
            // scan, in its order, so nothing is rewritten on an element that
            // would never have been scanned. In particular a `charset`
            // attribute wins outright and the `content` branch is an `else if`
            // — `<meta charset=utf-8 http-equiv=content-type content=charset>`
            // is not a trap, and must come out the way it went in.
            let scanned = first(tag, local_name!("charset")).is_none()
                && first(tag, local_name!("http-equiv"))
                    .is_some_and(|at| tag.attrs[at].value.eq_ignore_ascii_case("content-type"));
            if scanned
                && let Some(at) = first(tag, local_name!("content"))
                && runs_off_the_end(&tag.attrs[at].value)
            {
                tag.attrs[at].value.push_char('=');
            }
        }
        self.0.process_token(token, line_number)
    }

    fn end(&self) {
        self.0.end();
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.0
            .adjusted_current_node_present_but_not_in_html_namespace()
    }
}

/// Where `tag`'s first attribute with this name is, matched the way
/// `html5ever`'s own `Tag::get_attribute` matches: the null namespace, and the
/// first one wins if a malformed tag somehow carries two.
fn first(tag: &Tag, name: LocalName) -> Option<usize> {
    tag.attrs
        .iter()
        .position(|attr| attr.name.ns == ns!() && attr.name.local == name)
}

/// Whether the upstream scan would walk `value`'s cursor past its last byte.
///
/// A transcription of `extract_a_character_encoding_from_a_meta_element` with
/// the indexing replaced by a `get`, kept deliberately close to the shape of
/// the original so the two can be read side by side. It answers the question
/// the original does not ask; it does not try to be an encoding parser.
///
/// The cursor advances by at least the length of `charset` on every turn of the
/// outer loop, so this terminates on any input.
fn runs_off_the_end(value: &str) -> bool {
    const CHARSET: &[u8] = b"charset";

    let bytes = value.as_bytes();
    let mut position = 0;
    loop {
        // Step 2. Find the next case-insensitive `charset`. Running out of
        // input here is the case upstream already handles, with a `?`.
        loop {
            let Some(candidate) = bytes.get(position..position + CHARSET.len()) else {
                return false;
            };
            if candidate.eq_ignore_ascii_case(CHARSET) {
                break;
            }
            position += 1;
        }
        position += CHARSET.len();

        // Step 3. Skip the whitespace after it, if any.
        position += bytes[position..]
            .iter()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count();

        // Step 4. Look at what follows. Upstream indexes here.
        match bytes.get(position) {
            // Nothing follows. This is the panic.
            None => return true,
            // An encoding is being named; the scan stops and reads it.
            Some(b'=') => return false,
            // Some other `charset` — a word in a MIME type, say. The scan goes
            // round again from where it got to.
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_ending_in_the_word_is_the_trap() {
        assert!(runs_off_the_end("charset"));
        assert!(runs_off_the_end("text/html; charset"));
        assert!(runs_off_the_end("CharSet"), "the match is case-insensitive");
        // Step 3 skips trailing whitespace and lands one past the end just the
        // same, which is the second way in and easy to miss.
        assert!(runs_off_the_end("text/html; charset \t\r\n"));
    }

    #[test]
    fn an_ordinary_content_type_is_not() {
        assert!(!runs_off_the_end("text/html; charset=utf-8"));
        assert!(!runs_off_the_end("text/html; charset = utf-8"));
        assert!(!runs_off_the_end("text/html"));
        assert!(!runs_off_the_end(""));
        // Truncated to less than the word itself: upstream's `?` catches this.
        assert!(!runs_off_the_end("chars"));
    }

    #[test]
    fn the_scan_keeps_going_past_a_charset_that_names_nothing() {
        // The outer loop. A first `charset` followed by something other than
        // `=` does not end the scan, so a later one still decides the answer.
        assert!(!runs_off_the_end("charset/x charset=utf-8"));
        assert!(runs_off_the_end("charset/x charset"));
        // Overlapping and adjacent copies, which is where an implementation
        // that failed to advance would spin rather than answer.
        assert!(runs_off_the_end("charsetcharsetcharset"));
    }

    #[test]
    fn appending_an_equals_is_what_makes_the_trap_safe() {
        // The property the whole workaround rests on: whatever the trap was,
        // one `=` on the end is enough, and the result is not a new trap.
        for trap in [
            "charset",
            "text/html; charset",
            "text/html; charset \t",
            "charsetcharset",
        ] {
            assert!(runs_off_the_end(trap), "{trap:?} was supposed to be a trap");
            assert!(
                !runs_off_the_end(&format!("{trap}=")),
                "{trap:?} was still a trap after the fix"
            );
        }
    }
}
