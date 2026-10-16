use print_core::finished::*;
use print_core::{FinishedPdfWriter, PdfCandidateMetadata};

fn options() -> FinishedPdfOptions<'static> {
    FinishedPdfOptions {
        metadata: PdfCandidateMetadata::at_utc("Neutral test", "2026-10-04T00:00:00Z").unwrap(),
        mode: FinishedOutputMode::Ordinary,
    }
}

fn page() -> FinishedPage {
    FinishedPage {
        boxes: FinishedPageBoxes {
            media: PdfBox::new(-10.0, -10.0, 210.0, 310.0),
            bleed: PdfBox::new(-5.0, -5.0, 205.0, 305.0),
            trim: PdfBox::new(0.0, 0.0, 200.0, 300.0),
        },
        items: vec![FinishedItem::new(FinishedContent::Path(
            FinishedPathPaint {
                path: FinishedPath {
                    commands: vec![
                        PathCommand::MoveTo(Point::new(10.0, 20.0)),
                        PathCommand::CubicTo(
                            Point::new(30.0, 40.0),
                            Point::new(50.0, 60.0),
                            Point::new(70.0, 80.0),
                        ),
                        PathCommand::Close,
                    ],
                    fill_rule: FillRule::NonZero,
                },
                fill: Some(Paint::cmyk([0.1, 0.2, 0.3, 0.4], 1.0)),
                stroke: None,
            },
        ))],
    }
}

#[test]
fn two_ordered_pages_keep_boxes_cubics_and_repeatable_ordinary_bytes() {
    let document = FinishedDocument {
        pages: vec![page(), page()],
        resources: FinishedResources::default(),
    };
    let first = FinishedPdfWriter::write(&document, &options()).unwrap();
    let second = FinishedPdfWriter::write(&document, &options()).unwrap();
    assert_eq!(first.bytes(), second.bytes());
    let text = String::from_utf8_lossy(first.bytes());
    assert!(text.contains("/Count 2"));
    assert!(text.contains("/MediaBox [-10 -10 210 310]"));
    assert!(text.contains("/TrimBox [0 0 200 300]"));
    assert!(text.contains("30 40 50 60 70 80 c"));
    assert!(!text.contains("PDF/X"));
    assert!(!text.contains("GTS_PDFX"));
}

fn document() -> FinishedDocument {
    FinishedDocument {
        pages: vec![page()],
        resources: FinishedResources::default(),
    }
}

fn object_text(text: &str, reference: i32) -> &str {
    text.split(&format!("\n{reference} 0 obj\n"))
        .nth(1)
        .unwrap()
        .split("\nendobj")
        .next()
        .unwrap()
}

fn save_compositing_sample(name: &str, bytes: &[u8]) {
    if let Some(directory) = std::env::var_os("PDF_LIBRARY_COMPOSITING_ARTIFACTS") {
        std::fs::write(std::path::Path::new(&directory).join(name), bytes).unwrap();
    }
}

#[test]
fn blending_space_follows_each_page_and_group_painted_colors() {
    let mut d = document();
    let cmyk = d.pages[0].items[0].clone();
    let mut rgb = cmyk.clone();
    if let FinishedContent::Path(p) = &mut rgb.content {
        p.fill = Some(Paint::rgb([1.0, 0.0, 0.0], 1.0));
    }
    let bounds = d.pages[0].boxes.media;
    let group = |child| {
        FinishedItem::new(FinishedContent::Group(FinishedGroup {
            bounds,
            opacity: 0.5,
            items: vec![child],
        }))
    };
    // The RGB source supplies alpha only; its discarded color must not select
    // the first page's space. Registration still matters for PDF/X admission.
    d.resources.images.push(
        ImageResource::from_decoded_rgb("alpha-only".into(), 1, 1, &[255, 0, 0], Some(&[128]))
            .unwrap(),
    );
    d.pages[0].items = vec![
        group(cmyk.clone()),
        FinishedItem::new(FinishedContent::ImageAlpha {
            image: "alpha-only".into(),
            paint: Paint::cmyk([0.0, 1.0, 0.0, 0.0], 1.0),
        }),
    ];
    let mut second = page();
    second.items = vec![rgb.clone(), group(cmyk), group(rgb)];
    d.pages.push(second);
    let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
    let text = String::from_utf8_lossy(pdf.bytes());
    assert!(object_text(&text, 3).contains("/CS /DeviceCMYK"));
    assert!(object_text(&text, 5).contains("/CS /DeviceRGB"));
    let forms: Vec<_> = text
        .split("\nendobj")
        .filter(|s| s.contains("/Subtype /Form"))
        .collect();
    assert_eq!(
        forms
            .iter()
            .filter(|s| s.contains("/CS /DeviceCMYK"))
            .count(),
        2
    );
    assert_eq!(
        forms
            .iter()
            .filter(|s| s.contains("/CS /DeviceRGB"))
            .count(),
        1
    );
    assert_eq!(
        forms
            .iter()
            .filter(|s| s.contains("/CS /DeviceGray"))
            .count(),
        1
    );
    save_compositing_sample("local-color.pdf", pdf.bytes());
}

#[test]
fn stream_dictionaries_include_only_direct_images_states_and_child_forms() {
    use sha2::{Digest, Sha256};
    let mut d = document();
    let image = |id: &str| {
        FinishedItem::new(FinishedContent::Image(ImagePlacement {
            image: id.into(),
            alpha: 0.75,
        }))
    };
    let bounds = d.pages[0].boxes.media;
    let group = |items| {
        FinishedItem::new(FinishedContent::Group(FinishedGroup {
            bounds,
            opacity: 0.5,
            items,
        }))
    };
    d.pages[0].items = vec![group(vec![group(vec![image("a")])])];
    let mut second = page();
    second.items = vec![image("b")];
    d.pages.push(second);
    for id in ["a".into(), "b".into()]
        .into_iter()
        .chain((0..24).map(|i| format!("unused-{i}")))
    {
        d.resources
            .images
            .push(ImageResource::from_decoded_gray(id, 1, 1, &[128], None).unwrap());
    }
    let first = FinishedPdfWriter::write(&d, &options()).unwrap();
    let second = FinishedPdfWriter::write(&d, &options()).unwrap();
    assert_eq!(first.bytes(), second.bytes());
    let text = String::from_utf8_lossy(first.bytes());
    for id in ["a", "b"] {
        let name = format!("/I{:X}", Sha256::digest(id.as_bytes()));
        assert_eq!(
            text.matches(&name).count(),
            2,
            "one direct dictionary and one Do for {id}"
        );
    }
    for i in 0..24 {
        let name = format!("/I{:X}", Sha256::digest(format!("unused-{i}").as_bytes()));
        assert!(!text.contains(&name));
    }
    assert_eq!(
        text.matches("/A3F000000_3F000000").count(),
        4,
        "half alpha in two group placements and their dictionaries"
    );
    assert_eq!(
        text.matches("/A3F400000_3F400000").count(),
        4,
        "three-quarter alpha in two image paints and their dictionaries"
    );
    // Each Form name occurs only at its immediate parent's dictionary and Do.
    for form in text
        .split("\nendobj")
        .filter(|s| s.contains("/Subtype /Form"))
    {
        let reference = form
            .split_whitespace()
            .find_map(|s| s.parse::<i32>().ok())
            .unwrap();
        assert_eq!(text.matches(&format!("/G{reference}")).count(), 2);
    }
    save_compositing_sample("local-images-states-forms.pdf", first.bytes());
}

