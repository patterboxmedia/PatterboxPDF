use application_core::{
    ApplyTransactionInput, EditorCommandInput, EditorModule, ObjectCornersInput,
};
use document_core::model::{
    Color, DocumentId, Insets, ObjectDraft, ObjectKind, PhysicalSize, ShapeKind, ShapeStroke,
    TextFrame, TextStoryFormatting, TextStyle, Transform, WrapMode,
};
use document_core::{
    AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
    save_new_package,
};

fn fixture() -> (EditorModule, std::path::PathBuf) {
    let mut session = DocumentSession::new(
        Document::new(
            DocumentId::parse("bulk-rounding").unwrap(),
            "Large selection",
            PhysicalSize::new(215_900, 279_400).unwrap(),
            Insets::default(),
        )
        .unwrap(),
    )
    .unwrap();
    let location = ObjectLocation::Page(session.document().pages[0].id.clone());
    let shape = |shape| ObjectKind::Shape {
        shape,
        fill: Color {
            red: 20,
            green: 30,
            blue: 40,
            alpha: 255,
        },
        stroke: ShapeStroke::default(),
        wrap_mode: WrapMode::None,
    };
    let kinds = (0..500).map(|_| shape(ShapeKind::Rectangle)).chain([
        shape(ShapeKind::Ellipse),
        ObjectKind::Text {
            text: String::new(),
            style: TextStyle::default(),
            frame: TextFrame {
                column_count: 2,
                column_gap_um: 55_000,
                ..TextFrame::default()
            },
            story: TextStoryFormatting::default(),
        },
    ]);
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Objects".into(),
                commands: kinds
                    .enumerate()
                    .map(|(index, kind)| Command::AddObject {
                        location: location.clone(),
                        object: ObjectDraft {
                            name: format!("Object {index}"),
                            size: PhysicalSize::new(60_000, 40_000).unwrap(),
                            transform: Transform::identity_at(0, 0),
                            opacity_millionths: 1_000_000,
                            kind,
                        },
                    })
                    .collect(),
            },
        )
        .unwrap();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Group",
                Command::GroupObjects {
                    object_ids: session.document().pages[0].objects[..2]
                        .iter()
                        .map(|object| object.id.clone())
                        .collect(),
                },
            ),
        )
        .unwrap();
    let path = std::env::temp_dir().join(format!(
        "publisher-bulk-corners-{}-{}.publisher",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    save_new_package(
        &path,
        &ProjectSnapshot::from_session(&session),
        &AssetStore::default(),
    )
    .unwrap();
    let mut editor = EditorModule::new();
    editor.open(&path).unwrap();
    (editor, path)
}

fn action(ids: Vec<String>) -> EditorCommandInput {
    EditorCommandInput::UpdateSelectionCornerRounding {
        object_ids: ids,
        top_left_um: 20_000,
        top_right_um: 20_000,
        bottom_right_um: 20_000,
        bottom_left_um: 20_000,
        linked: true,
    }
}

fn per_object_action(ids: Vec<String>) -> EditorCommandInput {
    EditorCommandInput::UpdateObjectCorners {
        objects: ids
            .into_iter()
            .map(|object_id| ObjectCornersInput {
                object_id,
                top_left_um: 20_000,
                top_right_um: 20_000,
                bottom_right_um: 20_000,
                bottom_left_um: 20_000,
                linked: true,
            })
            .collect(),
    }
}
fn apply(
    editor: &mut EditorModule,
    command: EditorCommandInput,
) -> application_core::EditorResult<application_core::EditorSnapshot> {
    editor.apply(ApplyTransactionInput {
        expected_revision: editor.snapshot().unwrap().project.revision.value(),
        label: "Round selected objects".into(),
        commands: vec![command],
    })
}

#[test]
fn five_hundred_corner_targets_are_one_undo_and_member_scope_keeps_siblings() {
    let (mut editor, path) = fixture();
    let before = editor.snapshot().unwrap();
    let ids = before.project.document.pages[0].objects[..500]
        .iter()
        .map(|object| object.id.as_str().to_owned())
        .collect::<Vec<_>>();
    let rounded = apply(&mut editor, action(ids.clone())).unwrap();
    for (old, new) in before.project.document.pages[0]
        .objects
        .iter()
        .zip(&rounded.project.document.pages[0].objects)
        .take(500)
    {
        let mut expected = old.clone();
        expected.corner_rounding = new.corner_rounding;
        assert_eq!(&expected, new);
        assert_eq!(new.corner_rounding.top_left_um, 20_000);
    }
    assert_eq!(
        rounded.project.document.pages[0].objects[500..],
        before.project.document.pages[0].objects[500..]
    );
    let undone = editor.undo(rounded.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, before.project.document);
    let member = apply(&mut editor, action(vec![ids[0].clone()])).unwrap();
    assert_eq!(
        member.project.document.pages[0].objects[1],
        before.project.document.pages[0].objects[1]
    );
    assert_eq!(
        member.project.document.pages[0].objects[0].group_id,
        before.project.document.pages[0].objects[0].group_id
    );
    let undone = editor.undo(member.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, before.project.document);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn bulk_corner_refusals_preserve_every_target_revision_and_history() {
    let (mut editor, path) = fixture();
    let before = editor.snapshot().unwrap();
    let ids = before.project.document.pages[0]
        .objects
        .iter()
        .map(|object| object.id.as_str().to_owned())
        .collect::<Vec<_>>();
    for selected in [
        Vec::new(),
        vec![ids[0].clone(); 2],
        ids[..501].to_vec(),
        vec![ids[0].clone(), "missing-object".into()],
        vec![ids[0].clone(), ids[500].clone()],
        vec![ids[0].clone(), ids[501].clone()],
    ] {
        assert!(apply(&mut editor, action(selected.clone())).is_err());
        assert_eq!(editor.snapshot().unwrap(), before);
        assert!(apply(&mut editor, per_object_action(selected)).is_err());
        assert_eq!(editor.snapshot().unwrap(), before);
    }
    let locked = apply(
        &mut editor,
        EditorCommandInput::SetObjectLock {
            object_ids: vec![ids[499].clone()],
            locked: true,
        },
    )
    .unwrap();
    assert!(apply(&mut editor, action(ids[..500].to_vec())).is_err());
    assert_eq!(editor.snapshot().unwrap(), locked);
    assert!(apply(&mut editor, per_object_action(ids[..500].to_vec())).is_err());
    assert_eq!(editor.snapshot().unwrap(), locked);
    let undone = editor.undo(locked.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, before.project.document);
    assert!(!undone.can_undo);
    assert!(undone.can_redo);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn mixed_five_hundred_corner_tuples_keep_unedited_corners_and_undo_once() {
    let (mut editor, path) = fixture();
    let before = editor.snapshot().unwrap();
    let objects = before.project.document.pages[0].objects[..500]
        .iter()
        .enumerate()
        .map(|(index, object)| {
            let variation = i64::try_from(index).unwrap();
            ObjectCornersInput {
                object_id: object.id.as_str().into(),
                top_left_um: 1_000 + variation,
                top_right_um: 2_000 + 2 * variation,
                bottom_right_um: 3_000 + 3 * variation,
                bottom_left_um: 4_000 + 4 * variation,
                linked: false,
            }
        })
        .collect();
    let mixed = apply(
        &mut editor,
        EditorCommandInput::UpdateObjectCorners { objects },
    )
    .unwrap();
    let rows=mixed.project.document.pages[0].objects[..500].iter().map(|object| {
        let corners=object.corner_rounding;
        serde_json::json!({"objectId":object.id.as_str(),"topLeftUm":8_000,"topRightUm":corners.top_right_um,"bottomRightUm":corners.bottom_right_um,"bottomLeftUm":corners.bottom_left_um,"linked":corners.linked})
    }).collect::<Vec<_>>();
    let wire: EditorCommandInput =
        serde_json::from_value(serde_json::json!({"type":"updateObjectCorners","objects":rows}))
            .unwrap();
    let edited = apply(&mut editor, wire).unwrap();
    assert_eq!(
        edited.project.revision.value(),
        mixed.project.revision.value() + 1
    );
    for (old, new) in mixed.project.document.pages[0]
        .objects
        .iter()
        .zip(&edited.project.document.pages[0].objects)
        .take(500)
    {
        let mut expected = old.clone();
        expected.corner_rounding.top_left_um = 8_000;
        assert_eq!(&expected, new);
    }
    assert_eq!(
        edited.project.document.pages[0].objects[500..],
        mixed.project.document.pages[0].objects[500..]
    );
    let undone = editor.undo(edited.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, mixed.project.document);
    let redone = editor.redo(undone.project.revision.value()).unwrap();
    assert_eq!(redone.project.document, edited.project.document);
    std::fs::remove_file(path).unwrap();
}
