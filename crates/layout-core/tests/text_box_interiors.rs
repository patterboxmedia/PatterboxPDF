use document_core::model::{
    Color, DocumentId, FontFaceBinding, Insets, ObjectDraft, ObjectKind, PhysicalSize, ShapeKind,
    ShapeStroke, TextBoxSizingMode, TextFrame, TextStoryFormatting, TextStyle,
    TextVerticalAlignment, Transform, WrapMode, WrapSettings, WrapSide,
};
use document_core::{
    AssetHash, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Revision,
    Transaction,
};
use layout_core::{
    ExactFontResource, PublicationLayoutError, ResolvedPublication, ResolvedTextBoxPreview,
};

fn font() -> ExactFontResource {
    let bytes = include_bytes!("fixtures/DejaVuSans.ttf").to_vec();
    ExactFontResource {
        source: AssetHash::for_bytes(&bytes),
        face_index: 0,
        bytes,
    }
}

fn project(text: &str, frame: TextFrame, obstacle: bool) -> ProjectSnapshot {
    let document = Document::new(
        DocumentId::parse("text-box-interior").unwrap(),
        "Instructions",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::default(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let mut objects = vec![ObjectDraft {
        name: "Instructions".to_owned(),
        size: PhysicalSize::new(100_000, 80_000).unwrap(),
        transform: Transform::identity_at(0, 0),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Text {
            text: text.to_owned(),
            style: TextStyle {
                font_face: Some(FontFaceBinding {
                    source: font().source,
                    face_index: 0,
                }),
                underline: true,
                ..TextStyle::default()
            },
            frame,
            story: TextStoryFormatting::default(),
        },
    }];
    if obstacle {
        objects.push(ObjectDraft {
            name: "Diagram".to_owned(),
            size: PhysicalSize::new(20_000, 25_000).unwrap(),
            transform: Transform::identity_at(40_000, 35_000),
            opacity_millionths: 1_000_000,
            kind: ObjectKind::Shape {
                shape: ShapeKind::Rectangle,
                fill: Color {
                    red: 0,
                    green: 0,
                    blue: 0,
                    alpha: 255,
                },
                stroke: ShapeStroke::default(),
                wrap_mode: WrapMode::Rectangular,
            },
        });
    }
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add composition".to_owned(),
                commands: objects
                    .into_iter()
                    .map(|object| Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object,
                    })
                    .collect(),
            },
        )
        .unwrap();
    ProjectSnapshot::from_session(&session)
}

fn layout(project: &ProjectSnapshot) -> ResolvedTextBoxPreview {
    let resources = [font()];
    let preview = ResolvedPublication::resolve_with_font_resources(project, &resources).unwrap();
    let output =
        ResolvedPublication::resolve_output_with_font_resources(project, &resources).unwrap();
    let result = preview.text_boxes()[0].clone();
    assert_eq!(
        &result,
        output.text_boxes()[0],
        "preview and resource-independent output share interior and glyph geometry"
    );
    result
}

fn millipoints(um: i64) -> i64 {
    um * 360 / 127
}

#[test]
fn margins_columns_and_vertical_alignment_use_physical_native_geometry() {
    let frame = TextFrame {
        margins: Insets::uniform(5_000).unwrap(),
        column_count: 2,
        column_gap_um: 8_000,
        ..TextFrame::default()
    };
    let project = project(
        "Read each step carefully. ".repeat(80).as_str(),
        frame,
        false,
    );
    let composed = layout(&project);
    assert_eq!(composed.content_bounds.x.value(), 5_000);
    assert_eq!(composed.content_bounds.y.value(), 5_000);
    assert_eq!(composed.content_bounds.width.value(), 90_000);
    assert_eq!(composed.content_bounds.height.value(), 70_000);
    assert!(composed.lines.iter().any(|line| line.column_index == 1));
    assert!(
        composed.lines.iter().all(
            |line| line.x.value() >= millipoints(5_000) && line.y.value() >= millipoints(5_000)
        )
    );
    assert!(
        composed.overset,
        "exact unplaced source survives smaller margins and columns"
    );
    assert_eq!(composed.vertical_offset.value(), 0);
    let mut short = project;
    let ObjectKind::Text { text, frame, .. } = &mut short.document.pages[0].objects[0].kind else {
        panic!("Text Box")
    };
    *text = "Read these three steps.\nPlace the band.\nOpen your hand.".to_owned();
    frame.column_count = 1;
    let top = layout(&short);
    for alignment in [TextVerticalAlignment::Middle, TextVerticalAlignment::Bottom] {
        let ObjectKind::Text { frame, .. } = &mut short.document.pages[0].objects[0].kind else {
            panic!("Text Box")
        };
        frame.vertical_alignment = alignment;
        let aligned = layout(&short);
        assert!(!aligned.overset);
        assert!(aligned.vertical_offset.value() > 0);
        for (before, after) in top.lines.iter().zip(&aligned.lines) {
            assert_eq!(before.source, after.source);
            assert_eq!(before.x, after.x);
            assert_eq!(
                after.y.value() - before.y.value(),
                aligned.vertical_offset.value()
            );
            assert_eq!(
                after.baseline.value() - before.baseline.value(),
                aligned.vertical_offset.value()
            );
            assert_eq!(
                after.glyph_runs[0].glyphs[0].y.value() - before.glyph_runs[0].glyphs[0].y.value(),
                aligned.vertical_offset.value()
            );
            assert_eq!(
                after.glyph_runs[0].underline.as_ref().unwrap().y.value()
                    - before.glyph_runs[0].underline.as_ref().unwrap().y.value(),
                aligned.vertical_offset.value()
            );
        }
    }
}

