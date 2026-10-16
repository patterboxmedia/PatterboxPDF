//! Same-directory staged commit for an already authorized publication PDF.

#[cfg(windows)]
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::io::{Seek, SeekFrom};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{ArtifactHash, PublicationAuthorization};

const MAX_EXPORT_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(windows)]
const MAX_EXPORT_JOURNAL_BYTES: u64 = 8 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationExpectation {
    /// Save a Copy: refuse when any entry already occupies the destination.
    CreateNew,
    /// Replace only the exact prior regular-file generation the caller observed.
    ReplaceExact(ArtifactHash),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportCommitOutcome {
    Created,
    Replaced,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportCommitError {
    UnauthorizedBytes,
    InvalidDestination,
    DestinationAlreadyExists,
    DestinationMissing,
    DestinationNotRegularFile,
    DestinationChanged,
    ResourceLimit,
    StageUnavailable(String),
    StageWriteFailed(String),
    StageIdentityDrift,
    CommitFailed(String),
    FinalIdentityDrift,
    InterruptedExportPresent(PathBuf),
    RecoveryFailed {
        backup_path: PathBuf,
        detail: String,
    },
}

/// Bounded observation made after an export operation releases its handles.
/// This is a snapshot, not a promise of durability or continued ownership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportArtifactState {
    Missing,
    RegularFile(ArtifactHash),
    NotRegularFile,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportArtifactObservation {
    pub path: PathBuf,
    pub state: ExportArtifactState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportFailureState {
    pub destination: ExportArtifactObservation,
    /// None means this operation did not establish the artifact path.
    pub backup: Option<ExportArtifactObservation>,
    pub journal: Option<ExportArtifactObservation>,
    pub receipt: Option<ExportArtifactObservation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportCommitFailure {
    pub error: ExportCommitError,
    pub state: Box<ExportFailureState>,
}

#[derive(Default)]
struct ExportPaths {
    backup: Option<PathBuf>,
    journal: Option<PathBuf>,
    receipt: Option<PathBuf>,
}

/// Commits authorized bytes and reports observed artifacts if the operation fails.
///
/// # Errors
///
/// Returns the original error and a bounded filesystem snapshot. Unavailable
/// observations must never be interpreted as absent or preserved output.
pub fn commit_authorized_pdf_with_report(
    authorization: &PublicationAuthorization,
    candidate_bytes: &[u8],
    destination: &Path,
    expectation: DestinationExpectation,
) -> Result<ExportCommitOutcome, ExportCommitFailure> {
    commit_with_report_inner(
        authorization,
        candidate_bytes,
        destination,
        expectation,
        FaultPoint::None,
    )
}

fn commit_with_report_inner(
    authorization: &PublicationAuthorization,
    candidate_bytes: &[u8],
    destination: &Path,
    expectation: DestinationExpectation,
    fault: FaultPoint,
) -> Result<ExportCommitOutcome, ExportCommitFailure> {
    let mut paths = ExportPaths::default();
    commit_operation(
        authorization,
        candidate_bytes,
        destination,
        expectation,
        fault,
        &mut paths,
    )
    .map_err(|error| ExportCommitFailure {
        error,
        state: Box::new(ExportFailureState {
            destination: observe_export_artifact(destination),
            backup: paths.backup.as_deref().map(observe_export_artifact),
            journal: paths.journal.as_deref().map(observe_export_artifact),
            receipt: paths.receipt.as_deref().map(observe_export_artifact),
        }),
    })
}

fn observe_export_artifact(path: &Path) -> ExportArtifactObservation {
    let state = match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ExportArtifactState::Missing,
        Err(_) => ExportArtifactState::Unavailable,
        Ok(metadata) if !metadata.file_type().is_file() => ExportArtifactState::NotRegularFile,
        Ok(_) => observe_regular_export_artifact(path),
    };
    ExportArtifactObservation {
        path: path.to_path_buf(),
        state,
    }
}

#[cfg(windows)]
fn observe_regular_export_artifact(path: &Path) -> ExportArtifactState {
    // The existing boundary rejects redirected parents and pins the exact regular
    // file against writes/renames while the bounded hash is read.
    let observed = (|| {
        let directory =
            publisher_windows_atomic_file::PinnedDirectory::for_destination(path).ok()?;
        let pinned = directory.pin_file(path).ok()?;
        hash_file_handle(pinned.file().ok()?).ok()
    })();
    observed.map_or(
        ExportArtifactState::Unavailable,
        ExportArtifactState::RegularFile,
    )
}

#[cfg(not(windows))]
fn observe_regular_export_artifact(_path: &Path) -> ExportArtifactState {
    // No equivalent nonfollowing, pinned observation boundary is supported here.
    ExportArtifactState::Unavailable
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptedExportState {
    Prepared,
    PriorPreserved,
    Committed,
    Ambiguous,
}

#[cfg(windows)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterruptedExportStatus {
    pub state: InterruptedExportState,
    pub prior_hash: ArtifactHash,
    pub replacement_hash: ArtifactHash,
    pub backup_path: PathBuf,
    pub stage_path: PathBuf,
    pub receipt_path: PathBuf,
    pub journal_hash: ArtifactHash,
    identities: recovery::RecoveryIdentities,
}

#[cfg(windows)]
mod recovery;
#[cfg(windows)]
pub use recovery::{
    InterruptedExportRecovery, InterruptedExportRecoveryOutcome, inspect_interrupted_export,
    recover_interrupted_export,
};

#[cfg(windows)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ExportJournal {
    schema_version: u32,
    operation_id: String,
    destination_name: String,
    stage_name: String,
    backup_name: String,
    failed_name: String,
    receipt_name: String,
    prior_hash: String,
    replacement_hash: String,
}

#[cfg(windows)]
impl ExportJournal {
    fn validate_for(&self, destination: &Path) -> Result<(), ExportCommitError> {
        let parent = destination
            .parent()
            .ok_or(ExportCommitError::InvalidDestination)?;
        let operation = &self.operation_id;
        let expected_failed = format!(".publisher-export-{operation}.failed");
        let expected_receipt = format!(".publisher-export-{operation}.completed.json");
        if !recovery::unique_names(self, destination)?
            || self.schema_version != 1
            || !is_lower_hex(&self.operation_id)
            || self.destination_name != file_name_string(destination)?
            || !is_safe_sibling_name(&self.stage_name)
            || !recovery::role_name(&self.stage_name, &self.destination_name, "stage")
            || !is_safe_sibling_name(&self.backup_name)
            || !recovery::role_name(&self.backup_name, &self.destination_name, "prior")
            || self.failed_name != expected_failed
            || self.receipt_name != expected_receipt
            || parse_artifact_hash(&self.prior_hash).is_none()
            || parse_artifact_hash(&self.replacement_hash).is_none()
            || [
                &self.stage_name,
                &self.backup_name,
                &self.failed_name,
                &self.receipt_name,
            ]
            .iter()
            .any(|name| parent.join(name).parent() != Some(parent))
        {
            return Err(ExportCommitError::InvalidDestination);
        }
        Ok(())
    }
}

/// Commits exact authorized bytes through a same-directory, create-new stage.
///
/// Existing output is replaced only when its exact observed fingerprint still
/// matches. On Windows, replacement is a durably journaled, handle-bound,
/// no-clobber transition; retained artifacts remain intentionally available
/// for classification because directory-metadata power-loss durability is not claimed.
///
/// # Errors
///
/// Returns a typed conflict, identity, resource, staging, or commit error.
pub fn commit_authorized_pdf(
    authorization: &PublicationAuthorization,
    candidate_bytes: &[u8],
    destination: &Path,
    expectation: DestinationExpectation,
) -> Result<ExportCommitOutcome, ExportCommitError> {
    commit_authorized_pdf_inner(
        authorization,
        candidate_bytes,
        destination,
        expectation,
        FaultPoint::None,
    )
}

fn commit_authorized_pdf_inner(
    authorization: &PublicationAuthorization,
    candidate_bytes: &[u8],
    destination: &Path,
    expectation: DestinationExpectation,
    fault: FaultPoint,
) -> Result<ExportCommitOutcome, ExportCommitError> {
    commit_operation(
        authorization,
        candidate_bytes,
        destination,
        expectation,
        fault,
        &mut ExportPaths::default(),
    )
}

fn commit_operation(
    authorization: &PublicationAuthorization,
    candidate_bytes: &[u8],
    destination: &Path,
    expectation: DestinationExpectation,
    fault: FaultPoint,
    paths: &mut ExportPaths,
) -> Result<ExportCommitOutcome, ExportCommitError> {
    if candidate_bytes.is_empty()
        || candidate_bytes.len() as u64 > MAX_EXPORT_BYTES
        || ArtifactHash::of(candidate_bytes) != authorization.artifact_hash()
    {
        return Err(ExportCommitError::UnauthorizedBytes);
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(ExportCommitError::InvalidDestination)?;
    let file_name = destination
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or(ExportCommitError::InvalidDestination)?;
    if !parent.is_dir() {
        return Err(ExportCommitError::InvalidDestination);
    }
    #[cfg(windows)]
    {
        paths.journal = Some(export_journal_path(destination)?);
        refuse_active_export_journal(destination)?;
    }
    verify_destination(destination, expectation)?;
    #[cfg(windows)]
    let directory = publisher_windows_atomic_file::PinnedDirectory::for_destination(destination)
        .map_err(|error| ExportCommitError::StageUnavailable(error.to_string()))?;
    let stage = StageFile::create(
        parent,
        file_name,
        #[cfg(windows)]
        &directory,
    )?;
    stage.write_and_verify(candidate_bytes, authorization.artifact_hash())?;
    if fault == FaultPoint::AfterStageVerification {
        return Err(ExportCommitError::CommitFailed(
            "injected failure after stage verification".into(),
        ));
    }
    verify_destination(destination, expectation)?;
    if fault == FaultPoint::AfterDestinationRecheck {
        return Err(ExportCommitError::CommitFailed(
            "injected failure after destination recheck".into(),
        ));
    }
    if fault == FaultPoint::ChangeDestinationAfterRecheck {
        std::fs::write(destination, b"external generation after recheck")
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    }

    let outcome = match expectation {
        DestinationExpectation::CreateNew => {
            #[cfg(windows)]
            stage.commit_new()?;
            #[cfg(not(windows))]
            {
                std::fs::hard_link(&stage.path, destination).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        ExportCommitError::DestinationAlreadyExists
                    } else {
                        ExportCommitError::CommitFailed(error.to_string())
                    }
                })?;
                stage.remove_after_link()?;
            }
            ExportCommitOutcome::Created
        }
        DestinationExpectation::ReplaceExact(expected_prior) => commit_replacement(
            authorization,
            destination,
            parent,
            file_name,
            &stage,
            expected_prior,
            fault,
            paths,
        )?,
    };
    sync_parent(parent);
    if hash_regular_file(destination)? != authorization.artifact_hash() {
        return Err(ExportCommitError::FinalIdentityDrift);
    }
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
fn commit_replacement(
    authorization: &PublicationAuthorization,
    destination: &Path,
    parent: &Path,
    file_name: &std::ffi::OsStr,
    stage: &StageFile,
    expected_prior: ArtifactHash,
    fault: FaultPoint,
    paths: &mut ExportPaths,
) -> Result<ExportCommitOutcome, ExportCommitError> {
    #[cfg(windows)]
    return commit_replacement_windows(
        authorization,
        destination,
        parent,
        file_name,
        stage,
        expected_prior,
        fault,
        paths,
    );
    #[cfg(not(windows))]
    return commit_replacement_non_windows(
        authorization,
        destination,
        parent,
        file_name,
        stage,
        expected_prior,
        fault,
        paths,
    );
}

#[cfg(not(windows))]
#[allow(clippy::too_many_arguments)]
fn commit_replacement_non_windows(
    authorization: &PublicationAuthorization,
    destination: &Path,
    parent: &Path,
    file_name: &std::ffi::OsStr,
    stage: &StageFile,
    expected_prior: ArtifactHash,
    fault: FaultPoint,
    paths: &mut ExportPaths,
) -> Result<ExportCommitOutcome, ExportCommitError> {
    let backup = BackupFile::allocate(parent, file_name)?;
    paths.backup = Some(backup.path.clone());
    if let Err(error) = atomic_replace_with_backup(destination, &stage.path, &backup.path) {
        let failure = ExportCommitError::CommitFailed(error.to_string());
        if backup.path.exists() {
            if hash_regular_file(destination).ok() == Some(expected_prior) {
                backup.remove_after_success()?;
                return Err(failure);
            }
            return Err(restore_or_retain(destination, &backup, failure));
        }
        return Err(failure);
    }
    stage.disarm();
    sync_parent(parent);
    if hash_regular_file(&backup.path).ok() != Some(expected_prior) {
        return Err(restore_or_retain(
            destination,
            &backup,
            ExportCommitError::DestinationChanged,
        ));
    }
    inject_post_replace_fault(destination, fault)?;
    if hash_regular_file(destination).ok() != Some(authorization.artifact_hash()) {
        return Err(restore_or_retain(
            destination,
            &backup,
            ExportCommitError::FinalIdentityDrift,
        ));
    }
    backup.remove_after_success()?;
    Ok(ExportCommitOutcome::Replaced)
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn commit_replacement_windows(
    authorization: &PublicationAuthorization,
    destination: &Path,
    parent: &Path,
    file_name: &std::ffi::OsStr,
    stage: &StageFile,
    expected_prior: ArtifactHash,
    fault: FaultPoint,
    paths: &mut ExportPaths,
) -> Result<ExportCommitOutcome, ExportCommitError> {
    let backup = BackupFile::allocate(parent, file_name)?;
    paths.backup = Some(backup.path.clone());
    let operation_id = export_operation_id()?;
    let failed = parent.join(format!(".publisher-export-{operation_id}.failed"));
    let receipt = parent.join(format!(".publisher-export-{operation_id}.completed.json"));
    paths.receipt = Some(receipt.clone());
    let journal_path = export_journal_path(destination)?;
    if journal_path.exists() {
        return Err(ExportCommitError::InterruptedExportPresent(journal_path));
    }
    let journal = ExportJournal {
        schema_version: 1,
        operation_id,
        destination_name: file_name.to_string_lossy().into_owned(),
        stage_name: file_name_string(&stage.path)?,
        backup_name: file_name_string(&backup.path)?,
        failed_name: file_name_string(&failed)?,
        receipt_name: file_name_string(&receipt)?,
        prior_hash: expected_prior.hex(),
        replacement_hash: authorization.artifact_hash().hex(),
    };
    write_export_journal(&journal_path, &journal)?;

    let prepared_receipt =
        prepare_export_receipt(stage.directory, &journal_path, &receipt, &journal)?;
    let prepared = stage
        .prepare_replace(&backup.path)
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    if hash_file_handle(
        prepared
            .destination_file()
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?,
    )? != expected_prior
        || hash_file_handle(
            prepared
                .staged_file()
                .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?,
        )? != authorization.artifact_hash()
    {
        return Err(ExportCommitError::DestinationChanged);
    }
    #[cfg(test)]
    abort_export_if(fault, FaultPoint::AbortPrepared, "prepared");
    if fault == FaultPoint::OccupyBackupBeforePreserve {
        std::fs::write(&backup.path, b"independent backup occupant")
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    }
    let preserved = prepared
        .preserve_prior()
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    #[cfg(test)]
    if fault == FaultPoint::AttemptMarkerMutationAfterPriorPreserved {
        assert_marker_is_pinned(&journal_path)?;
    }
    #[cfg(test)]
    abort_export_if(
        fault,
        FaultPoint::AbortAfterPriorPreserved,
        "prior_preserved",
    );
    let committed = preserved
        .commit()
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    stage.disarm();
    if hash_file_handle(
        committed
            .destination_file()
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?,
    )? != authorization.artifact_hash()
        || hash_file_handle(
            committed
                .prior_file()
                .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?,
        )? != expected_prior
    {
        return Err(ExportCommitError::FinalIdentityDrift);
    }
    drop(committed);
    sync_parent(parent);
    #[cfg(test)]
    abort_export_if(fault, FaultPoint::AbortCommitted, "committed");
    if fault == FaultPoint::AfterInstallBeforeReceipt {
        return Err(ExportCommitError::CommitFailed(
            "injected failure after install".into(),
        ));
    }
    inject_post_replace_fault(destination, fault)?;
    if hash_regular_file(destination).ok() != Some(authorization.artifact_hash()) {
        return Err(restore_journaled_export(
            destination,
            &backup,
            &failed,
            prepared_receipt,
            expected_prior,
            ExportCommitError::FinalIdentityDrift,
        ));
    }
    finalize_export_journal(prepared_receipt)?;
    Ok(ExportCommitOutcome::Replaced)
}

fn inject_post_replace_fault(
    destination: &Path,
    fault: FaultPoint,
) -> Result<(), ExportCommitError> {
    if fault == FaultPoint::CorruptAfterReplacement
        || fault == FaultPoint::BlockRestoreAfterReplacement
    {
        std::fs::write(destination, b"injected post-replace corruption")
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    }
    if fault == FaultPoint::BlockRestoreAfterReplacement {
        std::fs::remove_file(destination)
            .and_then(|()| std::fs::create_dir(destination))
            .and_then(|()| std::fs::write(destination.join("occupied"), b"fault"))
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn restore_or_retain(
    destination: &Path,
    backup: &BackupFile,
    restored_error: ExportCommitError,
) -> ExportCommitError {
    match restore_backup(destination, &backup.path) {
        Ok(()) => {
            sync_parent(destination.parent().unwrap_or_else(|| Path::new(".")));
            restored_error
        }
        Err(error) => ExportCommitError::RecoveryFailed {
            backup_path: backup.path.clone(),
            detail: error.to_string(),
        },
    }
}

#[cfg(windows)]
fn restore_journaled_export(
    destination: &Path,
    backup: &BackupFile,
    failed: &Path,
    prepared_receipt: publisher_windows_atomic_file::PreparedMove<'_>,
    expected_prior: ArtifactHash,
    restored_error: ExportCommitError,
) -> ExportCommitError {
    let result = (|| -> Result<(), ExportCommitError> {
        let directory =
            publisher_windows_atomic_file::PinnedDirectory::for_destination(destination)
                .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
        let prepared = directory
            .prepare_replace(destination, &backup.path, failed)
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
        if hash_file_handle(
            prepared
                .staged_file()
                .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?,
        )? != expected_prior
        {
            return Err(ExportCommitError::DestinationChanged);
        }
        let committed = prepared
            .preserve_prior()
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?
            .commit()
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
        if hash_file_handle(
            committed
                .destination_file()
                .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?,
        )? != expected_prior
        {
            return Err(ExportCommitError::FinalIdentityDrift);
        }
        drop(committed);
        finalize_export_journal(prepared_receipt)
    })();
    match result {
        Ok(()) => restored_error,
        Err(error) => ExportCommitError::RecoveryFailed {
            backup_path: backup.path.clone(),
            detail: format!("{error:?}"),
        },
    }
}

#[cfg(windows)]
fn export_operation_id() -> Result<String, ExportCommitError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| ExportCommitError::StageUnavailable(error.to_string()))?;
    Ok(ArtifactHash::from_bytes(bytes).hex())
}

#[cfg(windows)]
fn export_journal_path(destination: &Path) -> Result<PathBuf, ExportCommitError> {
    let parent = destination
        .parent()
        .ok_or(ExportCommitError::InvalidDestination)?;
    let name = file_name_string(destination)?;
    Ok(parent.join(format!(".{name}.publisher-export-journal.json")))
}

#[cfg(windows)]
fn file_name_string(path: &Path) -> Result<String, ExportCommitError> {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .ok_or(ExportCommitError::InvalidDestination)
}

#[cfg(windows)]
fn is_safe_sibling_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':', '\0'])
        && !name.ends_with(['.', ' '])
}

