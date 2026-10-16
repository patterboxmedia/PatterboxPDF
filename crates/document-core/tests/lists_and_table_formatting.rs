mod common;

use common::{TempDirectory, letter_document, read_archive_entry, replace_archive_entry};
use document_core::assets::AssetStore;
use document_core::commands::{
    Command, CommandError, DocumentSession, ObjectLocation, Transaction,
};
use document_core::model::{
    Color, ModelError, ObjectDraft, ObjectKind, ParagraphListStyle, ParagraphRange, ParagraphStyle,
    PhysicalSize, Revision, TableCellFormatting, TableData, TableGridBorder, TableTrackAxis,
    TableVerticalAlignment, TextAlignment, TextFrame, TextStoryFormatting, TextStyle, Transform,
};
use document_core::package::{
    PackageError, ProjectSnapshot, open_package, save_new_package,
};

fn text_session() -> (DocumentSession, document_core::model::ObjectId) {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add instructions",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Instructions".to_owned(),
                        size: PhysicalSize::new(100_000, 100_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Gather cards\nShuffle\nReveal\nBow".to_owned(),
                            style: TextStyle::default(),
                            frame: TextFrame::default(),
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .expect("add text");
    (session, added.created_objects[0].clone())
}

fn table_session() -> (DocumentSession, document_core::model::ObjectId) {
    let mut table = TableData::empty(3, 3).unwrap();
    table.cells = "MAGICWORD"
        .chars()
        .map(|character| character.to_string())
        .collect();
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add word search",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Word search".to_owned(),
                        size: PhysicalSize::new(90_000, 60_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .expect("add table");
    (session, added.created_objects[0].clone())
}

fn first_table(session: &DocumentSession) -> &TableData {
    let ObjectKind::Table { table } = &session.document().pages[0].objects[0].kind else {
        panic!("fixture remains a table");
    };
    table
}

#[test]
fn paragraph_lists_are_range_based_clearable_and_one_undo_step() {
    let (mut session, object_id) = text_session();
    let before = session.document().clone();
    let numbered = session
        .execute(
            session.revision(),
            Transaction {
                label: "Format instruction lists".to_owned(),
                commands: vec![
                    Command::UpdateParagraphStyle {
                        object_id: object_id.clone(),
                        range: ParagraphRange { start: 0, end: 1 },
                        style: ParagraphStyle {
                            list_style: Some(ParagraphListStyle::Bullet),
                            ..ParagraphStyle::default()
                        },
                    },
                    Command::UpdateParagraphStyle {
                        object_id,
                        range: ParagraphRange { start: 1, end: 4 },
                        style: ParagraphStyle {
                            list_style: Some(ParagraphListStyle::Numbered { start_at: 4 }),
                            ..ParagraphStyle::default()
                        },
                    },
                ],
            },
        )
        .expect("apply list styles");
    let ObjectKind::Text { story, .. } = &session.document().pages[0].objects[0].kind else {
        panic!("fixture remains text");
    };
    assert_eq!(story.paragraph_runs.len(), 2);
    assert_eq!(
        story.paragraph_runs[1].range,
        ParagraphRange { start: 1, end: 4 }
    );
    assert_eq!(
        story.paragraph_runs[1].style.list_style,
        Some(ParagraphListStyle::Numbered { start_at: 4 })
    );

    session
        .undo(numbered.revision)
        .expect("one undo restores both list operations");
    assert_eq!(session.document(), &before);

    let (mut session, object_id) = text_session();
    let applied = session
        .execute(
            session.revision(),
            Transaction::single(
                "Number all steps",
                Command::UpdateParagraphStyle {
                    object_id: object_id.clone(),
                    range: ParagraphRange { start: 0, end: 4 },
                    style: ParagraphStyle {
                        list_style: Some(ParagraphListStyle::Numbered { start_at: 1 }),
                        ..ParagraphStyle::default()
                    },
                },
            ),
        )
        .unwrap();
    session
        .execute(
            applied.revision,
            Transaction::single(
                "Remove list from one paragraph",
                Command::UpdateParagraphStyle {
                    object_id,
                    range: ParagraphRange { start: 2, end: 3 },
                    style: ParagraphStyle {
                        list_style: Some(ParagraphListStyle::None),
                        ..ParagraphStyle::default()
                    },
                },
            ),
        )
        .expect("explicit None clears one paragraph");
    let ObjectKind::Text { story, .. } = &session.document().pages[0].objects[0].kind else {
        panic!("fixture remains text");
    };
    assert_eq!(story.paragraph_runs.len(), 3);
    assert_eq!(
        story.paragraph_runs[1].style.list_style,
        Some(ParagraphListStyle::None)
    );

    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("paragraph-lists");
    let path = temp.path().join("instructions.publisher-v2");
    save_new_package(&path, &snapshot, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, snapshot);
}

#[test]
fn invalid_number_start_rejects_atomically() {
    let (mut session, object_id) = text_session();
    let before = session.document().clone();
    let revision = session.revision();
    let error = session
        .execute(
            revision,
            Transaction::single(
                "Invalid numbered list",
                Command::UpdateParagraphStyle {
                    object_id,
                    range: ParagraphRange { start: 0, end: 2 },
                    style: ParagraphStyle {
                        list_style: Some(ParagraphListStyle::Numbered { start_at: 0 }),
                        ..ParagraphStyle::default()
                    },
                },
            ),
        )
        .expect_err("numbering starts at one or later");
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::ParagraphListStartOutOfRange)
    );
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
}

