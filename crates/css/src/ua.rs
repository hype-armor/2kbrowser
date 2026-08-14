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

/* Only an `a` with an href is a link. An anchor without one is a named
   destination, which was how in-page navigation worked and which must not be
   painted as though it were clickable. */
a[href] { color: #0000ee; text-decoration: underline; }

del, s, strike { text-decoration: line-through; }

ul, ol, dd { padding-left: 40px; }
li { display: list-item; }

/* A list nested inside a list item takes no vertical margin of its own. The
   sample stylesheet omits this and every browser adds it: without it a
   sublist floats away from the item it belongs to. */
ul ul, ul ol, ol ul, ol ol { margin-top: 0; margin-bottom: 0; }

ul { list-style-type: disc; }
ol { list-style-type: decimal; }
/* Nested unordered lists step through the three bullets, which is what makes
   the levels of a deep list tellable apart. */
ul ul { list-style-type: circle; }
ul ul ul { list-style-type: square; }

blockquote { padding-left: 40px; padding-right: 40px; }

/* Not plain `center`: `<center>` centres block children too, which is how
   `<center><table>` centred a table and is why the element existed. */
center { text-align: -webkit-center; }

/* A rule is an empty block with a border, which is how every browser has
   drawn it. Without one it is a zero-height box that draws nothing, and the
   era's pages used it constantly as a section divider. */
hr { margin: 0.5em 0; height: 0; border-top: 1px solid #999999; }

table { display: table; }
thead, tbody, tfoot { display: table-row-group; }
tr { display: table-row; }
th, td { display: table-cell; padding: 1px; }
th { text-align: center; }
caption { display: block; text-align: center; }

big { font-size: 1.17em; }
small, sub, sup { font-size: 0.83em; }
"#;

/// Styles applied on top of the UA sheet when a document is rendered as a
/// document rather than with the author's layout (ADR-0009).
///
/// Deliberately opinionated and narrow-measure: once the author's layout has
/// been discarded, something has to make the result pleasant, and defaulting to
/// full-window line lengths would trade one unreadable rendering for another.
///
/// Dark, for the same reason it is narrow. This rendering is what a reader
/// falls back to for the pages they mean to sit and read, and a full window of
/// white is the wrong thing to hand them — which is why every reading mode
/// worth using, Firefox's included, offers one. The palette is Firefox's: a
/// near-black that is not quite black, off-white text so the contrast is not
/// the maximum the display can produce, and a pale blue for links, since the
/// UA sheet's `#0000ee` is unreadable against it.
///
/// The colours have to be stated rather than inherited from anywhere: the
/// author's sheets are gone by the time this is applied, so what is left
/// underneath is the UA sheet's black-on-white.
pub const READER_STYLESHEET: &str = r#"
body {
  margin: 40px;
  max-width: 42em;
  line-height: 1.5;
  background-color: #1c1b22;
  color: #fbfbfe;
}
a[href] { color: #8cb4ff; }
img { max-width: 100%; }
hr { border-top: 1px solid #5b5b66; }
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

    /// Perceived brightness, 0 for black and 1 for white.
    ///
    /// The coefficients are Rec. 601's, which is what "is this dark?" wants:
    /// green reads as far brighter than blue at the same number, and comparing
    /// the channels evenly would call a saturated blue light.
    fn brightness(colour: crate::Color) -> f32 {
        (0.299 * f32::from(colour.r) + 0.587 * f32::from(colour.g) + 0.114 * f32::from(colour.b))
            / 255.0
    }

    #[test]
    fn the_reader_sheet_is_dark() {
        let doc = dom::parse("<body><p>text</p></body>");
        let sheets = [Stylesheet::parse(super::READER_STYLESHEET)];
        let map = crate::cascade::cascade(&doc, &sheets);
        let body = map
            .get(doc.find_element("body").expect("a body"))
            .expect("a styled body");

        assert!(
            brightness(body.background_color) < 0.2,
            "the reader background is {:?}, which is not dark",
            body.background_color
        );
        assert!(
            brightness(body.color) > 0.8,
            "the reader text is {:?}, which will not read on a dark page",
            body.color
        );
    }

    #[test]
    fn the_reader_sheet_recolours_links_for_a_dark_page() {
        // The UA sheet's `#0000ee` is very nearly invisible against a near
        // black background, and the reader sheet replaces the author's — so
        // nothing else is going to fix this.
        let doc = dom::parse(r#"<body><p><a href="/next">onwards</a></p></body>"#);
        let plain = crate::cascade::cascade(&doc, &[]);
        let reader = crate::cascade::cascade(&doc, &[Stylesheet::parse(super::READER_STYLESHEET)]);
        let link = doc.find_element("a").expect("a link");

        let default = plain.get(link).expect("a styled link").color;
        let recoloured = reader.get(link).expect("a styled link").color;
        assert_ne!(recoloured, default, "links kept the UA sheet's blue");
        assert!(
            brightness(recoloured) > 0.5,
            "the link colour is {recoloured:?}, which will not read on a dark page"
        );
    }
}
