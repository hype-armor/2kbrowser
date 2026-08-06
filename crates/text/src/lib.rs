//! Text shaping and line breaking, over bundled fonts only.
//!
//! Shaping goes through `cosmic-text` (`rustybuzz` + `swash` + Unicode line
//! breaking) rather than being written here — it is the hardest part of a
//! renderer and getting it wrong makes entire writing systems unreadable
//! (ADR-0007).
//!
//! The fonts are ours and the rasteriser is ours, which is what makes rendering
//! identical on Linux, macOS, and Windows (ADR-0005). The system font source is
//! never consulted; a [`FontStore`] is built from embedded font data alone. That
//! is the difference between one set of reference baselines and three.

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Stretch, Style, SwashCache, Weight,
};
use css::style::{ComputedStyle, FontStyle, GenericFamily};

/// Liberation Sans — metric-compatible with Arial and Helvetica (ADR-0008).
const SANS: &[(&str, &[u8])] = &[
    (
        "regular",
        include_bytes!("../../../fonts/liberation/LiberationSans-Regular.ttf"),
    ),
    (
        "bold",
        include_bytes!("../../../fonts/liberation/LiberationSans-Bold.ttf"),
    ),
    (
        "italic",
        include_bytes!("../../../fonts/liberation/LiberationSans-Italic.ttf"),
    ),
    (
        "bolditalic",
        include_bytes!("../../../fonts/liberation/LiberationSans-BoldItalic.ttf"),
    ),
];

/// Liberation Serif — metric-compatible with Times New Roman.
const SERIF: &[(&str, &[u8])] = &[
    (
        "regular",
        include_bytes!("../../../fonts/liberation/LiberationSerif-Regular.ttf"),
    ),
    (
        "bold",
        include_bytes!("../../../fonts/liberation/LiberationSerif-Bold.ttf"),
    ),
    (
        "italic",
        include_bytes!("../../../fonts/liberation/LiberationSerif-Italic.ttf"),
    ),
    (
        "bolditalic",
        include_bytes!("../../../fonts/liberation/LiberationSerif-BoldItalic.ttf"),
    ),
];

/// Liberation Mono — metric-compatible with Courier New.
const MONO: &[(&str, &[u8])] = &[
    (
        "regular",
        include_bytes!("../../../fonts/liberation/LiberationMono-Regular.ttf"),
    ),
    (
        "bold",
        include_bytes!("../../../fonts/liberation/LiberationMono-Bold.ttf"),
    ),
    (
        "italic",
        include_bytes!("../../../fonts/liberation/LiberationMono-Italic.ttf"),
    ),
    (
        "bolditalic",
        include_bytes!("../../../fonts/liberation/LiberationMono-BoldItalic.ttf"),
    ),
];

/// One shaped, positioned glyph.
#[derive(Debug, Clone, Copy)]
pub struct PositionedGlyph {
    /// Index of the font in the store's font list.
    pub font_id: cosmic_text::fontdb::ID,
    /// Glyph index within that font.
    pub glyph_id: u16,
    /// Horizontal position relative to the text origin.
    pub x: f32,
    /// Baseline position relative to the text origin.
    pub y: f32,
    /// Font size in pixels.
    pub font_size: f32,
}

/// One laid-out line.
#[derive(Debug, Clone)]
pub struct Line {
    /// Glyphs on this line.
    pub glyphs: Vec<PositionedGlyph>,
    /// Width of the line's inked content.
    pub width: f32,
    /// Distance from the line box top to the baseline.
    pub baseline: f32,
}

/// A laid-out run of text.
#[derive(Debug, Clone, Default)]
pub struct TextLayout {
    /// Lines in visual order.
    pub lines: Vec<Line>,
    /// Total height of all lines.
    pub height: f32,
    /// Width of the widest line.
    pub width: f32,
}

/// Owns the font database and the glyph raster cache.
pub struct FontStore {
    system: FontSystem,
    cache: SwashCache,
}

