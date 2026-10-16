use document_core::model::{
    BleedExpectation, Color, DocumentId, EdgeExpectations, FontFaceBinding, HardShadow,
    InlineTextSpan, InlineTextStyle, Insets, Micrometres, ObjectDraft, ObjectEffects, ObjectKind,
    ParagraphListStyle, ParagraphRange, ParagraphStyle, ParagraphStyleRun, PhysicalSize, Revision,
    ShapeKind, ShapeStroke, TableCellFormatting, TableData, TableGridBorder,
    TableVerticalAlignment, TextAlignment, TextFrame, TextSourceRange, TextStoryFormatting,
    TextStyle, Transform, WrapMode,
};
use document_core::{
    AssetHash, AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot,
    Transaction, open_package, save_new_package,
};
use layout_core::{
    ExactFontResource, ResolvedGlyphFallback, ResolvedOutlineCommand, ResolvedPublication,
    ResolvedSceneObjectKind,
};
use print_core::{MissingImagePolicy, ScenePdfOptions, ScenePdfWriter, SceneSerializationError};

#[path = "support/pdf_operators.rs"]
mod pdf_operators;
use pdf_operators::alpha_invocation_count;

#[test]
fn scene_pdf_preserves_pages_boxes_order_and_excludes_the_pasteboard() {
    let mut document = document("ordered-scene");
    document.pages[0].bleed_expectations = EdgeExpectations {
        top: BleedExpectation::Required,
        right: BleedExpectation::NotIntended,
        bottom: BleedExpectation::Auto,
        left: BleedExpectation::Required,
    };
    let first_page = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Compose export scenes".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(first_page.clone()),
                        object: shape("Back", 12_000, 15_000, ShapeKind::Rectangle),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(first_page.clone()),
                        object: ObjectDraft {
                            name: "Word search grid".to_owned(),
                            size: PhysicalSize::new(70_000, 40_000).unwrap(),
                            transform: Transform::identity_at(20_000, 30_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Table {
                                table: TableData::empty(4, 6).unwrap(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(first_page.clone()),
                        object: image("Page image", b"page-image", 90_000, 10_000),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Pasteboard,
                        object: image("Pasteboard only", b"pasteboard-image", 5_000, 5_000),
                    },
                    Command::AddPage {
                        after: Some(first_page),
                        trim: PhysicalSize::new(215_900, 279_400).unwrap(),
                        bleed: Insets::uniform(3_175).unwrap(),
                    },
                ],
            },
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);
    let publication = ResolvedPublication::resolve(&project).unwrap();
    let expected_order = project.document.pages[0]
        .objects
        .iter()
        .map(|object| object.id.as_str().to_owned())
        .collect::<Vec<_>>();

    let options = ScenePdfOptions {
        missing_images: MissingImagePolicy::VisiblePlaceholder,
    };
    let first = ScenePdfWriter::write(&publication, options).unwrap();
    let second = ScenePdfWriter::write(&publication, options).unwrap();

    assert_eq!(
        first.bytes(),
        second.bytes(),
        "scene output must be deterministic"
    );
    assert_eq!(first.evidence().scene_digest, publication.semantic_digest());
    assert_eq!(first.evidence().pages.len(), 2);
    assert_eq!(
        first.evidence().pages[0].object_ids_in_paint_order,
        expected_order
    );
    assert!(
        first.evidence().pages[1]
            .object_ids_in_paint_order
            .is_empty()
    );
    assert_eq!(first.evidence().pages[0].placeholder_image_ids.len(), 1);
    assert_eq!(first.evidence().pasteboard_object_count_excluded, 1);
    assert!(first.evidence().transparency.object_opacity);
    assert!(first.evidence().transparency.paint_alpha);
    assert!(!first.evidence().transparency.raster_soft_mask);
    let bytes = String::from_utf8_lossy(first.bytes());
    assert!(bytes.starts_with("%PDF-1.6"));
    assert!(bytes.contains("/Count 2"));
    assert!(bytes.contains("/MediaBox"));
    assert!(bytes.contains("/TrimBox"));
    assert!(bytes.contains("/BleedBox"));
    assert_eq!(
        first.evidence().pages[0].bleed_expectations.top,
        BleedExpectation::Required
    );
    assert_eq!(
        first.evidence().pages[0].bleed_expectations.right,
        BleedExpectation::NotIntended
    );
}

