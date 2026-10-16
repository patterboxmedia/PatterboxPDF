//! Bounded disk admission and session-authorized cleanup. Filenames never grant deletion authority.

use super::{RecoveryCandidate, RecoveryError, require_plain_directory};
use crate::{AssetStore, PackageFingerprint, ProjectSnapshot};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

const MAX_ENVELOPES: usize = 64;
const MAX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 16;
const LOCK_NAME: &str = ".recovery.lock";

pub(super) struct AllowanceLock {
    _file: File,
    #[cfg(windows)]
    _directory: publisher_windows_atomic_file::PinnedDirectory,
}

pub(super) fn lock_document(
    root: &Path,
    document_id: &str,
) -> Result<AllowanceLock, RecoveryError> {
    let directory = root.join(document_id);
    for ancestor in directory.ancestors() {
        if !ancestor.as_os_str().is_empty() {
            require_plain_directory(ancestor)?;
        }
    }
    let path = directory.join(LOCK_NAME);
    #[cfg(windows)]
    let pinned = publisher_windows_atomic_file::PinnedDirectory::for_destination(&path)?;
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && (!metadata.is_file()
            || metadata.file_type().is_symlink()
            || super::is_windows_reparse_point(&metadata)
            || metadata.len() != 0)
    {
        return Err(RecoveryError::StoragePaused);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0002_0000); // O_NOFOLLOW: refuse a substituted lock symlink.
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Refuse path redirection and hold this name against replacement. File::try_lock
        // provides the same cooperating-process exclusion on every supported host.
        options.custom_flags(0x0020_0000).share_mode(0x0000_0003);
    }
    let file = options.open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || super::is_windows_reparse_point(&metadata) || metadata.len() != 0 {
        return Err(RecoveryError::StoragePaused);
    }
    file.try_lock().map_err(|_| RecoveryError::StoragePaused)?;
    Ok(AllowanceLock {
        _file: file,
        #[cfg(windows)]
        _directory: pinned,
    })
}

pub(super) fn admit(
    root: &Path,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
) -> Result<AllowanceLock, RecoveryError> {
    let lock = lock_document(root, snapshot.document.id.as_str())
        .map_err(|_| RecoveryError::StoragePaused)?;
    let mut inventory = Inventory::default();
    inventory.document(&root.join(snapshot.document.id.as_str()), true)?;
    let discarded = root.join(super::DISCARDED_DIRECTORY);
    match fs::symlink_metadata(&discarded) {
        Ok(_) => {
            require_plain_directory(&discarded).map_err(|_| RecoveryError::StoragePaused)?;
            let document = discarded.join(snapshot.document.id.as_str());
            match fs::symlink_metadata(&document) {
                Ok(_) => inventory.document(&document, false)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(RecoveryError::StoragePaused),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(RecoveryError::StoragePaused),
    }
    // Stored ZIP payload plus a conservative upper bound on its manifest, names,
    // local/central headers, ZIP64 records and recovery metadata. The package writer
    // separately enforces these same manifest and entry limits before writing.
    let selected = assets.for_document(&snapshot.document);
    let prospective = selected
        .iter()
        .try_fold(
            crate::package::PACKAGE_LIMITS_V1.manifest_bytes
                + super::MAX_RECOVERY_METADATA_BYTES
                + 1024,
            |total, (_, asset)| {
                total
                    .checked_add(asset.bytes.len() as u64)?
                    .checked_add(400)
            },
        )
        .ok_or(RecoveryError::StoragePaused)?;
    if inventory.envelopes >= MAX_ENVELOPES
        || inventory
            .bytes
            .checked_add(prospective)
            .is_none_or(|bytes| bytes > MAX_BYTES)
    {
        return Err(RecoveryError::StoragePaused);
    }
    Ok(lock)
}

#[derive(Default)]
struct Inventory {
    envelopes: usize,
    entries: usize,
    bytes: u64,
}

impl Inventory {
    fn document(&mut self, path: &Path, active: bool) -> Result<(), RecoveryError> {
        require_plain_directory(path).map_err(|_| RecoveryError::StoragePaused)?;
        for entry in fs::read_dir(path).map_err(|_| RecoveryError::StoragePaused)? {
            let entry = entry.map_err(|_| RecoveryError::StoragePaused)?;
            if active && entry.file_name() == LOCK_NAME {
                continue;
            }
            self.envelopes += 1;
            self.visit(&entry.path(), 0)?;
        }
        Ok(())
    }
    fn visit(&mut self, path: &Path, depth: usize) -> Result<(), RecoveryError> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES || depth > MAX_DEPTH || self.envelopes > MAX_ENVELOPES {
            return Err(RecoveryError::StoragePaused);
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| RecoveryError::StoragePaused)?;
        if metadata.file_type().is_symlink() || super::is_windows_reparse_point(&metadata) {
            return Err(RecoveryError::StoragePaused);
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path).map_err(|_| RecoveryError::StoragePaused)? {
                self.visit(
                    &entry.map_err(|_| RecoveryError::StoragePaused)?.path(),
                    depth + 1,
                )?;
            }
        } else if metadata.is_file() {
            self.bytes = self
                .bytes
                .checked_add(metadata.len())
                .ok_or(RecoveryError::StoragePaused)?;
            if self.bytes > MAX_BYTES {
                return Err(RecoveryError::StoragePaused);
            }
        } else {
            return Err(RecoveryError::StoragePaused);
        }
        Ok(())
    }
}

