//! Table layout.
//!
//! The 2000s web laid out with tables (ADR-0004), so this is load-bearing
//! rather than a compatibility footnote.
//!
//! Implements CSS 2.1's *automatic* table layout in both border models:
//! column widths come from cell content, and the table is only as wide as it
//! needs to be unless a width is declared. Fixed layout (`table-layout: fixed`)
//! is not here yet.
//!
//! The two border models are genuinely different geometries rather than two
//! ways of drawing the same one, which is why [`Collapsed`] exists at all. In
//! the separated model each cell owns its four borders and `border-spacing`
//! sits between them. In the collapsing model there are no cell borders: there
//! are *grid lines*, each carrying one border resolved from everything that
//! touches it (§17.6.2.1), drawn centred on the line so half of it falls into
//! the cell on either side.

use css::style::{BorderStyle, ComputedStyle, Display};
use css::value::Color;
use dom::{Document, NodeId};

/// Largest span honoured on a `colspan` or `rowspan` attribute.
///
/// The attributes are unbounded in the markup and a mistyped one — or a
/// deliberately hostile `rowspan="99999999"` — would otherwise size an
/// allocation from untrusted input.
const MAX_SPAN: usize = 1000;

/// One cell in the grid.
#[derive(Debug, Clone)]
pub struct Cell {
    /// The cell element.
    pub node: NodeId,
    /// Its computed style.
    pub style: ComputedStyle,
    /// Columns spanned, at least 1.
    pub colspan: usize,
    /// Rows spanned, at least 1.
    pub rowspan: usize,
    /// Index of the first column this cell occupies.
    pub column: usize,
}

/// One row: the `tr` itself, plus its cells.
///
/// The row is kept rather than flattened away because it can carry a
/// background of its own — `<tr bgcolor>` striping is how the era's tables
/// were made readable — and that background paints behind the whole row, not
/// behind each cell.
#[derive(Debug, Clone)]
pub struct Row {
    /// The `tr` element.
    pub node: NodeId,
    /// Its computed style.
    pub style: ComputedStyle,
    /// Cells in document order.
    pub cells: Vec<Cell>,
    /// Index into [`Grid::row_groups`] of the `thead`, `tbody` or `tfoot` this
    /// row sits in, where there is one.
    pub group: Option<usize>,
}

/// A `thead`, `tbody` or `tfoot`, and the rows it covers.
///
/// Kept only because the collapsing model needs it: a row group is one of the
/// six things §17.6.2.1 lets contribute a border, and its top and bottom edges
/// are grid lines that no row or cell can speak for. In the separated model it
/// is transparent, which is why the rows are still flattened.
#[derive(Debug, Clone)]
pub struct RowBand {
    /// Its computed style.
    pub style: ComputedStyle,
    /// Index of its first row in [`Grid::rows`].
    pub first: usize,
    /// One past its last row. Equal to `first` when the group held no rows.
    pub end: usize,
}

/// A `col` or `colgroup`, and the columns it covers.
#[derive(Debug, Clone)]
pub struct ColumnBand {
    /// Its computed style.
    pub style: ComputedStyle,
    /// First column covered.
    pub start: usize,
    /// One past the last column covered.
    pub end: usize,
}

/// A table flattened into rows of cells.
#[derive(Debug, Clone, Default)]
pub struct Grid {
    /// Rows in document order.
    pub rows: Vec<Row>,
    /// Number of columns, accounting for spans.
    pub columns: usize,
    /// Row groups in document order, referenced by [`Row::group`].
    pub row_groups: Vec<RowBand>,
    /// `col` elements, in document order.
    pub columns_declared: Vec<ColumnBand>,
    /// `colgroup` elements, in document order.
    pub column_groups: Vec<ColumnBand>,
}

impl Grid {
    /// Which cell occupies each `(row, column)` slot, as an index into
    /// `rows[r].cells`.
    ///
    /// Spans are expanded, so a `rowspan="3"` cell appears in three rows. That
    /// is the whole point: the collapsing model asks "what is on either side of
    /// this grid line segment", and a spanning cell is on the side of several
    /// of them.
    pub fn occupancy(&self) -> Vec<Vec<Option<(usize, usize)>>> {
        let mut map = vec![vec![None; self.columns]; self.rows.len()];
        for (index, row) in self.rows.iter().enumerate() {
            for (position, cell) in row.cells.iter().enumerate() {
                let last_row = (index + cell.rowspan).min(self.rows.len());
                let last_column = (cell.column + cell.colspan).min(self.columns);
                for slot in map.iter_mut().take(last_row).skip(index) {
                    for entry in slot.iter_mut().take(last_column).skip(cell.column) {
                        *entry = Some((index, position));
                    }
                }
            }
        }
        map
    }
}

/// Reads a table subtree into a grid, descending through row groups.
///
/// `thead`, `tbody`, and `tfoot` are transparent here: the parser inserts a
/// `tbody` whether or not the author wrote one, so rows are almost never direct
/// children of the table.
pub fn build_grid(doc: &Document, styles: &css::cascade::StyleMap, table: NodeId) -> Grid {
    let mut grid = Grid::default();
    // How many further rows each column is still occupied by a cell spanning
    // down from above. Without this a `rowspan` cell's column is handed to the
    // next row's first cell, and every row below it shifts left.
    let mut occupied: Vec<usize> = Vec::new();
    collect_rows(doc, styles, table, None, &mut occupied, &mut grid);
    // The rightmost column any cell reaches, not the widest row: with row
    // spanning a row's own cells no longer cover every column.
    grid.columns = grid
        .rows
        .iter()
        .flat_map(|row| row.cells.iter())
        .map(|cell| cell.column + cell.colspan)
        .max()
        .unwrap_or(0);
    collect_columns(doc, styles, table, &mut grid);
    grid
}

