//! The user-agent stylesheet.
//!
//! This is what makes unstyled HTML readable: headings bold and large,
//! paragraphs separated, lists indented. Modelled on the CSS 2.1 sample
//! stylesheet in Appendix D, which is what the era's browsers converged on and
//! therefore what pages of that era were authored against.

/// The default stylesheet, applied beneath every author rule.
pub const UA_STYLESHEET: &str = r#"
html, body, div, p, h1, h2, h3, h4, h5, h6, blockquote, pre,
ul, ol, li, dl, dt, dd, address, article, aside, footer, header,
main, nav, section, figure, figcaption, hr, form, fieldset, table {
  display: block;
}

head, script, style, title, meta, link, base, noscript { display: none; }

body { margin: 8px; line-height: 1.2; }

p, blockquote, dl, ul, ol, form, pre, figure { margin: 1em 0; }

h1 { font-size: 2em;    font-weight: bold; margin: 0.67em 0; }
h2 { font-size: 1.5em;  font-weight: bold; margin: 0.83em 0; }
h3 { font-size: 1.17em; font-weight: bold; margin: 1em 0; }
h4 { font-size: 1em;    font-weight: bold; margin: 1.33em 0; }
h5 { font-size: 0.83em; font-weight: bold; margin: 1.67em 0; }
h6 { font-size: 0.67em; font-weight: bold; margin: 2.33em 0; }

b, strong, th { font-weight: bold; }
i, em, cite, var, address, dfn { font-style: italic; }
u, ins { text-decoration: underline; }

pre, code, kbd, samp, tt { font-family: monospace; }
pre { white-space: pre; margin: 1em 0; }

a { color: #0000ee; }

ul, ol, dd { padding-left: 40px; }
li { display: list-item; }

blockquote { padding-left: 40px; padding-right: 40px; }

center { text-align: center; }
hr { margin: 0.5em 0; }

table { display: table; }
th, td { padding: 1px; }

big { font-size: 1.17em; }
small, sub, sup { font-size: 0.83em; }
"#;

/// Styles applied on top of the UA sheet when a document is rendered as a
/// document rather than with the author's layout (ADR-0009).
///
/// Deliberately opinionated and narrow-measure: once the author's layout has
/// been discarded, something has to make the result pleasant, and defaulting to
/// full-window line lengths would trade one unreadable rendering for another.
pub const READER_STYLESHEET: &str = r#"
body { margin: 40px; max-width: 42em; line-height: 1.5; }
"#;

#[cfg(test)]
mod tests {
    use crate::Stylesheet;

    #[test]
    fn ua_stylesheet_parses_completely() {
        let sheet = Stylesheet::parse(super::UA_STYLESHEET);
        // A silent parse failure here would strip default styling from every
        // page, so assert the sheet is substantial rather than merely non-empty.
        assert!(
            sheet.rules.len() > 20,
            "parsed only {} rules",
            sheet.rules.len()
        );
        assert!(sheet.rules.iter().all(|r| !r.selectors.is_empty()));
        assert!(sheet.rules.iter().all(|r| !r.declarations.is_empty()));
    }

    #[test]
    fn reader_stylesheet_parses() {
        assert!(!Stylesheet::parse(super::READER_STYLESHEET).rules.is_empty());
    }
}
