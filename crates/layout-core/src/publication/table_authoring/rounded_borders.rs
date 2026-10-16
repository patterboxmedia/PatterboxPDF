use super::ResolvedTableBorderSegment;
use document_core::model::{CornerRounding, ModelError, PhysicalSize};
use document_core::shape_geometry::{ShapePathCommand, normalized_corner_radii, rectangle_outline};

/// An outer corner belongs to its clockwise outgoing edge: top-left to top,
/// top-right to right, bottom-right to bottom, bottom-left to left. The corner
/// anchor's explicit/default edge appearance paints the whole arc. Interior
/// shared-edge precedence remains below/right, and every edge is painted once.
pub(crate) fn round_outer_borders(
    segments: &mut Vec<ResolvedTableBorderSegment>,
    size: PhysicalSize,
    rounding: CornerRounding,
) -> Result<(), ModelError> {
    let radii = normalized_corner_radii(size, rounding)?;
    if radii == [0; 4] {
        return Ok(());
    }
    let path = rectangle_outline(size, rounding)?;
    let mut output = Vec::with_capacity(segments.len());
    for mut edge in std::mem::take(segments) {
        if let Some(outline) = edge_outline(&edge, size, radii, &path) {
            if outline.is_empty() {
                continue;
            }
            edge.outline = outline;
        }
        output.push(edge);
    }
    *segments = output;
    Ok(())
}
fn edge_outline(
    edge: &ResolvedTableBorderSegment,
    size: PhysicalSize,
    [tl, tr, br, bl]: [i64; 4],
    path: &[ShapePathCommand],
) -> Option<Vec<ShapePathCommand>> {
    use ShapePathCommand::{LineTo, MoveTo};
    let w = size.width.value();
    let h = size.height.value();
    let mut result = Vec::new();
    if edge.y1_um == 0 && edge.y2_um == 0 {
        if edge.x1_um == 0 && tl > 0 {
            result.extend([MoveTo(0, tl), path[8]]);
        }
        let left = edge.x1_um.max(tl);
        let right = edge.x2_um.min(w - tr);
        if right > left {
            result.extend([MoveTo(left, 0), LineTo(right, 0)]);
        }
        Some(result)
    } else if edge.x1_um == w && edge.x2_um == w {
        if edge.y1_um == 0 && tr > 0 {
            result.extend([MoveTo(w - tr, 0), path[2]]);
        }
        let top = edge.y1_um.max(tr);
        let bottom = edge.y2_um.min(h - br);
        if bottom > top {
            result.extend([MoveTo(w, top), LineTo(w, bottom)]);
        }
        Some(result)
    } else if edge.y1_um == h && edge.y2_um == h {
        if edge.x2_um == w && br > 0 {
            result.extend([MoveTo(w, h - br), path[4]]);
        }
        let left = edge.x1_um.max(bl);
        let right = edge.x2_um.min(w - br);
        if right > left {
            result.extend([MoveTo(right, h), LineTo(left, h)]);
        }
        Some(result)
    } else if edge.x1_um == 0 && edge.x2_um == 0 {
        if edge.y2_um == h && bl > 0 {
            result.extend([MoveTo(bl, h), path[6]]);
        }
        let top = edge.y1_um.max(tl);
        let bottom = edge.y2_um.min(h - bl);
        if bottom > top {
            result.extend([MoveTo(0, bottom), LineTo(0, top)]);
        }
        Some(result)
    } else {
        None
    }
}
