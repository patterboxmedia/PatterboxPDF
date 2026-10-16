#[path = "../../layout-core/tests/fixtures/table_authoring.rs"]
mod fixture;
use document_core::model::{
    Color, ExactFontFamilyBinding, ExactFontVariantBinding, FontFaceBinding, InlineTextStyle,
    ObjectKind, PhysicalSize, TableCellBorders, TableCellFormattingPatch, TableCellRange,
    TableGridBorder,
};
use document_core::{
    AssetStore, Command, DocumentSession, ProjectSnapshot, Transaction, open_package,
    save_new_package,
};
use fixture::instruction_page;
use layout_core::{
    ExactFontResource, FontSubstitutionStatus, PreviewBudgetState, ResolvedPublication,
    ResolvedSceneObjectKind, output_font_face_bindings,
};
#[cfg(windows)]
use print_core::{
    FixtureContract, FontBoundary, PreflightBasis, PublicationCandidateFindingCode,
    PublicationCandidatePreflight, PublicationCandidateWriter, inspect_output_intent,
};
use print_core::{ScenePdfOptions, ScenePdfWriter};
#[cfg(windows)]
const PROFILE: &[u8] = include_bytes!("fixtures/SWOP2006coated_300_mth.icc");

#[cfg(windows)]
fn pdf(scene: &ResolvedPublication) -> print_core::PublicationCandidatePdf {
    let contract = FixtureContract::new(
        PreflightBasis::from(scene.clone()),
        inspect_output_intent(PROFILE).unwrap(),
        FontBoundary::NotExercised,
    );
    PublicationCandidateWriter::write(scene, &contract, PROFILE).unwrap()
}

