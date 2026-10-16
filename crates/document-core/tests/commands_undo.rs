mod common;

use common::{letter_document, rectangle};
use document_core::AssetHash;
use document_core::commands::{
    ArrangeGeometryOperation, ArrangeOperation, Command, CommandError, DocumentSession,
    FontFaceBindingUpdate, ObjectLocation, Transaction,
};
use document_core::model::Micrometres;

#[test]
fn arrange_uses_rotated_visible_edges_without_changing_member_content() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page_id = session.document().pages[0].id.clone();
    let mut turned = rectangle("Turned", 20_000, 30_000);
    turned.transform.rotation_microdegrees = 90_000_000;
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add rotated composition".to_owned(),
                commands: [turned, rectangle("Plain", 80_000, 50_000)]
                    .into_iter()
                    .map(|object| Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object,
                    })
                    .collect(),
            },
        )
        .unwrap();
    let before = session.document().clone();
    let aligned = session
        .execute(
            added.revision,
            Transaction::single(
                "Align visible left edges",
                Command::ArrangeGeometry {
                    object_ids: added.created_objects,
                    operation: ArrangeGeometryOperation::AlignLeft,
                },
            ),
        )
        .unwrap();
    let objects = &session.document().pages[0].objects;
    assert_eq!(
        objects[1].transform.translation_x.value(),
        objects[0].transform.translation_x.value() - 25_400
    );
    assert_eq!(objects[0].kind, before.pages[0].objects[0].kind);
    assert_eq!(objects[0].size, before.pages[0].objects[0].size);
    session.undo(aligned.revision).unwrap();
    assert_eq!(session.document(), &before);
}

#[test]
fn arrange_accepts_all_selected_members_of_a_group_as_one_unit() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add composition".to_owned(),
                commands: [
                    rectangle("Die", 10_000, 20_000),
                    rectangle("Pip", 15_000, 25_000),
                    rectangle("Caption", 80_000, 50_000),
                ]
                .into_iter()
                .map(|object| Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object,
                })
                .collect(),
            },
        )
        .unwrap();
    session
        .execute(
            added.revision,
            Transaction::single(
                "Group die",
                Command::GroupObjects {
                    object_ids: added.created_objects[..2].to_vec(),
                },
            ),
        )
        .unwrap();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Align selected group and caption",
                Command::ArrangeGeometry {
                    object_ids: added.created_objects,
                    operation: ArrangeGeometryOperation::AlignTop,
                },
            ),
        )
        .expect("ordinary whole-group selection is one Arrange unit");
    let objects = &session.document().pages[0].objects;
    assert_eq!(
        objects[0].transform.translation_y,
        objects[2].transform.translation_y
    );
    assert_eq!(
        objects[1].transform.translation_y.value() - objects[0].transform.translation_y.value(),
        5_000
    );
}

#[test]
fn setting_position_preserves_affine_components_across_containers_and_rejects_locked_objects() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page_id = session.document().pages[0].id.clone();
    let mut draft = rectangle("Transformed object", 20_000, 30_000);
    draft.transform.rotation_microdegrees = 32_500_000;
    draft.transform.pivot_x = Micrometres::new(7_000);
    draft.transform.pivot_y = Micrometres::new(-2_000);
    draft.transform.scale_x_nanos = -1_200_000_000;
    draft.transform.scale_y_nanos = 700_000_000;
    draft.transform.shear_x_nanos = 200_000_000;
    draft.transform.shear_y_nanos = -300_000_000;
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object: draft,
                },
            ),
        )
        .unwrap();
    let original = session.document().pages[0].objects[0].clone();
    for destination in [
        ObjectLocation::Page(page_id.clone()),
        ObjectLocation::Pasteboard,
        ObjectLocation::Page(page_id),
    ] {
        let before = session.document().clone();
        let edited = session
            .execute(
                session.revision(),
                Transaction::single(
                    "Move",
                    Command::SetObjectPosition {
                        object_id: original.id.clone(),
                        destination: destination.clone(),
                        x: Micrometres::new(45_000),
                        y: Micrometres::new(55_000),
                    },
                ),
            )
            .unwrap();
        let actual = match destination {
            ObjectLocation::Page(_) => &session.document().pages[0].objects[0],
            ObjectLocation::Pasteboard => &session.document().pasteboard.objects[0],
            ObjectLocation::Master(id) => &session.document().masters.iter().find(|master| master.id == id).unwrap().objects[0],
        };
        let mut expected = original.clone();
        expected.transform.translation_x = Micrometres::new(45_000);
        expected.transform.translation_y = Micrometres::new(55_000);
        assert_eq!(*actual, expected);
        let after = session.document().clone();
        let undone = session.undo(edited.revision).unwrap();
        assert_eq!(*session.document(), before);
        session.redo(undone).unwrap();
        assert_eq!(*session.document(), after);
    }
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Lock",
                Command::SetObjectLock {
                    object_ids: vec![original.id.clone()],
                    locked: true,
                },
            ),
        )
        .unwrap();
    let before = session.document().clone();
    let revision = session.revision();
    assert!(matches!(
        session.execute(
            revision,
            Transaction::single(
                "Refused move",
                Command::SetObjectPosition {
                    object_id: original.id,
                    destination: ObjectLocation::Pasteboard,
                    x: Micrometres::new(0),
                    y: Micrometres::new(0),
                }
            )
        ),
        Err(CommandError::ObjectLocked(_))
    ));
    assert_eq!(*session.document(), before);
    assert_eq!(session.revision(), revision);
}

