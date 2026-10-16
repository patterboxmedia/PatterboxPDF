//! Sustained application acceptance on the Windows exact-retirement boundary.
//! This small Text Box fixture is not the representative 24-page milestone workload.
#![cfg(windows)]

use application_core::{
    ApplyTransactionInput, CreateDocumentInput, EditorCommandInput, EditorModule, EditorSnapshot,
    TextBoxSizingModeInput,
};
use document_core::model::{FontFaceBinding, ObjectKind};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "publisher-sustained-recovery-{}-{nonce}",
            std::process::id()
        ));
        // Exclusive creation: never adopt or delete a pre-existing directory.
        std::fs::create_dir(&path).unwrap();
        assert!(path.is_absolute());
        Self(path)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Only the exact absolute directory successfully created by this fixture.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn apply(editor: &mut EditorModule, command: EditorCommandInput) -> EditorSnapshot {
    editor
        .apply(ApplyTransactionInput {
            expected_revision: editor.snapshot().unwrap().project.revision.value(),
            label: "Edit acceptance Text Box".into(),
            commands: vec![command],
        })
        .unwrap()
}

fn seed(path: &Path) -> String {
    let mut editor = EditorModule::new();
    let created = editor
        .create(CreateDocumentInput {
            document_id: "sustained-text-box".into(),
            title: "Original acceptance instructions".into(),
            trim_width_um: 215_900,
            trim_height_um: 279_400,
            bleed_um: 3_175,
            page_count: 1,
        })
        .unwrap();
    let assembled = apply(
        &mut editor,
        EditorCommandInput::AddText {
            page_id: Some(created.project.document.pages[0].id.as_str().into()),
            name: "Instructions".into(),
            text: "Place the paper on the table.".into(),
            style: None,
            sizing_mode: TextBoxSizingModeInput::Fixed,
            x_um: 12_000,
            y_um: 12_000,
            width_um: 180_000,
            height_um: 100_000,
        },
    );
    // Use the same embedded, checked-in font package boundary as the instructional
    // page contract. All subsequent content edits pass through committed APIs.
    let mut project = assembled.project;
    let mut assets = document_core::AssetStore::default();
    let source = assets.insert(
        "font/ttf",
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec(),
    );
    let object = &mut project.document.pages[0].objects[0];
    let id = object.id.as_str().to_owned();
    let ObjectKind::Text { style, .. } = &mut object.kind else {
        panic!("Text Box")
    };
    style.font_face = Some(FontFaceBinding {
        source,
        face_index: 0,
    });
    document_core::package::save_new_package(path, &project, &assets).unwrap();
    id
}

fn assert_content(snapshot: &EditorSnapshot, expected: &str, revision: u64) {
    assert_eq!(snapshot.project.revision.value(), revision);
    assert_eq!(snapshot.project.document.pages.len(), 1);
    let objects = &snapshot.project.document.pages[0].objects;
    assert_eq!(objects.len(), 1);
    assert!(matches!(&objects[0].kind, ObjectKind::Text { text, .. } if text == expected));
}

fn bytes_under(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                bytes_under(&entry.path())
            } else {
                metadata.len()
            }
        })
        .sum()
}

fn envelopes(root: &Path) -> usize {
    std::fs::read_dir(root.join("sustained-text-box"))
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .count()
}

