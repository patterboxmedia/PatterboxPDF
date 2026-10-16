mod common;

use common::{TempDirectory, letter_document};
use document_core::{AssetStore, ProjectSnapshot, Revision, save_recovery};
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[test]
fn existing_empty_envelopes_stop_the_next_recovery_before_writing() {
    let temp = TempDirectory::new("recovery-quota");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let document_root = temp.path().join(snapshot.document.id.as_str());
    std::fs::create_dir_all(&document_root).unwrap();
    for index in 0..64 {
        std::fs::create_dir(document_root.join(format!("unknown-{index}"))).unwrap();
    }
    assert!(save_recovery(temp.path(), 1, &snapshot, &AssetStore::default(), None).is_err());
    assert_eq!(
        std::fs::read_dir(document_root)
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != ".recovery.lock")
            .count(),
        64
    );
}

#[test]
fn quarantine_and_unknown_bytes_still_consume_the_allowance() {
    let temp = TempDirectory::new("recovery-quarantine-quota");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let candidate = save_recovery(temp.path(), 1, &snapshot, &AssetStore::default(), None).unwrap();
    document_core::discard_recovery(temp.path(), &candidate).unwrap();
    let document = temp.path().join(snapshot.document.id.as_str());
    for index in 0..63 {
        std::fs::create_dir(document.join(format!("unknown-{index}"))).unwrap();
    }
    for _restart in 0..2 {
        assert!(save_recovery(temp.path(), 2, &snapshot, &AssetStore::default(), None).is_err());
    }
    let other = TempDirectory::new("recovery-byte-quota");
    let document = other.path().join(snapshot.document.id.as_str());
    std::fs::create_dir(&document).unwrap();
    let unknown = std::fs::File::create(document.join("damaged.bin")).unwrap();
    // The reserved next archive must fit; admitting then deleting would lose work.
    unknown.set_len(505 * 1024 * 1024).unwrap();
    assert!(save_recovery(other.path(), 1, &snapshot, &AssetStore::default(), None).is_err());
    assert_eq!(unknown.metadata().unwrap().len(), 505 * 1024 * 1024);
}

