//! Canonical object-transform composition shared by preview, layout, and export.
//!
//! This module deliberately returns conservative axis-aligned bounds: wrap
//! exclusions must never let text overlap a transformed object merely because
//! its rotated corners fall outside its untransformed rectangle.

use crate::model::{Micrometres, PhysicalSize, Transform};

/// A non-empty axis-aligned rectangle in canonical micrometres.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalBounds {
    pub x: Micrometres,
    pub y: Micrometres,
    pub width: Micrometres,
    pub height: Micrometres,
}

/// Encloses already canonical object bounds without changing their rounding.
///
/// # Errors
/// Rejects an empty selection or physical coordinate overflow.
pub fn union_bounds(bounds: &[PhysicalBounds]) -> Result<PhysicalBounds, TransformBoundsError> {
    let first = bounds.first().ok_or(TransformBoundsError::OutOfRange)?;
    let mut left = first.x.value();
    let mut top = first.y.value();
    let mut right = left
        .checked_add(first.width.value())
        .ok_or(TransformBoundsError::OutOfRange)?;
    let mut bottom = top
        .checked_add(first.height.value())
        .ok_or(TransformBoundsError::OutOfRange)?;
    for bounds in &bounds[1..] {
        left = left.min(bounds.x.value());
        top = top.min(bounds.y.value());
        right = right.max(
            bounds
                .x
                .value()
                .checked_add(bounds.width.value())
                .ok_or(TransformBoundsError::OutOfRange)?,
        );
        bottom = bottom.max(
            bounds
                .y
                .value()
                .checked_add(bounds.height.value())
                .ok_or(TransformBoundsError::OutOfRange)?,
        );
    }
    Ok(PhysicalBounds {
        x: Micrometres::new(left),
        y: Micrometres::new(top),
        width: Micrometres::new(
            right
                .checked_sub(left)
                .ok_or(TransformBoundsError::OutOfRange)?,
        ),
        height: Micrometres::new(
            bottom
                .checked_sub(top)
                .ok_or(TransformBoundsError::OutOfRange)?,
        ),
    })
}

