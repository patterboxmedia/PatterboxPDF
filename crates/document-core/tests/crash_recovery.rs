mod common;

use common::{TempDirectory, letter_document, rectangle};
use document_core::assets::AssetStore;
use document_core::commands::{Command, DocumentSession, ObjectLocation, Transaction};
use document_core::model::Revision;
use document_core::package::{ProjectSnapshot, save_new_package};
use document_core::recovery::{
    RecoveryDiscovery, discard_recovery, discover_recoveries, open_recovery_package, save_recovery,
};
use sha2::{Digest, Sha256};

#[test]
fn recovery_discovery_uses_generation_and_validated_ancestry_not_timestamps() {
    let temp = TempDirectory::new("recovery");
    let recovery_root = temp.path().join("recovery");
    let manual_path = temp.path().join("book.publisher-v2");
    let assets = AssetStore::default();
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let initial = ProjectSnapshot::from_session(&session);
    let base = save_new_package(&manual_path, &initial, &assets).expect("manual save");
    let first =
        save_recovery(&recovery_root, 4, &initial, &assets, Some(&base)).expect("first recovery");

    let page_id = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Recover shape",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: rectangle("Recovered", 10_000, 20_000),
                },
            ),
        )
        .expect("edit publication");
    let revised = ProjectSnapshot::from_session(&session);
    let second =
        save_recovery(&recovery_root, 5, &revised, &assets, Some(&base)).expect("second recovery");

    let candidates = discover_recoveries(&recovery_root)
        .expect("discover valid recoveries")
        .candidates;
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0], second, "higher generation sorts first");
    assert_eq!(candidates[1], first);
    assert_eq!(candidates[0].base_fingerprint.as_ref(), Some(&base));
}

#[test]
fn pre_archive_recoveries_remain_discoverable_and_quarantinable() {
    let temp = TempDirectory::new("legacy-recovery");
    let recovery_root = temp.path().join("recovery");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let document_root = recovery_root.join(snapshot.document.id.as_str());
    let package_path = document_root
        .join("generation-00000000000000000007-revision-00000000000000000000-legacy.publisher-v2");
    std::fs::create_dir_all(&package_path).expect("create legacy recovery package");
    let manifest = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 10,
        "snapshot": &snapshot,
        "assets": {}
    }))
    .expect("serialize legacy manifest");
    std::fs::write(package_path.join("manifest.json"), &manifest).expect("write legacy manifest");
    let mut fingerprint = Sha256::new();
    fingerprint.update(b"publisher-v2-package-v1\0");
    fingerprint.update((manifest.len() as u64).to_le_bytes());
    fingerprint.update(&manifest);
    let fingerprint = format!("{:x}", fingerprint.finalize());
    let metadata = serde_json::json!({
        "schema_version": 1,
        "document_id": snapshot.document.id.as_str(),
        "document_title": snapshot.document.title,
        "generation": 7,
        "revision": 0,
        "base_fingerprint": null,
        "package_fingerprint": fingerprint
    });
    std::fs::write(
        package_path.join("recovery.json"),
        serde_json::to_vec_pretty(&metadata).expect("serialize recovery metadata"),
    )
    .expect("write recovery metadata");

    let candidates = discover_recoveries(&recovery_root)
        .expect("discover legacy recovery")
        .candidates;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].package_path, package_path);
    assert_eq!(candidates[0].source_schema_version, 10);
    let opened = open_recovery_package(&candidates[0]).expect("open the indexed legacy recovery");
    assert_eq!(opened.snapshot, snapshot);
    assert!(opened.assets.is_empty());
    assert_eq!(opened.schema_version, 10);
    let quarantined =
        discard_recovery(&recovery_root, &candidates[0]).expect("quarantine legacy recovery");
    assert!(quarantined.is_dir());
    assert!(!package_path.exists());
}

