use application_core::{
    ApplyTransactionInput, ArrangeGeometryOperationInput, ColorInput, CreateDocumentInput,
    DocumentLifecycleState, EditorCommandInput, EditorErrorCode, EditorModule,
    FontFaceBindingInput, GuideAxisInput, HardShadowInput, ImageContentKindInput, ImageCropInput,
    ObjectEffectsInput, ParagraphListStyleInput, ParagraphStyleInput, RotateObjectInput,
    ShapeInput, ShapeStrokeInput, TableGridBorderInput, TableTrackAxisInput,
    TableVerticalAlignmentInput, TextAlignmentInput, TextBoxSizingModeInput, TextEditInput,
    TextStyleInput, TransformObjectInput, UntaggedRasterPolicyInput, VerifiedImageImport,
    VerifiedImageReplacement, WrapModeInput,
};
use document_core::model::{
    ImageContentKind, ImageCrop, ObjectKind, UntaggedRasterPolicy, WrapMode,
};
use document_core::{AssetHash, inspect_raster};
use image::{DynamicImage, ImageFormat};
use layout_core::{ExactFontResource, ResolvedPublication};
use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn invalid_group_rotations(changes: &[RotateObjectInput]) -> Vec<Vec<RotateObjectInput>> {
    let mut invalid = Vec::new();
    invalid.push(vec![changes[0].clone()]);
    invalid.push(vec![changes[0].clone(); 501]);
    invalid.push(vec![changes[0].clone(), changes[0].clone()]);
    let mut other_page = changes.to_vec();
    other_page[1].page_id = None;
    invalid.push(other_page);
    let mut bad_angle = changes.to_vec();
    bad_angle[1].rotation_microdegrees = 180_000_000;
    invalid.push(bad_angle);
    let mut bad_position = changes.to_vec();
    bad_position[1].x_um = 10_000_001;
    invalid.push(bad_position);
    let mut missing = changes.to_vec();
    "missing".clone_into(&mut missing[1].object_id);
    invalid.push(missing);
    invalid
}

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[test]
#[allow(clippy::too_many_lines)] // Keep the group's success, history and refusal journey together.
fn rotating_a_large_group_preserves_members_and_rejects_invalid_or_locked_changes_atomically() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).unwrap();
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add 51 text boxes",
            (0..51)
                .map(|index| EditorCommandInput::AddText {
                    page_id: Some(page_id.clone()),
                    name: format!("Part {index}"),
                    text: format!("Keep {index}"),
                    style: None,
                    sizing_mode: TextBoxSizingModeInput::Fit,
                    x_um: 20_000 + index * 1_000,
                    y_um: 30_000,
                    width_um: 10_000,
                    height_um: 10_000,
                })
                .collect(),
        ))
        .unwrap();
    let grouped = editor
        .apply(transaction(
            added.project.revision.value(),
            "Group",
            vec![EditorCommandInput::GroupObjects {
                object_ids: added.project.document.pages[0]
                    .objects
                    .iter()
                    .map(|object| object.id.as_str().to_owned())
                    .collect(),
            }],
        ))
        .unwrap();
    let changes = grouped.project.document.pages[0]
        .objects
        .iter()
        .map(|object| RotateObjectInput {
            object_id: object.id.as_str().to_owned(),
            page_id: Some(page_id.clone()),
            x_um: object.transform.translation_x.value() + 5_000,
            y_um: object.transform.translation_y.value() - 7_000,
            rotation_microdegrees: 35_000_000,
        })
        .collect::<Vec<_>>();
    let rotated = editor
        .apply(transaction(
            grouped.project.revision.value(),
            "Rotate group",
            vec![EditorCommandInput::RotateObjects {
                objects: changes.clone(),
            }],
        ))
        .unwrap();
    let mut expected = grouped.project.document.clone();
    for object in &mut expected.pages[0].objects {
        object.transform.translation_x =
            document_core::model::Micrometres::new(object.transform.translation_x.value() + 5_000);
        object.transform.translation_y =
            document_core::model::Micrometres::new(object.transform.translation_y.value() - 7_000);
        object.transform.rotation_microdegrees = 35_000_000;
    }
    assert_eq!(rotated.project.document, expected);
    let undone = editor.undo(rotated.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, grouped.project.document);
    let redone = editor.redo(undone.project.revision.value()).unwrap();
    assert_eq!(redone.project.document, rotated.project.document);

    for objects in invalid_group_rotations(&changes) {
        assert!(
            editor
                .apply(transaction(
                    redone.project.revision.value(),
                    "Refused group turn",
                    vec![EditorCommandInput::RotateObjects { objects }]
                ))
                .is_err()
        );
        let current = editor.snapshot().unwrap();
        assert_eq!(current.project, redone.project);
        assert_eq!(current.can_undo, redone.can_undo);
        assert_eq!(current.can_redo, redone.can_redo);
    }
    let locked = editor
        .apply(transaction(
            redone.project.revision.value(),
            "Lock",
            vec![EditorCommandInput::SetObjectLock {
                object_ids: vec![changes[1].object_id.clone()],
                locked: true,
            }],
        ))
        .unwrap();
    assert!(
        editor
            .apply(transaction(
                locked.project.revision.value(),
                "Refused locked turn",
                vec![EditorCommandInput::RotateObjects { objects: changes }]
            ))
            .is_err()
    );
    assert_eq!(editor.snapshot().unwrap().project, locked.project);
}

#[test]
fn rotating_and_moving_keeps_the_angle_in_one_transaction_and_on_later_moves() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).unwrap();
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add",
            vec![EditorCommandInput::AddShape {
                page_id: Some(page_id.clone()),
                name: "Rotate me".to_owned(),
                shape: ShapeInput::Rectangle,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 40_000,
                height_um: 20_000,
                fill: ColorInput {
                    red: 20,
                    green: 40,
                    blue: 60,
                    alpha: 255,
                },
                stroke: None,
                corner_rounding: None,
            }],
        ))
        .unwrap();
    let original = added.project.document.pages[0].objects[0].clone();
    let object_id = original.id.as_str().to_owned();
    let rotated = editor
        .apply(transaction(
            added.project.revision.value(),
            "Rotate at center",
            vec![
                EditorCommandInput::SetRotation {
                    object_id: object_id.clone(),
                    rotation_microdegrees: 90_000_000,
                },
                EditorCommandInput::MoveObject {
                    object_id: object_id.clone(),
                    page_id: Some(page_id),
                    x_um: 50_000,
                    y_um: 20_000,
                },
            ],
        ))
        .unwrap();
    let mut expected = original;
    expected.transform.rotation_microdegrees = 90_000_000;
    expected.transform.translation_x = document_core::model::Micrometres::new(50_000);
    expected.transform.translation_y = document_core::model::Micrometres::new(20_000);
    assert_eq!(rotated.project.document.pages[0].objects[0], expected);
    let undone = editor.undo(rotated.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, added.project.document);
    let redone = editor.redo(undone.project.revision.value()).unwrap();
    assert_eq!(redone.project.document, rotated.project.document);
    let moved = editor
        .apply(transaction(
            redone.project.revision.value(),
            "Move rotated object",
            vec![EditorCommandInput::MoveObject {
                object_id,
                page_id: None,
                x_um: -5_000,
                y_um: 15_000,
            }],
        ))
        .unwrap();
    expected.transform.translation_x = document_core::model::Micrometres::new(-5_000);
    expected.transform.translation_y = document_core::model::Micrometres::new(15_000);
    assert_eq!(moved.project.document.pasteboard.objects[0], expected);
    let undone = editor.undo(moved.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, rotated.project.document);
}

#[cfg(windows)]
#[test]
fn save_retry_after_windows_sharing_failure_preserves_edits_and_history() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = TempDirectory::new("save-retry");
    let package = temp.path().join("retry.publisher-v2");
    let mut editor = EditorModule::new();
    editor.create(create_input()).unwrap();
    editor.save_new(&package).unwrap();
    let edited = editor
        .apply(transaction(
            0,
            "Add page",
            vec![EditorCommandInput::AddPage {
                after_page_id: None,
                trim_width_um: 215_900,
                trim_height_um: 279_400,
                bleed_um: 3_175,
            }],
        ))
        .unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&package)
        .unwrap();
    editor
        .save_current()
        .expect_err("sharing lock refuses save");
    assert_eq!(editor.snapshot().unwrap(), edited);
    drop(lock);
    editor
        .save_current()
        .expect("retry succeeds once the external lock is released");
    assert_eq!(
        document_core::open_package(&package).unwrap().snapshot,
        edited.project
    );
    assert!(!editor.snapshot().unwrap().is_dirty);
    assert_eq!(editor.undo(1).unwrap().project.document.pages.len(), 1);
}

#[cfg(windows)]
mod save_retry_safety {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn unsupported_first_save_location_gives_plain_guidance_and_keeps_work() {
        let mut editor = EditorModule::new();
        let before = editor.create(create_input()).unwrap();
        for path in [
            r"relative.publisher-v2",
            r"\\server\share\book.publisher-v2",
        ] {
            let error = editor.save_new(Path::new(path)).unwrap_err();
            assert!(
                error.message.contains("fixed local NTFS"),
                "{}",
                error.message
            );
            assert_eq!(editor.snapshot().unwrap(), before);
        }
    }

    fn add_page(editor: &mut EditorModule) {
        editor
            .apply(transaction(
                editor.snapshot().unwrap().project.revision.value(),
                "Add page",
                vec![EditorCommandInput::AddPage {
                    after_page_id: None,
                    trim_width_um: 215_900,
                    trim_height_um: 279_400,
                    bleed_um: 3_175,
                }],
            ))
            .unwrap();
    }

    fn fixture(label: &str) -> (TempDirectory, PathBuf, EditorModule) {
        let temp = TempDirectory::new(label);
        let path = temp.path().join("retry.publisher-v2");
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        editor.save_new(&path).unwrap();
        add_page(&mut editor);
        (temp, path, editor)
    }

    fn fail_save(editor: &mut EditorModule, path: &Path) {
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(path)
            .unwrap();
        let error = editor.save_current().unwrap_err();
        assert!(
            error.message.contains("Another program"),
            "{}",
            error.message
        );
        drop(lock);
    }

    #[test]
    fn retry_saves_edits_made_after_the_failure_without_resetting_history() {
        let (_temp, path, mut editor) = fixture("retry-latest");
        fail_save(&mut editor, &path);
        add_page(&mut editor);
        let before = editor.snapshot().unwrap();
        editor.save_current().unwrap();
        assert_eq!(
            document_core::open_package(&path).unwrap().snapshot,
            before.project
        );
        assert!(!editor.snapshot().unwrap().is_dirty);
        assert_eq!(editor.undo(2).unwrap().project.document.pages.len(), 2);
        assert_eq!(editor.undo(3).unwrap().project.document.pages.len(), 1);
    }

    #[test]
    fn unrelated_pending_attempt_never_grants_retry_authority() {
        let (_temp, path, mut owner) = fixture("retry-unrelated");
        let mut other = EditorModule::new();
        other.open(&path).unwrap();
        add_page(&mut other); // Even matching document/revision/content grants no authority.
        fail_save(&mut owner, &path);
        let before = std::fs::read(&path).unwrap();
        for _ in 0..2 {
            let error = other.save_current().unwrap_err();
            assert!(error.message.contains("earlier save"));
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(other.snapshot().unwrap().is_dirty);
        }
        owner.save_current().unwrap();
    }

    #[test]
    fn changed_destination_is_never_restored_over_on_retry() {
        let (temp, path, mut editor) = fixture("retry-changed");
        fail_save(&mut editor, &path);
        let external = temp.path().join("external.publisher-v2");
        let mut outsider = EditorModule::new();
        let mut input = create_input();
        input.document_id = "external-publication".to_owned();
        outsider.create(input).unwrap();
        outsider.save_new(&external).unwrap();
        std::fs::copy(&external, &path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let session = editor.snapshot().unwrap();
        editor.save_current().unwrap_err();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(editor.snapshot().unwrap(), session);
        let copy = temp.path().join("preserved.publisher-v2");
        editor.save_new(&copy).unwrap();
        add_page(&mut editor);
        editor.save_current().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            document_core::open_package(&copy)
                .unwrap()
                .snapshot
                .document
                .pages
                .len(),
            3
        );
    }

    #[test]
    fn changed_journal_is_refused_even_when_revision_and_document_match() {
        let (temp, path, mut editor) = fixture("retry-forged");
        fail_save(&mut editor, &path);
        let journal = temp.path().join(".retry.publisher-v2.save-journal.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        value["semantic_digest"] = serde_json::Value::String("a".repeat(64));
        std::fs::write(&journal, serde_json::to_vec(&value).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let snapshot = editor.snapshot().unwrap();
        editor.save_current().unwrap_err();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(editor.snapshot().unwrap(), snapshot);
    }
}

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new(label: &str) -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "publisher-v2-app-{label}-{}-{sequence}",
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
        document_id: "document-test".to_owned(),
        title: "Test publication".to_owned(),
        trim_width_um: 215_900,
        trim_height_um: 279_400,
        bleed_um: 3_175,
        page_count: 1,
    }
}