/// Applies one selection resize in page axes while preserving intrinsic size,
/// source content, pivot, and the authored rotation parameter.
///
/// # Errors
/// Refuses a singular decomposition, overflow or more than three micrometres
/// of corner displacement after fixed-point rounding, within AS7's 0.01 point.
#[expect(
    clippy::cast_precision_loss,
    reason = "The shared affine evaluator uses f64, then checks every resulting corner before admission"
)]
pub fn resize_transform_in_bounds(
    transform: Transform,
    size: PhysicalSize,
    source: PhysicalBounds,
    target: PhysicalBounds,
) -> Result<Transform, TransformBoundsError> {
    if source.width.value() <= 0
        || source.height.value() <= 0
        || target.width.value() <= 0
        || target.height.value() <= 0
    {
        return Err(TransformBoundsError::NonInvertible);
    }
    let original = Affine::from_transform(transform)?;
    let scale_x = target.width.value() as f64 / source.width.value() as f64;
    let scale_y = target.height.value() as f64 / source.height.value() as f64;
    let angle = (transform.rotation_microdegrees as f64).to_radians() / 1_000_000.0;
    let (sin, cos) = angle.sin_cos();
    let local_x = cos * scale_x * original.a + sin * scale_y * original.b;
    let local_y = -sin * scale_x * original.c + cos * scale_y * original.d;
    let shear_x = (cos * scale_x * original.c + sin * scale_y * original.d) / local_y;
    let shear_y = (-sin * scale_x * original.a + cos * scale_y * original.b) / local_x;
    let nano = |value: f64| {
        if !value.is_finite() {
            return Err(TransformBoundsError::NonRepresentableResize);
        }
        canonical_round((value * Transform::IDENTITY_SCALE as f64).round())
    };
    let mut updated = Transform {
        scale_x_nanos: nano(local_x)?,
        scale_y_nanos: nano(local_y)?,
        shear_x_nanos: nano(shear_x)?,
        shear_y_nanos: nano(shear_y)?,
        ..transform
    };
    if updated.scale_x_nanos == 0 || updated.scale_y_nanos == 0 {
        return Err(TransformBoundsError::NonRepresentableResize);
    }
    let matrix = Affine::from_transform(updated)?;
    let original_x = original.translation_x + original.pivot_x
        - original.a * original.pivot_x
        - original.c * original.pivot_y;
    let original_y = original.translation_y + original.pivot_y
        - original.b * original.pivot_x
        - original.d * original.pivot_y;
    updated.translation_x = Micrometres::new(canonical_round(
        (target.x.value() as f64 + scale_x * (original_x - source.x.value() as f64)
            - matrix.pivot_x
            + matrix.a * matrix.pivot_x
            + matrix.c * matrix.pivot_y)
            .round(),
    )?);
    updated.translation_y = Micrometres::new(canonical_round(
        (target.y.value() as f64 + scale_y * (original_y - source.y.value() as f64)
            - matrix.pivot_y
            + matrix.b * matrix.pivot_x
            + matrix.d * matrix.pivot_y)
            .round(),
    )?);
    let accepted = Affine::from_transform(updated)?;
    for (x, y) in [
        (0.0, 0.0),
        (size.width.value() as f64, 0.0),
        (0.0, size.height.value() as f64),
        (size.width.value() as f64, size.height.value() as f64),
    ] {
        let (old_x, old_y) = original.forward(x, y);
        let expected_x = target.x.value() as f64 + scale_x * (old_x - source.x.value() as f64);
        let expected_y = target.y.value() as f64 + scale_y * (old_y - source.y.value() as f64);
        let (actual_x, actual_y) = accepted.forward(x, y);
        if (expected_x - actual_x).abs() > 3.0 || (expected_y - actual_y).abs() > 3.0 {
            return Err(TransformBoundsError::NonRepresentableResize);
        }
    }
    transformed_bounds(size, updated)?;
    Ok(updated)
}

/// Computes the conservative page-space bounds of an object's local rectangle.
///
/// The composed transform is
/// `translation + pivot + rotation * shear * scale * (point - pivot)`.
///
/// This is the single canonical evaluator for preview and export. Minima are
/// floored and maxima are ceiled, so the result conservatively contains both
/// floating-point rounding error and every transformed corner that falls
/// between integer micrometre coordinates.
///
/// # Errors
///
/// Returns [`TransformBoundsError::NonFinite`] or
/// [`TransformBoundsError::OutOfRange`] when a caller-supplied transform cannot
/// be represented in canonical physical units.
pub fn transformed_bounds(
    size: PhysicalSize,
    transform: Transform,
) -> Result<PhysicalBounds, TransformBoundsError> {
    transformed_local_bounds(
        PhysicalBounds {
            x: Micrometres::new(0),
            y: Micrometres::new(0),
            width: size.width,
            height: size.height,
        },
        transform,
    )
}

/// Evaluates one local physical point with the shared affine transform.
///
/// # Errors
/// Rejects nonfinite or unrepresentable physical coordinates.
#[expect(
    clippy::cast_precision_loss,
    reason = "Shared affine evaluation checks physical coordinate range before returning"
)]
pub fn transformed_point_um(
    transform: Transform,
    x_um: i64,
    y_um: i64,
) -> Result<(f64, f64), TransformBoundsError> {
    let point = Affine::from_transform(transform)?.forward(x_um as f64, y_um as f64);
    if !point.0.is_finite() || !point.1.is_finite() {
        return Err(TransformBoundsError::NonFinite);
    }
    canonical_round(point.0.round())?;
    canonical_round(point.1.round())?;
    Ok(point)
}

