mod common;

use common::{TempDirectory, letter_document, rectangle};
use document_core::assets::AssetStore;
use document_core::commands::{
    Command, CommandError, DocumentSession, ObjectLocation, Transaction,
};
use document_core::model::{
    ModelError, ObjectDraft, ObjectKind, PhysicalSize, Revision, TableCellFormatting, TableData,
    TableTrackAxis, TableVerticalAlignment, TextAlignment, Transform,
};
use document_core::package::{ProjectSnapshot, open_package, save_new_package};

fn formatted_table(rows: u16, columns: u16) -> TableData {
    let mut table = TableData::empty(rows, columns).expect("valid table dimensions");
    if rows == 3 && columns == 3 {
        table.cells = "ABCDEFGHI"
            .chars()
            .map(|character| character.to_string())
            .collect();
        table.row_weights = vec![100, 200, 300];
        table.column_weights = vec![400, 500, 600];
        table.cell_formatting = vec![
            TableCellFormatting {
                row: 0,
                column: 2,
                horizontal_alignment: Some(TextAlignment::Left),
                vertical_alignment: Some(TableVerticalAlignment::Top),
                ..TableCellFormatting::default()
            },
            TableCellFormatting {
                row: 1,
                column: 1,
                horizontal_alignment: Some(TextAlignment::Right),
                vertical_alignment: Some(TableVerticalAlignment::Bottom),
                ..TableCellFormatting::default()
            },
            TableCellFormatting {
                row: 2,
                column: 0,
                horizontal_alignment: Some(TextAlignment::Justify),
                vertical_alignment: Some(TableVerticalAlignment::Middle),
                ..TableCellFormatting::default()
            },
            TableCellFormatting {
                row: 2,
                column: 2,
                horizontal_alignment: Some(TextAlignment::Center),
                vertical_alignment: Some(TableVerticalAlignment::Top),
                ..TableCellFormatting::default()
            },
        ];
    }
    table
}

fn table_session(table: TableData) -> (DocumentSession, document_core::model::ObjectId) {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let outcome = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add table",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Word search".to_owned(),
                        size: PhysicalSize::new(90_000, 60_000).expect("positive size"),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .expect("add table");
    (session, outcome.created_objects[0].clone())
}

fn first_table(session: &DocumentSession) -> &TableData {
    let ObjectKind::Table { table } = &session.document().pages[0].objects[0].kind else {
        panic!("fixture remains a table");
    };
    table
}

#[test]
fn indexed_insertions_remap_cells_formatting_and_weights_in_one_undo_step() {
    let original = formatted_table(3, 3);
    let (mut session, object_id) = table_session(original.clone());
    let inserted = session
        .execute(
            session.revision(),
            Transaction {
                label: "Insert selected tracks".to_owned(),
                commands: vec![
                    Command::InsertTableTrack {
                        object_id: object_id.clone(),
                        axis: TableTrackAxis::Row,
                        index: 1,
                    },
                    Command::InsertTableTrack {
                        object_id,
                        axis: TableTrackAxis::Column,
                        index: 2,
                    },
                ],
            },
        )
        .expect("insert row and column");
    let expected_formatting = vec![
        TableCellFormatting {
            row: 0,
            column: 3,
            horizontal_alignment: Some(TextAlignment::Left),
            vertical_alignment: Some(TableVerticalAlignment::Top),
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 2,
            column: 1,
            horizontal_alignment: Some(TextAlignment::Right),
            vertical_alignment: Some(TableVerticalAlignment::Bottom),
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 3,
            column: 0,
            horizontal_alignment: Some(TextAlignment::Justify),
            vertical_alignment: Some(TableVerticalAlignment::Middle),
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 3,
            column: 3,
            horizontal_alignment: Some(TextAlignment::Center),
            vertical_alignment: Some(TableVerticalAlignment::Top),
            ..TableCellFormatting::default()
        },
    ];
    let table = first_table(&session);
    assert_eq!((table.rows, table.columns), (4, 4));
    assert_eq!(
        table.cells,
        [
            "A", "B", "", "C", "", "", "", "", "D", "E", "", "F", "G", "H", "", "I"
        ]
    );
    assert_eq!(table.row_weights, [100, 1_000, 200, 300]);
    assert_eq!(table.column_weights, [400, 500, 1_000, 600]);
    assert_eq!(table.cell_formatting, expected_formatting);

    let undone = session.undo(inserted.revision).expect("undo insertions");
    assert_eq!(first_table(&session), &original);
    session.redo(undone).expect("redo insertions");
    assert_eq!(first_table(&session).cell_formatting, expected_formatting);
}