#[test]
fn grouping_compacts_at_the_highest_selected_stack_position_and_arrange_moves_one_block() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Build interleaved dice".to_owned(),
                commands: ["Back die", "Unrelated star", "Front die", "Caption"]
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle(
                            name,
                            10_000 + i64::try_from(index).unwrap() * 10_000,
                            20_000,
                        ),
                    })
                    .collect(),
            },
        )
        .expect("add stack");
    let ids = added.created_objects;
    let grouped = session
        .execute(
            added.revision,
            Transaction::single(
                "Group dice",
                Command::GroupObjects {
                    object_ids: vec![ids[0].clone(), ids[2].clone()],
                },
            ),
        )
        .expect("group interleaved members");
    assert_eq!(
        session.document().pages[0]
            .objects
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>(),
        ["Unrelated star", "Back die", "Front die", "Caption"]
    );

    let arranged = session
        .execute(
            grouped.revision,
            Transaction::single(
                "Bring dice forward",
                Command::ArrangeObject {
                    object_id: ids[0].clone(),
                    operation: ArrangeOperation::BringForward,
                },
            ),
        )
        .expect("arrange group as a block");
    assert_eq!(
        session.document().pages[0]
            .objects
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>(),
        ["Unrelated star", "Caption", "Back die", "Front die"]
    );
    session
        .undo(arranged.revision)
        .expect("one undo restores the stack");
    assert_eq!(
        session.document().pages[0]
            .objects
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>(),
        ["Unrelated star", "Back die", "Front die", "Caption"]
    );
}

#[test]
fn paste_selection_remaps_group_and_object_ids_unlocks_copies_and_undoes_once() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Build copy selection".to_owned(),
                commands: [("Die", 10_000, 20_000), ("Pip", 20_000, 30_000)]
                    .into_iter()
                    .map(|(name, x, y)| Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle(name, x, y),
                    })
                    .collect(),
            },
        )
        .expect("add selection");
    let source_ids = added.created_objects;
    let grouped = session
        .execute(
            added.revision,
            Transaction::single(
                "Group selection",
                Command::GroupObjects {
                    object_ids: source_ids.clone(),
                },
            ),
        )
        .expect("group selection");
    let locked = session
        .execute(
            grouped.revision,
            Transaction::single(
                "Lock source",
                Command::SetObjectLock {
                    object_ids: source_ids.clone(),
                    locked: true,
                },
            ),
        )
        .expect("lock source");
    let source_group = session.document().pages[0].objects[0]
        .group_id
        .clone()
        .expect("source group");
    let copied_objects = session.document().pages[0].objects.clone();

    let pasted = session
        .execute(
            locked.revision,
            Transaction::single(
                "Paste",
                Command::PasteObjects {
                    objects: copied_objects,
                    destination: ObjectLocation::Page(page_id),
                    offset_x_um: 12_000,
                    offset_y_um: 12_000,
                },
            ),
        )
        .expect("paste locked source as editable copy");
    assert_eq!(pasted.created_objects.len(), 2);
    let copies = &session.document().pages[0].objects[2..];
    assert!(copies.iter().all(|object| !object.locked));
    assert_eq!(copies[0].kind, session.document().pages[0].objects[0].kind);
    assert_eq!(copies[0].transform.translation_x.value(), 22_000);
    assert_eq!(copies[1].transform.translation_y.value(), 42_000);
    let pasted_group = copies[0].group_id.clone().expect("pasted group");
    assert_ne!(pasted_group, source_group);
    assert_eq!(copies[1].group_id.as_ref(), Some(&pasted_group));
    assert!(
        pasted
            .created_objects
            .iter()
            .all(|id| !source_ids.contains(id))
    );

    session.undo(pasted.revision).expect("undo paste once");
    assert_eq!(session.document().pages[0].objects.len(), 2);
}

#[test]
fn paste_selection_refuses_partial_groups_duplicates_and_coordinate_overflow_atomically() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Build group".to_owned(),
                commands: [
                    rectangle("Die", 10_000, 20_000),
                    rectangle("Pip", i64::MAX, 30_000),
                    rectangle("Caption", 50_000, 40_000),
                ]
                .into_iter()
                .map(|object| Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object,
                })
                .collect(),
            },
        )
        .expect("add group");
    let ids = added.created_objects;
    let grouped = session
        .execute(
            added.revision,
            Transaction::single(
                "Group selection",
                Command::GroupObjects {
                    object_ids: ids.clone(),
                },
            ),
        )
        .expect("group selection");
    let grouped_objects = session.document().pages[0].objects.clone();

    for invalid_objects in [
        vec![grouped_objects[0].clone()],
        vec![grouped_objects[0].clone(), grouped_objects[2].clone()],
        vec![grouped_objects[0].clone(), grouped_objects[0].clone()],
    ] {
        session
            .execute(
                grouped.revision,
                Transaction::single(
                    "Invalid paste",
                    Command::PasteObjects {
                        objects: invalid_objects,
                        destination: ObjectLocation::Page(page_id.clone()),
                        offset_x_um: 12_000,
                        offset_y_um: 12_000,
                    },
                ),
            )
            .expect_err("invalid selection rejected");
        assert_eq!(session.revision(), grouped.revision);
        assert_eq!(session.document().pages[0].objects.len(), 3);
    }

    session
        .execute(
            grouped.revision,
            Transaction::single(
                "Overflow paste",
                Command::PasteObjects {
                    objects: grouped_objects,
                    destination: ObjectLocation::Page(page_id),
                    offset_x_um: 1,
                    offset_y_um: 0,
                },
            ),
        )
        .expect_err("overflow rejected");
    assert_eq!(session.revision(), grouped.revision);
    assert_eq!(session.document().pages[0].objects.len(), 3);
}

