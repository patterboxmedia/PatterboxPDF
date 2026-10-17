//! Deterministic public-API controls for non-font composition inputs.
//! Ordinary generation is worker-free; optional print generation checks the
//! supplied exact CMYK profile through the configured native color worker.
use print_core::finished::{
    AffineTransform, BlendMode, FillRule, FinishedContent, FinishedDocument, FinishedGradientFill,
    FinishedGroup, FinishedItem, FinishedOutputMode, FinishedPage, FinishedPageBoxes, FinishedPath,
    FinishedPathPaint, FinishedPdfError, FinishedPdfOptions, FinishedResources, Gradient,
    GradientGeometry, GradientStop, ImagePlacement, ImageResource, LineCap, LineJoin, Paint,
    PathCommand, PdfBox, Point, Stroke,
};
use print_core::{FinishedPdfWriter, PdfCandidateMetadata};
use std::error::Error;
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

const TITLE: &str = "Composition (\\) <&>\rUnicode \u{00e9} \u{1f642}";
const CREATED: &str = "2017-01-01T00:59:60+01:00";
const ALPHA: [u8; 4] = [0, 64, 128, 255];
const GRAY: [u8; 4] = [0, 85, 170, 255];
const RGB: [u8; 12] = [255, 0, 32, 0, 255, 64, 16, 32, 255, 255, 128, 0];
const CMYK: [u8; 16] = [255, 0, 32, 0, 0, 255, 64, 0, 16, 32, 255, 0, 0, 0, 0, 255];
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

struct Output {
    name: &'static str,
    mode: &'static str,
    reversed: bool,
    bytes: Vec<u8>,
}

struct Rejection {
    case: &'static str,
    mode: &'static str,
    error: &'static str,
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("accepted composition inputs: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut output = None;
    let mut profile = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--help" {
            println!(
                "accepted-composition-input --output NEW_DIRECTORY [--profile CMYK.icc]\nCreates ordinary base/reordered PDFs and, with a profile, print base/reordered PDFs.\nAll writes and rejection controls finish before NEW_DIRECTORY is created."
            );
            return Ok(());
        }
        let value = args.next().ok_or("option requires a value")?;
        match flag.to_str() {
            Some("--output") if output.is_none() => output = Some(PathBuf::from(value)),
            Some("--profile") if profile.is_none() => profile = Some(PathBuf::from(value)),
            _ => return Err("unknown or repeated option; use --help".into()),
        }
    }
    let output = output.ok_or("missing --output; use --help")?;
    let profile = profile.map(fs::read).transpose()?;
    let mut outputs = Vec::new();
    let mut rejected = rejection_controls(&options(None), false)?;
    for (print, bytes) in [(false, None), (true, profile.as_deref())] {
        if print && bytes.is_none() {
            continue;
        }
        let options = options(bytes);
        for reversed in [false, true] {
            let document = corpus(print, reversed)?;
            let first = FinishedPdfWriter::write(&document, &options)?.into_bytes();
            let second = FinishedPdfWriter::write(&document, &options)?;
            if first != second.bytes() {
                return Err("identical inputs produced different bytes".into());
            }
            outputs.push(Output {
                name: match (print, reversed) {
                    (false, false) => "ordinary-base.pdf",
                    (false, true) => "ordinary-reordered.pdf",
                    (true, false) => "print-base.pdf",
                    (true, true) => "print-reordered.pdf",
                },
                mode: if print { "pdfx4" } else { "ordinary" },
                reversed,
                bytes: first,
            });
        }
        if print {
            rejected.extend(rejection_controls(&options, true)?);
        }
    }
    let manifest = manifest(&outputs, &rejected, profile.is_some())?;
    // create_dir and create_new preserve an existing caller directory. Every
    // writer outcome is established before any generated result is exposed.
    fs::create_dir(&output)?;
    for generated in &outputs {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output.join(generated.name))?;
        file.write_all(&generated.bytes)?;
        file.sync_all()?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("manifest.json"))?;
    file.write_all(manifest.as_bytes())?;
    file.sync_all()?;
    println!(
        "created {} PDFs; {} matched rejections; manifest.json",
        outputs.len(),
        rejected.len()
    );
    Ok(())
}

fn options(profile: Option<&[u8]>) -> FinishedPdfOptions<'_> {
    FinishedPdfOptions {
        metadata: PdfCandidateMetadata::at_utc(TITLE, CREATED).expect("fixed valid leap timestamp"),
        mode: profile.map_or(FinishedOutputMode::Ordinary, |output_profile| {
            FinishedOutputMode::PdfX4 { output_profile }
        }),
    }
}