#[test]
#[allow(clippy::too_many_lines)] // One end-to-end lifecycle proves the same formatting survives every copy/save boundary.
fn practical_table_formatting_resizes_duplicates_pastes_and_round_trips() {
    let (mut session, object_id) = table_session();
    let styled = session
        .execute(
            session.revision(),
            Transaction {
                label: "Format word search".to_owned(),
                commands: vec![
                    Command::UpdateTableGridBorder {
                        object_id: object_id.clone(),
                        border: TableGridBorder {
                            color: Color {
                                red: 12,
                                green: 34,
                                blue: 56,
                                alpha: 255,
                            },
                            width_millipoints: 0,
                        },
                    },
                    Command::UpdateTableCellAlignment {
                        object_id: object_id.clone(),
                        row: 1,
                        column: 2,
                        horizontal_alignment: Some(TextAlignment::Right),
                        vertical_alignment: Some(TableVerticalAlignment::Bottom),
                    },
                    Command::UpdateTableTrackSize {
                        object_id: object_id.clone(),
                        axis: TableTrackAxis::Row,
                        index: 1,
                        weight: 2_000,
                    },
                    Command::UpdateTableTrackSize {
                        object_id: object_id.clone(),
                        axis: TableTrackAxis::Column,
                        index: 2,
                        weight: 3_000,
                    },
                ],
            },
        )
        .expect("format table atomically");
    let styled_table = first_table(&session).clone();
    assert_eq!(styled_table.grid_border.width_millipoints, 0);
    assert_eq!(styled_table.row_weights, vec![1_000, 2_000, 1_000]);
    assert_eq!(styled_table.column_weights, vec![1_000, 1_000, 3_000]);
    assert_eq!(styled_table.cell_formatting.len(), 1);

    let resized_object = session
        .execute(
            styled.revision,
            Transaction::single(
                "Resize word search object",
                Command::ResizeObject {
                    object_id: object_id.clone(),
                    size: PhysicalSize::new(180_000, 120_000).unwrap(),
                },
            ),
        )
        .unwrap();
    assert_eq!(
        first_table(&session),
        &styled_table,
        "object resize preserves relative tracks"
    );

    let expanded = session
        .execute(
            resized_object.revision,
            Transaction::single(
                "Add a row and column",
                Command::UpdateTableDimensions {
                    object_id: object_id.clone(),
                    rows: 4,
                    columns: 4,
                },
            ),
        )
        .unwrap();
    assert_eq!(
        first_table(&session).row_weights,
        vec![1_000, 2_000, 1_000, 1_000]
    );
    assert_eq!(
        first_table(&session).column_weights,
        vec![1_000, 1_000, 3_000, 1_000]
    );

    let duplicated = session
        .execute(
            expanded.revision,
            Transaction::single(
                "Duplicate formatted table",
                Command::DuplicateObject {
                    object_id: object_id.clone(),
                    offset_x_um: 5_000,
                    offset_y_um: 5_000,
                },
            ),
        )
        .unwrap();
    let duplicate = session.document().pages[0].objects[1].clone();
    let ObjectKind::Table {
        table: duplicated_table,
    } = &duplicate.kind
    else {
        panic!("duplicate remains table");
    };
    assert_eq!(duplicated_table, first_table(&session));

    let pasted = session
        .execute(
            duplicated.revision,
            Transaction::single(
                "Paste formatted table",
                Command::PasteObjects {
                    objects: vec![duplicate],
                    destination: ObjectLocation::Pasteboard,
                    offset_x_um: 5_000,
                    offset_y_um: 5_000,
                },
            ),
        )
        .unwrap();
    let ObjectKind::Table {
        table: pasted_table,
    } = &session.document().pasteboard.objects[0].kind
    else {
        panic!("paste remains table");
    };
    assert_eq!(pasted_table, first_table(&session));

    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("table-formatting");
    let path = temp.path().join("word-search.publisher-v2");
    save_new_package(&path, &snapshot, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, snapshot);
    session
        .undo(pasted.revision)
        .expect("paste is one undo step");
    assert!(session.document().pasteboard.objects.is_empty());
}

