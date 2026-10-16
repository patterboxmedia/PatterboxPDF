use document_core::model::{
    Color, DocumentId, FontFaceBinding, GuideAxis, HardShadow, ImageContentKind, ImageCrop,
    InlineTextSpan, InlineTextStyle, Insets, Micrometres, ObjectDraft, ObjectEffects, ObjectId,
    ObjectKind, PageId, ParagraphListStyle, ParagraphRange, ParagraphStyle, ParagraphStyleRun,
    PhysicalSize, Revision, ShapeKind, ShapeStroke, TableCellFormatting, TableData,
    TableGridBorder, TableVerticalAlignment, TextAlignment, TextBoxSizingMode, TextFrame,
    TextSourceRange, TextStoryFormatting, TextStyle, Transform, WrapMode,
};

#[test]
fn page_guides_do_not_enter_the_resolved_publication_scene() {
    let document = Document::new(
        DocumentId::parse("guided-preview-document").unwrap(),
        "Guided preview",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(0).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add guide",
                Command::AddGuide {
                    page_id,
                    axis: GuideAxis::Vertical,
                    position: Micrometres::new(25_400),
                },
            ),
        )
        .unwrap();
    let preview = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    assert_eq!(preview.pages().len(), 1);
    assert!(
        preview.pages()[0].objects.is_empty(),
        "guides are workspace aids, not export objects"
    );
}
use document_core::{
    AssetHash, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
};
use layout_core::{
    ExactFontResource, FontSubstitutionStatus, PreviewBudgetState, ResolvedPublication,
    ResolvedSceneObjectKind, TextBoxPreviewState, preview_font_face_bindings,
};

#[test]
fn resolved_effects_are_digest_bound_and_keep_paint_regions_separate_before_transform() {
    let document = Document::new(
        DocumentId::parse("effect-preview-document").unwrap(),
        "Effect preview",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add picture",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Picture".to_owned(),
                        size: PhysicalSize::new(10_000, 20_000).unwrap(),
                        transform: Transform::identity_at(30_000, 40_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Image {
                            asset: AssetHash::for_bytes(b"effect picture"),
                            wrap_mode: WrapMode::None,
                            content_kind: ImageContentKind::ContinuousTone,
                            crop: ImageCrop::default(),
                            flip: document_core::model::ImageFlip::default(),
                            lock_aspect_ratio: true,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let without_effects = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session))
        .unwrap()
        .semantic_digest();
    let object_id = session.document().pages[0].objects[0].id.clone();
    let effects = ObjectEffects {
        shadow: Some(HardShadow {
            color: Color {
                red: 12,
                green: 34,
                blue: 56,
                alpha: 180,
            },
            offset_x: Micrometres::new(-2_000),
            offset_y: Micrometres::new(3_000),
        }),
        text_outline: None,
        picture_border: Some(ShapeStroke {
            color: Color {
                red: 210,
                green: 20,
                blue: 40,
                alpha: 255,
            },
            width_millipoints: 7_200,
        }),
    };
    session
        .execute(
            added.revision,
            Transaction::single(
                "Style picture",
                Command::UpdateObjectEffects { object_id, effects },
            ),
        )
        .unwrap();

    let resolved = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let object = &resolved.pages()[0].objects[0];
    assert_eq!(object.effects, effects);
    assert_ne!(resolved.semantic_digest(), without_effects);
    let regions = object.paint_regions().unwrap();
    assert_eq!(regions.len(), 3);
    assert!(regions.iter().any(|bounds| {
        bounds.x.value() == 30_000
            && bounds.y.value() == 40_000
            && bounds.width.value() == 10_000
            && bounds.height.value() == 20_000
    }));
    assert!(regions.iter().any(|bounds| {
        bounds.x.value() == 28_730
            && bounds.y.value() == 38_730
            && bounds.width.value() == 12_540
            && bounds.height.value() == 22_540
    }));
    assert!(regions.iter().any(|bounds| {
        bounds.x.value() == 28_000
            && bounds.y.value() == 43_000
            && bounds.width.value() == 10_000
            && bounds.height.value() == 20_000
    }));
}

#[test]
fn publication_preview_binds_wrapped_text_lines_to_revision_and_text_box() {
    let document = Document::new(
        DocumentId::parse("preview-document").unwrap(),
        "Preview",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let outcome = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Compose wrapped text".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Body copy".to_owned(),
                            size: PhysicalSize::new(100_000, 80_000).unwrap(),
                            transform: Transform::identity_at(10_000, 10_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "Text should wrap around the picture before continuing across the text box. ".repeat(8),
                                style: TextStyle::default(),
                                frame: TextFrame::default(),
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Picture area".to_owned(),
                            size: PhysicalSize::new(30_000, 24_000).unwrap(),
                            transform: Transform::identity_at(10_000, 10_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Shape {
                                shape: ShapeKind::Rectangle,
                                fill: Color { red: 20, green: 80, blue: 120, alpha: 255 },
                                stroke: ShapeStroke::default(),
                                wrap_mode: WrapMode::Rectangular,
                            },
                        },
                    },
                ],
            },
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);
    let text_box_id = project.document.pages[0].objects[0].id.clone();

    let preview = ResolvedPublication::resolve(&project).expect("resolve native preview");

    assert_eq!(preview.revision(), outcome.revision);
    let text_box = preview
        .text_boxes()
        .into_iter()
        .find(|candidate| candidate.object_id == text_box_id)
        .expect("text box preview");
    assert!(!text_box.lines.is_empty());
    assert_eq!(text_box.lines[0].source.start, 0);
    assert!(
        text_box.lines[0].x.value() > 0,
        "wrapped picture must offset the first line"
    );
    assert!(!text_box.lines[0].text.is_empty());
}

fn fit_text_fixture() -> (
    DocumentSession,
    ObjectId,
    ObjectId,
    PageId,
    ExactFontResource,
) {
    let font_bytes = include_bytes!("fixtures/DejaVuSans.ttf").to_vec();
    let source = AssetHash::for_bytes(&font_bytes);
    let document = Document::new(
        DocumentId::parse("fit-text-layout").unwrap(),
        "Fit text layout",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let composed = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Compose fit text beside a wrapped picture".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Display text".to_owned(),
                            size: PhysicalSize::new(6_350, 6_350).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "Magic\nLesson".to_owned(),
                                style: TextStyle {
                                    font_face: Some(FontFaceBinding {
                                        source: source.clone(),
                                        face_index: 0,
                                    }),
                                    font_family: "Exact fit face".to_owned(),
                                    ..TextStyle::default()
                                },
                                frame: TextFrame {
                                    sizing_mode: TextBoxSizingMode::Fit,
                                    ..TextFrame::default()
                                },
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Overlapping picture".to_owned(),
                            size: PhysicalSize::new(80_000, 80_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Image {
                                asset: AssetHash::for_bytes(b"fit exclusion decoy"),
                                wrap_mode: WrapMode::Rectangular,
                                content_kind: ImageContentKind::ContinuousTone,
                                crop: ImageCrop::default(),
                                flip: document_core::model::ImageFlip::default(),
                                lock_aspect_ratio: true,
                            },
                        },
                    },
                ],
            },
        )
        .unwrap();
    (
        session,
        composed.created_objects[0].clone(),
        composed.created_objects[1].clone(),
        page_id,
        ExactFontResource {
            source,
            face_index: 0,
            bytes: font_bytes,
        },
    )
}