fn corpus(print: bool, reversed: bool) -> Result<FinishedDocument, FinishedPdfError> {
    let mut images = Vec::new();
    for space in ["gray", "cmyk", "rgb"] {
        if print && space == "rgb" {
            continue;
        }
        for suffix in ["opaque", "alpha", "unused-opaque", "unused-alpha"] {
            let alpha = suffix.ends_with("alpha").then_some(ALPHA.as_slice());
            let id = format!("{space}-{suffix}");
            images.push(match space {
                "gray" => ImageResource::from_decoded_gray(id, 2, 2, &GRAY, alpha)?,
                "cmyk" => ImageResource::from_decoded_cmyk(id, 2, 2, &CMYK, alpha)?,
                _ => ImageResource::from_decoded_rgb(id, 2, 2, &RGB, alpha)?,
            });
        }
    }
    if reversed {
        images.reverse();
    }
    Ok(FinishedDocument {
        resources: FinishedResources {
            fonts: vec![],
            images,
        },
        pages: vec![
            paths_page(print),
            images_page(print),
            gradients_page(print),
            blends_page(print),
        ],
    })
}

fn paths_page(print: bool) -> FinishedPage {
    let mut items = Vec::new();
    for (rule, x) in [(FillRule::NonZero, 30.0), (FillRule::EvenOdd, 230.0)] {
        // Both subpaths have the same winding. The center remains filled under
        // NonZero and becomes a hole under EvenOdd.
        let mut ring = rectangle(0.0, 0.0, 140.0, 90.0, rule);
        ring.commands
            .extend(rectangle(35.0, 20.0, 105.0, 70.0, rule).commands);
        for (both, y) in [(false, 440.0), (true, 320.0)] {
            let mut item = solid(ring.clone(), color(print, 0, 0.75));
            if both && let FinishedContent::Path(path) = &mut item.content {
                path.stroke = Some(stroke(LineCap::Round, LineJoin::Miter, 2.0, vec![], 0.0));
            }
            item.transform = AffineTransform([1.0, 0.0, 0.125, 1.0, x, y]);
            items.push(item);
        }
    }
    for (cap, column) in [
        (LineCap::Butt, 0_i16),
        (LineCap::Round, 1),
        (LineCap::Square, 2),
    ] {
        for (join, row) in [
            (LineJoin::Miter, 0_i16),
            (LineJoin::Round, 1),
            (LineJoin::Bevel, 2),
        ] {
            let (width, dash) = match row {
                0 => (0.0, vec![]), // PDF hairline is an accepted width.
                1 => (2.0, vec![0.0, 4.0]),
                _ => (6.0, vec![3.0, 1.0, 2.0]),
            };
            let mut item = FinishedItem::new(FinishedContent::Path(FinishedPathPaint {
                path: FinishedPath {
                    commands: vec![
                        PathCommand::MoveTo(Point::new(0.0, 0.0)),
                        PathCommand::LineTo(Point::new(40.0, 45.0)),
                        PathCommand::CubicTo(
                            Point::new(65.0, 70.0),
                            Point::new(90.0, -20.0),
                            Point::new(115.0, 30.0),
                        ),
                        PathCommand::LineTo(Point::new(140.0, 0.0)),
                    ],
                    fill_rule: FillRule::NonZero,
                },
                fill: None,
                stroke: Some(stroke(cap, join, width, dash, f32::from(column) * 1.25)),
            }));
            if let FinishedContent::Path(path) = &mut item.content {
                path.stroke.as_mut().expect("authored stroke").miter_limit =
                    1.0 + f32::from(row) * 4.0;
            }
            item.transform = AffineTransform([
                1.0,
                0.0,
                0.0,
                1.0,
                25.0 + f32::from(column) * 185.0,
                35.0 + f32::from(row) * 85.0,
            ]);
            items.push(item);
        }
    }
    page(items)
}

