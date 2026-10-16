mod common;

use common::{
    TempDirectory, letter_document, read_archive_entry, rectangle, replace_archive_entry,
};
use document_core::assets::{AssetHash, AssetStore};
use document_core::commands::{
    Command, CommandError, DocumentSession, ObjectLocation, Transaction,
};
use document_core::model::{
    Color, HardShadow, ImageContentKind, ImageCrop, Micrometres, ModelError, ObjectDraft,
    ObjectEffects, ObjectKind, PhysicalSize, Revision, ShapeStroke, TextFrame, TextStyle,
    Transform, WrapMode,
};
use document_core::package::{ProjectSnapshot, open_package, save_new_package};

fn red() -> Color {
    Color {
        red: 200,
        green: 20,
        blue: 30,
        alpha: 255,
    }
}

fn blue() -> Color {
    Color {
        red: 10,
        green: 40,
        blue: 220,
        alpha: 255,
    }
}

fn text(name: &str) -> ObjectDraft {
    ObjectDraft {
        name: name.to_owned(),
        size: PhysicalSize::new(100_000, 30_000).expect("positive text size"),
        transform: Transform::identity_at(10_000, 20_000),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Text {
            text: "Magic and Coloring".to_owned(),
            style: TextStyle::default(),
            frame: TextFrame::default(),
            story: document_core::model::TextStoryFormatting::default(),
        },
    }
}

fn image(name: &str) -> ObjectDraft {
    ObjectDraft {
        name: name.to_owned(),
        size: PhysicalSize::new(60_000, 40_000).expect("positive image size"),
        transform: Transform::identity_at(10_000, 20_000),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Image {
            asset: AssetHash::for_bytes(b"picture fixture"),
            wrap_mode: WrapMode::None,
            content_kind: ImageContentKind::ContinuousTone,
            crop: ImageCrop::default(),
            flip: document_core::model::ImageFlip::default(),
            lock_aspect_ratio: false,
        },
    }
}

fn shadow() -> HardShadow {
    HardShadow {
        color: blue(),
        offset_x: Micrometres::new(4_000),
        offset_y: Micrometres::new(6_000),
    }
}

fn outline() -> ShapeStroke {
    ShapeStroke {
        color: red(),
        width_millipoints: 2_000,
    }
}

#[test]
fn new_objects_default_to_no_effects_and_omit_the_empty_envelope() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add shape",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: rectangle("Plain shape", 0, 0),
                },
            ),
        )
        .expect("add shape");
    let object = &session.document().pages[0].objects[0];
    assert_eq!(object.effects, ObjectEffects::default());
    assert!(
        serde_json::to_value(object)
            .expect("serialize object")
            .get("effects")
            .is_none(),
        "disabled effects should not add package noise"
    );
}

#[test]
fn effects_are_kind_valid_atomic_and_undoable() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add effect targets".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: text("Title"),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: image("Picture"),
                    },
                ],
            },
        )
        .expect("add effect targets");
    let text_id = added.created_objects[0].clone();
    let image_id = added.created_objects[1].clone();
    let text_effects = ObjectEffects {
        shadow: Some(shadow()),
        text_outline: Some(outline()),
        picture_border: None,
    };
    let updated = session
        .execute(
            added.revision,
            Transaction::single(
                "Style title",
                Command::UpdateObjectEffects {
                    object_id: text_id.clone(),
                    effects: text_effects,
                },
            ),
        )
        .expect("apply text effects");
    assert_eq!(session.document().pages[0].objects[0].effects, text_effects);

    let undone = session.undo(updated.revision).expect("undo effects");
    assert_eq!(
        session.document().pages[0].objects[0].effects,
        ObjectEffects::default()
    );
    let redone = session.redo(undone).expect("redo effects");
    assert_eq!(session.document().pages[0].objects[0].effects, text_effects);

    let before_oversized = session.document().clone();
    let oversized = session
        .execute(
            redone,
            Transaction::single(
                "Reject oversized shadow",
                Command::UpdateObjectEffects {
                    object_id: image_id.clone(),
                    effects: ObjectEffects {
                        shadow: Some(HardShadow {
                            offset_x: Micrometres::new(HardShadow::MAX_OFFSET_MICROMETRES + 1),
                            ..shadow()
                        }),
                        picture_border: Some(outline()),
                        ..ObjectEffects::default()
                    },
                },
            ),
        )
        .expect_err("out-of-range shadow is rejected");
    assert_eq!(
        oversized,
        CommandError::InvalidModel(ModelError::HardShadowOffsetOutOfRange)
    );
    assert_eq!(session.document(), &before_oversized);
    assert_eq!(session.revision(), redone);
}

