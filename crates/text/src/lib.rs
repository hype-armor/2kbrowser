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

pub mod first_letter;

use cosmic_text::{
    Attrs, AttrsOwned, Buffer, Family, FontSystem, Metrics, Shaping, Stretch, Style, SwashCache,
    Weight,
};
use css::style::{
    ComputedStyle, Direction, FontStyle, FontVariant, GenericFamily, TextDecoration, UnicodeBidi,
    VerticalAlign, Visibility, WhiteSpace,
};
use unicode_bidi::{BidiClass, BidiInfo, Level};

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

/// Largest font size we will ask the outline rasteriser for, in pixels.
///
/// Well past any size a document uses on purpose — a 2048-pixel letter already
/// fills a screen — and far below where the glyph bitmap stops being a sensible
/// allocation. Browsers all clamp somewhere similar; the number is a judgement
/// call, the existence of one is not.
const MAX_GLYPH_SIZE: f32 = 2048.0;

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
    /// Whether `visibility: hidden` applies to the span this glyph came from.
    ///
    /// Carried per glyph for exactly the reason the colour above is: the spans
    /// on a line are merged into one layout, so by paint time there is no way
    /// back to which span a glyph belonged to. Hidden glyphs are still shaped,
    /// placed, and measured — they hold their advance open — and simply are not
    /// drawn, which is what §11.2 asks for and the whole difference between
    /// this and `display: none`.
    pub hidden: bool,
    /// Byte range this glyph covers in its line's text.
    ///
    /// What lets a match in the text become a rectangle on the screen. Shaping
    /// is not one glyph per character — ligatures and complex scripts both
    /// break that — so the mapping has to come from the shaper rather than
    /// being counted afterwards.
    pub start: usize,
    /// End of that range, exclusive.
    pub end: usize,
}

/// The colour a span's glyphs and rules take, as stored on a glyph.
fn span_color(style: &ComputedStyle) -> Option<(u8, u8, u8, u8)> {
    let color = style.color;
    Some((color.r, color.g, color.b, color.a))
}

/// The x-height every face bundled here is treated as having, as a fraction of
/// the font size.
///
/// Only `vertical-align: middle` needs it, and only to decide where "the middle
/// of the parent's text" is. Measuring the real x-height per face would be more
/// correct and would move nothing perceptible: the four bundled faces sit
/// between 0.52 and 0.53, and the value is halved before it is used.
const X_HEIGHT: f32 = 0.5;

/// How much smaller a synthesised small capital is than a full one.
///
/// The bundled Liberation faces carry no small-caps variant, so `small-caps`
/// has to be drawn rather than asked for: lowercase letters are set as capitals
/// at this fraction of the size.
///
/// Measured rather than chosen: a 100px `x` in `small-caps` beside a 100px `X`
/// in Chromium gives cap heights of 46 and 65, which is this number to within
/// a rounded pixel. Reading it off a rendering was quicker than arguing about
/// what a face with real small capitals would have done, and it puts this
/// engine where the rest of the web already is.
const SMALL_CAPS: f32 = 0.7;

/// Maximal stretches of lowercase and of everything else, with their byte
/// offsets into `text`.
///
/// Split at the case boundary rather than per character so a word keeps its
/// shaping: `Word` is two pieces and not five, and the four letters after the
/// capital are kerned against each other as they would be anywhere else.
fn case_runs(text: &str) -> Vec<(usize, &str)> {
    let mut out: Vec<(usize, &str)> = Vec::new();
    let mut start = 0;
    let mut lower: Option<bool> = None;
    for (at, ch) in text.char_indices() {
        let is_lower = ch.is_lowercase();
        if lower.is_some_and(|was| was != is_lower) {
            out.push((start, &text[start..at]));
            start = at;
        }
        lower = Some(is_lower);
    }
    if start < text.len() {
        out.push((start, &text[start..]));
    }
    out
}

/// Whether a line box gets §10.8.1's strut.
///
/// It always does in standards mode. The quirk is the one the era's layouts
/// were built on: in quirks mode a line box holding no text at all — an image
/// alone in a table cell, a spacer GIF, a sliced-image row — has no strut, so
/// the cell is exactly as tall as the image and the famous few pixels of
/// descender space below it do not appear. Pages of the period were authored
/// against that, and a sliced image with a gap under every tile is not a near
/// miss: it is the page visibly coming apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strut {
    /// Standards mode: every line box has one.
    Always,
    /// Quirks mode: only a line with text on it.
    WhereThereIsText,
}

/// A paragraph's bidi embedding levels, one per byte of the text analysed.
///
/// Owned rather than a borrowed `BidiInfo`, because the text it was computed
/// over is built here and thrown away: the levels are the only part anything
/// downstream needs, and keeping them by value spares every caller a lifetime.
struct Bidi {
    levels: Vec<Level>,
    base: Level,
}

impl Bidi {
    /// The level at a byte offset, or the paragraph's own past the end.
    fn at(&self, offset: usize) -> Level {
        self.levels.get(offset).copied().unwrap_or(self.base)
    }
}

/// Runs the bidi algorithm over a whole inline formatting context, and says
/// where each run's text landed in the string it was run over.
///
/// That string is not the text that will be drawn. §8.6 defines `unicode-bidi`
/// by saying which control characters an element is equivalent to, so `embed`
/// and `bidi-override` are implemented by writing those characters: they steer
/// the algorithm and are never shaped. An atomic inline box becomes U+FFFC,
/// the object replacement character UAX #9 asks for, so an image between two
/// Hebrew words is ordered along with them instead of cutting the run in two.
///
/// One analysis for the whole context rather than one per run, because
/// reordering is a property of the paragraph: a run measured on its own has no
/// way to know that the word before it was Hebrew.
fn analyse_bidi(runs: &[InlineRun], base: Direction) -> (Bidi, Vec<usize>) {
    let base = match base {
        Direction::Ltr => Level::ltr(),
        Direction::Rtl => Level::rtl(),
    };
    let mut text = String::new();
    let mut starts = Vec::with_capacity(runs.len());
    for run in runs {
        let (open, close) = match (run.style.unicode_bidi, run.style.direction) {
            (UnicodeBidi::Normal, _) => ("", ""),
            (UnicodeBidi::Embed, Direction::Ltr) => ("\u{202a}", "\u{202c}"),
            (UnicodeBidi::Embed, Direction::Rtl) => ("\u{202b}", "\u{202c}"),
            (UnicodeBidi::BidiOverride, Direction::Ltr) => ("\u{202d}", "\u{202c}"),
            (UnicodeBidi::BidiOverride, Direction::Rtl) => ("\u{202e}", "\u{202c}"),
        };
        text.push_str(open);
        starts.push(text.len());
        if run.replaced.is_some() {
            text.push('\u{fffc}');
        } else {
            text.push_str(&run.text);
        }
        text.push_str(close);
    }
    let info = BidiInfo::new(&text, Some(base));
    (
        Bidi {
            levels: info.levels,
            base,
        },
        starts,
    )
}

/// Splits a piece of text into maximal stretches of one embedding level, with
/// the level each carries.
///
/// `at` is where the text begins in the string [`analyse_bidi`] ran over.
/// Always at least one stretch, so a caller can treat the last one as the end
/// of the word without checking.
fn level_runs<'a>(bidi: &Bidi, at: usize, text: &'a str) -> Vec<(Level, &'a str)> {
    let mut out: Vec<(Level, &str)> = Vec::new();
    let mut start = 0;
    let mut level = bidi.at(at);
    for (offset, _) in text.char_indices() {
        let here = bidi.at(at + offset);
        if here != level && offset > start {
            out.push((level, &text[start..offset]));
            start = offset;
            level = here;
        }
    }
    out.push((level, &text[start..]));
    out
}

/// The text without the bidi formatting characters.
///
/// They steer the algorithm and are never drawn: UAX #9 removes them, and a
/// shaper handed one either draws a box for it or — worse here — acts on it.
/// Kept out of the shaped text as well as out of the glyphs, so a search of
/// the page matches what a reader sees.
fn without_bidi_controls(text: &str) -> String {
    text.chars().filter(|&c| !is_bidi_control(c)).collect()
}

/// Whether a character is one of UAX #9's explicit formatting codes.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Whether a character is strongly right-to-left in its own right, rather than
/// having been put that way by an override.
fn is_strongly_rtl(c: char) -> bool {
    matches!(
        unicode_bidi::bidi_class(c),
        BidiClass::R | BidiClass::AL | BidiClass::AN
    )
}

/// The text with UAX #9's mirrored characters swapped for their pairs.
///
/// Only the pairs that turn up in running text: brackets, braces, angle
/// brackets and the two sets of quotation guillemets. The full
/// `Bidi_Mirroring_Glyph` property is a few hundred entries of mathematics
/// that no page of this era contains, and the table below is data rather than
/// an algorithm — which is the line this project draws around what it writes
/// itself.
///
/// Every mapping is its own inverse, so this is safe to apply once and no more.
fn mirrored(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '(' => ')',
            ')' => '(',
            '[' => ']',
            ']' => '[',
            '{' => '}',
            '}' => '{',
            '<' => '>',
            '>' => '<',
            '\u{00ab}' => '\u{00bb}',
            '\u{00bb}' => '\u{00ab}',
            '\u{2039}' => '\u{203a}',
            '\u{203a}' => '\u{2039}',
            other => other,
        })
        .collect()
}

/// Where a line sits and how much of the width it had.
///
/// Two numbers that always travel together and that a float pulls apart from
/// the block's own content box: the line starts at `offset` and is `available`
/// wide, which is what alignment measures against.
#[derive(Debug, Clone, Copy)]
struct Room {
    offset: f32,
    available: f32,
}

/// Turns a forwards-shaped run back to front, in place.
///
/// Each glyph keeps its own advance and takes the place its mirror image had:
/// the last glyph starts at zero and the first ends at the run's width. The
/// advances are read back out of the positions, which is what the shaper leaves
/// behind — the final one is whatever is left between the last glyph and the
/// end of the run.
fn mirror(shaped: &mut Shaped) {
    let width = shaped.width;
    let mut advances: Vec<f32> = Vec::with_capacity(shaped.glyphs.len());
    for at in 0..shaped.glyphs.len() {
        let next = shaped.glyphs.get(at + 1).map_or(width, |glyph| glyph.x);
        advances.push((next - shaped.glyphs[at].x).max(0.0));
    }
    for (glyph, advance) in shaped.glyphs.iter_mut().zip(&advances) {
        glyph.x = width - glyph.x - advance;
    }
    // Left to right again, so anything downstream that walks the glyphs in
    // order walks them across the page.
    shaped.glyphs.reverse();
}

/// The order to draw a line's segments in, left to right (UAX #9's rule L2).
///
/// Indices into `levels`. An all-even line comes back in its own order, which
/// is why this can run on every line rather than behind a test for
/// right-to-left content — one code path that every page exercises beats a
/// rare one that nothing does.
fn visual_order(levels: &[Level]) -> Vec<usize> {
    BidiInfo::reorder_visual(levels)
}

/// A face's vertical metrics, as fractions of the font size.
///
/// Two questions are answered from these and they are not the same question.
/// §10.8.1's `line-height: normal` is the whole of it — ascent, descent and the
/// gap the face asks for between one line and the next. §10.6.1's content area,
/// which is what an inline box's background covers, is the ascent and descent
/// *without* the gap: the gap is space between two lines rather than part of
/// either, and a background drawn over it would run into the line above.
#[derive(Debug, Clone, Copy)]
struct FaceMetrics {
    ascent: f32,
    descent: f32,
    leading: f32,
}

impl FaceMetrics {
    /// For text with no face to measure — an empty span, a line of spaces, a
    /// script no bundled face covers.
    ///
    /// The numbers are the bundled faces' own, near enough: Liberation Serif is
    /// 0.891 over 0.216, Sans 0.905 over 0.212, and Mono 0.833 over 0.300, and
    /// all three ask for no line gap at all. Their *sums* agree to within 0.026
    /// even where the split does not, which is why a single fallback is
    /// defensible and why it comes to about the same 1.11 a real face reports.
    const FALLBACK: Self = Self {
        ascent: 0.89,
        descent: 0.22,
        leading: 0.0,
    };
}

/// An atomic inline box that takes up room on a line without contributing
/// glyphs — an image, in practice.
///
/// Identified by an opaque id rather than a DOM node: line breaking has no
/// business knowing what a document is, and the caller only needs the id
/// handed back so it can find the box again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplacedInline {
    /// The caller's identifier for this box.
    pub id: usize,
    /// Used width.
    pub width: f32,
    /// Used height.
    pub height: f32,
    /// Distance from the box's top edge to the baseline it aligns on.
    ///
    /// Equal to `height` for an image, which is what `vertical-align: baseline`
    /// means for a replaced element: the bottom edge sits on the line's
    /// baseline. An inline-block aligns on the baseline of its own last line
    /// instead (§10.8.1), so part of it hangs below the line's baseline and
    /// this is smaller than its height.
    pub baseline: f32,
}

/// A replaced box after line breaking has placed it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedReplaced {
    /// The caller's identifier, as given.
    pub id: usize,
    /// Left edge, relative to the text origin.
    pub x: f32,
    /// Top edge, relative to the text origin.
    pub y: f32,
    /// Used width.
    pub width: f32,
    /// Used height.
    pub height: f32,
}