#[test]
fn table_dimension_reduction_trims_tracks_and_sparse_cell_formatting() {
    let (mut session, object_id) = table_session();
    let formatted = session
        .execute(
            session.revision(),
            Transaction {
                label: "Format lower-right cell".to_owned(),
                commands: vec![
                    Command::UpdateTableCellAlignment {
                        object_id: object_id.clone(),
                        row: 2,
                        column: 2,
                        horizontal_alignment: Some(TextAlignment::Left),
                        vertical_alignment: Some(TableVerticalAlignment::Top),
                    },
                    Command::UpdateTableTrackSize {
                        object_id: object_id.clone(),
                        axis: TableTrackAxis::Row,
                        index: 0,
                        weight: 2_000,
                    },
                ],
            },
        )
        .unwrap();
    session
        .execute(
            formatted.revision,
            Transaction::single(
                "Reduce table",
                Command::UpdateTableDimensions {
                    object_id,
                    rows: 2,
                    columns: 2,
                },
            ),
        )
        .unwrap();
    let table = first_table(&session);
    assert_eq!(table.row_weights, vec![2_000, 1_000]);
    assert_eq!(table.column_weights, vec![1_000, 1_000]);
    assert!(table.cell_formatting.is_empty());
}

#[test]
fn distributing_tracks_is_atomic_undoable_and_persistent() {
    let (mut session, object_id) = table_session();
    let weighted = session
        .execute(
            session.revision(),
            Transaction::single(
                "Widen one row",
                Command::UpdateTableTrackSize {
                    object_id: object_id.clone(),
                    axis: TableTrackAxis::Row,
                    index: 1,
                    weight: 4_000,
                },
            ),
        )
        .unwrap();
    let distributed = session
        .execute(
            weighted.revision,
            Transaction::single(
                "Distribute rows evenly",
                Command::DistributeTableTracks {
                    object_id,
                    axis: TableTrackAxis::Row,
                },
            ),
        )
        .unwrap();
    assert_eq!(first_table(&session).row_weights, vec![1_000; 3]);

    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("distributed-tracks");
    let path = temp.path().join("distributed.publisher-v2");
    save_new_package(&path, &snapshot, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, snapshot);

    let undone = session
        .undo(distributed.revision)
        .expect("distribution is one undo step");
    assert_eq!(first_table(&session).row_weights, vec![1_000, 4_000, 1_000]);
    session.redo(undone).expect("distribution can be redone");
    assert_eq!(first_table(&session).row_weights, vec![1_000; 3]);
}

