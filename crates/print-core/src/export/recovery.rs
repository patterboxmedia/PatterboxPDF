//! Explicit, no-clobber recovery of an inspected interrupted Windows export.

use super::{
    ArtifactHash, ExportCommitError, ExportJournal, InterruptedExportState,
    InterruptedExportStatus, MAX_EXPORT_JOURNAL_BYTES, export_journal_path, file_name_string,
    hash_file_handle, parse_artifact_hash,
};
use publisher_windows_atomic_file::{FileIdentity, PinnedDirectory, PinnedFile, PreparedMove};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RecoveryIdentities {
    directory: FileIdentity,
    journal: FileIdentity,
    destination: Option<FileIdentity>,
    stage: Option<FileIdentity>,
    backup: Option<FileIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptedExportRecoveryOutcome {
    PriorRetained,
    PriorRestored,
    ReplacementRetained,
}

/// Verified files retained when the active journal becomes a completed receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterruptedExportRecovery {
    pub outcome: InterruptedExportRecoveryOutcome,
    pub destination_path: PathBuf,
    pub receipt_path: PathBuf,
    pub stage_path: Option<PathBuf>,
    pub backup_path: Option<PathBuf>,
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "map_err consumes the IO error"
)]
fn io_error(error: std::io::Error) -> ExportCommitError {
    ExportCommitError::CommitFailed(error.to_string())
}

pub(super) fn role_name(name: &str, destination: &str, role: &str) -> bool {
    let prefix = format!(".{destination}.publisher-{role}-");
    let Some(suffix) = name.strip_prefix(&prefix) else {
        return false;
    };
    let parts: Vec<_> = suffix.split('-').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

pub(super) fn unique_names(
    journal: &ExportJournal,
    destination: &Path,
) -> Result<bool, ExportCommitError> {
    let marker = file_name_string(&export_journal_path(destination)?)?;
    let names = [
        &journal.destination_name,
        &journal.stage_name,
        &journal.backup_name,
        &journal.failed_name,
        &journal.receipt_name,
        &marker,
    ];
    Ok(names.iter().enumerate().all(|(index, name)| {
        names[..index]
            .iter()
            .all(|other| !name.eq_ignore_ascii_case(other))
    }))
}

fn missing(path: &Path) -> Result<bool, ExportCommitError> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(io_error(error)),
        Ok(_) => Ok(false),
    }
}

