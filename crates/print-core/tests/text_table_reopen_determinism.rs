use std::path::{Path, PathBuf};

use document_core::model::{
    Color, DocumentId, FontFaceBinding, InlineTextSpan, InlineTextStyle, Insets, ObjectDraft,
    ObjectKind, ParagraphListStyle, ParagraphRange, ParagraphStyle, ParagraphStyleRun,
    PhysicalSize, Revision, ShapeKind, ShapeStroke, TableCellFormatting, TableData,
    TableGridBorder, TableTrackAxis, TableVerticalAlignment, TextAlignment, TextFrame,
    TextSourceRange, TextStoryFormatting, TextStyle, Transform, WrapMode,
};
use document_core::{
    AssetHash, AssetStore, Command, Document, DocumentSession, ObjectId, ObjectLocation,
    ProjectSnapshot, Transaction, open_package, save_new_package,
};
use layout_core::{ExactFontResource, ResolvedPublication, ResolvedSceneObjectKind};
use print_core::{
    FixtureContract, FontBoundary, PreflightBasis, PublicationCandidateFindingCode,
    PublicationCandidatePreflight, PublicationCandidateWriter, PublicationTextEvidence,
    SerializedTableCellAddress, inspect_output_intent,
};

const FONT_BYTES: &[u8] = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
const PROFILE: &[u8] = include_bytes!("fixtures/SWOP2006coated_300_mth.icc");

