//! Computed values.

use crate::value::{Color, Length, Raw};

/// The `display` property.
///
/// Flex and grid are represented rather than ignored. That is the whole
/// mechanism behind ADR-0009: we tokenise and cascade `display: flex` like any
/// other declaration and simply have no layout algorithm for it, so the engine
/// can *know* a page needs layout it cannot perform instead of guessing from
/// the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// Generates a block box.
    Block,
    /// Generates one or more inline boxes.
    Inline,
    /// Inline-level box with a block container inside.
    ///
    /// Parsed and cascaded, and **not laid out**: it is treated as plain
    /// `inline`, so a width, a height, a border and a background on one are all
    /// dropped, and an empty one collapses to nothing at all. Counted as
    /// unsupported layout for that reason — see [`Display::is_supported_layout`].
    InlineBlock,
    /// A list item; laid out as a block for now.
    ListItem,
    /// A table box.
    Table,
    /// A table row.
    TableRow,
    /// A row group: `thead`, `tbody`, or `tfoot`.
    TableRowGroup,
    /// A table cell. A block container in its own right.
    TableCell,
    /// Generates no box at all.
    None,
    /// `flex` or `inline-flex` — recognised, not implemented (ADR-0004).
    Flex,
    /// `grid` or `inline-grid` — recognised, not implemented (ADR-0004).
    Grid,
}

impl Display {
    /// Whether this engine can lay the box out.
    ///
    /// `false` feeds the document-fallback classifier (ADR-0009), not an error
    /// path: the page is still rendered, just as a document rather than with
    /// the author's layout.
    ///
    /// `InlineBlock` is here alongside flex and grid, and it is the one that
    /// reads as a mistake. The difference between it and them is only that it
    /// *nearly* works: an inline-block is laid out as a plain inline, so its
    /// content still appears and only its box is lost. That made it the one
    /// unimplemented thing here that failed **silently** — no fallback, no
    /// notice, just a page that is subtly wrong and an empty spacer that
    /// vanishes. Being nearly right is not a reason to say nothing; it is the
    /// case ADR-0009 was written for.
    ///
    /// This is a share, not a switch: the classifier weighs how much of the
    /// page's text sits under unsupported layout, so a navigation bar built
    /// from inline-blocks does not push an article into document mode, and a
    /// page whose body depends on them does.
    pub fn is_supported_layout(self) -> bool {
        !matches!(self, Display::Flex | Display::Grid | Display::InlineBlock)
    }

    /// Whether the box participates in inline layout.
    pub fn is_inline(self) -> bool {
        matches!(self, Display::Inline | Display::InlineBlock)
    }

    /// Whether the box is internal table structure, laid out by the table
    /// rather than by normal block flow.
    pub fn is_table_internal(self) -> bool {
        matches!(
            self,
            Display::TableRow | Display::TableRowGroup | Display::TableCell
        )
    }

    fn parse(name: &str) -> Option<Self> {
        let display = match name {
            "block" => Display::Block,
            "inline" => Display::Inline,
            "inline-block" => Display::InlineBlock,
            "list-item" => Display::ListItem,
            "none" => Display::None,
            "flex" | "inline-flex" => Display::Flex,
            "grid" | "inline-grid" => Display::Grid,
            "table" | "inline-table" => Display::Table,
            "table-row" => Display::TableRow,
            "table-row-group" | "table-header-group" | "table-footer-group" => {
                Display::TableRowGroup
            }
            "table-cell" => Display::TableCell,
            // Column and caption boxes are not implemented; treating them as
            // blocks keeps their content visible rather than dropping it.
            name if name.starts_with("table") => Display::Block,
            _ => return None,
        };
        Some(display)
    }
}

/// The `font-style` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStyle {
    /// Upright.
    Normal,
    /// Italic or oblique.
    Italic,
}

/// The `text-align` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    /// Align to the start edge.
    Left,
    /// Centre within the line box.
    Center,
    /// Centre the line box *and* any block child narrow enough to move.
    ///
    /// What `<center>` and `align="center"` actually do, and the difference
    /// between them and the CSS property: `<center><table></center>` was the
    /// commonest way to centre a table on the era's web, and plain
    /// `text-align: center` does not move a table at all. Browsers spell this
    /// `-webkit-center`; it is a separate value precisely so a stylesheet
    /// asking for centred *text* does not start moving boxes around.
    CenterBlocks,
    /// Align to the end edge.
    Right,
    /// Stretch to both edges.
    Justify,
}

