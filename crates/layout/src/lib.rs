//! Box tree construction and block layout.
//!
//! Block boxes stack vertically, each laying its inline content out as an
//! inline formatting context: differently-styled spans share line boxes and
//! break as one paragraph. Floats, tables, and positioning are the rest of M2
//! (ADR-0004).

pub mod classify;
pub mod floats;
pub mod table;

use css::cascade::StyleMap;
use css::style::{ComputedStyle, Display, Float, TextAlign, WhiteSpace};
use css::value::Length;
use dom::{Document, NodeId};
use floats::FloatContext;
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
    /// Width of the content box.
    ///
    /// Stored rather than derived: with asymmetric borders and padding the
    /// content width cannot be recovered from `rect` and `content_origin`.
    pub content_width: f32,
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
        content_width: viewport_width,
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
        FloatContext::new(viewport_width),
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
    inherited: FloatContext,
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
    let border_left = style.border.left.used_width(font_size);
    let border_right = style.border.right.used_width(font_size);
    let border_top = style.border.top.used_width(font_size);
    let border_bottom = style.border.bottom.used_width(font_size);

    // CSS 2.1 `width` is the *content* width, so borders and padding grow the
    // box outwards rather than being absorbed by it.
    let surround = padding_left + padding_right + border_left + border_right;
    let outer_width = match style.width {
        Length::Auto => (available_width - margin_left - margin_right).max(0.0),
        length => length.to_px(font_size, available_width) + surround,
    };
    let content_width = (outer_width - surround).max(0.0);

    let mut box_ = LayoutBox {
        rect: Rect {
            x: x + margin_left,
            y: y + margin_top,
            width: outer_width,
            height: 0.0,
        },
        style: style.clone(),
        text: None,
        content_origin: (padding_left + border_left, padding_top + border_top),
        content_width,
        children: Vec::new(),
    };

    // Inline children become styled runs shaped as one paragraph, so a <b> or
    // <code> inside this block keeps its own style while still breaking lines
    // with the text around it.
    let runs = collect_inline_runs(doc, styles, node, style);
    let mut content_height = 0.0;
    // Floats declared by ancestors still apply here, shifted into this block's
    // coordinates; this block's own floats are added on top.
    let mut context = inherited.translated(
        padding_left + border_left,
        padding_top + border_top,
        content_width,
    );

    // Floats declared before any in-flow block are placed first, so this
    // block's own text knows to flow around them. Floats declared later are
    // placed during the walk below, at the height they actually appear —
    // placing everything up front would lift a float above content that
    // precedes it in the source.
    let mut early: Vec<(NodeId, ComputedStyle)> = Vec::new();
    let mut late: Vec<(NodeId, ComputedStyle)> = Vec::new();
    let mut seen_in_flow = false;
    for &child in doc.children(node) {
        let Some(child_style) = styles.get(child) else {
            continue;
        };
        if child_style.display == Display::None {
            continue;
        }
        if child_style.float != Float::None {
            if seen_in_flow {
                late.push((child, child_style.clone()));
            } else {
                early.push((child, child_style.clone()));
            }
        } else if !child_style.display.is_inline() && !child_style.display.is_table_internal() {
            seen_in_flow = true;
        }
    }
    for (child, child_style) in &early {
        place_float(
            doc,
            styles,
            fonts,
            *child,
            child_style,
            content_width,
            0.0,
            (padding_left + border_left, padding_top + border_top),
            &mut context,
            &mut box_,
        );
    }

    if runs.iter().any(|run| !run.text.trim().is_empty()) {
        let layout = if context.is_empty() {
            fonts.layout_runs(&runs, style, content_width)
        } else {
            fonts.layout_runs_constrained(&runs, style, |y, height| context.line_box(y, height))
        };
        content_height = layout.height;
        box_.text = Some(layout);
    }

    if style.display == Display::Table {
        let table_height = layout_table(
            doc,
            styles,
            fonts,
            node,
            style,
            padding_left + border_left,
            padding_top + border_top,
            content_width,
            &mut box_,
        );
        box_.rect.height = padding_top + border_top + table_height + padding_bottom + border_bottom;
        let outer = box_.outer_height(font_size);
        parent.children.push(box_);
        return outer;
    }

    let mut cursor_y = padding_top + border_top + content_height;
    for &child in doc.children(node) {
        let Some(child_style) = styles.get(child) else {
            continue;
        };
        // Table-internal boxes are positioned by their table, not by block
        // flow. A stray one outside a table falls through to block layout so
        // its content is still shown.
        if child_style.display == Display::None
            || child_style.display.is_inline()
            || child_style.display.is_table_internal()
            // Floated children were placed above, out of the normal flow.
            || child_style.float != Float::None
        {
            continue;
        }
        // Place any float declared before this child, at the height reached so
        // far rather than at the top of the container.
        while let Some((float_node, float_style)) = late.first().cloned() {
            if !precedes(doc, node, float_node, child) {
                break;
            }
            late.remove(0);
            place_float(
                doc,
                styles,
                fonts,
                float_node,
                &float_style,
                content_width,
                cursor_y - padding_top - border_top,
                (padding_left + border_left, padding_top + border_top),
                &mut context,
                &mut box_,
            );
        }

        // `clear` pushes this box below the floats it names.
        cursor_y = context.clearance(child_style.clear, cursor_y);
        let child_context =
            context.translated(0.0, cursor_y - padding_top - border_top, content_width);
        let consumed = layout_block(
            doc,
            styles,
            fonts,
            child,
            child_style,
            padding_left + border_left,
            cursor_y,
            content_width,
            child_context,
            &mut box_,
        );
        cursor_y += consumed;
    }

    // A block must be tall enough to contain its own floats, or the next
    // block would start beside one and overlap it.
    let content_end = cursor_y.max(padding_top + border_top + context.lowest_edge())
        + padding_bottom
        + border_bottom;
    box_.rect.height = match style.height {
        Length::Auto => content_end,
        length => {
            length.to_px(font_size, available_width)
                + padding_top
                + padding_bottom
                + border_top
                + border_bottom
        }
    };

    let outer = box_.outer_height(font_size);
    parent.children.push(box_);
    outer
}

