//! The cascade: matching rules to elements and resolving computed values.

use std::collections::HashMap;

use dom::{Document, ElementData, NodeId};

use crate::style::{
    BackgroundRepeat, BorderSide, BorderStyle, Borders, ComputedStyle, DEFAULT_FONT_SIZE, Edges,
    FontStack, FontStyle, GenericFamily, MEDIUM_BORDER, NORMAL_LINE_HEIGHT, TextAlign, WhiteSpace,
    parse_background_repeat, parse_border_style, parse_clear, parse_display, parse_float,
    parse_list_style_type, parse_position, parse_text_decoration, parse_vertical_align,
};
use crate::value::{
    Color, Length, Raw, parse_color, parse_color_quirky, parse_length, parse_length_quirky,
};
use crate::{Declaration, Specificity, Stylesheet};

/// Where a declaration came from. Origin outranks specificity in the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    /// The user-agent stylesheet.
    UserAgent,
    /// Attributes like `bgcolor` and `<font color>`, which the era's markup
    /// used in place of CSS. Below author rules, above the UA sheet.
    Presentational,
    /// Stylesheets supplied by the page.
    Author,
    /// A `style` attribute on the element itself, which outranks every rule.
    Inline,
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
    // Quirks mode is a property of the document, decided by its doctype, and
    // changes how values parse (ADR-0004).
    let quirks = doc.is_quirks();
    style_subtree(
        doc,
        doc.root(),
        &root_style,
        &ua,
        author_sheets,
        quirks,
        &mut map,
    );
    map
}

fn style_subtree(
    doc: &Document,
    node: NodeId,
    parent_style: &ComputedStyle,
    ua: &Stylesheet,
    author: &[Stylesheet],
    quirks: bool,
    out: &mut StyleMap,
) {
    let style = if doc.element(node).is_some() {
        let computed = compute(doc, node, parent_style, ua, author, quirks);
        out.styles.insert(node, computed.clone());
        computed
    } else {
        parent_style.clone()
    };

    for &child in doc.children(node) {
        style_subtree(doc, child, &style, ua, author, quirks, out);
    }
}

fn compute(
    doc: &Document,
    node: NodeId,
    parent: &ComputedStyle,
    ua: &Stylesheet,
    author: &[Stylesheet],
    quirks: bool,
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

    // Presentational attributes, which carry most of the era's styling. They
    // sit below author CSS so a stylesheet can always override them, and above
    // the UA sheet so they actually take effect.
    let hints = presentational_hints(doc, node);
    for declaration in &hints {
        order += 1;
        matched.push((
            Precedence {
                important: false,
                origin: Origin::Presentational,
                specificity: Specificity::default(),
                order,
            },
            declaration,
        ));
    }

    // A `style` attribute applies to this element alone and beats every rule.
    let inline = doc
        .element(node)
        .and_then(|element| element.attr("style"))
        .map(crate::parse_style_attribute)
        .unwrap_or_default();
    for declaration in &inline {
        order += 1;
        matched.push((
            Precedence {
                important: declaration.important,
                origin: Origin::Inline,
                specificity: Specificity::default(),
                order,
            },
            declaration,
        ));
    }

    matched.sort_by(|a, b| a.0.cmp(&b.0));

    let mut style = ComputedStyle::inherit_from(parent);
    // The UA sheet gives `display: block` to block-level elements; everything
    // else starts inline, which is the CSS initial value.
    for (_, declaration) in matched {
        apply(&mut style, declaration, parent, quirks);
    }

    // §16.3: an ancestor's decoration is drawn across this element's text too,
    // and this element cannot switch it off. Merged after the cascade rather
    // than inherited before it, so that a `text-decoration: none` here still
    // beats a rule that would otherwise underline *this* element.
    style.text_decoration.underline |= parent.text_decoration.underline;
    style.text_decoration.line_through |= parent.text_decoration.line_through;
    style.text_decoration.overline |= parent.text_decoration.overline;

    style
}

/// Reads a `url(...)` value in either of its two token forms.
///
/// Unquoted it arrives as a URL token; quoted, the tokenizer sees an ordinary
/// function call. Both are ordinary in the wild.
fn url_value(raw: &Raw) -> Option<&str> {
    match raw {
        Raw::Url(url) => Some(url),
        Raw::Function(name, args) if name == "url" => match args.first() {
            Some(Raw::Str(url)) => Some(url),
            Some(Raw::Url(url)) => Some(url),
            _ => None,
        },
        _ => None,
    }
}