impl TextAlign {
    /// Whether lines are centred.
    pub fn centres_text(self) -> bool {
        matches!(self, TextAlign::Center | TextAlign::CenterBlocks)
    }
}

/// The `text-decoration` property.
///
/// Not inherited in the usual sense: CSS 2.1 §16.3 says a decoration is drawn
/// across the whole of the element's inline content including its descendants,
/// which for our purposes amounts to propagating it downwards. The visible
/// consequence is that a link is underlined all the way through any `<b>` or
/// `<span>` inside it, which is how a link looked and how it was recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextDecoration {
    /// A line along the text's baseline.
    pub underline: bool,
    /// A line through the middle of the text.
    pub line_through: bool,
    /// A line above the text.
    pub overline: bool,
}

impl TextDecoration {
    /// Whether anything would be drawn.
    pub fn is_none(self) -> bool {
        !self.underline && !self.line_through && !self.overline
    }
}

/// Parses a `text-decoration` value, which is a space-separated list.
///
/// `none` clears everything, which is how a page turns off the underline the UA
/// sheet gives its links.
pub fn parse_text_decoration(words: &[String]) -> TextDecoration {
    let mut out = TextDecoration::default();
    for word in words {
        match word.as_str() {
            "underline" => out.underline = true,
            "line-through" => out.line_through = true,
            "overline" => out.overline = true,
            "none" => return TextDecoration::default(),
            // `blink` is recognised and deliberately ignored.
            _ => {}
        }
    }
    out
}

/// The `vertical-align` property, restricted to the values a table cell uses.
///
/// Only the cell case is modelled. Vertical alignment *within a line box* — a
/// superscript, an image raised off the baseline — is a different mechanism in
/// a different place, and the era's markup reaches for `valign` on cells far
/// more than for either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerticalAlign {
    /// Align the content's top with the cell's.
    Top,
    /// Centre it in the cell. The default for a cell, and the reason a short
    /// column looks centred against a long one unless told otherwise.
    #[default]
    Middle,
    /// Align the content's bottom with the cell's.
    Bottom,
    /// Align the first line's baseline with the row's.
    Baseline,
}

/// Parses a `vertical-align` keyword, or `None` for a value out of scope.
pub fn parse_vertical_align(name: &str) -> Option<VerticalAlign> {
    let value = match name {
        "top" | "text-top" => VerticalAlign::Top,
        "middle" => VerticalAlign::Middle,
        "bottom" | "text-bottom" => VerticalAlign::Bottom,
        "baseline" => VerticalAlign::Baseline,
        _ => return None,
    };
    Some(value)
}

/// The `overflow` property, as far as layout needs it.
///
/// **The visual effect is not implemented** — content that overflows a box
/// still paints outside it. This is here for the other thing `overflow` does,
/// which is structural rather than visual: any value but `visible` makes a box
/// establish a block formatting context, and margins do not collapse through
/// one (CSS 2.1 §8.3.1).
///
/// Recognising a property for one of its effects while not implementing the
/// other is worth being uneasy about, and it is still the better of the two
/// options: ignoring `overflow` entirely does not make the clipping appear, it
/// just also gets the margins wrong. The suite caught exactly that — a
/// container with `overflow: hidden` whose last child had a negative bottom
/// margin, which must not escape and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overflow {
    /// Content is visible outside the box. The initial value.
    #[default]
    Visible,
    /// Anything but `visible`. The distinction between `hidden`, `scroll` and
    /// `auto` matters only to clipping and scrollbars, neither of which exists
    /// here, so they are one value rather than three that behave identically.
    Clipped,
}

/// Parses an `overflow` keyword.
pub fn parse_overflow(name: &str) -> Option<Overflow> {
    let value = match name {
        "visible" => Overflow::Visible,
        "hidden" | "scroll" | "auto" => Overflow::Clipped,
        _ => return None,
    };
    Some(value)
}

