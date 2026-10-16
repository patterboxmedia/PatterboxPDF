//! Separate validated autosave generations and bounded recovery discovery.

use crate::assets::AssetStore;
use crate::package::{
    MIN_READABLE_SCHEMA_VERSION, OpenedPackage, PackageError, PackageFingerprint, ProjectSnapshot,
    SCHEMA_VERSION, open_legacy_recovery_package, open_package, save_new_package,
};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const RECOVERY_SCHEMA: u32 = 1;
const MAX_RECOVERY_METADATA_BYTES: u64 = 128 * 1024;
const MAX_DISCOVERY_ENTRIES: usize = 1_000;
const MAX_DISCOVERY_PACKAGES: usize = 25;
const DISCARDED_DIRECTORY: &str = ".discarded";
const RECOVERY_PACKAGE_FILE: &str = "publication.publisher-v2";
const RECOVERY_METADATA_FILE: &str = "recovery.json";
static RECOVERY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryWritePoint {
    EnvelopeCreated,
    PackageCompleted,
    MetadataFlushed,
    MetadataCommitted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryCandidate {
    pub package_path: PathBuf,
    pub document_id: String,
    pub document_title: String,
    pub generation: u64,
    pub revision: u64,
    /// Original manual package schema, even when this recovery uses the current writer.
    pub source_schema_version: u32,
    pub base_fingerprint: Option<PackageFingerprint>,
    pub package_fingerprint: PackageFingerprint,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RecoveryMetadata {
    schema_version: u32,
    document_id: String,
    document_title: String,
    generation: u64,
    revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_schema_version: Option<u32>,
    base_fingerprint: Option<String>,
    package_fingerprint: String,
}

/// Writes a new recovery generation without modifying a manual-save destination.
///
/// # Errors
///
/// Returns [`RecoveryError`] when the root is unavailable or the package/metadata cannot be
/// staged, validated, and persisted.
pub fn save_recovery(
    root: &Path,
    generation: u64,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
    base_fingerprint: Option<&PackageFingerprint>,
) -> Result<RecoveryCandidate, RecoveryError> {
    save_recovery_with_observer(
        root,
        generation,
        snapshot,
        assets,
        base_fingerprint,
        SCHEMA_VERSION,
        |_, _| {},
    )
}

/// Writes a recovery while preserving the opened publication's upgrade requirement.
/// Compatibility ancestry is metadata, independent from snapshot semantic identity.
///
/// # Errors
///
/// Returns [`RecoveryError`] for an unreadable source schema or when recovery
/// storage cannot stage and validate the new generation.
pub fn save_recovery_with_source_schema(
    root: &Path,
    generation: u64,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
    base_fingerprint: Option<&PackageFingerprint>,
    source_schema_version: u32,
) -> Result<RecoveryCandidate, RecoveryError> {
    save_recovery_with_observer(
        root,
        generation,
        snapshot,
        assets,
        base_fingerprint,
        source_schema_version,
        |_, _| {},
    )
}

fn save_recovery_with_observer(
    root: &Path,
    generation: u64,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
    base_fingerprint: Option<&PackageFingerprint>,
    source_schema_version: u32,
    mut observe: impl FnMut(&Path, RecoveryWritePoint),
) -> Result<RecoveryCandidate, RecoveryError> {
    if !(MIN_READABLE_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&source_schema_version) {
        return Err(RecoveryError::InvalidCandidate);
    }
    fs::create_dir_all(root)?;
    let document_root = root.join(snapshot.document.id.as_str());
    fs::create_dir_all(&document_root)?;
    let _allowance = retention::admit(root, snapshot, assets)?;
    let sequence = RECOVERY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let envelope_path = document_root.join(format!(
        "generation-{generation:020}-revision-{:020}-{}-{sequence}",
        snapshot.revision.value(),
        std::process::id()
    ));
    fs::create_dir(&envelope_path)?;
    observe(&envelope_path, RecoveryWritePoint::EnvelopeCreated);
    let package_path = envelope_path.join(RECOVERY_PACKAGE_FILE);
    #[cfg(windows)]
    let envelope = publisher_windows_atomic_file::PinnedDirectory::for_destination(&package_path)?;
    let result = (|| {
        let package_fingerprint = save_new_package(&package_path, snapshot, assets)?;
        observe(&envelope_path, RecoveryWritePoint::PackageCompleted);
        let metadata = RecoveryMetadata {
            schema_version: RECOVERY_SCHEMA,
            document_id: snapshot.document.id.as_str().to_owned(),
            document_title: snapshot.document.title.clone(),
            generation,
            revision: snapshot.revision.value(),
            source_schema_version: (source_schema_version != SCHEMA_VERSION)
                .then_some(source_schema_version),
            base_fingerprint: base_fingerprint.map(|fingerprint| fingerprint.as_str().to_owned()),
            package_fingerprint: package_fingerprint.as_str().to_owned(),
        };
        let bytes = serde_json::to_vec_pretty(&metadata)?;
        if bytes.len() as u64 > MAX_RECOVERY_METADATA_BYTES {
            return Err(RecoveryError::MetadataTooLarge);
        }
        let metadata_path = envelope_path.join(RECOVERY_METADATA_FILE);
        #[cfg(windows)]
        let stage = envelope.create_file_for_destination(
            &envelope_path.join("recovery.json.stage"),
            &metadata_path,
        )?;
        #[cfg(windows)]
        let mut file = stage.file()?;
        #[cfg(not(windows))]
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&metadata_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        observe(&envelope_path, RecoveryWritePoint::MetadataFlushed);
        drop(file);
        #[cfg(windows)]
        drop(stage.commit()?);
        observe(&envelope_path, RecoveryWritePoint::MetadataCommitted);

        let candidate = read_candidate(&package_path)?;
        if candidate.generation != generation
            || candidate.package_fingerprint != package_fingerprint
        {
            return Err(RecoveryError::InvalidCandidate);
        }
        Ok(candidate)
    })();
    // A failed write remains counted. Never recursively delete a path that another
    // process could have populated or replaced while recovery was being written.
    result
}

#[path = "recovery_retention.rs"]
mod retention;
pub use retention::{
    RecoveryRetirementCandidate, RecoveryRetirementOutcome, RecoveryRetirementReport,
    capture_recovery_retirement, retire_recoveries,
};

/// One bounded discovery batch. A partial scan never hides validated choices.
#[derive(Debug)]
pub struct RecoveryDiscoveryBatch {
    pub candidates: Vec<RecoveryCandidate>,
    pub has_more: bool,
    pub skipped_entries: usize,
    pub examined_entries: usize,
}

/// Retains directory iterators so continuation does not repeatedly scan earlier entries.
/// Enumeration order is not a promise of recency; sort validated choices separately.
pub struct RecoveryDiscovery {
    documents: Option<fs::ReadDir>,
    packages: Option<fs::ReadDir>,
}

impl RecoveryDiscovery {
    /// Starts discovery without modifying any recovery package.
    ///
    /// # Errors
    /// Returns an error if the root exists but cannot be safely enumerated.
    pub fn open(root: &Path) -> Result<Self, RecoveryError> {
        let documents = match fs::symlink_metadata(root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
            Ok(_) => {
                require_plain_directory(root)?;
                Some(fs::read_dir(root)?)
            }
        };
        Ok(Self {
            documents,
            packages: None,
        })
    }

    /// Checks at most 1,000 directory entries and 25 packages, including damaged entries.
    /// Subsequent calls resume where this batch stopped. Child errors are counted instead
    /// of discarding valid choices; opening a selected copy still revalidates it.
    pub fn next_batch(&mut self) -> RecoveryDiscoveryBatch {
        let mut batch = RecoveryDiscoveryBatch {
            candidates: Vec::new(),
            has_more: false,
            skipped_entries: 0,
            examined_entries: 0,
        };
        let mut examined_packages = 0;
        while batch.examined_entries < MAX_DISCOVERY_ENTRIES
            && examined_packages < MAX_DISCOVERY_PACKAGES
        {
            let is_package = self.packages.is_some();
            let entry = if let Some(packages) = &mut self.packages {
                packages.next()
            } else if let Some(documents) = &mut self.documents {
                documents.next()
            } else {
                break;
            };
            let Some(entry) = entry else {
                if is_package {
                    self.packages = None;
                } else {
                    self.documents = None;
                }
                continue;
            };
            batch.examined_entries += 1;
            let Ok(entry) = entry else {
                batch.skipped_entries += 1;
                continue;
            };
            if !is_package && entry.file_name() == OsStr::new(DISCARDED_DIRECTORY) {
                continue;
            }
            if is_package && entry.file_name() == OsStr::new(".recovery.lock") {
                if !fs::symlink_metadata(entry.path()).is_ok_and(|metadata| {
                    metadata.is_file()
                        && !metadata.file_type().is_symlink()
                        && !is_windows_reparse_point(&metadata)
                        && metadata.len() == 0
                }) {
                    batch.skipped_entries += 1;
                }
                continue;
            }
            // Reject links and Windows reparse directories before descending.
            if require_plain_directory(&entry.path()).is_err() {
                batch.skipped_entries += 1;
                continue;
            }
            if !is_package {
                match fs::read_dir(entry.path()) {
                    Ok(packages) => self.packages = Some(packages),
                    Err(_) => batch.skipped_entries += 1,
                }
                continue;
            }
            examined_packages += 1;
            let storage_path = entry.path();
            let package_path = if storage_path.extension() == Some(OsStr::new("publisher-v2")) {
                storage_path
            } else {
                storage_path.join(RECOVERY_PACKAGE_FILE)
            };
            match read_candidate(&package_path) {
                Ok(candidate) => batch.candidates.push(candidate),
                Err(_) => batch.skipped_entries += 1,
            }
        }
        batch.has_more = self.documents.is_some() || self.packages.is_some();
        batch.candidates.sort_by(|left, right| {
            left.document_id
                .cmp(&right.document_id)
                .then_with(|| right.generation.cmp(&left.generation))
                .then_with(|| right.revision.cmp(&left.revision))
                .then_with(|| left.package_path.cmp(&right.package_path))
        });
        batch
    }
}

/// Returns the first bounded batch of valid recovery copies and explicit scan warnings.
/// Use [`RecoveryDiscovery`] to continue a partial scan.
///
/// # Errors
/// Returns an error only when the recovery root itself cannot be safely enumerated.
pub fn discover_recoveries(root: &Path) -> Result<RecoveryDiscoveryBatch, RecoveryError> {
    Ok(RecoveryDiscovery::open(root)?.next_batch())
}

/// Opens the exact recovery candidate previously returned by discovery.
///
/// Both current archive envelopes and pre-archive recovery directories remain readable here.
/// Metadata, ancestry, and package identity are revalidated against the selected candidate so a
/// self-consistent replacement cannot open under a stale Start-screen label. The returned package
/// is deliberately not associated with a manual-save destination.
///
/// # Errors
///
/// Returns [`RecoveryError`] when the recovery metadata, package, or their shared identity is
/// invalid or changed since discovery.
pub fn open_recovery_package(expected: &RecoveryCandidate) -> Result<OpenedPackage, RecoveryError> {
    let (candidate, opened) = read_candidate_with_package(&expected.package_path)?;
    if candidate != *expected {
        return Err(RecoveryError::InvalidCandidate);
    }
    Ok(opened)
}

/// Quarantines one exact recovery package that was previously returned by discovery.
///
/// The candidate is revalidated immediately before the quarantine move, including its package fingerprint,
/// and must remain a direct grandchild of the supplied recovery root. The exact directory is
/// atomically renamed beneath the non-discoverable `.discarded` area on the same filesystem.
/// No recursive deletion is performed, so the dismissed package remains recoverable on disk.
///
/// # Errors
///
/// Returns [`RecoveryError`] when the path escapes the recovery root, any directory in the
/// recovery ancestry is a symlink/reparse point, the candidate no longer validates exactly, or
/// the same-filesystem quarantine move fails.
pub fn discard_recovery(
    root: &Path,
    candidate: &RecoveryCandidate,
) -> Result<PathBuf, RecoveryError> {
    let package_path = &candidate.package_path;
    let paths = recovery_paths(package_path)?;
    let Some(document_root) = paths.storage_path.parent() else {
        return Err(RecoveryError::InvalidCandidate);
    };
    if document_root.parent() != Some(root) {
        return Err(RecoveryError::InvalidCandidate);
    }
    let _allowance = retention::lock_document(root, &candidate.document_id)?;

    for directory in [root, document_root, paths.storage_path.as_path()] {
        require_plain_directory(directory)?;
    }

    if read_candidate(package_path)? != *candidate {
        return Err(RecoveryError::InvalidCandidate);
    }

    let discarded_root = root.join(DISCARDED_DIRECTORY);
    create_plain_directory(&discarded_root)?;
    let document_name = document_root
        .file_name()
        .ok_or(RecoveryError::InvalidCandidate)?;
    let discarded_document_root = discarded_root.join(document_name);
    create_plain_directory(&discarded_document_root)?;
    let storage_name = paths
        .storage_path
        .file_name()
        .ok_or(RecoveryError::InvalidCandidate)?;
    let destination = discarded_document_root.join(storage_name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(RecoveryError::InvalidCandidate),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    fs::rename(&paths.storage_path, &destination)?;
    let mut expected = candidate.clone();
    expected.package_path = if paths.legacy_directory {
        destination.clone()
    } else {
        destination.join(RECOVERY_PACKAGE_FILE)
    };
    match read_candidate(&expected.package_path) {
        Ok(found) if found == expected => {}
        Ok(_) => {
            let _rollback = fs::rename(&destination, &paths.storage_path);
            return Err(RecoveryError::InvalidCandidate);
        }
        Err(error) => {
            let _rollback = fs::rename(&destination, &paths.storage_path);
            return Err(error);
        }
    }
    Ok(destination)
}

fn create_plain_directory(path: &Path) -> Result<(), RecoveryError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    require_plain_directory(path)
}

fn require_plain_directory(path: &Path) -> Result<(), RecoveryError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || is_windows_reparse_point(&metadata)
    {
        return Err(RecoveryError::InvalidCandidate);
    }
    Ok(())
}

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
const fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn read_candidate(package_path: &Path) -> Result<RecoveryCandidate, RecoveryError> {
    read_candidate_with_package(package_path).map(|(candidate, _)| candidate)
}

fn read_candidate_with_package(
    package_path: &Path,
) -> Result<(RecoveryCandidate, OpenedPackage), RecoveryError> {
    let paths = recovery_paths(package_path)?;
    let metadata_info = fs::symlink_metadata(&paths.metadata_path)?;
    if !metadata_info.file_type().is_file() {
        return Err(RecoveryError::InvalidCandidate);
    }
    if metadata_info.len() > MAX_RECOVERY_METADATA_BYTES {
        return Err(RecoveryError::MetadataTooLarge);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata_info.len()).unwrap_or(0));
    File::open(&paths.metadata_path)?
        .take(MAX_RECOVERY_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECOVERY_METADATA_BYTES {
        return Err(RecoveryError::MetadataTooLarge);
    }
    let metadata: RecoveryMetadata = serde_json::from_slice(&bytes)?;
    if metadata.schema_version != RECOVERY_SCHEMA {
        return Err(RecoveryError::InvalidCandidate);
    }
    let opened = if paths.legacy_directory {
        open_legacy_recovery_package(package_path)?
    } else {
        open_package(package_path)?
    };
    let source_schema_version = metadata
        .source_schema_version
        .unwrap_or(opened.schema_version);
    if !(MIN_READABLE_SCHEMA_VERSION..=opened.schema_version).contains(&source_schema_version) {
        return Err(RecoveryError::InvalidCandidate);
    }
    let package_fingerprint = PackageFingerprint::parse(metadata.package_fingerprint)?;
    let base_fingerprint = metadata
        .base_fingerprint
        .map(PackageFingerprint::parse)
        .transpose()?;
    if opened.snapshot.document.id.as_str() != metadata.document_id
        || opened.snapshot.document.title != metadata.document_title
        || opened.snapshot.revision.value() != metadata.revision
        || opened.fingerprint != package_fingerprint
    {
        return Err(RecoveryError::InvalidCandidate);
    }
    Ok((
        RecoveryCandidate {
            package_path: package_path.to_path_buf(),
            document_id: metadata.document_id,
            document_title: metadata.document_title,
            generation: metadata.generation,
            revision: metadata.revision,
            source_schema_version,
            base_fingerprint,
            package_fingerprint,
        },
        opened,
    ))
}

struct RecoveryPaths {
    storage_path: PathBuf,
    metadata_path: PathBuf,
    legacy_directory: bool,
}

fn recovery_paths(package_path: &Path) -> Result<RecoveryPaths, RecoveryError> {
    if package_path.file_name() == Some(OsStr::new(RECOVERY_PACKAGE_FILE)) {
        let storage_path = package_path
            .parent()
            .ok_or(RecoveryError::InvalidCandidate)?
            .to_path_buf();
        return Ok(RecoveryPaths {
            metadata_path: storage_path.join(RECOVERY_METADATA_FILE),
            storage_path,
            legacy_directory: false,
        });
    }
    if package_path.extension() == Some(OsStr::new("publisher-v2")) {
        return Ok(RecoveryPaths {
            metadata_path: package_path.join(RECOVERY_METADATA_FILE),
            storage_path: package_path.to_path_buf(),
            legacy_directory: true,
        });
    }
    Err(RecoveryError::InvalidCandidate)
}

#[derive(Debug)]
pub enum RecoveryError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Package(PackageError),
    MetadataTooLarge,
    InvalidCandidate,
    StoragePaused,
}

