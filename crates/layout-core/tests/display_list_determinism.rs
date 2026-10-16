use std::path::{Path, PathBuf};

use document_core::model::{
    Color, DocumentId, FontFaceBinding, InlineTextSpan, InlineTextStyle, Insets, ObjectDraft,
    ObjectKind, ParagraphListStyle, ParagraphRange, ParagraphStyle, ParagraphStyleRun,
    PhysicalSize, Revision, TableCellFormatting, TableData, TableGridBorder, TableTrackAxis,
    TableVerticalAlignment, TextAlignment, TextFrame, TextSourceRange, TextStoryFormatting,
    TextStyle, Transform,
};
use document_core::{
    AssetHash, AssetStore, Command, Document, DocumentSession, ObjectId, ObjectLocation,
    ProjectSnapshot, Transaction, open_package, save_new_package,
};
use layout_core::{
    ExactFontResource, FontSubstitutionStatus, ResolvedPublication, ResolvedSceneObjectKind,
};

const FONT_BYTES: &[u8] = include_bytes!("fixtures/DejaVuSans.ttf");

struct CorpusFixture {
    snapshot: ProjectSnapshot,
    assets: AssetStore,
    font_resource: ExactFontResource,
    text_id: ObjectId,
    table_id: ObjectId,
    story: String,
    clipped_cell_text: String,
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "publisher-v2-{label}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("remove stale deterministic-test directory");
        }
        std::fs::create_dir(&path).expect("create deterministic-test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}

fn bound_style(source: &AssetHash, font_size_millipoints: u32) -> TextStyle {
    TextStyle {
        font_face: Some(FontFaceBinding {
            source: source.clone(),
            face_index: 0,
        }),
        font_family: "DejaVu Sans fixture".to_owned(),
        font_size_millipoints,
        ..TextStyle::default()
    }
}

fn source_range(text: &str, needle: &str, include_trailing_space: bool) -> TextSourceRange {
    let start = text.find(needle).expect("fixture token");
    TextSourceRange {
        start,
        end: start + needle.len() + usize::from(include_trailing_space),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one cross-layer fixture intentionally keeps rich text, list, table, and indexed-track facts together"
)]
fn corpus_fixture() -> CorpusFixture {
    let mut assets = AssetStore::default();
    let source = assets.insert("font/ttf", FONT_BYTES.to_vec());
    let font_resource = ExactFontResource {
        source: source.clone(),
        face_index: 0,
        bytes: FONT_BYTES.to_vec(),
    };
    let document = Document::new(
        DocumentId::parse("u3-display-list-corpus").unwrap(),
        "U3 display-list corpus",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let story =
        "Ordinary Bold Italic Off\nBullet café\nFirst numbered\nInterruption\nRestart numbered"
            .to_owned();
    let rich_story = TextStoryFormatting {
        inline_spans: vec![
            InlineTextSpan {
                range: source_range(&story, "Bold", true),
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(true),
                    italic: None,
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: source_range(&story, "Italic", true),
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: None,
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: source_range(&story, "Off", false),
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(false),
                    italic: Some(false),
                    ..InlineTextStyle::default()
                },
            },
        ],
        paragraph_runs: vec![
            ParagraphStyleRun {
                range: ParagraphRange { start: 0, end: 1 },
                style: ParagraphStyle {
                    alignment: Some(TextAlignment::Right),
                    start_indent_um: Some(3_000),
                    end_indent_um: Some(4_000),
                    first_line_indent_um: Some(2_000),
                    line_height_per_thousand: Some(1_300),
                    space_after_millipoints: Some(2_500),
                    ..ParagraphStyle::default()
                },
            },
            ParagraphStyleRun {
                range: ParagraphRange { start: 1, end: 2 },
                style: ParagraphStyle {
                    list_style: Some(ParagraphListStyle::Bullet),
                    start_indent_um: Some(5_000),
                    first_line_indent_um: Some(-2_000),
                    space_before_millipoints: Some(1_500),
                    space_after_millipoints: Some(2_000),
                    ..ParagraphStyle::default()
                },
            },
            ParagraphStyleRun {
                range: ParagraphRange { start: 2, end: 3 },
                style: ParagraphStyle {
                    list_style: Some(ParagraphListStyle::Numbered { start_at: 7 }),
                    ..ParagraphStyle::default()
                },
            },
            ParagraphStyleRun {
                range: ParagraphRange { start: 3, end: 4 },
                style: ParagraphStyle {
                    list_style: Some(ParagraphListStyle::None),
                    alignment: Some(TextAlignment::Center),
                    ..ParagraphStyle::default()
                },
            },
            ParagraphStyleRun {
                range: ParagraphRange { start: 4, end: 5 },
                style: ParagraphStyle {
                    list_style: Some(ParagraphListStyle::Numbered { start_at: 7 }),
                    alignment: Some(TextAlignment::Right),
                    ..ParagraphStyle::default()
                },
            },
        ],
    };

    let clipped_cell_text =
        "E magic instructions overflow the deliberately short cell. ".repeat(12);
    let mut table = TableData::empty(3, 3).unwrap();
    table.cells = vec![
        "A".to_owned(),
        "B".to_owned(),
        "C".to_owned(),
        "D".to_owned(),
        clipped_cell_text.clone(),
        "F".to_owned(),
        "G".to_owned(),
        "H".to_owned(),
        "I".to_owned(),
    ];
    table.style = bound_style(&source, 10_000);
    table.grid_border = TableGridBorder {
        color: Color {
            red: 18,
            green: 42,
            blue: 66,
            alpha: 255,
        },
        width_millipoints: 1_500,
    };
    table.row_weights = vec![1_000, 2_000, 3_000];
    table.column_weights = vec![4_000, 5_000, 6_000];
    table.cell_formatting = vec![
        TableCellFormatting {
            row: 0,
            column: 1,
            horizontal_alignment: Some(TextAlignment::Right),
            vertical_alignment: Some(TableVerticalAlignment::Bottom),
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 1,
            column: 1,
            horizontal_alignment: Some(TextAlignment::Left),
            vertical_alignment: Some(TableVerticalAlignment::Top),
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 1,
            column: 2,
            horizontal_alignment: Some(TextAlignment::Center),
            vertical_alignment: Some(TableVerticalAlignment::Middle),
            ..TableCellFormatting::default()
        },
    ];

    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add deterministic text and table".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Rich instructions".to_owned(),
                            size: PhysicalSize::new(170_000, 150_000).unwrap(),
                            transform: Transform::identity_at(10_000, 10_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: story.clone(),
                                style: bound_style(&source, 12_000),
                                frame: TextFrame::default(),
                                story: rich_story,
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Mutated word search".to_owned(),
                            size: PhysicalSize::new(120_000, 60_000).unwrap(),
                            transform: Transform::identity_at(20_000, 180_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Table { table },
                        },
                    },
                ],
            },
        )
        .unwrap();
    let text_id = added.created_objects[0].clone();
    let table_id = added.created_objects[1].clone();
    session
        .execute(
            added.revision,
            Transaction {
                label: "Mutate indexed table tracks".to_owned(),
                commands: vec![
                    Command::InsertTableTrack {
                        object_id: table_id.clone(),
                        axis: TableTrackAxis::Row,
                        index: 1,
                    },
                    Command::InsertTableTrack {
                        object_id: table_id.clone(),
                        axis: TableTrackAxis::Column,
                        index: 2,
                    },
                    Command::DeleteTableTrack {
                        object_id: table_id.clone(),
                        axis: TableTrackAxis::Row,
                        index: 3,
                    },
                    Command::DeleteTableTrack {
                        object_id: table_id.clone(),
                        axis: TableTrackAxis::Column,
                        index: 0,
                    },
                ],
            },
        )
        .unwrap();

    CorpusFixture {
        snapshot: ProjectSnapshot::from_session(&session),
        assets,
        font_resource,
        text_id,
        table_id,
        story,
        clipped_cell_text,
    }
}

