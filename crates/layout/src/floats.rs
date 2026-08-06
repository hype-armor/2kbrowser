//! Float placement, and the line boxes floats leave behind.
//!
//! Floats are the other half of how the era laid pages out (ADR-0004): a
//! floated image or sidebar is taken out of the normal flow, shifted to one
//! edge, and the following text flows down beside it.

use css::style::{Clear, Float};

/// A float that has been placed, in coordinates local to its block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedFloat {
    /// Which side it was floated to.
    pub side: Float,
    /// Left edge.
    pub left: f32,
    /// Right edge.
    pub right: f32,
    /// Top edge.
    pub top: f32,
    /// Bottom edge.
    pub bottom: f32,
}

/// The floats currently affecting a block formatting context.
#[derive(Debug, Clone, Default)]
pub struct FloatContext {
    floats: Vec<PlacedFloat>,
    /// Width of the containing block's content box.
    width: f32,
}

impl FloatContext {
    /// A context for a container of the given content width.
    pub fn new(width: f32) -> Self {
        Self {
            floats: Vec::new(),
            width,
        }
    }

    /// Whether any float has been placed.
    pub fn is_empty(&self) -> bool {
        self.floats.is_empty()
    }

    /// Horizontal offset and width available to a line box occupying the
    /// vertical band `[y, y + height)`.
    ///
    /// A float only affects lines that actually overlap it vertically, which is
    /// why the band matters rather than a single coordinate: a tall line beside
    /// the bottom edge of a float is still beside the float.
    pub fn line_box(&self, y: f32, height: f32) -> (f32, f32) {
        let bottom = y + height.max(1.0);
        let mut left = 0.0f32;
        let mut right = self.width;
        for float in &self.floats {
            // Strictly overlapping: a float ending exactly at y does not
            // constrain the line starting there.
            if float.bottom <= y || float.top >= bottom {
                continue;
            }
            match float.side {
                Float::Left => left = left.max(float.right),
                Float::Right => right = right.min(float.left),
                Float::None => {}
            }
        }
        (left, (right - left).max(0.0))
    }

    /// Places a float of the given size no higher than `y`, returning its
    /// top-left corner.
    ///
    /// Walks downwards until a band is found that the float fits in. That is
    /// the specified behaviour and the reason two wide floats stack instead of
    /// overlapping.
    pub fn place(&mut self, side: Float, width: f32, height: f32, y: f32) -> (f32, f32) {
        let mut top = y;
        // Bounded by the number of floats: each iteration drops below at least
        // one of them, so this cannot spin.
        for _ in 0..=self.floats.len() {
            let (offset, available) = self.line_box(top, height);
            if width <= available || self.floats.is_empty() {
                let left = match side {
                    Float::Right => offset + available - width,
                    _ => offset,
                };
                self.floats.push(PlacedFloat {
                    side,
                    left,
                    right: left + width,
                    top,
                    bottom: top + height,
                });
                return (left, top);
            }
            // Drop below the shallowest float still in the way.
            let next = self
                .floats
                .iter()
                .map(|f| f.bottom)
                .filter(|bottom| *bottom > top)
                .fold(f32::INFINITY, f32::min);
            if !next.is_finite() {
                break;
            }
            top = next;
        }

        let left = match side {
            Float::Right => (self.width - width).max(0.0),
            _ => 0.0,
        };
        self.floats.push(PlacedFloat {
            side,
            left,
            right: left + width,
            top,
            bottom: top + height,
        });
        (left, top)
    }

    /// The y below which the given `clear` value permits content.
    pub fn clearance(&self, clear: Clear, y: f32) -> f32 {
        let relevant = |side: Float| {
            self.floats
                .iter()
                .filter(|float| float.side == side)
                .map(|float| float.bottom)
                .fold(y, f32::max)
        };
        match clear {
            Clear::None => y,
            Clear::Left => relevant(Float::Left),
            Clear::Right => relevant(Float::Right),
            Clear::Both => relevant(Float::Left).max(relevant(Float::Right)),
        }
    }