/// A table's caption children, in document order, with their styles.
///
/// Found by `display`, as the rows and the column bands are. The UA sheet is
/// what gives `<caption>` its display, so era markup still works and a
/// `<div style="display: table-caption">` works too.
///
/// Only direct children. A caption deeper in the subtree belongs to a nested
/// table, and stealing it would move somebody else's heading.
pub fn captions(
    doc: &Document,
    styles: &css::cascade::StyleMap,
    table: NodeId,
) -> Vec<(NodeId, ComputedStyle)> {
    doc.children(table)
        .iter()
        .filter_map(|&child| {
            let style = styles.get(child)?;
            (style.display == Display::TableCaption).then(|| (child, style.clone()))
        })
        .collect()
}

/// Reads `col` and `colgroup` elements into column bands.
///
/// They contribute nothing to the separated model — this engine sizes columns
/// from cell content, and a `<col width>` already reaches the cascade through
/// the presentational attributes — but §17.6.2.1 lets both offer a border, and
/// a band cannot be recovered from the cells once it is thrown away.
fn collect_columns(
    doc: &Document,
    styles: &css::cascade::StyleMap,
    table: NodeId,
    grid: &mut Grid,
) {
    let span_of = |element: &dom::ElementData| {
        element
            .attr("span")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, MAX_SPAN)
    };
    let mut column = 0;
    for &child in doc.children(table) {
        let Some(element) = doc.element(child) else {
            continue;
        };
        let Some(style) = styles.get(child) else {
            continue;
        };
        match style.display {
            Display::TableColumn => {
                let span = span_of(element);
                grid.columns_declared.push(ColumnBand {
                    style: style.clone(),
                    start: column,
                    end: column + span,
                });
                column += span;
            }
            Display::TableColumnGroup => {
                let start = column;
                // A group's own `span` counts only when it has no column
                // children; with them, the children decide how wide it is.
                let mut has_children = false;
                for &inner in doc.children(child) {
                    let Some(col) = doc.element(inner) else {
                        continue;
                    };
                    let Some(col_style) = styles.get(inner) else {
                        continue;
                    };
                    if col_style.display != Display::TableColumn {
                        continue;
                    }
                    has_children = true;
                    let span = span_of(col);
                    grid.columns_declared.push(ColumnBand {
                        style: col_style.clone(),
                        start: column,
                        end: column + span,
                    });
                    column += span;
                }
                if !has_children {
                    column += span_of(element);
                }
                grid.column_groups.push(ColumnBand {
                    style: style.clone(),
                    start,
                    end: column,
                });
            }
            _ => {}
        }
    }
}

fn collect_rows(
    doc: &Document,
    styles: &css::cascade::StyleMap,
    node: NodeId,
    group: Option<usize>,
    occupied: &mut Vec<usize>,
    grid: &mut Grid,
) {
    for &child in doc.children(node) {
        let Some(style) = styles.get(child) else {
            continue;
        };
        // A float or an absolutely positioned box is out of the table's flow —
        // §9.7 has already made it a block — so neither it nor anything under
        // it is part of this grid. Without this a floated row group's rows are
        // collected into the table they were taken out of, and then laid out
        // twice.
        if style.float != css::style::Float::None || style.position.is_out_of_flow() {
            continue;
        }
        match style.display {
            Display::None => {}
            Display::TableRow => {
                let mut cells = Vec::new();
                let mut column = 0;
                for &cell_node in doc.children(child) {
                    // Step over columns a cell from an earlier row still holds.
                    while occupied.get(column).is_some_and(|rows| *rows > 0) {
                        column += 1;
                    }
                    let Some(cell_element) = doc.element(cell_node) else {
                        continue;
                    };
                    let Some(cell_style) = styles.get(cell_node) else {
                        continue;
                    };
                    if cell_style.display != Display::TableCell {
                        continue;
                    }
                    let span = |name: &str| {
                        cell_element
                            .attr(name)
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, MAX_SPAN)
                    };
                    let colspan = span("colspan");
                    let rowspan = span("rowspan");

                    if occupied.len() < column + colspan {
                        occupied.resize(column + colspan, 0);
                    }
                    for slot in &mut occupied[column..column + colspan] {
                        // This row included, so a rowspan of 1 leaves nothing
                        // behind once the row is done.
                        *slot = rowspan;
                    }

                    cells.push(Cell {
                        node: cell_node,
                        style: cell_style.clone(),
                        colspan,
                        rowspan,
                        column,
                    });
                    column += colspan;
                }

                // The row is finished, so every occupancy count owes one fewer
                // row from here on.
                for slot in occupied.iter_mut() {
                    *slot = slot.saturating_sub(1);
                }
                if !cells.is_empty() {
                    grid.rows.push(Row {
                        node: child,
                        style: style.clone(),
                        cells,
                        group,
                    });
                }
            }
            // A row group is still descended through — its rows are flattened
            // into the grid exactly as before — but it is recorded on the way
            // past, because the collapsing model needs its borders and its
            // first and last rows.
            Display::TableRowGroup => {
                let band = grid.row_groups.len();
                let first = grid.rows.len();
                grid.row_groups.push(RowBand {
                    style: style.clone(),
                    first,
                    end: first,
                });
                collect_rows(doc, styles, child, Some(band), occupied, grid);
                // Set once the rows are in. A group that held none keeps an
                // empty range, which no row points at and nothing reads.
                grid.row_groups[band].end = grid.rows.len();
            }
            // A caption or a column band is the table's business, not a row's:
            // descending into one would read its contents as rows.
            Display::TableCaption | Display::TableColumn | Display::TableColumnGroup => {}
            // Any other wrapper is transparent, and a row inside one keeps the
            // group it is nested in. §17.2.1 would make an anonymous row box
            // here instead; flattening is what this engine does until it does.
            _ => collect_rows(doc, styles, child, group, occupied, grid),
        }
    }
}

/// Which edge of a box a candidate border came from.
///
/// Kept because it decides how the border is *drawn*, not only how wide it is:
/// `inset`, `outset`, `groove` and `ridge` are lit from above and to the left,
/// so a cell's `border-bottom` and the cell below it's `border-top` are two
/// different pictures of the same width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderEdge {
    /// The box's top edge.
    Top,
    /// Its right edge.
    Right,
    /// Its bottom edge.
    Bottom,
    /// Its left edge.
    Left,
}

