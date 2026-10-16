use document_core::model::{
    Color, DocumentId, FontFaceBinding, ImageContentKind, ImageCrop, Insets, ObjectEffects,
    ObjectKind, PageId, PhysicalSize, PublicationObject, ShapeKind, ShapeStroke, TextFrame,
    TextStoryFormatting, TextStyle, Transform, WrapMode,
};
use document_core::{
    AssetHash, Command, Document, DocumentSession, ObjectId, ProjectSnapshot, Revision, Transaction,
};
use layout_core::{
    ExactFontResource, PreviewBudgetState, PublicationLayoutError, ResolvedPagePreview,
    ResolvedPublication, ResolvedSceneObjectKind, TextBoxPreviewState,
    page_preview_font_face_bindings,
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

fn preview(project: &ProjectSnapshot, page: usize, pasteboard: bool) -> ResolvedPagePreview {
    ResolvedPagePreview::resolve_with_font_resources(
        project,
        &project.document.pages[page].id,
        pasteboard,
        &[font()],
    )
    .unwrap()
}

#[test]
fn late_page_remains_complete_beyond_the_publication_item_ceiling() {
    let mut project = project(24);
    let source = font().source;
    for (page_index, page) in project.document.pages.iter_mut().enumerate() {
        for item in 0..19 {
            page.objects.push(text(
                &format!("text-{page_index}-{item}"),
                "Read this.",
                &source,
            ));
        }
    }
    let full = ResolvedPublication::resolve_with_font_resources(&project, &[font()]).unwrap();
    assert_eq!(full.budget_state(), PreviewBudgetState::Truncated);
    let ResolvedSceneObjectKind::Text { layout, .. } = &full.pages()[23].objects[0].kind else {
        panic!("text fixture");
    };
    assert_eq!(layout.preview_state, TextBoxPreviewState::BudgetExceeded);
    let late = preview(&project, 23, false);
    assert_eq!(late.budget_state(), PreviewBudgetState::Complete);
    assert_eq!(late.page().page_id, project.document.pages[23].id);
    for object in &late.page().objects {
        let ResolvedSceneObjectKind::Text { layout, .. } = &object.kind else {
            panic!("text fixture")
        };
        assert_eq!(layout.preview_state, TextBoxPreviewState::Complete);
        assert!(!layout.lines.is_empty());
    }
    assert_eq!(
        late.source_project_digest(),
        project.semantic_digest().unwrap()
    );
}

#[test]
fn early_and_late_page_wrapping_equals_complete_publication_authority() {
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
                &"Text wraps around the shape while preserving source ranges. ".repeat(8),
                &source,
            ),
            obstacle,
        ];
    }
    let full = ResolvedPublication::resolve_with_font_resources(&project, &[font()]).unwrap();
    assert_eq!(full.budget_state(), PreviewBudgetState::Complete);
    for index in [0, 23] {
        assert_eq!(preview(&project, index, false).page(), &full.pages()[index]);
    }
}

#[test]
fn both_entrypoints_reject_unknown_pages() {
    let project = project(1);
    let missing = PageId::parse("missing-page").unwrap();
    assert_eq!(
        page_preview_font_face_bindings(&project, &missing, true),
        Err(PublicationLayoutError::PageNotFound)
    );
    assert_eq!(
        ResolvedPagePreview::resolve_with_font_resources(&project, &missing, true, &[]),
        Err(PublicationLayoutError::PageNotFound)
    );
}

#[test]
fn font_discovery_excludes_other_pages_and_unrequested_pasteboard() {
    let mut project = project(2);
    let source = font().source;
    let absent = AssetHash::for_bytes(b"unrelated missing font");
    project.document.pages[0]
        .objects
        .push(text("first", "First", &absent));
    project.document.pages[1]
        .objects
        .push(text("last", "Last", &source));
    project
        .document
        .pasteboard
        .objects
        .push(text("pasteboard", "Pasteboard", &absent));
    let page_id = &project.document.pages[1].id;
    assert_eq!(
        page_preview_font_face_bindings(&project, page_id, false).unwrap(),
        [(source.clone(), 0)].into()
    );
    assert_eq!(
        page_preview_font_face_bindings(&project, page_id, true).unwrap(),
        [(source, 0), (absent, 0)].into()
    );
    let scoped = preview(&project, 1, false);
    assert!(scoped.pasteboard().is_none());
    let ResolvedSceneObjectKind::Text { layout, .. } = &scoped.page().objects[0].kind else {
        panic!("text fixture")
    };
    assert_eq!(layout.preview_state, TextBoxPreviewState::Complete);
    let included = preview(&project, 1, true);
    let ResolvedSceneObjectKind::Text { layout, .. } =
        &included.pasteboard().unwrap().objects[0].kind
    else {
        panic!("text fixture")
    };
    assert_eq!(layout.preview_state, TextBoxPreviewState::MissingFont);
}