#[test]
fn lock_contention_and_scan_overflow_pause_without_creating_an_envelope() {
    let temp = TempDirectory::new("recovery-lock-quota");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let document = temp.path().join(snapshot.document.id.as_str());
    std::fs::create_dir(&document).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(document.join(".recovery.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert!(save_recovery(temp.path(), 1, &snapshot, &AssetStore::default(), None).is_err());
    lock.unlock().unwrap();
    drop(lock);
    save_recovery(temp.path(), 2, &snapshot, &AssetStore::default(), None).unwrap();
    let mut nested = document.join("unknown");
    for _ in 0..18 {
        std::fs::create_dir(&nested).unwrap();
        nested = nested.join("nested");
    }
    assert!(save_recovery(temp.path(), 3, &snapshot, &AssetStore::default(), None).is_err());
}

#[cfg(windows)]
mod windows {
    use super::*;
    use document_core::recovery::{capture_recovery_retirement, retire_recoveries};
    use document_core::{open_package, save_new_package};

    #[test]
    fn exact_retirement_preserves_siblings_and_refuses_changed_save() {
        let temp = TempDirectory::new("exact-retirement");
        let root = temp.path().join("recovery");
        let snapshot = ProjectSnapshot {
            document: letter_document(),
            revision: Revision::initial(),
        };
        let candidate = save_recovery(&root, 1, &snapshot, &AssetStore::default(), None).unwrap();
        let sibling = save_recovery(&root, 2, &snapshot, &AssetStore::default(), None).unwrap();
        let token = capture_recovery_retirement(&candidate).unwrap();
        let saved_path = temp.path().join("saved.publisher-v2");
        let fingerprint = save_new_package(&saved_path, &snapshot, &AssetStore::default()).unwrap();
        let mut changed = snapshot.clone();
        changed.revision = Revision::new(1);
        let refused = retire_recoveries(
            &root,
            &saved_path,
            &fingerprint,
            &changed,
            std::slice::from_ref(&token),
        );
        assert!(refused.warning.is_some());
        assert!(candidate.package_path.exists());
        let result = retire_recoveries(&root, &saved_path, &fingerprint, &snapshot, &[token]);
        assert!(result.warning.is_none(), "{result:?}");
        assert!(result.outcomes[0].retired, "{result:?}");
        assert!(!candidate.package_path.parent().unwrap().exists());
        assert!(sibling.package_path.exists());
        assert_eq!(open_package(&saved_path).unwrap().snapshot, snapshot);
        let mut newer = snapshot.clone();
        newer.revision = Revision::new(3);
        let newer_candidate =
            save_recovery(&root, 3, &newer, &AssetStore::default(), None).unwrap();
        let newer_token = capture_recovery_retirement(&newer_candidate).unwrap();
        let refused =
            retire_recoveries(&root, &saved_path, &fingerprint, &snapshot, &[newer_token]);
        assert!(!refused.outcomes[0].retired);
        assert!(newer_candidate.package_path.exists());
    }

    #[test]
    fn package_metadata_and_envelope_substitution_refuse_retirement() {
        for target in ["publication.publisher-v2", "recovery.json", "envelope"] {
            let temp = TempDirectory::new("retirement-substitution");
            let root = temp.path().join("recovery");
            let snapshot = ProjectSnapshot {
                document: letter_document(),
                revision: Revision::initial(),
            };
            let candidate =
                save_recovery(&root, 1, &snapshot, &AssetStore::default(), None).unwrap();
            let token = capture_recovery_retirement(&candidate).unwrap();
            let envelope = candidate.package_path.parent().unwrap();
            if target == "envelope" {
                let old = envelope.with_extension("old");
                std::fs::rename(envelope, &old).unwrap();
                std::fs::create_dir(envelope).unwrap();
                for name in ["publication.publisher-v2", "recovery.json"] {
                    std::fs::copy(old.join(name), envelope.join(name)).unwrap();
                }
            } else {
                let path = envelope.join(target);
                let old = path.with_extension("old");
                std::fs::rename(&path, &old).unwrap();
                std::fs::copy(old, path).unwrap();
            }
            let saved = temp.path().join("saved.publisher-v2");
            let fingerprint = save_new_package(&saved, &snapshot, &AssetStore::default()).unwrap();
            let result = retire_recoveries(&root, &saved, &fingerprint, &snapshot, &[token]);
            assert!(!result.outcomes[0].retired, "{target}: {result:?}");
            assert!(candidate.package_path.exists());
        }
    }

    #[test]
    fn extra_files_leave_a_counted_envelope_and_successful_save() {
        let temp = TempDirectory::new("retirement-leftovers");
        let root = temp.path().join("recovery");
        let snapshot = ProjectSnapshot {
            document: letter_document(),
            revision: Revision::initial(),
        };
        let candidate = save_recovery(&root, 1, &snapshot, &AssetStore::default(), None).unwrap();
        let token = capture_recovery_retirement(&candidate).unwrap();
        let extra = candidate.package_path.parent().unwrap().join("unknown.bin");
        std::fs::write(&extra, "preserve me").unwrap();
        let saved = temp.path().join("saved.publisher-v2");
        let fingerprint = save_new_package(&saved, &snapshot, &AssetStore::default()).unwrap();
        let result = retire_recoveries(&root, &saved, &fingerprint, &snapshot, &[token]);
        assert!(!result.outcomes[0].retired);
        assert!(result.outcomes[0].warning.is_some());
        assert_eq!(std::fs::read_to_string(extra).unwrap(), "preserve me");
        assert_eq!(open_package(&saved).unwrap().snapshot, snapshot);
        assert!(!candidate.package_path.exists());
        for index in 0..63 {
            std::fs::create_dir(
                root.join(snapshot.document.id.as_str())
                    .join(format!("leftover-{index}")),
            )
            .unwrap();
        }
        assert!(
            save_recovery(&root, 2, &snapshot, &AssetStore::default(), None).is_err(),
            "partially retired envelope still consumes quota"
        );
    }

    #[test]
    fn same_saved_name_and_hard_link_cannot_be_retired() {
        let temp = TempDirectory::new("retirement-same-save");
        let root = temp.path().join("recovery");
        let snapshot = ProjectSnapshot {
            document: letter_document(),
            revision: Revision::initial(),
        };
        let candidate = save_recovery(&root, 1, &snapshot, &AssetStore::default(), None).unwrap();
        let token = capture_recovery_retirement(&candidate).unwrap();
        let link = temp.path().join("saved.publisher-v2");
        std::fs::hard_link(&candidate.package_path, &link).unwrap();
        for path in [&candidate.package_path, &link] {
            let result = retire_recoveries(
                &root,
                path,
                &candidate.package_fingerprint,
                &snapshot,
                std::slice::from_ref(&token),
            );
            assert!(result.warning.is_some() || !result.outcomes[0].retired);
            assert!(candidate.package_path.exists());
        }
    }

    #[test]
    fn one_thousand_one_save_cycles_keep_recovery_storage_bounded() {
        let temp = TempDirectory::new("retirement-1001-cycles");
        let root = temp.path().join("recovery");
        let mut peak_bytes = 0;
        for revision in 1..=1001 {
            let mut snapshot = ProjectSnapshot {
                document: letter_document(),
                revision: Revision::new(revision),
            };
            snapshot.document.title = format!("Edit {revision}");
            let candidate =
                save_recovery(&root, revision, &snapshot, &AssetStore::default(), None).unwrap();
            let token = capture_recovery_retirement(&candidate).unwrap();
            peak_bytes = peak_bytes.max(
                std::fs::metadata(&candidate.package_path).unwrap().len()
                    + std::fs::metadata(
                        candidate
                            .package_path
                            .parent()
                            .unwrap()
                            .join("recovery.json"),
                    )
                    .unwrap()
                    .len(),
            );
            let saved = temp.path().join(format!("save-{revision}.publisher-v2"));
            let fingerprint = save_new_package(&saved, &snapshot, &AssetStore::default()).unwrap();
            let report = retire_recoveries(&root, &saved, &fingerprint, &snapshot, &[token]);
            assert!(report.outcomes[0].retired, "cycle {revision}: {report:?}");
            assert_eq!(open_package(&saved).unwrap().snapshot, snapshot);
            assert_eq!(
                std::fs::read_dir(root.join(snapshot.document.id.as_str()))
                    .unwrap()
                    .count(),
                1,
                "only zero-byte lock remains"
            );
        }
        assert!(peak_bytes < 16 * 1024);
        eprintln!(
            "1001 recovery/manual-save cycles: peak recovery bytes={peak_bytes}; final bytes=0, envelopes=0"
        );
    }
}

#[test]
fn competing_writers_cannot_both_admit_the_last_envelope() {
    let temp = TempDirectory::new("recovery-competing-quota");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let document = temp.path().join(snapshot.document.id.as_str());
    std::fs::create_dir(&document).unwrap();
    for index in 0..63 {
        std::fs::create_dir(document.join(format!("unknown-{index}"))).unwrap();
    }
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let run = || {
            barrier.wait();
            save_recovery(temp.path(), 1, &snapshot, &AssetStore::default(), None).is_ok()
        };
        let first = scope.spawn(run);
        let second = scope.spawn(run);
        [first.join().unwrap(), second.join().unwrap()]
    });
    assert_eq!(results.into_iter().filter(|result| *result).count(), 1);
    assert_eq!(std::fs::read_dir(document).unwrap().count(), 65);
}

