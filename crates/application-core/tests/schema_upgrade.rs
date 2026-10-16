use application_core::{
    ApplyTransactionInput, CreateDocumentInput, DocumentLifecycleState, EditorCommandInput,
    EditorModule,
};
use document_core::package::{SCHEMA_VERSION, open_package};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

struct TempDirectory(PathBuf);

impl TempDirectory {
    fn new() -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "publisher-v2-schema-upgrade-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create temporary fixture folder");
        Self(path)
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}

fn write_older_publication(path: &Path) {
    let mut editor = EditorModule::new();
    editor
        .create(CreateDocumentInput {
            document_id: "upgrade-document".to_owned(),
            title: "Preserve this publication".to_owned(),
            trim_width_um: 215_900,
            trim_height_um: 279_400,
            bleed_um: 3_175,
            page_count: 1,
        })
        .expect("create source publication");
    editor.save_new(path).expect("save fixture publication");

    let entries = {
        let file = std::fs::File::open(path).expect("open fixture archive");
        let mut archive = zip::ZipArchive::new(file).expect("parse fixture archive");
        (0..archive.len())
            .map(|index| {
                let mut entry = archive.by_index(index).expect("read fixture entry");
                let name = entry.name().to_owned();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).expect("read fixture bytes");
                if name == "manifest.json" {
                    let mut manifest: serde_json::Value =
                        serde_json::from_slice(&bytes).expect("parse fixture manifest");
                    manifest["schema_version"] = serde_json::json!(SCHEMA_VERSION - 1);
                    bytes = serde_json::to_vec_pretty(&manifest).expect("encode older manifest");
                }
                (name, bytes)
            })
            .collect::<Vec<_>>()
    };
    let file = std::fs::File::create(path).expect("rewrite temporary fixture archive");
    let mut archive = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o600);
    for (name, bytes) in entries {
        archive
            .start_file(name, options)
            .expect("start fixture entry");
        archive.write_all(&bytes).expect("write fixture bytes");
    }
    archive.finish().expect("finish older fixture");
}

#[test]
fn older_source_preserves_its_binding_until_a_new_copy_has_succeeded() {
    let temp = TempDirectory::new();
    let original = temp.0.join("original.publisher-v2");
    let copy = temp.0.join("current-copy.publisher-v2");
    write_older_publication(&original);
    let original_bytes = std::fs::read(&original).expect("read original bytes");
    let mut editor = EditorModule::new();
    let opened = editor.open(&original).expect("open older publication");
    assert!(!opened.is_dirty, "opening an older file is not an edit");
    assert_eq!(opened.source_schema_version, SCHEMA_VERSION - 1);
    assert!(opened.requires_save_copy);
    assert_eq!(
        editor.document_lifecycle(),
        DocumentLifecycleState::Clean {
            revision: 0,
            has_project_path: true,
            requires_save_copy: true,
        }
    );
    assert_eq!(editor.project_path(), Some(original.as_path()));
    let edited = editor
        .apply(ApplyTransactionInput {
            expected_revision: opened.project.revision.value(),
            label: "Edit older publication".to_owned(),
            commands: vec![EditorCommandInput::UpdatePageBleed {
                page_id: opened.project.document.pages[0].id.as_str().to_owned(),
                bleed_um: 6_350,
            }],
        })
        .expect("older source remains editable");
    assert!(edited.is_dirty);
    assert!(edited.requires_save_copy);

    editor
        .save_current()
        .expect_err("an older source must require Save Copy before replacement");
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
    assert_eq!(editor.snapshot().unwrap(), edited);

    editor
        .save_new(&original)
        .expect_err("a copy cannot replace its original");
    editor
        .save_new(&temp.0.join("missing-parent").join("copy.publisher-v2"))
        .expect_err("failed copy cannot rebind the publication");
    assert_eq!(editor.project_path(), Some(original.as_path()));
    assert_eq!(editor.snapshot().unwrap(), edited);
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);

    editor
        .save_new(&copy)
        .expect("save verified current-format copy");
    assert_eq!(editor.project_path(), Some(copy.as_path()));
    let saved = editor.snapshot().unwrap();
    assert!(!saved.is_dirty);
    assert!(!saved.requires_save_copy);
    assert_eq!(saved.source_schema_version, SCHEMA_VERSION);
    assert_eq!(open_package(&copy).unwrap().snapshot, edited.project);
    editor
        .save_current()
        .expect("repeat Save uses the verified copy");
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
}

#[test]
fn current_writer_recovery_retains_older_source_ancestry_until_save_copy() {
    let temp = TempDirectory::new();
    let original = temp.0.join("original.publisher-v2");
    let copy = temp.0.join("recovered-copy.publisher-v2");
    let recovery_root = temp.0.join("recovery");
    write_older_publication(&original);
    let original_bytes = std::fs::read(&original).unwrap();
    let mut editor = EditorModule::new();
    let opened = editor.open(&original).unwrap();
    let candidate = editor
        .autosave(&recovery_root)
        .expect("autosave older-source session");
    assert_eq!(candidate.source_schema_version, SCHEMA_VERSION - 1);
    assert_eq!(
        open_package(&candidate.package_path)
            .unwrap()
            .schema_version,
        SCHEMA_VERSION
    );

    let mut recovered = EditorModule::default();
    let snapshot = recovered
        .open_recovery(&candidate)
        .expect("reopen exact recovery");
    assert_eq!(snapshot.project, opened.project);
    assert_eq!(snapshot.source_schema_version, SCHEMA_VERSION - 1);
    assert!(snapshot.requires_save_copy);
    assert!(!snapshot.has_project_path);
    assert!(snapshot.is_dirty, "recovery remains unsaved work");
    recovered
        .save_current()
        .expect_err("recovery cannot bind to an older source or autosave");
    recovered
        .save_new(&copy)
        .expect("save recovered work to a new verified destination");
    assert!(!recovered.snapshot().unwrap().requires_save_copy);
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
    let next = recovered.autosave(&recovery_root).unwrap();
    assert_eq!(next.source_schema_version, SCHEMA_VERSION);
}
