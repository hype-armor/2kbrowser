//! The cascade: matching rules to elements and resolving computed values.

use std::collections::HashMap;

use dom::{Document, NodeId};

use crate::style::{
    BorderSide, BorderStyle, Borders, ComputedStyle, DEFAULT_FONT_SIZE, Edges, FontStack,
    FontStyle, GenericFamily, MEDIUM_BORDER, NORMAL_LINE_HEIGHT, TextAlign, WhiteSpace,
    parse_border_style, parse_clear, parse_display, parse_float,
};
use crate::value::{Color, Length, Raw, parse_color, parse_length};
use crate::{Declaration, Specificity, Stylesheet};

/// Where a stylesheet came from. Origin outranks specificity in the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// The user-agent stylesheet.
    UserAgent,
    /// Stylesheets supplied by the page.
    Author,
}

/// Computed styles for every element in a document.
#[derive(Debug, Clone, Default)]
pub struct StyleMap {
    styles: HashMap<NodeId, ComputedStyle>,
}

impl StyleMap {
    /// The computed style for a node, if it is a styled element.
    pub fn get(&self, node: NodeId) -> Option<&ComputedStyle> {
        self.styles.get(&node)
    }
}

/// Sort key for a matched declaration, in increasing precedence order.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Precedence {
    important: bool,
    origin: Origin,
    specificity: Specificity,
    order: usize,
}

/// Resolves computed styles for the whole document.
///
/// Sheets are applied in the order given, after the user-agent sheet.
pub fn cascade(doc: &Document, author_sheets: &[Stylesheet]) -> StyleMap {
    let ua = Stylesheet::parse(crate::ua::UA_STYLESHEET);
    let mut map = StyleMap::default();
    let root_style = ComputedStyle::default();
    style_subtree(doc, doc.root(), &root_style, &ua, author_sheets, &mut map);
    map
}

fn style_subtree(
    doc: &Document,
    node: NodeId,
    parent_style: &ComputedStyle,
    ua: &Stylesheet,
    author: &[Stylesheet],
    out: &mut StyleMap,
) {
    let style = if doc.element(node).is_some() {
        let computed = compute(doc, node, parent_style, ua, author);
        out.styles.insert(node, computed.clone());
        computed
    } else {
        parent_style.clone()
    };

    for &child in doc.children(node) {
        style_subtree(doc, child, &style, ua, author, out);
    }
}

fn compute(
    doc: &Document,
    node: NodeId,
    parent: &ComputedStyle,
    ua: &Stylesheet,
    author: &[Stylesheet],
) -> ComputedStyle {
    let mut matched: Vec<(Precedence, &Declaration)> = Vec::new();
    let mut order = 0usize;

    for (sheet, origin) in
        std::iter::once((ua, Origin::UserAgent)).chain(author.iter().map(|s| (s, Origin::Author)))
    {
        for rule in &sheet.rules {
            let best = rule
                .selectors
                .iter()
                .filter(|selector| selector.matches(doc, node))
                .map(|selector| selector.specificity())
                .max();
            if let Some(specificity) = best {
                for declaration in &rule.declarations {
                    order += 1;
                    matched.push((
                        Precedence {
                            important: declaration.important,
                            origin,
                            specificity,
                            order,
                        },
                        declaration,
                    ));
                }
            }
        }
    }

    matched.sort_by(|a, b| a.0.cmp(&b.0));

    let mut style = ComputedStyle::inherit_from(parent);
    // The UA sheet gives `display: block` to block-level elements; everything
    // else starts inline, which is the CSS initial value.
    for (_, declaration) in matched {
        apply(&mut style, declaration, parent);
    }
    style
}