/// What kind of box offered a border, for §17.6.2.1's last tie-break.
///
/// Declared in precedence order — a cell beats a row, a row beats a row group,
/// and so on down — so `Ord` is the rule rather than a restatement of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BorderOrigin {
    /// A `td` or `th`.
    Cell,
    /// A `tr`.
    Row,
    /// A `thead`, `tbody` or `tfoot`.
    RowGroup,
    /// A `col`.
    Column,
    /// A `colgroup`.
    ColumnGroup,
    /// The `table` itself.
    Table,
}

/// One border offered to a grid line, before the conflict is resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    /// Used width in pixels — zero when the style reserves no space.
    pub width: f32,
    /// Line style.
    pub style: BorderStyle,
    /// Line colour, already defaulted to the contributing box's `color`.
    pub color: Color,
    /// Which edge of that box it came from.
    pub edge: BorderEdge,
    /// What kind of box it came from.
    pub origin: BorderOrigin,
}

/// The border this box offers on one of its edges.
fn candidate(style: &ComputedStyle, edge: BorderEdge, origin: BorderOrigin) -> Candidate {
    let side = match edge {
        BorderEdge::Top => &style.border.top,
        BorderEdge::Right => &style.border.right,
        BorderEdge::Bottom => &style.border.bottom,
        BorderEdge::Left => &style.border.left,
    };
    Candidate {
        width: side.used_width(style.font_size),
        style: side.style,
        // `border-color` defaults to the element's own `color`, and the element
        // is gone by the time this border is drawn — so it is resolved here,
        // while there is still something to ask.
        color: side.color.unwrap_or(style.color),
        edge,
        origin,
    }
}

/// Where a style sits in §17.6.2.1's ordering, lowest first.
///
/// `double` over `solid` is the surprising one and it is not arbitrary: the
/// list runs from the styles that look most deliberate to the ones a browser
/// might have supplied itself.
fn style_precedence(style: BorderStyle) -> u8 {
    match style {
        BorderStyle::Double => 0,
        BorderStyle::Solid => 1,
        BorderStyle::Dashed => 2,
        BorderStyle::Dotted => 3,
        BorderStyle::Ridge => 4,
        BorderStyle::Outset => 5,
        BorderStyle::Groove => 6,
        BorderStyle::Inset => 7,
        // Neither reaches the comparison: `hidden` has already won outright and
        // `none` has already been discarded.
        BorderStyle::None | BorderStyle::Hidden => 8,
    }
}

/// Resolves a border conflict, per CSS 2.1 §17.6.2.1.
///
/// `None` means nothing is drawn on this grid line and it takes no space —
/// either something asked for `hidden`, or every candidate was `none`.
///
/// The rules, in order: `hidden` beats everything; `none` loses to everything;
/// then the widest wins; then the style order `double > solid > dashed >
/// dotted > ridge > outset > groove > inset`; then the origin order `cell >
/// row > row group > column > column group > table`.
///
/// The sixth rule — two boxes of the same kind, where the one further to the
/// left and further to the top wins — is carried by the order of `candidates`
/// rather than by a comparison here, because "further to the left" is not
/// something a border knows about itself. Callers push the upper and the
/// left-hand contributor first, and a tie keeps the one already held.
pub fn resolve_conflict(candidates: &[Candidate]) -> Option<Candidate> {
    // Checked before anything else and over the whole set: `hidden` on any one
    // contributor suppresses the border however wide or emphatic the others
    // are. This is the rule that lets a page punch a hole in a grid.
    if candidates
        .iter()
        .any(|border| border.style == BorderStyle::Hidden)
    {
        return None;
    }
    candidates
        .iter()
        .filter(|border| border.style != BorderStyle::None)
        .copied()
        .reduce(|best, next| if wins_over(next, best) { next } else { best })
}

/// Whether `next` beats the best candidate so far. Ties keep `best`.
fn wins_over(next: Candidate, best: Candidate) -> bool {
    if next.width > best.width {
        return true;
    }
    if next.width < best.width {
        return false;
    }
    let (challenger, holder) = (style_precedence(next.style), style_precedence(best.style));
    if challenger != holder {
        return challenger < holder;
    }
    next.origin < best.origin
}

/// A table's borders, resolved into one per grid line segment (§17.6.2).
///
/// The grid lines run *between* the cells: `columns + 1` vertical ones and
/// `rows + 1` horizontal ones. Each is divided into segments — one per row for
/// a vertical line, one per column for a horizontal one — and each segment
/// carries its own resolved border, because a `rowspan` cell can face two
/// different neighbours down one edge.
#[derive(Debug, Clone)]
pub struct Collapsed {
    /// The grid these borders belong to.
    pub grid: Grid,
    /// `vertical[line][row]`, with `line` in `0..=columns`.
    pub vertical: Vec<Vec<Option<Candidate>>>,
    /// `horizontal[line][column]`, with `line` in `0..=rows`.
    pub horizontal: Vec<Vec<Option<Candidate>>>,
    /// The widest border on each vertical grid line.
    ///
    /// The geometry uses this rather than the per-segment width, because a
    /// column edge is one straight line: cells on both sides of it are inset
    /// by half of the widest border anywhere along it, or a table whose first
    /// row has a thick border and whose second has a thin one would have a
    /// ragged column.
    pub vertical_widths: Vec<f32>,
    /// The same for each horizontal grid line.
    pub horizontal_widths: Vec<f32>,
}