#[test]
fn fit_text_shrink_wraps_all_source_and_ignores_page_exclusions() {
    let (mut session, text_id, picture_id, page_id, resource) = fit_text_fixture();
    let resolve = |session: &DocumentSession| {
        ResolvedPublication::resolve_with_font_resources(
            &ProjectSnapshot::from_session(session),
            core::slice::from_ref(&resource),
        )
        .unwrap()
    };
    let before = resolve(&session);
    let before_object = before.pages()[0]
        .objects
        .iter()
        .find(|object| object.object_id == text_id)
        .unwrap();
    let ResolvedSceneObjectKind::Text { layout, .. } = &before_object.kind else {
        panic!("resolved fit Text Box");
    };
    assert_eq!(layout.sizing_mode, TextBoxSizingMode::Fit);
    assert_eq!(layout.composed_size, before_object.size);
    assert!(before_object.size.width.value() > 6_350);
    assert!(before_object.size.height.value() > 6_350);
    assert!(!layout.overset);
    assert_eq!(layout.overset_source, None);
    assert_eq!(layout.lines.first().unwrap().x.value(), 0);
    assert_eq!(
        layout.lines.last().unwrap().source.end,
        "Magic\nLesson".len()
    );
    let before_size = before_object.size;
    let before_lines = layout.lines.clone();

    session
        .execute(
            session.revision(),
            Transaction::single(
                "Move irrelevant wrapped picture",
                Command::MoveObject {
                    object_id: picture_id,
                    destination: ObjectLocation::Page(page_id),
                    transform: Transform::identity_at(100_000, 120_000),
                },
            ),
        )
        .unwrap();
    let after = resolve(&session);
    let after_object = after.pages()[0]
        .objects
        .iter()
        .find(|object| object.object_id == text_id)
        .unwrap();
    let ResolvedSceneObjectKind::Text { layout, .. } = &after_object.kind else {
        panic!("resolved fit Text Box after page-object move");
    };
    assert_eq!(after_object.size, before_size);
    assert_eq!(layout.lines, before_lines);
}

#[test]
fn empty_and_whitespace_fit_text_keep_visible_minimum_bounds_without_overset() {
    for (document_id, text) in [("empty-fit-text", ""), ("whitespace-fit-text", "  \n  ")] {
        let document = Document::new(
            DocumentId::parse(document_id).unwrap(),
            "Minimum fit text",
            PhysicalSize::new(215_900, 279_400).unwrap(),
            Insets::uniform(0).unwrap(),
        )
        .unwrap();
        let page_id = document.pages[0].id.clone();
        let mut session = DocumentSession::new(document).unwrap();
        session
            .execute(
                Revision::initial(),
                Transaction::single(
                    "Add minimum fit text",
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Fit text".to_owned(),
                            size: PhysicalSize::new(6_350, 6_350).unwrap(),
                            transform: Transform::identity_at(0, 0),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: text.to_owned(),
                                style: TextStyle::default(),
                                frame: TextFrame {
                                    sizing_mode: TextBoxSizingMode::Fit,
                                    ..TextFrame::default()
                                },
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                ),
            )
            .unwrap();
        let resolved =
            ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
        let object = &resolved.pages()[0].objects[0];
        let ResolvedSceneObjectKind::Text { layout, .. } = &object.kind else {
            panic!("fit text scene object");
        };
        assert!(object.size.width.value() >= TextFrame::MIN_FIT_WIDTH_MICROMETRES);
        assert!(object.size.height.value() >= TextFrame::MIN_FIT_HEIGHT_MICROMETRES);
        assert!(!layout.overset);
        assert_eq!(layout.overset_source, None);
    }
}