#[cfg(windows)]
fn is_lower_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(windows)]
fn parse_artifact_hash(value: &str) -> Option<ArtifactHash> {
    if !is_lower_hex(value) {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let pair = &value.as_bytes()[index * 2..index * 2 + 2];
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        *byte = u8::try_from((high << 4) | low).ok()?;
    }
    Some(ArtifactHash::from_bytes(bytes))
}

#[cfg(windows)]
fn write_export_journal(path: &Path, journal: &ExportJournal) -> Result<(), ExportCommitError> {
    let bytes = serde_json::to_vec_pretty(journal)
        .map_err(|error| ExportCommitError::StageWriteFailed(error.to_string()))?;
    if bytes.len() as u64 > MAX_EXPORT_JOURNAL_BYTES {
        return Err(ExportCommitError::ResourceLimit);
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| ExportCommitError::StageWriteFailed(error.to_string()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| ExportCommitError::StageWriteFailed(error.to_string()))?;
    Ok(())
}

#[cfg(windows)]
fn prepare_export_receipt<'a>(
    directory: &'a publisher_windows_atomic_file::PinnedDirectory,
    journal_path: &Path,
    receipt: &Path,
    journal: &ExportJournal,
) -> Result<publisher_windows_atomic_file::PreparedMove<'a>, ExportCommitError> {
    let prepared = directory
        .prepare_move_file_no_replace(journal_path, receipt)
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    let source = prepared
        .source_file()
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    let mut bytes = Vec::new();
    source
        .take(MAX_EXPORT_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    let exact: ExportJournal = serde_json::from_slice(&bytes)
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    if exact != *journal {
        return Err(ExportCommitError::FinalIdentityDrift);
    }
    Ok(prepared)
}