#[test]
fn text_outline_and_picture_border_accept_only_their_matching_kinds() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add effect targets".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: text("Title"),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: image("Picture"),
                    },
                ],
            },
        )
        .expect("add effect targets");
    let text_id = added.created_objects[0].clone();
    let image_id = added.created_objects[1].clone();
    let styled = session
        .execute(
            added.revision,
            Transaction {
                label: "Style matching object kinds".to_owned(),
                commands: vec![
                    Command::UpdateObjectEffects {
                        object_id: text_id.clone(),
                        effects: ObjectEffects {
                            text_outline: Some(outline()),
                            ..ObjectEffects::default()
                        },
                    },
                    Command::UpdateObjectEffects {
                        object_id: image_id.clone(),
                        effects: ObjectEffects {
                            shadow: Some(shadow()),
                            picture_border: Some(outline()),
                            ..ObjectEffects::default()
                        },
                    },
                ],
            },
        )
        .expect("matching effect kinds are accepted atomically");
    assert!(
        session.document().pages[0].objects[0]
            .effects
            .text_outline
            .is_some()
    );
    assert!(
        session.document().pages[0].objects[1]
            .effects
            .picture_border
            .is_some()
    );
    let before_invalid = session.document().clone();

    for (object_id, effects, expected) in [
        (
            text_id,
            ObjectEffects {
                picture_border: Some(outline()),
                ..ObjectEffects::default()
            },
            ModelError::PictureBorderKindMismatch,
        ),
        (
            image_id,
            ObjectEffects {
                text_outline: Some(outline()),
                ..ObjectEffects::default()
            },
            ModelError::TextOutlineKindMismatch,
        ),
    ] {
        let rejected = session
            .execute(
                styled.revision,
                Transaction::single(
                    "Reject cross-kind effect",
                    Command::UpdateObjectEffects { object_id, effects },
                ),
            )
            .expect_err("cross-kind effect is rejected");
        assert_eq!(rejected, CommandError::InvalidModel(expected));
        assert_eq!(session.document(), &before_invalid);
        assert_eq!(session.revision(), styled.revision);
    }
}

#[test]
fn locked_objects_reject_effect_changes_without_mutation() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add title",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: text("Title"),
                },
            ),
        )
        .expect("add title");
    let text_id = added.created_objects[0].clone();
    let locked = session
        .execute(
            added.revision,
            Transaction::single(
                "Lock title",
                Command::SetObjectLock {
                    object_ids: vec![text_id.clone()],
                    locked: true,
                },
            ),
        )
        .expect("lock title");
    let before_locked = session.document().clone();
    let rejected = session
        .execute(
            locked.revision,
            Transaction::single(
                "Reject locked effect edit",
                Command::UpdateObjectEffects {
                    object_id: text_id.clone(),
                    effects: ObjectEffects::default(),
                },
            ),
        )
        .expect_err("locked effects cannot change");
    assert_eq!(rejected, CommandError::ObjectLocked(text_id));
    assert_eq!(session.document(), &before_locked);
    assert_eq!(session.revision(), locked.revision);
}

