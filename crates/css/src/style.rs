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
    /// Laid out: the box is sized by its own content, placed on a line as one
    /// atom, and aligned on the baseline of its own last line (§10.3.9,
    /// §10.8.1). It was for a long time the one unimplemented thing here that
    /// failed *silently* — laid out as a plain inline, so its width, height,
    /// border and background were dropped and an empty spacer vanished — which
    /// is why it is called out here rather than left to the enum.
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
    /// `table-column` and `table-column-group`.
    ///
    /// A column box sizes a column and paints its own background; §17.2 gives
    /// it no content of its own, and anything inside it is not rendered. This
    /// engine does not lay out column boxes at all — a table's widths come
    /// from its cells — so the variant exists to say "generates no content
    /// box", which is the part that is observable. Mapping these to `Block`,
    /// as the catch-all below used to, made a `::before` with
    /// `display: table-column` draw its content, and the suite puts the word
    /// FAIL in exactly that place.
    TableColumn,
    /// `table-column-group` — a band of columns.
    ///
    /// Apart from `TableColumn` because a group's extent is its `table-column`
    /// children where it has any, and its own `span` where it has none; the
    /// grid cannot tell those two apart from one variant.
    TableColumnGroup,
    /// `table-caption` — a table's heading, outside its border box (§17.4).
    TableCaption,
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
    /// `InlineBlock` used to be here alongside flex and grid, because it was
    /// laid out as a plain inline and so failed silently — the content still
    /// appeared and only the box was lost. It is laid out properly now, so a
    /// page built out of inline-blocks no longer falls back to document mode.
    ///
    /// This is a share, not a switch: the classifier weighs how much of the
    /// page's text sits under unsupported layout, so one flex container in a
    /// navigation bar does not push an article into document mode, and a page
    /// whose body depends on them does.
    pub fn is_supported_layout(self) -> bool {
        !matches!(self, Display::Flex | Display::Grid)
    }

    /// Whether the box participates in inline layout.
    pub fn is_inline(self) -> bool {
        matches!(self, Display::Inline | Display::InlineBlock)
    }

    /// Whether the box is internal table structure, laid out by the table
    /// rather than by normal block flow.
    ///
    /// A caption is here too. It is not *inside* the table's border box —
    /// §17.4 makes it a sibling — but it is the table that places it, and a
    /// block walk that laid it out as an ordinary child would draw it twice.
    pub fn is_table_internal(self) -> bool {
        matches!(
            self,
            Display::TableRow
                | Display::TableRowGroup
                | Display::TableCell
                | Display::TableColumn
                | Display::TableColumnGroup
                | Display::TableCaption
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
            "table-column" => Display::TableColumn,
            "table-column-group" => Display::TableColumnGroup,
            "table-caption" => Display::TableCaption,
            // Nothing else in CSS 2.1 begins with `table`, so this catches a
            // misspelling rather than a value. A block keeps its content
            // visible, which is the better of the two ways to be wrong.
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

/// The `font-variant` property (§15.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontVariant {
    /// Ordinary glyphs.
    #[default]
    Normal,
    /// Lowercase letters drawn as smaller capitals.
    SmallCaps,
}

/// Parses a `font-variant` keyword.
pub fn parse_font_variant(name: &str) -> Option<FontVariant> {
    match name.to_ascii_lowercase().as_str() {
        "normal" => Some(FontVariant::Normal),
        "small-caps" => Some(FontVariant::SmallCaps),
        _ => None,
    }
}

/// The `direction` property (§8.6): which way inline content runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Left to right. The initial value.
    #[default]
    Ltr,
    /// Right to left.
    Rtl,
}

/// Parses a `direction` keyword.
pub fn parse_direction(name: &str) -> Option<Direction> {
    match name.to_ascii_lowercase().as_str() {
        "ltr" => Some(Direction::Ltr),
        "rtl" => Some(Direction::Rtl),
        _ => None,
    }
}

/// The `unicode-bidi` property (§8.6): how an element joins the bidi algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnicodeBidi {
    /// The element's text takes part in the surrounding paragraph's ordering.
    #[default]
    Normal,
    /// The element opens an embedding at its own `direction`.
    Embed,
    /// The element's characters are forced to its own `direction`, whatever
    /// they are — the property's equivalent of U+202D/U+202E.
    BidiOverride,
}

/// Parses a `unicode-bidi` keyword.
pub fn parse_unicode_bidi(name: &str) -> Option<UnicodeBidi> {
    match name.to_ascii_lowercase().as_str() {
        "normal" => Some(UnicodeBidi::Normal),
        "embed" => Some(UnicodeBidi::Embed),
        "bidi-override" => Some(UnicodeBidi::BidiOverride),
        _ => None,
    }
}