/// Lays out a table's rows and cells, appending them to `parent`.
///
/// Returns the height consumed. Column widths come from cell content
/// (`table::distribute_widths`); each cell is then laid out as an ordinary
/// block at its column's width, and a row is as tall as its tallest cell.
#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn layout_table(
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
    let grid = table::build_grid(doc, styles, node);
    if grid.columns == 0 {
        return 0.0;
    }

    // Intrinsic widths per column, from the cells that span exactly one.
    let mut mins = vec![0.0f32; grid.columns];
    let mut maxes = vec![0.0f32; grid.columns];
    let mut spans: Vec<(usize, usize, f32, f32)> = Vec::new();

    for row in &grid.rows {
        for cell in row {
            let runs = collect_inline_runs(doc, styles, cell.node, &cell.style);
            let (mut min, mut max) = fonts.intrinsic_widths(&runs, &cell.style);
            // A cell's own padding and borders are part of what it needs.
            let surround = cell.style.padding.left.to_px(cell.style.font_size, 0.0)
                + cell.style.padding.right.to_px(cell.style.font_size, 0.0)
                + cell.style.border.left.used_width(cell.style.font_size)
                + cell.style.border.right.used_width(cell.style.font_size);
            min += surround;
            max += surround;

            if cell.colspan == 1 {
                if let (Some(column_min), Some(column_max)) =
                    (mins.get_mut(cell.column), maxes.get_mut(cell.column))
                {
                    *column_min = column_min.max(min);
                    *column_max = column_max.max(max);
                }
            } else {
                // Spanning cells are applied after the single-column cells have
                // set a baseline, so they only ever widen columns.
                spans.push((cell.column, cell.colspan, min, max));
            }
        }
    }
    for (column, colspan, min, max) in spans {
        table::apply_span(&mut mins, column, colspan, min);
        table::apply_span(&mut maxes, column, colspan, max);
    }

    let spacing_total = table::BORDER_SPACING * (grid.columns + 1) as f32;
    let usable = (available_width - spacing_total).max(0.0);
    let mut widths = table::distribute_widths(&mins, &maxes, Some(usable));

    // A table with no declared width shrinks to fit its content. One with a
    // declared width fills it, which is exactly what `<table width="100%">`
    // meant on the era's pages and why so many of them used it.
    if style.width != Length::Auto {
        let total: f32 = widths.iter().sum();
        if total > 0.0 && usable > total {
            let scale = usable / total;
            for width in &mut widths {
                *width *= scale;
            }
        }
    }

    let mut cursor_y = y + table::BORDER_SPACING;
    for row in &grid.rows {
        let mut row_height = 0.0f32;
        let mut cell_boxes = Vec::new();

        for cell in row {
            let end = (cell.column + cell.colspan).min(widths.len());
            if cell.column >= end {
                continue;
            }
            let width: f32 = widths[cell.column..end].iter().sum::<f32>()
                + table::BORDER_SPACING * (end - cell.column - 1) as f32;
            let cell_x = x
                + table::BORDER_SPACING
                + widths[..cell.column].iter().sum::<f32>()
                + table::BORDER_SPACING * cell.column as f32;

            // Each cell is an ordinary block in a box of its column's width.
            let mut holder = LayoutBox {
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width,
                    height: 0.0,
                },
                style: cell.style.clone(),
                text: None,
                content_origin: (0.0, 0.0),
                content_width: width,
                children: Vec::new(),
            };
            // A cell establishes its own formatting context, so floats outside
            // the table do not reach into it.
            let consumed = layout_block(
                doc,
                styles,
                fonts,
                cell.node,
                &cell.style,
                cell_x,
                cursor_y,
                width,
                FloatContext::new(width),
                &mut holder,
            );
            row_height = row_height.max(consumed);
            if let Some(cell_box) = holder.children.pop() {
                cell_boxes.push(cell_box);
            }
        }

        // Cells stretch to the row's height so backgrounds and borders line up.
        for mut cell_box in cell_boxes {
            cell_box.rect.height = cell_box.rect.height.max(row_height);
            parent.children.push(cell_box);
        }
        cursor_y += row_height + table::BORDER_SPACING;
    }

    cursor_y - y
}