/// Resolves every grid line of `table`, which must be a collapsing table.
pub fn collapse_borders(
    doc: &Document,
    styles: &css::cascade::StyleMap,
    node: NodeId,
    style: &ComputedStyle,
) -> Collapsed {
    let grid = build_grid(doc, styles, node);
    let (rows, columns) = (grid.rows.len(), grid.columns);
    let occupancy = grid.occupancy();

    // The cell occupying a slot, or `None` past the edge of the table.
    let cell_at = |row: usize, column: usize| -> Option<(usize, usize)> {
        occupancy.get(row)?.get(column).copied().flatten()
    };
    let cell_style = |slot: (usize, usize)| &grid.rows[slot.0].cells[slot.1].style;
    let band_covering = |bands: &[ColumnBand], column: usize| -> Option<usize> {
        bands
            .iter()
            .position(|band| band.start <= column && column < band.end)
    };

    let mut horizontal = Vec::with_capacity(rows + 1);
    for line in 0..=rows {
        let mut segments = Vec::with_capacity(columns);
        for column in 0..columns {
            let mut candidates = Vec::new();
            let above = (line > 0).then(|| cell_at(line - 1, column)).flatten();
            let below = (line < rows).then(|| cell_at(line, column)).flatten();
            // A cell spanning this line has no border in the middle of itself,
            // so neither side contributes — the grid line runs through it.
            if above != below {
                // Upper contributor first, so §17.6.2.1's "further to the top
                // wins" falls out of the tie keeping what it already holds.
                if let Some(slot) = above {
                    candidates.push(candidate(
                        cell_style(slot),
                        BorderEdge::Bottom,
                        BorderOrigin::Cell,
                    ));
                }
                if let Some(slot) = below {
                    candidates.push(candidate(
                        cell_style(slot),
                        BorderEdge::Top,
                        BorderOrigin::Cell,
                    ));
                }
            }
            if line > 0 {
                candidates.push(candidate(
                    &grid.rows[line - 1].style,
                    BorderEdge::Bottom,
                    BorderOrigin::Row,
                ));
            }
            if line < rows {
                candidates.push(candidate(
                    &grid.rows[line].style,
                    BorderEdge::Top,
                    BorderOrigin::Row,
                ));
            }
            // A row group's own edges, which are grid lines no row speaks for:
            // the bottom of a `thead` is not the bottom of the table.
            if let Some(band) = (line > 0)
                .then(|| grid.rows[line - 1].group)
                .flatten()
                .filter(|band| grid.row_groups[*band].end == line)
            {
                candidates.push(candidate(
                    &grid.row_groups[band].style,
                    BorderEdge::Bottom,
                    BorderOrigin::RowGroup,
                ));
            }
            if let Some(band) = (line < rows)
                .then(|| grid.rows[line].group)
                .flatten()
                .filter(|band| grid.row_groups[*band].first == line)
            {
                candidates.push(candidate(
                    &grid.row_groups[band].style,
                    BorderEdge::Top,
                    BorderOrigin::RowGroup,
                ));
            }
            // A column runs the whole height of the table, so its horizontal
            // edges are the table's own top and bottom and nowhere else.
            if line == 0 || line == rows {
                let edge = if line == 0 {
                    BorderEdge::Top
                } else {
                    BorderEdge::Bottom
                };
                if let Some(band) = band_covering(&grid.columns_declared, column) {
                    candidates.push(candidate(
                        &grid.columns_declared[band].style,
                        edge,
                        BorderOrigin::Column,
                    ));
                }
                if let Some(band) = band_covering(&grid.column_groups, column) {
                    candidates.push(candidate(
                        &grid.column_groups[band].style,
                        edge,
                        BorderOrigin::ColumnGroup,
                    ));
                }
                candidates.push(candidate(style, edge, BorderOrigin::Table));
            }
            segments.push(resolve_conflict(&candidates));
        }
        horizontal.push(segments);
    }

    let mut vertical = Vec::with_capacity(columns + 1);
    for line in 0..=columns {
        let mut segments = Vec::with_capacity(rows);
        for row in 0..rows {
            let mut candidates = Vec::new();
            let left = (line > 0).then(|| cell_at(row, line - 1)).flatten();
            let right = (line < columns).then(|| cell_at(row, line)).flatten();
            if left != right {
                // Left-hand contributor first, for the same reason.
                if let Some(slot) = left {
                    candidates.push(candidate(
                        cell_style(slot),
                        BorderEdge::Right,
                        BorderOrigin::Cell,
                    ));
                }
                if let Some(slot) = right {
                    candidates.push(candidate(
                        cell_style(slot),
                        BorderEdge::Left,
                        BorderOrigin::Cell,
                    ));
                }
            }
            // A column's vertical edges are its own two sides; the boundaries
            // *inside* a `<col span="3">` are not edges of anything.
            if line > 0 {
                if let Some(band) = band_covering(&grid.columns_declared, line - 1)
                    .filter(|band| grid.columns_declared[*band].end == line)
                {
                    candidates.push(candidate(
                        &grid.columns_declared[band].style,
                        BorderEdge::Right,
                        BorderOrigin::Column,
                    ));
                }
                if let Some(band) = band_covering(&grid.column_groups, line - 1)
                    .filter(|band| grid.column_groups[*band].end == line)
                {
                    candidates.push(candidate(
                        &grid.column_groups[band].style,
                        BorderEdge::Right,
                        BorderOrigin::ColumnGroup,
                    ));
                }
            }
            if line < columns {
                if let Some(band) = band_covering(&grid.columns_declared, line)
                    .filter(|band| grid.columns_declared[*band].start == line)
                {
                    candidates.push(candidate(
                        &grid.columns_declared[band].style,
                        BorderEdge::Left,
                        BorderOrigin::Column,
                    ));
                }
                if let Some(band) = band_covering(&grid.column_groups, line)
                    .filter(|band| grid.column_groups[*band].start == line)
                {
                    candidates.push(candidate(
                        &grid.column_groups[band].style,
                        BorderEdge::Left,
                        BorderOrigin::ColumnGroup,
                    ));
                }
            }
            // A row spans the table's whole width, so its left and right edges
            // are the table's — as are its group's, and the table's own.
            if line == 0 || line == columns {
                let edge = if line == 0 {
                    BorderEdge::Left
                } else {
                    BorderEdge::Right
                };
                candidates.push(candidate(&grid.rows[row].style, edge, BorderOrigin::Row));
                if let Some(band) = grid.rows[row].group {
                    candidates.push(candidate(
                        &grid.row_groups[band].style,
                        edge,
                        BorderOrigin::RowGroup,
                    ));
                }
                candidates.push(candidate(style, edge, BorderOrigin::Table));
            }
            segments.push(resolve_conflict(&candidates));
        }
        vertical.push(segments);
    }

    let widest = |segments: &Vec<Option<Candidate>>| {
        segments
            .iter()
            .flatten()
            .map(|border| border.width)
            .fold(0.0f32, f32::max)
    };
    let vertical_widths = vertical.iter().map(widest).collect();
    let horizontal_widths = horizontal.iter().map(widest).collect();

    Collapsed {
        grid,
        vertical,
        horizontal,
        vertical_widths,
        horizontal_widths,
    }
}