/// The `text-align` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    /// The initial value: whichever edge `direction` makes the start one.
    ///
    /// CSS 2.1 §16.2 writes the initial value as "`left` if `direction` is
    /// `ltr`, `right` if it is `rtl`", which is not a value any stylesheet can
    /// name and so has to be one this enum can hold. Resolving it at the point
    /// of use rather than in the cascade is what keeps it right through
    /// inheritance: `text-align` inherits and `direction` inherits separately,
    /// so a child can be handed this from one ancestor and its direction from
    /// another.
    Start,
    /// Align to the left edge.
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

    /// The value with [`TextAlign::Start`] settled against a direction.
    pub fn against(self, direction: Direction) -> Self {
        match (self, direction) {
            (TextAlign::Start, Direction::Ltr) => TextAlign::Left,
            (TextAlign::Start, Direction::Rtl) => TextAlign::Right,
            (align, _) => align,
        }
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

/// The `vertical-align` property, restricted to its keyword values.
///
/// Two mechanisms share one property. In a table cell it aligns the cell's
/// content within the row; on an atomic inline box — an image, an inline-block
/// — it decides where the box hangs on the line. Raising and lowering *text*
/// (a superscript, a `<sub>`) is the part still not modelled: that needs a
/// baseline shift applied to a run's glyphs, which is a different place again.
///
/// A cell's `middle` comes from the UA sheet rather than from this enum's
/// default, because the two contexts disagree about what "not stated" means:
/// CSS's initial value is `baseline`, which is what an inline-block must get,
/// and what a cell must not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerticalAlign {
    /// Align the box's or content's top with the line box's or cell's.
    Top,
    /// Centre it: in the cell, or against the middle of the parent's text.
    Middle,
    /// Align the box's or content's bottom with the line box's or cell's.
    Bottom,
    /// Sit on the baseline — the initial value, and for a cell the row's.
    #[default]
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
    /// 01, 02, 03 — padded to two digits, and no wider than the number needs.
    DecimalLeadingZero,
    /// a, b, c.
    LowerAlpha,
    /// A, B, C.
    UpperAlpha,
    /// i, ii, iii.
    LowerRoman,
    /// I, II, III.
    UpperRoman,
    /// Lowercase classical Greek: alpha, beta, gamma.
    LowerGreek,
    /// Traditional Armenian numbering, which is additive like Roman.
    Armenian,
    /// Traditional Georgian numbering, likewise additive.
    Georgian,
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
            _ => format!("{}.", self.counter(ordinal)),
        }
    }

    /// The same ordinal as a bare counter value, with no trailing stop.
    ///
    /// §12.4.3's `counter()` prints the number and nothing else: the full stop
    /// in a list marker is the marker's, not the number's, and
    /// `content: counter(chapter) ". "` writes its own.
    ///
    /// §12.4.3 supports every `list-style-type` here, the glyph ones included:
    /// `counter(c, square)` prints a square, not nothing. It read the other way
    /// here for a while, and the suite could not tell — the test that checks it
    /// has a reference built out of `list-style-position: inside` markers,
    /// which this engine also drew nowhere, so a blank matched a blank.
    ///
    /// `none` is the one that prints nothing, and it is the only one.
    pub fn counter(self, ordinal: usize) -> String {
        match self {
            ListStyleType::Disc | ListStyleType::Circle | ListStyleType::Square => {
                self.marker(ordinal)
            }
            ListStyleType::None => String::new(),
            ListStyleType::Decimal => format!("{ordinal}"),
            ListStyleType::DecimalLeadingZero => format!("{ordinal:02}"),
            ListStyleType::LowerAlpha => alphabetic(ordinal, 'a'),
            ListStyleType::UpperAlpha => alphabetic(ordinal, 'A'),
            ListStyleType::LowerRoman => roman(ordinal).to_lowercase(),
            ListStyleType::UpperRoman => roman(ordinal),
            ListStyleType::LowerGreek => greek(ordinal),
            ListStyleType::Armenian => additive(ordinal, &ARMENIAN, 9999),
            ListStyleType::Georgian => additive(ordinal, &GEORGIAN, 19999),
        }
    }
}

/// The four offsets of a `clip: rect(…)`.
///
/// Every one is measured from the *top-left* of the border box, including
/// `right` and `bottom` — they are not insets from the far edges, which is
/// the trap in this property and the reason it is worth a type of its own.
/// CSS 2.1 §11.1.2 is explicit about it, and later specifications kept the
/// shape for compatibility rather than because anyone liked it.
///
/// `None` on a side is `auto`: that edge of the clip is the border edge, so
/// the side does not clip.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ClipRect {
    /// Distance down from the border box's top edge.
    pub top: Option<Length>,
    /// Distance right from the border box's *left* edge.
    pub right: Option<Length>,
    /// Distance down from the border box's *top* edge.
    pub bottom: Option<Length>,
    /// Distance right from the border box's left edge.
    pub left: Option<Length>,
}

