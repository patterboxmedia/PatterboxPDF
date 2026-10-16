use super::{Millipoints, ResolvedTextLinePreview};
use crate::text::ResolvedOutlineCommand;

/// Conservative bounds include control points, generated list markers and
/// underline paint. Rounded Fit boxes must preserve ink beyond glyph advances.
pub(super) fn fit_ink_bounds(
    lines: &[ResolvedTextLinePreview],
    width: Millipoints,
    height: Millipoints,
) -> [i64; 4] {
    let mut bounds = [0, 0, width.value(), height.value()];
    let mut include = |x: i64, y: i64| {
        bounds[0] = bounds[0].min(x);
        bounds[1] = bounds[1].min(y);
        bounds[2] = bounds[2].max(x);
        bounds[3] = bounds[3].max(y);
    };
    for run in lines.iter().flat_map(|line| &line.glyph_runs) {
        if let Some(underline) = &run.underline {
            include(underline.x.value(), underline.y.value());
            include(
                underline.x.value().saturating_add(underline.width.value()),
                underline
                    .y
                    .value()
                    .saturating_add(underline.thickness.value()),
            );
        }
        for glyph in &run.glyphs {
            let mut point = |x: Millipoints, y: Millipoints| {
                include(
                    glyph.x.value().saturating_add(x.value()),
                    glyph.y.value().saturating_sub(y.value()),
                );
            };
            if let Some(outline) = &glyph.outline {
                for command in &outline.commands {
                    match *command {
                        ResolvedOutlineCommand::MoveTo { x, y }
                        | ResolvedOutlineCommand::LineTo { x, y } => point(x, y),
                        ResolvedOutlineCommand::QuadTo {
                            control_x,
                            control_y,
                            x,
                            y,
                        } => {
                            point(control_x, control_y);
                            point(x, y);
                        }
                        ResolvedOutlineCommand::CurveTo {
                            control_1_x,
                            control_1_y,
                            control_2_x,
                            control_2_y,
                            x,
                            y,
                        } => {
                            point(control_1_x, control_1_y);
                            point(control_2_x, control_2_y);
                            point(x, y);
                        }
                        ResolvedOutlineCommand::Close => {}
                    }
                }
            }
        }
    }
    bounds
}
