use application_core::{
    ApplyTransactionInput, ArrangeGeometryOperationInput, CaptureClipboardInput,
    EditorCommandInput, EditorModule, MasterTransactionInput, SelectionScopeInput,
};
use document_core::model::{
    DocumentId, FontFaceBinding, Insets, ObjectDraft, ObjectKind, PhysicalSize, TextField,
    TextFrame, TextStoryFormatting, TextStyle, Transform,
};
use document_core::{
    AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
    save_new_package,
};

fn text(source: &document_core::AssetHash, value: &str) -> ObjectDraft {
    ObjectDraft {
        name: "Text".into(),
        size: PhysicalSize::new(100_000, 25_000).unwrap(),
        transform: Transform::identity_at(0, 0),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Text {
            text: value.into(),
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
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One real application session proves context isolation, field creation and one-step history.
fn master_context_creates_semantic_number_once_and_refuses_outside_edits() {
    let path = std::env::temp_dir().join(format!(
        "publisher-master-app-{}-{}.publisher",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut assets = AssetStore::default();
    let source = assets.insert(
        "font/ttf",
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec(),
    );
    let trim = PhysicalSize::new(215_900, 279_400).unwrap();
    let mut session = DocumentSession::new(
        Document::new(
            DocumentId::parse("master-app").unwrap(),
            "Master session",
            trim,
            Insets::default(),
        )
        .unwrap(),
    )
    .unwrap();
    let page = session.document().pages[0].id.clone();
    let master = session
        .execute(
            session.revision(),
            Transaction::single(
                "Master",
                Command::AddMaster {
                    name: "Footer".into(),
                    trim,
                },
            ),
        )
        .unwrap()
        .created_masters[0]
        .clone();
    let body = session
        .execute(
            session.revision(),
            Transaction::single(
                "Body",
                Command::AddObject {
                    location: ObjectLocation::Page(page),
                    object: text(&source, "Body"),
                },
            ),
        )
        .unwrap()
        .created_objects[0]
        .clone();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Seed style",
                Command::AddObject {
                    location: ObjectLocation::Master(master.clone()),
                    object: text(&source, "Footer"),
                },
            ),
        )
        .unwrap();
    save_new_package(&path, &ProjectSnapshot::from_session(&session), &assets).unwrap();
    let mut editor = EditorModule::default();
    let opened = editor.open(&path).unwrap();
    let created = editor
        .apply_master(MasterTransactionInput {
            master_id: master.as_str().into(),
            transaction: ApplyTransactionInput {
                expected_revision: opened.project.revision.value(),
                label: "Insert automatic number".into(),
                commands: vec![EditorCommandInput::AddPageNumber {
                    page_id: None,
                    name: "Page number".into(),
                    style: None,
                    x_um: 10_000,
                    y_um: 250_000,
                    width_um: 25_400,
                    height_um: 12_700,
                }],
            },
        })
        .unwrap();
    assert_eq!(
        created.project.document.pages,
        opened.project.document.pages
    );
    assert_eq!(created.project.document.masters[0].objects.len(), 2);
    let field = &created.project.document.masters[0].objects[1];
    assert_eq!(field.text_field, Some(TextField::PageNumber));
    assert!(
        matches!(&field.kind,ObjectKind::Text {text,style,..} if text.is_empty() && style.font_face.as_ref().is_some_and(|face| face.source==source))
    );
    let id = field.id.as_str().to_owned();
    let geometry = editor
        .selection_geometry_scoped(
            created.project.revision.value(),
            core::slice::from_ref(&id),
            SelectionScopeInput::Members,
        )
        .unwrap();
    assert_eq!(geometry.object_ids.as_slice(), core::slice::from_ref(&id));
    let captured = editor
        .capture_clipboard(CaptureClipboardInput {
            document_id: created.project.document.id.as_str().into(),
            expected_revision: created.project.revision.value(),
            object_ids: vec![id.clone()],
            cut: false,
        })
        .unwrap();
    assert_eq!(captured.source_master_id, Some(master.clone()));
    assert_eq!(captured.objects[0].text_field, Some(TextField::PageNumber));
    let undone = editor.undo(created.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, opened.project.document);
    let redone = editor.redo(undone.project.revision.value()).unwrap();
    let centered = editor
        .apply_master(MasterTransactionInput {
            master_id: master.as_str().into(),
            transaction: ApplyTransactionInput {
                expected_revision: redone.project.revision.value(),
                label: "Center on Master".into(),
                commands: vec![
                    EditorCommandInput::ArrangeGeometryToPage {
                        object_ids: vec![id.clone()],
                        operation: ArrangeGeometryOperationInput::AlignCenter,
                    },
                    EditorCommandInput::ArrangeGeometryToPage {
                        object_ids: vec![id.clone()],
                        operation: ArrangeGeometryOperationInput::AlignMiddle,
                    },
                ],
            },
        })
        .unwrap();
    let bounds = editor
        .selection_geometry_scoped(
            centered.project.revision.value(),
            core::slice::from_ref(&id),
            SelectionScopeInput::Members,
        )
        .unwrap()
        .bounds;
    assert!((2 * bounds.x_um + bounds.width_um - trim.width.value()).abs() <= 1);
    assert!((2 * bounds.y_um + bounds.height_um - trim.height.value()).abs() <= 1);
    assert_eq!(
        centered.project.document.pages,
        opened.project.document.pages
    );
    let redone = editor.undo(centered.project.revision.value()).unwrap();
    let original = editor.snapshot().unwrap();
    assert!(
        editor
            .apply_master(MasterTransactionInput {
                master_id: master.as_str().into(),
                transaction: ApplyTransactionInput {
                    expected_revision: redone.project.revision.value(),
                    label: "Wrong workspace".into(),
                    commands: vec![EditorCommandInput::UpdateText {
                        object_id: body.as_str().into(),
                        text: "Changed outside".into()
                    },],
                },
            })
            .is_err()
    );
    assert_eq!(editor.snapshot().unwrap(), original);
    assert!(
        editor
            .preview_text_box_changes(
                redone.project.revision.value(),
                &id,
                vec![application_core::TextEditInput {
                    start_byte: 0,
                    end_byte: 0,
                    text: "typed".into(),
                    bold: None,
                    italic: None,
                    style: None,
                }],
                None
            )
            .is_err()
    );
    assert_eq!(editor.snapshot().unwrap(), original);
    let wire:MasterTransactionInput=serde_json::from_value(serde_json::json!({
        "masterId":master.as_str(),"transaction":{"expectedRevision":redone.project.revision.value(),
        "label":"Shared edit","commands":[{"type":"updateText","objectId":created.project.document.masters[0].objects[0].id.as_str(),"text":"Revised footer"}]}
    })).unwrap();
    let changed = editor.apply_master(wire).unwrap();
    assert!(
        matches!(&changed.project.document.masters[0].objects[0].kind,ObjectKind::Text {text,..} if text=="Revised footer")
    );
    editor.save_current().unwrap();
    assert_eq!(
        document_core::open_package(&path)
            .unwrap()
            .snapshot
            .document,
        changed.project.document
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
#[allow(clippy::too_many_lines)] // One capacity journey proves rejection, unchanged history and the nearest supported assignment.
fn master_instances_obey_the_existing_publication_object_limit_atomically() {
    let path = std::env::temp_dir().join(format!(
        "publisher-master-limit-{}.publisher",
        std::process::id()
    ));
    let mut session = DocumentSession::new(
        Document::new_with_page_count(
            DocumentId::parse("master-limit").unwrap(),
            "Master limit",
            PhysicalSize::new(215_900, 279_400).unwrap(),
            Insets::default(),
            500,
        )
        .unwrap(),
    )
    .unwrap();
    let master = session
        .execute(
            session.revision(),
            Transaction::single(
                "Master",
                Command::AddMaster {
                    name: "Shared artwork".into(),
                    trim: session.document().pages[0].trim,
                },
            ),
        )
        .unwrap()
        .created_masters[0]
        .clone();
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Shared shapes".into(),
                commands: (0..101)
                    .map(|_| Command::AddObject {
                        location: ObjectLocation::Master(master.clone()),
                        object: ObjectDraft {
                            name: "Shape".into(),
                            size: PhysicalSize::new(1_000, 1_000).unwrap(),
                            transform: Transform::identity_at(0, 0),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Shape {
                                shape: document_core::model::ShapeKind::Rectangle,
                                fill: document_core::model::Color {
                                    red: 0,
                                    green: 0,
                                    blue: 0,
                                    alpha: 255,
                                },
                                stroke: document_core::model::ShapeStroke::default(),
                                wrap_mode: document_core::model::WrapMode::None,
                            },
                        },
                    })
                    .collect(),
            },
        )
        .unwrap();
    save_new_package(
        &path,
        &ProjectSnapshot::from_session(&session),
        &AssetStore::default(),
    )
    .unwrap();
    let mut editor = EditorModule::new();
    let original = editor.open(&path).unwrap();
    let pages = original
        .project
        .document
        .pages
        .iter()
        .map(|page| page.id.as_str().to_owned())
        .collect::<Vec<_>>();
    let error = editor
        .apply(ApplyTransactionInput {
            expected_revision: original.project.revision.value(),
            label: "Over limit".into(),
            commands: vec![EditorCommandInput::AssignMaster {
                page_ids: pages.clone(),
                master_id: Some(master.as_str().into()),
            }],
        })
        .unwrap_err();
    assert!(error.message.contains("50,000"));
    assert_eq!(editor.snapshot().unwrap(), original);
    let accepted = editor
        .apply(ApplyTransactionInput {
            expected_revision: original.project.revision.value(),
            label: "Within limit".into(),
            commands: vec![EditorCommandInput::AssignMaster {
                page_ids: pages[..495].to_vec(),
                master_id: Some(master.as_str().into()),
            }],
        })
        .unwrap();
    assert_eq!(
        accepted
            .project
            .document
            .pages
            .iter()
            .filter(|page| page.master_id.is_some())
            .count(),
        495
    );
    let undone = editor.undo(accepted.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, original.project.document);
    std::fs::remove_file(path).unwrap();
}