#[test]
fn unresolved_page_images_fail_closed_by_default() {
    let document = document("missing-image");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add unresolved picture",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: image("Picture", b"not-supplied-to-print", 10_000, 10_000),
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();

    assert_eq!(
        ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap_err(),
        SceneSerializationError::MissingImageAsset
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One output fixture checks range paint and shared geometry.
fn selected_color_alpha_and_underline_paint_the_resolved_physical_geometry() {
    let bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&bytes);
    let document = document("selected-text-appearance-output");
    let mut session = DocumentSession::new(document.clone()).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add selected appearance",
                Command::AddObject {
                    location: ObjectLocation::Page(document.pages[0].id.clone()),
                    object: ObjectDraft {
                        name: "Selected appearance".to_owned(),
                        size: PhysicalSize::new(100_000, 30_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Black Red Black".to_owned(),
                            style: TextStyle {
                                font_family: "DejaVu Sans".to_owned(),
                                font_face: Some(FontFaceBinding {
                                    source: source.clone(),
                                    face_index: 0,
                                }),
                                color: Color {
                                    red: 0,
                                    green: 0,
                                    blue: 0,
                                    alpha: 255,
                                },
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story: TextStoryFormatting {
                                inline_spans: vec![InlineTextSpan {
                                    range: TextSourceRange { start: 6, end: 9 },
                                    style: InlineTextStyle {
                                        color: Some(Color {
                                            red: 255,
                                            green: 0,
                                            blue: 0,
                                            alpha: 128,
                                        }),
                                        underline: Some(true),
                                        ..InlineTextStyle::default()
                                    },
                                }],
                                paragraph_runs: Vec::new(),
                            },
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_output_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &[ExactFontResource {
            source,
            face_index: 0,
            bytes,
        }],
    )
    .unwrap();
    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    assert!(pdf.evidence().transparency.paint_alpha);
    let output = String::from_utf8_lossy(pdf.bytes());
    assert!(output.contains("1 0 0 rg"));
    assert!(output.contains("0 0 0 rg"));
    assert!(alpha_invocation_count(&output, 0.501_961) > 0);
    let rectangles: Vec<Vec<f64>> = output
        .lines()
        .filter_map(|line| line.strip_suffix(" re"))
        .filter_map(|line| {
            line.split_whitespace()
                .map(str::parse)
                .collect::<Result<Vec<_>, _>>()
                .ok()
        })
        .collect();
    let underlines: Vec<_> = publication.text_boxes()[0]
        .lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .filter_map(|run| run.underline.as_ref())
        .collect();
    assert!(!underlines.is_empty());
    for underline in underlines {
        let expected = [
            underline.x.value(),
            underline.y.value(),
            underline.width.value(),
            underline.thickness.value(),
        ]
        .map(|value| f64::from(i32::try_from(value).expect("fixture geometry fits i32")) / 1_000.0);
        assert!(
            rectangles.iter().any(|rectangle| rectangle.len() == 4
                && rectangle
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| (actual - expected).abs() < 0.001)),
            "underline must use the resolved shared geometry"
        );
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one differential journey proves that print consumes both wrapped text geometries without reshaping"
)]
fn print_uses_the_same_two_column_wrapped_glyph_geometry_before_and_after_reflow() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = document("instructional-page-print-geometry");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let composed = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Compose wrapped instructions".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Instructions".to_owned(),
                            size: PhysicalSize::new(120_000, 55_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "Hold the cards carefully, show both hands, and pause for the surprise.\n"
                                    .repeat(24),
                                style: TextStyle {
                                    font_face: Some(FontFaceBinding {
                                        source: source.clone(),
                                        face_index: 0,
                                    }),
                                    font_family: "Exact instructional face".to_owned(),
                                    ..TextStyle::default()
                                },
                                frame: TextFrame {
                                    column_count: 2,
                                    column_gap_um: 8_000,
                                    ..TextFrame::default()
                                },
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Wrapped diagram".to_owned(),
                            size: PhysicalSize::new(18_000, 20_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Shape {
                                shape: ShapeKind::Rectangle,
                                fill: Color {
                                    red: 230,
                                    green: 220,
                                    blue: 180,
                                    alpha: 255,
                                },
                                stroke: ShapeStroke::default(),
                                wrap_mode: WrapMode::Rectangular,
                            },
                        },
                    },
                ],
            },
        )
        .unwrap();
    let text_box_id = composed.created_objects[0].as_str().to_owned();
    let shape_id = composed.created_objects[1].clone();
    let resource = ExactFontResource {
        source,
        face_index: 0,
        bytes: font_bytes,
    };
    let before = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();
    let before_point = first_resolved_glyph_move(&before, &text_box_id);
    let before_pdf = ScenePdfWriter::write(&before, ScenePdfOptions::default()).unwrap();
    assert_eq!(before_pdf.evidence().scene_digest, before.semantic_digest());
    assert_pdf_contains_move(before_pdf.bytes(), before_point);

    session
        .execute(
            composed.revision,
            Transaction::single(
                "Move wrapped diagram",
                Command::MoveObject {
                    object_id: shape_id,
                    destination: ObjectLocation::Page(page_id),
                    transform: Transform::identity_at(45_000, 20_000),
                },
            ),
        )
        .unwrap();
    let after = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();
    let after_point = first_resolved_glyph_move(&after, &text_box_id);
    assert_ne!(
        after_point, before_point,
        "moving the wrap object must reflow text"
    );
    let after_pdf = ScenePdfWriter::write(&after, ScenePdfOptions::default()).unwrap();
    assert_eq!(after_pdf.evidence().scene_digest, after.semantic_digest());
    assert_pdf_contains_move(after_pdf.bytes(), after_point);
    assert_ne!(before_pdf.bytes(), after_pdf.bytes());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one save-reopen-print journey proves the durable rich story and PDF consume identical native geometry"
)]
fn rich_story_print_is_byte_stable_after_package_reopen() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = document("rich-story-print-reopen");
    let page_id = document.pages[0].id.clone();
    let story = format!(
        "Ordinary introduction.\nBold café instructions {}",
        "continue slowly, show both hands, and pause for the surprise. ".repeat(24)
    );
    let emphasis_start = story.find("Bold café").unwrap();
    let emphasis_end = emphasis_start + "Bold café".len();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Compose printable rich story".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Rich instructions".to_owned(),
                            size: PhysicalSize::new(120_000, 55_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: story,
                                style: TextStyle {
                                    font_face: Some(FontFaceBinding {
                                        source: source.clone(),
                                        face_index: 0,
                                    }),
                                    exact_font_families: vec![
                                        document_core::model::ExactFontFamilyBinding {
                                            font_family: "Exact alternate DejaVu".to_owned(),
                                            font_weight: 400,
                                            italic: false,
                                            face: FontFaceBinding {
                                                source: source.clone(),
                                                face_index: 0,
                                            },
                                        },
                                    ],
                                    font_family: "Exact DejaVu Sans".to_owned(),
                                    ..TextStyle::default()
                                },
                                frame: TextFrame {
                                    column_count: 2,
                                    column_gap_um: 8_000,
                                    ..TextFrame::default()
                                },
                                story: TextStoryFormatting {
                                    inline_spans: vec![InlineTextSpan {
                                        range: TextSourceRange {
                                            start: emphasis_start,
                                            end: emphasis_end,
                                        },
                                        style: InlineTextStyle {
                                            font_family: Some("Exact alternate DejaVu".to_owned()),
                                            font_weight: Some(400),
                                            color: Some(Color {
                                                red: 255,
                                                green: 0,
                                                blue: 0,
                                                alpha: 128,
                                            }),
                                            underline: Some(true),
                                            font_size_millipoints: None,
                                            bold: Some(false),
                                            italic: Some(false),
                                        },
                                    }],
                                    paragraph_runs: vec![ParagraphStyleRun {
                                        range: ParagraphRange { start: 1, end: 2 },
                                        style: ParagraphStyle {
                                            list_style: Some(ParagraphListStyle::Numbered {
                                                start_at: 7,
                                            }),
                                            alignment: Some(TextAlignment::Justify),
                                            start_indent_um: Some(6_000),
                                            end_indent_um: Some(4_000),
                                            first_line_indent_um: Some(3_000),
                                            line_height_per_thousand: Some(1_500),
                                            space_before_millipoints: Some(5_000),
                                            space_after_millipoints: Some(2_000),
                                        },
                                    }],
                                },
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Wrapped illustration".to_owned(),
                            size: PhysicalSize::new(18_000, 25_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Shape {
                                shape: ShapeKind::Rectangle,
                                fill: Color {
                                    red: 220,
                                    green: 210,
                                    blue: 180,
                                    alpha: 255,
                                },
                                stroke: ShapeStroke::default(),
                                wrap_mode: WrapMode::Rectangular,
                            },
                        },
                    },
                ],
            },
        )
        .unwrap();
    let text_box_id = added.created_objects[0].as_str().to_owned();
    let snapshot = ProjectSnapshot::from_session(&session);
    let resource = ExactFontResource {
        source: source.clone(),
        face_index: 0,
        bytes: font_bytes.clone(),
    };
    let before = ResolvedPublication::resolve_with_font_resources(
        &snapshot,
        core::slice::from_ref(&resource),
    )
    .unwrap();
    let marker_text = before.pages()[0]
        .objects
        .iter()
        .find_map(|object| match &object.kind {
            ResolvedSceneObjectKind::Text { layout, .. } => Some(
                layout
                    .lines
                    .iter()
                    .flat_map(|line| &line.glyph_runs)
                    .filter(|run| run.list_marker_paragraph == Some(1))
                    .map(|run| run.text.as_str())
                    .collect::<String>(),
            ),
            ResolvedSceneObjectKind::Image { .. }
            | ResolvedSceneObjectKind::Shape { .. }
            | ResolvedSceneObjectKind::Table { .. } => None,
        })
        .expect("text object");
    assert_eq!(marker_text, "7.");
    let before_move = resolved_glyph_move_at_or_after(&before, &text_box_id, emphasis_start);
    let before_pdf = ScenePdfWriter::write(&before, ScenePdfOptions::default()).unwrap();
    assert_pdf_contains_move(before_pdf.bytes(), before_move);

    let package = TemporaryPackage::new("rich-story-print-reopen");
    let mut assets = AssetStore::default();
    assert_eq!(assets.insert("font/ttf", font_bytes), source);
    save_new_package(package.path(), &snapshot, &assets).unwrap();
    let reopened = open_package(package.path()).unwrap();
    let reopened_resource = ExactFontResource {
        source: source.clone(),
        face_index: 0,
        bytes: reopened.assets.get(&source).unwrap().bytes.clone(),
    };
    let after = ResolvedPublication::resolve_with_font_resources(
        &reopened.snapshot,
        core::slice::from_ref(&reopened_resource),
    )
    .unwrap();
    let after_pdf = ScenePdfWriter::write(&after, ScenePdfOptions::default()).unwrap();

    assert_eq!(after, before);
    assert_eq!(after.semantic_digest(), before.semantic_digest());
    assert_eq!(after_pdf.evidence().scene_digest, after.semantic_digest());
    assert_eq!(after_pdf.bytes(), before_pdf.bytes());
    assert_eq!(
        resolved_glyph_move_at_or_after(&after, &text_box_id, emphasis_start),
        before_move
    );
}

