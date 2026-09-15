//! Deciding what bytes a page is written in.
//!
//! Not a footnote for the era this browser targets. Pages of the 2000s
//! routinely declare no encoding, or declare one and serve another, and the
//! consequence is not a subtle difference: every accented letter, curly quote,
//! and dash becomes a replacement character. Treating everything as UTF-8 —
//! which is what this did before — is wrong for most of the surviving old web.
//!
//! The order below is the HTML standard's, minus the parts that need a running
//! parser: byte-order mark, then the transport's declaration, then the
//! document's own `<meta>`, then a default.

pub use encoding_rs::Encoding;
use encoding_rs::WINDOWS_1252;

/// What a document is assumed to be in when nothing says otherwise.
///
/// windows-1252, not UTF-8: see `detect`. Exposed because a stylesheet's
/// encoding falls back to its referring document's, and a caller that has lost
/// the document's bytes has only this to offer.
pub const ERA_DEFAULT: &Encoding = WINDOWS_1252;

/// Resolves an encoding label, as a `charset` attribute carries one.
pub fn for_label(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(label.trim().as_bytes())
}

/// How far into a document a `<meta>` declaration is looked for.
///
/// The HTML standard's prescan limit. A declaration after this much markup is
/// past the point where a browser could have acted on it without re-parsing,
/// so it is ignored by everyone and pages do not rely on it.
const PRESCAN_LIMIT: usize = 1024;

/// Where an encoding came from, in decreasing order of authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingSource {
    /// A byte-order mark, which outranks every declaration.
    ByteOrderMark,
    /// The transport's `Content-Type` header.
    Transport,
    /// A `<meta>` element in the document itself.
    Document,
    /// Nothing said, so the era's default was assumed.
    Default,
}

/// Decodes a document body, deciding its encoding first.
///
/// Returns the text, the encoding used, and where that decision came from.
/// Decoding never fails: an undecodable byte becomes a replacement character,
/// because a page with one bad byte should still be readable.
pub fn decode_document(
    bytes: &[u8],
    content_type: Option<&str>,
) -> (String, &'static Encoding, EncodingSource) {
    let (encoding, source) = detect(bytes, content_type);
    // The BOM is consumed rather than decoded, so it does not appear as a
    // zero-width space at the top of every page.
    let (text, _, _) = encoding.decode(bytes);
    (text.into_owned(), encoding, source)
}

/// Decides which encoding a document is in, without decoding it.
pub fn detect(bytes: &[u8], content_type: Option<&str>) -> (&'static Encoding, EncodingSource) {
    // A byte-order mark is authoritative: it is the one declaration that
    // cannot be a stale copy-paste, because it is in the bytes themselves.
    if let Some((encoding, _)) = Encoding::for_bom(bytes) {
        return (encoding, EncodingSource::ByteOrderMark);
    }
    if let Some(encoding) = content_type.and_then(charset_from_content_type) {
        return (encoding, EncodingSource::Transport);
    }
    if let Some(encoding) = prescan(bytes) {
        return (encoding, EncodingSource::Document);
    }
    // windows-1252, not ISO-8859-1 and not UTF-8. The encoding standard maps
    // `iso-8859-1` onto windows-1252 precisely because so many pages declared
    // the former while using the latter's curly quotes and dashes; the same
    // reasoning makes it the right guess when nothing is declared at all.
    (WINDOWS_1252, EncodingSource::Default)
}

