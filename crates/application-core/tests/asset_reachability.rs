use application_core::{
    ApplyTransactionInput, CaptureClipboardInput, CreateDocumentInput, EditorCommandInput,
    EditorModule, TextBoxSizingModeInput, VerifiedImageImport, VerifiedImageReplacement,
};
use document_core::model::{FontFaceBinding, ObjectKind};
use document_core::{
    AssetHash, ProjectSnapshot, RasterInspection, inspect_raster, open_package, save_new_package,
};
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn capture(
    editor: &mut EditorModule,
    ids: Vec<String>,
    cut: bool,
) -> application_core::ClipboardSelection {
    let snapshot = editor.snapshot().unwrap();
    editor
        .capture_clipboard(CaptureClipboardInput {
            document_id: snapshot.project.document.id.as_str().to_owned(),
            expected_revision: snapshot.project.revision.value(),
            object_ids: ids,
            cut,
        })
        .unwrap()
}

#[test]
fn cut_picture_and_font_survive_history_retirement_save_and_paste() {
    let temp = TempDirectory::new("cut-resources");
    let source = temp.path().join("source.publisher-v2");
    let seed = seed_fixture(&source);
    let mut editor = EditorModule::new();
    let before = editor.open(&source).unwrap();
    let ids = vec![seed.text_id.clone(), seed.picture_id.clone()];
    let cut = capture(&mut editor, ids, true);
    assert!(
        cut.snapshot.as_ref().unwrap().project.document.pages[0]
            .objects
            .is_empty()
    );
    let cut_revision = cut.snapshot.unwrap().project.revision.value();
    assert_eq!(
        editor.undo(cut_revision).unwrap().project.document,
        before.project.document
    );
    editor.redo(cut_revision + 1).unwrap();
    editor
        .set_history_retention_limits(document_core::commands::HistoryRetentionLimits {
            max_transactions: 0,
            ..Default::default()
        })
        .unwrap();
    assert!(!editor.snapshot().unwrap().can_undo);
    let empty = temp.path().join("after-cut.publisher-v2");
    editor.save_new(&empty).unwrap();
    assert_exact_package_assets(&empty, &[]);
    let recovery = editor.autosave(&temp.path().join("recovery")).unwrap();
    assert!(
        document_core::open_package(&recovery.package_path)
            .unwrap()
            .assets
            .is_empty()
    );
    let revision = editor.snapshot().unwrap().project.revision.value();
    let pasted = editor
        .apply(transaction(
            revision,
            "Paste",
            vec![EditorCommandInput::PasteObjects {
                objects: cut.objects,
                destination_page_id: cut.source_page_id.map(|id| id.as_str().to_owned()),
                offset_x_um: 0,
                offset_y_um: 0,
            }],
        ))
        .unwrap();
    for (original, pasted) in before.project.document.pages[0]
        .objects
        .iter()
        .zip(&pasted.project.document.pages[0].objects)
    {
        let mut expected = original.clone();
        expected.id = pasted.id.clone();
        assert_eq!(&expected, pasted);
    }
    let saved = temp.path().join("pasted.publisher-v2");
    editor.save_new(&saved).unwrap();
    let reopened = assert_exact_package_assets(&saved, &expected_seed_assets(&seed));
    assert_eq!(reopened, pasted.project);
}

#[test]
fn clipboard_resources_are_replaced_and_released_with_the_document() {
    let temp = TempDirectory::new("clipboard-lifetime");
    let source = temp.path().join("source.publisher-v2");
    let seed = seed_fixture(&source);
    let mut editor = EditorModule::new();
    editor.open(&source).unwrap();
    editor
        .set_history_retention_limits(document_core::commands::HistoryRetentionLimits {
            max_transactions: 0,
            ..Default::default()
        })
        .unwrap();
    capture(&mut editor, vec![seed.picture_id.clone()], true);
    let revision = editor.snapshot().unwrap().project.revision.value();
    assert!(
        editor
            .snapshot_for_preflight(revision)
            .unwrap()
            .assets
            .contains(&seed.picture_hash)
    );
    capture(&mut editor, vec![seed.text_id], false);
    let resources = editor.snapshot_for_preflight(revision).unwrap().assets;
    assert!(!resources.contains(&seed.picture_hash));
    assert!(resources.contains(&seed.font_hash));
    editor.create(create_input()).unwrap();
    assert!(editor.snapshot_for_preflight(0).unwrap().assets.is_empty());
}