#[expect(
    clippy::cast_precision_loss,
    reason = "the PDF writer deliberately narrows bounded canonical geometry to f32 point operands"
)]
fn first_resolved_glyph_move(publication: &ResolvedPublication, object_id: &str) -> (f32, f32) {
    let layout = publication
        .pages()
        .iter()
        .flat_map(|page| &page.objects)
        .find_map(|object| {
            (object.object_id.as_str() == object_id)
                .then_some(&object.kind)
                .and_then(|kind| match kind {
                    ResolvedSceneObjectKind::Text { layout, .. } => Some(layout),
                    ResolvedSceneObjectKind::Image { .. }
                    | ResolvedSceneObjectKind::Shape { .. }
                    | ResolvedSceneObjectKind::Table { .. } => None,
                })
        })
        .expect("resolved instructional Text Box");
    layout
        .lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .flat_map(|run| &run.glyphs)
        .find_map(|glyph| {
            glyph.outline.as_ref().and_then(|outline| {
                outline.commands.iter().find_map(|command| match command {
                    ResolvedOutlineCommand::MoveTo { x, y } => Some((
                        (glyph.x.value() + x.value()) as f32 / 1_000.0,
                        (glyph.y.value() - y.value()) as f32 / 1_000.0,
                    )),
                    ResolvedOutlineCommand::LineTo { .. }
                    | ResolvedOutlineCommand::QuadTo { .. }
                    | ResolvedOutlineCommand::CurveTo { .. }
                    | ResolvedOutlineCommand::Close => None,
                })
            })
        })
        .expect("first resolved glyph outline move")
}

