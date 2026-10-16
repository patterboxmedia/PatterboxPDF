use document_core::model::{
    DocumentId, FontFaceBinding, Insets, ObjectDraft, ObjectKind, PageNumberFormat,
    PageNumberingSection, PhysicalSize, TextField, TextFrame, TextStoryFormatting, TextStyle,
    Transform,
};
use document_core::{
    AssetHash, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
};
use layout_core::{
    ExactFontResource, ResolvedPagePreview, ResolvedPublication, ResolvedSceneObjectKind,
    master_preview_font_face_bindings, output_font_face_bindings, page_preview_font_face_bindings,
};

fn font() -> ExactFontResource {
    let bytes = include_bytes!("fixtures/DejaVuSans.ttf").to_vec();
    ExactFontResource {
        source: AssetHash::for_bytes(&bytes),
        face_index: 0,
        bytes,
    }
}
fn text(resource: &ExactFontResource, contents: &str, name: &str) -> ObjectDraft {
    ObjectDraft {
        name: name.into(),
        size: PhysicalSize::new(100_000, 25_000).unwrap(),
        transform: Transform::identity_at(10_000, 30_000),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Text {
            text: contents.into(),
            style: TextStyle {
                font_face: Some(FontFaceBinding {
                    source: resource.source.clone(),
                    face_index: 0,
                }),
                font_family: "DejaVu Sans".into(),
                ..TextStyle::default()
            },
            frame: TextFrame::default(),
            story: TextStoryFormatting::default(),
        },
    }
}
fn visible_text(publication: &ResolvedPublication, page: usize) -> Vec<String> {
    publication.pages()[page]
        .objects
        .iter()
        .filter_map(|object| {
            if let ResolvedSceneObjectKind::Text { layout, .. } = &object.kind {
                Some(
                    layout
                        .lines
                        .iter()
                        .map(|line| line.text.as_str())
                        .collect::<String>(),
                )
            } else {
                None
            }
        })
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)] // One exact-font composition verifies inherited fields and source repair identity.
fn masters_and_number_fields_resolve_once_per_page_with_local_paint_above_shared_content() {
    let resource = font();
    let trim = PhysicalSize::new(215_900, 279_400).unwrap();
    let mut session = DocumentSession::new(
        Document::new_with_page_count(
            DocumentId::parse("master-layout").unwrap(),
            "Numbered book",
            trim,
            Insets::default(),
            4,
        )
        .unwrap(),
    )
    .unwrap();
    let pages = session
        .document()
        .pages
        .iter()
        .map(|page| page.id.clone())
        .collect::<Vec<_>>();
    let master = session
        .execute(
            session.revision(),
            Transaction::single(
                "Footer Master",
                Command::AddMaster {
                    name: "Footer".into(),
                    trim,
                },
            ),
        )
        .unwrap()
        .created_masters[0]
        .clone();
    let created = session
        .execute(
            session.revision(),
            Transaction {
                label: "Shared content".into(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Master(master.clone()),
                        object: text(&resource, "Shared footer", "Footer"),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Master(master.clone()),
                        object: text(&resource, "", "Number"),
                    },
                ],
            },
        )
        .unwrap()
        .created_objects;
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Book setup".into(),
                commands: vec![
                    Command::SetTextField {
                        object_id: created[1].clone(),
                        field: Some(TextField::PageNumber),
                    },
                    Command::AssignMaster {
                        page_ids: pages[1..].to_vec(),
                        master_id: Some(master.clone()),
                    },
                    Command::UpdatePageNumbering {
                        page_id: pages[1].clone(),
                        section: Some(PageNumberingSection {
                            format: PageNumberFormat::RomanUpper,
                            start: Some(9),
                        }),
                    },
                    Command::SetPageNumberSuppression {
                        page_ids: vec![pages[2].clone()],
                        suppressed: true,
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(pages[1].clone()),
                        object: text(&resource, "Local", "Body"),
                    },
                ],
            },
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);
    let output = ResolvedPublication::resolve_output_with_font_resources(
        &project,
        core::slice::from_ref(&resource),
    )
    .unwrap();
    assert!(output.pages()[0].objects.is_empty());
    assert_eq!(visible_text(&output, 1), ["Shared footer", "IX", "Local"]);
    assert_eq!(visible_text(&output, 2), ["Shared footer"]);
    assert_eq!(visible_text(&output, 3), ["Shared footer", "XI"]);
    assert_eq!(output.pages()[1].objects[0].object_id, created[0]);
    assert_eq!(output.pages()[1].objects[0].master_id, Some(master.clone()));
    assert!(
        output.pages()[1]
            .objects
            .last()
            .unwrap()
            .master_id
            .is_none()
    );
    assert_eq!(session.document().pages[1].objects.len(), 1);
    assert!(
        matches!(&session.document().masters[0].objects[1].kind,ObjectKind::Text {text,..} if text.is_empty())
    );
    let preview = ResolvedPagePreview::resolve_with_font_resources(
        &project,
        &pages[1],
        false,
        core::slice::from_ref(&resource),
    )
    .unwrap();
    assert_eq!(preview.page(), &output.pages()[1]);
    let master_preview = ResolvedPagePreview::resolve_master_with_font_resources(
        &project,
        &master,
        core::slice::from_ref(&resource),
    )
    .unwrap();
    assert_eq!(master_preview.page().objects.len(), 2);
    assert!(
        master_preview
            .page()
            .objects
            .iter()
            .all(|object| object.master_id.as_ref() == Some(&master))
    );
    let expected = std::collections::BTreeSet::from([(resource.source.clone(), 0)]);
    assert_eq!(output_font_face_bindings(&project).unwrap(), expected);
    assert_eq!(
        page_preview_font_face_bindings(&project, &pages[1], false).unwrap(),
        expected
    );
    assert_eq!(
        master_preview_font_face_bindings(&project, &master).unwrap(),
        expected
    );
    let fitted = ResolvedPublication::resolve_fit_text_box(
        &project,
        core::slice::from_ref(&resource),
        &created[1],
    )
    .unwrap();
    assert_eq!(fitted.layout.lines[0].text, "1");
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Shared edit",
                Command::UpdateText {
                    object_id: created[0].clone(),
                    text: "Revised shared footer".into(),
                },
            ),
        )
        .unwrap();
    let updated = ResolvedPublication::resolve_output_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &[resource],
    )
    .unwrap();
    assert_ne!(updated.semantic_digest(), output.semantic_digest());
    assert_eq!(visible_text(&updated, 3), ["Revised shared footer", "XI"]);
}