#[test]
fn create_builds_the_complete_publication_at_revision_zero_and_starts_dirty() {
    let mut editor = EditorModule::new();
    assert!(!editor.has_document());
    let mut input = create_input();
    input.title = "Pocket magic".to_owned();
    input.trim_width_um = 139_700;
    input.trim_height_um = 215_900;
    input.page_count = 24;

    let created = editor.create(input).expect("create configured publication");

    assert!(editor.has_document());
    assert_eq!(created.project.revision.value(), 0);
    assert_eq!(created.project.document.pages.len(), 24);
    assert!(created.is_dirty);
    assert!(!created.has_project_path);
    assert!(!created.can_undo);
    let ids = created
        .project
        .document
        .pages
        .iter()
        .map(|page| page.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 24, "initial pages need stable unique identities");
    assert!(created.project.document.pages.iter().all(|page| {
        page.trim.width.value() == 139_700
            && page.trim.height.value() == 215_900
            && page.bleed.top.value() == 3_175
    }));
}

#[test]
fn create_rejects_page_counts_outside_the_publication_bounds_without_replacing_work() {
    let mut editor = EditorModule::new();
    let original = editor.create(create_input()).expect("create original");
    for page_count in [0, 501] {
        let mut invalid = create_input();
        invalid.document_id = format!("invalid-{page_count}");
        invalid.page_count = page_count;
        let error = editor
            .create(invalid)
            .expect_err("reject invalid page count");
        assert_eq!(error.code, EditorErrorCode::InvalidInput);
        assert_eq!(editor.snapshot().expect("original remains"), original);
    }
}

#[test]
fn configured_physical_pages_survive_save_and_reopen_and_dirty_state_tracks_the_binding() {
    let temp = TempDirectory::new("configured-roundtrip");
    let package = temp.path().join("pocket-magic.publisher-v2");
    let mut editor = EditorModule::new();
    let mut input = create_input();
    input.title = "Pocket magic".to_owned();
    input.trim_width_um = 139_700;
    input.trim_height_um = 215_900;
    input.page_count = 24;
    let created = editor.create(input).expect("create configured publication");
    assert!(created.is_dirty);

    editor
        .save_new(&package)
        .expect("save configured publication");
    let saved = editor.snapshot().expect("saved snapshot");
    assert!(!saved.is_dirty);

    let page_id = saved.project.document.pages[0].id.as_str().to_owned();
    let edited = editor
        .apply(transaction(
            0,
            "Add cover title",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Cover title".to_owned(),
                text: "Magic".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 10_000,
                y_um: 10_000,
                width_um: 80_000,
                height_um: 20_000,
            }],
        ))
        .expect("edit saved publication");
    assert!(edited.is_dirty);

    editor.save_current().expect("save edited publication");
    assert!(!editor.snapshot().expect("resaved snapshot").is_dirty);

    let mut reopened = EditorModule::new();
    let reopened = reopened
        .open(&package)
        .expect("reopen configured publication");
    assert!(!reopened.is_dirty);
    assert_eq!(reopened.project.document.pages.len(), 24);
    assert!(reopened.project.document.pages.iter().all(|page| {
        page.trim.width.value() == 139_700
            && page.trim.height.value() == 215_900
            && page.bleed.top.value() == 3_175
    }));
}

#[test]
fn document_lifecycle_distinguishes_empty_clean_and_dirty_publications() {
    let temp = TempDirectory::new("document-lifecycle");
    let package = temp.path().join("lifecycle.publisher-v2");
    let mut editor = EditorModule::new();
    assert_eq!(editor.document_lifecycle(), DocumentLifecycleState::Empty);

    let created = editor.create(create_input()).expect("create publication");
    assert_eq!(
        editor.document_lifecycle(),
        DocumentLifecycleState::Dirty {
            revision: 0,
            has_project_path: false,
            requires_save_copy: false,
        }
    );

    editor.save_new(&package).expect("save publication");
    assert_eq!(
        editor.document_lifecycle(),
        DocumentLifecycleState::Clean {
            revision: 0,
            has_project_path: true,
            requires_save_copy: false,
        }
    );

    editor
        .apply(transaction(
            0,
            "Add lifecycle text",
            vec![EditorCommandInput::AddText {
                page_id: Some(created.project.document.pages[0].id.as_str().to_owned()),
                name: "Lifecycle text".to_owned(),
                text: "Unsaved".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 10_000,
                y_um: 10_000,
                width_um: 50_000,
                height_um: 10_000,
            }],
        ))
        .expect("edit publication");
    assert_eq!(
        editor.document_lifecycle(),
        DocumentLifecycleState::Dirty {
            revision: 1,
            has_project_path: true,
            requires_save_copy: false,
        }
    );
    assert_eq!(
        serde_json::to_value(editor.document_lifecycle()).expect("serialize lifecycle state"),
        serde_json::json!({
            "state": "dirty",
            "revision": 1,
            "hasProjectPath": true,
            "requiresSaveCopy": false,
        })
    );
}

#[test]
fn clean_release_requires_a_successful_save_and_the_exact_revision() {
    let temp = TempDirectory::new("clean-release");
    let package = temp.path().join("clean-release.publisher-v2");
    let mut editor = EditorModule::new();
    let dirty = editor.create(create_input()).expect("create publication");

    let error = editor
        .release_clean_document(0)
        .expect_err("unsaved publication must not be released as clean");
    assert_eq!(error.code, EditorErrorCode::CommandRejected);
    assert_eq!(editor.snapshot().expect("dirty session survives"), dirty);

    editor.save_new(&package).expect("save publication");
    let clean = editor.snapshot().expect("clean snapshot");
    let error = editor
        .release_clean_document(1)
        .expect_err("stale revision must not release publication");
    assert_eq!(error.code, EditorErrorCode::CommandRejected);
    assert_eq!(editor.snapshot().expect("clean session survives"), clean);

    editor
        .release_clean_document(0)
        .expect("release exact clean revision");
    assert_eq!(editor.document_lifecycle(), DocumentLifecycleState::Empty);
    assert!(!editor.has_document());
    assert_eq!(editor.project_path(), None);
}

#[test]
fn discard_requires_the_exact_revision_and_never_changes_the_saved_publication() {
    let temp = TempDirectory::new("explicit-discard");
    let package = temp.path().join("explicit-discard.publisher-v2");
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create publication");
    editor.save_new(&package).expect("save publication");
    let saved_bytes = std::fs::read(&package).expect("read saved publication");
    let dirty = editor
        .apply(transaction(
            0,
            "Add discarded text",
            vec![EditorCommandInput::AddText {
                page_id: Some(created.project.document.pages[0].id.as_str().to_owned()),
                name: "Discarded text".to_owned(),
                text: "Discard me".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 10_000,
                y_um: 10_000,
                width_um: 50_000,
                height_um: 10_000,
            }],
        ))
        .expect("edit publication");

    let error = editor
        .discard_document(0)
        .expect_err("stale prompt must not discard newer work");
    assert_eq!(error.code, EditorErrorCode::CommandRejected);
    assert_eq!(editor.snapshot().expect("newer work survives"), dirty);

    editor
        .discard_document(1)
        .expect("explicitly discard exact revision");
    assert_eq!(editor.document_lifecycle(), DocumentLifecycleState::Empty);
    assert_eq!(
        std::fs::read(&package).expect("reread saved publication"),
        saved_bytes,
        "discard only releases in-memory work"
    );

    let mut reopened = EditorModule::new();
    let reopened = reopened.open(&package).expect("reopen saved publication");
    assert!(reopened.project.document.pages[0].objects.is_empty());
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

fn black() -> ColorInput {
    ColorInput {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 255,
    }
}

fn png_fixture() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(80, 40)
        .write_to(&mut bytes, ImageFormat::Png)
        .expect("encode PNG");
    bytes.into_inner()
}

#[test]
fn one_interface_owns_revision_checked_page_object_and_history_work() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let first_page = created.project.document.pages[0].id.as_str().to_owned();

    let added = editor
        .apply(transaction(
            0,
            "Build cover",
            vec![
                EditorCommandInput::AddText {
                    page_id: Some(first_page.clone()),
                    name: "Cover title".to_owned(),
                    text: "Magic".to_owned(),
                    style: None,
                    sizing_mode: TextBoxSizingModeInput::Fixed,
                    x_um: 20_000,
                    y_um: 30_000,
                    width_um: 80_000,
                    height_um: 20_000,
                },
                EditorCommandInput::AddShape {
                    page_id: None,
                    name: "Candidate".to_owned(),
                    shape: ShapeInput::Ellipse,
                    x_um: -30_000,
                    y_um: 40_000,
                    width_um: 25_400,
                    height_um: 25_400,
                    fill: ColorInput {
                        red: 200,
                        green: 100,
                        blue: 50,
                        alpha: 200,
                    },
                    stroke: None,
                    corner_rounding: None,
                },
            ],
        ))
        .expect("apply one atomic transaction");
    assert_eq!(added.project.revision.value(), 1);
    assert!(added.can_undo);
    assert_eq!(added.project.document.pages[0].objects.len(), 1);
    assert_eq!(added.project.document.pasteboard.objects.len(), 1);

    let text_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let edited = editor
        .apply(transaction(
            1,
            "Edit cover title",
            vec![
                EditorCommandInput::UpdateText {
                    object_id: text_id.clone(),
                    text: "Magic and Coloring".to_owned(),
                },
                EditorCommandInput::ResizeObject {
                    object_id: text_id,
                    width_um: 100_000,
                    height_um: 22_000,
                },
                EditorCommandInput::SetOpacity {
                    object_id: added.project.document.pages[0].objects[0]
                        .id
                        .as_str()
                        .to_owned(),
                    opacity_millionths: 450_000,
                },
            ],
        ))
        .expect("edit text and geometry");
    let object = &edited.project.document.pages[0].objects[0];
    assert_eq!(object.size.width.value(), 100_000);
    assert_eq!(object.opacity_millionths, 450_000);
    assert!(matches!(&object.kind, ObjectKind::Text { text, .. } if text == "Magic and Coloring"));

    let undone = editor.undo(2).expect("undo complete transaction");
    assert_eq!(
        undone.project.document.pages[0].objects[0]
            .size
            .width
            .value(),
        80_000
    );
    assert_eq!(
        undone.project.document.pages[0].objects[0].opacity_millionths,
        1_000_000
    );
    assert!(undone.can_redo);
    let redone = editor.redo(3).expect("redo complete transaction");
    assert_eq!(
        redone.project.document.pages[0].objects[0]
            .size
            .width
            .value(),
        100_000
    );
}

#[test]
fn object_effects_cross_the_bridge_atomically_and_follow_history() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add cover title",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Cover title".to_owned(),
                text: "Magic".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 80_000,
                height_um: 20_000,
            }],
        ))
        .expect("add cover title");
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let effects = ObjectEffectsInput {
        shadow: Some(HardShadowInput {
            color: ColorInput {
                red: 20,
                green: 40,
                blue: 60,
                alpha: 180,
            },
            offset_x_um: 4_000,
            offset_y_um: 6_000,
        }),
        text_outline: Some(ShapeStrokeInput {
            color: black(),
            width_millipoints: 2_000,
        }),
        picture_border: None,
    };
    let styled = editor
        .apply(transaction(
            1,
            "Style cover title",
            vec![EditorCommandInput::UpdateObjectEffects { object_id, effects }],
        ))
        .expect("apply object effects");
    let styled_object = &styled.project.document.pages[0].objects[0];
    assert_eq!(
        styled_object
            .effects
            .shadow
            .expect("hard shadow")
            .offset_x
            .value(),
        4_000
    );
    assert_eq!(
        styled_object
            .effects
            .text_outline
            .expect("text outline")
            .width_millipoints,
        2_000
    );
    assert!(styled_object.effects.picture_border.is_none());

    let undone = editor.undo(2).expect("undo effects");
    assert!(
        undone.project.document.pages[0].objects[0]
            .effects
            .is_default()
    );
    let redone = editor.redo(3).expect("redo effects");
    assert_eq!(
        redone.project.document.pages[0].objects[0]
            .effects
            .shadow
            .expect("restored shadow")
            .offset_y
            .value(),
        6_000
    );
}

#[test]
fn invalid_effect_input_preserves_the_exact_session() {
    let mut editor = EditorModule::new();
    editor.create(create_input()).expect("create document");
    let before_rejection = editor.snapshot().expect("snapshot before rejection");
    let rejected = editor
        .apply(transaction(
            0,
            "Reject oversized shadow",
            vec![EditorCommandInput::UpdateObjectEffects {
                object_id: "object-1".to_owned(),
                effects: ObjectEffectsInput {
                    shadow: Some(HardShadowInput {
                        color: black(),
                        offset_x_um: document_core::model::HardShadow::MAX_OFFSET_MICROMETRES + 1,
                        offset_y_um: 0,
                    }),
                    ..ObjectEffectsInput::default()
                },
            }],
        ))
        .expect_err("oversized shadow is rejected at the bridge");
    assert_eq!(rejected.code, EditorErrorCode::InvalidInput);
    assert_eq!(
        editor.snapshot().expect("unchanged snapshot"),
        before_rejection
    );
}