/// Session-local authority captured from an exact, validated recovery envelope.
/// Keep only tokens created or explicitly opened by this editor lineage.
#[derive(Clone, Debug)]
pub struct RecoveryRetirementCandidate {
    candidate: RecoveryCandidate,
    #[cfg(windows)]
    identities: [publisher_windows_atomic_file::FileIdentity; 3],
}

/// A successful result means every file and its empty envelope was removed.
/// A warning may leave an incomplete envelope; never retry from filenames alone.
#[derive(Debug)]
pub struct RecoveryRetirementOutcome {
    pub package_path: PathBuf,
    pub retired: bool,
    pub warning: Option<String>,
}

#[derive(Debug, Default)]
pub struct RecoveryRetirementReport {
    pub outcomes: Vec<RecoveryRetirementOutcome>,
    pub warning: Option<String>,
}

/// Captures exact file identities for a caller-authorized candidate.
///
/// # Errors
/// Refuses changed candidates, legacy envelopes and hosts without safe deletion primitives.
pub fn capture_recovery_retirement(
    candidate: &RecoveryCandidate,
) -> Result<RecoveryRetirementCandidate, RecoveryError> {
    #[cfg(windows)]
    {
        let held = HeldCandidate::open(candidate)?;
        Ok(RecoveryRetirementCandidate {
            candidate: candidate.clone(),
            identities: held.identities()?,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = candidate;
        Err(RecoveryError::InvalidCandidate)
    }
}

/// Retires only exact session-authorized candidates after validating a durable Save.
/// Keeps the saved file and ancestry pinned until all cleanup ends. Failures are warnings;
/// callers must preserve the successful Save result and forget only successful tokens.
#[must_use]
pub fn retire_recoveries(
    root: &Path,
    saved_path: &Path,
    fingerprint: &PackageFingerprint,
    snapshot: &ProjectSnapshot,
    candidates: &[RecoveryRetirementCandidate],
) -> RecoveryRetirementReport {
    let mut report = RecoveryRetirementReport::default();
    if candidates.is_empty() {
        return report;
    }
    #[cfg(windows)]
    {
        match retire_windows(
            root,
            saved_path,
            fingerprint,
            snapshot,
            candidates,
            &mut report,
            &mut |_| Ok(()),
        ) {
            Ok(()) => {}
            Err(_) => {
                report.warning = Some(
                    "Publication saved. Recovery cleanup paused because the files could not be checked. Open recovery folder to inspect the remaining copies, then Save again to retry.".to_owned(),
                );
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (root, saved_path, fingerprint, snapshot);
        report.warning = Some(
            "Publication saved. Automatic recovery cleanup is unavailable on this computer. Open recovery folder to inspect the remaining copies.".to_owned(),
        );
        for candidate in candidates {
            report.outcomes.push(RecoveryRetirementOutcome {
                package_path: candidate.candidate.package_path.clone(),
                retired: false,
                warning: report.warning.clone(),
            });
        }
    }
    report
}

#[cfg(windows)]
struct HeldCandidate {
    directory: publisher_windows_atomic_file::PinnedDirectory,
    package: publisher_windows_atomic_file::PinnedFile,
    metadata: publisher_windows_atomic_file::PinnedFile,
}

#[cfg(windows)]
impl HeldCandidate {
    fn open(candidate: &RecoveryCandidate) -> Result<Self, RecoveryError> {
        let paths = super::recovery_paths(&candidate.package_path)?;
        if paths.legacy_directory {
            return Err(RecoveryError::InvalidCandidate);
        }
        let directory = publisher_windows_atomic_file::PinnedDirectory::for_removable_directory(
            &paths.storage_path,
        )?;
        let package = directory.pin_file(&candidate.package_path)?;
        let metadata = directory.pin_file(&paths.metadata_path)?;
        if super::read_candidate(&candidate.package_path)? != *candidate {
            return Err(RecoveryError::InvalidCandidate);
        }
        Ok(Self {
            directory,
            package,
            metadata,
        })
    }
    fn identities(
        &self,
    ) -> Result<[publisher_windows_atomic_file::FileIdentity; 3], RecoveryError> {
        Ok([
            self.directory.identity()?,
            self.package.identity(),
            self.metadata.identity(),
        ])
    }
}

#[cfg(windows)]
fn retire_windows(
    root: &Path,
    saved_path: &Path,
    fingerprint: &PackageFingerprint,
    snapshot: &ProjectSnapshot,
    candidates: &[RecoveryRetirementCandidate],
    report: &mut RecoveryRetirementReport,
    after_delete: &mut impl FnMut(usize) -> Result<(), RecoveryError>,
) -> Result<(), RecoveryError> {
    use publisher_windows_atomic_file::PinnedDirectory;
    let _lock = lock_document(root, snapshot.document.id.as_str())?;
    let saved_directory = PinnedDirectory::for_destination(saved_path)?;
    let saved = saved_directory.pin_file(saved_path)?;
    let opened = crate::package::open_package_file(saved.file()?)?;
    if &opened.fingerprint != fingerprint || &opened.snapshot != snapshot {
        return Err(RecoveryError::InvalidCandidate);
    }
    for token in candidates {
        let result = (|| {
            let candidate = &token.candidate;
            let document = root.join(snapshot.document.id.as_str());
            if candidate.document_id != snapshot.document.id.as_str()
                || candidate.revision > snapshot.revision.value()
                || candidate.package_path.parent().and_then(Path::parent)
                    != Some(document.as_path())
            {
                return Err(RecoveryError::InvalidCandidate);
            }
            let held = HeldCandidate::open(candidate)?;
            if held.identities()? != token.identities || held.package.identity() == saved.identity()
            {
                return Err(RecoveryError::InvalidCandidate);
            }
            // Mark only exact held files for deletion, closing each before asking Windows
            // to remove the exact directory. An extra file causes a safe nonempty failure.
            held.package.remove()?;
            after_delete(1)?;
            held.metadata.remove()?;
            after_delete(2)?;
            held.directory.remove_empty()?;
            after_delete(3)?;
            Ok::<(), RecoveryError>(())
        })();
        report.outcomes.push(RecoveryRetirementOutcome {
            package_path: token.candidate.package_path.clone(),
            retired: result.is_ok(),
            warning: result.err().map(|_| {
                "Publication saved. Some recovery files could not be removed. Close any other app using the recovery folder, then Save again to retry. If the warning remains, open the recovery folder to inspect the copies.".to_owned()
            }),
        });
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn interruption_after_each_deletion_preserves_saved_bytes_and_counts_leftovers() {
        use crate::model::{Document, DocumentId, Insets, PhysicalSize, Revision};
        for interruption in 1..=3 {
            let temp = std::env::temp_dir().join(format!(
                "publisher-retirement-interruption-{}-{interruption}",
                std::process::id()
            ));
            fs::create_dir(&temp).unwrap();
            let root = temp.join("recovery");
            let snapshot = ProjectSnapshot {
                document: Document::new(
                    DocumentId::parse("interruption-fixture").unwrap(),
                    "Retained Save",
                    PhysicalSize::new(215_900, 279_400).unwrap(),
                    Insets::uniform(0).unwrap(),
                )
                .unwrap(),
                revision: Revision::initial(),
            };
            let candidate =
                super::super::save_recovery(&root, 1, &snapshot, &AssetStore::default(), None)
                    .unwrap();
            let token = capture_recovery_retirement(&candidate).unwrap();
            let saved = temp.join("saved.publisher-v2");
            let fingerprint =
                crate::save_new_package(&saved, &snapshot, &AssetStore::default()).unwrap();
            let mut report = RecoveryRetirementReport::default();
            retire_windows(
                &root,
                &saved,
                &fingerprint,
                &snapshot,
                &[token],
                &mut report,
                &mut |step| {
                    if step == interruption {
                        Err(std::io::Error::other("simulated cleanup interruption").into())
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap();
            assert!(!report.outcomes[0].retired);
            assert!(report.outcomes[0].warning.is_some());
            assert_eq!(
                crate::open_package(&saved).unwrap().fingerprint,
                fingerprint
            );
            let mut inventory = Inventory::default();
            inventory
                .document(&root.join(snapshot.document.id.as_str()), true)
                .unwrap();
            assert_eq!(inventory.envelopes, usize::from(interruption < 3));
            assert_eq!(inventory.bytes > 0, interruption == 1);
            // Only this test-created subtree, whose full literal identity was established above.
            fs::remove_dir_all(temp).unwrap();
        }
    }
}