#[test]
fn all_standard_blends_apply_once_to_the_completed_group_and_children_reset_normal() {
    let modes = [
        (BlendMode::Normal, "Normal"),
        (BlendMode::Multiply, "Multiply"),
        (BlendMode::Screen, "Screen"),
        (BlendMode::Overlay, "Overlay"),
        (BlendMode::Darken, "Darken"),
        (BlendMode::Lighten, "Lighten"),
        (BlendMode::ColorDodge, "ColorDodge"),
        (BlendMode::ColorBurn, "ColorBurn"),
        (BlendMode::HardLight, "HardLight"),
        (BlendMode::SoftLight, "SoftLight"),
        (BlendMode::Difference, "Difference"),
        (BlendMode::Exclusion, "Exclusion"),
        (BlendMode::Hue, "Hue"),
        (BlendMode::Saturation, "Saturation"),
        (BlendMode::Color, "Color"),
        (BlendMode::Luminosity, "Luminosity"),
    ];
    let mut d = document();
    let child = d.pages[0].items[0].clone();
    assert_eq!(child.blend_mode, BlendMode::Normal);
    assert_eq!(BlendMode::default(), BlendMode::Normal);
    let bounds = d.pages[0].boxes.media;
    d.pages[0].items = modes
        .iter()
        .map(|(mode, _)| {
            let mut group = FinishedItem::new(FinishedContent::Group(FinishedGroup {
                bounds,
                opacity: 0.5,
                items: vec![child.clone(), child.clone()],
            }));
            group.blend_mode = *mode;
            group
        })
        .collect();
    let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
    let text = String::from_utf8_lossy(pdf.bytes());
    for (_, name) in modes {
        assert_eq!(
            text.matches(&format!("/BM /{name}\n")).count(),
            1,
            "shared blend object for {name}"
        );
    }
    assert_eq!(text.matches("/ca 0.5").count(), 1);
    let forms: Vec<_> = text
        .split("\nendobj")
        .filter(|s| s.contains("/Subtype /Form"))
        .collect();
    assert_eq!(forms.len(), 16);
    for form in forms {
        assert_eq!(form.matches("/BNormal gs").count(), 2);
        assert_eq!(form.matches("/A3F800000_3F800000 gs").count(), 2);
        assert!(!form.contains("/A3F000000_3F000000"));
        assert!(!form.contains("/BMultiply"));
    }
    save_compositing_sample("all-blend-names.pdf", pdf.bytes());
    let print = FinishedPdfWriter::write(&d, &pdfx_options()).unwrap();
    assert_eq!(
        String::from_utf8_lossy(print.bytes())
            .matches("/CS /DeviceCMYK")
            .count(),
        17
    );
    save_compositing_sample("all-blend-names-print.pdf", print.bytes());
}

#[test]
fn isolated_group_overlap_uses_placement_alpha_and_blend_once() {
    let bounds = PdfBox::new(0.0, 0.0, 200.0, 200.0);
    let rectangle = |x1, x2, paint| {
        FinishedItem::new(FinishedContent::Path(FinishedPathPaint {
            path: FinishedPath {
                commands: vec![
                    PathCommand::MoveTo(Point::new(x1, 20.0)),
                    PathCommand::LineTo(Point::new(x2, 20.0)),
                    PathCommand::LineTo(Point::new(x2, 180.0)),
                    PathCommand::LineTo(Point::new(x1, 180.0)),
                    PathCommand::Close,
                ],
                fill_rule: FillRule::NonZero,
            },
            fill: Some(paint),
            stroke: None,
        }))
    };
    let mut d = document();
    d.pages[0].boxes = FinishedPageBoxes {
        media: bounds,
        bleed: bounds,
        trim: bounds,
    };
    let mut group = FinishedItem::new(FinishedContent::Group(FinishedGroup {
        bounds,
        opacity: 0.5,
        items: vec![
            rectangle(20.0, 120.0, Paint::rgb([1.0, 0.0, 0.0], 1.0)),
            rectangle(80.0, 180.0, Paint::rgb([0.0, 0.0, 1.0], 1.0)),
        ],
    }));
    group.blend_mode = BlendMode::Multiply;
    d.pages[0].items = vec![
        rectangle(0.0, 200.0, Paint::rgb([0.8, 0.6, 0.4], 1.0)),
        group,
    ];
    let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
    let text = String::from_utf8_lossy(pdf.bytes());
    assert_eq!(text.matches("/BMultiply gs").count(), 1);
    assert_eq!(text.matches("/A3F000000_3F000000 gs").count(), 1);
    let form = text
        .split("\nendobj")
        .find(|s| s.contains("/Subtype /Form"))
        .unwrap();
    assert_eq!(form.matches("/BNormal gs").count(), 2);
    assert_eq!(form.matches("/A3F800000_3F800000 gs").count(), 2);
    save_compositing_sample("isolated-overlap.pdf", pdf.bytes());
    let print = FinishedPdfWriter::write(&d, &pdfx_options());
    assert_eq!(print.unwrap_err(), FinishedPdfError::RgbInPdfX);
}

