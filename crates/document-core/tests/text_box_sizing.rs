mod common;

use common::{TempDirectory, letter_document, read_archive_entry, replace_archive_entry};
use document_core::assets::AssetStore;
use document_core::commands::{
    Command, CommandError, DocumentSession, ObjectLocation, Transaction,
};
use document_core::model::{
    Color, Insets, ModelError, ObjectDraft, ObjectKind, PhysicalSize, Revision, ShapeKind,
    ShapeStroke, TextBoxSizingMode, TextFrame, TextStyle, TextVerticalAlignment, Transform,
    WrapMode, WrapSettings, WrapSide,
};
use document_core::package::{ProjectSnapshot, SCHEMA_VERSION, open_package, save_new_package};

fn text_box(text: &str, sizing_mode: TextBoxSizingMode, size: PhysicalSize) -> ObjectDraft {
    ObjectDraft {
        name: "Instructions".to_owned(),
        size,
        transform: Transform::identity_at(10_000, 20_000),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Text {
            text: text.to_owned(),
            style: TextStyle::default(),
            frame: TextFrame {
                sizing_mode,
                ..TextFrame::default()
            },
            story: document_core::model::TextStoryFormatting::default(),
        },
    }
}

#[test]
fn unusable_columns_are_rejected_without_changing_text_or_history() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add instructions",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: text_box(
                        "Preserve these instructions",
                        TextBoxSizingMode::Fixed,
                        PhysicalSize::new(100_000, 50_000).unwrap(),
                    ),
                },
            ),
        )
        .unwrap();
    let before = session.document().clone();
    session
        .execute(
            added.revision,
            Transaction::single(
                "Set unusable columns",
                Command::UpdateTextFrame {
                    object_id: added.created_objects[0].clone(),
                    column_count: 2,
                    column_gap_um: 100_000,
                },
            ),
        )
        .expect_err("a gap that consumes the complete text width must be refused");
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), added.revision);
    session
        .undo(added.revision)
        .expect("rejected property change adds no history entry");
    assert!(session.document().pages[0].objects.is_empty());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "One preservation journey covers property refusals, copy, Save/reopen and undo"
)]
fn interior_and_wrap_settings_survive_copy_save_and_undo_with_atomic_refusals() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add instructional objects".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: text_box(
                            "Keep every instruction",
                            TextBoxSizingMode::Fixed,
                            PhysicalSize::new(100_000, 50_000).unwrap(),
                        ),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Diagram".to_owned(),
                            size: PhysicalSize::new(20_000, 20_000).unwrap(),
                            transform: Transform::identity_at(50_000, 30_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Shape {
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
                        },
                    },
                ],
            },
        )
        .unwrap();
    let text_id = added.created_objects[0].clone();
    let diagram_id = added.created_objects[1].clone();
    let defaults = serde_json::to_value(ProjectSnapshot::from_session(&session)).unwrap();
    let old_object = &defaults["document"]["pages"][0]["objects"][0];
    assert!(old_object.get("wrap_settings").is_none());
    assert!(old_object["kind"]["frame"].get("margins").is_none());
    assert!(
        old_object["kind"]["frame"]
            .get("vertical_alignment")
            .is_none()
    );
    let styled = session
        .execute(
            added.revision,
            Transaction {
                label: "Compose interior and wrap".to_owned(),
                commands: vec![
                    Command::UpdateTextBoxInterior {
                        object_id: text_id.clone(),
                        margins: Insets::uniform(10_000).unwrap(),
                        vertical_alignment: TextVerticalAlignment::Middle,
                    },
                    Command::UpdateTextFrame {
                        object_id: text_id.clone(),
                        column_count: 2,
                        column_gap_um: 8_000,
                    },
                    Command::UpdateWrapSettings {
                        object_id: diagram_id.clone(),
                        settings: WrapSettings {
                            distance_um: 3_000,
                            side: WrapSide::Left,
                        },
                    },
                ],
            },
        )
        .unwrap();
    let before = session.document().clone();
    for command in [
        Command::UpdateTextBoxInterior {
            object_id: text_id.clone(),
            margins: Insets::uniform(25_000).unwrap(),
            vertical_alignment: TextVerticalAlignment::Bottom,
        },
        Command::ResizeObject {
            object_id: text_id.clone(),
            size: PhysicalSize::new(25_000, 50_000).unwrap(),
        },
        Command::UpdateTextFrame {
            object_id: text_id.clone(),
            column_count: 2,
            column_gap_um: 80_000,
        },
        Command::UpdateWrapSettings {
            object_id: diagram_id.clone(),
            settings: WrapSettings {
                distance_um: -1,
                side: WrapSide::Right,
            },
        },
    ] {
        session
            .execute(
                styled.revision,
                Transaction::single("Refuse unusable geometry", command),
            )
            .expect_err("invalid geometry is refused");
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), styled.revision);
    }
    let copied = session
        .execute(
            styled.revision,
            Transaction {
                label: "Copy composition".to_owned(),
                commands: vec![
                    Command::DuplicateObject {
                        object_id: text_id,
                        offset_x_um: 10_000,
                        offset_y_um: 10_000,
                    },
                    Command::DuplicateObject {
                        object_id: diagram_id,
                        offset_x_um: 10_000,
                        offset_y_um: 10_000,
                    },
                ],
            },
        )
        .unwrap();
    let objects = &session.document().pages[0].objects;
    let copied_text = objects
        .iter()
        .find(|object| object.id == copied.created_objects[0])
        .unwrap();
    let copied_diagram = objects
        .iter()
        .find(|object| object.id == copied.created_objects[1])
        .unwrap();
    assert_eq!(before.pages[0].objects[0].kind, copied_text.kind);
    assert_eq!(
        before.pages[0].objects[1].wrap_settings,
        copied_diagram.wrap_settings
    );
    let temporary = TempDirectory::new("text-box-interior");
    let path = temporary.path().join("composed.publisher-v2");
    let snapshot = ProjectSnapshot::from_session(&session);
    save_new_package(&path, &snapshot, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, snapshot);
    session.undo(copied.revision).unwrap();
    assert_eq!(session.document(), &before);
}

