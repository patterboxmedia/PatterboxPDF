use document_core::model::{
    Color, DocumentId, FontFaceBinding, Insets, ObjectEffects, ObjectKind, PhysicalSize,
    PublicationObject, ShapeKind, ShapeStroke, TextFrame, TextStoryFormatting, TextStyle,
    Transform, WrapMode,
};
use document_core::{
    AssetHash, Command, Document, DocumentSession, ObjectId, ProjectSnapshot, Revision, Transaction,
};
use layout_core::{
    ExactFontResource, PreviewBudgetState, PublicationLayoutError, ResolvedPagePreview,
    ResolvedPublication, ResolvedSceneObjectKind, TextBoxPreviewState, output_font_face_bindings,
};

fn font() -> ExactFontResource {
    let bytes = include_bytes!("fixtures/DejaVuSans.ttf").to_vec();
    ExactFontResource {
        source: AssetHash::for_bytes(&bytes),
        face_index: 0,
        bytes,
    }
}

fn project(pages: u16) -> ProjectSnapshot {
    ProjectSnapshot {
        document: Document::new_with_page_count(
            DocumentId::parse("page-preview-fixture").unwrap(),
            "Page preview",
            PhysicalSize::new(215_900, 279_400).unwrap(),
            Insets::uniform(3_175).unwrap(),
            pages,
        )
        .unwrap(),
        revision: Revision::initial(),
    }
}

fn object(id: &str, kind: ObjectKind) -> PublicationObject {
    PublicationObject {
        id: ObjectId::parse(id).unwrap(),
        group_id: None,
        locked: false,
        name: id.to_owned(),
        size: PhysicalSize::new(90_000, 50_000).unwrap(),
        transform: Transform::identity_at(10_000, 10_000),
        opacity_millionths: 1_000_000,
        effects: ObjectEffects::default(),
        wrap_settings: document_core::model::WrapSettings::default(),
        corner_rounding: document_core::model::CornerRounding::default(),
        text_field: None,
        kind,
    }
}

fn text(id: &str, contents: &str, source: &AssetHash) -> PublicationObject {
    object(
        id,
        ObjectKind::Text {
            text: contents.to_owned(),
            style: TextStyle {
                font_face: Some(FontFaceBinding {
                    source: source.clone(),
                    face_index: 0,
                }),
                ..TextStyle::default()
            },
            frame: TextFrame::default(),
            story: TextStoryFormatting::default(),
        },
    )
}

fn output(project: &ProjectSnapshot) -> Result<ResolvedPublication, PublicationLayoutError> {
    ResolvedPublication::resolve_output_with_font_resources(project, &[font()])
}

#[test]
fn all_pages_exceed_aggregate_limits_without_omission_and_keep_blanks() {
    let mut project = project(24);
    let source = font().source;
    for (page_index, page) in project.document.pages.iter_mut().enumerate() {
        if page_index == 11 {
            continue;
        }
        for item in 0..19 {
            page.objects.push(text(
                &format!("body-{page_index}-{item}"),
                &"Page text. ".repeat(20),
                &source,
            ));
        }
    }
    let old = ResolvedPublication::resolve_with_font_resources(&project, &[font()]).unwrap();
    assert_eq!(old.budget_state(), PreviewBudgetState::Truncated);
    let full = output(&project).unwrap();
    assert_eq!(full.budget_state(), PreviewBudgetState::Complete);
    assert_eq!(full.pages().len(), 24);
    assert!(full.pages()[11].objects.is_empty());
    assert_eq!(
        full.source_project_digest(),
        project.semantic_digest().unwrap()
    );
    assert_eq!(full.revision(), project.revision);
    for (index, page) in full.pages().iter().enumerate() {
        assert_eq!(page.page_id, project.document.pages[index].id);
        let scoped = ResolvedPagePreview::resolve_with_font_resources(
            &project,
            &page.page_id,
            false,
            &[font()],
        )
        .unwrap();
        assert_eq!(page, scoped.page());
        for object in &page.objects {
            let ResolvedSceneObjectKind::Text { layout, .. } = &object.kind else {
                panic!("text")
            };
            assert_eq!(layout.preview_state, TextBoxPreviewState::Complete);
            assert!(!layout.lines.is_empty());
        }
    }
}