#[test]
fn object_effects_dto_is_camel_case_and_rejects_unmodeled_blur() {
    let command: EditorCommandInput = serde_json::from_value(serde_json::json!({
        "type": "updateObjectEffects",
        "objectId": "object-1",
        "effects": {
            "shadow": {
                "color": { "red": 1, "green": 2, "blue": 3, "alpha": 255 },
                "offsetXUm": 4000,
                "offsetYUm": 6000
            },
            "textOutline": {
                "color": { "red": 4, "green": 5, "blue": 6, "alpha": 255 },
                "widthMillipoints": 2000
            },
            "pictureBorder": null
        }
    }))
    .expect("deserialize stable camel-case effects DTO");
    assert!(matches!(
        command,
        EditorCommandInput::UpdateObjectEffects {
            effects: ObjectEffectsInput {
                shadow: Some(HardShadowInput {
                    offset_x_um: 4_000,
                    offset_y_um: 6_000,
                    ..
                }),
                text_outline: Some(ShapeStrokeInput {
                    width_millipoints: 2_000,
                    ..
                }),
                picture_border: None,
            },
            ..
        }
    ));

    let blurred = serde_json::from_value::<EditorCommandInput>(serde_json::json!({
        "type": "updateObjectEffects",
        "objectId": "object-1",
        "effects": {
            "shadow": {
                "color": { "red": 1, "green": 2, "blue": 3, "alpha": 255 },
                "offsetXUm": 4000,
                "offsetYUm": 6000,
                "blurUm": 1000
            }
        }
    }));
    assert!(
        blurred.is_err(),
        "blur is outside the bounded hard-shadow model"
    );
}

#[test]
fn outlined_text_survives_the_serialized_application_bridge() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add text box",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Text Box".to_owned(),
                text: "Magic".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 80_000,
                height_um: 20_000,
            }],
        ))
        .expect("add text box");
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let command: EditorCommandInput = serde_json::from_value(serde_json::json!({
        "type": "updateObjectEffects",
        "objectId": object_id,
        "effects": {
            "shadow": null,
            "textOutline": {
                "color": { "red": 0, "green": 0, "blue": 0, "alpha": 255 },
                "widthMillipoints": 1000
            },
            "pictureBorder": null
        }
    }))
    .expect("deserialize native outline request");
    let outlined = editor
        .apply(transaction(1, "Outline text", vec![command]))
        .expect("apply outline through application boundary");
    let serialized = serde_json::to_value(&outlined).expect("serialize native response");
    let objects = serialized["project"]["document"]["pages"][0]["objects"]
        .as_array()
        .expect("serialized page object list");

    assert_eq!(objects.len(), 1, "outlining cannot remove the text box");
    assert_eq!(objects[0]["id"], object_id);
    assert_eq!(objects[0]["name"], "Text Box");
    assert_eq!(
        objects[0]["effects"]["text_outline"]["width_millipoints"],
        1_000
    );
    assert_eq!(
        outlined.project.document.pages[0].objects[0].id.as_str(),
        object_id
    );
    assert!(matches!(
        &outlined.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { text, .. } if text == "Magic"
    ));
}

#[test]
fn guide_intents_share_the_revision_checked_native_bridge() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add guide",
            vec![EditorCommandInput::AddGuide {
                page_id: page_id.clone(),
                axis: GuideAxisInput::Vertical,
                position_um: 25_400,
            }],
        ))
        .expect("add guide through bridge");
    let guide_id = added.project.document.pages[0].guides[0]
        .id
        .as_str()
        .to_owned();
    let moved = editor
        .apply(transaction(
            1,
            "Move guide",
            vec![EditorCommandInput::MoveGuide {
                page_id,
                guide_id,
                position_um: 50_800,
            }],
        ))
        .expect("move guide through bridge");
    assert_eq!(
        moved.project.document.pages[0].guides[0].position.value(),
        50_800
    );
    let undone = editor.undo(2).expect("undo guide movement");
    assert_eq!(
        undone.project.document.pages[0].guides[0].position.value(),
        25_400
    );
}

#[test]
fn text_style_is_canonical_and_undoable() {
    let temp = TempDirectory::new("text-style");
    let package = temp.path().join("styled.publisher-v2");
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add title",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Title".to_owned(),
                text: "Magic".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 80_000,
                height_um: 20_000,
            }],
        ))
        .expect("add text");
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    assert!(matches!(
        &added.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { style, .. } if style == &document_core::model::TextStyle::default()
    ));
    let styled = editor
        .apply(transaction(
            1,
            "Style title",
            vec![EditorCommandInput::UpdateTextStyle {
                object_id,
                style: TextStyleInput {
                    font_face: None,
                    font_family: "Georgia".to_owned(),
                    font_size_millipoints: 36_000,
                    font_weight: 700,
                    italic: true,
                    alignment: TextAlignmentInput::Center,
                    color: ColorInput {
                        red: 80,
                        green: 20,
                        blue: 120,
                        alpha: 255,
                    },
                    line_height_per_thousand: 1_100,
                    underline: false,
                },
            }],
        ))
        .expect("style text");
    assert!(matches!(
        &styled.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { style, .. }
            if style.font_family == "Georgia"
                && style.font_size_millipoints == 36_000
                && style.font_weight == 700
                && style.italic
    ));
    editor.save_new(&package).expect("save styled text");
    let mut reopened = EditorModule::new();
    let reopened = reopened.open(&package).expect("reopen styled text");
    assert!(matches!(
        &reopened.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { style, .. } if style.font_family == "Georgia"
    ));
    let undone = editor.undo(2).expect("undo text style");
    assert!(matches!(
        &undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { style, .. } if style == &document_core::model::TextStyle::default()
    ));
}

#[test]
fn rich_text_ranges_cross_the_application_boundary_atomically_and_reopen() {
    let temp = TempDirectory::new("rich-text-ranges");
    let package = temp.path().join("rich-text.publisher-v2");
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add instructions",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Instructions".to_owned(),
                text: "One 🪙 trick\nSecond paragraph".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 160_000,
                height_um: 100_000,
            }],
        ))
        .expect("add text");
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();

    let formatted = editor
        .apply(transaction(
            1,
            "Format selected text",
            vec![
                EditorCommandInput::UpdateInlineTextStyle {
                    object_id: object_id.clone(),
                    start_byte: 4,
                    end_byte: 8,
                    bold: Some(true),
                    italic: Some(false),
                },
                EditorCommandInput::UpdateParagraphStyle {
                    object_id,
                    start_paragraph: 1,
                    end_paragraph: 2,
                    style: ParagraphStyleInput {
                        list_style: Some(ParagraphListStyleInput::Numbered { start_at: 1 }),
                        alignment: Some(TextAlignmentInput::Center),
                        start_indent_um: Some(6_350),
                        first_line_indent_um: Some(-3_175),
                        line_height_per_thousand: Some(1_400),
                        space_before_millipoints: Some(6_000),
                        space_after_millipoints: Some(3_000),
                        ..ParagraphStyleInput::default()
                    },
                },
            ],
        ))
        .expect("apply rich formatting");

    let ObjectKind::Text { story, .. } = &formatted.project.document.pages[0].objects[0].kind
    else {
        panic!("expected text object");
    };
    assert_eq!(story.inline_spans.len(), 1);
    assert_eq!(story.inline_spans[0].range.start, 4);
    assert_eq!(story.inline_spans[0].range.end, 8);
    assert_eq!(story.inline_spans[0].style.bold, Some(true));
    assert_eq!(story.paragraph_runs.len(), 1);
    assert_eq!(story.paragraph_runs[0].range.start, 1);
    assert_eq!(story.paragraph_runs[0].range.end, 2);
    assert_eq!(
        story.paragraph_runs[0].style.list_style,
        Some(document_core::model::ParagraphListStyle::Numbered { start_at: 1 })
    );
    assert_eq!(
        story.paragraph_runs[0].style.alignment,
        Some(document_core::model::TextAlignment::Center)
    );
    let expected_story = story.clone();

    editor.save_new(&package).expect("save rich text");
    let mut reopened = EditorModule::new();
    let reopened = reopened.open(&package).expect("reopen rich text");
    assert!(matches!(
        &reopened.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { story, .. } if story == &expected_story
    ));

    let undone = editor.undo(2).expect("undo rich formatting transaction");
    assert!(matches!(
        &undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { story, .. } if story.is_default()
    ));
}

#[test]
#[allow(clippy::too_many_lines)] // One linear bridge, atomicity, persistence, and undo acceptance story.
fn a_long_disjoint_text_session_is_one_bridge_command_and_one_undo_step() {
    let temp = TempDirectory::new("long-rich-text-session");
    let package = temp.path().join("long-rich-text.publisher-v2");
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add editable story",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Story".to_owned(),
                text: "AxMIDyZ".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 160_000,
                height_um: 100_000,
            }],
        ))
        .expect("add text");
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    editor
        .apply(transaction(
            1,
            "Bold middle",
            vec![EditorCommandInput::UpdateInlineTextStyle {
                object_id: object_id.clone(),
                start_byte: 2,
                end_byte: 5,
                bold: Some(true),
                italic: None,
            }],
        ))
        .expect("format middle");
    let mut edits = vec![
        TextEditInput {
            start_byte: 0,
            end_byte: 1,
            text: "B".to_owned(),
            bold: None,
            italic: None,
            style: None,
        },
        TextEditInput {
            start_byte: 6,
            end_byte: 7,
            text: "Q".to_owned(),
            bold: None,
            italic: None,
            style: None,
        },
    ];
    edits.extend((0..150).map(|index| TextEditInput {
        start_byte: 7 + index,
        end_byte: 7 + index,
        text: "x".to_owned(),
        bold: None,
        italic: None,
        style: None,
    }));
    edits.push(TextEditInput {
        start_byte: 157,
        end_byte: 157,
        text: "🪙".to_owned(),
        bold: None,
        italic: None,
        style: None,
    });
    let edited = editor
        .apply(transaction(
            2,
            "Long direct edit",
            vec![EditorCommandInput::ApplyTextEdits {
                object_id: object_id.clone(),
                edits,
            }],
        ))
        .expect("apply more than one hundred ordered edits through one bridge command");
    let ObjectKind::Text { text, story, .. } = &edited.project.document.pages[0].objects[0].kind
    else {
        panic!("expected text")
    };
    assert_eq!(text, &format!("BxMIDyQ{}🪙", "x".repeat(150)));
    assert_eq!(
        story.inline_spans[0].range,
        document_core::model::TextSourceRange { start: 2, end: 5 }
    );
    editor
        .apply(transaction(
            3,
            "Invalid text edit",
            vec![EditorCommandInput::ApplyTextEdits {
                object_id: object_id.clone(),
                edits: vec![TextEditInput {
                    start_byte: 5,
                    end_byte: 4,
                    text: "bad".to_owned(),
                    bold: None,
                    italic: None,
                    style: None,
                }],
            }],
        ))
        .expect_err("invalid ordered range must fail atomically");
    assert_eq!(
        editor
            .snapshot()
            .expect("snapshot after rejected edit")
            .project
            .revision
            .value(),
        3
    );
    editor.save_new(&package).expect("save ordered text edits");
    let mut reopened = EditorModule::new();
    let reopened = reopened.open(&package).expect("reopen ordered text edits");
    assert!(matches!(
        &reopened.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { text, story, .. } if text == &format!("BxMIDyQ{}🪙", "x".repeat(150)) && story.inline_spans[0].range.start == 2 && story.inline_spans[0].range.end == 5
    ));
    let undone = editor.undo(3).expect("undo one edit session");
    assert!(matches!(
        &undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { text, story, .. } if text == "AxMIDyZ" && story.inline_spans[0].range == document_core::model::TextSourceRange { start: 2, end: 5 }
    ));
}

#[test]
fn text_box_edit_preview_applies_ordered_unicode_without_mutating_live_state() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add preview story",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Preview story".to_owned(),
                text: "A🪙MIDZ".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 160_000,
                height_um: 100_000,
            }],
        ))
        .expect("add preview story");
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    editor
        .apply(transaction(
            1,
            "Format preview middle",
            vec![EditorCommandInput::UpdateInlineTextStyle {
                object_id: object_id.clone(),
                start_byte: 5,
                end_byte: 8,
                bold: Some(true),
                italic: None,
            }],
        ))
        .expect("format preview story");
    let before = editor.snapshot().expect("snapshot before preview");
    let lifecycle_before = editor.document_lifecycle();

    let preview = editor
        .preview_text_box_edits(
            before.project.revision.value(),
            &object_id,
            vec![
                TextEditInput {
                    start_byte: 0,
                    end_byte: 1,
                    text: "Bé".to_owned(),
                    bold: None,
                    italic: None,
                    style: None,
                },
                TextEditInput {
                    start_byte: 10,
                    end_byte: 11,
                    text: "✨".to_owned(),
                    bold: None,
                    italic: None,
                    style: None,
                },
            ],
        )
        .expect("preview ordered Unicode edits");

    assert_eq!(preview.revision, before.project.revision);
    let ObjectKind::Text { text, story, .. } = &preview.document.pages[0].objects[0].kind else {
        panic!("expected Text Box preview");
    };
    assert_eq!(text, "Bé🪙MID✨");
    // Replacing unstyled Z preserves its first-character style; adjacent MID stays Bold.
    assert_eq!(
        story.inline_spans[0].range,
        document_core::model::TextSourceRange { start: 7, end: 10 }
    );
    assert_eq!(editor.snapshot().expect("snapshot after preview"), before);
    assert_eq!(editor.document_lifecycle(), lifecycle_before);

    let undone = editor
        .undo(before.project.revision.value())
        .expect("preview did not add an undo entry");
    assert_eq!(undone.project.document, added.project.document);
}

