mod common;

use common::{letter_document, rectangle};
use document_core::commands::{Command, DocumentSession, ObjectLocation, Transaction};
use document_core::geometry::{
    PhysicalBounds, resize_transform_in_bounds, rotate_transform_in_bounds, transformed_bounds,
};
use document_core::model::{
    CornerRounding, Micrometres, PhysicalSize, Revision, ShapeKind, Transform,
};
use document_core::shape_geometry::{ShapePathCommand, normalized_corner_radii, shape_outline};

#[test]
fn independent_resize_preserves_negative_near_quarter_turn_transforms_and_round_trips() {
    let size = PhysicalSize::new(32_000, 18_000).unwrap();
    for angle in [0, 45_000_000, 89_999_999, 90_000_000, -90_000_001] {
        let original = Transform {
            rotation_microdegrees: angle,
            scale_x_nanos: -1_200_000_000,
            scale_y_nanos: 850_000_000,
            shear_x_nanos: 200_000_000,
            shear_y_nanos: -150_000_000,
            pivot_x: Micrometres::new(8_000),
            pivot_y: Micrometres::new(5_000),
            ..Transform::identity_at(45_000, 52_000)
        };
        let source = transformed_bounds(size, original).unwrap();
        let target = PhysicalBounds {
            x: Micrometres::new(-20_000),
            y: Micrometres::new(12_000),
            width: Micrometres::new(source.width.value() * 3 / 2),
            height: Micrometres::new(source.height.value() * 2 / 3),
        };
        let resized = resize_transform_in_bounds(original, size, source, target).unwrap();
        assert_eq!(
            resized.rotation_microdegrees,
            original.rotation_microdegrees
        );
        assert_eq!(resized.pivot_x, original.pivot_x);
        assert_bounds_close(transformed_bounds(size, resized).unwrap(), target, 3);
        let restored = resize_transform_in_bounds(resized, size, target, source).unwrap();
        assert_bounds_close(transformed_bounds(size, restored).unwrap(), source, 3);
        assert!((restored.scale_x_nanos - original.scale_x_nanos).abs() <= 2);
        assert!((restored.shear_x_nanos - original.shear_x_nanos).abs() <= 2);
        let rotated = rotate_transform_in_bounds(resized, size, target, 90_000_000).unwrap();
        assert_eq!(rotated.scale_x_nanos, resized.scale_x_nanos);
        assert_eq!(rotated.shear_x_nanos, resized.shear_x_nanos);
        let round_trip = rotate_transform_in_bounds(rotated, size, target, -90_000_000).unwrap();
        assert_bounds_close(
            transformed_bounds(size, round_trip).unwrap(),
            transformed_bounds(size, resized).unwrap(),
            2,
        );
    }
    let singular_decomposition = Transform {
        rotation_microdegrees: 45_000_000,
        shear_y_nanos: -3_000_000_000,
        ..Transform::identity_at(0, 0)
    };
    let source = transformed_bounds(size, singular_decomposition).unwrap();
    let target = PhysicalBounds {
        height: Micrometres::new(source.height.value() * 2),
        ..source
    };
    assert!(
        resize_transform_in_bounds(singular_decomposition, size, source, target).is_err(),
        "a zero decomposed axis cannot silently discard the authored transform"
    );
}