impl From<std::io::Error> for RecoveryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for RecoveryError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<PackageError> for RecoveryError {
    fn from(error: PackageError) -> Self {
        Self::Package(error)
    }
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if matches!(self, Self::StoragePaused) {
            return formatter.write_str("Automatic recovery is paused: its storage allowance is full or could not be checked safely. Save your publication manually and inspect the recovery folder.");
        }
        write!(formatter, "recovery operation failed: {self:?}")
    }
}

impl std::error::Error for RecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Package(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(all(test, windows))]
mod process_exit_tests {
    use super::{
        RecoveryWritePoint, discover_recoveries, open_recovery_package, save_recovery,
        save_recovery_with_observer,
    };
    use crate::assets::AssetStore;
    use crate::commands::{Command, DocumentSession, Transaction};
    use crate::model::{Document, DocumentId, Insets, PhysicalSize, Revision};
    use crate::package::ProjectSnapshot;
    use std::fs;
    use std::io::Write;
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    const CHILD_EXIT_CODE: i32 = 92;

    #[test]
    fn automatic_recovery_process_exit_preserves_prior_work_and_allows_retry() {
        for point in [
            "envelope",
            "package",
            "metadata_flushed",
            "metadata_committed",
        ] {
            run_exit_case(point);
        }
    }