#[test]
fn cut_refuses_locked_groups_stale_identity_and_partial_selections_atomically() {
    let temp = TempDirectory::new("cut-refusal");
    let source = temp.path().join("source.publisher-v2");
    let seed = seed_fixture(&source);
    let mut editor = EditorModule::new();
    let opened = editor.open(&source).unwrap();
    let ids = vec![seed.text_id, seed.picture_id];
    let grouped = editor
        .apply(transaction(
            opened.project.revision.value(),
            "Group",
            vec![EditorCommandInput::GroupObjects {
                object_ids: ids.clone(),
            }],
        ))
        .unwrap();
    let base = CaptureClipboardInput {
        document_id: grouped.project.document.id.as_str().to_owned(),
        expected_revision: grouped.project.revision.value(),
        object_ids: ids.clone(),
        cut: true,
    };
    for input in [
        CaptureClipboardInput {
            object_ids: vec![ids[0].clone()],
            ..base.clone()
        },
        CaptureClipboardInput {
            document_id: "another-publication".to_owned(),
            ..base.clone()
        },
        CaptureClipboardInput {
            expected_revision: 0,
            ..base.clone()
        },
        CaptureClipboardInput {
            object_ids: vec![ids[0].clone(), ids[0].clone()],
            ..base.clone()
        },
    ] {
        assert!(editor.capture_clipboard(input).is_err());
        assert_eq!(editor.snapshot().unwrap(), grouped);
    }
    let locked = editor
        .apply(transaction(
            grouped.project.revision.value(),
            "Lock",
            vec![EditorCommandInput::SetObjectLock {
                object_ids: ids.clone(),
                locked: true,
            }],
        ))
        .unwrap();
    assert!(
        editor
            .capture_clipboard(CaptureClipboardInput {
                expected_revision: locked.project.revision.value(),
                ..base
            })
            .is_err()
    );
    assert_eq!(editor.snapshot().unwrap(), locked);
    let copy = capture(&mut editor, ids, false);
    assert_eq!(copy.objects, locked.project.document.pages[0].objects);
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "publisher-v2-{label}-{}-{sequence}",
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

fn create_input() -> CreateDocumentInput {
    CreateDocumentInput {
        document_id: "asset-reachability-test".to_owned(),
        title: "Asset reachability test".to_owned(),
        trim_width_um: 215_900,
        trim_height_um: 279_400,
        bleed_um: 3_175,
        page_count: 1,
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

fn png_fixture(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(width, height)
        .write_to(&mut bytes, ImageFormat::Png)
        .expect("encode PNG");
    bytes.into_inner()
}

struct ExpectedAsset<'a> {
    hash: &'a AssetHash,
    bytes: &'a [u8],
    media_type: &'a str,
    raster: Option<&'a RasterInspection>,
}

fn assert_exact_package_assets(path: &Path, expected: &[ExpectedAsset<'_>]) -> ProjectSnapshot {
    let opened = open_package(path).expect("reopen saved package through document boundary");
    assert_eq!(opened.assets.len(), expected.len());
    for expected in expected {
        let asset = opened
            .assets
            .get(expected.hash)
            .expect("expected reachable asset");
        assert_eq!(asset.bytes, expected.bytes);
        assert_eq!(asset.media_type, expected.media_type);
        assert_eq!(asset.raster.as_ref(), expected.raster);
    }
    opened.snapshot
}

struct SeedFixture {
    project: ProjectSnapshot,
    text_id: String,
    picture_id: String,
    picture_hash: AssetHash,
    picture_bytes: Vec<u8>,
    picture_inspection: RasterInspection,
    font_hash: AssetHash,
    font_bytes: Vec<u8>,
}

fn seed_fixture(path: &Path) -> SeedFixture {
    let picture_bytes = png_fixture(80, 40);
    let picture_inspection = inspect_raster(&picture_bytes).expect("inspect first PNG");
    let picture_hash = AssetHash::for_bytes(&picture_bytes);
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let font_hash = AssetHash::for_bytes(&font_bytes);
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create fixture");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let with_text = editor
        .apply(transaction(
            0,
            "Add exact-font Text Box",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id.clone()),
                name: "Exact-font Text Box".to_owned(),
                text: "Embedded exactly".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 20_000,
                width_um: 80_000,
                height_um: 20_000,
            }],
        ))
        .expect("add fixture Text Box");
    let text_id = with_text.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let with_picture = editor
        .import_verified_image(VerifiedImageImport {
            expected_revision: 1,
            page_id: Some(page_id),
            name: "Original Picture.png".to_owned(),
            x_um: 20_000,
            y_um: 60_000,
            width_um: 60_000,
            height_um: 30_000,
            media_type: picture_inspection.media_type.clone(),
            asset_hash: picture_hash.clone(),
            inspection: picture_inspection.clone(),
            bytes: picture_bytes.clone(),
        })
        .expect("import fixture Picture");
    let picture_id = with_picture.project.document.pages[0].objects[1]
        .id
        .as_str()
        .to_owned();
    let mut seeded = editor
        .snapshot_for_preflight(2)
        .expect("capture fixture project and Picture resource");
    let ObjectKind::Text { style, .. } = &mut seeded.project.document.pages[0].objects[0].kind
    else {
        panic!("fixture must contain a Text Box");
    };
    "DejaVu Sans".clone_into(&mut style.font_family);
    style.font_face = Some(FontFaceBinding {
        source: font_hash.clone(),
        face_index: 0,
    });
    assert_eq!(
        seeded.assets.insert("font/ttf", font_bytes.clone()),
        font_hash
    );
    save_new_package(path, &seeded.project, &seeded.assets).expect("save fixture package");
    SeedFixture {
        project: seeded.project,
        text_id,
        picture_id,
        picture_hash,
        picture_bytes,
        picture_inspection,
        font_hash,
        font_bytes,
    }
}