/// The `background-repeat` property.
///
/// Tiling is the point: the era's pages were built on small images repeated
/// across the whole canvas, because that was the only way to get a texture
/// without paying for the bandwidth of a full-size one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundRepeat {
    /// Tile in both directions.
    #[default]
    Repeat,
    /// Tile horizontally only.
    RepeatX,
    /// Tile vertically only.
    RepeatY,
    /// Draw once.
    NoRepeat,
}

impl BackgroundRepeat {
    /// Whether the image tiles along each axis, as `(horizontal, vertical)`.
    pub fn axes(self) -> (bool, bool) {
        match self {
            BackgroundRepeat::Repeat => (true, true),
            BackgroundRepeat::RepeatX => (true, false),
            BackgroundRepeat::RepeatY => (false, true),
            BackgroundRepeat::NoRepeat => (false, false),
        }
    }
}

/// Parses a `background-repeat` keyword.
pub fn parse_background_repeat(name: &str) -> Option<BackgroundRepeat> {
    let value = match name {
        "repeat" => BackgroundRepeat::Repeat,
        "repeat-x" => BackgroundRepeat::RepeatX,
        "repeat-y" => BackgroundRepeat::RepeatY,
        "no-repeat" => BackgroundRepeat::NoRepeat,
        _ => return None,
    };
    Some(value)
}

/// The `background-position` property (CSS 2.1 §14.2.1).
///
/// Two [`Length`]s, and only `Px` and `Percent` ever appear in them: `em` is
/// resolved during the cascade against the element's own font size, which is
/// what "computed value: absolute length or percentage" means, and `auto` is
/// not a value this property takes.
///
/// The percentage is the interesting half, because it does not mean what a
/// percentage usually means. `50%` does not offset by half the box — it lines
/// the point halfway across the *image* up with the point halfway across the
/// *box*, so the resolved offset is `p × (box − image)` and goes negative when
/// the image is larger than the box. That is why this is resolved at paint
/// time: the cascade does not know how big the image is, and may not, since it
/// has not been fetched yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackgroundPosition {
    /// Horizontal component. `left` is `0%`, `center` `50%`, `right` `100%`.
    pub x: Length,
    /// Vertical component. `top` is `0%`, `center` `50%`, `bottom` `100%`.
    pub y: Length,
}

impl Default for BackgroundPosition {
    fn default() -> Self {
        Self {
            x: Length::Percent(0.0),
            y: Length::Percent(0.0),
        }
    }
}

/// Where the image's edge goes along one axis, relative to the box's.
///
/// `box_size` and `image_size` are along the same axis. A percentage resolves
/// against their *difference*, so an image wider than its box is pulled left
/// rather than pushed right — the correct and surprising half of §14.2.1. The
/// font size is not needed: `em` was already resolved during the cascade.
pub fn background_offset(component: Length, box_size: f32, image_size: f32) -> f32 {
    component.to_px(0.0, box_size - image_size)
}

/// One keyword of `background-position`, as the percentage it stands for.
///
/// Returned with which axes it may apply to: `left` and `right` are horizontal
/// only, `top` and `bottom` vertical only, and `center` is either. That is what
/// makes `top left` and `left top` both legal and `top bottom` not.
fn position_keyword(name: &str) -> Option<(Length, Axes)> {
    let value = match name {
        "left" => (Length::Percent(0.0), Axes::Horizontal),
        "right" => (Length::Percent(100.0), Axes::Horizontal),
        "top" => (Length::Percent(0.0), Axes::Vertical),
        "bottom" => (Length::Percent(100.0), Axes::Vertical),
        "center" => (Length::Percent(50.0), Axes::Either),
        _ => return None,
    };
    Some(value)
}

/// Which axis a `background-position` keyword can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axes {
    Horizontal,
    Vertical,
    Either,
}