fn resolve(snapshot: &ProjectSnapshot, font: &ExactFontResource) -> ResolvedPublication {
    ResolvedPublication::resolve_with_font_resources(snapshot, core::slice::from_ref(font)).unwrap()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the assertions pin one coherent durable-to-display-list determinism contract"
)]
fn rich_text_lists_and_mutated_table_resolve_exactly_across_reopen() {
    let fixture = corpus_fixture();
    let first = resolve(&fixture.snapshot, &fixture.font_resource);
    let second = resolve(&fixture.snapshot, &fixture.font_resource);
    assert_eq!(
        first, second,
        "repeated resolution must be structurally exact"
    );
    assert_eq!(first.semantic_digest(), second.semantic_digest());

    let temporary = TemporaryDirectory::new("display-list-determinism");
    let package = temporary.path().join("corpus.publisher-v2");
    save_new_package(&package, &fixture.snapshot, &fixture.assets).unwrap();
    let reopened = open_package(&package).unwrap();
    assert_eq!(reopened.snapshot, fixture.snapshot);
    assert_eq!(reopened.assets, fixture.assets);
    let reopened_font = ExactFontResource {
        source: fixture.font_resource.source.clone(),
        face_index: 0,
        bytes: reopened
            .assets
            .get(&fixture.font_resource.source)
            .expect("packaged exact font")
            .bytes
            .clone(),
    };
    let after_reopen = resolve(&reopened.snapshot, &reopened_font);
    assert_eq!(
        first, after_reopen,
        "reopen must preserve the exact display list"
    );
    assert_eq!(first.semantic_digest(), after_reopen.semantic_digest());

    let page = &first.pages()[0];
    assert_eq!(
        page.objects
            .iter()
            .map(|object| &object.object_id)
            .collect::<Vec<_>>(),
        vec![&fixture.text_id, &fixture.table_id]
    );
    let ResolvedSceneObjectKind::Text { layout, .. } = &page.objects[0].kind else {
        panic!("first object must remain rich text");
    };
    assert_eq!(
        layout.font_binding.substitution_status,
        FontSubstitutionStatus::Unbound,
        "the compatibility status aggregates uncaptured inline faces"
    );
    assert_eq!(layout.lines.len(), 5);
    assert!(!layout.overset);

    let mut marker_text = Vec::<(u32, String)>::new();
    for run in layout.lines.iter().flat_map(|line| &line.glyph_runs) {
        let Some(paragraph) = run.list_marker_paragraph else {
            continue;
        };
        if let Some((previous, text)) = marker_text.last_mut()
            && *previous == paragraph
        {
            text.push_str(&run.text);
        } else {
            marker_text.push((paragraph, run.text.clone()));
        }
    }
    assert_eq!(
        marker_text,
        vec![
            (1, "•".to_owned()),
            (2, "7.".to_owned()),
            (4, "7.".to_owned())
        ]
    );

    let span_runs = |needle: &str| {
        let range = source_range(&fixture.story, needle, needle != "Off");
        layout
            .lines
            .iter()
            .flat_map(|line| &line.glyph_runs)
            .filter(|run| {
                run.list_marker_paragraph.is_none()
                    && run.source.start >= range.start
                    && run.source.end <= range.end
            })
            .collect::<Vec<_>>()
    };
    for needle in ["Bold", "Italic"] {
        assert!(span_runs(needle).into_iter().all(|run| {
            layout.resolved_fonts[run.font_index].sha256_hex
                != fixture.font_resource.source.as_str()
        }));
    }
    assert!(span_runs("Off").into_iter().all(|run| {
        !run.requires_font_synthesis
            && layout.resolved_fonts[run.font_index].sha256_hex
                == fixture.font_resource.source.as_str()
    }));
    let first_glyph_run = layout.lines[0]
        .glyph_runs
        .iter()
        .find(|run| run.list_marker_paragraph.is_none())
        .unwrap();
    assert_eq!(first_glyph_run.source.start, 0);
    assert_eq!(first_glyph_run.text, "O");
    assert_eq!(first_glyph_run.glyphs[0].id, 50);
    // Bold/italic are intentionally unbound in this compatibility fixture.
    // Their machine-local metrics affect the right-aligned line's origin and
    // baseline. Exact repeated/reopened scene equality above remains required;
    // portable geometry assertions use the resolved line rather than one host.
    let first_line = &layout.lines[0];
    let first_glyph = &first_glyph_run.glyphs[0];
    assert!(first_glyph.x.value() >= first_line.x.value());
    assert!(
        first_glyph.x.value() + first_glyph.advance.value()
            <= first_line.x.value() + first_line.width.value()
    );
    assert_eq!(first_glyph.y, first_line.baseline);
    assert_eq!(
        (
            layout.lines[0].x.value(),
            layout.lines[0].width.value(),
            layout.lines[0].height.value()
        ),
        (14_172, 456_379, 15_600)
    );
    assert!(first_line.baseline.value() >= first_line.y.value());
    assert!(first_line.baseline.value() <= first_line.y.value() + first_line.height.value());

    let ResolvedSceneObjectKind::Table { table } = &page.objects[1].kind else {
        panic!("second object must remain the mutated table");
    };
    assert_eq!((table.rows, table.columns), (3, 3));
    assert_eq!(table.row_boundaries_um, vec![0, 15_000, 30_000, 60_000]);
    assert_eq!(table.column_boundaries_um, vec![0, 50_000, 60_000, 120_000]);
    assert_eq!(
        table
            .populated_cells
            .iter()
            .map(|cell| (cell.row, cell.column, cell.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (0, 0, "B"),
            (0, 2, "C"),
            (2, 0, fixture.clipped_cell_text.as_str()),
            (2, 2, "F"),
        ]
    );
    assert_eq!(
        table
            .populated_cells
            .iter()
            .map(|cell| cell.clipped)
            .collect::<Vec<_>>(),
        vec![false, false, true, false]
    );
    assert_eq!(
        table.populated_cells[0].layout.lines[0].glyph_runs[0].glyphs[0].id,
        37
    );
    let table_glyph = &table.populated_cells[0].layout.lines[0].glyph_runs[0].glyphs[0];
    assert_eq!(
        (table_glyph.x.value(), table_glyph.y.value()),
        (132_036, 38_564)
    );
}