#[test]
fn text_box_edit_preview_drives_exact_unicode_crlf_overset() {
    let font_bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let font_source = AssetHash::for_bytes(&font_bytes);
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add draft layout target",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Draft layout target".to_owned(),
                text: "Seed".to_owned(),
                style: Some(TextStyleInput {
                    font_face: Some(FontFaceBindingInput {
                        source: font_source.as_str().to_owned(),
                        face_index: 0,
                    }),
                    font_family: "DejaVu Sans".to_owned(),
                    font_size_millipoints: 12_000,
                    font_weight: 400,
                    italic: false,
                    alignment: TextAlignmentInput::Left,
                    color: black(),
                    line_height_per_thousand: 1_200,
                    underline: false,
                }),
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 80_000,
                height_um: 24_000,
            }],
        ))
        .expect("add draft layout target");
    let object_id = added.project.document.pages[0].objects[0].id.clone();
    let before = editor.snapshot().expect("snapshot before draft layout");
    let draft_text = "Magic café\r\npresto! ".repeat(40);

    let draft = editor
        .preview_text_box_edits(
            before.project.revision.value(),
            object_id.as_str(),
            vec![TextEditInput {
                start_byte: 0,
                end_byte: "Seed".len(),
                text: draft_text.clone(),
                bold: None,
                italic: None,
                style: None,
            }],
        )
        .expect("preview Unicode CRLF story");
    let resolved = ResolvedPublication::resolve_with_font_resources(
        &draft,
        &[ExactFontResource {
            source: font_source,
            face_index: 0,
            bytes: font_bytes,
        }],
    )
    .expect("resolve draft layout");
    let text_box = resolved
        .text_boxes()
        .into_iter()
        .find(|text_box| text_box.object_id == object_id)
        .expect("resolved draft Text Box");
    let overset = text_box
        .overset_source
        .as_ref()
        .expect("small fixed Text Box must overset");

    assert_eq!(overset.end, draft_text.len());
    assert!(draft_text.is_char_boundary(overset.start));
    let last_line_end = text_box.lines.last().map_or(0, |line| line.source.end);
    assert_eq!(&draft_text[last_line_end - 1..overset.start], "\r\n");
    assert_eq!(last_line_end + 1, overset.start);
    assert_eq!(
        editor.snapshot().expect("snapshot after draft layout"),
        before
    );
}

#[test]
fn text_style_preview_preserves_live_state_and_rejects_invalid_changes() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create");
    let added = editor
        .apply(transaction(
            0,
            "Add preview target",
            vec![EditorCommandInput::AddText {
                page_id: Some(created.project.document.pages[0].id.as_str().to_owned()),
                name: "Style preview".to_owned(),
                text: "Keep this text".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 10_000,
                y_um: 10_000,
                width_um: 80_000,
                height_um: 30_000,
            }],
        ))
        .expect("add text");
    let id = added.project.document.pages[0].objects[0].id.as_str();
    let style = TextStyleInput {
        font_face: None,
        font_family: "Arial".to_owned(),
        font_size_millipoints: 24_000,
        font_weight: 400,
        italic: false,
        alignment: TextAlignmentInput::Left,
        color: black(),
        line_height_per_thousand: 1_500,
        underline: false,
    };
    let before = editor.snapshot().expect("before");
    let lifecycle = editor.document_lifecycle();
    let preview = editor
        .preview_text_box_changes(1, id, vec![], Some(style.clone()))
        .expect("preview style");
    assert_eq!(preview.revision, before.project.revision);
    let ObjectKind::Text {
        text,
        style: result,
        ..
    } = &preview.document.pages[0].objects[0].kind
    else {
        panic!("text")
    };
    assert_eq!(text, "Keep this text");
    assert_eq!(result.font_size_millipoints, 24_000);
    assert_eq!(result.line_height_per_thousand, 1_500);
    let combined = editor
        .preview_text_box_changes(
            1,
            id,
            vec![TextEditInput {
                start_byte: 0,
                end_byte: 4,
                text: "Replace".to_owned(),
                bold: None,
                italic: None,
                style: None,
            }],
            Some(style.clone()),
        )
        .expect("combined preview");
    assert!(
        matches!(&combined.document.pages[0].objects[0].kind, ObjectKind::Text { text, .. } if text == "Replace this text")
    );
    assert!(
        editor
            .preview_text_box_changes(0, id, vec![], Some(style.clone()))
            .is_err()
    );
    let mut invalid = style;
    invalid.font_size_millipoints = 0;
    assert!(
        editor
            .preview_text_box_changes(1, id, vec![], Some(invalid))
            .is_err()
    );
    assert_eq!(editor.snapshot().expect("after"), before);
    assert_eq!(editor.document_lifecycle(), lifecycle);
    assert_eq!(
        editor
            .undo(1)
            .expect("only add is undoable")
            .project
            .document,
        created.project.document
    );
}

struct TextEditPreviewRejectionFixture {
    editor: EditorModule,
    text_object_id: String,
    non_text_object_id: String,
}

fn text_edit_preview_rejection_fixture() -> TextEditPreviewRejectionFixture {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add preview target",
            vec![
                EditorCommandInput::AddText {
                    page_id: Some(page_id.clone()),
                    name: "Preview target".to_owned(),
                    text: "A🪙Z".to_owned(),
                    style: None,
                    sizing_mode: TextBoxSizingModeInput::Fixed,
                    x_um: 20_000,
                    y_um: 30_000,
                    width_um: 160_000,
                    height_um: 100_000,
                },
                EditorCommandInput::AddShape {
                    page_id: Some(page_id),
                    name: "Non-text preview target".to_owned(),
                    shape: ShapeInput::Rectangle,
                    x_um: 20_000,
                    y_um: 140_000,
                    width_um: 20_000,
                    height_um: 20_000,
                    fill: black(),
                    stroke: None,
                    corner_rounding: None,
                },
            ],
        ))
        .expect("add preview target");
    let text_object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let non_text_object_id = added.project.document.pages[0].objects[1]
        .id
        .as_str()
        .to_owned();

    TextEditPreviewRejectionFixture {
        editor,
        text_object_id,
        non_text_object_id,
    }
}

#[test]
fn text_box_edit_preview_rejects_stale_and_invalid_edits_atomically() {
    let TextEditPreviewRejectionFixture {
        editor,
        text_object_id,
        ..
    } = text_edit_preview_rejection_fixture();
    let before_rejections = editor.snapshot().expect("snapshot before rejections");
    let lifecycle_before = editor.document_lifecycle();

    let stale = editor
        .preview_text_box_edits(
            0,
            &text_object_id,
            vec![TextEditInput {
                start_byte: 0,
                end_byte: 1,
                text: "B".to_owned(),
                bold: None,
                italic: None,
                style: None,
            }],
        )
        .expect_err("stale draft preview must fail");
    assert_eq!(stale.code, EditorErrorCode::StaleRevision);
    assert_eq!(
        editor.snapshot().expect("snapshot after stale preview"),
        before_rejections
    );

    editor
        .preview_text_box_edits(
            1,
            &text_object_id,
            vec![TextEditInput {
                start_byte: 2,
                end_byte: 3,
                text: "bad boundary".to_owned(),
                bold: None,
                italic: None,
                style: None,
            }],
        )
        .expect_err("mid-code-point draft preview must fail");
    assert_eq!(
        editor.snapshot().expect("snapshot after invalid preview"),
        before_rejections
    );
    assert_eq!(editor.document_lifecycle(), lifecycle_before);
}

#[test]
fn text_box_edit_preview_rejects_wrong_targets_atomically() {
    let TextEditPreviewRejectionFixture {
        editor,
        non_text_object_id,
        ..
    } = text_edit_preview_rejection_fixture();
    let before_rejections = editor.snapshot().expect("snapshot before rejections");
    let lifecycle_before = editor.document_lifecycle();

    for (label, rejected_object_id) in [
        ("non-text", non_text_object_id.as_str()),
        ("unknown", "object-missing"),
    ] {
        let rejected = editor
            .preview_text_box_edits(
                before_rejections.project.revision.value(),
                rejected_object_id,
                vec![TextEditInput {
                    start_byte: 0,
                    end_byte: 0,
                    text: "B".to_owned(),
                    bold: None,
                    italic: None,
                    style: None,
                }],
            )
            .expect_err(&format!("{label} draft preview target must fail"));
        assert_eq!(rejected.code, EditorErrorCode::CommandRejected);
        assert_eq!(
            editor
                .snapshot()
                .expect("snapshot after rejected object preview"),
            before_rejections
        );
        assert_eq!(editor.document_lifecycle(), lifecycle_before);
    }
}

#[test]
fn text_box_edit_preview_rejects_locked_target_atomically() {
    let TextEditPreviewRejectionFixture {
        mut editor,
        text_object_id,
        ..
    } = text_edit_preview_rejection_fixture();
    let locked = editor
        .apply(transaction(
            1,
            "Lock preview target",
            vec![EditorCommandInput::SetObjectLock {
                object_ids: vec![text_object_id.clone()],
                locked: true,
            }],
        ))
        .expect("lock preview target");
    let locked_lifecycle = editor.document_lifecycle();
    editor
        .preview_text_box_edits(
            locked.project.revision.value(),
            &text_object_id,
            vec![TextEditInput {
                start_byte: 0,
                end_byte: 1,
                text: "B".to_owned(),
                bold: None,
                italic: None,
                style: None,
            }],
        )
        .expect_err("locked Text Box draft preview must fail");
    assert_eq!(
        editor.snapshot().expect("snapshot after locked preview"),
        locked
    );
    assert_eq!(editor.document_lifecycle(), locked_lifecycle);
}

#[test]
fn new_text_retains_a_deliberately_requested_exact_face() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let source = AssetHash::for_bytes(b"exact embedded font resource");
    let added = editor
        .apply(transaction(
            0,
            "Add exactly bound title",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Title".to_owned(),
                text: "Magic".to_owned(),
                style: Some(TextStyleInput {
                    font_face: Some(FontFaceBindingInput {
                        source: source.as_str().to_owned(),
                        face_index: 3,
                    }),
                    font_family: "Bound family label".to_owned(),
                    font_size_millipoints: 36_000,
                    font_weight: 700,
                    italic: false,
                    alignment: TextAlignmentInput::Center,
                    color: ColorInput {
                        red: 80,
                        green: 20,
                        blue: 120,
                        alpha: 255,
                    },
                    line_height_per_thousand: 1_100,
                    underline: false,
                }),
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 80_000,
                height_um: 20_000,
            }],
        ))
        .expect("add exactly bound text");
    assert!(matches!(
        &added.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { style, .. }
            if style.font_face.as_ref().is_some_and(|binding|
                binding.source == source && binding.face_index == 3)
    ));
}

#[test]
fn text_columns_are_canonical_and_undoable() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add body text",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Instructions".to_owned(),
                text: "A long body of instructions".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 160_000,
                height_um: 220_000,
            }],
        ))
        .expect("add body text");
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let columns = editor
        .apply(transaction(
            1,
            "Use two columns",
            vec![EditorCommandInput::UpdateTextFrame {
                object_id,
                column_count: 2,
                column_gap_um: 8_000,
            }],
        ))
        .expect("set text columns");
    assert!(matches!(
        &columns.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { frame, .. }
            if frame.column_count == 2 && frame.column_gap_um == 8_000
    ));
    let undone = editor.undo(2).expect("undo columns");
    assert!(matches!(
        &undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Text { frame, .. } if frame == &document_core::model::TextFrame::default()
    ));
}

#[test]
fn object_duplication_preserves_editable_content_with_new_identity() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add reusable text",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Reusable".to_owned(),
                text: "Copy me".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 80_000,
                height_um: 20_000,
            }],
        ))
        .expect("add text");
    let source = added.project.document.pages[0].objects[0].clone();
    let duplicated = editor
        .apply(transaction(
            1,
            "Duplicate object",
            vec![EditorCommandInput::DuplicateObject {
                object_id: source.id.as_str().to_owned(),
                offset_x_um: 5_000,
                offset_y_um: 7_000,
            }],
        ))
        .expect("duplicate object");
    let copy = &duplicated.project.document.pages[0].objects[1];
    assert_ne!(copy.id, source.id);
    assert_eq!(copy.kind, source.kind);
    assert_eq!(copy.transform.translation_x.value(), 25_000);
    assert_eq!(copy.transform.translation_y.value(), 37_000);
    let undone = editor.undo(2).expect("undo duplicate");
    assert_eq!(undone.project.document.pages[0].objects.len(), 1);
}

