//! The cascade: matching rules to elements and resolving computed values.

use std::collections::HashMap;

use dom::{Document, ElementData, NodeId};

use crate::selector::PseudoElement;
use crate::style::{
    BackgroundPosition, BackgroundRepeat, BorderSide, BorderStyle, Borders, ComputedStyle,
    DEFAULT_FONT_SIZE, Edges, Float, FontStack, FontStyle, GenericFamily, MEDIUM_BORDER,
    NORMAL_LINE_HEIGHT, TextAlign, WhiteSpace, parse_background_position, parse_background_repeat,
    parse_border_collapse, parse_border_style, parse_caption_side, parse_clear, parse_display,
    parse_float, parse_list_style_type, parse_overflow, parse_position, parse_text_decoration,
    parse_text_transform, parse_vertical_align, parse_visibility,
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
    /// Styles for the boxes a stylesheet asks for that the document does not
    /// contain. Kept beside the element styles rather than in them because a
    /// pseudo-element is a box, with its own colour, font and display, and
    /// folding it into its originator's style would flatten that.
    pseudos: HashMap<(NodeId, PseudoElement), ComputedStyle>,
}

impl StyleMap {
    /// The computed style for a node, if it is a styled element.
    pub fn get(&self, node: NodeId) -> Option<&ComputedStyle> {
        self.styles.get(&node)
    }

    /// The style for one of a node's generated boxes, if it generates one.
    ///
    /// Absent unless the cascade gave it `content`, since a pseudo-element
    /// without content generates no box at all (§12.1) — so a caller can take
    /// the presence of a style here as "this box exists".
    pub fn pseudo(&self, node: NodeId, which: PseudoElement) -> Option<&ComputedStyle> {
        self.pseudos.get(&(node, which))
    }

    /// Takes a node out of the flow, exactly as `display: none` does.
    ///
    /// For the document fallback, which drops the blank lines a page used as
    /// spacing (ADR-0009). No selector can say "the second of two consecutive
    /// line breaks", so this cannot be a rule in the reader sheet; it is
    /// decided after the cascade and applied here.
    ///
    /// Deliberately the only way to change a computed style after the fact. A
    /// general setter would be an invitation to patch the cascade from
    /// anywhere, and then a stylesheet stops being the explanation for what a
    /// page looks like.
    pub fn hide(&mut self, node: NodeId) {
        if let Some(style) = self.styles.get_mut(&node) {
            style.display = crate::style::Display::None;
        }
    }

    /// Raises a node's left and right margins to at least `least` pixels,
    /// leaving a wider margin alone.
    ///
    /// For the page gutter. The UA sheet's `body { margin: 8px }` is a default
    /// and an author's `margin: 0` beats it, which leaves text against the
    /// glass. What is wanted is a floor, and CSS has no way to write one: a UA
    /// rule loses to the author, and an `!important` UA rule would win so hard
    /// that a page could never ask for a *wider* margin either. So the floor is
    /// applied after the cascade, like [`Self::hide`], instead of pretending to
    /// be a stylesheet.
    ///
    /// `available_width` resolves a percentage margin, which has to be measured
    /// against something before it can be compared with a length in pixels.
    pub fn keep_off_the_edges(&mut self, node: NodeId, least: f32, available_width: f32) {
        let Some(style) = self.styles.get_mut(&node) else {
            return;
        };
        let font_size = style.font_size;
        for margin in [&mut style.margin.left, &mut style.margin.right] {
            // An `auto` horizontal margin on a block of automatic width
            // resolves to zero, so it has asked for nothing and the floor
            // applies.
            let asked = match *margin {
                Length::Auto => 0.0,
                length => length.to_px(font_size, available_width),
            };
            if asked < least {
                *margin = Length::Px(least);
            }
        }
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

/// Whose colours a rendering uses where the author has named one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colours {
    /// The author's, wherever they wrote them.
    Authors,
    /// The sheets': a colour written into the markup is ignored, and the
    /// stylesheets decide.
    ///
    /// For the document fallback, where this is not a preference but a
    /// correctness problem. That rendering drops the author's sheet and
    /// applies the reader's, which paints a dark page — and dropping a sheet
    /// does not drop the colours in the markup. Wikipedia's taxobox carries
    /// its bands as `<tr style="background-color: rgb(235,235,210)">` on
    /// every row, so the reading view came out as pale strips of near-white
    /// text on near-white: less legible than the page it was rescuing.
    ///
    /// Both properties and not only the background, because they are legible
    /// only as a pair: an author's dark `color` left standing over the
    /// reader's dark page is the same bug upside down.
    ///
    /// Only `style` and presentational attributes are ignored — the sheets
    /// handed in still apply in full. On this path the author has none, and
    /// the one sheet there is belongs to the reader.
    Readers,
}

/// Resolves computed styles for the whole document.
///
/// Sheets are applied in the order given, after the user-agent sheet.
pub fn cascade(doc: &Document, author_sheets: &[Stylesheet]) -> StyleMap {
    cascade_as(doc, author_sheets, 1.0, Colours::Authors)
}

/// The same, at a zoom factor.
///
/// Zoom multiplies every pixel length as it is computed — the font sizes, the
/// margins, the borders — and then nothing downstream knows about it. A glyph
/// shaped at twice the size is twice as sharp, where a rendering scaled up
/// afterwards is twice as blurry; and because the viewport keeps its real
/// width, text reflows to the window instead of running off the side of it.
/// That is what a browser means by zoom, as against a magnifying glass.
pub fn cascade_at(doc: &Document, author_sheets: &[Stylesheet], zoom: f32) -> StyleMap {
    cascade_as(doc, author_sheets, zoom, Colours::Authors)
}

/// The whole of it: at a zoom, and saying whose colours win.
///
/// The document fallback wants both at once — a reader who has zoomed in is
/// still reading on the reader's page.
pub fn cascade_as(
    doc: &Document,
    author_sheets: &[Stylesheet],
    zoom: f32,
    colours: Colours,
) -> StyleMap {
    let ua = Stylesheet::parse(crate::ua::UA_STYLESHEET);
    let mut map = StyleMap::default();
    // The root carries the zoomed default, so an element that says nothing
    // about its font size inherits one that has already been scaled — and with
    // it every `em` measured against it. Most text on most pages is this case:
    // without it, zoom would move the headings and leave the prose alone.
    //
    // The line height is not set here, and deliberately: the UA sheet gives
    // `body` a unitless `line-height: 1.2`, which resolves against each
    // element's own font size — already zoomed — so everything inside the body
    // is covered, and there is nothing outside it. A second scaling here was
    // dead code, and mutation testing said so.
    let root_style = ComputedStyle {
        font_size: DEFAULT_FONT_SIZE * zoom,
        ..ComputedStyle::default()
    };
    let rules = Rules {
        ua: &ua,
        sheets: author_sheets,
        // Quirks mode is a property of the document, decided by its doctype,
        // and changes how values parse (ADR-0004).
        quirks: doc.is_quirks(),
        zoom,
        colours,
    };
    style_subtree(doc, doc.root(), &root_style, &rules, &mut map);
    map
}

/// What the cascade works from, fixed for the whole of one document.
struct Rules<'a> {
    /// The user-agent sheet.
    ua: &'a Stylesheet,
    /// The sheets handed in: the author's, or the reader's on a document
    /// rendering.
    sheets: &'a [Stylesheet],
    /// Whether the document is in quirks mode.
    quirks: bool,
    /// How much bigger than its own pixels the page is drawn.
    zoom: f32,
    /// Whose colours win where the markup names one.
    colours: Colours,
}

