//! Physical outlines shared by authoring and output. Requested radii stay durable.

mod rounded_interiors;
use crate::model::{CornerRounding, ModelError, PhysicalSize, ShapeKind};
pub use rounded_interiors::{
    fit_rounded_text_size, rounded_cell_text_bounds, rounded_interior_insets, rounded_text_margins,
    table_track_boundaries, validate_corner_interior,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShapePathCommand {
    MoveTo(i64, i64),
    LineTo(i64, i64),
    CubicTo(i64, i64, i64, i64, i64, i64),
    Close,
}

/// Reduces all four radii by one proportional factor until adjacent sums fit.
///
/// # Errors
/// Rejects invalid requested radii or nonpositive object dimensions.
pub fn normalized_corner_radii(
    size: PhysicalSize,
    requested: CornerRounding,
) -> Result<[i64; 4], ModelError> {
    PhysicalSize::new(size.width.value(), size.height.value())?;
    requested.validate()?;
    let radii = [
        requested.top_left_um,
        requested.top_right_um,
        requested.bottom_right_um,
        requested.bottom_left_um,
    ];
    let mut factor = (1_i128, 1_i128);
    for (limit, sum) in [
        (size.width.value(), radii[0] + radii[1]),
        (size.height.value(), radii[1] + radii[2]),
        (size.width.value(), radii[2] + radii[3]),
        (size.height.value(), radii[3] + radii[0]),
    ] {
        if sum > 0 && i128::from(limit) * factor.1 < factor.0 * i128::from(sum) {
            factor = (i128::from(limit), i128::from(sum));
        }
    }
    let mut normalized = radii;
    for radius in &mut normalized {
        *radius = i64::try_from(i128::from(*radius) * factor.0 / factor.1)
            .map_err(|_| ModelError::CoordinateOverflow)?;
    }
    Ok(normalized)
}

/// The closed physical outline for rectangles, triangles and right-facing arrows.
/// Other shape families retain their established dedicated outline.
///
/// # Errors
/// Rejects invalid geometry or an unsupported shape family.
pub fn shape_outline(
    shape: ShapeKind,
    size: PhysicalSize,
    requested: CornerRounding,
) -> Result<Vec<ShapePathCommand>, ModelError> {
    PhysicalSize::new(size.width.value(), size.height.value())?;
    let (w, h) = (size.width.value(), size.height.value());
    let vertices = match shape {
        ShapeKind::Triangle => vec![(w / 2, 0), (w, h), (0, h)],
        ShapeKind::Arrow => vec![
            (0, h / 4),
            (w / 5 * 3 + w % 5 * 3 / 5, h / 4),
            (w / 5 * 3 + w % 5 * 3 / 5, 0),
            (w, h / 2),
            (w / 5 * 3 + w % 5 * 3 / 5, h),
            (w / 5 * 3 + w % 5 * 3 / 5, h / 4 * 3 + h % 4 * 3 / 4),
            (0, h / 4 * 3 + h % 4 * 3 / 4),
        ],
        ShapeKind::Rectangle => return rectangle_outline(size, requested),
        ShapeKind::Ellipse | ShapeKind::Cube | ShapeKind::Line => {
            return Err(ModelError::CornerRoundingNotSupported);
        }
    };
    let mut path = vec![ShapePathCommand::MoveTo(vertices[0].0, vertices[0].1)];
    path.extend(
        vertices[1..]
            .iter()
            .map(|&(x, y)| ShapePathCommand::LineTo(x, y)),
    );
    path.push(ShapePathCommand::Close);
    Ok(path)
}

/// Shared closed outline for every supported rectangular object family.
/// # Errors
/// Rejects invalid requested radii or nonpositive dimensions.
pub fn rectangle_outline(
    size: PhysicalSize,
    requested: CornerRounding,
) -> Result<Vec<ShapePathCommand>, ModelError> {
    use ShapePathCommand::{Close, CubicTo, LineTo, MoveTo};
    let [tl, tr, br, bl] = normalized_corner_radii(size, requested)?;
    let (w, h) = (size.width.value(), size.height.value());
    // Rounded integer micrometres; the shared cubic approximation is below AS7.
    let k = |radius: i64| (radius * 552_285 + 500_000) / 1_000_000;
    Ok(vec![
        MoveTo(tl, 0),
        LineTo(w - tr, 0),
        CubicTo(w - tr + k(tr), 0, w, tr - k(tr), w, tr),
        LineTo(w, h - br),
        CubicTo(w, h - br + k(br), w - br + k(br), h, w - br, h),
        LineTo(bl, h),
        CubicTo(bl - k(bl), h, 0, h - bl + k(bl), 0, h - bl),
        LineTo(0, tl),
        CubicTo(0, tl - k(tl), tl - k(tl), 0, tl, 0),
        Close,
    ])
}