fn assert_pdf_contains_move(pdf: &[u8], expected: (f32, f32)) {
    let content = String::from_utf8_lossy(pdf);
    let found = content.lines().any(|line| {
        let Some(operands) = line.trim().strip_suffix(" m") else {
            return false;
        };
        let mut values = operands
            .split_ascii_whitespace()
            .filter_map(|value| value.parse::<f32>().ok());
        let Some(x) = values.next() else {
            return false;
        };
        let Some(y) = values.next() else {
            return false;
        };
        values.next().is_none() && (x - expected.0).abs() < 0.001 && (y - expected.1).abs() < 0.001
    });
    assert!(
        found,
        "PDF path must use the resolved glyph move ({}, {})",
        expected.0, expected.1
    );
}

#[expect(
    clippy::cast_precision_loss,
    reason = "bounded canonical geometry narrows only to the PDF writer's point operands"
)]
fn resolved_glyph_move_at_or_after(
    publication: &ResolvedPublication,
    object_id: &str,
    source_start: usize,
) -> (f32, f32) {
    publication
        .pages()
        .iter()
        .flat_map(|page| &page.objects)
        .find(|object| object.object_id.as_str() == object_id)
        .and_then(|object| match &object.kind {
            ResolvedSceneObjectKind::Text { layout, .. } => Some(layout),
            ResolvedSceneObjectKind::Image { .. }
            | ResolvedSceneObjectKind::Shape { .. }
            | ResolvedSceneObjectKind::Table { .. } => None,
        })
        .into_iter()
        .flat_map(|layout| &layout.lines)
        .flat_map(|line| &line.glyph_runs)
        .filter(|run| run.source.end > source_start)
        .flat_map(|run| &run.glyphs)
        .find_map(|glyph| {
            glyph.outline.as_ref().and_then(|outline| {
                outline.commands.iter().find_map(|command| match command {
                    ResolvedOutlineCommand::MoveTo { x, y } => Some((
                        (glyph.x.value() + x.value()) as f32 / 1_000.0,
                        (glyph.y.value() - y.value()) as f32 / 1_000.0,
                    )),
                    ResolvedOutlineCommand::LineTo { .. }
                    | ResolvedOutlineCommand::QuadTo { .. }
                    | ResolvedOutlineCommand::CurveTo { .. }
                    | ResolvedOutlineCommand::Close => None,
                })
            })
        })
        .expect("source-addressed rich-text glyph outline")
}

struct TemporaryPackage(std::path::PathBuf);

impl TemporaryPackage {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(std::env::temp_dir().join(format!(
            "publisher-v2-{label}-{}-{nonce}.publisher-v2",
            std::process::id()
        )))
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TemporaryPackage {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_file(&self.0);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scene test proves native-outline fill, outline, shadow, extraction, and transparency together"
)]
fn text_is_painted_from_the_native_resolved_outlines() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = document("native-outline-text");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add exact text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Headline".to_owned(),
                        size: PhysicalSize::new(120_000, 40_000).unwrap(),
                        transform: Transform::identity_at(10_000, 15_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Magic".to_owned(),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: source.clone(),
                                    face_index: 0,
                                }),
                                font_family: "Exact test face".to_owned(),
                                font_size_millipoints: 24_000,
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .unwrap();
    session
        .execute(
            added.revision,
            Transaction::single(
                "Add headline effects",
                Command::UpdateObjectEffects {
                    object_id: added.created_objects[0].clone(),
                    effects: ObjectEffects {
                        shadow: Some(HardShadow {
                            color: Color {
                                red: 30,
                                green: 40,
                                blue: 50,
                                alpha: 128,
                            },
                            offset_x: Micrometres::new(2_540),
                            offset_y: Micrometres::new(1_270),
                        }),
                        text_outline: Some(ShapeStroke {
                            color: Color {
                                red: 210,
                                green: 20,
                                blue: 40,
                                alpha: 255,
                            },
                            width_millipoints: 1_500,
                        }),
                        picture_border: None,
                    },
                },
            ),
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);
    let publication = ResolvedPublication::resolve_with_font_resources(
        &project,
        &[ExactFontResource {
            source,
            face_index: 0,
            bytes: font_bytes,
        }],
    )
    .unwrap();

    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    let bytes = String::from_utf8_lossy(pdf.bytes());

    assert!(pdf.evidence().text_painted_as_native_outlines);
    assert!(
        bytes.contains(" m\n"),
        "glyph contours should contain move-to operators"
    );
    assert!(
        bytes.contains(" l\n") || bytes.contains(" c\n"),
        "glyph contours should contain native line or curve operators"
    );
    assert!(
        !bytes.contains(" BT\n"),
        "the writer must not reshape text as PDF text"
    );
    let (finished, _) = print_core::PublisherSceneAdapter::lower(
        &publication,
        ScenePdfOptions::default(),
        &[],
        None,
        None,
    )
    .unwrap();
    let shadow_matrix = finished.pages[0].items[0].transform.0;
    let outline_matrix = finished.pages[0].items[1].transform.0;
    assert!((shadow_matrix[4] - outline_matrix[4] - 7.2).abs() < 0.001);
    assert!((shadow_matrix[5] - outline_matrix[5] + 3.6).abs() < 0.001);
    let shadow = bytes.find("\nf\n").expect("shadow fill operator");
    let outline = bytes.rfind("\nS\n").expect("text outline stroke operator");
    let fill = bytes.rfind("\nf\n").expect("authored text fill operator");
    assert!(
        shadow < outline && outline < fill,
        "shadow, outline, and authored fill retain their back-to-front paint order"
    );
    assert!(pdf.evidence().transparency.paint_alpha);
}