/// Lays out a floated child and places it in `context`.
///
/// `y` is where the float may first sit, in the container's content
/// coordinates; `origin` is the container's content-box offset within its own
/// border box, needed to position the resulting box.
#[expect(
    clippy::too_many_arguments,
    reason = "layout context, threaded explicitly for clarity"
)]
fn place_float(
    doc: &Document,
    styles: &StyleMap,
    fonts: &mut FontStore,
    child: NodeId,
    child_style: &ComputedStyle,
    content_width: f32,
    y: f32,
    origin: (f32, f32),
    context: &mut FloatContext,
    parent: &mut LayoutBox,
) {
    // A float shrinks to fit its content unless a width is declared, so its
    // natural width has to be measured before it can be placed.
    let runs = collect_inline_runs(doc, styles, child, child_style);
    let (_, natural) = fonts.intrinsic_widths(&runs, child_style);
    let surround = child_style
        .padding
        .left
        .to_px(child_style.font_size, content_width)
        + child_style
            .padding
            .right
            .to_px(child_style.font_size, content_width)
        + child_style.border.left.used_width(child_style.font_size)
        + child_style.border.right.used_width(child_style.font_size);
    let float_width = match child_style.width {
        Length::Auto => (natural + surround).min(content_width),
        length => length.to_px(child_style.font_size, content_width) + surround,
    };

    let mut probe = LayoutBox {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: float_width,
            height: 0.0,
        },
        style: child_style.clone(),
        text: None,
        content_origin: (0.0, 0.0),
        content_width: float_width,
        children: Vec::new(),
    };
    let float_height = layout_block(
        doc,
        styles,
        fonts,
        child,
        child_style,
        0.0,
        0.0,
        float_width,
        FloatContext::new(float_width),
        &mut probe,
    );
    let (left, top) = context.place(child_style.float, float_width, float_height, y);

    if let Some(mut float_box) = probe.children.pop() {
        // Offsetting rather than assigning keeps the float's own margins,
        // which `layout_block` already folded into its rect.
        float_box.rect.x += origin.0 + left;
        float_box.rect.y += origin.1 + top;
        parent.children.push(float_box);
    }
}

