//! Computed values.

use crate::value::{Color, Length};

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
    pub fn is_supported_layout(self) -> bool {
        !matches!(self, Display::Flex | Display::Grid)
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
    /// Align to the end edge.
    Right,
    /// Stretch to both edges.
    Justify,
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
            font_family: FontStack::default(),
            font_size: DEFAULT_FONT_SIZE,
            font_weight: 400,
            font_style: FontStyle::Normal,
            line_height: DEFAULT_FONT_SIZE * NORMAL_LINE_HEIGHT,
            text_align: TextAlign::Left,
            white_space: WhiteSpace::Normal,
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