/// Rotates the whole selection rigidly about its page-space center.
///
/// # Errors
/// Rejects angle/coordinate overflow or unsafe rounded geometry.
#[expect(
    clippy::cast_precision_loss,
    reason = "Shared bounded affine geometry verifies every rounded corner"
)]
pub fn rotate_transform_in_bounds(
    transform: Transform,
    size: PhysicalSize,
    bounds: PhysicalBounds,
    delta_microdegrees: i64,
) -> Result<Transform, TransformBoundsError> {
    let rotation = transform
        .rotation_microdegrees
        .checked_add(delta_microdegrees)
        .ok_or(TransformBoundsError::OutOfRange)?;
    let angle = (delta_microdegrees as f64).to_radians() / 1_000_000.0;
    let (sin, cos) = angle.sin_cos();
    let center_x = bounds.x.value() as f64 + bounds.width.value() as f64 / 2.0;
    let center_y = bounds.y.value() as f64 + bounds.height.value() as f64 / 2.0;
    let x = transform.translation_x.value() as f64 + transform.pivot_x.value() as f64 - center_x;
    let y = transform.translation_y.value() as f64 + transform.pivot_y.value() as f64 - center_y;
    let updated = Transform {
        translation_x: Micrometres::new(canonical_round(
            (center_x + cos * x - sin * y - transform.pivot_x.value() as f64).round(),
        )?),
        translation_y: Micrometres::new(canonical_round(
            (center_y + sin * x + cos * y - transform.pivot_y.value() as f64).round(),
        )?),
        rotation_microdegrees: (rotation.rem_euclid(360_000_000) + 180_000_000)
            .rem_euclid(360_000_000)
            - 180_000_000,
        ..transform
    };
    let original = Affine::from_transform(transform)?;
    let accepted = Affine::from_transform(updated)?;
    for (x, y) in [
        (0.0, 0.0),
        (size.width.value() as f64, 0.0),
        (0.0, size.height.value() as f64),
        (size.width.value() as f64, size.height.value() as f64),
    ] {
        let (old_x, old_y) = original.forward(x, y);
        let (actual_x, actual_y) = accepted.forward(x, y);
        if (center_x + cos * (old_x - center_x) - sin * (old_y - center_y) - actual_x).abs() > 3.0
            || (center_y + sin * (old_x - center_x) + cos * (old_y - center_y) - actual_y).abs()
                > 3.0
        {
            return Err(TransformBoundsError::NonRepresentableResize);
        }
    }
    transformed_bounds(size, updated)?;
    Ok(updated)
}

/// Computes conservative page-space bounds for an arbitrary non-empty local rectangle.
///
/// This is used when paint extends beyond an object's nominal `0..width` and
/// `0..height` box, for example a hard shadow or centered border stroke.
///
/// # Errors
///
/// Returns the same finite/range errors as [`transformed_bounds`].
#[expect(
    clippy::cast_precision_loss,
    reason = "The canonical preview/export evaluator converts fixed-point physical units to f64 only for trigonometry; its enclosing floor/ceil bounds intentionally contain rounding error."
)]
pub fn transformed_local_bounds(
    bounds: PhysicalBounds,
    transform: Transform,
) -> Result<PhysicalBounds, TransformBoundsError> {
    let affine = Affine::from_transform(transform)?;

    let right = bounds
        .x
        .value()
        .checked_add(bounds.width.value())
        .ok_or(TransformBoundsError::OutOfRange)?;
    let bottom = bounds
        .y
        .value()
        .checked_add(bounds.height.value())
        .ok_or(TransformBoundsError::OutOfRange)?;

    let mut minimum_x = f64::INFINITY;
    let mut minimum_y = f64::INFINITY;
    let mut maximum_x = f64::NEG_INFINITY;
    let mut maximum_y = f64::NEG_INFINITY;
    for (x, y) in [
        (bounds.x.value() as f64, bounds.y.value() as f64),
        (right as f64, bounds.y.value() as f64),
        (bounds.x.value() as f64, bottom as f64),
        (right as f64, bottom as f64),
    ] {
        let (page_x, page_y) = affine.forward(x, y);
        if !page_x.is_finite() || !page_y.is_finite() {
            return Err(TransformBoundsError::NonFinite);
        }
        minimum_x = minimum_x.min(page_x);
        minimum_y = minimum_y.min(page_y);
        maximum_x = maximum_x.max(page_x);
        maximum_y = maximum_y.max(page_y);
    }

    physical_bounds_from_extrema(minimum_x, minimum_y, maximum_x, maximum_y)
}