/// Parses `clip: rect(t, r, b, l)`, or `auto`.
///
/// Both separators are accepted. CSS 2.1 specifies commas and notes that
/// implementations also took spaces, which the era's pages duly used.
pub fn parse_clip(values: &[Raw]) -> Option<Option<ClipRect>> {
    if let [Raw::Ident(name)] = values {
        return (name == "auto").then_some(None);
    }
    let [Raw::Function(name, args)] = values else {
        return None;
    };
    if name != "rect" {
        return None;
    }
    let sides: Vec<Option<Length>> = args
        .iter()
        .filter(|arg| !matches!(arg, Raw::Comma))
        .map(|arg| match arg {
            Raw::Ident(name) if name == "auto" => Some(None),
            other => crate::value::parse_length(other).map(Some),
        })
        .collect::<Option<Vec<_>>>()?;
    let [top, right, bottom, left] = sides.as_slice() else {
        return None;
    };
    Some(Some(ClipRect {
        top: *top,
        right: *right,
        bottom: *bottom,
        left: *left,
    }))
}

/// `list-style-position` (§12.5.1): where the marker sits relative to the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListStylePosition {
    /// Outside the item's box, in the list's own padding. The initial value.
    #[default]
    Outside,
    /// The first inline box of the item's content, which text flows after and
    /// wraps *under* rather than beside.
    Inside,
}

/// Parses a `list-style-position` keyword.
pub fn parse_list_style_position(name: &str) -> Option<ListStylePosition> {
    match name.to_ascii_lowercase().as_str() {
        "outside" => Some(ListStylePosition::Outside),
        "inside" => Some(ListStylePosition::Inside),
        _ => None,
    }
}