#[test]
fn sizing_conversion_is_atomic_content_preserving_and_undoable() {
    let original_size = PhysicalSize::new(100_000, 50_000).expect("fixed size");
    let fit_size = PhysicalSize::new(42_000, 18_000).expect("composed fit size");
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add fixed text box",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: text_box(
                        "First line\nSecond line",
                        TextBoxSizingMode::Fixed,
                        original_size,
                    ),
                },
            ),
        )
        .expect("add text box");
    let object_id = added.created_objects[0].clone();
    let before = session.document().pages[0].objects[0].clone();

    let converted = session
        .execute(
            added.revision,
            Transaction::single(
                "Fit text",
                Command::UpdateTextBoxSizing {
                    object_id,
                    sizing_mode: TextBoxSizingMode::Fit,
                    size: fit_size,
                },
            ),
        )
        .expect("convert to fit text");
    let fitted = session.document().pages[0].objects[0].clone();
    assert_eq!(fitted.size, fit_size);
    assert!(matches!(
        &fitted.kind,
        ObjectKind::Text {
            text, style, frame, ..
        }
            if text == "First line\nSecond line"
                && style == &TextStyle::default()
                && frame.sizing_mode == TextBoxSizingMode::Fit
    ));

    let undone = session.undo(converted.revision).expect("undo conversion");
    assert_eq!(session.document().pages[0].objects[0], before);
    session.redo(undone).expect("redo conversion");
    assert_eq!(session.document().pages[0].objects[0], fitted);
}

#[test]
fn fit_geometry_has_a_nonzero_floor_and_invalid_conversion_preserves_exact_state() {
    let minimum = PhysicalSize::new(
        TextFrame::MIN_FIT_WIDTH_MICROMETRES,
        TextFrame::MIN_FIT_HEIGHT_MICROMETRES,
    )
    .expect("nonzero fit minimum");
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add empty fit text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: text_box("   \n", TextBoxSizingMode::Fit, minimum),
                },
            ),
        )
        .expect("minimum fit geometry accepts empty or whitespace content");
    let before = session.document().clone();

    let error = session
        .execute(
            added.revision,
            Transaction::single(
                "Reject invalid fit geometry",
                Command::UpdateTextBoxSizing {
                    object_id: added.created_objects[0].clone(),
                    sizing_mode: TextBoxSizingMode::Fit,
                    size: PhysicalSize::new(TextFrame::MIN_FIT_WIDTH_MICROMETRES - 1, 10_000)
                        .expect("positive but below fit minimum"),
                },
            ),
        )
        .expect_err("below-minimum fit geometry must be rejected");
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::FitTextSizeBelowMinimum)
    );
    assert_eq!(session.revision(), added.revision);
    assert_eq!(session.document(), &before);
}