/// Maps the corners of a conservative page-space rectangle through the inverse
/// of an object's canonical transform and encloses them in a local-space AABB.
///
/// This is intentionally conservative: callers such as text wrap may start
/// with an already-enclosing page-space rectangle, so the returned local bounds
/// can be larger than the exact transformed shape but never smaller.
///
/// # Errors
///
/// Returns [`TransformBoundsError::NonInvertible`] for a singular scale/shear
/// combination, or the usual finite/range errors for invalid geometry.
#[expect(
    clippy::cast_precision_loss,
    reason = "The canonical inverse evaluator converts fixed-point physical units to f64 for the same bounded affine calculation as transformed_bounds."
)]
pub fn inverse_transformed_bounds(
    page_bounds: PhysicalBounds,
    transform: Transform,
) -> Result<PhysicalBounds, TransformBoundsError> {
    let affine = Affine::from_transform(transform)?;
    let right = page_bounds
        .x
        .value()
        .checked_add(page_bounds.width.value())
        .ok_or(TransformBoundsError::OutOfRange)?;
    let bottom = page_bounds
        .y
        .value()
        .checked_add(page_bounds.height.value())
        .ok_or(TransformBoundsError::OutOfRange)?;
    let mut minimum_x = f64::INFINITY;
    let mut minimum_y = f64::INFINITY;
    let mut maximum_x = f64::NEG_INFINITY;
    let mut maximum_y = f64::NEG_INFINITY;
    for (x, y) in [
        (page_bounds.x.value() as f64, page_bounds.y.value() as f64),
        (right as f64, page_bounds.y.value() as f64),
        (page_bounds.x.value() as f64, bottom as f64),
        (right as f64, bottom as f64),
    ] {
        let (local_x, local_y) = affine.inverse(x, y)?;
        minimum_x = minimum_x.min(local_x);
        minimum_y = minimum_y.min(local_y);
        maximum_x = maximum_x.max(local_x);
        maximum_y = maximum_y.max(local_y);
    }
    physical_bounds_from_extrema(minimum_x, minimum_y, maximum_x, maximum_y)
}

#[derive(Clone, Copy)]
struct Affine {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    pivot_x: f64,
    pivot_y: f64,
    translation_x: f64,
    translation_y: f64,
}