impl Collapsed {
    /// Half the width of vertical grid line `line`, which is how far it reaches
    /// into the cell on either side.
    pub fn half_vertical(&self, line: usize) -> f32 {
        self.vertical_widths.get(line).copied().unwrap_or(0.0) / 2.0
    }

    /// Half the width of horizontal grid line `line`.
    pub fn half_horizontal(&self, line: usize) -> f32 {
        self.horizontal_widths.get(line).copied().unwrap_or(0.0) / 2.0
    }

    /// The borders a box occupying these grid lines reserves space for, as
    /// `(top, right, bottom, left)`.
    ///
    /// Half-widths, and [`BorderStyle::Hidden`] rather than the resolved style:
    /// the space has to be reserved so the content sits where it should, but
    /// the border itself is drawn once, by the table, centred on the grid line
    /// — not twice, half by each neighbour, which is what asking the cells to
    /// draw it would mean.
    pub fn reserved(&self, rows: (usize, usize), columns: (usize, usize)) -> (f32, f32, f32, f32) {
        (
            self.half_horizontal(rows.0),
            self.half_vertical(columns.1),
            self.half_horizontal(rows.1),
            self.half_vertical(columns.0),
        )
    }
}

/// Rewrites a style so its borders reserve `(top, right, bottom, left)` and
/// paint nothing, and it has no padding if `drop_padding`.
///
/// The collapsing model gives a table no padding at all (§17.6.2), which is
/// separate from the cells: a cell keeps its padding, and only the table loses
/// its own.
pub fn with_reserved_borders(
    style: &ComputedStyle,
    reserved: (f32, f32, f32, f32),
    drop_padding: bool,
) -> ComputedStyle {
    use css::style::{BorderSide, Edges};
    use css::value::Length;

    let side = |width: f32| BorderSide {
        width: Length::Px(width),
        // Reserves space, paints nothing — exactly what is wanted, and already
        // what `hidden` means everywhere else in the engine.
        style: BorderStyle::Hidden,
        color: None,
    };
    let mut out = style.clone();
    out.border.top = side(reserved.0);
    out.border.right = side(reserved.1);
    out.border.bottom = side(reserved.2);
    out.border.left = side(reserved.3);
    if drop_padding {
        out.padding = Edges::ZERO;
    }
    out
}

/// Distributes `available` width across columns given their intrinsic widths.
///
/// This is the heart of automatic table layout. Below the minimum the table
/// overflows rather than shredding words; above the maximum the surplus is
/// shared out so the table fills its container; in between, each column grows
/// from its minimum in proportion to how much room it actually wants.
pub fn distribute_widths(mins: &[f32], maxes: &[f32], available: Option<f32>) -> Vec<f32> {
    let total_min: f32 = mins.iter().sum();
    let total_max: f32 = maxes.iter().sum();

    let Some(available) = available else {
        return maxes.to_vec();
    };

    if total_max <= available {
        // Content fits. A table with no declared width stays at its maximum
        // rather than stretching, which is what CSS 2.1 specifies and what
        // makes narrow tables look right.
        return maxes.to_vec();
    }
    if total_min >= available {
        return mins.to_vec();
    }

    let slack = available - total_min;
    let growth: f32 = total_max - total_min;
    mins.iter()
        .zip(maxes)
        .map(|(min, max)| {
            let share = if growth > 0.0 {
                (max - min) / growth
            } else {
                0.0
            };
            min + slack * share
        })
        .collect()
}