#[test]
fn paste_selection_crosses_the_revision_checked_application_boundary() {
    let mut editor = EditorModule::new();
    create_grouped_dice(&mut editor);
    let source_group = editor
        .snapshot()
        .expect("grouped snapshot")
        .project
        .document
        .pages[0]
        .objects[0]
        .group_id
        .clone()
        .expect("source group");
    let page_id = editor
        .snapshot()
        .expect("grouped snapshot")
        .project
        .document
        .pages[0]
        .id
        .as_str()
        .to_owned();
    let copied_objects = editor
        .snapshot()
        .expect("grouped snapshot")
        .project
        .document
        .pages[0]
        .objects
        .clone();
    let pasted = editor
        .apply(transaction(
            2,
            "Paste",
            vec![EditorCommandInput::PasteObjects {
                objects: copied_objects,
                destination_page_id: Some(page_id),
                offset_x_um: 12_000,
                offset_y_um: 12_000,
            }],
        ))
        .expect("paste through bridge");
    assert_eq!(pasted.project.document.pages[0].objects.len(), 4);
    let pasted_group = pasted.project.document.pages[0].objects[2]
        .group_id
        .as_ref()
        .expect("pasted group");
    assert_ne!(pasted_group, &source_group);
    assert_eq!(
        pasted.project.document.pages[0].objects[3]
            .group_id
            .as_ref(),
        Some(pasted_group)
    );
    assert_eq!(
        editor.undo(3).expect("undo paste").project.document.pages[0]
            .objects
            .len(),
        2
    );
}

fn create_grouped_dice(editor: &mut EditorModule) -> Vec<String> {
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let shape = |name: &str, x_um| EditorCommandInput::AddShape {
        page_id: Some(page_id.clone()),
        name: name.to_owned(),
        shape: ShapeInput::Ellipse,
        x_um,
        y_um: 30_000,
        width_um: 10_000,
        height_um: 10_000,
        fill: ColorInput {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        },
        stroke: None,
        corner_rounding: None,
    };
    let added = editor
        .apply(transaction(
            0,
            "Add dice parts",
            vec![shape("Die", 20_000), shape("Pip", 25_000)],
        ))
        .expect("add group candidates");
    let object_ids = added.project.document.pages[0]
        .objects
        .iter()
        .map(|object| object.id.as_str().to_owned())
        .collect::<Vec<_>>();
    let grouped = editor
        .apply(transaction(
            1,
            "Group dice",
            vec![EditorCommandInput::GroupObjects {
                object_ids: object_ids.clone(),
            }],
        ))
        .expect("group through bridge");
    let group_id = grouped.project.document.pages[0].objects[0]
        .group_id
        .clone()
        .expect("group id");
    assert_eq!(
        grouped.project.document.pages[0].objects[1]
            .group_id
            .as_ref(),
        Some(&group_id)
    );
    object_ids
}

#[test]
fn group_and_ungroup_cross_the_revision_checked_application_boundary() {
    let mut editor = EditorModule::new();
    let object_ids = create_grouped_dice(&mut editor);
    let ungrouped = editor
        .apply(transaction(
            2,
            "Ungroup dice",
            vec![EditorCommandInput::UngroupObjects {
                object_ids: vec![object_ids[0].clone()],
            }],
        ))
        .expect("ungroup through bridge");
    assert!(
        ungrouped.project.document.pages[0]
            .objects
            .iter()
            .all(|object| object.group_id.is_none())
    );
}

#[test]
fn lock_and_arrange_cross_the_revision_checked_application_boundary() {
    let mut editor = EditorModule::new();
    let object_ids = create_grouped_dice(&mut editor);
    let locked = editor
        .apply(transaction(
            2,
            "Lock dice",
            vec![EditorCommandInput::SetObjectLock {
                object_ids: object_ids.clone(),
                locked: true,
            }],
        ))
        .expect("lock through bridge");
    assert!(
        locked.project.document.pages[0]
            .objects
            .iter()
            .all(|object| object.locked)
    );
    editor
        .apply(transaction(
            3,
            "Blocked arrange",
            vec![EditorCommandInput::ArrangeObject {
                object_id: object_ids[0].clone(),
                operation: application_core::ArrangeOperationInput::BringToFront,
            }],
        ))
        .expect_err("locked group rejects arrange through bridge");
    assert_eq!(
        editor
            .snapshot()
            .expect("snapshot after rejection")
            .project
            .revision
            .value(),
        3
    );
    editor
        .apply(transaction(
            3,
            "Unlock dice",
            vec![EditorCommandInput::SetObjectLock {
                object_ids: object_ids.clone(),
                locked: false,
            }],
        ))
        .expect("unlock through bridge");
    editor
        .apply(transaction(
            4,
            "Bring dice to front",
            vec![EditorCommandInput::ArrangeObject {
                object_id: object_ids[0].clone(),
                operation: application_core::ArrangeOperationInput::BringToFront,
            }],
        ))
        .expect("arrange through bridge");
}

#[test]
fn align_and_distribute_cross_the_revision_checked_application_boundary() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let mut shapes = Vec::new();
    for (index, x_um) in [10_000, 50_000, 100_000].into_iter().enumerate() {
        shapes.push(EditorCommandInput::AddShape {
            page_id: Some(page_id.clone()),
            name: format!("Shape {index}"),
            shape: ShapeInput::Rectangle,
            x_um,
            y_um: [10_000, 20_000, 50_000][index],
            width_um: 10_000,
            height_um: 10_000,
            fill: black(),
            stroke: None,
            corner_rounding: None,
        });
    }
    let added = editor
        .apply(transaction(0, "Add alignment set", shapes))
        .expect("add alignment set");
    let ids = added.project.document.pages[0]
        .objects
        .iter()
        .map(|object| object.id.as_str().to_owned())
        .collect::<Vec<_>>();
    let aligned = editor
        .apply(transaction(
            1,
            "Align left",
            vec![EditorCommandInput::ArrangeGeometry {
                object_ids: ids.clone(),
                operation: ArrangeGeometryOperationInput::AlignLeft,
            }],
        ))
        .expect("align through bridge");
    assert!(
        aligned.project.document.pages[0]
            .objects
            .iter()
            .all(|object| object.transform.translation_x.value() == 10_000)
    );
    let distributed = editor
        .apply(transaction(
            2,
            "Distribute vertically",
            vec![EditorCommandInput::ArrangeGeometry {
                object_ids: ids,
                operation: ArrangeGeometryOperationInput::DistributeVertically,
            }],
        ))
        .expect("distribute through bridge");
    assert_eq!(distributed.project.revision.value(), 3);
    assert_eq!(
        distributed.project.document.pages[0]
            .objects
            .iter()
            .map(|object| object.transform.translation_y.value())
            .collect::<Vec<_>>(),
        [10_000, 30_000, 50_000]
    );
    let undone = editor.undo(3).expect("undo one arrangement intent");
    assert_eq!(undone.project.revision.value(), 4);
    assert!(
        undone.project.document.pages[0]
            .objects
            .iter()
            .all(|object| object.transform.translation_x.value() == 10_000)
    );

    let mixed_error = editor
        .apply(transaction(
            4,
            "Duplicate selection",
            vec![EditorCommandInput::ArrangeGeometry {
                object_ids: vec![
                    undone.project.document.pages[0].objects[0]
                        .id
                        .as_str()
                        .to_owned(),
                    undone.project.document.pages[0].objects[0]
                        .id
                        .as_str()
                        .to_owned(),
                ],
                operation: ArrangeGeometryOperationInput::AlignRight,
            }],
        ))
        .expect_err("bridge rejects duplicate selection");
    assert_eq!(mixed_error.code, EditorErrorCode::CommandRejected);
    assert_eq!(
        editor
            .snapshot()
            .expect("snapshot after rejection")
            .project
            .revision
            .value(),
        4
    );
}

#[test]
fn a_large_group_transform_is_one_bounded_undoable_input() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add large group",
            (0..51)
                .map(|index| EditorCommandInput::AddShape {
                    page_id: Some(page_id.clone()),
                    name: format!("Part {index}"),
                    shape: ShapeInput::Ellipse,
                    x_um: 20_000 + i64::from(index) * 1_000,
                    y_um: 30_000,
                    width_um: 10_000,
                    height_um: 10_000,
                    fill: ColorInput {
                        red: 0,
                        green: 0,
                        blue: 0,
                        alpha: 255,
                    },
                    stroke: None,
                    corner_rounding: None,
                })
                .collect(),
        ))
        .expect("add group candidates");
    let object_ids = added.project.document.pages[0]
        .objects
        .iter()
        .map(|object| object.id.as_str().to_owned())
        .collect::<Vec<_>>();
    let grouped = editor
        .apply(transaction(
            1,
            "Group objects",
            vec![EditorCommandInput::GroupObjects {
                object_ids: object_ids.clone(),
            }],
        ))
        .expect("group objects");
    let transformed = editor
        .apply(transaction(
            2,
            "Resize group",
            vec![EditorCommandInput::TransformObjects {
                objects: grouped.project.document.pages[0]
                    .objects
                    .iter()
                    .map(|object| TransformObjectInput {
                        object_id: object.id.as_str().to_owned(),
                        page_id: Some(page_id.clone()),
                        x_um: object.transform.translation_x.value() - 500,
                        y_um: object.transform.translation_y.value() - 500,
                        width_um: object.size.width.value() + 1_000,
                        height_um: object.size.height.value() + 1_000,
                    })
                    .collect(),
            }],
        ))
        .expect("transform 51 objects through one input");
    assert_eq!(transformed.project.document.pages[0].objects.len(), 51);
    assert!(
        transformed.project.document.pages[0]
            .objects
            .iter()
            .all(|object| object.size.width.value() == 11_000)
    );
    let undone = editor.undo(3).expect("undo whole grouped transform");
    assert!(
        undone.project.document.pages[0]
            .objects
            .iter()
            .all(|object| object.size.width.value() == 10_000)
    );
    assert_eq!(
        undone.project.document.pages[0]
            .objects
            .iter()
            .map(|object| object.id.as_str())
            .collect::<Vec<_>>(),
        object_ids.iter().map(String::as_str).collect::<Vec<_>>()
    );
}

#[test]
fn shape_primitive_fill_and_stroke_are_canonical_and_undoable() {
    for (input_shape, expected_shape) in [
        (ShapeInput::Cube, document_core::model::ShapeKind::Cube),
        (ShapeInput::Line, document_core::model::ShapeKind::Line),
    ] {
        let mut editor = EditorModule::new();
        let created = editor.create(create_input()).expect("create document");
        let page_id = created.project.document.pages[0].id.as_str().to_owned();
        let added = editor
            .apply(transaction(
                0,
                "Add shape",
                vec![EditorCommandInput::AddShape {
                    page_id: Some(page_id),
                    name: "Die pip".to_owned(),
                    shape: ShapeInput::Ellipse,
                    x_um: 20_000,
                    y_um: 30_000,
                    width_um: 10_000,
                    height_um: 10_000,
                    fill: ColorInput {
                        red: 0,
                        green: 0,
                        blue: 0,
                        alpha: 255,
                    },
                    stroke: None,
                    corner_rounding: None,
                }],
            ))
            .expect("add shape");
        let object_id = added.project.document.pages[0].objects[0]
            .id
            .as_str()
            .to_owned();
        let edited = editor
            .apply(transaction(
                1,
                "Edit shape",
                vec![
                    EditorCommandInput::UpdateShape {
                        object_id: object_id.clone(),
                        shape: input_shape,
                        fill: ColorInput {
                            red: 20,
                            green: 40,
                            blue: 60,
                            alpha: 255,
                        },
                        stroke: ShapeStrokeInput {
                            color: ColorInput {
                                red: 220,
                                green: 210,
                                blue: 200,
                                alpha: 255,
                            },
                            width_millipoints: 2_000,
                        },
                    },
                    EditorCommandInput::SetRotation {
                        object_id: object_id.clone(),
                        rotation_microdegrees: -32_500_000,
                    },
                    EditorCommandInput::UpdateWrapMode {
                        object_id,
                        wrap_mode: WrapModeInput::Rectangular,
                    },
                ],
            ))
            .expect("edit shape");
        assert!(matches!(
            &edited.project.document.pages[0].objects[0].kind,
            ObjectKind::Shape { shape, fill, stroke, wrap_mode: WrapMode::Rectangular }
                if *shape == expected_shape && fill.red == 20 && fill.green == 40 && fill.blue == 60
                    && stroke.color.red == 220 && stroke.width_millipoints == 2_000
        ));
        assert_eq!(
            edited.project.document.pages[0].objects[0]
                .transform
                .rotation_microdegrees,
            -32_500_000
        );
        let undone = editor.undo(2).expect("undo shape edit");
        assert!(matches!(
            &undone.project.document.pages[0].objects[0].kind,
            ObjectKind::Shape {
                shape: document_core::model::ShapeKind::Ellipse,
                wrap_mode: WrapMode::None,
                ..
            }
        ));
        assert_eq!(
            undone.project.document.pages[0].objects[0]
                .transform
                .rotation_microdegrees,
            0
        );
    }
}