#[test]
fn align_and_distribute_treat_a_flat_group_as_one_undoable_selection_unit() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Build alignment set".to_owned(),
                commands: [
                    ("Die", 10_000, 10_000),
                    ("Pip", 20_000, 20_000),
                    ("Middle", 50_000, 40_000),
                    ("Right", 110_000, 70_000),
                ]
                .into_iter()
                .map(|(name, x, y)| Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object: rectangle(name, x, y),
                })
                .collect(),
            },
        )
        .expect("add objects");
    let ids = added.created_objects;
    let grouped = session
        .execute(
            added.revision,
            Transaction::single(
                "Group die",
                Command::GroupObjects {
                    object_ids: vec![ids[0].clone(), ids[1].clone()],
                },
            ),
        )
        .expect("group die");

    let aligned = session
        .execute(
            grouped.revision,
            Transaction::single(
                "Align tops",
                Command::ArrangeGeometry {
                    object_ids: vec![ids[0].clone(), ids[2].clone(), ids[3].clone()],
                    operation: ArrangeGeometryOperation::AlignTop,
                },
            ),
        )
        .expect("align three units");
    let objects = &session.document().pages[0].objects;
    assert_eq!(objects[0].transform.translation_y.value(), 10_000);
    assert_eq!(objects[1].transform.translation_y.value(), 20_000);
    assert_eq!(objects[2].transform.translation_y.value(), 10_000);
    assert_eq!(objects[3].transform.translation_y.value(), 10_000);

    let distributed = session
        .execute(
            aligned.revision,
            Transaction::single(
                "Distribute horizontally",
                Command::ArrangeGeometry {
                    object_ids: vec![ids[3].clone(), ids[0].clone(), ids[2].clone()],
                    operation: ArrangeGeometryOperation::DistributeHorizontally,
                },
            ),
        )
        .expect("distribute regardless of input order");
    let objects = &session.document().pages[0].objects;
    assert_eq!(objects[0].transform.translation_x.value(), 10_000);
    assert_eq!(objects[1].transform.translation_x.value(), 20_000);
    assert_eq!(objects[2].transform.translation_x.value(), 62_500);
    assert_eq!(objects[3].transform.translation_x.value(), 110_000);

    session
        .undo(distributed.revision)
        .expect("one undo restores every translated unit");
    let objects = &session.document().pages[0].objects;
    assert_eq!(objects[2].transform.translation_x.value(), 50_000);
    assert_eq!(objects[3].transform.translation_x.value(), 110_000);
}

fn assert_arrange_geometry_error(
    session: &mut DocumentSession,
    revision: Revision,
    object_ids: Vec<ObjectId>,
    operation: ArrangeGeometryOperation,
    expected: &CommandError,
) {
    let actual = session
        .execute(
            revision,
            Transaction::single(
                "Invalid geometry",
                Command::ArrangeGeometry {
                    object_ids,
                    operation,
                },
            ),
        )
        .expect_err("invalid arrangement is rejected");
    assert_eq!(&actual, expected);
    assert_eq!(session.revision(), revision);
}

#[test]
fn arrange_geometry_rejects_invalid_locked_and_mixed_container_selections_atomically() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add candidates".to_owned(),
                commands: (0..3)
                    .map(|index| Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle("Candidate", i64::from(index) * 40_000, 0),
                    })
                    .collect(),
            },
        )
        .expect("add candidates");
    let ids = added.created_objects;

    for (selection, operation, expected) in [
        (
            vec![ids[0].clone()],
            ArrangeGeometryOperation::AlignLeft,
            CommandError::InvalidArrangeGeometrySelection,
        ),
        (
            vec![ids[0].clone(), ids[0].clone()],
            ArrangeGeometryOperation::AlignLeft,
            CommandError::InvalidArrangeGeometrySelection,
        ),
        (
            vec![ids[0].clone(), ids[1].clone()],
            ArrangeGeometryOperation::DistributeVertically,
            CommandError::InvalidArrangeGeometrySelection,
        ),
    ] {
        assert_arrange_geometry_error(
            &mut session,
            added.revision,
            selection,
            operation,
            &expected,
        );
    }

    let moved = session
        .execute(
            added.revision,
            Transaction::single(
                "Move to pasteboard",
                Command::MoveObject {
                    object_id: ids[2].clone(),
                    destination: ObjectLocation::Pasteboard,
                    transform: Transform::identity_at(80_000, 0),
                },
            ),
        )
        .expect("move candidate");
    assert_eq!(
        session
            .execute(
                moved.revision,
                Transaction::single(
                    "Mixed containers",
                    Command::ArrangeGeometry {
                        object_ids: vec![ids[0].clone(), ids[2].clone()],
                        operation: ArrangeGeometryOperation::AlignLeft,
                    },
                ),
            )
            .expect_err("mixed page and pasteboard rejected"),
        CommandError::ArrangeGeometryObjectsInDifferentContainers
    );
    assert_eq!(session.revision(), moved.revision);

    let locked = session
        .execute(
            moved.revision,
            Transaction::single(
                "Lock candidate",
                Command::SetObjectLock {
                    object_ids: vec![ids[1].clone()],
                    locked: true,
                },
            ),
        )
        .expect("lock candidate");
    assert_eq!(
        session
            .execute(
                locked.revision,
                Transaction::single(
                    "Locked align",
                    Command::ArrangeGeometry {
                        object_ids: vec![ids[0].clone(), ids[1].clone()],
                        operation: ArrangeGeometryOperation::AlignBottom,
                    },
                ),
            )
            .expect_err("locked candidate rejected"),
        CommandError::ObjectLocked(ids[1].clone())
    );
    assert_eq!(session.revision(), locked.revision);
}