/// Decodes an external stylesheet, deciding its encoding first.
///
/// §4.4 gives a different order from a document's, and the difference is not
/// cosmetic. A stylesheet has no `<meta>` and no locale to fall back on, so the
/// declarations it does carry are the whole of what a browser has:
///
/// 1. the transport's `charset` parameter;
/// 2. a byte-order mark, or the `@charset` rule at the very start;
/// 3. what the *link* said — `<link charset>` or `@import`'s optional charset;
/// 4. the encoding of the document that referred to it;
/// 5. UTF-8.
///
/// Note the last one: an unlabelled stylesheet is UTF-8 and not windows-1252,
/// which is the reverse of a document's default and is what §4.4 says. The
/// referring document's encoding comes first, though, so a legacy page's
/// stylesheet still decodes as the page does.
pub fn decode_stylesheet(
    bytes: &[u8],
    content_type: Option<&str>,
    linked: Option<&str>,
    referrer: Option<&'static Encoding>,
) -> (String, &'static Encoding) {
    let encoding = content_type
        .and_then(charset_from_content_type)
        .or_else(|| Encoding::for_bom(bytes).map(|(encoding, _)| encoding))
        .or_else(|| at_charset(bytes))
        .or_else(|| linked.and_then(|label| Encoding::for_label(label.trim().as_bytes())))
        .or(referrer)
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    (text.into_owned(), encoding)
}

/// The `@charset` rule at the very start of a stylesheet.
///
/// Only at byte zero and only in the one spelling §4.4 allows —
/// `@charset "…";` with a single space and no comment before it. A rule
/// anywhere else is not a `@charset` rule at all, which is what keeps a
/// `content: "@charset \"utf-8\";"` inside a stylesheet from redecoding it.
///
/// Read as ASCII, which is safe for every encoding this can select: the ones
/// where `@charset` would not be spelled in ASCII bytes cannot be reached from
/// an ASCII rule in the first place.
fn at_charset(bytes: &[u8]) -> Option<&'static Encoding> {
    let rest = bytes.strip_prefix(b"@charset \"")?;
    let end = rest.iter().position(|&b| b == b'"')?;
    if rest.get(end + 1) != Some(&b';') {
        return None;
    }
    Encoding::for_label(&rest[..end])
}

/// Reads `charset=` out of a `Content-Type` value.
pub fn charset_from_content_type(value: &str) -> Option<&'static Encoding> {
    // Deliberately not a full media-type parser: everything before the first
    // `charset=` is a type and parameters we have no use for.
    let lower = value.to_ascii_lowercase();
    let at = lower.find("charset")?;
    let rest = value[at + "charset".len()..].trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let label: String = rest
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| !matches!(c, '"' | '\'' | ';' | ' ' | '\t'))
        .collect();
    Encoding::for_label(label.as_bytes())
}

/// Looks for a `<meta>` encoding declaration near the top of a document.
///
/// Both forms are in scope: the HTML5 `<meta charset>` and the older
/// `<meta http-equiv="Content-Type" content="…; charset=…">`, which is the one
/// the era's pages actually carry.
fn prescan(bytes: &[u8]) -> Option<&'static Encoding> {
    let window = &bytes[..bytes.len().min(PRESCAN_LIMIT)];
    // ASCII-lossy is safe for the prescan: the markup being searched for is
    // ASCII, and mangling the high bytes around it cannot invent a `<meta`.
    let text: String = window.iter().map(|&b| b as char).collect();
    let lower = text.to_ascii_lowercase();

    let mut at = 0usize;
    while let Some(found) = lower[at..].find("<meta") {
        let start = at + found;
        let end = lower[start..]
            .find('>')
            .map(|offset| start + offset)
            .unwrap_or(lower.len());
        let tag = &text[start..end];

        if let Some(encoding) = attribute(tag, "charset").and_then(|value| {
            Encoding::for_label(value.trim().trim_matches(['"', '\'']).as_bytes())
        }) {
            return Some(encoding);
        }
        // The `http-equiv` form. The `content` attribute holds a whole media
        // type, so it goes through the same reader as the real header.
        if attribute(tag, "http-equiv").is_some_and(|value| {
            value
                .trim()
                .trim_matches(['"', '\''])
                .eq_ignore_ascii_case("content-type")
        }) && let Some(content) = attribute(tag, "content")
            && let Some(encoding) = charset_from_content_type(content)
        {
            return Some(encoding);
        }

        at = end.max(start + 1);
    }
    None
}

