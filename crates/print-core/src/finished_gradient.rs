//! Native functions/shadings and a matching straight-opacity luminosity mask.
use crate::finished::{
    FinishedPath, FinishedPdfError, Gradient, GradientGeometry, PaintColor, PathCommand, PdfBox,
    Point,
};
use pdf_writer::types::{FunctionShadingType, MaskType};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref};

pub(crate) struct GradientReferences {
    pub shading: Ref,
    pub mask_state: Option<Ref>,
}

#[expect(
    clippy::float_cmp,
    reason = "only exactly equal authored opacities can reuse a scalar state"
)]
pub(crate) fn constant_alpha(gradient: &Gradient) -> Option<f32> {
    let first = gradient.stops.first()?.paint.alpha;
    gradient
        .stops
        .iter()
        .all(|s| s.paint.alpha == first)
        .then_some(first)
}

/// Returns the actual interpolation-function count, including stitching functions.
#[expect(
    clippy::float_cmp,
    reason = "endpoint uniqueness and hard stops use exact authored f32 positions"
)]
pub(crate) fn validate(
    gradient: &Gradient,
    maximum_point: f32,
    pdfx: bool,
) -> Result<usize, FinishedPdfError> {
    let stops = &gradient.stops;
    if stops.len() < 2
        || stops[0].offset != 0.0
        || stops[stops.len() - 1].offset != 1.0
        || stops[1].offset == 0.0
        || stops[stops.len() - 2].offset == 1.0
        || stops.windows(2).any(|s| s[0].offset > s[1].offset)
        || stops.iter().any(|s| {
            !unit(s.offset)
                || !unit(s.paint.alpha)
                || !valid_color(s.paint.color)
                || !same_space(stops[0].paint.color, s.paint.color)
        })
        || !valid_geometry(gradient.geometry, maximum_point)
    {
        return Err(FinishedPdfError::InvalidGradient);
    }
    if pdfx {
        // Table C.1 receivers can turn nonzero subnormals into zero. Reject
        // only a resulting interval/geometry collapse; ordinary f32 values
        // remain valid and no arbitrary minimum stop spacing is introduced.
        if stops.windows(2).any(|s| {
            s[0].offset < s[1].offset && receiver_real(s[0].offset) >= receiver_real(s[1].offset)
        }) || !valid_geometry(receiver_geometry(gradient.geometry), maximum_point)
        {
            return Err(FinishedPdfError::InvalidGradient);
        }
    }
    let segments = stops
        .windows(2)
        .filter(|s| s[0].offset < s[1].offset)
        .count();
    Ok((segments + 1)
        * if constant_alpha(gradient).is_some() {
            1
        } else {
            2
        })
}

fn unit(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}
fn same_space(a: PaintColor, b: PaintColor) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}
fn valid_color(color: PaintColor) -> bool {
    match color {
        PaintColor::Gray(v) => unit(v),
        PaintColor::Rgb(v) => v.into_iter().all(unit),
        PaintColor::Cmyk(v) => v.into_iter().all(unit),
    }
}
#[expect(
    clippy::float_cmp,
    reason = "reject exactly identical radial circles without an arbitrary geometry epsilon"
)]
fn valid_geometry(geometry: GradientGeometry, maximum_point: f32) -> bool {
    let bounded = |v: f32| v.is_finite() && v.abs() <= maximum_point;
    match geometry {
        GradientGeometry::Axial { start, end } => {
            [start.x, start.y, end.x, end.y].into_iter().all(bounded) && start != end
        }
        GradientGeometry::Radial {
            start_center,
            start_radius,
            end_center,
            end_radius,
        } => {
            [
                start_center.x,
                start_center.y,
                start_radius,
                end_center.x,
                end_center.y,
                end_radius,
            ]
            .into_iter()
            .all(bounded)
                && start_radius >= 0.0
                && end_radius >= 0.0
                && (start_radius > 0.0 || end_radius > 0.0)
                && (start_center != end_center || start_radius != end_radius)
        }
    }
}
fn receiver_real(value: f32) -> f32 {
    if value != 0.0 && value.abs() < f32::MIN_POSITIVE {
        0.0
    } else {
        value
    }
}
fn receiver_point(p: Point) -> Point {
    Point::new(receiver_real(p.x), receiver_real(p.y))
}
fn receiver_geometry(geometry: GradientGeometry) -> GradientGeometry {
    match geometry {
        GradientGeometry::Axial { start, end } => GradientGeometry::Axial {
            start: receiver_point(start),
            end: receiver_point(end),
        },
        GradientGeometry::Radial {
            start_center,
            start_radius,
            end_center,
            end_radius,
        } => GradientGeometry::Radial {
            start_center: receiver_point(start_center),
            start_radius: receiver_real(start_radius),
            end_center: receiver_point(end_center),
            end_radius: receiver_real(end_radius),
        },
    }
}