#[test]
#[ignore = "Sustained Windows acceptance: run in release mode with --ignored"]
#[allow(
    clippy::too_many_lines,
    reason = "one sustained edit/save/reopen acceptance journey"
)]
fn committed_text_edits_remain_bounded_across_reopened_sessions() {
    const SESSIONS: u64 = 6;
    const EDITS_PER_SESSION: u64 = 170;
    const SAVE_INTERVAL: u64 = 10;
    let started = Instant::now();
    let fixture = Fixture::new();
    let seed_path = fixture.0.join("seed.publisher-v2");
    let saved_path = fixture.0.join("saved.publisher-v2");
    let root = fixture.0.join("recovery");
    let object_id = seed(&seed_path);
    let mut unknown = EditorModule::new();
    unknown.open(&seed_path).unwrap();
    let sibling = unknown.autosave(&root).unwrap();
    drop(unknown);
    let sibling_bytes = std::fs::read(&sibling.package_path).unwrap();
    let baseline_bytes = bytes_under(&root);
    let mut peak_bytes = baseline_bytes;
    let mut peak_envelopes = envelopes(&root);
    let mut retired_total = 0;
    let mut expected = "Place the paper on the table.".to_owned();
    let mut revision = 1;
    for session in 0..SESSIONS {
        let mut editor = EditorModule::new();
        let reopened = editor
            .open(if session == 0 {
                &seed_path
            } else {
                &saved_path
            })
            .unwrap();
        assert_content(&reopened, &expected, revision);
        let mut owned = BTreeSet::new();
        for edit in 1..=EDITS_PER_SESSION {
            let sequence = session * EDITS_PER_SESSION + edit;
            expected = format!(
                "STEP {sequence:04}\nPlace the paper on the table.\nFold it carefully, then check the edge."
            );
            let snapshot = apply(
                &mut editor,
                EditorCommandInput::UpdateText {
                    object_id: object_id.clone(),
                    text: expected.clone(),
                },
            );
            revision += 1;
            assert_content(&snapshot, &expected, revision);
            assert!(snapshot.is_dirty);
            let candidate = editor.autosave(&root).unwrap();
            assert!(owned.insert(candidate.package_path));
            peak_bytes = peak_bytes.max(bytes_under(&root));
            peak_envelopes = peak_envelopes.max(envelopes(&root));
            if edit % SAVE_INTERVAL == 0 {
                if sequence == SAVE_INTERVAL {
                    editor.save_new(&saved_path).unwrap();
                } else {
                    editor.save_current().unwrap();
                }
                let report = editor.retire_saved_recoveries(&root);
                assert!(report.warning.is_none(), "{report:?}");
                assert!(
                    report
                        .outcomes
                        .iter()
                        .all(|outcome| outcome.retired && outcome.warning.is_none()),
                    "{report:?}"
                );
                let retired: BTreeSet<_> = report
                    .outcomes
                    .iter()
                    .map(|outcome| outcome.package_path.clone())
                    .collect();
                assert_eq!(
                    retired, owned,
                    "only this session's exact tokens may retire"
                );
                assert!(owned.iter().all(|path| !path.exists()));
                retired_total += report.outcomes.len();
                owned.clear();
                assert_eq!(envelopes(&root), 1);
                assert_eq!(
                    bytes_under(&root),
                    baseline_bytes,
                    "Save returns storage to the preserved sibling baseline"
                );
                assert_eq!(std::fs::read(&sibling.package_path).unwrap(), sibling_bytes);
            }
        }
        drop(editor);
        let mut reopened = EditorModule::new();
        let snapshot = reopened.open(&saved_path).unwrap();
        assert_content(&snapshot, &expected, revision);
        assert!(!snapshot.is_dirty);
        println!(
            "reopen {}: exact text {:?}; revision {revision}",
            session + 1,
            expected
        );
    }
    let final_bytes = bytes_under(&root);
    assert_eq!(retired_total, 1020);
    assert_eq!(peak_envelopes, 11);
    assert_eq!(envelopes(&root), 1);
    // The bound scales with the save cadence and embedded resource, not edit count.
    assert!(
        peak_bytes <= baseline_bytes * 12,
        "peak {peak_bytes}; baseline {baseline_bytes}"
    );
    assert_eq!(final_bytes, baseline_bytes);
    println!(
        "1020 committed Text Box edits/autosaves; 102 manual Saves; 6 final reopens; retired {retired_total}; recovery envelopes peak={peak_envelopes} final=1; recovery bytes peak={peak_bytes} final={final_bytes}; elapsed={:?}",
        started.elapsed()
    );
}