fn read_journal(file: File) -> Result<(ExportJournal, ArtifactHash), ExportCommitError> {
    let mut bytes = Vec::new();
    file.take(MAX_EXPORT_JOURNAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > MAX_EXPORT_JOURNAL_BYTES {
        return Err(ExportCommitError::ResourceLimit);
    }
    let journal = serde_json::from_slice(&bytes)
        .map_err(|error| ExportCommitError::CommitFailed(error.to_string()))?;
    Ok((journal, ArtifactHash::of(&bytes)))
}

struct Observed {
    file: Option<PinnedFile>,
    hash: Option<ArtifactHash>,
}

impl Observed {
    fn open(directory: &PinnedDirectory, path: &Path) -> Result<Self, ExportCommitError> {
        if missing(path)? {
            return Ok(Self {
                file: None,
                hash: None,
            });
        }
        let file = directory.pin_file(path).map_err(io_error)?;
        let hash = hash_file_handle(file.file().map_err(io_error)?)?;
        Ok(Self {
            file: Some(file),
            hash: Some(hash),
        })
    }
    fn identity(&self) -> Option<FileIdentity> {
        self.file.as_ref().map(PinnedFile::identity)
    }
}

fn classify(
    destination: Option<ArtifactHash>,
    stage: Option<ArtifactHash>,
    backup: Option<ArtifactHash>,
    prior: ArtifactHash,
    replacement: ArtifactHash,
) -> InterruptedExportState {
    if destination == Some(prior) && stage == Some(replacement) && backup.is_none() {
        InterruptedExportState::Prepared
    } else if destination.is_none() && stage == Some(replacement) && backup == Some(prior) {
        InterruptedExportState::PriorPreserved
    } else if destination == Some(replacement) && stage.is_none() && backup == Some(prior) {
        InterruptedExportState::Committed
    } else {
        InterruptedExportState::Ambiguous
    }
}

/// Inspects exact held regular files without changing any artifact.
///
/// # Errors
/// Rejects unsafe paths, malformed journals, sharing conflicts, and unreadable files.
pub fn inspect_interrupted_export(
    destination: &Path,
) -> Result<Option<InterruptedExportStatus>, ExportCommitError> {
    let journal_path = export_journal_path(destination)?;
    let directory = PinnedDirectory::for_destination(destination).map_err(io_error)?;
    if missing(&journal_path)? {
        return Ok(None);
    }
    let pinned = directory.pin_file(&journal_path).map_err(io_error)?;
    let (journal, journal_hash) = read_journal(pinned.file().map_err(io_error)?)?;
    journal.validate_for(destination)?;
    let parent = destination
        .parent()
        .ok_or(ExportCommitError::InvalidDestination)?;
    let stage_path = parent.join(&journal.stage_name);
    let backup_path = parent.join(&journal.backup_name);
    let receipt_path = parent.join(&journal.receipt_name);
    let prior_hash =
        parse_artifact_hash(&journal.prior_hash).ok_or(ExportCommitError::InvalidDestination)?;
    let replacement_hash = parse_artifact_hash(&journal.replacement_hash)
        .ok_or(ExportCommitError::InvalidDestination)?;
    let destination_file = Observed::open(&directory, destination)?;
    let stage = Observed::open(&directory, &stage_path)?;
    let backup = Observed::open(&directory, &backup_path)?;
    let classification = if missing(&receipt_path)? && missing(&parent.join(&journal.failed_name))?
    {
        classify(
            destination_file.hash,
            stage.hash,
            backup.hash,
            prior_hash,
            replacement_hash,
        )
    } else {
        InterruptedExportState::Ambiguous
    };
    Ok(Some(InterruptedExportStatus {
        state: classification,
        prior_hash,
        replacement_hash,
        backup_path,
        stage_path,
        receipt_path,
        journal_hash,
        identities: RecoveryIdentities {
            directory: directory.identity().map_err(io_error)?,
            journal: pinned.identity(),
            destination: destination_file.identity(),
            stage: stage.identity(),
            backup: backup.identity(),
        },
    }))
}

/// Completes only the recovery operation the caller previously inspected.
///
/// Staged output and prior backups are preserved. No occupied name is overwritten.
/// After a failed receipt rename, inspect again before retrying: a restored prior
/// is then classified as `Prepared` and can safely be retained.
///
/// # Errors
/// Returns an error without authorizing changed or ambiguous artifacts. A failure
/// after restoring the prior may leave that verified prior at the destination;
/// the active journal remains available for inspection and retry.
pub fn recover_interrupted_export(
    destination: &Path,
    expected: &InterruptedExportStatus,
) -> Result<InterruptedExportRecovery, ExportCommitError> {
    recover_inner(destination, expected, false)
}

fn recover_inner(
    destination: &Path,
    expected: &InterruptedExportStatus,
    fail_after_restore: bool,
) -> Result<InterruptedExportRecovery, ExportCommitError> {
    if expected.state == InterruptedExportState::Ambiguous {
        return Err(ExportCommitError::DestinationChanged);
    }
    if inspect_interrupted_export(destination)?.as_ref() != Some(expected) {
        return Err(ExportCommitError::DestinationChanged);
    }
    let directory = PinnedDirectory::for_destination(destination).map_err(io_error)?;
    if directory.identity().map_err(io_error)? != expected.identities.directory {
        return Err(ExportCommitError::DestinationChanged);
    }
    let journal_path = export_journal_path(destination)?;
    let receipt = directory
        .prepare_move_file_no_replace(&journal_path, &expected.receipt_path)
        .map_err(io_error)?;
    let (journal, hash) = read_journal(receipt.source_file().map_err(io_error)?)?;
    journal.validate_for(destination)?;
    if hash != expected.journal_hash || receipt.identity() != expected.identities.journal {
        return Err(ExportCommitError::DestinationChanged);
    }
    let parent = destination
        .parent()
        .ok_or(ExportCommitError::InvalidDestination)?;
    if !missing(&expected.receipt_path)? || !missing(&parent.join(&journal.failed_name))? {
        return Err(ExportCommitError::DestinationAlreadyExists);
    }
    let stage = Observed::open(&directory, &expected.stage_path)?;
    check_observed(
        &stage,
        expected.identities.stage,
        if expected.state == InterruptedExportState::Committed {
            None
        } else {
            Some(expected.replacement_hash)
        },
    )?;
    let result = match expected.state {
        InterruptedExportState::Prepared => {
            let prior = Observed::open(&directory, destination)?;
            check_observed(
                &prior,
                expected.identities.destination,
                Some(expected.prior_hash),
            )?;
            if !missing(&expected.backup_path)? {
                return Err(ExportCommitError::DestinationChanged);
            }
            retire(receipt)?;
            InterruptedExportRecoveryOutcome::PriorRetained
        }
        InterruptedExportState::PriorPreserved => {
            let prior = directory
                .prepare_move_file_no_replace(&expected.backup_path, destination)
                .map_err(io_error)?;
            if Some(prior.identity()) != expected.identities.backup
                || hash_file_handle(prior.source_file().map_err(io_error)?)? != expected.prior_hash
            {
                return Err(ExportCommitError::DestinationChanged);
            }
            let restored = prior.commit().map_err(io_error)?;
            if hash_file_handle(restored.reopened_destination_file().map_err(io_error)?)?
                != expected.prior_hash
            {
                return Err(ExportCommitError::FinalIdentityDrift);
            }
            if fail_after_restore {
                return Err(ExportCommitError::CommitFailed(
                    "injected failure after prior restoration".into(),
                ));
            }
            retire(receipt)?;
            InterruptedExportRecoveryOutcome::PriorRestored
        }
        InterruptedExportState::Committed => {
            let replacement = Observed::open(&directory, destination)?;
            let backup = Observed::open(&directory, &expected.backup_path)?;
            check_observed(
                &replacement,
                expected.identities.destination,
                Some(expected.replacement_hash),
            )?;
            check_observed(
                &backup,
                expected.identities.backup,
                Some(expected.prior_hash),
            )?;
            retire(receipt)?;
            InterruptedExportRecoveryOutcome::ReplacementRetained
        }
        InterruptedExportState::Ambiguous => return Err(ExportCommitError::DestinationChanged),
    };
    Ok(InterruptedExportRecovery {
        outcome: result,
        destination_path: destination.to_path_buf(),
        receipt_path: expected.receipt_path.clone(),
        stage_path: (expected.state != InterruptedExportState::Committed)
            .then(|| expected.stage_path.clone()),
        backup_path: (expected.state == InterruptedExportState::Committed)
            .then(|| expected.backup_path.clone()),
    })
}

fn check_observed(
    observed: &Observed,
    identity: Option<FileIdentity>,
    hash: Option<ArtifactHash>,
) -> Result<(), ExportCommitError> {
    if observed.identity() != identity || observed.hash != hash {
        Err(ExportCommitError::DestinationChanged)
    } else {
        Ok(())
    }
}

fn retire(receipt: PreparedMove<'_>) -> Result<(), ExportCommitError> {
    let moved = receipt.commit().map_err(io_error)?;
    moved.reopened_destination_file().map_err(io_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture {
        directory: PathBuf,
        destination: PathBuf,
        journal: ExportJournal,
    }
    impl Fixture {
        fn new(state: InterruptedExportState) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let directory = std::env::temp_dir()
                .join(format!("publisher-recovery-{}-{nonce}", std::process::id()));
            fs::create_dir(&directory).unwrap();
            let destination = directory.join("publication.pdf");
            let operation_id = ArtifactHash::of(directory.to_string_lossy().as_bytes()).hex();
            let journal = ExportJournal {
                schema_version: 1,
                operation_id: operation_id.clone(),
                destination_name: "publication.pdf".into(),
                stage_name: ".publication.pdf.publisher-stage-123-456-0".into(),
                backup_name: ".publication.pdf.publisher-prior-123-456-0".into(),
                failed_name: format!(".publisher-export-{operation_id}.failed"),
                receipt_name: format!(".publisher-export-{operation_id}.completed.json"),
                prior_hash: ArtifactHash::of(b"prior PDF").hex(),
                replacement_hash: ArtifactHash::of(b"replacement PDF").hex(),
            };
            if state == InterruptedExportState::Prepared {
                fs::write(&destination, b"prior PDF").unwrap();
            }
            if state != InterruptedExportState::Committed {
                fs::write(directory.join(&journal.stage_name), b"replacement PDF").unwrap();
            }
            if state != InterruptedExportState::Prepared {
                fs::write(directory.join(&journal.backup_name), b"prior PDF").unwrap();
            }
            if state == InterruptedExportState::Committed {
                fs::write(&destination, b"replacement PDF").unwrap();
            }
            let fixture = Self {
                directory,
                destination,
                journal,
            };
            fixture.write_journal();
            fixture
        }
        fn write_journal(&self) {
            fs::write(
                export_journal_path(&self.destination).unwrap(),
                serde_json::to_vec(&self.journal).unwrap(),
            )
            .unwrap();
        }
        fn inspect(&self) -> InterruptedExportStatus {
            inspect_interrupted_export(&self.destination)
                .unwrap()
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    #[test]
    fn every_supported_recovery_preserves_expected_artifacts_and_retires_journal() {
        for (state, outcome, output) in [
            (
                InterruptedExportState::Prepared,
                InterruptedExportRecoveryOutcome::PriorRetained,
                b"prior PDF".as_slice(),
            ),
            (
                InterruptedExportState::PriorPreserved,
                InterruptedExportRecoveryOutcome::PriorRestored,
                b"prior PDF".as_slice(),
            ),
            (
                InterruptedExportState::Committed,
                InterruptedExportRecoveryOutcome::ReplacementRetained,
                b"replacement PDF".as_slice(),
            ),
        ] {
            let fixture = Fixture::new(state);
            let expected = fixture.inspect();
            assert_eq!(expected.state, state);
            let journal_bytes =
                fs::read(export_journal_path(&fixture.destination).unwrap()).unwrap();
            let result = recover_interrupted_export(&fixture.destination, &expected).unwrap();
            assert_eq!(result.outcome, outcome);
            assert_eq!(fs::read(&fixture.destination).unwrap(), output);
            assert_eq!(fs::read(result.receipt_path).unwrap(), journal_bytes);
            if let Some(stage) = result.stage_path {
                assert_eq!(fs::read(stage).unwrap(), b"replacement PDF");
            }
            if let Some(backup) = result.backup_path {
                assert_eq!(fs::read(backup).unwrap(), b"prior PDF");
            }
            assert!(
                inspect_interrupted_export(&fixture.destination)
                    .unwrap()
                    .is_none()
            );
            assert!(recover_interrupted_export(&fixture.destination, &expected).is_err());
            assert_eq!(fs::read(&fixture.destination).unwrap(), output);
        }
    }

    #[test]
    fn restored_prior_can_be_reinspected_and_retried_after_retirement_failure() {
        let fixture = Fixture::new(InterruptedExportState::PriorPreserved);
        let selected = fixture.inspect();
        assert!(recover_inner(&fixture.destination, &selected, true).is_err());
        assert_eq!(fs::read(&fixture.destination).unwrap(), b"prior PDF");
        assert_eq!(fs::read(&selected.stage_path).unwrap(), b"replacement PDF");
        assert!(!selected.backup_path.exists());
        assert!(recover_interrupted_export(&fixture.destination, &selected).is_err());
        let retry = fixture.inspect();
        assert_eq!(retry.state, InterruptedExportState::Prepared);
        assert_eq!(
            recover_interrupted_export(&fixture.destination, &retry)
                .unwrap()
                .outcome,
            InterruptedExportRecoveryOutcome::PriorRetained
        );
    }

    #[test]
    fn changed_destination_or_receipt_is_never_overwritten() {
        for occupy_receipt in [false, true] {
            let fixture = Fixture::new(InterruptedExportState::PriorPreserved);
            let selected = fixture.inspect();
            let occupied = if occupy_receipt {
                &selected.receipt_path
            } else {
                &fixture.destination
            };
            fs::write(occupied, b"unrelated occupant").unwrap();
            assert!(recover_interrupted_export(&fixture.destination, &selected).is_err());
            assert_eq!(fs::read(occupied).unwrap(), b"unrelated occupant");
            assert_eq!(fs::read(&selected.backup_path).unwrap(), b"prior PDF");
            assert!(export_journal_path(&fixture.destination).unwrap().exists());
        }
    }

    #[test]
    fn same_byte_replacement_of_any_selected_generation_refuses_recovery() {
        for role in ["journal", "stage", "backup", "destination"] {
            let fixture = Fixture::new(if role == "destination" {
                InterruptedExportState::Prepared
            } else {
                InterruptedExportState::PriorPreserved
            });
            let selected = fixture.inspect();
            let path = match role {
                "journal" => export_journal_path(&fixture.destination).unwrap(),
                "stage" => selected.stage_path.clone(),
                "backup" => selected.backup_path.clone(),
                _ => fixture.destination.clone(),
            };
            let bytes = fs::read(&path).unwrap();
            fs::rename(&path, fixture.directory.join("retained-original")).unwrap();
            fs::write(&path, &bytes).unwrap();
            assert!(
                recover_interrupted_export(&fixture.destination, &selected).is_err(),
                "{role}"
            );
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert!(export_journal_path(&fixture.destination).unwrap().exists());
        }
    }

    #[test]
    fn journal_roles_cannot_alias_destination_or_unrelated_siblings() {
        for invalid in [
            "publication.pdf",
            ".other.pdf.publisher-stage-123-456-0",
            ".publication.pdf.publisher-stage-x-456-0",
            "../outside.publisher-stage-1-2-0",
            ".publication.pdf.publisher-prior-123-456-0",
        ] {
            let mut fixture = Fixture::new(InterruptedExportState::Prepared);
            fixture.journal.stage_name = invalid.into();
            fixture.write_journal();
            assert!(
                inspect_interrupted_export(&fixture.destination).is_err(),
                "{invalid}"
            );
            assert_eq!(fs::read(&fixture.destination).unwrap(), b"prior PDF");
        }
    }

    #[test]
    fn journal_directory_and_failed_artifact_never_look_like_safe_recovery() {
        let fixture = Fixture::new(InterruptedExportState::Prepared);
        fs::write(
            fixture.directory.join(&fixture.journal.failed_name),
            b"uncertain output",
        )
        .unwrap();
        let selected = fixture.inspect();
        assert_eq!(selected.state, InterruptedExportState::Ambiguous);
        assert!(recover_interrupted_export(&fixture.destination, &selected).is_err());
        let journal = export_journal_path(&fixture.destination).unwrap();
        fs::remove_file(&journal).unwrap();
        fs::create_dir(&journal).unwrap();
        assert!(inspect_interrupted_export(&fixture.destination).is_err());
    }
}