#[test]
fn duplicate_and_paste_preserve_effects_while_allocating_new_ids() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add title",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object: text("Title"),
                },
            ),
        )
        .expect("add title");
    let source_id = added.created_objects[0].clone();
    let effects = ObjectEffects {
        shadow: Some(shadow()),
        text_outline: Some(outline()),
        picture_border: None,
    };
    let styled = session
        .execute(
            added.revision,
            Transaction::single(
                "Style title",
                Command::UpdateObjectEffects {
                    object_id: source_id.clone(),
                    effects,
                },
            ),
        )
        .expect("style title");
    let source = session.document().pages[0].objects[0].clone();

    let duplicated = session
        .execute(
            styled.revision,
            Transaction::single(
                "Duplicate title",
                Command::DuplicateObject {
                    object_id: source_id.clone(),
                    offset_x_um: 5_000,
                    offset_y_um: 5_000,
                },
            ),
        )
        .expect("duplicate title");
    let duplicate = &session.document().pages[0].objects[1];
    assert_ne!(duplicate.id, source_id);
    assert_eq!(duplicate.effects, effects);

    let pasted = session
        .execute(
            duplicated.revision,
            Transaction::single(
                "Paste title",
                Command::PasteObjects {
                    objects: vec![source],
                    destination: ObjectLocation::Page(page_id),
                    offset_x_um: 10_000,
                    offset_y_um: 10_000,
                },
            ),
        )
        .expect("paste title");
    let pasted_object = &session.document().pages[0].objects[2];
    assert_eq!(pasted_object.id, pasted.created_objects[0]);
    assert_ne!(pasted_object.id, source_id);
    assert_eq!(pasted_object.effects, effects);
}

#[test]
fn current_schema_round_trips_effects_and_schema_nine_defaults_them_off() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add title",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: text("Title"),
                },
            ),
        )
        .expect("add title");
    let effects = ObjectEffects {
        shadow: Some(shadow()),
        text_outline: Some(outline()),
        picture_border: None,
    };
    session
        .execute(
            added.revision,
            Transaction::single(
                "Style title",
                Command::UpdateObjectEffects {
                    object_id: added.created_objects[0].clone(),
                    effects,
                },
            ),
        )
        .expect("style title");
    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("cover-effects");
    let current_path = temp.path().join("current.publisher-v2");
    save_new_package(&current_path, &snapshot, &AssetStore::default())
        .expect("save current package");
    assert_eq!(
        open_package(&current_path).expect("open effects").snapshot,
        snapshot
    );
    let current_manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&current_path, "manifest.json"))
            .expect("parse current manifest");
    assert_eq!(current_manifest["schema_version"], document_core::package::SCHEMA_VERSION);
    assert_eq!(
        current_manifest["snapshot"]["document"]["pages"][0]["objects"][0]["effects"]["shadow"]["offset_x"],
        4_000
    );

    let legacy_path = temp.path().join("legacy.publisher-v2");
    let mut legacy_session = DocumentSession::new(letter_document()).expect("valid legacy session");
    let legacy_page = legacy_session.document().pages[0].id.clone();
    legacy_session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add legacy title",
                Command::AddObject {
                    location: ObjectLocation::Page(legacy_page),
                    object: text("Legacy title"),
                },
            ),
        )
        .expect("add legacy title");
    let legacy_snapshot = ProjectSnapshot::from_session(&legacy_session);
    save_new_package(&legacy_path, &legacy_snapshot, &AssetStore::default())
        .expect("write legacy source package");
    let mut legacy_manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&legacy_path, "manifest.json"))
            .expect("parse legacy manifest");
    legacy_manifest["schema_version"] = serde_json::json!(9);
    let legacy_object = legacy_manifest["snapshot"]["document"]["pages"][0]["objects"][0]
        .as_object_mut()
        .expect("legacy object envelope");
    legacy_object.remove("effects");
    replace_archive_entry(
        &legacy_path,
        "manifest.json",
        serde_json::to_vec_pretty(&legacy_manifest).expect("encode schema nine manifest"),
    );
    let reopened = open_package(&legacy_path).expect("schema nine remains readable");
    assert_eq!(
        reopened.snapshot.document.pages[0].objects[0].effects,
        ObjectEffects::default()
    );
}
