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
use css::style::{ComputedStyle, FontStyle, GenericFamily, WhiteSpace};

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

/// A shaped, unbreakable piece of text.
#[derive(Debug, Clone, Default)]
struct Shaped {
    glyphs: Vec<PositionedGlyph>,
    width: f32,
    /// Distance from the line top to the baseline.
    ascent: f32,
    /// The segment's own line height.
    height: f32,
}

/// A segment placed on a line.
#[derive(Debug, Clone)]
struct Segment {
    shaped: Shaped,
    /// Width of collapsed whitespace following this segment.
    trailing_space: f32,
    /// Whether a line break is required after this segment.
    mandatory_break: bool,
    /// Horizontal position within the line, filled in during placement.
    x: f32,
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

    /// Shapes a single unbreakable segment at its natural width.
    ///
    /// Shaping happens per segment rather than per character, so joining
    /// scripts and ligatures within a segment stay correct; segments are cut
    /// only at Unicode break opportunities, where shaping does not carry over.
    fn shape_segment(&mut self, text: &str, style: &ComputedStyle) -> Shaped {
        if text.is_empty() {
            return Shaped::default();
        }
        let metrics = Metrics::new(style.font_size, style.line_height);
        let mut buffer = Buffer::new(&mut self.system, metrics);
        let mut buffer = buffer.borrow_with(&mut self.system);
        // No width limit: a segment is by definition not broken further.
        buffer.set_size(None, None);
        let attrs = Self::attrs_for(style);
        // Advanced shaping: required for correctness on anything beyond plain
        // Latin, and the cost is irrelevant next to being wrong.
        buffer.set_rich_text([(text, attrs.clone())], &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(false);

        let mut shaped = Shaped::default();
        if let Some(run) = buffer.layout_runs().next() {
            shaped.width = run.line_w;
            shaped.ascent = run.line_y - run.line_top;
            shaped.height = run.line_height;
            shaped.glyphs = run
                .glyphs
                .iter()
                .map(|glyph| PositionedGlyph {
                    font_id: glyph.font_id,
                    glyph_id: glyph.glyph_id,
                    x: glyph.x,
                    y: 0.0,
                    font_size: glyph.font_size,
                    color: glyph.color_opt.map(|c| (c.r(), c.g(), c.b(), c.a())),
                })
                .collect();
        }
        shaped
    }

    /// Shapes and wraps runs as one paragraph at a fixed width.
    pub fn layout_runs(
        &mut self,
        runs: &[InlineRun],
        default_style: &ComputedStyle,
        max_width: f32,
    ) -> TextLayout {
        self.layout_runs_constrained(runs, default_style, |_, _| (0.0, max_width))
    }

    /// Shapes and wraps runs where the available width varies down the page.
    ///
    /// `constraints` is asked, for a line starting at `y` and `height` tall,
    /// for the horizontal offset and width available to it. That is what makes
    /// floats possible: text beside a float gets a narrower, offset line box,
    /// and text below it gets the full width back.
    ///
    /// Line breaking happens across the whole run sequence rather than per run,
    /// which is the difference between real inline layout and concatenating
    /// separately-wrapped fragments: `<p>a <b>bold</b> word</p>` must break as
    /// if it were one sentence, because it is one.
    pub fn layout_runs_constrained<F>(
        &mut self,
        runs: &[InlineRun],
        default_style: &ComputedStyle,
        constraints: F,
    ) -> TextLayout
    where
        F: Fn(f32, f32) -> (f32, f32),
    {
        let segments = self.segment(runs);
        if segments.is_empty() {
            return TextLayout::default();
        }

        let mut layout = TextLayout::default();
        let mut y = 0.0f32;
        let mut current: Vec<Segment> = Vec::new();
        let mut x = 0.0f32;
        let mut line_height = default_style.line_height;
        let mut ascent = default_style.font_size * 0.8;

        // The available width depends on the line's height, and the height
        // depends on what lands on the line. Query with the height so far and
        // accept that a line which grows taller mid-fill keeps the width it
        // started with — the alternative is re-flowing, which can oscillate.
        let mut available = constraints(y, line_height).1;

        for segment in segments {
            let fits = current.is_empty() || x + segment.shaped.width <= available;
            if !fits {
                let (offset, _) = constraints(y, line_height);
                Self::push_line(&mut layout, &mut current, offset, y, ascent);
                y += line_height;
                x = 0.0;
                line_height = default_style.line_height;
                ascent = default_style.font_size * 0.8;
                available = constraints(y, line_height).1;
            }

            line_height = line_height.max(segment.shaped.height);
            ascent = ascent.max(segment.shaped.ascent);
            let advance = segment.shaped.width + segment.trailing_space;
            let forced = segment.mandatory_break;
            let mut placed = segment;
            placed.x = x;
            x += advance;
            current.push(placed);

            // A newline in `pre`, or any other mandatory opportunity, ends the
            // line regardless of how much room is left.
            if forced {
                let (offset, _) = constraints(y, line_height);
                Self::push_line(&mut layout, &mut current, offset, y, ascent);
                y += line_height;
                x = 0.0;
                line_height = default_style.line_height;
                ascent = default_style.font_size * 0.8;
                available = constraints(y, line_height).1;
            }
        }

        if !current.is_empty() {
            let (offset, _) = constraints(y, line_height);
            Self::push_line(&mut layout, &mut current, offset, y, ascent);
            y += line_height;
        }

        layout.height = y;
        layout.width = layout
            .lines
            .iter()
            .fold(0.0f32, |acc, line| acc.max(line.width));
        layout
    }

    /// Emits one line from the segments gathered for it.
    fn push_line(
        layout: &mut TextLayout,
        current: &mut Vec<Segment>,
        offset: f32,
        line_y: f32,
        ascent: f32,
    ) {
        let mut glyphs = Vec::new();
        let mut width = 0.0f32;
        for segment in current.iter() {
            for glyph in &segment.shaped.glyphs {
                glyphs.push(PositionedGlyph {
                    x: glyph.x + segment.x + offset,
                    // Absolute baseline within the block: the line's own top
                    // plus the shared ascent. Using the ascent alone would
                    // stack every line at the same y.
                    y: line_y + ascent,
                    ..*glyph
                });
            }
            // Trailing spaces are excluded: a line's width is its inked extent,
            // which is what centring and right alignment must measure.
            width = width.max(segment.x + segment.shaped.width);
        }
        layout.lines.push(Line {
            glyphs,
            width,
            baseline: ascent,
        });
        current.clear();
    }

    /// Cuts runs into unbreakable segments at Unicode break opportunities.
    ///
    /// Uses the Unicode line breaking algorithm rather than splitting on
    /// spaces. Scripts without spaces — CJK above all — break between
    /// characters, and a space-splitting breaker would hand them one
    /// unbreakable segment per paragraph that could never wrap.
    fn segment(&mut self, runs: &[InlineRun]) -> Vec<Segment> {
        let mut out: Vec<Segment> = Vec::new();
        for run in runs {
            if run.text.is_empty() {
                continue;
            }
            let preserve = run.style.white_space == WhiteSpace::Pre;
            let mut start = 0usize;
            for (index, opportunity) in unicode_linebreak::linebreaks(&run.text) {
                let piece = &run.text[start..index];
                start = index;
                // The algorithm reports Mandatory at end of text as well as at
                // hard line breaks. Requiring an actual break character tells
                // them apart — otherwise every run boundary would end a line,
                // and `<b>one</b> <i>two</i>` would render on two.
                let mandatory = opportunity == unicode_linebreak::BreakOpportunity::Mandatory
                    && piece.contains(['\n', '\r', '\u{0b}', '\u{0c}', '\u{85}']);

                let trimmed = piece.trim_end_matches([' ', '\t', '\n', '\r']);
                let whitespace = &piece[trimmed.len()..];
                // Newlines take no horizontal room; the break itself is the
                // effect. Tabs and spaces do.
                let spacing: String = whitespace
                    .chars()
                    .filter(|c| *c != '\n' && *c != '\r')
                    .collect();
                let space_width = if spacing.is_empty() {
                    0.0
                } else if preserve {
                    self.shape_segment(&spacing, &run.style).width
                } else {
                    // Collapsed runs already hold at most one space.
                    self.shape_segment(" ", &run.style).width
                };

                if trimmed.is_empty() {
                    // Whitespace with no text of its own still advances the pen
                    // and can still carry a mandatory break, so it must not be
                    // dropped — a blank line in a <pre> is exactly this case.
                    if let Some(last) = out.last_mut() {
                        last.trailing_space += space_width;
                        last.mandatory_break |= mandatory;
                    } else if mandatory {
                        out.push(Segment {
                            shaped: Shaped {
                                height: run.style.line_height,
                                ascent: run.style.font_size * 0.8,
                                ..Shaped::default()
                            },
                            trailing_space: space_width,
                            mandatory_break: true,
                            x: 0.0,
                        });
                    }
                    continue;
                }

                let shaped = self.shape_segment(trimmed, &run.style);
                out.push(Segment {
                    shaped,
                    trailing_space: space_width,
                    mandatory_break: mandatory,
                    x: 0.0,
                });
            }
        }
        // The algorithm reports a mandatory break at end of text; that is the
        // end of the paragraph, not a blank line after it.
        if let Some(last) = out.last_mut() {
            last.mandatory_break = false;
        }
        out
    }

    /// Minimum and maximum content widths of a set of runs.
    ///
    /// Table column sizing needs both: the maximum is the width at which the
    /// content would not wrap at all, the minimum is the widest single
    /// unbreakable piece. CSS 2.1's automatic table layout interpolates between
    /// them when the available width falls in between.
    pub fn intrinsic_widths(
        &mut self,
        runs: &[InlineRun],
        default_style: &ComputedStyle,
    ) -> (f32, f32) {
        let max = self.layout_runs(runs, default_style, f32::MAX).width;

        // The minimum is the widest word, measured in the style of the run it
        // came from — measuring everything in the default style would
        // under-report a bold or larger span and let its column collapse.
        let mut min: f32 = 0.0;
        for run in runs {
            for word in run.text.split_whitespace() {
                let single = [InlineRun {
                    text: word.to_owned(),
                    style: run.style.clone(),
                }];
                min = min.max(self.layout_runs(&single, default_style, f32::MAX).width);
            }
        }
        (min, max.max(min))
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
    fn a_narrowed_band_wraps_sooner_and_is_offset() {
        // The float case: the first band is narrow and pushed right, the rest
        // of the page is full width. Text must respect both.
        let mut store = FontStore::new();
        let plain = style(16.0);
        let runs = [InlineRun {
            text: "the quick brown fox jumps over the lazy dog and keeps on running".to_owned(),
            style: plain.clone(),
        }];
        // The band is one line tall at 16px/1.2, so only the first line is
        // narrowed and everything after it gets the full width back.
        let layout = store.layout_runs_constrained(&runs, &plain, |y, _| {
            if y < 19.0 {
                (120.0, 180.0)
            } else {
                (0.0, 500.0)
            }
        });

        let unconstrained = store.layout_runs(&runs, &plain, 500.0);
        assert!(
            layout.lines.len() > unconstrained.lines.len(),
            "the narrow band must force a wrap the full width does not: {} vs {}",
            layout.lines.len(),
            unconstrained.lines.len()
        );
        let first_line_x = layout.lines[0]
            .glyphs
            .first()
            .map(|g| g.x)
            .expect("glyphs on the first line");
        assert!(
            first_line_x >= 120.0,
            "first line must be offset past the float, got {first_line_x}"
        );
        assert!(
            layout.lines[0].width <= 180.0 + 120.0,
            "first line must fit the narrow band"
        );

        let last = layout.lines.last().expect("a last line");
        let last_x = last.glyphs.first().map(|g| g.x).expect("glyphs");
        assert!(
            last_x < 120.0,
            "lines below the float start at the left edge, got {last_x}"
        );
    }

    #[test]
    fn a_single_unbreakable_word_overflows_rather_than_being_split() {
        // CSS breaks lines at opportunities; a long word with none simply
        // overflows. Splitting it mid-word would be worse than overflowing.
        let mut store = FontStore::new();
        let plain = style(16.0);
        let runs = [InlineRun {
            text: "supercalifragilistic".to_owned(),
            style: plain.clone(),
        }];
        let layout = store.layout_runs(&runs, &plain, 20.0);
        assert_eq!(
            layout.lines.len(),
            1,
            "a word with no break opportunity stays whole"
        );
        assert!(layout.width > 20.0, "and overflows its container");
    }

    #[test]
    fn glyphs_on_a_line_share_one_baseline() {
        // Mixed sizes must sit on a common baseline, not each at its own top.
        let mut store = FontStore::new();
        let small = style(12.0);
        let large = ComputedStyle {
            font_size: 28.0,
            line_height: 34.0,
            ..style(28.0)
        };
        let runs = [
            InlineRun {
                text: "small".to_owned(),
                style: small.clone(),
            },
            InlineRun {
                text: "BIG".to_owned(),
                style: large,
            },
        ];
        let layout = store.layout_runs(&runs, &small, 1000.0);
        let ys: Vec<f32> = layout.lines[0].glyphs.iter().map(|g| g.y).collect();
        assert!(
            ys.windows(2).all(|w| (w[0] - w[1]).abs() < 0.01),
            "baselines differ: {ys:?}"
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
