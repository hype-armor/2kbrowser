//! Box tree construction and block layout.
//!
//! Block boxes stack vertically, each laying its inline content out as an
//! inline formatting context: differently-styled spans share line boxes and
//! break as one paragraph. Floats, tables, and positioning are the rest of M2
//! (ADR-0004).

pub mod classify;

use css::cascade::StyleMap;
use css::style::{ComputedStyle, Display, TextAlign, WhiteSpace};
use css::value::Length;
use dom::{Document, NodeId};
use text::{FontStore, InlineRun, TextLayout};

pub use classify::{RenderMode, classify};

/// A rectangle in CSS pixels, with the origin at the top left of the canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

/// A laid-out box.
#[derive(Debug, Clone)]
pub struct LayoutBox {
    /// Border-box geometry.
    pub rect: Rect,
    /// The style this box was laid out with.
    pub style: ComputedStyle,
    /// Inline content, already wrapped, positioned relative to the content box.
    pub text: Option<TextLayout>,
    /// Offset of the content box within `rect`.
    pub content_origin: (f32, f32),
    /// Child boxes.
    pub children: Vec<LayoutBox>,
}

impl LayoutBox {
    /// Total height of this box including its margins.
    fn outer_height(&self, font_size: f32) -> f32 {
        self.rect.height
            + self.style.margin.top.to_px(font_size, 0.0)
            + self.style.margin.bottom.to_px(font_size, 0.0)
    }
}

/// The result of laying out a document.
#[derive(Debug, Clone)]
pub struct Layout {
    /// Root box, covering the whole canvas.
    pub root: LayoutBox,
    /// Total content height, which may exceed the viewport.
    pub height: f32,
}

/// Lays out a styled document at a given viewport width.
pub fn layout(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    viewport_width: f32,
) -> Layout {
    let body = doc.find_element("body").unwrap_or_else(|| doc.root());
    let body_style = styles.get(body).cloned().unwrap_or_default();

    let mut root = LayoutBox {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: viewport_width,
            height: 0.0,
        },
        style: body_style.clone(),
        text: None,
        content_origin: (0.0, 0.0),
        children: Vec::new(),
    };

    let height = layout_block(
        doc,
        styles,
        fonts,
        body,
        &body_style,
        0.0,
        0.0,
        viewport_width,
        &mut root,
    );
    root.rect.height = height;
    Layout { root, height }
}

/// Lays out `node` as a block box at `(x, y)` within `available_width`,
/// appending it to `parent`. Returns the outer height consumed.
#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn layout_block(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    node: NodeId,
    style: &ComputedStyle,
    x: f32,
    y: f32,
    available_width: f32,
    parent: &mut LayoutBox,
) -> f32 {
    let font_size = style.font_size;
    let margin_left = style.margin.left.to_px(font_size, available_width);
    let margin_right = style.margin.right.to_px(font_size, available_width);
    let margin_top = style.margin.top.to_px(font_size, available_width);
    let padding_left = style.padding.left.to_px(font_size, available_width);
    let padding_right = style.padding.right.to_px(font_size, available_width);
    let padding_top = style.padding.top.to_px(font_size, available_width);
    let padding_bottom = style.padding.bottom.to_px(font_size, available_width);

    let border_width = match style.width {
        Length::Auto => (available_width - margin_left - margin_right).max(0.0),
        length => length.to_px(font_size, available_width) + padding_left + padding_right,
    };
    let content_width = (border_width - padding_left - padding_right).max(0.0);

    let mut box_ = LayoutBox {
        rect: Rect {
            x: x + margin_left,
            y: y + margin_top,
            width: border_width,
            height: 0.0,
        },
        style: style.clone(),
        text: None,
        content_origin: (padding_left, padding_top),
        children: Vec::new(),
    };

    // Inline children become styled runs shaped as one paragraph, so a <b> or
    // <code> inside this block keeps its own style while still breaking lines
    // with the text around it.
    let runs = collect_inline_runs(doc, styles, node, style);
    let mut content_height = 0.0;

    if runs.iter().any(|run| !run.text.trim().is_empty()) {
        let layout = fonts.layout_runs(&runs, style, content_width);
        content_height = layout.height;
        box_.text = Some(layout);
    }

    let mut cursor_y = padding_top + content_height;
    for &child in doc.children(node) {
        let Some(child_style) = styles.get(child) else {
            continue;
        };
        if child_style.display == Display::None || child_style.display.is_inline() {
            continue;
        }
        let consumed = layout_block(
            doc,
            styles,
            fonts,
            child,
            child_style,
            padding_left,
            cursor_y,
            content_width,
            &mut box_,
        );
        cursor_y += consumed;
    }

    let content_end = cursor_y + padding_bottom;
    box_.rect.height = match style.height {
        Length::Auto => content_end,
        length => length.to_px(font_size, available_width),
    };

    let outer = box_.outer_height(font_size);
    parent.children.push(box_);
    outer
}