/// Whether a property names a colour the reader's sheet should be choosing.
///
/// Backgrounds and foregrounds together, because they are only legible as a
/// pair. Not borders: a border the reader cannot see against the page is a
/// line missing, not a line of text lost, and leaving them alone keeps a
/// table's rules visible where the author drew them.
fn names_a_colour(name: &str) -> bool {
    matches!(
        name,
        "color" | "background" | "background-color" | "background-image"
    )
}

fn style_subtree(
    doc: &Document,
    node: NodeId,
    parent_style: &ComputedStyle,
    rules: &Rules,
    out: &mut StyleMap,
) {
    let style = if doc.element(node).is_some() {
        let computed = compute(doc, node, parent_style, rules, None);
        out.styles.insert(node, computed.clone());
        // §12.1: a pseudo-element generates a box only when `content` gives it
        // one. Computing the style and then throwing it away when there is no
        // content keeps that decision in one place, and lets every later stage
        // read "a style exists here" as "this box exists".
        for which in [PseudoElement::Before, PseudoElement::After] {
            let generated = compute(doc, node, &computed, rules, Some(which));
            if generated.content.is_some() {
                out.pseudos.insert((node, which), generated);
            }
        }
        computed
    } else {
        parent_style.clone()
    };

    for &child in doc.children(node) {
        style_subtree(doc, child, &style, rules, out);
    }
}