/// Applies one declaration to a style in progress.
///
/// Unknown properties and unparseable values are dropped, which is the
/// specified behaviour and the only workable one for the real web.
fn apply(style: &mut ComputedStyle, declaration: &Declaration, parent: &ComputedStyle) {
    let values = &declaration.value;
    let Some(first) = values.first() else { return };

    match declaration.name.as_str() {
        "display" => {
            if let Raw::Ident(name) = first
                && let Some(display) = parse_display(name)
            {
                style.display = display;
            }
        }
        "color" => {
            if let Some(color) = parse_color(first) {
                style.color = color;
            }
        }
        "background-color" | "background" => {
            if let Some(color) = parse_color(first) {
                style.background_color = color;
            }
        }
        // font-size resolves em and % against the *parent's* size, not its own.
        "font-size" => {
            if let Some(size) = parse_font_size(first, parent.font_size) {
                style.font_size = size;
                if style.line_height == parent.line_height {
                    style.line_height = size * NORMAL_LINE_HEIGHT;
                }
            }
        }
        "font-weight" => {
            style.font_weight = match first {
                Raw::Ident(name) if name == "bold" => 700,
                Raw::Ident(name) if name == "normal" => 400,
                Raw::Ident(name) if name == "bolder" => (parent.font_weight + 300).min(900),
                Raw::Ident(name) if name == "lighter" => parent.font_weight.saturating_sub(300),
                Raw::Number(n) => (*n as u16).clamp(100, 900),
                _ => style.font_weight,
            };
        }
        "font-style" => {
            if let Raw::Ident(name) = first {
                style.font_style = match name.as_str() {
                    "italic" | "oblique" => FontStyle::Italic,
                    _ => FontStyle::Normal,
                };
            }
        }
        "font-family" => style.font_family = parse_font_family(values),
        "line-height" => {
            style.line_height = match first {
                // A unitless number is a multiplier, and inherits as a
                // multiplier rather than as a resolved length.
                Raw::Number(n) => style.font_size * n,
                Raw::Ident(name) if name == "normal" => style.font_size * NORMAL_LINE_HEIGHT,
                other => match parse_length(other) {
                    Some(length) => length.to_px(style.font_size, style.font_size),
                    None => style.line_height,
                },
            };
        }
        "text-align" => {
            if let Raw::Ident(name) = first {
                style.text_align = match name.as_str() {
                    "center" => TextAlign::Center,
                    "right" => TextAlign::Right,
                    "justify" => TextAlign::Justify,
                    _ => TextAlign::Left,
                };
            }
        }
        "white-space" => {
            if let Raw::Ident(name) = first {
                style.white_space = match name.as_str() {
                    "pre" | "pre-wrap" | "pre-line" => WhiteSpace::Pre,
                    "nowrap" => WhiteSpace::NoWrap,
                    _ => WhiteSpace::Normal,
                };
            }
        }
        "float" => {
            if let Raw::Ident(name) = first
                && let Some(float) = parse_float(name)
            {
                style.float = float;
            }
        }
        "clear" => {
            if let Raw::Ident(name) = first
                && let Some(clear) = parse_clear(name)
            {
                style.clear = clear;
            }
        }
        "margin" => style.margin = parse_edges(values),
        "padding" => style.padding = parse_edges(values),
        "width" => {
            if let Some(length) = parse_length(first) {
                style.width = length;
            }
        }
        "height" => {
            if let Some(length) = parse_length(first) {
                style.height = length;
            }
        }
        // `border: 1px solid red` sets width, style, and colour on all four
        // sides from whichever components are present.
        "border" => {
            let parsed = parse_border_shorthand(values);
            for side in border_sides(&mut style.border) {
                apply_border_shorthand(side, &parsed);
            }
        }
        "border-width" => {
            let lengths: Vec<Length> = values.iter().filter_map(parse_length).collect();
            let widths = expand_four(&lengths);
            for (side, width) in border_sides(&mut style.border).into_iter().zip(widths) {
                if let Some(width) = width {
                    side.width = width;
                }
            }
        }
        "border-style" => {
            let styles: Vec<BorderStyle> = values
                .iter()
                .filter_map(|raw| match raw {
                    Raw::Ident(name) => parse_border_style(name),
                    _ => None,
                })
                .collect();
            let expanded = expand_four(&styles);
            for (side, border_style) in border_sides(&mut style.border).into_iter().zip(expanded) {
                if let Some(border_style) = border_style {
                    side.style = border_style;
                }
            }
        }
        "border-color" => {
            let colors: Vec<Color> = values.iter().filter_map(parse_color).collect();
            let expanded = expand_four(&colors);
            for (side, color) in border_sides(&mut style.border).into_iter().zip(expanded) {
                if color.is_some() {
                    side.color = color;
                }
            }
        }
        name => {
            if let Some(side) = name.strip_prefix("margin-") {
                set_edge(&mut style.margin, side, first);
            } else if let Some(side) = name.strip_prefix("padding-") {
                set_edge(&mut style.padding, side, first);
            } else if let Some(rest) = name.strip_prefix("border-") {
                apply_border_longhand(&mut style.border, rest, values);
            }
        }
    }
}

fn parse_font_size(raw: &Raw, parent_size: f32) -> Option<f32> {
    if let Raw::Ident(name) = raw {
        // The CSS 2.1 absolute-size keywords, as scale factors from medium.
        let factor = match name.as_str() {
            "xx-small" => 0.5625,
            "x-small" => 0.6875,
            "small" => 0.8125,
            "medium" => 1.0,
            "large" => 1.125,
            "x-large" => 1.5,
            "xx-large" => 2.0,
            "smaller" => return Some(parent_size / 1.2),
            "larger" => return Some(parent_size * 1.2),
            _ => return None,
        };
        return Some(DEFAULT_FONT_SIZE * factor);
    }
    match parse_length(raw)? {
        Length::Auto => None,
        length => Some(length.to_px(parent_size, parent_size)),
    }
}