/// Applies one declaration to a style in progress.
///
/// Unknown properties and unparseable values are dropped, which is the
/// specified behaviour and the only workable one for the real web.
fn apply(
    style: &mut ComputedStyle,
    declaration: &Declaration,
    parent: &ComputedStyle,
    quirks: bool,
) {
    let values = &declaration.value;
    let Some(first) = values.first() else { return };
    // Shadow the strict parsers so every property below picks up the
    // quirks-mode forms without each having to remember to ask.
    let parse_length = |raw: &Raw| parse_length_quirky(raw, quirks);
    let parse_color = |raw: &Raw| parse_color_quirky(raw, quirks);

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
        "background-color" => {
            if let Some(color) = parse_color(first) {
                style.background_color = color;
            }
        }
        "background-image" => {
            // `none` is the way to remove an inherited-looking background that
            // a broader rule set; it must clear rather than be ignored.
            style.background_image = url_value(first).map(str::to_owned);
        }
        "background-repeat" => {
            if let Raw::Ident(name) = first
                && let Some(repeat) = parse_background_repeat(name)
            {
                style.background_repeat = repeat;
            }
        }
        // The shorthand sets everything it names and resets everything it does
        // not — that reset is the whole reason `background: white` reliably
        // clears an image, and skipping it leaves the image showing through.
        "background" => {
            style.background_color = Color::TRANSPARENT;
            style.background_image = None;
            style.background_repeat = BackgroundRepeat::Repeat;
            for value in values {
                if let Some(url) = url_value(value) {
                    style.background_image = Some(url.to_owned());
                } else if let Raw::Ident(name) = value
                    && let Some(repeat) = parse_background_repeat(name)
                {
                    style.background_repeat = repeat;
                } else if let Some(color) = parse_color(value) {
                    style.background_color = color;
                }
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
                    // Browsers spell this `-webkit-center`; it centres block
                    // children as well as text, which is what `<center>` and
                    // `align="center"` mean and what plain `center` does not.
                    "-webkit-center" | "-moz-center" => TextAlign::CenterBlocks,
                    "right" => TextAlign::Right,
                    "justify" => TextAlign::Justify,
                    _ => TextAlign::Left,
                };
            }
        }
        // Replaces, like any other property: this is the ordinary cascade, and
        // a `text-decoration: none` that lost to it would be unable to turn off
        // the underline the UA sheet gives a link. An *ancestor's* decoration
        // is a separate matter, merged in after the cascade has settled.
        "text-decoration" => {
            let words: Vec<String> = values
                .iter()
                .filter_map(|raw| match raw {
                    Raw::Ident(name) => Some(name.clone()),
                    _ => None,
                })
                .collect();
            style.text_decoration = parse_text_decoration(&words);
        }
        // `list-style` is a shorthand; only the type is modelled, so scan the
        // whole value for a keyword we recognise rather than reading the first.
        "list-style-type" | "list-style" => {
            if let Some(kind) = values.iter().find_map(|raw| match raw {
                Raw::Ident(name) => parse_list_style_type(name),
                _ => None,
            }) {
                style.list_style_type = kind;
            }
        }
        // Two values are allowed — horizontal then vertical — but a table
        // using different ones is vanishingly rare, so the first is used for
        // both rather than modelling an axis that nothing sets.
        "vertical-align" => {
            if let Raw::Ident(name) = first
                && let Some(align) = parse_vertical_align(name)
            {
                style.vertical_align = align;
            }
        }
        "border-spacing" => {
            if let Some(length) = parse_length(first) {
                style.border_spacing = length;
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
        "position" => {
            if let Raw::Ident(name) = first
                && let Some(position) = parse_position(name)
            {
                style.position = position;
            }
        }
        "top" | "right" | "bottom" | "left" => {
            if let Some(length) = parse_length(first) {
                match declaration.name.as_str() {
                    "top" => style.offsets.top = length,
                    "right" => style.offsets.right = length,
                    "bottom" => style.offsets.bottom = length,
                    _ => style.offsets.left = length,
                }
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
        "margin" => style.margin = parse_edges(values, quirks),
        "padding" => style.padding = parse_edges(values, quirks),
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
                set_edge(&mut style.margin, side, first, quirks);
            } else if let Some(side) = name.strip_prefix("padding-") {
                set_edge(&mut style.padding, side, first, quirks);
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
fn parse_edges(values: &[Raw], quirks: bool) -> Edges {
    let lengths: Vec<Length> = values
        .iter()
        .filter_map(|raw| parse_length_quirky(raw, quirks))
        .collect();
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

/// Declarations implied by an element's presentational attributes.
///
/// The era's pages carry most of their styling here rather than in CSS, so a
/// browser that ignores these renders them as unstyled text. Values are parsed
/// through the ordinary value machinery, and colours go through the quirky
/// parser because `bgcolor="dfe8ff"` without a `#` is the common form.
fn presentational_hints(doc: &Document, node: NodeId) -> Vec<Declaration> {
    let Some(element) = doc.element(node) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut push = |name: &str, value: &str| {
        let mut input = cssparser::ParserInput::new(value);
        let mut parser = cssparser::Parser::new(&mut input);
        let components = crate::value::read_components(&mut parser);
        if !components.is_empty() {
            out.push(Declaration {
                name: name.to_owned(),
                value: components,
                important: false,
            });
        }
    };

    let tag = element.local_name();
    if let Some(color) = element.attr("bgcolor") {
        push("background-color", &attr_color(color));
    }
    // `<body background="tile.gif">` is how the era's tiled backgrounds were
    // almost always written — the CSS property existed but the attribute is
    // what pages used. Quoted so a filename with parentheses or spaces still
    // makes it through the tokenizer intact.
    if matches!(tag, "body" | "table" | "td" | "th" | "tr")
        && let Some(source) = element.attr("background")
        && !source.trim().is_empty()
    {
        push(
            "background-image",
            &format!("url(\"{}\")", source.trim().replace('"', "%22")),
        );
    }
    // `text` on <body> sets the document's foreground colour.
    if tag == "body"
        && let Some(color) = element.attr("text")
    {
        push("color", &attr_color(color));
    }
    if let Some(align) = element.attr("align") {
        match align.trim().to_ascii_lowercase().as_str() {
            // On an image or table, `align` floats it; elsewhere it aligns text.
            "left" | "right" if matches!(tag, "img" | "table") => push("float", align),
            // `<table align="center">` centres the *table*, not its contents.
            // Mapping it to `text-align` centres every line of text on the
            // page, because `text-align` inherits and a table of this era
            // wraps the whole document.
            "center" if tag == "table" => {
                push("margin-left", "auto");
                push("margin-right", "auto");
            }
            // `align="center"` centres the block children too, which is how
            // `<div align="center"><table>` centres its table.
            "center" | "middle" => push("text-align", "-webkit-center"),
            "left" | "right" | "justify" => push("text-align", align),
            _ => {}
        }
    }
    // `valign` is the cell's vertical alignment. `valign="top"` in particular
    // is on nearly every layout table's cells, to stop a short column being
    // centred against a long one.
    if matches!(tag, "td" | "th" | "tr")
        && let Some(align) = element.attr("valign")
        && let value @ ("top" | "middle" | "bottom" | "baseline") =
            align.trim().to_ascii_lowercase().as_str()
    {
        push("vertical-align", value);
    }
    // `hspace` and `vspace` are margins, and are how the era's markup kept
    // text off a floated image.
    if tag == "img" {
        if let Some(space) = element.attr("hspace") {
            let space = attr_length(space);
            push("margin-left", &space);
            push("margin-right", &space);
        }
        if let Some(space) = element.attr("vspace") {
            let space = attr_length(space);
            push("margin-top", &space);
            push("margin-bottom", &space);
        }
    }
    // `<body link>` colours every link in the document, so a link has to look
    // up to the body for it — the same shape as a cell reading its table's
    // `cellpadding`. `vlink` and `alink` need history and interaction, neither
    // of which exists yet, so they are deliberately not read.
    if tag == "a"
        && element.attr("href").is_some()
        && let Some(color) = body_of(doc).and_then(|body| body.attr("link"))
    {
        push("color", &attr_color(color));
    }
    if let Some(color) = element.attr("color")
        && tag == "font"
    {
        push("color", &attr_color(color));
    }
    if tag == "font"
        && let Some(face) = element.attr("face")
    {
        push("font-family", face);
    }
    // <font size> is a 1-7 scale, or a relative "+2"/"-1".
    if tag == "font"
        && let Some(size) = element.attr("size")
        && let Some(keyword) = font_size_keyword(size)
    {
        push("font-size", keyword);
    }
    // Table sizing attributes, which predate CSS entirely.
    if matches!(tag, "table" | "td" | "th" | "col")
        && let Some(width) = element.attr("width")
    {
        push("width", &attr_length(width));
    }
    if matches!(tag, "table" | "td" | "th" | "tr")
        && let Some(height) = element.attr("height")
    {
        push("height", &attr_length(height));
    }
    // `cellpadding` and `border` are written on the table but describe its
    // cells, so a cell has to look upwards to find them. `<table border="1">`
    // in particular is the single most recognisable piece of the era's markup:
    // it draws a rule around the table *and* around every cell.
    if matches!(tag, "td" | "th")
        && let Some(table) = enclosing_table(doc, node)
    {
        if let Some(padding) = table.attr("cellpadding") {
            push("padding", &attr_length(padding));
        }
        if table_border_width(table).is_some() {
            // The cell rule is always 1px however thick the table's own is,
            // which is what the attribute meant.
            push("border", "1px solid");
        }
    }
    if tag == "table"
        && let Some(width) = table_border_width(element)
    {
        push("border", &format!("{width}px solid"));
    }
    // `cellspacing` is `border-spacing` by another name, and the attribute is
    // what the era's markup used. `cellspacing="0"` in particular is how a
    // table used for page layout closed the gaps between its cells — leaving
    // the 2px initial value there puts a visible seam through the layout.
    if tag == "table"
        && let Some(spacing) = element.attr("cellspacing")
    {
        push("border-spacing", &attr_length(spacing));
    }
    out
}

/// The document's `body`, if it has one.
fn body_of(doc: &Document) -> Option<&ElementData> {
    doc.find_element("body").and_then(|node| doc.element(node))
}

/// The `table` element enclosing a cell, skipping the row and row group.
fn enclosing_table(doc: &Document, node: NodeId) -> Option<&ElementData> {
    let mut current = doc.node(node).parent;
    while let Some(id) = current {
        let element = doc.element(id)?;
        if element.local_name() == "table" {
            return Some(element);
        }
        current = doc.node(id).parent;
    }
    None
}

/// The width `<table border>` asks for, or `None` when it asks for no border.
///
/// `border="0"` is the era's idiom for a table used purely as a layout grid,
/// and it must not draw anything.
fn table_border_width(element: &ElementData) -> Option<f32> {
    let value = element.attr("border")?.trim();
    // A valueless `border` attribute means 1px.
    let width: f32 = if value.is_empty() {
        1.0
    } else {
        value.parse().ok()?
    };
    (width > 0.0).then_some(width)
}

/// Normalises a colour attribute into CSS syntax.
///
/// `bgcolor="dfe8ff"` is the common form and is not CSS: the attribute has its
/// own grammar, so a bare hex string is valid here whatever mode the document
/// is in. Adding the `#` lets the ordinary colour parser take it from there.
fn attr_color(value: &str) -> String {
    let trimmed = value.trim();
    if matches!(trimmed.len(), 3 | 6) && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("#{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// Normalises a length attribute into CSS syntax.
///
/// `width="300"` means 300 pixels regardless of document mode, for the same
/// reason: the attribute is not a CSS declaration and never had CSS's units.
fn attr_length(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.ends_with('%') {
        return trimmed.to_owned();
    }
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit() || c == '.') {
        format!("{trimmed}px")
    } else {
        trimmed.to_owned()
    }
}

/// Maps a `<font size>` value onto a CSS absolute-size keyword.
fn font_size_keyword(value: &str) -> Option<&'static str> {
    let value = value.trim();
    // Relative forms are resolved against size 3, the default.
    let level: i32 = if let Some(rest) = value.strip_prefix('+') {
        3 + rest.parse::<i32>().ok()?
    } else if let Some(rest) = value.strip_prefix('-') {
        3 - rest.parse::<i32>().ok()?
    } else {
        value.parse().ok()?
    };
    Some(match level.clamp(1, 7) {
        1 => "x-small",
        2 => "small",
        3 => "medium",
        4 => "large",
        5 => "x-large",
        6 => "xx-large",
        _ => "xx-large",
    })
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

fn set_edge(edges: &mut Edges, side: &str, raw: &Raw, quirks: bool) {
    let Some(length) = parse_length_quirky(raw, quirks) else {
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
    use crate::style::{BackgroundRepeat, Display, ListStyleType, VerticalAlign};

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

    /// Same helper as `style_of`, but without a doctype, so the parser puts the
    /// document in quirks mode.
    fn quirks_style_of(html: &str, css: &str, tag: &str) -> ComputedStyle {
        let doc = dom::parse(html);
        assert!(doc.is_quirks(), "fixture should be in quirks mode");
        let sheets = [Stylesheet::parse(css)];
        let map = cascade(&doc, &sheets);
        let node = doc.find_element(tag).expect("element present");
        map.get(node).expect("element styled").clone()
    }

    fn standards_style_of(html: &str, css: &str, tag: &str) -> ComputedStyle {
        let doc = dom::parse(&format!("<!doctype html>{html}"));
        assert!(!doc.is_quirks(), "fixture should be in standards mode");
        let sheets = [Stylesheet::parse(css)];
        let map = cascade(&doc, &sheets);
        let node = doc.find_element(tag).expect("element present");
        map.get(node).expect("element styled").clone()
    }

    #[test]
    fn quirks_mode_accepts_a_unitless_length() {
        // `width: 100` is invalid in standards mode and everywhere in the era's
        // markup. Rejecting it collapses the page it was meant to size.
        let quirky = quirks_style_of("<p>x</p>", "p { width: 100; margin: 20 }", "p");
        assert_eq!(quirky.width, Length::Px(100.0));
        assert_eq!(quirky.margin.top, Length::Px(20.0));

        let strict = standards_style_of("<p>x</p>", "p { width: 100; margin: 20 }", "p");
        assert_eq!(strict.width, Length::Auto, "standards mode must reject it");
        assert_eq!(strict.margin.top, Length::Px(0.0));
    }

    #[test]
    fn quirks_mode_accepts_a_hashless_hex_colour() {
        let quirky = quirks_style_of("<p>x</p>", "p { color: ff0000 }", "p");
        assert_eq!(quirky.color, crate::Color::rgb(255, 0, 0));

        let strict = standards_style_of("<p>x</p>", "p { color: ff0000 }", "p");
        assert_eq!(
            strict.color,
            crate::Color::BLACK,
            "standards mode must reject it"
        );
    }

    #[test]
    fn a_three_digit_hashless_colour_expands_like_a_hash_one() {
        assert_eq!(
            quirks_style_of("<p>x</p>", "p { color: f00 }", "p").color,
            crate::Color::rgb(255, 0, 0)
        );
    }

    #[test]
    fn a_hashless_colour_starting_with_digits_still_parses() {
        // `00ff00` tokenises as a dimension — the number 00 with unit ff00 —
        // not as an identifier, so it needs its own path.
        assert_eq!(
            quirks_style_of("<p>x</p>", "p { color: 00ff00 }", "p").color,
            crate::Color::rgb(0, 255, 0)
        );
    }

    #[test]
    fn a_keyword_is_not_mistaken_for_a_hashless_colour() {
        // `dad` and `beaded` are hex-looking words; `solid` and `inherit` are
        // not. Only three- and six-digit strings may be read as colours, and a
        // real keyword must keep winning.
        let style = quirks_style_of("<p>x</p>", "p { color: red; display: block }", "p");
        assert_eq!(style.color, crate::Color::rgb(255, 0, 0));
        assert_eq!(style.display, Display::Block);
    }

    #[test]
    fn quirks_parsing_does_not_leak_into_standards_documents() {
        // The whole point of gating: a standards-mode page must not silently
        // gain permissive parsing.
        let strict = standards_style_of("<p>x</p>", "p { padding: 5 }", "p");
        assert_eq!(strict.padding.top, Length::Px(0.0));
    }

    #[test]
    fn bgcolor_sets_a_background() {
        // Hash-less is the common form, so it has to work in both modes: the
        // attribute value is not CSS and is not subject to CSS strictness.
        let style = standards_style_of(r##"<body bgcolor="#ff0000">x</body>"##, "", "body");
        assert_eq!(style.background_color, crate::Color::rgb(255, 0, 0));

        let hashless = standards_style_of(r#"<body bgcolor="00ff00">x</body>"#, "", "body");
        assert_eq!(hashless.background_color, crate::Color::rgb(0, 255, 0));
    }

    #[test]
    fn author_css_overrides_a_presentational_attribute() {
        // The ordering that matters: attributes sit below author rules so a
        // stylesheet can always win, and above the UA sheet so they take
        // effect at all.
        let style = standards_style_of(
            r##"<body bgcolor="#ff0000">x</body>"##,
            "body { background-color: #0000ff }",
            "body",
        );
        assert_eq!(style.background_color, crate::Color::rgb(0, 0, 255));
    }

    #[test]
    fn a_style_attribute_beats_an_author_rule() {
        let style = standards_style_of(
            r#"<p id="a" style="color: lime">x</p>"#,
            "#a { color: red }",
            "p",
        );
        assert_eq!(style.color, crate::Color::rgb(0, 255, 0));
    }

    #[test]
    fn body_text_attribute_sets_the_foreground_colour() {
        let style = standards_style_of(r##"<body text="#0000ff">x</body>"##, "", "body");
        assert_eq!(style.color, crate::Color::rgb(0, 0, 255));
    }

    #[test]
    fn align_becomes_text_align_but_floats_an_image() {
        // `align="center"` centres block children too, so it is the value that
        // does both — not the one a stylesheet gets from `text-align: center`.
        let paragraph = standards_style_of(r#"<p align="center">x</p>"#, "", "p");
        assert_eq!(paragraph.text_align, TextAlign::CenterBlocks);
        assert!(paragraph.text_align.centres_text());

        let right = standards_style_of(r#"<p align="right">x</p>"#, "", "p");
        assert_eq!(right.text_align, TextAlign::Right);

        // On an image the same attribute means float, not text alignment.
        let image = standards_style_of(r#"<body><img align="right"></body>"#, "", "img");
        assert_eq!(image.float, crate::style::Float::Right);
    }

    #[test]
    fn font_size_maps_the_one_to_seven_scale() {
        let big = standards_style_of(r#"<font size="6">x</font>"#, "", "font");
        let small = standards_style_of(r#"<font size="1">x</font>"#, "", "font");
        let default = standards_style_of(r#"<font size="3">x</font>"#, "", "font");
        assert!(big.font_size > default.font_size);
        assert!(small.font_size < default.font_size);
        assert_eq!(default.font_size, 16.0, "size 3 is the default size");
    }

    #[test]
    fn a_relative_font_size_resolves_against_the_default() {
        let plus = standards_style_of(r#"<font size="+2">x</font>"#, "", "font");
        let explicit = standards_style_of(r#"<font size="5">x</font>"#, "", "font");
        assert_eq!(plus.font_size, explicit.font_size, "+2 is size 5");
    }

    #[test]
    fn font_color_and_face_apply() {
        let style = standards_style_of(
            r##"<font color="#ff00ff" face="Courier">x</font>"##,
            "",
            "font",
        );
        assert_eq!(style.color, crate::Color::rgb(255, 0, 255));
        // Identifiers are lowercased by the tokenizer; family matching is
        // case-insensitive, so this is the stored form.
        assert_eq!(style.font_family.families, vec!["courier".to_owned()]);
    }

    #[test]
    fn table_width_attribute_sizes_the_table() {
        let style = standards_style_of(
            r#"<table width="300"><tr><td>x</td></tr></table>"#,
            "",
            "table",
        );
        // Attribute values are not CSS, so a bare number is a length here even
        // in standards mode.
        assert_eq!(style.width, Length::Px(300.0));
    }

    #[test]
    fn cellpadding_pads_the_cells_not_the_table() {
        // The attribute is written on the table but describes its cells. Put it
        // on the table itself and the whole grid shifts inwards while the text
        // stays jammed against the cell edges — the opposite of what it means.
        let html = r#"<table cellpadding="6"><tr><td>x</td></tr></table>"#;
        let cell = standards_style_of(html, "", "td");
        assert_eq!(cell.padding.left, Length::Px(6.0));
        assert_eq!(cell.padding.top, Length::Px(6.0));

        let table = standards_style_of(html, "", "table");
        assert_eq!(
            table.padding.left,
            Length::Px(0.0),
            "the table is not padded"
        );
    }

    #[test]
    fn table_border_attribute_rules_the_table_and_its_cells() {
        // `<table border="1">` draws a rule around the table and around every
        // cell, which is why the era's tables look the way they do.
        let html = r#"<table border="1"><tr><td>x</td></tr></table>"#;
        let table = standards_style_of(html, "", "table");
        assert_eq!(table.border.left.used_width(table.font_size), 1.0);

        let cell = standards_style_of(html, "", "td");
        assert_eq!(
            cell.border.top.used_width(cell.font_size),
            1.0,
            "every cell is ruled too"
        );

        // A thicker table border still gives cells a 1px rule.
        let thick = r#"<table border="4"><tr><td>x</td></tr></table>"#;
        assert_eq!(
            standards_style_of(thick, "", "table")
                .border
                .left
                .used_width(16.0),
            4.0
        );
        assert_eq!(
            standards_style_of(thick, "", "td")
                .border
                .left
                .used_width(16.0),
            1.0
        );
    }

    #[test]
    fn border_zero_draws_nothing() {
        // `border="0"` is how a table used purely for page layout said "do not
        // draw me". Getting this wrong puts a grid over the whole page.
        let html = r#"<table border="0"><tr><td>x</td></tr></table>"#;
        assert_eq!(
            standards_style_of(html, "", "table")
                .border
                .left
                .used_width(16.0),
            0.0
        );
        assert_eq!(
            standards_style_of(html, "", "td")
                .border
                .left
                .used_width(16.0),
            0.0
        );
    }

    #[test]
    fn background_image_parses_both_url_forms() {
        for css in [
            "body { background-image: url(tile.gif) }",
            r#"body { background-image: url("tile.gif") }"#,
            "body { background-image: url('tile.gif') }",
        ] {
            assert_eq!(
                style_of("<body>x</body>", css, "body")
                    .background_image
                    .as_deref(),
                Some("tile.gif"),
                "failed for {css}"
            );
        }
    }

    #[test]
    fn the_background_shorthand_resets_what_it_does_not_name() {
        // Without the reset, `background: white` leaves an earlier rule's tile
        // showing through — the shorthand is how a page clears one.
        let style = style_of(
            "<body>x</body>",
            "body { background-image: url(tile.gif) } body { background: #ffffff }",
            "body",
        );
        assert_eq!(style.background_image, None);
        assert_eq!(style.background_color, crate::Color::WHITE);
    }

    #[test]
    fn the_background_shorthand_reads_its_parts_in_any_order() {
        let style = style_of(
            "<body>x</body>",
            "body { background: no-repeat #ff0000 url(tile.gif) }",
            "body",
        );
        assert_eq!(style.background_image.as_deref(), Some("tile.gif"));
        assert_eq!(style.background_color, crate::Color::rgb(255, 0, 0));
        assert_eq!(style.background_repeat, BackgroundRepeat::NoRepeat);
    }

    #[test]
    fn the_background_attribute_sets_a_tile() {
        // How the era actually wrote it. The CSS property existed; the
        // attribute is what pages used.
        let style =
            standards_style_of(r#"<body background="images/tile.gif">x</body>"#, "", "body");
        assert_eq!(style.background_image.as_deref(), Some("images/tile.gif"));
    }

    #[test]
    fn a_background_image_is_not_inherited() {
        // Inheriting it would draw the tile again on every descendant box,
        // which is both wrong and expensive.
        let doc = dom::parse("<body background=\"tile.gif\"><p>x</p></body>");
        let map = cascade(&doc, &[]);
        let paragraph = doc.find_element("p").expect("p");
        assert_eq!(map.get(paragraph).expect("styled").background_image, None);
    }

    #[test]
    fn only_an_anchor_with_an_href_is_styled_as_a_link() {
        // `<a name="x">` was how in-page destinations were written. Painting
        // one blue and underlined tells the reader to click something that
        // does nothing.
        let link = style_of(r#"<a href="x.html">x</a>"#, "", "a");
        assert!(link.text_decoration.underline);
        assert_eq!(link.color, crate::Color::rgb(0, 0, 238));

        let anchor = style_of(r#"<a name="here">x</a>"#, "", "a");
        assert!(!anchor.text_decoration.underline);
        assert_eq!(anchor.color, crate::Color::BLACK);
    }

    #[test]
    fn a_page_can_turn_off_the_default_underline() {
        let style = style_of(
            r#"<a href="x.html">x</a>"#,
            "a { text-decoration: none }",
            "a",
        );
        assert!(
            !style.text_decoration.underline,
            "an author rule must beat the UA sheet on the same element"
        );
    }

    #[test]
    fn a_decoration_reaches_descendants_and_cannot_be_removed_by_them() {
        // §16.3: the rule belongs to the ancestor and is drawn across all of
        // its inline content, so a link stays underlined through a <b> inside
        // it — even one that asks for no decoration.
        let doc = dom::parse(r#"<a href="x"><b>bold</b><i>italic</i></a>"#);
        let sheets = [Stylesheet::parse("b { text-decoration: none }")];
        let map = cascade(&doc, &sheets);
        for tag in ["b", "i"] {
            let node = doc.find_element(tag).expect("element present");
            assert!(
                map.get(node).expect("styled").text_decoration.underline,
                "<{tag}> inside a link must stay underlined"
            );
        }
    }

    #[test]
    fn attribute_selectors_match_by_presence_and_value() {
        let html = r#"<p class="a b" lang="en-GB" title="x">t</p>"#;
        let matched = |css: &str| style_of(html, css, "p").color == crate::Color::rgb(255, 0, 0);

        assert!(matched("p[title] { color: red }"), "presence");
        assert!(matched(r#"p[title="x"] { color: red }"#), "exact");
        assert!(matched("p[class~=b] { color: red }"), "one of the words");
        assert!(matched("p[lang|=en] { color: red }"), "language prefix");

        assert!(!matched("p[href] { color: red }"), "absent attribute");
        assert!(!matched(r#"p[title="y"] { color: red }"#), "wrong value");
        assert!(!matched("p[class~=ab] { color: red }"), "not a whole word");
        assert!(
            !matched("p[lang|=e] { color: red }"),
            "a prefix must end at a hyphen"
        );
    }

    #[test]
    fn an_attribute_selector_counts_as_a_class_for_specificity() {
        let style = style_of(
            r#"<p title="x">t</p>"#,
            "p[title] { color: red } p { color: lime }",
            "p",
        );
        assert_eq!(style.color, crate::Color::rgb(255, 0, 0));
    }

    #[test]
    fn a_list_takes_its_marker_from_its_type_and_passes_it_down() {
        let items = style_of("<ul><li>x</li></ul>", "", "li");
        assert_eq!(items.list_style_type, ListStyleType::Disc);

        let ordered = style_of("<ol><li>x</li></ol>", "", "li");
        assert_eq!(ordered.list_style_type, ListStyleType::Decimal);

        // Nesting steps through the bullets so the levels are tellable apart.
        let doc = dom::parse("<ul><li><ul><li><ul><li>x</li></ul></li></ul></li></ul>");
        let map = cascade(&doc, &[]);
        let types: Vec<ListStyleType> = doc
            .descendants(doc.root())
            .into_iter()
            .filter(|&node| {
                doc.element(node)
                    .is_some_and(|element| element.local_name() == "ul")
            })
            .filter_map(|node| map.get(node).map(|style| style.list_style_type))
            .collect();
        assert_eq!(
            types,
            vec![
                ListStyleType::Disc,
                ListStyleType::Circle,
                ListStyleType::Square
            ]
        );
    }

    #[test]
    fn a_row_can_carry_its_own_background() {
        // Striped tables put the colour on `<tr>`.
        let style = standards_style_of(
            r##"<table><tr bgcolor="#c0c0c0"><td>x</td></tr></table>"##,
            "",
            "tr",
        );
        assert_eq!(style.background_color, crate::Color::rgb(192, 192, 192));
    }

    #[test]
    fn a_centred_table_gets_auto_margins_not_centred_text() {
        // `text-align` inherits, and a table of this era wraps the whole
        // document — so mapping `align="center"` to it centres every line on
        // the page rather than the table.
        let style = standards_style_of(
            r#"<table align="center"><tr><td>x</td></tr></table>"#,
            "",
            "table",
        );
        assert_eq!(style.margin.left, Length::Auto);
        assert_eq!(style.margin.right, Length::Auto);
        assert_eq!(style.text_align, TextAlign::Left);
    }

    #[test]
    fn valign_sets_a_cells_vertical_alignment() {
        let cell = |markup: &str| standards_style_of(markup, "", "td").vertical_align;
        assert_eq!(
            cell("<table><tr><td>x</td></tr></table>"),
            VerticalAlign::Middle,
            "a cell is middle-aligned by default, which is why valign exists"
        );
        assert_eq!(
            cell(r#"<table><tr><td valign="top">x</td></tr></table>"#),
            VerticalAlign::Top
        );
        assert_eq!(
            cell(r#"<table><tr><td valign="BOTTOM">x</td></tr></table>"#),
            VerticalAlign::Bottom
        );
    }

    #[test]
    fn hspace_and_vspace_are_margins() {
        let style = standards_style_of(r#"<img src="x.png" hspace="8" vspace="4">"#, "", "img");
        assert_eq!(style.margin.left, Length::Px(8.0));
        assert_eq!(style.margin.right, Length::Px(8.0));
        assert_eq!(style.margin.top, Length::Px(4.0));
        assert_eq!(style.margin.bottom, Length::Px(4.0));
    }

    #[test]
    fn the_body_link_attribute_colours_every_link() {
        // It is written once on `<body>` and applies to every link in the
        // document, so a link has to look up to find it.
        let html = r##"<body link="#000080"><p><a href="x.html">go</a></p>
                       <a name="here">not a link</a></body>"##;
        assert_eq!(
            standards_style_of(html, "", "a").color,
            crate::Color::rgb(0, 0, 128)
        );

        let doc = dom::parse(html);
        let map = cascade(&doc, &[]);
        let anchor = doc
            .descendants(doc.root())
            .into_iter()
            .rfind(|&node| {
                doc.element(node)
                    .is_some_and(|element| element.local_name() == "a")
            })
            .expect("the named anchor");
        assert_eq!(
            map.get(anchor).expect("styled").color,
            crate::Color::BLACK,
            "a named anchor is a destination, not a link"
        );
    }

    #[test]
    fn cellspacing_maps_to_border_spacing() {
        let style = standards_style_of(
            r#"<table cellspacing="0"><tr><td>x</td></tr></table>"#,
            "",
            "table",
        );
        assert_eq!(style.border_spacing, Length::Px(0.0));

        let default = standards_style_of("<table><tr><td>x</td></tr></table>", "", "table");
        assert_eq!(default.border_spacing, Length::Px(2.0));
    }

    #[test]
    fn align_floats_an_image_but_aligns_text() {
        // The same attribute means two different things depending on what it is
        // written on, which is a genuine quirk of the era's HTML rather than an
        // inconsistency we could tidy away.
        let image = standards_style_of(r#"<img align="right" src="x.png">"#, "", "img");
        assert_eq!(image.float, crate::style::Float::Right);

        let paragraph = standards_style_of(r#"<p align="right">x</p>"#, "", "p");
        assert_eq!(paragraph.float, crate::style::Float::None);
        assert_eq!(paragraph.text_align, TextAlign::Right);
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
