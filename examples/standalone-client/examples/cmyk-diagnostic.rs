//! Worker-free native gradients beside independently authored flat controls.
use print_core::finished::{
    AffineTransform, FillRule, FinishedContent, FinishedDocument, FinishedGradientFill,
    FinishedGroup, FinishedItem, FinishedOutputMode, FinishedPage, FinishedPageBoxes, FinishedPath,
    FinishedPathPaint, FinishedPdfOptions, FinishedResources, Gradient, GradientGeometry,
    GradientStop, Paint, PathCommand, PdfBox, Point,
};
use print_core::{FinishedPdfWriter, PdfCandidateMetadata};
use std::{error::Error, fs::OpenOptions, io::Write, path::PathBuf};

const C0: [f32; 4] = [0.0, 1.0, 0.0, 0.0];
const C1: [f32; 4] = [1.0, 0.0, 1.0, 0.25];
const SAMPLES: [f32; 5] = [0.125, 0.25, 0.5, 0.75, 0.875];

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = match (args.next(), args.next(), args.next()) {
        (Some(flag), Some(value), None) if flag == "--output" => PathBuf::from(value),
        _ => return Err("usage: cmyk-diagnostic --output NEW_FILE.pdf".into()),
    };
    let document = FinishedDocument {
        resources: FinishedResources::default(),
        pages: vec![page(false), page(true)],
    };
    let options = FinishedPdfOptions {
        metadata: PdfCandidateMetadata::at_utc("CMYK diagnostic controls", "2026-10-16T00:00:00Z")
            .ok_or("invalid fixed timestamp")?,
        mode: FinishedOutputMode::Ordinary,
    };
    let pdf = FinishedPdfWriter::write(&document, &options)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(pdf.bytes())?;
    file.sync_all()?;
    Ok(())
}

fn page(rgb_group: bool) -> FinishedPage {
    let mut items = Vec::new();
    for case in 0..8 {
        let y = 864.0 - f32::from(case) * 112.0;
        items.push(solid(32.0, y, 256.0, Paint::gray(0.8, 1.0)));
        items.push(solid(352.0, y, 256.0, Paint::gray(0.8, 1.0)));
        let mut gradient = FinishedItem::new(FinishedContent::Gradient(FinishedGradientFill {
            path: rectangle(0.0, 0.0, 256.0, 48.0),
            gradient: Gradient {
                geometry: if matches!(case, 1 | 5) {
                    GradientGeometry::Radial {
                        start_center: Point::new(0.0, 24.0),
                        start_radius: 0.0,
                        end_center: Point::new(0.0, 24.0),
                        end_radius: 256.0,
                    }
                } else {
                    GradientGeometry::Axial {
                        start: Point::new(0.0, 0.0),
                        end: Point::new(256.0, 0.0),
                    }
                },
                stops: if case == 2 {
                    [(0.0, C0), (0.5, C0), (0.5, C1), (1.0, C1)]
                        .map(|(offset, color)| GradientStop {
                            offset,
                            paint: Paint::cmyk(color, 1.0),
                        })
                        .to_vec()
                } else {
                    [0.0, 1.0]
                        .map(|offset| GradientStop {
                            offset,
                            paint: sample(case, offset),
                        })
                        .to_vec()
                },
            },
        }));
        gradient.transform = AffineTransform([1.0, 0.0, 0.0, 1.0, 32.0, y]);
        let controls = SAMPLES
            .into_iter()
            .enumerate()
            .map(|(index, t)| {
                let index = f32::from(u8::try_from(index).expect("five samples"));
                solid(352.0 + index * 52.0, y, 48.0, sample(case, t))
            })
            .collect::<Vec<_>>();
        if case >= 6 {
            // Each side is composed before applying group opacity exactly once.
            items.push(group(vec![gradient], 0.5));
            items.push(group(controls, 0.5));
        } else {
            items.push(gradient);
            items.extend(controls);
        }
    }
    if rgb_group {
        // A visible one-point RGB marker selects the automatic RGB page blending
        // space. It is outside every sampled panel, whose components are identical.
        items.push(FinishedItem::new(FinishedContent::Path(
            FinishedPathPaint {
                path: rectangle(2.0, 2.0, 3.0, 3.0),
                fill: Some(Paint::rgb([1.0, 1.0, 1.0], 1.0)),
                stroke: None,
            },
        )));
    }
    let bounds = PdfBox::new(0.0, 0.0, 640.0, 960.0);
    FinishedPage {
        boxes: FinishedPageBoxes {
            media: bounds,
            bleed: bounds,
            trim: bounds,
        },
        items,
    }
}

fn sample(case: u8, t: f32) -> Paint {
    let fraction = if case == 2 {
        if t < 0.5 { 0.0 } else { 1.0 }
    } else {
        t
    };
    let color = std::array::from_fn(|i| C0[i] + (C1[i] - C0[i]) * fraction);
    let alpha = match case {
        3 => 0.5,
        4 | 5 | 7 => t,
        _ => 1.0,
    };
    Paint::cmyk(color, alpha)
}

fn group(items: Vec<FinishedItem>, opacity: f32) -> FinishedItem {
    FinishedItem::new(FinishedContent::Group(FinishedGroup {
        bounds: PdfBox::new(0.0, 0.0, 640.0, 960.0),
        opacity,
        items,
    }))
}

fn solid(x: f32, y: f32, width: f32, paint: Paint) -> FinishedItem {
    FinishedItem::new(FinishedContent::Path(FinishedPathPaint {
        path: rectangle(x, y, x + width, y + 48.0),
        fill: Some(paint),
        stroke: None,
    }))
}

fn rectangle(x1: f32, y1: f32, x2: f32, y2: f32) -> FinishedPath {
    FinishedPath {
        fill_rule: FillRule::NonZero,
        commands: vec![
            PathCommand::MoveTo(Point::new(x1, y1)),
            PathCommand::LineTo(Point::new(x2, y1)),
            PathCommand::LineTo(Point::new(x2, y2)),
            PathCommand::LineTo(Point::new(x1, y2)),
            PathCommand::Close,
        ],
    }
}