#[test]
fn page_and_pasteboard_share_the_unchanged_request_budget() {
    let mut project = project(1);
    let source = font().source;
    for index in 0..256 {
        project.document.pages[0]
            .objects
            .push(text(&format!("text-{index}"), "x", &source));
    }
    let absent = AssetHash::for_bytes(b"over budget face");
    project
        .document
        .pasteboard
        .objects
        .push(text("extra", "x", &absent));
    assert_eq!(
        preview(&project, 0, false).budget_state(),
        PreviewBudgetState::Complete
    );
    let included = preview(&project, 0, true);
    assert_eq!(included.budget_state(), PreviewBudgetState::Truncated);
    let ResolvedSceneObjectKind::Text { layout, .. } =
        &included.pasteboard().unwrap().objects[0].kind
    else {
        panic!("text fixture")
    };
    assert_eq!(layout.preview_state, TextBoxPreviewState::BudgetExceeded);
    assert!(
        !page_preview_font_face_bindings(&project, &project.document.pages[0].id, true)
            .unwrap()
            .contains(&(absent, 0))
    );
}

#[test]
fn page_preview_preserves_total_and_per_item_byte_ceilings() {
    let mut project = project(1);
    let unavailable = AssetHash::for_bytes(b"unavailable base");
    let limited = AssetHash::for_bytes(b"limited font");
    project.document.pages[0].objects = vec![
        text("large-first", &"x".repeat(32 * 1024), &unavailable),
        text("large-second", &"y".repeat(32 * 1024), &unavailable),
        text("over-total", "z", &limited),
        text("over-item", &"w".repeat(32 * 1024 + 1), &limited),
    ];
    let result = preview(&project, 0, false);
    assert_eq!(result.budget_state(), PreviewBudgetState::Truncated);
    let states = result
        .page()
        .objects
        .iter()
        .map(|object| {
            let ResolvedSceneObjectKind::Text { layout, .. } = &object.kind else {
                panic!("text fixture")
            };
            layout.preview_state
        })
        .collect::<Vec<_>>();
    assert_eq!(
        states,
        vec![
            TextBoxPreviewState::MissingFont,
            TextBoxPreviewState::MissingFont,
            TextBoxPreviewState::BudgetExceeded,
            TextBoxPreviewState::BudgetExceeded
        ]
    );
    assert_eq!(
        page_preview_font_face_bindings(&project, &project.document.pages[0].id, false).unwrap(),
        [(unavailable, 0)].into()
    );
    project.document.pages[0].objects = vec![text(
        "over-item-alone",
        &"w".repeat(32 * 1024 + 1),
        &limited,
    )];
    assert_eq!(
        preview(&project, 0, false).budget_state(),
        PreviewBudgetState::Truncated
    );
    assert!(
        page_preview_font_face_bindings(&project, &project.document.pages[0].id, false)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn full_identity_tracks_shared_asset_references_outside_the_page_and_undo_revision() {
    let mut project = project(2);
    let image = object(
        "shared-picture",
        ObjectKind::Image {
            asset: AssetHash::for_bytes(b"shared picture original"),
            wrap_mode: WrapMode::None,
            content_kind: ImageContentKind::ContinuousTone,
            crop: ImageCrop::default(),
            flip: document_core::model::ImageFlip::default(),
            lock_aspect_ratio: true,
        },
    );
    project.document.pages[1].objects.push(image);
    let initial = preview(&project, 0, false);
    let mut replaced_asset = project.clone();
    let ObjectKind::Image { asset, .. } = &mut replaced_asset.document.pages[1].objects[0].kind
    else {
        panic!("picture fixture")
    };
    *asset = AssetHash::for_bytes(b"shared picture replacement");
    let changed = preview(&replaced_asset, 0, false);
    assert_eq!(initial.page(), changed.page());
    assert_ne!(
        initial.source_project_digest(),
        changed.source_project_digest()
    );

    let mut session = DocumentSession::new(project.document.clone()).unwrap();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Change picture",
                Command::UpdateImageContentKind {
                    object_id: ObjectId::parse("shared-picture").unwrap(),
                    content_kind: ImageContentKind::LineArt,
                },
            ),
        )
        .unwrap();
    session.undo(session.revision()).unwrap();
    let undone = ProjectSnapshot::from_session(&session);
    assert_eq!(undone.document, project.document);
    let result = preview(&undone, 0, false);
    assert_eq!(result.page(), initial.page());
    assert_ne!(result.revision(), initial.revision());
    assert_ne!(
        result.source_project_digest(),
        initial.source_project_digest()
    );
    assert_eq!(
        result.source_project_digest(),
        undone.semantic_digest().unwrap()
    );
}
