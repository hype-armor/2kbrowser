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
    /// Table display types, collapsed into one variant until M2 implements them.
    Table,
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

    fn parse(name: &str) -> Option<Self> {
        let display = match name {
            "block" => Display::Block,
            "inline" => Display::Inline,
            "inline-block" => Display::InlineBlock,
            "list-item" => Display::ListItem,
            "none" => Display::None,
            "flex" | "inline-flex" => Display::Flex,
            "grid" | "inline-grid" => Display::Grid,
            name if name.starts_with("table") || name == "inline-table" => Display::Table,
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