fn images_page(print: bool) -> FinishedPage {
    let mut items = Vec::new();
    for (space, column) in [("gray", 0_i16), ("cmyk", 1), ("rgb", 2)] {
        if print && space == "rgb" {
            continue;
        }
        for (suffix, row) in [("opaque", 0_i16), ("alpha", 1)] {
            let id = format!("{space}-{suffix}");
            // Each visible image is shared by two placements. Resource order
            // is the only change in the metamorphic document.
            for copy in 0_i16..2 {
                let mut image = FinishedItem::new(FinishedContent::Image(ImagePlacement {
                    image: id.clone(),
                    alpha: if copy == 0 { 1.0 } else { 0.5 },
                }));
                image.transform = AffineTransform([
                    55.0,
                    4.0,
                    7.0,
                    55.0,
                    25.0 + f32::from(column) * 185.0 + f32::from(copy) * 75.0,
                    420.0 - f32::from(row) * 150.0,
                ]);
                image.clip = Some(rectangle(0.0, 0.0, 0.875, 1.0, FillRule::EvenOdd));
                items.push(image);
            }
        }
        let mut mask = FinishedItem::new(FinishedContent::ImageAlpha {
            image: format!("{space}-alpha"),
            paint: Paint::gray(0.25, 0.75),
        });
        mask.transform =
            AffineTransform([90.0, 0.0, 0.0, 90.0, 30.0 + f32::from(column) * 185.0, 80.0]);
        mask.clip = Some(rectangle(0.0, 0.0, 1.0, 0.875, FillRule::NonZero));
        items.push(mask);
    }
    page(items)
}

fn gradients_page(print: bool) -> FinishedPage {
    let mut items = Vec::new();
    for (radial, column) in [(false, 0_i16), (true, 1)] {
        for style in 0_i16..3 {
            let stops = match style {
                0 => vec![(0.0, 0, 0.5), (0.25, 1, 0.5), (1.0, 2, 0.5)],
                1 => vec![(0.0, 0, 0.0), (0.25, 1, 0.5), (1.0, 2, 1.0)],
                _ => vec![
                    (0.0, 0, 0.75),
                    (0.5, 0, 0.75),
                    (0.5, 2, 0.75),
                    (1.0, 2, 0.75),
                ],
            };
            let geometry = if radial {
                GradientGeometry::Radial {
                    start_center: Point::new(60.0, 50.0),
                    start_radius: 0.0,
                    end_center: Point::new(80.0, 50.0),
                    end_radius: 60.0,
                }
            } else {
                GradientGeometry::Axial {
                    start: Point::new(20.0, 50.0),
                    end: Point::new(120.0, 50.0),
                }
            };
            let mut item = FinishedItem::new(FinishedContent::Gradient(FinishedGradientFill {
                path: rectangle(0.0, 0.0, 150.0, 100.0, FillRule::EvenOdd),
                gradient: Gradient {
                    geometry,
                    stops: stops
                        .into_iter()
                        .map(|(offset, index, alpha)| GradientStop {
                            offset,
                            paint: color(print, index, alpha),
                        })
                        .collect(),
                },
            }));
            item.transform = AffineTransform([
                1.25,
                0.125,
                0.25,
                0.875,
                30.0 + f32::from(column) * 280.0,
                50.0 + f32::from(style) * 165.0,
            ]);
            item.clip = Some(rectangle(5.0, 5.0, 145.0, 95.0, FillRule::NonZero));
            items.push(item);
        }
    }
    page(items)
}

fn blends_page(print: bool) -> FinishedPage {
    let mut items = Vec::new();
    for (blend, (column, row)) in BLENDS
        .into_iter()
        .zip((0_i16..4).flat_map(|row| (0_i16..4).map(move |column| (column, row))))
    {
        let mut nested = FinishedItem::new(FinishedContent::Group(FinishedGroup {
            bounds: PdfBox::new(0.0, 0.0, 90.0, 60.0),
            opacity: 0.5,
            items: vec![
                solid(
                    rectangle(0.0, 0.0, 65.0, 60.0, FillRule::NonZero),
                    color(print, 0, 1.0),
                ),
                solid(
                    rectangle(25.0, 0.0, 90.0, 60.0, FillRule::NonZero),
                    color(print, 2, 1.0),
                ),
            ],
        }));
        nested.transform = AffineTransform([1.0, 0.0, 0.0, 1.0, 10.0, 10.0]);
        nested.blend_mode = BlendMode::Multiply;
        let mut outer = FinishedItem::new(FinishedContent::Group(FinishedGroup {
            bounds: PdfBox::new(0.0, 0.0, 115.0, 100.0),
            opacity: 0.75,
            items: vec![
                solid(
                    rectangle(0.0, 0.0, 115.0, 100.0, FillRule::EvenOdd),
                    Paint::gray(0.8, 1.0),
                ),
                nested,
                solid(
                    rectangle(55.0, 35.0, 110.0, 95.0, FillRule::NonZero),
                    color(print, 1, 0.5),
                ),
            ],
        }));
        outer.blend_mode = blend;
        outer.transform = AffineTransform([
            1.0,
            0.0,
            0.0,
            1.0,
            20.0 + f32::from(column) * 145.0,
            25.0 + f32::from(row) * 140.0,
        ]);
        outer.clip = Some(rectangle(0.0, 0.0, 112.0, 98.0, FillRule::EvenOdd));
        items.push(outer);
    }
    page(items)
}