/// Parses a `list-style-type` keyword.
pub fn parse_list_style_type(name: &str) -> Option<ListStyleType> {
    let value = match name {
        "disc" => ListStyleType::Disc,
        "circle" => ListStyleType::Circle,
        "square" => ListStyleType::Square,
        "decimal" => ListStyleType::Decimal,
        "decimal-leading-zero" => ListStyleType::DecimalLeadingZero,
        "lower-alpha" | "lower-latin" => ListStyleType::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyleType::UpperAlpha,
        "lower-roman" => ListStyleType::LowerRoman,
        "upper-roman" => ListStyleType::UpperRoman,
        "lower-greek" => ListStyleType::LowerGreek,
        "armenian" => ListStyleType::Armenian,
        "georgian" => ListStyleType::Georgian,
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
/// The classical Greek alphabet, which is 24 letters and not 25.
///
/// Final sigma is absent: it is a positional form of the same letter, so a
/// list numbered with it would count sigma twice. CSS 2.1 says "lowercase
/// classical Greek" and means exactly this sequence.
const GREEK: [char; 24] = [
    '\u{3b1}', '\u{3b2}', '\u{3b3}', '\u{3b4}', '\u{3b5}', '\u{3b6}', '\u{3b7}', '\u{3b8}',
    '\u{3b9}', '\u{3ba}', '\u{3bb}', '\u{3bc}', '\u{3bd}', '\u{3be}', '\u{3bf}', '\u{3c0}',
    '\u{3c1}', '\u{3c3}', '\u{3c4}', '\u{3c5}', '\u{3c6}', '\u{3c7}', '\u{3c8}', '\u{3c9}',
];

/// Traditional Armenian numbering: nine ones, nine tens, nine hundreds, nine
/// thousands, each its own letter, written largest first and added up.
///
/// Additive rather than positional, so there is no zero and nothing to carry:
/// 1996 is 1000 + 900 + 90 + 6, four letters, one per non-zero digit.
const ARMENIAN: [(usize, char); 36] = [
    (9000, '\u{554}'),
    (8000, '\u{553}'),
    (7000, '\u{552}'),
    (6000, '\u{551}'),
    (5000, '\u{550}'),
    (4000, '\u{54f}'),
    (3000, '\u{54e}'),
    (2000, '\u{54d}'),
    (1000, '\u{54c}'),
    (900, '\u{54b}'),
    (800, '\u{54a}'),
    (700, '\u{549}'),
    (600, '\u{548}'),
    (500, '\u{547}'),
    (400, '\u{546}'),
    (300, '\u{545}'),
    (200, '\u{544}'),
    (100, '\u{543}'),
    (90, '\u{542}'),
    (80, '\u{541}'),
    (70, '\u{540}'),
    (60, '\u{53f}'),
    (50, '\u{53e}'),
    (40, '\u{53d}'),
    (30, '\u{53c}'),
    (20, '\u{53b}'),
    (10, '\u{53a}'),
    (9, '\u{539}'),
    (8, '\u{538}'),
    (7, '\u{537}'),
    (6, '\u{536}'),
    (5, '\u{535}'),
    (4, '\u{534}'),
    (3, '\u{533}'),
    (2, '\u{532}'),
    (1, '\u{531}'),
];

/// Traditional Georgian numbering, built the same way and reaching ten
/// thousand, which Armenian does not.
const GEORGIAN: [(usize, char); 37] = [
    (10000, '\u{10f5}'),
    (9000, '\u{10f0}'),
    (8000, '\u{10ef}'),
    (7000, '\u{10f4}'),
    (6000, '\u{10ee}'),
    (5000, '\u{10ed}'),
    (4000, '\u{10ec}'),
    (3000, '\u{10eb}'),
    (2000, '\u{10ea}'),
    (1000, '\u{10e9}'),
    (900, '\u{10e8}'),
    (800, '\u{10e7}'),
    (700, '\u{10e6}'),
    (600, '\u{10e5}'),
    (500, '\u{10e4}'),
    (400, '\u{10f3}'),
    (300, '\u{10e2}'),
    (200, '\u{10e1}'),
    (100, '\u{10e0}'),
    (90, '\u{10df}'),
    (80, '\u{10de}'),
    (70, '\u{10dd}'),
    (60, '\u{10f2}'),
    (50, '\u{10dc}'),
    (40, '\u{10db}'),
    (30, '\u{10da}'),
    (20, '\u{10d9}'),
    (10, '\u{10d8}'),
    (9, '\u{10d7}'),
    (8, '\u{10f1}'),
    (7, '\u{10d6}'),
    (6, '\u{10d5}'),
    (5, '\u{10d4}'),
    (4, '\u{10d3}'),
    (3, '\u{10d2}'),
    (2, '\u{10d1}'),
    (1, '\u{10d0}'),
];

/// Lowercase classical Greek, wrapping past omega the way the alphabetic
/// systems do: alpha, … omega, then alpha alpha.
///
/// CSS 2.1 does not say what happens past the twenty-fourth item, and every
/// browser repeats the letter. Doing something else would number a long list
/// with digits halfway down it.
fn greek(ordinal: usize) -> String {
    if ordinal == 0 {
        return String::new();
    }
    let mut out = Vec::new();
    let mut n = ordinal;
    while n > 0 {
        out.push(GREEK[(n - 1) % GREEK.len()]);
        n = (n - 1) / GREEK.len();
    }
    out.iter().rev().collect()
}

/// An additive numeral system: the largest letter that fits, repeatedly.
///
/// Outside `limit` the system has no notation at all — unlike Roman, where
/// the convention merely runs out — so the number is written in digits, which
/// is what a reader can still use.
fn additive(ordinal: usize, table: &[(usize, char)], limit: usize) -> String {
    if ordinal == 0 || ordinal > limit {
        return ordinal.to_string();
    }
    let mut out = String::new();
    let mut n = ordinal;
    for &(value, letter) in table {
        while n >= value {
            out.push(letter);
            n -= value;
        }
    }
    out
}

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

/// The `text-transform` property (CSS 2.1 §16.5).
///
/// Applied to the text before it is shaped rather than at paint time, because
/// it changes how wide the text is: `uppercase` is wider than what it replaced
/// in every face here, so a line that was measured lowercase would wrap in the
/// wrong place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextTransform {
    /// Left as written. The initial value.
    #[default]
    None,
    /// Every character uppercased.
    Uppercase,
    /// Every character lowercased.
    Lowercase,
    /// The first letter of each word uppercased, the rest left alone.
    Capitalize,
}

impl TextTransform {
    /// Applies the transform to a run of text.
    ///
    /// `capitalize` uppercases the first letter of each *word* and leaves the
    /// rest as the author wrote it — not lowercasing the remainder, which is
    /// what §16.5 says and what stops `HTML` becoming `Html`.
    pub fn apply(self, text: &str) -> String {
        match self {
            TextTransform::None => text.to_owned(),
            TextTransform::Uppercase => text.to_uppercase(),
            TextTransform::Lowercase => text.to_lowercase(),
            TextTransform::Capitalize => {
                let mut out = String::with_capacity(text.len());
                let mut at_start = true;
                for ch in text.chars() {
                    if at_start && ch.is_alphanumeric() {
                        out.extend(ch.to_uppercase());
                        at_start = false;
                    } else {
                        out.push(ch);
                        // A word boundary is whitespace here. Punctuation does
                        // not start a new word, so `o'clock` is not `O'Clock`.
                        at_start = ch.is_whitespace();
                    }
                }
                out
            }
        }
    }
}

/// Parses a `text-transform` keyword.
pub fn parse_text_transform(name: &str) -> Option<TextTransform> {
    match name {
        "none" => Some(TextTransform::None),
        "uppercase" => Some(TextTransform::Uppercase),
        "lowercase" => Some(TextTransform::Lowercase),
        "capitalize" => Some(TextTransform::Capitalize),
        _ => None,
    }
}

/// The `visibility` property (CSS 2.1 §11.2).
///
/// Not `display: none` with a different name, and the difference is the whole
/// point: a hidden box still takes up exactly the room it would have taken, so
/// the layout around it does not move. That is what an author reaches for it
/// *for* — a menu that appears without shifting the page, a spacer that holds a
/// column open.
///
/// Inherited, and that is what makes it useful rather than merely
/// per-element: hiding a container hides everything inside it. A descendant can
/// still set `visibility: visible` and reappear inside a hidden ancestor, which
/// falls out of inheritance rather than needing a rule of its own — and is the
/// one thing about this property that surprises people.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Drawn. The initial value.
    #[default]
    Visible,
    /// Not drawn, but still occupying its space.
    Hidden,
}

