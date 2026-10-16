use application_core::{
    CaptureClipboardInput, CaptureStyleClipboardInput, ClipboardDestinationInput, EditorErrorCode,
    EditorModule, PasteObjectClipboardInput, PasteStyleClipboardInput, SessionClipboard,
};
use document_core::model::{
    Color, CornerRounding, DocumentId, HardShadow, ImageContentKind, ImageCrop, ImageFlip, Insets,
    Micrometres, ObjectDraft, ObjectEffects, ObjectKind, PhysicalSize, ShapeKind, ShapeStroke,
    TextFrame, TextStoryFormatting, TextStyle, Transform, WrapMode,
};
use document_core::{
    AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
    save_new_package,
};
use std::path::PathBuf;

struct Fixture {
    folder: PathBuf,
    editor: EditorModule,
}
impl Fixture {
    fn new(name: &str, mut session: DocumentSession, assets: &AssetStore) -> Self {
        let folder = std::env::temp_dir().join(format!(
            "publisher-session-clipboard-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("source.publisher");
        save_new_package(&path, &ProjectSnapshot::from_session(&session), assets).unwrap();
        session.set_history_retention_limits(document_core::commands::HistoryRetentionLimits {
            max_transactions: 0,
            max_snapshot_bytes: 0,
            max_history_asset_bytes: 0,
        });
        let mut editor = EditorModule::default();
        editor.open(&path).unwrap();
        Self { folder, editor }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.folder);
    }
}
fn session(name: &str) -> DocumentSession {
    DocumentSession::new(
        Document::new(
            DocumentId::parse(name).unwrap(),
            name,
            PhysicalSize::new(215_900, 279_400).unwrap(),
            Insets::default(),
        )
        .unwrap(),
    )
    .unwrap()
}
fn add(session: &mut DocumentSession, kind: ObjectKind) -> document_core::ObjectId {
    let page = session.document().pages[0].id.clone();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Add",
                Command::AddObject {
                    location: ObjectLocation::Page(page),
                    object: ObjectDraft {
                        name: "Object".into(),
                        size: PhysicalSize::new(50_000, 30_000).unwrap(),
                        transform: Transform::identity_at(10_000, 20_000),
                        opacity_millionths: 1_000_000,
                        kind,
                    },
                },
            ),
        )
        .unwrap()
        .created_objects[0]
        .clone()
}
fn shape(shape: ShapeKind) -> ObjectKind {
    ObjectKind::Shape {
        shape,
        fill: Color {
            red: 220,
            green: 40,
            blue: 10,
            alpha: 255,
        },
        stroke: ShapeStroke {
            width_millipoints: 2_000,
            ..ShapeStroke::default()
        },
        wrap_mode: WrapMode::None,
    }
}
fn image(assets: &mut AssetStore) -> ObjectKind {
    let pixels = image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 70, 130, 100]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(pixels)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let bytes = bytes.into_inner();
    let raster = document_core::inspect_raster(&bytes).unwrap();
    let asset = assets.insert_raster(bytes, raster);
    ObjectKind::Image {
        asset,
        content_kind: ImageContentKind::ContinuousTone,
        wrap_mode: WrapMode::Rectangular,
        crop: ImageCrop::new(100_000, 0, 100_000, 0).unwrap(),
        lock_aspect_ratio: false,
        flip: ImageFlip {
            horizontal: true,
            vertical: false,
        },
    }
}
fn copy_objects(
    clipboard: &mut SessionClipboard,
    editor: &mut EditorModule,
    ids: Vec<String>,
    cut: bool,
) {
    let snapshot = editor.snapshot().unwrap();
    let plan = editor
        .plan_object_clipboard_capture(CaptureClipboardInput {
            document_id: snapshot.project.document.id.as_str().into(),
            expected_revision: snapshot.project.revision.value(),
            object_ids: ids,
            cut,
        })
        .unwrap();
    assert!(plan.font_resources().is_empty());
    clipboard.capture_objects(editor, plan, vec![]).unwrap();
}
fn copy_style(
    clipboard: &mut SessionClipboard,
    editor: &EditorModule,
    id: &document_core::ObjectId,
) {
    let snapshot = editor.snapshot().unwrap();
    let plan = editor
        .plan_style_clipboard_capture(CaptureStyleClipboardInput {
            document_id: snapshot.project.document.id.as_str().into(),
            expected_revision: snapshot.project.revision.value(),
            object_id: id.as_str().into(),
        })
        .unwrap();
    assert!(plan.font_resources().is_empty());
    clipboard.capture_style(editor, plan, vec![]).unwrap();
}
fn object_paste(
    editor: &EditorModule,
    destination: ClipboardDestinationInput,
) -> PasteObjectClipboardInput {
    let snapshot = editor.snapshot().unwrap();
    PasteObjectClipboardInput {
        document_id: snapshot.project.document.id.as_str().into(),
        expected_revision: snapshot.project.revision.value(),
        destination,
        horizontal_offset_um: 3_000,
        vertical_offset_um: 4_000,
    }
}
fn style_paste(editor: &EditorModule, ids: &[document_core::ObjectId]) -> PasteStyleClipboardInput {
    let snapshot = editor.snapshot().unwrap();
    PasteStyleClipboardInput {
        document_id: snapshot.project.document.id.as_str().into(),
        expected_revision: snapshot.project.revision.value(),
        object_ids: ids.iter().map(|id| id.as_str().into()).collect(),
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One session journey proves independent ownership, Cut, retirement, replacement and deliberate destinations.
fn independent_captures_survive_cut_retirement_and_editor_replacement() {
    let mut assets = AssetStore::default();
    let mut source = session("source");
    let picture = add(&mut source, image(&mut assets));
    let rectangle = add(&mut source, shape(ShapeKind::Rectangle));
    source
        .execute(
            source.revision(),
            Transaction::single(
                "Group",
                Command::GroupObjects {
                    object_ids: vec![picture.clone(), rectangle.clone()],
                },
            ),
        )
        .unwrap();
    source
        .execute(
            source.revision(),
            Transaction {
                label: "Appearance".into(),
                commands: vec![
                    Command::UpdateCornerRounding {
                        object_id: rectangle.clone(),
                        rounding: CornerRounding {
                            top_left_um: 5_000,
                            top_right_um: 7_000,
                            bottom_right_um: 8_000,
                            bottom_left_um: 2_000,
                            linked: false,
                        },
                    },
                    Command::SetOpacity {
                        object_id: rectangle.clone(),
                        opacity_millionths: 620_000,
                    },
                    Command::UpdateObjectEffects {
                        object_id: rectangle.clone(),
                        effects: ObjectEffects {
                            shadow: Some(HardShadow {
                                color: Color {
                                    red: 0,
                                    green: 0,
                                    blue: 0,
                                    alpha: 120,
                                },
                                offset_x: Micrometres::new(2_000),
                                offset_y: Micrometres::new(3_000),
                            }),
                            ..ObjectEffects::default()
                        },
                    },
                ],
            },
        )
        .unwrap();
    let mut source = Fixture::new("source", source, &assets);
    let original = source.editor.snapshot().unwrap();
    let mut clipboard = SessionClipboard::default();
    copy_style(&mut clipboard, &source.editor, &rectangle);
    assert_eq!(source.editor.snapshot().unwrap(), original);
    source
        .editor
        .set_history_retention_limits(document_core::commands::HistoryRetentionLimits {
            max_transactions: 0,
            max_snapshot_bytes: 0,
            max_history_asset_bytes: 0,
        })
        .unwrap();
    copy_objects(
        &mut clipboard,
        &mut source.editor,
        vec![picture.as_str().into(), rectangle.as_str().into()],
        true,
    );
    let cut = source.editor.snapshot().unwrap();
    assert!(cut.project.document.pages[0].objects.is_empty());
    assert!(!cut.can_undo);
    source
        .editor
        .discard_document(cut.project.revision.value())
        .unwrap();
    drop(source);
    assert!(clipboard.has_objects() && clipboard.has_style());
    let mut target_session = session("destination");
    let ellipse = add(&mut target_session, shape(ShapeKind::Ellipse));
    let target_picture = add(&mut target_session, image(&mut assets));
    let mut target = Fixture::new("destination", target_session, &assets);
    let before = target.editor.snapshot().unwrap();
    let before_picture = before.project.document.pages[0].objects[1].clone();
    let intent = style_paste(&target.editor, &[ellipse.clone(), target_picture.clone()]);
    let styled = clipboard.paste_style(&mut target.editor, intent).unwrap();
    assert!(
        styled
            .skipped_properties
            .iter()
            .any(|property| property.contains("corners"))
    );
    assert!(styled.snapshot.can_undo);
    assert_eq!(
        styled.snapshot.project.document.pages[0].objects[1].kind,
        before_picture.kind
    );
    assert_eq!(
        styled.snapshot.project.document.pages[0].objects[1].transform,
        before_picture.transform
    );
    assert_eq!(
        styled.snapshot.project.document.pages[0].objects[1].size,
        before_picture.size
    );
    assert_eq!(
        styled.snapshot.project.document.pages[0].objects[1].corner_rounding,
        original.project.document.pages[0].objects[1].corner_rounding
    );
    let undone = target
        .editor
        .undo(styled.snapshot.project.revision.value())
        .unwrap();
    assert_eq!(undone.project.document, before.project.document);
    let intent = object_paste(&target.editor, ClipboardDestinationInput::Pasteboard);
    let pasted = clipboard.paste_objects(&mut target.editor, intent).unwrap();
    assert_eq!(pasted.project.document.pasteboard.objects.len(), 2);
    let group = pasted.project.document.pasteboard.objects[0]
        .group_id
        .as_ref()
        .unwrap();
    assert_eq!(
        Some(group),
        pasted.project.document.pasteboard.objects[1]
            .group_id
            .as_ref()
    );
    assert_ne!(
        Some(group),
        original.project.document.pages[0].objects[0]
            .group_id
            .as_ref()
    );
    assert_eq!(
        pasted.project.document.pasteboard.objects[0]
            .transform
            .translation_x
            .value(),
        13_000
    );
    let ObjectKind::Image { asset, .. } = &pasted.project.document.pasteboard.objects[0].kind
    else {
        panic!("Picture");
    };
    assert_eq!(
        target
            .editor
            .referenced_raster(asset.as_str())
            .unwrap()
            .bytes,
        assets.get(asset).unwrap().bytes
    );
    let path = target.folder.join("pasted.publisher");
    target.editor.save_new(&path).unwrap();
    let reopened = document_core::open_package(&path).unwrap();
    assert_eq!(reopened.snapshot.document, pasted.project.document);
}

#[test]
fn stale_capture_same_identity_and_locked_paste_preserve_previous_clipboards() {
    let mut source = session("same");
    let first = add(&mut source, shape(ShapeKind::Rectangle));
    let mut source = Fixture::new("same", source, &AssetStore::default());
    let mut clipboard = SessionClipboard::default();
    copy_style(&mut clipboard, &source.editor, &first);
    copy_objects(
        &mut clipboard,
        &mut source.editor,
        vec![first.as_str().into()],
        false,
    );
    let snapshot = source.editor.snapshot().unwrap();
    let plan = source
        .editor
        .plan_style_clipboard_capture(CaptureStyleClipboardInput {
            document_id: "same".into(),
            expected_revision: snapshot.project.revision.value(),
            object_id: first.as_str().into(),
        })
        .unwrap();
    source
        .editor
        .open(&source.folder.join("source.publisher"))
        .unwrap();
    assert_eq!(
        clipboard
            .capture_style(&source.editor, plan, vec![])
            .unwrap_err()
            .code,
        EditorErrorCode::StaleRevision
    );
    assert!(clipboard.has_style() && clipboard.has_objects());
    let mut destination = session("locked");
    let a = add(&mut destination, shape(ShapeKind::Rectangle));
    let b = add(&mut destination, shape(ShapeKind::Rectangle));
    destination
        .execute(
            destination.revision(),
            Transaction::single(
                "Lock",
                Command::SetObjectLock {
                    object_ids: vec![b.clone()],
                    locked: true,
                },
            ),
        )
        .unwrap();
    let mut destination = Fixture::new("locked", destination, &AssetStore::default());
    let before = destination.editor.snapshot().unwrap();
    let intent = style_paste(&destination.editor, &[a, b]);
    assert!(
        clipboard
            .paste_style(&mut destination.editor, intent)
            .is_err()
    );
    assert_eq!(destination.editor.snapshot().unwrap(), before);
}

#[test]
fn failed_object_destination_admission_keeps_assets_and_publication() {
    let mut assets = AssetStore::default();
    let mut source = session("resource-source");
    let id = add(&mut source, image(&mut assets));
    let mut source = Fixture::new("resource-source", source, &assets);
    let mut clipboard = SessionClipboard::default();
    copy_objects(
        &mut clipboard,
        &mut source.editor,
        vec![id.as_str().into()],
        false,
    );
    let mut destination = Fixture::new("empty", session("empty"), &AssetStore::default());
    let before = destination.editor.snapshot().unwrap();
    let intent = object_paste(
        &destination.editor,
        ClipboardDestinationInput::Page {
            page_id: "missing-page".into(),
        },
    );
    assert!(
        clipboard
            .paste_objects(&mut destination.editor, intent)
            .is_err()
    );
    assert_eq!(destination.editor.snapshot().unwrap(), before);
    let intent = object_paste(
        &destination.editor,
        ClipboardDestinationInput::Page {
            page_id: before.project.document.pages[0].id.as_str().into(),
        },
    );
    let pasted = clipboard
        .paste_objects(&mut destination.editor, intent)
        .unwrap();
    assert!(pasted.can_undo);
    let undone = destination
        .editor
        .undo(pasted.project.revision.value())
        .unwrap();
    assert_eq!(undone.project.document, before.project.document);
    let redone = destination
        .editor
        .redo(undone.project.revision.value())
        .unwrap();
    assert_eq!(redone.project.document, pasted.project.document);
}

#[test]
fn corner_style_refuses_unusable_text_interior_atomically() {
    let mut source = session("corners");
    let id = add(&mut source, shape(ShapeKind::Rectangle));
    source
        .execute(
            source.revision(),
            Transaction::single(
                "Round",
                Command::UpdateCornerRounding {
                    object_id: id.clone(),
                    rounding: CornerRounding {
                        top_left_um: 30_000,
                        top_right_um: 30_000,
                        bottom_right_um: 30_000,
                        bottom_left_um: 30_000,
                        linked: true,
                    },
                },
            ),
        )
        .unwrap();
    let source = Fixture::new("corners", source, &AssetStore::default());
    let mut clipboard = SessionClipboard::default();
    copy_style(&mut clipboard, &source.editor, &id);
    let mut destination = session("narrow");
    let first = add(&mut destination, shape(ShapeKind::Rectangle));
    let text = add(
        &mut destination,
        ObjectKind::Text {
            text: String::new(),
            style: TextStyle::default(),
            frame: TextFrame {
                column_count: 12,
                column_gap_um: 80,
                ..TextFrame::default()
            },
            story: TextStoryFormatting::default(),
        },
    );
    destination
        .execute(
            destination.revision(),
            Transaction::single(
                "Narrow",
                Command::ResizeObject {
                    object_id: text.clone(),
                    size: PhysicalSize::new(1_000, 30_000).unwrap(),
                },
            ),
        )
        .unwrap();
    let mut destination = Fixture::new("narrow", destination, &AssetStore::default());
    let before = destination.editor.snapshot().unwrap();
    let intent = style_paste(&destination.editor, &[first, text]);
    assert!(
        clipboard
            .paste_style(&mut destination.editor, intent)
            .is_err()
    );
    assert_eq!(destination.editor.snapshot().unwrap(), before);
}

#[test]
fn wire_carries_targets_only_and_clipboards_are_independent() {
    assert!(serde_json::from_value::<PasteObjectClipboardInput>(serde_json::json!({"documentId":"target","expectedRevision":0,"destination":{"type":"pasteboard"},"horizontalOffsetUm":0,"verticalOffsetUm":0,"objects":[]})).is_err());
    assert!(serde_json::from_value::<PasteStyleClipboardInput>(serde_json::json!({"documentId":"target","expectedRevision":0,"objectIds":["o1"],"fontFace":{"source":"fake"}})).is_err());
}

#[cfg(windows)]
#[path = "fixtures/session_clipboard_native.rs"]
mod native;
#[test]
#[allow(clippy::too_many_lines)] // One mixed-source journey compares both Text and Table content/override preservation.
fn mixed_source_style_skips_properties_and_preserves_target_ranges_and_table_structure() {
    use document_core::model::{
        InlineTextSpan, InlineTextStyle, TableCellFormatting, TableData, TextSourceRange,
    };
    let color = Color {
        red: 20,
        green: 80,
        blue: 170,
        alpha: 255,
    };
    let mut source = session("mixed-style");
    let id = add(
        &mut source,
        ObjectKind::Text {
            text: "first second".into(),
            style: TextStyle {
                color,
                ..TextStyle::default()
            },
            frame: TextFrame::default(),
            story: TextStoryFormatting {
                inline_spans: vec![InlineTextSpan {
                    range: TextSourceRange { start: 0, end: 5 },
                    style: InlineTextStyle {
                        bold: Some(true),
                        font_size_millipoints: Some(24_000),
                        ..InlineTextStyle::default()
                    },
                }],
                ..TextStoryFormatting::default()
            },
        },
    );
    let source = Fixture::new("mixed-style", source, &AssetStore::default());
    let mut clipboard = SessionClipboard::default();
    copy_style(&mut clipboard, &source.editor, &id);
    let summary = clipboard.style_summary().unwrap();
    assert!(summary.skipped_properties.contains(&"font weight".into()));
    assert!(summary.skipped_properties.contains(&"font size".into()));
    let mut destination = session("mixed-target");
    let target = add(
        &mut destination,
        ObjectKind::Text {
            text: "X long target story".into(),
            style: TextStyle {
                font_size_millipoints: 9_000,
                ..TextStyle::default()
            },
            frame: TextFrame::default(),
            story: TextStoryFormatting {
                inline_spans: vec![InlineTextSpan {
                    range: TextSourceRange { start: 2, end: 6 },
                    style: InlineTextStyle {
                        underline: Some(true),
                        ..InlineTextStyle::default()
                    },
                }],
                ..TextStoryFormatting::default()
            },
        },
    );
    let mut table = TableData::empty(2, 2).unwrap();
    table.cells = vec!["A".into(), "B".into(), "C".into(), "D".into()];
    table.cell_formatting = vec![TableCellFormatting {
        row: 1,
        column: 1,
        style: InlineTextStyle {
            color: Some(Color {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255,
            }),
            ..InlineTextStyle::default()
        },
        ..TableCellFormatting::default()
    }];
    let table_id = add(&mut destination, ObjectKind::Table { table });
    let mut destination = Fixture::new("mixed-target", destination, &AssetStore::default());
    let before = destination.editor.snapshot().unwrap();
    let intent = style_paste(&destination.editor, &[target, table_id]);
    let result = clipboard
        .paste_style(&mut destination.editor, intent)
        .unwrap();
    let (
        ObjectKind::Text {
            text, style, story, ..
        },
        ObjectKind::Text {
            text: old_text,
            story: old_story,
            ..
        },
    ) = (
        &result.snapshot.project.document.pages[0].objects[0].kind,
        &before.project.document.pages[0].objects[0].kind,
    )
    else {
        panic!("Text");
    };
    assert_eq!(text, old_text);
    assert_eq!(story.inline_spans, old_story.inline_spans);
    assert_eq!(style.font_size_millipoints, 9_000);
    assert_eq!(style.color, color);
    let (ObjectKind::Table { table }, ObjectKind::Table { table: old }) = (
        &result.snapshot.project.document.pages[0].objects[1].kind,
        &before.project.document.pages[0].objects[1].kind,
    ) else {
        panic!("Table");
    };
    assert_eq!(table.cells, old.cells);
    assert_eq!(table.cell_formatting, old.cell_formatting);
    assert_eq!(table.column_weights, old.column_weights);
    assert_eq!(table.row_weights, old.row_weights);
    assert_eq!(table.style.color, color);
    let undone = destination
        .editor
        .undo(result.snapshot.project.revision.value())
        .unwrap();
    assert_eq!(undone.project.document, before.project.document);
}
#[test]
fn aggregate_style_command_limit_refuses_every_target_without_partial_changes() {
    let mut source = session("bulk-style");
    let id = add(&mut source, shape(ShapeKind::Rectangle));
    let source = Fixture::new("bulk-style", source, &AssetStore::default());
    let mut clipboard = SessionClipboard::default();
    copy_style(&mut clipboard, &source.editor, &id);
    let mut destination = session("bulk-target");
    let page = destination.document().pages[0].id.clone();
    let commands = (0..251)
        .map(|index| Command::AddObject {
            location: ObjectLocation::Page(page.clone()),
            object: ObjectDraft {
                name: format!("Target {index}"),
                size: PhysicalSize::new(10_000, 10_000).unwrap(),
                transform: Transform::identity_at(0, 0),
                opacity_millionths: 1_000_000,
                kind: shape(ShapeKind::Rectangle),
            },
        })
        .collect();
    destination
        .execute(
            destination.revision(),
            Transaction {
                label: "Targets".into(),
                commands,
            },
        )
        .unwrap();
    let ids = destination.document().pages[0]
        .objects
        .iter()
        .map(|object| object.id.clone())
        .collect::<Vec<_>>();
    let mut destination = Fixture::new("bulk-target", destination, &AssetStore::default());
    let before = destination.editor.snapshot().unwrap();
    let intent = style_paste(&destination.editor, &ids);
    let error = clipboard
        .paste_style(&mut destination.editor, intent)
        .unwrap_err();
    assert!(error.message.contains("fewer"));
    assert_eq!(destination.editor.snapshot().unwrap(), before);
    assert!(clipboard.has_style());
}
#[test]
fn paint_only_style_does_not_require_unbound_fit_font() {
    let mut source = session("paint-only");
    let id = add(&mut source, shape(ShapeKind::Ellipse));
    source
        .execute(
            source.revision(),
            Transaction::single(
                "Opacity",
                Command::SetOpacity {
                    object_id: id.clone(),
                    opacity_millionths: 500_000,
                },
            ),
        )
        .unwrap();
    let source = Fixture::new("paint-only", source, &AssetStore::default());
    let mut clipboard = SessionClipboard::default();
    copy_style(&mut clipboard, &source.editor, &id);
    let mut destination = session("unbound-fit");
    let target = add(
        &mut destination,
        ObjectKind::Text {
            text: "An unbound Fit face".into(),
            style: TextStyle::default(),
            frame: TextFrame {
                sizing_mode: document_core::model::TextBoxSizingMode::Fit,
                ..TextFrame::default()
            },
            story: TextStoryFormatting::default(),
        },
    );
    let mut destination = Fixture::new("unbound-fit", destination, &AssetStore::default());
    let before = destination.editor.snapshot().unwrap();
    let intent = style_paste(&destination.editor, &[target]);
    let result = clipboard
        .paste_style(&mut destination.editor, intent)
        .unwrap();
    assert_eq!(
        result.snapshot.project.document.pages[0].objects[0].kind,
        before.project.document.pages[0].objects[0].kind
    );
    assert_eq!(
        result.snapshot.project.document.pages[0].objects[0].opacity_millionths,
        500_000
    );
}