/// Parses `background-position`, returning `None` if the whole value is invalid.
///
/// All-or-nothing on purpose. A declaration this does not understand must leave
/// the previous value alone rather than half-apply — CSS says an invalid
/// declaration is dropped, and half a position is worse than none, because it
/// puts the image somewhere the author never asked for.
///
/// `font_size` resolves `em`, which is the element's own here rather than the
/// parent's: unlike `font-size` itself, this property's lengths are relative to
/// the size it ends up with.
pub fn parse_background_position(values: &[Raw], font_size: f32) -> Option<BackgroundPosition> {
    /// A length, a percentage, or a keyword — the three shapes a component
    /// takes, with the keyword's axis kept so the pair can be checked.
    fn component(raw: &Raw, font_size: f32) -> Option<(Length, Axes)> {
        if let Raw::Ident(name) = raw {
            return position_keyword(name);
        }
        let length = crate::value::parse_length(raw)?;
        let resolved = match length {
            Length::Px(v) => Length::Px(v),
            Length::Em(v) => Length::Px(v * font_size),
            Length::Percent(v) => Length::Percent(v),
            // Not a value this property takes. Refused rather than treated as
            // zero, so the declaration is dropped as CSS requires.
            Length::Auto => return None,
        };
        Some((resolved, Axes::Either))
    }

    match values {
        // One value sets that axis and centres the other. Which axis depends on
        // the value: `background-position: top` is horizontally centred, not
        // `top` across and centre down, because `top` cannot be horizontal.
        [only] => {
            let (value, axes) = component(only, font_size)?;
            Some(match axes {
                Axes::Vertical => BackgroundPosition {
                    x: Length::Percent(50.0),
                    y: value,
                },
                _ => BackgroundPosition {
                    x: value,
                    y: Length::Percent(50.0),
                },
            })
        }
        [first, second] => {
            let (first_value, first_axes) = component(first, font_size)?;
            let (second_value, second_axes) = component(second, font_size)?;
            match (first_axes, second_axes) {
                // Written the other way round, which only keywords may do:
                // `top left` is legal and `0% left` is not, because a bare
                // length is horizontal by position rather than by meaning.
                (Axes::Vertical, Axes::Horizontal) => Some(BackgroundPosition {
                    x: second_value,
                    y: first_value,
                }),
                (Axes::Vertical, Axes::Either) if matches!(second, Raw::Ident(_)) => {
                    Some(BackgroundPosition {
                        x: second_value,
                        y: first_value,
                    })
                }
                // Everything else with an axis in the wrong place. Two of the
                // same one (`top bottom`), a vertical keyword followed by a
                // number (`top 50%`), or a horizontal keyword second
                // (`50% left`). CSS 2.1's grammar allows the reversed order
                // only when *both* components are keywords, which is what the
                // arms above cover; the rest is invalid and dropped rather than
                // guessed at.
                (Axes::Vertical, _) | (_, Axes::Horizontal) => None,
                _ => Some(BackgroundPosition {
                    x: first_value,
                    y: second_value,
                }),
            }
        }
        // Zero values, or the three- and four-value forms that arrived with
        // CSS3. Out of scope (ADR-0004) and refused rather than half-read.
        _ => None,
    }
}

/// The `list-style-type` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListStyleType {
    /// A filled circle.
    #[default]
    Disc,
    /// A hollow circle.
    Circle,
    /// A filled square.
    Square,
    /// 1, 2, 3.
    Decimal,
    /// a, b, c.
    LowerAlpha,
    /// A, B, C.
    UpperAlpha,
    /// i, ii, iii.
    LowerRoman,
    /// I, II, III.
    UpperRoman,
    /// No marker at all.
    None,
}

impl ListStyleType {
    /// Whether the marker counts items rather than repeating a glyph.
    pub fn is_ordered(self) -> bool {
        !matches!(
            self,
            ListStyleType::Disc
                | ListStyleType::Circle
                | ListStyleType::Square
                | ListStyleType::None
        )
    }

    /// The marker text for the item at `ordinal`, counting from one.
    ///
    /// Returns the text without its trailing separator; the caller adds the
    /// `.` that ordered lists carry, since unordered markers take none.
    pub fn marker(self, ordinal: usize) -> String {
        match self {
            ListStyleType::Disc => "\u{2022}".to_owned(),
            ListStyleType::Circle => "\u{25e6}".to_owned(),
            ListStyleType::Square => "\u{25aa}".to_owned(),
            ListStyleType::None => String::new(),
            ListStyleType::Decimal => format!("{ordinal}."),
            ListStyleType::LowerAlpha => format!("{}.", alphabetic(ordinal, 'a')),
            ListStyleType::UpperAlpha => format!("{}.", alphabetic(ordinal, 'A')),
            ListStyleType::LowerRoman => format!("{}.", roman(ordinal).to_lowercase()),
            ListStyleType::UpperRoman => format!("{}.", roman(ordinal)),
        }
    }
}