/// Whether `first` comes before `second` among `parent`'s children.
fn precedes(doc: &Document, parent: NodeId, first: NodeId, second: NodeId) -> bool {
    let children = doc.children(parent);
    let index = |target: NodeId| children.iter().position(|&c| c == target);
    match (index(first), index(second)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
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
    fn borders_grow_the_box_and_inset_its_content() {
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { border: 5px solid black; padding: 10px }",
            400.0,
        );
        let div = content_boxes(&rendered)[0];
        assert_eq!(
            div.content_origin,
            (15.0, 15.0),
            "content sits inside border then padding"
        );
        assert_eq!(
            div.rect.width, 400.0,
            "an auto-width box still fills its container"
        );
        assert_eq!(
            div.content_width, 370.0,
            "content shrinks by both borders and paddings"
        );
    }

    #[test]
    fn an_explicit_width_is_the_content_width() {
        // CSS 2.1 is content-box: borders and padding grow the box outwards.
        let rendered = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { width: 100px; border: 2px solid black; padding: 8px }",
            400.0,
        );
        let div = content_boxes(&rendered)[0];
        assert_eq!(div.content_width, 100.0);
        assert_eq!(div.rect.width, 120.0, "100 + 2*8 padding + 2*2 border");
    }

    #[test]
    fn a_border_without_a_style_occupies_no_space() {
        let styled = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { border-width: 20px; border-style: solid }",
            400.0,
        );
        let unstyled = run(
            "<body><div>x</div></body>",
            "body { margin: 0 } div { border-width: 20px }",
            400.0,
        );
        assert_eq!(content_boxes(&styled)[0].content_width, 360.0);
        assert_eq!(content_boxes(&unstyled)[0].content_width, 400.0);
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
    fn table_cells_are_laid_out_side_by_side() {
        let rendered = run(
            "<body><table><tr><td>one</td><td>two</td></tr></table></body>",
            "body { margin: 0 }",
            600.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert_eq!(cells.len(), 2);
        assert!(
            cells[1].rect.x > cells[0].rect.x,
            "second cell must sit to the right"
        );
        // Same row, so the same top edge.
        assert!((cells[0].rect.y - cells[1].rect.y).abs() < 0.01);
    }

    #[test]
    fn table_rows_stack_downwards() {
        let rendered = run(
            "<body><table><tr><td>one</td></tr><tr><td>two</td></tr></table></body>",
            "body { margin: 0 }",
            600.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert_eq!(cells.len(), 2);
        assert!(
            cells[1].rect.y > cells[0].rect.y,
            "second row must sit below"
        );
        assert!(
            (cells[0].rect.x - cells[1].rect.x).abs() < 0.01,
            "same column, same x"
        );
    }

    #[test]
    fn a_narrow_table_shrinks_to_fit_rather_than_stretching() {
        let rendered = run(
            "<body><table><tr><td>a</td><td>b</td></tr></table></body>",
            "body { margin: 0 }",
            800.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        let total: f32 = cells.iter().map(|c| c.rect.width).sum();
        assert!(
            total < 200.0,
            "two one-letter cells should not fill 800px, got {total}"
        );
    }

    #[test]
    fn a_wide_column_gets_more_room_than_a_narrow_one() {
        let rendered = run(
            "<body><table><tr><td>x</td>\
             <td>a considerably longer piece of cell content here</td></tr></table></body>",
            "body { margin: 0 }",
            400.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert!(
            cells[1].rect.width > cells[0].rect.width * 3.0,
            "column sizing ignored content: {} vs {}",
            cells[0].rect.width,
            cells[1].rect.width
        );
    }

    #[test]
    fn a_full_width_table_fills_its_container() {
        // `<table width="100%">` was ubiquitous on the era's pages.
        let rendered = run(
            "<body><table><tr><td>a</td><td>b</td></tr></table></body>",
            "body { margin: 0 } table { width: 100% }",
            500.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        let total: f32 = cells.iter().map(|c| c.rect.width).sum();
        assert!(
            total > 450.0,
            "declared width should fill the container, got {total}"
        );
    }

    #[test]
    fn a_spanning_cell_covers_the_columns_below_it() {
        let rendered = run(
            r#"<body><table><tr><td colspan="2">wide header</td></tr>
               <tr><td>a</td><td>b</td></tr></table></body>"#,
            "body { margin: 0 }",
            600.0,
        );
        let cells: Vec<_> = content_boxes(&rendered)
            .into_iter()
            .filter(|b| b.text.is_some())
            .collect();
        assert_eq!(cells.len(), 3);
        let spanning = cells[0].rect.width;
        let below = cells[1].rect.width + cells[2].rect.width;
        assert!(
            spanning >= below - 5.0,
            "span {spanning} should cover both columns {below}"
        );
    }

    #[test]
    fn text_flows_beside_a_left_float() {
        let html = "<body><div class=\"f\">side</div><p>the quick brown fox jumps over the \
                    lazy dog and keeps on running for a good while longer than one line \
                    so that several lines sit beside the float</p></body>";
        // Tall enough to narrow several lines; a one-line float would narrow
        // only the first and the test could not tell the cases apart.
        let floated = run(
            html,
            "body { margin: 0 } .f { float: left; width: 150px; height: 120px }",
            500.0,
        );
        let plain = run(
            html,
            "body { margin: 0 } .f { width: 150px; height: 120px }",
            500.0,
        );

        let float_box = content_boxes(&floated)
            .into_iter()
            .find(|b| b.style.float == Float::Left)
            .expect("float box");
        assert_eq!(
            float_box.rect.y, 0.0,
            "the float sits at the top, not below the text"
        );

        // Beside a 150px float the paragraph has less room, so it needs more
        // lines than the same paragraph laid out at full width.
        let lines = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .filter_map(|b| b.text.as_ref().map(|t| t.lines.len()))
                .max()
                .unwrap_or(0)
        };
        assert!(
            lines(&floated) > lines(&plain),
            "text did not narrow beside the float: {} vs {}",
            lines(&floated),
            lines(&plain)
        );
    }

    #[test]
    fn a_right_float_sits_against_the_right_edge() {
        let rendered = run(
            "<body><div class=\"f\">side</div><p>text</p></body>",
            "body { margin: 0 } .f { float: right; width: 100px }",
            600.0,
        );
        let float_box = content_boxes(&rendered)
            .into_iter()
            .find(|b| b.style.float == Float::Right)
            .expect("float box");
        assert!(
            float_box.rect.x > 400.0,
            "expected a right-hand position, got {}",
            float_box.rect.x
        );
    }

    #[test]
    fn clear_pushes_a_block_below_the_float() {
        let cleared = run(
            "<body><div class=\"f\">side</div><p class=\"c\">after</p></body>",
            "body { margin: 0 } .f { float: left; width: 100px; height: 80px } .c { clear: left }",
            500.0,
        );
        let uncleared = run(
            "<body><div class=\"f\">side</div><p class=\"c\">after</p></body>",
            "body { margin: 0 } .f { float: left; width: 100px; height: 80px }",
            500.0,
        );
        // Select the paragraph specifically: the float also has text, and it is
        // pushed into the box list first.
        let paragraph_y = |r: &Rendered| {
            content_boxes(r)
                .into_iter()
                .find(|b| b.style.float == Float::None && b.text.is_some())
                .map(|b| b.rect.y)
                .expect("paragraph box")
        };
        assert!(
            paragraph_y(&cleared) >= 80.0,
            "clear:left must move below the 80px float, got {}",
            paragraph_y(&cleared)
        );
        assert!(
            paragraph_y(&uncleared) < 80.0,
            "without clear it stays alongside"
        );
    }

    #[test]
    fn a_container_encloses_a_float_taller_than_its_text() {
        // Otherwise the next block starts beside the float and overlaps it.
        let rendered = run(
            "<body><div class=\"box\"><div class=\"f\">side</div>short</div></body>",
            "body { margin: 0 } .f { float: left; width: 60px; height: 120px }",
            400.0,
        );
        let container = content_boxes(&rendered)[0];
        assert!(
            container.rect.height >= 120.0,
            "container must enclose its float, got {}",
            container.rect.height
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
