use print_core::finished::*;
use print_core::{FinishedPdfWriter, MAX_FINISHED_ITEMS, PdfCandidateMetadata};

fn path() -> FinishedPath {
    FinishedPath {
        commands: vec![
            PathCommand::MoveTo(Point::new(10.0, 10.0)),
            PathCommand::LineTo(Point::new(190.0, 10.0)),
            PathCommand::LineTo(Point::new(190.0, 190.0)),
            PathCommand::LineTo(Point::new(10.0, 190.0)),
            PathCommand::Close,
        ],
        fill_rule: FillRule::NonZero,
    }
}
fn gradient(stops: Vec<GradientStop>) -> Gradient {
    Gradient {
        geometry: GradientGeometry::Axial {
            start: Point::new(30.0, 10.0),
            end: Point::new(170.0, 10.0),
        },
        stops,
    }
}
fn stop(offset: f32, paint: Paint) -> GradientStop {
    GradientStop { offset, paint }
}
fn gray_stops() -> Vec<GradientStop> {
    vec![
        stop(0.0, Paint::gray(0.0, 1.0)),
        stop(1.0, Paint::gray(1.0, 1.0)),
    ]
}
fn item(gradient: Gradient) -> FinishedItem {
    FinishedItem::new(FinishedContent::Gradient(FinishedGradientFill {
        path: path(),
        gradient,
    }))
}
fn document(items: Vec<FinishedItem>) -> FinishedDocument {
    let bounds = PdfBox::new(0.0, 0.0, 200.0, 200.0);
    FinishedDocument {
        pages: vec![FinishedPage {
            boxes: FinishedPageBoxes {
                media: bounds,
                bleed: bounds,
                trim: bounds,
            },
            items,
        }],
        resources: FinishedResources::default(),
    }
}
fn options(mode: FinishedOutputMode<'_>) -> FinishedPdfOptions<'_> {
    FinishedPdfOptions {
        metadata: PdfCandidateMetadata::at_utc("Native gradients", "2026-10-04T00:00:00Z").unwrap(),
        mode,
    }
}
fn write(items: Vec<FinishedItem>) -> Vec<u8> {
    FinishedPdfWriter::write(&document(items), &options(FinishedOutputMode::Ordinary))
        .unwrap()
        .into_bytes()
}
fn save_sample(name: &str, bytes: &[u8]) {
    if let Some(directory) = std::env::var_os("PDF_LIBRARY_GRADIENT_ARTIFACTS") {
        std::fs::write(std::path::Path::new(&directory).join(name), bytes).unwrap();
    }
}

#[test]
fn native_multistop_axial_functions_preserve_all_three_component_spaces() {
    let paints = [
        [
            Paint::rgb([1.0, 0.0, 0.0], 1.0),
            Paint::rgb([0.0, 1.0, 0.0], 1.0),
            Paint::rgb([0.0, 0.0, 1.0], 1.0),
        ],
        [
            Paint::gray(0.0, 1.0),
            Paint::gray(0.75, 1.0),
            Paint::gray(1.0, 1.0),
        ],
        [
            Paint::cmyk([1.0, 0.0, 0.0, 0.0], 1.0),
            Paint::cmyk([0.0, 1.0, 0.0, 0.0], 1.0),
            Paint::cmyk([0.0, 0.0, 1.0, 0.0], 1.0),
        ],
    ];
    let build_items = || {
        paints
            .into_iter()
            .zip([0.0, 60.0, 120.0])
            .map(|(p, x)| {
                let mut fill = item(gradient(vec![
                    stop(0.0, p[0]),
                    stop(0.4, p[1]),
                    stop(1.0, p[2]),
                ]));
                fill.transform = AffineTransform([0.3, 0.0, 0.0, 1.0, x, 0.0]);
                fill
            })
            .collect()
    };
    let bytes = write(build_items());
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(text.matches("/ShadingType 2").count(), 3);
    assert_eq!(text.matches("/FunctionType 2").count(), 6);
    assert_eq!(text.matches("/FunctionType 3").count(), 3);
    assert_eq!(text.matches("/Bounds [0.4]").count(), 3);
    assert_eq!(text.matches("/Encode [0 1 0 1]").count(), 3);
    assert_eq!(text.matches("/Extend [true true]").count(), 3);
    for color in [
        "/ColorSpace /DeviceRGB",
        "/ColorSpace /DeviceGray",
        "/ColorSpace /DeviceCMYK",
    ] {
        assert!(text.contains(color));
    }
    assert!(text.contains("/C0 [1 0 0]"));
    assert!(text.contains("/C1 [0 1 0 0]"));
    assert!(!text.contains("/SMask"));
    assert_eq!(bytes, write(build_items()));
    save_sample("axial-components.pdf", &bytes);
}

#[test]
fn hard_interior_stops_have_left_and_right_limits_without_empty_functions() {
    let bytes = write(vec![item(gradient(vec![
        stop(0.0, Paint::rgb([1.0, 0.0, 0.0], 1.0)),
        stop(0.5, Paint::rgb([1.0, 0.0, 0.0], 1.0)),
        stop(0.5, Paint::rgb([0.0, 1.0, 0.0], 1.0)),
        stop(0.5, Paint::rgb([0.0, 0.0, 1.0], 1.0)),
        stop(1.0, Paint::rgb([0.0, 0.0, 1.0], 1.0)),
    ]))]);
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(text.matches("/FunctionType 2").count(), 2);
    assert!(text.contains("/Bounds [0.5]"));
    assert!(text.contains("/C0 [1 0 0]\n"));
    assert!(text.contains("/C0 [0 0 1]\n"));
    assert!(!text.contains("/C0 [0 1 0]"));
    save_sample("hard-edge.pdf", &bytes);
}

#[test]
fn variable_alpha_uses_matching_bounded_gray_mask_and_preserves_clip_transform_group() {
    let mut g = gradient(vec![
        stop(0.0, Paint::rgb([1.0, 0.0, 0.0], 0.0)),
        stop(0.3, Paint::rgb([0.0, 1.0, 0.0], 0.75)),
        stop(1.0, Paint::rgb([0.0, 0.0, 1.0], 1.0)),
    ]);
    g.geometry = GradientGeometry::Radial {
        start_center: Point::new(80.0, 90.0),
        start_radius: 0.0,
        end_center: Point::new(100.0, 100.0),
        end_radius: 100.0,
    };
    let mut child = item(g);
    child.transform = AffineTransform([0.8, 0.1, 0.0, 0.5, 10.0, 20.0]);
    child.clip = Some(FinishedPath {
        commands: vec![
            PathCommand::MoveTo(Point::new(20.0, 20.0)),
            PathCommand::CubicTo(
                Point::new(30.0, 170.0),
                Point::new(170.0, 170.0),
                Point::new(180.0, 20.0),
            ),
            PathCommand::Close,
        ],
        fill_rule: FillRule::EvenOdd,
    });
    let bytes = write(vec![FinishedItem::new(FinishedContent::Group(
        FinishedGroup {
            bounds: PdfBox::new(0.0, 0.0, 200.0, 200.0),
            opacity: 0.5,
            items: vec![child],
        },
    ))]);
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(text.matches("/ShadingType 3").count(), 2);
    assert_eq!(text.matches("/Coords [80 90 0 100 100 100]").count(), 2);
    assert_eq!(text.matches("/Bounds [0.3]").count(), 2);
    assert_eq!(text.matches("/Extend [true true]").count(), 2);
    assert!(text.contains("/S /Luminosity"));
    assert!(text.contains("/BC [0]"));
    assert!(text.contains("/BBox [10 10 190 190]"));
    assert!(text.contains("/CS /DeviceGray"));
    assert!(text.contains("0.8 0.1 0 0.5 10 20 cm"));
    assert!(text.contains("W*\nn"));
    assert_eq!(text.matches("/ca 0.5").count(), 1);
    save_sample("radial-alpha-transform-clip.pdf", &bytes);
}

#[test]
fn constant_alpha_reuses_scalar_state_without_mask() {
    let bytes = write(vec![item(gradient(vec![
        stop(0.0, Paint::gray(0.0, 0.25)),
        stop(0.5, Paint::gray(1.0, 0.25)),
        stop(1.0, Paint::gray(0.0, 0.25)),
    ]))]);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/ca 0.25"));
    assert_eq!(text.matches("/ShadingType 2").count(), 1);
    assert!(!text.contains("/SMask"));
}

#[test]
fn invalid_gradient_geometry_stops_and_excess_are_rejected_before_output() {
    let mut invalid = Vec::new();
    let mut g = gradient(gray_stops());
    g.geometry = GradientGeometry::Axial {
        start: Point::new(1.0, 1.0),
        end: Point::new(1.0, 1.0),
    };
    invalid.push(g);
    let mut g = gradient(gray_stops());
    g.geometry = GradientGeometry::Radial {
        start_center: Point::new(0.0, 0.0),
        start_radius: 0.0,
        end_center: Point::new(1.0, 1.0),
        end_radius: 0.0,
    };
    invalid.push(g);
    let mut g = gradient(gray_stops());
    g.geometry = GradientGeometry::Radial {
        start_center: Point::new(0.0, 0.0),
        start_radius: 1.0,
        end_center: Point::new(0.0, 0.0),
        end_radius: 1.0,
    };
    invalid.push(g);
    let mut g = gradient(gray_stops());
    g.geometry = GradientGeometry::Radial {
        start_center: Point::new(0.0, 0.0),
        start_radius: -1.0,
        end_center: Point::new(0.0, 0.0),
        end_radius: 1.0,
    };
    invalid.push(g);
    for stops in [
        vec![],
        vec![stop(0.0, Paint::gray(0.0, 1.0))],
        vec![
            stop(0.1, Paint::gray(0.0, 1.0)),
            stop(1.0, Paint::gray(1.0, 1.0)),
        ],
        vec![
            stop(0.0, Paint::gray(0.0, 1.0)),
            stop(0.0, Paint::gray(1.0, 1.0)),
            stop(1.0, Paint::gray(1.0, 1.0)),
        ],
        vec![
            stop(0.0, Paint::gray(0.0, 1.0)),
            stop(1.0, Paint::gray(0.0, 1.0)),
            stop(1.0, Paint::gray(1.0, 1.0)),
        ],
        vec![
            stop(0.0, Paint::gray(0.0, 1.0)),
            stop(0.8, Paint::gray(1.0, 1.0)),
            stop(0.5, Paint::gray(0.0, 1.0)),
            stop(1.0, Paint::gray(1.0, 1.0)),
        ],
        vec![
            stop(0.0, Paint::gray(0.0, 1.0)),
            stop(1.0, Paint::rgb([1.0, 1.0, 1.0], 1.0)),
        ],
        vec![
            stop(0.0, Paint::gray(f32::NAN, 1.0)),
            stop(1.0, Paint::gray(1.0, 1.0)),
        ],
        vec![
            stop(0.0, Paint::gray(0.0, 1.0)),
            stop(1.0, Paint::gray(1.0, f32::INFINITY)),
        ],
        vec![
            stop(0.0, Paint::gray(0.0, 1.0)),
            stop(f32::NAN, Paint::gray(1.0, 1.0)),
            stop(1.0, Paint::gray(1.0, 1.0)),
        ],
    ] {
        invalid.push(gradient(stops));
    }
    for g in invalid {
        assert_eq!(
            FinishedPdfWriter::write(
                &document(vec![item(g)]),
                &options(FinishedOutputMode::Ordinary)
            )
            .unwrap_err(),
            FinishedPdfError::InvalidGradient
        );
    }
    let too_many = gradient(vec![stop(0.0, Paint::gray(0.0, 1.0)); MAX_FINISHED_ITEMS]);
    assert_eq!(
        FinishedPdfWriter::write(
            &document(vec![item(too_many)]),
            &options(FinishedOutputMode::Ordinary)
        )
        .unwrap_err(),
        FinishedPdfError::ResourceLimit
    );
}

#[test]
fn closely_spaced_distinct_stops_preserve_their_serialized_bounds() {
    let bytes = write(vec![item(gradient(vec![
        stop(0.0, Paint::gray(0.0, 1.0)),
        stop(0.000_000_1, Paint::gray(0.25, 1.0)),
        stop(0.000_000_2, Paint::gray(0.75, 1.0)),
        stop(1.0, Paint::gray(1.0, 1.0)),
    ]))]);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Bounds [0.0000001 0.0000002]"));
    assert_eq!(text.matches("/FunctionType 2").count(), 3);
}

#[test]
fn cmyk_and_gray_gradients_work_in_pdfx_but_rgb_and_underflow_collapse_fail() {
    let profile = include_bytes!("fixtures/SWOP2006coated_300_mth.icc");
    let pdfx = options(FinishedOutputMode::PdfX4 {
        output_profile: profile,
    });
    let mut g = gradient(vec![
        stop(0.0, Paint::cmyk([1.0, 0.0, 0.0, 0.0], 0.0)),
        stop(1.0, Paint::cmyk([0.0, 1.0, 0.0, 0.0], 1.0)),
    ]);
    let mut cmyk_fill = item(g.clone());
    cmyk_fill.transform = AffineTransform([0.45, 0.0, 0.0, 1.0, 0.0, 0.0]);
    let mut gray_fill = item(gradient(gray_stops()));
    gray_fill.transform = AffineTransform([0.45, 0.0, 0.0, 1.0, 100.0, 0.0]);
    let bytes = FinishedPdfWriter::write(&document(vec![cmyk_fill, gray_fill]), &pdfx)
        .unwrap()
        .into_bytes();
    save_sample("cmyk-gray-print.pdf", &bytes);
    g.stops[0].paint = Paint::rgb([1.0, 0.0, 0.0], 0.0);
    g.stops[1].paint = Paint::rgb([0.0, 1.0, 0.0], 1.0);
    assert_eq!(
        FinishedPdfWriter::write(&document(vec![item(g)]), &pdfx).unwrap_err(),
        FinishedPdfError::RgbInPdfX
    );
    let tiny = f32::from_bits(1);
    let mut allowed = gradient(gray_stops());
    allowed.geometry = GradientGeometry::Radial {
        start_center: Point::new(tiny, 0.0),
        start_radius: tiny,
        end_center: Point::new(0.0, 0.0),
        end_radius: 1.0,
    };
    // Receiver zeroing here still leaves a valid point-to-circle gradient.
    assert!(FinishedPdfWriter::write(&document(vec![item(allowed)]), &pdfx).is_ok());
    let g = gradient(vec![
        stop(0.0, Paint::gray(0.0, 1.0)),
        stop(tiny, Paint::gray(0.5, 1.0)),
        stop(1.0, Paint::gray(1.0, 1.0)),
    ]);
    assert!(
        FinishedPdfWriter::write(
            &document(vec![item(g.clone())]),
            &options(FinishedOutputMode::Ordinary)
        )
        .is_ok()
    );
    assert_eq!(
        FinishedPdfWriter::write(&document(vec![item(g)]), &pdfx).unwrap_err(),
        FinishedPdfError::InvalidGradient
    );
    let mut g = gradient(gray_stops());
    g.geometry = GradientGeometry::Axial {
        start: Point::new(0.0, 0.0),
        end: Point::new(tiny, 0.0),
    };
    assert!(
        FinishedPdfWriter::write(
            &document(vec![item(g.clone())]),
            &options(FinishedOutputMode::Ordinary)
        )
        .is_ok()
    );
    assert_eq!(
        FinishedPdfWriter::write(&document(vec![item(g)]), &pdfx).unwrap_err(),
        FinishedPdfError::InvalidGradient
    );
}

#[test]
fn mask_invocation_depth_is_counted_along_the_executed_group_path() {
    let profile = include_bytes!("fixtures/SWOP2006coated_300_mth.icc");
    let pdfx = options(FinishedOutputMode::PdfX4 {
        output_profile: profile,
    });
    let nested = |depth| {
        let mut child = item(gradient(vec![
            stop(0.0, Paint::gray(0.0, 0.0)),
            stop(1.0, Paint::gray(1.0, 1.0)),
        ]));
        for _ in 0..depth {
            child = FinishedItem::new(FinishedContent::Group(FinishedGroup {
                bounds: PdfBox::new(0.0, 0.0, 200.0, 200.0),
                opacity: 1.0,
                items: vec![child],
            }));
        }
        document(vec![child])
    };
    assert!(FinishedPdfWriter::write(&nested(13), &pdfx).is_ok());
    assert_eq!(
        FinishedPdfWriter::write(&nested(14), &pdfx).unwrap_err(),
        FinishedPdfError::PdfXGraphicsStateDepthExceeded {
            depth: 30,
            maximum: 28
        }
    );
}