/// Parses a `list-style-type` keyword.
pub fn parse_list_style_type(name: &str) -> Option<ListStyleType> {
    let value = match name {
        "disc" => ListStyleType::Disc,
        "circle" => ListStyleType::Circle,
        "square" => ListStyleType::Square,
        "decimal" => ListStyleType::Decimal,
        "lower-alpha" | "lower-latin" => ListStyleType::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyleType::UpperAlpha,
        "lower-roman" => ListStyleType::LowerRoman,
        "upper-roman" => ListStyleType::UpperRoman,
        "none" => ListStyleType::None,
        _ => return None,
    };
    Some(value)
}

/// Bijective base-26: a, b, … z, aa, ab. Not ordinary base 26 — there is no
/// digit for zero, so `z` is followed by `aa` rather than by `ba`.
fn alphabetic(ordinal: usize, first: char) -> String {
    if ordinal == 0 {
        return String::new();
    }
    let mut out = Vec::new();
    let mut n = ordinal;
    while n > 0 {
        let digit = (n - 1) % 26;
        out.push((first as u8 + digit as u8) as char);
        n = (n - 1) / 26;
    }
    out.iter().rev().collect()
}

/// Roman numerals, in the subtractive form.
fn roman(ordinal: usize) -> String {
    // Above this the numeral system has no agreed notation, and a list that
    // long is not going to be read by its numbers anyway.
    if ordinal == 0 || ordinal > 3999 {
        return ordinal.to_string();
    }
    const TABLE: [(usize, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    let mut n = ordinal;
    for (value, numeral) in TABLE {
        while n >= value {
            out.push_str(numeral);
            n -= value;
        }
    }
    out
}

/// The `white-space` property, restricted to the values that change layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteSpace {
    /// Collapse runs of whitespace and wrap.
    Normal,
    /// Preserve whitespace and newlines, do not wrap.
    Pre,
    /// Collapse whitespace, do not wrap.
    NoWrap,
}

/// A generic font family, per CSS 2.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericFamily {
    /// Serif.
    Serif,
    /// Sans-serif.
    SansSerif,
    /// Monospace.
    Monospace,
    /// Cursive. Resolves to sans-serif for now (ADR-0008, issue #6).
    Cursive,
    /// Fantasy. Resolves to sans-serif for now (ADR-0008, issue #6).
    Fantasy,
}

/// A `font-family` list: requested names in order, ending in a generic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontStack {
    /// Family names as authored, in preference order.
    pub families: Vec<String>,
    /// The generic to fall back to.
    pub generic: GenericFamily,
}

impl Default for FontStack {
    fn default() -> Self {
        Self {
            families: Vec::new(),
            generic: GenericFamily::Serif,
        }
    }
}

/// The `position` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// In normal flow.
    #[default]
    Static,
    /// In normal flow, then shifted; the space it would have taken is kept.
    Relative,
    /// Out of flow, placed against the nearest positioned ancestor.
    Absolute,
    /// Out of flow, placed against the viewport.
    Fixed,
}

impl Position {
    /// Whether this element establishes a containing block for absolutely
    /// positioned descendants.
    pub fn is_positioned(self) -> bool {
        self != Position::Static
    }

    /// Whether the element is removed from normal flow.
    pub fn is_out_of_flow(self) -> bool {
        matches!(self, Position::Absolute | Position::Fixed)
    }
}

/// Parses a `position` keyword.
pub fn parse_position(name: &str) -> Option<Position> {
    match name {
        "static" => Some(Position::Static),
        "relative" => Some(Position::Relative),
        "absolute" => Some(Position::Absolute),
        "fixed" => Some(Position::Fixed),
        _ => None,
    }
}