#[test]
fn source_bearing_notdef_fails_closed_as_an_explicit_missing_glyph() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = document("exact-face-missing-glyph-gate");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add unsupported exact-font character",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Missing glyph gate".to_owned(),
                        size: PhysicalSize::new(120_000, 40_000).unwrap(),
                        transform: Transform::identity_at(10_000, 15_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Unsupported \u{10FFFF}".to_owned(),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: source.clone(),
                                    face_index: 0,
                                }),
                                font_family: "Exact DejaVu Sans".to_owned(),
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &[ExactFontResource {
            source,
            face_index: 0,
            bytes: font_bytes,
        }],
    )
    .unwrap();
    let missing = publication
        .text_boxes()
        .into_iter()
        .flat_map(|layout| &layout.lines)
        .flat_map(|line| &line.glyph_runs)
        .flat_map(|run| &run.glyphs)
        .find(|glyph| glyph.id == 0)
        .expect("the unsupported source must resolve to an explicit notdef glyph");

    assert_eq!(missing.fallback, Some(ResolvedGlyphFallback::Missing));
    assert!(missing.outline.is_none());
    assert_eq!(
        ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap_err(),
        SceneSerializationError::UnsupportedGlyphFallback
    );
}

#[test]
fn generated_marker_uses_and_validates_the_exact_base_face() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = document("empty-list-marker-synthesis-gate");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add empty bold list item",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Empty list item".to_owned(),
                        size: PhysicalSize::new(80_000, 30_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: String::new(),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: source.clone(),
                                    face_index: 0,
                                }),
                                font_family: "Exact regular face".to_owned(),
                                font_weight: 700,
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story: TextStoryFormatting {
                                inline_spans: Vec::new(),
                                paragraph_runs: vec![ParagraphStyleRun {
                                    range: ParagraphRange { start: 0, end: 1 },
                                    style: ParagraphStyle {
                                        list_style: Some(ParagraphListStyle::Bullet),
                                        ..ParagraphStyle::default()
                                    },
                                }],
                            },
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &[ExactFontResource {
            source,
            face_index: 0,
            bytes: font_bytes,
        }],
    )
    .unwrap();
    let marker = publication.text_boxes()[0]
        .lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .find(|run| run.list_marker_paragraph == Some(0))
        .expect("empty paragraph marker");
    assert!(!marker.requires_font_synthesis);
    ScenePdfWriter::write(&publication, ScenePdfOptions::default())
        .expect("an empty paragraph marker uses its exact base face");
}

#[test]
fn unresolved_table_cell_font_fails_closed_in_direct_scene_serialization() {
    let document = document("unresolved-table-cell-font");
    let page_id = document.pages[0].id.clone();
    let mut table = TableData::empty(1, 1).unwrap();
    table.cells[0] = "A".to_owned();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add unresolved table text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Unresolved table".to_owned(),
                        size: PhysicalSize::new(40_000, 20_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();

    assert_eq!(
        ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap_err(),
        SceneSerializationError::FontBindingUnresolved
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one table fixture proves exact-font layout, clipping, alignment, grid paint, and combined transparency together"
)]
fn populated_table_cells_are_painted_from_native_layout_and_clipped_to_the_grid() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = document("table-cell-layout");
    let page_id = document.pages[0].id.clone();
    let mut table = TableData::empty(1, 2).unwrap();
    table.cells[0] = "A".to_owned();
    table.cells[1] = "MAGIC ".repeat(1_000);
    table.column_weights = vec![1, 3];
    table.grid_border = TableGridBorder {
        color: Color {
            red: 80,
            green: 100,
            blue: 120,
            alpha: 180,
        },
        width_millipoints: 2_500,
    };
    table.cell_formatting = vec![
        TableCellFormatting {
            row: 0,
            column: 0,
            horizontal_alignment: Some(TextAlignment::Right),
            vertical_alignment: Some(TableVerticalAlignment::Bottom),
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 0,
            column: 1,
            horizontal_alignment: Some(TextAlignment::Left),
            vertical_alignment: Some(TableVerticalAlignment::Bottom),
            ..TableCellFormatting::default()
        },
    ];
    table.style.color.alpha = 120;
    table.style.font_face = Some(FontFaceBinding {
        source: source.clone(),
        face_index: 0,
    });
    table.style.font_family = "Exact table face".to_owned();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add populated table",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Word search".to_owned(),
                        size: PhysicalSize::new(80_000, 20_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 650_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &[ExactFontResource {
            source,
            face_index: 0,
            bytes: font_bytes,
        }],
    )
    .unwrap();
    let ResolvedSceneObjectKind::Table { table } = &publication.pages()[0].objects[0].kind else {
        panic!("expected table");
    };
    assert_eq!(table.column_boundaries_um, vec![0, 20_000, 80_000]);
    assert_eq!(table.grid_width_millipoints, 2_500);
    assert!(
        table.populated_cells[0].layout.lines[0].glyph_runs[0].glyphs[0]
            .y
            .value()
            > table.populated_cells[1].layout.lines[0].glyph_runs[0].glyphs[0]
                .y
                .value(),
        "a fitting bottom-aligned cell moves down, while an overset cell stays top anchored"
    );

    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    if let Some(path) = std::env::var_os("PUBLISHER_OPACITY_TABLE_PDF") {
        std::fs::write(path, pdf.bytes()).unwrap();
    }
    let bytes = String::from_utf8_lossy(pdf.bytes());

    assert!(pdf.evidence().text_painted_as_native_outlines);
    assert_eq!(pdf.evidence().pages[0].clipped_table_cells.len(), 1);
    assert_eq!(pdf.evidence().pages[0].clipped_table_cells[0].row, 0);
    assert_eq!(pdf.evidence().pages[0].clipped_table_cells[0].column, 1);
    assert!(pdf.evidence().transparency.object_opacity);
    assert!(pdf.evidence().transparency.paint_alpha);
    assert!(!pdf.evidence().transparency.raster_soft_mask);
    assert!(
        bytes.contains("W\n"),
        "cell text must be clipped to its grid rectangle"
    );
    assert!(
        bytes.contains("2.5 w\n"),
        "the durable grid width must reach PDF paint"
    );
    assert!(
        alpha_invocation_count(&bytes, 0.65) == 1
            && alpha_invocation_count(&bytes, 0.705_882) > 0
            && alpha_invocation_count(&bytes, 0.470_588) > 0,
        "table opacity is applied to the group; grid and text retain their paint alpha"
    );
    assert!(
        bytes.contains(" cm\n"),
        "the table object transform must be preserved"
    );
    assert!(
        !bytes.contains("BT\n"),
        "table text must not be reshaped as PDF text"
    );
}