#[test]
fn a_dead_process_does_not_strand_the_allowance_lock() {
    use std::io::{BufRead, BufReader};
    let temp = TempDirectory::new("recovery-crash-lock");
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let document = temp.path().join(snapshot.document.id.as_str());
    std::fs::create_dir(&document).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "hold_quota_lock_child",
            "--nocapture",
        ])
        .env("PUBLISHER_TEST_QUOTA_LOCK", document.join(".recovery.lock"))
        .stdout(std::process::Stdio::piped())
        .stdin(std::process::Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = command.spawn().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "child exited before locking"
        );
        if line.contains("LOCK HELD") {
            break;
        }
        line.clear();
    }
    assert!(save_recovery(temp.path(), 1, &snapshot, &AssetStore::default(), None).is_err());
    child.kill().unwrap();
    child.wait().unwrap();
    save_recovery(temp.path(), 2, &snapshot, &AssetStore::default(), None).unwrap();
}

#[test]
#[ignore = "child harness used by crash-released lock test"]
fn hold_quota_lock_child() {
    use std::io::{Read, Write};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(std::env::var_os("PUBLISHER_TEST_QUOTA_LOCK").unwrap())
        .unwrap();
    file.lock().unwrap();
    println!("LOCK HELD");
    std::io::stdout().flush().unwrap();
    let mut byte = [0];
    let _ = std::io::stdin().read(&mut byte);
}