/// Reads one attribute's value out of a tag's source text.
///
/// A small reader rather than a real tokenizer: this runs before parsing by
/// definition, on at most a kilobyte, looking for two known attribute names.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut at = 0usize;
    while let Some(found) = lower[at..].find(name) {
        let start = at + found;
        at = start + name.len();
        // Must be a whole attribute name, not the tail of another one.
        let before_ok = start == 0
            || lower.as_bytes()[start - 1].is_ascii_whitespace()
            || lower.as_bytes()[start - 1] == b'<';
        if !before_ok {
            continue;
        }
        let rest = tag[at..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(quote @ ('"' | '\'')) => rest[1..].split(quote).next().unwrap_or(""),
            _ => rest
                .split(|c: char| c.is_ascii_whitespace() || c == '>')
                .next()
                .unwrap_or(""),
        };
        return Some(value);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::UTF_8;

    #[test]
    fn a_stylesheet_reads_its_own_charset_rule() {
        // §4.4's second step, and the only declaration most stylesheets carry.
        let (text, encoding) = decode_stylesheet(
            b"@charset \"iso-8859-7\";\n.a { content: \"\xe1\" }",
            None,
            None,
            None,
        );
        assert_eq!(encoding.name(), "ISO-8859-7");
        assert!(
            text.contains('\u{3b1}'),
            "greek alpha, not a mangled byte: {text}"
        );
    }

    #[test]
    fn a_charset_rule_only_counts_at_the_very_start() {
        // §4.4 allows one spelling in one place. Anywhere else it is an
        // ordinary rule, and reading it would let a stylesheet redecode itself
        // from inside a string.
        for sheet in [
            &b"/* a comment first */ @charset \"utf-8\";"[..],
            b"\n@charset \"utf-8\";",
            b"@charset 'utf-8';",
            b"@charset  \"utf-8\";",
            b"@charset \"utf-8\"",
            b"a { content: \"@charset \\\"utf-8\\\";\" }",
        ] {
            assert_eq!(at_charset(sheet), None, "{:?}", sheet);
        }
    }

    #[test]
    fn a_stylesheet_falls_back_to_the_document_and_then_to_utf8() {
        // §4.4's fourth and fifth steps. The order matters both ways: a legacy
        // page's unlabelled stylesheet must not be read as UTF-8, and a
        // stylesheet with no referrer at all must not be read as the era's
        // default.
        let (_, encoding) = decode_stylesheet(b".a { color: red }", None, None, Some(WINDOWS_1252));
        assert_eq!(encoding, WINDOWS_1252);
        let (_, encoding) = decode_stylesheet(b".a { color: red }", None, None, None);
        assert_eq!(encoding, UTF_8);
    }

    #[test]
    fn the_transport_outranks_the_charset_rule_and_the_rule_outranks_the_link() {
        let sheet = b"@charset \"iso-8859-7\"; .a { color: red }";
        let (_, encoding) =
            decode_stylesheet(sheet, Some("text/css; charset=utf-8"), Some("koi8-r"), None);
        assert_eq!(encoding, UTF_8, "the header wins");
        let (_, encoding) = decode_stylesheet(sheet, None, Some("koi8-r"), None);
        assert_eq!(encoding.name(), "ISO-8859-7", "the rule beats the link");
        let (_, encoding) = decode_stylesheet(
            b".a { color: red }",
            None,
            Some("koi8-r"),
            Some(WINDOWS_1252),
        );
        assert_eq!(encoding.name(), "KOI8-R", "the link beats the document");
    }

    #[test]
    fn a_byte_order_mark_outranks_everything() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"<meta charset="shift_jis">"#);
        let (encoding, source) = detect(&bytes, Some("text/html; charset=iso-8859-1"));
        assert_eq!(encoding, UTF_8);
        assert_eq!(source, EncodingSource::ByteOrderMark);
    }

    #[test]
    fn the_transport_beats_the_document() {
        // A server that says one thing while the markup says another is
        // usually a stale `<meta>` left behind by a template.
        let bytes = br#"<html><head><meta charset="iso-8859-1"></head>"#;
        let (encoding, source) = detect(bytes, Some("text/html; charset=utf-8"));
        assert_eq!(encoding, UTF_8);
        assert_eq!(source, EncodingSource::Transport);
    }

    #[test]
    fn a_meta_charset_is_read() {
        let bytes = br#"<html><head><meta charset="utf-8"><title>x</title>"#;
        let (encoding, source) = detect(bytes, None);
        assert_eq!(encoding, UTF_8);
        assert_eq!(source, EncodingSource::Document);
    }

    #[test]
    fn the_http_equiv_form_is_read() {
        // The form the era's pages actually carry.
        let bytes = br#"<html><head><meta http-equiv="Content-Type"
                        content="text/html; charset=windows-1251">"#;
        let (encoding, source) = detect(bytes, None);
        assert_eq!(encoding.name(), "windows-1251");
        assert_eq!(source, EncodingSource::Document);
    }

    #[test]
    fn a_declaration_past_the_prescan_limit_is_ignored() {
        // Nobody could act on it without re-parsing, so no page relies on it.
        let mut bytes = vec![b' '; PRESCAN_LIMIT + 10];
        bytes.extend_from_slice(br#"<meta charset="utf-8">"#);
        assert_eq!(detect(&bytes, None).1, EncodingSource::Default);
    }

    #[test]
    fn nothing_declared_means_windows_1252() {
        let (encoding, source) = detect(b"<html><body>plain</body>", None);
        assert_eq!(encoding, WINDOWS_1252);
        assert_eq!(source, EncodingSource::Default);
    }

    #[test]
    fn iso_8859_1_is_decoded_as_windows_1252() {
        // The label is a lie almost everywhere it appears: pages declaring it
        // use the curly quotes and dashes that only windows-1252 has. The
        // encoding standard maps one onto the other for exactly this reason.
        let (encoding, _) = detect(b"<html>", Some("text/html; charset=iso-8859-1"));
        assert_eq!(encoding, WINDOWS_1252);

        // 0x93 and 0x94 are curly quotes there, and unassigned in ISO-8859-1.
        let (text, _, _) =
            decode_document(b"\x93quoted\x94", Some("text/html; charset=iso-8859-1"));
        assert_eq!(text, "\u{201c}quoted\u{201d}");
    }

    #[test]
    fn a_bad_byte_does_not_lose_the_page() {
        // 0xC0 alone is not valid UTF-8. The page still has to render.
        let (text, _, _) = decode_document(b"ok \xC0 still here", Some("text/html; charset=utf-8"));
        assert!(text.contains("ok"), "got {text:?}");
        assert!(text.contains("still here"), "got {text:?}");
    }

    #[test]
    fn a_byte_order_mark_is_not_left_in_the_text() {
        // Otherwise it turns up as a zero-width space at the top of the page,
        // and in quirks mode as a stray character before the doctype.
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"<html>");
        let (text, _, _) = decode_document(&bytes, None);
        assert_eq!(text, "<html>");
    }

    #[test]
    fn charset_is_read_out_of_a_messy_content_type() {
        for value in [
            "text/html; charset=utf-8",
            "text/html;charset=UTF-8",
            "text/html; charset = \"utf-8\"",
            "text/html; charset=utf-8; boundary=x",
        ] {
            assert_eq!(
                charset_from_content_type(value),
                Some(UTF_8),
                "failed on {value:?}"
            );
        }
        assert_eq!(charset_from_content_type("text/html"), None);
        assert_eq!(
            charset_from_content_type("text/html; charset=nonsense"),
            None
        );
    }

    #[test]
    fn an_attribute_name_must_stand_alone() {
        // `data-charset` is not `charset`, and reading it as one would take
        // the encoding from an attribute that has nothing to do with it.
        assert_eq!(
            detect(br#"<meta data-charset="utf-8">"#, None).1,
            EncodingSource::Default
        );
    }
}
