mod rounded_borders;
pub(super) use rounded_borders::round_outer_borders;
use document_core::model::{Color, TableData, TableGridBorder};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedTableCellFill {
    pub row: u16,
    pub column: u16,
    pub row_span: u16,
    pub column_span: u16,
    pub color: Color,
}

/// One unique, coalesced physical border. Shared edges are painted once;
/// conflicting explicit overrides use the below/right cell's edge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTableBorderSegment {
    pub x1_um: i64,
    pub y1_um: i64,
    pub x2_um: i64,
    pub y2_um: i64,
    pub color: Color,
    pub width_millipoints: u32,
    /// Empty uses the existing straight segment; rounded outer edges use this shared physical path.
    pub outline: Vec<document_core::shape_geometry::ShapePathCommand>,
}

pub(super) struct TablePaint {
    pub owners: Vec<(u16, u16)>,
    pub fills: Vec<ResolvedTableCellFill>,
    pub borders: Vec<ResolvedTableBorderSegment>,
}

pub(super) fn resolve_paint(table: &TableData, columns: &[i64], rows: &[i64]) -> TablePaint {
    let column_count = usize::from(table.columns);
    let mut owners = (0..table.rows)
        .flat_map(|row| (0..table.columns).map(move |column| (row, column)))
        .collect::<Vec<_>>();
    for span in &table.merged_cells {
        for row in span.start_row..span.end_row {
            for column in span.start_column..span.end_column {
                owners[usize::from(row) * column_count + usize::from(column)] =
                    (span.start_row, span.start_column);
            }
        }
    }
    let mut fills = Vec::new();
    for formatting in &table.cell_formatting {
        let (row, column) = (formatting.row, formatting.column);
        if owners[usize::from(row) * column_count + usize::from(column)] != (row, column) {
            continue;
        }
        if let Some(color) = formatting.fill.filter(|color| color.alpha > 0) {
            let span = table.cell_span(row, column);
            fills.push(ResolvedTableCellFill {
                row,
                column,
                row_span: span.map_or(1, |span| span.end_row - span.start_row),
                column_span: span.map_or(1, |span| span.end_column - span.start_column),
                color,
            });
        }
    }
    let mut borders = Vec::new();
    for row in 0..=table.rows {
        for column in 0..table.columns {
            let above = (row > 0)
                .then(|| owners[usize::from(row - 1) * column_count + usize::from(column)]);
            let below = (row < table.rows)
                .then(|| owners[usize::from(row) * column_count + usize::from(column)]);
            if above.is_some() && above == below {
                continue;
            }
            let edge = below
                .and_then(|(row, column)| table.cell_format(row, column))
                .and_then(|format| format.borders.top)
                .or_else(|| {
                    above
                        .and_then(|(row, column)| table.cell_format(row, column))
                        .and_then(|format| format.borders.bottom)
                })
                .unwrap_or(table.grid_border);
            append_border(
                &mut borders,
                edge,
                columns[usize::from(column)],
                rows[usize::from(row)],
                columns[usize::from(column) + 1],
                rows[usize::from(row)],
            );
        }
    }
    for column in 0..=table.columns {
        for row in 0..table.rows {
            let left = (column > 0)
                .then(|| owners[usize::from(row) * column_count + usize::from(column - 1)]);
            let right = (column < table.columns)
                .then(|| owners[usize::from(row) * column_count + usize::from(column)]);
            if left.is_some() && left == right {
                continue;
            }
            let edge = right
                .and_then(|(row, column)| table.cell_format(row, column))
                .and_then(|format| format.borders.left)
                .or_else(|| {
                    left.and_then(|(row, column)| table.cell_format(row, column))
                        .and_then(|format| format.borders.right)
                })
                .unwrap_or(table.grid_border);
            append_border(
                &mut borders,
                edge,
                columns[usize::from(column)],
                rows[usize::from(row)],
                columns[usize::from(column)],
                rows[usize::from(row) + 1],
            );
        }
    }
    TablePaint {
        owners,
        fills,
        borders,
    }
}

fn append_border(
    segments: &mut Vec<ResolvedTableBorderSegment>,
    edge: TableGridBorder,
    x1: i64,
    y1: i64,
    x2: i64,
    y2: i64,
) {
    if edge.width_millipoints == 0 || edge.color.alpha == 0 {
        return;
    }
    if let Some(previous) = segments.last_mut()
        && previous.color == edge.color
        && previous.width_millipoints == edge.width_millipoints
        && previous.x2_um == x1
        && previous.y2_um == y1
        && ((previous.x1_um == previous.x2_um && x1 == x2)
            || (previous.y1_um == previous.y2_um && y1 == y2))
    {
        previous.x2_um = x2;
        previous.y2_um = y2;
        return;
    }
    segments.push(ResolvedTableBorderSegment {
        x1_um: x1,
        y1_um: y1,
        x2_um: x2,
        y2_um: y2,
        color: edge.color,
        width_millipoints: edge.width_millipoints,
        outline: Vec::new(),
    });
}