#[test]
fn output_keeps_unincluded_fonts_as_repairable_facts() {
    for sizing_mode in [
        document_core::model::TextBoxSizingMode::Fixed,
        document_core::model::TextBoxSizingMode::Fit,
    ] {
        let mut project = project(1);
        let mut item = text(
            "unbound-text",
            "Draw a box and start writing.",
            &font().source,
        );
        let ObjectKind::Text { style, frame, .. } = &mut item.kind else {
            unreachable!()
        };
        style.font_face = None;
        style.font_family = "Arial".to_owned();
        frame.sizing_mode = sizing_mode;
        project.document.pages[0].objects.push(item);
        let full = ResolvedPublication::resolve_output_with_font_resources(&project, &[])
            .expect("a font that needs inclusion must not abort print checking");
        let ResolvedSceneObjectKind::Text { layout, .. } = &full.pages()[0].objects[0].kind else {
            panic!("text")
        };
        assert_eq!(layout.preview_state, TextBoxPreviewState::MissingFont);
        assert_eq!(
            layout.font_binding.substitution_status,
            layout_core::FontSubstitutionStatus::Unbound
        );
        assert!(layout.lines.is_empty());
    }
}

#[test]
fn late_fonts_are_discovered_and_missing_fonts_remain_explicit() {
    let mut project = project(24);
    let early = font().source;
    let late = AssetHash::for_bytes(b"unavailable exact font");
    for index in 0..23 {
        for item in 0..12 {
            project.document.pages[index].objects.push(text(
                &format!("early-{index}-{item}"),
                "Hello",
                &early,
            ));
        }
    }
    project.document.pages[23]
        .objects
        .push(text("late-font", "Still present", &late));
    assert_eq!(
        output_font_face_bindings(&project).unwrap(),
        [(early, 0), (late, 0)].into()
    );
    let full = output(&project).unwrap();
    let ResolvedSceneObjectKind::Text { layout, .. } = &full.pages()[23].objects[0].kind else {
        panic!("text")
    };
    assert_eq!(layout.preview_state, TextBoxPreviewState::MissingFont);
    assert!(layout.lines.is_empty());
    assert_eq!(
        layout.font_binding.substitution_status,
        layout_core::FontSubstitutionStatus::Missing
    );
}

#[test]
fn output_evaluates_complete_pages_beyond_interactive_item_and_byte_budgets() {
    for byte_limited in [false, true] {
        let mut project = project(2);
        let source = font().source;
        let (count, contents) = if byte_limited {
            (3, "x".repeat(24 * 1024))
        } else {
            (257, "x".to_owned())
        };
        for item in 0..count {
            project.document.pages[1].objects.push(text(
                &format!("limited-{item}"),
                &contents,
                &source,
            ));
        }
        assert_eq!(output_font_face_bindings(&project).unwrap().len(), 1);
        let complete = output(&project).unwrap();
        assert_eq!(complete.pages()[1].objects.len(), count);
        assert_eq!(complete.budget_state(), PreviewBudgetState::Complete);
        let preview = ResolvedPagePreview::resolve_with_font_resources(
            &project,
            &project.document.pages[1].id,
            false,
            &[font()],
        )
        .unwrap();
        assert_eq!(preview.budget_state(), PreviewBudgetState::Truncated);
    }
}

#[test]
fn pasteboard_complexity_and_fonts_do_not_consume_output_budget_but_keep_identity() {
    let mut project = project(1);
    let source = font().source;
    project.document.pages[0]
        .objects
        .push(text("published", "Print this", &source));
    let before = output(&project).unwrap();
    let outside = AssetHash::for_bytes(b"pasteboard-only font");
    for item in 0..257 {
        project.document.pasteboard.objects.push(text(
            &format!("outside-{item}"),
            "Do not print",
            &outside,
        ));
    }
    assert_eq!(
        output_font_face_bindings(&project).unwrap(),
        [(source, 0)].into()
    );
    let after = output(&project).unwrap();
    assert_eq!(before.pages(), after.pages());
    assert!(after.pasteboard().objects.is_empty());
    assert_ne!(
        before.source_project_digest(),
        after.source_project_digest()
    );
    assert_eq!(
        after.source_project_digest(),
        project.semantic_digest().unwrap()
    );
}

#[test]
fn invalid_and_over_budget_sources_fail_before_composition() {
    let mut invalid = project(1);
    invalid.document.pages.clear();
    assert_eq!(
        output(&invalid).unwrap_err(),
        PublicationLayoutError::InvalidOutputSource
    );
    assert_eq!(
        output_font_face_bindings(&invalid).unwrap_err(),
        PublicationLayoutError::InvalidOutputSource
    );
    let mut oversized = project(1);
    oversized.document.title = "t".repeat(document_core::PACKAGE_LIMITS_V1.string_bytes + 1);
    assert_eq!(
        output(&oversized).unwrap_err(),
        PublicationLayoutError::InvalidOutputSource
    );
    assert_eq!(
        output_font_face_bindings(&oversized).unwrap_err(),
        PublicationLayoutError::InvalidOutputSource
    );
}