/// Spreads a spanning cell's intrinsic width across the columns it covers.
///
/// A `colspan` cell constrains its columns jointly, not individually. Charging
/// its full width to each column would inflate every one of them; ignoring it
/// entirely lets a wide spanning cell overflow. Splitting the shortfall evenly
/// is the standard compromise.
pub fn apply_span(widths: &mut [f32], column: usize, colspan: usize, wanted: f32, spacing: f32) {
    let end = (column + colspan).min(widths.len());
    if column >= end {
        return;
    }
    let covered: f32 = widths[column..end].iter().sum();
    let between = spacing * (end - column - 1) as f32;
    if covered + between >= wanted {
        return;
    }
    let shortfall = (wanted - covered - between) / (end - column) as f32;
    for width in &mut widths[column..end] {
        *width += shortfall;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_take_their_maximum_when_it_fits() {
        let widths = distribute_widths(&[10.0, 20.0], &[50.0, 60.0], Some(500.0));
        assert_eq!(
            widths,
            vec![50.0, 60.0],
            "a table that fits does not stretch"
        );
    }

    #[test]
    fn columns_fall_back_to_their_minimum_when_squeezed() {
        let widths = distribute_widths(&[40.0, 60.0], &[200.0, 300.0], Some(50.0));
        assert_eq!(
            widths,
            vec![40.0, 60.0],
            "below the minimum the table overflows"
        );
    }

    #[test]
    fn intermediate_widths_grow_in_proportion_to_demand() {
        // Growth is shared in proportion to `max - min`, not to `max`: the
        // first column wants 50 more, the second wants 150, so of 100px of
        // slack they take 25 and 75.
        let widths = distribute_widths(&[50.0, 50.0], &[100.0, 200.0], Some(200.0));
        assert!((widths[0] - 75.0).abs() < 0.01, "got {widths:?}");
        assert!((widths[1] - 125.0).abs() < 0.01, "got {widths:?}");
        assert!(
            (widths.iter().sum::<f32>() - 200.0).abs() < 0.01,
            "must fill exactly"
        );
    }

    #[test]
    fn a_column_that_wants_nothing_extra_does_not_grow() {
        // First column is already at its maximum; all slack goes to the second.
        let widths = distribute_widths(&[50.0, 50.0], &[50.0, 250.0], Some(150.0));
        assert!((widths[0] - 50.0).abs() < 0.01, "got {widths:?}");
        assert!((widths[1] - 100.0).abs() < 0.01, "got {widths:?}");
    }

    #[test]
    fn an_unconstrained_table_uses_its_maximum() {
        assert_eq!(distribute_widths(&[10.0], &[80.0], None), vec![80.0]);
    }

    #[test]
    fn a_spanning_cell_widens_every_column_it_covers() {
        let mut widths = vec![20.0, 20.0, 100.0];
        apply_span(&mut widths, 0, 2, 100.0, css::style::DEFAULT_BORDER_SPACING);
        // Wanted 100 across two 20px columns with 2px spacing between them:
        // the 58px shortfall splits evenly.
        assert!((widths[0] - 49.0).abs() < 0.01, "got {widths:?}");
        assert!((widths[1] - 49.0).abs() < 0.01, "got {widths:?}");
        assert_eq!(widths[2], 100.0, "untouched columns stay put");
    }

    #[test]
    fn a_spanning_cell_that_already_fits_changes_nothing() {
        let mut widths = vec![80.0, 80.0];
        apply_span(&mut widths, 0, 2, 100.0, css::style::DEFAULT_BORDER_SPACING);
        assert_eq!(widths, vec![80.0, 80.0]);
    }

    #[test]
    fn a_span_running_past_the_last_column_is_clamped() {
        let mut widths = vec![10.0, 10.0];
        apply_span(&mut widths, 1, 5, 100.0, css::style::DEFAULT_BORDER_SPACING);
        assert_eq!(widths[0], 10.0);
        assert!(widths[1] > 10.0);
    }

    fn grid_of(html: &str) -> Grid {
        let doc = dom::parse(html);
        let styles = css::cascade::cascade(&doc, &[]);
        let table = doc.find_element("table").expect("table");
        build_grid(&doc, &styles, table)
    }

    /// The grid of a table built out of `display` values rather than markup,
    /// rooted at the element with `id="t"`.
    fn css_grid_of(html: &str, css_text: &str) -> Grid {
        let doc = dom::parse(html);
        let sheets = [css::Stylesheet::parse(css_text)];
        let styles = css::cascade::cascade(&doc, &sheets);
        let table = doc
            .descendants(doc.root())
            .into_iter()
            .find(|&node| doc.element(node).is_some_and(|e| e.id() == Some("t")))
            .expect("the table element");
        build_grid(&doc, &styles, table)
    }

    #[test]
    fn a_table_built_out_of_display_values_has_a_grid() {
        // The whole point of reading `display` rather than tag names. This is
        // the shape most of the CSS 2.1 suite's table tests use, and what a
        // page that is not from the era means by a table.
        let grid = css_grid_of(
            "<body><div id=t><div class=r><div class=c>a</div><div class=c>b</div></div>\
             <div class=r><div class=c>c</div><div class=c>d</div></div></div></body>",
            "#t { display: table } .r { display: table-row } .c { display: table-cell }",
        );
        assert_eq!(grid.rows.len(), 2, "no rows were found");
        assert_eq!(grid.columns, 2);
        assert_eq!(grid.rows[0].cells.len(), 2);
    }

    #[test]
    fn a_row_group_by_display_is_a_band_like_thead_is() {
        let grid = css_grid_of(
            "<body><div id=t><div class=g><div class=r><div class=c>a</div></div></div></div></body>",
            "#t { display: table } .g { display: table-row-group } \
             .r { display: table-row } .c { display: table-cell }",
        );
        assert_eq!(grid.row_groups.len(), 1, "the group was not recorded");
        assert_eq!(grid.rows.len(), 1);
        assert_eq!(grid.rows[0].group, Some(0));
    }

    #[test]
    fn a_caption_is_found_by_display_and_not_by_tag() {
        let doc = dom::parse(
            "<body><div id=t><div class=cap>heading</div>\
             <div class=r><div class=c>a</div></div></div></body>",
        );
        let sheets = [css::Stylesheet::parse(
            "#t { display: table } .cap { display: table-caption }              .r { display: table-row } .c { display: table-cell }",
        )];
        let styles = css::cascade::cascade(&doc, &sheets);
        let table = doc
            .descendants(doc.root())
            .into_iter()
            .find(|&node| doc.element(node).is_some_and(|e| e.id() == Some("t")))
            .expect("the table element");
        assert_eq!(
            captions(&doc, &styles, table).len(),
            1,
            "a div captioning a div table was not found"
        );
    }

    #[test]
    fn a_column_band_by_display_sizes_the_same_columns() {
        let grid = css_grid_of(
            "<body><div id=t><div class=cg><div class=col></div><div class=col></div></div>\
             <div class=r><div class=c>a</div><div class=c>b</div></div></div></body>",
            "#t { display: table } .cg { display: table-column-group } \
             .col { display: table-column } .r { display: table-row } \
             .c { display: table-cell }",
        );
        assert_eq!(grid.columns_declared.len(), 2);
        assert_eq!(grid.column_groups.len(), 1);
        assert_eq!(grid.column_groups[0].end, 2, "the group did not span both");
    }

    #[test]
    fn a_floated_row_group_is_no_longer_part_of_the_table() {
        // §9.7 takes it out of the table and makes it a block. Collecting its
        // rows anyway would lay them out twice: once where the float went, and
        // once in a grid that no longer contains them.
        let grid = css_grid_of(
            "<body><div id=t><div class=g><div class=r><div class=c>a</div></div></div>\
             <div class=r><div class=c>b</div></div></div></body>",
            "#t { display: table } .g { display: table-row-group; float: left } \
             .r { display: table-row } .c { display: table-cell }",
        );
        assert_eq!(grid.rows.len(), 1, "the floated group's row was collected");
        assert_eq!(grid.row_groups.len(), 0);
    }

    #[test]
    fn a_caption_is_not_searched_for_rows() {
        // A caption and a column band are the table's business but not the
        // grid's. Descending into one reads whatever is inside it as rows,
        // and a caption holding a `display: table-row` div would gain the
        // table a row that is not in it.
        let grid = css_grid_of(
            "<body><div id=t><div class=cap><div class=r><div class=c>x</div></div></div>\
             <div class=r><div class=c>a</div></div></div></body>",
            "#t { display: table } .cap { display: table-caption } \
             .r { display: table-row } .c { display: table-cell }",
        );
        assert_eq!(grid.rows.len(), 1, "a row inside the caption was collected");
    }

    #[test]
    fn only_a_table_cell_is_a_cell() {
        // A row's other children are not cells. Taking every element in a row
        // as one is what a tag-name check used to prevent, and the display
        // check has to keep preventing it.
        let grid = css_grid_of(
            "<body><div id=t><div class=r><div class=c>a</div><div class=b>not a cell</div>\
             </div></div></body>",
            "#t { display: table } .r { display: table-row } \
             .c { display: table-cell } .b { display: block }",
        );
        assert_eq!(grid.rows[0].cells.len(), 1, "a block child became a cell");
    }

    #[test]
    fn markup_tables_still_build_the_same_grid() {
        // The UA sheet is what gives `<tr>` and `<td>` their displays, so era
        // markup reaches the same place by a different road.
        let grid = grid_of("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>");
        assert_eq!(grid.rows.len(), 2);
        assert_eq!(grid.columns, 2);
    }

    #[test]
    fn reads_rows_through_an_implied_tbody() {
        // The parser inserts tbody whether or not the author wrote one, so rows
        // are almost never direct children of the table.
        let grid =
            grid_of("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>");
        assert_eq!(grid.rows.len(), 2);
        assert_eq!(grid.columns, 2);
    }

    #[test]
    fn counts_columns_across_spans() {
        let grid = grid_of(
            r#"<table><tr><td colspan="3">wide</td></tr><tr><td>a</td><td>b</td></tr></table>"#,
        );
        assert_eq!(grid.columns, 3);
        assert_eq!(grid.rows[0].cells[0].colspan, 3);
        assert_eq!(grid.rows[1].cells[1].column, 1);
    }

    #[test]
    fn header_cells_are_collected_like_data_cells() {
        let grid = grid_of(
            "<table><thead><tr><th>h</th></tr></thead><tbody><tr><td>d</td></tr></tbody></table>",
        );
        assert_eq!(grid.rows.len(), 2);
    }

    /// A candidate of a given width and style, from a given kind of box.
    ///
    /// The edge is fixed and the colour is arbitrary: neither takes part in the
    /// conflict, which is the point worth keeping in front of these tests.
    fn offered(width: f32, style: BorderStyle, origin: BorderOrigin) -> Candidate {
        Candidate {
            width,
            style,
            color: Color::BLACK,
            edge: BorderEdge::Top,
            origin,
        }
    }

    #[test]
    fn hidden_beats_every_other_candidate() {
        // Not "the widest of the visible ones" — `hidden` suppresses the border
        // outright, however emphatic its rivals. It is the only way a page can
        // punch a hole in a collapsed grid, so losing this rule loses the
        // feature rather than shifting a pixel.
        let resolved = resolve_conflict(&[
            offered(20.0, BorderStyle::Double, BorderOrigin::Cell),
            offered(1.0, BorderStyle::Hidden, BorderOrigin::Table),
        ]);
        assert!(resolved.is_none(), "a 20px double outranked `hidden`");
    }

    #[test]
    fn none_loses_to_everything_and_to_nothing() {
        let over = resolve_conflict(&[
            offered(0.0, BorderStyle::None, BorderOrigin::Cell),
            offered(1.0, BorderStyle::Dotted, BorderOrigin::Table),
        ])
        .expect("the dotted border");
        assert_eq!(over.style, BorderStyle::Dotted);

        // Every candidate `none` is not the same as `hidden`, but it draws the
        // same nothing.
        assert!(
            resolve_conflict(&[
                offered(0.0, BorderStyle::None, BorderOrigin::Cell),
                offered(0.0, BorderStyle::None, BorderOrigin::Row),
            ])
            .is_none()
        );
        assert!(resolve_conflict(&[]).is_none(), "nothing offered at all");
    }

    #[test]
    fn the_widest_border_wins_regardless_of_style_or_origin() {
        // Width is checked before style and before origin, so a thick border on
        // the table beats a thin one on a cell even though both later rules
        // would go the other way.
        let resolved = resolve_conflict(&[
            offered(1.0, BorderStyle::Double, BorderOrigin::Cell),
            offered(5.0, BorderStyle::Inset, BorderOrigin::Table),
        ])
        .expect("a border");
        assert_eq!(resolved.width, 5.0);
        assert_eq!(resolved.style, BorderStyle::Inset);
    }

    #[test]
    fn equal_widths_are_settled_by_the_style_order() {
        // §17.6.2.1 in full: double > solid > dashed > dotted > ridge > outset
        // > groove > inset. Checked pairwise down the whole chain, and in both
        // presentation orders, so a rule that happens to work because the
        // winner was listed first does not pass.
        let order = [
            BorderStyle::Double,
            BorderStyle::Solid,
            BorderStyle::Dashed,
            BorderStyle::Dotted,
            BorderStyle::Ridge,
            BorderStyle::Outset,
            BorderStyle::Groove,
            BorderStyle::Inset,
        ];
        for (rank, stronger) in order.iter().enumerate() {
            for weaker in &order[rank + 1..] {
                for pair in [[*stronger, *weaker], [*weaker, *stronger]] {
                    let candidates = [
                        offered(3.0, pair[0], BorderOrigin::Cell),
                        offered(3.0, pair[1], BorderOrigin::Cell),
                    ];
                    let won = resolve_conflict(&candidates).expect("a border").style;
                    assert_eq!(
                        won, *stronger,
                        "{stronger:?} should beat {weaker:?}, offered as {pair:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_tie_on_width_and_style_is_settled_by_where_it_came_from() {
        // cell > row > row group > column > column group > table, checked
        // pairwise and in both orders for the same reason as the style chain.
        let order = [
            BorderOrigin::Cell,
            BorderOrigin::Row,
            BorderOrigin::RowGroup,
            BorderOrigin::Column,
            BorderOrigin::ColumnGroup,
            BorderOrigin::Table,
        ];
        for (rank, stronger) in order.iter().enumerate() {
            for weaker in &order[rank + 1..] {
                for pair in [[*stronger, *weaker], [*weaker, *stronger]] {
                    let candidates = [
                        offered(2.0, BorderStyle::Solid, pair[0]),
                        offered(2.0, BorderStyle::Solid, pair[1]),
                    ];
                    let won = resolve_conflict(&candidates).expect("a border").origin;
                    assert_eq!(
                        won, *stronger,
                        "{stronger:?} should beat {weaker:?}, offered as {pair:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn two_borders_alike_in_every_way_keep_the_one_offered_first() {
        // The sixth rule — further to the left, further to the top — is carried
        // by the caller's ordering rather than by a comparison, so what has to
        // hold here is that a tie does not swap. Distinguished by colour, which
        // takes no part in the conflict and so is the only thing left to tell
        // two otherwise identical candidates apart.
        let first = Candidate {
            color: Color::BLACK,
            ..offered(2.0, BorderStyle::Solid, BorderOrigin::Cell)
        };
        let second = Candidate {
            color: Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            },
            ..offered(2.0, BorderStyle::Solid, BorderOrigin::Cell)
        };
        let won = resolve_conflict(&[first, second]).expect("a border");
        assert_eq!(won.color, Color::BLACK, "the later candidate took a tie");
    }

    #[test]
    fn a_collapsed_grid_line_is_resolved_once_per_segment() {
        // The case a per-cell border cannot express: one edge of a spanning
        // cell facing two different neighbours. The colspan's bottom edge is a
        // single edge of a single box and has to come out red on one half and
        // blue on the other.
        let doc = dom::parse(
            r#"<table><tr><td colspan="2">wide</td></tr>
               <tr><td class="red">r</td><td class="blue">b</td></tr></table>"#,
        );
        let sheets = [css::Stylesheet::parse(
            "table { border-collapse: collapse }
             td { border: 1px solid black }
             .red { border-top: 6px solid #ff0000 }
             .blue { border-top: 6px solid #0000ff }",
        )];
        let styles = css::cascade::cascade(&doc, &sheets);
        let table = doc.find_element("table").expect("table");
        let style = styles.get(table).expect("a styled table");
        let collapsed = collapse_borders(&doc, &styles, table, style);

        let line = &collapsed.horizontal[1];
        assert_eq!(line.len(), 2, "one segment per column");
        assert_eq!(line[0].expect("a border").color.r, 255, "left half red");
        assert_eq!(line[1].expect("a border").color.b, 255, "right half blue");
        assert_eq!(
            collapsed.horizontal_widths[1], 6.0,
            "the line is as wide as its widest segment"
        );
    }

    #[test]
    fn a_row_group_offers_its_own_edges_and_only_those() {
        // A `thead`'s bottom edge is a grid line no row and no cell speaks for,
        // and it is where the era's tables put the rule under their headings.
        let doc = dom::parse(
            "<table><thead><tr><td>h</td></tr></thead>
             <tbody><tr><td>a</td></tr><tr><td>b</td></tr></tbody></table>",
        );
        let sheets = [css::Stylesheet::parse(
            "table { border-collapse: collapse }
             td { border: 1px solid black }
             thead { border-bottom: 7px solid black }",
        )];
        let styles = css::cascade::cascade(&doc, &sheets);
        let table = doc.find_element("table").expect("table");
        let style = styles.get(table).expect("a styled table");
        let collapsed = collapse_borders(&doc, &styles, table, style);

        assert_eq!(collapsed.grid.row_groups.len(), 2, "thead and tbody");
        assert_eq!(collapsed.horizontal_widths[1], 7.0, "under the heading");
        assert_eq!(
            collapsed.horizontal_widths[2], 1.0,
            "the group's border does not reach the rows inside it"
        );
    }

    #[test]
    fn a_column_offers_a_border_at_its_own_sides() {
        let doc = dom::parse(
            r#"<table><colgroup><col><col class="edge"></colgroup>
               <tr><td>a</td><td>b</td></tr></table>"#,
        );
        let sheets = [css::Stylesheet::parse(
            "table { border-collapse: collapse }
             td { border: 1px solid black }
             .edge { border-left: 9px solid black }",
        )];
        let styles = css::cascade::cascade(&doc, &sheets);
        let table = doc.find_element("table").expect("table");
        let style = styles.get(table).expect("a styled table");
        let collapsed = collapse_borders(&doc, &styles, table, style);

        assert_eq!(collapsed.grid.columns_declared.len(), 2);
        assert_eq!(collapsed.grid.column_groups.len(), 1);
        assert_eq!(
            collapsed.vertical_widths[1], 9.0,
            "the second column's left edge"
        );
    }

    #[test]
    fn a_spanning_cell_occupies_every_slot_it_covers() {
        let grid = grid_of(
            r#"<table><tr><td rowspan="2">tall</td><td>a</td></tr>
               <tr><td>b</td></tr></table>"#,
        );
        let map = grid.occupancy();
        assert_eq!(map[0][0], Some((0, 0)), "the tall cell, in its own row");
        assert_eq!(map[1][0], Some((0, 0)), "and in the row it spans into");
        assert_eq!(map[1][1], Some((1, 0)), "the row's own cell beside it");
    }

    #[test]
    fn hidden_rows_and_cells_are_skipped() {
        let doc =
            dom::parse(r#"<table><tr class="x"><td>gone</td></tr><tr><td>kept</td></tr></table>"#);
        let sheets = [css::Stylesheet::parse(".x { display: none }")];
        let styles = css::cascade::cascade(&doc, &sheets);
        let table = doc.find_element("table").expect("table");
        let grid = build_grid(&doc, &styles, table);
        assert_eq!(grid.rows.len(), 1);
    }
}