fn color(print: bool, index: usize, alpha: f32) -> Paint {
    if print {
        Paint::cmyk(
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ][index],
            alpha,
        )
    } else {
        Paint::rgb(
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]][index],
            alpha,
        )
    }
}

fn stroke(cap: LineCap, join: LineJoin, width: f32, dash: Vec<f32>, dash_phase: f32) -> Stroke {
    Stroke {
        paint: Paint::gray(0.15, 1.0),
        width,
        cap,
        join,
        miter_limit: 4.0,
        dash,
        dash_phase,
    }
}

fn solid(path: FinishedPath, paint: Paint) -> FinishedItem {
    FinishedItem::new(FinishedContent::Path(FinishedPathPaint {
        path,
        fill: Some(paint),
        stroke: None,
    }))
}

fn rectangle(x1: f32, y1: f32, x2: f32, y2: f32, fill_rule: FillRule) -> FinishedPath {
    FinishedPath {
        fill_rule,
        commands: vec![
            PathCommand::MoveTo(Point::new(x1, y1)),
            PathCommand::LineTo(Point::new(x2, y1)),
            PathCommand::LineTo(Point::new(x2, y2)),
            PathCommand::LineTo(Point::new(x1, y2)),
            PathCommand::Close,
        ],
    }
}

fn page(items: Vec<FinishedItem>) -> FinishedPage {
    FinishedPage {
        boxes: FinishedPageBoxes {
            media: PdfBox::new(-10.0, -10.0, 610.0, 610.0),
            bleed: PdfBox::new(-5.0, -5.0, 605.0, 605.0),
            trim: PdfBox::new(0.0, 0.0, 600.0, 600.0),
        },
        items,
    }
}

fn minimal() -> FinishedDocument {
    FinishedDocument {
        resources: FinishedResources::default(),
        pages: vec![page(vec![solid(
            rectangle(0.0, 0.0, 10.0, 10.0, FillRule::NonZero),
            Paint::gray(0.0, 1.0),
        )])],
    }
}

fn rejection_cases(print: bool) -> Vec<(&'static str, FinishedPdfError, &'static str)> {
    if print {
        vec![
            (
                "registered-rgb-unused",
                FinishedPdfError::RgbInPdfX,
                "RgbInPdfX",
            ),
            (
                "registered-rgb-alpha-only",
                FinishedPdfError::RgbInPdfX,
                "RgbInPdfX",
            ),
            ("rgb-paint", FinishedPdfError::RgbInPdfX, "RgbInPdfX"),
        ]
    } else {
        vec![
            (
                "missing-image",
                FinishedPdfError::MissingResource("absent".into()),
                "MissingResource",
            ),
            (
                "duplicate-unused-image",
                FinishedPdfError::DuplicateResource("same".into()),
                "DuplicateResource",
            ),
            (
                "nan-box",
                FinishedPdfError::InvalidGeometry,
                "InvalidGeometry",
            ),
            (
                "source-singular-matrix",
                FinishedPdfError::InvalidGeometry,
                "InvalidGeometry",
            ),
            (
                "all-zero-dash",
                FinishedPdfError::InvalidPaint,
                "InvalidPaint",
            ),
            (
                "clip-without-move",
                FinishedPdfError::InvalidPath,
                "InvalidPath",
            ),
            (
                "mask-without-alpha",
                FinishedPdfError::InvalidImage("opaque".into()),
                "InvalidImage",
            ),
        ]
    }
}