#[test]
fn verified_image_import_is_hash_bound_undoable_and_package_backed() {
    let temp = TempDirectory::new("image-import");
    let package = temp.path().join("images.publisher-v2");
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let bytes = png_fixture();
    let inspection = inspect_raster(&bytes).expect("inspect PNG");
    let hash = AssetHash::for_bytes(&bytes);
    let imported = editor
        .import_verified_image(VerifiedImageImport {
            expected_revision: 0,
            page_id: Some(page_id),
            name: "Socks.png".to_owned(),
            x_um: 20_000,
            y_um: 30_000,
            width_um: 50_000,
            height_um: 25_000,
            media_type: inspection.media_type.clone(),
            asset_hash: hash.clone(),
            inspection: inspection.clone(),
            bytes,
        })
        .expect("import image");
    assert!(matches!(
        &imported.project.document.pages[0].objects[0].kind,
        ObjectKind::Image { asset, .. } if asset == &hash
    ));
    let served = editor
        .referenced_raster(hash.as_str())
        .expect("read referenced raster");
    assert_eq!(
        editor.referenced_raster_media_type(hash.as_str()).unwrap(),
        "image/png"
    );
    assert_eq!(served.media_type, "image/png");
    assert_eq!(AssetHash::for_bytes(&served.bytes), hash);
    editor.save_new(&package).expect("save image package");
    let reopened = document_core::open_package(&package).expect("reopen image package");
    assert_eq!(
        reopened
            .assets
            .get(&hash)
            .and_then(|asset| asset.raster.as_ref()),
        Some(&inspection)
    );
    let undone = editor.undo(1).expect("undo image import");
    assert!(undone.project.document.pages[0].objects.is_empty());
    assert!(editor.referenced_raster(hash.as_str()).is_err());
    assert!(editor.referenced_raster_media_type(hash.as_str()).is_err());
    editor.save_current().unwrap();
    assert!(
        document_core::open_package(&package)
            .unwrap()
            .assets
            .is_empty()
    );
    let empty_copy = temp.path().join("empty-save-as.publisher-v2");
    editor.save_new(&empty_copy).unwrap();
    assert!(
        document_core::open_package(&empty_copy)
            .unwrap()
            .assets
            .is_empty()
    );
    let recovery = editor.autosave(&temp.path().join("recovery")).unwrap();
    assert!(
        document_core::open_package(&recovery.package_path)
            .unwrap()
            .assets
            .is_empty()
    );
    editor.redo(undone.project.revision.value()).unwrap();
    editor.save_current().unwrap();
    assert!(
        document_core::open_package(&empty_copy)
            .unwrap()
            .assets
            .contains(&hash)
    );
    let recovery = editor.autosave(&temp.path().join("recovery")).unwrap();
    assert!(
        document_core::open_package(&recovery.package_path)
            .unwrap()
            .assets
            .contains(&hash)
    );
}

#[test]
fn verified_picture_replacement_preserves_placement_and_save_undo_resources() {
    let temp = TempDirectory::new("picture-replacement");
    let package = temp.path().join("replacement.publisher-v2");
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).unwrap();
    let bytes = png_fixture();
    let original_hash = AssetHash::for_bytes(&bytes);
    let inspection = inspect_raster(&bytes).unwrap();
    let original = editor
        .import_verified_image(VerifiedImageImport {
            expected_revision: 0,
            page_id: Some(created.project.document.pages[0].id.as_str().to_owned()),
            name: "Original.png".to_owned(),
            x_um: 20_000,
            y_um: 30_000,
            width_um: 50_000,
            height_um: 25_000,
            media_type: inspection.media_type.clone(),
            asset_hash: original_hash.clone(),
            inspection,
            bytes,
        })
        .unwrap();
    let object = original.project.document.pages[0].objects[0].clone();
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(40, 80)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    let bytes = encoded.into_inner();
    let hash = AssetHash::for_bytes(&bytes);
    let replacement = VerifiedImageReplacement {
        expected_revision: 1,
        object_id: object.id.as_str().to_owned(),
        name: "Replacement.png".to_owned(),
        asset_hash: hash.clone(),
        inspection: inspect_raster(&bytes).unwrap(),
        bytes,
    };
    let mut invalid = replacement.clone();
    invalid.bytes.push(0);
    assert!(editor.replace_verified_image(invalid).is_err());
    assert_eq!(editor.snapshot().unwrap(), original);
    let mut stale = replacement.clone();
    stale.expected_revision = 0;
    assert!(editor.replace_verified_image(stale).is_err());
    assert_eq!(editor.snapshot().unwrap(), original);
    let replaced = editor.replace_verified_image(replacement.clone()).unwrap();
    let mut expected = object.clone();
    expected.name = "Replacement.png".to_owned();
    let ObjectKind::Image { asset, .. } = &mut expected.kind else {
        panic!("picture")
    };
    *asset = hash.clone();
    assert_eq!(replaced.project.document.pages[0].objects[0], expected);
    editor.save_new(&package).unwrap();
    let saved = document_core::open_package(&package).unwrap();
    assert!(saved.assets.contains(&hash));
    assert!(!saved.assets.contains(&original_hash));
    let undone = editor.undo(2).unwrap();
    assert_eq!(undone.project.document.pages[0].objects[0], object);
    editor.save_current().unwrap();
    let saved = document_core::open_package(&package).unwrap();
    assert!(saved.assets.contains(&original_hash));
    assert!(!saved.assets.contains(&hash));
    editor.redo(3).unwrap();
    editor.save_current().unwrap();
    assert!(
        document_core::open_package(&package)
            .unwrap()
            .assets
            .contains(&hash)
    );
    let locked = editor
        .apply(transaction(
            4,
            "Lock Picture",
            vec![EditorCommandInput::SetObjectLock {
                object_ids: vec![object.id.as_str().to_owned()],
                locked: true,
            }],
        ))
        .unwrap();
    let mut blocked = replacement;
    blocked.expected_revision = 5;
    assert!(editor.replace_verified_image(blocked).is_err());
    assert_eq!(editor.snapshot().unwrap(), locked);
}

#[test]
fn picture_content_kind_crosses_the_revision_checked_application_boundary() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).unwrap();
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let bytes = png_fixture();
    let inspection = inspect_raster(&bytes).unwrap();
    let imported = editor
        .import_verified_image(VerifiedImageImport {
            expected_revision: 0,
            page_id: Some(page_id),
            name: "Coloring art.png".into(),
            x_um: 20_000,
            y_um: 30_000,
            width_um: 50_000,
            height_um: 25_000,
            media_type: inspection.media_type.clone(),
            asset_hash: AssetHash::for_bytes(&bytes),
            inspection,
            bytes,
        })
        .unwrap();
    let object_id = imported.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let changed = editor
        .apply(transaction(
            imported.project.revision.value(),
            "Treat picture as line art",
            vec![EditorCommandInput::UpdateImageContentKind {
                object_id,
                content_kind: ImageContentKindInput::LineArt,
            }],
        ))
        .unwrap();
    assert!(matches!(
        &changed.project.document.pages[0].objects[0].kind,
        ObjectKind::Image {
            content_kind: ImageContentKind::LineArt,
            ..
        }
    ));

    let undone = editor.undo(changed.project.revision.value()).unwrap();
    assert!(matches!(
        &undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Image {
            content_kind: ImageContentKind::ContinuousTone,
            ..
        }
    ));
}

#[test]
fn picture_crop_and_aspect_lock_cross_the_revision_checked_application_boundary() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).unwrap();
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let bytes = png_fixture();
    let inspection = inspect_raster(&bytes).unwrap();
    let imported = editor
        .import_verified_image(VerifiedImageImport {
            expected_revision: 0,
            page_id: Some(page_id),
            name: "Cover.png".into(),
            x_um: 20_000,
            y_um: 30_000,
            width_um: 50_000,
            height_um: 25_000,
            media_type: inspection.media_type.clone(),
            asset_hash: AssetHash::for_bytes(&bytes),
            inspection,
            bytes,
        })
        .unwrap();
    let object_id = imported.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let changed = editor
        .apply(transaction(
            imported.project.revision.value(),
            "Crop and unlock picture",
            vec![
                EditorCommandInput::UpdateImageCrop {
                    object_id: object_id.clone(),
                    crop: ImageCropInput {
                        left_millionths: 100_000,
                        top_millionths: 50_000,
                        right_millionths: 200_000,
                        bottom_millionths: 150_000,
                    },
                },
                EditorCommandInput::SetImageAspectLock {
                    object_id,
                    locked: false,
                },
            ],
        ))
        .unwrap();
    assert!(matches!(
        changed.project.document.pages[0].objects[0].kind,
        ObjectKind::Image { crop, lock_aspect_ratio: false, .. }
            if crop == ImageCrop::new(100_000, 50_000, 200_000, 150_000).unwrap()
    ));
    let undone = editor.undo(changed.project.revision.value()).unwrap();
    assert!(matches!(
        undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Image { crop, lock_aspect_ratio: true, .. } if crop == ImageCrop::default()
    ));
}

#[test]
fn untagged_raster_policy_crosses_the_revision_checked_application_boundary() {
    let mut editor = EditorModule::new();
    editor.create(create_input()).unwrap();
    let changed = editor
        .apply(transaction(
            0,
            "Assume sRGB for untagged pictures",
            vec![EditorCommandInput::UpdateUntaggedRasterPolicy {
                policy: UntaggedRasterPolicyInput::AssumeSrgb,
            }],
        ))
        .unwrap();
    assert_eq!(
        changed
            .project
            .document
            .color_settings
            .untagged_raster_policy,
        UntaggedRasterPolicy::AssumeSrgb
    );
    let undone = editor.undo(changed.project.revision.value()).unwrap();
    assert_eq!(
        undone
            .project
            .document
            .color_settings
            .untagged_raster_policy,
        UntaggedRasterPolicy::RequireEmbeddedProfile
    );
}

#[test]
fn tables_remain_structured_and_cell_edits_are_undoable() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let first_page = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add word search",
            vec![EditorCommandInput::AddTable {
                page_id: Some(first_page),
                name: "Word search".to_owned(),
                rows: 12,
                columns: 12,
                style: None,
                x_um: 30_000,
                y_um: 80_000,
                width_um: 120_000,
                height_um: 120_000,
            }],
        ))
        .expect("add table");
    let table_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let edited = editor
        .apply(transaction(
            1,
            "Set first letter",
            vec![EditorCommandInput::UpdateTableCell {
                object_id: table_id.clone(),
                row: 0,
                column: 0,
                text: "M".to_owned(),
            }],
        ))
        .expect("edit table cell");
    assert!(matches!(
        &edited.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table }
            if table.rows == 12 && table.columns == 12 && table.cells[0] == "M"
    ));
    let resized = editor
        .apply(transaction(
            2,
            "Resize word search",
            vec![EditorCommandInput::UpdateTableDimensions {
                object_id: table_id,
                rows: 13,
                columns: 10,
            }],
        ))
        .expect("resize table");
    assert!(matches!(
        &resized.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table }
            if table.rows == 13 && table.columns == 10 && table.cells[0] == "M"
    ));
    let resize_undone = editor.undo(3).expect("undo table resize");
    assert!(matches!(
        &resize_undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table }
            if table.rows == 12 && table.columns == 12 && table.cells[0] == "M"
    ));
    let undone = editor.undo(4).expect("undo cell edit");
    assert!(matches!(
        &undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table } if table.cells[0].is_empty()
    ));

    let rejected = editor
        .apply(transaction(
            5,
            "Reject invalid cell",
            vec![EditorCommandInput::UpdateTableCell {
                object_id: undone.project.document.pages[0].objects[0]
                    .id
                    .as_str()
                    .to_owned(),
                row: 12,
                column: 0,
                text: "X".to_owned(),
            }],
        ))
        .expect_err("out-of-range table cell must reject");
    assert_eq!(rejected.code, EditorErrorCode::CommandRejected);
    assert_eq!(
        editor
            .snapshot()
            .expect("snapshot")
            .project
            .revision
            .value(),
        5
    );
}