struct CorpusFixture {
    snapshot: ProjectSnapshot,
    assets: AssetStore,
    font: ExactFontResource,
    text_id: ObjectId,
    table_id: ObjectId,
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

fn bound_style(source: &AssetHash, size: u32) -> TextStyle {
    TextStyle {
        font_face: Some(FontFaceBinding {
            source: source.clone(),
            face_index: 0,
        }),
        font_family: "DejaVu Sans fixture".to_owned(),
        font_size_millipoints: size,
        ..TextStyle::default()
    }
}

fn source_range(text: &str, needle: &str, trailing_space: bool) -> TextSourceRange {
    let start = text.find(needle).expect("fixture token");
    TextSourceRange {
        start,
        end: start + needle.len() + usize::from(trailing_space),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the print fixture deliberately combines rich text, restarted lists, table formatting, and indexed mutations"
)]
fn corpus_fixture() -> CorpusFixture {
    let mut assets = AssetStore::default();
    let source = assets.insert("font/ttf", FONT_BYTES.to_vec());
    let font = ExactFontResource {
        source: source.clone(),
        face_index: 0,
        bytes: FONT_BYTES.to_vec(),
    };
    let document = Document::new(
        DocumentId::parse("u3-print-reopen-corpus").unwrap(),
        "U3 print reopen corpus",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let text =
        "Ordinary Bold Italic Off\nBullet café\nFirst numbered\nInterruption\nRestart numbered";
    let story = TextStoryFormatting {
        inline_spans: vec![
            InlineTextSpan {
                range: source_range(text, "Bold", true),
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(true),
                    italic: None,
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: source_range(text, "Italic", true),
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: None,
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: source_range(text, "Off", false),
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

    let mut table = TableData::empty(2, 2).unwrap();
    table.cells = vec![
        "B".to_owned(),
        "C".to_owned(),
        "E magic instructions overflow the deliberately short cell. ".repeat(12),
        "F".to_owned(),
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
    table.row_weights = vec![1_000, 2_000];
    table.column_weights = vec![5_000, 6_000];
    table.cell_formatting = vec![
        TableCellFormatting {
            row: 0,
            column: 0,
            horizontal_alignment: Some(TextAlignment::Right),
            vertical_alignment: Some(TableVerticalAlignment::Bottom),
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 1,
            column: 0,
            horizontal_alignment: Some(TextAlignment::Left),
            vertical_alignment: Some(TableVerticalAlignment::Top),
            ..TableCellFormatting::default()
        },
        TableCellFormatting {
            row: 1,
            column: 1,
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
                label: "Add print determinism corpus".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Rich instructions".to_owned(),
                            size: PhysicalSize::new(170_000, 150_000).unwrap(),
                            transform: Transform::identity_at(10_000, 10_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: text.to_owned(),
                                style: bound_style(&source, 12_000),
                                frame: TextFrame::default(),
                                story,
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Mutated table".to_owned(),
                            size: PhysicalSize::new(120_000, 30_000).unwrap(),
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
                label: "Round-trip indexed table tracks".to_owned(),
                commands: vec![
                    Command::InsertTableTrack {
                        object_id: table_id.clone(),
                        axis: TableTrackAxis::Row,
                        index: 1,
                    },
                    Command::InsertTableTrack {
                        object_id: table_id.clone(),
                        axis: TableTrackAxis::Column,
                        index: 1,
                    },
                    Command::DeleteTableTrack {
                        object_id: table_id.clone(),
                        axis: TableTrackAxis::Row,
                        index: 1,
                    },
                    Command::DeleteTableTrack {
                        object_id: table_id.clone(),
                        axis: TableTrackAxis::Column,
                        index: 1,
                    },
                ],
            },
        )
        .unwrap();

    CorpusFixture {
        snapshot: ProjectSnapshot::from_session(&session),
        assets,
        font,
        text_id,
        table_id,
    }
}

fn resolve(snapshot: &ProjectSnapshot, font: &ExactFontResource) -> ResolvedPublication {
    ResolvedPublication::resolve_with_font_resources(snapshot, core::slice::from_ref(font)).unwrap()
}

fn fixture_contract(publication: &ResolvedPublication) -> FixtureContract {
    FixtureContract::new(
        PreflightBasis::from(publication.clone()),
        inspect_output_intent(PROFILE).unwrap(),
        FontBoundary::NotExercised,
    )
}

fn without_synthetic_inline_styles(
    mut snapshot: ProjectSnapshot,
    text_id: &ObjectId,
) -> ProjectSnapshot {
    let text_object = snapshot.document.pages[0]
        .objects
        .iter_mut()
        .find(|object| &object.id == text_id)
        .expect("rich text object");
    let ObjectKind::Text { story, .. } = &mut text_object.kind else {
        panic!("fixture text object remains text");
    };
    story.inline_spans.clear();
    snapshot
}

#[test]
fn reopened_text_and_table_keep_exact_scene_and_supported_pdf_bytes() {
    let corpus = corpus_fixture();
    let first = resolve(&corpus.snapshot, &corpus.font);
    let repeat = resolve(&corpus.snapshot, &corpus.font);
    assert_eq!(first, repeat);

    let temporary = TemporaryDirectory::new("print-reopen-determinism");
    let package = temporary.path().join("corpus.publisher-v2");
    save_new_package(&package, &corpus.snapshot, &corpus.assets).unwrap();
    let reopened = open_package(&package).unwrap();
    assert_eq!(reopened.snapshot, corpus.snapshot);
    let reopened_font = ExactFontResource {
        source: corpus.font.source.clone(),
        face_index: 0,
        bytes: reopened
            .assets
            .get(&corpus.font.source)
            .expect("reopened exact font")
            .bytes
            .clone(),
    };
    let reopened_scene = resolve(&reopened.snapshot, &reopened_font);
    assert_eq!(first, reopened_scene);
    assert_eq!(first.semantic_digest(), reopened_scene.semantic_digest());

    let styled_contract = fixture_contract(&first);
    let styled_candidate = PublicationCandidateWriter::write(&first, &styled_contract, PROFILE)
        .expect("diagnostic candidate preserves unresolved inline-font evidence");
    let reopened_styled_candidate =
        PublicationCandidateWriter::write(&reopened_scene, &styled_contract, PROFILE)
            .expect("reopened diagnostic candidate");
    assert_eq!(
        styled_candidate, reopened_styled_candidate,
        "the diagnostic styled candidate must survive reopen deterministically"
    );
    let styled_preflight =
        PublicationCandidatePreflight::for_candidate(&first, &styled_contract, &styled_candidate);
    assert!(styled_preflight.has_blockers());
    assert!(!styled_preflight.ready_for_independent_validation());
    assert!(
        styled_preflight.findings.iter().any(|finding| finding.code
            == PublicationCandidateFindingCode::FontEmbeddingUnresolved),
        "the unresolved inline faces must remain actionable preflight blockers"
    );

    let printable_before =
        without_synthetic_inline_styles(corpus.snapshot.clone(), &corpus.text_id);
    let printable_after = without_synthetic_inline_styles(reopened.snapshot, &corpus.text_id);
    let printable_scene = resolve(&printable_before, &corpus.font);
    let printable_reopened_scene = resolve(&printable_after, &reopened_font);
    assert_eq!(printable_scene, printable_reopened_scene);
    let printable_contract = fixture_contract(&printable_scene);
    let first_pdf =
        PublicationCandidateWriter::write(&printable_scene, &printable_contract, PROFILE).unwrap();
    let repeat_pdf =
        PublicationCandidateWriter::write(&printable_scene, &printable_contract, PROFILE).unwrap();
    let reopened_pdf =
        PublicationCandidateWriter::write(&printable_reopened_scene, &printable_contract, PROFILE)
            .unwrap();
    assert_eq!(first_pdf, repeat_pdf);
    assert_eq!(first_pdf, reopened_pdf);
    assert_eq!(first_pdf.bytes(), reopened_pdf.bytes());
    assert_eq!(
        first_pdf.evidence().scene_digest,
        printable_scene.semantic_digest()
    );
    assert_eq!(
        first_pdf.evidence().text,
        PublicationTextEvidence::NativeOutlines { source_count: 5 }
    );
    assert_eq!(
        first_pdf.evidence().pages[0].object_ids_in_paint_order,
        vec![
            corpus.text_id.as_str().to_owned(),
            corpus.table_id.as_str().to_owned()
        ]
    );
    assert_eq!(
        first_pdf.evidence().pages[0].clipped_table_cells,
        vec![SerializedTableCellAddress {
            object_id: corpus.table_id.as_str().to_owned(),
            row: 1,
            column: 0,
        }]
    );
    assert!(first_pdf.bytes().starts_with(b"%PDF-"));
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one acceptance journey deliberately joins instructional text, wrapping, a word-search table, reopen, PDF, and preflight"
)]
fn instructional_page_and_word_search_survive_reflow_undo_reopen_and_preflight() {
    let mut assets = AssetStore::default();
    let source = assets.insert("font/ttf", FONT_BYTES.to_vec());
    let font = ExactFontResource {
        source: source.clone(),
        face_index: 0,
        bytes: FONT_BYTES.to_vec(),
    };
    let document = Document::new(
        DocumentId::parse("u6-instruction-word-search").unwrap(),
        "U6 instruction and word search acceptance",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut table = TableData::empty(4, 4).unwrap();
    table.cells = [
        "M", "A", "G", "I", "A", "B", "O", "C", "G", "O", "O", "K", "I", "C", "K", "S",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    table.style = bound_style(&source, 11_000);
    table.grid_border = TableGridBorder {
        color: Color {
            red: 18,
            green: 42,
            blue: 66,
            alpha: 255,
        },
        width_millipoints: 1_000,
    };
    table.cell_formatting = vec![TableCellFormatting {
        row: 3,
        column: 3,
        horizontal_alignment: Some(TextAlignment::Right),
        vertical_alignment: Some(TableVerticalAlignment::Bottom),
        ..TableCellFormatting::default()
    }];
    let instructions =
        "Hold the cards carefully and show both hands. Pause before the surprise. ".repeat(42);
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Compose U6 acceptance page".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Magic instructions".to_owned(),
                            size: PhysicalSize::new(120_000, 55_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: instructions,
                                style: bound_style(&source, 12_000),
                                frame: TextFrame {
                                    column_count: 2,
                                    column_gap_um: 8_000,
                                    ..TextFrame::default()
                                },
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Wrapped trick diagram".to_owned(),
                            size: PhysicalSize::new(18_000, 20_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Shape {
                                shape: ShapeKind::Rectangle,
                                fill: Color {
                                    red: 230,
                                    green: 220,
                                    blue: 180,
                                    alpha: 255,
                                },
                                stroke: ShapeStroke::default(),
                                wrap_mode: WrapMode::Rectangular,
                            },
                        },
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: ObjectDraft {
                            name: "Word search".to_owned(),
                            size: PhysicalSize::new(72_000, 72_000).unwrap(),
                            transform: Transform::identity_at(132_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Table { table },
                        },
                    },
                ],
            },
        )
        .unwrap();
    let text_id = added.created_objects[0].clone();
    let diagram_id = added.created_objects[1].clone();
    let table_id = added.created_objects[2].clone();
    let wrapped = resolve(&ProjectSnapshot::from_session(&session), &font);
    let wrapped_text = wrapped.text_boxes()[0];
    assert!(wrapped_text.overset);
    assert_eq!(
        wrapped_text.sizing_mode,
        document_core::model::TextBoxSizingMode::Fixed
    );
    let wrapped_first_x = wrapped_text.lines[0].x;
    let wrapped_overset_source = wrapped_text.overset_source.clone();

    let moved = session
        .execute(
            added.revision,
            Transaction::single(
                "Move wrapped diagram",
                Command::MoveObject {
                    object_id: diagram_id,
                    destination: ObjectLocation::Page(page_id),
                    transform: Transform::identity_at(190_000, 245_000),
                },
            ),
        )
        .unwrap();
    let reflowed = resolve(&ProjectSnapshot::from_session(&session), &font);
    let reflowed_text = reflowed.text_boxes()[0];
    assert_ne!(
        reflowed_text.lines[0].x, wrapped_first_x,
        "moving the diagram must change authoritative wrapped line geometry"
    );
    assert!(reflowed_text.overset);
    assert_ne!(
        reflowed_text.overset_source, wrapped_overset_source,
        "movement-driven reflow must update the exact unplaced source tail"
    );
    session.undo(moved.revision).unwrap();
    let restored_snapshot = ProjectSnapshot::from_session(&session);
    let restored = resolve(&restored_snapshot, &font);
    assert_eq!(restored.text_boxes()[0].lines[0].x, wrapped_first_x);

    let resolved_table = restored.pages()[0]
        .objects
        .iter()
        .find(|object| object.object_id == table_id)
        .and_then(|object| match &object.kind {
            ResolvedSceneObjectKind::Table { table } => Some(table),
            _ => None,
        })
        .expect("authoritative word-search table");
    assert_eq!((resolved_table.rows, resolved_table.columns), (4, 4));
    assert_eq!(resolved_table.populated_cells.len(), 16);
    assert!(
        resolved_table
            .populated_cells
            .iter()
            .all(|cell| !cell.clipped)
    );
    assert_eq!(resolved_table.grid_width_millipoints, 1_000);

    let temporary = TemporaryDirectory::new("u6-instruction-word-search");
    let package = temporary.path().join("u6-acceptance.publisher-v2");
    save_new_package(&package, &restored_snapshot, &assets).unwrap();
    let reopened = open_package(&package).unwrap();
    assert_eq!(reopened.snapshot, restored_snapshot);
    let reopened_scene = resolve(&reopened.snapshot, &font);
    assert_eq!(reopened_scene, restored);

    let contract = fixture_contract(&reopened_scene);
    let candidate = PublicationCandidateWriter::write(&reopened_scene, &contract, PROFILE).unwrap();
    assert!(candidate.bytes().starts_with(b"%PDF-"));
    assert!(candidate.evidence().pages[0].clipped_table_cells.is_empty());
    let preflight =
        PublicationCandidatePreflight::for_candidate(&reopened_scene, &contract, &candidate);
    let overflow = preflight
        .findings
        .iter()
        .find(|finding| finding.code == PublicationCandidateFindingCode::TextOverset)
        .expect("preflight must identify the same overset instructional Text Box");
    let target = overflow.target.as_ref().expect("overset repair target");
    assert_eq!(target.object_id.as_deref(), Some(text_id.as_str()));
    assert_eq!(
        target.page_id,
        reopened.snapshot.document.pages[0].id.as_str()
    );
    assert!(
        preflight
            .findings
            .iter()
            .all(|finding| { finding.code != PublicationCandidateFindingCode::TableCellOverset })
    );
}