fn rejection_controls(
    options: &FinishedPdfOptions<'_>,
    print: bool,
) -> Result<Vec<Rejection>, Box<dyn Error>> {
    let cases = rejection_cases(print);
    let mut results = Vec::new();
    for (case, expected, error) in cases {
        let mut document = minimal();
        match case {
            "missing-image" => {
                document.pages[0]
                    .items
                    .push(FinishedItem::new(FinishedContent::Image(ImagePlacement {
                        image: "absent".into(),
                        alpha: 1.0,
                    })));
            }
            "duplicate-unused-image" => {
                for _ in 0..2 {
                    document
                        .resources
                        .images
                        .push(ImageResource::from_decoded_gray(
                            "same".into(),
                            2,
                            2,
                            &GRAY,
                            None,
                        )?);
                }
            }
            "nan-box" => document.pages[0].boxes.trim.x2 = f32::NAN,
            "source-singular-matrix" => {
                document.pages[0].items[0].transform =
                    AffineTransform([1.0, 2.0, 2.0, 4.0, 0.0, 0.0]);
            }
            "all-zero-dash" => {
                if let FinishedContent::Path(path) = &mut document.pages[0].items[0].content {
                    path.stroke = Some(stroke(
                        LineCap::Butt,
                        LineJoin::Miter,
                        1.0,
                        vec![0.0, 0.0],
                        0.0,
                    ));
                }
            }
            "clip-without-move" => {
                document.pages[0].items[0].clip = Some(FinishedPath {
                    commands: vec![PathCommand::LineTo(Point::new(0.0, 1.0))],
                    fill_rule: FillRule::NonZero,
                });
            }
            "mask-without-alpha" => {
                document
                    .resources
                    .images
                    .push(ImageResource::from_decoded_gray(
                        "opaque".into(),
                        2,
                        2,
                        &GRAY,
                        None,
                    )?);
                document.pages[0]
                    .items
                    .push(FinishedItem::new(FinishedContent::ImageAlpha {
                        image: "opaque".into(),
                        paint: Paint::gray(0.0, 1.0),
                    }));
            }
            "registered-rgb-unused" | "registered-rgb-alpha-only" | "rgb-paint" => {
                apply_print_rejection(&mut document, case)?;
            }
            _ => return Err("unknown fixed rejection control".into()),
        }
        match FinishedPdfWriter::write(&document, options) {
            Err(actual) if actual == expected => results.push(Rejection {
                case,
                mode: if print { "pdfx4" } else { "ordinary" },
                error,
            }),
            Err(actual) => {
                return Err(format!("{case}: expected {expected:?}, got {actual:?}").into());
            }
            Ok(_) => {
                return Err(format!("{case}: invalid input unexpectedly returned bytes").into());
            }
        }
    }
    Ok(results)
}

fn apply_print_rejection(
    document: &mut FinishedDocument,
    case: &str,
) -> Result<(), Box<dyn Error>> {
    match case {
        "registered-rgb-unused" | "registered-rgb-alpha-only" => {
            document
                .resources
                .images
                .push(ImageResource::from_decoded_rgb(
                    "registered-rgb".into(),
                    2,
                    2,
                    &RGB,
                    Some(&ALPHA),
                )?);
            if case == "registered-rgb-alpha-only" {
                document.pages[0]
                    .items
                    .push(FinishedItem::new(FinishedContent::ImageAlpha {
                        image: "registered-rgb".into(),
                        paint: Paint::gray(0.0, 1.0),
                    }));
            }
        }
        "rgb-paint" => document.pages[0].items.push(solid(
            rectangle(0.0, 0.0, 10.0, 10.0, FillRule::NonZero),
            Paint::rgb([0.25, 0.5, 0.75], 1.0),
        )),
        _ => return Err("unknown fixed print rejection control".into()),
    }
    Ok(())
}