impl Affine {
    #[expect(
        clippy::cast_precision_loss,
        reason = "Fixed-point canonical transform components are converted to f64 solely for bounded affine and trigonometric evaluation."
    )]
    fn from_transform(transform: Transform) -> Result<Self, TransformBoundsError> {
        let angle = (transform.rotation_microdegrees as f64).to_radians() / 1_000_000.0;
        let (sin, cos) = angle.sin_cos();
        let scale_x = transform.scale_x_nanos as f64 / Transform::IDENTITY_SCALE as f64;
        let scale_y = transform.scale_y_nanos as f64 / Transform::IDENTITY_SCALE as f64;
        let shear_x = transform.shear_x_nanos as f64 / Transform::IDENTITY_SCALE as f64;
        let shear_y = transform.shear_y_nanos as f64 / Transform::IDENTITY_SCALE as f64;
        let affine = Self {
            a: scale_x * (cos - sin * shear_y),
            b: scale_x * (sin + cos * shear_y),
            c: scale_y * (cos * shear_x - sin),
            d: scale_y * (sin * shear_x + cos),
            pivot_x: transform.pivot_x.value() as f64,
            pivot_y: transform.pivot_y.value() as f64,
            translation_x: transform.translation_x.value() as f64,
            translation_y: transform.translation_y.value() as f64,
        };
        if [
            affine.a,
            affine.b,
            affine.c,
            affine.d,
            affine.pivot_x,
            affine.pivot_y,
            affine.translation_x,
            affine.translation_y,
        ]
        .iter()
        .any(|value| !value.is_finite())
        {
            return Err(TransformBoundsError::NonFinite);
        }
        Ok(affine)
    }

    fn forward(self, x: f64, y: f64) -> (f64, f64) {
        let local_x = x - self.pivot_x;
        let local_y = y - self.pivot_y;
        (
            self.translation_x + self.pivot_x + self.a * local_x + self.c * local_y,
            self.translation_y + self.pivot_y + self.b * local_x + self.d * local_y,
        )
    }

    fn inverse(self, x: f64, y: f64) -> Result<(f64, f64), TransformBoundsError> {
        let determinant = self.a * self.d - self.b * self.c;
        if !determinant.is_finite() || determinant.abs() <= f64::EPSILON {
            return Err(TransformBoundsError::NonInvertible);
        }
        let page_x = x - self.translation_x - self.pivot_x;
        let page_y = y - self.translation_y - self.pivot_y;
        let local_x = (self.d * page_x - self.c * page_y) / determinant + self.pivot_x;
        let local_y = (-self.b * page_x + self.a * page_y) / determinant + self.pivot_y;
        if !local_x.is_finite() || !local_y.is_finite() {
            return Err(TransformBoundsError::NonFinite);
        }
        Ok((local_x, local_y))
    }
}

fn physical_bounds_from_extrema(
    minimum_x: f64,
    minimum_y: f64,
    maximum_x: f64,
    maximum_y: f64,
) -> Result<PhysicalBounds, TransformBoundsError> {
    let x = canonical_round(minimum_x.floor())?;
    let y = canonical_round(minimum_y.floor())?;
    let right = canonical_round(maximum_x.ceil())?;
    let bottom = canonical_round(maximum_y.ceil())?;
    let width = right
        .checked_sub(x)
        .ok_or(TransformBoundsError::OutOfRange)?;
    let height = bottom
        .checked_sub(y)
        .ok_or(TransformBoundsError::OutOfRange)?;
    if width <= 0 || height <= 0 {
        return Err(TransformBoundsError::OutOfRange);
    }
    Ok(PhysicalBounds {
        x: Micrometres::new(x),
        y: Micrometres::new(y),
        width: Micrometres::new(width),
        height: Micrometres::new(height),
    })
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "The checked floor/ceil value is within i64 bounds before conversion back to canonical physical units."
)]
fn canonical_round(value: f64) -> Result<i64, TransformBoundsError> {
    const I64_MIN_AS_F64: f64 = -9_223_372_036_854_775_808.0;
    const I64_MAX_EXCLUSIVE_AS_F64: f64 = 9_223_372_036_854_775_808.0;
    if value < I64_MIN_AS_F64 || value >= I64_MAX_EXCLUSIVE_AS_F64 {
        Err(TransformBoundsError::OutOfRange)
    } else {
        Ok(value as i64)
    }
}

/// Transform-bound calculation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransformBoundsError {
    NonFinite,
    NonInvertible,
    OutOfRange,
    NonRepresentableResize,
}

impl core::fmt::Display for TransformBoundsError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if *self == Self::NonRepresentableResize {
            return formatter.write_str("This selection cannot reach that size safely. Use a larger size or reset the object transforms.");
        }
        write!(formatter, "transformed bounds failed: {self:?}")
    }
}

impl std::error::Error for TransformBoundsError {}

#[cfg(test)]
mod tests {
    use crate::model::{Micrometres, PhysicalSize, Transform};

    use super::{PhysicalBounds, inverse_transformed_bounds, transformed_bounds};

