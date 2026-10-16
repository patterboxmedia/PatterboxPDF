use application_core::{
    ApplyTransactionInput, ColorInput, CreateDocumentInput, EditorCommandInput, EditorModule,
    EditorSnapshot, FontFaceBindingInput, ShapeInput, TextAlignmentInput, TextBoxSizingModeInput,
    TextStyleInput, WrapModeInput,
};
use document_core::model::{ObjectKind, TextFrame, Transform, WrapMode};
use layout_core::{
    ExactFontResource, FontSubstitutionStatus, ResolvedPublication, ResolvedTextBoxPreview,
    preview_font_face_bindings,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "publisher-v2-instructional-page-{}-{sequence}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("remove stale test directory");
        }
        std::fs::create_dir(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}

fn transaction(
    expected_revision: u64,
    label: &str,
    commands: Vec<EditorCommandInput>,
) -> ApplyTransactionInput {
    ApplyTransactionInput {
        expected_revision,
        label: label.to_owned(),
        commands,
    }
}

const INSTRUCTIONS: &str = "THE JUMPING RUBBER BAND\n\nPlace the band across two fingers.\nClose your hand, then open it slowly.";

struct FixtureIds {
    page: String,
    text: String,
    diagram: String,
    pasteboard: String,
}

fn assemble_instructional_page(editor: &mut EditorModule) -> (EditorSnapshot, FixtureIds) {
    let created = editor
        .create(CreateDocumentInput {
            document_id: "instructional-page".to_owned(),
            title: "Magic Instructions".to_owned(),
            trim_width_um: 139_700,
            trim_height_um: 215_900,
            bleed_um: 3_175,
            page_count: 1,
        })
        .expect("create instructional publication");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let assembled = editor
        .apply(transaction(
            0,
            "Assemble instructional page",
            vec![
                EditorCommandInput::AddText {
                    page_id: Some(page_id.clone()),
                    name: "Trick instructions".to_owned(),
                    text: INSTRUCTIONS.to_owned(),
                    style: None,
                    sizing_mode: TextBoxSizingModeInput::Fixed,
                    x_um: 12_000,
                    y_um: 18_000,
                    width_um: 115_000,
                    height_um: 175_000,
                },
                EditorCommandInput::AddShape {
                    page_id: Some(page_id.clone()),
                    name: "Wrapped hand diagram".to_owned(),
                    shape: ShapeInput::Rectangle,
                    x_um: 20_000,
                    y_um: 40_000,
                    width_um: 35_000,
                    height_um: 30_000,
                    fill: ColorInput {
                        red: 230,
                        green: 230,
                        blue: 230,
                        alpha: 255,
                    },
                    stroke: None,
                    corner_rounding: None,
                },
                EditorCommandInput::AddShape {
                    page_id: None,
                    name: "Alternate diagram on pasteboard".to_owned(),
                    shape: ShapeInput::Ellipse,
                    x_um: -50_000,
                    y_um: 30_000,
                    width_um: 28_000,
                    height_um: 28_000,
                    fill: ColorInput {
                        red: 180,
                        green: 200,
                        blue: 220,
                        alpha: 255,
                    },
                    stroke: None,
                    corner_rounding: None,
                },
            ],
        ))
        .expect("assemble page and pasteboard");
    let page_objects = &assembled.project.document.pages[0].objects;
    let ids = FixtureIds {
        page: page_id,
        text: page_objects[0].id.as_str().to_owned(),
        diagram: page_objects[1].id.as_str().to_owned(),
        pasteboard: assembled.project.document.pasteboard.objects[0]
            .id
            .as_str()
            .to_owned(),
    };
    (assembled, ids)
}