#[test]
fn locked_objects_reject_every_direct_mutation_until_unlocked() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add die",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: rectangle("Die", 10_000, 20_000),
                },
            ),
        )
        .expect("add object");
    let object_id = added.created_objects[0].clone();
    let locked = session
        .execute(
            added.revision,
            Transaction::single(
                "Lock die",
                Command::SetObjectLock {
                    object_ids: vec![object_id.clone()],
                    locked: true,
                },
            ),
        )
        .expect("lock object");
    assert!(session.document().pages[0].objects[0].locked);

    for command in [
        Command::MoveObject {
            object_id: object_id.clone(),
            destination: ObjectLocation::Pasteboard,
            transform: Transform::identity_at(0, 0),
        },
        Command::ResizeObject {
            object_id: object_id.clone(),
            size: PhysicalSize::new(50_000, 50_000).unwrap(),
        },
        Command::DeleteObject {
            object_id: object_id.clone(),
        },
    ] {
        let error = session
            .execute(
                locked.revision,
                Transaction::single("Blocked edit", command),
            )
            .expect_err("locked object must reject mutation");
        assert_eq!(error, CommandError::ObjectLocked(object_id.clone()));
        assert_eq!(session.revision(), locked.revision);
    }

    let unlocked = session
        .execute(
            locked.revision,
            Transaction::single(
                "Unlock die",
                Command::SetObjectLock {
                    object_ids: vec![object_id.clone()],
                    locked: false,
                },
            ),
        )
        .expect("unlock object");
    session
        .execute(
            unlocked.revision,
            Transaction::single("Delete die", Command::DeleteObject { object_id }),
        )
        .expect("unlocked object can be edited");
}

#[test]
fn legacy_reorder_cannot_split_a_group_and_locked_siblings_block_member_deletion() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add dice".to_owned(),
                commands: ["Die", "Pip", "Outsider"]
                    .into_iter()
                    .map(|name| Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle(name, 0, 0),
                    })
                    .collect(),
            },
        )
        .expect("add objects");
    let ids = added.created_objects;
    let grouped = session
        .execute(
            added.revision,
            Transaction::single(
                "Group dice",
                Command::GroupObjects {
                    object_ids: vec![ids[0].clone(), ids[1].clone()],
                },
            ),
        )
        .expect("group objects");
    assert_eq!(
        session
            .execute(
                grouped.revision,
                Transaction::single(
                    "Legacy reorder",
                    Command::ReorderObject {
                        object_id: ids[0].clone(),
                        new_index: 2,
                    }
                )
            )
            .expect_err("raw reorder must not split a group"),
        CommandError::GroupedObjectRequiresArrange(ids[0].clone())
    );
    let locked = session
        .execute(
            grouped.revision,
            Transaction::single(
                "Lock sibling",
                Command::SetObjectLock {
                    object_ids: vec![ids[1].clone()],
                    locked: true,
                },
            ),
        )
        .expect("lock one group member");
    assert_eq!(
        session
            .execute(
                locked.revision,
                Transaction::single(
                    "Delete member",
                    Command::DeleteObject {
                        object_id: ids[0].clone(),
                    }
                )
            )
            .expect_err("deletion would indirectly mutate the locked sibling"),
        CommandError::ObjectLocked(ids[1].clone())
    );
    assert_eq!(session.revision(), locked.revision);
}
use document_core::model::{
    FontFaceBinding, ImageContentKind, ImageCrop, Insets, ObjectDraft, ObjectId, ObjectKind,
    PhysicalSize, Revision, TableData, TextFrame, TextStyle, Transform, UntaggedRasterPolicy,
    WrapMode,
};

#[test]
fn grouping_is_stable_undoable_and_remapped_when_a_page_is_duplicated() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Build dice".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle("Die", 10_000, 20_000),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle("Pip", 15_000, 25_000),
                    },
                ],
            },
        )
        .expect("add group candidates");
    let object_ids = added.created_objects.clone();
    let grouped = session
        .execute(
            added.revision,
            Transaction::single(
                "Group dice",
                Command::GroupObjects {
                    object_ids: object_ids.clone(),
                },
            ),
        )
        .expect("group objects");
    let source_group = session.document().pages[0].objects[0]
        .group_id
        .clone()
        .expect("group id");
    assert_eq!(
        session.document().pages[0].objects[1].group_id.as_ref(),
        Some(&source_group)
    );

    let duplicated = session
        .execute(
            grouped.revision,
            Transaction::single(
                "Duplicate grouped page",
                Command::DuplicatePage {
                    page_id: page_id.clone(),
                },
            ),
        )
        .expect("duplicate grouped page");
    let duplicate_group = session.document().pages[1].objects[0]
        .group_id
        .clone()
        .expect("duplicate group id");
    assert_ne!(duplicate_group, source_group);
    assert_eq!(
        session.document().pages[1].objects[1].group_id.as_ref(),
        Some(&duplicate_group)
    );

    let ungrouped = session
        .execute(
            duplicated.revision,
            Transaction::single(
                "Ungroup dice",
                Command::UngroupObjects {
                    object_ids: vec![object_ids[0].clone()],
                },
            ),
        )
        .expect("ungroup through one member");
    assert!(
        session.document().pages[0]
            .objects
            .iter()
            .all(|object| object.group_id.is_none())
    );

    session.undo(ungrouped.revision).expect("undo ungroup");
    assert!(
        session.document().pages[0]
            .objects
            .iter()
            .all(|object| object.group_id.as_ref() == Some(&source_group))
    );
}

