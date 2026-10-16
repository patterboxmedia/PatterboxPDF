//! Worker-free ordinary drawing, and CMYK/Gray print drawing with a checked profile.
use print_core::finished::{
    AffineTransform, BlendMode, FillRule, FinishedContent, FinishedDocument, FinishedGradientFill,
    FinishedGroup, FinishedItem, FinishedOutputMode, FinishedPage, FinishedPageBoxes, FinishedPath,
    FinishedPathPaint, FinishedPdfOptions, FinishedResources, Gradient, GradientGeometry,
    GradientStop, ImagePlacement, ImageResource, Paint, PathCommand, PdfBox, Point,
};
use print_core::{FinishedPdfWriter, PdfCandidateMetadata};
use std::error::Error;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const CREATED: &str = "2026-10-04T00:00:00Z";
const BLENDS: [BlendMode; 16] = [
    BlendMode::Normal,
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::Darken,
    BlendMode::Lighten,
    BlendMode::ColorDodge,
    BlendMode::ColorBurn,
    BlendMode::HardLight,
    BlendMode::SoftLight,
    BlendMode::Difference,
    BlendMode::Exclusion,
    BlendMode::Hue,
    BlendMode::Saturation,
    BlendMode::Color,
    BlendMode::Luminosity,
];

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("feature gallery: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut output = None;
    let mut profile = None;
    let mut mode = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--help" {
            println!(
                "feature-gallery --output NEW_DIRECTORY --mode ordinary\nfeature-gallery --output NEW_DIRECTORY --mode pdfx4 --profile CMYK.icc\nCreates a three-page native feature gallery with fixed metadata.\nOrdinary drawing needs no workers; print profile checking needs the color worker."
            );
            return Ok(());
        }
        let value = args.next().ok_or("option requires a value")?;
        match flag.to_str() {
            Some("--output") if output.is_none() => output = Some(PathBuf::from(value)),
            Some("--profile") if profile.is_none() => profile = Some(PathBuf::from(value)),
            Some("--mode") if mode.is_none() => mode = Some(value),
            _ => return Err("unknown or repeated option; use --help".into()),
        }
    }
    let output = output.ok_or("missing --output; use --help")?;
    let print = match mode.as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("ordinary") if profile.is_none() => false,
        Some("pdfx4") if profile.is_some() => true,
        _ => return Err("use --mode ordinary, or --mode pdfx4 with --profile".into()),
    };
    let profile_bytes = profile.map(fs::read).transpose()?;
    let document = gallery(print)?;
    let metadata = PdfCandidateMetadata::at_utc("Native feature gallery", CREATED)
        .ok_or("invalid fixed timestamp")?;
    let options = FinishedPdfOptions {
        metadata,
        mode: match &profile_bytes {
            Some(bytes) => FinishedOutputMode::PdfX4 {
                output_profile: bytes,
            },
            None => FinishedOutputMode::Ordinary,
        },
    };
    // All resource admission, print profile preparation and serialization finish
    // before the example exposes an output directory. Preserve existing output.
    let pdf = FinishedPdfWriter::write(&document, &options)?;
    fs::create_dir(&output)?;
    let name = if print {
        "candidate.pdf"
    } else {
        "ordinary.pdf"
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join(name))?;
    file.write_all(pdf.bytes())?;
    file.sync_all()?;
    println!("created {name}; pages=3; metadata_utc={CREATED}");
    Ok(())
}

fn gallery(print: bool) -> Result<FinishedDocument, Box<dyn Error>> {
    // These are decoded, top-to-bottom 8-bit planes, not PNG/JPEG file bytes.
    // Alpha is an independent straight-opacity plane, with no premultiplication.
    let alpha = [
        255, 190, 120, 40, 190, 120, 40, 255, 120, 40, 255, 190, 40, 255, 190, 120,
    ];
    let rgb = [
        255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 200, 0, 0, 255, 0, 0, 0, 255, 255, 200, 0, 255, 0, 0,
        0, 0, 255, 255, 200, 0, 255, 0, 0, 0, 255, 0, 255, 200, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255,
    ];
    let gray = [
        0, 85, 170, 255, 85, 170, 255, 0, 170, 255, 0, 85, 255, 0, 85, 170,
    ];
    let cmyk = [
        255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0, 255, 0, 0, 0, 0, 255, 0, 0, 0,
        0, 255, 255, 0, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 0, 255,
        255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0,
    ];
    let primary = if print {
        ImageResource::from_decoded_cmyk("primary".into(), 4, 4, &cmyk, Some(&alpha))?
    } else {
        ImageResource::from_decoded_rgb("primary".into(), 4, 4, &rgb, Some(&alpha))?
    };
    Ok(FinishedDocument {
        resources: FinishedResources {
            fonts: vec![],
            images: vec![
                primary,
                ImageResource::from_decoded_gray("gray".into(), 4, 4, &gray, Some(&alpha))?,
                ImageResource::from_decoded_cmyk("cmyk".into(), 4, 4, &cmyk, Some(&alpha))?,
            ],
        },
        pages: vec![color_page(print), components_page(), blend_page(print)],
    })
}