    #[test]
    fn identity_bounds_are_the_local_size_at_translation() {
        let bounds = transformed_bounds(
            PhysicalSize::new(30_000, 20_000).unwrap(),
            Transform::identity_at(10_000, 15_000),
        )
        .unwrap();
        assert_eq!(bounds.x.value(), 10_000);
        assert_eq!(bounds.y.value(), 15_000);
        assert_eq!(bounds.width.value(), 30_000);
        assert_eq!(bounds.height.value(), 20_000);
    }

    #[test]
    fn rotation_expands_wrap_bounds_around_all_corners() {
        let transform = Transform {
            rotation_microdegrees: 90_000_000,
            ..Transform::identity_at(50_000, 60_000)
        };
        let bounds =
            transformed_bounds(PhysicalSize::new(30_000, 20_000).unwrap(), transform).unwrap();
        assert_eq!(bounds.x.value(), 30_000);
        assert_eq!(bounds.y.value(), 60_000);
        assert_eq!(bounds.width.value(), 20_000);
        assert_eq!(bounds.height.value(), 30_000);
    }

    #[test]
    fn scale_and_shear_change_bounds_before_rotation() {
        let transform = Transform {
            scale_x_nanos: 2_000_000_000,
            shear_x_nanos: 500_000_000,
            ..Transform::identity_at(0, 0)
        };
        let bounds =
            transformed_bounds(PhysicalSize::new(10_000, 20_000).unwrap(), transform).unwrap();
        assert_eq!(bounds.width.value(), 30_000);
        assert_eq!(bounds.height.value(), 20_000);
    }

    #[test]
    fn forty_five_degree_bounds_floor_and_ceil_all_transformed_corners() {
        let transform = Transform {
            rotation_microdegrees: 45_000_000,
            ..Transform::identity_at(50_000, 60_000)
        };
        let bounds =
            transformed_bounds(PhysicalSize::new(30_000, 20_000).unwrap(), transform).unwrap();
        assert_eq!(bounds.x.value(), 35_857);
        assert_eq!(bounds.y.value(), 60_000);
        assert_eq!(bounds.width.value(), 35_357);
        assert_eq!(bounds.height.value(), 35_356);
    }

    #[test]
    fn identity_inverse_recovers_local_bounds() {
        let page_bounds = PhysicalBounds {
            x: Micrometres::new(10_000),
            y: Micrometres::new(15_000),
            width: Micrometres::new(30_000),
            height: Micrometres::new(20_000),
        };

        let bounds =
            inverse_transformed_bounds(page_bounds, Transform::identity_at(10_000, 15_000))
                .unwrap();

        assert_eq!(bounds.x.value(), 0);
        assert_eq!(bounds.y.value(), 0);
        assert_eq!(bounds.width.value(), 30_000);
        assert_eq!(bounds.height.value(), 20_000);
    }

    #[test]
    fn inverse_rotation_encloses_page_bounds_in_local_space() {
        let page_bounds = PhysicalBounds {
            x: Micrometres::new(-30_000),
            y: Micrometres::new(20_000),
            width: Micrometres::new(20_000),
            height: Micrometres::new(30_000),
        };
        let transform = Transform {
            rotation_microdegrees: 90_000_000,
            ..Transform::identity_at(10_000, 10_000)
        };

        let bounds = inverse_transformed_bounds(page_bounds, transform).unwrap();

        let right = bounds.x.value() + bounds.width.value();
        let bottom = bounds.y.value() + bounds.height.value();

        // Floating-point sin/cos can put an exact right angle a fraction of a
        // micrometre beyond its mathematical result. The production contract
        // is conservative enclosure, not brittle equality at that boundary.
        assert!((9_999..=10_000).contains(&bounds.x.value()));
        assert!((19_999..=20_000).contains(&bounds.y.value()));
        assert!((40_000..=40_001).contains(&right));
        assert!((40_000..=40_001).contains(&bottom));
    }
}