/// The `top`, `right`, `bottom`, and `left` offsets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Offsets {
    /// `top`.
    pub top: Length,
    /// `right`.
    pub right: Length,
    /// `bottom`.
    pub bottom: Length,
    /// `left`.
    pub left: Length,
}

impl Default for Offsets {
    fn default() -> Self {
        Self {
            top: Length::Auto,
            right: Length::Auto,
            bottom: Length::Auto,
            left: Length::Auto,
        }
    }
}

/// The `float` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Float {
    /// Not floated.
    #[default]
    None,
    /// Floated to the left; content flows down its right side.
    Left,
    /// Floated to the right.
    Right,
}

/// The `clear` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Clear {
    /// Does not clear.
    #[default]
    None,
    /// Moves below any left float.
    Left,
    /// Moves below any right float.
    Right,
    /// Moves below every float.
    Both,
}

/// Parses a `float` keyword.
pub fn parse_float(name: &str) -> Option<Float> {
    match name {
        "none" => Some(Float::None),
        "left" => Some(Float::Left),
        "right" => Some(Float::Right),
        _ => None,
    }
}

/// Parses a `clear` keyword.
pub fn parse_clear(name: &str) -> Option<Clear> {
    match name {
        "none" => Some(Clear::None),
        "left" => Some(Clear::Left),
        "right" => Some(Clear::Right),
        "both" => Some(Clear::Both),
        _ => None,
    }
}

/// The `border-style` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderStyle {
    /// No border box is generated.
    #[default]
    None,
    /// Generates space but paints nothing.
    Hidden,
    /// A solid line.
    Solid,
    /// Dotted. Painted solid for now — see `BorderStyle::is_visible`.
    Dotted,
    /// Dashed. Painted solid for now.
    Dashed,
    /// Two lines. Painted solid for now.
    Double,
    /// Carved. Painted solid for now.
    Groove,
    /// Embossed. Painted solid for now.
    Ridge,
    /// Inset. Painted solid for now.
    Inset,
    /// Outset. Painted solid for now.
    Outset,
}

impl BorderStyle {
    /// Whether this style paints anything.
    ///
    /// Every non-`none` style reserves space, but `hidden` deliberately paints
    /// nothing. The decorative styles all currently paint as solid: their
    /// *metrics* are right, which is what layout depends on, and drawing dots
    /// and bevels is cosmetic work that would not change any box's position.
    pub fn is_visible(self) -> bool {
        !matches!(self, BorderStyle::None | BorderStyle::Hidden)
    }

    /// Whether this style reserves space, even if it paints nothing.
    pub fn reserves_space(self) -> bool {
        self != BorderStyle::None
    }

    fn parse(name: &str) -> Option<Self> {
        let style = match name {
            "none" => BorderStyle::None,
            "hidden" => BorderStyle::Hidden,
            "solid" => BorderStyle::Solid,
            "dotted" => BorderStyle::Dotted,
            "dashed" => BorderStyle::Dashed,
            "double" => BorderStyle::Double,
            "groove" => BorderStyle::Groove,
            "ridge" => BorderStyle::Ridge,
            "inset" => BorderStyle::Inset,
            "outset" => BorderStyle::Outset,
            _ => return None,
        };
        Some(style)
    }
}

/// Parses a `border-style` keyword.
pub fn parse_border_style(name: &str) -> Option<BorderStyle> {
    BorderStyle::parse(name)
}

/// The `medium` border width, and the initial value.
pub const MEDIUM_BORDER: f32 = 3.0;

/// One edge of a border.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderSide {
    /// Declared width, used only when the style reserves space.
    pub width: Length,
    /// Line style.
    pub style: BorderStyle,
    /// Line colour, or `None` to use the element's `color`.
    pub color: Option<Color>,
}

impl Default for BorderSide {
    fn default() -> Self {
        Self {
            width: Length::Px(MEDIUM_BORDER),
            style: BorderStyle::None,
            color: None,
        }
    }
}

impl BorderSide {
    /// Width actually occupied, in pixels.
    ///
    /// `border-width` is ignored unless a style is set — the single most common
    /// authoring mistake with borders is expecting `border-width: 1px` alone to
    /// draw something.
    pub fn used_width(&self, font_size: f32) -> f32 {
        if self.style.reserves_space() {
            self.width.to_px(font_size, 0.0).max(0.0)
        } else {
            0.0
        }
    }
}