#[test]
fn zero_width_table_border_does_not_become_a_pdf_hairline() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = document("borderless-table");
    let page_id = document.pages[0].id.clone();
    let mut table = TableData::empty(1, 1).unwrap();
    table.cells[0] = "A".to_owned();
    table.grid_border.width_millipoints = 0;
    table.style.font_face = Some(FontFaceBinding {
        source: source.clone(),
        face_index: 0,
    });
    table.style.font_family = "Exact table face".to_owned();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add borderless table",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Borderless".to_owned(),
                        size: PhysicalSize::new(40_000, 20_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &[ExactFontResource {
            source,
            face_index: 0,
            bytes: font_bytes,
        }],
    )
    .unwrap();
    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    let bytes = String::from_utf8_lossy(pdf.bytes());
    assert!(!bytes.contains("0 w\n"));
    assert!(!bytes.contains("\nS\n"));
}

#[test]
fn zero_width_transparent_stroke_does_not_claim_live_transparency() {
    let document = document("zero-width-transparent-stroke");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add opaque shape with unused transparent stroke",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Opaque panel".to_owned(),
                        size: PhysicalSize::new(60_000, 30_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Shape {
                            shape: ShapeKind::Rectangle,
                            fill: Color {
                                red: 20,
                                green: 40,
                                blue: 60,
                                alpha: 255,
                            },
                            stroke: ShapeStroke {
                                color: Color {
                                    red: 0,
                                    green: 0,
                                    blue: 0,
                                    alpha: 0,
                                },
                                width_millipoints: 0,
                            },
                            wrap_mode: WrapMode::None,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    assert!(!pdf.evidence().transparency.has_live_transparency());
}

fn document(id: &str) -> Document {
    Document::new(
        DocumentId::parse(id).unwrap(),
        id,
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap()
}

fn shape(name: &str, x: i64, y: i64, kind: ShapeKind) -> ObjectDraft {
    ObjectDraft {
        name: name.to_owned(),
        size: PhysicalSize::new(60_000, 30_000).unwrap(),
        transform: Transform::identity_at(x, y),
        opacity_millionths: 850_000,
        kind: ObjectKind::Shape {
            shape: kind,
            fill: Color {
                red: 180,
                green: 40,
                blue: 70,
                alpha: 220,
            },
            stroke: ShapeStroke {
                color: Color {
                    red: 20,
                    green: 30,
                    blue: 40,
                    alpha: 255,
                },
                width_millipoints: 750,
            },
            wrap_mode: WrapMode::None,
        },
    }
}

#[test]
fn cube_shades_are_solid_and_preserve_editor_faces() {
    for alpha in [255, 128] {
        let mut session = DocumentSession::new(document("cube-faces")).unwrap();
        let page = session.document().pages[0].id.clone();
        let mut cube = shape("Cube", 0, 0, ShapeKind::Cube);
        cube.size = PhysicalSize::new(25_400, 50_800).unwrap();
        cube.opacity_millionths = 1_000_000;
        if let ObjectKind::Shape { fill, stroke, .. } = &mut cube.kind {
            fill.alpha = alpha;
            stroke.width_millipoints = 0;
        }
        session
            .execute(
                Revision::initial(),
                Transaction {
                    label: "Add Cube".to_owned(),
                    commands: vec![Command::AddObject {
                        location: ObjectLocation::Page(page),
                        object: cube,
                    }],
                },
            )
            .unwrap();
        let publication =
            ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
        let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
        let bytes = String::from_utf8_lossy(pdf.bytes());
        for alpha in ["/ca 0.86", "/ca 0.7", "/ca 0.55"] {
            assert!(
                !bytes.contains(alpha),
                "Cube shading must not introduce opacity {alpha}"
            );
        }
        assert_eq!(pdf.evidence().transparency.paint_alpha, alpha < 255);
        // Non-square dimensions preserve the same normalized diamond faces as the
        // editor: top/left/right each begin at these scaled positions.
        for vertex in ["7.2 43.2 m", "36 74.88 m"] {
            assert!(bytes.contains(vertex), "missing Cube face vertex {vertex}");
        }
    }
}

fn image(name: &str, identity: &[u8], x: i64, y: i64) -> ObjectDraft {
    ObjectDraft {
        name: name.to_owned(),
        size: PhysicalSize::new(45_000, 30_000).unwrap(),
        transform: Transform::identity_at(x, y),
        opacity_millionths: 750_000,
        kind: ObjectKind::Image {
            asset: AssetHash::for_bytes(identity),
            wrap_mode: WrapMode::None,
            content_kind: document_core::model::ImageContentKind::ContinuousTone,
            crop: document_core::model::ImageCrop::default(),
            flip: document_core::model::ImageFlip::default(),
            lock_aspect_ratio: true,
        },
    }
}

#[test]
fn line_paints_an_open_stroke_and_stroked_shadow_without_fill() {
    let mut session = DocumentSession::new(document("line-scene")).unwrap();
    let mut line = shape("Line", 10_000, 20_000, ShapeKind::Line);
    line.size = PhysicalSize::new(25_400, 12_700).unwrap();
    line.opacity_millionths = 1_000_000;
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add line",
                Command::AddObject {
                    location: ObjectLocation::Page(session.document().pages[0].id.clone()),
                    object: line,
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    let content = String::from_utf8_lossy(pdf.bytes());
    assert!(
        content.contains("0 0 m\n72 36 l\nS"),
        "line must be an open stroke"
    );
    assert!(
        !content.lines().any(|line| matches!(line, "f" | "f*" | "h")),
        "no filled or closed line path"
    );
    assert!(
        !pdf.evidence().transparency.has_live_transparency(),
        "unused translucent fill must not count as paint"
    );
    session
        .execute(
            added.revision,
            Transaction::single(
                "Shadow",
                Command::UpdateObjectEffects {
                    object_id: added.created_objects[0].clone(),
                    effects: ObjectEffects {
                        shadow: Some(HardShadow {
                            color: Color {
                                red: 0,
                                green: 0,
                                blue: 0,
                                alpha: 255,
                            },
                            offset_x: Micrometres::new(1_000),
                            offset_y: Micrometres::new(2_000),
                        }),
                        ..ObjectEffects::default()
                    },
                },
            ),
        )
        .unwrap();
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    let content = String::from_utf8_lossy(pdf.bytes());
    assert_eq!(
        content.matches("0 0 m\n72 36 l\nS").count(),
        2,
        "line and shadow share the open stroke silhouette"
    );
    assert!(!content.lines().any(|line| matches!(line, "f" | "f*" | "h")));
}

#[test]
fn rounded_rectangle_uses_the_same_outline_for_fill_border_and_shadow() {
    let mut session = DocumentSession::new(document("rounded-shape")).unwrap();
    let page = session.document().pages[0].id.clone();
    let mut rounded = shape("Rounded rectangle", 10_000, 20_000, ShapeKind::Rectangle);
    rounded.size = PhysicalSize::new(25_400, 12_700).unwrap();
    rounded.opacity_millionths = 1_000_000;
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add rectangle",
                Command::AddObject {
                    location: ObjectLocation::Page(page),
                    object: rounded,
                },
            ),
        )
        .unwrap();
    let id = added.created_objects[0].clone();
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Rounded appearance".into(),
                commands: vec![
                    Command::UpdateCornerRounding {
                        object_id: id.clone(),
                        rounding: document_core::model::CornerRounding {
                            top_left_um: 6_350,
                            top_right_um: 6_350,
                            bottom_right_um: 6_350,
                            bottom_left_um: 6_350,
                            linked: true,
                        },
                    },
                    Command::UpdateObjectEffects {
                        object_id: id,
                        effects: ObjectEffects {
                            shadow: Some(HardShadow {
                                color: Color {
                                    red: 0,
                                    green: 0,
                                    blue: 0,
                                    alpha: 255,
                                },
                                offset_x: Micrometres::new(1_000),
                                offset_y: Micrometres::new(2_000),
                            }),
                            ..ObjectEffects::default()
                        },
                    },
                ],
            },
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);
    let publication = ResolvedPublication::resolve(&project).unwrap();
    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    let content = String::from_utf8_lossy(pdf.bytes());
    assert_eq!(
        content.matches("18 0 m\n54 0 l\n").count(),
        2,
        "combined fill/stroke and shadow start on the shared physical path"
    );
    assert_eq!(
        content.lines().filter(|line| line.ends_with(" c")).count(),
        8
    );
    assert!(
        !content.contains("0 0 72 36 re"),
        "rounded appearance must never fall back to a square"
    );
    assert_eq!(
        publication.pages()[0].objects[0].corner_rounding,
        project.document.pages[0].objects[0].corner_rounding
    );
}

