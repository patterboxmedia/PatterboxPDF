mod common;
use common::{TempDirectory, letter_document};
use document_core::model::{
    CornerRounding, ImageContentKind, ImageCrop, ImageFlip, Insets, ModelError, ObjectDraft,
    ObjectKind, PhysicalSize, TableData, TextFrame, TextStoryFormatting, TextStyle, Transform,
    WrapMode,
};
use document_core::shape_geometry::{
    fit_rounded_text_size, normalized_corner_radii, rounded_interior_insets,
};
use document_core::{
    AssetStore, Command, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
    inspect_raster, open_package, save_new_package,
};

const PNG: &[u8] = include_bytes!("../../raster-boundary/tests/fixtures/valid-3x2.png");

fn corners(radius: i64) -> CornerRounding {
    CornerRounding {
        top_left_um: radius,
        top_right_um: radius,
        bottom_right_um: radius,
        bottom_left_um: radius,
        linked: true,
    }
}
fn session() -> DocumentSession {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let mut table = TableData::empty(2, 2).unwrap();
    table.cells = vec![
        "first\nline".into(),
        "second".into(),
        "third".into(),
        "last".into(),
    ];
    let kinds = [
        ObjectKind::Text {
            text: "preserved text".into(),
            style: TextStyle::default(),
            frame: TextFrame::default(),
            story: TextStoryFormatting::default(),
        },
        ObjectKind::Table { table },
        ObjectKind::Image {
            asset: document_core::AssetHash::for_bytes(PNG),
            content_kind: ImageContentKind::ContinuousTone,
            crop: ImageCrop::new(100_000, 0, 200_000, 0).unwrap(),
            flip: ImageFlip {
                horizontal: true,
                vertical: false,
            },
            lock_aspect_ratio: true,
            wrap_mode: WrapMode::None,
        },
    ];
    let page = session.document().pages[0].id.clone();
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Compose".into(),
                commands: kinds
                    .into_iter()
                    .map(|kind| Command::AddObject {
                        location: ObjectLocation::Page(page.clone()),
                        object: ObjectDraft {
                            name: "Object".into(),
                            size: PhysicalSize::new(60_000, 40_000).unwrap(),
                            transform: Transform::identity_at(10_000, 10_000),
                            opacity_millionths: 1_000_000,
                            kind,
                        },
                    })
                    .collect(),
            },
        )
        .unwrap();
    session
}

#[test]
fn common_corners_preserve_all_source_appearance_and_round_trip_one_undo() {
    let mut session = session();
    let before = session.document().clone();
    let rounding = CornerRounding {
        top_left_um: 40_000,
        top_right_um: 20_000,
        bottom_right_um: 30_000,
        bottom_left_um: 10_000,
        linked: false,
    };
    assert_eq!(
        normalized_corner_radii(before.pages[0].objects[0].size, rounding).unwrap(),
        [32_000, 16_000, 24_000, 8_000]
    );
    let commands = before.pages[0]
        .objects
        .iter()
        .map(|object| Command::UpdateCornerRounding {
            object_id: object.id.clone(),
            rounding,
        })
        .collect();
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Round objects".into(),
                commands,
            },
        )
        .unwrap();
    for (old, new) in before.pages[0]
        .objects
        .iter()
        .zip(&session.document().pages[0].objects)
    {
        assert_eq!(new.corner_rounding, rounding);
        assert_eq!(new.kind, old.kind);
        assert_eq!(new.transform, old.transform);
        assert_eq!(new.effects, old.effects);
        assert_eq!(new.size, old.size);
    }
    let after = session.document().clone();
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &before);
    session.redo(session.revision()).unwrap();
    assert_eq!(session.document(), &after);
    let mut assets = AssetStore::default();
    assets.insert_raster(PNG.to_vec(), inspect_raster(PNG).unwrap());
    let directory = TempDirectory::new("common-corners");
    let path = directory.path().join("corners.pubv2");
    let saved = ProjectSnapshot::from_session(&session);
    save_new_package(&path, &saved, &assets).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, saved);
}

#[test]
fn unusable_rounded_interiors_are_refused_atomically_and_fit_clearance_settles() {
    let mut session = session();
    let id = session.document().pages[0].objects[1].id.clone();
    let before = session.document().clone();
    let revision = session.revision();
    let error = session
        .execute(
            revision,
            Transaction {
                label: "Narrow tracks and corners".into(),
                commands: vec![
                    Command::UpdateCornerRounding {
                        object_id: id.clone(),
                        rounding: corners(20_000),
                    },
                    Command::UpdateTableTrackSize {
                        object_id: id,
                        axis: document_core::model::TableTrackAxis::Column,
                        index: 0,
                        weight: 1,
                    },
                ],
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        document_core::commands::CommandError::InvalidModel(ModelError::CornerInteriorDoesNotFit)
    ));
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    let text_id = before.pages[0].objects[0].id.clone();
    let refusal = session
        .execute(
            revision,
            Transaction {
                label: "Rounded columns".into(),
                commands: vec![
                    Command::UpdateCornerRounding {
                        object_id: text_id.clone(),
                        rounding: corners(20_000),
                    },
                    Command::UpdateTextFrame {
                        object_id: text_id,
                        column_count: 2,
                        column_gap_um: 55_000,
                    },
                ],
            },
        )
        .unwrap_err();
    assert!(matches!(
        refusal,
        document_core::commands::CommandError::InvalidModel(ModelError::CornerInteriorDoesNotFit)
    ));
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    let content = PhysicalSize::new(6_350, 6_350).unwrap();
    let rounding = corners(1_000_000);
    let (size, margins) =
        fit_rounded_text_size(content, rounding, Insets::uniform(0).unwrap()).unwrap();
    assert_eq!(margins, rounded_interior_insets(size, rounding).unwrap());
    assert_eq!(
        size.width.value(),
        content.width.value() + margins.left.value() + margins.right.value()
    );
    assert_eq!(
        size.height.value(),
        content.height.value() + margins.top.value() + margins.bottom.value()
    );
}