/// The four border edges.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Borders {
    /// Top edge.
    pub top: BorderSide,
    /// Right edge.
    pub right: BorderSide,
    /// Bottom edge.
    pub bottom: BorderSide,
    /// Left edge.
    pub left: BorderSide,
}

/// Lengths on the four sides of a box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edges {
    /// Top edge.
    pub top: Length,
    /// Right edge.
    pub right: Length,
    /// Bottom edge.
    pub bottom: Length,
    /// Left edge.
    pub left: Length,
}

impl Edges {
    /// All four edges set to the same length.
    pub const fn all(length: Length) -> Self {
        Self {
            top: length,
            right: length,
            bottom: length,
            left: length,
        }
    }

    /// All four edges zero.
    pub const ZERO: Self = Self::all(Length::Px(0.0));
}

/// Fully resolved style for one element.
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    /// `display`.
    pub display: Display,
    /// `color`, inherited.
    pub color: Color,
    /// `background-color`.
    pub background_color: Color,
    /// `background-image`, as the URL was authored. Resolved against the
    /// document's base when the image is fetched, not here: the cascade has no
    /// business knowing where the document came from.
    pub background_image: Option<String>,
    /// `background-repeat`.
    pub background_repeat: BackgroundRepeat,
    /// `overflow`, used for formatting-context effects only.
    pub overflow: Overflow,
    /// `background-position`.
    pub background_position: BackgroundPosition,
    /// `vertical-align`, as it applies to a table cell.
    pub vertical_align: VerticalAlign,
    /// `border-spacing`, the gap between cell borders in the separated model.
    ///
    /// On a table only. Kept as a length rather than pixels because it is
    /// resolved against the table's own font size, like any other length.
    pub border_spacing: Length,
    /// `font-family`, inherited.
    pub font_family: FontStack,
    /// `font-size` in pixels, inherited.
    pub font_size: f32,
    /// `font-weight` as a numeric weight, inherited.
    pub font_weight: u16,
    /// `font-style`, inherited.
    pub font_style: FontStyle,
    /// `line-height` in pixels, inherited.
    pub line_height: f32,
    /// `text-align`, inherited.
    pub text_align: TextAlign,
    /// `white-space`, inherited.
    pub white_space: WhiteSpace,
    /// `text-decoration`, propagated to inline descendants (CSS 2.1 §16.3).
    pub text_decoration: TextDecoration,
    /// `list-style-type`, inherited so a list's items pick it up from the list.
    pub list_style_type: ListStyleType,
    /// `margin`.
    pub margin: Edges,
    /// `padding`.
    pub padding: Edges,
    /// `border`.
    pub border: Borders,
    /// `position`.
    pub position: Position,
    /// `top`/`right`/`bottom`/`left`.
    pub offsets: Offsets,
    /// `float`.
    pub float: Float,
    /// `clear`.
    pub clear: Clear,
    /// `width`.
    pub width: Length,
    /// `height`.
    pub height: Length,
}

/// The CSS 2.1 initial value of `border-spacing`.
///
/// Two pixels, and it matters: getting it wrong by 2px per edge is plainly
/// visible on a dense table, which the era's pages are full of.
pub const DEFAULT_BORDER_SPACING: f32 = 2.0;

/// The initial font size, and the basis for `em` at the root.
pub const DEFAULT_FONT_SIZE: f32 = 16.0;

/// `line-height: normal`, as a multiple of font size.
pub const NORMAL_LINE_HEIGHT: f32 = 1.2;

impl Default for ComputedStyle {
    fn default() -> Self {
        Self {
            display: Display::Inline,
            color: Color::BLACK,
            background_color: Color::TRANSPARENT,
            background_image: None,
            background_repeat: BackgroundRepeat::Repeat,
            background_position: BackgroundPosition::default(),
            overflow: Overflow::Visible,
            vertical_align: VerticalAlign::Middle,
            border_spacing: Length::Px(DEFAULT_BORDER_SPACING),
            font_family: FontStack::default(),
            font_size: DEFAULT_FONT_SIZE,
            font_weight: 400,
            font_style: FontStyle::Normal,
            line_height: DEFAULT_FONT_SIZE * NORMAL_LINE_HEIGHT,
            text_align: TextAlign::Left,
            white_space: WhiteSpace::Normal,
            text_decoration: TextDecoration::default(),
            list_style_type: ListStyleType::Disc,
            margin: Edges::ZERO,
            padding: Edges::ZERO,
            border: Borders::default(),
            position: Position::Static,
            offsets: Offsets::default(),
            float: Float::None,
            clear: Clear::None,
            width: Length::Auto,
            height: Length::Auto,
        }
    }
}

