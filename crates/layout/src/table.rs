//! Table layout.
//!
//! The 2000s web laid out with tables (ADR-0004), so this is load-bearing
//! rather than a compatibility footnote.
//!
//! Implements CSS 2.1's *automatic* table layout in the separated-borders
//! model: column widths come from cell content, and the table is only as wide
//! as it needs to be unless a width is declared. Fixed layout, collapsed
//! borders, and row spanning are not here yet — see the notes on [`Grid`].

use css::style::{ComputedStyle, Display};
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
}

/// A table flattened into rows of cells.
#[derive(Debug, Clone, Default)]
pub struct Grid {
    /// Rows in document order.
    pub rows: Vec<Row>,
    /// Number of columns, accounting for spans.
    pub columns: usize,
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
    collect_rows(doc, styles, table, &mut occupied, &mut grid);
    // The rightmost column any cell reaches, not the widest row: with row
    // spanning a row's own cells no longer cover every column.
    grid.columns = grid
        .rows
        .iter()
        .flat_map(|row| row.cells.iter())
        .map(|cell| cell.column + cell.colspan)
        .max()
        .unwrap_or(0);
    grid
}

fn collect_rows(
    doc: &Document,
    styles: &css::cascade::StyleMap,
    node: NodeId,
    occupied: &mut Vec<usize>,
    grid: &mut Grid,
) {
    for &child in doc.children(node) {
        let Some(element) = doc.element(child) else {
            continue;
        };
        let Some(style) = styles.get(child) else {
            continue;
        };
        if style.display == Display::None {
            continue;
        }
        match element.local_name() {
            "tr" => {
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
                    if !matches!(cell_element.local_name(), "td" | "th") {
                        continue;
                    }
                    let Some(cell_style) = styles.get(cell_node) else {
                        continue;
                    };
                    if cell_style.display == Display::None {
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
                    });
                }
            }
            // Row groups, and any other wrapper, are descended through.
            _ => collect_rows(doc, styles, child, occupied, grid),
        }
    }
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