    /// This context seen from a descendant block's coordinate space.
    ///
    /// Floats belong to a block formatting context, not to the single block
    /// that declared them: a float before a run of paragraphs must narrow the
    /// line boxes *inside* each of those paragraphs. Descendants therefore
    /// inherit a shifted copy rather than starting empty.
    pub fn translated(&self, dx: f32, dy: f32, width: f32) -> Self {
        Self {
            floats: self
                .floats
                .iter()
                .map(|float| PlacedFloat {
                    left: float.left - dx,
                    right: float.right - dx,
                    top: float.top - dy,
                    bottom: float.bottom - dy,
                    ..*float
                })
                .collect(),
            width,
        }
    }

    /// The lowest edge of any float, used so a container encloses its floats.
    pub fn lowest_edge(&self) -> f32 {
        self.floats
            .iter()
            .map(|float| float.bottom)
            .fold(0.0, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_context_offers_the_whole_width() {
        let context = FloatContext::new(500.0);
        assert_eq!(context.line_box(0.0, 20.0), (0.0, 500.0));
    }

    #[test]
    fn a_left_float_offsets_and_narrows_lines_beside_it() {
        let mut context = FloatContext::new(500.0);
        context.place(Float::Left, 100.0, 50.0, 0.0);
        assert_eq!(
            context.line_box(0.0, 20.0),
            (100.0, 400.0),
            "beside the float"
        );
        assert_eq!(context.line_box(60.0, 20.0), (0.0, 500.0), "below it");
    }

    #[test]
    fn a_right_float_narrows_without_offsetting() {
        let mut context = FloatContext::new(500.0);
        let (left, top) = context.place(Float::Right, 120.0, 40.0, 0.0);
        assert_eq!((left, top), (380.0, 0.0), "sits against the right edge");
        assert_eq!(context.line_box(0.0, 20.0), (0.0, 380.0));
    }

    #[test]
    fn floats_on_both_sides_narrow_from_both() {
        let mut context = FloatContext::new(500.0);
        context.place(Float::Left, 100.0, 50.0, 0.0);
        context.place(Float::Right, 100.0, 50.0, 0.0);
        assert_eq!(context.line_box(10.0, 20.0), (100.0, 300.0));
    }

    #[test]
    fn a_float_that_does_not_fit_drops_below_the_one_in_the_way() {
        let mut context = FloatContext::new(300.0);
        context.place(Float::Left, 200.0, 40.0, 0.0);
        // 200 more will not fit beside 200 in a 300px container.
        let (left, top) = context.place(Float::Left, 200.0, 40.0, 0.0);
        assert_eq!(left, 0.0);
        assert_eq!(top, 40.0, "must stack below rather than overlap");
    }

    #[test]
    fn a_second_float_sits_beside_the_first_when_there_is_room() {
        let mut context = FloatContext::new(500.0);
        context.place(Float::Left, 100.0, 40.0, 0.0);
        let (left, top) = context.place(Float::Left, 100.0, 40.0, 0.0);
        assert_eq!((left, top), (100.0, 0.0));
    }

    #[test]
    fn a_line_only_overlapping_the_float_vertically_is_constrained() {
        let mut context = FloatContext::new(400.0);
        context.place(Float::Left, 80.0, 30.0, 0.0);
        // A tall line starting just above the float's bottom still overlaps it.
        assert_eq!(context.line_box(29.0, 20.0).0, 80.0);
        // One starting exactly at the bottom does not.
        assert_eq!(context.line_box(30.0, 20.0).0, 0.0);
    }

    #[test]
    fn clear_moves_below_the_matching_side_only() {
        let mut context = FloatContext::new(500.0);
        context.place(Float::Left, 100.0, 80.0, 0.0);
        context.place(Float::Right, 100.0, 40.0, 0.0);
        assert_eq!(context.clearance(Clear::None, 0.0), 0.0);
        assert_eq!(context.clearance(Clear::Right, 0.0), 40.0);
        assert_eq!(context.clearance(Clear::Left, 0.0), 80.0);
        assert_eq!(context.clearance(Clear::Both, 0.0), 80.0);
    }

    #[test]
    fn clearance_never_moves_content_upwards() {
        let mut context = FloatContext::new(500.0);
        context.place(Float::Left, 100.0, 20.0, 0.0);
        assert_eq!(context.clearance(Clear::Both, 100.0), 100.0);
    }

    #[test]
    fn the_lowest_edge_covers_every_float() {
        let mut context = FloatContext::new(500.0);
        context.place(Float::Left, 50.0, 30.0, 0.0);
        context.place(Float::Right, 50.0, 90.0, 0.0);
        assert_eq!(context.lowest_edge(), 90.0);
    }
}