fn parse_font_family(values: &[Raw]) -> FontStack {
    let mut families = Vec::new();
    let mut generic = None;
    for value in values {
        let name = match value {
            Raw::Ident(name) => name.clone(),
            Raw::Str(name) => name.clone(),
            _ => continue,
        };
        match name.to_ascii_lowercase().as_str() {
            "serif" => generic = Some(GenericFamily::Serif),
            "sans-serif" => generic = Some(GenericFamily::SansSerif),
            "monospace" => generic = Some(GenericFamily::Monospace),
            "cursive" => generic = Some(GenericFamily::Cursive),
            "fantasy" => generic = Some(GenericFamily::Fantasy),
            _ => families.push(name),
        }
    }
    FontStack {
        families,
        generic: generic.unwrap_or(GenericFamily::Serif),
    }
}

/// Parses the one-to-four value `margin`/`padding` shorthand.
fn parse_edges(values: &[Raw]) -> Edges {
    let lengths: Vec<Length> = values.iter().filter_map(parse_length).collect();
    match lengths.len() {
        1 => Edges::all(lengths[0]),
        2 => Edges {
            top: lengths[0],
            bottom: lengths[0],
            left: lengths[1],
            right: lengths[1],
        },
        3 => Edges {
            top: lengths[0],
            left: lengths[1],
            right: lengths[1],
            bottom: lengths[2],
        },
        4 => Edges {
            top: lengths[0],
            right: lengths[1],
            bottom: lengths[2],
            left: lengths[3],
        },
        _ => Edges::ZERO,
    }
}

/// The four border sides in CSS shorthand order.
fn border_sides(borders: &mut Borders) -> [&mut BorderSide; 4] {
    [
        &mut borders.top,
        &mut borders.right,
        &mut borders.bottom,
        &mut borders.left,
    ]
}

/// Expands a one-to-four value list to top, right, bottom, left.
fn expand_four<T: Copy>(values: &[T]) -> [Option<T>; 4] {
    match values.len() {
        1 => [Some(values[0]); 4],
        2 => [
            Some(values[0]),
            Some(values[1]),
            Some(values[0]),
            Some(values[1]),
        ],
        3 => [
            Some(values[0]),
            Some(values[1]),
            Some(values[2]),
            Some(values[1]),
        ],
        4 => [
            Some(values[0]),
            Some(values[1]),
            Some(values[2]),
            Some(values[3]),
        ],
        _ => [None; 4],
    }
}

/// Components of a `border`-style shorthand, in any order.
#[derive(Default)]
struct BorderShorthand {
    width: Option<Length>,
    style: Option<BorderStyle>,
    color: Option<Color>,
}

/// Reads `1px solid red` in any order, since CSS does not fix one.
fn parse_border_shorthand(values: &[Raw]) -> BorderShorthand {
    let mut out = BorderShorthand::default();
    for raw in values {
        if let Raw::Ident(name) = raw {
            // A keyword may be a style, a named width, or a colour, and the
            // order matters: `solid` is a style, not a failed colour lookup.
            if let Some(style) = parse_border_style(name) {
                out.style = Some(style);
                continue;
            }
            match name.as_str() {
                "thin" => {
                    out.width = Some(Length::Px(1.0));
                    continue;
                }
                "medium" => {
                    out.width = Some(Length::Px(MEDIUM_BORDER));
                    continue;
                }
                "thick" => {
                    out.width = Some(Length::Px(5.0));
                    continue;
                }
                _ => {}
            }
        }
        if let Some(color) = parse_color(raw) {
            out.color = Some(color);
        } else if let Some(length) = parse_length(raw) {
            out.width = Some(length);
        }
    }
    out
}

fn apply_border_shorthand(side: &mut BorderSide, parsed: &BorderShorthand) {
    // The shorthand resets omitted components to their initial values, which is
    // why `border: solid` produces a medium border rather than keeping whatever
    // width an earlier rule set.
    side.width = parsed.width.unwrap_or(Length::Px(MEDIUM_BORDER));
    side.style = parsed.style.unwrap_or_default();
    side.color = parsed.color;
}