fn assert_bounds_close(actual: PhysicalBounds, expected: PhysicalBounds, tolerance: i64) {
    for (a, b) in [
        (actual.x, expected.x),
        (actual.y, expected.y),
        (actual.width, expected.width),
        (actual.height, expected.height),
    ] {
        assert!(
            (a.value() - b.value()).abs() <= tolerance,
            "{actual:?} differs from {expected:?}"
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One durable corner fixture covers normalization and reversible edits.
fn requested_corners_normalize_proportionally_and_survive_resize_undo_and_copy() {
    let requested = CornerRounding {
        top_left_um: 30_000,
        top_right_um: 10_000,
        bottom_right_um: 20_000,
        bottom_left_um: 40_000,
        linked: false,
    };
    assert_eq!(
        normalized_corner_radii(PhysicalSize::new(100_000, 35_000).unwrap(), requested).unwrap(),
        [15_000, 5_000, 10_000, 20_000]
    );
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Rectangle",
                Command::AddObject {
                    location: ObjectLocation::Page(page),
                    object: rectangle("Rounded", 10_000, 12_000),
                },
            ),
        )
        .unwrap();
    let id = session.document().pages[0].objects[0].id.clone();
    assert!(
        !serde_json::to_value(session.document())
            .unwrap()
            .to_string()
            .contains("corner_rounding")
    );
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Corners",
                Command::UpdateCornerRounding {
                    object_id: id.clone(),
                    rounding: requested,
                },
            ),
        )
        .unwrap();
    let before = session.document().clone();
    for invalid in [
        CornerRounding {
            top_left_um: -1,
            ..requested
        },
        CornerRounding {
            linked: true,
            ..requested
        },
    ] {
        assert!(
            session
                .execute(
                    session.revision(),
                    Transaction::single(
                        "Invalid corners",
                        Command::UpdateCornerRounding {
                            object_id: id.clone(),
                            rounding: invalid
                        }
                    )
                )
                .is_err()
        );
        assert_eq!(session.document(), &before);
    }
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Resize",
                Command::ResizeObject {
                    object_id: id.clone(),
                    size: PhysicalSize::new(60_000, 35_000).unwrap(),
                },
            ),
        )
        .unwrap();
    let object = &session.document().pages[0].objects[0];
    assert_eq!(object.corner_rounding, requested);
    assert!(
        shape_outline(ShapeKind::Rectangle, object.size, requested)
            .unwrap()
            .iter()
            .any(|command| matches!(command, ShapePathCommand::CubicTo(..)))
    );
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &before);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Copy",
                Command::DuplicateObject {
                    object_id: id,
                    offset_x_um: 100_000,
                    offset_y_um: 0,
                },
            ),
        )
        .unwrap();
    assert!(
        session.document().pages[0]
            .objects
            .iter()
            .all(|object| object.corner_rounding == requested)
    );
}

#[test]
#[allow(clippy::too_many_lines)] // Whole-group movement, destination changes and failure share one preserved fixture.
fn move_and_delete_expand_whole_groups_and_reject_partial_matrix_updates() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Composition".into(),
                commands: [
                    rectangle("A", 10_000, 10_000),
                    rectangle("B", 45_000, 15_000),
                ]
                .into_iter()
                .map(|object| Command::AddObject {
                    location: ObjectLocation::Page(page.clone()),
                    object,
                })
                .collect(),
            },
        )
        .unwrap();
    let ids = session.document().pages[0]
        .objects
        .iter()
        .map(|object| object.id.clone())
        .collect::<Vec<_>>();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Group",
                Command::GroupObjects {
                    object_ids: ids.clone(),
                },
            ),
        )
        .unwrap();
    let grouped = session.document().clone();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Move group",
                Command::MoveSelection {
                    object_ids: vec![ids[0].clone()],
                    destination: None,
                    delta_x_um: -3_000,
                    delta_y_um: 5_000,
                },
            ),
        )
        .unwrap();
    for (original, moved) in grouped.pages[0]
        .objects
        .iter()
        .zip(&session.document().pages[0].objects)
    {
        assert_eq!(
            moved.transform.translation_x.value(),
            original.transform.translation_x.value() - 3_000
        );
        assert_eq!(moved.kind, original.kind);
    }
    let moved = session.document().clone();
    assert!(
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Partial matrix",
                    Command::ApplySelectionTransforms {
                        objects: vec![document_core::commands::ObjectTransformUpdate {
                            object_id: ids[0].clone(),
                            transform: Transform::identity_at(0, 0)
                        }]
                    }
                )
            )
            .is_err()
    );
    assert_eq!(session.document(), &moved);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Group to pasteboard",
                Command::MoveSelection {
                    object_ids: vec![ids[0].clone()],
                    destination: Some(ObjectLocation::Pasteboard),
                    delta_x_um: 100_000,
                    delta_y_um: -10_000,
                },
            ),
        )
        .unwrap();
    assert!(session.document().pages[0].objects.is_empty());
    assert_eq!(session.document().pasteboard.objects.len(), 2);
    assert!(
        session
            .document()
            .pasteboard
            .objects
            .iter()
            .all(|object| object.group_id == moved.pages[0].objects[0].group_id)
    );
    let group_on_pasteboard = session.document().clone();
    assert!(
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Invalid destination",
                    Command::MoveSelection {
                        object_ids: vec![ids[1].clone()],
                        destination: Some(ObjectLocation::Page(
                            document_core::model::PageId::parse("missing-page").unwrap()
                        )),
                        delta_x_um: 0,
                        delta_y_um: 0
                    }
                )
            )
            .is_err()
    );
    assert_eq!(session.document(), &group_on_pasteboard);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Group back to page",
                Command::MoveSelection {
                    object_ids: vec![ids[1].clone()],
                    destination: Some(ObjectLocation::Page(moved.pages[0].id.clone())),
                    delta_x_um: -100_000,
                    delta_y_um: 10_000,
                },
            ),
        )
        .unwrap();
    assert_eq!(session.document(), &moved);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Delete group",
                Command::DeleteSelection {
                    object_ids: vec![ids[1].clone()],
                },
            ),
        )
        .unwrap();
    assert!(session.document().pages[0].objects.is_empty());
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &moved);
}