#[cfg(windows)]
fn finalize_export_journal(
    prepared: publisher_windows_atomic_file::PreparedMove<'_>,
) -> Result<(), ExportCommitError> {
    let moved = prepared
        .commit()
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    moved
        .reopened_destination_file()
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    Ok(())
}

#[cfg(windows)]
fn refuse_active_export_journal(destination: &Path) -> Result<(), ExportCommitError> {
    let journal = export_journal_path(destination)?;
    match journal.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(ExportCommitError::InterruptedExportPresent(journal)),
        Err(error) => Err(ExportCommitError::CommitFailed(error.to_string())),
    }
}

#[cfg(all(windows, test))]
fn assert_marker_is_pinned(journal_path: &Path) -> Result<(), ExportCommitError> {
    let renamed = journal_path.with_extension("attacker-renamed");
    if std::fs::rename(journal_path, &renamed).is_ok()
        || std::fs::remove_file(journal_path).is_ok()
        || OpenOptions::new().write(true).open(journal_path).is_ok()
    {
        return Err(ExportCommitError::CommitFailed(
            "the active export journal was not pinned against mutation".into(),
        ));
    }
    Ok(())
}

#[cfg(all(windows, test))]
fn write_export_abort_marker(state: &str) {
    let marker =
        std::env::var_os("PUBLISHER_EXPORT_ABORT_MARKER").expect("child abort marker environment");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .expect("create export abort marker");
    file.write_all(state.as_bytes())
        .expect("write export abort marker");
    file.sync_all().expect("sync export abort marker");
}