fn assert_assembled_intent(assembled: &EditorSnapshot, ids: &FixtureIds) {
    assert_eq!(assembled.project.revision.value(), 1);
    let page_objects = &assembled.project.document.pages[0].objects;
    assert_eq!(page_objects.len(), 2);
    assert_eq!(assembled.project.document.pasteboard.objects.len(), 1);
    assert!(
        assembled
            .project
            .document
            .pages
            .iter()
            .flat_map(|page| &page.objects)
            .all(|object| object.id.as_str() != ids.pasteboard),
        "pasteboard membership is not page membership"
    );
    assert!(matches!(
        &page_objects[0].kind,
        ObjectKind::Text { text, .. } if text == INSTRUCTIONS && text.contains("\n\n")
    ));
}

fn compose_instructional_page(editor: &mut EditorModule, ids: &FixtureIds) -> EditorSnapshot {
    editor
        .apply(transaction(
            1,
            "Compose fixed text around diagram",
            vec![
                EditorCommandInput::UpdateTextFrame {
                    object_id: ids.text.clone(),
                    column_count: 2,
                    column_gap_um: 6_350,
                },
                EditorCommandInput::UpdateWrapMode {
                    object_id: ids.diagram.clone(),
                    wrap_mode: WrapModeInput::Rectangular,
                },
                EditorCommandInput::MoveObject {
                    object_id: ids.diagram.clone(),
                    page_id: Some(ids.page.clone()),
                    x_um: 45_000,
                    y_um: 55_000,
                },
                EditorCommandInput::ResizeObject {
                    object_id: ids.text.clone(),
                    width_um: 120_000,
                    height_um: 180_000,
                },
            ],
        ))
        .expect("compose fixed text box and wrapped diagram")
}

fn assert_composed_intent(composed: &EditorSnapshot, ids: &FixtureIds) {
    assert_eq!(composed.project.revision.value(), 2);
    let composed_page = &composed.project.document.pages[0];
    let composed_text = &composed_page.objects[0];
    let composed_diagram = &composed_page.objects[1];
    assert_eq!(composed_text.id.as_str(), ids.text);
    assert_eq!(composed_text.size.width.value(), 120_000);
    assert_eq!(composed_text.size.height.value(), 180_000);
    assert!(matches!(
        &composed_text.kind,
        ObjectKind::Text { text, frame, .. }
            if text == INSTRUCTIONS
                && frame == &TextFrame {
                    sizing_mode: document_core::model::TextBoxSizingMode::Fixed,
                    column_count: 2,
                    column_gap_um: 6_350,
                    ..TextFrame::default()
                }
    ));
    assert_eq!(
        composed_diagram.transform,
        Transform::identity_at(45_000, 55_000)
    );
    assert!(matches!(
        composed_diagram.kind,
        ObjectKind::Shape {
            wrap_mode: WrapMode::Rectangular,
            ..
        }
    ));
}

fn assert_undone_intent(undone: &EditorSnapshot) {
    assert_eq!(undone.project.revision.value(), 3);
    assert_eq!(
        undone.project.document.pages[0].objects[0]
            .size
            .width
            .value(),
        115_000
    );
    assert!(matches!(
        &undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { frame, .. } if frame == &TextFrame::default()
    ));
    assert_eq!(
        undone.project.document.pages[0].objects[1].transform,
        Transform::identity_at(20_000, 40_000)
    );
    assert!(matches!(
        undone.project.document.pages[0].objects[1].kind,
        ObjectKind::Shape {
            wrap_mode: WrapMode::None,
            ..
        }
    ));
}

fn assert_reopened_intent(reopened: &EditorSnapshot, expected: &EditorSnapshot, ids: &FixtureIds) {
    assert_eq!(reopened.project.revision.value(), 4);
    assert_eq!(reopened.project.document, expected.project.document);
    assert_eq!(reopened.project.document.pages[0].objects.len(), 2);
    assert_eq!(reopened.project.document.pasteboard.objects.len(), 1);
    assert_eq!(
        reopened.project.document.pasteboard.objects[0].id.as_str(),
        ids.pasteboard
    );
    assert!(
        reopened
            .project
            .document
            .pages
            .iter()
            .flat_map(|page| &page.objects)
            .all(|object| object.id.as_str() != ids.pasteboard)
    );
}