#[test]
fn grouped_members_move_between_page_and_pasteboard_in_one_transaction() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add alternatives".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle("One", 10_000, 20_000),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: rectangle("Two", 40_000, 20_000),
                    },
                ],
            },
        )
        .expect("add objects");
    let grouped = session
        .execute(
            added.revision,
            Transaction::single(
                "Group alternatives",
                Command::GroupObjects {
                    object_ids: added.created_objects.clone(),
                },
            ),
        )
        .expect("group objects");
    let moved = session
        .execute(
            grouped.revision,
            Transaction {
                label: "Move group to pasteboard".to_owned(),
                commands: added
                    .created_objects
                    .iter()
                    .enumerate()
                    .map(|(index, object_id)| Command::MoveObject {
                        object_id: object_id.clone(),
                        destination: ObjectLocation::Pasteboard,
                        transform: Transform::identity_at(
                            -40_000
                                + i64::try_from(index).expect("test group index fits i64") * 30_000,
                            20_000,
                        ),
                    })
                    .collect(),
            },
        )
        .expect("move complete group atomically");
    assert!(session.document().pages[0].objects.is_empty());
    let group_id = session.document().pasteboard.objects[0]
        .group_id
        .clone()
        .expect("group id retained");
    assert!(
        session
            .document()
            .pasteboard
            .objects
            .iter()
            .all(|object| object.group_id.as_ref() == Some(&group_id))
    );

    let deleted = session
        .execute(
            moved.revision,
            Transaction::single(
                "Delete one grouped member",
                Command::DeleteObject {
                    object_id: added.created_objects[0].clone(),
                },
            ),
        )
        .expect("delete one grouped member");
    assert_eq!(session.document().pasteboard.objects.len(), 1);
    assert!(session.document().pasteboard.objects[0].group_id.is_none());
    session
        .undo(deleted.revision)
        .expect("undo grouped deletion");
    assert!(
        session
            .document()
            .pasteboard
            .objects
            .iter()
            .all(|object| object.group_id.as_ref() == Some(&group_id))
    );
}

#[test]
fn cloned_sessions_keep_independent_edits_and_history() {
    let mut original = DocumentSession::new(letter_document()).expect("valid session");
    let initial = original.document().clone();
    original
        .execute(
            original.revision(),
            Transaction::single(
                "Add shared object",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: rectangle("Shared", 1_000, 2_000),
                },
            ),
        )
        .expect("add object");
    let shared = original.document().clone();
    let original_revision = original.revision();
    let mut branch = original.clone();
    branch.undo(branch.revision()).expect("undo on branch");
    assert_eq!(branch.document(), &initial);
    assert_eq!(original.document(), &shared);
    assert_eq!(original.revision(), original_revision);

    // Clone a session with redo history, then abandon that history in only one branch.
    let mut redo_branch = branch.clone();
    branch
        .execute(
            branch.revision(),
            Transaction::single(
                "Add branch object",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: rectangle("Branch", 3_000, 4_000),
                },
            ),
        )
        .expect("independent edit");
    let branch_document = branch.document().clone();
    assert!(!branch.can_redo());
    redo_branch
        .redo(redo_branch.revision())
        .expect("other branch retains redo");
    assert_eq!(redo_branch.document(), &shared);
    assert_eq!(original.document(), &shared);
    assert_eq!(branch.document(), &branch_document);

    original.undo(original.revision()).expect("original undo");
    assert_eq!(original.document(), &initial);
    assert_eq!(redo_branch.document(), &shared);
    branch.undo(branch.revision()).expect("branch undo");
    assert_eq!(branch.document(), &initial);
    branch.redo(branch.revision()).expect("branch redo");
    assert_eq!(branch.document(), &branch_document);
}

#[test]
fn rejected_admission_preserves_shared_document_and_both_history_stacks() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Add object",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: rectangle("First", 1_000, 2_000),
                },
            ),
        )
        .expect("first edit");
    let first = session.document().clone();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Add second object",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: rectangle("Second", 3_000, 4_000),
                },
            ),
        )
        .expect("second edit");
    let second = session.document().clone();
    session
        .undo(session.revision())
        .expect("retain both stacks");
    let untouched = session.clone();
    let revision = session.revision();
    let rejection = CommandError::Saveability("Test resource rejection".to_owned());
    assert_eq!(
        session.execute_checked(
            revision,
            Transaction::single(
                "Rejected edit",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: rectangle("Rejected", 5_000, 6_000),
                },
            ),
            |candidate, _| {
                assert_eq!(candidate.pasteboard.objects.len(), 2);
                Err(rejection.clone())
            },
        ),
        Err(rejection.clone())
    );
    assert_eq!(
        session.undo_checked(revision, |_, _| Err(rejection.clone())),
        Err(rejection.clone())
    );
    assert_eq!(
        session.redo_checked(revision, |_, _| Err(rejection.clone())),
        Err(rejection)
    );
    assert_eq!(session.document(), &first);
    assert_eq!(session.revision(), revision);
    assert_eq!(untouched.document(), &first);
    assert_eq!(untouched.revision(), revision);
    assert!(session.can_undo());
    assert!(session.can_redo());
    let redone = session.redo(revision).expect("redo survives rejection");
    assert_eq!(redone.value(), revision.value() + 1);
    assert_eq!(session.document(), &second);
    session.undo(redone).expect("undo second edit");
    assert_eq!(session.document(), &first);
    session.undo(session.revision()).expect("undo first edit");
    assert!(session.document().pasteboard.objects.is_empty());
    assert_eq!(untouched.document(), &first);
}