#[test]
fn full_grid_heading_and_list_survive_save_reopen_preflight_and_serialized_paint() {
    let (project, font) = instruction_page();
    let mut assets = AssetStore::default();
    assets.insert("font/ttf", font.bytes.clone());
    let directory =
        std::env::temp_dir().join(format!("publisher-table-capacity-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("grid.pubv2");
    if path.exists() {
        std::fs::remove_file(&path).unwrap();
    }
    save_new_package(&path, &project, &assets).unwrap();
    let reopened = open_package(&path).unwrap();
    let scene =
        ResolvedPublication::resolve_output_with_font_resources(&reopened.snapshot, &[font])
            .unwrap();
    assert_eq!(scene.budget_state(), PreviewBudgetState::Complete);
    let serialized = ScenePdfWriter::write(&scene, ScenePdfOptions::default()).unwrap();
    assert_eq!(serialized.evidence().text_object_count, 902);
    assert!(
        serialized.evidence().pages[0]
            .clipped_table_cells
            .is_empty()
    );
    #[cfg(windows)]
    {
        let candidate = pdf(&scene);
        assert_eq!(
            candidate.evidence().text,
            print_core::PublicationTextEvidence::NativeOutlines { source_count: 902 }
        );
        assert!(candidate.evidence().pages[0].clipped_table_cells.is_empty());
        let contract = FixtureContract::new(
            PreflightBasis::from(scene.clone()),
            inspect_output_intent(PROFILE).unwrap(),
            FontBoundary::NotExercised,
        );
        let preflight = PublicationCandidatePreflight::for_candidate(&scene, &contract, &candidate);
        assert!(preflight.findings.iter().all(|finding| !matches!(
            finding.code,
            PublicationCandidateFindingCode::PreviewBudgetIncomplete
                | PublicationCandidateFindingCode::TableCellOverset
                | PublicationCandidateFindingCode::TextOverset
        )));
    }
    std::fs::remove_dir_all(directory).unwrap();
}

fn border(color: Color, width_millipoints: u32) -> TableGridBorder {
    TableGridBorder {
        color,
        width_millipoints,
    }
}
fn font(bytes: &[u8]) -> ExactFontResource {
    ExactFontResource {
        source: document_core::AssetHash::for_bytes(bytes),
        face_index: 0,
        bytes: bytes.to_vec(),
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one output journey joins merged multiline text, exact cell faces, sparse paint and deterministic shared border precedence"
)]
fn merged_cell_fonts_colors_underlines_fills_and_unique_shared_borders_reach_output() {
    let (mut project, regular) = instruction_page();
    project.document.pages[0].objects.drain(..2);
    let object = &mut project.document.pages[0].objects[0];
    object.size = PhysicalSize::new(90_000, 70_000).unwrap();
    let ObjectKind::Table { table } = &mut object.kind else {
        panic!("Table")
    };
    *table = document_core::model::TableData::empty(2, 3).unwrap();
    table.cells = vec!["a", "b", "right", "c", "d", "last"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let bold = font(include_bytes!(
        "../../layout-core/tests/fixtures/DejaVuSans-Bold.ttf"
    ));
    let italic = font(include_bytes!(
        "../../layout-core/tests/fixtures/DejaVuSans-Oblique.ttf"
    ));
    let face = |font: &ExactFontResource| FontFaceBinding {
        source: font.source.clone(),
        face_index: 0,
    };
    table.style.font_face = Some(face(&regular));
    table.style.exact_font_variants = vec![ExactFontVariantBinding {
        font_weight: 700,
        italic: false,
        face: face(&bold),
    }];
    table.style.exact_font_families = vec![ExactFontFamilyBinding {
        font_family: "Alternate".into(),
        font_weight: 400,
        italic: true,
        face: face(&italic),
    }];
    let id = object.id.clone();
    let red = Color {
        red: 200,
        green: 20,
        blue: 30,
        alpha: 180,
    };
    let blue = Color {
        red: 20,
        green: 30,
        blue: 200,
        alpha: 255,
    };
    let range = TableCellRange {
        start_row: 0,
        end_row: 2,
        start_column: 0,
        end_column: 2,
    };
    let mut session = DocumentSession::from_saved(project.document, project.revision).unwrap();
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Format and merge cells".into(),
                commands: vec![
                    Command::UpdateTableCellFormatting {
                        object_id: id.clone(),
                        range: TableCellRange {
                            end_row: 1,
                            end_column: 1,
                            ..range
                        },
                        formatting: TableCellFormattingPatch {
                            style: InlineTextStyle {
                                bold: Some(true),
                                underline: Some(true),
                                color: Some(red),
                                ..InlineTextStyle::default()
                            },
                            fill: Some(red),
                            borders: TableCellBorders {
                                right: Some(border(red, 2_000)),
                                ..TableCellBorders::default()
                            },
                            ..TableCellFormattingPatch::default()
                        },
                    },
                    Command::UpdateTableCellFormatting {
                        object_id: id.clone(),
                        range: TableCellRange {
                            start_row: 0,
                            end_row: 1,
                            start_column: 2,
                            end_column: 3,
                        },
                        formatting: TableCellFormattingPatch {
                            style: InlineTextStyle {
                                font_family: Some("Alternate".into()),
                                italic: Some(true),
                                color: Some(blue),
                                ..InlineTextStyle::default()
                            },
                            borders: TableCellBorders {
                                left: Some(border(blue, 3_000)),
                                ..TableCellBorders::default()
                            },
                            ..TableCellFormattingPatch::default()
                        },
                    },
                    Command::MergeTableCells {
                        object_id: id,
                        range,
                    },
                ],
            },
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);
    let fonts = [regular, bold, italic];
    assert_eq!(output_font_face_bindings(&project).unwrap().len(), 3);
    let scene = ResolvedPublication::resolve_output_with_font_resources(&project, &fonts).unwrap();
    let ResolvedSceneObjectKind::Table { table } = &scene.pages()[0].objects[0].kind else {
        panic!("Table")
    };
    assert_eq!(table.populated_cells.len(), 3);
    assert_eq!(table.cell_fills.len(), 1);
    let anchor = &table.populated_cells[0];
    assert_eq!((anchor.row_span, anchor.column_span), (2, 2));
    assert_eq!(anchor.text, "a\nb\nc\nd");
    assert!(!anchor.clipped);
    assert_eq!(
        anchor.layout.font_binding.substitution_status,
        FontSubstitutionStatus::Exact
    );
    let runs = anchor
        .layout
        .lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .collect::<Vec<_>>();
    assert!(
        runs.iter()
            .filter(|run| !run.glyphs.is_empty())
            .all(|run| run.color == red && run.underline.is_some() && !run.requires_font_synthesis),
        "drawn run appearances: {:?}",
        runs.iter()
            .map(|run| (
                run.text.as_str(),
                run.color,
                &run.underline,
                run.requires_font_synthesis,
                run.glyphs.len()
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        anchor.layout.resolved_fonts[0].sha256_hex,
        fonts[1].source.as_str()
    );
    let neighbor = &table.populated_cells[1];
    assert_eq!(
        neighbor.layout.font_binding.substitution_status,
        FontSubstitutionStatus::Exact
    );
    assert_eq!(
        neighbor.layout.resolved_fonts[0].sha256_hex,
        fonts[2].source.as_str()
    );
    let shared = table
        .border_segments
        .iter()
        .filter(|edge| edge.x1_um == 60_000 && edge.x2_um == 60_000 && edge.y1_um == 0)
        .collect::<Vec<_>>();
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].color, blue);
    assert_eq!(shared[0].width_millipoints, 3_000);
    assert!(
        table
            .border_segments
            .iter()
            .all(|edge| !(edge.x1_um == 30_000 && edge.x2_um == 30_000))
    );
    let serialized = ScenePdfWriter::write(&scene, ScenePdfOptions::default()).unwrap();
    assert!(serialized.evidence().transparency.paint_alpha);
    assert!(
        serialized.evidence().pages[0]
            .clipped_table_cells
            .is_empty()
    );
    assert_eq!(serialized.evidence().text_object_count, 3);
    assert!(serialized.bytes().len() > 1_000);
}