impl ComputedStyle {
    /// A child's starting style: inherited properties carried over, everything
    /// else reset to its initial value.
    pub fn inherit_from(parent: &ComputedStyle) -> Self {
        Self {
            color: parent.color,
            font_family: parent.font_family.clone(),
            font_size: parent.font_size,
            font_weight: parent.font_weight,
            font_style: parent.font_style,
            line_height: parent.line_height,
            text_align: parent.text_align,
            white_space: parent.white_space,
            list_style_type: parent.list_style_type,
            ..Self::default()
        }
    }

    /// Whether text in this style should be rendered bold.
    pub fn is_bold(&self) -> bool {
        self.font_weight >= 600
    }
}

/// Parses a `display` keyword.
pub fn parse_display(name: &str) -> Option<Display> {
    Display::parse(name)
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    #[test]
    fn unordered_markers_are_a_fixed_glyph() {
        assert_eq!(ListStyleType::Disc.marker(1), "\u{2022}");
        assert_eq!(ListStyleType::Disc.marker(9), "\u{2022}", "not a count");
        assert_eq!(ListStyleType::None.marker(1), "");
    }

    #[test]
    fn decimal_markers_count() {
        assert_eq!(ListStyleType::Decimal.marker(1), "1.");
        assert_eq!(ListStyleType::Decimal.marker(42), "42.");
    }

    #[test]
    fn alphabetic_markers_are_bijective_base_26() {
        // There is no digit for zero, so `z` is followed by `aa`, not `ba`.
        // Ordinary base 26 gets this wrong from the 27th item onwards.
        assert_eq!(ListStyleType::LowerAlpha.marker(1), "a.");
        assert_eq!(ListStyleType::LowerAlpha.marker(26), "z.");
        assert_eq!(ListStyleType::LowerAlpha.marker(27), "aa.");
        assert_eq!(ListStyleType::LowerAlpha.marker(52), "az.");
        assert_eq!(ListStyleType::LowerAlpha.marker(53), "ba.");
        assert_eq!(ListStyleType::UpperAlpha.marker(28), "AB.");
    }

    #[test]
    fn roman_markers_use_the_subtractive_forms() {
        for (ordinal, expected) in [
            (1, "I."),
            (4, "IV."),
            (9, "IX."),
            (14, "XIV."),
            (40, "XL."),
            (1990, "MCMXC."),
            (3999, "MMMCMXCIX."),
        ] {
            assert_eq!(ListStyleType::UpperRoman.marker(ordinal), expected);
        }
        assert_eq!(ListStyleType::LowerRoman.marker(4), "iv.");
        // Past the point where the notation is agreed, fall back to digits
        // rather than emitting a wall of Ms.
        assert_eq!(ListStyleType::UpperRoman.marker(4000), "4000.");
    }

    #[test]
    fn only_counting_markers_are_ordered() {
        assert!(ListStyleType::Decimal.is_ordered());
        assert!(ListStyleType::LowerRoman.is_ordered());
        assert!(!ListStyleType::Disc.is_ordered());
        assert!(!ListStyleType::None.is_ordered());
    }

    #[test]
    fn text_decoration_parses_combinations_and_none() {
        let parse =
            |s: &str| parse_text_decoration(&s.split(' ').map(str::to_owned).collect::<Vec<_>>());
        assert!(parse("underline").underline);
        let both = parse("underline line-through");
        assert!(both.underline && both.line_through);
        assert!(parse("none").is_none());
        // `none` anywhere in the list clears the lot.
        assert!(parse("underline none").is_none());
        assert!(parse("blink").is_none(), "blink is recognised and ignored");
    }
}