#[test]
#[allow(clippy::too_many_lines)] // The atomicity matrix keeps each rejected command beside its exact unchanged-state assertion.
fn malformed_table_formatting_and_mixed_transactions_fail_atomically() {
    let (mut session, object_id) = table_session();
    let before = session.document().clone();
    let revision = session.revision();
    let error = session
        .execute(
            revision,
            Transaction {
                label: "Reject mixed table transaction".to_owned(),
                commands: vec![
                    Command::UpdateTableGridBorder {
                        object_id: object_id.clone(),
                        border: TableGridBorder {
                            color: Color {
                                red: 1,
                                green: 2,
                                blue: 3,
                                alpha: 255,
                            },
                            width_millipoints: 1_000,
                        },
                    },
                    Command::UpdateTableTrackSize {
                        object_id,
                        axis: TableTrackAxis::Column,
                        index: 0,
                        weight: 0,
                    },
                ],
            },
        )
        .expect_err("invalid second command rejects the first too");
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::TableTrackWeightOutOfRange)
    );
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);

    for (command, expected) in [
        (
            Command::UpdateTableGridBorder {
                object_id: session.document().pages[0].objects[0].id.clone(),
                border: TableGridBorder {
                    color: Color {
                        red: 0,
                        green: 0,
                        blue: 0,
                        alpha: 255,
                    },
                    width_millipoints: 72_001,
                },
            },
            ModelError::TableGridBorderWidthOutOfRange,
        ),
        (
            Command::UpdateTableCellAlignment {
                object_id: session.document().pages[0].objects[0].id.clone(),
                row: 3,
                column: 0,
                horizontal_alignment: Some(TextAlignment::Left),
                vertical_alignment: None,
            },
            ModelError::TableCellOutOfRange,
        ),
        (
            Command::UpdateTableTrackSize {
                object_id: session.document().pages[0].objects[0].id.clone(),
                axis: TableTrackAxis::Row,
                index: 3,
                weight: 1_000,
            },
            ModelError::TableTrackOutOfRange,
        ),
        (
            Command::UpdateTableTrackSize {
                object_id: session.document().pages[0].objects[0].id.clone(),
                axis: TableTrackAxis::Column,
                index: 0,
                weight: TableData::MAX_TRACK_WEIGHT + 1,
            },
            ModelError::TableTrackWeightOutOfRange,
        ),
    ] {
        assert_eq!(
            session
                .execute(
                    revision,
                    Transaction::single("Reject invalid format", command)
                )
                .unwrap_err(),
            CommandError::InvalidModel(expected)
        );
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), revision);
    }

    let mut malformed = TableData::empty(2, 2).unwrap();
    malformed.cell_formatting = vec![
        TableCellFormatting {
            row: 1,
            column: 1,
            horizontal_alignment: Some(TextAlignment::Left),
            vertical_alignment: None,
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 0,
            column: 0,
            horizontal_alignment: None,
            vertical_alignment: Some(TableVerticalAlignment::Top),
            ..TableCellFormatting::default()
        },
    ];
    let mut invalid_session = DocumentSession::new(letter_document()).unwrap();
    let page_id = invalid_session.document().pages[0].id.clone();
    assert_eq!(
        invalid_session
            .execute(
                Revision::initial(),
                Transaction::single(
                    "Reject unsorted formatting",
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Malformed".to_owned(),
                            size: PhysicalSize::new(20_000, 20_000).unwrap(),
                            transform: Transform::identity_at(0, 0),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Table { table: malformed },
                        },
                    },
                ),
            )
            .unwrap_err(),
        CommandError::InvalidModel(ModelError::InvalidTableCellFormatting)
    );
}

#[test]
fn malformed_table_track_vectors_are_rejected_instead_of_repaired() {
    for (row_weights, column_weights, expected) in [
        (
            vec![1_000],
            vec![1_000, 1_000],
            ModelError::InvalidTableTrackWeights,
        ),
        (
            vec![1_000, 1_000],
            vec![0, 1_000],
            ModelError::TableTrackWeightOutOfRange,
        ),
    ] {
        let mut table = TableData::empty(2, 2).unwrap();
        table.row_weights = row_weights;
        table.column_weights = column_weights;
        let mut session = DocumentSession::new(letter_document()).unwrap();
        let page_id = session.document().pages[0].id.clone();
        assert_eq!(
            session
                .execute(
                    Revision::initial(),
                    Transaction::single(
                        "Reject malformed tracks",
                        Command::AddObject {
                            location: ObjectLocation::Page(page_id),
                            object: ObjectDraft {
                                name: "Malformed tracks".to_owned(),
                                size: PhysicalSize::new(20_000, 20_000).unwrap(),
                                transform: Transform::identity_at(0, 0),
                                opacity_millionths: 1_000_000,
                                kind: ObjectKind::Table { table },
                            },
                        },
                    ),
                )
                .unwrap_err(),
            CommandError::InvalidModel(expected)
        );
    }
}