/// A stretch of one line that came from a single element.
///
/// What turns a point into the thing under it. An inline element has no box of
/// its own — its text lives in the containing block's line boxes — so without
/// this there is nothing to hit: a link is not a rectangle anywhere until the
/// line breaker says where its glyphs ended up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InlineSpan {
    /// The caller's identifier for the element this came from.
    pub source: usize,
    /// Left edge, relative to the text origin.
    pub x: f32,
    /// Width of the stretch.
    pub width: f32,
    /// Top edge, relative to the text origin.
    pub y: f32,
    /// Height of the line it sits on.
    pub height: f32,
}

/// One side of an inline box, as a width the line has to make room for.
///
/// §8.4: horizontal margins, borders and padding on a non-replaced inline box
/// *are* inserted into the line, at the box's start and end — and only there,
/// however many lines the box is broken across. The vertical ones are drawn but
/// do not enter the line's height, which is §10.6.1 and is a separate rule.
#[derive(Debug, Clone, Copy)]
pub struct InlineEdge {
    /// Room to reserve: the margin, border and padding on this side.
    pub width: f32,
    /// Whether this is the box's opening side rather than its closing one.
    pub opening: bool,
    /// Whether the box's own `direction` is right-to-left.
    ///
    /// Which side "opening" means, and not answerable from the bidi level the
    /// side happens to sit at: an override inside a left-to-right `<span>`
    /// puts the span's own closing side at a right-to-left level, and the
    /// border still belongs on the right.
    pub rtl: bool,
}

/// A run of text with its own style, within a block's inline content.
#[derive(Debug, Clone)]
pub struct InlineRun {
    /// The run's text.
    pub text: String,
    /// The style computed for the element the text came from.
    pub style: ComputedStyle,
    /// Set when this run is an atomic inline box rather than text, in which
    /// case `text` is ignored.
    pub replaced: Option<ReplacedInline>,
    /// Set when this run is one side of an inline box rather than content, in
    /// which case `text` is ignored too.
    pub edge: Option<InlineEdge>,
    /// The element this run's text came from, for hit testing.
    pub source: Option<usize>,
    /// The inline boxes this run sits inside that have something to draw,
    /// outermost first, by the caller's own numbering.
    ///
    /// Not the same question as `source`, which is the innermost *element* and
    /// is about hit testing. A background belongs to an ancestor as much as to
    /// the element the text is in — `<span class=hl>a <b>b</b> c</span>` is one
    /// yellow stretch and not two with a hole where the bold is — so the whole
    /// chain is carried and a fragment is measured over everything inside it.
    /// And a box need not be an element: `::before` draws one and is none.
    /// Empty for almost every run on almost every page, which costs nothing.
    pub boxes: Vec<usize>,
}

impl InlineRun {
    /// A run of styled text.
    pub fn text(text: impl Into<String>, style: ComputedStyle) -> Self {
        Self {
            text: text.into(),
            style,
            replaced: None,
            edge: None,
            source: None,
            boxes: Vec::new(),
        }
    }

    /// Names the element this run came from, so a point can be traced back to
    /// it later.
    pub fn from_element(mut self, source: usize) -> Self {
        self.source = Some(source);
        self
    }

    /// Names the drawn inline boxes this run sits inside, outermost first.
    pub fn inside(mut self, boxes: Vec<usize>) -> Self {
        self.boxes = boxes;
        self
    }

    /// A run that is one atomic inline box.
    pub fn replaced(box_: ReplacedInline, style: ComputedStyle) -> Self {
        Self {
            // Empty rather than a placeholder: whitespace collapsing runs over
            // these runs too, and any text here would be shaped and drawn.
            text: String::new(),
            style,
            replaced: Some(box_),
            edge: None,
            source: Some(box_.id),
            boxes: Vec::new(),
        }
    }

    /// A run that is one side of an inline box: no content, just its room.
    ///
    /// `source` is for hit testing as everywhere else — the element the box
    /// came from, or the one a pseudo-element hangs off. Which box it opens is
    /// the last entry of `boxes`, set with [`InlineRun::inside`].
    pub fn edge(source: Option<usize>, edge: InlineEdge, style: ComputedStyle) -> Self {
        Self {
            text: String::new(),
            style,
            replaced: None,
            edge: Some(edge),
            source,
            boxes: Vec::new(),
        }
    }
}

/// One line's worth of an inline box: the background, padding and border it
/// draws where it crosses that line.
///
/// An inline box broken across three lines draws three of these (§8.4). The
/// horizontal padding and border are on the first and last only, which is what
/// `opens` and `closes` record; the vertical ones are on every fragment,
/// because the box is as tall as its content area wherever it appears.
#[derive(Debug, Clone, Copy)]
pub struct InlineBoxFragment {
    /// The box this fragment belongs to, keying it to a style in
    /// [`TextLayout::inline_boxes`].
    pub source: usize,
    /// Left edge of the content-and-padding area, relative to the text origin.
    pub x: f32,
    /// Top edge of the content area, relative to the text origin.
    pub y: f32,
    /// Width of the stretch, including whichever horizontal edges are on it.
    pub width: f32,
    /// Height of the content area: what the text in it occupies, which is not
    /// the line's height and does not grow with `line-height`.
    pub height: f32,
    /// Whether the box's opening side falls on this fragment.
    pub opens: bool,
    /// Whether its closing side does.
    pub closes: bool,
}

/// A rule drawn under, over, or through a stretch of text.
///
/// Emitted here rather than derived at paint time because only the text layout
/// knows how far a decorated span actually reaches: a glyph carries its origin
/// but not its advance, so paint could only guess at where the last one ends.
#[derive(Debug, Clone, Copy)]
pub struct DecorationRun {
    /// Left edge, relative to the text origin.
    pub x: f32,
    /// Length of the rule.
    pub width: f32,
    /// Top edge, relative to the text origin.
    pub y: f32,
    /// Rule thickness.
    pub thickness: f32,
    /// Colour of the span the rule belongs to, or `None` for the block's.
    pub color: Option<(u8, u8, u8, u8)>,
    /// Whether the span the rule belongs to is hidden, in which case its
    /// underline goes with it.
    pub hidden: bool,
}

/// One laid-out line.
#[derive(Debug, Clone)]
pub struct Line {
    /// Glyphs on this line.
    pub glyphs: Vec<PositionedGlyph>,
    /// Atomic inline boxes sitting on this line.
    pub replaced: Vec<PlacedReplaced>,
    /// Which element each stretch of this line came from.
    pub spans: Vec<InlineSpan>,
    /// Inline boxes crossing this line, outermost first, each with the piece of
    /// itself it draws here.
    pub boxes: Vec<InlineBoxFragment>,
    /// Rules under, over, and through this line's text.
    pub decorations: Vec<DecorationRun>,
    /// The line's text, with glyph offsets pointing into it.
    ///
    /// After whitespace collapsing, so it is what the reader sees rather than
    /// what the source said — which is what a search has to match against.
    pub text: String,
    /// Width of the line's inked content.
    pub width: f32,
    /// Room this line actually had, which is not always the block's content
    /// width: a float narrows it, and `text-align: right` beside one must
    /// measure from the edge of the room rather than from the edge of the box.
    ///
    /// Never wider than the content box in practice, but not guaranteed to be:
    /// a form control's label is shaped at an unwrapped width on purpose, so
    /// callers take the smaller of this and the box they are aligning within.
    pub available: f32,
    /// Top of the line box, relative to the text origin.
    pub y: f32,
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
    /// The style of each inline box that draws something, keyed by the number
    /// its fragments name.
    ///
    /// Held once here rather than copied onto every fragment: a box broken over
    /// twenty lines has one style and twenty rectangles.
    pub inline_boxes: Vec<(usize, ComputedStyle)>,
}

/// A shaped, unbreakable piece of text.
#[derive(Debug, Clone, Default)]
struct Shaped {
    glyphs: Vec<PositionedGlyph>,
    /// The text these glyphs came from, so a line can be reassembled from its
    /// segments and searched.
    text: String,
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
    /// Set when this segment is an atomic inline box rather than text.
    replaced: Option<ReplacedInline>,
    /// The element this segment's text came from.
    source: Option<usize>,
    /// Width of collapsed whitespace following this segment.
    trailing_space: f32,
    /// Whether a line break is required after this segment.
    mandatory_break: bool,
    /// Where this segment hangs on the line. Read for atomic inline boxes
    /// only: raising and lowering *text* is not modelled here.
    align: VerticalAlign,
    /// Horizontal position within the line, filled in during placement.
    x: f32,
    /// The embedding level of the whitespace that follows this segment, which
    /// is not always the segment's own.
    ///
    /// A space between two Hebrew words is right-to-left and travels with
    /// them; the space between the last Hebrew word and the English after it
    /// is a neutral that takes the paragraph's direction, and belongs at the
    /// far end of the Hebrew rather than inside it. Same character, same
    /// place in the source, two different answers — which is why it needs a
    /// level of its own rather than borrowing the segment's.
    space_level: Level,
    /// This segment's bidi embedding level (§8.6 / UAX #9).
    ///
    /// Even is left-to-right, odd is right-to-left. Carried per segment because
    /// reordering happens per *line*, and which segments share a line is not
    /// known until the line has been filled.
    level: Level,
    /// Decoration the segment's style asks for.
    decoration: TextDecoration,
    /// The segment's font size, which sets where its rules sit and how thick
    /// they are.
    font_size: f32,
    /// Whether the span this segment came from is hidden.
    hidden: bool,
    /// Colour of the span this segment came from, for its rules to match.
    color: Option<(u8, u8, u8, u8)>,
    /// Set when this segment is one side of an inline box rather than content.
    edge: Option<InlineEdge>,
    /// The drawn inline boxes this segment sits inside, outermost first.
    boxes: Vec<usize>,
    /// The content area of the face this segment is set in: its ascent above
    /// the baseline and its descent below it, in pixels (§10.6.1). What an
    /// inline box's background covers, and not the line box, which
    /// `line-height` moves around independently.
    content: (f32, f32),
}

impl Segment {
    /// Whether this segment is the opening side of box `id`.
    ///
    /// Asked of the innermost box the segment is inside rather than of its
    /// `source`, which names an *element* and is about hit testing: a box can
    /// be a pseudo-element, which is no element at all.
    fn opens(&self, id: usize) -> bool {
        self.boxes.last() == Some(&id) && self.edge.is_some_and(|edge| edge.opening)
    }

    /// Whether it is the closing side.
    fn closes(&self, id: usize) -> bool {
        self.boxes.last() == Some(&id) && self.edge.is_some_and(|edge| !edge.opening)
    }
}

/// Most shaped segments one store will remember.
///
/// The cache is filled by whatever words a page contains, and a page is a
/// stranger's, so it needs a ceiling. Eight thousand distinct segments is far
/// past what ordinary prose reaches — the era's web repeats itself constantly,
/// which is the whole reason this pays — while staying small enough to sit
/// inside the memory budget the renderer is measured against.
const MAX_SHAPED: usize = 8192;