#[cfg(all(windows, test))]
fn abort_export_if(fault: FaultPoint, expected: FaultPoint, state: &str) {
    if fault == expected {
        write_export_abort_marker(state);
        std::process::abort();
    }
}

#[cfg(windows)]
fn hash_file_handle(mut file: File) -> Result<ArtifactHash, ExportCommitError> {
    let metadata = file
        .metadata()
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_EXPORT_BYTES {
        return Err(ExportCommitError::ResourceLimit);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(MAX_EXPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    if bytes.len() as u64 > MAX_EXPORT_BYTES {
        return Err(ExportCommitError::ResourceLimit);
    }
    Ok(ArtifactHash::of(&bytes))
}

fn verify_destination(
    destination: &Path,
    expectation: DestinationExpectation,
) -> Result<(), ExportCommitError> {
    match expectation {
        DestinationExpectation::CreateNew => match destination.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(ExportCommitError::DestinationAlreadyExists),
            Err(error) => Err(ExportCommitError::CommitFailed(error.to_string())),
        },
        DestinationExpectation::ReplaceExact(expected) => {
            let actual = hash_regular_file(destination)?;
            if actual != expected {
                return Err(ExportCommitError::DestinationChanged);
            }
            Ok(())
        }
    }
}

fn hash_regular_file(path: &Path) -> Result<ArtifactHash, ExportCommitError> {
    let metadata = path.symlink_metadata().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ExportCommitError::DestinationMissing
        } else {
            ExportCommitError::CommitFailed(error.to_string())
        }
    })?;
    if !metadata.file_type().is_file() {
        return Err(ExportCommitError::DestinationNotRegularFile);
    }
    if metadata.len() > MAX_EXPORT_BYTES {
        return Err(ExportCommitError::ResourceLimit);
    }
    let capacity = usize::try_from(metadata.len()).map_err(|_| ExportCommitError::ResourceLimit)?;
    let mut bytes = Vec::with_capacity(capacity);
    File::open(path)
        .and_then(|file| file.take(MAX_EXPORT_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    if bytes.len() as u64 > MAX_EXPORT_BYTES {
        return Err(ExportCommitError::ResourceLimit);
    }
    Ok(ArtifactHash::of(&bytes))
}

#[cfg(windows)]
struct StageFile<'a> {
    path: PathBuf,
    directory: &'a publisher_windows_atomic_file::PinnedDirectory,
    created: std::cell::RefCell<Option<publisher_windows_atomic_file::CreatedFile<'a>>>,
}