/// Handles `border-top`, `border-left-width`, and friends.
fn apply_border_longhand(borders: &mut Borders, rest: &str, values: &[Raw]) {
    let (side_name, property) = match rest.split_once('-') {
        Some((side, property)) => (side, Some(property)),
        None => (rest, None),
    };
    let side = match side_name {
        "top" => &mut borders.top,
        "right" => &mut borders.right,
        "bottom" => &mut borders.bottom,
        "left" => &mut borders.left,
        _ => return,
    };
    let Some(first) = values.first() else { return };

    match property {
        // `border-top: 1px solid red`
        None => apply_border_shorthand(side, &parse_border_shorthand(values)),
        Some("width") => {
            if let Some(width) = parse_length(first) {
                side.width = width;
            }
        }
        Some("style") => {
            if let Raw::Ident(name) = first
                && let Some(style) = parse_border_style(name)
            {
                side.style = style;
            }
        }
        Some("color") => {
            if let Some(color) = parse_color(first) {
                side.color = Some(color);
            }
        }
        _ => {}
    }
}

fn set_edge(edges: &mut Edges, side: &str, raw: &Raw) {
    let Some(length) = parse_length(raw) else {
        return;
    };
    match side {
        "top" => edges.top = length,
        "right" => edges.right = length,
        "bottom" => edges.bottom = length,
        "left" => edges.left = length,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Display;

    fn style_of(html: &str, css: &str, tag: &str) -> ComputedStyle {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css)];
        let map = cascade(&doc, &sheets);
        let node = doc.find_element(tag).expect("element present");
        map.get(node).expect("element styled").clone()
    }

    #[test]
    fn ua_stylesheet_supplies_defaults() {
        let style = style_of("<p>x</p>", "", "p");
        assert_eq!(style.display, Display::Block);
        let h1 = style_of("<h1>x</h1>", "", "h1");
        assert_eq!(h1.font_size, 32.0, "h1 is 2em of the 16px default");
        assert!(h1.is_bold());
    }

    #[test]
    fn author_rules_beat_the_ua_sheet() {
        let style = style_of("<p>x</p>", "p { display: inline }", "p");
        assert_eq!(style.display, Display::Inline);
    }

    #[test]
    fn specificity_and_order_decide_ties() {
        let style = style_of(
            r#"<p id="a" class="b">x</p>"#,
            "p { color: red } .b { color: green } #a { color: blue }",
            "p",
        );
        assert_eq!(style.color, crate::Color::rgb(0, 0, 255), "id wins");

        let later = style_of("<p>x</p>", "p { color: red } p { color: lime }", "p");
        assert_eq!(
            later.color,
            crate::Color::rgb(0, 255, 0),
            "later rule wins a tie"
        );
    }

    #[test]
    fn important_outranks_specificity() {
        let style = style_of(
            r#"<p id="a">x</p>"#,
            "#a { color: red } p { color: lime !important }",
            "p",
        );
        assert_eq!(style.color, crate::Color::rgb(0, 255, 0));
    }

    #[test]
    fn inherited_properties_reach_descendants() {
        let doc = dom::parse("<div><span>x</span></div>");
        let sheets = [Stylesheet::parse("div { color: teal; font-size: 20px }")];
        let map = cascade(&doc, &sheets);
        let span = doc.find_element("span").expect("span");
        let style = map.get(span).expect("styled");
        assert_eq!(
            style.color,
            crate::Color::rgb(0, 128, 128),
            "color inherits"
        );
        assert_eq!(style.font_size, 20.0, "font-size inherits");
    }

    #[test]
    fn non_inherited_properties_do_not_leak_down() {
        let doc = dom::parse("<div><span>x</span></div>");
        let sheets = [Stylesheet::parse("div { margin: 10px }")];
        let map = cascade(&doc, &sheets);
        let span = doc.find_element("span").expect("span");
        assert_eq!(map.get(span).unwrap().margin.top, Length::Px(0.0));
    }

    #[test]
    fn em_resolves_against_the_parent_font_size() {
        let doc = dom::parse("<div><p>x</p></div>");
        let sheets = [Stylesheet::parse(
            "div { font-size: 20px } p { font-size: 1.5em }",
        )];
        let map = cascade(&doc, &sheets);
        let p = doc.find_element("p").expect("p");
        assert_eq!(map.get(p).unwrap().font_size, 30.0);
    }

    #[test]
    fn margin_shorthand_expands_by_arity() {
        let one = style_of("<p>x</p>", "p { margin: 5px }", "p").margin;
        assert_eq!(one, Edges::all(Length::Px(5.0)));

        let two = style_of("<p>x</p>", "p { margin: 1px 2px }", "p").margin;
        assert_eq!(two.top, Length::Px(1.0));
        assert_eq!(two.left, Length::Px(2.0));

        let four = style_of("<p>x</p>", "p { margin: 1px 2px 3px 4px }", "p").margin;
        assert_eq!(four.top, Length::Px(1.0));
        assert_eq!(four.right, Length::Px(2.0));
        assert_eq!(four.bottom, Length::Px(3.0));
        assert_eq!(four.left, Length::Px(4.0));
    }

    #[test]
    fn longhand_overrides_shorthand_when_it_comes_later() {
        let style = style_of("<p>x</p>", "p { margin: 5px; margin-left: 9px }", "p");
        assert_eq!(style.margin.left, Length::Px(9.0));
        assert_eq!(style.margin.top, Length::Px(5.0));
    }

    #[test]
    fn border_shorthand_sets_all_three_components() {
        let style = style_of("<p>x</p>", "p { border: 2px solid red }", "p");
        for side in [
            style.border.top,
            style.border.right,
            style.border.bottom,
            style.border.left,
        ] {
            assert_eq!(side.width, Length::Px(2.0));
            assert_eq!(side.style, BorderStyle::Solid);
            assert_eq!(side.color, Some(crate::Color::rgb(255, 0, 0)));
        }
    }

    #[test]
    fn border_shorthand_accepts_components_in_any_order() {
        // CSS does not fix the order, and real sheets use all of them.
        let a = style_of("<p>x</p>", "p { border: solid 3px blue }", "p");
        let b = style_of("<p>x</p>", "p { border: blue solid 3px }", "p");
        assert_eq!(a.border.top, b.border.top);
        assert_eq!(a.border.top.style, BorderStyle::Solid);
        assert_eq!(a.border.top.width, Length::Px(3.0));
    }

    #[test]
    fn a_width_without_a_style_occupies_nothing() {
        // The commonest border mistake: `border-width` alone draws nothing,
        // because the initial `border-style` is none.
        let style = style_of("<p>x</p>", "p { border-width: 10px }", "p");
        assert_eq!(style.border.top.width, Length::Px(10.0));
        assert_eq!(style.border.top.used_width(16.0), 0.0);

        let with_style = style_of(
            "<p>x</p>",
            "p { border-width: 10px; border-style: solid }",
            "p",
        );
        assert_eq!(with_style.border.top.used_width(16.0), 10.0);
    }

    #[test]
    fn per_side_longhands_override_the_shorthand() {
        let style = style_of(
            "<p>x</p>",
            "p { border: 1px solid black; border-left: 5px solid red }",
            "p",
        );
        assert_eq!(style.border.left.width, Length::Px(5.0));
        assert_eq!(style.border.left.color, Some(crate::Color::rgb(255, 0, 0)));
        assert_eq!(style.border.top.width, Length::Px(1.0));
    }

    #[test]
    fn border_longhand_components_are_settable_individually() {
        let style = style_of(
            "<p>x</p>",
            "p { border-top-style: dashed; border-top-width: 4px; border-top-color: lime }",
            "p",
        );
        assert_eq!(style.border.top.style, BorderStyle::Dashed);
        assert_eq!(style.border.top.width, Length::Px(4.0));
        assert_eq!(style.border.top.color, Some(crate::Color::rgb(0, 255, 0)));
    }

    #[test]
    fn hidden_reserves_space_without_painting() {
        let style = style_of("<p>x</p>", "p { border: 4px hidden red }", "p");
        assert_eq!(
            style.border.top.used_width(16.0),
            4.0,
            "hidden still occupies space"
        );
        assert!(!style.border.top.style.is_visible(), "but paints nothing");
    }

    #[test]
    fn border_style_expands_by_arity() {
        let style = style_of("<p>x</p>", "p { border-style: solid dashed }", "p");
        assert_eq!(style.border.top.style, BorderStyle::Solid);
        assert_eq!(style.border.right.style, BorderStyle::Dashed);
        assert_eq!(style.border.bottom.style, BorderStyle::Solid);
        assert_eq!(style.border.left.style, BorderStyle::Dashed);
    }

    #[test]
    fn flex_and_grid_are_recognised_but_unsupported() {
        // The mechanism ADR-0009 depends on: the engine must know it cannot lay
        // this out, rather than silently treating it as a block.
        let flex = style_of("<div>x</div>", "div { display: flex }", "div");
        assert_eq!(flex.display, Display::Flex);
        assert!(!flex.display.is_supported_layout());

        let grid = style_of("<div>x</div>", "div { display: grid }", "div");
        assert!(!grid.display.is_supported_layout());

        let block = style_of("<div>x</div>", "div { display: block }", "div");
        assert!(block.display.is_supported_layout());
    }
}