#[test]
fn grid_paste_admits_225_cells_atomically_and_rejects_invalid_rectangles() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create");
    let added = editor
        .apply(transaction(
            0,
            "Add grid",
            vec![EditorCommandInput::AddTable {
                page_id: Some(created.project.document.pages[0].id.as_str().to_owned()),
                name: "Word search".to_owned(),
                rows: 15,
                columns: 15,
                style: None,
                x_um: 30_000,
                y_um: 80_000,
                width_um: 165_100,
                height_um: 165_100,
            }],
        ))
        .expect("table");
    let original = added.project.document.clone();
    let object_id = original.pages[0].objects[0].id.as_str().to_owned();
    let cells: Vec<Vec<String>> = (0..15)
        .map(|row| (0..15).map(|column| format!("{row}:{column}")).collect())
        .collect();
    let pasted = editor
        .apply(transaction(
            1,
            "Paste grid",
            vec![EditorCommandInput::UpdateTableCells {
                object_id: object_id.clone(),
                row: 0,
                column: 0,
                cells: cells.clone(),
            }],
        ))
        .expect("225 cells in one command");
    let ObjectKind::Table { table } = &pasted.project.document.pages[0].objects[0].kind else {
        panic!("table");
    };
    assert_eq!(table.cells, cells.into_iter().flatten().collect::<Vec<_>>());
    let undone = editor.undo(2).expect("one Undo");
    assert_eq!(undone.project.document, original);
    let redone = editor.redo(3).expect("one Redo");
    assert_eq!(redone.project.document, pasted.project.document);
    for (row, column, cells) in [
        (14, 14, vec![vec!["fits".to_owned(), "outside".to_owned()]]),
        (0, 0, vec![vec!["one".to_owned()], vec![]]),
        (0, 0, vec![vec!["x".repeat(32 * 1024 + 1)]]),
        (0, 0, vec![]),
    ] {
        editor
            .apply(transaction(
                4,
                "Reject invalid paste",
                vec![EditorCommandInput::UpdateTableCells {
                    object_id: object_id.clone(),
                    row,
                    column,
                    cells,
                }],
            ))
            .expect_err("must reject the whole paste");
        let unchanged = editor.snapshot().expect("snapshot");
        assert_eq!(unchanged.project.document, redone.project.document);
        assert_eq!(unchanged.project.revision.value(), 4);
    }
}

#[test]
fn table_layout_controls_cross_the_bridge_atomically_and_are_undoable() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add word search",
            vec![EditorCommandInput::AddTable {
                page_id: Some(page_id),
                name: "Word search".to_owned(),
                rows: 2,
                columns: 2,
                style: None,
                x_um: 30_000,
                y_um: 80_000,
                width_um: 120_000,
                height_um: 120_000,
            }],
        ))
        .expect("add table");
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();

    let formatted = editor
        .apply(transaction(
            1,
            "Format word search",
            vec![
                EditorCommandInput::UpdateTableGridBorder {
                    object_id: object_id.clone(),
                    border: TableGridBorderInput {
                        color: ColorInput {
                            red: 40,
                            green: 50,
                            blue: 60,
                            alpha: 255,
                        },
                        width_millipoints: 1_500,
                    },
                },
                EditorCommandInput::UpdateTableCellAlignment {
                    object_id: object_id.clone(),
                    row: 0,
                    column: 1,
                    horizontal_alignment: TextAlignmentInput::Right,
                    vertical_alignment: TableVerticalAlignmentInput::Bottom,
                },
                EditorCommandInput::UpdateTableTrackSize {
                    object_id: object_id.clone(),
                    axis: TableTrackAxisInput::Column,
                    index: 1,
                    weight: 2_000,
                },
            ],
        ))
        .expect("format table");
    let ObjectKind::Table { table } = &formatted.project.document.pages[0].objects[0].kind else {
        panic!("expected table");
    };
    assert_eq!(table.grid_border.width_millipoints, 1_500);
    assert_eq!(table.grid_border.color.red, 40);
    assert_eq!(table.column_weights, vec![1_000, 2_000]);
    assert_eq!(table.cell_formatting.len(), 1);
    assert_eq!(
        table.cell_formatting[0].vertical_alignment,
        Some(document_core::model::TableVerticalAlignment::Bottom)
    );

    let distributed = editor
        .apply(transaction(
            2,
            "Make columns equal",
            vec![EditorCommandInput::DistributeTableTracks {
                object_id: object_id.clone(),
                axis: TableTrackAxisInput::Column,
            }],
        ))
        .expect("distribute columns");
    assert!(matches!(
        &distributed.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table } if table.column_weights == vec![1_000, 1_000]
    ));

    let distribution_undone = editor.undo(3).expect("undo distribution");
    assert!(matches!(
        &distribution_undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table } if table.column_weights == vec![1_000, 2_000]
    ));
    let formatting_undone = editor.undo(4).expect("undo table formatting");
    assert!(matches!(
        &formatting_undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table }
            if table.column_weights == vec![1_000, 1_000]
                && table.cell_formatting.is_empty()
                && table.grid_border == document_core::model::TableGridBorder::default()
    ));
}

#[test]
fn indexed_table_structure_commands_cross_the_application_bridge_and_undo_once() {
    let (mut editor, object_id) = prepared_table_editor();
    let structured = editor
        .apply(transaction(
            2,
            "Insert selected tracks",
            vec![
                EditorCommandInput::InsertTableTrack {
                    object_id: object_id.clone(),
                    axis: TableTrackAxisInput::Row,
                    index: 1,
                },
                EditorCommandInput::InsertTableTrack {
                    object_id,
                    axis: TableTrackAxisInput::Column,
                    index: 1,
                },
            ],
        ))
        .expect("insert tracks");
    let ObjectKind::Table { table } = &structured.project.document.pages[0].objects[0].kind else {
        panic!("expected table");
    };
    assert_eq!((table.rows, table.columns), (3, 3));
    assert_eq!(table.cells[2 * 3 + 2], "M");
    assert_eq!(table.row_weights, [1_000, 1_000, 2_000]);
    assert_eq!(
        (
            table.cell_formatting[0].row,
            table.cell_formatting[0].column
        ),
        (2, 2)
    );

    let reduced = editor
        .apply(transaction(
            3,
            "Delete selected tracks",
            vec![
                EditorCommandInput::DeleteTableTrack {
                    object_id: structured.project.document.pages[0].objects[0]
                        .id
                        .as_str()
                        .to_owned(),
                    axis: TableTrackAxisInput::Row,
                    index: 0,
                },
                EditorCommandInput::DeleteTableTrack {
                    object_id: structured.project.document.pages[0].objects[0]
                        .id
                        .as_str()
                        .to_owned(),
                    axis: TableTrackAxisInput::Column,
                    index: 0,
                },
            ],
        ))
        .expect("delete tracks");
    assert!(matches!(
        &reduced.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table }
            if table.rows == 2 && table.columns == 2 && table.cells[3] == "M"
    ));

    let deletion_undone = editor.undo(4).expect("undo deletions");
    assert!(matches!(
        &deletion_undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table } if table.rows == 3 && table.columns == 3
    ));
    let undone = editor.undo(5).expect("undo insertions");
    let ObjectKind::Table { table } = &undone.project.document.pages[0].objects[0].kind else {
        panic!("expected table");
    };
    assert_eq!((table.rows, table.columns), (2, 2));
    assert_eq!(table.cells[3], "M");
}

fn prepared_table_editor() -> (EditorModule, String) {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let added = editor
        .apply(transaction(
            0,
            "Add table",
            vec![EditorCommandInput::AddTable {
                page_id: Some(page_id),
                name: "Word search".to_owned(),
                rows: 2,
                columns: 2,
                style: None,
                x_um: 30_000,
                y_um: 80_000,
                width_um: 120_000,
                height_um: 120_000,
            }],
        ))
        .expect("add table");
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    editor
        .apply(transaction(
            1,
            "Prepare selected cell",
            vec![
                EditorCommandInput::UpdateTableCell {
                    object_id: object_id.clone(),
                    row: 1,
                    column: 1,
                    text: "M".to_owned(),
                },
                EditorCommandInput::UpdateTableCellAlignment {
                    object_id: object_id.clone(),
                    row: 1,
                    column: 1,
                    horizontal_alignment: TextAlignmentInput::Right,
                    vertical_alignment: TableVerticalAlignmentInput::Bottom,
                },
                EditorCommandInput::UpdateTableTrackSize {
                    object_id: object_id.clone(),
                    axis: TableTrackAxisInput::Row,
                    index: 1,
                    weight: 2_000,
                },
            ],
        ))
        .expect("prepare table");
    (editor, object_id)
}

fn exact_table_style(source: &AssetHash, family: &str, size_millipoints: u32) -> TextStyleInput {
    TextStyleInput {
        font_face: Some(FontFaceBindingInput {
            source: source.as_str().to_owned(),
            face_index: 2,
        }),
        font_family: family.to_owned(),
        font_size_millipoints: size_millipoints,
        font_weight: 400,
        italic: false,
        alignment: TextAlignmentInput::Center,
        color: ColorInput {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        },
        line_height_per_thousand: 1_200,
          underline: false,
    }
}

#[test]
fn table_text_style_preserves_and_clears_exact_font_binding_deliberately() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).unwrap();
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let source = AssetHash::for_bytes(b"exact table font fixture");
    let added = editor
        .apply(transaction(
            0,
            "Add exactly bound table",
            vec![EditorCommandInput::AddTable {
                page_id: Some(page_id),
                name: "Word search".to_owned(),
                rows: 2,
                columns: 2,
                style: Some(exact_table_style(&source, "Exact table face", 12_000)),
                x_um: 30_000,
                y_um: 80_000,
                width_um: 120_000,
                height_um: 120_000,
            }],
        ))
        .unwrap();
    let object_id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let mut larger = exact_table_style(&source, "Exact table face", 18_000);
    larger.font_face = None;
    let styled = editor
        .apply(transaction(
            1,
            "Format table",
            vec![EditorCommandInput::UpdateTableTextStyle {
                object_id: object_id.clone(),
                style: larger,
            }],
        ))
        .unwrap();
    assert!(matches!(
        &styled.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table }
            if table.style.font_size_millipoints == 18_000
                && table.style.font_face.as_ref().is_some_and(|face| face.source == source)
    ));

    let mut changed_family = exact_table_style(&source, "Different family", 18_000);
    changed_family.font_face = None;
    let changed = editor
        .apply(transaction(
            2,
            "Change table font family",
            vec![EditorCommandInput::UpdateTableTextStyle {
                object_id,
                style: changed_family,
            }],
        ))
        .unwrap();
    assert!(matches!(
        &changed.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table } if table.style.font_face.is_none()
    ));
    let undone = editor.undo(3).unwrap();
    assert!(matches!(
        &undone.project.document.pages[0].objects[0].kind,
        ObjectKind::Table { table }
            if table.style.font_face.as_ref().is_some_and(|face| face.source == source)
    ));
}

#[test]
fn stale_or_hostile_intent_fails_without_mutating_the_session() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let first_page = created.project.document.pages[0].id.as_str().to_owned();

    let stale = editor
        .apply(transaction(
            9,
            "Stale mutation",
            vec![EditorCommandInput::DeletePage {
                page_id: first_page,
            }],
        ))
        .expect_err("stale revision must reject");
    assert_eq!(stale.code, EditorErrorCode::CommandRejected);
    assert_eq!(
        editor
            .snapshot()
            .expect("snapshot")
            .project
            .revision
            .value(),
        0
    );

    let oversized = editor
        .apply(transaction(
            0,
            "Oversized text",
            vec![EditorCommandInput::UpdateText {
                object_id: "object-missing".to_owned(),
                text: "x".repeat(32 * 1024 + 1),
            }],
        ))
        .expect_err("oversized text must reject before dispatch");
    assert_eq!(oversized.code, EditorErrorCode::InvalidInput);
    assert_eq!(
        editor
            .snapshot()
            .expect("snapshot")
            .project
            .revision
            .value(),
        0
    );
}

#[test]
fn deserialization_rejects_unknown_bridge_fields() {
    let raw = r#"{
        "documentId":"document-test",
        "title":"Test",
        "trimWidthUm":215900,
        "trimHeightUm":279400,
        "bleedUm":3175,
        "pageCount":1,
        "unexpected":"not accepted"
    }"#;
    assert!(serde_json::from_str::<CreateDocumentInput>(raw).is_err());

    let valid_command = r#"{
        "type":"deleteObject",
        "objectId":"object-1"
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(valid_command).is_ok());

    let command = r#"{
        "type":"deleteObject",
        "objectId":"object-1",
        "shell":"cmd.exe"
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(command).is_err());

    let picture_kind = r#"{
        "type":"updateImageContentKind",
        "objectId":"object-1",
        "contentKind":"line_art"
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(picture_kind).is_ok());

    let ambiguous_picture_kind = r#"{
        "type":"updateImageContentKind",
        "objectId":"object-1",
        "contentKind":"lineArt"
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(ambiguous_picture_kind).is_err());

    let color_policy = r#"{
        "type":"updateUntaggedRasterPolicy",
        "policy":"assume_srgb"
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(color_policy).is_ok());

    let ambiguous_color_policy = r#"{
        "type":"updateUntaggedRasterPolicy",
        "policy":"assumeSrgb"
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(ambiguous_color_policy).is_err());

    let packed_text_edits = r#"{
        "type":"applyTextEdits",
        "objectId":"object-1",
        "edits":[
            {"startByte":0,"endByte":1,"text":"B"},
            {"startByte":6,"endByte":7,"text":"Q"}
        ]
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(packed_text_edits).is_ok());
    let ambiguous_text_edits = packed_text_edits.replace("startByte", "start_byte");
    assert!(serde_json::from_str::<EditorCommandInput>(&ambiguous_text_edits).is_err());
}