#[cfg(windows)]
impl<'a> StageFile<'a> {
    fn create(
        parent: &Path,
        file_name: &std::ffi::OsStr,
        directory: &'a publisher_windows_atomic_file::PinnedDirectory,
    ) -> Result<Self, ExportCommitError> {
        let destination = parent.join(file_name);
        for attempt in 0..16_u32 {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| ExportCommitError::StageUnavailable(error.to_string()))?
                .as_nanos();
            let path = parent.join(format!(
                ".{}.publisher-stage-{}-{nonce}-{attempt}",
                file_name.to_string_lossy(),
                std::process::id()
            ));
            match directory.create_file_for_destination(&path, &destination) {
                Ok(created) => {
                    return Ok(Self {
                        path,
                        directory,
                        created: std::cell::RefCell::new(Some(created)),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(ExportCommitError::StageUnavailable(error.to_string())),
            }
        }
        Err(ExportCommitError::StageUnavailable(
            "could not allocate a unique same-directory stage".into(),
        ))
    }

    fn write_and_verify(
        &self,
        bytes: &[u8],
        expected: ArtifactHash,
    ) -> Result<(), ExportCommitError> {
        let created = self.created.borrow();
        let mut file = created
            .as_ref()
            .ok_or(ExportCommitError::StageIdentityDrift)?
            .file()
            .map_err(|error| ExportCommitError::StageWriteFailed(error.to_string()))?;
        // The exclusive create-new handle stays held across writing, verification and
        // commit. Never reopen the pathname with truncation, even on a failed write.
        file.write_all(bytes)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
            .map_err(|error| ExportCommitError::StageWriteFailed(error.to_string()))?;
        if hash_file_handle(file)? != expected {
            return Err(ExportCommitError::StageIdentityDrift);
        }
        Ok(())
    }

    fn prepare_replace(
        &self,
        prior: &Path,
    ) -> std::io::Result<publisher_windows_atomic_file::PreparedReplacement<'a>> {
        self.created
            .borrow()
            .as_ref()
            .ok_or_else(|| std::io::Error::other("export stage handle is missing"))?
            .prepare_replace(prior)
    }

    fn commit_new(&self) -> Result<(), ExportCommitError> {
        self.created
            .borrow_mut()
            .take()
            .ok_or(ExportCommitError::StageIdentityDrift)?
            .commit()
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    ExportCommitError::DestinationAlreadyExists
                } else {
                    ExportCommitError::CommitFailed(error.to_string())
                }
            })?;
        Ok(())
    }

    fn disarm(&self) {
        if let Some(created) = self.created.borrow_mut().take() {
            created.retain();
        }
    }
}

#[cfg(not(windows))]
struct StageFile {
    path: PathBuf,
    armed: std::cell::Cell<bool>,
}

#[cfg(not(windows))]
impl StageFile {
    fn create(parent: &Path, file_name: &std::ffi::OsStr) -> Result<Self, ExportCommitError> {
        for attempt in 0..16_u32 {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| ExportCommitError::StageUnavailable(error.to_string()))?
                .as_nanos();
            let path = parent.join(format!(
                ".{}.publisher-stage-{}-{nonce}-{attempt}",
                file_name.to_string_lossy(),
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => {
                    return Ok(Self {
                        path,
                        armed: std::cell::Cell::new(true),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(ExportCommitError::StageUnavailable(error.to_string())),
            }
        }
        Err(ExportCommitError::StageUnavailable(
            "could not allocate a unique same-directory stage".into(),
        ))
    }

    fn write_and_verify(
        &self,
        bytes: &[u8],
        expected: ArtifactHash,
    ) -> Result<(), ExportCommitError> {
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)
            .map_err(|error| ExportCommitError::StageWriteFailed(error.to_string()))?;
        file.write_all(bytes)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
            .map_err(|error| ExportCommitError::StageWriteFailed(error.to_string()))?;
        drop(file);
        if hash_regular_file(&self.path)? != expected {
            return Err(ExportCommitError::StageIdentityDrift);
        }
        Ok(())
    }

    fn remove_after_link(&self) -> Result<(), ExportCommitError> {
        std::fs::remove_file(&self.path)
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
        self.armed.set(false);
        Ok(())
    }

    fn disarm(&self) {
        self.armed.set(false);
    }
}

#[cfg(not(windows))]
impl Drop for StageFile {
    fn drop(&mut self) {
        if self.armed.get() {
            let _ignored = std::fs::remove_file(&self.path);
        }
    }
}

struct BackupFile {
    path: PathBuf,
}

impl BackupFile {
    fn allocate(parent: &Path, file_name: &std::ffi::OsStr) -> Result<Self, ExportCommitError> {
        for attempt in 0..16_u32 {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| ExportCommitError::StageUnavailable(error.to_string()))?
                .as_nanos();
            let path = parent.join(format!(
                ".{}.publisher-prior-{}-{nonce}-{attempt}",
                file_name.to_string_lossy(),
                std::process::id()
            ));
            match path.symlink_metadata() {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Self { path });
                }
                Ok(_) => {}
                Err(error) => return Err(ExportCommitError::StageUnavailable(error.to_string())),
            }
        }
        Err(ExportCommitError::StageUnavailable(
            "could not allocate a unique same-directory prior-generation backup".into(),
        ))
    }

    #[cfg(not(windows))]
    fn remove_after_success(&self) -> Result<(), ExportCommitError> {
        std::fs::remove_file(&self.path)
            .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))
    }
}

#[cfg(not(windows))]
fn atomic_replace_with_backup(
    destination: &Path,
    stage: &Path,
    backup: &Path,
) -> std::io::Result<()> {
    std::fs::hard_link(destination, backup)?;
    std::fs::rename(stage, destination)
}

#[cfg(not(windows))]
fn restore_backup(destination: &Path, backup: &Path) -> std::io::Result<()> {
    std::fs::rename(backup, destination)
}