#[test]
fn recovery_open_refuses_a_self_consistent_replacement_after_discovery() {
    let temp = TempDirectory::new("recovery-open-race");
    let recovery_root = temp.path().join("recovery");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let candidate = save_recovery(&recovery_root, 1, &snapshot, &AssetStore::default(), None)
        .expect("save selected recovery");
    let metadata_path = candidate
        .package_path
        .parent()
        .expect("recovery envelope")
        .join("recovery.json");
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&metadata_path).expect("read selected metadata"))
            .expect("parse selected metadata");
    metadata["generation"] = serde_json::json!(2);
    std::fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&metadata).expect("serialize replacement metadata"),
    )
    .expect("replace selected metadata");

    assert!(open_recovery_package(&candidate).is_err());
    // Compatibility ancestry is part of the selected recovery authority too.
    metadata["generation"] = serde_json::json!(candidate.generation);
    metadata["source_schema_version"] = serde_json::json!(0);
    std::fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    assert!(
        open_recovery_package(&candidate).is_err(),
        "unreadable source ancestry cannot open"
    );
    metadata["source_schema_version"] =
        serde_json::json!(document_core::package::SCHEMA_VERSION + 1);
    std::fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .unwrap();
    assert!(
        open_recovery_package(&candidate).is_err(),
        "future source ancestry cannot open"
    );
}

#[test]
fn damaged_recovery_is_ignored_without_hiding_a_valid_sibling() {
    let temp = TempDirectory::new("damaged-recovery");
    let recovery_root = temp.path().join("recovery");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let valid = save_recovery(&recovery_root, 1, &snapshot, &AssetStore::default(), None)
        .expect("valid recovery");
    let damaged = recovery_root
        .join(snapshot.document.id.as_str())
        .join("generation-damaged");
    std::fs::create_dir_all(&damaged).expect("create damaged candidate");
    std::fs::write(damaged.join("recovery.json"), b"not json").expect("damage metadata");

    assert_eq!(
        discover_recoveries(&recovery_root)
            .expect("discover remaining candidate")
            .candidates,
        vec![valid]
    );
}

#[test]
fn discard_quarantines_only_the_exact_revalidated_recovery_package() {
    let temp = TempDirectory::new("discard-recovery");
    let recovery_root = temp.path().join("recovery");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let assets = AssetStore::default();
    let first = save_recovery(&recovery_root, 1, &snapshot, &assets, None).expect("first recovery");
    let second =
        save_recovery(&recovery_root, 2, &snapshot, &assets, None).expect("second recovery");

    let quarantined =
        discard_recovery(&recovery_root, &second).expect("quarantine exact candidate");

    assert!(!second.package_path.exists());
    assert!(quarantined.starts_with(recovery_root.join(".discarded")));
    assert!(
        quarantined.exists(),
        "discard remains recoverable from quarantine"
    );
    assert!(
        document_core::open_package(&quarantined.join("publication.publisher-v2")).is_ok(),
        "quarantined package remains a valid editable publication"
    );
    assert!(
        first.package_path.exists(),
        "a sibling recovery must remain"
    );
    assert_eq!(
        discover_recoveries(&recovery_root)
            .expect("discover remaining candidate")
            .candidates,
        vec![first]
    );
}

#[test]
fn discard_refuses_a_reparse_replacement_when_the_platform_can_create_one() {
    let temp = TempDirectory::new("refuse-reparse-recovery");
    let recovery_root = temp.path().join("recovery");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let candidate = save_recovery(&recovery_root, 1, &snapshot, &AssetStore::default(), None)
        .expect("recovery");
    let envelope = candidate.package_path.parent().expect("recovery envelope");
    let outside = temp.path().join("outside-recovery");
    std::fs::rename(envelope, &outside).expect("move recovery envelope outside");
    if create_directory_link(&outside, envelope).is_err() {
        return;
    }

    assert!(discard_recovery(&recovery_root, &candidate).is_err());
    assert!(outside.exists(), "reparse target must remain untouched");
    assert!(
        candidate.package_path.exists(),
        "replacement link must not be moved"
    );
}