#[test]
fn gesture_transaction_undoes_and_redoes_as_one_revision_checked_change() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let first_page = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add candidate",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: rectangle("Candidate cover", 1_000, 2_000),
                },
            ),
        )
        .expect("add object");
    let object_id = added.created_objects[0].clone();
    let before_drag = session.document().clone();

    let drag = Transaction {
        label: "Move onto page".to_owned(),
        commands: vec![Command::MoveObject {
            object_id: object_id.clone(),
            destination: ObjectLocation::Page(first_page),
            transform: Transform::identity_at(40_000, 50_000),
        }],
    };
    let moved = session
        .execute(added.revision, drag)
        .expect("commit one drag transaction");
    let after_drag = session.document().clone();
    assert_eq!(session.document().pasteboard.objects.len(), 0);
    assert_eq!(session.document().pages[0].objects[0].id, object_id);

    let undo_revision = session.undo(moved.revision).expect("undo drag");
    assert_eq!(session.document(), &before_drag);
    assert_eq!(undo_revision.value(), moved.revision.value() + 1);

    let redo_revision = session.redo(undo_revision).expect("redo drag");
    assert_eq!(session.document(), &after_drag);
    assert_eq!(redo_revision.value(), undo_revision.value() + 1);
}

#[test]
fn invalid_multi_command_transaction_is_atomic() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let before = session.document().clone();
    let missing_page = document_core::PageId::parse("page-does-not-exist").expect("valid id");

    let error = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Atomic edit".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Pasteboard,
                        object: rectangle("Would otherwise be added", 0, 0),
                    },
                    Command::DeletePage {
                        page_id: missing_page.clone(),
                    },
                ],
            },
        )
        .expect_err("missing page rejects transaction");

    assert_eq!(error, CommandError::PageNotFound(missing_page));
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), Revision::initial());
}

#[test]
fn page_commands_preserve_stable_order_and_reject_stale_callers() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let first_page = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add page",
                Command::AddPage {
                    after: Some(first_page.clone()),
                    trim: PhysicalSize::new(148_000, 210_000).expect("positive A5 size"),
                    bleed: Insets::uniform(4_000).expect("valid bleed"),
                },
            ),
        )
        .expect("add page");
    let second_page = added.created_pages[0].clone();

    let stale = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Stale reorder",
                Command::ReorderPage {
                    page_id: second_page.clone(),
                    new_index: 0,
                },
            ),
        )
        .expect_err("stale expected revision");
    assert_eq!(
        stale,
        CommandError::RevisionConflict {
            expected: Revision::initial(),
            actual: added.revision,
        }
    );

    session
        .execute(
            added.revision,
            Transaction::single(
                "Reorder page",
                Command::ReorderPage {
                    page_id: second_page.clone(),
                    new_index: 0,
                },
            ),
        )
        .expect("reorder page");
    assert_eq!(session.document().pages[0].id, second_page);
    assert_eq!(session.document().pages[1].id, first_page);
}

#[test]
fn duplicate_allocates_new_ids_and_delete_preserves_intentional_blank_pages() {
    use document_core::model::{GuideAxis, Micrometres};

    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let first_page = session.document().pages[0].id.clone();
    let with_object = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add editable page data".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(first_page.clone()),
                        object: rectangle("Original", 10_000, 20_000),
                    },
                    Command::AddGuide {
                        page_id: first_page.clone(),
                        axis: GuideAxis::Horizontal,
                        position: Micrometres::new(25_400),
                    },
                ],
            },
        )
        .expect("add object");
    let original_object = with_object.created_objects[0].clone();
    let duplicated = session
        .execute(
            with_object.revision,
            Transaction::single(
                "Duplicate page",
                Command::DuplicatePage {
                    page_id: first_page.clone(),
                },
            ),
        )
        .expect("duplicate page");
    let duplicate_page = duplicated.created_pages[0].clone();
    assert_ne!(duplicate_page, first_page);
    assert_ne!(duplicated.created_objects[0], original_object);
    assert_eq!(session.document().pages[1].objects[0].name, "Original");
    assert_ne!(
        session.document().pages[1].guides[0].id,
        session.document().pages[0].guides[0].id
    );

    let trim = session.document().pages[0].trim;
    let bleed = session.document().pages[0].bleed;
    let blank = session
        .execute(
            duplicated.revision,
            Transaction::single(
                "Add intentional blank",
                Command::AddPage {
                    after: Some(duplicate_page.clone()),
                    trim,
                    bleed,
                },
            ),
        )
        .expect("add blank page");
    let blank_id = blank.created_pages[0].clone();
    session
        .execute(
            blank.revision,
            Transaction::single(
                "Delete duplicate",
                Command::DeletePage {
                    page_id: duplicate_page,
                },
            ),
        )
        .expect("delete duplicate");
    assert_eq!(session.document().pages.len(), 2);
    assert_eq!(session.document().pages[1].id, blank_id);
    assert!(session.document().pages[1].objects.is_empty());
}