#[test]
fn early_late_edits_and_undo_match_existing_wrapping_authority() {
    let mut project = project(24);
    let source = font().source;
    for index in [0, 23] {
        let mut obstacle = object(
            &format!("wrap-{index}"),
            ObjectKind::Shape {
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
        );
        obstacle.size = PhysicalSize::new(30_000, 15_000).unwrap();
        obstacle.transform = Transform::identity_at(35_000, 10_000);
        project.document.pages[index].objects = vec![
            text(
                &format!("body-{index}"),
                &"Words wrap around the obstacle. ".repeat(15),
                &source,
            ),
            obstacle,
        ];
    }
    let before = output(&project).unwrap();
    assert_eq!(
        before,
        ResolvedPublication::resolve_with_font_resources(&project, &[font()]).unwrap()
    );
    let mut session = DocumentSession::new(project.document).unwrap();
    for index in [0, 23] {
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Edit page",
                    Command::UpdateText {
                        object_id: ObjectId::parse(format!("body-{index}")).unwrap(),
                        text: "Edited page text".to_owned(),
                    },
                ),
            )
            .unwrap();
        let current = ProjectSnapshot::from_session(&session);
        assert_eq!(
            output(&current).unwrap(),
            ResolvedPublication::resolve_with_font_resources(&current, &[font()]).unwrap()
        );
    }
    session.undo(session.revision()).unwrap();
    session.undo(session.revision()).unwrap();
    let undone = ProjectSnapshot::from_session(&session);
    let after = output(&undone).unwrap();
    assert_eq!(before.pages(), after.pages());
    assert_ne!(before.revision(), after.revision());
    assert_eq!(
        after.source_project_digest(),
        undone.semantic_digest().unwrap()
    );
}

#[test]
fn document_fallbacks_remain_reachable_even_on_blank_pages() {
    let mut project = project(2);
    let source = font().source;
    project
        .document
        .exact_font_fallbacks
        .push(document_core::ExactFontFallbackChain {
            script: document_core::FontFallbackScript::Latin,
            faces: vec![FontFaceBinding {
                source: source.clone(),
                face_index: 0,
            }],
        });
    assert_eq!(
        output_font_face_bindings(&project).unwrap(),
        [(source, 0)].into()
    );
    assert_eq!(output(&project).unwrap().pages().len(), 2);
}

#[test]
fn a_table_counts_once_and_complete_output_keeps_late_empty_tables() {
    let source = font().source;
    let mut project = project(2);
    let mut table = document_core::model::TableData::empty(17, 16).unwrap();
    table.style.font_face = Some(FontFaceBinding {
        source: source.clone(),
        face_index: 0,
    });
    table.cells.fill("Cell".to_owned());
    project.document.pages[1]
        .objects
        .push(object("dense-table", ObjectKind::Table { table }));
    let complete = output(&project).unwrap();
    let ResolvedSceneObjectKind::Table { table } = &complete.pages()[1].objects[0].kind else {
        panic!("Table")
    };
    assert_eq!(table.populated_cells.len(), 272);
    assert_eq!(output_font_face_bindings(&project).unwrap().len(), 1);
    let preview = ResolvedPagePreview::resolve_with_font_resources(
        &project,
        &project.document.pages[1].id,
        false,
        &[font()],
    )
    .unwrap();
    assert_eq!(preview.budget_state(), PreviewBudgetState::Complete);
    project.document.pages[1].objects.clear();
    for item in 0..256 {
        project.document.pages[1]
            .objects
            .push(text(&format!("body-{item}"), "x", &source));
    }
    project.document.pages[1].objects.push(object(
        "empty-table",
        ObjectKind::Table {
            table: document_core::model::TableData::empty(1, 1).unwrap(),
        },
    ));
    assert_eq!(output(&project).unwrap().pages()[1].objects.len(), 257);
    assert_eq!(output_font_face_bindings(&project).unwrap().len(), 1);
    let preview = ResolvedPagePreview::resolve_with_font_resources(
        &project,
        &project.document.pages[1].id,
        false,
        &[font()],
    )
    .unwrap();
    assert_eq!(preview.budget_state(), PreviewBudgetState::Truncated);
}