impl Default for FontStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FontStore {
    /// Builds a store from the bundled fonts, and *only* those.
    ///
    /// The database is constructed here and handed over already populated.
    /// Neither `FontSystem::new` nor `new_with_fonts` is usable: both call
    /// `load_system_fonts`, so the face set would depend on the host — this
    /// container yields 63 faces rather than 12 — and identical input would
    /// render differently on each platform, which is exactly what ADR-0005
    /// exists to prevent.
    pub fn new() -> Self {
        let mut db = cosmic_text::fontdb::Database::new();
        for (_, data) in SANS.iter().chain(SERIF).chain(MONO) {
            db.load_font_source(cosmic_text::fontdb::Source::Binary(std::sync::Arc::new(
                *data,
            )));
        }
        db.set_sans_serif_family("Liberation Sans");
        db.set_serif_family("Liberation Serif");
        db.set_monospace_family("Liberation Mono");
        // Cursive and fantasy have no bundled face; point them at sans-serif so
        // they resolve rather than falling through to nothing (issue #6).
        db.set_cursive_family("Liberation Sans");
        db.set_fantasy_family("Liberation Sans");

        let system = FontSystem::new_with_locale_and_db("en-US".to_owned(), db);
        Self {
            system,
            cache: SwashCache::new(),
        }
    }

    /// Number of loaded faces. Twelve for the M1 bundle.
    pub fn face_count(&self) -> usize {
        self.system.db().len()
    }