#[test]
fn deleting_the_final_page_clears_unlocked_content_and_guides_as_one_undoable_change() {
    use document_core::model::{GuideAxis, Micrometres};

    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let populated = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Populate final page".to_owned(),
                commands: vec![
                    Command::AddGuide {
                        page_id: page_id.clone(),
                        axis: GuideAxis::Vertical,
                        position: Micrometres::new(25_400),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle("Last object", 10_000, 20_000),
                    },
                ],
            },
        )
        .expect("populate final page");
    let object_id = populated.created_objects[0].clone();
    let locked = session
        .execute(
            populated.revision,
            Transaction::single(
                "Lock final-page object",
                Command::SetObjectLock {
                    object_ids: vec![object_id.clone()],
                    locked: true,
                },
            ),
        )
        .expect("lock final-page object");
    let locked_document = session.document().clone();
    assert_eq!(
        session
            .execute(
                locked.revision,
                Transaction::single(
                    "Refuse locked final-page clear",
                    Command::DeletePage {
                        page_id: page_id.clone(),
                    },
                ),
            )
            .expect_err("locked page content blocks clearing"),
        CommandError::ObjectLocked(object_id.clone())
    );
    assert_eq!(session.document(), &locked_document);
    assert_eq!(session.revision(), locked.revision);
    let unlocked = session
        .execute(
            locked.revision,
            Transaction::single(
                "Unlock final-page object",
                Command::SetObjectLock {
                    object_ids: vec![object_id],
                    locked: false,
                },
            ),
        )
        .expect("unlock final-page object");
    let before_clear = session.document().clone();

    let cleared = session
        .execute(
            unlocked.revision,
            Transaction::single(
                "Clear final page",
                Command::DeletePage {
                    page_id: page_id.clone(),
                },
            ),
        )
        .expect("clear rather than remove final page");
    assert_eq!(session.document().pages.len(), 1);
    assert_eq!(session.document().pages[0].id, page_id);
    assert!(session.document().pages[0].objects.is_empty());
    assert!(session.document().pages[0].guides.is_empty());

    session
        .undo(cleared.revision)
        .expect("undo final-page clear");
    assert_eq!(session.document(), &before_clear);
}

#[test]
fn object_layer_order_is_container_scoped_and_undoable() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add overlapping shapes".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle("Back", 10_000, 20_000),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: rectangle("Front", 20_000, 30_000),
                    },
                ],
            },
        )
        .expect("add shapes");
    let back_id = added.created_objects[0].clone();
    let front_id = added.created_objects[1].clone();
    let reordered = session
        .execute(
            added.revision,
            Transaction::single(
                "Bring back forward",
                Command::ReorderObject {
                    object_id: back_id.clone(),
                    new_index: 1,
                },
            ),
        )
        .expect("reorder layer");
    assert_eq!(session.document().pages[0].objects[0].id, front_id);
    assert_eq!(session.document().pages[0].objects[1].id, back_id.clone());
    session.undo(reordered.revision).expect("undo reorder");
    assert_eq!(session.document().pages[0].objects[0].id, back_id);
}

#[test]
fn rectangular_wrap_is_canonical_and_undoable_for_shapes() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add wrapping shape",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: rectangle("Callout", 10_000, 20_000),
                },
            ),
        )
        .expect("add shape");
    let object_id = added.created_objects[0].clone();

    let wrapped = session
        .execute(
            added.revision,
            Transaction::single(
                "Wrap text around shape",
                Command::UpdateWrapMode {
                    object_id: object_id.clone(),
                    wrap_mode: WrapMode::Rectangular,
                },
            ),
        )
        .expect("set wrap mode");
    assert!(matches!(
        &session.document().pages[0].objects[0].kind,
        ObjectKind::Shape {
            wrap_mode: WrapMode::Rectangular,
            ..
        }
    ));

    session.undo(wrapped.revision).expect("undo wrap mode");
    assert!(matches!(
        &session.document().pages[0].objects[0].kind,
        ObjectKind::Shape {
            wrap_mode: WrapMode::None,
            ..
        }
    ));
}

#[test]
fn text_style_edits_preserve_or_clear_exact_font_binding_deliberately() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let source = AssetHash::for_bytes(b"embedded face bytes");
    let original = TextStyle {
        font_face: Some(FontFaceBinding {
            source: source.clone(),
            face_index: 2,
        }),
        ..TextStyle::default()
    };
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add bound text",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Bound text".to_owned(),
                        size: PhysicalSize::new(10_000, 10_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Bound".to_owned(),
                            style: original.clone(),
                            frame: TextFrame::default(),
                            story: document_core::model::TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .expect("add bound text");
    let object_id = added.created_objects[0].clone();

    let bold = TextStyle {
        font_weight: 700,
        ..original.clone()
    };
    let styled = session
        .execute(
            added.revision,
            Transaction::single(
                "Make bold",
                Command::UpdateTextStyle {
                    object_id: object_id.clone(),
                    style: bold,
                    font_face_update: FontFaceBindingUpdate::PreserveIfFamilyUnchanged,
                },
            ),
        )
        .expect("retain the prior exact face under its original style key");
    assert!(matches!(
        &session.document().pasteboard.objects[0].kind,
        ObjectKind::Text { style, .. }
            if style.font_face.is_none()
                && style.exact_font_face_for(700, false).is_none()
                && style.exact_font_face_for(400, false).is_some_and(|binding| binding.source == source && binding.face_index == 2)
    ));

    let changed_family = TextStyle {
        font_family: "Different family".to_owned(),
        ..original
    };
    session
        .execute(
            styled.revision,
            Transaction::single(
                "Change family",
                Command::UpdateTextStyle {
                    object_id,
                    style: changed_family,
                    font_face_update: FontFaceBindingUpdate::PreserveIfFamilyUnchanged,
                },
            ),
        )
        .expect("clear stale face on family change");
    assert!(matches!(
        &session.document().pasteboard.objects[0].kind,
        ObjectKind::Text { style, .. }
            if style.font_face.is_none() && style.exact_font_variants.is_empty()
    ));
}