/// Parses a `visibility` keyword.
///
/// `collapse` is treated as `hidden`, which is exactly what CSS 2.1 §11.2 says
/// to do everywhere except on a table row or column — where it should remove
/// the track and let the rest of the table close up. That part is not
/// implemented, so a `collapse` row hides its contents and keeps its height.
pub fn parse_visibility(name: &str) -> Option<Visibility> {
    match name {
        "visible" => Some(Visibility::Visible),
        "hidden" | "collapse" => Some(Visibility::Hidden),
        _ => None,
    }
}

/// The `caption-side` property (CSS 2.1 §17.4.1).
///
/// Which side of the table its caption sits on. Inherited, because it is set on
/// the table and read on the caption — the same shape as `border-collapse`, and
/// for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptionSide {
    /// Above the table. The initial value.
    #[default]
    Top,
    /// Below it.
    Bottom,
}

/// Parses a `caption-side` keyword.
///
/// `left` and `right` are CSS 2.0 values that CSS 2.1 dropped, and no browser
/// kept them; they are refused rather than guessed at, which leaves the
/// declaration invalid and the caption where the initial value puts it.
pub fn parse_caption_side(name: &str) -> Option<CaptionSide> {
    match name {
        "top" => Some(CaptionSide::Top),
        "bottom" => Some(CaptionSide::Bottom),
        _ => None,
    }
}

/// The `border-collapse` property (CSS 2.1 §17.6).
///
/// Which of the two border models a table uses, and the two are not variations
/// on each other. In the separated model every cell draws its own border and
/// `border-spacing` sits between them. In the collapsing model the borders of
/// adjoining cells — and of the rows, row groups, columns, column groups and
/// the table itself — are resolved against one another into a single border
/// centred on the grid line between them, `border-spacing` and `empty-cells`
/// stop applying, and the table has no padding.
///
/// Inherited, which is what makes `table { border-collapse: collapse }` on a
/// page of nested tables do what its author meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderCollapse {
    /// Each cell draws its own border. The initial value.
    #[default]
    Separate,
    /// Adjoining borders collapse into one, centred on the grid line.
    Collapse,
}

/// `empty-cells`: whether a cell with nothing in it draws itself.
///
/// Separated model only (§17.6.1.1). In the collapsing model a cell has no
/// border of its own to hide — it shares the grid line — and CSS 2.1 says the
/// property does not apply there at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmptyCells {
    /// An empty cell draws its border and background like any other. The
    /// initial value, and what the era's table layouts depend on: a spacer cell
    /// with a `bgcolor` and no content is how a coloured rule was drawn.
    #[default]
    Show,
    /// An empty cell draws neither, leaving the table's background showing.
    Hide,
}

/// Parses an `empty-cells` keyword.
pub fn parse_empty_cells(name: &str) -> Option<EmptyCells> {
    match name {
        "show" => Some(EmptyCells::Show),
        "hide" => Some(EmptyCells::Hide),
        _ => None,
    }
}

/// `table-layout`: where a table's column widths come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableLayout {
    /// Measured from every cell's content (§17.5.2.2). The initial value, and
    /// the one the era's pages are built on — a layout table sized by what is
    /// in it is the whole technique.
    #[default]
    Auto,
    /// Taken from the columns and the first row alone (§17.5.2.1), so the rest
    /// of the table never has to be measured. Faster, and what an author reaches
    /// for when they want the widths they wrote rather than the widths their
    /// content implies.
    Fixed,
}

/// Parses a `table-layout` keyword.
pub fn parse_table_layout(name: &str) -> Option<TableLayout> {
    match name {
        "auto" => Some(TableLayout::Auto),
        "fixed" => Some(TableLayout::Fixed),
        _ => None,
    }
}

/// `outline`: a ring drawn outside the border edge that takes up no room.
///
/// §18.4. Not a fifth border: it is drawn *outside* the border box, it is the
/// same on all four sides, and it does not influence layout at all — which is
/// the whole point of it, since an outline that moved the page could not be
/// used to mark focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Outline {
    /// Declared width, used only when the style draws.
    pub width: Length,
    /// Line style. `none` by default, so an `outline-width` alone draws
    /// nothing — the same trap as `border-width`.
    pub style: BorderStyle,
    /// Colour, or `None` for the element's own `color`.
    ///
    /// CSS 2.1's initial value is `invert`, which inverts whatever is under the
    /// outline so that it is visible against any background. That needs the
    /// pixels already drawn and the display list is built before anything is
    /// rasterised, so `invert` is taken as the element's colour — visible
    /// against the page for the same reason its text is.
    pub color: Option<Color>,
}