#[test]
fn triangle_and_arrow_emit_editable_closed_paths_and_survive_reopen() {
    let mut session = DocumentSession::new(document("common-shapes")).unwrap();
    let page = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Common shapes".into(),
                commands: [ShapeKind::Triangle, ShapeKind::Arrow]
                    .into_iter()
                    .map(|kind| Command::AddObject {
                        location: ObjectLocation::Page(page.clone()),
                        object: shape("Common shape", 10_000, 20_000, kind),
                    })
                    .collect(),
            },
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);
    let folder =
        std::env::temp_dir().join(format!("publisher-common-shapes-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("shapes.publisher-v2");
    save_new_package(&path, &project, &AssetStore::default()).unwrap();
    let reopened = open_package(&path).unwrap();
    let _ignored = std::fs::remove_dir_all(&folder);
    assert_eq!(reopened.snapshot.document, project.document);
    let publication = ResolvedPublication::resolve(&reopened.snapshot).unwrap();
    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    let content = String::from_utf8_lossy(pdf.bytes());
    assert_eq!(
        content.lines().filter(|line| *line == "h").count(),
        2,
        "each shape has one closed combined fill/stroke outline"
    );
    assert_eq!(
        content.lines().filter(|line| line.ends_with(" l")).count(),
        8
    );
    let mut editing =
        DocumentSession::from_saved(reopened.snapshot.document, reopened.snapshot.revision)
            .unwrap();
    let id = editing.document().pages[0].objects[1].id.clone();
    editing
        .execute(
            editing.revision(),
            Transaction::single(
                "Resize arrow",
                Command::ResizeObject {
                    object_id: id,
                    size: PhysicalSize::new(80_000, 20_000).unwrap(),
                },
            ),
        )
        .unwrap();
    assert!(matches!(
        editing.document().pages[0].objects[1].kind,
        ObjectKind::Shape {
            shape: ShapeKind::Arrow,
            ..
        }
    ));
}

/// Set `PUBLISHER_OPACITY_FIXTURE_PDF` to write these exact scene bytes for
/// independent rendering. All artwork is generated geometry, with no assets.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one generated differential fixture binds overlapping paints and opacity controls"
)]
fn object_opacity_composites_overlapping_paints_once() {
    let mut session = DocumentSession::new(document("object-opacity-fixture")).unwrap();
    for (index, opacity, alpha) in [(0, 500_000, 255), (1, 500_000, 128), (2, 1_000_000, 255)] {
        let black = Color {
            red: 0,
            green: 0,
            blue: 0,
            alpha,
        };
        let added = session
            .execute(
                session.revision(),
                Transaction::single(
                    "Overlap fixture",
                    Command::AddObject {
                        location: ObjectLocation::Page(session.document().pages[0].id.clone()),
                        object: ObjectDraft {
                            name: format!("Opacity control {index}"),
                            size: PhysicalSize::new(25_400, 25_400).unwrap(),
                            transform: Transform::identity_at(25_400 + index * 50_800, 25_400),
                            opacity_millionths: opacity,
                            kind: ObjectKind::Shape {
                                shape: ShapeKind::Rectangle,
                                fill: black,
                                stroke: ShapeStroke {
                                    color: black,
                                    width_millipoints: 12_000,
                                },
                                wrap_mode: WrapMode::None,
                            },
                        },
                    },
                ),
            )
            .unwrap();
        session
            .execute(
                added.revision,
                Transaction::single(
                    "Overlapping shadow",
                    Command::UpdateObjectEffects {
                        object_id: added.created_objects[0].clone(),
                        effects: ObjectEffects {
                            shadow: Some(HardShadow {
                                color: black,
                                offset_x: Micrometres::new(12_700),
                                offset_y: Micrometres::new(12_700),
                            }),
                            ..ObjectEffects::default()
                        },
                    },
                ),
            )
            .unwrap();
    }
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    if let Some(path) = std::env::var_os("PUBLISHER_OPACITY_FIXTURE_PDF") {
        std::fs::write(path, pdf.bytes()).unwrap();
    }
    let bytes = String::from_utf8_lossy(pdf.bytes());
    assert_eq!(
        bytes.matches("/Subtype /Form").count(),
        2,
        "only translucent objects require completed visual groups"
    );
    assert_eq!(
        bytes.matches("/I true").count(),
        3,
        "page and object groups isolate overlapping paints"
    );
    assert_eq!(
        alpha_invocation_count(&bytes, 0.5),
        2,
        "object alpha belongs only at each group invocation"
    );
    assert!(
        alpha_invocation_count(&bytes, 0.501_961) > 0,
        "paint alpha stays independent inside the group"
    );
    assert!(
        alpha_invocation_count(&bytes, 0.250_980) == 0,
        "paint alpha must not absorb object alpha"
    );
}