#[test]
fn wrapped_middle_and_bottom_keep_distance_and_chosen_side_clear() {
    let text = "Read the step.\nPlace the band.\nClose your hand.\nOpen it slowly.";
    let frame = TextFrame {
        margins: Insets::uniform(5_000).unwrap(),
        ..TextFrame::default()
    };
    let mut project = project(text, frame, true);
    for side in [WrapSide::Largest, WrapSide::Left, WrapSide::Right] {
        project.document.pages[0].objects[1].wrap_settings = WrapSettings {
            distance_um: 3_000,
            side,
        };
        for alignment in [TextVerticalAlignment::Middle, TextVerticalAlignment::Bottom] {
            let ObjectKind::Text { frame, .. } = &mut project.document.pages[0].objects[0].kind
            else {
                panic!("Text Box")
            };
            frame.vertical_alignment = alignment;
            let result = layout(&project);
            assert!(!result.overset);
            assert!(result.vertical_offset.value() > 0);
            let mut alongside = false;
            for line in &result.lines {
                if line.y.value() < millipoints(63_000)
                    && line.y.value() + line.height.value() > millipoints(32_000)
                {
                    alongside = true;
                    let left = line.x.value() + line.width.value() <= millipoints(37_000);
                    let right = line.x.value() >= millipoints(63_000);
                    assert!(
                        left || right,
                        "authored wrap clearance separates the complete line fragment from the obstacle"
                    );
                    if side == WrapSide::Left {
                        assert!(left);
                    }
                    if side == WrapSide::Right {
                        assert!(right);
                    }
                }
            }
            if alignment == TextVerticalAlignment::Middle {
                assert!(
                    alongside,
                    "middle composition actually encounters the obstacle"
                );
            }
        }
    }
}

#[test]
fn fit_text_includes_margins_in_composed_bounds_and_glyph_offsets() {
    let mut project = project(
        "A heading",
        TextFrame {
            sizing_mode: TextBoxSizingMode::Fit,
            ..TextFrame::default()
        },
        false,
    );
    let bare = layout(&project);
    let ObjectKind::Text { frame, .. } = &mut project.document.pages[0].objects[0].kind else {
        panic!("Text Box")
    };
    frame.margins = Insets::uniform(4_000).unwrap();
    let padded = layout(&project);
    assert_eq!(
        padded.composed_size.width.value(),
        bare.composed_size.width.value() + 8_000
    );
    assert_eq!(
        padded.composed_size.height.value(),
        bare.composed_size.height.value() + 8_000
    );
    assert_eq!(
        padded.lines[0].x.value() - bare.lines[0].x.value(),
        millipoints(4_000)
    );
    assert_eq!(
        padded.lines[0].y.value() - bare.lines[0].y.value(),
        millipoints(4_000)
    );
    assert_eq!(
        padded.lines[0].glyph_runs[0].glyphs[0].x.value()
            - bare.lines[0].glyph_runs[0].glyphs[0].x.value(),
        millipoints(4_000)
    );
}

#[test]
fn wrapped_alignment_that_exceeds_the_search_bound_is_explicitly_unavailable() {
    let mut project = project(
        "Read the step.",
        TextFrame {
            vertical_alignment: TextVerticalAlignment::Bottom,
            ..TextFrame::default()
        },
        true,
    );
    // Native admission can read historical physical sizes beyond the desktop
    // authoring limit; an unsettled composition must never claim acceptance.
    project.document.pages[0].objects[0].size = PhysicalSize::new(100_000, 10_000_000_000).unwrap();
    assert_eq!(
        ResolvedPublication::resolve_with_font_resources(&project, &[font()]).err(),
        Some(PublicationLayoutError::VerticalAlignmentDidNotSettle)
    );
}