impl Default for Outline {
    fn default() -> Self {
        Self {
            width: Length::Px(MEDIUM_BORDER),
            style: BorderStyle::None,
            color: None,
        }
    }
}

impl Outline {
    /// How thick the ring is drawn. Outside the box, so it occupies nothing.
    pub fn used_width(&self, font_size: f32) -> f32 {
        if self.style.reserves_space() {
            self.width.to_px(font_size, 0.0).max(0.0)
        } else {
            0.0
        }
    }
}

/// Parses a `border-collapse` keyword.
pub fn parse_border_collapse(name: &str) -> Option<BorderCollapse> {
    match name {
        "separate" => Some(BorderCollapse::Separate),
        "collapse" => Some(BorderCollapse::Collapse),
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

/// The `thin` border width.
pub const THIN_BORDER: f32 = 1.0;
/// The `medium` border width, and the initial value.
pub const MEDIUM_BORDER: f32 = 3.0;
/// The `thick` border width.
pub const THICK_BORDER: f32 = 5.0;

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
    /// `border-spacing`, the gap between cell borders in the separated model:
    /// horizontal first, then vertical.
    ///
    /// On a table only. Kept as lengths rather than pixels because they are
    /// resolved against the table's own font size, like any other length.
    /// Ignored entirely when `border_collapse` is `Collapse`.
    ///
    /// Two values, because §17.6.1 allows two and a page that writes
    /// `border-spacing: 0 8px` means the rows to be spaced and the columns not
    /// to be. One value applies to both axes.
    pub border_spacing: (Length, Length),
    /// `border-collapse`, inherited, which model a table's borders use.
    pub border_collapse: BorderCollapse,
    /// `empty-cells`, inherited, whether an empty cell draws itself.
    pub empty_cells: EmptyCells,
    /// `table-layout`, on a table, where its column widths come from.
    pub table_layout: TableLayout,
    /// `outline`, drawn outside the border box and taking up no room.
    pub outline: Outline,
    /// `caption-side`, inherited, which side of a table its caption sits on.
    pub caption_side: CaptionSide,
    /// `visibility`, inherited. A hidden box keeps its space.
    pub visibility: Visibility,
    /// `text-transform`, inherited.
    pub text_transform: TextTransform,
    /// `letter-spacing` in pixels, inherited. Zero is `normal`.
    pub letter_spacing: f32,
    /// `word-spacing` in pixels, inherited: extra room added at every space.
    pub word_spacing: f32,
    /// `text-indent`, inherited, applied to a block's first line.
    ///
    /// Kept as a length because it may be a percentage, which resolves against
    /// the containing block's width and so cannot be settled in the cascade.
    pub text_indent: Length,
    /// A lower bound on the used height, which `height` is clamped to.
    ///
    /// `Auto` means no bound, as it does for `max_width`.
    pub min_height: Length,
    /// An upper bound on the used height.
    ///
    /// `Auto` means no bound. Where both apply, `min_height` wins: §10.7
    /// applies the maximum first and the minimum second, so a box asked to be
    /// at most 10px and at least 20px is 20px.
    pub max_height: Length,
    /// Generated content, already resolved to the text it stands for.
    ///
    /// Only ever set on a `::before` or `::after` style. Resolved in the
    /// cascade rather than carried as a value list because every form in
    /// scope here — a string, `attr()` — is known there, and the originating
    /// element is in hand for `attr()`, which it is not by layout time.
    ///
    /// `None` is `content: none` and `content: normal`, both of which mean the
    /// pseudo-element generates no box at all.
    pub content: Option<String>,
    /// `clip`, and `None` for `auto` — no clipping at all.
    ///
    /// Only consulted on an absolutely positioned box, which is the only
    /// place CSS 2.1 §11.1.2 gives it any meaning.
    pub clip: Option<ClipRect>,
    /// `z-index`, and `None` for `auto`.
    ///
    /// Only consulted on a positioned box, which is the only place §9.9 gives
    /// it any meaning.
    pub z_index: Option<i32>,
    /// `font-family`, inherited.
    pub font_family: FontStack,
    /// `font-size` in pixels, inherited.
    pub font_size: f32,
    /// `font-weight` as a numeric weight, inherited.
    pub font_weight: u16,
    /// `font-style`, inherited.
    pub font_style: FontStyle,
    /// `font-variant`, which here means small capitals or not.
    pub font_variant: FontVariant,
    /// `direction` (§8.6), which decides the base level of a paragraph and
    /// which end of the line its text starts from.
    pub direction: Direction,
    /// `unicode-bidi` (§8.6). Does *not* inherit, unlike `direction`: an
    /// override applies to the element that declares it and to text directly
    /// inside it, and a nested element opens its own.
    pub unicode_bidi: UnicodeBidi,
    /// `line-height`, inherited. `normal` until a font resolves it.
    pub line_height: LineHeight,
    /// `text-align`, inherited.
    pub text_align: TextAlign,
    /// `white-space`, inherited.
    pub white_space: WhiteSpace,
    /// `text-decoration`, propagated to inline descendants (CSS 2.1 §16.3).
    pub text_decoration: TextDecoration,
    /// `list-style-type`, inherited so a list's items pick it up from the list.
    pub list_style_type: ListStyleType,
    /// Where the marker sits (§12.5.1). Inherited, like the type.
    pub list_style_position: ListStylePosition,
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
    /// An upper bound on the used width, which `width` is clamped to.
    ///
    /// `Auto` means no bound.
    pub max_width: Length,
    /// A lower bound on the used width, applied after `max_width` (§10.4).
    ///
    /// `Auto` means no bound, as it does for `max_width`. This used to be
    /// absent on the grounds that nothing needed it — a property parsed and
    /// ignored reads as supported, which is worse than one that is missing.
    /// Wikipedia's stylesheet asks for it 33 times.
    pub min_width: Length,
    /// `height`.
    pub height: Length,
    /// `counter-reset`, as `(name, value)` pairs in source order (§12.4).
    ///
    /// A list because one declaration can reset several counters, and their
    /// order matters when two of them share a name.
    pub counter_reset: Vec<(String, i32)>,
    /// `counter-increment`, as `(name, delta)` pairs in source order.
    pub counter_increment: Vec<(String, i32)>,
}

/// What a `<table>` element gets for `border-spacing` when nothing says
/// otherwise.
///
/// Two pixels, and it matters: getting it wrong by 2px per edge is plainly
/// visible on a dense table, which the era's pages are full of.
///
/// **Not the initial value.** §17.6.1 makes that zero; the two pixels are the
/// HTML user-agent sheet's rule for the `table` *element*, and applying them
/// as the initial value instead gave every `display: table` box a gap it never
/// asked for. That is invisible in era markup, where a table is always a
/// `<table>`, and plainly wrong on a table built out of `display` values —
/// which is most of the CSS 2.1 suite's tables, and a modern page's.
pub const DEFAULT_BORDER_SPACING: f32 = 2.0;

/// The initial font size, and the basis for `em` at the root.
pub const DEFAULT_FONT_SIZE: f32 = 16.0;

/// `line-height: normal` as a multiple of font size, for the one case where
/// the font's own answer cannot be had.
///
/// §10.8.1 leaves `normal` to the user agent and asks for a "reasonable" value
/// "based on the font". The real value is the face's ascent plus its descent
/// plus its line gap, which only the shaper can report — see
/// `text::FontStore::used_line_height`. This is the fallback for a run with no
/// glyphs to name a face with, and the number every browser used before anyone
/// measured: close enough that nothing jumps, wrong enough to be worth not
/// using.
pub const NORMAL_LINE_HEIGHT: f32 = 1.2;

/// A used `line-height`.
///
/// `normal` stays unresolved through the cascade, which is the whole point: it
/// depends on the face the text is set in, and the cascade has no fonts. Every
/// other form — a length, a percentage, a unitless multiplier — is a number of
/// pixels the moment the font size is known, and is resolved where it is read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    /// The font's own: ascent + descent + line gap, at this size.
    Normal,
    /// A unitless multiplier.
    ///
    /// It inherits as the *number* and resolves against each element's own font
    /// size (§10.8.1), which is the whole reason the era's sheets write
    /// `line-height: 1.4` rather than `line-height: 1.4em`: a heading inside a
    /// body set that way gets a line proportional to the heading. Resolved to
    /// pixels it would not — every heading on every page would be given the
    /// body's line and its text would overlap.
    Number(f32),
    /// A used value in pixels, from a length or a percentage.
    Px(f32),
}