/// Collects a block's inline content as styled runs, stopping at block children
/// so their text is not counted twice.
///
/// Each inline element contributes its own run carrying its own computed style,
/// which is what lets `<b>` and `<code>` inside a paragraph render differently
/// from the text around them.
fn collect_inline_runs(
    doc: &Document,
    styles: &StyleMap,
    node: NodeId,
    inherited: &ComputedStyle,
) -> Vec<InlineRun> {
    let mut runs = Vec::new();
    gather_runs(doc, styles, node, inherited, &mut runs);

    // Whitespace collapsing spans run boundaries: `<b>bold</b> <i>italic</i>`
    // must not lose the space between the runs, and `a <b> b</b>` must not keep
    // two. Collapsing each run in isolation would get both wrong, so the runs
    // are collapsed as one stream with the boundary state carried across.
    let mut previous_ended_in_space = true;
    for run in &mut runs {
        if run.style.white_space == WhiteSpace::Pre {
            previous_ended_in_space = run.text.ends_with(char::is_whitespace);
            continue;
        }
        let collapsed = collapse_whitespace_from(&run.text, previous_ended_in_space);
        previous_ended_in_space = collapsed.ends_with(' ');
        run.text = collapsed;
    }
    // Leading and trailing whitespace of the whole block is dropped.
    if let Some(first) = runs.first_mut() {
        first.text = first.text.trim_start().to_owned();
    }
    if let Some(last) = runs.last_mut() {
        last.text = last.text.trim_end().to_owned();
    }
    runs
}

fn gather_runs(
    doc: &Document,
    styles: &StyleMap,
    node: NodeId,
    inherited: &ComputedStyle,
    out: &mut Vec<InlineRun>,
) {
    for &child in doc.children(node) {
        if let Some(text) = doc.text(child) {
            out.push(InlineRun {
                text: text.to_owned(),
                style: inherited.clone(),
            });
        } else if let Some(style) = styles.get(child) {
            if style.display == Display::None || !style.display.is_inline() {
                continue;
            }
            gather_runs(doc, styles, child, style, out);
        }
    }
}

/// Collapses whitespace per `white-space: normal`.
///
/// Every run of spaces, tabs, and newlines becomes a single space. Without
/// this, source indentation and line breaks reach the shaper verbatim and every
/// wrapped line inherits the author's leading whitespace — visible as a ragged
/// indent on continuation lines.
pub fn collapse_whitespace(text: &str) -> String {
    collapse_whitespace_from(text, true)
}

/// Collapses whitespace, treating the preceding text as already ending in a
/// space when `after_space` is set.
///
/// The carried state is what makes collapsing correct across inline run
/// boundaries: without it, the space between `</b>` and `<i>` either vanishes
/// or doubles depending on which run it landed in.
fn collapse_whitespace_from(text: &str, after_space: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_whitespace = after_space;
    for c in text.chars() {
        if c.is_whitespace() {
            if !in_whitespace {
                out.push(' ');
            }
            in_whitespace = true;
        } else {
            out.push(c);
            in_whitespace = false;
        }
    }
    out
}