fn manifest(
    outputs: &[Output],
    rejected: &[Rejection],
    print: bool,
) -> Result<String, std::fmt::Error> {
    let mut json = String::from(
        r#"{
  "schema": "accepted-composition-input-v1",
  "entry_point": "FinishedPdfWriter::write",
  "independent_evaluation": "pending",
  "metadata": {"title": "Composition (\\) <&>\rUnicode \u00e9 \ud83d\ude42", "input_timestamp": "2017-01-01T00:59:60+01:00", "expected_utc": "2016-12-31T23:59:59Z", "expected_info_date": "D:20161231235959Z"},
  "partitions": {
    "page_order": ["paths", "images", "gradients", "blends"],
    "page_boxes": {"media": [-10,-10,610,610], "bleed": [-5,-5,605,605], "trim": [0,0,600,600]},
    "path_commands": ["MoveTo", "LineTo", "CubicTo", "Close"],
    "fill_rules": ["NonZero", "EvenOdd"],
    "path_paints": ["fill-only", "stroke-only", "fill-and-stroke"],
    "stroke_cross_product": {"caps": ["Butt","Round","Square"], "joins": ["Miter","Round","Bevel"], "widths_by_join": [0,2,6], "miter_limits_by_join": [1,5,9], "dash_by_join": [[],[0,4],[3,1,2]], "phase_by_cap": [0,1.25,2.5], "items": 9},
    "images": {"width": 2, "height": 2, "order": "top-to-bottom-interleaved", "alpha": "straight-independent", "planes": {"gray": [0,85,170,255], "rgb": [255,0,32,0,255,64,16,32,255,255,128,0], "cmyk": [255,0,32,0,0,255,64,0,16,32,255,0,0,0,0,255], "alpha": [0,64,128,255]}, "registered_suffixes_per_space": ["opaque","alpha","unused-opaque","unused-alpha"], "image_placements_per_visible_resource": 2, "placement_opacities": [1,0.5], "image_alpha_placements_per_space": 1, "image_alpha_paint": {"gray": 0.25, "alpha": 0.75}, "unused_resource_direct_references": 0},
    "gradients": {"geometries": ["Axial","Radial"], "styles": ["constant-alpha","varying-alpha","hard-step"], "items": 6, "constant_alpha": 0.5, "varying_alphas": [0,0.5,1], "varying_offsets": [0,0.25,1], "hard_offsets": [0,0.5,0.5,1], "hard_alpha": 0.75, "endpoint_padding": true},
    "blends": ["Normal","Multiply","Screen","Overlay","Darken","Lighten","ColorDodge","ColorBurn","HardLight","SoftLight","Difference","Exclusion","Hue","Saturation","Color","Luminosity"],
    "groups": {"outer_count": 16, "inner_count": 16, "outer_opacity": 0.75, "inner_opacity": 0.5, "inner_blend": "Multiply", "isolated": true, "opacity_applied_at_placement": true},
    "transforms_and_clips": ["path-shear", "image-shear-evenodd-clip", "image-alpha-nonzero-clip", "gradient-shear-nonzero-clip", "nested-group-evenodd-clip"],
    "mode_difference": {"ordinary": {"paint_space": "DeviceRGB", "image_spaces": ["DeviceGray","DeviceCMYK","DeviceRGB"], "registered_images": 12, "output_intent": false}, "pdfx4": {"paint_space": "DeviceCMYK", "image_spaces": ["DeviceGray","DeviceCMYK"], "registered_images": 8, "output_intent": "exact-supplied-profile"}, "common_gray_paints": true, "cross_mode_semantics_equal": false}
  },
  "outputs": [
"#,
    );
    for (index, output) in outputs.iter().enumerate() {
        writeln!(
            json,
            "    {{\"file\":\"{}\",\"mode\":\"{}\",\"registration_order\":\"{}\",\"outcome\":\"accepted\",\"pages\":4,\"bytes\":{},\"deterministic_repeat_equal\":true}}{}",
            output.name,
            output.mode,
            if output.reversed { "reversed" } else { "base" },
            output.bytes.len(),
            if index + 1 == outputs.len() { "" } else { "," }
        )?;
    }
    json.push_str("  ],\n  \"metamorphic_pairs\": [\n");
    for (index, pair) in outputs.as_chunks::<2>().0.iter().enumerate() {
        writeln!(
            json,
            "    {{\"base\":\"{}\",\"variant\":\"{}\",\"change\":\"reverse-registered-image-order\",\"expected_decoded_semantics_equal\":true,\"actual_bytes_equal\":{}}}{}",
            pair[0].name,
            pair[1].name,
            pair[0].bytes == pair[1].bytes,
            if index + 1 == outputs.len() / 2 {
                ""
            } else {
                ","
            }
        )?;
    }
    json.push_str("  ],\n  \"rejections\": [\n");
    for (index, rejection) in rejected.iter().enumerate() {
        writeln!(
            json,
            "    {{\"case\":\"{}\",\"mode\":\"{}\",\"outcome\":\"rejected\",\"expected_error\":\"{}\",\"matched_error\":\"{}\",\"returned_pdf\":false,\"output\":null}}{}",
            rejection.case,
            rejection.mode,
            rejection.error,
            rejection.error,
            if index + 1 == rejected.len() { "" } else { "," }
        )?;
    }
    writeln!(
        json,
        "  ],\n  \"print_executed\":{print},\n  \"omitted\":{},\n  \"discarded_cases\":0\n}}",
        if print {
            "[]"
        } else {
            "[\"print outputs and three print rejection controls: no --profile supplied\"]"
        }
    )?;
    Ok(json)
}