#[test]
fn all_table_formatting_commands_respect_object_locking() {
    let (mut session, object_id) = table_session();
    let locked = session
        .execute(
            session.revision(),
            Transaction::single(
                "Lock word search",
                Command::SetObjectLock {
                    object_ids: vec![object_id.clone()],
                    locked: true,
                },
            ),
        )
        .unwrap();
    for command in [
        Command::UpdateTableGridBorder {
            object_id: object_id.clone(),
            border: TableGridBorder::default(),
        },
        Command::UpdateTableCellAlignment {
            object_id: object_id.clone(),
            row: 0,
            column: 0,
            horizontal_alignment: Some(TextAlignment::Left),
            vertical_alignment: None,
        },
        Command::UpdateTableTrackSize {
            object_id: object_id.clone(),
            axis: TableTrackAxis::Row,
            index: 0,
            weight: 2_000,
        },
        Command::DistributeTableTracks {
            object_id: object_id.clone(),
            axis: TableTrackAxis::Column,
        },
    ] {
        assert_eq!(
            session
                .execute(
                    locked.revision,
                    Transaction::single("Blocked format", command)
                )
                .unwrap_err(),
            CommandError::ObjectLocked(object_id.clone())
        );
    }
}

#[test]
fn table_formatting_commands_reject_non_table_objects() {
    let (mut session, object_id) = text_session();
    let revision = session.revision();
    for command in [
        Command::UpdateTableGridBorder {
            object_id: object_id.clone(),
            border: TableGridBorder::default(),
        },
        Command::UpdateTableCellAlignment {
            object_id: object_id.clone(),
            row: 0,
            column: 0,
            horizontal_alignment: Some(TextAlignment::Center),
            vertical_alignment: Some(TableVerticalAlignment::Middle),
        },
        Command::UpdateTableTrackSize {
            object_id: object_id.clone(),
            axis: TableTrackAxis::Column,
            index: 0,
            weight: 2_000,
        },
        Command::DistributeTableTracks {
            object_id: object_id.clone(),
            axis: TableTrackAxis::Column,
        },
    ] {
        assert_eq!(
            session
                .execute(revision, Transaction::single("Reject wrong kind", command))
                .unwrap_err(),
            CommandError::ObjectKindMismatch(object_id.clone())
        );
        assert_eq!(session.revision(), revision);
    }
}

#[test]
fn schema_twelve_defaults_preserve_the_existing_table_grid_and_equal_tracks() {
    let (session, _) = table_session();
    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("schema-twelve-table-formatting");
    let path = temp.path().join("legacy.publisher-v2");
    save_new_package(&path, &snapshot, &AssetStore::default()).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&path, "manifest.json")).unwrap();
    manifest["schema_version"] = serde_json::json!(12);
    let table = manifest["snapshot"]["document"]["pages"][0]["objects"][0]["kind"]["table"]
        .as_object_mut()
        .unwrap();
    table.remove("grid_border");
    table.remove("cell_formatting");
    table.remove("row_weights");
    table.remove("column_weights");
    replace_archive_entry(
        &path,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).unwrap(),
    );

    let reopened = open_package(&path).expect("schema twelve table remains readable");
    let ObjectKind::Table { table } = &reopened.snapshot.document.pages[0].objects[0].kind else {
        panic!("legacy object remains a table");
    };
    assert_eq!(table.grid_border, TableGridBorder::default());
    assert_eq!(table.row_weights, vec![1_000; 3]);
    assert_eq!(table.column_weights, vec![1_000; 3]);
    assert!(table.cell_formatting.is_empty());
}

#[test]
fn explicit_null_table_track_weights_are_rejected_instead_of_legacy_defaulted() {
    let (session, _) = table_session();
    let temp = TempDirectory::new("null-table-track-weights");
    let path = temp.path().join("malformed.publisher-v2");
    save_new_package(
        &path,
        &ProjectSnapshot::from_session(&session),
        &AssetStore::default(),
    )
    .unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&path, "manifest.json")).unwrap();
    manifest["snapshot"]["document"]["pages"][0]["objects"][0]["kind"]["table"]["row_weights"] =
        serde_json::Value::Null;
    replace_archive_entry(
        &path,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).unwrap(),
    );

    assert!(matches!(open_package(&path), Err(PackageError::Json(_))));
}