fn expected_seed_assets(seed: &SeedFixture) -> [ExpectedAsset<'_>; 2] {
    [
        ExpectedAsset {
            hash: &seed.picture_hash,
            bytes: &seed.picture_bytes,
            media_type: "image/png",
            raster: Some(&seed.picture_inspection),
        },
        ExpectedAsset {
            hash: &seed.font_hash,
            bytes: &seed.font_bytes,
            media_type: "font/ttf",
            raster: None,
        },
    ]
}

fn assert_pruned_application(
    path: &Path,
    expected_project: &ProjectSnapshot,
    hash: &AssetHash,
    bytes: &[u8],
    inspection: &RasterInspection,
) {
    let mut reopened = EditorModule::new();
    let snapshot = reopened
        .open(path)
        .expect("reopen pruned publication through EditorModule");
    assert_eq!(&snapshot.project, expected_project);
    let resources = reopened
        .snapshot_for_preflight(snapshot.project.revision.value())
        .expect("read pruned resources through public preflight boundary")
        .assets;
    assert_eq!(resources.len(), 1);
    let replacement = resources
        .get(hash)
        .expect("replacement Picture remains reachable");
    assert_eq!(replacement.bytes, bytes);
    assert_eq!(replacement.media_type, "image/png");
    assert_eq!(replacement.raster.as_ref(), Some(inspection));
}

#[test]
fn public_save_filters_replaced_and_deleted_assets_then_undo_restores_exact_resources() {
    let temp = TempDirectory::new("application-asset-reachability");
    let seed_path = temp.path().join("seed.publisher-v2");
    let package_path = temp.path().join("public-save.publisher-v2");
    let seed = seed_fixture(&seed_path);
    let replacement_picture_bytes = png_fixture(81, 41);
    let replacement_picture_inspection =
        inspect_raster(&replacement_picture_bytes).expect("inspect replacement PNG");
    let replacement_picture_hash = AssetHash::for_bytes(&replacement_picture_bytes);

    let mut editor = EditorModule::new();
    editor
        .open(&seed_path)
        .expect("open fixture through EditorModule");
    editor
        .save_new(&package_path)
        .expect("write both referenced resources through public Save");
    let initially_saved = assert_exact_package_assets(&package_path, &expected_seed_assets(&seed));
    assert_eq!(initially_saved, seed.project);

    editor
        .replace_verified_image(VerifiedImageReplacement {
            expected_revision: 2,
            object_id: seed.picture_id.clone(),
            name: "Replacement Picture.png".to_owned(),
            asset_hash: replacement_picture_hash.clone(),
            inspection: replacement_picture_inspection.clone(),
            bytes: replacement_picture_bytes.clone(),
        })
        .expect("replace Picture through public boundary");
    editor
        .apply(transaction(
            3,
            "Delete exact-font Text Box",
            vec![EditorCommandInput::DeleteObject {
                object_id: seed.text_id.clone(),
            }],
        ))
        .expect("delete exact-font Text Box through public boundary");
    editor
        .save_current()
        .expect("save after replacement and deletion");
    let omitted = assert_exact_package_assets(
        &package_path,
        &[ExpectedAsset {
            hash: &replacement_picture_hash,
            bytes: &replacement_picture_bytes,
            media_type: "image/png",
            raster: Some(&replacement_picture_inspection),
        }],
    );
    assert_eq!(omitted.document.pages[0].objects.len(), 1);
    assert_pruned_application(
        &package_path,
        &omitted,
        &replacement_picture_hash,
        &replacement_picture_bytes,
        &replacement_picture_inspection,
    );

    editor.undo(4).expect("undo Text Box deletion");
    let restored = editor.undo(5).expect("undo Picture replacement");
    assert_eq!(restored.project.document, seed.project.document);
    editor
        .save_current()
        .expect("save exact resources restored by Undo");
    let reopened = assert_exact_package_assets(&package_path, &expected_seed_assets(&seed));
    assert_eq!(reopened, restored.project);

    let mut final_reopen = EditorModule::new();
    let final_snapshot = final_reopen
        .open(&package_path)
        .expect("reopen restored publication through EditorModule");
    assert_eq!(final_snapshot.project, restored.project);
    let resources = final_reopen
        .snapshot_for_preflight(final_snapshot.project.revision.value())
        .expect("read restored resources through public preflight boundary")
        .assets;
    assert_eq!(resources.len(), 2);
    assert_eq!(
        resources.get(&seed.picture_hash).unwrap().bytes,
        seed.picture_bytes
    );
    assert_eq!(
        resources.get(&seed.font_hash).unwrap().bytes,
        seed.font_bytes
    );
    assert!(!resources.contains(&replacement_picture_hash));
}