#[cfg(unix)]
fn create_directory_link(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_directory_link(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[test]
fn discard_refuses_an_out_of_root_or_changed_candidate() {
    let temp = TempDirectory::new("refuse-discard-recovery");
    let recovery_root = temp.path().join("recovery");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let candidate = save_recovery(&recovery_root, 1, &snapshot, &AssetStore::default(), None)
        .expect("recovery");
    let outside = temp.path().join("outside-recovery");
    std::fs::create_dir_all(&outside).expect("outside directory");
    let mut forged = candidate.clone();
    forged.package_path = outside.join("publication.publisher-v2");

    assert!(discard_recovery(&recovery_root, &forged).is_err());
    assert!(outside.exists(), "an outside path must never be deleted");

    std::fs::write(
        candidate
            .package_path
            .parent()
            .expect("recovery envelope")
            .join("recovery.json"),
        b"changed",
    )
    .expect("change indexed candidate");
    assert!(discard_recovery(&recovery_root, &candidate).is_err());
    assert!(
        candidate.package_path.exists(),
        "a candidate changed after discovery must not be deleted"
    );
}

#[test]
fn recovery_discovery_keeps_valid_work_when_candidate_budget_is_reached() {
    let temp = TempDirectory::new("recovery-partial");
    let root = temp.path().join("recovery");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let first = save_recovery(&root, 1, &snapshot, &AssetStore::default(), None).unwrap();
    let envelope = first.package_path.parent().unwrap();
    for index in 0..1_000 {
        let copy = envelope.parent().unwrap().join(format!("copy-{index}"));
        std::fs::create_dir(&copy).unwrap();
        std::fs::copy(&first.package_path, copy.join("publication.publisher-v2")).unwrap();
        std::fs::copy(envelope.join("recovery.json"), copy.join("recovery.json")).unwrap();
    }
    let discovered =
        discover_recoveries(&root).expect("keep validated choices when scan budget is reached");
    assert!(!discovered.candidates.is_empty());
    assert!(discovered.has_more);
    let mut scan = RecoveryDiscovery::open(&root).unwrap();
    let mut paths = std::collections::BTreeSet::new();
    let mut examined = 0;
    loop {
        let batch = scan.next_batch();
        assert!(batch.candidates.len() <= 25);
        assert!(batch.examined_entries <= 1_000);
        examined += batch.examined_entries;
        for candidate in batch.candidates {
            assert!(paths.insert(candidate.package_path));
        }
        if !batch.has_more {
            break;
        }
    }
    assert_eq!(paths.len(), 1_001);
    assert_eq!(
        examined, 1_003,
        "continuation never revisits directory entries"
    );
}

#[test]
fn recovery_discovery_bounds_invalid_entries_and_reports_damage_without_hiding_valid_copies() {
    let temp = TempDirectory::new("recovery-damaged-partial");
    let root = temp.path().join("recovery");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let valid = save_recovery(&root, 1, &snapshot, &AssetStore::default(), None).unwrap();
    for index in 0..1_001 {
        std::fs::write(
            root.join(format!("unexpected-{index}")),
            b"invalid root entry",
        )
        .unwrap();
    }
    for index in 0..50 {
        std::fs::create_dir(
            valid
                .package_path
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(format!("damaged-{index}")),
        )
        .unwrap();
    }
    let mut scan = RecoveryDiscovery::open(&root).unwrap();
    let mut candidates = Vec::new();
    let mut skipped = 0;
    let mut examined = 0;
    let mut batches = 0;
    loop {
        let batch = scan.next_batch();
        assert!(batch.examined_entries <= 1_000);
        skipped += batch.skipped_entries;
        examined += batch.examined_entries;
        candidates.extend(batch.candidates);
        batches += 1;
        assert!(
            batches < 10,
            "continuation must make progress through damage"
        );
        if !batch.has_more {
            break;
        }
    }
    assert_eq!(candidates, vec![valid]);
    assert_eq!(skipped, 1_051);
    assert_eq!(examined, 1_054);
    assert!(batches > 1);
    assert!(!scan.next_batch().has_more);
}