impl LineHeight {
    /// The value in pixels, given the font size it resolves against and what
    /// `normal` comes to for that font.
    ///
    /// Non-finite and negative values fall back to `normal` as well: they can
    /// only come from a font size that overflowed, and a line height of `NaN`
    /// poisons every coordinate downstream of it.
    pub fn resolve(self, font_size: f32, normal: f32) -> f32 {
        let used = match self {
            Self::Normal => normal,
            Self::Number(n) => n * font_size,
            Self::Px(px) => px,
        };
        if used.is_finite() && used >= 0.0 {
            used
        } else {
            normal
        }
    }

    /// Whether resolving this needs a font measured, which the cascade cannot
    /// do and everything downstream of it can.
    pub fn is_normal(self) -> bool {
        matches!(self, Self::Normal)
    }
}

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
            vertical_align: VerticalAlign::Baseline,
            border_spacing: (Length::Px(0.0), Length::Px(0.0)),
            border_collapse: BorderCollapse::Separate,
            empty_cells: EmptyCells::Show,
            table_layout: TableLayout::Auto,
            outline: Outline::default(),
            caption_side: CaptionSide::Top,
            visibility: Visibility::Visible,
            text_transform: TextTransform::None,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            text_indent: Length::Px(0.0),
            min_height: Length::Auto,
            max_height: Length::Auto,
            content: None,
            clip: None,
            min_width: Length::Auto,
            z_index: None,
            font_family: FontStack::default(),
            font_size: DEFAULT_FONT_SIZE,
            font_weight: 400,
            font_style: FontStyle::Normal,
            font_variant: FontVariant::Normal,
            direction: Direction::Ltr,
            unicode_bidi: UnicodeBidi::Normal,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Start,
            white_space: WhiteSpace::Normal,
            text_decoration: TextDecoration::default(),
            list_style_type: ListStyleType::Disc,
            list_style_position: ListStylePosition::Outside,
            margin: Edges::ZERO,
            padding: Edges::ZERO,
            border: Borders::default(),
            position: Position::Static,
            offsets: Offsets::default(),
            float: Float::None,
            clear: Clear::None,
            width: Length::Auto,
            max_width: Length::Auto,
            height: Length::Auto,
            counter_reset: Vec::new(),
            counter_increment: Vec::new(),
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
            font_variant: parent.font_variant,
            direction: parent.direction,
            unicode_bidi: UnicodeBidi::Normal,
            line_height: parent.line_height,
            text_align: parent.text_align,
            white_space: parent.white_space,
            list_style_type: parent.list_style_type,
            list_style_position: parent.list_style_position,
            // §17.6: inherited, so a rule on `table` reaches the cells that
            // have to agree with it about where their borders are.
            border_collapse: parent.border_collapse,
            empty_cells: parent.empty_cells,
            // §17.4.1: set on the table, read on the caption.
            caption_side: parent.caption_side,
            // §11.2: hiding a container hides what is inside it, and a
            // descendant can set `visible` to come back out.
            visibility: parent.visibility,
            text_transform: parent.text_transform,
            letter_spacing: parent.letter_spacing,
            word_spacing: parent.word_spacing,
            text_indent: parent.text_indent,
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
    fn capitalize_uppercases_word_starts_and_leaves_the_rest_alone() {
        // §16.5 capitalises the first letter of each word and says nothing
        // about the others, so `HTML` stays `HTML`. Lowercasing the remainder
        // is the obvious-looking mistake and it mangles every acronym on the
        // page.
        let cap = |s: &str| TextTransform::Capitalize.apply(s);
        assert_eq!(cap("hello world"), "Hello World");
        assert_eq!(cap("the HTML spec"), "The HTML Spec");
        // Punctuation does not start a word, or `o'clock` becomes `O'Clock`.
        assert_eq!(cap("o'clock"), "O'clock");
        assert_eq!(cap("  leading space"), "  Leading Space");
    }

    #[test]
    fn the_other_transforms_are_wholesale() {
        assert_eq!(TextTransform::Uppercase.apply("MiXeD"), "MIXED");
        assert_eq!(TextTransform::Lowercase.apply("MiXeD"), "mixed");
        assert_eq!(TextTransform::None.apply("MiXeD"), "MiXeD");
    }

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
    fn decimal_leading_zero_pads_to_two_and_no_further() {
        for (ordinal, expected) in [(1, "01"), (9, "09"), (10, "10"), (99, "99"), (100, "100")] {
            assert_eq!(ListStyleType::DecimalLeadingZero.counter(ordinal), expected);
        }
    }

    #[test]
    fn lower_greek_skips_final_sigma() {
        // Twenty-four letters, not twenty-five: final sigma is a positional
        // form of the same letter and counting it would number two items
        // sigma.
        assert_eq!(ListStyleType::LowerGreek.counter(1), "\u{3b1}");
        assert_eq!(ListStyleType::LowerGreek.counter(17), "\u{3c1}");
        assert_eq!(ListStyleType::LowerGreek.counter(18), "\u{3c3}");
        assert_eq!(ListStyleType::LowerGreek.counter(24), "\u{3c9}");
        assert_eq!(
            ListStyleType::LowerGreek.counter(25),
            "\u{3b1}\u{3b1}",
            "past omega it repeats, as the alphabetic systems do"
        );
    }

    #[test]
    fn armenian_and_georgian_are_additive() {
        // 1996 is 1000 + 900 + 90 + 6: one letter per non-zero digit, largest
        // first, and no subtractive pairs of the Roman kind.
        assert_eq!(ListStyleType::Armenian.counter(1), "\u{531}");
        assert_eq!(
            ListStyleType::Armenian.counter(1996),
            "\u{54c}\u{54b}\u{542}\u{536}"
        );
        assert_eq!(ListStyleType::Georgian.counter(1), "\u{10d0}");
        assert_eq!(
            ListStyleType::Georgian.counter(1996),
            "\u{10e9}\u{10e8}\u{10df}\u{10d5}"
        );
        // Past the top of each system there is no notation at all, so the
        // number is written in digits rather than in a wall of letters.
        assert_eq!(ListStyleType::Armenian.counter(10000), "10000");
        assert_eq!(ListStyleType::Georgian.counter(20000), "20000");
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
