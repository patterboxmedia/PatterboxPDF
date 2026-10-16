use application_core::{
    ApplyTransactionInput, CaptureClipboardInput, CaptureStyleClipboardInput,
    ClipboardDestinationInput, EditorModule, EditorSnapshot, FindTextInput, MasterTransactionInput,
    PasteObjectClipboardInput, PasteStyleClipboardInput, ReplaceAllTextInput, SessionClipboard,
    TextSearchScope,
};
use document_core::commands::HistoryRetentionLimits;
use document_core::model::{
    DocumentId, FontFaceBinding, ImageContentKind, ImageCrop, ImageFlip, Insets, ObjectDraft,
    ObjectKind, PhysicalSize, ShapeKind, ShapeStroke, TableData, TextField, TextFrame,
    TextStoryFormatting, TextStyle, Transform, WrapMode,
};
use document_core::package::{SCHEMA_VERSION, open_package};
use document_core::{
    AssetHash, AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot,
    Transaction, save_new_package,
};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const FONT: &[u8] = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
const BOLD: &[u8] = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans-Bold.ttf");
const PICTURE: &[u8] = include_bytes!("../../raster-boundary/tests/fixtures/valid-3x2.png");

struct Fixture {
    folder: PathBuf,
    font: AssetHash,
    bold: AssetHash,
    picture: AssetHash,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.folder);
    }
}
fn text(source: &AssetHash, weight: u16, value: &str) -> ObjectKind {
    ObjectKind::Text {
        text: value.into(),
        style: TextStyle {
            font_weight: weight,
            font_face: Some(FontFaceBinding {
                source: source.clone(),
                face_index: 0,
            }),
            ..TextStyle::default()
        },
        frame: TextFrame::default(),
        story: TextStoryFormatting::default(),
    }
}
fn legacy_fixture() -> Fixture {
    let folder = std::env::temp_dir().join(format!(
        "publisher-u19-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&folder).unwrap();
    let mut assets = AssetStore::default();
    let font = assets.insert("font/ttf", FONT.to_vec());
    let bold = assets.insert("font/ttf", BOLD.to_vec());
    let picture = assets.insert_raster(
        PICTURE.to_vec(),
        document_core::inspect_raster(PICTURE).unwrap(),
    );
    let mut table = TableData::empty(2, 2).unwrap();
    table.cells = vec![
        "alpha anchor".into(),
        String::new(),
        String::new(),
        "alpha cell".into(),
    ];
    table.style.font_face = Some(FontFaceBinding {
        source: font.clone(),
        face_index: 0,
    });
    let mut session = DocumentSession::new(
        Document::new_with_page_count(
            DocumentId::parse("combined-authoring").unwrap(),
            "Existing publication",
            PhysicalSize::new(215_900, 279_400).unwrap(),
            Insets::default(),
            2,
        )
        .unwrap(),
    )
    .unwrap();
    let location = ObjectLocation::Page(session.document().pages[0].id.clone());
    let kinds = [
        text(&font, 400, "alpha body"),
        text(&bold, 700, "alpha archive"),
        ObjectKind::Table { table },
        ObjectKind::Image {
            asset: picture.clone(),
            content_kind: ImageContentKind::ContinuousTone,
            crop: ImageCrop::new(100_000, 0, 100_000, 0).unwrap(),
            flip: ImageFlip::default(),
            lock_aspect_ratio: false,
            wrap_mode: WrapMode::None,
        },
        ObjectKind::Shape {
            shape: ShapeKind::Rectangle,
            fill: document_core::model::Color {
                red: 20,
                green: 70,
                blue: 130,
                alpha: 255,
            },
            stroke: ShapeStroke::default(),
            wrap_mode: WrapMode::None,
        },
    ];
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Existing content".into(),
                commands: kinds
                    .into_iter()
                    .map(|kind| Command::AddObject {
                        location: location.clone(),
                        object: ObjectDraft {
                            name: "Existing object".into(),
                            kind,
                            size: PhysicalSize::new(80_000, 40_000).unwrap(),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 600_000,
                        },
                    })
                    .collect(),
            },
        )
        .unwrap();
    let path = folder.join("original.publisher-v2");
    save_new_package(&path, &ProjectSnapshot::from_session(&session), &assets).unwrap();
    rewrite_as_schema17(&path);
    Fixture {
        folder,
        font,
        bold,
        picture,
    }
}
fn rewrite_as_schema17(path: &Path) {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let entries = (0..archive.len())
        .map(|index| {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().to_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            if name == "manifest.json" {
                let mut manifest: Value = serde_json::from_slice(&bytes).unwrap();
                // The seeded body uses only schema17 fields; new sparse fields must be absent.
                let body = serde_json::to_string(&manifest).unwrap();
                for field in [
                    "masters",
                    "corner_rounding",
                    "merged_cells",
                    "text_field",
                    "exact_font_families",
                ] {
                    assert!(
                        !body.contains(&format!("\"{field}\"")),
                        "legacy fixture contains {field}"
                    );
                }
                manifest["schema_version"] = json!(17);
                bytes = serde_json::to_vec(&manifest).unwrap();
            }
            (name, bytes)
        })
        .collect::<Vec<_>>();
    drop(archive);
    let mut output = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    for (name, bytes) in entries {
        output
            .start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        output.write_all(&bytes).unwrap();
    }
    output.finish().unwrap();
}
fn apply(editor: &mut EditorModule, commands: Value) -> EditorSnapshot {
    let revision = editor.snapshot().unwrap().project.revision.value();
    editor
        .apply(ApplyTransactionInput {
            expected_revision: revision,
            label: "Combined authoring".into(),
            commands: serde_json::from_value(commands).unwrap(),
        })
        .unwrap()
}
fn master(editor: &mut EditorModule, id: &str, commands: Value) -> EditorSnapshot {
    let revision = editor.snapshot().unwrap().project.revision.value();
    editor
        .apply_master(MasterTransactionInput {
            master_id: id.into(),
            transaction: ApplyTransactionInput {
                expected_revision: revision,
                label: "Master authoring".into(),
                commands: serde_json::from_value(commands).unwrap(),
            },
        })
        .unwrap()
}
fn paste_input(
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
fn assert_resources(path: &Path, fixture: &Fixture, picture: bool) {
    let package = open_package(path).unwrap();
    assert_eq!(package.schema_version, SCHEMA_VERSION);
    assert_eq!(package.assets.get(&fixture.font).unwrap().bytes, FONT);
    assert_eq!(package.assets.get(&fixture.bold).unwrap().bytes, BOLD);
    assert_eq!(package.assets.len(), if picture { 3 } else { 2 });
    if picture {
        assert_eq!(package.assets.get(&fixture.picture).unwrap().bytes, PICTURE);
    } else {
        assert!(package.assets.get(&fixture.picture).is_none());
    }
}

fn replace_input(editor: &EditorModule, replacement: String) -> ReplaceAllTextInput {
    ReplaceAllTextInput {
        document_id: "combined-authoring".into(),
        expected_revision: editor.snapshot().unwrap().project.revision.value(),
        query: "alpha".into(),
        replacement,
        match_case: true,
        whole_word: true,
        scope: TextSearchScope::Publication,
    }
}

#[test]
#[allow(clippy::too_many_lines)] // One cross-feature journey follows exact owners through history, recovery and verified upgrade copies.
fn new_authoring_preserves_legacy_ancestry_and_independent_resource_owners() {
    let fixture = legacy_fixture();
    let original = fixture.folder.join("original.publisher-v2");
    let copy = fixture.folder.join("upgraded.publisher-v2");
    let original_bytes = std::fs::read(&original).unwrap();
    let mut editor = EditorModule::default();
    let opened = editor.open(&original).unwrap();
    assert_eq!(opened.source_schema_version, 17);
    assert!(opened.requires_save_copy);
    assert!(!opened.is_dirty);
    let page = opened.project.document.pages[0].id.as_str().to_owned();
    let ids = opened.project.document.pages[0]
        .objects
        .iter()
        .map(|object| object.id.as_str().to_owned())
        .collect::<Vec<_>>();
    let archive_text = opened.project.document.pages[0].objects[1].clone();
    let created = apply(
        &mut editor,
        json!([
        {"type":"addMaster","name":"Footer","trimWidthUm":215_900,"trimHeightUm":279_400},
        {"type":"addMaster","name":"Unassigned archive","trimWidthUm":215_900,"trimHeightUm":279_400}
        ]),
    );
    let assigned = created.project.document.masters[0].id.as_str().to_owned();
    let archive = created.project.document.masters[1].id.as_str().to_owned();
    master(
        &mut editor,
        &assigned,
        json!([
        {"type":"addText","pageId":null,"name":"Footer","text":"alpha footer","xUm":10000,"yUm":240_000,"widthUm":80000,"heightUm":15000},
        {"type":"addPageNumber","pageId":null,"name":"Automatic number","xUm":10000,"yUm":260_000,"widthUm":25000,"heightUm":15000}
        ]),
    );
    master(
        &mut editor,
        &archive,
        json!([
            {"type":"pasteObjects","objects":[archive_text],"destinationPageId":null,"offsetXUm":0,"offsetYUm":0}
        ]),
    );
    let numbered = apply(
        &mut editor,
        json!([
            {"type":"deleteObject","objectId":ids[1]},
            {"type":"assignMaster","pageIds":created.project.document.pages.iter().map(|page| page.id.as_str()).collect::<Vec<_>>(),"masterId":assigned},
            {"type":"updatePageNumbering","pageId":page,"section":{"format":"romanLower","start":7}},
            {"type":"renamePage","pageId":page,"name":"Opening page"},
            {"type":"mergeTableCells","objectId":ids[2],"range":{"startRow":0,"endRow":1,"startColumn":0,"endColumn":2}},
            {"type":"updateTableCellFormatting","objectId":ids[2],"range":{"startRow":1,"endRow":2,"startColumn":1,"endColumn":2},
                "formatting":{"style":{"underline":true,"color":{"red":140,"green":20,"blue":50,"alpha":150}},
                    "fill":{"red":200,"green":210,"blue":220,"alpha":255},"borders":{"bottom":{"color":{"red":0,"green":0,"blue":0,"alpha":255},"widthMillipoints":1200}}}},
            {"type":"updateCornerRounding","objectId":ids[4],"topLeftUm":6000,"topRightUm":4000,"bottomRightUm":2000,"bottomLeftUm":1000,"linked":false}
        ]),
    );
    let mut clipboard = SessionClipboard::default();
    let object_plan = editor
        .plan_object_clipboard_capture(CaptureClipboardInput {
            document_id: "combined-authoring".into(),
            expected_revision: numbered.project.revision.value(),
            object_ids: vec![ids[3].clone()],
            cut: false,
        })
        .unwrap();
    assert!(object_plan.font_resources().is_empty());
    clipboard
        .capture_objects(&mut editor, object_plan, vec![])
        .unwrap();
    let style_plan = editor
        .plan_style_clipboard_capture(CaptureStyleClipboardInput {
            document_id: "combined-authoring".into(),
            expected_revision: numbered.project.revision.value(),
            object_id: ids[4].clone(),
        })
        .unwrap();
    assert!(style_plan.font_resources().is_empty());
    clipboard
        .capture_style(&editor, style_plan, vec![])
        .unwrap();
    assert_eq!(
        editor.snapshot().unwrap(),
        numbered,
        "captures are session values, not edits"
    );
    assert!(clipboard.has_objects() && clipboard.has_style());
    let style_before = editor.snapshot().unwrap();
    let styled = clipboard
        .paste_style(
            &mut editor,
            PasteStyleClipboardInput {
                document_id: "combined-authoring".into(),
                expected_revision: style_before.project.revision.value(),
                object_ids: vec![ids[2].clone()],
            },
        )
        .unwrap()
        .snapshot;
    let table_object = &styled.project.document.pages[0].objects[1];
    assert_eq!(
        table_object.corner_rounding,
        styled.project.document.pages[0].objects[3].corner_rounding
    );
    let ObjectKind::Table { table } = &table_object.kind else {
        panic!("expected Table")
    };
    assert_eq!(table.merged_cells.len(), 1);
    assert_eq!(table.cell_formatting.len(), 1);
    assert_eq!(table.style.font_face.as_ref().unwrap().source, fixture.font);
    let table_metadata = table.clone();
    let undone = editor.undo(styled.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, style_before.project.document);
    editor.redo(undone.project.revision.value()).unwrap();
    let input = paste_input(
        &editor,
        ClipboardDestinationInput::Master {
            master_id: assigned.clone(),
        },
    );
    let pasted = clipboard.paste_objects(&mut editor, input).unwrap();
    let master_picture = pasted.project.document.masters[0]
        .objects
        .last()
        .unwrap()
        .clone();
    assert_ne!(master_picture.id.as_str(), ids[3]);
    assert_eq!(
        master_picture.kind,
        opened.project.document.pages[0].objects[3].kind
    );
    apply(
        &mut editor,
        json!([{"type":"deleteObject","objectId":ids[3]}]),
    );
    let before_replace = editor.snapshot().unwrap();
    let found = editor
        .find_text(FindTextInput {
            document_id: "combined-authoring".into(),
            expected_revision: before_replace.project.revision.value(),
            query: "alpha".into(),
            match_case: true,
            whole_word: true,
            scope: TextSearchScope::Publication,
            cursor: None,
            limit: 500,
        })
        .unwrap();
    assert_eq!(
        found.matches.len(),
        5,
        "Master definitions occur once, number fields never occur"
    );
    let excessive_cell = replace_input(&editor, "x".repeat(32 * 1024));
    editor.replace_all_text(excessive_cell).unwrap_err();
    assert_eq!(
        editor.snapshot().unwrap(),
        before_replace,
        "one excessive Table cell refuses every owner and history"
    );
    assert_eq!(
        editor
            .referenced_raster(fixture.picture.as_str())
            .unwrap()
            .bytes,
        PICTURE
    );
    let input = replace_input(&editor, "beta".into());
    let replaced = editor.replace_all_text(input).unwrap();
    assert_eq!(replaced.replaced, 5);
    let ObjectKind::Table { table } = &replaced.snapshot.project.document.pages[0].objects[1].kind
    else {
        panic!("expected Table")
    };
    assert_eq!(table.cell_formatting, table_metadata.cell_formatting);
    assert_eq!(table.merged_cells, table_metadata.merged_cells);
    assert_eq!(table.style, table_metadata.style);
    assert_eq!(table.cells[0], "beta anchor");
    assert_eq!(table.cells[3], "beta cell");
    let document = &replaced.snapshot.project.document;
    assert_eq!(
        document.masters[0].objects[1].text_field,
        Some(TextField::PageNumber)
    );
    let numbers = document.page_numbers().unwrap();
    assert_eq!(
        numbers
            .iter()
            .map(|number| number.display.as_str())
            .collect::<Vec<_>>(),
        ["vii", "viii"]
    );
    let composed = document.composed_page(&document.pages[1].id).unwrap();
    assert!(
        composed
            .objects
            .iter()
            .any(|object| matches!(&object.kind, ObjectKind::Text { text, .. } if text == "viii"))
    );
    let undone = editor
        .undo(replaced.snapshot.project.revision.value())
        .unwrap();
    assert_eq!(
        undone.project.document, before_replace.project.document,
        "Replace All is one Undo across owners"
    );
    let accepted = editor.redo(undone.project.revision.value()).unwrap();
    let candidate = editor.autosave(&fixture.folder.join("recovery")).unwrap();
    assert_eq!(candidate.source_schema_version, 17);
    assert_resources(&candidate.package_path, &fixture, true);
    let mut recovered = EditorModule::default();
    let recovery = recovered.open_recovery(&candidate).unwrap();
    assert_eq!(recovery.project, accepted.project);
    assert_eq!(recovery.source_schema_version, 17);
    assert!(recovery.requires_save_copy && recovery.is_dirty && !recovery.has_project_path);
    recovered.save_current().unwrap_err();
    recovered.save_new(&original).unwrap_err();
    recovered
        .save_new(
            &fixture
                .folder
                .join("missing-parent")
                .join("copy.publisher-v2"),
        )
        .unwrap_err();
    assert_eq!(recovered.snapshot().unwrap(), recovery);
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
    recovered.save_new(&copy).unwrap();
    let saved = recovered.snapshot().unwrap();
    assert!(!saved.requires_save_copy && !saved.is_dirty);
    assert_eq!(saved.source_schema_version, SCHEMA_VERSION);
    assert_resources(&copy, &fixture, true);
    let mut reopened = EditorModule::default();
    assert_eq!(reopened.open(&copy).unwrap().project, accepted.project);
    assert_eq!(
        reopened
            .referenced_raster(fixture.picture.as_str())
            .unwrap()
            .bytes,
        PICTURE
    );
    let deleted = apply(
        &mut reopened,
        json!([{"type":"deleteObject","objectId":master_picture.id.as_str()}]),
    );
    reopened
        .referenced_raster(fixture.picture.as_str())
        .unwrap_err();
    reopened.save_current().unwrap();
    assert_resources(&copy, &fixture, false);
    let undone = reopened.undo(deleted.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, accepted.project.document);
    assert_eq!(
        reopened
            .referenced_raster(fixture.picture.as_str())
            .unwrap()
            .bytes,
        PICTURE
    );
    let redone = reopened.redo(undone.project.revision.value()).unwrap();
    reopened
        .set_history_retention_limits(HistoryRetentionLimits {
            max_transactions: 0,
            max_snapshot_bytes: 0,
            max_history_asset_bytes: 0,
        })
        .unwrap();
    assert!(!reopened.snapshot().unwrap().can_undo);
    let after_release = reopened
        .autosave(&fixture.folder.join("recovery-after-release"))
        .unwrap();
    assert_resources(&after_release.package_path, &fixture, false);
    reopened.save_current().unwrap();
    reopened
        .release_clean_document(redone.project.revision.value())
        .unwrap();
    drop(editor);
    drop(recovered);
    let mut replacement = EditorModule::default();
    replacement.open(&copy).unwrap();
    let input = paste_input(
        &replacement,
        ClipboardDestinationInput::Page { page_id: page },
    );
    let restored = clipboard.paste_objects(&mut replacement, input).unwrap();
    let restored_picture = restored.project.document.pages[0].objects.last().unwrap();
    assert_ne!(restored_picture.id, master_picture.id);
    assert_eq!(restored_picture.kind, master_picture.kind);
    assert_eq!(
        replacement
            .referenced_raster(fixture.picture.as_str())
            .unwrap()
            .bytes,
        PICTURE
    );
    replacement.save_current().unwrap();
    assert_resources(&copy, &fixture, true);
    assert_eq!(open_package(&copy).unwrap().snapshot, restored.project);
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
}