/// Set `PUBLISHER_OPACITY_GEOMETRY_PDF` to render rotated objects and shadows.
#[test]
fn grouped_opacity_preserves_rotated_lines_and_shadow_geometry() {
    let mut session = DocumentSession::new(document("opacity-rotated-geometry")).unwrap();
    for (index, opacity) in [(0, 500_000), (1, 1_000_000)] {
        let mut draft = shape(
            "Rotated line",
            25_400 + index * 76_200,
            101_600,
            ShapeKind::Line,
        );
        draft.size = PhysicalSize::new(25_400, 12_700).unwrap();
        draft.opacity_millionths = opacity;
        draft.transform.rotation_microdegrees = -30_000_000;
        draft.transform.scale_x_nanos = 1_250_000_000;
        draft.transform.scale_y_nanos = 800_000_000;
        if let ObjectKind::Shape { stroke, .. } = &mut draft.kind {
            stroke.color = Color {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            };
            stroke.width_millipoints = 12_000;
        }
        let added = session
            .execute(
                session.revision(),
                Transaction::single(
                    "Rotated line",
                    Command::AddObject {
                        location: ObjectLocation::Page(session.document().pages[0].id.clone()),
                        object: draft,
                    },
                ),
            )
            .unwrap();
        session
            .execute(
                added.revision,
                Transaction::single(
                    "Line shadow",
                    Command::UpdateObjectEffects {
                        object_id: added.created_objects[0].clone(),
                        effects: ObjectEffects {
                            shadow: Some(HardShadow {
                                color: Color {
                                    red: 0,
                                    green: 0,
                                    blue: 0,
                                    alpha: 255,
                                },
                                offset_x: Micrometres::new(2_000),
                                offset_y: Micrometres::new(2_000),
                            }),
                            ..ObjectEffects::default()
                        },
                    },
                ),
            )
            .unwrap();
    }
    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let pdf = ScenePdfWriter::write(&publication, ScenePdfOptions::default()).unwrap();
    if let Some(path) = std::env::var_os("PUBLISHER_OPACITY_GEOMETRY_PDF") {
        std::fs::write(path, pdf.bytes()).unwrap();
    }
    let bytes = String::from_utf8_lossy(pdf.bytes());
    assert_eq!(bytes.matches("/Subtype /Form").count(), 1);
    assert_eq!(
        bytes.matches("0 0 m\n72 36 l\nS").count(),
        4,
        "both rotated strokes and their shadows retain the open silhouette"
    );
    assert!(!bytes.lines().any(|line| matches!(line, "f" | "f*" | "h")));
    assert!(
        bytes.contains("/BBox [-9 -9 621 801]"),
        "Form clips at the top-down bleed extent, not the object rectangle"
    );
    assert_eq!(alpha_invocation_count(&bytes, 0.5), 1);
}