/// Owns the font database and the glyph raster cache.
pub struct FontStore {
    system: FontSystem,
    cache: SwashCache,
    /// Segments already shaped, by their text and the attributes they were
    /// shaped under.
    ///
    /// Shaping is the bulk of layout — 84ms for 35 KB of plain paragraphs
    /// measured, and laying the same document out twice cost the same both
    /// times — and a document asks for the same short strings over and over:
    /// the words in its navigation, the labels in its tables, every `the` on
    /// the page. Each was shaped from nothing every time.
    ///
    /// Safe to reuse an answer here only because ADR-0005 already demands the
    /// engine be deterministic: identical input must produce identical output,
    /// so a cache can change how long a page takes and cannot change how it
    /// looks. The reference tests are what hold that — they compare rendered
    /// pages against baselines byte for byte, so a cache that returned the
    /// wrong glyphs would fail them rather than pass quietly.
    shaped: std::collections::HashMap<(AttrsOwned, String), Shaped>,
    /// What `line-height: normal` comes to for a set of attributes, per em.
    ///
    /// Asked once per line of every page and answered from a face's metrics,
    /// which means finding the face — so it is cached by the attributes rather
    /// than by the size, and multiplied by the size on the way out.
    face_metrics: std::collections::HashMap<AttrsOwned, FaceMetrics>,
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
            shaped: std::collections::HashMap::new(),
            face_metrics: std::collections::HashMap::new(),
        }
    }

    /// Forgets every shaped segment, keeping the loaded faces.
    ///
    /// For a caller that renders unrelated documents through one store, which
    /// in this project means `tests/fuzz` and nothing else. The browser does
    /// not need it: a renderer child holds exactly one page and is killed when
    /// that page is replaced (see `sandbox::child::serve`), which is why
    /// [`MAX_SHAPED`] can reason about a ceiling "within one page's life" and
    /// refuse to evict.
    ///
    /// A harness that keeps one store for millions of documents breaks that
    /// assumption in the way that matters most to it. The cache saturates a few
    /// hundred documents in; from then on nothing new is ever cached, so every
    /// later document is shaped from nothing — while the baseline it is being
    /// compared against was measured before saturation. That is not a slow
    /// document, it is a slow *store*, and it made the fuzzer report a finding
    /// that could not be reproduced from the bytes it recorded.
    ///
    /// Faces are kept deliberately. Building the bundled database is the one
    /// cost a real child does pay at startup, and paying it per input would
    /// make the render target dramatically slower to no purpose.
    pub fn forget_page(&mut self) {
        self.shaped.clear();
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

    /// The line height to lay out with.
    ///
    /// `normal` arrives unresolved from the cascade, which has no fonts, and is
    /// answered here from the face's own metrics (§10.8.1). Everything else is
    /// already a number of pixels — and is checked, because the cascade computes
    /// it from the font size and `font-size: 1e40px` parses to infinity in an
    /// `f32`. Geometry that leaves this crate is finite.
    pub fn used_line_height(&mut self, style: &ComputedStyle) -> f32 {
        // Measured only where it is needed: `normal` is the one form that costs
        // a font lookup, and the other two are arithmetic.
        let normal = if style.line_height.is_normal() {
            self.normal_line_height(style)
        } else {
            0.0
        };
        style.line_height.resolve(style.font_size, normal)
    }

    /// What `normal` comes to for the face this style resolves to: its ascent,
    /// its descent and the line gap between one line and the next, which is
    /// what §10.8.1's "based on the font" means and what every browser uses.
    fn normal_line_height(&mut self, style: &ComputedStyle) -> f32 {
        let (ascent, descent) = self.content_box(style);
        let face = self.face_metrics_for(style);
        // Rounded, like the halves it is built from. A line box a fraction of a
        // pixel tall accumulates down a page: forty lines of a third of a pixel
        // is a line's worth of drift by the bottom, and it lands differently
        // depending on how many lines came first.
        (ascent + descent + face.leading * style.font_size.max(0.0)).round()
    }

    /// The content area an inline box covers: the face's ascent above the
    /// baseline and its descent below it, in pixels (§10.6.1).
    ///
    /// The line *gap* is deliberately absent. It is space between one line and
    /// the next, not part of either — so a background drawn over it would run
    /// into the line above.
    /// Rounded to whole pixels, which is what every browser built on FreeType
    /// does and is not cosmetic: the ascent decides where a baseline sits, and
    /// a baseline on a half pixel is a line of text rendered through a filter.
    /// It is also what makes a line height stable — the same font at the same
    /// size gives the same integer however many lines precede it.
    pub fn content_box(&mut self, style: &ComputedStyle) -> (f32, f32) {
        let face = self.face_metrics_for(style);
        let size = style.font_size.max(0.0);
        ((face.ascent * size).round(), (face.descent * size).round())
    }

    /// The face's vertical metrics, per em.
    ///
    /// The face is found by shaping one character rather than by asking the
    /// font database, because `cosmic-text` keeps its matching to itself: the
    /// id inside a `FontMatchKey` is private and a glyph's is not. `x` is the
    /// probe — present in every face bundled here, and cheap because the
    /// shaping cache answers the second request for it.
    ///
    /// Cached, because it is asked once per line of every page. A run with no
    /// glyphs — an empty span, a line of spaces, a script no bundled face
    /// covers — has no face to ask and gets [`FaceMetrics::FALLBACK`].
    fn face_metrics_for(&mut self, style: &ComputedStyle) -> FaceMetrics {
        if !(style.font_size.is_finite() && style.font_size > 0.0) {
            return FaceMetrics::FALLBACK;
        }
        let key = AttrsOwned::new(&Self::attrs_for(style, style.font_size));
        if let Some(found) = self.face_metrics.get(&key) {
            return *found;
        }
        let measured = self.measure_face(style).unwrap_or(FaceMetrics::FALLBACK);
        self.face_metrics.insert(key, measured);
        measured
    }

    /// Reads the face's metrics, or `None` when no face could be found.
    fn measure_face(&mut self, style: &ComputedStyle) -> Option<FaceMetrics> {
        // A line height that cannot come back round to `normal`: this is what
        // answers that question, so it must not ask it.
        let probe = self.shape_with("x", style, style.font_size);
        let font = self.system.get_font(probe.glyphs.first()?.font_id)?;
        let metrics = font.as_swash().metrics(&[]);
        let units = f32::from(metrics.units_per_em);
        if units <= 0.0 {
            return None;
        }
        let face = FaceMetrics {
            ascent: metrics.ascent / units,
            descent: metrics.descent / units,
            leading: metrics.leading.max(0.0) / units,
        };
        let sane = face.ascent.is_finite()
            && face.descent.is_finite()
            && face.leading.is_finite()
            && face.ascent + face.descent > 0.0;
        sane.then_some(face)
    }

    /// Metrics cosmic-text will accept.
    ///
    /// `Buffer::new` asserts that the line height is not zero, and ours is
    /// derived from the font size — so `font-size: 0`, which is a legal
    /// declaration and a common one (it is how the era's authors and ours both
    /// close the gap between inline-blocks), took the whole browser down. A
    /// panic reachable from a stylesheet is not a rendering bug, it is a denial
    /// of service, and the fuzzer found it in the first soak.
    ///
    /// `line-height: 0` on its own is legal too and means something different:
    /// the text still has glyphs and width, the line box just contributes no
    /// height. So the floor here is only about keeping the shaper alive — what
    /// the caller reports as the line's height is decided in [`Self::shape_segment`].
    ///
    /// Non-finite values are floored for the same reason: a `NaN` size would
    /// propagate silently into every coordinate downstream of it.
    fn metrics_for(style: &ComputedStyle, line_height: f32) -> Metrics {
        /// Small enough to be invisible, large enough that no arithmetic
        /// downstream divides by something near zero.
        const FLOOR: f32 = 0.01;
        let size = if style.font_size.is_finite() && style.font_size > FLOOR {
            style.font_size
        } else {
            FLOOR
        };
        let line = if line_height.is_finite() && line_height > FLOOR {
            line_height
        } else {
            FLOOR
        };
        Metrics::new(size, line)
    }

    /// Attributes for one inline span.
    fn attrs_for(style: &ComputedStyle, line_height: f32) -> Attrs<'static> {
        Attrs::new()
            // Part of the shaping key, which is what makes this safe to cache:
            // the same words at two spacings are two different shapings and
            // `AttrsOwned` already knows it.
            //
            // Divided by the font size, and that is not a detail. `cosmic-text`
            // adds this to an advance it has *already* divided by the font
            // scale, so the number it wants is in em rather than in pixels —
            // handing it `5` for `letter-spacing: 5px` spaces the text by five
            // ems, which at 16px is eighty pixels a letter and looks exactly as
            // wrong as it sounds. Found by rendering it, not by reading it.
            .letter_spacing(if style.font_size > 0.0 {
                style.letter_spacing / style.font_size
            } else {
                // The fuzzer has already found `font-size: 0` once. Nothing to
                // scale against, and no spacing worth adding to invisible text.
                0.0
            })
            .family(Self::family_for(style))
            .weight(Weight(style.font_weight))
            .stretch(Stretch::Normal)
            .style(match style.font_style {
                FontStyle::Italic => Style::Italic,
                FontStyle::Normal => Style::Normal,
            })
            .metrics(Self::metrics_for(style, line_height))
            .color(cosmic_text::Color::rgba(
                style.color.r,
                style.color.g,
                style.color.b,
                style.color.a,
            ))
    }

    /// Shapes and wraps `text` to `max_width`, in a single style.
    pub fn layout(&mut self, text: &str, style: &ComputedStyle, max_width: f32) -> TextLayout {
        let runs = [InlineRun::text(text, style.clone())];
        self.layout_runs(&runs, style, max_width)
    }

    /// Shapes a single unbreakable segment at its natural width.
    ///
    /// Shaping happens per segment rather than per character, so joining
    /// scripts and ligatures within a segment stay correct; segments are cut
    /// only at Unicode break opportunities, where shaping does not carry over.
    /// Shapes a segment that the bidi algorithm has put at `level`.
    ///
    /// Right-to-left *script* needs nothing: `cosmic-text` shapes Hebrew and
    /// Arabic in their own direction from the characters themselves, which is
    /// why they came out right long before any of this existed.
    ///
    /// What needs handling is left-to-right text that an override has put at a
    /// right-to-left level — `unicode-bidi: bidi-override`, or a U+202E in the
    /// source. The shaper does not act on the explicit formatting codes at all:
    /// handed `<RLO>abcdef<PDF>` it shapes two blank glyphs and six letters
    /// forwards, which was worth measuring before believing. So the run is
    /// shaped forwards and then mirrored here.
    ///
    /// That is rule L2 applied to glyphs rather than to segments, and it is the
    /// one piece of UAX #9 this engine does itself. It is not the part the
    /// plan says to leave alone — the levels still come from `unicode-bidi` —
    /// and there is nowhere else to put it: no shaper will reverse letters it
    /// has been given no reason to reverse.
    fn shape_directed(&mut self, text: &str, style: &ComputedStyle, level: Level) -> Shaped {
        // The controls have done their work in `analyse_bidi` and are not
        // content. Left in, they shape as blank boxes that take real width.
        let bare = without_bidi_controls(text);
        if bare.is_empty() {
            return Shaped::default();
        }
        if level.is_ltr() || bare.chars().any(is_strongly_rtl) {
            return self.shape_segment(&bare, style);
        }
        // Rule L4 as well as L2: a bracket in right-to-left text is drawn as
        // the bracket that faces the other way, so that `(x)` reads as `(x)`
        // once the run has been turned round rather than as `)x(`. Done by
        // shaping the mirrored characters rather than by substituting glyphs,
        // which leaves the shaper to find them — it is the one that knows what
        // is in the face.
        let mut shaped = self.shape_segment(&mirrored(&bare), style);
        mirror(&mut shaped);
        // The offsets index the text the author wrote, and mirroring is
        // character for character, so they still line up.
        shaped.text = bare;
        shaped
    }

    fn shape_segment(&mut self, text: &str, style: &ComputedStyle) -> Shaped {
        let line_height = self.used_line_height(style);
        if style.font_variant == FontVariant::SmallCaps {
            return self.shape_small_caps(text, style, line_height);
        }
        self.shape_with(text, style, line_height)
    }

    /// Draws lowercase letters as smaller capitals (§15.8's `small-caps`).
    ///
    /// Synthesised, because the bundled faces have no small-caps variant to ask
    /// for and there is nowhere else to get one. The synthesis is the standard
    /// one: uppercase the lowercase letters and shape them at [`SMALL_CAPS`] of
    /// the size, leaving every other character alone.
    ///
    /// The line's metrics come from the full-size shaping rather than from the
    /// pieces. A word that happens to be all lowercase would otherwise sit on a
    /// shorter line than the word beside it, and a paragraph of small caps
    /// would read as a paragraph somebody set in a smaller font.
    fn shape_small_caps(&mut self, text: &str, style: &ComputedStyle, line_height: f32) -> Shaped {
        let mut plain = style.clone();
        plain.font_variant = FontVariant::Normal;
        let mut small = plain.clone();
        small.font_size = style.font_size * SMALL_CAPS;

        // Full size, for the ascent and the line height — and for the whole
        // answer when the segment has no lowercase in it to shrink, which on a
        // page that sets this is most of them.
        let full = self.shape_with(text, &plain, line_height);
        if !text.chars().any(char::is_lowercase) {
            return full;
        }

        let mut out = Shaped {
            text: text.to_owned(),
            ascent: full.ascent,
            height: full.height,
            ..Shaped::default()
        };
        for (at, piece) in case_runs(text) {
            let raised = piece.to_uppercase();
            let lower = piece.starts_with(char::is_lowercase);
            let shaped = if lower {
                self.shape_with(&raised, &small, line_height)
            } else {
                self.shape_with(piece, &plain, line_height)
            };
            // `to_uppercase` can change a piece's length — ß becomes SS — while
            // the glyph offsets index the *original* text, which is what a
            // search and a selection are made of. So they are shifted where the
            // two lengths agree, which is every ASCII word, and collapsed to the
            // whole piece where they do not: a selection that snaps to one word
            // beats an offset pointing into the middle of a character.
            let exact = !lower || raised.len() == piece.len();
            for glyph in &shaped.glyphs {
                let (start, end) = if exact {
                    (at + glyph.start, at + glyph.end)
                } else {
                    (at, at + piece.len())
                };
                out.glyphs.push(PositionedGlyph {
                    x: glyph.x + out.width,
                    start,
                    end,
                    ..*glyph
                });
            }
            out.width += shaped.width;
        }
        out
    }

    /// The same, at a line height already resolved.
    ///
    /// Split out so that measuring what `normal` means can shape its probe
    /// without asking what `normal` means.
    fn shape_with(&mut self, text: &str, style: &ComputedStyle, line_height: f32) -> Shaped {
        if text.is_empty() {
            return Shaped::default();
        }
        // Text at zero size occupies nothing. Shaping it would be work whose
        // every result is multiplied by zero, and the glyphs would be invisible
        // either way — so it is skipped rather than floored, which is also what
        // keeps `font-size: 0` from quietly rendering at the floor size.
        if !(style.font_size.is_finite() && style.font_size > 0.0) {
            return Shaped {
                text: text.to_owned(),
                ..Shaped::default()
            };
        }
        let attrs = Self::attrs_for(style, line_height);
        // Keyed on the attributes themselves rather than on a list of the style
        // properties that matter. `AttrsOwned` carries exactly what `Attrs`
        // carries — the family, the weight, the slant, the colour, and the
        // metrics folded in by `attrs_for` — so a property added there is in
        // the key by construction. A hand-written key would be one someone
        // could forget to extend, and the failure would be a page drawn with
        // another style's glyphs.
        //
        // Below the guards above, deliberately. `metrics_for` floors a
        // non-positive size to something visible while the guard returns early
        // for one, so a zero size and a nearly-zero one share a key and mean
        // different things. Only shaped text is ever stored or looked up here,
        // and the zero-size path never reaches this.
        let key = (AttrsOwned::new(&attrs), text.to_owned());
        if let Some(shaped) = self.shaped.get(&key) {
            return shaped.clone();
        }

        let mut buffer = Buffer::new(&mut self.system, Self::metrics_for(style, line_height));
        let mut buffer = buffer.borrow_with(&mut self.system);
        // No width limit: a segment is by definition not broken further.
        buffer.set_size(None, None);
        // Advanced shaping: required for correctness on anything beyond plain
        // Latin, and the cost is irrelevant next to being wrong.
        buffer.set_rich_text([(text, attrs.clone())], &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(false);

        let mut shaped = Shaped {
            text: text.to_owned(),
            ..Shaped::default()
        };
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
                    // Stamped per segment when the line is assembled: keeping
                    // it out of the shaping cache is what lets one cached
                    // shaping serve a hidden span and a visible one.
                    hidden: false,
                    start: glyph.start,
                    end: glyph.end,
                })
                .collect();
        }
        // Full means stop rather than evict, as elsewhere: within one page's
        // life there is no access pattern worth modelling, and what stopping
        // costs is the speed this exists for rather than correctness.
        if self.shaped.len() < MAX_SHAPED {
            self.shaped.insert(key, shaped.clone());
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
        self.layout_runs_in(runs, default_style, Strut::Always, |_, _| (0.0, max_width))
    }

    /// The same, told whether the document's mode puts a strut on every line.
    pub fn layout_runs_with(
        &mut self,
        runs: &[InlineRun],
        default_style: &ComputedStyle,
        strut: Strut,
        max_width: f32,
    ) -> TextLayout {
        self.layout_runs_in(runs, default_style, strut, |_, _| (0.0, max_width))
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
        self.layout_runs_in(runs, default_style, Strut::Always, constraints)
    }

    /// The whole of it: runs, a strut rule, and a width that varies down the
    /// page.
    pub fn layout_runs_in<F>(
        &mut self,
        runs: &[InlineRun],
        default_style: &ComputedStyle,
        strut: Strut,
        constraints: F,
    ) -> TextLayout
    where
        F: Fn(f32, f32) -> (f32, f32),
    {
        let segments = self.segment(runs, default_style.direction);
        if segments.is_empty() {
            return TextLayout::default();
        }

        let mut layout = TextLayout::default();
        // One entry per inline box that draws something, taken from its opening
        // edge — which the caller emits for exactly the boxes that have
        // anything to draw, so there is no filtering to do here.
        for run in runs {
            if let (Some(edge), Some(&id)) = (run.edge, run.boxes.last())
                && edge.opening
            {
                layout.inline_boxes.push((id, run.style.clone()));
            }
        }
        let mut y = 0.0f32;
        let mut current: Vec<Segment> = Vec::new();
        let mut x = 0.0f32;
        let held_height = self.used_line_height(default_style);
        let line_height = held_height;
        // §10.8.1's strut: an invisible box of the block's own font and line
        // height, on every line box whether or not there is text on it.
        //
        // Its ascent was `font_size * 0.8` — near enough for deciding how tall
        // a line is, and nowhere near enough for deciding how far *below* the
        // baseline the line reaches, which is the same number subtracted from
        // the line height. The face's own metrics put an image alone in a table
        // cell on the pixel row a browser puts it on; the approximation put it
        // more than a pixel out, and that difference is the whole of the era's
        // sliced-image layouts.
        let (strut_ascent, strut_descent) =
            Self::strut(self.content_box(default_style), line_height);
        let (strut_ascent, strut_descent, strut_height) = match strut {
            Strut::Always => (strut_ascent, strut_descent, line_height),
            // Held back until a line turns out to have text on it, and applied
            // then rather than here: what it contributes is the same either
            // way, and this is the only place that knows the difference.
            Strut::WhereThereIsText => (0.0, 0.0, 0.0),
        };
        let mut line_height = strut_height;
        let mut ascent = if default_style.font_size.is_finite() {
            strut_ascent
        } else {
            0.0
        };
        // What hangs below the baseline, counted for atomic inline boxes only.
        //
        // A line's height here is the tallest thing on it, which is right while
        // everything on the line is hung from the same baseline. An inline-block
        // is not: it brings its own, and whatever sits below that baseline is
        // height the line has to find from somewhere or the box overlaps the
        // line beneath it.
        //
        // Text is deliberately left out. Its ascent and height come from
        // different places — the shaper's metrics and the computed
        // `line-height` — and mixing them here is the half-leading model, which
        // is a larger change than this: it moves every line on every page,
        // including ones with nothing atomic on them at all.
        //
        // The strut's own descent is the exception, and it is not an exception
        // to the paragraph above: it is the same number for every line in the
        // block, so it can be taken once here without asking what landed on
        // any particular line. §10.8.1 puts the strut on every line box whether
        // or not there is text on it, which is what gives an image alone in a
        // table cell the few pixels of room below it that the era's sliced-image
        // layouts are famous for tripping over. Without it a line held nothing
        // but its tallest box and the descender space vanished.
        let mut descent = strut_descent;

        // The available width depends on the line's height, and the height
        // depends on what lands on the line. Query with the height so far and
        // accept that a line which grows taller mid-fill keeps the width it
        // started with — the alternative is re-flowing, which can oscillate.
        let mut available = constraints(y, line_height).1;

        // §16.4: `text-indent` moves the first line only, and takes its room
        // out of that line rather than out of the block — so the indent both
        // shifts the line right and gives it that much less to fill, which is
        // why it cannot be added at paint time. A percentage resolves against
        // the containing block's width, which is what the constraint just
        // reported for a full-height line.
        //
        // Tracked as a flag rather than by testing `y == 0.0`, because a first
        // line that is also the last never advances `y` and the two cases would
        // be indistinguishable.
        let indent = default_style
            .text_indent
            .to_px(default_style.font_size, available);
        let mut first_line = true;
        available -= indent;

        for segment in segments {
            let fits = current.is_empty() || x + segment.shaped.width <= available;
            if !fits {
                // Ended because the next word would not fit, so this is not the
                // last line of its paragraph and §16.2 justifies it.
                let justify = (default_style.text_align == css::style::TextAlign::Justify)
                    .then_some(available);
                let (offset, room) = constraints(y, line_height);
                let offset = offset + if first_line { indent } else { 0.0 };
                first_line = false;
                Self::push_line(
                    &mut layout,
                    &mut current,
                    Room {
                        offset,
                        available: room,
                    },
                    y,
                    ascent,
                    line_height,
                    justify,
                );
                y += line_height;
                x = 0.0;
                line_height = strut_height;
                ascent = strut_ascent;
                descent = strut_descent;
                available = constraints(y, line_height).1;
            }

            match (segment.replaced, segment.align) {
                // §10.8.1: aligned to the line box rather than to a baseline.
                // Such a box makes the line at least as tall as itself and
                // moves the baseline not at all — which is what lets a row of
                // `vertical-align: bottom` inline-blocks line their bottoms up
                // however deep each one is.
                (Some(box_), VerticalAlign::Top | VerticalAlign::Bottom) => {
                    line_height = line_height.max(box_.height);
                }
                // `middle` centres the box on the parent's baseline raised by
                // half an x-height. The x-height here is taken as a quarter of
                // the font size rather than measured, which is close for every
                // face bundled with this engine.
                (Some(box_), VerticalAlign::Middle) => {
                    let half = box_.height / 2.0;
                    let shift = segment.font_size * X_HEIGHT / 2.0;
                    ascent = ascent.max(half + shift);
                    descent = descent.max((half - shift).max(0.0));
                }
                (replaced, _) => {
                    ascent = ascent.max(segment.shaped.ascent);
                    if replaced.is_some() {
                        descent = descent.max(segment.shaped.height - segment.shaped.ascent);
                    } else if strut == Strut::WhereThereIsText {
                        // Text on the line, so the quirk does not apply to it
                        // after all and the strut joins in — from here on, and
                        // not retroactively, which is the same thing: every
                        // contribution is a maximum.
                        let (held_ascent, held_descent) =
                            Self::strut(self.content_box(default_style), held_height);
                        ascent = ascent.max(held_ascent);
                        descent = descent.max(held_descent);
                        line_height = line_height.max(held_height);
                    }
                    line_height = line_height.max(segment.shaped.height);
                }
            }
            if descent > 0.0 {
                line_height = line_height.max(ascent + descent);
            }
            let advance = segment.shaped.width + segment.trailing_space;
            let forced = segment.mandatory_break;
            let mut placed = segment;
            placed.x = x;
            x += advance;
            current.push(placed);

            // A newline in `pre`, or any other mandatory opportunity, ends the
            // line regardless of how much room is left.
            if forced {
                let (offset, room) = constraints(y, line_height);
                let offset = offset + if first_line { indent } else { 0.0 };
                first_line = false;
                Self::push_line(
                    &mut layout,
                    &mut current,
                    Room {
                        offset,
                        available: room,
                    },
                    y,
                    ascent,
                    line_height,
                    None,
                );
                y += line_height;
                x = 0.0;
                line_height = self.used_line_height(default_style);
                ascent = default_style.font_size * 0.8;
                descent = 0.0;
                available = constraints(y, line_height).1;
            }
        }

        if !current.is_empty() {
            let (offset, room) = constraints(y, line_height);
            let offset = offset + if first_line { indent } else { 0.0 };
            Self::push_line(
                &mut layout,
                &mut current,
                Room {
                    offset,
                    available: room,
                },
                y,
                ascent,
                line_height,
                None,
            );
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
        room: Room,
        line_y: f32,
        ascent: f32,
        line_height: f32,
        justify_to: Option<f32>,
    ) {
        // UAX #9's rule L2, and the whole of what bidi costs this engine: the
        // segments were filled in *logical* order, and a line is drawn in
        // visual order. `reorder_visual` returns its input's own order for an
        // all-left-to-right line, so this runs unconditionally rather than
        // behind a test for right-to-left content — one code path that is
        // exercised by every page rather than a rare one that is not.
        //
        // The x positions are recomputed here rather than trusted from the
        // fill loop, which assigned them left to right as each word arrived
        // and could not have known what would land beside them.
        let order = Self::visual_line(current);
        let mut pen = 0.0;
        // Whitespace owed to the far end of a right-to-left run: it follows the
        // run in logical order and so follows it visually too, which is past
        // every segment of the run rather than beside the one that carries it.
        let mut owed = 0.0;
        for &at in &order {
            let segment = &mut current[at];
            if segment.level.is_rtl() {
                // A space *inside* a right-to-left run is drawn before the word
                // it follows, "after" being the logical side and this the
                // visual one. Keeping it on the right in both cases sends the
                // space between two Hebrew words to the end of the line, where
                // it is invisible and the two words end up touching.
                if segment.space_level.is_rtl() {
                    pen += segment.trailing_space;
                    segment.x = pen;
                    pen += segment.shaped.width;
                } else {
                    segment.x = pen;
                    pen += segment.shaped.width;
                    owed += segment.trailing_space;
                }
            } else {
                pen += std::mem::take(&mut owed);
                segment.x = pen;
                pen += segment.shaped.width + segment.trailing_space;
            }
        }

        // §16.2: `justify` stretches the spaces until the line fills its box.
        // Only the spaces — letters stay where the shaper put them — and only
        // on a line that was ended because the next word would not fit. The
        // last line of a paragraph, and the line before a forced break, keep
        // their natural width, which is why the caller decides rather than this.
        if let Some(target) = justify_to {
            let inked = order
                .last()
                .map_or(0.0, |&at| current[at].x + current[at].shaped.width);
            let gaps = order
                .iter()
                .take(order.len().saturating_sub(1))
                .filter(|&&at| current[at].trailing_space > 0.0)
                .count();
            let slack = target - inked;
            if gaps > 0 && slack > 0.0 {
                let each = slack / gaps as f32;
                let mut shift = 0.0;
                // In visual order, because that is the order the gaps appear
                // in: walking the logical one would widen the space after a
                // Hebrew word by however much the words drawn to its *right*
                // had claimed.
                for &at in &order {
                    current[at].x += shift;
                    if current[at].trailing_space > 0.0 {
                        shift += each;
                    }
                }
            }
        }
        let Room { offset, available } = room;
        let mut glyphs = Vec::new();
        let mut text = String::new();
        let mut width = 0.0f32;
        for segment in current.iter() {
            // Glyph offsets are relative to their own segment; the line's text
            // is the segments joined, so they shift by however much came first.
            let base = text.len();
            text.push_str(&segment.shaped.text);
            for glyph in &segment.shaped.glyphs {
                glyphs.push(PositionedGlyph {
                    hidden: segment.hidden,
                    x: glyph.x + segment.x + offset,
                    // Absolute baseline within the block: the line's own top
                    // plus the shared ascent. Using the ascent alone would
                    // stack every line at the same y.
                    y: line_y + ascent,
                    start: base + glyph.start,
                    end: base + glyph.end,
                    ..*glyph
                });
            }
            // The space between two segments is real text even though it has
            // no glyphs: a search for "one two" has to find it.
            if segment.trailing_space > 0.0 {
                text.push(' ');
            }
            // Trailing spaces are excluded from the *width*: a line's width is
            // its inked extent, which is what centring must measure.
            width = width.max(segment.x + segment.shaped.width);
        }
        layout.lines.push(Line {
            glyphs,
            replaced: Self::replaced_for(current, offset, line_y, ascent, line_height),
            spans: Self::spans_for(current, offset, line_y, line_height),
            boxes: Self::boxes_for(current, &order, offset, line_y + ascent),
            decorations: Self::decorations_for(current, offset, line_y + ascent),
            text,
            width,
            available,
            y: line_y,
            baseline: ascent,
        });
        current.clear();
    }

    /// The strut's half of a line box: how far it reaches above and below the
    /// baseline (§10.8.1).
    ///
    /// `line-height` is distributed evenly above and below the content area —
    /// half-leading — rather than being added underneath. A paragraph set at
    /// `line-height: 2` has its extra room split between the lines, not hung
    /// off the bottom of each one, and putting it all below is what tips text
    /// out of the middle of a table cell.
    fn strut(content: (f32, f32), line_height: f32) -> (f32, f32) {
        let (ascent, descent) = content;
        let leading = (line_height - ascent - descent) / 2.0;
        ((ascent + leading).max(0.0), (descent + leading).max(0.0))
    }

    /// The order to draw this line's segments in, left to right.
    ///
    /// UAX #9's rule L2 over the *text*, and then the inline boxes' own sides
    /// put back where they belong. An `<a>`'s left border is not a character
    /// and does not reorder like one: §8.4 puts it at the start edge of the
    /// box, which is the left of its leftmost fragment for a left-to-right
    /// box and the right of its rightmost for a right-to-left one, however the
    /// text inside it was rearranged.
    ///
    /// Reordering the sides along with the text is what put a span's left
    /// padding on the wrong side of the word it belonged to, and it only
    /// showed up once `direction` was honoured at all — before that, no line
    /// was ever reordered and every side happened to be in the right place.
    fn visual_line(current: &[Segment]) -> Vec<usize> {
        let content: Vec<usize> = (0..current.len())
            .filter(|&at| current[at].edge.is_none())
            .collect();
        let levels: Vec<Level> = content.iter().map(|&at| current[at].level).collect();
        // Visual position of each content segment, by its index in `current`.
        let mut visual = vec![0usize; current.len()];
        for (position, &at) in visual_order(&levels).iter().enumerate() {
            visual[content[at]] = position;
        }

        // Where a box's content sits on this line, visually.
        let span_of = |box_: usize| {
            content
                .iter()
                .filter(|&&at| current[at].boxes.contains(&box_))
                .fold(None, |found: Option<(usize, usize)>, &at| {
                    let here = visual[at];
                    Some(match found {
                        Some((first, last)) => (first.min(here), last.max(here)),
                        None => (here, here),
                    })
                })
        };

        let mut keys: Vec<(f32, usize)> = Vec::with_capacity(current.len());
        for (at, segment) in current.iter().enumerate() {
            let key = match segment.edge {
                None => visual[at] as f32,
                Some(edge) => {
                    let Some(&box_) = segment.boxes.last() else {
                        continue;
                    };
                    // How deeply nested this box is, so that when two boxes
                    // share an edge the outer one's side stays outside the
                    // inner one's. A *larger* depth sits nearer the content,
                    // which is why it moves the key inwards on both sides —
                    // getting that backwards nests `<div><div>` the wrong way
                    // round and is not a bidi bug at all: it shows up on a
                    // plain left-to-right page with two borders in a row.
                    let depth = (segment.boxes.len() as f32 / 1000.0).min(0.4);
                    match span_of(box_) {
                        // An empty box has no content to hang from and keeps
                        // the place the source gave it.
                        None => visual.get(at).copied().unwrap_or(at) as f32,
                        Some((first, last)) => {
                            if edge.opening != edge.rtl {
                                first as f32 - 0.5 + depth
                            } else {
                                last as f32 + 0.5 - depth
                            }
                        }
                    }
                }
            };
            keys.push((key, at));
        }
        // Stable, so segments that tie keep source order — which is what
        // settles two empty boxes side by side.
        keys.sort_by(|a, b| a.0.total_cmp(&b.0));
        keys.into_iter().map(|(_, at)| at).collect()
    }

    /// One fragment per inline box crossing this line.
    ///
    /// Measured over every segment *inside* the box rather than over the ones
    /// that name it as their innermost element, which is the difference between
    /// one background behind a whole highlighted phrase and two with a hole
    /// where a nested `<b>` sits.
    ///
    /// The height is the content area — what the text occupies — and not the
    /// line box. §10.6.1: `line-height` does not grow an inline box's
    /// background, so a paragraph set double-spaced highlights its phrases at
    /// the size of the words rather than in bands that touch.
    fn boxes_for(
        segments: &[Segment],
        order: &[usize],
        offset: f32,
        baseline: f32,
    ) -> Vec<InlineBoxFragment> {
        let mut out: Vec<InlineBoxFragment> = Vec::new();
        // In *visual* order, and merging only into a fragment the previous
        // segment was also part of. Reordering can cut a box in two on one
        // line — a `<span>` around text an override sends to the far side of
        // its neighbours is drawn as two boxes with the neighbours between
        // them — and merging by element alone would draw one box spanning the
        // gap, with a border straight through the text that is not inside it.
        let mut previous: Vec<usize> = Vec::new();
        for &at in order {
            let segment = &segments[at];
            let left = segment.x + offset;
            let right = left + segment.shaped.width;
            let (top, height) = match segment.replaced {
                // An atomic box hangs from the baseline by its own; the inline
                // box around it has to cover all of it.
                Some(box_) => (baseline - box_.baseline, box_.height),
                // §10.6.1: the content area, from the face's own ascent and
                // descent. Not from the shaped metrics, which carry the CSS
                // line height — that would put a double-spaced paragraph's
                // highlights in bands that touch each other rather than at the
                // size of the words.
                None => (
                    baseline - segment.content.0,
                    segment.content.0 + segment.content.1,
                ),
            };
            for &source in &segment.boxes {
                let joins = previous.contains(&source);
                match out
                    .iter_mut()
                    .rev()
                    .find(|box_| box_.source == source)
                    .filter(|_| joins)
                {
                    Some(found) => {
                        found.width = right - found.x;
                        let bottom = (found.y + found.height).max(top + height);
                        found.y = found.y.min(top);
                        found.height = bottom - found.y;
                        found.opens |= segment.opens(source);
                        found.closes |= segment.closes(source);
                    }
                    None => out.push(InlineBoxFragment {
                        source,
                        x: left,
                        y: top,
                        width: right - left,
                        height,
                        opens: segment.opens(source),
                        closes: segment.closes(source),
                    }),
                }
            }
            previous.clone_from(&segment.boxes);
        }
        out
    }

    /// Merges the line's segments into one span per element.
    ///
    /// Adjacent segments from the same element become one stretch, so a link
    /// of several words is one target rather than one per word — and so the
    /// space between those words is inside it, which is where a pointer
    /// travelling along a link spends much of its time.
    fn spans_for(
        segments: &[Segment],
        offset: f32,
        line_y: f32,
        line_height: f32,
    ) -> Vec<InlineSpan> {
        let mut out: Vec<InlineSpan> = Vec::new();
        for segment in segments {
            let Some(source) = segment.source else {
                continue;
            };
            let left = segment.x + offset;
            let right = left + segment.shaped.width;
            match out.last_mut() {
                Some(last) if last.source == source => last.width = right - last.x,
                _ => out.push(InlineSpan {
                    source,
                    x: left,
                    width: right - left,
                    y: line_y,
                    height: line_height,
                }),
            }
        }
        out
    }

    /// Places one line's atomic inline boxes.
    ///
    /// Each is hung from the line's baseline by its own: an image, whose
    /// baseline is its bottom edge, sits on the line rather than floating above
    /// or below it, and an inline-block lines its last line of text up with the
    /// text beside it.
    fn replaced_for(
        segments: &[Segment],
        offset: f32,
        line_y: f32,
        ascent: f32,
        line_height: f32,
    ) -> Vec<PlacedReplaced> {
        segments
            .iter()
            .filter_map(|segment| {
                let box_ = segment.replaced?;
                let y = match segment.align {
                    VerticalAlign::Top => line_y,
                    VerticalAlign::Bottom => line_y + line_height - box_.height,
                    VerticalAlign::Middle => {
                        line_y + ascent - box_.height / 2.0 - segment.font_size * X_HEIGHT / 2.0
                    }
                    VerticalAlign::Baseline => line_y + ascent - box_.baseline,
                };
                Some(PlacedReplaced {
                    id: box_.id,
                    x: segment.x + offset,
                    y,
                    width: box_.width,
                    height: box_.height,
                })
            })
            .collect()
    }

    /// Builds the rules for one line's decorated spans.
    ///
    /// Adjacent segments sharing a decoration are merged into a single rule
    /// that runs through the space between them. Emitting one rule per segment
    /// instead would leave a gap under every space, which is not how an
    /// underline has ever looked.
    fn decorations_for(segments: &[Segment], offset: f32, baseline: f32) -> Vec<DecorationRun> {
        /// A decorated stretch being accumulated across segments.
        struct Open {
            left: f32,
            right: f32,
            decoration: TextDecoration,
            font_size: f32,
            color: Option<(u8, u8, u8, u8)>,
            hidden: bool,
        }

        let mut out: Vec<DecorationRun> = Vec::new();
        let mut open: Option<Open> = None;

        let close = |open: Option<Open>, out: &mut Vec<DecorationRun>| {
            let Some(run) = open else { return };
            let width = run.right - run.left;
            if width <= 0.0 {
                return;
            }
            // Proportions taken from the CSS 2.1 sample rendering rather than
            // from the font's own post table: with three bundled families the
            // difference is invisible, and a fixed ratio keeps a rule under
            // mixed spans of one size from stepping up and down.
            let thickness = (run.font_size / 14.0).max(1.0).round();
            let mut push = |y: f32| {
                out.push(DecorationRun {
                    x: run.left,
                    width,
                    y,
                    thickness,
                    color: run.color,
                    hidden: run.hidden,
                });
            };
            if run.decoration.underline {
                push(baseline + run.font_size * 0.12);
            }
            if run.decoration.line_through {
                push(baseline - run.font_size * 0.28);
            }
            if run.decoration.overline {
                push(baseline - run.font_size * 0.85);
            }
        };

        for segment in segments {
            let left = segment.x + offset;
            let right = left + segment.shaped.width;
            if segment.decoration.is_none() {
                close(open.take(), &mut out);
                continue;
            }
            match &mut open {
                // Same decoration as the run in progress: extend it, which
                // carries the rule across the space that separated them.
                Some(run)
                    if run.decoration == segment.decoration
                        && run.color == segment.color
                        && run.font_size == segment.font_size
                        && run.hidden == segment.hidden =>
                {
                    run.right = right;
                }
                _ => {
                    close(open.take(), &mut out);
                    open = Some(Open {
                        left,
                        right,
                        decoration: segment.decoration,
                        font_size: segment.font_size,
                        color: segment.color,
                        hidden: segment.hidden,
                    });
                }
            }
        }
        close(open, &mut out);
        out
    }

    /// Cuts runs into unbreakable segments at Unicode break opportunities.
    ///
    /// Uses the Unicode line breaking algorithm rather than splitting on
    /// spaces. Scripts without spaces — CJK above all — break between
    /// characters, and a space-splitting breaker would hand them one
    /// unbreakable segment per paragraph that could never wrap.
    fn segment(&mut self, runs: &[InlineRun], base: Direction) -> Vec<Segment> {
        let (bidi, starts) = analyse_bidi(runs, base);
        let mut out: Vec<Segment> = Vec::new();
        for (index, run) in runs.iter().enumerate() {
            // Where this run's text begins in the string the algorithm saw,
            // which is not where it begins in the text that will be drawn: the
            // control characters `unicode-bidi` stands for sit in between.
            let run_start = starts[index];
            // Measured once per run rather than once per segment: a paragraph
            // is one run and dozens of segments, all in the same face.
            let content = self.content_box(&run.style);
            // An atomic inline box is one unbreakable segment of its own size,
            // aligned on the baseline it declares. For an image that is its
            // bottom edge, which is what `vertical-align: baseline` means for a
            // replaced element and why an inline image sits on the text's
            // baseline rather than centred on it.
            if let Some(box_) = run.replaced {
                out.push(Segment {
                    shaped: Shaped {
                        glyphs: Vec::new(),
                        text: String::new(),
                        width: box_.width,
                        ascent: box_.baseline,
                        height: box_.height,
                    },
                    trailing_space: 0.0,
                    mandatory_break: false,
                    align: run.style.vertical_align,
                    x: 0.0,
                    level: bidi.at(run_start),
                    space_level: bidi.at(run_start),
                    replaced: Some(box_),
                    source: run.source,
                    decoration: run.style.text_decoration,
                    font_size: run.style.font_size,
                    color: span_color(&run.style),
                    hidden: run.style.visibility == Visibility::Hidden,
                    edge: None,
                    boxes: run.boxes.clone(),
                    content,
                });
                continue;
            }

            // One side of an inline box: no glyphs and no height of its own,
            // just the room §8.4 puts on the line for it. Zero-height so that a
            // box whose padding is taller than its text does not make the line
            // taller — §10.6.1, which the fragment drawn later overflows on
            // purpose.
            if let Some(edge) = run.edge {
                // Shaped metrics, not a guess: an inline box is as tall as the
                // font it is set in whether or not any text of its own reaches
                // this line, so an empty `<span>` with a border draws the same
                // height as one holding a word. A space is shaped for its
                // metrics alone and its width thrown away.
                let metrics = self.shape_segment(" ", &run.style);
                out.push(Segment {
                    shaped: Shaped {
                        // The ascent and height only. The space's glyphs and
                        // its text would otherwise be drawn and searched: an
                        // edge is room on the line, not a character on it.
                        glyphs: Vec::new(),
                        text: String::new(),
                        width: edge.width,
                        ..metrics
                    },
                    trailing_space: 0.0,
                    mandatory_break: false,
                    align: run.style.vertical_align,
                    x: 0.0,
                    // An inline box's own side is not text and has no level of
                    // its own; it takes the one where it stands so that it
                    // travels with the words it brackets.
                    level: bidi.at(run_start),
                    space_level: bidi.at(run_start),
                    replaced: None,
                    source: run.source,
                    decoration: TextDecoration::default(),
                    font_size: run.style.font_size,
                    color: span_color(&run.style),
                    hidden: run.style.visibility == Visibility::Hidden,
                    edge: Some(edge),
                    boxes: run.boxes.clone(),
                    content,
                });
                continue;
            }
            if run.text.is_empty() {
                continue;
            }
            let preserve = run.style.white_space == WhiteSpace::Pre;
            // §16.6: `nowrap` collapses whitespace like `normal` and wraps
            // like `pre` — which is to say not at all. So the opportunities
            // are filtered rather than the segmentation rewritten: a run that
            // may not break is one long segment, and everything downstream
            // already knows what to do with a segment too wide for its line.
            let opportunities: Vec<_> = unicode_linebreak::linebreaks(&run.text)
                .filter(|(_, opportunity)| {
                    run.style.white_space != WhiteSpace::NoWrap
                        || *opportunity == unicode_linebreak::BreakOpportunity::Mandatory
                })
                .collect();
            let mut start = 0usize;
            for (at, opportunity) in opportunities {
                let piece = &run.text[start..at];
                let piece_start = start;
                start = at;
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
                    // §16.4: `word-spacing` is added to *each* space, and a
                    // preformatted run can hold a row of them.
                    self.shape_segment(&spacing, &run.style).width
                        + run.style.word_spacing * spacing.chars().count() as f32
                } else {
                    // Collapsed runs already hold at most one space.
                    self.shape_segment(" ", &run.style).width + run.style.word_spacing
                };

                if trimmed.is_empty() {
                    // Whitespace with no text of its own still advances the pen
                    // and can still carry a mandatory break, so it must not be
                    // dropped — a blank line in a <pre> is exactly this case.
                    //
                    // A second break in a row needs a segment of its own. Only
                    // one flag exists per segment, so folding it into the
                    // previous one would turn `<br><br>` into a single break —
                    // and the era's pages used exactly that pair wherever a
                    // paragraph gap was wanted.
                    let already_breaking =
                        out.last().is_some_and(|last| last.mandatory_break) && mandatory;
                    if let Some(last) = out.last_mut()
                        && !already_breaking
                    {
                        last.trailing_space += space_width;
                        last.space_level = bidi.at(run_start + piece_start);
                        last.mandatory_break |= mandatory;
                    } else if mandatory {
                        let height = self.used_line_height(&run.style);
                        out.push(Segment {
                            shaped: Shaped {
                                height,
                                ascent: run.style.font_size * 0.8,
                                ..Shaped::default()
                            },
                            trailing_space: space_width,
                            mandatory_break: true,
                            align: run.style.vertical_align,
                            x: 0.0,
                            level: bidi.at(run_start + piece_start),
                            space_level: bidi.at(run_start + piece_start),
                            replaced: None,
                            source: run.source,
                            decoration: run.style.text_decoration,
                            font_size: run.style.font_size,
                            color: span_color(&run.style),
                            hidden: run.style.visibility == Visibility::Hidden,
                            edge: None,
                            boxes: run.boxes.clone(),
                            content,
                        });
                    }
                    continue;
                }

                // Split again, at every change of embedding level. A piece
                // is the text between two line-break opportunities, and there
                // is no rule that the algorithm holds one level across it:
                // `AAA<RLO>BBB` is one unbreakable word and two directions.
                // Without this the whole word takes the level it started at
                // and the override does nothing at all.
                let pieces = level_runs(&bidi, run_start + piece_start, trimmed);
                let last_piece = pieces.len() - 1;
                for (at, (level, part)) in pieces.into_iter().enumerate() {
                    let tail = at == last_piece;
                    let shaped = self.shape_directed(part, &run.style, level);
                    out.push(Segment {
                        shaped,
                        // Only the last piece of a word carries what follows
                        // the word.
                        trailing_space: if tail { space_width } else { 0.0 },
                        mandatory_break: mandatory && tail,
                        align: run.style.vertical_align,
                        x: 0.0,
                        level,
                        space_level: bidi.at(run_start + piece_start + trimmed.len()),
                        replaced: None,
                        source: run.source,
                        decoration: run.style.text_decoration,
                        font_size: run.style.font_size,
                        color: span_color(&run.style),
                        hidden: run.style.visibility == Visibility::Hidden,
                        edge: None,
                        boxes: run.boxes.clone(),
                        content,
                    });
                }
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
            // What counts as one unbreakable piece depends on where the run is
            // allowed to break at all. Neither `pre` nor `nowrap` breaks at a
            // space, so their narrowest piece is a whole line rather than a
            // word — which is what makes a `white-space: nowrap` caption widen
            // the table under it instead of being measured by its longest word
            // and then overflowing.
            let pieces: Vec<&str> = match run.style.white_space {
                WhiteSpace::Normal => run.text.split_whitespace().collect(),
                WhiteSpace::Pre | WhiteSpace::NoWrap => run.text.split('\n').collect(),
            };
            for piece in pieces {
                let single = [InlineRun::text(piece, run.style.clone())];
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
        // A glyph this large is a resource attack rather than typography: the
        // outline rasteriser allocates a bitmap proportional to the em square,
        // so `font-size: 99999px` asks for something on the order of ten
        // billion pixels. Upstream panics rather than refusing, so the refusal
        // has to happen here — and a glyph nobody could read is no loss.
        //
        // Same shape of guard as the decompression-bomb limit on images: the
        // number that matters is the decoded size, not the source's.
        if !glyph.font_size.is_finite() || glyph.font_size > MAX_GLYPH_SIZE {
            return None;
        }
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
    use css::style::{FontStack, GenericFamily, LineHeight};

    fn style(size: f32) -> ComputedStyle {
        ComputedStyle {
            font_size: size,
            line_height: LineHeight::Px(size * 1.2),
            ..Default::default()
        }
    }

    /// A glyph reduced to what a reader could tell apart, with the floats as
    /// bits so comparing them is exact rather than approximate.
    type VisibleGlyph = (u16, u32, u32, Option<(u8, u8, u8, u8)>);
    /// A whole segment the same way: width, ascent, height, and its glyphs.
    type Visible = (u32, u32, u32, Vec<VisibleGlyph>);

    /// Everything about a shaped segment that a reader could see.
    fn visible(shaped: &Shaped) -> Visible {
        (
            shaped.width.to_bits(),
            shaped.ascent.to_bits(),
            shaped.height.to_bits(),
            shaped
                .glyphs
                .iter()
                .map(|glyph| {
                    (
                        glyph.glyph_id,
                        glyph.x.to_bits(),
                        glyph.font_size.to_bits(),
                        glyph.color,
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn two_hebrew_words_are_drawn_in_the_other_order() {
        // The case that showed the gap: each word was already shaped
        // right-to-left, and the two words were placed left to right.
        let mut fonts = FontStore::new();
        let style = ComputedStyle::default();
        let runs = [InlineRun::text(
            "\u{5d0}\u{5d1} \u{5d2}\u{5d3}",
            style.clone(),
        )];
        let laid = fonts.layout_runs(&runs, &style, 400.0);
        let line = laid.lines.first().expect("a line");

        let first = line
            .glyphs
            .iter()
            .find(|glyph| glyph.start == 0)
            .expect("the first word's glyphs");
        let second = line
            .glyphs
            .iter()
            .find(|glyph| glyph.start > 4)
            .expect("the second word's glyphs");
        assert!(
            first.x > second.x,
            "the word written first is at {} and the next at {}; the first belongs further right",
            first.x,
            second.x
        );
    }

    #[test]
    fn an_override_turns_latin_round() {
        let mut fonts = FontStore::new();
        let style = ComputedStyle::default();
        let plain = fonts.shape_directed("abc", &style, Level::ltr());
        let forced = fonts.shape_directed("abc", &style, Level::rtl());

        assert_eq!(plain.width, forced.width, "reversing must not resize");
        let ids = |shaped: &Shaped| {
            shaped
                .glyphs
                .iter()
                .map(|glyph| glyph.glyph_id)
                .collect::<Vec<_>>()
        };
        let mut backwards = ids(&plain);
        backwards.reverse();
        assert_eq!(ids(&forced), backwards);
    }

    #[test]
    fn right_to_left_script_is_left_to_the_shaper() {
        // Hebrew is already shaped right-to-left from its own characters, and
        // reversing it a second time here would put it back the way it came.
        let mut fonts = FontStore::new();
        let style = ComputedStyle::default();
        let natural = fonts.shape_segment("\u{5d0}\u{5d1}\u{5d2}", &style);
        let directed = fonts.shape_directed("\u{5d0}\u{5d1}\u{5d2}", &style, Level::rtl());

        let xs = |shaped: &Shaped| shaped.glyphs.iter().map(|g| g.x).collect::<Vec<_>>();
        assert_eq!(xs(&natural), xs(&directed));
    }

    #[test]
    fn a_bracket_in_right_to_left_text_faces_the_other_way() {
        // Rule L4. Without it `(x)` reads as `)x(` once the run has been
        // turned round, which is the sort of wrongness a reader sees at once.
        assert_eq!(mirrored("(x)"), ")x(");
        assert_eq!(mirrored("[a]{b}"), "]a[}b{");
        // Every mapping is its own inverse, so applying it twice changes
        // nothing.
        assert_eq!(
            mirrored(&mirrored("\u{00ab}q\u{00bb}")),
            "\u{00ab}q\u{00bb}"
        );
        assert_eq!(mirrored("abc123"), "abc123");
    }

    #[test]
    fn bidi_controls_are_steering_and_not_content() {
        assert_eq!(without_bidi_controls("a\u{202e}b\u{202c}c"), "abc");
        assert_eq!(without_bidi_controls("plain"), "plain");
    }

    #[test]
    fn small_caps_draws_lowercase_as_smaller_capitals() {
        let mut fonts = FontStore::new();
        let mut style = ComputedStyle {
            font_size: 100.0,
            ..ComputedStyle::default()
        };

        let plain = fonts.shape_segment("x", &style);
        let caps = fonts.shape_segment("X", &style);
        style.font_variant = FontVariant::SmallCaps;
        let small = fonts.shape_segment("x", &style);

        // It is a capital, not a lowercase letter: it is nothing like as wide
        // as the `x` it was written as.
        assert!(
            small.width > plain.width,
            "small cap {} is no wider than the lowercase {}",
            small.width,
            plain.width
        );
        // And it is a *small* one.
        assert!(
            small.width < caps.width,
            "small cap {} is not smaller than the full capital {}",
            small.width,
            caps.width
        );
        let ratio = small.width / caps.width;
        assert!(
            (ratio - SMALL_CAPS).abs() < 0.02,
            "expected about {SMALL_CAPS} of a capital, got {ratio}"
        );
    }

    #[test]
    fn a_line_reaches_below_its_baseline_by_the_struts_descent() {
        // An image alone on a line: the line is taller than the picture,
        // because §10.8.1's strut is on it whether or not there is text.
        let mut fonts = FontStore::new();
        let style = ComputedStyle::default();
        let runs = [InlineRun::replaced(
            ReplacedInline {
                id: 1,
                width: 40.0,
                height: 40.0,
                baseline: 40.0,
            },
            style.clone(),
        )];

        let laid = fonts.layout_runs_with(&runs, &style, Strut::Always, 400.0);
        assert!(
            laid.height > 40.0,
            "no room below the image: line is {} for a 40px picture",
            laid.height
        );
    }

    #[test]
    fn small_caps_leaves_capitals_and_the_line_alone() {
        let mut fonts = FontStore::new();
        let mut style = ComputedStyle {
            font_size: 40.0,
            ..ComputedStyle::default()
        };
        let plain = fonts.shape_segment("ABC", &style);
        style.font_variant = FontVariant::SmallCaps;
        let small = fonts.shape_segment("ABC", &style);

        assert_eq!(small.width, plain.width, "capitals must not shrink");

        // A word of nothing but lowercase still sits on a full-size line, or a
        // paragraph of small caps would read as one set in a smaller font.
        let lower = fonts.shape_segment("abc", &style);
        assert_eq!(lower.height, plain.height);
        assert_eq!(lower.ascent, plain.ascent);
    }

    #[test]
    fn small_caps_keeps_the_text_it_was_written_as() {
        // What a search and a selection match against is the source text, not
        // the capitals drawn in its place.
        let mut fonts = FontStore::new();
        let style = ComputedStyle {
            font_variant: FontVariant::SmallCaps,
            ..ComputedStyle::default()
        };
        let shaped = fonts.shape_segment("Word", &style);

        assert_eq!(shaped.text, "Word");
        for glyph in &shaped.glyphs {
            assert!(
                glyph.end <= shaped.text.len(),
                "glyph range {}..{} escapes {:?}",
                glyph.start,
                glyph.end,
                shaped.text
            );
        }
    }

    #[test]
    fn case_runs_split_on_the_boundary_and_nowhere_else() {
        assert_eq!(case_runs("Word"), vec![(0, "W"), (1, "ord")]);
        assert_eq!(case_runs("abc"), vec![(0, "abc")]);
        assert_eq!(case_runs(""), Vec::new());
        // Digits and spaces are not lowercase, so they join the capitals.
        assert_eq!(case_runs("A1 b"), vec![(0, "A1 "), (3, "b")]);
    }

    #[test]
    fn quirks_mode_takes_the_strut_off_a_line_with_no_text() {
        // The quirk the era's sliced-image tables were built on: the cell is
        // exactly as tall as the picture, with no descender space under it.
        let mut fonts = FontStore::new();
        let style = ComputedStyle::default();
        let image = ReplacedInline {
            id: 1,
            width: 40.0,
            height: 40.0,
            baseline: 40.0,
        };
        let runs = [InlineRun::replaced(image, style.clone())];

        let laid = fonts.layout_runs_with(&runs, &style, Strut::WhereThereIsText, 400.0);
        assert_eq!(laid.height, 40.0);
    }

    #[test]
    fn quirks_mode_keeps_the_strut_where_there_is_text() {
        // Only a line with *nothing* on it but boxes loses the strut. One word
        // beside the picture brings it back, in either mode.
        let mut fonts = FontStore::new();
        let style = ComputedStyle::default();
        let image = ReplacedInline {
            id: 1,
            width: 40.0,
            height: 40.0,
            baseline: 40.0,
        };
        let runs = [
            InlineRun::text("x", style.clone()),
            InlineRun::replaced(image, style.clone()),
        ];

        let quirks = fonts.layout_runs_with(&runs, &style, Strut::WhereThereIsText, 400.0);
        let standards = fonts.layout_runs_with(&runs, &style, Strut::Always, 400.0);
        assert_eq!(quirks.height, standards.height);
        assert!(quirks.height > 40.0, "line is only {}", quirks.height);
    }

    #[test]
    fn line_height_is_split_above_and_below_the_text() {
        // Half-leading: a paragraph set double-spaced puts the extra room
        // evenly around each line rather than hanging it underneath, which is
        // what keeps text in the middle of a table cell.
        let (ascent, descent) = FontStore::strut((14.0, 4.0), 40.0);

        assert_eq!(ascent, 25.0);
        assert_eq!(descent, 15.0);
        assert_eq!(ascent + descent, 40.0);
    }

    #[test]
    fn a_remembered_segment_is_never_handed_to_a_style_it_was_not_shaped_for() {
        // The cache is keyed on the attributes a segment was shaped under, and
        // `AttrsOwned` *is* those attributes rather than a hand-listed subset
        // of the style — so a property added to `attrs_for` is in the key by
        // construction. This is the test that would notice if that stopped
        // being true: the same text under styles differing one property at a
        // time, each asked of a store that has already shaped all the others.
        //
        // A key that could not tell two of them apart would hand back the
        // first style's glyphs for the second, and the page would be drawn in
        // somebody else's font, weight, size, or colour.
        let text = "shape me";
        let mut styles = vec![style(16.0), style(12.0), style(24.0), style(37.5)];
        // Line height alone, with the size held still: it rides in the metrics
        // `attrs_for` folds in, and nothing about the glyphs shows it.
        let mut taller = style(16.0);
        taller.line_height = LineHeight::Px(40.0);
        styles.push(taller);
        for weight in [300u16, 400, 700] {
            let mut bold = style(16.0);
            bold.font_weight = weight;
            styles.push(bold);
        }
        let mut italic = style(16.0);
        italic.font_style = FontStyle::Italic;
        styles.push(italic);
        for generic in [
            GenericFamily::SansSerif,
            GenericFamily::Serif,
            GenericFamily::Monospace,
        ] {
            let mut family = style(16.0);
            family.font_family = FontStack {
                families: Vec::new(),
                generic,
            };
            styles.push(family);
        }
        for named in ["Georgia", "Courier New", "Verdana"] {
            let mut family = style(16.0);
            family.font_family = FontStack {
                families: vec![named.to_owned()],
                generic: GenericFamily::SansSerif,
            };
            styles.push(family);
        }
        for (r, g, b) in [(255u8, 0u8, 0u8), (0, 128, 255)] {
            let mut coloured = style(16.0);
            coloured.color = css::value::Color::rgb(r, g, b);
            styles.push(coloured);
        }

        let mut shared = FontStore::new();
        // Warmed on every style first, so each lookup below is made against a
        // cache holding all the others — which is when a key that cannot tell
        // them apart returns the wrong one.
        for style in &styles {
            let _ = shared.shape_segment(text, style);
        }
        for (index, style) in styles.iter().enumerate() {
            let alone = FontStore::new().shape_segment(text, style);
            let remembered = shared.shape_segment(text, style);
            assert_eq!(
                visible(&alone),
                visible(&remembered),
                "style {index} was handed a segment shaped for another one"
            );
        }
    }

    #[test]
    fn forgetting_a_page_changes_the_speed_and_not_the_glyphs() {
        // `forget_page` exists so `tests/fuzz` can measure each input against
        // the store a renderer child would hand it. It must be a pure
        // memoisation reset: the same text, shaped before and after, has to
        // come out identical, or it would move the reference baselines
        // (ADR-0005) rather than just the clock.
        let text = "the quick brown fox";
        let style = style(16.0);

        let mut store = FontStore::new();
        let faces = store.face_count();
        let before = store.shape_segment(text, &style);

        store.forget_page();

        assert_eq!(
            store.face_count(),
            faces,
            "the faces went with the cache; only the shaping is meant to"
        );
        assert_eq!(
            visible(&before),
            visible(&store.shape_segment(text, &style)),
            "the same text shaped differently after the cache was cleared"
        );
    }

    #[test]
    fn a_saturated_cache_is_what_forgetting_is_for() {
        // The mechanism behind a fuzzer finding that could not be reproduced.
        // `MAX_SHAPED` stops inserting rather than evicting, on the stated
        // grounds that a store lives one page — so a caller that runs unrelated
        // documents through one store fills it and then caches nothing at all,
        // for the rest of its life. Filling it here and clearing it proves the
        // way out exists.
        let mut store = FontStore::new();
        for n in 0..(MAX_SHAPED + 64) {
            let _ = store.shape_segment(&format!("segment number {n}"), &style(16.0));
        }
        // Saturated: a segment never seen before cannot get in.
        assert_eq!(store.shaped.len(), MAX_SHAPED);
        let _ = store.shape_segment("a stranger", &style(16.0));
        assert_eq!(store.shaped.len(), MAX_SHAPED, "it evicted after all");

        store.forget_page();
        assert!(
            store.shaped.is_empty(),
            "the cache survived being forgotten"
        );
        let _ = store.shape_segment("a stranger", &style(16.0));
        assert_eq!(store.shaped.len(), 1, "it still cannot cache anything");
    }

    #[test]
    fn a_remembered_segment_is_actually_used() {
        // A correct cache is invisible in its output, which is the point and
        // also the problem: no comparison of what gets drawn can tell whether
        // the lookup happened at all, so removing it would break nothing any
        // other test asserts. The entry is poisoned instead — something that
        // could never have come out of the shaper is put where the answer
        // lives, and getting it back is proof the lookup is what answered.
        let mut fonts = FontStore::new();
        let ordinary = style(16.0);
        let real = fonts.shape_segment("hello", &ordinary);
        assert!(
            !real.glyphs.is_empty(),
            "the fixture has to shape to glyphs"
        );
        assert_eq!(fonts.shaped.len(), 1, "shaping remembered nothing");

        let key = fonts
            .shaped
            .keys()
            .next()
            .cloned()
            .expect("something was remembered");
        fonts.shaped.insert(
            key,
            Shaped {
                text: "hello".to_owned(),
                width: 1234.0,
                ..Shaped::default()
            },
        );
        let again = fonts.shape_segment("hello", &ordinary);
        assert_eq!(
            again.width, 1234.0,
            "the segment was shaped again rather than remembered"
        );

        // And asking a second time did not remember a second copy of it.
        assert_eq!(fonts.shaped.len(), 1);
    }

    #[test]
    fn what_a_store_remembers_is_bounded() {
        // Filled by whatever words a page contains, and a page is a
        // stranger's. Without a ceiling, a document of nothing but distinct
        // words would grow this until the renderer ran out of memory — a
        // denial of service reachable from ordinary markup.
        let mut fonts = FontStore::new();
        let ordinary = style(16.0);
        for word in 0..MAX_SHAPED + 500 {
            let _ = fonts.shape_segment(&format!("w{word}"), &ordinary);
        }
        assert!(
            fonts.shaped.len() <= MAX_SHAPED,
            "{} remembered against a limit of {MAX_SHAPED}",
            fonts.shaped.len()
        );
    }

    #[test]
    fn text_at_no_size_is_never_answered_from_the_cache() {
        // `metrics_for` floors a non-positive size to something visible, so a
        // size of zero and a nearly-zero one share a key — while the guard
        // above the cache returns early for the first and shapes the second.
        // The lookup sits below that guard for exactly this reason.
        let mut fonts = FontStore::new();
        let mut nearly = style(16.0);
        nearly.font_size = 0.005;
        let shaped = fonts.shape_segment("invisible", &nearly);
        assert!(!shaped.glyphs.is_empty(), "a nearly-zero size still shapes");

        let mut none = style(16.0);
        none.font_size = 0.0;
        let nothing = fonts.shape_segment("invisible", &none);
        assert!(
            nothing.glyphs.is_empty(),
            "zero-sized text was answered with glyphs shaped for another size"
        );
    }

    #[test]
    fn zero_sized_text_takes_no_space_instead_of_taking_the_browser_down() {
        // `font-size: 0` is legal CSS and a common one — it is how the gap
        // between inline-blocks gets closed. Our line height derives from the
        // font size, and cosmic-text asserts a line height is never zero, so
        // this panicked: a stylesheet could stop the browser. The fuzzer found
        // it in the first soak.
        let mut fonts = FontStore::new();
        let laid = fonts.layout("invisible", &style(0.0), 400.0);
        assert_eq!(laid.width, 0.0);
        assert!(
            laid.lines.iter().all(|line| line.glyphs.is_empty()),
            "zero-sized text has nothing to draw"
        );
    }

    #[test]
    fn a_line_height_of_zero_still_has_glyphs() {
        // Different declaration, different meaning: `line-height: 0` leaves the
        // text its glyphs and its width and contributes no height to the line
        // box. Flooring it the way the shaper needs must not turn into
        // flooring what the author asked for.
        let mut fonts = FontStore::new();
        let flat = ComputedStyle {
            font_size: 16.0,
            line_height: LineHeight::Px(0.0),
            ..Default::default()
        };
        let laid = fonts.layout("visible", &flat, 400.0);
        assert!(laid.width > 0.0, "the text still has width");
        assert!(
            laid.lines.iter().any(|line| !line.glyphs.is_empty()),
            "and still has glyphs"
        );
    }

    #[test]
    fn a_nonsense_font_size_does_not_propagate() {
        // A `NaN` size would otherwise reach every coordinate downstream of it,
        // where it stops being traceable to anything.
        let mut fonts = FontStore::new();
        for size in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -12.0] {
            let laid = fonts.layout("text", &style(size), 400.0);
            assert!(
                laid.width.is_finite() && laid.height.is_finite(),
                "size {size} produced {}x{}",
                laid.width,
                laid.height
            );
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
            InlineRun::text("regular ", plain.clone()),
            InlineRun::text("heavy", bold),
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
            InlineRun::text("a".to_owned(), black.clone()),
            InlineRun::text("b".to_owned(), red),
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
            InlineRun::text("the quick brown ".to_owned(), plain.clone()),
            InlineRun::text("fox jumps over the lazy dog".to_owned(), plain.clone()),
        ];
        let split = store.layout_runs(&runs, &plain, 160.0);
        let whole = store.layout("the quick brown fox jumps over the lazy dog", &plain, 160.0);
        assert_eq!(split.lines.len(), whole.lines.len());
        assert!((split.height - whole.height).abs() < 0.01);
    }

    /// The runs for `before <span>…</span> after`, where the span's two sides
    /// each take `edge` pixels and the box is numbered 1.
    fn bracketed(text: &str, edge: f32, style: &ComputedStyle) -> Vec<InlineRun> {
        let side = |opening| {
            InlineRun::edge(
                Some(7),
                InlineEdge {
                    width: edge,
                    opening,
                    rtl: false,
                },
                style.clone(),
            )
            .inside(vec![1])
        };
        vec![
            InlineRun::text("before ".to_owned(), style.clone()),
            side(true),
            InlineRun::text(text.to_owned(), style.clone()).inside(vec![1]),
            side(false),
            InlineRun::text(" after".to_owned(), style.clone()),
        ]
    }

    #[test]
    fn an_inline_box_takes_room_on_the_line_for_its_two_sides() {
        // §8.4. The bug this pins: the edges were painted and never measured,
        // so text after a padded span kept going and wrapped late.
        let mut store = FontStore::new();
        let plain = style(16.0);
        let bare = store.layout_runs(&bracketed("middle", 0.0, &plain), &plain, 1000.0);
        let padded = store.layout_runs(&bracketed("middle", 25.0, &plain), &plain, 1000.0);
        let grown = padded.lines[0].width - bare.lines[0].width;
        assert!(
            (grown - 50.0).abs() < 0.01,
            "two 25px sides grew the line by {grown}"
        );
    }

    #[test]
    fn an_inline_box_gets_one_fragment_per_line_it_crosses() {
        // §8.4 again: a box broken across lines paints one rectangle per line,
        // and its two horizontal sides belong to the whole box rather than to
        // each piece — so only the first fragment opens and only the last
        // closes.
        let mut store = FontStore::new();
        let plain = style(16.0);
        let long = "a phrase long enough that it cannot possibly fit on one line";
        let layout = store.layout_runs(&bracketed(long, 8.0, &plain), &plain, 200.0);
        assert!(layout.lines.len() > 2, "the phrase did not break");

        let fragments: Vec<_> = layout
            .lines
            .iter()
            .filter_map(|line| line.boxes.first())
            .collect();
        assert_eq!(
            fragments.len(),
            layout.lines.len(),
            "a line the box crosses drew no fragment"
        );
        assert!(
            fragments[0].opens,
            "the first fragment did not open the box"
        );
        assert!(
            fragments.last().expect("a fragment").closes,
            "the last fragment did not close it"
        );
        assert!(
            fragments[1..fragments.len() - 1]
                .iter()
                .all(|fragment| !fragment.opens && !fragment.closes),
            "a middle fragment carried a horizontal side"
        );
    }

    #[test]
    fn an_inline_boxs_fragment_covers_what_is_nested_inside_it() {
        // An outer background must run behind an inner span rather than
        // stopping either side of it: `<span class=hl>a <b>b</b> c</span>` is
        // one yellow stretch. The inner runs name the outer box too, which is
        // what a fragment is measured over.
        let mut store = FontStore::new();
        let plain = style(16.0);
        let runs = vec![
            InlineRun::edge(
                Some(1),
                InlineEdge {
                    width: 0.0,
                    opening: true,
                    rtl: false,
                },
                plain.clone(),
            )
            .inside(vec![1]),
            InlineRun::text("one ".to_owned(), plain.clone()).inside(vec![1]),
            InlineRun::text("two".to_owned(), plain.clone()).inside(vec![1, 2]),
            InlineRun::text(" three".to_owned(), plain.clone()).inside(vec![1]),
            InlineRun::edge(
                Some(1),
                InlineEdge {
                    width: 0.0,
                    opening: false,
                    rtl: false,
                },
                plain.clone(),
            )
            .inside(vec![1]),
        ];
        let layout = store.layout_runs(&runs, &plain, 1000.0);
        let line = &layout.lines[0];
        let outer = line
            .boxes
            .iter()
            .find(|box_| box_.source == 1)
            .expect("outer");
        let inner = line
            .boxes
            .iter()
            .find(|box_| box_.source == 2)
            .expect("inner");
        assert!(
            outer.x <= inner.x && outer.x + outer.width >= inner.x + inner.width,
            "the outer box {:?} did not cover the inner one {:?}",
            (outer.x, outer.width),
            (inner.x, inner.width)
        );
        assert_eq!(
            line.boxes[0].source, 1,
            "the outer box must come first so the inner paints over it"
        );
    }

    #[test]
    fn an_inline_box_is_as_tall_as_its_text_and_not_as_its_line() {
        // §10.6.1: `line-height` moves the lines apart and does not grow the
        // box. A double-spaced paragraph highlights its phrases at the size of
        // the words rather than in bands that touch.
        let mut store = FontStore::new();
        let tight = style(16.0);
        let airy = ComputedStyle {
            line_height: LineHeight::Px(40.0),
            ..style(16.0)
        };
        let mut of = |style: &ComputedStyle| {
            store
                .layout_runs(&bracketed("middle", 0.0, style), style, 1000.0)
                .lines[0]
                .boxes[0]
                .height
        };
        let tight_height = of(&tight);
        let airy_height = of(&airy);
        assert!(
            (tight_height - airy_height).abs() < 0.01,
            "line-height grew the box from {tight_height} to {airy_height}"
        );
    }

    #[test]
    fn an_inline_box_with_nothing_in_it_is_still_as_tall_as_its_font() {
        // A `<span>` holding only padding — a pill, a coloured rule — has no
        // text to measure, and measuring it as nothing would draw nothing.
        let mut store = FontStore::new();
        let plain = style(16.0);
        let runs = vec![
            InlineRun::text("before ".to_owned(), plain.clone()),
            InlineRun::edge(
                Some(1),
                InlineEdge {
                    width: 10.0,
                    opening: true,
                    rtl: false,
                },
                plain.clone(),
            )
            .inside(vec![1]),
            InlineRun::edge(
                Some(1),
                InlineEdge {
                    width: 10.0,
                    opening: false,
                    rtl: false,
                },
                plain.clone(),
            )
            .inside(vec![1]),
            InlineRun::text(" after".to_owned(), plain.clone()),
        ];
        let layout = store.layout_runs(&runs, &plain, 1000.0);
        let fragment = layout.lines[0].boxes[0];
        assert!(
            fragment.height > 10.0,
            "an empty inline box came out {} tall",
            fragment.height
        );
        assert!(
            (fragment.width - 20.0).abs() < 0.01,
            "its width is its two sides, not {}",
            fragment.width
        );
        assert!(fragment.opens && fragment.closes);
    }

    #[test]
    fn a_larger_span_makes_its_line_taller() {
        let mut store = FontStore::new();
        let small = style(12.0);
        let large = ComputedStyle {
            font_size: 30.0,
            line_height: LineHeight::Px(36.0),
            ..style(30.0)
        };
        let uniform = store.layout("small text", &small, 1000.0);
        let runs = [
            InlineRun::text("small ".to_owned(), small.clone()),
            InlineRun::text("BIG".to_owned(), large),
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
        let runs = [InlineRun::text(
            "the quick brown fox jumps over the lazy dog and keeps on running".to_owned(),
            plain.clone(),
        )];
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
        let runs = [InlineRun::text(
            "supercalifragilistic".to_owned(),
            plain.clone(),
        )];
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
            line_height: LineHeight::Px(34.0),
            ..style(28.0)
        };
        let runs = [
            InlineRun::text("small".to_owned(), small.clone()),
            InlineRun::text("BIG".to_owned(), large),
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
    #[test]
    fn word_spacing_widens_every_gap_and_nothing_else() {
        let mut store = FontStore::new();
        let plain = style(16.0);
        let spaced = ComputedStyle {
            word_spacing: 20.0,
            ..style(16.0)
        };
        let bare = store.layout("one two three", &plain, 1000.0);
        let wide = store.layout("one two three", &spaced, 1000.0);
        let grew = wide.lines[0].width - bare.lines[0].width;
        assert!(
            (grew - 40.0).abs() < 0.01,
            "two gaps at 20px grew the line by {grew}"
        );
    }

    #[test]
    fn justify_fills_every_line_but_the_last() {
        // §16.2. The last line of a paragraph keeps its natural width, which is
        // the difference between justified text and a page of stretched
        // fragments.
        let mut store = FontStore::new();
        let justified = ComputedStyle {
            text_align: css::style::TextAlign::Justify,
            ..style(16.0)
        };
        let text = "The quick brown fox jumps over the lazy dog and keeps on running";
        let layout = store.layout(text, &justified, 200.0);
        assert!(layout.lines.len() > 2, "the text did not wrap enough");

        for (index, line) in layout.lines.iter().enumerate() {
            if index + 1 == layout.lines.len() {
                assert!(
                    line.width < 200.0 - 1.0,
                    "the last line was stretched to {}",
                    line.width
                );
            } else {
                assert!(
                    (line.width - 200.0).abs() < 0.01,
                    "line {index} came to {} rather than filling 200",
                    line.width
                );
            }
        }

        // And left alignment leaves them all ragged.
        let ragged = store.layout(text, &style(16.0), 200.0);
        assert!(
            ragged.lines.iter().any(|line| line.width < 199.0),
            "unjustified text filled every line exactly"
        );
    }
}

#[cfg(test)]
mod decoration_tests {
    use super::*;
    use css::style::{LineHeight, TextDecoration};

    fn run(text: &str, decoration: TextDecoration) -> InlineRun {
        InlineRun::text(
            text,
            ComputedStyle {
                font_size: 16.0,
                line_height: LineHeight::Px(19.2),
                text_decoration: decoration,
                ..Default::default()
            },
        )
    }

    const UNDERLINE: TextDecoration = TextDecoration {
        underline: true,
        line_through: false,
        overline: false,
    };

    #[test]
    fn undecorated_text_produces_no_rules() {
        let mut store = FontStore::new();
        let layout = store.layout("plain text", &ComputedStyle::default(), 1000.0);
        assert!(layout.lines[0].decorations.is_empty());
    }

    #[test]
    fn a_rule_spans_the_text_it_decorates() {
        let mut store = FontStore::new();
        let layout = store.layout_runs(
            &[run("underlined", UNDERLINE)],
            &ComputedStyle::default(),
            1000.0,
        );
        let rules = &layout.lines[0].decorations;
        assert_eq!(rules.len(), 1);
        assert!(rules[0].thickness >= 1.0);
        assert!(
            (rules[0].width - layout.lines[0].width).abs() < 1.0,
            "rule {} should span the line's {}",
            rules[0].width,
            layout.lines[0].width
        );
        assert!(
            rules[0].y > layout.lines[0].baseline,
            "an underline sits below the baseline"
        );
    }

    #[test]
    fn a_rule_carries_across_the_space_between_two_decorated_runs() {
        // Two spans of one link. Emitting a rule per run would leave a gap
        // under the space between them, which is not how an underline looks.
        let mut store = FontStore::new();
        let layout = store.layout_runs(
            &[run("one ", UNDERLINE), run("two", UNDERLINE)],
            &ComputedStyle::default(),
            1000.0,
        );
        let rules = &layout.lines[0].decorations;
        assert_eq!(rules.len(), 1, "the two runs share one rule");
        assert!((rules[0].width - layout.lines[0].width).abs() < 1.0);
    }

    #[test]
    fn an_undecorated_run_between_two_decorated_ones_breaks_the_rule() {
        let mut store = FontStore::new();
        let layout = store.layout_runs(
            &[
                run("one ", UNDERLINE),
                run("two ", TextDecoration::default()),
                run("three", UNDERLINE),
            ],
            &ComputedStyle::default(),
            1000.0,
        );
        let rules = &layout.lines[0].decorations;
        assert_eq!(rules.len(), 2);
        assert!(
            rules[0].x + rules[0].width < rules[1].x,
            "the rules must not meet across the undecorated run"
        );
    }

    #[test]
    fn each_wrapped_line_gets_its_own_rule() {
        let mut store = FontStore::new();
        let text = "a fairly long stretch of underlined text that has to wrap";
        let layout = store.layout_runs(&[run(text, UNDERLINE)], &ComputedStyle::default(), 120.0);
        assert!(layout.lines.len() > 1, "the text must actually wrap");
        for (index, line) in layout.lines.iter().enumerate() {
            assert_eq!(line.decorations.len(), 1, "line {index} has no rule");
        }
    }
}

#[cfg(test)]
mod break_tests {
    use super::*;

    fn pre(text: &str) -> InlineRun {
        InlineRun::text(
            text,
            ComputedStyle {
                white_space: WhiteSpace::Pre,
                ..Default::default()
            },
        )
    }

    fn plain(text: &str) -> InlineRun {
        InlineRun::text(text.to_owned(), ComputedStyle::default())
    }

    #[test]
    fn a_forced_break_starts_a_new_line() {
        let mut store = FontStore::new();
        let layout = store.layout_runs(
            &[plain("one"), pre("\n"), plain("two")],
            &ComputedStyle::default(),
            1000.0,
        );
        assert_eq!(layout.lines.len(), 2);
    }

    #[test]
    fn two_forced_breaks_leave_a_blank_line() {
        // `<br><br>` was how the era's pages spaced paragraphs. Folding the
        // second break into the first gives one break, not two, and the gap
        // the author asked for disappears.
        let mut store = FontStore::new();
        let layout = store.layout_runs(
            &[plain("one"), pre("\n"), pre("\n"), plain("two")],
            &ComputedStyle::default(),
            1000.0,
        );
        assert_eq!(layout.lines.len(), 3, "one blank line between the two");
        assert!(
            layout.lines[1].glyphs.is_empty(),
            "the middle line is the blank one"
        );
    }

    #[test]
    fn a_run_boundary_alone_does_not_break_a_line() {
        // The break algorithm reports Mandatory at end of text as well as at a
        // real break, so `<b>one</b> <i>two</i>` would otherwise wrap.
        let mut store = FontStore::new();
        let layout = store.layout_runs(
            &[plain("one "), plain("two")],
            &ComputedStyle::default(),
            1000.0,
        );
        assert_eq!(layout.lines.len(), 1);
    }

    #[test]
    fn blank_lines_in_preformatted_text_survive() {
        let mut store = FontStore::new();
        let layout = store.layout_runs(&[pre("a\n\nb")], &ComputedStyle::default(), 1000.0);
        assert_eq!(layout.lines.len(), 3);
    }
}
