//! Durable, bounded journal for replacing an existing publication archive.

use crate::assets::AssetStore;
use crate::model::{DocumentId, Revision};
use crate::package::{
    MAX_ARCHIVE_FILE_BYTES, OpenedPackage, PackageError, PackageFingerprint, ProjectSnapshot,
    open_package, write_replacement_archive,
};
#[cfg(windows)]
use crate::package::{WindowsReplaceState, open_package_file};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
#[cfg(windows)]
use std::io::{Seek, SeekFrom};
use std::ops::Deref;
use std::path::{Path, PathBuf};

const JOURNAL_SCHEMA_VERSION: u32 = 2;
const MAX_JOURNAL_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SaveFaultPoint {
    JournalDurable,
    ReplacementFileCreated,
    ReplacementManifestWritten,
    ReplacementAssetWritten(usize),
    ReplacementFlushed,
    ReplacementValidated,
    ReplacementRecorded,
    #[cfg(unix)]
    PriorFileCreated,
    #[cfg(unix)]
    PriorCopied,
    #[cfg(unix)]
    PriorFlushed,
    #[cfg(unix)]
    PriorValidated,
    PriorPreserved,
    #[cfg(unix)]
    BeforeExchange,
    #[cfg(unix)]
    AfterExchange,
    ReplacementCommitted,
    CommitVerified,
    BeforeReceiptFinalization,
    ReadyReceiptFinalized,
    IntentSourceValidated,
    AfterReceiptFinalized,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryFaultPoint {
    BeforeArchiveMutation,
    PriorPreserved,
    ArchiveCommitted,
    ReadyReceiptFinalized,
    IntentFinalized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptedSaveState {
    IntentRecorded,
    ReplacementReady,
    PriorPreserved,
    ReplacementCommitted,
    Ambiguous,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterruptedSaveStatus {
    pub state: InterruptedSaveState,
    pub document_id: DocumentId,
    pub revision: Revision,
    pub semantic_digest: String,
}

/// Opaque, short-lived authority to recover the exact transaction that was inspected.
///
/// Callers may display the dereferenced status, but cannot construct or alter recovery authority.
/// Recovery re-inspects every journal and archive before using this value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterruptedSaveCandidate {
    status: InterruptedSaveStatus,
    destination: PathBuf,
    authority: InterruptedSaveAuthority,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum InterruptedSaveAuthority {
    LegacyV1,
    SchemaV2 {
        intent: Box<SaveJournal>,
        ready: Option<Box<SaveJournal>>,
    },
}

impl Deref for InterruptedSaveCandidate {
    type Target = InterruptedSaveStatus;

    fn deref(&self) -> &Self::Target {
        &self.status
    }
}

impl InterruptedSaveCandidate {
    /// Reports whether this exact inspected transaction uses the current recoverable journal.
    /// Ambiguous and legacy journal states remain visible but inspection-only.
    #[must_use]
    pub fn recovery_available(&self) -> bool {
        self.finish_available() || self.restore_previous_available()
    }

    #[must_use]
    pub fn finish_available(&self) -> bool {
        cfg!(windows)
            && matches!(self.authority, InterruptedSaveAuthority::SchemaV2 { .. })
            && matches!(
                self.status.state,
                InterruptedSaveState::ReplacementReady
                    | InterruptedSaveState::PriorPreserved
                    | InterruptedSaveState::ReplacementCommitted
            )
    }

    #[must_use]
    pub fn restore_previous_available(&self) -> bool {
        cfg!(windows)
            && matches!(self.authority, InterruptedSaveAuthority::SchemaV2 { .. })
            && !matches!(self.status.state, InterruptedSaveState::Ambiguous)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptedSaveRecoveryChoice {
    Finish,
    RestorePrevious,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InterruptedSaveRecoveryOutcome {
    Finished(PackageFingerprint),
    RestoredPrevious(PackageFingerprint),
    NoLongerPending,
}

/// A failed save and authority limited to the exact journals authored by that attempt.
#[derive(Debug)]
pub struct SaveAttemptFailure {
    pub error: PackageError,
    pub retry: Option<InterruptedSaveCandidate>,
}

/// Saves while retaining retry authority for a safely inspectable failed attempt.
/// Existing or changed journals never grant authority to the caller.
///
/// # Errors
/// Returns the save error and, only for this call's exact recoverable transaction,
/// an opaque candidate that must still pass recovery's full identity checks.
pub fn save_existing_package_retryable(
    destination: &Path,
    expected: &PackageFingerprint,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
) -> Result<PackageFingerprint, Box<SaveAttemptFailure>> {
    save_existing_package_retryable_with_cancel(destination, expected, snapshot, assets, || false)
}

/// Checks cancellation at archive staging boundaries, before atomic publication.
/// A cancelled owned transaction restores its exact previous generation where supported.
/// Once publication begins, verification completes and its real result is returned.
///
/// # Errors
/// Returns the save failure and exact retry authority if safe restoration cannot complete.
pub fn save_existing_package_retryable_with_cancel(
    destination: &Path,
    expected: &PackageFingerprint,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
    cancelled: impl Fn() -> bool,
) -> Result<PackageFingerprint, Box<SaveAttemptFailure>> {
    if cancelled() {
        return Err(Box::new(SaveAttemptFailure { error: cancellation_error(), retry: None }));
    }
    let mut authored = None;
    save_existing_with_journal_observer(
        destination,
        expected,
        snapshot,
        assets,
        |point| {
            if matches!(point, SaveFaultPoint::JournalDurable | SaveFaultPoint::ReplacementFileCreated
                | SaveFaultPoint::ReplacementManifestWritten | SaveFaultPoint::ReplacementAssetWritten(_)
                | SaveFaultPoint::ReplacementFlushed | SaveFaultPoint::ReplacementValidated
                | SaveFaultPoint::ReplacementRecorded) && cancelled() {
                Err(cancellation_error())
            } else { Ok(()) }
        },
        |intent, ready| {
            authored = Some(InterruptedSaveAuthority::SchemaV2 {
                intent: Box::new(intent.clone()),
                ready: ready.cloned().map(Box::new),
            });
        },
    )
    .map_err(|error| {
        let retry = authored.and_then(|authority| {
            inspect_interrupted_save(destination)
                .ok()
                .flatten()
                .filter(|candidate| {
                    candidate.authority == authority && candidate.recovery_available()
                })
        });
        #[cfg(windows)]
        let retry = if matches!(&error, PackageError::Io(source) if source.kind() == std::io::ErrorKind::Interrupted) {
            retry.filter(|candidate| {
                !matches!(recover_interrupted_save(candidate, InterruptedSaveRecoveryChoice::RestorePrevious),
                    Ok(InterruptedSaveRecoveryOutcome::RestoredPrevious(fingerprint)) if &fingerprint == expected)
            })
        } else { retry };
        Box::new(SaveAttemptFailure { error, retry })
    })
}

fn cancellation_error() -> PackageError {
    PackageError::Io(std::io::Error::new(std::io::ErrorKind::Interrupted, "Save cancelled before the publication was replaced"))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SaveOperation {
    ReplaceExisting,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SavePhase {
    IntentRecorded,
    ReplacementVerified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArchiveIdentity {
    sha256: String,
    byte_length: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SaveJournal {
    schema_version: u32,
    operation: SaveOperation,
    phase: SavePhase,
    operation_id: String,
    destination_name: String,
    prior_name: String,
    replacement_name: String,
    receipt_name: String,
    completed_intent_name: String,
    prior: ArchiveIdentity,
    replacement: Option<ArchiveIdentity>,
    document_id: String,
    revision: u64,
    semantic_digest: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum LegacySavePhaseV1 {
    IntentRecorded,
    ReplacementVerified,
    PriorPreserved,
    ReplacementCommitted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct LegacySaveJournalV1 {
    schema_version: u32,
    operation: SaveOperation,
    phase: LegacySavePhaseV1,
    operation_id: String,
    destination_name: String,
    prior_name: String,
    replacement_name: String,
    receipt_name: String,
    prior: ArchiveIdentity,
    replacement: Option<ArchiveIdentity>,
    document_id: String,
    revision: u64,
    semantic_digest: String,
}

#[derive(Deserialize)]
struct JournalSchemaProbe {
    schema_version: u32,
}

#[derive(Clone, Debug)]
struct SavePaths {
    destination: PathBuf,
    journal: PathBuf,
    journal_next: PathBuf,
    replacement: PathBuf,
    prior: PathBuf,
    receipt: PathBuf,
    completed_intent: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdentityMatch {
    Missing,
    Exact,
    Other,
}

pub(crate) fn save_existing(
    destination: &Path,
    expected: &PackageFingerprint,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
    inject: impl FnMut(SaveFaultPoint) -> Result<(), PackageError>,
) -> Result<PackageFingerprint, PackageError> {
    save_existing_with_journal_observer(destination, expected, snapshot, assets, inject, |_, _| {})
}

// The ordering is the durability protocol: keep it visible in one sequence.
#[allow(clippy::too_many_lines)]
fn save_existing_with_journal_observer(
    destination: &Path,
    expected: &PackageFingerprint,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
    mut inject: impl FnMut(SaveFaultPoint) -> Result<(), PackageError>,
    mut journal_authored: impl FnMut(&SaveJournal, Option<&SaveJournal>),
) -> Result<PackageFingerprint, PackageError> {
    #[cfg(windows)]
    let windows_directory =
        publisher_windows_atomic_file::PinnedDirectory::for_destination(destination)?;
    let selected_assets = assets.for_document(&snapshot.document);
    let assets = &selected_assets;
    let (journal_path, journal_next_path) = SavePaths::journal_paths(destination)?;
    for path in [&journal_path, &journal_next_path] {
        if path_exists(path)? {
            return Err(PackageError::InterruptedSavePresent(path.clone()));
        }
    }
    let current = open_package(destination)?;
    if &current.fingerprint != expected {
        return Err(PackageError::DestinationChanged);
    }
    if current.snapshot == *snapshot && current.assets == *assets {
        return Ok(current.fingerprint);
    }
    let prior_identity = ArchiveIdentity::from_opened(destination, &current)?;
    let semantic_digest = digest_hex(snapshot.semantic_digest()?);
    let operation_id = new_operation_id()?;
    let paths = SavePaths::for_transaction(destination, &operation_id)?;
    refuse_interrupted_artifacts(&paths)?;
    let intent = SaveJournal {
        schema_version: JOURNAL_SCHEMA_VERSION,
        operation: SaveOperation::ReplaceExisting,
        phase: SavePhase::IntentRecorded,
        operation_id,
        destination_name: paths.destination_name()?,
        prior_name: paths.prior_name()?,
        replacement_name: paths.replacement_name()?,
        receipt_name: paths.receipt_name()?,
        completed_intent_name: paths.completed_intent_name()?,
        prior: prior_identity.clone(),
        replacement: None,
        document_id: snapshot.document.id.as_str().to_owned(),
        revision: snapshot.revision.value(),
        semantic_digest,
    };
    intent.validate_for(&paths)?;
    create_journal(&paths.journal, &paths, &intent)?;
    journal_authored(&intent, None);
    inject(SaveFaultPoint::JournalDurable)?;

    let replacement_opened =
        write_replacement_archive(&paths.replacement, snapshot, assets, &mut inject)?;
    let replacement_identity =
        ArchiveIdentity::from_opened(&paths.replacement, &replacement_opened)?;
    let ready = SaveJournal {
        phase: SavePhase::ReplacementVerified,
        replacement: Some(replacement_identity.clone()),
        ..intent.clone()
    };
    create_journal(&paths.journal_next, &paths, &ready)?;
    journal_authored(&intent, Some(&ready));
    inject(SaveFaultPoint::ReplacementRecorded)?;

    ensure_destination_identity(destination, &prior_identity)?;

    #[cfg(windows)]
    let committed_windows = commit_replacement_windows(
        &windows_directory,
        &paths,
        &prior_identity,
        &replacement_identity,
        &mut inject,
    )?;
    #[cfg(unix)]
    {
        copy_independent_prior(
            &paths.destination,
            &paths.prior,
            &prior_identity,
            &mut inject,
        )?;
        inject(SaveFaultPoint::PriorPreserved)?;
        ensure_destination_identity(destination, &prior_identity)?;
        inject(SaveFaultPoint::BeforeExchange)?;
        exchange_paths(&paths.destination, &paths.replacement)?;
        sync_parent(&paths.destination)?;
        inject(SaveFaultPoint::AfterExchange)?;
        if identity_match(&paths.replacement, &prior_identity) != IdentityMatch::Exact {
            exchange_paths(&paths.destination, &paths.replacement)?;
            sync_parent(&paths.destination)?;
            return Err(PackageError::DestinationChanged);
        }
        inject(SaveFaultPoint::ReplacementCommitted)?;
    }
    #[cfg(all(not(windows), not(unix)))]
    return Err(PackageError::AtomicExchangeUnavailable);

    #[cfg(windows)]
    return complete_windows_save(
        &windows_directory,
        &committed_windows,
        &paths,
        &ready,
        snapshot,
        assets,
        &mut inject,
    );
    #[cfg(not(windows))]
    {
        let fingerprint = validate_committed_generations(
            &paths,
            &prior_identity,
            &replacement_identity,
            snapshot,
            assets,
        )?;
        inject(SaveFaultPoint::CommitVerified)?;
        finalize_receipt(&paths, &intent, &ready, &mut inject)?;
        Ok(fingerprint)
    }
}

#[cfg(windows)]
fn commit_replacement_windows(
    directory: &publisher_windows_atomic_file::PinnedDirectory,
    paths: &SavePaths,
    prior_identity: &ArchiveIdentity,
    replacement_identity: &ArchiveIdentity,
    inject: &mut impl FnMut(SaveFaultPoint) -> Result<(), PackageError>,
) -> Result<publisher_windows_atomic_file::CommittedReplacement, PackageError> {
    let prepared =
        directory.prepare_replace(&paths.destination, &paths.replacement, &paths.prior)?;
    validate_prepared_windows(&prepared, prior_identity, replacement_identity)?;
    let preserved = prepared.preserve_prior().map_err(windows_replace_error)?;
    if let Err(source) = inject(SaveFaultPoint::PriorPreserved) {
        let source = std::io::Error::other(source.to_string());
        return Err(windows_replace_error(preserved.restore_after_error(source)));
    }
    let committed = preserved.commit().map_err(windows_replace_error)?;
    inject(SaveFaultPoint::ReplacementCommitted)?;
    Ok(committed)
}

#[cfg(not(windows))]
fn validate_committed_generations(
    paths: &SavePaths,
    prior_identity: &ArchiveIdentity,
    replacement_identity: &ArchiveIdentity,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
) -> Result<PackageFingerprint, PackageError> {
    let committed = open_package(&paths.destination)?;
    let committed_identity = ArchiveIdentity::from_opened(&paths.destination, &committed)?;
    let retained_prior = open_package(&paths.prior)?;
    let retained_prior_identity = ArchiveIdentity::from_opened(&paths.prior, &retained_prior)?;
    #[cfg(unix)]
    let exchanged_original_is_exact =
        identity_match(&paths.replacement, prior_identity) == IdentityMatch::Exact;
    #[cfg(windows)]
    let exchanged_original_is_exact =
        identity_match(&paths.replacement, prior_identity) == IdentityMatch::Missing;
    #[cfg(all(not(windows), not(unix)))]
    let exchanged_original_is_exact = false;
    if committed_identity != *replacement_identity
        || committed.snapshot != *snapshot
        || committed.assets != *assets
        || retained_prior_identity != *prior_identity
        || !exchanged_original_is_exact
    {
        return Err(PackageError::CommittedValidationMismatch);
    }
    Ok(committed.fingerprint)
}

#[cfg(windows)]
fn validate_prepared_windows(
    prepared: &publisher_windows_atomic_file::PreparedReplacement<'_>,
    prior_identity: &ArchiveIdentity,
    replacement_identity: &ArchiveIdentity,
) -> Result<(), PackageError> {
    let destination_file = prepared.destination_file()?;
    let destination = open_package_file(destination_file.try_clone()?)?;
    let staged_file = prepared.staged_file()?;
    let staged = open_package_file(staged_file.try_clone()?)?;
    if ArchiveIdentity::from_file_opened(&destination_file, &destination)? != *prior_identity
        || ArchiveIdentity::from_file_opened(&staged_file, &staged)? != *replacement_identity
    {
        return Err(PackageError::DestinationChanged);
    }
    Ok(())
}

#[cfg(windows)]
fn validate_committed_generations_windows(
    committed: &publisher_windows_atomic_file::CommittedReplacement,
    prior_identity: &ArchiveIdentity,
    replacement_identity: &ArchiveIdentity,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
) -> Result<PackageFingerprint, PackageError> {
    let _named_destination = committed.reopened_destination_file()?;
    let _named_prior = committed.reopened_prior_file()?;
    let destination_file = committed.destination_file()?;
    let destination = open_package_file(destination_file.try_clone()?)?;
    let prior_file = committed.prior_file()?;
    let prior = open_package_file(prior_file.try_clone()?)?;
    if ArchiveIdentity::from_file_opened(&destination_file, &destination)? != *replacement_identity
        || destination.snapshot != *snapshot
        || destination.assets != *assets
        || ArchiveIdentity::from_file_opened(&prior_file, &prior)? != *prior_identity
    {
        return Err(PackageError::CommittedValidationMismatch);
    }
    Ok(destination.fingerprint)
}

#[cfg(windows)]
fn complete_windows_save(
    directory: &publisher_windows_atomic_file::PinnedDirectory,
    committed: &publisher_windows_atomic_file::CommittedReplacement,
    paths: &SavePaths,
    ready: &SaveJournal,
    snapshot: &ProjectSnapshot,
    assets: &AssetStore,
    inject: &mut impl FnMut(SaveFaultPoint) -> Result<(), PackageError>,
) -> Result<PackageFingerprint, PackageError> {
    let intent = SaveJournal {
        phase: SavePhase::IntentRecorded,
        replacement: None,
        ..ready.clone()
    };
    let replacement_identity = ready
        .replacement
        .as_ref()
        .ok_or(PackageError::InvalidSaveJournal)?;
    let fingerprint = validate_committed_generations_windows(
        committed,
        &ready.prior,
        replacement_identity,
        snapshot,
        assets,
    )?;
    inject(SaveFaultPoint::CommitVerified)?;
    finalize_receipt_windows(directory, paths, &intent, ready, inject)?;
    Ok(fingerprint)
}

#[cfg(windows)]
fn windows_replace_error(error: publisher_windows_atomic_file::ReplaceError) -> PackageError {
    let state = match error.state() {
        publisher_windows_atomic_file::ReplaceState::Unchanged => WindowsReplaceState::Unchanged,
        publisher_windows_atomic_file::ReplaceState::Restored => WindowsReplaceState::Restored,
        publisher_windows_atomic_file::ReplaceState::Ambiguous => WindowsReplaceState::Ambiguous,
    };
    PackageError::WindowsAtomicReplace {
        state,
        source: error.into_io_error(),
    }
}

#[cfg(not(windows))]
fn finalize_receipt(
    paths: &SavePaths,
    intent: &SaveJournal,
    ready: &SaveJournal,
    inject: &mut impl FnMut(SaveFaultPoint) -> Result<(), PackageError>,
) -> Result<(), PackageError> {
    inject(SaveFaultPoint::BeforeReceiptFinalization)?;
    move_no_replace(&paths.journal_next, &paths.receipt)?;
    sync_parent(&paths.destination)?;
    if read_journal_at(&paths.receipt)? != *ready {
        return Err(PackageError::InvalidSaveJournal);
    }
    inject(SaveFaultPoint::ReadyReceiptFinalized)?;
    if read_journal_at(&paths.journal)? != *intent {
        return Err(PackageError::InvalidSaveJournal);
    }
    create_journal(&paths.completed_intent, paths, intent)?;
    sync_parent(&paths.destination)?;
    if read_journal_at(&paths.completed_intent)? != *intent {
        return Err(PackageError::InvalidSaveJournal);
    }
    inject(SaveFaultPoint::IntentSourceValidated)?;
    fs::remove_file(&paths.journal)?;
    sync_parent(&paths.destination)?;
    inject(SaveFaultPoint::AfterReceiptFinalized)
}

#[cfg(windows)]
fn finalize_receipt_windows(
    directory: &publisher_windows_atomic_file::PinnedDirectory,
    paths: &SavePaths,
    intent: &SaveJournal,
    ready: &SaveJournal,
    inject: &mut impl FnMut(SaveFaultPoint) -> Result<(), PackageError>,
) -> Result<(), PackageError> {
    inject(SaveFaultPoint::BeforeReceiptFinalization)?;
    let moved_ready = directory.move_file_no_replace(&paths.journal_next, &paths.receipt)?;
    let _named_receipt = moved_ready.reopened_destination_file()?;
    let mut receipt_file = moved_ready.file()?;
    if read_journal_file(&mut receipt_file)? != *ready {
        return Err(PackageError::InvalidSaveJournal);
    }
    inject(SaveFaultPoint::ReadyReceiptFinalized)?;
    let prepared_intent =
        directory.prepare_move_file_no_replace(&paths.journal, &paths.completed_intent)?;
    let mut source_intent = prepared_intent.source_file()?;
    if read_journal_file(&mut source_intent)? != *intent {
        return Err(PackageError::InvalidSaveJournal);
    }
    inject(SaveFaultPoint::IntentSourceValidated)?;
    let moved_intent = prepared_intent.commit()?;
    let _named_intent = moved_intent.reopened_destination_file()?;
    let mut intent_file = moved_intent.file()?;
    if read_journal_file(&mut intent_file)? != *intent {
        return Err(PackageError::InvalidSaveJournal);
    }
    inject(SaveFaultPoint::AfterReceiptFinalized)
}

/// Classifies a bounded interrupted save without changing any archive generation.
///
/// # Errors
///
/// Returns [`PackageError::InvalidSaveJournal`] when the marker is malformed,
/// unbounded, or inconsistent with its deterministic destination identity, and
/// returns an I/O or path error when the sibling state cannot be inspected.
pub fn inspect_interrupted_save(
    destination: &Path,
) -> Result<Option<InterruptedSaveCandidate>, PackageError> {
    let (journal_path, journal_next_path) = SavePaths::journal_paths(destination)?;
    let journal_exists = path_exists(&journal_path)?;
    let next_exists = path_exists(&journal_next_path)?;
    if !journal_exists {
        return if next_exists {
            Err(PackageError::InvalidSaveJournal)
        } else {
            Ok(None)
        };
    }
    let journal_bytes = read_journal_bytes_at(&journal_path)?;
    let schema: JournalSchemaProbe =
        serde_json::from_slice(&journal_bytes).map_err(|_| PackageError::InvalidSaveJournal)?;
    if schema.schema_version == 1 {
        return inspect_legacy_v1(
            destination,
            &journal_path,
            &journal_next_path,
            &journal_bytes,
        );
    }
    let intent: SaveJournal =
        serde_json::from_slice(&journal_bytes).map_err(|_| PackageError::InvalidSaveJournal)?;
    let paths = SavePaths::from_journal(destination, &intent)?;
    intent.validate_for(&paths)?;
    if intent.phase != SavePhase::IntentRecorded {
        return Err(PackageError::InvalidSaveJournal);
    }
    let receipt_exists = path_exists(&paths.receipt)?;
    let completed_intent_exists = path_exists(&paths.completed_intent)?;
    let mut recovery_ready = None;
    let state = if next_exists {
        let ready = read_journal_at(&journal_next_path)?;
        validate_journal_pair(&intent, &ready, &paths)?;
        recovery_ready = Some(ready.clone());
        if receipt_exists && read_journal_at(&paths.receipt)? != ready {
            return Err(PackageError::InvalidSaveJournal);
        }
        if completed_intent_exists && read_journal_at(&paths.completed_intent)? != intent {
            return Err(PackageError::InvalidSaveJournal);
        }
        if receipt_exists || completed_intent_exists {
            InterruptedSaveState::Ambiguous
        } else {
            classify(&paths, &ready)
        }
    } else if receipt_exists {
        let ready = read_journal_at(&paths.receipt)?;
        validate_journal_pair(&intent, &ready, &paths)?;
        if completed_intent_exists {
            if read_journal_at(&paths.completed_intent)? != intent {
                return Err(PackageError::InvalidSaveJournal);
            }
            InterruptedSaveState::Ambiguous
        } else {
            recovery_ready = Some(ready.clone());
            classify(&paths, &ready)
        }
    } else if completed_intent_exists {
        if read_journal_at(&paths.completed_intent)? != intent {
            return Err(PackageError::InvalidSaveJournal);
        }
        InterruptedSaveState::Ambiguous
    } else {
        classify(&paths, &intent)
    };
    Ok(Some(InterruptedSaveCandidate {
        status: InterruptedSaveStatus {
            state,
            document_id: DocumentId::parse(intent.document_id.clone())
                .map_err(|_| PackageError::InvalidSaveJournal)?,
            revision: Revision::new(intent.revision),
            semantic_digest: intent.semantic_digest.clone(),
        },
        destination: destination.to_path_buf(),
        authority: InterruptedSaveAuthority::SchemaV2 {
            intent: Box::new(intent),
            ready: recovery_ready.map(Box::new),
        },
    }))
}

/// Opens a publication for editing only when no interrupted save is visible before or after the
/// package read.
///
/// The second inspection detects the ordinary race in which a save journal appears while the
/// archive is being validated. Callers must not bind the returned package to an editing session
/// until this function succeeds.
///
/// # Errors
///
/// Returns [`PackageError::InterruptedSavePresent`] when either inspection finds an interrupted
/// save, or propagates journal and package validation failures without changing any file.
pub fn open_package_for_editing(destination: &Path) -> Result<OpenedPackage, PackageError> {
    open_package_for_editing_with(destination, || Ok(()))
}

fn open_package_for_editing_with(
    destination: &Path,
    after_package_opened: impl FnOnce() -> Result<(), PackageError>,
) -> Result<OpenedPackage, PackageError> {
    refuse_interrupted_open(destination)?;
    let opened = open_package(destination)?;
    after_package_opened()?;
    refuse_interrupted_open(destination)?;
    Ok(opened)
}

fn refuse_interrupted_open(destination: &Path) -> Result<(), PackageError> {
    if inspect_interrupted_save(destination)?.is_some() {
        return Err(PackageError::InterruptedSavePresent(
            destination.to_path_buf(),
        ));
    }
    Ok(())
}

/// Resolves the exact schema-v2 interrupted save represented by `candidate`.
///
/// The candidate is deliberately short-lived: journals and archive identities are inspected
/// again immediately before any mutation. Legacy schema-v1 journals remain inspection-only.
/// Ambiguous states are never mutated.
///
/// # Errors
///
/// Returns a typed recovery error if the candidate changed, is ambiguous, uses a read-only
/// journal schema, requests an unavailable transition, or the platform cannot provide the
/// required race-resistant mutation primitives.
pub fn recover_interrupted_save(
    candidate: &InterruptedSaveCandidate,
    choice: InterruptedSaveRecoveryChoice,
) -> Result<InterruptedSaveRecoveryOutcome, PackageError> {
    let Some(current) = inspect_interrupted_save(&candidate.destination)? else {
        return Ok(InterruptedSaveRecoveryOutcome::NoLongerPending);
    };
    if current != *candidate {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    let InterruptedSaveAuthority::SchemaV2 { intent, ready } = &current.authority else {
        return Err(PackageError::InterruptedSaveRecoveryReadOnly);
    };
    if current.state == InterruptedSaveState::Ambiguous {
        return Err(PackageError::InterruptedSaveRecoveryAmbiguous);
    }
    if current.state == InterruptedSaveState::IntentRecorded
        && choice == InterruptedSaveRecoveryChoice::Finish
    {
        return Err(PackageError::InterruptedSaveRecoveryChoiceUnavailable);
    }

    #[cfg(windows)]
    {
        recover_interrupted_save_windows(&current, intent, ready.as_deref(), choice, |_| Ok(()))
    }
    #[cfg(not(windows))]
    {
        let _ = (intent, ready, choice);
        Err(PackageError::InterruptedSaveRecoveryUnsupported)
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_lines)]
fn recover_interrupted_save_windows(
    candidate: &InterruptedSaveCandidate,
    intent: &SaveJournal,
    ready: Option<&SaveJournal>,
    choice: InterruptedSaveRecoveryChoice,
    mut inject: impl FnMut(RecoveryFaultPoint) -> Result<(), PackageError>,
) -> Result<InterruptedSaveRecoveryOutcome, PackageError> {
    let paths = SavePaths::from_journal(&candidate.destination, intent)?;
    let directory =
        publisher_windows_atomic_file::PinnedDirectory::for_destination(&candidate.destination)?;

    if candidate.state == InterruptedSaveState::IntentRecorded {
        let destination = directory.pin_file(&paths.destination)?;
        let prior = validate_pinned_archive(
            &destination,
            &intent.prior,
            &candidate.document_id,
            None,
            None,
        )?;
        let prepared_intent =
            directory.prepare_move_file_no_replace(&paths.journal, &paths.completed_intent)?;
        validate_intent_marker(&prepared_intent, intent)?;
        inject(RecoveryFaultPoint::BeforeArchiveMutation)?;
        let moved = prepared_intent.commit()?;
        validate_moved_marker(&moved, intent)?;
        inject(RecoveryFaultPoint::IntentFinalized)?;
        return Ok(InterruptedSaveRecoveryOutcome::RestoredPrevious(
            prior.fingerprint,
        ));
    }

    let ready = ready.ok_or(PackageError::InvalidSaveJournal)?;
    let replacement_identity = ready
        .replacement
        .as_ref()
        .ok_or(PackageError::InvalidSaveJournal)?;
    let markers = PreparedRecoveryMarkers::new(&directory, &paths, intent, ready)?;

    let outcome = match (candidate.state, choice) {
        (InterruptedSaveState::ReplacementReady, InterruptedSaveRecoveryChoice::Finish) => {
            if path_exists(&paths.prior)? {
                return Err(PackageError::InterruptedSaveRecoveryAmbiguous);
            }
            let prepared =
                directory.prepare_replace(&paths.destination, &paths.replacement, &paths.prior)?;
            validate_prepared_recovery(
                &prepared,
                &intent.prior,
                replacement_identity,
                candidate,
                false,
            )?;
            inject(RecoveryFaultPoint::BeforeArchiveMutation)?;
            let preserved = prepared.preserve_prior().map_err(windows_replace_error)?;
            inject(RecoveryFaultPoint::PriorPreserved)?;
            let committed = preserved.commit().map_err(windows_replace_error)?;
            inject(RecoveryFaultPoint::ArchiveCommitted)?;
            let fingerprint = validate_recovered_commit(
                &committed,
                replacement_identity,
                &intent.prior,
                candidate,
            )?;
            InterruptedSaveRecoveryOutcome::Finished(fingerprint)
        }
        (
            InterruptedSaveState::ReplacementReady,
            InterruptedSaveRecoveryChoice::RestorePrevious,
        ) => {
            let destination = directory.pin_file(&paths.destination)?;
            let replacement = directory.pin_file(&paths.replacement)?;
            let prior = validate_pinned_archive(
                &destination,
                &intent.prior,
                &candidate.document_id,
                None,
                None,
            )?;
            validate_pinned_archive(
                &replacement,
                replacement_identity,
                &candidate.document_id,
                Some(candidate.revision),
                Some(&candidate.semantic_digest),
            )?;
            inject(RecoveryFaultPoint::BeforeArchiveMutation)?;
            InterruptedSaveRecoveryOutcome::RestoredPrevious(prior.fingerprint)
        }
        (InterruptedSaveState::PriorPreserved, InterruptedSaveRecoveryChoice::Finish) => {
            let prior = directory.pin_file(&paths.prior)?;
            validate_pinned_archive(&prior, &intent.prior, &candidate.document_id, None, None)?;
            let replacement =
                directory.prepare_move_file_no_replace(&paths.replacement, &paths.destination)?;
            validate_prepared_moved_archive(&replacement, replacement_identity, candidate)?;
            inject(RecoveryFaultPoint::BeforeArchiveMutation)?;
            let moved = replacement.commit()?;
            inject(RecoveryFaultPoint::ArchiveCommitted)?;
            let opened = validate_moved_archive(&moved, replacement_identity, candidate)?;
            prior.reopened_file()?;
            InterruptedSaveRecoveryOutcome::Finished(opened.fingerprint)
        }
        (InterruptedSaveState::PriorPreserved, InterruptedSaveRecoveryChoice::RestorePrevious) => {
            let replacement = directory.pin_file(&paths.replacement)?;
            validate_pinned_archive(
                &replacement,
                replacement_identity,
                &candidate.document_id,
                Some(candidate.revision),
                Some(&candidate.semantic_digest),
            )?;
            let prior = directory.prepare_move_file_no_replace(&paths.prior, &paths.destination)?;
            let opened = validate_prepared_prior(&prior, &intent.prior, candidate)?;
            inject(RecoveryFaultPoint::BeforeArchiveMutation)?;
            let moved = prior.commit()?;
            inject(RecoveryFaultPoint::ArchiveCommitted)?;
            validate_moved_prior(&moved, &intent.prior, candidate)?;
            replacement.reopened_file()?;
            InterruptedSaveRecoveryOutcome::RestoredPrevious(opened.fingerprint)
        }
        (InterruptedSaveState::ReplacementCommitted, InterruptedSaveRecoveryChoice::Finish) => {
            let destination = directory.pin_file(&paths.destination)?;
            let prior = directory.pin_file(&paths.prior)?;
            let opened = validate_pinned_archive(
                &destination,
                replacement_identity,
                &candidate.document_id,
                Some(candidate.revision),
                Some(&candidate.semantic_digest),
            )?;
            validate_pinned_archive(&prior, &intent.prior, &candidate.document_id, None, None)?;
            inject(RecoveryFaultPoint::BeforeArchiveMutation)?;
            InterruptedSaveRecoveryOutcome::Finished(opened.fingerprint)
        }
        (
            InterruptedSaveState::ReplacementCommitted,
            InterruptedSaveRecoveryChoice::RestorePrevious,
        ) => {
            if path_exists(&paths.replacement)? {
                return Err(PackageError::InterruptedSaveRecoveryAmbiguous);
            }
            let prepared =
                directory.prepare_replace(&paths.destination, &paths.prior, &paths.replacement)?;
            validate_prepared_recovery(
                &prepared,
                replacement_identity,
                &intent.prior,
                candidate,
                true,
            )?;
            inject(RecoveryFaultPoint::BeforeArchiveMutation)?;
            let preserved = prepared.preserve_prior().map_err(windows_replace_error)?;
            inject(RecoveryFaultPoint::PriorPreserved)?;
            let committed = preserved.commit().map_err(windows_replace_error)?;
            inject(RecoveryFaultPoint::ArchiveCommitted)?;
            let fingerprint = validate_restored_commit(
                &committed,
                &intent.prior,
                replacement_identity,
                candidate,
            )?;
            InterruptedSaveRecoveryOutcome::RestoredPrevious(fingerprint)
        }
        _ => return Err(PackageError::InterruptedSaveRecoveryChoiceUnavailable),
    };

    markers.finalize(intent, ready, &mut inject)?;
    Ok(outcome)
}

#[cfg(windows)]
struct PreparedRecoveryMarkers<'a> {
    ready: PreparedReadyMarker<'a>,
    intent: publisher_windows_atomic_file::PreparedMove<'a>,
}

#[cfg(windows)]
enum PreparedReadyMarker<'a> {
    Active(publisher_windows_atomic_file::PreparedMove<'a>),
    Finalized(publisher_windows_atomic_file::PinnedFile),
}

#[cfg(windows)]
impl<'a> PreparedRecoveryMarkers<'a> {
    fn new(
        directory: &'a publisher_windows_atomic_file::PinnedDirectory,
        paths: &SavePaths,
        intent: &SaveJournal,
        ready: &SaveJournal,
    ) -> Result<Self, PackageError> {
        let ready_move = if path_exists(&paths.journal_next)? {
            let prepared =
                directory.prepare_move_file_no_replace(&paths.journal_next, &paths.receipt)?;
            validate_ready_marker(&prepared, ready)?;
            PreparedReadyMarker::Active(prepared)
        } else {
            let receipt = directory.pin_file(&paths.receipt)?;
            validate_pinned_marker(&receipt, ready)?;
            PreparedReadyMarker::Finalized(receipt)
        };
        let intent_move =
            directory.prepare_move_file_no_replace(&paths.journal, &paths.completed_intent)?;
        validate_intent_marker(&intent_move, intent)?;
        Ok(Self {
            ready: ready_move,
            intent: intent_move,
        })
    }

    fn finalize(
        self,
        intent: &SaveJournal,
        ready: &SaveJournal,
        inject: &mut impl FnMut(RecoveryFaultPoint) -> Result<(), PackageError>,
    ) -> Result<(), PackageError> {
        match self.ready {
            PreparedReadyMarker::Active(ready_move) => {
                let moved_ready = ready_move.commit()?;
                validate_moved_marker(&moved_ready, ready)?;
                inject(RecoveryFaultPoint::ReadyReceiptFinalized)?;
            }
            PreparedReadyMarker::Finalized(receipt) => {
                validate_pinned_marker(&receipt, ready)?;
            }
        }
        let moved_intent = self.intent.commit()?;
        validate_moved_marker(&moved_intent, intent)?;
        inject(RecoveryFaultPoint::IntentFinalized)
    }
}

#[cfg(windows)]
fn validate_pinned_archive(
    pinned: &publisher_windows_atomic_file::PinnedFile,
    expected_identity: &ArchiveIdentity,
    expected_document: &DocumentId,
    expected_revision: Option<Revision>,
    expected_semantic_digest: Option<&str>,
) -> Result<OpenedPackage, PackageError> {
    let file = pinned.file()?;
    let opened = open_package_file(file.try_clone()?)?;
    if ArchiveIdentity::from_file_opened(&file, &opened)? != *expected_identity
        || opened.snapshot.document.id != *expected_document
        || expected_revision.is_some_and(|revision| opened.snapshot.revision != revision)
        || expected_semantic_digest.is_some_and(|digest| {
            opened
                .snapshot
                .semantic_digest()
                .map(digest_hex)
                .map_or(true, |found| found != digest)
        })
    {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    pinned.reopened_file()?;
    Ok(opened)
}

#[cfg(windows)]
fn validate_prepared_recovery(
    prepared: &publisher_windows_atomic_file::PreparedReplacement<'_>,
    destination_identity: &ArchiveIdentity,
    staged_identity: &ArchiveIdentity,
    candidate: &InterruptedSaveCandidate,
    destination_is_replacement: bool,
) -> Result<(), PackageError> {
    let destination_file = prepared.destination_file()?;
    let destination = open_package_file(destination_file.try_clone()?)?;
    let staged_file = prepared.staged_file()?;
    let staged = open_package_file(staged_file.try_clone()?)?;
    if ArchiveIdentity::from_file_opened(&destination_file, &destination)? != *destination_identity
        || ArchiveIdentity::from_file_opened(&staged_file, &staged)? != *staged_identity
    {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    let (replacement, prior) = if destination_is_replacement {
        (&destination, &staged)
    } else {
        (&staged, &destination)
    };
    validate_replacement_semantics(replacement, candidate)?;
    validate_prior_semantics(prior, candidate)
}

#[cfg(windows)]
fn validate_replacement_semantics(
    opened: &OpenedPackage,
    candidate: &InterruptedSaveCandidate,
) -> Result<(), PackageError> {
    if opened.snapshot.document.id != candidate.document_id
        || opened.snapshot.revision != candidate.revision
        || digest_hex(opened.snapshot.semantic_digest()?) != candidate.semantic_digest
    {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    Ok(())
}

#[cfg(windows)]
fn validate_prior_semantics(
    opened: &OpenedPackage,
    candidate: &InterruptedSaveCandidate,
) -> Result<(), PackageError> {
    if opened.snapshot.document.id != candidate.document_id
        || opened.snapshot.revision.value() > candidate.revision.value()
    {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    let _validated_digest = opened.snapshot.semantic_digest()?;
    Ok(())
}

#[cfg(windows)]
fn validate_prepared_moved_archive(
    prepared: &publisher_windows_atomic_file::PreparedMove<'_>,
    identity: &ArchiveIdentity,
    candidate: &InterruptedSaveCandidate,
) -> Result<(), PackageError> {
    let file = prepared.source_file()?;
    let opened = open_package_file(file.try_clone()?)?;
    if ArchiveIdentity::from_file_opened(&file, &opened)? != *identity {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    validate_replacement_semantics(&opened, candidate)
}

#[cfg(windows)]
fn validate_moved_archive(
    moved: &publisher_windows_atomic_file::MovedFile,
    identity: &ArchiveIdentity,
    candidate: &InterruptedSaveCandidate,
) -> Result<OpenedPackage, PackageError> {
    let file = moved.file()?;
    let opened = open_package_file(file.try_clone()?)?;
    if ArchiveIdentity::from_file_opened(&file, &opened)? != *identity {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    validate_replacement_semantics(&opened, candidate)?;
    moved.reopened_destination_file()?;
    Ok(opened)
}

#[cfg(windows)]
fn validate_prepared_prior(
    prepared: &publisher_windows_atomic_file::PreparedMove<'_>,
    identity: &ArchiveIdentity,
    candidate: &InterruptedSaveCandidate,
) -> Result<OpenedPackage, PackageError> {
    let file = prepared.source_file()?;
    let opened = open_package_file(file.try_clone()?)?;
    if ArchiveIdentity::from_file_opened(&file, &opened)? != *identity {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    validate_prior_semantics(&opened, candidate)?;
    Ok(opened)
}

#[cfg(windows)]
fn validate_moved_prior(
    moved: &publisher_windows_atomic_file::MovedFile,
    identity: &ArchiveIdentity,
    candidate: &InterruptedSaveCandidate,
) -> Result<(), PackageError> {
    let file = moved.file()?;
    let opened = open_package_file(file.try_clone()?)?;
    if ArchiveIdentity::from_file_opened(&file, &opened)? != *identity {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    validate_prior_semantics(&opened, candidate)?;
    moved.reopened_destination_file()?;
    Ok(())
}

#[cfg(windows)]
fn validate_recovered_commit(
    committed: &publisher_windows_atomic_file::CommittedReplacement,
    replacement_identity: &ArchiveIdentity,
    prior_identity: &ArchiveIdentity,
    candidate: &InterruptedSaveCandidate,
) -> Result<PackageFingerprint, PackageError> {
    let destination_file = committed.destination_file()?;
    let destination = open_package_file(destination_file.try_clone()?)?;
    let prior_file = committed.prior_file()?;
    let prior = open_package_file(prior_file.try_clone()?)?;
    if ArchiveIdentity::from_file_opened(&destination_file, &destination)? != *replacement_identity
        || ArchiveIdentity::from_file_opened(&prior_file, &prior)? != *prior_identity
    {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    validate_replacement_semantics(&destination, candidate)?;
    validate_prior_semantics(&prior, candidate)?;
    committed.reopened_destination_file()?;
    committed.reopened_prior_file()?;
    Ok(destination.fingerprint)
}

#[cfg(windows)]
fn validate_restored_commit(
    committed: &publisher_windows_atomic_file::CommittedReplacement,
    prior_identity: &ArchiveIdentity,
    replacement_identity: &ArchiveIdentity,
    candidate: &InterruptedSaveCandidate,
) -> Result<PackageFingerprint, PackageError> {
    let destination_file = committed.destination_file()?;
    let destination = open_package_file(destination_file.try_clone()?)?;
    let retained_file = committed.prior_file()?;
    let retained = open_package_file(retained_file.try_clone()?)?;
    if ArchiveIdentity::from_file_opened(&destination_file, &destination)? != *prior_identity
        || ArchiveIdentity::from_file_opened(&retained_file, &retained)? != *replacement_identity
    {
        return Err(PackageError::InterruptedSaveRecoveryChanged);
    }
    validate_prior_semantics(&destination, candidate)?;
    validate_replacement_semantics(&retained, candidate)?;
    committed.reopened_destination_file()?;
    committed.reopened_prior_file()?;
    Ok(destination.fingerprint)
}

#[cfg(windows)]
fn validate_intent_marker(
    prepared: &publisher_windows_atomic_file::PreparedMove<'_>,
    expected: &SaveJournal,
) -> Result<(), PackageError> {
    let mut file = prepared.source_file()?;
    if read_journal_file(&mut file)? != *expected {
        return Err(PackageError::InvalidSaveJournal);
    }
    Ok(())
}

#[cfg(windows)]
fn validate_ready_marker(
    prepared: &publisher_windows_atomic_file::PreparedMove<'_>,
    expected: &SaveJournal,
) -> Result<(), PackageError> {
    validate_intent_marker(prepared, expected)
}

#[cfg(windows)]
fn validate_pinned_marker(
    pinned: &publisher_windows_atomic_file::PinnedFile,
    expected: &SaveJournal,
) -> Result<(), PackageError> {
    let mut file = pinned.file()?;
    if read_journal_file(&mut file)? != *expected {
        return Err(PackageError::InvalidSaveJournal);
    }
    pinned.reopened_file()?;
    Ok(())
}

#[cfg(windows)]
fn validate_moved_marker(
    moved: &publisher_windows_atomic_file::MovedFile,
    expected: &SaveJournal,
) -> Result<(), PackageError> {
    let mut file = moved.file()?;
    if read_journal_file(&mut file)? != *expected {
        return Err(PackageError::InvalidSaveJournal);
    }
    moved.reopened_destination_file()?;
    Ok(())
}

fn inspect_legacy_v1(
    destination: &Path,
    _journal_path: &Path,
    journal_next_path: &Path,
    bytes: &[u8],
) -> Result<Option<InterruptedSaveCandidate>, PackageError> {
    let journal: LegacySaveJournalV1 =
        serde_json::from_slice(bytes).map_err(|_| PackageError::InvalidSaveJournal)?;
    let paths = SavePaths::for_transaction(destination, &journal.operation_id)?;
    journal.validate_for(&paths)?;
    let mut state = classify_legacy_v1(&paths, &journal);
    let next_exists = path_exists(journal_next_path)?;
    let receipt_exists = path_exists(&paths.receipt)?;
    if next_exists {
        read_legacy_v1_at(journal_next_path)?.validate_for(&paths)?;
    }
    if receipt_exists {
        read_legacy_v1_at(&paths.receipt)?.validate_for(&paths)?;
    }
    if next_exists || receipt_exists {
        state = InterruptedSaveState::Ambiguous;
    }
    Ok(Some(InterruptedSaveCandidate {
        status: InterruptedSaveStatus {
            state,
            document_id: DocumentId::parse(journal.document_id)
                .map_err(|_| PackageError::InvalidSaveJournal)?,
            revision: Revision::new(journal.revision),
            semantic_digest: journal.semantic_digest,
        },
        destination: destination.to_path_buf(),
        authority: InterruptedSaveAuthority::LegacyV1,
    }))
}

fn read_legacy_v1_at(path: &Path) -> Result<LegacySaveJournalV1, PackageError> {
    let bytes = read_journal_bytes_at(path)?;
    serde_json::from_slice(&bytes).map_err(|_| PackageError::InvalidSaveJournal)
}

fn classify_legacy_v1(paths: &SavePaths, journal: &LegacySaveJournalV1) -> InterruptedSaveState {
    let destination_prior = identity_match(&paths.destination, &journal.prior);
    let stable_prior = identity_match(&paths.prior, &journal.prior);
    let replacement = journal.replacement.as_ref();
    let destination_replacement = replacement.map_or(IdentityMatch::Other, |identity| {
        identity_match(&paths.destination, identity)
    });
    let staged_replacement = replacement.map_or(IdentityMatch::Other, |identity| {
        identity_match(&paths.replacement, identity)
    });
    let exchanged_prior = identity_match(&paths.replacement, &journal.prior);
    match journal.phase {
        LegacySavePhaseV1::IntentRecorded
            if destination_prior == IdentityMatch::Exact
                && path_exists(&paths.replacement).ok() == Some(false) =>
        {
            InterruptedSaveState::IntentRecorded
        }
        LegacySavePhaseV1::ReplacementVerified | LegacySavePhaseV1::PriorPreserved
            if destination_prior == IdentityMatch::Exact
                && staged_replacement == IdentityMatch::Exact
                && matches!(stable_prior, IdentityMatch::Missing | IdentityMatch::Exact) =>
        {
            InterruptedSaveState::ReplacementReady
        }
        LegacySavePhaseV1::ReplacementVerified | LegacySavePhaseV1::PriorPreserved
            if destination_prior == IdentityMatch::Missing
                && stable_prior == IdentityMatch::Exact
                && staged_replacement == IdentityMatch::Exact =>
        {
            InterruptedSaveState::PriorPreserved
        }
        LegacySavePhaseV1::ReplacementVerified
        | LegacySavePhaseV1::PriorPreserved
        | LegacySavePhaseV1::ReplacementCommitted
            if destination_replacement == IdentityMatch::Exact
                && stable_prior == IdentityMatch::Exact
                && matches!(
                    staged_replacement,
                    IdentityMatch::Missing | IdentityMatch::Exact
                ) =>
        {
            InterruptedSaveState::ReplacementCommitted
        }
        LegacySavePhaseV1::ReplacementVerified
        | LegacySavePhaseV1::PriorPreserved
        | LegacySavePhaseV1::ReplacementCommitted
            if destination_replacement == IdentityMatch::Exact
                && stable_prior == IdentityMatch::Exact
                && exchanged_prior == IdentityMatch::Exact =>
        {
            InterruptedSaveState::ReplacementCommitted
        }
        _ => InterruptedSaveState::Ambiguous,
    }
}

fn validate_journal_pair(
    intent: &SaveJournal,
    ready: &SaveJournal,
    paths: &SavePaths,
) -> Result<(), PackageError> {
    ready.validate_for(paths)?;
    if ready.phase != SavePhase::ReplacementVerified {
        return Err(PackageError::InvalidSaveJournal);
    }
    let expected_intent = SaveJournal {
        phase: SavePhase::IntentRecorded,
        replacement: None,
        ..ready.clone()
    };
    if *intent != expected_intent {
        return Err(PackageError::InvalidSaveJournal);
    }
    Ok(())
}

fn classify(paths: &SavePaths, journal: &SaveJournal) -> InterruptedSaveState {
    let destination_prior = identity_match(&paths.destination, &journal.prior);
    let stable_prior = identity_match(&paths.prior, &journal.prior);
    let replacement = journal.replacement.as_ref();
    let destination_replacement = replacement.map_or(IdentityMatch::Other, |identity| {
        identity_match(&paths.destination, identity)
    });
    let staged_replacement = replacement.map_or(IdentityMatch::Other, |identity| {
        identity_match(&paths.replacement, identity)
    });
    let exchanged_prior = identity_match(&paths.replacement, &journal.prior);

    match journal.phase {
        SavePhase::IntentRecorded
            if destination_prior == IdentityMatch::Exact
                && path_exists(&paths.replacement).ok() == Some(false) =>
        {
            InterruptedSaveState::IntentRecorded
        }
        SavePhase::ReplacementVerified
            if destination_prior == IdentityMatch::Exact
                && staged_replacement == IdentityMatch::Exact
                && matches!(stable_prior, IdentityMatch::Missing | IdentityMatch::Exact) =>
        {
            InterruptedSaveState::ReplacementReady
        }
        SavePhase::ReplacementVerified
            if destination_prior == IdentityMatch::Missing
                && stable_prior == IdentityMatch::Exact
                && staged_replacement == IdentityMatch::Exact =>
        {
            InterruptedSaveState::PriorPreserved
        }
        SavePhase::ReplacementVerified
            if destination_replacement == IdentityMatch::Exact
                && stable_prior == IdentityMatch::Exact
                && matches!(
                    staged_replacement,
                    IdentityMatch::Missing | IdentityMatch::Exact
                ) =>
        {
            InterruptedSaveState::ReplacementCommitted
        }
        SavePhase::ReplacementVerified
            if destination_replacement == IdentityMatch::Exact
                && stable_prior == IdentityMatch::Exact
                && exchanged_prior == IdentityMatch::Exact =>
        {
            InterruptedSaveState::ReplacementCommitted
        }
        _ => InterruptedSaveState::Ambiguous,
    }
}

fn refuse_interrupted_artifacts(paths: &SavePaths) -> Result<(), PackageError> {
    for path in [
        &paths.journal,
        &paths.journal_next,
        &paths.replacement,
        &paths.prior,
        &paths.receipt,
        &paths.completed_intent,
    ] {
        if path_exists(path)? {
            return Err(PackageError::InterruptedSavePresent(path.clone()));
        }
    }
    Ok(())
}

fn ensure_destination_identity(
    destination: &Path,
    expected: &ArchiveIdentity,
) -> Result<(), PackageError> {
    let current = open_package(destination)?;
    if ArchiveIdentity::from_opened(destination, &current)? != *expected {
        return Err(PackageError::DestinationChanged);
    }
    Ok(())
}

#[cfg(unix)]
fn copy_independent_prior(
    source_path: &Path,
    prior_path: &Path,
    expected: &ArchiveIdentity,
    inject: &mut impl FnMut(SaveFaultPoint) -> Result<(), PackageError>,
) -> Result<(), PackageError> {
    let mut source = File::open(source_path)?;
    let source_metadata = source.metadata()?;
    if !source_metadata.file_type().is_file() || source_metadata.len() != expected.byte_length {
        return Err(PackageError::DestinationChanged);
    }
    let mut prior = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(prior_path)?;
    inject(SaveFaultPoint::PriorFileCreated)?;
    let copied = std::io::copy(
        &mut Read::by_ref(&mut source).take(expected.byte_length + 1),
        &mut prior,
    )?;
    if copied != expected.byte_length {
        return Err(PackageError::DestinationChanged);
    }
    inject(SaveFaultPoint::PriorCopied)?;
    prior.sync_all()?;
    drop(prior);
    sync_parent(prior_path)?;
    inject(SaveFaultPoint::PriorFlushed)?;
    if identity_match(prior_path, expected) != IdentityMatch::Exact {
        return Err(PackageError::DestinationChanged);
    }
    inject(SaveFaultPoint::PriorValidated)?;
    Ok(())
}

fn create_journal(
    path: &Path,
    paths: &SavePaths,
    journal: &SaveJournal,
) -> Result<(), PackageError> {
    let bytes = journal_bytes(journal, paths)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    sync_parent(path)
}

fn journal_bytes(journal: &SaveJournal, paths: &SavePaths) -> Result<Vec<u8>, PackageError> {
    journal.validate_for(paths)?;
    let bytes = serde_json::to_vec_pretty(journal)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(PackageError::InvalidSaveJournal);
    }
    Ok(bytes)
}

fn read_journal_at(journal_path: &Path) -> Result<SaveJournal, PackageError> {
    let bytes = read_journal_bytes_at(journal_path)?;
    serde_json::from_slice(&bytes).map_err(|_| PackageError::InvalidSaveJournal)
}

fn read_journal_bytes_at(journal_path: &Path) -> Result<Vec<u8>, PackageError> {
    let metadata = fs::symlink_metadata(journal_path)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_JOURNAL_BYTES {
        return Err(PackageError::InvalidSaveJournal);
    }
    let mut file = File::open(journal_path)?;
    read_journal_bytes_file(&mut file)
}

#[cfg(windows)]
fn read_journal_file(file: &mut File) -> Result<SaveJournal, PackageError> {
    let bytes = read_journal_bytes_file(file)?;
    serde_json::from_slice(&bytes).map_err(|_| PackageError::InvalidSaveJournal)
}

fn read_journal_bytes_file(file: &mut File) -> Result<Vec<u8>, PackageError> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_JOURNAL_BYTES {
        return Err(PackageError::InvalidSaveJournal);
    }
    #[cfg(windows)]
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(PackageError::InvalidSaveJournal);
    }
    Ok(bytes)
}

impl SaveJournal {
    fn validate_for(&self, paths: &SavePaths) -> Result<(), PackageError> {
        if self.schema_version != JOURNAL_SCHEMA_VERSION
            || self.operation != SaveOperation::ReplaceExisting
            || !is_lower_hex_digest(&self.operation_id)
            || self.destination_name != paths.destination_name()?
            || self.prior_name != paths.prior_name()?
            || self.replacement_name != paths.replacement_name()?
            || self.receipt_name != paths.receipt_name()?
            || self.completed_intent_name != paths.completed_intent_name()?
            || self.document_id.len() > 128
            || DocumentId::parse(self.document_id.clone()).is_err()
            || !is_lower_hex_digest(&self.semantic_digest)
            || self.prior.validate().is_err()
            || self
                .replacement
                .as_ref()
                .is_some_and(|identity| identity.validate().is_err())
            || (self.phase == SavePhase::IntentRecorded && self.replacement.is_some())
            || (self.phase != SavePhase::IntentRecorded && self.replacement.is_none())
        {
            return Err(PackageError::InvalidSaveJournal);
        }
        Ok(())
    }
}

impl LegacySaveJournalV1 {
    fn validate_for(&self, paths: &SavePaths) -> Result<(), PackageError> {
        if self.schema_version != 1
            || self.operation != SaveOperation::ReplaceExisting
            || !is_lower_hex_digest(&self.operation_id)
            || self.destination_name != paths.destination_name()?
            || self.prior_name != paths.prior_name()?
            || self.replacement_name != paths.replacement_name()?
            || self.receipt_name != paths.receipt_name()?
            || self.document_id.len() > 128
            || DocumentId::parse(self.document_id.clone()).is_err()
            || !is_lower_hex_digest(&self.semantic_digest)
            || self.prior.validate().is_err()
            || self
                .replacement
                .as_ref()
                .is_some_and(|identity| identity.validate().is_err())
            || (self.phase == LegacySavePhaseV1::IntentRecorded && self.replacement.is_some())
            || (self.phase != LegacySavePhaseV1::IntentRecorded && self.replacement.is_none())
        {
            return Err(PackageError::InvalidSaveJournal);
        }
        Ok(())
    }
}

impl ArchiveIdentity {
    fn from_opened(path: &Path, opened: &OpenedPackage) -> Result<Self, PackageError> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_ARCHIVE_FILE_BYTES {
            return Err(PackageError::InvalidSaveJournal);
        }
        Ok(Self {
            sha256: opened.fingerprint.as_str().to_owned(),
            byte_length: metadata.len(),
        })
    }

    #[cfg(windows)]
    fn from_file_opened(file: &File, opened: &OpenedPackage) -> Result<Self, PackageError> {
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_ARCHIVE_FILE_BYTES {
            return Err(PackageError::InvalidSaveJournal);
        }
        Ok(Self {
            sha256: opened.fingerprint.as_str().to_owned(),
            byte_length: metadata.len(),
        })
    }

    fn validate(&self) -> Result<(), PackageError> {
        PackageFingerprint::parse(self.sha256.clone())?;
        if self.byte_length == 0 || self.byte_length > MAX_ARCHIVE_FILE_BYTES {
            return Err(PackageError::InvalidSaveJournal);
        }
        Ok(())
    }
}

impl SavePaths {
    fn journal_paths(destination: &Path) -> Result<(PathBuf, PathBuf), PackageError> {
        let parent = destination
            .parent()
            .ok_or(PackageError::DestinationHasNoParent)?;
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or(PackageError::InvalidDestinationName)?;
        Ok((
            parent.join(format!(".{name}.save-journal.json")),
            parent.join(format!(".{name}.save-journal.next")),
        ))
    }

    fn for_transaction(destination: &Path, operation_id: &str) -> Result<Self, PackageError> {
        let parent = destination
            .parent()
            .ok_or(PackageError::DestinationHasNoParent)?;
        if !is_lower_hex_digest(operation_id) {
            return Err(PackageError::InvalidSaveJournal);
        }
        let (journal, journal_next) = Self::journal_paths(destination)?;
        Ok(Self {
            destination: destination.to_path_buf(),
            journal,
            journal_next,
            replacement: parent.join(format!(".publisher-v2-save-{operation_id}.replacement")),
            prior: parent.join(format!(".publisher-v2-save-{operation_id}.prior")),
            receipt: parent.join(format!(".publisher-v2-save-{operation_id}.completed.json")),
            completed_intent: parent.join(format!(
                ".publisher-v2-save-{operation_id}.completed-intent.json"
            )),
        })
    }

    fn from_journal(destination: &Path, journal: &SaveJournal) -> Result<Self, PackageError> {
        let expected = Self::for_transaction(destination, &journal.operation_id)?;
        if expected.prior.file_name().and_then(|name| name.to_str()) != Some(&journal.prior_name)
            || expected
                .replacement
                .file_name()
                .and_then(|name| name.to_str())
                != Some(&journal.replacement_name)
            || expected.receipt.file_name().and_then(|name| name.to_str())
                != Some(&journal.receipt_name)
            || expected
                .completed_intent
                .file_name()
                .and_then(|name| name.to_str())
                != Some(&journal.completed_intent_name)
        {
            return Err(PackageError::InvalidSaveJournal);
        }
        Ok(expected)
    }

    fn destination_name(&self) -> Result<String, PackageError> {
        self.destination
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or(PackageError::InvalidDestinationName)
    }

    fn prior_name(&self) -> Result<String, PackageError> {
        self.prior
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or(PackageError::InvalidDestinationName)
    }

    fn replacement_name(&self) -> Result<String, PackageError> {
        self.replacement
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or(PackageError::InvalidDestinationName)
    }

    fn receipt_name(&self) -> Result<String, PackageError> {
        self.receipt
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or(PackageError::InvalidDestinationName)
    }

    fn completed_intent_name(&self) -> Result<String, PackageError> {
        self.completed_intent
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .ok_or(PackageError::InvalidDestinationName)
    }
}

fn new_operation_id() -> Result<String, PackageError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| PackageError::RandomIdentityUnavailable)?;
    Ok(digest_hex(bytes))
}

fn identity_match(path: &Path, expected: &ArchiveIdentity) -> IdentityMatch {
    if !path_exists(path).unwrap_or(true) {
        return IdentityMatch::Missing;
    }
    let Ok(opened) = open_package(path) else {
        return IdentityMatch::Other;
    };
    match ArchiveIdentity::from_opened(path, &opened) {
        Ok(found) if found == *expected => IdentityMatch::Exact,
        _ => IdentityMatch::Other,
    }
}

fn path_exists(path: &Path) -> Result<bool, PackageError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn digest_hex(bytes: [u8; 32]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            use std::fmt::Write as _;
            write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
            output
        })
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), PackageError> {
    let parent = path.parent().ok_or(PackageError::DestinationHasNoParent)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(all(not(windows), not(unix)))]
fn sync_parent(_path: &Path) -> Result<(), PackageError> {
    Ok(())
}

#[cfg(unix)]
fn exchange_paths(left: &Path, right: &Path) -> Result<(), PackageError> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        left,
        rustix::fs::CWD,
        right,
        rustix::fs::RenameFlags::EXCHANGE,
    )
    .map_err(|error| PackageError::Io(error.into()))
}

#[cfg(unix)]
fn move_no_replace(source: &Path, destination: &Path) -> Result<(), PackageError> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        source,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| PackageError::Io(error.into()))
}

#[cfg(all(not(windows), not(unix)))]
fn move_no_replace(_source: &Path, _destination: &Path) -> Result<(), PackageError> {
    Err(PackageError::AtomicExchangeUnavailable)
}

#[cfg(windows)]
#[allow(clippy::unnecessary_wraps)]
fn sync_parent(_path: &Path) -> Result<(), PackageError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ArchiveIdentity, InterruptedSaveRecoveryChoice, InterruptedSaveState, LegacySavePhaseV1,
        PackageError, SaveFaultPoint, SaveJournal, SaveOperation, SavePaths, SavePhase, digest_hex,
        inspect_interrupted_save, is_lower_hex_digest, open_package_for_editing,
        open_package_for_editing_with, read_journal_at, recover_interrupted_save, save_existing,
    };
    #[cfg(unix)]
    use super::{IdentityMatch, identity_match};
    #[cfg(windows)]
    use super::{
        InterruptedSaveAuthority, InterruptedSaveCandidate, InterruptedSaveRecoveryOutcome,
        RecoveryFaultPoint, recover_interrupted_save_windows,
    };
    use crate::assets::AssetStore;
    #[cfg(windows)]
    use crate::commands::{
        Command as DocumentCommand, DocumentSession, ObjectLocation, Transaction,
    };
    use crate::model::{Document, DocumentId, Insets, PhysicalSize, Revision};
    #[cfg(windows)]
    use crate::model::{
        ObjectDraft, ObjectKind, TextBoxSizingMode, TextFrame, TextStyle, Transform,
    };
    #[cfg(windows)]
    use crate::package::WindowsReplaceState;
    use crate::package::{ProjectSnapshot, open_package, save_existing_package, save_new_package};
    #[cfg(windows)]
    use sha2::{Digest as _, Sha256};
    use std::io::Write as _;
    #[cfg(unix)]
    use std::io::{Seek as _, SeekFrom};
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);
    const CHILD_DESTINATION: &str = "PUBLISHER_V2_ABORT_DESTINATION";
    const CHILD_POINT: &str = "PUBLISHER_V2_ABORT_POINT";
    const CHILD_MARKER: &str = "PUBLISHER_V2_ABORT_REACHED_MARKER";
    #[cfg(windows)]
    const RECOVERY_CHILD_DESTINATION: &str = "PUBLISHER_V2_RECOVERY_ABORT_DESTINATION";
    #[cfg(windows)]
    const RECOVERY_CHILD_POINT: &str = "PUBLISHER_V2_RECOVERY_ABORT_POINT";
    #[cfg(windows)]
    const RECOVERY_CHILD_MARKER: &str = "PUBLISHER_V2_RECOVERY_ABORT_REACHED_MARKER";
    #[cfg(windows)]
    const RECENT_ACCEPTANCE_FIXTURE_ROOT: &str = "PUBLISHER_V2_RECENT_ACCEPTANCE_FIXTURE_ROOT";

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new(label: &str) -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "publisher-v2-journal-{label}-{}-{sequence}",
                std::process::id()
            ));
            if path.exists() {
                std::fs::remove_dir_all(&path).expect("remove stale journal fixture");
            }
            std::fs::create_dir(&path).expect("create journal fixture");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ignored = std::fs::remove_dir_all(&self.0);
        }
    }

    fn snapshot(document_id: &str, title: &str, revision: u64) -> ProjectSnapshot {
        ProjectSnapshot {
            document: Document::new(
                DocumentId::parse(document_id).expect("valid test document id"),
                title,
                PhysicalSize::new(215_900, 279_400).expect("valid trim"),
                Insets::uniform(3_175).expect("valid bleed"),
            )
            .expect("valid test document"),
            revision: Revision::new(revision),
        }
    }

    #[cfg(windows)]
    fn fixed_text_snapshot(document_id: &str, title: &str) -> ProjectSnapshot {
        let mut session = DocumentSession::new(snapshot(document_id, title, 0).document)
            .expect("valid fixed-text test document");
        let page_id = session.document().pages[0].id.clone();
        session
            .execute(
                Revision::initial(),
                Transaction::single(
                    "Add fixed Text Box",
                    DocumentCommand::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: ObjectDraft {
                            name: "Instructions".to_owned(),
                            size: PhysicalSize::new(100_000, 50_000)
                                .expect("valid fixed Text Box size"),
                            transform: Transform::identity_at(10_000, 20_000),
                            opacity_millionths: 1_000_000,
                            kind: ObjectKind::Text {
                                text: "First line\nSecond line".to_owned(),
                                style: TextStyle::default(),
                                frame: TextFrame {
                                    sizing_mode: TextBoxSizingMode::Fixed,
                                    ..TextFrame::default()
                                },
                                story: crate::model::TextStoryFormatting::default(),
                            },
                        },
                    },
                ),
            )
            .expect("add fixed Text Box");
        ProjectSnapshot::from_session(&session)
    }

    #[cfg(windows)]
    fn schema_ten_semantic_digest(snapshot: &ProjectSnapshot) -> String {
        let bytes = serde_json::to_vec(snapshot).expect("serialize legacy-compatible snapshot");
        assert!(
            !bytes
                .windows(b"\"sizing_mode\"".len())
                .any(|window| window == b"\"sizing_mode\""),
            "schema-ten Fixed Text Frames did not serialize a sizing-mode field"
        );
        let mut digest = Sha256::new();
        digest.update(b"publisher-v2/project-snapshot-semantics/v2\0");
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
        format!("{:x}", digest.finalize())
    }

    fn fixture(label: &str) -> (TempDirectory, PathBuf, crate::PackageFingerprint) {
        fixture_named(label, "book.publisher-v2")
    }

    fn fixture_named(
        label: &str,
        destination_name: &str,
    ) -> (TempDirectory, PathBuf, crate::PackageFingerprint) {
        let temp = TempDirectory::new(label);
        let destination = temp.path().join(destination_name);
        let fingerprint = save_new_package(
            &destination,
            &snapshot("journal-document", "Initial", 0),
            &AssetStore::default(),
        )
        .expect("save initial journal fixture");
        (temp, destination, fingerprint)
    }

    fn fault_point_name(point: SaveFaultPoint) -> String {
        match point {
            SaveFaultPoint::JournalDurable => "journal_durable".to_owned(),
            SaveFaultPoint::ReplacementFileCreated => "replacement_file_created".to_owned(),
            SaveFaultPoint::ReplacementManifestWritten => "replacement_manifest_written".to_owned(),
            SaveFaultPoint::ReplacementAssetWritten(index) => {
                format!("replacement_asset_written_{index}")
            }
            SaveFaultPoint::ReplacementFlushed => "replacement_flushed".to_owned(),
            SaveFaultPoint::ReplacementValidated => "replacement_validated".to_owned(),
            SaveFaultPoint::ReplacementRecorded => "replacement_recorded".to_owned(),
            #[cfg(unix)]
            SaveFaultPoint::PriorFileCreated => "prior_file_created".to_owned(),
            #[cfg(unix)]
            SaveFaultPoint::PriorCopied => "prior_copied".to_owned(),
            #[cfg(unix)]
            SaveFaultPoint::PriorFlushed => "prior_flushed".to_owned(),
            #[cfg(unix)]
            SaveFaultPoint::PriorValidated => "prior_validated".to_owned(),
            SaveFaultPoint::PriorPreserved => "prior_preserved".to_owned(),
            #[cfg(unix)]
            SaveFaultPoint::BeforeExchange => "before_exchange".to_owned(),
            #[cfg(unix)]
            SaveFaultPoint::AfterExchange => "after_exchange".to_owned(),
            SaveFaultPoint::ReplacementCommitted => "replacement_committed".to_owned(),
            SaveFaultPoint::CommitVerified => "commit_verified".to_owned(),
            SaveFaultPoint::BeforeReceiptFinalization => "before_receipt_finalization".to_owned(),
            SaveFaultPoint::ReadyReceiptFinalized => "ready_receipt_finalized".to_owned(),
            SaveFaultPoint::IntentSourceValidated => "intent_source_validated".to_owned(),
            SaveFaultPoint::AfterReceiptFinalized => "after_receipt_finalized".to_owned(),
        }
    }

    fn parse_fault_point(value: &str) -> SaveFaultPoint {
        all_fault_points()
            .into_iter()
            .find(|point| fault_point_name(*point) == value)
            .expect("known child fault point")
    }

    fn all_fault_points() -> Vec<SaveFaultPoint> {
        let points = vec![
            SaveFaultPoint::JournalDurable,
            SaveFaultPoint::ReplacementFileCreated,
            SaveFaultPoint::ReplacementManifestWritten,
            SaveFaultPoint::ReplacementAssetWritten(0),
            SaveFaultPoint::ReplacementAssetWritten(1),
            SaveFaultPoint::ReplacementAssetWritten(2),
            SaveFaultPoint::ReplacementFlushed,
            SaveFaultPoint::ReplacementValidated,
            SaveFaultPoint::ReplacementRecorded,
            SaveFaultPoint::PriorPreserved,
            SaveFaultPoint::ReplacementCommitted,
            SaveFaultPoint::CommitVerified,
            SaveFaultPoint::BeforeReceiptFinalization,
            SaveFaultPoint::ReadyReceiptFinalized,
            SaveFaultPoint::IntentSourceValidated,
            SaveFaultPoint::AfterReceiptFinalized,
        ];
        #[cfg(unix)]
        let mut points = points;
        #[cfg(unix)]
        {
            let prior_index = points
                .iter()
                .position(|point| *point == SaveFaultPoint::PriorPreserved)
                .expect("prior-preserved boundary");
            points.insert(prior_index, SaveFaultPoint::PriorFileCreated);
            points.insert(prior_index + 1, SaveFaultPoint::PriorCopied);
            points.insert(prior_index + 2, SaveFaultPoint::PriorFlushed);
            points.insert(prior_index + 3, SaveFaultPoint::PriorValidated);
            let exchange_index = points
                .iter()
                .position(|point| *point == SaveFaultPoint::ReplacementCommitted)
                .expect("replacement committed boundary");
            points.insert(exchange_index, SaveFaultPoint::BeforeExchange);
            points.insert(exchange_index + 1, SaveFaultPoint::AfterExchange);
        }
        points
    }

    fn expected_state_after_abort(point: SaveFaultPoint) -> Option<InterruptedSaveState> {
        match point {
            SaveFaultPoint::JournalDurable => Some(InterruptedSaveState::IntentRecorded),
            SaveFaultPoint::ReplacementFileCreated
            | SaveFaultPoint::ReplacementManifestWritten
            | SaveFaultPoint::ReplacementAssetWritten(_)
            | SaveFaultPoint::ReplacementFlushed
            | SaveFaultPoint::ReplacementValidated => Some(InterruptedSaveState::Ambiguous),
            #[cfg(unix)]
            SaveFaultPoint::ReplacementRecorded
            | SaveFaultPoint::PriorCopied
            | SaveFaultPoint::PriorFlushed
            | SaveFaultPoint::PriorValidated
            | SaveFaultPoint::BeforeExchange => Some(InterruptedSaveState::ReplacementReady),
            #[cfg(unix)]
            SaveFaultPoint::PriorFileCreated => Some(InterruptedSaveState::Ambiguous),
            #[cfg(not(unix))]
            SaveFaultPoint::ReplacementRecorded => Some(InterruptedSaveState::ReplacementReady),
            SaveFaultPoint::PriorPreserved => {
                if cfg!(windows) {
                    Some(InterruptedSaveState::PriorPreserved)
                } else {
                    Some(InterruptedSaveState::ReplacementReady)
                }
            }
            #[cfg(unix)]
            SaveFaultPoint::AfterExchange
            | SaveFaultPoint::ReplacementCommitted
            | SaveFaultPoint::CommitVerified
            | SaveFaultPoint::BeforeReceiptFinalization => {
                Some(InterruptedSaveState::ReplacementCommitted)
            }
            #[cfg(not(unix))]
            SaveFaultPoint::ReplacementCommitted
            | SaveFaultPoint::CommitVerified
            | SaveFaultPoint::BeforeReceiptFinalization => {
                Some(InterruptedSaveState::ReplacementCommitted)
            }
            SaveFaultPoint::ReadyReceiptFinalized => {
                Some(InterruptedSaveState::ReplacementCommitted)
            }
            #[cfg(windows)]
            SaveFaultPoint::IntentSourceValidated => {
                Some(InterruptedSaveState::ReplacementCommitted)
            }
            #[cfg(not(windows))]
            SaveFaultPoint::IntentSourceValidated => Some(InterruptedSaveState::Ambiguous),
            SaveFaultPoint::AfterReceiptFinalized => None,
        }
    }

    fn write_named_reached_marker(path: &Path, name: &str) {
        let mut marker = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("create boundary marker");
        marker
            .write_all(name.as_bytes())
            .expect("write boundary marker");
        marker.sync_all().expect("sync boundary marker");
        #[cfg(unix)]
        std::fs::File::open(path.parent().expect("marker parent"))
            .expect("open marker parent")
            .sync_all()
            .expect("sync marker parent");
    }

    fn write_reached_marker(path: &Path, point: SaveFaultPoint) {
        write_named_reached_marker(path, &fault_point_name(point));
    }

    #[cfg(windows)]
    fn recovery_fault_point_name(point: RecoveryFaultPoint) -> &'static str {
        match point {
            RecoveryFaultPoint::BeforeArchiveMutation => "before_archive_mutation",
            RecoveryFaultPoint::PriorPreserved => "prior_preserved",
            RecoveryFaultPoint::ArchiveCommitted => "archive_committed",
            RecoveryFaultPoint::ReadyReceiptFinalized => "ready_receipt_finalized",
            RecoveryFaultPoint::IntentFinalized => "intent_finalized",
        }
    }

    #[cfg(windows)]
    fn parse_recovery_fault_point(value: &str) -> RecoveryFaultPoint {
        [
            RecoveryFaultPoint::BeforeArchiveMutation,
            RecoveryFaultPoint::PriorPreserved,
            RecoveryFaultPoint::ArchiveCommitted,
            RecoveryFaultPoint::ReadyReceiptFinalized,
            RecoveryFaultPoint::IntentFinalized,
        ]
        .into_iter()
        .find(|point| recovery_fault_point_name(*point) == value)
        .expect("known recovery child fault point")
    }

    #[cfg(windows)]
    fn leave_schema_v2_state(
        label: &str,
        state: InterruptedSaveState,
    ) -> (
        TempDirectory,
        PathBuf,
        crate::PackageFingerprint,
        InterruptedSaveCandidate,
        SavePaths,
    ) {
        leave_schema_v2_state_named(label, state, "book.publisher-v2")
    }

    #[cfg(windows)]
    fn leave_schema_v2_state_named(
        label: &str,
        state: InterruptedSaveState,
        destination_name: &str,
    ) -> (
        TempDirectory,
        PathBuf,
        crate::PackageFingerprint,
        InterruptedSaveCandidate,
        SavePaths,
    ) {
        let (temp, destination, initial) = fixture_named(label, destination_name);
        let stop = if state == InterruptedSaveState::IntentRecorded {
            SaveFaultPoint::JournalDurable
        } else {
            SaveFaultPoint::ReplacementRecorded
        };
        let interrupted = save_existing(
            &destination,
            &initial,
            &snapshot("journal-document", "Replacement", 1),
            &AssetStore::default(),
            |point| {
                if point == stop {
                    Err(PackageError::Io(std::io::Error::other(
                        "leave requested recovery state",
                    )))
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("interrupt transaction at requested state");
        assert!(
            matches!(&interrupted, PackageError::Io(error) if error.to_string() == "leave requested recovery state"),
            "transaction must reach requested fault point, got {interrupted:?}"
        );
        let (_journal, paths) = journal_and_paths(&destination);
        if state == InterruptedSaveState::PriorPreserved
            || state == InterruptedSaveState::ReplacementCommitted
        {
            std::fs::rename(&destination, &paths.prior).expect("preserve exact prior fixture");
        }
        if state == InterruptedSaveState::ReplacementCommitted {
            std::fs::rename(&paths.replacement, &destination)
                .expect("commit exact replacement fixture");
        }
        let candidate = inspect_interrupted_save(&destination)
            .expect("inspect requested state")
            .expect("pending recovery candidate");
        assert_eq!(candidate.state, state);
        (temp, destination, initial, candidate, paths)
    }

    #[cfg(windows)]
    fn write_recent_acceptance_fixture(
        root: &Path,
        directory_name: &str,
        document_id: &str,
        initial_title: &str,
        replacement_title: &str,
    ) -> serde_json::Value {
        let directory = root.join(directory_name);
        std::fs::create_dir(&directory).expect("create isolated Recent acceptance fixture");
        let destination = directory.join(format!("{directory_name}.publisher-v2"));
        let initial = save_new_package(
            &destination,
            &snapshot(document_id, initial_title, 0),
            &AssetStore::default(),
        )
        .expect("save prior Recent acceptance publication");
        let interrupted = save_existing(
            &destination,
            &initial,
            &snapshot(document_id, replacement_title, 1),
            &AssetStore::default(),
            |point| {
                if point == SaveFaultPoint::ReplacementRecorded {
                    Err(PackageError::Io(std::io::Error::other(
                        "retain replacement-ready Recent acceptance fixture",
                    )))
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("interrupt fixture after replacement validation");
        assert!(
            matches!(&interrupted, PackageError::Io(error) if error.to_string() == "retain replacement-ready Recent acceptance fixture"),
            "fixture must stop at the requested boundary, got {interrupted:?}"
        );
        let candidate = inspect_interrupted_save(&destination)
            .expect("inspect retained Recent acceptance fixture")
            .expect("fixture has interrupted-save evidence");
        assert_eq!(candidate.state, InterruptedSaveState::ReplacementReady);
        assert!(candidate.finish_available());
        assert!(candidate.restore_previous_available());
        let prior = open_package(&destination).expect("open exact prior fixture publication");
        assert_eq!(prior.snapshot.document.title, initial_title);
        assert_eq!(prior.snapshot.revision, Revision::new(0));
        serde_json::json!({
            "destination": destination,
            "documentId": document_id,
            "initialTitle": initial_title,
            "replacementTitle": replacement_title,
            "priorFingerprint": initial.as_str(),
            "state": "replacementReady",
            "finishAvailable": true,
            "restorePreviousAvailable": true
        })
    }

    /// Generates two disposable, replacement-ready publications for the packaged
    /// Recent recovery journey. The caller must supply an existing empty directory;
    /// the generator never removes or replaces any path.
    #[cfg(windows)]
    #[test]
    #[ignore = "explicit fixture generator for native Recent interrupted-save acceptance"]
    fn generate_recent_interrupted_save_acceptance_fixtures() {
        let Some(root) = std::env::var_os(RECENT_ACCEPTANCE_FIXTURE_ROOT) else {
            return;
        };
        let root = PathBuf::from(root);
        assert!(root.is_dir(), "fixture root must be an existing directory");
        assert!(
            root.read_dir()
                .expect("inspect fixture root")
                .next()
                .is_none(),
            "fixture root must be empty so existing evidence cannot be replaced"
        );
        let fixtures = [
            write_recent_acceptance_fixture(
                &root,
                "finish-save",
                "recent-finish-save",
                "Previous saved version",
                "Finished replacement version",
            ),
            write_recent_acceptance_fixture(
                &root,
                "restore-previous",
                "recent-restore-previous",
                "Restored previous version",
                "Discarded interrupted replacement",
            ),
        ];
        let manifest = serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "fixtures": fixtures
        }))
        .expect("serialize fixture manifest");
        let manifest_path = root.join("recent-interrupted-save-fixtures.json");
        let mut manifest_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&manifest_path)
            .expect("create fixture manifest without replacing evidence");
        manifest_file
            .write_all(&manifest)
            .expect("write fixture manifest");
        manifest_file.sync_all().expect("flush fixture manifest");
        println!("{}", manifest_path.display());
    }

    fn journal_and_paths(destination: &Path) -> (SaveJournal, SavePaths) {
        let (journal_path, next) = SavePaths::journal_paths(destination).expect("journal paths");
        let intent = read_journal_at(&journal_path).expect("read intent journal");
        let paths = SavePaths::from_journal(destination, &intent).expect("bound journal paths");
        let active = if next.exists() {
            read_journal_at(&next).expect("read ready journal")
        } else {
            intent
        };
        (active, paths)
    }

    fn completed_receipt_and_paths(destination: &Path) -> (SaveJournal, SavePaths) {
        let receipt = destination
            .parent()
            .expect("destination parent")
            .read_dir()
            .expect("list receipt directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with(".publisher-v2-save-") && name.ends_with(".completed.json")
                })
            })
            .expect("one completed receipt");
        let journal = read_journal_at(&receipt).expect("read completed receipt");
        let paths = SavePaths::from_journal(destination, &journal).expect("receipt-bound paths");
        assert_eq!(paths.receipt, receipt);
        (journal, paths)
    }

    fn overwrite_journal_value(path: &Path, value: &serde_json::Value) {
        let bytes = serde_json::to_vec(value).expect("encode forged journal");
        std::fs::write(path, bytes).expect("write forged journal");
    }

    #[test]
    fn editing_open_refuses_an_existing_or_newly_racing_interrupted_save() {
        let (_temp, destination, initial) = fixture("editing-open-existing-interruption");
        save_existing(
            &destination,
            &initial,
            &snapshot("journal-document", "Replacement", 1),
            &AssetStore::default(),
            |point| {
                if point == SaveFaultPoint::JournalDurable {
                    Err(PackageError::Io(std::io::Error::other(
                        "leave interrupted save before open",
                    )))
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("leave an actual interrupted save");
        assert!(matches!(
            open_package_for_editing(&destination),
            Err(PackageError::InterruptedSavePresent(path)) if path == destination
        ));

        let (_temp, destination, initial) = fixture("editing-open-racing-interruption");
        let result = open_package_for_editing_with(&destination, || {
            save_existing(
                &destination,
                &initial,
                &snapshot("journal-document", "Racing replacement", 1),
                &AssetStore::default(),
                |point| {
                    if point == SaveFaultPoint::JournalDurable {
                        Err(PackageError::Io(std::io::Error::other(
                            "save appeared during open",
                        )))
                    } else {
                        Ok(())
                    }
                },
            )
            .expect_err("leave a save that starts during package validation");
            Ok(())
        });
        assert!(matches!(
            result,
            Err(PackageError::InterruptedSavePresent(path)) if path == destination
        ));
    }

    #[test]
    fn schema_v1_journals_for_every_committed_phase_remain_classifiable() {
        let cases = [
            (
                LegacySavePhaseV1::IntentRecorded,
                InterruptedSaveState::IntentRecorded,
            ),
            (
                LegacySavePhaseV1::ReplacementVerified,
                InterruptedSaveState::ReplacementReady,
            ),
            (
                LegacySavePhaseV1::PriorPreserved,
                InterruptedSaveState::PriorPreserved,
            ),
            (
                LegacySavePhaseV1::ReplacementCommitted,
                InterruptedSaveState::ReplacementCommitted,
            ),
        ];
        for (phase, expected_state) in cases {
            let (temp, destination, _fingerprint) = fixture(&format!("legacy-v1-{phase:?}"));
            let initial = open_package(&destination).expect("open legacy prior");
            let prior = ArchiveIdentity::from_opened(&destination, &initial)
                .expect("legacy prior identity");
            let operation_id = "11".repeat(32);
            let paths =
                SavePaths::for_transaction(&destination, &operation_id).expect("legacy paths");
            let replacement_snapshot = snapshot("journal-document", "Legacy replacement", 1);
            save_new_package(
                &paths.replacement,
                &replacement_snapshot,
                &AssetStore::default(),
            )
            .expect("write legacy replacement");
            let replacement_opened =
                open_package(&paths.replacement).expect("open legacy replacement");
            let replacement = ArchiveIdentity::from_opened(&paths.replacement, &replacement_opened)
                .expect("legacy replacement identity");

            match phase {
                LegacySavePhaseV1::IntentRecorded => {
                    std::fs::remove_file(&paths.replacement)
                        .expect("remove unrecorded replacement");
                }
                LegacySavePhaseV1::ReplacementVerified => {}
                LegacySavePhaseV1::PriorPreserved => {
                    std::fs::rename(&destination, &paths.prior).expect("preserve legacy prior");
                }
                LegacySavePhaseV1::ReplacementCommitted => {
                    std::fs::copy(&destination, &paths.prior).expect("copy legacy prior");
                    std::fs::rename(&paths.replacement, &destination)
                        .expect("commit legacy replacement");
                }
            }
            let phase_name = match phase {
                LegacySavePhaseV1::IntentRecorded => "intent_recorded",
                LegacySavePhaseV1::ReplacementVerified => "replacement_verified",
                LegacySavePhaseV1::PriorPreserved => "prior_preserved",
                LegacySavePhaseV1::ReplacementCommitted => "replacement_committed",
            };
            let journal = serde_json::json!({
                "schema_version": 1,
                "operation": "replace_existing",
                "phase": phase_name,
                "operation_id": operation_id,
                "destination_name": paths.destination_name().expect("destination name"),
                "prior_name": paths.prior_name().expect("prior name"),
                "replacement_name": paths.replacement_name().expect("replacement name"),
                "receipt_name": paths.receipt_name().expect("receipt name"),
                "prior": {"sha256": prior.sha256, "byte_length": prior.byte_length},
                "replacement": if phase == LegacySavePhaseV1::IntentRecorded {
                    serde_json::Value::Null
                } else {
                    serde_json::json!({
                        "sha256": replacement.sha256,
                        "byte_length": replacement.byte_length
                    })
                },
                "document_id": "journal-document",
                "revision": 1,
                "semantic_digest": "22".repeat(32),
            });
            overwrite_journal_value(&paths.journal, &journal);
            let candidate = inspect_interrupted_save(&destination)
                .expect("inspect legacy v1 journal")
                .expect("legacy interruption");
            assert_eq!(candidate.state, expected_state, "legacy phase {phase:?}");
            assert!(!candidate.recovery_available());
            assert!(matches!(
                recover_interrupted_save(&candidate, InterruptedSaveRecoveryChoice::Finish),
                Err(PackageError::InterruptedSaveRecoveryReadOnly)
            ));
            drop(temp);
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn schema_v2_recovery_mutation_is_explicitly_unsupported_off_windows() {
        let (_temp, destination, initial) = fixture("unsupported-recovery");
        save_existing(
            &destination,
            &initial,
            &snapshot("journal-document", "Replacement", 1),
            &AssetStore::default(),
            |point| {
                if point == SaveFaultPoint::ReplacementRecorded {
                    Err(PackageError::Io(std::io::Error::other(
                        "stop before commit",
                    )))
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("leave a recoverable schema-v2 transaction");
        let candidate = inspect_interrupted_save(&destination)
            .expect("inspect transaction")
            .expect("pending transaction");
        assert_eq!(candidate.state, InterruptedSaveState::ReplacementReady);
        assert!(!candidate.recovery_available());
        assert!(!candidate.finish_available());
        assert!(!candidate.restore_previous_available());
        assert!(matches!(
            recover_interrupted_save(&candidate, InterruptedSaveRecoveryChoice::Finish),
            Err(PackageError::InterruptedSaveRecoveryUnsupported)
        ));
        assert!(
            inspect_interrupted_save(&destination)
                .expect("reinspect unchanged transaction")
                .is_some()
        );
    }

    #[cfg(windows)]
    #[test]
    fn schema_v2_recovery_finishes_or_restores_every_classified_state() {
        let (_temp, destination, initial, candidate, paths) = leave_schema_v2_state(
            "recover-intent-restore",
            InterruptedSaveState::IntentRecorded,
        );
        assert!(candidate.recovery_available());
        assert!(!candidate.finish_available());
        assert!(candidate.restore_previous_available());
        assert!(matches!(
            recover_interrupted_save(&candidate, InterruptedSaveRecoveryChoice::Finish),
            Err(PackageError::InterruptedSaveRecoveryChoiceUnavailable)
        ));
        assert_eq!(
            recover_interrupted_save(&candidate, InterruptedSaveRecoveryChoice::RestorePrevious)
                .expect("restore intent-only transaction"),
            InterruptedSaveRecoveryOutcome::RestoredPrevious(initial.clone())
        );
        assert!(!paths.journal.exists());
        assert!(paths.completed_intent.exists());
        assert!(
            inspect_interrupted_save(&destination)
                .expect("inspect restored intent")
                .is_none()
        );
        assert_eq!(
            recover_interrupted_save(&candidate, InterruptedSaveRecoveryChoice::RestorePrevious)
                .expect("repeat completed recovery"),
            InterruptedSaveRecoveryOutcome::NoLongerPending
        );

        for state in [
            InterruptedSaveState::ReplacementReady,
            InterruptedSaveState::PriorPreserved,
            InterruptedSaveState::ReplacementCommitted,
        ] {
            for choice in [
                InterruptedSaveRecoveryChoice::Finish,
                InterruptedSaveRecoveryChoice::RestorePrevious,
            ] {
                let label = format!("recover-{state:?}-{choice:?}");
                let (_temp, destination, initial, candidate, paths) =
                    leave_schema_v2_state(&label, state);
                let outcome = recover_interrupted_save(&candidate, choice)
                    .expect("recover exact schema-v2 transaction");
                let opened = open_package(&destination).expect("open recovered destination");
                match choice {
                    InterruptedSaveRecoveryChoice::Finish => {
                        assert_eq!(opened.snapshot.document.title, "Replacement");
                        assert_eq!(
                            outcome,
                            InterruptedSaveRecoveryOutcome::Finished(opened.fingerprint)
                        );
                    }
                    InterruptedSaveRecoveryChoice::RestorePrevious => {
                        assert_eq!(opened.snapshot.document.title, "Initial");
                        assert_eq!(opened.fingerprint, initial);
                        assert_eq!(
                            outcome,
                            InterruptedSaveRecoveryOutcome::RestoredPrevious(opened.fingerprint)
                        );
                    }
                }
                assert!(!paths.journal.exists(), "active intent must be finalized");
                assert!(
                    !paths.journal_next.exists(),
                    "active ready record must be finalized"
                );
                assert!(paths.receipt.exists(), "ready receipt must be retained");
                assert!(
                    paths.completed_intent.exists(),
                    "completed intent must be retained"
                );
                assert!(
                    inspect_interrupted_save(&destination)
                        .expect("inspect completed recovery")
                        .is_none()
                );
                assert_eq!(
                    recover_interrupted_save(&candidate, choice)
                        .expect("repeat completed recovery"),
                    InterruptedSaveRecoveryOutcome::NoLongerPending
                );
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn schema_ten_fixed_text_digest_remains_valid_for_interrupted_replacement_recovery() {
        let (temp, destination, initial) = fixture("schema-ten-fixed-text-recovery");
        let replacement = fixed_text_snapshot("journal-document", "Replacement");
        let legacy_digest = schema_ten_semantic_digest(&replacement);
        assert_eq!(
            legacy_digest,
            digest_hex(
                replacement
                    .semantic_digest()
                    .expect("current semantic digest")
            ),
            "default Fixed serialization must retain schema-ten semantic bytes"
        );

        save_existing(
            &destination,
            &initial,
            &replacement,
            &AssetStore::default(),
            |point| {
                if point == SaveFaultPoint::ReplacementRecorded {
                    Err(PackageError::Io(std::io::Error::other(
                        "leave legacy-digest replacement pending",
                    )))
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("interrupt after the replacement and its journal are durable");

        let (ready, paths) = journal_and_paths(&destination);
        assert_eq!(ready.phase, SavePhase::ReplacementVerified);
        assert_eq!(ready.semantic_digest, legacy_digest);
        let candidate = inspect_interrupted_save(&destination)
            .expect("inspect legacy-digest interrupted save")
            .expect("legacy-digest replacement remains recoverable");
        assert_eq!(candidate.state, InterruptedSaveState::ReplacementReady);
        assert_eq!(candidate.semantic_digest, legacy_digest);

        assert!(matches!(
            recover_interrupted_save(&candidate, InterruptedSaveRecoveryChoice::Finish)
                .expect("finish replacement validated with schema-ten semantic bytes"),
            InterruptedSaveRecoveryOutcome::Finished(_)
        ));
        assert_eq!(
            open_package(&destination)
                .expect("open recovered fixed-text replacement")
                .snapshot,
            replacement
        );
        assert!(!paths.journal.exists());
        assert!(!paths.journal_next.exists());
        assert!(
            inspect_interrupted_save(&destination)
                .expect("reinspect completed legacy-digest recovery")
                .is_none()
        );
        drop(temp);
    }

    #[cfg(windows)]
    #[test]
    fn one_character_destination_recovery_uses_a_complete_rename_record() {
        let (_temp, destination, _initial, candidate, _paths) = leave_schema_v2_state_named(
            "one-character-finish",
            InterruptedSaveState::ReplacementReady,
            "f",
        );
        assert!(matches!(
            recover_interrupted_save(&candidate, InterruptedSaveRecoveryChoice::Finish)
                .expect("finish one-character destination"),
            InterruptedSaveRecoveryOutcome::Finished(_)
        ));
        assert_eq!(
            open_package(&destination)
                .expect("open one-character finished destination")
                .snapshot
                .document
                .title,
            "Replacement"
        );

        let (_temp, destination, initial, candidate, _paths) = leave_schema_v2_state_named(
            "one-character-restore",
            InterruptedSaveState::PriorPreserved,
            "r",
        );
        assert_eq!(
            recover_interrupted_save(&candidate, InterruptedSaveRecoveryChoice::RestorePrevious)
                .expect("restore one-character destination"),
            InterruptedSaveRecoveryOutcome::RestoredPrevious(initial.clone())
        );
        assert_eq!(
            open_package(&destination)
                .expect("open one-character restored destination")
                .fingerprint,
            initial
        );
    }

    #[cfg(windows)]
    #[test]
    fn recovery_rejects_changed_or_ambiguous_candidates_without_mutation() {
        let (_temp, destination, _initial, candidate, paths) = leave_schema_v2_state(
            "recovery-candidate-changed",
            InterruptedSaveState::ReplacementReady,
        );
        std::fs::rename(&destination, &paths.prior).expect("advance transaction externally");
        assert!(matches!(
            recover_interrupted_save(&candidate, InterruptedSaveRecoveryChoice::Finish),
            Err(PackageError::InterruptedSaveRecoveryChanged)
        ));
        assert!(!destination.exists());
        assert!(paths.prior.exists());
        assert!(paths.replacement.exists());

        let (_temp, destination, _initial, _candidate, paths) = leave_schema_v2_state(
            "recovery-candidate-ambiguous",
            InterruptedSaveState::ReplacementReady,
        );
        let competing = b"independent competing destination";
        std::fs::write(&destination, competing).expect("replace destination with competitor");
        let ambiguous = inspect_interrupted_save(&destination)
            .expect("inspect ambiguous transaction")
            .expect("ambiguous candidate");
        assert_eq!(ambiguous.state, InterruptedSaveState::Ambiguous);
        assert!(matches!(
            recover_interrupted_save(&ambiguous, InterruptedSaveRecoveryChoice::Finish),
            Err(PackageError::InterruptedSaveRecoveryAmbiguous)
        ));
        assert_eq!(
            std::fs::read(&destination).expect("read competing destination"),
            competing
        );
        assert!(paths.replacement.exists());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "launched as an abrupt-death subprocess by the parent test"]
    fn child_abort_recovery() {
        let Ok(destination) = std::env::var(RECOVERY_CHILD_DESTINATION) else {
            return;
        };
        let destination = PathBuf::from(destination);
        let point = parse_recovery_fault_point(
            &std::env::var(RECOVERY_CHILD_POINT).expect("recovery child point environment"),
        );
        let marker = PathBuf::from(
            std::env::var(RECOVERY_CHILD_MARKER)
                .expect("recovery child reached-marker environment"),
        );
        let candidate = inspect_interrupted_save(&destination)
            .expect("child inspects pending recovery")
            .expect("child recovery candidate");
        let InterruptedSaveAuthority::SchemaV2 { intent, ready } = &candidate.authority else {
            panic!("child requires schema-v2 authority");
        };
        let result = recover_interrupted_save_windows(
            &candidate,
            intent,
            ready.as_deref(),
            InterruptedSaveRecoveryChoice::Finish,
            |reached| {
                if reached == point {
                    write_named_reached_marker(&marker, recovery_fault_point_name(reached));
                    std::process::abort();
                }
                Ok(())
            },
        );
        panic!("child reached the end instead of aborting at {point:?}: {result:?}");
    }

    #[cfg(windows)]
    #[test]
    fn abrupt_recovery_death_is_classifiable_at_every_mutation_boundary() {
        let cases = [
            (
                RecoveryFaultPoint::BeforeArchiveMutation,
                Some(InterruptedSaveState::ReplacementReady),
            ),
            (
                RecoveryFaultPoint::PriorPreserved,
                Some(InterruptedSaveState::PriorPreserved),
            ),
            (
                RecoveryFaultPoint::ArchiveCommitted,
                Some(InterruptedSaveState::ReplacementCommitted),
            ),
            (
                RecoveryFaultPoint::ReadyReceiptFinalized,
                Some(InterruptedSaveState::ReplacementCommitted),
            ),
            (RecoveryFaultPoint::IntentFinalized, None),
        ];
        for (point, expected) in cases {
            let point_name = recovery_fault_point_name(point);
            let (_temp, destination, _initial, _candidate, paths) = leave_schema_v2_state(
                &format!("recovery-abort-{point_name}"),
                InterruptedSaveState::ReplacementReady,
            );
            let marker = destination
                .parent()
                .expect("destination parent")
                .join(format!(".{point_name}.recovery-reached"));
            let output = Command::new(std::env::current_exe().expect("current test executable"))
                .args([
                    "--ignored",
                    "--exact",
                    "save_transaction::tests::child_abort_recovery",
                ])
                .env(RECOVERY_CHILD_DESTINATION, &destination)
                .env(RECOVERY_CHILD_POINT, point_name)
                .env(RECOVERY_CHILD_MARKER, &marker)
                .current_dir(destination.parent().expect("destination parent"))
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .output()
                .expect("launch abrupt recovery child");
            assert!(!output.status.success(), "child must abort at {point:?}");
            assert_eq!(
                std::fs::read_to_string(&marker).expect("child reached exact recovery boundary"),
                point_name
            );
            assert_eq!(
                inspect_interrupted_save(&destination)
                    .expect("classify abrupt recovery death")
                    .map(|candidate| candidate.state),
                expected,
                "classification after {point:?}"
            );
            if point == RecoveryFaultPoint::ReadyReceiptFinalized {
                assert!(paths.receipt.exists());
                assert!(paths.journal.exists());
                let candidate = inspect_interrupted_save(&destination)
                    .expect("reinspect resumable receipt topology")
                    .expect("receipt topology remains pending");
                assert!(matches!(
                    recover_interrupted_save(&candidate, InterruptedSaveRecoveryChoice::Finish)
                        .expect("resume after ready receipt finalization"),
                    InterruptedSaveRecoveryOutcome::Finished(_)
                ));
                assert!(
                    inspect_interrupted_save(&destination)
                        .expect("inspect resumed receipt topology")
                        .is_none()
                );
            }
            if point == RecoveryFaultPoint::IntentFinalized {
                assert!(paths.receipt.exists());
                assert!(paths.completed_intent.exists());
                assert!(!paths.journal.exists());
                assert!(!paths.journal_next.exists());
            }
        }
    }

    #[test]
    #[ignore = "launched as an abrupt-death subprocess by the parent test"]
    fn child_abort_save() {
        let Ok(destination) = std::env::var(CHILD_DESTINATION) else {
            return;
        };
        let destination = PathBuf::from(destination);
        let target =
            parse_fault_point(&std::env::var(CHILD_POINT).expect("child point environment"));
        let marker =
            PathBuf::from(std::env::var(CHILD_MARKER).expect("child reached-marker environment"));
        let opened = open_package(&destination).expect("child opens initial publication");
        let mut replacement = snapshot("journal-document", "Replacement", 1);
        let mut session = replacement.clone().into_session().unwrap();
        let mut assets = AssetStore::default();
        // The asset-written fault points require live references; history-only
        // resources are deliberately absent from the replacement archive.
        for index in 0..3 {
            let asset = assets.insert(
                "image/png",
                format!("abrupt child asset {index}").into_bytes(),
            );
            session
                .execute(
                    session.revision(),
                    crate::commands::Transaction::single(
                        "Add fault fixture Picture",
                        crate::commands::Command::AddObject {
                            location: crate::commands::ObjectLocation::Pasteboard,
                            object: crate::model::ObjectDraft {
                                name: format!("Picture {index}"),
                                size: PhysicalSize::new(10_000, 10_000).unwrap(),
                                transform: crate::model::Transform::identity_at(0, 0),
                                opacity_millionths: 1_000_000,
                                kind: crate::model::ObjectKind::Image {
                                    asset,
                                    wrap_mode: crate::model::WrapMode::None,
                                    content_kind: crate::model::ImageContentKind::ContinuousTone,
                                    crop: crate::model::ImageCrop::default(),
                                    flip: crate::model::ImageFlip::default(),
                                    lock_aspect_ratio: true,
                                },
                            },
                        },
                    ),
                )
                .unwrap();
        }
        replacement.document = session.document().clone();
        let result = save_existing(
            &destination,
            &opened.fingerprint,
            &replacement,
            &assets,
            |point| {
                if point == target {
                    write_reached_marker(&marker, point);
                    std::process::abort();
                }
                Ok(())
            },
        );
        panic!("child reached the end instead of aborting at {target:?}: {result:?}");
    }

    #[test]
    fn abrupt_child_death_at_every_boundary_leaves_only_deterministic_classifiable_artifacts() {
        for point in all_fault_points() {
            let point_name = fault_point_name(point);
            let (_temp, destination, _fingerprint) = fixture(&point_name);
            let marker = destination
                .parent()
                .expect("destination parent")
                .join(format!(".{point_name}.reached"));
            let mut command =
                Command::new(std::env::current_exe().expect("current test executable"));
            command
                .args([
                    "--ignored",
                    "--exact",
                    "save_transaction::tests::child_abort_save",
                ])
                .env(CHILD_DESTINATION, &destination)
                .env(CHILD_POINT, &point_name)
                .env(CHILD_MARKER, &marker)
                .current_dir(destination.parent().expect("destination parent"));
            #[cfg(windows)]
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            let output = command.output().expect("launch abrupt-death child");
            assert!(!output.status.success(), "child must abort at {point:?}");
            assert_eq!(
                std::fs::read_to_string(&marker).expect("child reached exact boundary"),
                point_name,
                "child must durably mark the boundary before abrupt death"
            );

            let status = inspect_interrupted_save(&destination)
                .expect("abrupt process death leaves a readable journal");
            assert_eq!(
                status.map(|status| status.state),
                expected_state_after_abort(point),
                "exact classification after {point:?}"
            );

            let (_journal, paths) = if point == SaveFaultPoint::AfterReceiptFinalized {
                completed_receipt_and_paths(&destination)
            } else {
                journal_and_paths(&destination)
            };
            let allowed = [
                destination.clone(),
                paths.journal,
                paths.journal_next,
                paths.replacement,
                paths.prior,
                paths.receipt,
                paths.completed_intent,
                marker,
            ];
            for entry in destination
                .parent()
                .expect("destination parent")
                .read_dir()
                .expect("list child artifacts")
            {
                let path = entry.expect("read child artifact").path();
                assert!(
                    allowed.contains(&path),
                    "unexpected artifact after {point:?}: {path:?}"
                );
                assert!(
                    !path.to_string_lossy().contains(".stage-"),
                    "nested unjournaled stage after {point:?}: {path:?}"
                );
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn ordinary_error_after_prior_preservation_restores_without_clobbering() {
        let (_temp, destination, initial) = fixture("windows-prior-restore");
        let result = save_existing(
            &destination,
            &initial,
            &snapshot("journal-document", "Replacement", 1),
            &AssetStore::default(),
            |point| {
                if point == SaveFaultPoint::PriorPreserved {
                    Err(PackageError::Io(std::io::Error::other(
                        "injected after prior preservation",
                    )))
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(
            result,
            Err(PackageError::WindowsAtomicReplace {
                state: WindowsReplaceState::Restored,
                ..
            })
        ));
        assert_eq!(
            open_package(&destination)
                .expect("restored destination")
                .fingerprint,
            initial
        );
        assert_eq!(
            inspect_interrupted_save(&destination)
                .expect("inspect restored interruption")
                .expect("active journals remain")
                .state,
            InterruptedSaveState::ReplacementReady
        );
    }

    #[cfg(windows)]
    #[test]
    fn competing_destination_after_prior_preservation_reports_ambiguous() {
        let (_temp, destination, initial) = fixture("windows-prior-ambiguous");
        let result = save_existing(
            &destination,
            &initial,
            &snapshot("journal-document", "Replacement", 1),
            &AssetStore::default(),
            |point| {
                if point == SaveFaultPoint::PriorPreserved {
                    std::fs::write(&destination, b"concurrent destination")?;
                    Err(PackageError::Io(std::io::Error::other(
                        "injected after concurrent destination",
                    )))
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(
            result,
            Err(PackageError::WindowsAtomicReplace {
                state: WindowsReplaceState::Ambiguous,
                ..
            })
        ));
        let (_ready, paths) = journal_and_paths(&destination);
        assert_eq!(
            open_package(&paths.prior)
                .expect("exact prior survives")
                .fingerprint,
            initial
        );
        assert_eq!(
            std::fs::read(&destination).expect("concurrent destination survives"),
            b"concurrent destination"
        );
        assert_eq!(
            inspect_interrupted_save(&destination)
                .expect("inspect ambiguous interruption")
                .expect("active journals remain")
                .state,
            InterruptedSaveState::Ambiguous
        );
    }

    #[test]
    fn successful_saves_retain_a_unique_prior_for_every_transaction() {
        let (temp, destination, initial) = fixture("unique-priors");
        let first = snapshot("journal-document", "First replacement", 1);
        let first_fingerprint =
            save_existing_package(&destination, &initial, &first, &AssetStore::default())
                .expect("save first replacement");
        let second = snapshot("journal-document", "Second replacement", 2);
        save_existing_package(
            &destination,
            &first_fingerprint,
            &second,
            &AssetStore::default(),
        )
        .expect("save second replacement");

        let unchanged = open_package(&destination).expect("open second replacement");
        let unchanged_fingerprint = save_existing_package(
            &destination,
            &unchanged.fingerprint,
            &unchanged.snapshot,
            &unchanged.assets,
        )
        .expect("an unchanged explicit save is a no-op");
        assert_eq!(unchanged_fingerprint, unchanged.fingerprint);

        let priors = temp
            .path()
            .read_dir()
            .expect("list retained priors")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with(".publisher-v2-save-") && name.ends_with(".prior")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(priors.len(), 2);
        let receipts = temp
            .path()
            .read_dir()
            .expect("list completed receipts")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with(".publisher-v2-save-") && name.ends_with(".completed.json")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(receipts.len(), 2);
        let mut titles = priors
            .iter()
            .map(|path| {
                open_package(path)
                    .expect("open retained prior")
                    .snapshot
                    .document
                    .title
            })
            .collect::<Vec<_>>();
        titles.sort();
        assert_eq!(titles, ["First replacement", "Initial"]);
        assert!(
            inspect_interrupted_save(&destination)
                .expect("inspect complete saves")
                .is_none()
        );
    }

    #[test]
    fn repeated_identical_replacement_after_restoring_the_prior_gets_a_new_operation_identity() {
        let (temp, destination, initial) = fixture("repeated-identical-save");
        let initial_bytes = std::fs::read(&destination).expect("read exact initial generation");
        let replacement = snapshot("journal-document", "Same replacement", 1);

        save_existing_package(&destination, &initial, &replacement, &AssetStore::default())
            .expect("first identical-content transaction");
        std::fs::write(&destination, &initial_bytes).expect("restore exact initial generation");
        let restored = open_package(&destination).expect("open restored initial generation");
        assert_eq!(restored.fingerprint, initial);

        save_existing_package(
            &destination,
            &restored.fingerprint,
            &replacement,
            &AssetStore::default(),
        )
        .expect("second identical-content transaction");

        let priors = temp
            .path()
            .read_dir()
            .expect("list repeated transaction priors")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with(".publisher-v2-save-") && name.ends_with(".prior")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(priors.len(), 2, "each transaction retains its own prior");
        assert_ne!(
            priors[0].file_name(),
            priors[1].file_name(),
            "operation identity cannot depend only on identical content"
        );
        assert!(
            priors
                .iter()
                .all(|path| { std::fs::read(path).expect("read retained prior") == initial_bytes })
        );
        let receipts = temp
            .path()
            .read_dir()
            .expect("list repeated transaction receipts")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.starts_with(".publisher-v2-save-") && name.ends_with(".completed.json")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(receipts.len(), 2);
        assert_ne!(receipts[0].file_name(), receipts[1].file_name());
    }

    #[cfg(unix)]
    #[test]
    fn successful_receipt_binds_the_destination_and_both_retained_prior_witnesses() {
        let (_temp, destination, initial) = fixture("completed-receipt");
        let initial_bytes = std::fs::read(&destination).expect("read initial bytes");
        let replacement = snapshot("journal-document", "Replacement", 1);
        save_existing_package(&destination, &initial, &replacement, &AssetStore::default())
            .expect("complete receipt-bound save");

        assert!(
            inspect_interrupted_save(&destination)
                .expect("inspect completed save")
                .is_none(),
            "completed receipt must not remain an active interrupted journal"
        );
        let (receipt, paths) = completed_receipt_and_paths(&destination);
        assert!(!paths.journal.exists());
        assert!(!paths.journal_next.exists());
        assert_eq!(
            read_journal_at(&paths.completed_intent)
                .expect("read completed intent")
                .phase,
            SavePhase::IntentRecorded
        );
        assert_eq!(
            receipt.receipt_name,
            paths.receipt_name().expect("receipt name")
        );
        let replacement_identity = receipt.replacement.as_ref().expect("replacement identity");
        assert_eq!(
            identity_match(&destination, replacement_identity),
            IdentityMatch::Exact
        );
        assert_eq!(
            identity_match(&paths.prior, &receipt.prior),
            IdentityMatch::Exact
        );
        assert_eq!(
            identity_match(&paths.replacement, &receipt.prior),
            IdentityMatch::Exact
        );
        assert_eq!(
            std::fs::read(&paths.prior).expect("read independent prior"),
            initial_bytes
        );
        assert_eq!(
            std::fs::read(&paths.replacement).expect("read exchanged original"),
            initial_bytes
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_open_writer_to_the_old_generation_cannot_mutate_the_independent_prior() {
        let (_temp, destination, initial) = fixture("open-writer-race");
        let initial_bytes = std::fs::read(&destination).expect("read original bytes");
        let mut old_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&destination)
            .expect("open old generation for writing");
        let replacement = snapshot("journal-document", "Replacement", 1);
        let result = save_existing(
            &destination,
            &initial,
            &replacement,
            &AssetStore::default(),
            |point| {
                if point == SaveFaultPoint::AfterExchange {
                    old_writer.seek(SeekFrom::Start(0))?;
                    old_writer.write_all(b"mutated through old handle")?;
                    old_writer.sync_all()?;
                }
                Ok(())
            },
        );
        assert!(matches!(result, Err(PackageError::DestinationChanged)));

        let (_journal, paths) = journal_and_paths(&destination);
        assert_eq!(
            std::fs::read(&paths.prior).expect("read independent retained prior"),
            initial_bytes,
            "the prior copy must not share the old generation's inode"
        );
        assert_eq!(
            open_package(&paths.replacement)
                .expect("replacement survives rollback")
                .snapshot,
            replacement
        );
        assert_eq!(
            inspect_interrupted_save(&destination)
                .expect("inspect old-handle race")
                .expect("active journal remains")
                .state,
            InterruptedSaveState::Ambiguous
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_destination_swap_immediately_before_exchange_preserves_every_generation() {
        let (temp, destination, initial) = fixture("exchange-race");
        let initial_bytes = std::fs::read(&destination).expect("read initial generation");
        let external = temp.path().join("external.publisher-v2");
        save_new_package(
            &external,
            &snapshot("external-document", "External generation", 7),
            &AssetStore::default(),
        )
        .expect("save external generation");
        let external_bytes = std::fs::read(&external).expect("read external generation");
        let replacement_snapshot = snapshot("journal-document", "Replacement", 1);
        let result = save_existing(
            &destination,
            &initial,
            &replacement_snapshot,
            &AssetStore::default(),
            |point| {
                if point == SaveFaultPoint::BeforeExchange {
                    std::fs::rename(&external, &destination)?;
                }
                Ok(())
            },
        );
        assert!(matches!(result, Err(PackageError::DestinationChanged)));
        assert_eq!(
            std::fs::read(&destination).expect("external destination remains"),
            external_bytes
        );
        let (_journal, paths) = journal_and_paths(&destination);
        assert!(
            paths.prior.is_file(),
            "original generation remains as prior"
        );
        assert_eq!(
            std::fs::read(&paths.prior).expect("read retained original"),
            initial_bytes
        );
        assert!(
            paths.replacement.is_file(),
            "replacement generation remains staged"
        );
        assert_eq!(
            open_package(&paths.replacement)
                .expect("open retained replacement")
                .snapshot,
            replacement_snapshot
        );
        assert_eq!(
            inspect_interrupted_save(&destination)
                .expect("classify external race")
                .expect("journal remains")
                .state,
            InterruptedSaveState::Ambiguous
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn unknown_journal_fields_and_forged_sibling_names_are_rejected() {
        let (_temp, destination, initial) = fixture("journal-schema");
        let replacement_snapshot = snapshot("journal-document", "Replacement", 1);
        let _error = save_existing(
            &destination,
            &initial,
            &replacement_snapshot,
            &AssetStore::default(),
            |point| {
                if point == SaveFaultPoint::ReplacementRecorded {
                    Err(PackageError::Io(std::io::Error::other(
                        "injected test fault",
                    )))
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("stop with a complete bound journal");
        let (journal, paths) = journal_and_paths(&destination);
        let intent = read_journal_at(&paths.journal).expect("read immutable intent");
        assert_eq!(intent.phase, SavePhase::IntentRecorded);
        assert!(intent.replacement.is_none());
        assert_eq!(journal.operation, SaveOperation::ReplaceExisting);
        assert_eq!(journal.phase, SavePhase::ReplacementVerified);
        assert!(is_lower_hex_digest(&journal.operation_id));
        assert_eq!(journal.destination_name, "book.publisher-v2");
        assert_eq!(journal.prior_name, paths.prior_name().expect("prior name"));
        assert_eq!(
            journal.replacement_name,
            paths.replacement_name().expect("replacement name")
        );
        assert!(journal.prior_name.contains(&journal.operation_id));
        assert!(journal.replacement_name.contains(&journal.operation_id));
        assert!(journal.receipt_name.contains(&journal.operation_id));
        assert!(
            journal
                .completed_intent_name
                .contains(&journal.operation_id)
        );
        assert_eq!(
            journal.receipt_name,
            paths.receipt_name().expect("receipt name")
        );
        assert_eq!(
            journal.completed_intent_name,
            paths
                .completed_intent_name()
                .expect("completed intent name")
        );
        assert_eq!(journal.prior.sha256, initial.as_str());
        assert_eq!(
            journal.prior.byte_length,
            std::fs::metadata(&destination)
                .expect("destination metadata")
                .len()
        );
        let replacement = open_package(&paths.replacement).expect("open recorded replacement");
        assert_eq!(
            journal
                .replacement
                .as_ref()
                .expect("replacement identity")
                .sha256,
            replacement.fingerprint.as_str()
        );
        assert_eq!(
            journal
                .replacement
                .as_ref()
                .expect("replacement identity")
                .byte_length,
            std::fs::metadata(&paths.replacement)
                .expect("replacement metadata")
                .len()
        );
        assert_eq!(journal.document_id, "journal-document");
        assert_eq!(journal.revision, 1);
        assert_eq!(
            journal.semantic_digest,
            digest_hex(
                replacement_snapshot
                    .semantic_digest()
                    .expect("semantic digest")
            )
        );
        let journal_path = paths.journal_next;
        let bytes = std::fs::read(&journal_path).expect("read journal bytes");
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("parse journal");
        value["unexpected"] = serde_json::json!(true);
        overwrite_journal_value(&journal_path, &value);
        assert!(matches!(
            inspect_interrupted_save(&destination),
            Err(PackageError::InvalidSaveJournal)
        ));

        value
            .as_object_mut()
            .expect("journal object")
            .remove("unexpected");
        value["operation_id"] = serde_json::json!("A".repeat(64));
        overwrite_journal_value(&journal_path, &value);
        assert!(matches!(
            inspect_interrupted_save(&destination),
            Err(PackageError::InvalidSaveJournal)
        ));

        value["operation_id"] = serde_json::json!(journal.operation_id);
        value["prior_name"] = serde_json::json!("../outside.publisher-v2");
        overwrite_journal_value(&journal_path, &value);
        assert!(matches!(
            inspect_interrupted_save(&destination),
            Err(PackageError::InvalidSaveJournal)
        ));

        value["prior_name"] = serde_json::json!(journal.prior_name);
        value["completed_intent_name"] = serde_json::json!("../outside-intent.json");
        overwrite_journal_value(&journal_path, &value);
        assert!(matches!(
            inspect_interrupted_save(&destination),
            Err(PackageError::InvalidSaveJournal)
        ));

        value["completed_intent_name"] = serde_json::json!(journal.completed_intent_name);
        value["document_id"] = serde_json::json!("different-valid-document");
        overwrite_journal_value(&journal_path, &value);
        assert!(matches!(
            inspect_interrupted_save(&destination),
            Err(PackageError::InvalidSaveJournal)
        ));
    }
}