#[test]
fn indexed_deletions_remove_selected_content_and_shift_later_addresses() {
    let original = formatted_table(3, 3);
    let (mut session, object_id) = table_session(original.clone());
    let deleted = session
        .execute(
            session.revision(),
            Transaction {
                label: "Delete selected tracks".to_owned(),
                commands: vec![
                    Command::DeleteTableTrack {
                        object_id: object_id.clone(),
                        axis: TableTrackAxis::Row,
                        index: 1,
                    },
                    Command::DeleteTableTrack {
                        object_id,
                        axis: TableTrackAxis::Column,
                        index: 1,
                    },
                ],
            },
        )
        .expect("delete row and column");
    let table = first_table(&session);
    assert_eq!((table.rows, table.columns), (2, 2));
    assert_eq!(table.cells, ["A", "C", "G", "I"]);
    assert_eq!(table.row_weights, [100, 300]);
    assert_eq!(table.column_weights, [400, 600]);
    assert_eq!(
        table.cell_formatting,
        [
            TableCellFormatting {
                row: 0,
                column: 1,
                horizontal_alignment: Some(TextAlignment::Left),
                vertical_alignment: Some(TableVerticalAlignment::Top),
                ..TableCellFormatting::default()
            },
            TableCellFormatting {
                row: 1,
                column: 0,
                horizontal_alignment: Some(TextAlignment::Justify),
                vertical_alignment: Some(TableVerticalAlignment::Middle),
                ..TableCellFormatting::default()
            },
            TableCellFormatting {
                row: 1,
                column: 1,
                horizontal_alignment: Some(TextAlignment::Center),
                vertical_alignment: Some(TableVerticalAlignment::Top),
                ..TableCellFormatting::default()
            },
        ]
    );
    session.undo(deleted.revision).expect("undo deletions");
    assert_eq!(first_table(&session), &original);
}

#[test]
fn invalid_track_mutation_rolls_back_the_whole_transaction_and_stale_revision_is_rejected() {
    let (mut session, object_id) = table_session(formatted_table(3, 3));
    let before = session.document().clone();
    let revision = session.revision();
    let error = session
        .execute(
            revision,
            Transaction {
                label: "Reject partial insertion".to_owned(),
                commands: vec![
                    Command::InsertTableTrack {
                        object_id: object_id.clone(),
                        axis: TableTrackAxis::Row,
                        index: 1,
                    },
                    Command::DeleteTableTrack {
                        object_id: object_id.clone(),
                        axis: TableTrackAxis::Column,
                        index: 9,
                    },
                ],
            },
        )
        .expect_err("invalid second command rejects the transaction");
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::TableTrackOutOfRange)
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);

    let inserted = session
        .execute(
            revision,
            Transaction::single(
                "Insert row",
                Command::InsertTableTrack {
                    object_id: object_id.clone(),
                    axis: TableTrackAxis::Row,
                    index: 1,
                },
            ),
        )
        .expect("fresh revision inserts");
    let after = session.document().clone();
    assert_eq!(
        session
            .execute(
                revision,
                Transaction::single(
                    "Stale delete",
                    Command::DeleteTableTrack {
                        object_id,
                        axis: TableTrackAxis::Row,
                        index: 1,
                    },
                ),
            )
            .expect_err("stale revision is rejected"),
        CommandError::RevisionConflict {
            expected: revision,
            actual: inserted.revision,
        }
    );
    assert_eq!(session.document(), &after);
}

#[test]
fn table_track_mutations_reject_invalid_indices_last_tracks_and_dimension_overflow() {
    let (mut session, object_id) = table_session(formatted_table(3, 3));
    let revision = session.revision();
    for command in [
        Command::InsertTableTrack {
            object_id: object_id.clone(),
            axis: TableTrackAxis::Row,
            index: 4,
        },
        Command::InsertTableTrack {
            object_id: object_id.clone(),
            axis: TableTrackAxis::Column,
            index: 4,
        },
        Command::DeleteTableTrack {
            object_id: object_id.clone(),
            axis: TableTrackAxis::Row,
            index: 3,
        },
        Command::DeleteTableTrack {
            object_id: object_id.clone(),
            axis: TableTrackAxis::Column,
            index: 3,
        },
    ] {
        assert_eq!(
            session
                .execute(revision, Transaction::single("Reject index", command))
                .expect_err("index is invalid"),
            CommandError::InvalidModel(ModelError::TableTrackOutOfRange)
        );
        assert_eq!(session.revision(), revision);
    }

    let (mut single, single_id) = table_session(formatted_table(1, 1));
    let single_revision = single.revision();
    for axis in [TableTrackAxis::Row, TableTrackAxis::Column] {
        assert_eq!(
            single
                .execute(
                    single_revision,
                    Transaction::single(
                        "Keep final track",
                        Command::DeleteTableTrack {
                            object_id: single_id.clone(),
                            axis,
                            index: 0,
                        },
                    ),
                )
                .expect_err("last track cannot be deleted"),
            CommandError::InvalidModel(ModelError::InvalidTableDimensions)
        );
    }

    for (table, axis) in [
        (formatted_table(TableData::MAX_ROWS, 1), TableTrackAxis::Row),
        (
            formatted_table(1, TableData::MAX_COLUMNS),
            TableTrackAxis::Column,
        ),
    ] {
        let (mut maximum, maximum_id) = table_session(table);
        assert_eq!(
            maximum
                .execute(
                    maximum.revision(),
                    Transaction::single(
                        "Reject table overflow",
                        Command::InsertTableTrack {
                            object_id: maximum_id,
                            axis,
                            index: 0,
                        },
                    ),
                )
                .expect_err("maximum dimension cannot grow"),
            CommandError::InvalidModel(ModelError::InvalidTableDimensions)
        );
    }
}

