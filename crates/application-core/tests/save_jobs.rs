use application_core::{
    ApplyTransactionInput, CreateDocumentInput, EditorCommandInput, EditorModule,
    TextBoxSizingModeInput,
};
use document_core::{inspect_interrupted_save, open_package};
use std::{
    fs,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "publisher-save-job-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> std::path::PathBuf {
        self.0.join("publication.publisher-v2")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn editor() -> EditorModule {
    let mut editor = EditorModule::new();
    editor
        .create(CreateDocumentInput {
            document_id: "save-job-publication".to_owned(),
            title: "Snapshot Save".to_owned(),
            trim_width_um: 215_900,
            trim_height_um: 279_400,
            bleed_um: 3_175,
            page_count: 1,
        })
        .unwrap();
    editor
}
fn add_text(editor: &mut EditorModule, text: &str) {
    let before = editor.snapshot().unwrap();
    editor
        .apply(ApplyTransactionInput {
            expected_revision: before.project.revision.value(),
            label: "Add Text Box".to_owned(),
            commands: vec![EditorCommandInput::AddText {
                page_id: Some(before.project.document.pages[0].id.as_str().to_owned()),
                name: text.to_owned(),
                text: text.to_owned(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fit,
                x_um: 20_000,
                y_um: 20_000,
                width_um: 50_000,
                height_um: 20_000,
            }],
        })
        .unwrap();
}

#[test]
fn snapshot_save_keeps_newer_edits_dirty_and_preserves_exact_undo_history() {
    let fixture = Fixture::new();
    let mut editor = editor();
    editor.save_new(&fixture.path()).unwrap();
    add_text(&mut editor, "Saved snapshot");
    let captured = editor.snapshot().unwrap();
    let job = editor.prepare_save_job().unwrap();
    add_text(&mut editor, "Continue editing during Save");
    let current = editor.snapshot().unwrap();
    editor.complete_save_job(job.run(|| false)).unwrap();
    assert_eq!(
        open_package(&fixture.path()).unwrap().snapshot,
        captured.project
    );
    assert_eq!(editor.snapshot().unwrap().project, current.project);
    assert!(editor.snapshot().unwrap().is_dirty);
    editor.undo(current.project.revision.value()).unwrap();
    assert_eq!(
        editor.snapshot().unwrap().project.document,
        captured.project.document
    );
    editor.save_current().unwrap();
    assert!(!editor.snapshot().unwrap().is_dirty);
}

#[test]
#[cfg(windows)]
fn cancellation_after_journal_creation_restores_exact_previous_file_and_allows_retry() {
    let fixture = Fixture::new();
    let mut editor = editor();
    editor.save_new(&fixture.path()).unwrap();
    let original = fs::read(fixture.path()).unwrap();
    add_text(&mut editor, "Unsaved work survives cancellation");
    let before = editor.snapshot().unwrap();
    let saw_journal = AtomicBool::new(false);
    let job = editor.prepare_save_job().unwrap();
    let outcome = job.run(|| {
        let pending = inspect_interrupted_save(&fixture.path()).unwrap().is_some();
        saw_journal.fetch_or(pending, Ordering::Relaxed);
        pending
    });
    assert!(
        editor
            .complete_save_job(outcome)
            .unwrap_err()
            .message
            .contains("cancelled")
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    assert!(saw_journal.load(Ordering::Relaxed), "cancellation must observe a real durable retry journal");
    assert_eq!(editor.snapshot().unwrap().project, before.project);
    assert!(editor.snapshot().unwrap().is_dirty);
    assert!(inspect_interrupted_save(&fixture.path()).unwrap().is_none());
    editor.save_current().unwrap();
    assert_eq!(
        open_package(&fixture.path()).unwrap().snapshot,
        before.project
    );
}

#[test]
fn stale_save_result_cannot_bind_a_replacement_document() {
    let fixture = Fixture::new();
    let mut editor = editor();
    editor.save_new(&fixture.path()).unwrap();
    let job = editor.prepare_save_job().unwrap();
    editor
        .create(CreateDocumentInput {
            document_id: "replacement".to_owned(),
            title: "Replacement".to_owned(),
            trim_width_um: 215_900,
            trim_height_um: 279_400,
            bleed_um: 3_175,
            page_count: 1,
        })
        .unwrap();
    let replacement = editor.snapshot().unwrap();
    assert!(editor.complete_save_job(job.run(|| false)).is_err());
    assert_eq!(editor.snapshot().unwrap().project, replacement.project);
    assert!(!editor.snapshot().unwrap().has_project_path);
}

#[test]
fn first_save_and_save_copy_preserve_newer_edits_and_the_original_file() {
    let fixture = Fixture::new();
    let mut editor = editor();
    add_text(&mut editor, "First captured version");
    let captured = editor.snapshot().unwrap();
    let job = editor.prepare_save_new_job().unwrap();
    add_text(&mut editor, "Later edit");
    editor
        .complete_save_job(job.run_new(&fixture.path(), || false))
        .unwrap();
    assert_eq!(
        open_package(&fixture.path()).unwrap().snapshot,
        captured.project
    );
    assert!(editor.snapshot().unwrap().is_dirty);
    let original = fs::read(fixture.path()).unwrap();
    let copy = fixture.0.join("copy.publisher-v2");
    let latest = editor.snapshot().unwrap();
    let job = editor.prepare_save_new_job().unwrap();
    editor
        .complete_save_job(job.run_new(&copy, || false))
        .unwrap();
    assert_eq!(open_package(&copy).unwrap().snapshot, latest.project);
    assert_eq!(fs::read(fixture.path()).unwrap(), original);
    assert_eq!(editor.project_path().unwrap(), copy);
    assert!(!editor.snapshot().unwrap().is_dirty);
}

#[test]
#[cfg(windows)]
fn cancelled_first_save_leaves_no_destination_and_releases_its_owned_stage() {
    let fixture = Fixture::new();
    let mut editor = editor();
    add_text(&mut editor, "Keep unsaved first publication");
    let before = editor.snapshot().unwrap();
    let saw_stage = AtomicBool::new(false);
    let job = editor.prepare_save_new_job().unwrap();
    let outcome = job.run_new(&fixture.path(), || {
        let staged = fs::read_dir(&fixture.0).unwrap().next().is_some();
        saw_stage.fetch_or(staged, Ordering::Relaxed);
        staged
    });
    assert!(
        editor
            .complete_save_job(outcome)
            .unwrap_err()
            .message
            .contains("cancelled")
    );
    assert!(!fixture.path().exists());
    assert!(saw_stage.load(Ordering::Relaxed), "cancellation must observe a real owned staging file");
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    assert_eq!(editor.snapshot().unwrap().project, before.project);
    assert!(!editor.snapshot().unwrap().has_project_path);
}