fn color_page(print: bool) -> FinishedPage {
    let colors = palette(print, 1.0);
    let mut items = Vec::new();
    for (id, x) in [("primary", 35.0), ("gray", 215.0), ("cmyk", 395.0)] {
        items.push(solid(
            rectangle(x, 505.0, x + 160.0, 605.0),
            Paint::gray(0.85, 1.0),
        ));
        let mut picture = FinishedItem::new(FinishedContent::Image(ImagePlacement {
            image: id.into(),
            alpha: 1.0,
        }));
        picture.transform = AffineTransform([160.0, 0.0, 0.0, 100.0, x, 505.0]);
        items.push(picture);
    }
    items.push(axial(
        35.0,
        350.0,
        &[(0.0, colors[0]), (0.35, colors[1]), (1.0, colors[2])],
    ));
    items.push(radial(
        335.0,
        350.0,
        &[(0.0, colors[2]), (0.45, colors[1]), (1.0, colors[0])],
    ));
    // Equal interior offsets make a visible hard edge, with endpoint padding.
    items.push(axial(
        35.0,
        195.0,
        &[
            (0.0, colors[0]),
            (0.5, colors[0]),
            (0.5, colors[2]),
            (1.0, colors[2]),
        ],
    ));
    items.push(solid(
        rectangle(335.0, 195.0, 575.0, 305.0),
        Paint::gray(0.7, 1.0),
    ));
    let transparent = palette(print, 0.0);
    items.push(radial(
        335.0,
        195.0,
        &[
            (0.0, colors[1]),
            (0.5, palette(print, 0.5)[2]),
            (1.0, transparent[0]),
        ],
    ));
    // This probe combines overlapping inner paints before applying opacity once.
    let mut group = FinishedItem::new(FinishedContent::Group(FinishedGroup {
        bounds: PdfBox::new(0.0, 0.0, 240.0, 90.0),
        opacity: 0.5,
        items: vec![
            solid(rectangle(0.0, 0.0, 160.0, 90.0), colors[0]),
            solid(rectangle(80.0, 0.0, 240.0, 90.0), colors[2]),
        ],
    }));
    group.transform = AffineTransform([1.0, 0.0, 0.0, 1.0, 35.0, 45.0]);
    items.push(group);
    items.push(axial(
        335.0,
        35.0,
        &[
            (0.0, transparent[0]),
            (0.5, palette(print, 0.6)[1]),
            (1.0, colors[2]),
        ],
    ));
    page(items)
}

fn components_page() -> FinishedPage {
    let gray = [
        Paint::gray(0.0, 1.0),
        Paint::gray(0.6, 1.0),
        Paint::gray(1.0, 1.0),
    ];
    let cmyk = palette(true, 1.0);
    let mut items = vec![
        axial(
            35.0,
            480.0,
            &[(0.0, gray[0]), (0.35, gray[1]), (1.0, gray[2])],
        ),
        radial(
            335.0,
            480.0,
            &[(0.0, gray[2]), (0.45, gray[1]), (1.0, gray[0])],
        ),
        axial(
            35.0,
            315.0,
            &[(0.0, cmyk[0]), (0.35, cmyk[1]), (1.0, cmyk[2])],
        ),
        radial(
            335.0,
            315.0,
            &[(0.0, cmyk[2]), (0.45, cmyk[1]), (1.0, cmyk[0])],
        ),
        axial(
            35.0,
            155.0,
            &[
                (0.0, gray[0]),
                (0.5, gray[0]),
                (0.5, gray[2]),
                (1.0, gray[2]),
            ],
        ),
        axial(
            335.0,
            155.0,
            &[
                (0.0, cmyk[0]),
                (0.5, cmyk[0]),
                (0.5, cmyk[1]),
                (1.0, cmyk[1]),
            ],
        ),
    ];
    let mut clipped = radial(
        0.0,
        0.0,
        &[
            (0.0, Paint::gray(0.0, 0.8)),
            (0.5, Paint::gray(0.5, 0.45)),
            (1.0, Paint::gray(1.0, 0.0)),
        ],
    );
    clipped.transform = AffineTransform([0.9, 0.12, 0.22, 0.65, 35.0, 30.0]);
    clipped.clip = Some(triangle(10.0, 5.0, 230.0, 105.0));
    items.push(clipped);
    let mut clipped = radial(
        0.0,
        0.0,
        &[
            (0.0, cmyk[0]),
            (0.5, palette(true, 0.5)[1]),
            (1.0, palette(true, 0.0)[2]),
        ],
    );
    clipped.transform = AffineTransform([0.9, -0.08, 0.2, 0.7, 335.0, 45.0]);
    clipped.clip = Some(triangle(10.0, 5.0, 230.0, 105.0));
    items.push(clipped);
    page(items)
}