    /// The family name to shape with for a given style.
    ///
    /// `cursive` and `fantasy` fall through to sans-serif; CSS 2.1 requires the
    /// generics to resolve, not to be visually distinct (ADR-0008, issue #6).
    fn family_for(style: &ComputedStyle) -> Family<'static> {
        // An authored family name wins if we happen to bundle it.
        for name in &style.font_family.families {
            match name.to_ascii_lowercase().as_str() {
                "arial" | "helvetica" | "verdana" | "tahoma" | "liberation sans" => {
                    return Family::Name("Liberation Sans");
                }
                "times" | "times new roman" | "georgia" | "liberation serif" => {
                    return Family::Name("Liberation Serif");
                }
                "courier" | "courier new" | "monaco" | "consolas" | "liberation mono" => {
                    return Family::Name("Liberation Mono");
                }
                _ => {}
            }
        }
        match style.font_family.generic {
            GenericFamily::Monospace => Family::Name("Liberation Mono"),
            GenericFamily::SansSerif | GenericFamily::Cursive | GenericFamily::Fantasy => {
                Family::Name("Liberation Sans")
            }
            GenericFamily::Serif => Family::Name("Liberation Serif"),
        }
    }

    /// Shapes and wraps `text` to `max_width`, in the given style.
    pub fn layout(&mut self, text: &str, style: &ComputedStyle, max_width: f32) -> TextLayout {
        if text.is_empty() {
            return TextLayout::default();
        }

        let metrics = Metrics::new(style.font_size, style.line_height);
        let mut buffer = Buffer::new(&mut self.system, metrics);
        let mut buffer = buffer.borrow_with(&mut self.system);
        buffer.set_size(Some(max_width), None);

        let attrs = Attrs::new()
            .family(Self::family_for(style))
            .weight(Weight(style.font_weight))
            .stretch(Stretch::Normal)
            .style(match style.font_style {
                FontStyle::Italic => Style::Italic,
                FontStyle::Normal => Style::Normal,
            });

        // Advanced shaping: required for correctness on anything beyond plain
        // Latin, and the cost is irrelevant next to being wrong.
        buffer.set_text(text, &attrs, Shaping::Advanced);
        buffer.shape_until_scroll(false);

        let mut layout = TextLayout::default();
        for run in buffer.layout_runs() {
            let glyphs = run
                .glyphs
                .iter()
                .map(|glyph| PositionedGlyph {
                    font_id: glyph.font_id,
                    glyph_id: glyph.glyph_id,
                    x: glyph.x,
                    y: run.line_y,
                    font_size: glyph.font_size,
                })
                .collect();
            layout.width = layout.width.max(run.line_w);
            layout.lines.push(Line {
                glyphs,
                width: run.line_w,
                baseline: run.line_y - run.line_top,
            });
        }
        layout.height = layout.lines.len() as f32 * style.line_height;
        layout
    }

    /// Measures text without keeping the glyphs.
    pub fn measure(&mut self, text: &str, style: &ComputedStyle, max_width: f32) -> (f32, f32) {
        let layout = self.layout(text, style, max_width);
        (layout.width, layout.height)
    }

    /// Rasterises a glyph, returning its coverage bitmap and placement.
    ///
    /// The bitmap is 8-bit alpha; colour comes from the paint stage.
    pub fn rasterise(
        &mut self,
        glyph: &PositionedGlyph,
    ) -> Option<(Vec<u8>, i32, i32, usize, usize)> {
        let key = cosmic_text::CacheKey::new(
            glyph.font_id,
            glyph.glyph_id,
            glyph.font_size,
            (0.0, 0.0),
            cosmic_text::CacheKeyFlags::empty(),
        )
        .0;
        let image = self.cache.get_image(&mut self.system, key).as_ref()?;
        let width = image.placement.width as usize;
        let height = image.placement.height as usize;
        if width == 0 || height == 0 {
            return None;
        }
        Some((
            image.data.clone(),
            image.placement.left,
            image.placement.top,
            width,
            height,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use css::style::{FontStack, GenericFamily};

    fn style(size: f32) -> ComputedStyle {
        ComputedStyle {
            font_size: size,
            line_height: size * 1.2,
            ..Default::default()
        }
    }

    #[test]
    fn loads_exactly_the_bundled_faces() {
        // If this picks up system fonts, ADR-0005's determinism is gone and
        // reference baselines stop being portable.
        assert_eq!(FontStore::new().face_count(), 12);
    }

    #[test]
    fn lays_out_a_single_line() {
        let mut store = FontStore::new();
        let layout = store.layout("Hello", &style(16.0), 1000.0);
        assert_eq!(layout.lines.len(), 1);
        assert_eq!(layout.lines[0].glyphs.len(), 5);
        assert!(layout.width > 0.0);
    }

    #[test]
    fn wraps_at_the_available_width() {
        let mut store = FontStore::new();
        let text = "the quick brown fox jumps over the lazy dog";
        let wide = store.layout(text, &style(16.0), 1000.0);
        let narrow = store.layout(text, &style(16.0), 100.0);
        assert_eq!(wide.lines.len(), 1);
        assert!(narrow.lines.len() > 1, "narrow width must wrap");
        assert!(narrow.width <= 100.0);
    }

    #[test]
    fn larger_text_measures_wider() {
        let mut store = FontStore::new();
        let small = store.measure("Hello", &style(12.0), 1000.0).0;
        let large = store.measure("Hello", &style(24.0), 1000.0).0;
        assert!(large > small * 1.5, "{large} should be about twice {small}");
    }

    #[test]
    fn monospace_advances_are_uniform() {
        // A real check that family selection reaches the shaper: in Liberation
        // Mono every advance is equal, which is not true of the sans face.
        let mut store = FontStore::new();
        let mono = ComputedStyle {
            font_family: FontStack {
                families: vec![],
                generic: GenericFamily::Monospace,
            },
            ..style(16.0)
        };
        let layout = store.layout("iiiwww", &mono, 1000.0);
        let xs: Vec<f32> = layout.lines[0].glyphs.iter().map(|g| g.x).collect();
        let first_gap = xs[1] - xs[0];
        for pair in xs.windows(2) {
            assert!(
                (pair[1] - pair[0] - first_gap).abs() < 0.01,
                "advances differ: {xs:?}"
            );
        }
    }

    #[test]
    fn rasterises_a_glyph_to_a_bitmap() {
        let mut store = FontStore::new();
        let layout = store.layout("H", &style(32.0), 1000.0);
        let glyph = layout.lines[0].glyphs[0];
        let (data, _, _, width, height) = store.rasterise(&glyph).expect("bitmap");
        assert_eq!(data.len(), width * height);
        assert!(data.iter().any(|&a| a > 0), "glyph must have ink");
    }

    #[test]
    fn empty_text_lays_out_to_nothing() {
        let mut store = FontStore::new();
        let layout = store.layout("", &style(16.0), 100.0);
        assert!(layout.lines.is_empty());
        assert_eq!(layout.height, 0.0);
    }
}
