//! Opt-in retained combined-output appearance fixture. Never authorizes export.
#![cfg(windows)]

use document_core::model::{
    Color, DocumentId, FontFaceBinding, ImageContentKind, Insets, ObjectDraft, ObjectKind,
    PhysicalSize, Revision, ShapeKind, ShapeStroke, TableData, TextFrame, TextStoryFormatting,
    TextStyle, Transform, WrapMode,
};
use document_core::{
    AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use layout_core::{ExactFontResource, ResolvedPublication};
use print_core::{
    FixtureContract, FontBoundary, PreflightBasis, PublicationCandidatePreflight,
    PublicationCandidateWriter, PublicationTextEvidence, TrustedColorBoundary, TrustedFontBoundary,
    inspect_output_intent, prepare_publication_colors, prepare_publication_fonts,
    prepare_publication_rasters,
};
use raster_boundary::{RasterSourceProfileOrigin, TrustedRasterBoundary};

const PROFILE: &[u8] = include_bytes!("fixtures/SWOP2006coated_300_mth.icc");

#[test]
#[ignore = "requires explicit PUBLISHER_INPUT_PACKAGE, new PUBLISHER_REPRESENTATIVE_OUTPUT and packaged workers"]
fn retain_native_package_output() {
    let input = std::path::PathBuf::from(std::env::var_os("PUBLISHER_INPUT_PACKAGE").unwrap());
    let destination =
        std::path::PathBuf::from(std::env::var_os("PUBLISHER_REPRESENTATIVE_OUTPUT").unwrap());
    assert!(input.is_absolute() && destination.is_absolute());
    std::fs::create_dir(&destination).expect("exclusive new QA output directory");
    let original = std::fs::read(&input).unwrap();
    let reopened = document_core::open_package(&input).unwrap();
    let resources: Vec<_> = layout_core::output_font_face_bindings(&reopened.snapshot)
        .unwrap()
        .into_iter()
        .map(|(source, face_index)| {
            let bytes = reopened
                .assets
                .get(&source)
                .expect("embedded font")
                .bytes
                .clone();
            assert_eq!(document_core::AssetHash::for_bytes(&bytes), source);
            ExactFontResource {
                source,
                face_index,
                bytes,
            }
        })
        .collect();
    // Match desktop preflight: each published page gets its own bounded layout
    // budget, rather than sharing the interactive preview budget across a book.
    let publication =
        ResolvedPublication::resolve_output_with_font_resources(&reopened.snapshot, &resources)
            .unwrap();
    let contract = FixtureContract::new(
        PreflightBasis::from(publication.clone()),
        inspect_output_intent(PROFILE).unwrap(),
        FontBoundary::NotExercised,
    );
    let fonts = prepare_publication_fonts(
        &publication,
        &resources,
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();
    let colors = prepare_publication_colors(
        &publication,
        PROFILE,
        &TrustedColorBoundary::resolve_packaged(),
    )
    .unwrap()
    .unwrap();
    let rasters = prepare_publication_rasters(
        &publication,
        &reopened.assets,
        PROFILE,
        &TrustedRasterBoundary::resolve_packaged(),
        &TrustedColorBoundary::resolve_packaged(),
    )
    .unwrap();
    let write = || {
        PublicationCandidateWriter::write_with_resources(
            &publication,
            &contract,
            PROFILE,
            &rasters,
            Some(&fonts),
            Some(&colors),
        )
        .unwrap()
    };
    let candidate = write();
    assert_eq!(
        candidate.bytes(),
        write().bytes(),
        "deterministic output from saved native package"
    );
    let preflight = PublicationCandidatePreflight::for_candidate_with_font_resources(
        &publication,
        &contract,
        &candidate,
        &resources,
    );
    std::fs::write(destination.join("combined.pdf"), candidate.bytes()).unwrap();
    std::fs::write(destination.join("source.publisher-v2"), &original).unwrap();
    std::fs::write(destination.join("evidence.txt"), format!(
        "QA only; not authorized professional output.\nrevision={:?}\nartifact={:?}\nscene={:?}\nevidence={:#?}\npreflight={:#?}\n",
        reopened.snapshot.revision, candidate.artifact_hash(), publication.semantic_digest(), candidate.evidence(), preflight)).unwrap();
    assert_eq!(
        std::fs::read(input).unwrap(),
        original,
        "source package preserved"
    );
    println!(
        "Retained native package QA output: {}",
        destination.display()
    );
}

#[test]
#[ignore = "requires PUBLISHER_REPRESENTATIVE_OUTPUT naming a new absolute directory and packaged workers"]
#[allow(clippy::too_many_lines)] // One retained fixture keeps its content and evidence assertions together.
fn retain_combined_exact_output() {
    let destination = std::path::PathBuf::from(
        std::env::var_os("PUBLISHER_REPRESENTATIVE_OUTPUT").expect("explicit output bundle"),
    );
    assert!(destination.is_absolute());
    std::fs::create_dir(&destination).expect("exclusive new output directory");
    let mut assets = AssetStore::default();
    let resources: Vec<_> = [
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").as_slice(),
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans-Bold.ttf").as_slice(),
    ]
    .into_iter()
    .map(|bytes| ExactFontResource {
        source: assets.insert("font/ttf", bytes.to_vec()),
        face_index: 0,
        bytes: bytes.to_vec(),
    })
    .collect();
    // Locally generated ICC-tagged RGB stripes: opaque red, quarter-alpha green,
    // transparent blue. At the placed size this is 450 dpi, avoiding a low-DPI warning.
    let mut rgba = Vec::with_capacity(900 * 450 * 4);
    for _ in 0..450 {
        for x in 0..900 {
            rgba.extend_from_slice(match x {
                0..300 => &[255, 0, 0, 255],
                300..600 => &[0, 255, 0, 64],
                _ => &[0, 0, 255, 0],
            });
        }
    }
    let mut png = Vec::new();
    let mut encoder = PngEncoder::new(&mut png);
    encoder
        .set_icc_profile(lcms2::Profile::new_srgb().icc().unwrap())
        .unwrap();
    encoder
        .write_image(&rgba, 900, 450, ExtendedColorType::Rgba8)
        .unwrap();
    std::fs::write(destination.join("tagged-picture.png"), &png).unwrap();
    let inspection = document_core::inspect_raster(&png).unwrap();
    let picture = assets.insert_raster(png, inspection);
    let document = Document::new(
        DocumentId::parse("combined-renderer-proof").unwrap(),
        "Workshop print appearance fixture",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3175).unwrap(),
    )
    .unwrap();
    let page = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let style = |index: usize, size| TextStyle {
        font_face: Some(FontFaceBinding {
            source: resources[index].source.clone(),
            face_index: 0,
        }),
        font_family: if index == 0 {
            "DejaVu Sans"
        } else {
            "DejaVu Sans Bold"
        }
        .into(),
        font_weight: if index == 0 { 400 } else { 700 },
        font_size_millipoints: size,
        ..TextStyle::default()
    };
    let text = |value: &str, index, size| ObjectKind::Text {
        text: value.into(),
        style: style(index, size),
        frame: TextFrame::default(),
        story: TextStoryFormatting::default(),
    };
    let shape = |shade| ObjectKind::Shape {
        shape: ShapeKind::Rectangle,
        fill: Color {
            red: shade,
            green: shade,
            blue: shade,
            alpha: 255,
        },
        stroke: ShapeStroke::default(),
        wrap_mode: WrapMode::None,
    };
    let add = |name: &str, x, y, w, h, opacity, kind| Command::AddObject {
        location: ObjectLocation::Page(page.clone()),
        object: ObjectDraft {
            name: name.into(),
            size: PhysicalSize::new(w, h).unwrap(),
            transform: Transform::identity_at(x, y),
            opacity_millionths: opacity,
            kind,
        },
    };
    let mut table = TableData::empty(3, 3).unwrap();
    table.style = style(0, 11_000);
    table.cells = [
        "STEP",
        "ACTION",
        "CHECK",
        "1",
        "Observe café",
        "Ready",
        "2",
        "Arrange paper",
        "Complete",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let added = session.execute(Revision::initial(), Transaction {
        label: "Combined exact-font, table and transparency page".into(),
        commands: vec![
            add("Full-bleed pale background", -3175,-3175,222_250,285_750,1_000_000,shape(242)),
            add("Bold headline", 18_000,18_000,180_000,16_000,1_000_000,text("WORKSHOP NOTES",1,24_000)),
            add("Regular body",18_000,40_000,180_000,35_000,1_000_000,text("Observe, arrange, and explain.\nExact regular and bold fonts; café, naïve, and résumé.",0,12_000)),
            add("Populated checklist",18_000,85_000,180_000,60_000,1_000_000,ObjectKind::Table {table}),
            add("Opaque comparison panel",18_000,162_000,50_000,35_000,1_000_000,shape(0)),
            add("Half-opacity comparison panel",80_000,162_000,50_000,35_000,500_000,shape(0)),
            add("Tagged transparent Picture",145_000,162_000,50_800,25_400,1_000_000,ObjectKind::Image { asset: picture, wrap_mode: WrapMode::None, content_kind: ImageContentKind::ContinuousTone, crop: document_core::model::ImageCrop::default(), flip: document_core::model::ImageFlip::default(), lock_aspect_ratio: true }),
            add("Transparent panel label",18_000,207_000,180_000,30_000,1_000_000,text("Left: opaque. Right: 50% object opacity.\nLetter trim with 0.125-inch bleed; next page intentionally blank.",0,11_000)),
            Command::AddPage { after: Some(page.clone()), trim: PhysicalSize::new(215_900,279_400).unwrap(), bleed: Insets::uniform(3175).unwrap() },
        ],
    }).unwrap();
    let snapshot = ProjectSnapshot::from_session(&session);
    let package = destination.join("combined.publisher-v2");
    document_core::save_new_package(&package, &snapshot, &assets).unwrap();
    let reopened = document_core::open_package(&package).unwrap();
    assert_eq!(snapshot, reopened.snapshot);
    let publication =
        ResolvedPublication::resolve_with_font_resources(&reopened.snapshot, &resources).unwrap();
    assert!(publication.text_boxes().iter().all(|text| !text.overset));
    let contract = FixtureContract::new(
        PreflightBasis::from(publication.clone()),
        inspect_output_intent(PROFILE).unwrap(),
        FontBoundary::NotExercised,
    );
    let font_boundary = TrustedFontBoundary::resolve_packaged();
    let fonts = prepare_publication_fonts(&publication, &resources, &font_boundary)
        .expect("combined font preparation");
    let colors = prepare_publication_colors(
        &publication,
        PROFILE,
        &TrustedColorBoundary::resolve_packaged(),
    )
    .unwrap()
    .unwrap();
    let rasters = prepare_publication_rasters(
        &publication,
        &reopened.assets,
        PROFILE,
        &TrustedRasterBoundary::resolve_packaged(),
        &TrustedColorBoundary::resolve_packaged(),
    )
    .unwrap();
    assert_eq!(rasters.len(), 1);
    assert_eq!(
        rasters[0].evidence().source_profile_origin,
        RasterSourceProfileOrigin::Embedded
    );
    let candidate = PublicationCandidateWriter::write_with_resources(
        &publication,
        &contract,
        PROFILE,
        &rasters,
        Some(&fonts),
        Some(&colors),
    )
    .unwrap();
    let repeated = PublicationCandidateWriter::write_with_resources(
        &publication,
        &contract,
        PROFILE,
        &rasters,
        Some(&fonts),
        Some(&colors),
    )
    .unwrap();
    assert_eq!(candidate.evidence().raster_placements.len(), 1);
    let pdf_syntax = String::from_utf8_lossy(candidate.bytes());
    assert!(pdf_syntax.contains("/SMask"));
    assert!(pdf_syntax.contains("/DeviceCMYK"));
    assert!(!pdf_syntax.contains("/DeviceRGB"));
    assert_eq!(candidate.bytes(), repeated.bytes());
    assert_eq!(candidate.evidence().pages.len(), 2);
    assert_eq!(
        candidate.evidence().pages[0]
            .object_ids_in_paint_order
            .len(),
        8
    );
    assert!(candidate.evidence().pages[0].clipped_table_cells.is_empty());
    assert!(
        candidate.evidence().pages[1]
            .object_ids_in_paint_order
            .is_empty()
    );
    assert!(matches!(
        candidate.evidence().text,
        PublicationTextEvidence::EmbeddedFonts { font_count: 2, .. }
    ));
    let preflight = PublicationCandidatePreflight::for_candidate_with_font_resources(
        &publication,
        &contract,
        &candidate,
        &resources,
    );
    assert!(
        preflight.ready_for_independent_validation(),
        "{:#?}",
        preflight.findings
    );
    std::fs::write(destination.join("combined.pdf"), candidate.bytes()).unwrap();
    std::fs::write(destination.join("evidence.txt"),format!("revision={:?}\nartifact={:?}\nscene={:?}\nobjects={:?}\nevidence={:#?}\npreflight={:#?}\n",added.revision,candidate.artifact_hash(),publication.semantic_digest(),added.created_objects,candidate.evidence(),preflight)).unwrap();
    println!("Retained exact combined fixture: {}", destination.display());
}

#[test]
#[ignore = "requires a new absolute PUBLISHER_REPRESENTATIVE_OUTPUT directory"]
fn retain_asymmetric_crop_shadow_source() {
    use document_core::model::{HardShadow, ImageCrop, Micrometres, ObjectEffects};
    let destination = std::path::PathBuf::from(
        std::env::var_os("PUBLISHER_REPRESENTATIVE_OUTPUT").unwrap(),
    );
    assert!(destination.is_absolute());
    std::fs::create_dir(&destination).unwrap();
    let mut rgba = Vec::with_capacity(800 * 800 * 4);
    for y in 0..800 {
        for x in 0..800 {
            rgba.extend_from_slice(match (x < 400, y < 400) {
                (true, true) => &[255, 0, 0, 255],
                (false, true) => &[0, 255, 0, 64],
                (true, false) => &[0, 0, 255, 255],
                (false, false) => &[255, 255, 0, 0],
            });
        }
    }
    let mut png = Vec::new();
    let mut encoder = PngEncoder::new(&mut png);
    encoder.set_icc_profile(lcms2::Profile::new_srgb().icc().unwrap()).unwrap();
    encoder.write_image(&rgba, 800, 800, ExtendedColorType::Rgba8).unwrap();
    std::fs::write(destination.join("asymmetric.png"), &png).unwrap();
    let inspection = document_core::inspect_raster(&png).unwrap();
    let mut assets = AssetStore::default();
    let asset = assets.insert_raster(png, inspection);
    let document = Document::new(
        DocumentId::parse("asymmetric-crop-shadow").unwrap(),
        "Asymmetric image crop and shadow QA",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3175).unwrap(),
    ).unwrap();
    let page = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session.execute(Revision::initial(), Transaction::single(
        "Place cropped asymmetric image", Command::AddObject {
            location: ObjectLocation::Page(page),
            object: ObjectDraft {
                name: "Asymmetric cropped quadrants".into(),
                size: PhysicalSize::new(70_000, 65_000).unwrap(),
                transform: Transform::identity_at(25_000, 40_000),
                opacity_millionths: 1_000_000,
                kind: ObjectKind::Image {
                    asset, wrap_mode: WrapMode::None,
                    content_kind: ImageContentKind::ContinuousTone,
                    crop: ImageCrop::new(100_000, 250_000, 200_000, 100_000).unwrap(),
                    flip: document_core::model::ImageFlip::default(),
                    lock_aspect_ratio: false,
                },
            },
        },
    )).unwrap();
    session.execute(added.revision, Transaction::single(
        "Separate shadow for orientation inspection", Command::UpdateObjectEffects {
            object_id: added.created_objects[0].clone(),
            effects: ObjectEffects {
                shadow: Some(HardShadow {
                    color: Color { red: 0, green: 0, blue: 0, alpha: 255 },
                    offset_x: Micrometres::new(90_000), offset_y: Micrometres::new(0),
                }),
                ..ObjectEffects::default()
            },
        },
    )).unwrap();
    document_core::save_new_package(
        &destination.join("source.publisher-v2"),
        &ProjectSnapshot::from_session(&session), &assets,
    ).unwrap();
}
