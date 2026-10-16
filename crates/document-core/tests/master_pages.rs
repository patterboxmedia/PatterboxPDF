mod common;

use common::{TempDirectory, letter_document, rectangle};
use document_core::assets::referenced_asset_hashes;
use document_core::model::{
    FontFaceBinding, ObjectDraft, ObjectKind, PhysicalSize, TextFrame, TextStoryFormatting,
    TextStyle, Transform,
};
use document_core::{
    AssetStore, Command, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
    open_package, save_new_package,
};

#[test]
#[allow(clippy::too_many_lines)] // A single shared-definition journey covers references, independent copies and history.
fn named_masters_assign_update_duplicate_reassign_and_reopen_without_flattening() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page_id = session.document().pages[0].id.clone();
    let trim = session.document().pages[0].trim;
    let master = session
        .execute(
            session.revision(),
            Transaction::single(
                "Footer",
                Command::AddMaster {
                    name: "Footer".into(),
                    trim,
                },
            ),
        )
        .unwrap()
        .created_masters[0]
        .clone();
    let added = session
        .execute(
            session.revision(),
            Transaction {
                label: "Shared artwork".into(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Master(master.clone()),
                        object: rectangle("Left", 0, 0),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Master(master.clone()),
                        object: rectangle("Right", 30_000, 0),
                    },
                ],
            },
        )
        .unwrap();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Group",
                Command::GroupObjects {
                    object_ids: added.created_objects.clone(),
                },
            ),
        )
        .unwrap();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Assign",
                Command::AssignMaster {
                    page_ids: vec![page_id.clone()],
                    master_id: Some(master.clone()),
                },
            ),
        )
        .unwrap();
    assert!(session.document().pages[0].objects.is_empty());
    let initial = session.document().clone();
    let copied = session
        .execute(
            session.revision(),
            Transaction::single(
                "Duplicate Master",
                Command::DuplicateMaster {
                    master_id: master.clone(),
                    name: "Footer copy".into(),
                },
            ),
        )
        .unwrap();
    let copy = copied.created_masters[0].clone();
    assert_ne!(
        session.document().masters[0].objects[0].id,
        session.document().masters[1].objects[0].id
    );
    assert_ne!(
        session.document().masters[0].objects[0].group_id,
        session.document().masters[1].objects[0].group_id
    );
    let before_refusal = session.document().clone();
    assert!(
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Wrong size",
                    Command::ResizeMaster {
                        master_id: master.clone(),
                        trim: PhysicalSize::new(100_000, 100_000).unwrap(),
                    }
                )
            )
            .is_err()
    );
    assert_eq!(session.document(), &before_refusal);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Remove and replace",
                Command::RemoveMaster {
                    master_id: master.clone(),
                    replacement: Some(copy.clone()),
                },
            ),
        )
        .unwrap();
    assert_eq!(session.document().pages[0].master_id, Some(copy));
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &before_refusal);
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &initial);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Revise shared footer",
                Command::SetObjectPosition {
                    destination: ObjectLocation::Master(master),
                    object_id: added.created_objects[0].clone(),
                    x: document_core::model::Micrometres::new(5_000),
                    y: document_core::model::Micrometres::new(7_000),
                },
            ),
        )
        .unwrap();
    let composed = session.document().composed_page(&page_id).unwrap();
    assert_eq!(composed.objects[0].transform.translation_x.value(), 5_000);
    assert!(session.document().pages[0].objects.is_empty());
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Lock shared art",
                Command::SetObjectLock {
                    object_ids: added.created_objects,
                    locked: true,
                },
            ),
        )
        .unwrap();
    let locked = session.document().clone();
    let active_master = locked.masters[0].id.clone();
    assert!(
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Remove locked source",
                    Command::RemoveMaster {
                        master_id: active_master,
                        replacement: None,
                    }
                )
            )
            .is_err()
    );
    assert_eq!(session.document(), &locked);
    session.undo(session.revision()).unwrap();
    let temp = TempDirectory::new("master-roundtrip");
    let project = ProjectSnapshot::from_session(&session);
    let path = temp.path().join("master.publisher");
    save_new_package(&path, &project, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, project);
    let mut corrupt = project.document;
    corrupt.pages[0].master_id = Some(document_core::model::MasterId::parse("missing").unwrap());
    assert!(corrupt.validate().is_err());
}

#[test]
fn unassigned_master_font_resources_are_reachable_and_old_defaults_stay_sparse() {
    let document = letter_document();
    let old = serde_json::to_value(&document).unwrap();
    assert!(old.get("masters").is_none());
    assert!(old["pages"][0].get("master_id").is_none());
    assert!(old["pages"][0].get("numbering_section").is_none());
    assert!(old["pages"][0].get("suppress_page_number").is_none());
    let mut session = DocumentSession::new(document).unwrap();
    let master = session
        .execute(
            session.revision(),
            Transaction::single(
                "Unused master",
                Command::AddMaster {
                    name: "Not yet assigned".into(),
                    trim: session.document().pages[0].trim,
                },
            ),
        )
        .unwrap()
        .created_masters[0]
        .clone();
    let mut assets = AssetStore::default();
    let hash = assets.insert(
        "font/ttf",
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec(),
    );
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Shared text",
                Command::AddObject {
                    location: ObjectLocation::Master(master),
                    object: ObjectDraft {
                        name: "Shared heading".into(),
                        size: PhysicalSize::new(100_000, 30_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Reusable".into(),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: hash.clone(),
                                    face_index: 0,
                                }),
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
    assert_eq!(
        referenced_asset_hashes(session.document()),
        std::collections::BTreeSet::from([hash.clone()])
    );
    assert_eq!(
        session.retained_asset_hashes(),
        std::collections::BTreeSet::from([hash.clone()])
    );
    let temp = TempDirectory::new("unassigned-master-resource");
    let path = temp.path().join("master.publisher");
    save_new_package(&path, &ProjectSnapshot::from_session(&session), &assets).unwrap();
    let opened = open_package(&path).unwrap();
    assert!(opened.assets.get(&hash).is_some());
    assert!(opened.snapshot.document.pages[0].objects.is_empty());
}