/// The control-point hull conservatively encloses every cubic, including its
/// extrema. This bounds the mask without approximating or rasterizing the fill.
pub(crate) fn path_bounds(path: &FinishedPath) -> Result<PdfBox, FinishedPdfError> {
    let mut bounds = PdfBox::new(
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    let mut include = |p: Point| {
        bounds.x1 = bounds.x1.min(p.x);
        bounds.y1 = bounds.y1.min(p.y);
        bounds.x2 = bounds.x2.max(p.x);
        bounds.y2 = bounds.y2.max(p.y);
    };
    for command in &path.commands {
        match *command {
            PathCommand::MoveTo(p) | PathCommand::LineTo(p) => include(p),
            PathCommand::CubicTo(a, b, c) => {
                include(a);
                include(b);
                include(c);
            }
            PathCommand::Close => {}
        }
    }
    if bounds.x2 > bounds.x1 && bounds.y2 > bounds.y1 {
        Ok(bounds)
    } else {
        Err(FinishedPdfError::InvalidGradient)
    }
}

pub(crate) fn shading_name(reference: Ref) -> String {
    format!("S{}", reference.get())
}
pub(crate) fn mask_name(reference: Ref) -> String {
    format!("T{}", reference.get())
}

pub(crate) fn emit(
    pdf: &mut Pdf,
    gradient: &Gradient,
    bounds: PdfBox,
    mut alloc: impl FnMut() -> Result<Ref, FinishedPdfError>,
) -> Result<GradientReferences, FinishedPdfError> {
    let color_function = emit_function(pdf, gradient, false, &mut alloc)?;
    let shading = alloc()?;
    emit_shading(pdf, shading, color_function, gradient, false);
    let mask_state = if constant_alpha(gradient).is_none() {
        let alpha_function = emit_function(pdf, gradient, true, &mut alloc)?;
        let alpha_shading = alloc()?;
        emit_shading(pdf, alpha_shading, alpha_function, gradient, true);
        let form_ref = alloc()?;
        let state_ref = alloc()?;
        let name = shading_name(alpha_shading);
        // No explicit q is needed in this one-operation Form. Its invocation
        // adds exactly one implicit save to the executed graphics-state path.
        let mut content = Content::new();
        content.shading(Name(name.as_bytes()));
        let bytes = content.finish();
        let mut form = pdf.form_xobject(form_ref, &bytes);
        form.bbox(Rect::new(bounds.x1, bounds.y1, bounds.x2, bounds.y2));
        form.resources()
            .shadings()
            .pair(Name(name.as_bytes()), alpha_shading);
        {
            let mut group = form.group();
            group.transparency().isolated(true).knockout(false);
            group.color_space().device_gray();
        }
        form.finish();
        pdf.ext_graphics(state_ref)
            .soft_mask()
            .subtype(MaskType::Luminosity)
            .group(form_ref)
            .backdrop([0.0]);
        Some(state_ref)
    } else {
        None
    };
    Ok(GradientReferences {
        shading,
        mask_state,
    })
}

fn components(color: PaintColor) -> Vec<f32> {
    match color {
        PaintColor::Gray(c) => vec![c],
        PaintColor::Rgb(c) => c.to_vec(),
        PaintColor::Cmyk(c) => c.to_vec(),
    }
}
fn emit_function(
    pdf: &mut Pdf,
    gradient: &Gradient,
    alpha: bool,
    alloc: &mut impl FnMut() -> Result<Ref, FinishedPdfError>,
) -> Result<Ref, FinishedPdfError> {
    let mut functions = Vec::new();
    let mut bounds = Vec::new();
    for pair in gradient
        .stops
        .windows(2)
        .filter(|s| s[0].offset < s[1].offset)
    {
        let reference = alloc()?;
        let c0 = if alpha {
            vec![pair[0].paint.alpha]
        } else {
            components(pair[0].paint.color)
        };
        let c1 = if alpha {
            vec![pair[1].paint.alpha]
        } else {
            components(pair[1].paint.color)
        };
        pdf.exponential_function(reference)
            .domain([0.0, 1.0])
            .c0(c0)
            .c1(c1)
            .n(1.0);
        functions.push(reference);
        if pair[1].offset < 1.0 {
            bounds.push(pair[1].offset);
        }
    }
    let reference = alloc()?;
    pdf.stitching_function(reference)
        .domain([0.0, 1.0])
        .functions(functions.iter().copied())
        .bounds(bounds)
        .encode(functions.iter().flat_map(|_| [0.0, 1.0]));
    Ok(reference)
}
fn emit_shading(pdf: &mut Pdf, reference: Ref, function: Ref, gradient: &Gradient, alpha: bool) {
    let mut shading = pdf.function_shading(reference);
    // Axial/radial Domain defaults to [0 1]. The pinned helper accepts four
    // values for that entry, so omit it rather than emit an invalid array.
    match gradient.geometry {
        GradientGeometry::Axial { start, end } => {
            shading
                .shading_type(FunctionShadingType::Axial)
                .coords([start.x, start.y, end.x, end.y]);
        }
        GradientGeometry::Radial {
            start_center,
            start_radius,
            end_center,
            end_radius,
        } => {
            shading.shading_type(FunctionShadingType::Radial).coords([
                start_center.x,
                start_center.y,
                start_radius,
                end_center.x,
                end_center.y,
                end_radius,
            ]);
        }
    }
    {
        let space = shading.color_space();
        if alpha {
            space.device_gray();
        } else {
            match gradient.stops[0].paint.color {
                PaintColor::Gray(_) => {
                    space.device_gray();
                }
                PaintColor::Rgb(_) => {
                    space.device_rgb();
                }
                PaintColor::Cmyk(_) => {
                    space.device_cmyk();
                }
            }
        }
    }
    shading.function(function).extend([true, true]);
}