#[test]
fn fixed_instructional_page_round_trips_page_and_pasteboard_intent() {
    let temporary = TempDirectory::new();
    let package = temporary.path().join("instructional-page.publisher-v2");
    let mut editor = EditorModule::new();
    let (assembled, ids) = assemble_instructional_page(&mut editor);
    assert_assembled_intent(&assembled, &ids);
    let composed = compose_instructional_page(&mut editor, &ids);
    assert_composed_intent(&composed, &ids);

    let undone = editor.undo(2).expect("undo composed page transaction");
    assert_undone_intent(&undone);
    let redone = editor.redo(3).expect("redo composed page transaction");
    assert_eq!(redone.project.revision.value(), 4);
    assert_eq!(redone.project.document, composed.project.document);
    editor.save_new(&package).expect("save instructional page");
    assert!(!editor.snapshot().expect("saved snapshot").is_dirty);

    let mut reopened_editor = EditorModule::new();
    let reopened = reopened_editor
        .open(&package)
        .expect("reopen instructional page");
    assert_reopened_intent(&reopened, &redone, &ids);
}

fn resolve_instructions(
    editor: &EditorModule,
    snapshot: &EditorSnapshot,
    ids: &FixtureIds,
) -> ResolvedTextBoxPreview {
    let embedded = editor
        .referenced_font_faces(&preview_font_face_bindings(&snapshot.project), 1, 1_000_000)
        .expect("read the fixture font from the publication");
    assert!(!embedded.budget_exceeded);
    assert_eq!(embedded.faces.len(), 1);
    let resources = embedded
        .faces
        .into_iter()
        .map(|face| ExactFontResource {
            source: face.source,
            face_index: face.face_index,
            bytes: face.bytes,
        })
        .collect::<Vec<_>>();
    ResolvedPublication::resolve_with_font_resources(&snapshot.project, &resources)
        .expect("resolve canonical instructional page")
        .text_boxes()
        .into_iter()
        .find(|text_box| text_box.object_id.as_str() == ids.text)
        .expect("instructional Text Box is resolved")
        .clone()
}

