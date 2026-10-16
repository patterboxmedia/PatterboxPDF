use application_core::{
    ApplyTransactionInput, CreateDocumentInput, EditorCommandInput, EditorModule,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "publisher-recovery-lifecycle-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn root(&self) -> PathBuf {
        self.0.join("recovery")
    }
    fn saved(&self) -> PathBuf {
        self.0.join("saved.publisher-v2")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn create() -> EditorModule {
    let mut editor = EditorModule::new();
    editor.create(input()).unwrap();
    editor
}
fn input() -> CreateDocumentInput {
    CreateDocumentInput {
        document_id: "document-lineage".into(),
        title: "Recovery lineage".into(),
        trim_width_um: 215_900,
        trim_height_um: 279_400,
        bleed_um: 3_175,
        page_count: 1,
    }
}
fn edit(editor: &mut EditorModule) {
    editor
        .apply(ApplyTransactionInput {
            expected_revision: editor.snapshot().unwrap().project.revision.value(),
            label: "Add page".into(),
            commands: vec![EditorCommandInput::AddPage {
                after_page_id: None,
                trim_width_um: 215_900,
                trim_height_um: 279_400,
                bleed_um: 3_175,
            }],
        })
        .unwrap();
}

#[test]
fn quota_warning_survives_commands_save_and_a_recreated_session() {
    let fixture = Fixture::new();
    let document = fixture.root().join("document-lineage");
    std::fs::create_dir_all(&document).unwrap();
    for i in 0..64 {
        std::fs::create_dir(document.join(format!("unknown-{i}"))).unwrap();
    }
    let mut editor = create();
    editor.autosave(&fixture.root()).unwrap_err();
    let warning = editor.snapshot().unwrap().recovery_warning.unwrap();
    assert!(warning.contains("Automatic recovery is paused"));
    edit(&mut editor);
    editor.save_new(&fixture.saved()).unwrap();
    assert!(!editor.snapshot().unwrap().is_dirty);
    assert_eq!(
        editor.snapshot().unwrap().recovery_warning.as_ref(),
        Some(&warning)
    );
    drop(editor);
    let mut reopened = EditorModule::new();
    reopened.open(&fixture.saved()).unwrap();
    reopened.autosave(&fixture.root()).unwrap_err();
    assert!(
        reopened
            .snapshot()
            .unwrap()
            .recovery_warning
            .unwrap()
            .contains("Automatic recovery is paused")
    );
}

#[cfg(windows)]
#[test]
fn saves_retire_only_created_lineage_and_keep_history_and_unknown_siblings() {
    let fixture = Fixture::new();
    let mut unknown = create();
    let sibling = unknown.autosave(&fixture.root()).unwrap();
    let mut editor = create();
    let original = editor.autosave(&fixture.root()).unwrap();
    edit(&mut editor);
    let latest = editor.autosave(&fixture.root()).unwrap();
    editor.save_new(&fixture.saved()).unwrap();
    let report = editor.retire_saved_recoveries(&fixture.root());
    assert!(report.warning.is_none(), "{report:?}");
    assert_eq!(
        report
            .outcomes
            .iter()
            .filter(|outcome| outcome.retired)
            .count(),
        2
    );
    assert!(!original.package_path.exists());
    assert!(!latest.package_path.exists());
    assert!(sibling.package_path.exists());
    assert_eq!(editor.undo(1).unwrap().project.document.pages.len(), 1);
    assert_eq!(editor.redo(2).unwrap().project.document.pages.len(), 2);
}

#[cfg(windows)]
#[test]
fn opening_a_selected_recovery_grants_no_sibling_authority() {
    let fixture = Fixture::new();
    let mut editor = create();
    let first = editor.autosave(&fixture.root()).unwrap();
    edit(&mut editor);
    let second = editor.autosave(&fixture.root()).unwrap();
    editor.open_recovery(&first).unwrap();
    editor.save_new(&fixture.saved()).unwrap();
    assert_eq!(
        editor
            .retire_saved_recoveries(&fixture.root())
            .outcomes
            .len(),
        1
    );
    assert!(!first.package_path.exists());
    assert!(second.package_path.exists());
}

#[cfg(windows)]
#[test]
fn undo_and_redo_preserve_alternatives_but_new_autosaves_can_retire() {
    let fixture = Fixture::new();
    let mut editor = create();
    edit(&mut editor);
    let before_undo = editor.autosave(&fixture.root()).unwrap();
    editor.undo(1).unwrap();
    let before_redo = editor.autosave(&fixture.root()).unwrap();
    editor.redo(2).unwrap();
    let current = editor.autosave(&fixture.root()).unwrap();
    editor.save_new(&fixture.saved()).unwrap();
    let report = editor.retire_saved_recoveries(&fixture.root());
    assert!(report.warning.is_none(), "{report:?}");
    assert!(before_undo.package_path.exists());
    assert!(before_redo.package_path.exists());
    assert!(!current.package_path.exists());
}

#[cfg(windows)]
#[test]
fn failed_replacements_preserve_authority_successful_replacements_end_it() {
    let fixture = Fixture::new();
    let mut editor = create();
    let owned = editor.autosave(&fixture.root()).unwrap();
    editor.open(&fixture.saved()).unwrap_err();
    let mut invalid = input();
    invalid.page_count = 0;
    editor.create(invalid).unwrap_err();
    let mut forged = owned.clone();
    forged.generation += 1;
    editor.open_recovery(&forged).unwrap_err();
    editor.undo(0).unwrap_err();
    editor.save_new(&fixture.saved()).unwrap();
    assert!(editor.retire_saved_recoveries(&fixture.root()).outcomes[0].retired);
    let old = editor.autosave(&fixture.root()).unwrap();
    editor.open(&fixture.saved()).unwrap();
    assert!(
        editor
            .retire_saved_recoveries(&fixture.root())
            .outcomes
            .is_empty()
    );
    assert!(old.package_path.exists());
    let old = editor.autosave(&fixture.root()).unwrap();
    editor.create(input()).unwrap();
    editor
        .save_new(&fixture.0.join("new.publisher-v2"))
        .unwrap();
    assert!(
        editor
            .retire_saved_recoveries(&fixture.root())
            .outcomes
            .is_empty()
    );
    assert!(old.package_path.exists());
}

#[cfg(windows)]
#[test]
fn failed_save_and_changed_destination_never_retire_recovery() {
    let fixture = Fixture::new();
    let mut editor = create();
    editor.save_new(&fixture.saved()).unwrap();
    edit(&mut editor);
    let candidate = editor.autosave(&fixture.root()).unwrap();
    std::fs::write(fixture.saved(), b"external replacement").unwrap();
    editor.save_current().unwrap_err();
    assert!(
        editor
            .retire_saved_recoveries(&fixture.root())
            .outcomes
            .is_empty()
    );
    assert!(candidate.package_path.exists());
    let copy = fixture.0.join("copy.publisher-v2");
    editor.save_new(&copy).unwrap();
    std::fs::write(&copy, b"changed after save").unwrap();
    assert!(
        editor
            .retire_saved_recoveries(&fixture.root())
            .warning
            .is_some()
    );
    assert!(candidate.package_path.exists());
    assert!(!editor.snapshot().unwrap().is_dirty);
}

#[cfg(windows)]
#[test]
fn cleanup_failure_keeps_saved_state_and_persistent_warning() {
    let fixture = Fixture::new();
    let mut editor = create();
    let candidate = editor.autosave(&fixture.root()).unwrap();
    std::fs::write(
        candidate.package_path.parent().unwrap().join("unknown.txt"),
        b"preserve",
    )
    .unwrap();
    editor.save_new(&fixture.saved()).unwrap();
    let report = editor.retire_saved_recoveries(&fixture.root());
    assert!(
        report.warning.is_some()
            || report
                .outcomes
                .iter()
                .any(|outcome| outcome.warning.is_some())
    );
    assert!(!editor.snapshot().unwrap().is_dirty);
    let warning = editor.snapshot().unwrap().recovery_warning.unwrap();
    assert!(warning.contains("Publication saved"));
    edit(&mut editor);
    editor.save_current().unwrap();
    assert_eq!(editor.snapshot().unwrap().recovery_warning, Some(warning));
    assert!(
        candidate
            .package_path
            .parent()
            .unwrap()
            .join("unknown.txt")
            .exists()
    );
}

#[cfg(not(windows))]
#[test]
fn unsupported_cleanup_preserves_successful_autosave_and_explains_limitation() {
    let fixture = Fixture::new();
    let mut editor = create();
    let candidate = editor.autosave(&fixture.root()).unwrap();
    assert!(candidate.package_path.exists());
    assert!(
        editor
            .snapshot()
            .unwrap()
            .recovery_warning
            .unwrap()
            .contains("cleanup is unavailable")
    );
    editor.save_new(&fixture.saved()).unwrap();
    assert!(
        editor
            .retire_saved_recoveries(&fixture.root())
            .outcomes
            .is_empty()
    );
    assert!(candidate.package_path.exists());
}

#[cfg(windows)]
#[test]
fn quota_pause_clears_only_after_new_recovery_write_and_releases_owned_capacity() {
    let fixture = Fixture::new();
    let mut editor = create();
    for _ in 0..64 {
        editor.autosave(&fixture.root()).unwrap();
    }
    editor.autosave(&fixture.root()).unwrap_err();
    editor.save_new(&fixture.saved()).unwrap();
    let report = editor.retire_saved_recoveries(&fixture.root());
    assert_eq!(
        report
            .outcomes
            .iter()
            .filter(|outcome| outcome.retired)
            .count(),
        64
    );
    assert!(
        editor
            .recovery_warning()
            .unwrap()
            .contains("Automatic recovery is paused")
    );
    editor.autosave(&fixture.root()).unwrap();
    assert!(editor.recovery_warning().is_none());
    assert!(!editor.snapshot().unwrap().is_dirty);
}

#[cfg(windows)]
#[test]
fn token_cap_preserves_successful_copy_and_warns_without_false_autosave_failure() {
    let fixture = Fixture::new();
    let mut editor = create();
    for _ in 0..64 {
        editor.autosave(&fixture.root()).unwrap();
    }
    // Changing roots cannot make this module retain an unbounded list of tokens.
    let other_root = fixture.0.join("other-recovery");
    let extra = editor.autosave(&other_root).unwrap();
    assert!(extra.package_path.exists());
    assert!(
        editor
            .recovery_warning()
            .unwrap()
            .contains("additional copies")
    );
    editor.save_new(&fixture.saved()).unwrap();
    let report = editor.retire_saved_recoveries(&other_root);
    assert!(report.outcomes.iter().all(|outcome| !outcome.retired));
    assert!(extra.package_path.exists());
    let report = editor.retire_saved_recoveries(&fixture.root());
    assert!(report.outcomes.iter().all(|outcome| outcome.retired));
    assert!(
        editor.recovery_warning().is_some(),
        "untracked copy still needs attention"
    );
    assert!(extra.package_path.exists());
}

#[cfg(windows)]
#[test]
fn successful_cleanup_retry_clears_only_resolved_warning() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let mut editor = create();
    let candidate = editor.autosave(&fixture.root()).unwrap();
    for i in 0..63 {
        std::fs::create_dir(
            fixture
                .root()
                .join("document-lineage")
                .join(format!("unknown-{i}")),
        )
        .unwrap();
    }
    editor.autosave(&fixture.root()).unwrap_err();
    editor.save_new(&fixture.saved()).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&candidate.package_path)
        .unwrap();
    let report = editor.retire_saved_recoveries(&fixture.root());
    assert!(report.outcomes.iter().any(|outcome| !outcome.retired));
    assert!(
        editor
            .recovery_warning()
            .unwrap()
            .contains("could not be removed")
    );
    drop(lock);
    assert!(editor.retire_saved_recoveries(&fixture.root()).outcomes[0].retired);
    let warning = editor.recovery_warning().unwrap();
    assert!(warning.contains("Automatic recovery is paused"));
    assert!(!warning.contains("could not be removed"));
    editor.autosave(&fixture.root()).unwrap();
    assert!(editor.recovery_warning().is_none());
}