fn blend_page(print: bool) -> FinishedPage {
    let colors = palette(print, 1.0);
    let mut items = Vec::new();
    // Four columns, read left to right and top to bottom; README names each cell.
    for (mode, (column, row)) in BLENDS
        .into_iter()
        .zip((0_i16..4).flat_map(|row| (0_i16..4).map(move |column| (column, row))))
    {
        let x = 30.0 + f32::from(column) * 145.0;
        let y = 485.0 - f32::from(row) * 145.0;
        items.push(solid(
            rectangle(x, y, x + 120.0, y + 120.0),
            Paint::gray(0.8, 1.0),
        ));
        items.push(solid(
            rectangle(x + 5.0, y + 10.0, x + 85.0, y + 95.0),
            colors[0],
        ));
        items.push(solid(
            rectangle(x + 45.0, y + 35.0, x + 115.0, y + 110.0),
            colors[1],
        ));
        let mut group = FinishedItem::new(FinishedContent::Group(FinishedGroup {
            bounds: PdfBox::new(0.0, 0.0, 100.0, 75.0),
            opacity: 0.75,
            items: vec![
                solid(rectangle(0.0, 0.0, 70.0, 65.0), colors[2]),
                solid(rectangle(35.0, 15.0, 100.0, 75.0), Paint::gray(0.4, 1.0)),
            ],
        }));
        group.transform = AffineTransform([1.0, 0.0, 0.0, 1.0, x + 10.0, y + 5.0]);
        group.blend_mode = mode;
        items.push(group);
    }
    page(items)
}

fn palette(print: bool, alpha: f32) -> [Paint; 3] {
    if print {
        [
            Paint::cmyk([1.0, 0.0, 0.0, 0.0], alpha),
            Paint::cmyk([0.0, 1.0, 0.0, 0.0], alpha),
            Paint::cmyk([0.0, 0.0, 1.0, 0.0], alpha),
        ]
    } else {
        [
            Paint::rgb([1.0, 0.0, 0.0], alpha),
            Paint::rgb([0.0, 1.0, 0.0], alpha),
            Paint::rgb([0.0, 0.0, 1.0], alpha),
        ]
    }
}

fn axial(x: f32, y: f32, stops: &[(f32, Paint)]) -> FinishedItem {
    gradient(
        x,
        y,
        GradientGeometry::Axial {
            start: Point::new(20.0, 0.0),
            end: Point::new(220.0, 0.0),
        },
        stops,
    )
}

fn radial(x: f32, y: f32, stops: &[(f32, Paint)]) -> FinishedItem {
    gradient(
        x,
        y,
        GradientGeometry::Radial {
            start_center: Point::new(120.0, 55.0),
            start_radius: 0.0,
            end_center: Point::new(120.0, 55.0),
            end_radius: 80.0,
        },
        stops,
    )
}

fn gradient(x: f32, y: f32, geometry: GradientGeometry, stops: &[(f32, Paint)]) -> FinishedItem {
    let mut item = FinishedItem::new(FinishedContent::Gradient(FinishedGradientFill {
        path: rectangle(0.0, 0.0, 240.0, 110.0),
        gradient: Gradient {
            geometry,
            stops: stops
                .iter()
                .map(|&(offset, paint)| GradientStop { offset, paint })
                .collect(),
        },
    }));
    item.transform = AffineTransform([1.0, 0.0, 0.0, 1.0, x, y]);
    item
}

fn solid(path: FinishedPath, paint: Paint) -> FinishedItem {
    FinishedItem::new(FinishedContent::Path(FinishedPathPaint {
        path,
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

fn triangle(x1: f32, y1: f32, x2: f32, y2: f32) -> FinishedPath {
    FinishedPath {
        fill_rule: FillRule::NonZero,
        commands: vec![
            PathCommand::MoveTo(Point::new(x1, y1)),
            PathCommand::LineTo(Point::new(x2, y1)),
            PathCommand::LineTo(Point::new(x1.midpoint(x2), y2)),
            PathCommand::Close,
        ],
    }
}

fn page(items: Vec<FinishedItem>) -> FinishedPage {
    let bounds = PdfBox::new(0.0, 0.0, 610.0, 650.0);
    FinishedPage {
        boxes: FinishedPageBoxes {
            media: bounds,
            bleed: bounds,
            trim: bounds,
        },
        items,
    }
}