#[test]
fn missing_font_and_budget_limits_never_masquerade_as_geometric_overset() {
    let document = Document::new(
        DocumentId::parse("incomplete-text-preview-reasons").unwrap(),
        "Incomplete preview reasons",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(0).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add independently incomplete text".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Missing font".to_owned(),
                            size: PhysicalSize::new(40_000, 20_000).unwrap(),
                            transform: Transform::identity_at(0, 0),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "x".repeat(32 * 1024),
                                style: TextStyle {
                                    font_face: Some(FontFaceBinding {
                                        source: AssetHash::for_bytes(b"unavailable font"),
                                        face_index: 0,
                                    }),
                                    ..TextStyle::default()
                                },
                                frame: TextFrame {
                                    sizing_mode: TextBoxSizingMode::Fit,
                                    ..TextFrame::default()
                                },
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Budget limited".to_owned(),
                            size: PhysicalSize::new(40_000, 20_000).unwrap(),
                            transform: Transform::identity_at(50_000, 0),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "x".repeat(32 * 1024),
                                style: TextStyle {
                                    font_face: Some(FontFaceBinding {
                                        source: AssetHash::for_bytes(b"unavailable font"),
                                        face_index: 0,
                                    }),
                                    ..TextStyle::default()
                                },
                                frame: TextFrame::default(),
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Budget limited".to_owned(),
                            size: PhysicalSize::new(40_000, 20_000).unwrap(),
                            transform: Transform::identity_at(100_000, 0),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "x".to_owned(),
                                style: TextStyle::default(),
                                frame: TextFrame::default(),
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                ],
            },
        )
        .unwrap();
    let resolved = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let previews = resolved.text_boxes();
    assert_eq!(previews[0].preview_state, TextBoxPreviewState::MissingFont);
    assert_eq!(previews[0].sizing_mode, TextBoxSizingMode::Fit);
    assert_eq!(
        previews[0].composed_size,
        PhysicalSize::new(40_000, 20_000).unwrap()
    );
    assert_eq!(previews[1].preview_state, TextBoxPreviewState::MissingFont);
    assert_eq!(
        previews[2].preview_state,
        TextBoxPreviewState::BudgetExceeded
    );
    for preview in previews {
        assert!(!preview.overset);
        assert_eq!(preview.overset_source, None);
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one instructional-page journey keeps column geometry, two wrap objects, pasteboard isolation, reflow, and exact overset evidence comparable"
)]
fn instructional_page_reflows_two_columns_around_page_objects_but_never_pasteboard_objects() {
    let document = Document::new(
        DocumentId::parse("instructional-page-layout").unwrap(),
        "Instructional page",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let story = "Magic café instructions — hold the cards carefully, show both hands, and pause for the surprise. "
        .repeat(36);
    let mut session = DocumentSession::new(document).unwrap();
    let composed = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Compose instructional page".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Instructions".to_owned(),
                            size: PhysicalSize::new(120_000, 55_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: story.clone(),
                                style: TextStyle::default(),
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
                            name: "Card picture".to_owned(),
                            size: PhysicalSize::new(18_000, 55_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Image {
                                asset: AssetHash::for_bytes(b"instructional card picture"),
                                wrap_mode: WrapMode::Rectangular,
                                content_kind: ImageContentKind::ContinuousTone,
                                crop: ImageCrop::default(),
                                flip: document_core::model::ImageFlip::default(),
                                lock_aspect_ratio: false,
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Hand diagram".to_owned(),
                            size: PhysicalSize::new(20_000, 15_000).unwrap(),
                            transform: Transform::identity_at(80_000, 45_000),
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
    let text_box_id = composed.created_objects[0].clone();
    let picture_id = composed.created_objects[1].clone();
    let shape_id = composed.created_objects[2].clone();
    let baseline = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let baseline_text = baseline
        .text_boxes()
        .into_iter()
        .find(|text_box| text_box.object_id == text_box_id)
        .unwrap()
        .clone();
    let column_two_start = 181_417;
    let column_one_right = 158_740;
    assert!(
        baseline_text.lines.len() > 4,
        "multiline source stays in one fixed Text Box"
    );
    assert!(
        baseline_text
            .lines
            .windows(2)
            .all(|lines| lines[0].source.end <= lines[1].source.end)
    );
    assert!(
        baseline_text.lines[0].x.value() > 0,
        "the left-edge picture leaves the wider right-hand segment"
    );
    assert!(
        baseline_text
            .lines
            .iter()
            .any(|line| line.x.value() >= column_two_start)
    );
    assert!(baseline_text.lines.iter().any(|line| {
        line.x
            .value()
            .checked_add(line.width.value())
            .is_some_and(|right| right == column_one_right)
    }));
    assert!(
        baseline_text
            .lines
            .iter()
            .any(|line| { line.column_index == 1 && line.x.value() == column_two_start })
    );
    assert_eq!(baseline_text.lines.first().unwrap().column_index, 0);
    assert_eq!(
        column_two_start - column_one_right,
        22_677,
        "the two columns retain the configured 8,000 µm physical gap"
    );
    let overset = baseline_text
        .overset_source
        .as_ref()
        .expect("bounded instructions must expose their exact unplaced tail");
    assert_eq!(overset.end, story.len());
    assert_eq!(
        overset.start,
        baseline_text.lines.last().map_or(0, |line| line.source.end)
    );
    assert!(story.is_char_boundary(overset.start));

    session
        .execute(
            composed.revision,
            Transaction::single(
                "Park alternative illustration",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Pasteboard wrap decoy".to_owned(),
                        size: PhysicalSize::new(120_000, 55_000).unwrap(),
                        transform: Transform::identity_at(10_000, 20_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Shape {
                            shape: ShapeKind::Rectangle,
                            fill: Color {
                                red: 10,
                                green: 20,
                                blue: 30,
                                alpha: 255,
                            },
                            stroke: ShapeStroke::default(),
                            wrap_mode: WrapMode::Rectangular,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let with_pasteboard =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let with_pasteboard_text = with_pasteboard
        .text_boxes()
        .into_iter()
        .find(|text_box| text_box.object_id == text_box_id)
        .unwrap();
    assert_eq!(
        with_pasteboard_text.lines, baseline_text.lines,
        "pasteboard objects must never become page text exclusions"
    );
    assert_eq!(
        with_pasteboard_text.overset_source,
        baseline_text.overset_source
    );

    let moved = session
        .execute(
            session.revision(),
            Transaction::single(
                "Move wrapped picture",
                Command::MoveObject {
                    object_id: picture_id,
                    destination: ObjectLocation::Page(page_id),
                    transform: Transform::identity_at(45_000, 20_000),
                },
            ),
        )
        .unwrap();
    let after_move =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let moved_text = after_move
        .text_boxes()
        .into_iter()
        .find(|text_box| text_box.object_id == text_box_id)
        .unwrap()
        .clone();
    assert_ne!(moved_text.lines, baseline_text.lines);
    assert_ne!(
        moved_text.overset_source, baseline_text.overset_source,
        "moving a page exclusion must update the exact unplaced source boundary"
    );
    assert_eq!(
        moved_text.lines[0].x.value(),
        0,
        "after the picture moves right, rectangular wrap chooses the wider left-hand segment"
    );

    session
        .execute(
            moved.revision,
            Transaction::single(
                "Resize wrapped shape",
                Command::ResizeObject {
                    object_id: shape_id,
                    size: PhysicalSize::new(40_000, 50_000).unwrap(),
                },
            ),
        )
        .unwrap();
    let after_resize =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let resized_text = after_resize
        .text_boxes()
        .into_iter()
        .find(|text_box| text_box.object_id == text_box_id)
        .unwrap();
    assert_ne!(resized_text.lines, moved_text.lines);
    assert_ne!(
        resized_text.overset_source, moved_text.overset_source,
        "resizing a page exclusion must update the exact unplaced source boundary"
    );
    assert!(
        resized_text
            .lines
            .iter()
            .all(|line| { line.glyph_runs.iter().all(|run| !run.text.is_empty()) })
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one durable rich-story fixture proves inline face selection, paragraph geometry, wrap, columns, overset, and deterministic resolution"
)]
fn durable_rich_story_drives_inline_and_paragraph_geometry_through_page_wrap() {
    let document = Document::new(
        DocumentId::parse("rich-instructional-layout").unwrap(),
        "Rich instructional layout",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let story = format!(
        "Ordinary introduction.\nBold café instructions {}",
        "continue slowly, show both hands, and pause for the surprise. ".repeat(28)
    );
    let emphasis_start = story.find("Bold café").unwrap();
    let emphasis_end = emphasis_start + "Bold café".len();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Compose rich instructional page".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Rich instructions".to_owned(),
                            size: PhysicalSize::new(120_000, 55_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: story.clone(),
                                style: TextStyle {
                                    font_family: "DejaVu Sans".to_owned(),
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
                                            font_size_millipoints: None,
                                            bold: Some(true),
                                            italic: Some(true),
                                            ..InlineTextStyle::default()
                                        },
                                    }],
                                    paragraph_runs: vec![ParagraphStyleRun {
                                        range: ParagraphRange { start: 1, end: 2 },
                                        style: ParagraphStyle {
                                            list_style: None,
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
    let text_box_id = added.created_objects[0].clone();
    let first = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let second = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let layout = first
        .text_boxes()
        .into_iter()
        .find(|layout| layout.object_id == text_box_id)
        .unwrap();
    let second_paragraph_start = story.find("Bold café").unwrap();
    let emphasized_font_indices = layout
        .lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .filter(|run| run.source.start >= emphasis_start && run.source.end <= emphasis_end)
        .map(|run| run.font_index)
        .collect::<Vec<_>>();
    let ordinary_font_index = layout
        .lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .find(|run| run.source.start < second_paragraph_start)
        .unwrap()
        .font_index;

    assert_eq!(first, second, "same rich snapshot must resolve identically");
    assert!(!emphasized_font_indices.is_empty());
    assert!(
        emphasized_font_indices
            .iter()
            .any(|index| *index != ordinary_font_index),
        "bold italic selection must resolve a styled native face"
    );
    assert!(layout.lines.iter().any(|line| {
        line.source.start >= second_paragraph_start
            && line.x.value() >= 25_511
            && line.height.value() > 16_000
    }));
    let overset = layout
        .overset_source
        .as_ref()
        .expect("bounded rich instructions must retain exact unplaced source");
    assert_eq!(overset.end, story.len());
    assert_eq!(
        overset.start,
        layout.lines.last().map_or(0, |line| line.source.end)
    );
    assert!(story.is_char_boundary(overset.start));
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one pasteboard-text journey proves add, move, and resize invariance against the same baseline layout"
)]
fn pasteboard_fixed_text_never_wraps_around_pasteboard_objects() {
    let document = Document::new(
        DocumentId::parse("pasteboard-text-wrap-isolation").unwrap(),
        "Pasteboard text wrap isolation",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let story = "Pasteboard notes remain a stable working area while artwork is parked, moved, and resized nearby. "
        .repeat(24);
    let mut session = DocumentSession::new(document).unwrap();
    let text_added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add pasteboard notes",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Pasteboard notes".to_owned(),
                        size: PhysicalSize::new(100_000, 35_000).unwrap(),
                        transform: Transform::identity_at(20_000, 20_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: story.clone(),
                            style: TextStyle::default(),
                            frame: TextFrame {
                                sizing_mode: TextBoxSizingMode::Fixed,
                                ..TextFrame::default()
                            },
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .unwrap();
    let text_box_id = text_added.created_objects[0].clone();
    let baseline = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let baseline_text = baseline
        .text_boxes()
        .into_iter()
        .find(|text_box| text_box.object_id == text_box_id)
        .unwrap()
        .clone();
    assert!(baseline_text.lines.len() > 1);
    assert!(baseline_text.overset_source.is_some());

    let obstacle_added = session
        .execute(
            text_added.revision,
            Transaction::single(
                "Park wrapped artwork over pasteboard notes",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Pasteboard wrapped artwork".to_owned(),
                        size: PhysicalSize::new(35_000, 35_000).unwrap(),
                        transform: Transform::identity_at(20_000, 20_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Shape {
                            shape: ShapeKind::Rectangle,
                            fill: Color {
                                red: 40,
                                green: 80,
                                blue: 120,
                                alpha: 255,
                            },
                            stroke: ShapeStroke::default(),
                            wrap_mode: WrapMode::Rectangular,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let obstacle_id = obstacle_added.created_objects[0].clone();
    let after_add = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let after_add_text = after_add
        .text_boxes()
        .into_iter()
        .find(|text_box| text_box.object_id == text_box_id)
        .unwrap()
        .clone();
    assert_eq!(after_add_text.lines, baseline_text.lines);
    assert_eq!(after_add_text.overset_source, baseline_text.overset_source);

    let obstacle_moved = session
        .execute(
            obstacle_added.revision,
            Transaction::single(
                "Move pasteboard wrapped artwork",
                Command::MoveObject {
                    object_id: obstacle_id.clone(),
                    destination: ObjectLocation::Pasteboard,
                    transform: Transform::identity_at(55_000, 20_000),
                },
            ),
        )
        .unwrap();
    let after_move =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let after_move_text = after_move
        .text_boxes()
        .into_iter()
        .find(|text_box| text_box.object_id == text_box_id)
        .unwrap()
        .clone();
    assert_eq!(after_move_text.lines, baseline_text.lines);
    assert_eq!(after_move_text.overset_source, baseline_text.overset_source);

    session
        .execute(
            obstacle_moved.revision,
            Transaction::single(
                "Resize pasteboard wrapped artwork",
                Command::ResizeObject {
                    object_id: obstacle_id,
                    size: PhysicalSize::new(65_000, 35_000).unwrap(),
                },
            ),
        )
        .unwrap();
    let after_resize =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let after_resize_text = after_resize
        .text_boxes()
        .into_iter()
        .find(|text_box| text_box.object_id == text_box_id)
        .unwrap();
    assert_eq!(after_resize_text.lines, baseline_text.lines);
    assert_eq!(
        after_resize_text.overset_source,
        baseline_text.overset_source
    );
}

#[test]
fn rotated_text_box_wraps_against_exclusion_in_its_local_coordinates() {
    let document = Document::new(
        DocumentId::parse("rotated-preview-document").unwrap(),
        "Rotated preview",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let text_transform = Transform {
        rotation_microdegrees: 90_000_000,
        ..Transform::identity_at(10_000, 10_000)
    };
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Compose rotated wrapped text".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Rotated body copy".to_owned(),
                            size: PhysicalSize::new(100_000, 80_000).unwrap(),
                            transform: text_transform,
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "Text should wrap around the picture before continuing across the text box. ".repeat(8),
                                style: TextStyle::default(),
                                frame: TextFrame::default(),
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Page-space picture".to_owned(),
                            size: PhysicalSize::new(24_000, 30_000).unwrap(),
                            transform: Transform::identity_at(-14_000, 10_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Shape {
                                shape: ShapeKind::Rectangle,
                                fill: Color {
                                    red: 20,
                                    green: 80,
                                    blue: 120,
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
    let project = ProjectSnapshot::from_session(&session);
    let text_box_id = project.document.pages[0].objects[0].id.clone();

    let preview = ResolvedPublication::resolve(&project).expect("resolve rotated native preview");
    let text_box = preview
        .text_boxes()
        .into_iter()
        .find(|candidate| candidate.object_id == text_box_id)
        .expect("rotated text box preview");

    assert!(!text_box.lines.is_empty());
    assert!(
        text_box.lines[0].x.value() > 0,
        "page-space exclusion must be inverse-mapped before rotated text layout"
    );
}

#[test]
fn publication_preview_marks_text_beyond_the_total_budget_without_shaping_it() {
    let document = Document::new(
        DocumentId::parse("bounded-preview-document").unwrap(),
        "Bounded preview",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add text beyond the total preview budget".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Missing font story one".to_owned(),
                            size: PhysicalSize::new(100_000, 80_000).unwrap(),
                            transform: Transform::identity_at(10_000, 10_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "a".repeat(32 * 1024),
                                style: TextStyle {
                                    font_face: Some(FontFaceBinding {
                                        source: AssetHash::for_bytes(b"unavailable font"),
                                        face_index: 0,
                                    }),
                                    ..TextStyle::default()
                                },
                                frame: TextFrame::default(),
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Missing font story two".to_owned(),
                            size: PhysicalSize::new(100_000, 80_000).unwrap(),
                            transform: Transform::identity_at(10_000, 100_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "b".repeat(32 * 1024),
                                style: TextStyle {
                                    font_face: Some(FontFaceBinding {
                                        source: AssetHash::for_bytes(b"unavailable font"),
                                        face_index: 0,
                                    }),
                                    ..TextStyle::default()
                                },
                                frame: TextFrame::default(),
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Over total budget".to_owned(),
                            size: PhysicalSize::new(100_000, 80_000).unwrap(),
                            transform: Transform::identity_at(110_000, 10_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "c".to_owned(),
                                style: TextStyle::default(),
                                frame: TextFrame::default(),
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                ],
            },
        )
        .unwrap();

    let preview = ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();

    assert_eq!(preview.budget_state(), PreviewBudgetState::Truncated);
    assert_eq!(preview.text_boxes().len(), 3);
    assert_eq!(
        preview.text_boxes()[2].preview_state,
        TextBoxPreviewState::BudgetExceeded
    );
    assert!(preview.text_boxes()[2].lines.is_empty());
    assert!(!preview.text_boxes()[2].overset);
    assert_eq!(preview.text_boxes()[2].overset_source, None);
}

#[test]
fn bound_embedded_font_bytes_select_the_exact_resolved_face() {
    let bytes = usable_font_fixture();
    let source = AssetHash::for_bytes(&bytes);
    let document = Document::new(
        DocumentId::parse("bound-font-preview-document").unwrap(),
        "Bound font preview",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Compose exact embedded text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Exact font text".to_owned(),
                        size: PhysicalSize::new(100_000, 30_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Exact embedded font".to_owned(),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: source.clone(),
                                    face_index: 0,
                                }),
                                font_family: "An installed lookalike must not be used".to_owned(),
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
    let project = ProjectSnapshot::from_session(&session);

    let missing = ResolvedPublication::resolve(&project).unwrap();
    assert_eq!(
        missing.text_boxes()[0].font_binding.substitution_status,
        FontSubstitutionStatus::Missing,
        "a bound story must not silently use a same-family system fallback"
    );
    assert!(missing.text_boxes()[0].lines.is_empty());

    let preview = ResolvedPublication::resolve_with_font_resources(
        &project,
        &[ExactFontResource {
            source: source.clone(),
            face_index: 0,
            bytes: bytes.clone(),
        }],
    )
    .unwrap();
    let text_boxes = preview.text_boxes();
    let text_box = text_boxes[0];
    let scene_layout = match &preview.pages()[0].objects[0].kind {
        ResolvedSceneObjectKind::Text { layout, .. } => layout,
        ResolvedSceneObjectKind::Image { .. }
        | ResolvedSceneObjectKind::Shape { .. }
        | ResolvedSceneObjectKind::Table { .. } => panic!("expected resolved text scene"),
    };
    assert!(
        core::ptr::eq(scene_layout, text_box),
        "the compatibility projection must reference the scene's single layout authority"
    );
    assert_eq!(
        text_box.font_binding.substitution_status,
        FontSubstitutionStatus::Exact
    );
    assert!(
        text_box
            .resolved_fonts
            .iter()
            .any(|font| font.sha256_hex == source.as_str() && font.collection_index == 0),
        "the resolved glyph runs must retain the embedded source identity"
    );
    assert!(!text_box.lines.is_empty());
}

#[test]
fn bound_table_font_bytes_select_the_exact_face_for_every_populated_cell() {
    let bytes = usable_font_fixture();
    let source = AssetHash::for_bytes(&bytes);
    let document = Document::new(
        DocumentId::parse("bound-table-font-preview").unwrap(),
        "Bound table font preview",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut table = TableData::empty(1, 2).unwrap();
    table.cells = vec!["M".to_owned(), "A".to_owned()];
    table.style.font_face = Some(FontFaceBinding {
        source: source.clone(),
        face_index: 0,
    });
    table.style.font_family = "Installed lookalikes are not identity".into();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Compose exact table text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Word search".to_owned(),
                        size: PhysicalSize::new(100_000, 30_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);
    assert!(preview_font_face_bindings(&project).contains(&(source.clone(), 0)));

    let missing = ResolvedPublication::resolve(&project).unwrap();
    let ResolvedSceneObjectKind::Table { table } = &missing.pages()[0].objects[0].kind else {
        panic!("expected table");
    };
    assert_eq!(
        table.font_binding.substitution_status,
        FontSubstitutionStatus::Missing
    );
    assert!(table.populated_cells.iter().all(|cell| {
        cell.layout.font_binding.substitution_status == FontSubstitutionStatus::Missing
    }));

    let resolved = ResolvedPublication::resolve_with_font_resources(
        &project,
        &[ExactFontResource {
            source: source.clone(),
            face_index: 0,
            bytes: bytes.clone(),
        }],
    )
    .unwrap();
    let ResolvedSceneObjectKind::Table { table } = &resolved.pages()[0].objects[0].kind else {
        panic!("expected table");
    };
    assert_eq!(
        table.font_binding.substitution_status,
        FontSubstitutionStatus::Exact
    );
    assert!(table.populated_cells.iter().all(|cell| {
        cell.layout.font_binding.substitution_status == FontSubstitutionStatus::Exact
            && cell
                .layout
                .resolved_fonts
                .iter()
                .any(|font| font.sha256_hex == source.as_str())
    }));

    let mut empty_project = project.clone();
    let ObjectKind::Table { table } = &mut empty_project.document.pages[0].objects[0].kind else {
        panic!("expected table");
    };
    table.cells.fill(String::new());
    assert!(preview_font_face_bindings(&empty_project).contains(&(source.clone(), 0)));
    let resolved_empty = ResolvedPublication::resolve_with_font_resources(
        &empty_project,
        &[ExactFontResource {
            source,
            face_index: 0,
            bytes,
        }],
    )
    .unwrap();
    let ResolvedSceneObjectKind::Table { table } = &resolved_empty.pages()[0].objects[0].kind
    else {
        panic!("expected table");
    };
    assert!(table.populated_cells.is_empty());
    assert_eq!(
        table.font_binding.substitution_status,
        FontSubstitutionStatus::Exact
    );
}

#[test]
fn empty_table_font_reachability_matches_the_production_preview_budget_boundary() {
    let reachable_bytes = usable_font_fixture();
    let reachable_source = AssetHash::for_bytes(&reachable_bytes);
    let over_budget_source = AssetHash::for_bytes(b"distinct over-budget font face");
    let document = Document::new(
        DocumentId::parse("empty-table-font-budget-boundary").unwrap(),
        "Empty table font budget boundary",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(0).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut commands = Vec::with_capacity(257);
    for index in 0..257 {
        let mut table = TableData::empty(1, 1).unwrap();
        if index == 255 {
            table.style.font_face = Some(FontFaceBinding {
                source: reachable_source.clone(),
                face_index: 0,
            });
        } else if index == 256 {
            table.style.font_face = Some(FontFaceBinding {
                source: over_budget_source.clone(),
                face_index: 0,
            });
        }
        commands.push(Command::AddObject {
            location: ObjectLocation::Page(page_id.clone()),
            object: ObjectDraft {
                name: format!("Empty table {}", index + 1),
                size: PhysicalSize::new(10_000, 10_000).unwrap(),
                transform: Transform::identity_at(0, 0),
                opacity_millionths: 1_000_000,
                kind: ObjectKind::Table { table },
            },
        });
    }
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add empty table budget boundary".to_owned(),
                commands,
            },
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);

    let reachable_bindings = preview_font_face_bindings(&project);
    assert!(reachable_bindings.contains(&(reachable_source.clone(), 0)));
    assert!(!reachable_bindings.contains(&(over_budget_source, 0)));

    let resolved = ResolvedPublication::resolve_with_font_resources(
        &project,
        &[ExactFontResource {
            source: reachable_source,
            face_index: 0,
            bytes: reachable_bytes,
        }],
    )
    .unwrap();
    assert_eq!(resolved.budget_state(), PreviewBudgetState::Truncated);
    let tables = &resolved.pages()[0].objects;
    let ResolvedSceneObjectKind::Table { table: at_limit } = &tables[255].kind else {
        panic!("expected the 256th object to be a table");
    };
    let ResolvedSceneObjectKind::Table { table: over_limit } = &tables[256].kind else {
        panic!("expected the 257th object to be a table");
    };
    assert!(at_limit.populated_cells.is_empty());
    assert!(over_limit.populated_cells.is_empty());
    assert_eq!(
        at_limit.font_binding.substitution_status,
        FontSubstitutionStatus::Exact
    );
    assert_eq!(
        over_limit.font_binding.substitution_status,
        FontSubstitutionStatus::Missing
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The end-to-end scene contract deliberately keeps its ordered document setup and assertions together so changes to page, object, and pasteboard ordering remain visible in one test."
)]
fn resolved_page_scenes_preserve_blank_pages_boxes_stack_order_and_pasteboard_separation() {
    let document = Document::new(
        DocumentId::parse("page-scene-document").unwrap(),
        "Page scenes",
        PhysicalSize::new(100_000, 150_000).unwrap(),
        Insets::uniform(2_000).unwrap(),
    )
    .unwrap();
    let first_page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added_page = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add second page",
                Command::AddPage {
                    after: Some(first_page_id.clone()),
                    trim: PhysicalSize::new(200_000, 250_000).unwrap(),
                    bleed: Insets::uniform(3_000).unwrap(),
                },
            ),
        )
        .unwrap();
    let second_page_id = added_page.created_pages[0].clone();
    let placed = session
        .execute(
            added_page.revision,
            Transaction {
                label: "Place scene objects".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(second_page_id.clone()),
                        object: ObjectDraft {
                            name: "Shape first".to_owned(),
                            size: PhysicalSize::new(20_000, 10_000).unwrap(),
                            transform: Transform::identity_at(1_000, 2_000),
                            opacity_millionths: 750_000,
                            kind: ObjectKind::Shape {
                                shape: ShapeKind::Ellipse,
                                fill: Color {
                                    red: 10,
                                    green: 20,
                                    blue: 30,
                                    alpha: 255,
                                },
                                stroke: ShapeStroke::default(),
                                wrap_mode: WrapMode::None,
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(second_page_id.clone()),
                        object: ObjectDraft {
                            name: "Image second".to_owned(),
                            size: PhysicalSize::new(30_000, 15_000).unwrap(),
                            transform: Transform::identity_at(3_000, 4_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Image {
                                asset: AssetHash::for_bytes(b"scene image"),
                                wrap_mode: WrapMode::None,
                                content_kind: ImageContentKind::ContinuousTone,
                                crop: document_core::model::ImageCrop::new(
                                    100_000, 50_000, 200_000, 150_000,
                                )
                                .unwrap(),
                                flip: document_core::model::ImageFlip::default(),
                                lock_aspect_ratio: true,
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(second_page_id),
                        object: ObjectDraft {
                            name: "Table third".to_owned(),
                            size: PhysicalSize::new(40_000, 20_000).unwrap(),
                            transform: Transform::identity_at(5_000, 6_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Table {
                                table: TableData::empty(2, 3).unwrap(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Pasteboard,
                        object: ObjectDraft {
                            name: "Parked shape".to_owned(),
                            size: PhysicalSize::new(10_000, 10_000).unwrap(),
                            transform: Transform::identity_at(-20_000, 0),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Shape {
                                shape: ShapeKind::Rectangle,
                                fill: Color {
                                    red: 40,
                                    green: 50,
                                    blue: 60,
                                    alpha: 255,
                                },
                                stroke: ShapeStroke::default(),
                                wrap_mode: WrapMode::None,
                            },
                        },
                    },
                ],
            },
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);

    let first = ResolvedPublication::resolve(&project).unwrap();
    let second = ResolvedPublication::resolve(&project).unwrap();

    assert_eq!(
        first, second,
        "identical input must produce an identical scene"
    );
    assert_eq!(first.pages().len(), 2);
    assert_eq!(first.pages()[0].page_id, first_page_id);
    assert!(
        first.pages()[0].objects.is_empty(),
        "blank page must be retained"
    );
    let populated = &first.pages()[1];
    assert_eq!(populated.trim_box.x.value(), 0);
    assert_eq!(populated.trim_box.width.value(), 200_000);
    assert_eq!(populated.bleed_box.x.value(), -3_000);
    assert_eq!(populated.bleed_box.y.value(), -3_000);
    assert_eq!(populated.bleed_box.width.value(), 206_000);
    assert_eq!(populated.bleed_box.height.value(), 256_000);
    assert_eq!(
        populated
            .objects
            .iter()
            .map(|object| object.object_id.clone())
            .collect::<Vec<_>>(),
        placed.created_objects[..3]
    );
    assert!(matches!(
        populated.objects[0].kind,
        ResolvedSceneObjectKind::Shape {
            shape: ShapeKind::Ellipse,
            ..
        }
    ));
    assert!(matches!(
        populated.objects[1].kind,
        ResolvedSceneObjectKind::Image { crop, .. }
            if crop == document_core::model::ImageCrop::new(100_000, 50_000, 200_000, 150_000).unwrap()
    ));
    assert!(matches!(
        populated.objects[2].kind,
        ResolvedSceneObjectKind::Table { .. }
    ));
    assert_eq!(first.pasteboard().objects.len(), 1);
    assert_eq!(
        first.pasteboard().objects[0].object_id,
        placed.created_objects[3]
    );
    assert!(first.pages().iter().all(|page| {
        page.objects
            .iter()
            .all(|object| object.object_id != placed.created_objects[3])
    }));
}

#[test]
fn populated_table_cells_use_deterministic_native_layout_in_row_major_order() {
    let document = Document::new(
        DocumentId::parse("native-table-layout").unwrap(),
        "Native table layout",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut table = TableData::empty(3, 3).unwrap();
    table.cells[0] = "M".to_owned();
    table.cells[4] = "A".to_owned();
    table.cells[8] = "G".repeat(2_000);
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add word-search table",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Word search".to_owned(),
                        size: PhysicalSize::new(100_000, 50_000).unwrap(),
                        transform: Transform::identity_at(10_000, 20_000),
                        opacity_millionths: 800_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();

    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let ResolvedSceneObjectKind::Table { table } = &publication.pages()[0].objects[0].kind else {
        panic!("expected resolved table scene");
    };

    assert_eq!(table.column_boundaries_um, vec![0, 33_333, 66_666, 100_000]);
    assert_eq!(table.row_boundaries_um, vec![0, 16_666, 33_333, 50_000]);
    assert_eq!(
        table
            .populated_cells
            .iter()
            .map(|cell| (cell.row, cell.column))
            .collect::<Vec<_>>(),
        vec![(0, 0), (1, 1), (2, 2)]
    );
    assert_eq!(table.populated_cells[0].text, "M");
    assert_eq!(table.populated_cells[1].text, "A");
    assert_eq!(table.populated_cells[2].text.len(), 2_000);
    assert!(!table.populated_cells[0].layout.lines.is_empty());
    assert!(!table.populated_cells[1].layout.lines.is_empty());
    assert!(
        table.populated_cells[0].layout.lines[0].y.value() > 5_000,
        "ordinary word-search letters should be centered vertically inside their cells"
    );
    assert!(table.populated_cells[2].clipped);
}

#[test]
fn table_cell_layout_honors_whole_table_horizontal_alignment() {
    let document = Document::new(
        DocumentId::parse("table-cell-alignment").unwrap(),
        "Table cell alignment",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut left_table = TableData::empty(1, 1).unwrap();
    left_table.cells[0] = "MAGIC".to_owned();
    left_table.style.alignment = TextAlignment::Left;
    let mut right_table = left_table.clone();
    right_table.style.alignment = TextAlignment::Right;
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Compare table cell alignment".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Left aligned table".to_owned(),
                            size: PhysicalSize::new(100_000, 30_000).unwrap(),
                            transform: Transform::identity_at(10_000, 10_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Table { table: left_table },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Right aligned table".to_owned(),
                            size: PhysicalSize::new(100_000, 30_000).unwrap(),
                            transform: Transform::identity_at(10_000, 50_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Table { table: right_table },
                        },
                    },
                ],
            },
        )
        .unwrap();

    let publication =
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
    let first_table_glyph_x = |object_index: usize| {
        let ResolvedSceneObjectKind::Table { table } =
            &publication.pages()[0].objects[object_index].kind
        else {
            panic!("expected resolved table scene");
        };
        table.populated_cells[0].layout.lines[0].glyph_runs[0].glyphs[0]
            .x
            .value()
    };

    assert!(
        first_table_glyph_x(1) > first_table_glyph_x(0),
        "right-aligned table text must paint farther right than identical left-aligned text"
    );
}

#[test]
fn populated_table_cells_share_the_global_native_preview_budget() {
    let document = Document::new(
        DocumentId::parse("bounded-table-layout").unwrap(),
        "Bounded table layout",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut table = TableData::empty(1, 1).unwrap();
    table.cells[0] = "Valid cell".to_owned();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add table cell for lower-level budget test",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Oversized cell".to_owned(),
                        size: PhysicalSize::new(100_000, 50_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();

    // Session admission now rejects this oversized string before layout. Feed
    // it directly to the lower-level preview boundary to retain its own guard.
    let mut snapshot = ProjectSnapshot::from_session(&session);
    let ObjectKind::Table { table } = &mut snapshot.document.pages[0].objects[0].kind else {
        panic!("expected table fixture");
    };
    table.cells[0] = "A".repeat(32 * 1024 + 1);
    let publication = ResolvedPublication::resolve(&snapshot).unwrap();
    let ResolvedSceneObjectKind::Table { table } = &publication.pages()[0].objects[0].kind else {
        panic!("expected resolved table scene");
    };

    assert_eq!(publication.budget_state(), PreviewBudgetState::Truncated);
    assert_eq!(
        table.populated_cells[0].layout.preview_state,
        TextBoxPreviewState::BudgetExceeded
    );
    assert!(!table.populated_cells[0].clipped);
    assert!(!table.populated_cells[0].layout.overset);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one list fixture keeps sparse-style splitting, exact marker fonts, marker-only paragraphs, and canonical source assertions together"
)]
fn generated_list_markers_keep_story_ranges_canonical_and_number_across_style_splits() {
    let bytes = usable_font_fixture();
    let source = AssetHash::for_bytes(&bytes);
    let document = Document::new(
        DocumentId::parse("resolved-list-markers").unwrap(),
        "Resolved list markers",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let text = "First\nSecond\nThird\n";
    let story = TextStoryFormatting {
        inline_spans: Vec::new(),
        paragraph_runs: vec![
            ParagraphStyleRun {
                range: ParagraphRange { start: 0, end: 1 },
                style: ParagraphStyle {
                    list_style: Some(ParagraphListStyle::Numbered { start_at: 3 }),
                    ..ParagraphStyle::default()
                },
            },
            ParagraphStyleRun {
                range: ParagraphRange { start: 1, end: 2 },
                style: ParagraphStyle {
                    list_style: Some(ParagraphListStyle::Numbered { start_at: 3 }),
                    alignment: Some(TextAlignment::Right),
                    ..ParagraphStyle::default()
                },
            },
            ParagraphStyleRun {
                range: ParagraphRange { start: 2, end: 3 },
                style: ParagraphStyle {
                    list_style: Some(ParagraphListStyle::Numbered { start_at: 3 }),
                    ..ParagraphStyle::default()
                },
            },
            ParagraphStyleRun {
                range: ParagraphRange { start: 3, end: 4 },
                style: ParagraphStyle {
                    list_style: Some(ParagraphListStyle::Bullet),
                    ..ParagraphStyle::default()
                },
            },
        ],
    };
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Compose lists",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Instructions".to_owned(),
                        size: PhysicalSize::new(120_000, 100_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: text.to_owned(),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: source.clone(),
                                    face_index: 0,
                                }),
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &[ExactFontResource {
            source: source.clone(),
            face_index: 0,
            bytes,
        }],
    )
    .unwrap();
    let ResolvedSceneObjectKind::Text { layout, .. } = &publication.pages()[0].objects[0].kind
    else {
        panic!("expected text");
    };
    let mut markers: Vec<(u32, String)> = Vec::new();
    for run in layout.lines.iter().flat_map(|line| &line.glyph_runs) {
        let Some(paragraph) = run.list_marker_paragraph else {
            continue;
        };
        if let Some((previous, text)) = markers.last_mut()
            && *previous == paragraph
        {
            text.push_str(&run.text);
        } else {
            markers.push((paragraph, run.text.clone()));
        }
    }
    assert_eq!(
        markers,
        vec![
            (0, "3.".to_owned()),
            (1, "4.".to_owned()),
            (2, "5.".to_owned()),
            (3, "•".to_owned()),
        ]
    );
    assert!(layout.lines.iter().all(|line| {
        line.source.end <= text.len()
            && text.is_char_boundary(line.source.start)
            && text.is_char_boundary(line.source.end)
    }));
    assert!(
        layout
            .lines
            .iter()
            .flat_map(|line| &line.glyph_runs)
            .all(|run| {
                run.list_marker_paragraph.is_none()
                    || layout
                        .resolved_fonts
                        .get(run.font_index)
                        .is_some_and(|face| face.sha256_hex == source.as_str())
            })
    );
    let empty_marker_line = layout
        .lines
        .iter()
        .find(|line| {
            line.source.start == text.len()
                && line.source.end == text.len()
                && line
                    .glyph_runs
                    .iter()
                    .any(|run| run.list_marker_paragraph == Some(3))
        })
        .expect("the trailing empty paragraph must still paint its bullet");
    assert_eq!(empty_marker_line.text, "");
    assert!(!layout.overset);
}

#[test]
fn weighted_table_geometry_border_and_cell_alignment_are_resolved_before_rendering() {
    let document = Document::new(
        DocumentId::parse("weighted-table-layout").unwrap(),
        "Weighted table layout",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut table = TableData::empty(1, 3).unwrap();
    table.cells = vec!["A".to_owned(), "B".to_owned(), "C".to_owned()];
    table.column_weights = vec![1, 2, 3];
    table.grid_border = TableGridBorder {
        color: Color {
            red: 12,
            green: 34,
            blue: 56,
            alpha: 200,
        },
        width_millipoints: 2_500,
    };
    table.cell_formatting = vec![
        TableCellFormatting {
            row: 0,
            column: 0,
            horizontal_alignment: Some(TextAlignment::Left),
            vertical_alignment: Some(TableVerticalAlignment::Top),
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 0,
            column: 1,
            horizontal_alignment: Some(TextAlignment::Center),
            vertical_alignment: Some(TableVerticalAlignment::Middle),
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 0,
            column: 2,
            horizontal_alignment: Some(TextAlignment::Right),
            vertical_alignment: Some(TableVerticalAlignment::Bottom),
            ..TableCellFormatting::default()
        },
    ];
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add formatted grid",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Word search".to_owned(),
                        size: PhysicalSize::new(120_000, 60_000).unwrap(),
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
    let ResolvedSceneObjectKind::Table { table } = &publication.pages()[0].objects[0].kind else {
        panic!("expected table");
    };
    assert_eq!(table.column_boundaries_um, vec![0, 20_000, 60_000, 120_000]);
    assert_eq!(table.row_boundaries_um, vec![0, 60_000]);
    assert_eq!(table.grid_color.red, 12);
    assert_eq!(table.grid_width_millipoints, 2_500);
    let glyph_positions = table
        .populated_cells
        .iter()
        .map(|cell| {
            let glyph = &cell.layout.lines[0].glyph_runs[0].glyphs[0];
            (glyph.x.value(), glyph.y.value())
        })
        .collect::<Vec<_>>();
    assert!(glyph_positions[0].1 < glyph_positions[1].1);
    assert!(glyph_positions[1].1 < glyph_positions[2].1);
    assert!(glyph_positions[0].0 < 20_000 * 72_000 / 25_400);
    assert!(glyph_positions[1].0 > 20_000 * 72_000 / 25_400);
    assert!(glyph_positions[2].0 > 60_000 * 72_000 / 25_400);
}

#[test]
fn empty_tables_with_degenerate_weighted_tracks_fail_before_scene_creation() {
    let document = Document::new(
        DocumentId::parse("degenerate-empty-table").unwrap(),
        "Degenerate empty table",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let table = TableData::empty(1, 2).unwrap();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add impossible grid",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Impossible".to_owned(),
                        size: PhysicalSize::new(1, 10_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();
    assert!(matches!(
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)),
        Err(layout_core::PublicationLayoutError::TableCellGeometryTooSmall)
    ));
}

fn usable_font_fixture() -> Vec<u8> {
    include_bytes!("fixtures/DejaVuSans.ttf").to_vec()
}
