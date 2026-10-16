use super::normalized_corner_radii;
use crate::model::{
    CornerRounding, Insets, Micrometres, ModelError, ObjectKind, PhysicalSize, PublicationObject,
    TextBoxSizingMode,
};

/// Conservative inscribed rectangle: 1 - sqrt(1/2), rounded upward in integer
/// millionths. Both coordinates remain inside every quarter-circle corner.
/// # Errors
/// Rejects invalid geometry before deriving physical insets.
pub fn rounded_interior_insets(
    size: PhysicalSize,
    rounding: CornerRounding,
) -> Result<Insets, ModelError> {
    let [tl, tr, br, bl] = normalized_corner_radii(size, rounding)?;
    let inset = |radius: i64| Micrometres::new((radius * 292_894 + 999_999) / 1_000_000);
    Ok(Insets {
        top: inset(tl.max(tr)),
        right: inset(tr.max(br)),
        bottom: inset(br.max(bl)),
        left: inset(bl.max(tl)),
    })
}
/// Existing margins satisfy corner clearance when they are already larger.
/// # Errors
/// Rejects invalid requested radii or dimensions.
pub fn rounded_text_margins(
    size: PhysicalSize,
    rounding: CornerRounding,
    requested: Insets,
) -> Result<Insets, ModelError> {
    let corner = rounded_interior_insets(size, rounding)?;
    Ok(Insets {
        top: requested.top.max(corner.top),
        right: requested.right.max(corner.right),
        bottom: requested.bottom.max(corner.bottom),
        left: requested.left.max(corner.left),
    })
}
/// Finds the final Fit size using the same normalized corner clearance. Each
/// bounded iteration grows monotonically; normalization makes this a contraction.
/// # Errors
/// Rejects overflow or a size that cannot reach an integer fixed point.
pub fn fit_rounded_text_size(
    content: PhysicalSize,
    rounding: CornerRounding,
    requested: Insets,
) -> Result<(PhysicalSize, Insets), ModelError> {
    let add = |margins: Insets| {
        PhysicalSize::new(
            content
                .width
                .value()
                .checked_add(margins.left.value())
                .and_then(|value| value.checked_add(margins.right.value()))
                .ok_or(ModelError::CoordinateOverflow)?,
            content
                .height
                .value()
                .checked_add(margins.top.value())
                .and_then(|value| value.checked_add(margins.bottom.value()))
                .ok_or(ModelError::CoordinateOverflow)?,
        )
    };
    let mut size = add(requested)?;
    for _ in 0..64 {
        let margins = rounded_text_margins(size, rounding, requested)?;
        let next = add(margins)?;
        if next == size {
            return Ok((size, margins));
        }
        size = next;
    }
    Err(ModelError::CornerInteriorDoesNotFit)
}
/// One shared weighted boundary derivation for canonical corner admission and
/// native Table layout. Integer division retains the established track positions.
/// # Errors
/// Rejects zero or degenerate physical tracks.
pub fn table_track_boundaries(total: i64, weights: &[u32]) -> Result<Vec<i64>, ModelError> {
    let sum = weights.iter().map(|weight| u64::from(*weight)).sum::<u64>();
    if total <= 0 || sum == 0 || weights.is_empty() {
        return Err(ModelError::CornerInteriorDoesNotFit);
    }
    let mut boundaries = vec![0];
    let mut cumulative = 0_u64;
    for weight in weights {
        cumulative += u64::from(*weight);
        let boundary = i64::try_from(i128::from(total) * i128::from(cumulative) / i128::from(sum))
            .map_err(|_| ModelError::CoordinateOverflow)?;
        if boundary
            <= *boundaries
                .last()
                .ok_or(ModelError::CornerInteriorDoesNotFit)?
        {
            return Err(ModelError::CornerInteriorDoesNotFit);
        }
        boundaries.push(boundary);
    }
    Ok(boundaries)
}
/// Padded cell text is kept inside the same safe rounded rectangle. Cell tracks,
/// editable strings, spans and paint boundaries are preserved.
/// # Errors
/// Rejects an unusable cell instead of silently dropping its source text.
pub fn rounded_cell_text_bounds(
    size: PhysicalSize,
    clearance: Insets,
    cell: [i64; 4],
) -> Result<[i64; 4], ModelError> {
    let [left, top, right, bottom] = cell;
    let width = right - left;
    let height = bottom - top;
    if width <= 0 || height <= 0 {
        return Err(ModelError::CornerInteriorDoesNotFit);
    }
    let horizontal = 1_000.min(width / 4);
    let vertical = 500.min(height / 4);
    let result = [
        (left + horizontal).max(clearance.left.value()),
        (top + vertical).max(clearance.top.value()),
        (right - horizontal).min(size.width.value() - clearance.right.value()),
        (bottom - vertical).min(size.height.value() - clearance.bottom.value()),
    ];
    if result[2] <= result[0] || result[3] <= result[1] {
        return Err(ModelError::CornerInteriorDoesNotFit);
    }
    Ok(result)
}
/// Canonical admission uses the same interior policy as native composition.
/// # Errors
/// Refuses unsupported families or unusable rounded Text Box/Table interiors.
pub fn validate_corner_interior(object: &PublicationObject) -> Result<(), ModelError> {
    let rounding = object.corner_rounding;
    rounding.validate()?;
    if rounding.is_default() {
        return Ok(());
    }
    match &object.kind {
        ObjectKind::Shape {
            shape: crate::model::ShapeKind::Rectangle,
            ..
        }
        | ObjectKind::Image { .. } => Ok(()),
        ObjectKind::Shape { .. } => Err(ModelError::CornerRoundingNotSupported),
        ObjectKind::Text { frame, .. } => {
            let mut effective = *frame;
            effective.margins = rounded_text_margins(object.size, rounding, frame.margins)?;
            if frame.sizing_mode == TextBoxSizingMode::Fixed {
                effective
                    .validate_usable_for_size(object.size)
                    .map_err(|_| ModelError::CornerInteriorDoesNotFit)?;
            }
            Ok(())
        }
        ObjectKind::Table { table } => {
            if normalized_corner_radii(object.size, rounding)? == [0; 4] {
                return Ok(());
            }
            let clearance = rounded_interior_insets(object.size, rounding)?;
            let columns = table_track_boundaries(object.size.width.value(), &table.column_weights)?;
            let rows = table_track_boundaries(object.size.height.value(), &table.row_weights)?;
            for row in 0..table.rows {
                for column in 0..table.columns {
                    let span = table.cell_span(row, column);
                    if span.is_some_and(|span| (span.start_row, span.start_column) != (row, column))
                    {
                        continue;
                    }
                    let end_row = span.map_or(row + 1, |span| span.end_row);
                    let end_column = span.map_or(column + 1, |span| span.end_column);
                    rounded_cell_text_bounds(
                        object.size,
                        clearance,
                        [
                            columns[usize::from(column)],
                            rows[usize::from(row)],
                            columns[usize::from(end_column)],
                            rows[usize::from(end_row)],
                        ],
                    )?;
                }
            }
            Ok(())
        }
    }
}