#[test]
fn table_style_edits_and_resizing_preserve_exact_font_binding_and_undo() {
    let source = AssetHash::for_bytes(b"exact table font");
    let mut table = TableData::empty(2, 2).unwrap();
    table.style.font_face = Some(FontFaceBinding {
        source: source.clone(),
        face_index: 1,
    });
    table.style.font_family = "Bound table family".into();
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add bound table",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Word search".to_owned(),
                        size: PhysicalSize::new(40_000, 40_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();
    let object_id = added.created_objects[0].clone();
    let styled = session
        .execute(
            added.revision,
            Transaction::single(
                "Make table bold",
                Command::UpdateTableTextStyle {
                    object_id: object_id.clone(),
                    style: TextStyle {
                        font_family: "Bound table family".to_owned(),
                        font_weight: 700,
                        ..TextStyle::default()
                    },
                    font_face_update: FontFaceBindingUpdate::PreserveIfFamilyUnchanged,
                },
            ),
        )
        .unwrap();
    let resized = session
        .execute(
            styled.revision,
            Transaction::single(
                "Resize table",
                Command::UpdateTableDimensions {
                    object_id,
                    rows: 3,
                    columns: 3,
                },
            ),
        )
        .unwrap();
    assert!(matches!(
        &session.document().pasteboard.objects[0].kind,
        ObjectKind::Table { table }
            if table.rows == 3
                && table.style.font_weight == 700
                && table.style.font_face.is_none()
                && table.style.exact_font_face_for(700, false).is_none()
                && table.style.exact_font_face_for(400, false).is_some_and(|face| face.source == source)
    ));
    session.undo(resized.revision).unwrap();
    let undone = session.undo(session.revision()).unwrap();
    assert!(matches!(
        &session.document().pasteboard.objects[0].kind,
        ObjectKind::Table { table }
            if table.rows == 2
                && table.style.font_weight == 400
                && table.style.font_face.as_ref().is_some_and(|face| face.source == source)
                && table.style.exact_font_variants.is_empty()
    ));
    assert_eq!(undone, session.revision());
}

#[test]
fn picture_content_kind_is_canonical_and_undoable() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add picture",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Coloring art".into(),
                        size: PhysicalSize::new(40_000, 40_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Image {
                            asset: AssetHash::for_bytes(b"line art"),
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
    let changed = session
        .execute(
            added.revision,
            Transaction::single(
                "Treat picture as line art",
                Command::UpdateImageContentKind {
                    object_id: added.created_objects[0].clone(),
                    content_kind: ImageContentKind::LineArt,
                },
            ),
        )
        .unwrap();
    assert!(matches!(
        session.document().pasteboard.objects[0].kind,
        ObjectKind::Image {
            content_kind: ImageContentKind::LineArt,
            ..
        }
    ));

    session.undo(changed.revision).unwrap();
    assert!(matches!(
        session.document().pasteboard.objects[0].kind,
        ObjectKind::Image {
            content_kind: ImageContentKind::ContinuousTone,
            ..
        }
    ));
}

#[test]
fn picture_crop_and_aspect_lock_are_canonical_undoable_and_lock_safe() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add picture",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Cover picture".into(),
                        size: PhysicalSize::new(40_000, 30_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Image {
                            asset: AssetHash::for_bytes(b"cover"),
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
    let object_id = added.created_objects[0].clone();
    let crop = ImageCrop::new(100_000, 50_000, 200_000, 150_000).unwrap();
    let changed = session
        .execute(
            added.revision,
            Transaction {
                label: "Crop picture".into(),
                commands: vec![
                    Command::UpdateImageCrop {
                        object_id: object_id.clone(),
                        crop,
                    },
                    Command::SetImageAspectLock {
                        object_id: object_id.clone(),
                        locked: false,
                    },
                ],
            },
        )
        .unwrap();
    assert!(matches!(
        session.document().pasteboard.objects[0].kind,
        ObjectKind::Image { crop: current, lock_aspect_ratio: false, .. } if current == crop
    ));
    session.undo(changed.revision).unwrap();
    assert!(matches!(
        session.document().pasteboard.objects[0].kind,
        ObjectKind::Image { crop, lock_aspect_ratio: true, .. } if crop == ImageCrop::default()
    ));

    let locked = session
        .execute(
            session.revision(),
            Transaction::single(
                "Lock picture",
                Command::SetObjectLock {
                    object_ids: vec![object_id.clone()],
                    locked: true,
                },
            ),
        )
        .unwrap();
    assert!(matches!(
        session.execute(
            locked.revision,
            Transaction::single(
                "Crop locked picture",
                Command::UpdateImageCrop { object_id, crop },
            ),
        ),
        Err(CommandError::ObjectLocked(_))
    ));
    assert_eq!(session.revision(), locked.revision);
}

#[test]
fn untagged_raster_policy_is_explicit_and_undoable() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    assert_eq!(
        session.document().color_settings.untagged_raster_policy,
        UntaggedRasterPolicy::RequireEmbeddedProfile
    );
    let changed = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Assume sRGB for untagged pictures",
                Command::UpdateUntaggedRasterPolicy {
                    policy: UntaggedRasterPolicy::AssumeSrgb,
                },
            ),
        )
        .unwrap();
    assert_eq!(
        session.document().color_settings.untagged_raster_policy,
        UntaggedRasterPolicy::AssumeSrgb
    );
    session.undo(changed.revision).unwrap();
    assert_eq!(
        session.document().color_settings.untagged_raster_policy,
        UntaggedRasterPolicy::RequireEmbeddedProfile
    );
}
