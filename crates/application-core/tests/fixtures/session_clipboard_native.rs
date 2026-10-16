use super::*;
use document_core::model::{
    ExactFontVariantBinding, FontFaceBinding, InlineTextSpan, InlineTextStyle, ParagraphRange,
    ParagraphStyle, ParagraphStyleRun, TableCellFormatting, TableData, TextAlignment,
    TextBoxSizingMode, TextSourceRange,
};
use font_boundary::{InspectedFontFace, TrustedFontBoundary};
use layout_core::{
    ExactFontResource, ResolvedPublication, ResolvedSceneObjectKind, output_font_face_bindings,
};

fn font(assets: &mut AssetStore, bold: bool) -> FontFaceBinding {
    let bytes = if bold {
        include_bytes!("../../../layout-core/tests/fixtures/DejaVuSans-Bold.ttf").as_slice()
    } else {
        include_bytes!("../../../layout-core/tests/fixtures/DejaVuSans.ttf").as_slice()
    };
    FontFaceBinding {
        source: assets.insert("font/ttf", bytes.to_vec()),
        face_index: 0,
    }
}
fn inspect(orders: Vec<application_core::ClipboardFontWorkOrder>) -> Vec<InspectedFontFace> {
    let boundary = TrustedFontBoundary::resolve_packaged();
    orders
        .into_iter()
        .map(|order| {
            boundary
                .inspect(&order.bytes, order.face_index)
                .expect("actual confined exact-font inspection")
        })
        .collect()
}
fn text_style(face: FontFaceBinding) -> TextStyle {
    TextStyle {
        font_face: Some(face),
        font_family: "DejaVu Sans".into(),
        font_size_millipoints: 18_000,
        color: Color {
            red: 30,
            green: 80,
            blue: 180,
            alpha: 255,
        },
        underline: true,
        ..TextStyle::default()
    }
}
fn text(value: &str, style: TextStyle) -> ObjectKind {
    ObjectKind::Text {
        text: value.into(),
        style,
        frame: TextFrame::default(),
        story: TextStoryFormatting::default(),
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One actual-worker cross-publication journey proves exact faces, target overrides, complete output and save/history.
fn exact_style_resources_survive_source_close_and_reach_text_table_output() {
    let mut assets = AssetStore::default();
    let regular = font(&mut assets, false);
    let bold = font(&mut assets, true);
    let mut source = session("font-source");
    let mut style = text_style(regular.clone());
    style.exact_font_variants = vec![ExactFontVariantBinding {
        font_weight: 700,
        italic: false,
        face: bold.clone(),
    }];
    let source_id = add(
        &mut source,
        ObjectKind::Text {
            text: "Source two paragraphs\nLast".into(),
            style,
            frame: TextFrame {
                margins: Insets::uniform(1_000).unwrap(),
                ..TextFrame::default()
            },
            story: TextStoryFormatting {
                paragraph_runs: vec![ParagraphStyleRun {
                    range: ParagraphRange { start: 0, end: 2 },
                    style: ParagraphStyle {
                        alignment: Some(TextAlignment::Center),
                        space_after_millipoints: Some(3_000),
                        ..ParagraphStyle::default()
                    },
                }],
                ..TextStoryFormatting::default()
            },
        },
    );
    let mut source = Fixture::new("font-source", source, &assets);
    let snapshot = source.editor.snapshot().unwrap();
    let mut clipboard = SessionClipboard::default();
    let plan = source
        .editor
        .plan_style_clipboard_capture(CaptureStyleClipboardInput {
            document_id: "font-source".into(),
            expected_revision: snapshot.project.revision.value(),
            object_id: source_id.as_str().into(),
        })
        .unwrap();
    assert_eq!(plan.font_resources().len(), 2);
    let receipts = inspect(plan.font_resources());
    clipboard
        .capture_style(&source.editor, plan, receipts)
        .unwrap();
    assert_eq!(source.editor.snapshot().unwrap(), snapshot);
    source
        .editor
        .discard_document(snapshot.project.revision.value())
        .unwrap();
    drop(source);
    let mut destination = session("font-destination");
    let target = add(
        &mut destination,
        ObjectKind::Text {
            text: "B Short target and longer body\nThird paragraph\nFourth".into(),
            style: TextStyle::default(),
            frame: TextFrame::default(),
            story: TextStoryFormatting {
                inline_spans: vec![InlineTextSpan {
                    range: TextSourceRange { start: 0, end: 1 },
                    style: InlineTextStyle {
                        bold: Some(true),
                        font_size_millipoints: Some(11_000),
                        ..InlineTextStyle::default()
                    },
                }],
                ..TextStoryFormatting::default()
            },
        },
    );
    let mut table = TableData::empty(2, 2).unwrap();
    table.cells = vec![
        "One".into(),
        "Two\nlines".into(),
        "Three".into(),
        "Four".into(),
    ];
    table.cell_formatting = vec![TableCellFormatting {
        row: 1,
        column: 1,
        style: InlineTextStyle {
            bold: Some(true),
            font_size_millipoints: Some(9_000),
            ..InlineTextStyle::default()
        },
        ..TableCellFormatting::default()
    }];
    let table_id = add(&mut destination, ObjectKind::Table { table });
    destination
        .execute(
            destination.revision(),
            Transaction {
                label: "Room for output".into(),
                commands: vec![
                    Command::ResizeObject {
                        object_id: target.clone(),
                        size: PhysicalSize::new(100_000, 100_000).unwrap(),
                    },
                    Command::ResizeObject {
                        object_id: table_id.clone(),
                        size: PhysicalSize::new(100_000, 60_000).unwrap(),
                    },
                ],
            },
        )
        .unwrap();

    let mut destination = Fixture::new("font-destination", destination, &AssetStore::default());
    let before = destination.editor.snapshot().unwrap();
    let intent = style_paste(&destination.editor, &[target.clone(), table_id]);
    let result = clipboard
        .paste_style(&mut destination.editor, intent)
        .unwrap();
    let ObjectKind::Text {
        text, style, story, ..
    } = &result.snapshot.project.document.pages[0].objects[0].kind
    else {
        panic!("Text");
    };
    assert!(text.ends_with("Fourth"));
    assert_eq!(style.font_face, Some(regular.clone()));
    assert_eq!(style.exact_font_variants[0].face, bold);
    assert_eq!(story.inline_spans.len(), 1);
    assert_eq!(story.inline_spans[0].range.end, 1);
    assert_eq!(story.paragraph_runs[0].range.end, 3);
    assert_eq!(
        story.paragraph_runs[0].style.space_after_millipoints,
        Some(3_000)
    );
    let ObjectKind::Table { table } = &result.snapshot.project.document.pages[0].objects[1].kind
    else {
        panic!("Table");
    };
    let ObjectKind::Table { table: old_table } = &before.project.document.pages[0].objects[1].kind
    else {
        panic!("Table");
    };
    assert_eq!(table.cells, old_table.cells);
    assert_eq!(table.cell_formatting, old_table.cell_formatting);
    assert_eq!(
        table.effective_cell_style(1, 1).font_face,
        Some(bold.clone())
    );
    let output_bindings = output_font_face_bindings(&result.snapshot.project).unwrap();
    let resources = destination
        .editor
        .referenced_font_faces(&output_bindings, 64, 64 * 1024 * 1024)
        .unwrap()
        .faces
        .into_iter()
        .map(|face| ExactFontResource {
            source: face.source,
            face_index: face.face_index,
            bytes: face.bytes,
        })
        .collect::<Vec<_>>();
    let scene = ResolvedPublication::resolve_output_with_font_resources(
        &result.snapshot.project,
        &resources,
    )
    .unwrap();
    let ResolvedSceneObjectKind::Text { layout, .. } = &scene.pages()[0].objects[0].kind else {
        panic!("native Text output");
    };
    assert!(!layout.overset);
    assert!(layout.font_bindings.iter().any(|binding| {
        binding.requested_weight == 700
            && binding
                .exact_face
                .as_ref()
                .is_some_and(|face| face.sha256_hex == bold.source.as_str())
    }));
    assert!(layout.lines.iter().flat_map(|line| &line.glyph_runs).all(
        |run| run.underline.is_some() && run.color == style.color && !run.requires_font_synthesis
    ));
    let ResolvedSceneObjectKind::Table { table } = &scene.pages()[0].objects[1].kind else {
        panic!("native Table output");
    };
    assert_eq!(table.populated_cells.len(), 4);
    let path = destination.folder.join("style.publisher");
    destination.editor.save_new(&path).unwrap();
    let reopened = document_core::open_package(&path).unwrap();
    assert_eq!(reopened.snapshot.document, result.snapshot.project.document);
    assert!(reopened.assets.contains(&regular.source) && reopened.assets.contains(&bold.source));
    let undone = destination
        .editor
        .undo(result.snapshot.project.revision.value())
        .unwrap();
    assert_eq!(undone.project.document, before.project.document);
    assert!(!undone.can_undo);
    let redone = destination
        .editor
        .redo(undone.project.revision.value())
        .unwrap();
    assert_eq!(redone.project.document, result.snapshot.project.document);
}

#[test]
fn exact_object_capture_ignores_unrelated_unbound_fonts_and_retains_master_provenance() {
    let mut assets = AssetStore::default();
    let regular = font(&mut assets, false);
    let mut source = session("master-copy");
    let id = add(
        &mut source,
        text("Copied words", text_style(regular.clone())),
    );
    add(
        &mut source,
        text("Unrelated missing machine font", TextStyle::default()),
    );
    let master = source
        .execute(
            source.revision(),
            Transaction::single(
                "Master",
                Command::AddMaster {
                    name: "Source master".into(),
                    trim: PhysicalSize::new(215_900, 279_400).unwrap(),
                },
            ),
        )
        .unwrap()
        .created_masters[0]
        .clone();
    source
        .execute(
            source.revision(),
            Transaction::single(
                "Move",
                Command::SetObjectPosition {
                    object_id: id.clone(),
                    x: Micrometres::new(10_000),
                    y: Micrometres::new(20_000),
                    destination: ObjectLocation::Master(master.clone()),
                },
            ),
        )
        .unwrap();
    let mut source = Fixture::new("master-copy", source, &assets);
    let mut clipboard = SessionClipboard::default();
    let snapshot = source.editor.snapshot().unwrap();
    let plan = source
        .editor
        .plan_object_clipboard_capture(CaptureClipboardInput {
            document_id: "master-copy".into(),
            expected_revision: snapshot.project.revision.value(),
            object_ids: vec![id.as_str().into()],
            cut: true,
        })
        .unwrap();
    assert_eq!(plan.font_resources().len(), 1);
    let receipts = inspect(plan.font_resources());
    let selection = clipboard
        .capture_objects(&mut source.editor, plan, receipts)
        .unwrap();
    assert_eq!(selection.source_master_id, Some(master));
    assert_eq!(selection.source_page_id, None);
    let cut = source.editor.snapshot().unwrap();
    let undo = source.editor.undo(cut.project.revision.value()).unwrap();
    assert_eq!(undo.project.document, snapshot.project.document);
    source
        .editor
        .discard_document(undo.project.revision.value())
        .unwrap();
    drop(source);
    let mut destination = Fixture::new(
        "master-paste",
        session("master-paste"),
        &AssetStore::default(),
    );
    let destination_snapshot = destination.editor.snapshot().unwrap();
    let intent = object_paste(
        &destination.editor,
        ClipboardDestinationInput::Page {
            page_id: destination_snapshot.project.document.pages[0]
                .id
                .as_str()
                .into(),
        },
    );
    let pasted = clipboard
        .paste_objects(&mut destination.editor, intent)
        .unwrap();
    let ObjectKind::Text { text, style, .. } = &pasted.project.document.pages[0].objects[0].kind
    else {
        panic!("Text");
    };
    assert_eq!(text, "Copied words");
    assert_eq!(style.font_face, Some(regular));
    assert_eq!(pasted.project.document.id.as_str(), "master-paste");
}

#[test]
fn fit_style_dimensions_are_rehearsed_and_refused_without_canonical_changes() {
    let mut assets = AssetStore::default();
    let regular = font(&mut assets, false);
    let mut source = session("fit-source");
    let source_id = add(&mut source, text("Source", text_style(regular.clone())));
    let source = Fixture::new("fit-source", source, &assets);
    let mut clipboard = SessionClipboard::default();
    let source_snapshot = source.editor.snapshot().unwrap();
    let plan = source
        .editor
        .plan_style_clipboard_capture(CaptureStyleClipboardInput {
            document_id: "fit-source".into(),
            expected_revision: source_snapshot.project.revision.value(),
            object_id: source_id.as_str().into(),
        })
        .unwrap();
    let receipts = inspect(plan.font_resources());
    clipboard
        .capture_style(&source.editor, plan, receipts)
        .unwrap();
    let mut destination = session("fit-target");
    let id = add(
        &mut destination,
        ObjectKind::Text {
            text: "Fit target words".into(),
            style: TextStyle {
                font_face: Some(regular),
                font_family: "DejaVu Sans".into(),
                font_size_millipoints: 10_000,
                ..TextStyle::default()
            },
            frame: TextFrame {
                sizing_mode: TextBoxSizingMode::Fit,
                ..TextFrame::default()
            },
            story: TextStoryFormatting::default(),
        },
    );
    let mut destination = Fixture::new("fit-target", destination, &assets);
    let before = destination.editor.snapshot().unwrap();
    let intent = style_paste(&destination.editor, &[id]);
    let error = clipboard
        .paste_style(&mut destination.editor, intent)
        .unwrap_err();
    assert!(error.message.contains("Fixed"));
    assert_eq!(destination.editor.snapshot().unwrap(), before);
}

fn restricted_font() -> Vec<u8> {
    let mut bytes = include_bytes!("../../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let tables = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    for index in 0..tables {
        let record = 12 + index * 16;
        if &bytes[record..record + 4] == b"OS/2" {
            let offset =
                u32::from_be_bytes(bytes[record + 8..record + 12].try_into().unwrap()) as usize;
            bytes[offset + 8..offset + 10].copy_from_slice(&2_u16.to_be_bytes());
            return bytes;
        }
    }
    panic!("fixture OS/2 exists");
}
#[test]
fn denied_font_capture_before_cut_keeps_source_and_both_previous_clipboards() {
    let mut assets = AssetStore::default();
    let restricted = FontFaceBinding {
        source: assets.insert("font/ttf", restricted_font()),
        face_index: 0,
    };
    let mut source = session("denied-font");
    let shape = add(&mut source, shape(ShapeKind::Rectangle));
    let text = add(&mut source, text("Cannot cut this", text_style(restricted)));
    let mut source = Fixture::new("denied-font", source, &assets);
    let mut clipboard = SessionClipboard::default();
    copy_style(&mut clipboard, &source.editor, &shape);
    copy_objects(
        &mut clipboard,
        &mut source.editor,
        vec![shape.as_str().into()],
        false,
    );
    let before = source.editor.snapshot().unwrap();
    let plan = source
        .editor
        .plan_object_clipboard_capture(CaptureClipboardInput {
            document_id: "denied-font".into(),
            expected_revision: before.project.revision.value(),
            object_ids: vec![text.as_str().into()],
            cut: true,
        })
        .unwrap();
    let receipts = inspect(plan.font_resources());
    let error = clipboard
        .capture_objects(&mut source.editor, plan, receipts)
        .unwrap_err();
    assert_eq!(error.code, EditorErrorCode::FontUnavailable);
    assert_eq!(source.editor.snapshot().unwrap(), before);
    assert_eq!(clipboard.object_count(), 1);
    assert!(clipboard.has_style());
}
#[test]
fn unavailable_optional_variant_does_not_block_verified_base_style() {
    let mut assets = AssetStore::default();
    let regular = font(&mut assets, false);
    let mut source = session("optional-font");
    let mut style = text_style(regular.clone());
    // Deliberately malformed saved optional metadata cannot masquerade as Bold
    // and cannot invalidate an independently verified required Regular face.
    style.exact_font_variants = vec![ExactFontVariantBinding {
        font_weight: 700,
        italic: false,
        face: regular.clone(),
    }];
    let id = add(&mut source, text("Homogeneous Regular", style));
    let source = Fixture::new("optional-font", source, &assets);
    let snapshot = source.editor.snapshot().unwrap();
    let mut clipboard = SessionClipboard::default();
    let plan = source
        .editor
        .plan_style_clipboard_capture(CaptureStyleClipboardInput {
            document_id: "optional-font".into(),
            expected_revision: snapshot.project.revision.value(),
            object_id: id.as_str().into(),
        })
        .unwrap();
    assert_eq!(plan.font_resources().len(), 1);
    let receipts = inspect(plan.font_resources());
    let summary = clipboard
        .capture_style(&source.editor, plan, receipts)
        .unwrap();
    assert!(
        summary
            .skipped_properties
            .contains(&"unavailable optional font variants".into())
    );
    let mut destination = session("optional-target");
    let target = add(
        &mut destination,
        text("Unbound target", TextStyle::default()),
    );
    let mut destination = Fixture::new("optional-target", destination, &AssetStore::default());
    let intent = style_paste(&destination.editor, &[target]);
    let pasted = clipboard
        .paste_style(&mut destination.editor, intent)
        .unwrap();
    let ObjectKind::Text { style, .. } = &pasted.snapshot.project.document.pages[0].objects[0].kind
    else {
        panic!("Text");
    };
    assert_eq!(style.font_face, Some(regular));
    assert!(style.exact_font_variants.is_empty());
}