#[test]
#[allow(clippy::too_many_lines)] // One stacking fixture keeps mixed group order and lock admission together.
fn multi_selection_layers_group_units_once_and_preserves_relative_stacking_and_locks() {
    use document_core::commands::ArrangeOperation;
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Stack".into(),
                commands: ["Group A", "Group B", "Other X", "Selected C", "Other Y"]
                    .iter()
                    .map(|name| Command::AddObject {
                        location: ObjectLocation::Page(page.clone()),
                        object: rectangle(name, 10_000, 15_000),
                    })
                    .collect(),
            },
        )
        .unwrap();
    let ids = session.document().pages[0]
        .objects
        .iter()
        .map(|object| object.id.clone())
        .collect::<Vec<_>>();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Group",
                Command::GroupObjects {
                    object_ids: ids[..2].to_vec(),
                },
            ),
        )
        .unwrap();
    let before = session.document().clone();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Bring forward",
                Command::ArrangeSelection {
                    object_ids: vec![ids[0].clone(), ids[1].clone(), ids[3].clone()],
                    operation: ArrangeOperation::BringForward,
                },
            ),
        )
        .unwrap();
    assert_eq!(
        session.document().pages[0]
            .objects
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>(),
        ["Other X", "Group A", "Group B", "Other Y", "Selected C"]
    );
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &before);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Bring front",
                Command::ArrangeSelection {
                    object_ids: vec![ids[0].clone(), ids[3].clone()],
                    operation: ArrangeOperation::BringToFront,
                },
            ),
        )
        .unwrap();
    assert_eq!(
        session.document().pages[0]
            .objects
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>(),
        ["Other X", "Other Y", "Group A", "Group B", "Selected C"]
    );
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Lock group",
                Command::SetObjectLock {
                    object_ids: vec![ids[1].clone()],
                    locked: true,
                },
            ),
        )
        .unwrap();
    let locked = session.document().clone();
    assert!(
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Layer locked selection",
                    Command::ArrangeSelection {
                        object_ids: vec![ids[0].clone(), ids[3].clone()],
                        operation: ArrangeOperation::SendToBack
                    }
                )
            )
            .is_err()
    );
    assert_eq!(session.document(), &locked);
}