#[test]
fn list_and_table_json_contracts_are_explicit_and_unambiguous() {
    let numbered_list = r#"{
        "type":"updateParagraphStyle",
        "objectId":"object-1",
        "startParagraph":0,
        "endParagraph":2,
        "style":{"listStyle":{"kind":"numbered","startAt":1}}
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(numbered_list).is_ok());
    assert!(
        serde_json::from_str::<EditorCommandInput>(&numbered_list.replace("startAt", "start_at"))
            .is_err()
    );

    let table_border = r#"{
        "type":"updateTableGridBorder",
        "objectId":"object-1",
        "border":{"color":{"red":0,"green":0,"blue":0,"alpha":255},"widthMillipoints":500}
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(table_border).is_ok());
    let cell_alignment = r#"{
        "type":"updateTableCellAlignment",
        "objectId":"object-1",
        "row":0,
        "column":1,
        "horizontalAlignment":"center",
        "verticalAlignment":"middle"
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(cell_alignment).is_ok());
    assert!(
        serde_json::from_str::<EditorCommandInput>(
            &cell_alignment.replace(",\n        \"verticalAlignment\":\"middle\"", "")
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<EditorCommandInput>(
            &cell_alignment.replace("\n        \"horizontalAlignment\":\"center\",", "")
        )
        .is_err()
    );
    let track_size = r#"{
        "type":"updateTableTrackSize",
        "objectId":"object-1",
        "axis":"column",
        "index":1,
        "weight":2000
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(track_size).is_ok());
    let distribute = r#"{
        "type":"distributeTableTracks",
        "objectId":"object-1",
        "axis":"row"
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(distribute).is_ok());
    let insert_track = r#"{
        "type":"insertTableTrack",
        "objectId":"object-1",
        "axis":"row",
        "index":2
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(insert_track).is_ok());
    let delete_track = r#"{
        "type":"deleteTableTrack",
        "objectId":"object-1",
        "axis":"column",
        "index":1
    }"#;
    assert!(serde_json::from_str::<EditorCommandInput>(delete_track).is_ok());
    assert!(
        serde_json::from_str::<EditorCommandInput>(
            &insert_track.replace("\"index\":2", "\"trackIndex\":2")
        )
        .is_err()
    );
}

#[test]
fn save_and_open_round_trip_the_same_editable_session() {
    let temp = TempDirectory::new("roundtrip");
    let package = temp.path().join("magic-book.publisher-v2");
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let edited = editor
        .apply(transaction(
            0,
            "Add saved text",
            vec![EditorCommandInput::AddText {
                page_id: Some(page_id),
                name: "Instructions".to_owned(),
                text: "Saved magic trick".to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 20_000,
                y_um: 30_000,
                width_um: 80_000,
                height_um: 20_000,
            }],
        ))
        .expect("edit document");
    editor.save_new(&package).expect("save package");
    assert!(editor.snapshot().expect("saved snapshot").has_project_path);
    assert_eq!(editor.project_path(), Some(package.as_path()));

    let text_id = edited.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let resaved = editor
        .apply(transaction(
            1,
            "Revise saved text",
            vec![EditorCommandInput::UpdateText {
                object_id: text_id,
                text: "Resaved magic trick".to_owned(),
            }],
        ))
        .expect("revise saved document");
    editor.save_current().expect("save current package");

    let mut reopened = EditorModule::new();
    let snapshot = reopened.open(&package).expect("open package");
    assert_eq!(snapshot.project, resaved.project);
    assert!(snapshot.has_project_path);
    assert_eq!(reopened.project_path(), Some(package.as_path()));
    assert!(
        !snapshot.can_undo,
        "version-one undo history is session-only"
    );
    assert!(!snapshot.can_redo);
}

#[test]
fn failed_open_keeps_the_current_publication_intact() {
    let temp = TempDirectory::new("failed-open");
    let corrupt = temp.path().join("corrupt.publisher-v2");
    std::fs::write(&corrupt, b"not a ZIP archive").expect("write corrupt package");

    let mut editor = EditorModule::new();
    let before = editor.create(create_input()).expect("create document");
    let error = editor
        .open(&corrupt)
        .expect_err("corrupt package must fail");
    assert_eq!(error.code, EditorErrorCode::PersistenceFailed);
    assert!(
        error
            .message
            .contains("damaged or is not a Publisher V2 publication")
    );
    assert!(error.message.contains("Try another copy"));
    assert_eq!(editor.snapshot().expect("current snapshot"), before);
}

#[test]
fn interrupted_save_open_is_rejected_without_replacing_the_current_session() {
    let temp = TempDirectory::new("interrupted-open");
    let package = temp.path().join("blocked.publisher-v2");
    let mut saved = EditorModule::new();
    saved
        .create(create_input())
        .expect("create saved publication");
    saved.save_new(&package).expect("save target publication");
    let opened = document_core::open_package(&package).expect("inspect saved target");
    let operation_id = "11".repeat(32);
    let marker = serde_json::json!({
        "schema_version": 1,
        "operation": "replace_existing",
        "phase": "intent_recorded",
        "operation_id": operation_id.clone(),
        "destination_name": "blocked.publisher-v2",
        "prior_name": format!(".publisher-v2-save-{operation_id}.prior"),
        "replacement_name": format!(".publisher-v2-save-{operation_id}.replacement"),
        "receipt_name": format!(".publisher-v2-save-{operation_id}.completed.json"),
        "prior": {
            "sha256": opened.fingerprint.as_str(),
            "byte_length": std::fs::metadata(&package).expect("target metadata").len()
        },
        "replacement": null,
        "document_id": opened.snapshot.document.id.as_str(),
        "revision": opened.snapshot.revision.value(),
        "semantic_digest": "22".repeat(32)
    });
    std::fs::write(
        temp.path().join(".blocked.publisher-v2.save-journal.json"),
        serde_json::to_vec(&marker).expect("encode interrupted-save marker"),
    )
    .expect("write interrupted-save marker");

    let mut editor = EditorModule::new();
    let mut current_input = create_input();
    current_input.document_id = "current-document".to_owned();
    current_input.title = "Current publication".to_owned();
    let before = editor
        .create(current_input)
        .expect("create current publication");
    let error = editor
        .open(&package)
        .expect_err("interrupted target must not open for editing");
    assert_eq!(error.code, EditorErrorCode::PersistenceFailed);
    assert!(editor.has_document());
    assert_eq!(editor.snapshot().expect("current session survives"), before);
    assert_eq!(editor.project_path(), None);
}

#[test]
fn save_current_refuses_external_changes_and_keeps_the_session() {
    let temp = TempDirectory::new("save-conflict");
    let package = temp.path().join("magic-book.publisher-v2");
    let mut editor = EditorModule::new();
    editor.create(create_input()).expect("create document");
    editor.save_new(&package).expect("save initial package");
    let before = editor.snapshot().expect("snapshot before conflict");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&package)
        .expect("open package for external rewrite")
        .write_all(b"changed")
        .expect("simulate external rewrite");

    let error = editor
        .save_current()
        .expect_err("external change must block overwrite");
    assert_eq!(error.code, EditorErrorCode::PersistenceFailed);
    assert_eq!(
        editor.snapshot().expect("session survives conflict"),
        before
    );
}

#[test]
fn export_source_requires_an_unchanged_saved_package_generation() {
    let temp = TempDirectory::new("export-source");
    let package = temp.path().join("export.publisher-v2");
    let mut editor = EditorModule::new();
    editor.create(create_input()).expect("create document");
    assert_eq!(
        editor.freeze_export_source(0).unwrap_err().code,
        EditorErrorCode::PersistenceFailed
    );

    editor.save_new(&package).expect("save first generation");
    let first = editor.freeze_export_source(0).expect("freeze saved source");
    assert_eq!(first.package_generation, 1);
    assert_eq!(first.project.revision.value(), 0);

    editor
        .apply(transaction(
            0,
            "Add export page",
            vec![EditorCommandInput::AddPage {
                after_page_id: None,
                trim_width_um: 215_900,
                trim_height_um: 279_400,
                bleed_um: 3_175,
            }],
        ))
        .expect("make source dirty");
    assert_eq!(
        editor.freeze_export_source(1).unwrap_err().code,
        EditorErrorCode::PersistenceFailed
    );

    editor.save_current().expect("save second generation");
    let second = editor
        .freeze_export_source(1)
        .expect("freeze resaved source");
    assert_eq!(second.package_generation, 2);
    assert_ne!(
        first.package_fingerprint.as_str(),
        second.package_fingerprint.as_str()
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&package)
        .expect("open package for external rewrite")
        .write_all(b"changed")
        .expect("replace package bytes");
    assert_eq!(
        editor.freeze_export_source(1).unwrap_err().code,
        EditorErrorCode::PersistenceFailed
    );
}

#[test]
fn preflight_source_includes_unsaved_dirty_work_and_its_assets() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let bytes = png_fixture();
    let inspection = inspect_raster(&bytes).expect("inspect PNG");
    let hash = AssetHash::for_bytes(&bytes);
    let imported = editor
        .import_verified_image(VerifiedImageImport {
            expected_revision: 0,
            page_id: Some(page_id),
            name: "Unsaved preflight picture.png".to_owned(),
            x_um: 20_000,
            y_um: 30_000,
            width_um: 50_000,
            height_um: 25_000,
            media_type: inspection.media_type.clone(),
            asset_hash: hash.clone(),
            inspection,
            bytes: bytes.clone(),
        })
        .expect("import unsaved picture");

    assert!(
        !imported.has_project_path,
        "preflight must not depend on a saved package"
    );
    let source = editor
        .snapshot_for_preflight(imported.project.revision.value())
        .expect("snapshot dirty unsaved work for preflight");

    assert_eq!(source.project, imported.project);
    assert_eq!(source.project.revision.value(), 1);
    let asset = source.assets.get(&hash).expect("included picture asset");
    assert_eq!(asset.bytes, bytes);
    assert!(asset.raster.is_some());
}

#[test]
fn preflight_source_rejects_a_stale_expected_revision() {
    let mut editor = EditorModule::new();
    editor.create(create_input()).expect("create document");
    editor
        .apply(transaction(
            0,
            "Make preflight request stale",
            vec![EditorCommandInput::AddPage {
                after_page_id: None,
                trim_width_um: 215_900,
                trim_height_um: 279_400,
                bleed_um: 3_175,
            }],
        ))
        .expect("advance publication revision");

    let stale = editor
        .snapshot_for_preflight(0)
        .expect_err("stale preflight snapshot must fail");
    assert_eq!(stale.code, EditorErrorCode::CommandRejected);
}

#[test]
fn autosave_writes_separate_monotonic_recovery_generations() {
    let temp = TempDirectory::new("autosave");
    let recovery_root = temp.path().join("recovery");
    let mut editor = EditorModule::new();
    editor.create(create_input()).expect("create document");
    let first = editor.autosave(&recovery_root).expect("first autosave");
    let second = editor.autosave(&recovery_root).expect("second autosave");

    assert_eq!(first.generation, 1);
    assert_eq!(second.generation, 2);
    assert_ne!(first.package_path, second.package_path);
    assert!(first.base_fingerprint.is_none());
    assert!(second.base_fingerprint.is_none());
}

#[test]
fn page_bleed_is_canonical_and_undoable() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let changed = editor
        .apply(transaction(
            0,
            "Set page bleed",
            vec![EditorCommandInput::UpdatePageBleed {
                page_id,
                bleed_um: 6_350,
            }],
        ))
        .expect("update bleed");
    assert_eq!(changed.project.document.pages[0].bleed.top.value(), 6_350);
    let undone = editor.undo(1).expect("undo bleed");
    assert_eq!(undone.project.document.pages[0].bleed.top.value(), 3_175);
}

#[test]
fn page_trim_and_orientation_are_canonical_and_undoable() {
    let mut editor = EditorModule::new();
    let created = editor.create(create_input()).expect("create document");
    let page_id = created.project.document.pages[0].id.as_str().to_owned();
    let changed = editor
        .apply(transaction(
            0,
            "Make page landscape",
            vec![EditorCommandInput::UpdatePageTrim {
                page_id,
                trim_width_um: 279_400,
                trim_height_um: 215_900,
            }],
        ))
        .expect("update trim");
    assert_eq!(
        changed.project.document.pages[0].trim.width.value(),
        279_400
    );
    assert_eq!(
        changed.project.document.pages[0].trim.height.value(),
        215_900
    );
    let undone = editor.undo(1).expect("undo trim");
    assert_eq!(undone.project.document.pages[0].trim.width.value(), 215_900);
    assert_eq!(
        undone.project.document.pages[0].trim.height.value(),
        279_400
    );
}

#[test]
fn opening_recovery_never_binds_save_to_the_autosave_package() {
    let temp = TempDirectory::new("open-recovery");
    let recovery_root = temp.path().join("recovery");
    let mut source = EditorModule::new();
    source
        .create(create_input())
        .expect("create source document");
    let candidate = source.autosave(&recovery_root).expect("write recovery");

    let mut recovered = EditorModule::new();
    let snapshot = recovered.open_recovery(&candidate).expect("open recovery");
    assert!(!snapshot.has_project_path);
    assert!(
        recovered.save_current().is_err(),
        "recovery requires Save As"
    );
}