#[test]
fn table_track_mutations_respect_locks_and_reject_non_tables() {
    let (mut session, object_id) = table_session(formatted_table(3, 3));
    let locked = session
        .execute(
            session.revision(),
            Transaction::single(
                "Lock table",
                Command::SetObjectLock {
                    object_ids: vec![object_id.clone()],
                    locked: true,
                },
            ),
        )
        .expect("lock table");
    for command in [
        Command::InsertTableTrack {
            object_id: object_id.clone(),
            axis: TableTrackAxis::Row,
            index: 1,
        },
        Command::DeleteTableTrack {
            object_id: object_id.clone(),
            axis: TableTrackAxis::Column,
            index: 1,
        },
    ] {
        assert_eq!(
            session
                .execute(
                    locked.revision,
                    Transaction::single("Blocked mutation", command)
                )
                .expect_err("locked table rejects mutation"),
            CommandError::ObjectLocked(object_id.clone())
        );
    }

    let mut shapes = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = shapes.document().pages[0].id.clone();
    let added = shapes
        .execute(
            shapes.revision(),
            Transaction::single(
                "Add shape",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: rectangle("Not a table", 0, 0),
                },
            ),
        )
        .expect("add shape");
    let shape_id = added.created_objects[0].clone();
    for command in [
        Command::InsertTableTrack {
            object_id: shape_id.clone(),
            axis: TableTrackAxis::Column,
            index: 0,
        },
        Command::DeleteTableTrack {
            object_id: shape_id.clone(),
            axis: TableTrackAxis::Row,
            index: 0,
        },
    ] {
        assert_eq!(
            shapes
                .execute(
                    added.revision,
                    Transaction::single("Reject wrong kind", command)
                )
                .expect_err("shape is not a table"),
            CommandError::ObjectKindMismatch(shape_id.clone())
        );
    }
}

#[test]
fn indexed_table_mutations_save_and_reopen_exactly() {
    let (mut session, object_id) = table_session(formatted_table(3, 3));
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Change word-search structure".to_owned(),
                commands: vec![
                    Command::InsertTableTrack {
                        object_id: object_id.clone(),
                        axis: TableTrackAxis::Row,
                        index: 0,
                    },
                    Command::DeleteTableTrack {
                        object_id,
                        axis: TableTrackAxis::Column,
                        index: 1,
                    },
                ],
            },
        )
        .expect("mutate table");
    let expected = first_table(&session).clone();
    let temporary = TempDirectory::new("indexed-table-mutations");
    let path = temporary.path().join("table.publisher-v2");
    save_new_package(
        &path,
        &ProjectSnapshot::from_session(&session),
        &AssetStore::default(),
    )
    .expect("save table package");
    let reopened = open_package(&path).expect("reopen table package");
    let ObjectKind::Table { table } = &reopened.snapshot.document.pages[0].objects[0].kind else {
        panic!("reopened object remains a table");
    };
    assert_eq!(table, &expected);
}

#[test]
fn existing_dimension_resize_still_appends_and_truncates_at_the_bottom_and_right() {
    let (mut session, object_id) = table_session(formatted_table(3, 3));
    let reduced = session
        .execute(
            session.revision(),
            Transaction::single(
                "Reduce table",
                Command::UpdateTableDimensions {
                    object_id: object_id.clone(),
                    rows: 2,
                    columns: 2,
                },
            ),
        )
        .expect("reduce table");
    assert_eq!(first_table(&session).cells, ["A", "B", "D", "E"]);
    assert_eq!(first_table(&session).row_weights, [100, 200]);
    assert_eq!(first_table(&session).column_weights, [400, 500]);

    session
        .execute(
            reduced.revision,
            Transaction::single(
                "Grow table",
                Command::UpdateTableDimensions {
                    object_id,
                    rows: 3,
                    columns: 3,
                },
            ),
        )
        .expect("grow table");
    assert_eq!(
        first_table(&session).cells,
        ["A", "B", "", "D", "E", "", "", "", ""]
    );
    assert_eq!(first_table(&session).row_weights, [100, 200, 1_000]);
    assert_eq!(first_table(&session).column_weights, [400, 500, 1_000]);
}