    fn run_exit_case(point: &str) {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let fixture = std::env::temp_dir().join(format!(
            "publisher-recovery-exit-{}-{sequence}",
            std::process::id()
        ));
        let recovery_root = fixture.join("recovery");
        let marker = fixture.join("reached.json");
        fs::create_dir(&fixture).expect("create disposable recovery fixture");

        let prior_snapshot = recovery_exit_snapshot(false);
        let revised_snapshot = recovery_exit_snapshot(true);
        let prior = save_recovery(
            &recovery_root,
            1,
            &prior_snapshot,
            &AssetStore::default(),
            None,
        )
        .expect("write prior complete recovery");
        let envelope = spawn_exit_child(&recovery_root, &marker, &prior_snapshot, point);

        assert_interrupted_discovery(
            &recovery_root,
            &prior,
            &prior_snapshot,
            &revised_snapshot,
            point,
        );
        assert_interrupted_layout(point, &envelope);
        assert_retry(&recovery_root, &envelope, &revised_snapshot, point);
        fs::remove_dir_all(&fixture).expect("remove disposable recovery fixture");
    }

    fn spawn_exit_child(
        recovery_root: &Path,
        marker: &Path,
        prior_snapshot: &ProjectSnapshot,
        point: &str,
    ) -> PathBuf {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "recovery::process_exit_tests::automatic_recovery_exit_child",
            ])
            .env("PUBLISHER_RECOVERY_EXIT_ROOT", recovery_root)
            .env("PUBLISHER_RECOVERY_EXIT_POINT", point)
            .env("PUBLISHER_RECOVERY_EXIT_MARKER", marker)
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW for the test-only child.
            .output()
            .expect("run automatic-recovery exit child");
        assert_eq!(
            output.status.code(),
            Some(CHILD_EXIT_CODE),
            "{point}: {output:?}"
        );
        let receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(marker).expect("read flushed child marker"))
                .expect("parse child marker");
        assert_eq!(receipt["point"], point);
        let envelope = PathBuf::from(
            receipt["envelope"]
                .as_str()
                .expect("marker contains envelope path"),
        );
        let document_root = recovery_root.join(prior_snapshot.document.id.as_str());
        assert_eq!(envelope.parent(), Some(document_root.as_path()));
        assert!(
            envelope
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("generation-")),
            "child marker must identify one direct generated envelope"
        );
        envelope
    }

    fn assert_interrupted_discovery(
        recovery_root: &Path,
        prior: &super::RecoveryCandidate,
        prior_snapshot: &ProjectSnapshot,
        revised_snapshot: &ProjectSnapshot,
        point: &str,
    ) {
        let interrupted =
            discover_recoveries(recovery_root).expect("discover after abrupt recovery exit");
        let generations = interrupted
            .candidates
            .iter()
            .map(|candidate| candidate.generation)
            .collect::<Vec<_>>();
        if point == "metadata_committed" {
            assert_eq!(generations, vec![2, 1]);
            assert_eq!(interrupted.skipped_entries, 0);
            assert_eq!(
                open_recovery_package(&interrupted.candidates[0])
                    .expect("open fully committed child recovery")
                    .snapshot,
                *revised_snapshot
            );
        } else {
            assert_eq!(generations, vec![1]);
            assert_eq!(interrupted.skipped_entries, 1);
        }
        assert_eq!(
            open_recovery_package(prior)
                .expect("prior recovery remains openable")
                .snapshot,
            *prior_snapshot
        );
    }

    fn assert_retry(
        recovery_root: &Path,
        envelope: &Path,
        revised_snapshot: &ProjectSnapshot,
        point: &str,
    ) {
        let retry = save_recovery(
            recovery_root,
            3,
            revised_snapshot,
            &AssetStore::default(),
            None,
        )
        .expect("retry automatic recovery");
        assert_eq!(
            open_recovery_package(&retry)
                .expect("retry recovery opens")
                .snapshot,
            *revised_snapshot
        );
        assert!(envelope.exists(), "retry must not erase crash evidence");
        let final_batch = discover_recoveries(recovery_root)
            .expect("discover completed retry with retained evidence");
        let expected = if point == "metadata_committed" {
            vec![3, 2, 1]
        } else {
            vec![3, 1]
        };
        assert_eq!(
            final_batch
                .candidates
                .iter()
                .map(|candidate| candidate.generation)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            final_batch.skipped_entries,
            usize::from(point != "metadata_committed")
        );
    }

    #[test]
    #[ignore = "spawned by automatic_recovery_process_exit_preserves_prior_work_and_allows_retry"]
    fn automatic_recovery_exit_child() {
        let recovery_root =
            PathBuf::from(std::env::var_os("PUBLISHER_RECOVERY_EXIT_ROOT").expect("recovery root"));
        let requested = std::env::var("PUBLISHER_RECOVERY_EXIT_POINT").expect("exit point");
        let marker =
            PathBuf::from(std::env::var_os("PUBLISHER_RECOVERY_EXIT_MARKER").expect("marker path"));
        save_recovery_with_observer(
            &recovery_root,
            2,
            &recovery_exit_snapshot(true),
            &AssetStore::default(),
            None,
            crate::package::SCHEMA_VERSION,
            |envelope, point| {
                if point_name(point) != requested {
                    return;
                }
                let bytes = serde_json::to_vec(&serde_json::json!({
                    "point": requested,
                    "envelope": envelope,
                }))
                .expect("serialize child marker");
                let mut receipt = fs::File::create(&marker).expect("create child marker");
                receipt.write_all(&bytes).expect("write child marker");
                receipt.sync_all().expect("flush child marker");
                std::process::exit(CHILD_EXIT_CODE);
            },
        )
        .expect("child must exit at the requested recovery boundary");
        panic!("automatic recovery child did not exit at {requested}");
    }

    fn recovery_exit_snapshot(revised: bool) -> ProjectSnapshot {
        let document = Document::new(
            DocumentId::parse("automatic-recovery-exit").unwrap(),
            "Automatic recovery interrupted",
            PhysicalSize::new(215_900, 279_400).unwrap(),
            Insets::uniform(3_175).unwrap(),
        )
        .unwrap();
        let mut session = DocumentSession::new(document).unwrap();
        if revised {
            session
                .execute(
                    Revision::initial(),
                    Transaction::single(
                        "Add recovered page",
                        Command::AddPage {
                            after: None,
                            trim: PhysicalSize::new(215_900, 279_400).unwrap(),
                            bleed: Insets::uniform(3_175).unwrap(),
                        },
                    ),
                )
                .unwrap();
        }
        ProjectSnapshot::from_session(&session)
    }

    fn point_name(point: RecoveryWritePoint) -> &'static str {
        match point {
            RecoveryWritePoint::EnvelopeCreated => "envelope",
            RecoveryWritePoint::PackageCompleted => "package",
            RecoveryWritePoint::MetadataFlushed => "metadata_flushed",
            RecoveryWritePoint::MetadataCommitted => "metadata_committed",
        }
    }

    fn assert_interrupted_layout(point: &str, envelope: &Path) {
        let package = envelope.join("publication.publisher-v2");
        let metadata_stage = envelope.join("recovery.json.stage");
        let metadata = envelope.join("recovery.json");
        match point {
            "envelope" => {
                assert!(!package.exists());
                assert!(!metadata_stage.exists());
                assert!(!metadata.exists());
            }
            "package" => {
                assert!(package.exists());
                assert!(!metadata_stage.exists());
                assert!(!metadata.exists());
            }
            "metadata_flushed" => {
                assert!(package.exists());
                assert!(metadata_stage.exists());
                assert!(!metadata.exists());
            }
            "metadata_committed" => {
                assert!(package.exists());
                assert!(!metadata_stage.exists());
                assert!(metadata.exists());
            }
            _ => panic!("unexpected recovery boundary {point}"),
        }
    }
}