#[test]
fn invalid_geometry_paths_paint_and_missing_resources_return_errors() {
    let mut d = document();
    d.pages[0].boxes.trim.x2 = f32::NAN;
    assert_eq!(
        FinishedPdfWriter::write(&d, &options()).unwrap_err(),
        FinishedPdfError::InvalidGeometry
    );
    let mut d = document();
    d.pages[0].boxes.bleed.x1 = -11.0;
    assert_eq!(
        FinishedPdfWriter::write(&d, &options()).unwrap_err(),
        FinishedPdfError::InvalidGeometry
    );
    let mut d = document();
    d.pages[0].items[0].transform = AffineTransform([1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    assert_eq!(
        FinishedPdfWriter::write(&d, &options()).unwrap_err(),
        FinishedPdfError::InvalidGeometry
    );
    let mut d = document();
    d.pages[0].items[0].clip = Some(FinishedPath {
        commands: vec![PathCommand::LineTo(Point::new(0.0, 1.0))],
        fill_rule: FillRule::NonZero,
    });
    assert_eq!(
        FinishedPdfWriter::write(&d, &options()).unwrap_err(),
        FinishedPdfError::InvalidPath
    );
    let mut d = document();
    d.pages[0]
        .items
        .push(FinishedItem::new(FinishedContent::Image(ImagePlacement {
            image: "absent".into(),
            alpha: 1.0,
        })));
    assert_eq!(
        FinishedPdfWriter::write(&d, &options()).unwrap_err(),
        FinishedPdfError::MissingResource("absent".into())
    );
    let mut d = document();
    if let FinishedContent::Path(p) = &mut d.pages[0].items[0].content {
        p.fill.as_mut().unwrap().alpha = 1.1;
    }
    assert_eq!(
        FinishedPdfWriter::write(&d, &options()).unwrap_err(),
        FinishedPdfError::InvalidPaint
    );
}

#[test]
fn image_planes_clipping_transform_and_isolated_overlap_are_in_the_returned_pdf() {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let cmyk = [0, 64, 128, 255, 255, 128, 64, 0];
    let alpha = [128, 64];
    let mut d = document();
    d.resources.images.push(
        ImageResource::from_decoded_cmyk("picture".into(), 2, 1, &cmyk, Some(&alpha)).unwrap(),
    );
    let mut image = FinishedItem::new(FinishedContent::Image(ImagePlacement {
        image: "picture".into(),
        alpha: 1.0,
    }));
    image.transform = AffineTransform([40.0, 0.0, 0.0, 20.0, 10.0, 15.0]);
    image.clip = Some(FinishedPath {
        commands: vec![
            PathCommand::MoveTo(Point::new(0.0, 0.0)),
            PathCommand::LineTo(Point::new(0.8, 0.0)),
            PathCommand::LineTo(Point::new(0.8, 1.0)),
            PathCommand::Close,
        ],
        fill_rule: FillRule::EvenOdd,
    });
    d.pages[0].items = vec![FinishedItem::new(FinishedContent::Group(FinishedGroup {
        bounds: PdfBox::new(0.0, 0.0, 200.0, 300.0),
        opacity: 0.5,
        items: vec![image.clone(), image],
    }))];
    let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
    let text = String::from_utf8_lossy(pdf.bytes());
    assert!(text.contains("/S /Transparency"));
    assert!(text.contains("/I true"));
    assert!(text.contains("/ca 0.5"));
    assert!(text.contains("/SMask"));
    assert_eq!(text.matches("40 0 0 20 10 15 cm").count(), 2);
    assert_eq!(text.matches("W*").count(), 2);
    let mut planes = Vec::new();
    for marker in pdf
        .bytes()
        .windows(b"stream\n".len())
        .enumerate()
        .filter_map(|(i, s)| (s == b"stream\n").then_some(i + 7))
    {
        if let Some(length) = pdf.bytes()[marker..]
            .windows(b"\nendstream".len())
            .position(|s| s == b"\nendstream")
        {
            let mut decoded = Vec::new();
            if ZlibDecoder::new(&pdf.bytes()[marker..marker + length])
                .read_to_end(&mut decoded)
                .is_ok()
            {
                planes.push(decoded);
            }
        }
    }
    assert!(
        planes.contains(&cmyk.to_vec()),
        "losslessly decoded CMYK plane"
    );
    assert!(
        planes.contains(&alpha.to_vec()),
        "losslessly decoded alpha plane"
    );
}

#[test]
fn image_dimensions_planes_and_duplicate_resources_are_rejected() {
    assert!(matches!(
        ImageResource::from_decoded_cmyk("large".into(), 13_501, 1, &[], None),
        Err(FinishedPdfError::ResourceLimit)
    ));
    assert!(matches!(
        ImageResource::from_decoded_cmyk("bad".into(), 1, 1, &[0; 3], None),
        Err(FinishedPdfError::InvalidImage(_))
    ));
    assert!(matches!(
        ImageResource::from_decoded_cmyk("bad".into(), 1, 1, &[0; 4], Some(&[])),
        Err(FinishedPdfError::InvalidImage(_))
    ));
    let mut d = document();
    for _ in 0..2 {
        d.resources
            .images
            .push(ImageResource::from_decoded_cmyk("same".into(), 1, 1, &[0; 4], None).unwrap());
    }
    assert_eq!(
        FinishedPdfWriter::write(&d, &options()).unwrap_err(),
        FinishedPdfError::DuplicateResource("same".into())
    );
}

fn decoded_image_objects(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    use flate2::read::ZlibDecoder;
    use std::io::Read;
    let mut result = Vec::new();
    for start in bytes
        .windows(b"stream\n".len())
        .enumerate()
        .filter_map(|(i, marker)| {
            (marker == b"stream\n" && (i == 0 || bytes[i - 1] == b'\n')).then_some(i)
        })
    {
        let before = String::from_utf8_lossy(&bytes[..start]);
        let Some((_, dictionary)) = before.rsplit_once(" obj\n") else {
            continue;
        };
        if !dictionary.contains("/Subtype /Image") {
            continue;
        }
        let data_start = start + b"stream\n".len();
        let end = bytes[data_start..]
            .windows(b"\nendstream".len())
            .position(|marker| marker == b"\nendstream")
            .unwrap();
        let mut decoded = Vec::new();
        ZlibDecoder::new(&bytes[data_start..data_start + end])
            .read_to_end(&mut decoded)
            .unwrap();
        result.push((dictionary.to_owned(), decoded));
    }
    result
}

#[test]
fn decoded_rgb_gray_and_cmyk_images_keep_components_and_independent_alpha() {
    let rgb = [255, 32, 0, 4, 128, 250];
    let gray = [12, 201];
    let cmyk = [0, 64, 128, 255, 255, 128, 64, 0];
    let alpha = [0, 173];
    let mut d = document();
    d.resources.images = vec![
        ImageResource::from_decoded_rgb("rgb".into(), 2, 1, &rgb, Some(&alpha)).unwrap(),
        ImageResource::from_decoded_gray("gray".into(), 2, 1, &gray, Some(&alpha)).unwrap(),
        ImageResource::from_decoded_cmyk("cmyk".into(), 2, 1, &cmyk, Some(&alpha)).unwrap(),
    ];
    d.pages[0].items = ["rgb", "gray", "cmyk"]
        .into_iter()
        .map(|id| {
            FinishedItem::new(FinishedContent::Image(ImagePlacement {
                image: id.into(),
                alpha: 0.5,
            }))
        })
        .collect();
    let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
    let objects = decoded_image_objects(pdf.bytes());
    assert_eq!(objects.len(), 6);
    for (space, plane) in [
        ("/DeviceRGB", rgb.as_slice()),
        ("/DeviceGray", gray.as_slice()),
        ("/DeviceCMYK", cmyk.as_slice()),
    ] {
        let (dictionary, _) = objects.iter().find(|(_, data)| data == plane).unwrap();
        assert!(dictionary.contains(space));
        assert!(dictionary.contains("/BitsPerComponent 8"));
        assert!(dictionary.contains("/Width 2"));
        assert!(dictionary.contains("/Height 1"));
        assert!(dictionary.contains("/SMask"));
    }
    let masks = objects
        .iter()
        .filter(|(_, data)| data == &alpha)
        .collect::<Vec<_>>();
    assert_eq!(masks.len(), 3);
    assert!(
        masks
            .iter()
            .all(|(dictionary, _)| dictionary.contains("/DeviceGray"))
    );
    assert_eq!(
        pdf.bytes(),
        FinishedPdfWriter::write(&d, &options()).unwrap().bytes()
    );
}

#[test]
fn new_image_constructors_reject_invalid_planes_dimensions_and_admission() {
    use raster_capacity::{MAX_RETAINED_RASTER_BYTES, RasterMemoryPermit};
    for (width, height) in [(0, 1), (1, 0), (u32::MAX, u32::MAX), (13_500, 13_500)] {
        assert!(matches!(
            ImageResource::from_decoded_rgb("rgb".into(), width, height, &[], None),
            Err(FinishedPdfError::ResourceLimit)
        ));
        assert!(matches!(
            ImageResource::from_decoded_gray("gray".into(), width, height, &[], None),
            Err(FinishedPdfError::ResourceLimit)
        ));
    }
    assert!(matches!(
        ImageResource::from_decoded_rgb("rgb".into(), 1, 1, &[0; 4], None),
        Err(FinishedPdfError::InvalidImage(_))
    ));
    assert!(matches!(
        ImageResource::from_decoded_gray("gray".into(), 1, 1, &[], None),
        Err(FinishedPdfError::InvalidImage(_))
    ));
    assert!(matches!(
        ImageResource::from_decoded_rgb("rgb".into(), 1, 1, &[0; 3], Some(&[])),
        Err(FinishedPdfError::InvalidImage(_))
    ));
    assert!(matches!(
        ImageResource::from_decoded_gray("gray".into(), 1, 1, &[0], Some(&[0; 2])),
        Err(FinishedPdfError::InvalidImage(_))
    ));
    assert!(matches!(
        ImageResource::from_decoded_gray(String::new(), 1, 1, &[0], None),
        Err(FinishedPdfError::InvalidImage(_))
    ));
    let lease = RasterMemoryPermit::reserve(MAX_RETAINED_RASTER_BYTES).unwrap();
    assert!(matches!(
        ImageResource::from_decoded_rgb("rgb".into(), 1, 1, &[0; 3], None),
        Err(FinishedPdfError::ResourceLimit)
    ));
    assert!(matches!(
        ImageResource::from_decoded_gray("gray".into(), 1, 1, &[0], None),
        Err(FinishedPdfError::ResourceLimit)
    ));
    drop(lease);
    assert!(ImageResource::from_decoded_gray("gray".into(), 1, 1, &[0], None).is_ok());
}

#[test]
fn gray_paint_and_images_work_in_ordinary_and_cmyk_pdfx() {
    let mut d = document();
    if let FinishedContent::Path(path) = &mut d.pages[0].items[0].content {
        path.fill = Some(Paint::gray(0.25, 0.75));
        path.stroke = Some(Stroke {
            paint: Paint::gray(0.6, 1.0),
            width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: vec![],
            dash_phase: 0.0,
        });
    }
    d.resources
        .images
        .push(ImageResource::from_decoded_gray("gray".into(), 1, 1, &[201], Some(&[128])).unwrap());
    d.pages[0]
        .items
        .push(FinishedItem::new(FinishedContent::Image(ImagePlacement {
            image: "gray".into(),
            alpha: 1.0,
        })));
    d.pages[0]
        .items
        .push(FinishedItem::new(FinishedContent::ImageAlpha {
            image: "gray".into(),
            paint: Paint::gray(0.8, 0.5),
        }));
    for opts in [options(), pdfx_options()] {
        let pdf = FinishedPdfWriter::write(&d, &opts).unwrap();
        let text = String::from_utf8_lossy(pdf.bytes());
        assert!(text.contains("0.25 g"));
        assert!(text.contains("0.6 G"));
        assert!(text.contains("0.8 g"));
        assert!(!text.contains("/DeviceRGB"));
        assert!(text.contains("/DeviceGray"));
    }
    if let FinishedContent::Path(path) = &mut d.pages[0].items[0].content {
        path.fill = Some(Paint::gray(f32::NAN, 1.0));
    }
    assert_eq!(
        FinishedPdfWriter::write(&d, &options()).unwrap_err(),
        FinishedPdfError::InvalidPaint
    );
}

#[test]
fn pdfx_rejects_registered_rgb_images_even_when_unused_or_alpha_only() {
    for content in [
        None,
        Some(FinishedContent::Image(ImagePlacement {
            image: "rgb".into(),
            alpha: 1.0,
        })),
        Some(FinishedContent::ImageAlpha {
            image: "rgb".into(),
            paint: Paint::gray(0.3, 1.0),
        }),
    ] {
        let mut d = document();
        d.resources.images.push(
            ImageResource::from_decoded_rgb("rgb".into(), 1, 1, &[0, 255, 127], Some(&[128]))
                .unwrap(),
        );
        let actually_paints_rgb = matches!(content, Some(FinishedContent::Image(_)));
        if let Some(content) = content {
            d.pages[0].items.push(FinishedItem::new(content));
        }
        assert_eq!(
            FinishedPdfWriter::write(&d, &pdfx_options()).unwrap_err(),
            FinishedPdfError::RgbInPdfX
        );
        let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
        let text = String::from_utf8_lossy(pdf.bytes());
        assert!(
            text.contains("/ColorSpace /DeviceRGB"),
            "registered color image is still emitted"
        );
        assert_eq!(
            text.contains("/CS /DeviceRGB"),
            actually_paints_rgb,
            "alpha-only placement ignores discarded image color"
        );
    }
}

#[test]
fn pdfx_rejects_bad_profiles_and_rgb_and_binds_exact_icc() {
    let d = document();
    let mut opts = options();
    opts.mode = FinishedOutputMode::PdfX4 {
        output_profile: b"bad",
    };
    assert_eq!(
        FinishedPdfWriter::write(&d, &opts).unwrap_err(),
        FinishedPdfError::InvalidOutputProfile(print_core::ProfileError::TooShort)
    );
    let mut rgb = document();
    if let FinishedContent::Path(p) = &mut rgb.pages[0].items[0].content {
        p.fill = Some(Paint::rgb([0.0; 3], 1.0));
    }
    assert_eq!(
        FinishedPdfWriter::write(&rgb, &opts).unwrap_err(),
        FinishedPdfError::RgbInPdfX
    );
    let profile = include_bytes!("fixtures/SWOP2006coated_300_mth.icc");
    opts.mode = FinishedOutputMode::PdfX4 {
        output_profile: profile,
    };
    let pdf = FinishedPdfWriter::write(&d, &opts).unwrap();
    assert!(pdf.bytes().windows(profile.len()).any(|s| s == profile));
    let text = String::from_utf8_lossy(pdf.bytes());
    assert!(text.contains("/S /GTS_PDFX"));
    assert!(text.contains("/Info (Embedded CMYK printer profile; SHA-256 "));
    assert!(text.contains("/OutputConditionIdentifier (PDF Library CMYK SHA-256 "));
    assert!(text.contains("/Title (Neutral test)"));
    assert!(text.contains("xml:lang=\"x-default\">Neutral test</rdf:li>"));
    assert!(text.contains("<pdfxid:GTS_PDFXVersion>PDF/X-4"));
    assert_eq!(
        pdf.bytes(),
        FinishedPdfWriter::write(&d, &opts).unwrap().bytes()
    );
}

fn pdfx_options() -> FinishedPdfOptions<'static> {
    let mut opts = options();
    opts.mode = FinishedOutputMode::PdfX4 {
        output_profile: include_bytes!("fixtures/SWOP2006coated_300_mth.icc"),
    };
    opts
}

#[test]
fn pdfx_requires_a_nonblank_title_and_preserves_ordinary_title_policy() {
    for title in ["", " \t\n\r", "\u{2003}"] {
        let mut opts = options();
        opts.metadata = PdfCandidateMetadata::at_utc(title, "2026-10-04T00:00:00Z").unwrap();
        assert!(FinishedPdfWriter::write(&document(), &opts).is_ok());
        opts.mode = pdfx_options().mode;
        assert_eq!(
            FinishedPdfWriter::write(&document(), &opts).unwrap_err(),
            FinishedPdfError::EmptyPdfXTitle
        );
    }
}

#[test]
fn metadata_titles_are_outside_the_content_string_limit_but_keep_the_aggregate_budget() {
    for title in ["a".repeat(32_768), "試".repeat(16_383)] {
        for mut opts in [options(), pdfx_options()] {
            opts.metadata = PdfCandidateMetadata::at_utc(&title, "2026-10-04T00:00:00Z").unwrap();
            assert!(FinishedPdfWriter::write(&document(), &opts).is_ok());
        }
    }
    let mut opts = options();
    opts.metadata = PdfCandidateMetadata::at_utc(
        &"a".repeat(MAX_FINISHED_TEXT_BYTES + 1),
        "2026-10-04T00:00:00Z",
    )
    .unwrap();
    assert_eq!(
        FinishedPdfWriter::write(&document(), &opts).unwrap_err(),
        FinishedPdfError::ResourceLimit
    );
}

fn text_document() -> FinishedDocument {
    use std::collections::BTreeMap;
    let program = print_core::TrustedFontBoundary::resolve_packaged()
        .prepare_publication(
            include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf"),
            0,
            &[36],
        )
        .unwrap();
    let cid = program
        .remapped_glyphs()
        .iter()
        .find(|g| g.source_glyph_id() == 36)
        .unwrap()
        .cid();
    let mut d = document();
    d.resources.fonts.push(
        FontResource::from_prepared("font".into(), program, BTreeMap::from([(cid, "A".into())]))
            .unwrap(),
    );
    d.pages[0].items = vec![FinishedItem::new(FinishedContent::Glyphs(
        PositionedGlyphRun {
            font: "font".into(),
            font_size: 12.0,
            glyphs: vec![PositionedGlyph {
                cid,
                matrix: AffineTransform::default(),
            }],
            actual_text: "A".into(),
            paint: Paint::cmyk([0.0; 4], 1.0),
        },
    ))];
    d
}

#[test]
fn to_unicode_bounds_utf16_destinations_and_emits_unicode_map_type() {
    use std::collections::BTreeMap;
    for (name, text, accepted) in [
        ("bmp-512", "A".repeat(256), true),
        ("bmp-514", "A".repeat(257), false),
        ("surrogate-512", "😀".repeat(128), true),
        ("surrogate-516", "😀".repeat(129), false),
    ] {
        let program = print_core::TrustedFontBoundary::resolve_packaged()
            .prepare_publication(
                include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf"),
                0,
                &[36],
            )
            .unwrap();
        let cid = program
            .remapped_glyphs()
            .iter()
            .find(|g| g.source_glyph_id() == 36)
            .unwrap()
            .cid();
        let resource = FontResource::from_prepared(
            "font".into(),
            program,
            BTreeMap::from([(cid, text.clone())]),
        );
        if !accepted {
            assert!(
                matches!(resource, Err(FinishedPdfError::InvalidFont(_))),
                "{name} must be rejected without truncation"
            );
            continue;
        }
        let mut d = document();
        d.resources.fonts.push(resource.unwrap());
        d.pages[0].items = vec![FinishedItem::new(FinishedContent::Glyphs(
            PositionedGlyphRun {
                font: "font".into(),
                font_size: 12.0,
                glyphs: vec![PositionedGlyph {
                    cid,
                    matrix: AffineTransform::default(),
                }],
                actual_text: text.clone(),
                paint: Paint::gray(0.0, 1.0),
            },
        ))];
        let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
        save_compositing_sample(&format!("tounicode-{name}.pdf"), pdf.bytes());
        let serialized = String::from_utf8_lossy(pdf.bytes());
        assert!(serialized.contains("/CMapType 2 def"));
        assert!(serialized.contains("/Encoding /Identity-H"));
        let destination: String = text.encode_utf16().map(|u| format!("{u:04X}")).collect();
        assert!(serialized.contains(&format!("<{cid:04X}> <{destination}>")));
    }
}

#[test]
fn title_carriage_returns_are_identical_in_info_and_xmp() {
    for (name, title) in [("ascii", "a\rb\r\nc"), ("unicode", "試\r\n験\r & <title>")] {
        for (mode, mut opts) in [("ordinary", options()), ("print", pdfx_options())] {
            opts.metadata = PdfCandidateMetadata::at_utc(title, "2026-10-06T00:00:00Z").unwrap();
            let pdf = FinishedPdfWriter::write(&document(), &opts).unwrap();
            save_compositing_sample(&format!("metadata-{name}-{mode}.pdf"), pdf.bytes());
            let bytes = String::from_utf8_lossy(pdf.bytes());
            let xmp = bytes
                .split("<?xpacket")
                .nth(1)
                .unwrap()
                .split("\nendstream")
                .next()
                .unwrap();
            let xml = format!("<?xpacket{xmp}");
            let parsed = roxmltree::Document::parse(&xml).unwrap();
            let dc_title = parsed
                .descendants()
                .find(|n| n.has_tag_name(("http://purl.org/dc/elements/1.1/", "title")))
                .unwrap();
            assert_eq!(
                dc_title
                    .descendants()
                    .find(|n| n.has_tag_name(("http://www.w3.org/1999/02/22-rdf-syntax-ns#", "li")))
                    .unwrap()
                    .text()
                    .unwrap(),
                title
            );
        }
    }
}

#[test]
fn fonts_image_masks_and_gradient_resources_are_local_and_shared_objects_emit_once() {
    use sha2::{Digest, Sha256};
    let mut d = text_document();
    let glyph = d.pages[0].items[0].clone();
    let bounds = d.pages[0].boxes.media;
    let gradient = |varying: bool| {
        FinishedItem::new(FinishedContent::Gradient(FinishedGradientFill {
            path: match &page().items[0].content {
                FinishedContent::Path(p) => p.path.clone(),
                _ => unreachable!(),
            },
            gradient: Gradient {
                geometry: GradientGeometry::Axial {
                    start: Point::new(10.0, 20.0),
                    end: Point::new(70.0, 80.0),
                },
                stops: vec![
                    GradientStop {
                        offset: 0.0,
                        paint: Paint::gray(0.0, if varying { 0.0 } else { 0.25 }),
                    },
                    GradientStop {
                        offset: 1.0,
                        paint: Paint::gray(1.0, if varying { 1.0 } else { 0.25 }),
                    },
                ],
            },
        }))
    };
    let mask = FinishedItem::new(FinishedContent::ImageAlpha {
        image: "mask-source".into(),
        paint: Paint::gray(0.5, 0.5),
    });
    d.resources.images.push(
        ImageResource::from_decoded_gray("mask-source".into(), 1, 1, &[100], Some(&[128])).unwrap(),
    );
    d.pages[0].items = vec![FinishedItem::new(FinishedContent::Group(FinishedGroup {
        bounds,
        opacity: 0.75,
        items: vec![glyph.clone(), mask.clone(), gradient(true)],
    }))];
    // Page two uses no font/mask; page three reuses both shared resources.
    let mut second = page();
    second.items.push(gradient(false));
    let mut third = page();
    third.items = vec![glyph, mask];
    d.pages.extend([second, third]);
    let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
    assert_eq!(
        pdf.bytes(),
        FinishedPdfWriter::write(&d, &options()).unwrap().bytes()
    );
    let text = String::from_utf8_lossy(pdf.bytes());
    assert_eq!(text.matches("/Subtype /Type0").count(), 1);
    assert_eq!(
        text.matches("/Subtype /Image").count(),
        2,
        "one source and one alpha plane"
    );
    let font_name = format!("/F{:X}", Sha256::digest(b"font"));
    assert_eq!(text.matches(&font_name).count(), 4);
    assert!(!object_text(&text, 3).contains(&font_name));
    assert!(!object_text(&text, 5).contains(&font_name));
    assert!(object_text(&text, 7).contains(&font_name));
    let mask_name = format!("/M{:X}", Sha256::digest(b"mask-source"));
    assert_eq!(text.matches(&mask_name).count(), 4);
    assert!(!object_text(&text, 3).contains(&mask_name));
    assert!(!object_text(&text, 5).contains(&mask_name));
    assert!(object_text(&text, 7).contains(&mask_name));
    assert_eq!(
        text.matches("/S /Luminosity").count(),
        2,
        "one shared image mask and one gradient mask"
    );
    assert_eq!(text.matches("/ShadingType 2").count(), 3);
    save_compositing_sample("local-fonts-masks-gradients.pdf", pdf.bytes());
}

#[test]
fn pdfx_actual_text_checks_encoded_payload_including_bom_and_surrogate_pairs() {
    let mut d = text_document();
    for (text, allowed) in [
        ("a".repeat(32_767), true),
        ("a".repeat(32_768), false),
        // Literal-string escaping expands syntax, never the decoded payload.
        ("(".repeat(32_767), true),
        ("試".repeat(16_382), true),
        ("試".repeat(16_383), false),
        ("😀".repeat(8_191), true),
        ("😀".repeat(8_192), false),
        // A newline switches this serializer to UTF-16, even with ASCII input.
        (format!("{}\n", "a".repeat(16_381)), true),
        (format!("{}\n", "a".repeat(16_382)), false),
    ] {
        if let FinishedContent::Glyphs(run) = &mut d.pages[0].items[0].content {
            run.actual_text = text;
        }
        let result = FinishedPdfWriter::write(&d, &pdfx_options());
        assert_eq!(result.is_ok(), allowed, "encoded ActualText admission");
        if !allowed {
            assert!(matches!(
                result,
                Err(FinishedPdfError::PdfXContentStringTooLong {
                    maximum: 32_767,
                    ..
                })
            ));
        }
        assert!(
            FinishedPdfWriter::write(&d, &options()).is_ok(),
            "ordinary text retains aggregate policy"
        );
    }
}

fn nested_document(groups: usize, leaf: Option<FinishedItem>) -> FinishedDocument {
    let mut items = leaf.into_iter().collect::<Vec<_>>();
    for _ in 0..groups {
        items = vec![FinishedItem::new(FinishedContent::Group(FinishedGroup {
            bounds: PdfBox::new(0.0, 0.0, 200.0, 300.0),
            opacity: 1.0,
            items,
        }))];
    }
    let mut d = document();
    d.pages[0].items = items;
    d
}

#[test]
fn pdfx_graphics_depth_counts_item_saves_implicit_forms_and_mask_evaluation() {
    let path = page().items.pop().unwrap();
    assert!(
        FinishedPdfWriter::write(&nested_document(13, Some(path.clone())), &pdfx_options()).is_ok()
    );
    assert_eq!(
        FinishedPdfWriter::write(&nested_document(14, Some(path.clone())), &pdfx_options())
            .unwrap_err(),
        FinishedPdfError::PdfXGraphicsStateDepthExceeded {
            depth: 29,
            maximum: 28
        }
    );
    assert!(FinishedPdfWriter::write(&nested_document(14, None), &pdfx_options()).is_ok());
    assert_eq!(
        FinishedPdfWriter::write(&nested_document(15, None), &pdfx_options()).unwrap_err(),
        FinishedPdfError::PdfXGraphicsStateDepthExceeded {
            depth: 30,
            maximum: 28
        }
    );
    assert!(FinishedPdfWriter::write(&nested_document(64, Some(path)), &options()).is_ok());
    for (groups, allowed) in [(13, true), (14, false)] {
        let mut d = nested_document(
            groups,
            Some(FinishedItem::new(FinishedContent::ImageAlpha {
                image: "mask".into(),
                paint: Paint::cmyk([0.0; 4], 1.0),
            })),
        );
        d.resources.images.push(
            ImageResource::from_decoded_cmyk("mask".into(), 1, 1, &[0; 4], Some(&[128])).unwrap(),
        );
        let result = FinishedPdfWriter::write(&d, &pdfx_options());
        assert_eq!(result.is_ok(), allowed);
        if !allowed {
            assert_eq!(
                result.unwrap_err(),
                FinishedPdfError::PdfXGraphicsStateDepthExceeded {
                    depth: 30,
                    maximum: 28
                }
            );
        }
    }
}

#[test]
fn pdfx_rejects_unreferenced_and_malformed_icc_versions() {
    for version in [
        [2, 0x10, 0, 0],
        [2, 0x30, 0, 0],
        [4, 0x30, 0, 0],
        [5, 0, 0, 0],
        [2, 0x20, 1, 0],
        [2, 0x20, 0, 1],
        [4, 0x21, 0, 0],
        [4, 0xA0, 0, 0],
    ] {
        let mut profile = include_bytes!("fixtures/SWOP2006coated_300_mth.icc").to_vec();
        profile[8..12].copy_from_slice(&version);
        let mut opts = options();
        opts.mode = FinishedOutputMode::PdfX4 {
            output_profile: &profile,
        };
        let expected = if version[2] != 0 || version[3] != 0 || version[1] >> 4 > 9 {
            print_core::ProfileError::InvalidVersionEncoding(version)
        } else {
            print_core::ProfileError::UnsupportedVersion(version)
        };
        assert_eq!(
            FinishedPdfWriter::write(&document(), &opts).unwrap_err(),
            FinishedPdfError::InvalidOutputProfile(expected)
        );
    }
}

#[test]
fn prepared_truetype_and_cff_programs_cids_and_multicodepoint_actual_text_are_preserved() {
    use print_core::TrustedFontBoundary;
    use std::collections::BTreeMap;
    let boundary = TrustedFontBoundary::resolve_packaged();
    for bytes in [
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").as_slice(),
        include_bytes!("../../layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf").as_slice(),
    ] {
        let source_glyph = ttf_parser::Face::parse(bytes, 0)
            .unwrap()
            .glyph_index('A')
            .unwrap()
            .0;
        let program = boundary
            .prepare_publication(bytes, 0, &[source_glyph])
            .unwrap();
        let cid = program
            .remapped_glyphs()
            .iter()
            .find(|g| g.source_glyph_id() == source_glyph)
            .unwrap()
            .cid();
        let exact = program.prepared_bytes().to_vec();
        let mut d = document();
        d.resources.fonts.push(
            FontResource::from_prepared(
                "font".into(),
                program,
                BTreeMap::from([(cid, "ffi".into())]),
            )
            .unwrap(),
        );
        d.pages[0].items = vec![FinishedItem::new(FinishedContent::Glyphs(
            PositionedGlyphRun {
                font: "font".into(),
                font_size: 12.0,
                glyphs: vec![PositionedGlyph {
                    cid,
                    matrix: AffineTransform([1.0, 0.0, 0.0, 1.0, 25.0, 40.0]),
                }],
                actual_text: "ffi".into(),
                paint: Paint::cmyk([0.0, 0.0, 0.0, 1.0], 1.0),
            },
        ))];
        let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
        assert!(pdf.bytes().windows(exact.len()).any(|s| s == exact));
        let text = String::from_utf8_lossy(pdf.bytes());
        assert!(text.contains("/ActualText (ffi)"));
        assert!(text.contains("006600660069"));
        assert!(text.contains("1 0 0 1 25 40 Tm"));
        if let FinishedContent::Glyphs(run) = &mut d.pages[0].items[0].content {
            run.glyphs[0].cid = u16::MAX;
        }
        assert!(matches!(
            FinishedPdfWriter::write(&d, &options()),
            Err(FinishedPdfError::InvalidGlyph { cid: u16::MAX, .. })
        ));
    }
}

#[test]
fn constant_image_alpha_paint_reuses_exact_mask_and_rejects_missing_alpha() {
    let mut d = document();
    d.resources.images.push(
        ImageResource::from_decoded_cmyk("masked".into(), 1, 1, &[0; 4], Some(&[128])).unwrap(),
    );
    d.pages[0].items = vec![FinishedItem::new(FinishedContent::ImageAlpha {
        image: "masked".into(),
        paint: Paint::cmyk([0.0, 0.0, 0.0, 1.0], 0.75),
    })];
    let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
    let text = String::from_utf8_lossy(pdf.bytes());
    assert!(text.contains("/S /Luminosity"));
    assert!(text.contains("/CS /DeviceGray"));
    assert!(text.contains("0 0 1 1 re"));
    assert!(text.contains("/ca 0.75"));
    let mut no_alpha = document();
    no_alpha
        .resources
        .images
        .push(ImageResource::from_decoded_cmyk("opaque".into(), 1, 1, &[0; 4], None).unwrap());
    no_alpha.pages[0].items = vec![FinishedItem::new(FinishedContent::ImageAlpha {
        image: "opaque".into(),
        paint: Paint::cmyk([0.0; 4], 1.0),
    })];
    assert_eq!(
        FinishedPdfWriter::write(&no_alpha, &options()).unwrap_err(),
        FinishedPdfError::InvalidImage("opaque".into())
    );
}

#[test]
fn nested_group_resource_budget_is_checked_before_serialization() {
    let mut item = page().items.pop().unwrap();
    for _ in 0..=print_core::MAX_FINISHED_GROUP_DEPTH {
        item = FinishedItem::new(FinishedContent::Group(FinishedGroup {
            bounds: PdfBox::new(0.0, 0.0, 200.0, 300.0),
            opacity: 1.0,
            items: vec![item],
        }));
    }
    let mut d = document();
    d.pages[0].items = vec![item];
    assert_eq!(
        FinishedPdfWriter::write(&d, &options()).unwrap_err(),
        FinishedPdfError::ResourceLimit
    );
}

#[test]
fn maximum_caller_resource_ids_emit_bounded_pdf_names() {
    use std::collections::BTreeMap;
    let id = "r".repeat(MAX_FINISHED_RESOURCE_ID_BYTES);
    let image_id = "i".repeat(MAX_FINISHED_RESOURCE_ID_BYTES);
    let bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
    let program = print_core::TrustedFontBoundary::resolve_packaged()
        .prepare_publication(bytes, 0, &[36])
        .unwrap();
    let cid = program
        .remapped_glyphs()
        .iter()
        .find(|g| g.source_glyph_id() == 36)
        .unwrap()
        .cid();
    let mut d = document();
    d.resources.fonts.push(
        FontResource::from_prepared(id.clone(), program, BTreeMap::from([(cid, "A".into())]))
            .unwrap(),
    );
    d.resources.images.push(
        ImageResource::from_decoded_cmyk(image_id.clone(), 1, 1, &[0; 4], Some(&[128])).unwrap(),
    );
    d.pages[0].items = vec![
        FinishedItem::new(FinishedContent::Glyphs(PositionedGlyphRun {
            font: id.clone(),
            font_size: 12.0,
            glyphs: vec![PositionedGlyph {
                cid,
                matrix: AffineTransform([1.0, 0.0, 0.0, 1.0, 20.0, 20.0]),
            }],
            actual_text: "A".into(),
            paint: Paint::cmyk([0.0, 0.0, 0.0, 1.0], 1.0),
        })),
        FinishedItem::new(FinishedContent::Image(ImagePlacement {
            image: image_id.clone(),
            alpha: 1.0,
        })),
        FinishedItem::new(FinishedContent::ImageAlpha {
            image: image_id,
            paint: Paint::cmyk([0.0; 4], 1.0),
        }),
    ];
    let pdf = FinishedPdfWriter::write(&d, &options()).unwrap();
    // Resource dictionary keys and their content-stream operands must stay
    // inside the PDF/X Name implementation limit regardless of caller ID size.
    let text = String::from_utf8_lossy(pdf.bytes());
    for token in text.split_ascii_whitespace().filter(|t| t.starts_with('/')) {
        if matches!(token.as_bytes().get(1), Some(b'F' | b'I' | b'M')) {
            assert!(
                token.len() <= 128,
                "unbounded resource name: {} bytes",
                token.len() - 1
            );
        }
    }
}

#[test]
fn oversized_unicode_mapping_actual_text_and_resource_identity_are_rejected_before_output() {
    use std::collections::BTreeMap;
    let oversized = "a".repeat(MAX_FINISHED_TEXT_BYTES + 1);
    let bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
    let program = print_core::TrustedFontBoundary::resolve_packaged()
        .prepare_publication(bytes, 0, &[36])
        .unwrap();
    let cid = program
        .remapped_glyphs()
        .iter()
        .find(|g| g.source_glyph_id() == 36)
        .unwrap()
        .cid();
    assert!(matches!(
        FontResource::from_prepared(
            "font".into(),
            program,
            BTreeMap::from([(cid, oversized.clone())])
        ),
        Err(FinishedPdfError::ResourceLimit)
    ));
    let mut d = document();
    d.pages[0].items = vec![FinishedItem::new(FinishedContent::Glyphs(
        PositionedGlyphRun {
            font: "absent".into(),
            font_size: 12.0,
            glyphs: vec![],
            actual_text: oversized,
            paint: Paint::cmyk([0.0; 4], 1.0),
        },
    ))];
    assert_eq!(
        FinishedPdfWriter::write(&d, &options()).unwrap_err(),
        FinishedPdfError::ResourceLimit
    );
    assert!(matches!(
        ImageResource::from_decoded_cmyk(
            "x".repeat(MAX_FINISHED_RESOURCE_ID_BYTES + 1),
            1,
            1,
            &[0; 4],
            None
        ),
        Err(FinishedPdfError::ResourceLimit)
    ));
}
