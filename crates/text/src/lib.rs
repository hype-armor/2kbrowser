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
    /// Colour from the inline span this glyph came from.
    ///
    /// `None` means inherit the block's colour. Carried per glyph because a
    /// single line can contain spans of different colours, and the paint stage
    /// has no way to recover which span a glyph belonged to.
    pub color: Option<(u8, u8, u8, u8)>,
}

/// A run of text with its own style, within a block's inline content.
#[derive(Debug, Clone)]
pub struct InlineRun {
    /// The run's text.
    pub text: String,
    /// The style computed for the element the text came from.
    pub style: ComputedStyle,
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

    /// Attributes for one inline span.
    fn attrs_for(style: &ComputedStyle) -> Attrs<'static> {
        Attrs::new()
            .family(Self::family_for(style))
            .weight(Weight(style.font_weight))
            .stretch(Stretch::Normal)
            .style(match style.font_style {
                FontStyle::Italic => Style::Italic,
                FontStyle::Normal => Style::Normal,
            })
            .metrics(Metrics::new(style.font_size, style.line_height))
            .color(cosmic_text::Color::rgba(
                style.color.r,
                style.color.g,
                style.color.b,
                style.color.a,
            ))
    }

    /// Shapes and wraps `text` to `max_width`, in a single style.
    pub fn layout(&mut self, text: &str, style: &ComputedStyle, max_width: f32) -> TextLayout {
        let runs = [InlineRun {
            text: text.to_owned(),
            style: style.clone(),
        }];
        self.layout_runs(&runs, style, max_width)
    }

    /// Shapes and wraps a sequence of differently-styled runs as one paragraph.
    ///
    /// Line breaking happens across the whole sequence rather than per run,
    /// which is the difference between real inline layout and concatenating
    /// separately-wrapped fragments: `<p>a <b>bold</b> word</p>` must break as
    /// if it were one sentence, because it is one.
    pub fn layout_runs(
        &mut self,
        runs: &[InlineRun],
        default_style: &ComputedStyle,
        max_width: f32,
    ) -> TextLayout {
        if runs.iter().all(|run| run.text.is_empty()) {
            return TextLayout::default();
        }

        let metrics = Metrics::new(default_style.font_size, default_style.line_height);
        let mut buffer = Buffer::new(&mut self.system, metrics);
        let mut buffer = buffer.borrow_with(&mut self.system);
        buffer.set_size(Some(max_width), None);

        let default_attrs = Self::attrs_for(default_style);
        let spans: Vec<(&str, Attrs<'_>)> = runs
            .iter()
            .map(|run| (run.text.as_str(), Self::attrs_for(&run.style)))
            .collect();

        // Advanced shaping: required for correctness on anything beyond plain
        // Latin, and the cost is irrelevant next to being wrong.
        buffer.set_rich_text(spans, &default_attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(false);

        let mut layout = TextLayout::default();
        let mut height = 0.0f32;
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
                    color: glyph.color_opt.map(|c| (c.r(), c.g(), c.b(), c.a())),
                })
                .collect();
            layout.width = layout.width.max(run.line_w);
            layout.lines.push(Line {
                glyphs,
                width: run.line_w,
                baseline: run.line_y - run.line_top,
            });
            // Line heights vary when spans do, so accumulate rather than
            // multiplying a count by one line height.
            height = height.max(run.line_top + run.line_height);
        }
        layout.height = height;
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
    fn a_bold_span_shapes_differently_from_its_surroundings() {
        // The M1 gap this closes: <b> inside a paragraph must actually be bold.
        let mut store = FontStore::new();
        let plain = style(16.0);
        let bold = ComputedStyle {
            font_weight: 700,
            ..style(16.0)
        };

        let runs = [
            InlineRun {
                text: "regular ".to_owned(),
                style: plain.clone(),
            },
            InlineRun {
                text: "heavy".to_owned(),
                style: bold,
            },
        ];
        let mixed = store.layout_runs(&runs, &plain, 1000.0);
        let uniform = store.layout("regular heavy", &plain, 1000.0);

        // Bold advances are wider, so the mixed line must be wider overall.
        assert!(
            mixed.width > uniform.width,
            "bold run did not change shaping: {} vs {}",
            mixed.width,
            uniform.width
        );
    }

    #[test]
    fn runs_carry_their_own_colour() {
        let mut store = FontStore::new();
        let black = style(16.0);
        let red = ComputedStyle {
            color: css::value::Color::rgb(255, 0, 0),
            ..style(16.0)
        };
        let runs = [
            InlineRun {
                text: "a".to_owned(),
                style: black.clone(),
            },
            InlineRun {
                text: "b".to_owned(),
                style: red,
            },
        ];
        let layout = store.layout_runs(&runs, &black, 1000.0);
        let colors: Vec<_> = layout.lines[0].glyphs.iter().map(|g| g.color).collect();
        assert_eq!(colors[0], Some((0, 0, 0, 255)));
        assert_eq!(colors[1], Some((255, 0, 0, 255)));
    }

    #[test]
    fn line_breaking_spans_run_boundaries() {
        // Runs must wrap as one paragraph, not as separately-wrapped fragments.
        let mut store = FontStore::new();
        let plain = style(16.0);
        let runs = [
            InlineRun {
                text: "the quick brown ".to_owned(),
                style: plain.clone(),
            },
            InlineRun {
                text: "fox jumps over the lazy dog".to_owned(),
                style: plain.clone(),
            },
        ];
        let split = store.layout_runs(&runs, &plain, 160.0);
        let whole = store.layout("the quick brown fox jumps over the lazy dog", &plain, 160.0);
        assert_eq!(split.lines.len(), whole.lines.len());
        assert!((split.height - whole.height).abs() < 0.01);
    }

    #[test]
    fn a_larger_span_makes_its_line_taller() {
        let mut store = FontStore::new();
        let small = style(12.0);
        let large = ComputedStyle {
            font_size: 30.0,
            line_height: 36.0,
            ..style(30.0)
        };
        let uniform = store.layout("small text", &small, 1000.0);
        let runs = [
            InlineRun {
                text: "small ".to_owned(),
                style: small.clone(),
            },
            InlineRun {
                text: "BIG".to_owned(),
                style: large,
            },
        ];
        let mixed = store.layout_runs(&runs, &small, 1000.0);
        assert!(
            mixed.height > uniform.height,
            "a larger span must raise line height: {} vs {}",
            mixed.height,
            uniform.height
        );
    }

    #[test]
    fn empty_text_lays_out_to_nothing() {
        let mut store = FontStore::new();
        let layout = store.layout("", &style(16.0), 100.0);
        assert!(layout.lines.is_empty());
        assert_eq!(layout.height, 0.0);
    }
}