fn compute(
    doc: &Document,
    node: NodeId,
    parent: &ComputedStyle,
    rules: &Rules,
    pseudo: Option<PseudoElement>,
) -> ComputedStyle {
    let (quirks, zoom) = (rules.quirks, rules.zoom);
    let mut matched: Vec<(Precedence, &Declaration)> = Vec::new();
    let mut order = 0usize;

    for (sheet, origin) in std::iter::once((rules.ua, Origin::UserAgent))
        .chain(rules.sheets.iter().map(|s| (s, Origin::Author)))
    {
        for rule in &sheet.rules {
            let best = rule
                .selectors
                .iter()
                // A rule addressing `::before` styles that box and nothing
                // else, and a rule addressing no pseudo-element styles the
                // element and not its generated boxes. `matches` answers for
                // the originating element in both cases, so this is the only
                // thing keeping them apart.
                .filter(|selector| selector.pseudo == pseudo && selector.matches(doc, node))
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
    // Both these and the `style` attribute belong to the *element*. A
    // `bgcolor` is not also a request to paint the box `::before` generates,
    // so a pseudo-element takes neither.
    let hints = if pseudo.is_none() {
        presentational_hints(doc, node)
    } else {
        Vec::new()
    };
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
        .filter(|_| pseudo.is_none())
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

    if rules.colours == Colours::Readers {
        matched.retain(|(precedence, declaration)| {
            !matches!(precedence.origin, Origin::Presentational | Origin::Inline)
                || !names_a_colour(&declaration.name)
        });
    }

    matched.sort_by(|a, b| a.0.cmp(&b.0));

    let mut style = ComputedStyle::inherit_from(parent);
    // The UA sheet gives `display: block` to block-level elements; everything
    // else starts inline, which is the CSS initial value.
    for (_, declaration) in matched {
        apply(
            &mut style,
            declaration,
            parent,
            quirks,
            zoom,
            doc.element(node),
        );
    }

    // §16.3: an ancestor's decoration is drawn across this element's text too,
    // and this element cannot switch it off. Merged after the cascade rather
    // than inherited before it, so that a `text-decoration: none` here still
    // beats a rule that would otherwise underline *this* element.
    style.text_decoration.underline |= parent.text_decoration.underline;
    style.text_decoration.line_through |= parent.text_decoration.line_through;
    style.text_decoration.overline |= parent.text_decoration.overline;

    // §9.7: an absolutely positioned box is not a float, whatever `float`
    // says. Both properties take the box out of normal flow and each has its
    // own machinery for placing it, so a box that claims both gets placed
    // twice and drawn twice — which is exactly what
    // `position-absolute-008.xht` does, with `float: right` and
    // `position: absolute` on one div.
    //
    // Applied here rather than in layout because it is a rule about the
    // *computed value*, and because layout asks about `float` from several
    // places that would each have to remember to ask about `position` first.
    if style.position.is_out_of_flow() {
        style.float = Float::None;
    }

    style
}

/// Whether a value in the `background` shorthand belongs to the position.
///
/// Only asked inside that shorthand, where the question is answerable: nothing
/// else it takes is a length or a percentage, and none of the four edge
/// keywords is a colour or a repeat mode. `center` is the one that looks like
/// it might collide and does not.
///
/// Deliberately loose about *which* lengths are valid — `parse_background_position`
/// decides that, and it sees the pair. This only has to sort tokens into piles.
fn is_position_component(raw: &Raw) -> bool {
    match raw {
        Raw::Dimension { .. } | Raw::Percentage(_) => true,
        Raw::Ident(name) => {
            matches!(
                name.as_str(),
                "left" | "right" | "top" | "bottom" | "center"
            )
        }
        _ => false,
    }
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
    zoom: f32,
    // The originating element, for the one property that reads the document
    // rather than only its own value: `content: attr(href)`.
    element: Option<&ElementData>,
) {
    let values = &declaration.value;
    let Some(first) = values.first() else { return };
    // Shadow the strict parsers so every property below picks up the
    // quirks-mode forms without each having to remember to ask.
    // Zoom is applied here, at the one place every property's lengths come
    // through, rather than at each of the forty that use them. Shadowing was
    // already the trick for quirks mode, and the same shadow carries this.
    let parse_length = |raw: &Raw| parse_length_quirky(raw, quirks).map(|it| it.scaled(zoom));
    // For the properties CSS 2.1 forbids a negative value on. A negative one
    // there is invalid, and an invalid declaration is dropped rather than
    // clamped — the property keeps whatever it already had.
    let parse_size = |raw: &Raw| parse_length(raw).filter(|length| !length.is_negative());
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
        "overflow" => {
            if let Raw::Ident(name) = first
                && let Some(overflow) = parse_overflow(name)
            {
                style.overflow = overflow;
            }
        }
        "background-position" => {
            // The element's own font size, not the parent's: `em` here is
            // relative to the size this element ends up with, and `font-size`
            // is the one property where that is not true.
            if let Some(position) = parse_background_position(values, style.font_size) {
                style.background_position = position;
            }
        }
        // The shorthand sets everything it names and resets everything it does
        // not — that reset is the whole reason `background: white` reliably
        // clears an image, and skipping it leaves the image showing through.
        "background" => {
            style.background_color = Color::TRANSPARENT;
            style.background_image = None;
            style.background_repeat = BackgroundRepeat::Repeat;
            style.background_position = BackgroundPosition::default();
            // Position is the one component of this shorthand that is more than
            // one token, so its pieces are collected as they go by and parsed
            // together at the end. Gathered rather than parsed in place because
            // `10px 20px` only means anything as a pair, and the two are not
            // necessarily adjacent to anything that identifies them.
            let mut position = Vec::new();
            for value in values {
                if let Some(url) = url_value(value) {
                    style.background_image = Some(url.to_owned());
                } else if let Raw::Ident(name) = value
                    && let Some(repeat) = parse_background_repeat(name)
                {
                    style.background_repeat = repeat;
                } else if is_position_component(value) {
                    position.push(value.clone());
                } else if let Some(color) = parse_color(value) {
                    style.background_color = color;
                }
            }
            if !position.is_empty()
                && let Some(parsed) = parse_background_position(&position, style.font_size)
            {
                style.background_position = parsed;
            }
        }
        // §15.8. Era stylesheets are full of this — `font: bold 12px Arial`
        // was how a page set its type — and until now it parsed as nothing at
        // all, so both the size and the line height silently stayed at their
        // defaults. That is why a table cell with `font: 20px/1 Ahem` came out
        // a few pixels short and let the row's background show through.
        //
        // A shorthand resets every property it covers, including the ones it
        // does not mention: `font: 12px serif` after `font-weight: bold` is
        // not bold. Assigning only the parts that were written is the usual
        // way to get this wrong.
        "font" => {
            if let Some(font) = parse_font_shorthand(values, parent, zoom) {
                style.font_style = font.style;
                style.font_weight = font.weight;
                style.font_size = font.size;
                style.line_height = font.line_height.unwrap_or(font.size * NORMAL_LINE_HEIGHT);
                style.font_family = font.family;
            }
        }
        // font-size resolves em and % against the *parent's* size, not its own.
        "font-size" => {
            if let Some(size) = parse_font_size(first, parent.font_size, zoom) {
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
            if let Some(length) = parse_size(first) {
                style.border_spacing = length;
            }
        }
        "text-transform" => {
            if let Raw::Ident(name) = first
                && let Some(transform) = parse_text_transform(name)
            {
                style.text_transform = transform;
            }
        }
        // `normal` is zero rather than a value of its own: CSS 2.1 gives it no
        // meaning beyond "no extra space", and a separate variant would be a
        // second way to spell the same number.
        //
        // `word-spacing` is deliberately *not* handled alongside it. The two
        // look like a pair and are not implemented as one, and naming it here
        // to do nothing with it would make the gap invisible to the next
        // reader — it stays in the README's list until it is really done.
        "letter-spacing" => {
            if matches!(first, Raw::Ident(name) if name.eq_ignore_ascii_case("normal")) {
                style.letter_spacing = 0.0;
            } else if let Some(length) = parse_length(first) {
                // Resolved here because it is a used length by the time text is
                // shaped, and shaping is where it has to arrive.
                style.letter_spacing = length.to_px(style.font_size, 0.0);
            }
        }
        "text-indent" => {
            if let Some(length) = parse_length(first) {
                style.text_indent = length;
            }
        }
        // §12.2. Resolved to text here rather than carried as a value list:
        // every form in scope is known at this point, and `attr()` needs the
        // originating element, which layout does not have.
        "content" => {
            style.content = parse_content(element, values);
        }
        "min-height" => {
            if let Some(length) = parse_size(first) {
                style.min_height = length;
            }
        }
        "max-height" => {
            // `none` is the initial value and the way an author takes a cap
            // back off, which a page does by overriding an earlier rule. It
            // is a keyword rather than a length, so it has to be matched
            // before parsing one — the same shape as `max-width` above.
            if matches!(first, Raw::Ident(name) if name.eq_ignore_ascii_case("none")) {
                style.max_height = Length::Auto;
            } else if let Some(length) = parse_size(first) {
                style.max_height = length;
            }
        }
        // §9.9. `auto` is the initial value and means "no new stacking context
        // and paint in tree order", which is what `None` stands for.
        "z-index" => {
            style.z_index = match first {
                Raw::Number(n) => Some(*n as i32),
                Raw::Ident(name) if name.eq_ignore_ascii_case("auto") => None,
                _ => style.z_index,
            };
        }
        "visibility" => {
            if let Raw::Ident(name) = first
                && let Some(visibility) = parse_visibility(name)
            {
                style.visibility = visibility;
            }
        }
        "caption-side" => {
            if let Raw::Ident(name) = first
                && let Some(side) = parse_caption_side(name)
            {
                style.caption_side = side;
            }
        }
        // Ahead of the `border-*` longhand fallback at the bottom of this
        // match, which would otherwise hand `collapse` to the code that parses
        // edge names and get nothing for it.
        "border-collapse" => {
            if let Raw::Ident(name) = first
                && let Some(collapse) = parse_border_collapse(name)
            {
                style.border_collapse = collapse;
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
        "margin" => style.margin = parse_edges(values, quirks, zoom, false),
        "padding" => style.padding = parse_edges(values, quirks, zoom, true),
        "width" => {
            if let Some(length) = parse_size(first) {
                style.width = length;
            }
        }
        // `none` is the initial value and means no bound, which is what `Auto`
        // stands for here — there is no separate "auto" for a maximum.
        "max-width" => {
            if matches!(first, Raw::Ident(name) if name.eq_ignore_ascii_case("none")) {
                style.max_width = Length::Auto;
            } else if let Some(length) = parse_size(first) {
                style.max_width = length;
            }
        }
        "height" => {
            if let Some(length) = parse_size(first) {
                style.height = length;
            }
        }
        // `border: 1px solid red` sets width, style, and colour on all four
        // sides from whichever components are present.
        "border" => {
            let parsed = parse_border_shorthand(values, zoom);
            for side in border_sides(&mut style.border) {
                apply_border_shorthand(side, &parsed, zoom);
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
                set_edge(&mut style.margin, side, first, quirks, zoom, false);
            } else if let Some(side) = name.strip_prefix("padding-") {
                set_edge(&mut style.padding, side, first, quirks, zoom, true);
            } else if let Some(rest) = name.strip_prefix("border-") {
                apply_border_longhand(&mut style.border, rest, values, zoom);
            }
        }
    }
}

fn parse_font_size(raw: &Raw, parent_size: f32, zoom: f32) -> Option<f32> {
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
        return Some(DEFAULT_FONT_SIZE * zoom * factor);
    }
    match parse_length(raw)?.scaled(zoom) {
        Length::Auto => None,
        length => Some(length.to_px(parent_size, parent_size)),
    }
}

/// Everything a `font` shorthand sets.
struct FontShorthand {
    style: FontStyle,
    weight: u16,
    size: f32,
    /// `None` where the shorthand wrote no `/ line-height`, which means
    /// `normal` rather than "leave the old one".
    line_height: Option<f32>,
    family: FontStack,
}

/// Parses `font: [ style || variant || weight ]? size [ / line-height ]? family`.
///
/// The size and the family are required; anything before the size is optional
/// and may come in any order. A shorthand missing either is invalid and must
/// change nothing at all, which is why this returns an `Option` rather than
/// filling in defaults.
///
/// `small-caps` is accepted and then discarded. `font-variant` is not
/// implemented here, and rejecting the whole declaration over it would throw
/// away the size and family too — the page would lose styling it should have,
/// to no one's benefit.
///
/// The system font keywords — `font: menu`, `caption`, `status-bar` — need no
/// case of their own, though it is tempting to write one. CSS 2.1 §15.8 says
/// they take a platform widget's font, which cannot be asked for here; and
/// none of them is a valid font size, so each fails the required-size check
/// and leaves the page's own styling exactly where it was. A special case for
/// them was written first and deleted: it read as load-bearing and changed
/// nothing, which is worse than its absence.
fn parse_font_shorthand(
    values: &[Raw],
    parent: &ComputedStyle,
    zoom: f32,
) -> Option<FontShorthand> {
    let mut style = FontStyle::Normal;
    let mut weight = 400;
    let mut index = 0;

    // The optional prefix. `normal` is legal for all three of style, variant
    // and weight, so it is consumed without deciding which one it meant —
    // correct, because each already sits at the value `normal` names.
    while let Some(value) = values.get(index) {
        match value {
            Raw::Ident(name) => match name.as_str() {
                "normal" | "small-caps" => {}
                "italic" | "oblique" => style = FontStyle::Italic,
                "bold" => weight = 700,
                "bolder" => weight = (parent.font_weight + 300).min(900),
                "lighter" => weight = parent.font_weight.saturating_sub(300),
                _ => break,
            },
            Raw::Number(number) if (100.0..=900.0).contains(number) => {
                weight = (*number as u16).clamp(100, 900);
            }
            _ => break,
        }
        index += 1;
    }

    let size = parse_font_size(values.get(index)?, parent.font_size, zoom)?;
    index += 1;

    let mut line_height = None;
    if matches!(values.get(index), Some(Raw::Slash)) {
        index += 1;
        line_height = Some(match values.get(index)? {
            // Resolved against this shorthand's own size, not the parent's:
            // `font: 20px/1.5 serif` is a 30px line whatever the parent is.
            Raw::Number(number) => size * number,
            Raw::Ident(name) if name == "normal" => size * NORMAL_LINE_HEIGHT,
            other => parse_length(other)?.scaled(zoom).to_px(size, size),
        });
        index += 1;
    }

    let rest = values.get(index..).unwrap_or_default();
    if rest.is_empty() {
        return None;
    }
    Some(FontShorthand {
        style,
        weight,
        size,
        line_height,
        family: parse_font_family(rest),
    })
}

/// Parses `content`, resolving it to the text it stands for.
///
/// In scope: strings and `attr()`, which concatenate — that is how a page
/// writes `content: "[" attr(href) "]"`.
///
/// Out of scope, and dropping the whole declaration rather than half of it:
/// `counter()`, `counters()`, `open-quote` and its family, and `url()`. Each
/// needs machinery this does not have — a counter state, the `quotes`
/// property, an image load — and a pseudo-element showing *part* of what the
/// author asked for is worse than one showing nothing, because it looks
/// deliberate.
///
/// `none` and `normal` need no case of their own, though writing one is the
/// obvious thing to do. Neither is a string or an `attr()`, so both fall to
/// the rejection below and drop the declaration, which is exactly what they
/// mean. A branch for them was written first and deleted when a mutation
/// showed it changed nothing.
fn parse_content(element: Option<&ElementData>, values: &[Raw]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let mut out = String::new();
    for value in values {
        match value {
            Raw::Str(text) => out.push_str(text),
            Raw::Function(name, args) if name == "attr" => {
                // A missing attribute is the empty string, not a failure:
                // §12.2 says so, and it is what makes `content: attr(title)`
                // safe to write across a whole document.
                let attribute = match args.first() {
                    Some(Raw::Ident(name)) => name.clone(),
                    Some(Raw::Str(name)) => name.clone(),
                    _ => return None,
                };
                let value = element
                    .and_then(|element| element.attr(&attribute))
                    .unwrap_or_default();
                out.push_str(value);
            }
            _ => return None,
        }
    }
    Some(out)
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
///
/// `non_negative` is set for padding and clear for margin, which is the one
/// place the two differ: a negative margin is legal and useful — it is how the
/// era pulled a box back over its neighbour — and a negative padding is
/// invalid, so the declaration is dropped rather than clamped.
fn parse_edges(values: &[Raw], quirks: bool, zoom: f32, non_negative: bool) -> Edges {
    let lengths: Vec<Length> = values
        .iter()
        .filter_map(|raw| parse_length_quirky(raw, quirks).map(|it| it.scaled(zoom)))
        .filter(|length| !(non_negative && length.is_negative()))
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
    // A `<select>` has no widget here, so without this every one of its options
    // is laid out as ordinary inline text and they run together: a country
    // dropdown becomes two hundred country names in the middle of a sentence.
    // That is worse than drawing nothing, because a reader cannot tell it is
    // not part of the page.
    //
    // A closed dropdown shows exactly one option, so every other one is hidden.
    // A list box — `multiple`, or `size` above one — shows all of them, and
    // they are stacked rather than run together so the list still reads as a
    // list.
    if tag == "option"
        && let Some(select) = enclosing_select(doc, node)
    {
        if is_list_box(doc.element(select)) {
            push("display", "block");
        } else if !is_shown_option(doc, select, node) {
            push("display", "none");
        }
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

/// The `select` enclosing an option, skipping any `optgroup`.
fn enclosing_select(doc: &Document, node: NodeId) -> Option<NodeId> {
    let mut current = doc.node(node).parent;
    while let Some(id) = current {
        if doc.element(id)?.local_name() == "select" {
            return Some(id);
        }
        current = doc.node(id).parent;
    }
    None
}

/// Whether a `select` is drawn as a list rather than as a closed dropdown.
///
/// `size="1"` is a dropdown written the long way round, so the attribute is
/// read rather than merely tested for.
fn is_list_box(select: Option<&ElementData>) -> bool {
    let Some(select) = select else {
        return false;
    };
    if select.attr("multiple").is_some() {
        return true;
    }
    select
        .attr("size")
        .and_then(|value| value.trim().parse::<u32>().ok())
        .is_some_and(|size| size > 1)
}

/// Whether this is the one option a closed dropdown displays.
///
/// The last option carrying `selected` wins, which is what browsers do with the
/// malformed case of several; with none, the first option is shown, because
/// that is what a dropdown opens on.
fn is_shown_option(doc: &Document, select: NodeId, option: NodeId) -> bool {
    let mut options = Vec::new();
    collect_options(doc, select, &mut options);
    let selected = options
        .iter()
        .rev()
        .find(|&&id| {
            doc.element(id)
                .is_some_and(|element| element.attr("selected").is_some())
        })
        .copied();
    match selected {
        Some(id) => id == option,
        None => options.first() == Some(&option),
    }
}

/// Every `option` under a `select`, in document order, descending through
/// `optgroup`.
fn collect_options(doc: &Document, node: NodeId, out: &mut Vec<NodeId>) {
    for &child in doc.children(node) {
        let Some(element) = doc.element(child) else {
            continue;
        };
        match element.local_name() {
            "option" => out.push(child),
            _ => collect_options(doc, child, out),
        }
    }
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
fn parse_border_shorthand(values: &[Raw], zoom: f32) -> BorderShorthand {
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
                    out.width = Some(Length::Px(1.0 * zoom));
                    continue;
                }
                "medium" => {
                    out.width = Some(Length::Px(MEDIUM_BORDER * zoom));
                    continue;
                }
                "thick" => {
                    out.width = Some(Length::Px(5.0 * zoom));
                    continue;
                }
                _ => {}
            }
        }
        if let Some(color) = parse_color(raw) {
            out.color = Some(color);
        } else if let Some(length) = parse_length(raw) {
            out.width = Some(length.scaled(zoom));
        }
    }
    out
}

fn apply_border_shorthand(side: &mut BorderSide, parsed: &BorderShorthand, zoom: f32) {
    // The shorthand resets omitted components to their initial values, which is
    // why `border: solid` produces a medium border rather than keeping whatever
    // width an earlier rule set.
    side.width = parsed.width.unwrap_or(Length::Px(MEDIUM_BORDER * zoom));
    side.style = parsed.style.unwrap_or_default();
    side.color = parsed.color;
}

/// Handles `border-top`, `border-left-width`, and friends.
fn apply_border_longhand(borders: &mut Borders, rest: &str, values: &[Raw], zoom: f32) {
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
        None => apply_border_shorthand(side, &parse_border_shorthand(values, zoom), zoom),
        Some("width") => {
            if let Some(width) = parse_length(first) {
                side.width = width.scaled(zoom);
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

fn set_edge(edges: &mut Edges, side: &str, raw: &Raw, quirks: bool, zoom: f32, non_negative: bool) {
    let Some(length) = parse_length_quirky(raw, quirks).map(|it| it.scaled(zoom)) else {
        return;
    };
    if non_negative && length.is_negative() {
        return;
    }
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
    use crate::style::{
        BackgroundPosition, BackgroundRepeat, BorderCollapse, CaptionSide, Display, ListStyleType,
        VerticalAlign, Visibility,
    };

    fn style_of(html: &str, css: &str, tag: &str) -> ComputedStyle {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css)];
        let map = cascade(&doc, &sheets);
        let node = doc.find_element(tag).expect("element present");
        map.get(node).expect("element styled").clone()
    }

    fn zoomed_style_of(html: &str, css: &str, tag: &str, zoom: f32) -> ComputedStyle {
        style_from(html, css, tag, zoom, Colours::Authors)
    }

    fn reader_style_of(html: &str, css: &str, tag: &str) -> ComputedStyle {
        style_from(html, css, tag, 1.0, Colours::Readers)
    }

    fn style_from(html: &str, css: &str, tag: &str, zoom: f32, colours: Colours) -> ComputedStyle {
        let doc = dom::parse(html);
        let sheets = [Stylesheet::parse(css)];
        let map = cascade_as(&doc, &sheets, zoom, colours);
        let node = doc.find_element(tag).expect("element present");
        map.get(node).expect("element styled").clone()
    }

    #[test]
    fn zoom_multiplies_every_pixel_the_page_asked_for() {
        let css = "p { font-size: 20px; margin: 10px; padding-left: 4px; \
                   border: 2px solid red; width: 300px }";
        let plain = zoomed_style_of("<p>x</p>", css, "p", 1.0);
        let doubled = zoomed_style_of("<p>x</p>", css, "p", 2.0);

        assert_eq!(doubled.font_size, plain.font_size * 2.0);
        assert_eq!(doubled.margin.top, Length::Px(20.0));
        assert_eq!(doubled.padding.left, Length::Px(8.0));
        assert_eq!(doubled.border.top.width, Length::Px(4.0));
        assert_eq!(doubled.width, Length::Px(600.0));
    }

    #[test]
    fn a_percentage_is_left_alone_because_its_basis_is_already_zoomed() {
        // Scaling it too would apply the zoom twice: a column half the width
        // of the window is half the width of the window at any zoom, which is
        // what page zoom means as against a magnifying glass.
        let doubled = zoomed_style_of("<p>x</p>", "p { width: 50% }", "p", 2.0);

        assert_eq!(doubled.width, Length::Percent(50.0));
    }

    #[test]
    fn an_em_is_left_alone_because_the_font_size_is_already_zoomed() {
        let doubled = zoomed_style_of("<p>x</p>", "p { margin: 2em }", "p", 2.0);

        assert_eq!(doubled.margin.top, Length::Em(2.0));
        assert_eq!(doubled.font_size, DEFAULT_FONT_SIZE * 2.0);
    }

    #[test]
    fn a_page_that_names_no_size_is_zoomed_by_what_it_inherits() {
        // Most text on most pages sets no font size at all. If the root's
        // default were not scaled, zoom would move the headings and leave the
        // prose exactly where it was.
        let doubled = zoomed_style_of("<body><p>x</p></body>", "", "p", 2.0);

        assert_eq!(doubled.font_size, DEFAULT_FONT_SIZE * 2.0);
        assert_eq!(
            doubled.line_height,
            DEFAULT_FONT_SIZE * 2.0 * NORMAL_LINE_HEIGHT
        );
    }

    #[test]
    fn the_named_border_widths_and_font_sizes_are_zoomed_too() {
        // `medium` and `x-large` are pixel lengths spelled as words, and a
        // word is exactly as easy to forget as a number.
        let doubled = zoomed_style_of(
            "<p>x</p>",
            "p { border: medium solid red; font-size: x-large }",
            "p",
            2.0,
        );

        assert_eq!(doubled.border.top.width, Length::Px(MEDIUM_BORDER * 2.0));
        assert_eq!(doubled.font_size, DEFAULT_FONT_SIZE * 2.0 * 1.5);
    }

    #[test]
    fn zooming_out_is_the_same_rule_the_other_way() {
        let halved = zoomed_style_of("<p>x</p>", "p { font-size: 20px; margin: 10px }", "p", 0.5);

        assert_eq!(halved.font_size, 10.0);
        assert_eq!(halved.margin.top, Length::Px(5.0));
    }

    #[test]
    fn a_reading_view_ignores_the_colours_written_into_the_markup() {
        // Dropping the author's sheet does not drop the colours in their
        // markup, and on a dark reader page what is left is a pale band with
        // near-white text on it. Wikipedia's taxobox writes exactly this, on
        // every row.
        let html = r##"<body><table><tr style="background-color: rgb(235,235,210)">
            <td bgcolor="#e9e9c8"><font color="#333">Conservation status</font></td>
            </tr></table></body>"##;
        let sheet = "body { color: #eee; background-color: #1c1b22 }";

        for tag in ["tr", "td", "font"] {
            let reading = reader_style_of(html, sheet, tag);
            assert_eq!(
                reading.background_color,
                Color::TRANSPARENT,
                "<{tag}> kept a background from the markup"
            );
            assert_eq!(
                reading.color,
                Color::rgb(0xee, 0xee, 0xee),
                "<{tag}> kept a foreground from the markup"
            );
        }
    }

    #[test]
    fn an_authored_rendering_keeps_them() {
        // The other half, and the one that keeps this a fallback behaviour
        // rather than a policy: a page rendered as its author wrote it is
        // rendered as its author wrote it.
        let html = r#"<body><table><tr style="background-color: rgb(235,235,210)">
            <td>x</td></tr></table></body>"#;
        let authored = style_of(html, "body { color: #eee }", "tr");

        assert_eq!(authored.background_color, Color::rgb(235, 235, 210));
    }

    #[test]
    fn the_sheets_colours_still_apply_in_a_reading_view() {
        // What is ignored is the markup, not the cascade. The reader sheet is
        // handed in through the same slot the author's would use, and the
        // whole point of the reading view is that its colours win.
        let style = reader_style_of(
            "<body><p>x</p></body>",
            "body { background-color: #1c1b22 } p { color: #eee }",
            "p",
        );

        assert_eq!(style.color, Color::rgb(0xee, 0xee, 0xee));
    }

    #[test]
    fn a_reading_view_keeps_everything_in_the_markup_that_is_not_a_colour() {
        // Narrow on purpose. A `style` attribute is not all colour, and an
        // element hidden inline has to stay hidden — dropping that would
        // *reveal* content the page had put away, which is a worse failure
        // than an ugly one.
        let style = reader_style_of(
            r#"<body><p style="background-color: red; display: none; text-align: right">x</p></body>"#,
            "body { color: #eee }",
            "p",
        );

        assert_eq!(style.display, Display::None);
        assert_eq!(style.text_align, TextAlign::Right);
        assert_eq!(style.background_color, Color::TRANSPARENT);
    }

    #[test]
    fn a_reading_view_leaves_the_borders_where_the_author_drew_them() {
        // A border the reader cannot make out is a line missing; a paragraph
        // the reader cannot make out is the article missing. Only the second
        // is worth overriding an author for, so the rule stops at the two
        // properties that cause it.
        for declaration in [
            "border: 1px solid rgb(200,200,160)",
            "border-style: solid; border-color: rgb(200,200,160)",
        ] {
            let style = reader_style_of(
                &format!(
                    r#"<body><table><tr><td style="{declaration}">x</td></tr></table></body>"#
                ),
                "body { color: #eee }",
                "td",
            );

            assert_eq!(
                style.border.top.color,
                Some(Color::rgb(200, 200, 160)),
                "{declaration}"
            );
        }
    }

    /// The declaration era stylesheets are full of, which parsed as nothing
    /// at all until it was noticed making table rows a few pixels short.
    #[test]
    fn the_font_shorthand_sets_size_line_height_and_family() {
        let style = style_of("<p>x</p>", "p { font: 20px/1.5 Georgia, serif }", "p");
        assert_eq!(style.font_size, 20.0);
        assert_eq!(style.line_height, 30.0);
        assert_eq!(style.font_family.families, vec!["georgia".to_owned()]);
        assert_eq!(style.font_family.generic, GenericFamily::Serif);
    }

    #[test]
    fn the_font_shorthand_takes_style_and_weight_in_any_order() {
        for css in [
            "p { font: italic bold 20px serif }",
            "p { font: bold italic 20px serif }",
            "p { font: italic small-caps bold 20px serif }",
        ] {
            let style = style_of("<p>x</p>", css, "p");
            assert_eq!(style.font_style, FontStyle::Italic, "{css}");
            assert_eq!(style.font_weight, 700, "{css}");
            assert_eq!(style.font_size, 20.0, "{css}");
        }
    }

    #[test]
    fn the_font_shorthand_resets_what_it_does_not_mention() {
        // The usual way to get a shorthand wrong is to assign only the parts
        // that were written. `font` covers weight, style and line height, so
        // all three go back to their initial values here.
        let style = style_of(
            "<p>x</p>",
            "p { font-weight: bold; font-style: italic; line-height: 40px; font: 20px serif }",
            "p",
        );
        assert_eq!(style.font_weight, 400, "weight survived the shorthand");
        assert_eq!(style.font_style, FontStyle::Normal, "style survived it");
        assert_eq!(
            style.line_height,
            20.0 * NORMAL_LINE_HEIGHT,
            "line-height survived it"
        );
    }

    #[test]
    fn a_font_shorthand_without_a_size_or_family_changes_nothing() {
        // Both are required. An invalid shorthand must leave the earlier
        // declaration standing rather than half-applying itself.
        // `font: nonsense serif` matters more than the others: a family does
        // follow, so only the required-size check stands between an invalid
        // shorthand and it half-applying itself at a made-up size.
        for bad in [
            "font: serif",
            "font: 20px",
            "font: bold",
            "font: 20px/1.5",
            "font: nonsense serif",
        ] {
            let css = format!("p {{ font-size: 11px; font-family: monospace; {bad} }}");
            let style = style_of("<p>x</p>", &css, "p");
            assert_eq!(style.font_size, 11.0, "{bad} changed the size");
            assert_eq!(
                style.font_family.generic,
                GenericFamily::Monospace,
                "{bad} changed the family"
            );
        }
    }

    #[test]
    fn a_system_font_keyword_leaves_the_page_alone() {
        // There is no way to ask this platform for its menu font, and a guess
        // dressed up as the system's answer is worse than doing nothing.
        let style = style_of(
            "<p>x</p>",
            "p { font-size: 11px; font-family: monospace; font: menu }",
            "p",
        );
        assert_eq!(style.font_size, 11.0);
        assert_eq!(style.font_family.generic, GenericFamily::Monospace);
    }

    #[test]
    fn the_font_shorthands_line_height_resolves_against_its_own_size() {
        // `20px/1.5` is a 30px line whatever the parent's size is — the
        // shorthand's own size is the one in force by the time the slash is
        // read.
        let style = style_of(
            "<div><p>x</p></div>",
            "div { font-size: 40px } p { font: 20px/1.5 serif }",
            "p",
        );
        assert_eq!(style.font_size, 20.0);
        assert_eq!(style.line_height, 30.0);
    }

    #[test]
    fn a_slash_is_not_swallowed_as_an_unknown_token() {
        // Before `Raw::Slash` existed the separator arrived as an unmodelled
        // token, which is how the whole declaration came to be dropped.
        let style = style_of("<p>x</p>", "p { font: 20px/10px serif }", "p");
        assert_eq!(style.line_height, 10.0);
    }

    fn content_of(html: &str, css: &str, tag: &str, which: PseudoElement) -> Option<String> {
        let doc = dom::parse(html);
        let map = cascade(&doc, &[Stylesheet::parse(css)]);
        let node = doc.find_element(tag).expect("element present");
        map.pseudo(node, which)
            .and_then(|style| style.content.clone())
    }

    #[test]
    fn a_pseudo_element_gets_its_own_style_and_content() {
        let doc = dom::parse("<p>x</p>");
        let map = cascade(
            &doc,
            &[Stylesheet::parse(
                "p { color: #000000 } p::before { content: \"hi\"; color: #ff0000 }",
            )],
        );
        let node = doc.find_element("p").expect("p");
        let before = map
            .pseudo(node, PseudoElement::Before)
            .expect("a before box");
        assert_eq!(before.content.as_deref(), Some("hi"));
        assert_eq!(before.color, Color::rgb(255, 0, 0));
        assert_eq!(
            map.get(node).expect("the element").color,
            Color::rgb(0, 0, 0),
            "the pseudo-element's colour leaked onto its originator"
        );
    }

    #[test]
    fn both_spellings_of_the_pseudo_element_are_accepted() {
        // CSS 2.1 writes one colon, CSS 2.2 onwards writes two, and the era's
        // pages use the single-colon form.
        for css in [
            "p::before { content: \"x\" }",
            "p:before { content: \"x\" }",
        ] {
            assert_eq!(
                content_of("<p>y</p>", css, "p", PseudoElement::Before).as_deref(),
                Some("x"),
                "{css}"
            );
        }
    }

    #[test]
    fn no_content_means_no_box() {
        // §12.1, and the style has to be *absent* rather than merely
        // contentless: every later stage reads "a style is here" as "this box
        // exists".
        for css in [
            "p::before { color: red }",
            "p::before { content: none }",
            "p::before { content: normal }",
        ] {
            let doc = dom::parse("<p>y</p>");
            let map = cascade(&doc, &[Stylesheet::parse(css)]);
            let node = doc.find_element("p").expect("p");
            assert!(map.pseudo(node, PseudoElement::Before).is_none(), "{css}");
        }
    }

    #[test]
    fn content_concatenates_strings_and_attributes() {
        assert_eq!(
            content_of(
                r#"<p title="T">y</p>"#,
                r#"p::before { content: "[" attr(title) "]" }"#,
                "p",
                PseudoElement::Before
            )
            .as_deref(),
            Some("[T]")
        );
    }

    #[test]
    fn a_missing_attribute_is_the_empty_string_and_not_a_failure() {
        // §12.2 says so, and it is what makes `content: attr(title)` safe to
        // write across a whole document.
        assert_eq!(
            content_of(
                "<p>y</p>",
                "p::before { content: attr(title) }",
                "p",
                PseudoElement::Before
            )
            .as_deref(),
            Some("")
        );
    }

    #[test]
    fn a_content_form_out_of_scope_drops_the_whole_declaration() {
        // A pseudo-element showing *part* of what the author asked for is
        // worse than one showing nothing: it looks deliberate.
        for css in [
            "p::before { content: counter(x) }",
            "p::before { content: open-quote }",
            "p::before { content: \"a\" counter(x) }",
        ] {
            assert_eq!(
                content_of("<p>y</p>", css, "p", PseudoElement::Before),
                None,
                "{css}"
            );
        }
    }

    #[test]
    fn a_rule_without_a_pseudo_element_does_not_style_one() {
        let doc = dom::parse("<p>x</p>");
        let map = cascade(
            &doc,
            &[Stylesheet::parse(
                "p { background-color: #ff0000; color: #00ff00 } p::before { content: \"x\" }",
            )],
        );
        let node = doc.find_element("p").expect("p");
        let before = map
            .pseudo(node, PseudoElement::Before)
            .expect("a before box");
        assert_eq!(
            before.color,
            Color::rgb(0, 255, 0),
            "an inherited property should come across"
        );
        assert_eq!(
            before.background_color,
            Color::TRANSPARENT,
            "a non-inherited property came across from the element's own rule"
        );
    }

    #[test]
    fn a_presentational_attribute_does_not_reach_the_generated_box() {
        let doc = dom::parse(r##"<body bgcolor="#ff0000"><p>x</p></body>"##);
        let map = cascade(
            &doc,
            &[Stylesheet::parse("body::before { content: \"x\" }")],
        );
        let node = doc.find_element("body").expect("body");
        let before = map
            .pseudo(node, PseudoElement::Before)
            .expect("a before box");
        assert_eq!(
            before.background_color,
            Color::TRANSPARENT,
            "a `bgcolor` painted the box `::before` generates"
        );
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
    fn background_position_reads_keywords_lengths_and_percentages() {
        let position = |css: &str| {
            style_of("<body>x</body>", &format!("body {{ {css} }}"), "body").background_position
        };
        let percent = |x: f32, y: f32| BackgroundPosition {
            x: Length::Percent(x),
            y: Length::Percent(y),
        };

        // The initial value, which is what every other case is a change from.
        assert_eq!(position(""), percent(0.0, 0.0));
        assert_eq!(position("background-position: center"), percent(50.0, 50.0));
        assert_eq!(
            position("background-position: right bottom"),
            percent(100.0, 100.0)
        );

        // One value sets the horizontal and centres the other — unless it
        // cannot be horizontal. `top` alone is centred across, which is the
        // rule most easily got wrong by treating the first value positionally.
        assert_eq!(position("background-position: 25%"), percent(25.0, 50.0));
        assert_eq!(position("background-position: top"), percent(50.0, 0.0));
        assert_eq!(
            position("background-position: bottom"),
            percent(50.0, 100.0)
        );

        // Keywords may be written either way round; anything else may not.
        assert_eq!(position("background-position: top left"), percent(0.0, 0.0));
        assert_eq!(
            position("background-position: bottom center"),
            percent(50.0, 100.0)
        );

        assert_eq!(
            position("background-position: 10px 20px"),
            BackgroundPosition {
                x: Length::Px(10.0),
                y: Length::Px(20.0),
            }
        );
        // `em` is resolved during the cascade, against this element's own size.
        assert_eq!(
            position("font-size: 20px; background-position: 2em 0"),
            BackgroundPosition {
                x: Length::Px(40.0),
                y: Length::Px(0.0),
            }
        );
    }

    #[test]
    fn an_invalid_background_position_leaves_the_previous_one_alone() {
        // CSS drops a declaration it cannot parse, and here that matters more
        // than usual: half a position puts the image somewhere the author never
        // asked for, which is worse than ignoring them.
        for bad in [
            // Two of the same axis.
            "top bottom",
            "left right",
            // A vertical keyword where only a horizontal one is allowed. The
            // reversed order is legal only when *both* parts are keywords.
            "top 50%",
            "50% left",
            // Not lengths at all.
            "auto",
            "banana",
            // The three-value form, which is CSS3 (ADR-0004).
            "left top 10px",
        ] {
            let style = style_of(
                "<body>x</body>",
                &format!("body {{ background-position: 25% 75%; background-position: {bad} }}"),
                "body",
            );
            assert_eq!(
                style.background_position,
                BackgroundPosition {
                    x: Length::Percent(25.0),
                    y: Length::Percent(75.0),
                },
                "`{bad}` was not rejected"
            );
        }
    }

    #[test]
    fn the_background_shorthand_carries_and_resets_the_position() {
        let style = style_of(
            "<body>x</body>",
            "body { background: url(tile.gif) no-repeat 30% 10px }",
            "body",
        );
        assert_eq!(style.background_image.as_deref(), Some("tile.gif"));
        assert_eq!(style.background_repeat, BackgroundRepeat::NoRepeat);
        assert_eq!(
            style.background_position,
            BackgroundPosition {
                x: Length::Percent(30.0),
                y: Length::Px(10.0),
            }
        );

        // And the reset, which is the half that bites: a later shorthand naming
        // no position must put it back to the corner rather than leave the
        // earlier one in place.
        let style = style_of(
            "<body>x</body>",
            "body { background: url(a.gif) 30% 10px } body { background: url(b.gif) }",
            "body",
        );
        assert_eq!(style.background_image.as_deref(), Some("b.gif"));
        assert_eq!(style.background_position, BackgroundPosition::default());
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
    fn border_collapse_parses_and_reaches_the_cells() {
        // Inherited, and that is the whole reason it works: the property is
        // written on the table and every cell has to agree with it about where
        // its borders are. A cell reading the initial value while its table
        // read `collapse` would lay out against a grid nobody drew.
        let doc = dom::parse("<table><tr><td>x</td></tr></table>");
        let sheets = [Stylesheet::parse("table { border-collapse: collapse }")];
        let styles = cascade(&doc, &sheets);
        let of = |tag: &str| {
            styles
                .get(doc.find_element(tag).expect("an element"))
                .expect("a styled element")
                .border_collapse
        };
        assert_eq!(of("table"), BorderCollapse::Collapse);
        assert_eq!(of("td"), BorderCollapse::Collapse, "did not inherit");

        // The initial value, and the one the whole engine rendered as before
        // this property existed.
        let plain = cascade(&doc, &[]);
        assert_eq!(
            plain
                .get(doc.find_element("table").expect("table"))
                .expect("a styled table")
                .border_collapse,
            BorderCollapse::Separate
        );
    }

    #[test]
    fn letter_spacing_is_resolved_to_pixels_against_the_element_own_size() {
        // Stored in pixels rather than as a length, because shaping is where it
        // has to arrive and shaping has no containing block to ask.
        let style = standards_style_of(
            "<p>x</p>",
            "p { font-size: 20px; letter-spacing: 0.5em }",
            "p",
        );
        assert!(
            (style.letter_spacing - 10.0).abs() < 0.01,
            "got {}",
            style.letter_spacing
        );

        let normal = standards_style_of("<p>x</p>", "p { letter-spacing: normal }", "p");
        assert_eq!(normal.letter_spacing, 0.0, "`normal` is no extra space");
    }

    #[test]
    fn word_spacing_is_still_not_implemented() {
        // It sits beside `letter-spacing` in every stylesheet and is *not*
        // implemented, and this asserts the gap rather than leaving somebody to
        // assume the pair came together. Delete this test when it does.
        let style = standards_style_of("<p>x</p>", "p { word-spacing: 20px }", "p");
        assert_eq!(
            style.letter_spacing, 0.0,
            "word-spacing leaked into letter-spacing"
        );
    }

    #[test]
    fn z_index_is_an_integer_or_auto() {
        let of = |css: &str| standards_style_of("<p>x</p>", css, "p").z_index;
        assert_eq!(of("p { z-index: 3 }"), Some(3));
        assert_eq!(of("p { z-index: -2 }"), Some(-2));
        assert_eq!(of("p { z-index: auto }"), None, "auto is not a number");
        assert_eq!(of(""), None, "the initial value");
    }

    #[test]
    fn visibility_inherits_and_a_child_can_come_back_out() {
        // §11.2's one genuine surprise, and it falls out of inheritance rather
        // than needing a rule: a descendant of a hidden element can set
        // `visible` and reappear. Without inheritance, hiding a container would
        // not hide what is inside it, which is what the property is for.
        let doc =
            dom::parse(r#"<div id="outer">a<span id="inner">b</span><em id="deep">c</em></div>"#);
        let styles = cascade(
            &doc,
            &[Stylesheet::parse(
                "#outer { visibility: hidden } #inner { visibility: visible }",
            )],
        );
        let of = |tag: &str| {
            styles
                .get(doc.find_element(tag).expect("an element"))
                .expect("a styled element")
                .visibility
        };
        assert_eq!(of("div"), Visibility::Hidden);
        assert_eq!(of("span"), Visibility::Visible, "a child cannot come back");
        assert_eq!(of("em"), Visibility::Hidden, "did not inherit");
    }

    #[test]
    fn collapse_is_read_as_hidden() {
        // §11.2 makes `collapse` mean `hidden` everywhere but on a table row or
        // column. Treating it as an unknown value instead would leave the
        // element *visible*, which is the opposite of what was asked for.
        let style = standards_style_of("<p>x</p>", "p { visibility: collapse }", "p");
        assert_eq!(style.visibility, Visibility::Hidden);

        let nonsense = standards_style_of("<p>x</p>", "p { visibility: sideways }", "p");
        assert_eq!(
            nonsense.visibility,
            Visibility::Visible,
            "the initial value"
        );
    }

    #[test]
    fn a_closed_dropdown_shows_only_the_option_it_is_open_on() {
        // Without this every option is inline text and they run together: a
        // country dropdown becomes two hundred country names inside a sentence.
        // Content that is wrong rather than missing, which a reader cannot tell
        // is not part of the page.
        let doc = dom::parse(
            "<select><option>One</option><option selected>Two</option>\
             <option>Three</option></select>",
        );
        let styles = cascade(&doc, &[]);
        // The text of it, not merely the count: showing exactly one option and
        // showing the *wrong* one are the same number.
        let shown: Vec<String> = doc
            .children(doc.find_element("select").expect("select"))
            .iter()
            .filter(|&&id| {
                styles
                    .get(id)
                    .is_some_and(|style| style.display != Display::None)
            })
            .filter_map(|&id| doc.children(id).first().and_then(|&t| doc.text(t)))
            .map(str::to_owned)
            .collect();
        assert_eq!(
            shown,
            vec!["Two".to_owned()],
            "a dropdown opens on `selected`"
        );
    }

    #[test]
    fn a_list_box_shows_every_option_and_stacks_them() {
        // `multiple` and `size` above one are drawn as a list, so every option
        // is kept — but as blocks, or they run together as one line of text,
        // which is the bug in a different costume.
        for markup in [
            r#"<select multiple><option>A</option><option>B</option></select>"#,
            r#"<select size="4"><option>A</option><option>B</option></select>"#,
        ] {
            let doc = dom::parse(markup);
            let styles = cascade(&doc, &[]);
            let displays: Vec<Display> = doc
                .children(doc.find_element("select").expect("select"))
                .iter()
                .filter_map(|&id| Some(styles.get(id)?.display))
                .collect();
            assert_eq!(displays.len(), 2, "{markup} lost an option");
            assert!(
                displays.iter().all(|d| *d == Display::Block),
                "{markup} gave {displays:?}"
            );
        }
    }

    #[test]
    fn caption_side_parses_and_inherits_to_the_caption() {
        // Written on the table, read on the caption, which is only possible
        // because it inherits — the caption is where the value is consulted and
        // nothing sets it there.
        let doc = dom::parse("<table><caption>c</caption><tr><td>x</td></tr></table>");
        let styles = cascade(&doc, &[Stylesheet::parse("table { caption-side: bottom }")]);
        let of = |tag: &str| {
            styles
                .get(doc.find_element(tag).expect("an element"))
                .expect("a styled element")
                .caption_side
        };
        assert_eq!(of("table"), CaptionSide::Bottom);
        assert_eq!(of("caption"), CaptionSide::Bottom, "did not inherit");

        assert_eq!(
            cascade(&doc, &[])
                .get(doc.find_element("caption").expect("caption"))
                .expect("a styled caption")
                .caption_side,
            CaptionSide::Top,
            "the initial value"
        );
    }

    #[test]
    fn caption_side_refuses_the_values_css_21_dropped() {
        // `left` and `right` are CSS 2.0 and no browser kept them. Refusing
        // leaves the declaration invalid and the caption on top, which is what
        // a browser does; guessing an axis would put it somewhere nobody asked.
        for value in ["left", "right", "sideways"] {
            let style = standards_style_of(
                "<table><caption>c</caption><tr><td>x</td></tr></table>",
                &format!("table {{ caption-side: {value} }}"),
                "table",
            );
            assert_eq!(
                style.caption_side,
                CaptionSide::Top,
                "`{value}` was accepted"
            );
        }
    }

    #[test]
    fn border_collapse_is_not_read_as_a_border_edge() {
        // `border-collapse` shares its prefix with `border-top` and the rest,
        // and the longhand fallback at the bottom of `apply` will take anything
        // starting `border-`. An arm that arrived after it would silently do
        // nothing, which is the failure this guards: the property would parse,
        // cascade, and have no effect.
        let style = standards_style_of(
            "<table><tr><td>x</td></tr></table>",
            "table { border-collapse: collapse }",
            "table",
        );
        assert_eq!(style.border_collapse, BorderCollapse::Collapse);
        assert_eq!(
            style.border,
            Borders::default(),
            "`collapse` was read as a border edge"
        );

        // A value the property does not take leaves it alone rather than
        // half-applying, as an invalid declaration must.
        let nonsense = standards_style_of(
            "<table><tr><td>x</td></tr></table>",
            "table { border-collapse: sideways }",
            "table",
        );
        assert_eq!(nonsense.border_collapse, BorderCollapse::Separate);
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

    /// The body's horizontal margins after the floor has been applied.
    fn margins_after_floor(css: &str, least: f32, available_width: f32) -> (Length, Length) {
        let doc = dom::parse(&format!("<style>{css}</style><body>x</body>"));
        let sheets = [Stylesheet::parse(css)];
        let mut map = cascade(&doc, &sheets);
        let body = doc.find_element("body").expect("a body");
        map.keep_off_the_edges(body, least, available_width);
        let style = map.get(body).expect("a styled body");
        (style.margin.left, style.margin.right)
    }

    #[test]
    fn the_edge_floor_raises_a_margin_that_is_under_it() {
        let (left, right) = margins_after_floor("body { margin: 0 }", 8.0, 400.0);
        assert_eq!(left, Length::Px(8.0));
        assert_eq!(right, Length::Px(8.0));
    }

    #[test]
    fn the_edge_floor_leaves_a_wider_margin_alone() {
        // A floor that overwrote whatever it found would be a fixed margin, and
        // would flatten every page's own spacing to the same eight pixels.
        let (left, _) = margins_after_floor("body { margin: 40px }", 8.0, 400.0);
        assert_eq!(left, Length::Px(40.0));
    }

    #[test]
    fn the_edge_floor_measures_a_percentage_margin_before_judging_it() {
        // 5% of 400px is 20px, which already clears the floor; 1% is 4px, which
        // does not. Comparing the numbers unresolved would get both wrong.
        let (wide, _) = margins_after_floor("body { margin: 0 5% }", 8.0, 400.0);
        assert_eq!(wide, Length::Percent(5.0), "a wide percentage was replaced");

        let (narrow, _) = margins_after_floor("body { margin: 0 1% }", 8.0, 400.0);
        assert_eq!(narrow, Length::Px(8.0), "a narrow percentage was kept");
    }

    #[test]
    fn the_edge_floor_treats_an_auto_margin_as_asking_for_nothing() {
        // `margin: 0 auto` on a block of automatic width — which the body is —
        // resolves to zero, so the page has asked for no room at all.
        let (left, right) = margins_after_floor("body { margin: 0 auto }", 8.0, 400.0);
        assert_eq!((left, right), (Length::Px(8.0), Length::Px(8.0)));
    }

    #[test]
    fn the_edge_floor_ignores_the_vertical_margins() {
        // The complaint is about text against the side of the window. Topping
        // up the top margin as well would add a gap above every page.
        let doc = dom::parse("<style>body { margin: 0 }</style><body>x</body>");
        let sheets = [Stylesheet::parse("body { margin: 0 }")];
        let mut map = cascade(&doc, &sheets);
        let body = doc.find_element("body").expect("a body");
        map.keep_off_the_edges(body, 8.0, 400.0);
        let style = map.get(body).expect("a styled body");
        assert_eq!(style.margin.top, Length::Px(0.0));
        assert_eq!(style.margin.bottom, Length::Px(0.0));
    }
}