fn assert_instructional_layout(
    layout: &ResolvedTextBoxPreview,
    first_column_x: i64,
    first_column_width: i64,
    expected_sources: &[(usize, usize)],
    overset_start: usize,
) {
    // Coordinates are Text Box-local millipoints. Ten 14.4-point lines fit
    // per column; the fixed 55 mm height must leave an exact unplaced tail.
    assert_eq!(layout.lines.len(), 20);
    assert_eq!(
        layout.font_binding.substitution_status,
        FontSubstitutionStatus::Exact
    );
    assert_eq!(expected_sources.len(), layout.lines.len());
    for (index, (line, &(start, end))) in layout.lines.iter().zip(expected_sources).enumerate() {
        let second_column = index >= 10;
        assert_eq!(line.column_index, u16::from(second_column));
        assert_eq!(
            line.x.value(),
            if second_column {
                181_417
            } else {
                first_column_x
            }
        );
        assert_eq!(
            line.width.value(),
            if second_column {
                158_740
            } else {
                first_column_width
            }
        );
        assert_eq!(line.y.value(), i64::try_from(index % 10).unwrap() * 14_400);
        assert_eq!(line.height.value(), 14_400);
        assert_eq!((line.source.start, line.source.end), (start, end));
    }
    assert!(layout.overset);
    let tail = layout
        .overset_source
        .as_ref()
        .expect("overflow has an exact source range");
    assert_eq!((tail.start, tail.end), (overset_start, 2_261));
    assert_eq!(layout.lines.last().unwrap().source.end, tail.start);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one cross-layer journey records composition, movement and Save/reopen behavior"
)]
fn instructional_page_wrap_reflows_and_reopens_with_identical_resolved_behavior() {
    let temporary = TempDirectory::new();
    let package = temporary.path().join("instructional-layout.publisher-v2");
    let mut editor = EditorModule::new();
    let (assembled, ids) = assemble_instructional_page(&mut editor);
    // Seed an embedded resource through the package boundary; subsequent edits
    // and Save/reopen use EditorModule, including retrieval of those saved bytes.
    let mut assets = document_core::AssetStore::default();
    let font_source = assets.insert(
        "font/ttf",
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec(),
    );
    let seed_package = temporary.path().join("embedded-font.publisher-v2");
    let mut seed = assembled.project.clone();
    let ObjectKind::Text { style, .. } = &mut seed.document.pages[0].objects[0].kind else {
        panic!("instructional text fixture");
    };
    style.font_face = Some(document_core::model::FontFaceBinding {
        source: font_source.clone(),
        face_index: 0,
    });
    document_core::package::save_new_package(&seed_package, &seed, &assets)
        .expect("seed the instructional publication with the checked-in font");
    editor
        .open(&seed_package)
        .expect("open font-bearing instructional fixture");
    let instructions = format!(
        "THE JUMPING RUBBER BAND\n\n{}\n\n{}\n\n{}",
        "Place the band across two fingers and show your empty hand. ".repeat(12),
        "Close your hand slowly, keeping the band visible to your audience. ".repeat(12),
        "Open your hand and pause to let everyone see the surprise. ".repeat(12),
    );
    let composed = editor
        .apply(transaction(
            1,
            "Compose two columns around the instructional diagram",
            vec![
                EditorCommandInput::UpdateTextStyle {
                    object_id: ids.text.clone(),
                    style: TextStyleInput {
                        font_face: Some(FontFaceBindingInput {
                            source: font_source.as_str().to_owned(),
                            face_index: 0,
                        }),
                        font_family: "DejaVu Sans".to_owned(),
                        font_size_millipoints: 12_000,
                        font_weight: 400,
                        italic: false,
                        alignment: TextAlignmentInput::Left,
                        color: ColorInput {
                            red: 0,
                            green: 0,
                            blue: 0,
                            alpha: 255,
                        },
                        line_height_per_thousand: 1_200,
                        underline: false,
                    },
                },
                EditorCommandInput::UpdateText {
                    object_id: ids.text.clone(),
                    text: instructions.clone(),
                },
                EditorCommandInput::UpdateTextFrame {
                    object_id: ids.text.clone(),
                    column_count: 2,
                    column_gap_um: 8_000,
                },
                EditorCommandInput::ResizeObject {
                    object_id: ids.text.clone(),
                    width_um: 120_000,
                    height_um: 55_000,
                },
                EditorCommandInput::ResizeObject {
                    object_id: ids.diagram.clone(),
                    width_um: 18_000,
                    height_um: 55_000,
                },
                EditorCommandInput::MoveObject {
                    object_id: ids.diagram.clone(),
                    page_id: Some(ids.page.clone()),
                    x_um: 12_000,
                    y_um: 18_000,
                },
                EditorCommandInput::UpdateWrapMode {
                    object_id: ids.diagram.clone(),
                    wrap_mode: WrapModeInput::Rectangular,
                },
            ],
        ))
        .expect("compose bounded instructional story");
    let before = resolve_instructions(&editor, &composed, &ids);
    let moved = editor
        .apply(transaction(
            2,
            "Move the wrapped diagram across the first column",
            vec![EditorCommandInput::MoveObject {
                object_id: ids.diagram.clone(),
                page_id: Some(ids.page.clone()),
                x_um: 47_000,
                y_um: 18_000,
            }],
        ))
        .expect("move wrapped diagram");
    let after = resolve_instructions(&editor, &moved, &ids);
    assert_instructional_layout(
        &before,
        51_024,
        107_716,
        &[
            (0, 12),
            (12, 23),
            (24, 24),
            (25, 40),
            (40, 51),
            (51, 68),
            (68, 85),
            (85, 100),
            (100, 111),
            (111, 128),
            (128, 151),
            (151, 171),
            (171, 193),
            (193, 215),
            (215, 239),
            (239, 259),
            (259, 280),
            (280, 303),
            (303, 325),
            (325, 351),
        ],
        351,
    );
    assert_instructional_layout(
        &after,
        0,
        99_212,
        &[
            (0, 12),
            (12, 23),
            (24, 24),
            (25, 40),
            (40, 51),
            (51, 63),
            (63, 73),
            (73, 85),
            (85, 100),
            (100, 111),
            (111, 133),
            (133, 155),
            (155, 179),
            (179, 199),
            (199, 220),
            (220, 243),
            (243, 265),
            (265, 291),
            (291, 313),
            (313, 335),
        ],
        335,
    );
    assert_ne!(before.lines, after.lines);
    assert_ne!(before.overset_source, after.overset_source);
    let page = &moved.project.document.pages[0];
    assert_eq!(
        page.objects[0],
        composed.project.document.pages[0].objects[0]
    );
    assert!(
        matches!(&page.objects[0].kind, ObjectKind::Text { text, frame, .. }
        if text == &instructions && frame == &TextFrame {
            sizing_mode: document_core::model::TextBoxSizingMode::Fixed,
            column_count: 2,
            column_gap_um: 8_000,
            ..TextFrame::default()
        })
    );
    assert_eq!(
        page.objects[1].transform,
        Transform::identity_at(47_000, 18_000)
    );
    assert_eq!(
        page.objects[1].size,
        composed.project.document.pages[0].objects[1].size
    );
    assert!(matches!(
        page.objects[1].kind,
        ObjectKind::Shape {
            wrap_mode: WrapMode::Rectangular,
            ..
        }
    ));

    let resized = editor
        .apply(transaction(
            3,
            "Shorten the wrapped instructional diagram",
            vec![EditorCommandInput::ResizeObject {
                object_id: ids.diagram.clone(),
                width_um: 18_000,
                height_um: 10_000,
            }],
        ))
        .expect("resize wrapped diagram");
    let after_resize = resolve_instructions(&editor, &resized, &ids);
    assert_ne!(after_resize.lines, after.lines);
    assert_ne!(after_resize.overset_source, after.overset_source);
    assert_eq!(
        resized.project.document.pages[0].objects[0],
        page.objects[0]
    );
    assert_eq!(
        resized.project.document.pages[0].objects[1].transform,
        page.objects[1].transform
    );
    assert_eq!(
        resized.project.document.pages[0].objects[1]
            .size
            .height
            .value(),
        10_000
    );
    assert_eq!(after_resize.lines[3].x.value(), 0);
    assert_eq!(after_resize.lines[3].width.value(), 158_740);

    editor
        .save_new(&package)
        .expect("save composed instructions");
    let mut reopened_editor = EditorModule::new();
    let reopened = reopened_editor
        .open(&package)
        .expect("reopen composed instructions");
    assert_eq!(reopened.project.document, resized.project.document);
    assert_eq!(
        resolve_instructions(&reopened_editor, &reopened, &ids),
        after_resize
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "One instructional intent journey covers interior/wrap reflow, refusal, output and Save/reopen"
)]
fn instructional_interiors_and_wrap_clearance_reflow_refuse_and_reopen_through_intent() {
    let temporary = TempDirectory::new();
    let seed_path = temporary.path().join("interior-seed.publisher-v2");
    let saved_path = temporary.path().join("interior-composition.publisher-v2");
    let mut editor = EditorModule::new();
    let (assembled, ids) = assemble_instructional_page(&mut editor);
    let mut assets = document_core::AssetStore::default();
    let source = assets.insert(
        "font/ttf",
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec(),
    );
    let mut seed = assembled.project;
    let ObjectKind::Text { style, .. } = &mut seed.document.pages[0].objects[0].kind else {
        panic!("Text Box")
    };
    style.font_face = Some(document_core::model::FontFaceBinding {
        source: source.clone(),
        face_index: 0,
    });
    document_core::package::save_new_package(&seed_path, &seed, &assets).unwrap();
    editor.open(&seed_path).unwrap();
    let interior: EditorCommandInput = serde_json::from_value(serde_json::json!({
        "type": "updateTextBoxInterior", "objectId": ids.text,
        "margins": { "topUm": 5_000, "rightUm": 5_000, "bottomUm": 5_000, "leftUm": 5_000 }, "verticalAlignment": "middle",
    })).unwrap();
    let clearance: EditorCommandInput = serde_json::from_value(serde_json::json!({
        "type": "updateWrapSettings", "objectId": ids.diagram, "distanceUm": 3_000, "side": "left",
    }))
    .unwrap();
    let composed = editor
        .apply(transaction(
            1,
            "Compose instructional interior",
            vec![
                EditorCommandInput::ResizeObject {
                    object_id: ids.text.clone(),
                    width_um: 120_000,
                    height_um: 80_000,
                },
                EditorCommandInput::UpdateTextFrame {
                    object_id: ids.text.clone(),
                    column_count: 2,
                    column_gap_um: 8_000,
                },
                interior,
                EditorCommandInput::UpdateWrapMode {
                    object_id: ids.diagram.clone(),
                    wrap_mode: WrapModeInput::Rectangular,
                },
                clearance,
            ],
        ))
        .unwrap();
    let before = resolve_instructions(&editor, &composed, &ids);
    assert_eq!(before.content_bounds.x.value(), 5_000);
    assert_eq!(before.content_bounds.width.value(), 110_000);
    assert_eq!(before.content_bounds.height.value(), 70_000);
    assert!(!before.overset);
    assert!(before.vertical_offset.value() > 0);
    let refused: EditorCommandInput = serde_json::from_value(serde_json::json!({
        "type": "updateTextBoxInterior", "objectId": ids.text,
        "margins": { "topUm": 40_000, "rightUm": 5_000, "bottomUm": 40_000, "leftUm": 5_000 }, "verticalAlignment": "bottom",
    })).unwrap();
    editor
        .apply(transaction(2, "Refuse zero-height interior", vec![refused]))
        .expect_err("text and history survive unusable margins");
    assert_eq!(editor.snapshot().unwrap(), composed);
    let moved = editor
        .apply(transaction(
            2,
            "Move the wrapped diagram and continue typing",
            vec![
                EditorCommandInput::MoveObject {
                    object_id: ids.diagram.clone(),
                    page_id: Some(ids.page.clone()),
                    x_um: 20_000,
                    y_um: 135_000,
                },
                EditorCommandInput::UpdateText {
                    object_id: ids.text.clone(),
                    text: format!("{INSTRUCTIONS}\nPause for the surprise."),
                },
            ],
        ))
        .unwrap();
    let after = resolve_instructions(&editor, &moved, &ids);
    assert_ne!(before.lines, after.lines);
    let resources = [ExactFontResource {
        source,
        face_index: 0,
        bytes: include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec(),
    }];
    let output =
        ResolvedPublication::resolve_output_with_font_resources(&moved.project, &resources)
            .unwrap();
    assert_eq!(
        &after,
        output.text_boxes()[0],
        "native preview and output use the same accepted composition"
    );
    editor.save_new(&saved_path).unwrap();
    let mut reopened_editor = EditorModule::new();
    let reopened = reopened_editor.open(&saved_path).unwrap();
    assert_eq!(reopened.project.document, moved.project.document);
    assert_eq!(
        resolve_instructions(&reopened_editor, &reopened, &ids),
        after
    );
}
