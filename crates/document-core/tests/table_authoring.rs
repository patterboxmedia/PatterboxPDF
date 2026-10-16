mod common;
use common::{TempDirectory, letter_document};
use document_core::model::{
    Color, InlineTextStyle, ModelError, ObjectDraft, ObjectId, ObjectKind, PhysicalSize,
    TableCellBorders, TableCellFormattingPatch, TableCellRange, TableData, TableGridBorder,
    TableTrackAxis, Transform,
};
use document_core::{
    AssetStore, Command, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
    open_package, save_new_package,
};

fn range(start_row: u16, end_row: u16, start_column: u16, end_column: u16) -> TableCellRange {
    TableCellRange {
        start_row,
        end_row,
        start_column,
        end_column,
    }
}
fn fixture() -> (DocumentSession, ObjectId) {
    let mut table = TableData::empty(3, 3).unwrap();
    table.cells = vec![
        "first\nline",
        "second",
        "outside",
        "third",
        "fourth",
        "keep",
        "last",
        "row",
        "safe",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let result = session
        .execute(
            session.revision(),
            Transaction::single(
                "Add Table",
                Command::AddObject {
                    location: ObjectLocation::Page(session.document().pages[0].id.clone()),
                    object: ObjectDraft {
                        name: "Table".into(),
                        size: PhysicalSize::new(90_000, 90_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();
    (session, result.created_objects[0].clone())
}
fn table(session: &DocumentSession) -> &TableData {
    let ObjectKind::Table { table } = &session.document().pages[0].objects[0].kind else {
        panic!("Table")
    };
    table
}
fn apply(session: &mut DocumentSession, command: Command) {
    session
        .execute(
            session.revision(),
            Transaction::single("Edit Table", command),
        )
        .unwrap();
}
fn appearance() -> TableCellFormattingPatch {
    let color = Color {
        red: 70,
        green: 20,
        blue: 130,
        alpha: 180,
    };
    TableCellFormattingPatch {
        style: InlineTextStyle {
            bold: Some(true),
            color: Some(color),
            underline: Some(true),
            ..InlineTextStyle::default()
        },
        fill: Some(color),
        borders: TableCellBorders {
            bottom: Some(TableGridBorder {
                color,
                width_millipoints: 1_500,
            }),
            ..TableCellBorders::default()
        },
        ..TableCellFormattingPatch::default()
    }
}

#[test]
fn merge_split_preserves_multiline_text_and_sparse_appearance_with_undo_and_reopen() {
    let (mut session, id) = fixture();
    apply(
        &mut session,
        Command::UpdateTableCellFormatting {
            object_id: id.clone(),
            range: range(0, 1, 0, 1),
            formatting: appearance(),
        },
    );
    let original = table(&session).clone();
    let selected = range(0, 2, 0, 2);
    assert_eq!(
        table(&session).merge_preview(selected).unwrap(),
        "first\nline\nsecond\nthird\nfourth"
    );
    apply(
        &mut session,
        Command::MergeTableCells {
            object_id: id.clone(),
            range: selected,
        },
    );
    let merged = table(&session).clone();
    assert_eq!(merged.cells[0], "first\nline\nsecond\nthird\nfourth");
    assert_eq!(merged.cell_format(0, 0), original.cell_format(0, 0));
    assert_eq!(
        (&merged.cells[2], &merged.cells[5]),
        (&original.cells[2], &original.cells[5])
    );
    let directory = TempDirectory::new("table-merge");
    let path = directory.path().join("table.pubv2");
    let project = ProjectSnapshot::from_session(&session);
    save_new_package(&path, &project, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, project);
    session.undo(session.revision()).unwrap();
    assert_eq!(table(&session), &original);
    session.redo(session.revision()).unwrap();
    assert_eq!(table(&session), &merged);
    apply(
        &mut session,
        Command::UpdateTableCell {
            object_id: id.clone(),
            row: 0,
            column: 0,
            text: "edited\ncombined".into(),
        },
    );
    apply(
        &mut session,
        Command::SplitTableCells {
            object_id: id,
            range: selected,
        },
    );
    assert!(table(&session).merged_cells.is_empty());
    assert_eq!(table(&session).cells[0], "edited\ncombined");
    assert!(
        [1, 3, 4]
            .into_iter()
            .all(|index| table(&session).cells[index].is_empty())
    );
    assert_eq!(
        table(&session).cell_format(0, 0),
        original.cell_format(0, 0)
    );
}

#[test]
fn partial_merge_edits_and_invalid_appearance_are_atomic_refusals() {
    let (mut session, id) = fixture();
    let selected = range(0, 2, 0, 2);
    apply(
        &mut session,
        Command::MergeTableCells {
            object_id: id.clone(),
            range: selected,
        },
    );
    let before = session.document().clone();
    let revision = session.revision();
    let commands = vec![
        Command::SplitTableCells {
            object_id: id.clone(),
            range: range(0, 1, 0, 2),
        },
        Command::MergeTableCells {
            object_id: id.clone(),
            range: range(1, 3, 1, 3),
        },
        Command::UpdateTableCell {
            object_id: id.clone(),
            row: 1,
            column: 1,
            text: "hidden".into(),
        },
        Command::UpdateTableCells {
            object_id: id.clone(),
            row: 0,
            column: 1,
            cells: vec![vec!["partial".into()]],
        },
        Command::UpdateTableCellFormatting {
            object_id: id.clone(),
            range: range(0, 4, 0, 1),
            formatting: appearance(),
        },
        Command::UpdateTableCellFormatting {
            object_id: id.clone(),
            range: selected,
            formatting: TableCellFormattingPatch {
                style: InlineTextStyle {
                    font_weight: Some(0),
                    ..InlineTextStyle::default()
                },
                ..TableCellFormattingPatch::default()
            },
        },
    ];
    for command in commands {
        assert!(
            session
                .execute(revision, Transaction::single("Refuse", command))
                .is_err()
        );
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), revision);
    }
    apply(
        &mut session,
        Command::UpdateTableCells {
            object_id: id,
            row: 0,
            column: 0,
            cells: vec![vec!["a".into(), "b".into()], vec!["c".into(), "d".into()]],
        },
    );
    assert!(table(&session).merged_cells.is_empty());
    assert_eq!(table(&session).cells[4], "d");
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &before);
}

#[test]
fn deleting_a_merged_anchor_track_keeps_edited_text_and_anchor_formatting() {
    for axis in [TableTrackAxis::Row, TableTrackAxis::Column] {
        let (mut session, id) = fixture();
        apply(
            &mut session,
            Command::UpdateTableCellFormatting {
                object_id: id.clone(),
                range: range(0, 1, 0, 1),
                formatting: appearance(),
            },
        );
        apply(
            &mut session,
            Command::MergeTableCells {
                object_id: id.clone(),
                range: range(0, 2, 0, 2),
            },
        );
        apply(
            &mut session,
            Command::UpdateTableCell {
                object_id: id.clone(),
                row: 0,
                column: 0,
                text: "edited merged anchor".into(),
            },
        );
        let before = table(&session).clone();
        apply(
            &mut session,
            Command::DeleteTableTrack {
                object_id: id,
                axis,
                index: 0,
            },
        );
        assert!(table(&session).merged_cells.is_empty());
        assert_eq!(table(&session).cells[0], "edited merged anchor");
        assert_eq!(table(&session).cell_format(0, 0), before.cell_format(0, 0));
        session.undo(session.revision()).unwrap();
        assert_eq!(table(&session), &before);
    }
}

#[test]
fn sparse_weight_reset_and_default_encoding_preserve_legacy_table_semantics() {
    let (mut session, id) = fixture();
    let selected = range(0, 1, 0, 1);
    apply(
        &mut session,
        Command::UpdateTableCellFormatting {
            object_id: id.clone(),
            range: selected,
            formatting: appearance(),
        },
    );
    apply(
        &mut session,
        Command::UpdateTableCellFormatting {
            object_id: id.clone(),
            range: selected,
            formatting: TableCellFormattingPatch {
                style: InlineTextStyle {
                    font_weight: Some(400),
                    underline: Some(false),
                    ..InlineTextStyle::default()
                },
                ..TableCellFormattingPatch::default()
            },
        },
    );
    assert_eq!(table(&session).effective_cell_style(0, 0).font_weight, 400);
    assert_eq!(table(&session).cell_format(0, 0).unwrap().style.bold, None);
    apply(
        &mut session,
        Command::ResetTableCellFormatting {
            object_id: id,
            range: selected,
        },
    );
    assert!(table(&session).cell_formatting.is_empty());
    let encoded = serde_json::to_string(table(&session)).unwrap();
    assert!(!encoded.contains("merged_cells"));
    let invalid = TableCellRange {
        start_row: 0,
        end_row: 0,
        start_column: 0,
        end_column: 1,
    };
    assert_eq!(
        invalid.validate(table(&session)),
        Err(ModelError::InvalidTableCellRange)
    );
}