/// Horizontal offset for a line, given the alignment of its block.
pub fn line_offset(align: TextAlign, line_width: f32, content_width: f32) -> f32 {
    match align {
        TextAlign::Left | TextAlign::Justify => 0.0,
        TextAlign::Center => ((content_width - line_width) / 2.0).max(0.0),
        TextAlign::Right => (content_width - line_width).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use css::Stylesheet;

    struct Rendered {
        layout: Layout,
    }

    fn run(html: &str, css_text: &str, width: f32) -> Rendered {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css_text)];
        let styles = css::cascade::cascade(&doc, &sheets);
        let mut fonts = FontStore::new();
        Rendered {
            layout: layout(&doc, &styles, &mut fonts, width),
        }
    }

    /// Depth-first list of every box below `root`.
    fn boxes(root: &LayoutBox) -> Vec<&LayoutBox> {
        let mut out = Vec::new();
        let mut stack: Vec<&LayoutBox> = root.children.iter().rev().collect();
        while let Some(b) = stack.pop() {
            out.push(b);
            stack.extend(b.children.iter().rev());
        }
        out
    }

    /// Boxes inside `<body>`, which is what the tests actually care about.
    ///
    /// The layout root is the canvas and its sole child is the body box, so
    /// indexing the root directly returns the body and silently shifts every
    /// expectation by one level.
    fn content_boxes(rendered: &Rendered) -> Vec<&LayoutBox> {
        let body = rendered.layout.root.children.first().expect("body box");
        boxes(body)
    }

    #[test]
    fn blocks_stack_vertically_without_overlapping() {
        let rendered = run("<body><p>one</p><p>two</p><p>three</p></body>", "", 800.0);
        let all = content_boxes(&rendered);
        let paragraphs: Vec<_> = all.iter().filter(|b| b.text.is_some()).collect();
        assert_eq!(paragraphs.len(), 3);
        for pair in paragraphs.windows(2) {
            assert!(
                pair[1].rect.y >= pair[0].rect.y + pair[0].rect.height,
                "boxes overlap: {:?} then {:?}",
                pair[0].rect,
                pair[1].rect
            );
        }
    }

    #[test]
    fn a_block_fills_the_width_it_is_given() {
        let rendered = run("<body><div>x</div></body>", "body { margin: 0 }", 500.0);
        assert_eq!(content_boxes(&rendered)[0].rect.width, 500.0);
    }

    #[test]
    fn margins_offset_a_block() {
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { margin: 10px 20px }",
            500.0,
        );
        let div = content_boxes(&rendered)[0];
        assert_eq!(div.rect.x, 20.0);
        assert_eq!(div.rect.y, 10.0);
        assert_eq!(
            div.rect.width, 460.0,
            "width shrinks by both horizontal margins"
        );
    }

    #[test]
    fn padding_grows_a_box_and_insets_its_content() {
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { padding: 12px }",
            400.0,
        );
        let div = content_boxes(&rendered)[0];
        assert_eq!(div.content_origin, (12.0, 12.0));
        assert!(div.rect.height >= 24.0, "height includes both paddings");
    }

    #[test]
    fn narrow_columns_wrap_text_and_grow_taller() {
        let html = "<body><p>the quick brown fox jumps over the lazy dog repeatedly</p></body>";
        let wide = run(html, "", 900.0);
        let narrow = run(html, "", 150.0);
        assert!(
            narrow.layout.height > wide.layout.height,
            "narrow {} should exceed wide {}",
            narrow.layout.height,
            wide.layout.height
        );
    }

    #[test]
    fn display_none_removes_a_box_entirely() {
        let both = run("<body><p>one</p><p>two</p></body>", "", 800.0);
        let one_hidden = run(
            r#"<body><p>one</p><p class="h">two</p></body>"#,
            ".h { display: none }",
            800.0,
        );
        assert_eq!(
            content_boxes(&one_hidden).len(),
            content_boxes(&both).len() - 1
        );
    }

    #[test]
    fn headings_are_taller_than_paragraphs() {
        let heading = run("<body><h1>Title</h1></body>", "", 800.0);
        let paragraph = run("<body><p>Title</p></body>", "", 800.0);
        assert!(
            heading.layout.height > paragraph.layout.height,
            "h1 at 2em should exceed a paragraph"
        );
    }

    #[test]
    fn nested_blocks_indent_by_their_parents_padding() {
        let rendered = run(
            "<body><div class=\"outer\"><div class=\"inner\">x</div></div></body>",
            "body { margin: 0 } .outer { padding-left: 30px }",
            600.0,
        );
        let inner = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.padding.left == Length::Px(0.0) && b.text.is_some())
            .expect("inner box");
        assert_eq!(inner.rect.x, 30.0);
        assert_eq!(inner.rect.width, 570.0);
    }

    #[test]
    fn source_whitespace_is_collapsed() {
        assert_eq!(collapse_whitespace("a\n    b\tc  d"), "a b c d");
        assert_eq!(collapse_whitespace("\n   leading"), "leading");
    }

    #[test]
    fn indented_source_does_not_indent_wrapped_lines() {
        // Pretty-printed HTML is the norm, so this is the common case, not an
        // edge case: without collapsing, every continuation line is indented by
        // the author's source indentation.
        let pretty = "<body><p>the quick brown fox\n        jumps over the lazy dog\n        and \
                      keeps running onward</p></body>";
        let flat = "<body><p>the quick brown fox jumps over the lazy dog and keeps running \
                    onward</p></body>";
        let a = run(pretty, "", 200.0);
        let b = run(flat, "", 200.0);
        assert_eq!(
            a.layout.height, b.layout.height,
            "indentation changed the layout"
        );
    }

    #[test]
    fn whitespace_collapses_across_inline_run_boundaries() {
        // The space between two inline elements lives in neither of them.
        // Collapsing runs in isolation either loses it or doubles it, and both
        // are visible on any page that emphasises a word.
        let spaced = run("<body><p><b>one</b> <i>two</i></p></body>", "", 800.0);
        let joined = run("<body><p><b>one</b><i>two</i></p></body>", "", 800.0);
        let width = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .find_map(|b| b.text.as_ref().map(|t| t.width))
                .expect("text")
        };
        assert!(
            width(&spaced) > width(&joined),
            "the inter-element space vanished: {} vs {}",
            width(&spaced),
            width(&joined)
        );
    }

    #[test]
    fn runs_of_whitespace_around_a_span_collapse_to_one() {
        let single = run("<body><p>a <b>b</b></p></body>", "", 800.0);
        let many = run("<body><p>a   <b>  b</b></p></body>", "", 800.0);
        let width = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .find_map(|b| b.text.as_ref().map(|t| t.width))
                .expect("text")
        };
        assert!(
            (width(&single) - width(&many)).abs() < 0.01,
            "extra whitespace was not collapsed: {} vs {}",
            width(&single),
            width(&many)
        );
    }

    #[test]
    fn pre_preserves_whitespace() {
        let html = "<body><pre>one\ntwo\nthree</pre></body>";
        let rendered = run(html, "", 800.0);
        let pre = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.text.is_some())
            .expect("pre box");
        assert_eq!(
            pre.text.as_ref().unwrap().lines.len(),
            3,
            "newlines must survive in pre"
        );
    }

    #[test]
    fn text_align_offsets_lines_within_the_content_box() {
        assert_eq!(line_offset(TextAlign::Left, 100.0, 500.0), 0.0);
        assert_eq!(line_offset(TextAlign::Center, 100.0, 500.0), 200.0);
        assert_eq!(line_offset(TextAlign::Right, 100.0, 500.0), 400.0);
        // A line wider than its box never produces a negative offset.
        assert_eq!(line_offset(TextAlign::Center, 700.0, 500.0), 0.0);
    }
}