#[test]
fn changing_columns_does_not_implicitly_change_fit_sizing() {
    let minimum = PhysicalSize::new(
        TextFrame::MIN_FIT_WIDTH_MICROMETRES,
        TextFrame::MIN_FIT_HEIGHT_MICROMETRES,
    )
    .expect("nonzero fit minimum");
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add fit text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: text_box("Heading", TextBoxSizingMode::Fit, minimum),
                },
            ),
        )
        .expect("add fit text");
    session
        .execute(
            added.revision,
            Transaction::single(
                "Use two columns",
                Command::UpdateTextFrame {
                    object_id: added.created_objects[0].clone(),
                    column_count: 2,
                    column_gap_um: 8_000,
                },
            ),
        )
        .expect("update text columns");

    assert!(matches!(
        &session.document().pages[0].objects[0].kind,
        ObjectKind::Text { frame, .. }
            if frame.sizing_mode == TextBoxSizingMode::Fit
                && frame.column_count == 2
                && frame.column_gap_um == 8_000
    ));
}

#[test]
fn current_schema_preserves_sizing_and_schema_ten_defaults_to_fixed() {
    let fit_size = PhysicalSize::new(40_000, 12_000).expect("fit size");
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add fit text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: text_box("Heading", TextBoxSizingMode::Fit, fit_size),
                },
            ),
        )
        .expect("add fit text");
    let fit_snapshot = ProjectSnapshot::from_session(&session);
    let mut fixed_snapshot = fit_snapshot.clone();
    let ObjectKind::Text { frame, .. } = &mut fixed_snapshot.document.pages[0].objects[0].kind
    else {
        panic!("fixture remains text");
    };
    frame.sizing_mode = TextBoxSizingMode::Fixed;
    assert!(
        serde_json::to_value(&fixed_snapshot).expect("serialize Fixed snapshot")["document"]
            ["pages"][0]["objects"][0]["kind"]["frame"]
            .get("sizing_mode")
            .is_none(),
        "Fixed remains the omitted schema-ten default"
    );
    assert_ne!(
        fit_snapshot.semantic_digest().expect("digest Fit intent"),
        fixed_snapshot
            .semantic_digest()
            .expect("digest Fixed intent"),
        "sizing intent participates in semantic identity even when geometry is unchanged"
    );

    let temporary = TempDirectory::new("text-box-sizing");
    let current_path = temporary.path().join("schema-twelve.publisher-v2");
    save_new_package(&current_path, &fit_snapshot, &AssetStore::default())
        .expect("save current schema package");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&current_path, "manifest.json"))
            .expect("parse current manifest");
    assert_eq!(manifest["schema_version"], SCHEMA_VERSION);
    assert_eq!(
        manifest["snapshot"]["document"]["pages"][0]["objects"][0]["kind"]["frame"]["sizing_mode"],
        "fit"
    );

    manifest["schema_version"] = serde_json::json!(10);
    manifest["snapshot"]["document"]["pages"][0]["objects"][0]["kind"]["frame"]
        .as_object_mut()
        .expect("text frame")
        .remove("sizing_mode");
    let legacy_path = temporary.path().join("schema-ten.publisher-v2");
    std::fs::copy(&current_path, &legacy_path).expect("copy package for legacy fixture");
    replace_archive_entry(
        &legacy_path,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).expect("encode schema ten manifest"),
    );
    let reopened = open_package(&legacy_path).expect("schema ten remains readable");
    assert_eq!(
        reopened.snapshot.document.pages[0].objects[0].size,
        fit_size
    );
    assert!(matches!(
        &reopened.snapshot.document.pages[0].objects[0].kind,
        ObjectKind::Text { frame, .. }
            if frame.sizing_mode == TextBoxSizingMode::Fixed
    ));
}