#[cfg(unix)]
fn sync_parent(parent: &Path) {
    if let Ok(directory) = File::open(parent) {
        let _ignored = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultPoint {
    None,
    AfterStageVerification,
    AfterDestinationRecheck,
    ChangeDestinationAfterRecheck,
    CorruptAfterReplacement,
    BlockRestoreAfterReplacement,
    #[cfg(windows)]
    AfterInstallBeforeReceipt,
    #[cfg(windows)]
    OccupyBackupBeforePreserve,
    #[cfg(all(windows, test))]
    AttemptMarkerMutationAfterPriorPreserved,
    #[cfg(all(windows, test))]
    AbortPrepared,
    #[cfg(all(windows, test))]
    AbortAfterPriorPreserved,
    #[cfg(all(windows, test))]
    AbortCommitted,
}

#[cfg(test)]
mod tests {
    use super::{
        DestinationExpectation, ExportCommitError, ExportCommitOutcome, FaultPoint,
        commit_authorized_pdf_inner,
    };
    use crate::{ArtifactHash, PublicationAuthorization};
    #[cfg(windows)]
    use std::process::Command;
    use std::{fs, path::PathBuf, time::SystemTime};

    #[cfg(windows)]
    const ABORT_DESTINATION: &str = "PUBLISHER_EXPORT_ABORT_DESTINATION";
    #[cfg(windows)]
    const ABORT_POINT: &str = "PUBLISHER_EXPORT_ABORT_POINT";

    fn directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("publisher-export-{label}-{nonce}"));
        fs::create_dir(&path).expect("create test directory");
        path
    }

    fn authorization(bytes: &[u8]) -> PublicationAuthorization {
        PublicationAuthorization::for_test(ArtifactHash::of(bytes))
    }

    #[cfg(windows)]
    #[test]
    fn exclusive_stage_blocks_substitution_and_writers_before_verification() {
        let directory = directory("exclusive-stage");
        let pinned = publisher_windows_atomic_file::PinnedDirectory::for_destination(
            &directory.join("publication.pdf"),
        )
        .expect("pin private fixture");
        let stage =
            super::StageFile::create(&directory, std::ffi::OsStr::new("publication.pdf"), &pinned)
                .expect("create held stage");
        assert!(
            fs::rename(&stage.path, directory.join("displaced.stage")).is_err(),
            "the stage must retain its original identity from creation"
        );
        assert!(
            fs::write(&stage.path, b"independent writer").is_err(),
            "other writers must not alter the retained stage"
        );
        let bytes = b"authorized PDF";
        stage
            .write_and_verify(bytes, ArtifactHash::of(bytes))
            .expect("write held stage");
        let stage_path = stage.path.clone();
        drop(stage);
        assert!(!stage_path.exists(), "abandoned exact stage is cleaned");
        drop(pinned);
        fs::remove_dir_all(directory).expect("cleanup private fixture");
    }

    #[cfg(windows)]
    #[test]
    fn failed_stage_verification_cleans_only_the_held_stage() {
        let directory = directory("stage-verification-failure");
        let destination = directory.join("publication.pdf");
        fs::write(&destination, b"prior PDF").expect("write prior");
        let pinned = publisher_windows_atomic_file::PinnedDirectory::for_destination(&destination)
            .expect("pin private fixture");
        let stage =
            super::StageFile::create(&directory, std::ffi::OsStr::new("publication.pdf"), &pinned)
                .expect("create stage");
        let independent = directory.join("independent.pdf");
        fs::write(&independent, b"independent bytes").expect("write unrelated file");
        assert_eq!(
            stage.write_and_verify(b"partial PDF", ArtifactHash::of(b"complete PDF")),
            Err(ExportCommitError::StageIdentityDrift)
        );
        let stage_path = stage.path.clone();
        drop(stage);
        assert!(!stage_path.exists());
        assert_eq!(fs::read(&destination).expect("read prior"), b"prior PDF");
        assert_eq!(
            fs::read(&independent).expect("read unrelated file"),
            b"independent bytes"
        );
        drop(pinned);
        fs::remove_dir_all(directory).expect("cleanup private fixture");
    }

    #[test]
    fn save_a_copy_creates_once_and_never_overwrites() {
        let directory = directory("copy");
        let destination = directory.join("publication.pdf");
        let bytes = b"authorized PDF";
        let authorization = authorization(bytes);
        assert_eq!(
            commit_authorized_pdf_inner(
                &authorization,
                bytes,
                &destination,
                DestinationExpectation::CreateNew,
                FaultPoint::None,
            ),
            Ok(ExportCommitOutcome::Created)
        );
        assert_eq!(
            commit_authorized_pdf_inner(
                &authorization,
                bytes,
                &destination,
                DestinationExpectation::CreateNew,
                FaultPoint::None,
            ),
            Err(ExportCommitError::DestinationAlreadyExists)
        );
        assert_eq!(fs::read(&destination).expect("read copy"), bytes);
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn replace_refuses_external_destination_change() {
        let directory = directory("conflict");
        let destination = directory.join("publication.pdf");
        fs::write(&destination, b"external generation").expect("write destination");
        let authorization = authorization(b"new PDF");
        assert_eq!(
            commit_authorized_pdf_inner(
                &authorization,
                b"new PDF",
                &destination,
                DestinationExpectation::ReplaceExact(ArtifactHash::of(b"older generation")),
                FaultPoint::None,
            ),
            Err(ExportCommitError::DestinationChanged)
        );
        assert_eq!(
            fs::read(&destination).expect("read preserved destination"),
            b"external generation"
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn injected_precommit_faults_preserve_prior_valid_output_and_clean_stage() {
        for fault in [
            FaultPoint::AfterStageVerification,
            FaultPoint::AfterDestinationRecheck,
        ] {
            let directory = directory("fault");
            let destination = directory.join("publication.pdf");
            fs::write(&destination, b"prior valid PDF").expect("write destination");
            let authorization = authorization(b"new PDF");
            assert!(matches!(
                commit_authorized_pdf_inner(
                    &authorization,
                    b"new PDF",
                    &destination,
                    DestinationExpectation::ReplaceExact(ArtifactHash::of(b"prior valid PDF")),
                    fault,
                ),
                Err(ExportCommitError::CommitFailed(_))
            ));
            assert_eq!(
                fs::read(&destination).expect("read prior"),
                b"prior valid PDF"
            );
            assert_eq!(fs::read_dir(&directory).expect("list").count(), 1);
            fs::remove_dir_all(directory).expect("cleanup");
        }
    }

    #[test]
    fn exact_generation_replacement_commits_authorized_bytes() {
        let directory = directory("replace");
        let destination = directory.join("publication.pdf");
        fs::write(&destination, b"prior valid PDF").expect("write destination");
        let bytes = b"new authorized PDF";
        let authorization = authorization(bytes);
        assert_eq!(
            commit_authorized_pdf_inner(
                &authorization,
                bytes,
                &destination,
                DestinationExpectation::ReplaceExact(ArtifactHash::of(b"prior valid PDF")),
                FaultPoint::None,
            ),
            Ok(ExportCommitOutcome::Replaced)
        );
        assert_eq!(fs::read(&destination).expect("read committed"), bytes);
        #[cfg(windows)]
        assert!(
            super::inspect_interrupted_export(&destination)
                .expect("inspect completed export")
                .is_none()
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn independently_occupied_backup_name_is_never_clobbered() {
        let directory = directory("occupied-backup-race");
        let destination = directory.join("publication.pdf");
        let prior = b"prior valid PDF";
        std::fs::write(&destination, prior).expect("write destination");
        let bytes = b"new authorized PDF";
        let result = commit_authorized_pdf_inner(
            &authorization(bytes),
            bytes,
            &destination,
            DestinationExpectation::ReplaceExact(ArtifactHash::of(prior)),
            FaultPoint::OccupyBackupBeforePreserve,
        );
        assert!(matches!(result, Err(ExportCommitError::CommitFailed(_))));
        assert_eq!(
            std::fs::read(&destination).expect("prior destination survives"),
            prior
        );
        let occupied = std::fs::read_dir(&directory)
            .expect("list export artifacts")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".publisher-prior-"))
            })
            .expect("independently occupied backup survives");
        assert_eq!(
            std::fs::read(occupied).expect("read independent backup occupant"),
            b"independent backup occupant"
        );
        assert_eq!(
            super::inspect_interrupted_export(&destination)
                .expect("inspect occupied-backup interruption")
                .expect("active export journal")
                .state,
            super::InterruptedExportState::Ambiguous
        );
        std::fs::remove_dir_all(directory).expect("cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn active_journal_is_pinned_through_the_destructive_window() {
        let directory = directory("pinned-journal");
        let destination = directory.join("publication.pdf");
        let prior = b"prior valid PDF";
        let replacement = b"new authorized PDF";
        fs::write(&destination, prior).expect("write destination");
        assert_eq!(
            commit_authorized_pdf_inner(
                &authorization(replacement),
                replacement,
                &destination,
                DestinationExpectation::ReplaceExact(ArtifactHash::of(prior)),
                FaultPoint::AttemptMarkerMutationAfterPriorPreserved,
            ),
            Ok(ExportCommitOutcome::Replaced)
        );
        assert_eq!(
            fs::read(&destination).expect("read replacement"),
            replacement
        );
        assert!(
            super::inspect_interrupted_export(&destination)
                .expect("inspect completed pinned-journal export")
                .is_none()
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "launched as an abrupt-death subprocess by the parent test"]
    fn child_abort_export_at_selected_transition() {
        let Some(destination) = std::env::var_os(ABORT_DESTINATION) else {
            return;
        };
        let abort_point = std::env::var(ABORT_POINT).expect("child abort point environment");
        let fault = match abort_point.as_str() {
            "prepared" => FaultPoint::AbortPrepared,
            "prior_preserved" => FaultPoint::AbortAfterPriorPreserved,
            "committed" => FaultPoint::AbortCommitted,
            other => panic!("unsupported child abort point: {other}"),
        };
        let destination = PathBuf::from(destination);
        let prior = fs::read(&destination).expect("read child prior");
        let replacement = b"child replacement PDF";
        let result = commit_authorized_pdf_inner(
            &authorization(replacement),
            replacement,
            &destination,
            DestinationExpectation::ReplaceExact(ArtifactHash::of(&prior)),
            fault,
        );
        panic!("child did not abort at {abort_point}: {result:?}");
    }

    #[cfg(windows)]
    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_abrupt_export_transition_is_classifiable_recoverable_and_blocks_retry() {
        use std::os::windows::process::CommandExt;

        use super::{InterruptedExportRecoveryOutcome, InterruptedExportState};

        let prior = b"prior valid PDF";
        let replacement = b"child replacement PDF";
        for (abort_point, expected_state, expected_outcome) in [
            (
                "prepared",
                InterruptedExportState::Prepared,
                InterruptedExportRecoveryOutcome::PriorRetained,
            ),
            (
                "prior_preserved",
                InterruptedExportState::PriorPreserved,
                InterruptedExportRecoveryOutcome::PriorRestored,
            ),
            (
                "committed",
                InterruptedExportState::Committed,
                InterruptedExportRecoveryOutcome::ReplacementRetained,
            ),
        ] {
            let directory = directory(abort_point);
            let destination = directory.join("publication.pdf");
            let marker = directory.join(".abort-reached");
            fs::write(&destination, prior).expect("write destination");
            let output = Command::new(std::env::current_exe().expect("current test executable"))
                .args([
                    "--ignored",
                    "--exact",
                    "export::tests::child_abort_export_at_selected_transition",
                ])
                .env(ABORT_DESTINATION, &destination)
                .env(ABORT_POINT, abort_point)
                .env("PUBLISHER_EXPORT_ABORT_MARKER", &marker)
                .current_dir(&directory)
                // The crash fixture must not open a console during headless checks.
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .output()
                .expect("launch abrupt export child");
            assert!(
                !output.status.success(),
                "{abort_point} child must terminate abruptly"
            );
            assert_eq!(
                fs::read_to_string(&marker).expect("read exact abort marker"),
                abort_point
            );
            let status = super::inspect_interrupted_export(&destination)
                .expect("inspect abrupt export")
                .expect("active export journal");
            assert_eq!(status.state, expected_state, "{abort_point}");
            let journal_bytes = fs::read(super::export_journal_path(&destination).unwrap())
                .expect("read active journal");
            match expected_state {
                InterruptedExportState::Prepared => {
                    assert_eq!(fs::read(&destination).unwrap(), prior);
                    assert_eq!(fs::read(&status.stage_path).unwrap(), replacement);
                    assert!(!status.backup_path.exists());
                }
                InterruptedExportState::PriorPreserved => {
                    assert!(!destination.exists());
                    assert_eq!(fs::read(&status.stage_path).unwrap(), replacement);
                    assert_eq!(fs::read(&status.backup_path).unwrap(), prior);
                }
                InterruptedExportState::Committed => {
                    assert_eq!(fs::read(&destination).unwrap(), replacement);
                    assert!(!status.stage_path.exists());
                    assert_eq!(fs::read(&status.backup_path).unwrap(), prior);
                }
                InterruptedExportState::Ambiguous => unreachable!(),
            }

            let retry_bytes = b"retry PDF";
            for expectation in [
                DestinationExpectation::CreateNew,
                DestinationExpectation::ReplaceExact(ArtifactHash::of(prior)),
            ] {
                assert!(matches!(
                    super::commit_authorized_pdf(
                        &authorization(retry_bytes),
                        retry_bytes,
                        &destination,
                        expectation,
                    ),
                    Err(ExportCommitError::InterruptedExportPresent(_))
                ));
            }

            let recovered = super::recover_interrupted_export(&destination, &status)
                .expect("recover exact interrupted export");
            assert_eq!(recovered.outcome, expected_outcome, "{abort_point}");
            assert_eq!(
                fs::read(&destination).unwrap(),
                if expected_state == InterruptedExportState::Committed {
                    replacement.as_slice()
                } else {
                    prior.as_slice()
                }
            );
            assert_eq!(fs::read(&recovered.receipt_path).unwrap(), journal_bytes);
            if expected_state == InterruptedExportState::Committed {
                assert!(recovered.stage_path.is_none());
                assert_eq!(
                    fs::read(recovered.backup_path.expect("retained exact prior")).unwrap(),
                    prior
                );
            } else {
                assert_eq!(
                    fs::read(recovered.stage_path.expect("retained exact replacement")).unwrap(),
                    replacement
                );
                assert!(recovered.backup_path.is_none());
            }
            assert!(
                super::inspect_interrupted_export(&destination)
                    .expect("inspect recovered export")
                    .is_none()
            );
            fs::remove_dir_all(directory).expect("cleanup");
        }
    }

    #[test]
    fn post_replace_verification_failure_restores_exact_prior_generation() {
        let directory = directory("post-replace-fault");
        let destination = directory.join("publication.pdf");
        let prior = b"prior valid PDF";
        fs::write(&destination, prior).expect("write destination");
        let bytes = b"new authorized PDF";
        let authorization = authorization(bytes);
        assert_eq!(
            commit_authorized_pdf_inner(
                &authorization,
                bytes,
                &destination,
                DestinationExpectation::ReplaceExact(ArtifactHash::of(prior)),
                FaultPoint::CorruptAfterReplacement,
            ),
            Err(ExportCommitError::FinalIdentityDrift)
        );
        assert_eq!(fs::read(&destination).expect("read restored prior"), prior);
        #[cfg(not(windows))]
        #[cfg(not(windows))]
        assert_eq!(fs::read_dir(&directory).expect("list").count(), 1);
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn change_after_recheck_is_detected_and_external_generation_is_restored() {
        let directory = directory("replace-race");
        let destination = directory.join("publication.pdf");
        let prior = b"prior valid PDF";
        fs::write(&destination, prior).expect("write destination");
        let bytes = b"new authorized PDF";
        let authorization = authorization(bytes);
        assert_eq!(
            commit_authorized_pdf_inner(
                &authorization,
                bytes,
                &destination,
                DestinationExpectation::ReplaceExact(ArtifactHash::of(prior)),
                FaultPoint::ChangeDestinationAfterRecheck,
            ),
            Err(ExportCommitError::DestinationChanged)
        );
        assert_eq!(
            fs::read(&destination).expect("read restored external generation"),
            b"external generation after recheck"
        );
        #[cfg(not(windows))]
        assert_eq!(fs::read_dir(&directory).expect("list").count(), 1);
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[test]
    fn failed_restoration_retains_exact_prior_backup_and_reports_its_path() {
        let directory = directory("retained-backup");
        let destination = directory.join("publication.pdf");
        let prior = b"prior valid PDF";
        fs::write(&destination, prior).expect("write destination");
        let bytes = b"new authorized PDF";
        let authorization = authorization(bytes);
        let result = commit_authorized_pdf_inner(
            &authorization,
            bytes,
            &destination,
            DestinationExpectation::ReplaceExact(ArtifactHash::of(prior)),
            FaultPoint::BlockRestoreAfterReplacement,
        );
        let Err(ExportCommitError::RecoveryFailed { backup_path, .. }) = result else {
            panic!("failed restoration must report retained backup path");
        };
        assert_eq!(fs::read(&backup_path).expect("read retained backup"), prior);
        assert!(destination.is_dir());
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn failure_report_distinguishes_restored_prior_from_blocked_restoration() {
        for fault in [
            FaultPoint::CorruptAfterReplacement,
            FaultPoint::BlockRestoreAfterReplacement,
        ] {
            let directory = directory("failure-report");
            let destination = directory.join("publication.pdf");
            let prior = b"prior valid PDF";
            let bytes = b"new authorized PDF";
            fs::write(&destination, prior).expect("write prior");
            let failure = super::commit_with_report_inner(
                &authorization(bytes),
                bytes,
                &destination,
                DestinationExpectation::ReplaceExact(ArtifactHash::of(prior)),
                fault,
            )
            .expect_err("injected failure");
            let blocked = fault == FaultPoint::BlockRestoreAfterReplacement;
            assert_eq!(
                failure.state.destination.state,
                if blocked {
                    super::ExportArtifactState::NotRegularFile
                } else {
                    super::ExportArtifactState::RegularFile(ArtifactHash::of(prior))
                }
            );
            assert_eq!(
                failure.state.backup.expect("known backup").state,
                if blocked {
                    super::ExportArtifactState::RegularFile(ArtifactHash::of(prior))
                } else {
                    super::ExportArtifactState::Missing
                }
            );
            let journal = failure.state.journal.expect("known journal");
            let receipt = failure.state.receipt.expect("known receipt");
            assert_eq!(
                matches!(journal.state, super::ExportArtifactState::RegularFile(_)),
                blocked
            );
            assert_eq!(
                matches!(receipt.state, super::ExportArtifactState::RegularFile(_)),
                !blocked
            );
            assert_eq!(
                if blocked {
                    receipt.state
                } else {
                    journal.state
                },
                super::ExportArtifactState::Missing
            );
            fs::remove_dir_all(directory).expect("cleanup");
        }
    }

    #[cfg(windows)]
    #[test]
    fn failure_report_identifies_installed_pdf_and_prior_backup_before_receipt() {
        let directory = directory("installed-report");
        let destination = directory.join("publication.pdf");
        let prior = b"prior valid PDF";
        let bytes = b"new authorized PDF";
        fs::write(&destination, prior).expect("write prior");
        let failure = super::commit_with_report_inner(
            &authorization(bytes),
            bytes,
            &destination,
            DestinationExpectation::ReplaceExact(ArtifactHash::of(prior)),
            FaultPoint::AfterInstallBeforeReceipt,
        )
        .expect_err("injected post-install failure");
        assert!(matches!(failure.error, ExportCommitError::CommitFailed(_)));
        assert_eq!(
            failure.state.destination.state,
            super::ExportArtifactState::RegularFile(ArtifactHash::of(bytes))
        );
        let backup = failure.state.backup.expect("known backup");
        assert_eq!(
            backup.state,
            super::ExportArtifactState::RegularFile(ArtifactHash::of(prior))
        );
        assert_eq!(fs::read(backup.path).expect("retained exact prior"), prior);
        assert!(matches!(
            failure.state.journal.expect("known journal").state,
            super::ExportArtifactState::RegularFile(_)
        ));
        assert_eq!(
            failure.state.receipt.expect("known receipt").state,
            super::ExportArtifactState::Missing
        );
        // A retry reports only paths it established; it must not adopt even valid
        // embedded backup or receipt paths from an earlier operation's journal.
        let retry = super::commit_authorized_pdf_with_report(
            &authorization(bytes),
            bytes,
            &destination,
            DestinationExpectation::ReplaceExact(ArtifactHash::of(prior)),
        )
        .expect_err("active journal prevents retry");
        assert!(matches!(
            retry.error,
            ExportCommitError::InterruptedExportPresent(_)
        ));
        assert_eq!(
            retry.state.destination.state,
            super::ExportArtifactState::RegularFile(ArtifactHash::of(bytes))
        );
        assert!(retry.state.journal.is_some());
        assert!(retry.state.backup.is_none());
        assert!(retry.state.receipt.is_none());
        fs::remove_dir_all(directory).expect("cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn failure_observations_distinguish_missing_locked_and_oversized_files() {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;

        let directory = directory("observation-limits");
        let path = directory.join("publication.pdf");
        assert_eq!(
            super::observe_export_artifact(&path).state,
            super::ExportArtifactState::Missing
        );
        fs::write(&path, b"locked bytes").expect("write file");
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .expect("exclusive handle");
        assert_eq!(
            super::observe_export_artifact(&path).state,
            super::ExportArtifactState::Unavailable
        );
        drop(lock);
        OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open file")
            .set_len(super::MAX_EXPORT_BYTES + 1)
            .expect("oversized length");
        assert_eq!(
            super::observe_export_artifact(&path).state,
            super::ExportArtifactState::Unavailable
        );
        fs::remove_dir_all(directory).expect("cleanup");
    }
}
