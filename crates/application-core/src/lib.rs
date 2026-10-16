//! Application-owned editing sessions exposed through one revision-checked interface.
//!
//! Desktop and future web adapters submit bounded intent here. Canonical state,
//! validation, undo, and redo remain inside `document-core`.

mod clipboard;
mod session_clipboard;
mod style_transfer;
mod embedded_font_reuse;
mod save_job;
pub use save_job::{EditorSaveJob, EditorSaveJobOutcome};
mod font_selection;
mod table_authoring;
mod text_search;
mod selection;
mod shape_creation;
mod master_pages;
pub use master_pages::{MasterRemovalInput, MasterTransactionInput, NumberingSectionInput, PageNumberFormatInput, TextFieldInput};
#[cfg(test)]
mod image_batch_tests;

pub use clipboard::{CaptureClipboardInput, ClipboardSelection};
pub use session_clipboard::{ClipboardDestinationInput, ClipboardFontWorkOrder, ObjectClipboardCapturePlan, PasteObjectClipboardInput, SessionClipboard};
pub use style_transfer::{CaptureStyleClipboardInput, PasteStyleClipboardInput, StyleClipboardCapturePlan, StyleClipboardSummary, StylePasteOutcome};
pub use table_authoring::{TableCellBordersInput, TableCellFormattingInput, TableCellRangeInput};
pub use text_search::{AuthoredTextSource, FindTextInput, FindTextResult, ReplaceAllTextInput,
    ReplaceTextMatchInput, ReplaceTextResult, TextSearchCursor, TextSearchMatch, TextSearchScope,
    TextSourceLocation, TextSourceSnapshotInput, TextSourceSnapshot, OwnedTextSource, authored_text_sources};
pub use selection::{ResizeSelectionInput, RotateSelectionInput, SelectionBounds, SelectionDestinationInput, SelectionGeometry, SelectionScopeInput, SetLineEndpointsInput};
pub use shape_creation::CornerRoundingInput;
pub use font_selection::{
    FontSelectionDisposition, InlineFontSelectionIntent, InlineFontSelectionPlan,
    RequiredFontVariant, StyledTextTransactionPlan, TableCellFontTransactionPlan, VerifiedTableCellFontTransactionInput, VerifiedFontSelectionInput,
    VerifiedFontSelectionOutcome, VerifiedInlineFontSelectionInput, VerifiedInlineFontVariant,
    VerifiedStyledTextTransactionInput,
};

use document_core::assets::{
    AssetHash, AssetStore, EmbeddedAsset, RasterInspection, referenced_asset_hashes,
};
use document_core::commands::{
    ArrangeGeometryOperation, ArrangeOperation, Command, DocumentSession, FontFaceBindingUpdate,
    MAX_TEXT_EDIT_COUNT, MAX_TEXT_EDIT_INSERTED_BYTES, ObjectLocation, TextEdit, Transaction,
};
use document_core::model::{
    Color, CornerRounding, Document, DocumentId, FontFaceBinding, GuideAxis, GuideId, HardShadow, ImageContentKind,
    ImageCrop, ImageFlip, Insets, Micrometres, ObjectDraft, ObjectEffects, ObjectId, ObjectKind,
    PageId, ParagraphListStyle, ParagraphRange, ParagraphStyle, PhysicalSize, PublicationObject, Revision,
    ShapeKind, ShapeStroke, TableData, TableGridBorder, TableTrackAxis, TableVerticalAlignment,
    TextAlignment, TextBoxSizingMode, TextFrame, TextSourceRange, TextStoryFormatting, TextStyle,
    TextVerticalAlignment, Transform, UntaggedRasterPolicy, WrapMode, WrapSettings, WrapSide,
};
use document_core::package::{
    AssetDescriptor, PackageFingerprint, ProjectSnapshot, SCHEMA_VERSION, asset_descriptors,
    is_supported_font_media_type, open_package, save_new_package_with_cancel, validate_document_saveability,
};
use document_core::recovery::{
    RecoveryCandidate, RecoveryRetirementCandidate, RecoveryRetirementReport,
    capture_recovery_retirement, open_recovery_package, retire_recoveries,
    save_recovery_with_source_schema,
};
use document_core::{
    InterruptedSaveCandidate, InterruptedSaveRecoveryChoice, InterruptedSaveRecoveryOutcome,
    open_package_for_editing, recover_interrupted_save, save_existing_package_retryable_with_cancel,
};
use layout_core::{ExactFontResource, PublicationLayoutError, ResolvedPublication};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use ttf_parser::Face;

const MAX_TITLE_BYTES: usize = 32 * 1024;
const MAX_LABEL_BYTES: usize = 256;
const MAX_TEXT_BYTES: usize = document_core::model::MAX_TEXT_BYTES;
const MAX_TABLE_CELL_TEXT_BYTES: usize = 32 * 1024;
const MAX_FONT_FAMILY_BYTES: usize = 1_024;
const MAX_FONT_FAMILY_CHARS: usize = 256;
const MAX_COMMANDS: usize = 100;
const MAX_CANONICAL_COMMANDS: usize = 1_000;
const MAX_COORDINATE_UM: i64 = 10_000_000;
const MAX_DIMENSION_UM: i64 = 5_000_000;
const MAX_TEXT_BOX_FONT_RESOURCES: usize = 64;
const MAX_TEXT_BOX_FONT_BYTES_TOTAL: usize = 64 * 1024 * 1024;
const MAX_RECOVERY_RETIREMENT_TOKENS: usize = 64;

#[derive(Debug)]
pub struct EditorModule {
    session: Option<DocumentSession>,
    assets: AssetStore,
    clipboard_resources: BTreeSet<AssetHash>,
    clipboard_binding: std::sync::Arc<()>,
    project: Option<ProjectBinding>,
    // Session compatibility never participates in the canonical semantic digest.
    source_schema_version: u32,
    recovery_generation: u64,
    recovery_lineage: Vec<(PathBuf, RecoveryRetirementCandidate)>,
    recovery_warning: Option<String>,
    recovery_paused: bool,
    recovery_cleanup_warning: Option<String>,
    recovery_has_untracked_copies: bool,
}

#[derive(Clone, Debug)]
struct ProjectBinding {
    path: PathBuf,
    fingerprint: PackageFingerprint,
    generation: u64,
    revision: Revision,
    failed_save: Option<InterruptedSaveCandidate>,
}

impl Default for EditorModule {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorModule {
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: None,
            assets: AssetStore::default(),
            clipboard_resources: BTreeSet::new(),
            clipboard_binding: std::sync::Arc::new(()),
            project: None,
            source_schema_version: SCHEMA_VERSION,
            recovery_generation: 0,
            recovery_lineage: Vec::new(),
            recovery_warning: None,
            recovery_paused: false,
            recovery_cleanup_warning: None,
            recovery_has_untracked_copies: false,
        }
    }

    /// Reports whether this module currently owns an active publication session.
    ///
    /// Desktop adapters can use this to fail closed before replacing work until a
    /// Save/Discard/Cancel flow exists.
    #[must_use]
    pub fn has_document(&self) -> bool {
        self.session.is_some()
    }

    /// Reports the authoritative save state used to decide whether a document
    /// transition needs a Save/Discard/Cancel prompt.
    #[must_use]
    pub fn document_lifecycle(&self) -> DocumentLifecycleState {
        let Some(session) = self.session.as_ref() else {
            return DocumentLifecycleState::Empty;
        };
        let revision = session.revision().value();
        let has_project_path = self.project.is_some();
        let requires_save_copy = self.requires_save_copy();
        if self
            .project
            .as_ref()
            .is_some_and(|binding| binding.revision == session.revision())
        {
            DocumentLifecycleState::Clean {
                revision,
                has_project_path,
                requires_save_copy,
            }
        } else {
            DocumentLifecycleState::Dirty {
                revision,
                has_project_path,
                requires_save_copy,
            }
        }
    }

    /// Releases a clean publication after rechecking its exact revision.
    ///
    /// This is the safe completion step after Save succeeds. It refuses dirty
    /// work so a desktop adapter cannot accidentally turn a failed or cancelled
    /// save into data loss.
    ///
    /// # Errors
    ///
    /// Returns `no_document` when nothing is open, or `command_rejected` when
    /// the expected revision is stale or the current publication is dirty.
    pub fn release_clean_document(&mut self, expected_revision: u64) -> EditorResult<()> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        if session.revision().value() != expected_revision {
            return Err(stale_document_transition());
        }
        if self
            .project
            .as_ref()
            .is_none_or(|binding| binding.revision != session.revision())
        {
            return Err(EditorError::new(
                EditorErrorCode::CommandRejected,
                "Save this publication or explicitly discard its changes before continuing",
            ));
        }
        self.clear_document();
        Ok(())
    }

    /// Explicitly discards the open publication after rechecking its revision.
    ///
    /// Only in-memory editing state and its save binding are released. This
    /// method never deletes or changes the saved publication on disk. Recovery
    /// storage is managed outside this module, so it is intentionally untouched.
    ///
    /// # Errors
    ///
    /// Returns `no_document` when nothing is open, or `command_rejected` when
    /// the expected revision is stale.
    pub fn discard_document(&mut self, expected_revision: u64) -> EditorResult<()> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        if session.revision().value() != expected_revision {
            return Err(stale_document_transition());
        }
        self.clear_document();
        Ok(())
    }

    /// Returns the currently bound manual-save destination without exposing package authority.
    #[must_use]
    pub fn project_path(&self) -> Option<&Path> {
        self.project.as_ref().map(|project| project.path.as_path())
    }

    /// An older publication keeps its original destination until a verified copy
    /// has been saved in this build's format. Merely opening it remains a clean read.
    #[must_use]
    pub fn requires_save_copy(&self) -> bool {
        self.has_document() && self.source_schema_version < SCHEMA_VERSION
    }

    fn clear_document(&mut self) {
        self.session = None;
        self.assets = AssetStore::default();
        self.project = None;
        self.source_schema_version = SCHEMA_VERSION;
        self.recovery_generation = 0;
        self.reset_recovery_lineage();
    }

    fn reset_recovery_lineage(&mut self) {
        self.clipboard_binding = std::sync::Arc::new(());
        self.clipboard_resources.clear();
        self.recovery_lineage.clear();
        self.recovery_warning = None;
        self.recovery_paused = false;
        self.recovery_cleanup_warning = None;
        self.recovery_has_untracked_copies = false;
    }

    /// Persistent recovery storage guidance, including when no new snapshot is returned.
    #[must_use]
    pub fn recovery_warning(&self) -> Option<&str> {
        self.recovery_warning.as_deref()
    }

    /// Records unavailable recovery storage even when the host cannot resolve its root.
    /// This does not alter the publication's Save state or existing recovery authority.
    pub fn pause_recovery(&mut self) {
        self.recovery_paused = true;
        self.refresh_recovery_warning();
    }

    fn end_recovery_history_lineage(&mut self) {
        self.recovery_lineage.clear();
        // A storage problem is still unresolved if history navigation gives up the
        // authority that could have retried its cleanup.
        self.recovery_has_untracked_copies |= self.recovery_cleanup_warning.is_some();
    }

    fn refresh_recovery_warning(&mut self) {
        self.recovery_warning = if self.recovery_paused {
            let mut message = "Automatic recovery is paused. Existing copies are preserved. Save your publication manually and inspect the recovery folder.".to_owned();
            if let Some(cleanup) = &self.recovery_cleanup_warning {
                message.push(' ');
                message.push_str(cleanup);
            }
            Some(message)
        } else {
            self.recovery_cleanup_warning.clone()
        };
    }

    fn remember_recovery(&mut self, candidate: &RecoveryCandidate) {
        if self.recovery_lineage.len() >= MAX_RECOVERY_RETIREMENT_TOKENS {
            self.recovery_has_untracked_copies = true;
            self.recovery_cleanup_warning = Some(
                "Recovery copy preserved; automatic cleanup is unavailable for additional copies in this session. Inspect the recovery folder.".to_owned(),
            );
            self.refresh_recovery_warning();
            return;
        }
        if let Ok(token) = capture_recovery_retirement(candidate) {
            self.recovery_lineage
                .push((candidate.package_path.clone(), token));
        } else {
            self.recovery_has_untracked_copies = true;
            self.recovery_cleanup_warning = Some(
                "Recovery copy preserved; safe automatic cleanup is unavailable for this copy. Inspect the recovery folder.".to_owned(),
            );
        }
        self.refresh_recovery_warning();
    }

    /// Creates and owns one blank publication session.
    ///
    /// # Errors
    ///
    /// Returns a bounded structured error when identity, title, trim, or bleed is invalid.
    pub fn create(&mut self, input: CreateDocumentInput) -> EditorResult<EditorSnapshot> {
        require_text(&input.title, MAX_TITLE_BYTES, "title")?;
        let document = Document::new_with_page_count(
            DocumentId::parse(input.document_id).map_err(invalid_input)?,
            input.title,
            physical_size(input.trim_width_um, input.trim_height_um)?,
            Insets::uniform(input.bleed_um).map_err(invalid_input)?,
            input.page_count,
        )
        .map_err(invalid_input)?;
        self.session = Some(DocumentSession::new(document).map_err(command_rejected)?);
        self.assets = AssetStore::default();
        self.project = None;
        self.source_schema_version = SCHEMA_VERSION;
        self.recovery_generation = 0;
        self.reset_recovery_lineage();
        self.snapshot()
    }

    /// Opens a validated editable package without disturbing the current session on failure.
    ///
    /// # Errors
    ///
    /// Returns a structured persistence error when the package cannot be safely opened.
    pub fn open(&mut self, path: &Path) -> EditorResult<EditorSnapshot> {
        let opened = open_package_for_editing(path).map_err(publication_open_failed)?;
        let session = opened.snapshot.into_session().map_err(command_rejected)?;
        let revision = session.revision();
        self.recovery_generation = revision.value();
        self.session = Some(session);
        self.assets = opened.assets;
        self.source_schema_version = opened.schema_version;
        self.project = Some(ProjectBinding {
            path: path.to_path_buf(),
            fingerprint: opened.fingerprint,
            generation: 1,
            revision,
            failed_save: None,
        });
        self.reset_recovery_lineage();
        self.snapshot()
    }

    /// Opens a validated recovery package as unsaved work.
    ///
    /// The recovery path is deliberately not bound as the manual-save destination.
    ///
    /// # Errors
    ///
    /// Returns a structured persistence error when the recovery package cannot be safely opened.
    pub fn open_recovery(&mut self, candidate: &RecoveryCandidate) -> EditorResult<EditorSnapshot> {
        let opened = open_recovery_package(candidate).map_err(|error| {
            EditorError::new(EditorErrorCode::PersistenceFailed, error.to_string())
        })?;
        let session = opened.snapshot.into_session().map_err(command_rejected)?;
        self.recovery_generation = session.revision().value();
        self.session = Some(session);
        self.assets = opened.assets;
        self.project = None;
        self.source_schema_version = candidate.source_schema_version;
        self.reset_recovery_lineage();
        self.remember_recovery(candidate);
        self.snapshot()
    }

    /// Saves the current editable publication to a new package destination.
    ///
    /// # Errors
    ///
    /// Returns `no_document` when nothing is open or a structured persistence error when the
    /// sibling-stage write, reopen validation, or commit cannot complete.
    pub fn save_new(&mut self, path: &Path) -> EditorResult<()> {
        self.save_new_cancellable(path, || false)
    }

    fn save_new_cancellable(&mut self, path: &Path, cancelled: impl Fn() -> bool) -> EditorResult<()> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        let fingerprint =
            save_new_package_with_cancel(path, &ProjectSnapshot::from_session(session), &self.assets, cancelled)
                .map_err(persistence_failed)?;
        self.project = Some(ProjectBinding {
            path: path.to_path_buf(),
            fingerprint,
            generation: 1,
            revision: session.revision(),
            failed_save: None,
        });
        self.source_schema_version = SCHEMA_VERSION;
        Ok(())
    }

    /// Saves to the publication's currently bound package when it has not changed externally.
    /// A retry may finish this session's exact failed transaction before saving newer edits.
    /// Journals from other attempts remain an explicit recovery decision.
    ///
    /// # Errors
    ///
    /// Returns a persistence error when an older source requires Save Copy, the publication
    /// has no bound path, the package changed externally, or replacement cannot be verified.
    pub fn save_current(&mut self) -> EditorResult<()> {
        self.save_current_cancellable(|| false)
    }

    fn save_current_cancellable(&mut self, cancelled: impl Fn() -> bool) -> EditorResult<()> {
        if cancelled() {
            return Err(EditorError::new(
                EditorErrorCode::PersistenceFailed,
                "Save cancelled before the saved publication was changed",
            ));
        }
        let session = self.session.as_ref().ok_or_else(no_document)?;
        if self.requires_save_copy() {
            return Err(EditorError::new(
                EditorErrorCode::PersistenceFailed,
                "This publication uses an earlier format. Save a copy first to preserve its original file.",
            ));
        }
        let project = self.project.as_mut().ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::PersistenceFailed,
                "This publication must be saved to a new location first",
            )
        })?;
        let generation = project.generation.checked_add(1).ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::PersistenceFailed,
                "Publication package generation limit reached",
            )
        })?;
        if let Some(candidate) = project.failed_save.as_ref() {
            let choice = if candidate.finish_available() {
                InterruptedSaveRecoveryChoice::Finish
            } else {
                InterruptedSaveRecoveryChoice::RestorePrevious
            };
            match recover_interrupted_save(candidate, choice).map_err(persistence_failed)? {
                InterruptedSaveRecoveryOutcome::Finished(fingerprint) => {
                    project.fingerprint = fingerprint;
                    project.revision = candidate.revision;
                    project.generation = generation;
                }
                InterruptedSaveRecoveryOutcome::RestoredPrevious(fingerprint) => {
                    if fingerprint != project.fingerprint {
                        return Err(persistence_failed(
                            document_core::PackageError::DestinationChanged,
                        ));
                    }
                }
                InterruptedSaveRecoveryOutcome::NoLongerPending => {
                    // Missing journals do not prove that the saved destination is still ours.
                    return Err(persistence_failed(
                        document_core::PackageError::InterruptedSaveRecoveryChanged,
                    ));
                }
            }
            project.failed_save = None;
            if project.revision == session.revision() {
                return Ok(());
            }
        }
        let fingerprint = save_existing_package_retryable_with_cancel(
            &project.path,
            &project.fingerprint,
            &ProjectSnapshot::from_session(session),
            &self.assets,
            cancelled,
        )
        .map_err(|failure| {
            project.failed_save = failure.retry;
            persistence_failed(failure.error)
        })?;
        project.fingerprint = fingerprint;
        project.generation = generation;
        project.revision = session.revision();
        Ok(())
    }

    /// Freezes the exact saved publication generation that may enter export.
    ///
    /// Unsaved or dirty state is rejected: a package fingerprint can identify
    /// the canonical document only when its saved revision matches the open
    /// session. The package is reopened to detect external replacement before
    /// the immutable snapshot leaves application ownership.
    ///
    /// # Errors
    ///
    /// Returns a structured error when the expected revision is stale, the
    /// publication is unsaved or dirty, or the bound package changed on disk.
    pub fn freeze_export_source(
        &self,
        expected_revision: u64,
    ) -> EditorResult<ExportSourceSnapshot> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        if session.revision().value() != expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::CommandRejected,
                "The publication changed before export preparation started",
            ));
        }
        let binding = self.project.as_ref().ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::PersistenceFailed,
                "Save the publication before preparing an export",
            )
        })?;
        if binding.revision != session.revision() {
            return Err(EditorError::new(
                EditorErrorCode::PersistenceFailed,
                "Save the latest publication changes before preparing an export",
            ));
        }
        let reopened = open_package(&binding.path).map_err(persistence_failed)?;
        if reopened.fingerprint != binding.fingerprint
            || reopened.snapshot.revision != binding.revision
        {
            return Err(EditorError::new(
                EditorErrorCode::PersistenceFailed,
                "The saved publication changed outside Publisher V2",
            ));
        }
        Ok(ExportSourceSnapshot {
            project: ProjectSnapshot::from_session(session),
            package_generation: binding.generation,
            package_fingerprint: binding.fingerprint.clone(),
        })
    }

    /// Captures the current canonical publication and its embedded assets for preflight.
    ///
    /// Unlike export freezing, preflight deliberately accepts unsaved and dirty
    /// work so authors can find and repair problems before choosing a save
    /// destination. The revision check keeps the cloned project and assets
    /// bound to the document state the caller asked to inspect.
    ///
    /// # Errors
    ///
    /// Returns a structured error when no publication is open or the expected
    /// revision is stale.
    pub fn snapshot_for_preflight(
        &self,
        expected_revision: u64,
    ) -> EditorResult<PreflightSourceSnapshot> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        if session.revision().value() != expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::CommandRejected,
                "The publication changed before preflight started",
            ));
        }
        Ok(PreflightSourceSnapshot {
            project: ProjectSnapshot::from_session(session),
            assets: self.assets.clone(),
        })
    }

    /// Writes a separate validated recovery generation for the current canonical revision.
    ///
    /// # Errors
    ///
    /// Returns `no_document` when nothing is open or `recovery_failed` when the independent
    /// autosave package and metadata cannot be validated and persisted.
    pub fn autosave(&mut self, recovery_root: &Path) -> EditorResult<RecoveryCandidate> {
        let result = self.write_recovery(recovery_root);
        match result {
            Ok(candidate) => {
                // A successful new write proves the paused allowance is available again.
                // Existing cleanup warnings still describe copies this session cannot retire.
                self.recovery_paused = false;
                self.remember_recovery(&candidate);
                Ok(candidate)
            }
            Err(error) => {
                self.pause_recovery();
                Err(error)
            }
        }
    }

    fn write_recovery(&mut self, recovery_root: &Path) -> EditorResult<RecoveryCandidate> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        let generation = self.recovery_generation.checked_add(1).ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::RecoveryFailed,
                "Recovery generation limit reached",
            )
        })?;
        let base_fingerprint = self.project.as_ref().map(|project| &project.fingerprint);
        let candidate = save_recovery_with_source_schema(
            recovery_root,
            generation,
            &ProjectSnapshot::from_session(session),
            &self.assets,
            base_fingerprint,
            self.source_schema_version,
        )
        .map_err(recovery_failed)?;
        self.recovery_generation = generation;
        Ok(candidate)
    }

    /// Retires exact session-owned recovery copies only after a verified manual Save.
    /// Cleanup warnings never change the publication's successful Save or undo history.
    /// Call before releasing/replacing a clean session, and forget only retired paths.
    #[must_use]
    pub fn retire_saved_recoveries(&mut self, recovery_root: &Path) -> RecoveryRetirementReport {
        let Some((session, project)) = self.session.as_ref().zip(self.project.as_ref()) else {
            return RecoveryRetirementReport::default();
        };
        if project.revision != session.revision() {
            return RecoveryRetirementReport::default();
        }
        let tokens: Vec<_> = self
            .recovery_lineage
            .iter()
            .map(|(_, token)| token.clone())
            .collect();
        let report = retire_recoveries(
            recovery_root,
            &project.path,
            &project.fingerprint,
            &ProjectSnapshot::from_session(session),
            &tokens,
        );
        self.recovery_lineage.retain(|(path, _)| {
            !report
                .outcomes
                .iter()
                .any(|outcome| outcome.retired && outcome.package_path == *path)
        });
        if let Some(warning) = report.warning.as_ref().or_else(|| {
            report
                .outcomes
                .iter()
                .find_map(|outcome| outcome.warning.as_ref())
        }) {
            self.recovery_cleanup_warning = Some(warning.clone());
            self.refresh_recovery_warning();
        } else if self.recovery_lineage.is_empty() && !self.recovery_has_untracked_copies {
            // A retry resolved every owned copy. Copies for which capture failed
            // still need the user's attention and cannot be cleared by this proof.
            self.recovery_cleanup_warning = None;
            self.refresh_recovery_warning();
        }
        report
    }

    /// Applies one atomic, revision-checked transaction.
    ///
    /// # Errors
    ///
    /// Returns a structured error for missing sessions, stale revisions, excessive input,
    /// invalid identifiers or geometry, and rejected canonical commands.
    pub fn apply(&mut self, input: ApplyTransactionInput) -> EditorResult<EditorSnapshot> {
        let (expected_revision, mut transaction) = self.canonical_creation_transaction(input)?;
        if let Some(session) = &self.session {
            embedded_font_reuse::bind_new_text(session.document(), &self.assets, &mut transaction);
        }
        self.prepare_arrange_geometry(expected_revision, &mut transaction)?;
        self.execute_saveable(expected_revision, transaction, std::iter::empty())?;
        self.snapshot()
    }

    // All real command paths budget prospective referenced assets, including pending
    // imports, before the session changes. Only unreachable retired history assets
    // are released after admission; immutable bytes are never rewritten.
    fn execute_saveable(
        &mut self,
        revision: Revision,
        transaction: Transaction,
        pending: impl IntoIterator<Item = (AssetHash, AssetDescriptor)>,
    ) -> EditorResult<()> {
        let mut descriptors = asset_descriptors(&self.assets);
        for (hash, descriptor) in pending {
            descriptors.entry(hash).or_insert(descriptor);
        }
        self.session
            .as_mut()
            .ok_or_else(no_document)?
            .execute_checked(revision, transaction, |document, next_revision| {
                validate_reachable_saveability(document, next_revision, &descriptors).map_err(
                    |error| document_core::commands::CommandError::Saveability(error.to_string()),
                )
            })
            .map_err(command_rejected)?;
        self.retain_history_resources(descriptors);
        Ok(())
    }

    fn retain_history_resources(
        &mut self,
        descriptors: std::collections::BTreeMap<AssetHash, AssetDescriptor>,
    ) {
        if let Some(session) = &mut self.session {
            session.retain_history_resources(
                descriptors
                    .into_iter()
                    .map(|(hash, descriptor)| (hash, descriptor.byte_length))
                    .collect(),
            );
            let mut retained = session.retained_asset_hashes();
            retained.extend(self.clipboard_resources.iter().cloned());
            self.assets.retain_hashes(&retained);
        }
    }

    /// Reports bounded history accounting for the open publication.
    /// # Errors
    /// Returns `no_document` when no publication is open.
    pub fn history_retention_status(
        &self,
    ) -> EditorResult<document_core::commands::HistoryRetentionStatus> {
        Ok(self
            .session
            .as_ref()
            .ok_or_else(no_document)?
            .history_retention_status())
    }

    /// Applies a session-only history budget, releasing older history immediately.
    /// Current content and its embedded resources are preserved.
    /// # Errors
    /// Returns `no_document` without changes when no publication is open.
    pub fn set_history_retention_limits(
        &mut self,
        limits: document_core::commands::HistoryRetentionLimits,
    ) -> EditorResult<()> {
        self.session
            .as_mut()
            .ok_or_else(no_document)?
            .set_history_retention_limits(limits);
        self.retain_history_resources(asset_descriptors(&self.assets));
        Ok(())
    }

    /// Applies one optimistic Text Box edit batch to an isolated project snapshot.
    ///
    /// The live session, its history, save binding, recovery generation, assets,
    /// and canonical revision remain unchanged. The returned project retains the
    /// exact live revision so callers can bind a draft layout to its source state
    /// without presenting the temporary command revision as canonical.
    ///
    /// # Errors
    ///
    /// Returns a structured error when no document is open, the expected revision
    /// is stale, or the existing `ApplyTextEdits` validation rejects the object or
    /// edit batch.
    pub fn preview_text_box_edits(
        &self,
        expected_revision: u64,
        object_id: &str,
        edits: Vec<TextEditInput>,
    ) -> EditorResult<ProjectSnapshot> {
        self.preview_text_box_changes(expected_revision, object_id, edits, None)
    }

    /// Previews text and/or style changes without changing the live session.
    ///
    /// # Errors
    ///
    /// Rejects stale revisions and the same invalid edits, styles and targets as
    /// their canonical commands. Font bytes are neither imported nor replaced.
    pub fn preview_text_box_changes(
        &self,
        expected_revision: u64,
        object_id: &str,
        edits: Vec<TextEditInput>,
        style: Option<TextStyleInput>,
    ) -> EditorResult<ProjectSnapshot> {
        let live = self.session.as_ref().ok_or_else(no_document)?;
        let source_revision = live.revision();
        if source_revision.value() != expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The publication changed before the Text Box preview was prepared",
            ));
        }
        if !edits.is_empty() {
            let parsed = ObjectId::parse(object_id).map_err(invalid_input)?;
            if live.document().authored_objects().find(|object| object.id == parsed).is_some_and(|object| object.text_field.is_some()) {
                return Err(invalid_input("Automatic page numbers change with page order. Use formatting controls to change their appearance."));
            }
        }
        let mut commands = Vec::new();
        // Preserve the original empty-edit rejection unless a style is supplied.
        if !edits.is_empty() || style.is_none() {
            commands.push(apply_text_edits_command(object_id.to_owned(), edits)?);
        }
        if let Some(style) = style {
            commands.push(update_text_style_command(object_id.to_owned(), style)?);
        }
        let mut preview = DocumentSession::from_saved(live.document().clone(), source_revision)
            .map_err(command_rejected)?;
        preview
            .execute(
                source_revision,
                Transaction {
                    label: "Preview Text Box changes".to_owned(),
                    commands,
                },
            )
            .map_err(command_rejected)?;
        Ok(ProjectSnapshot {
            document: preview.document().clone(),
            revision: source_revision,
        })
    }

    /// Changes a Text Box between Fit text and Fixed box using authoritative layout.
    ///
    /// This intent-only boundary deliberately accepts no caller-supplied width or
    /// height. The application resolves the exact current object and its embedded
    /// font resources before committing one revision-checked command, so desktop
    /// and future adapters cannot fabricate canonical sizing geometry.
    ///
    /// # Errors
    ///
    /// Returns a structured error when the revision or object is invalid, the
    /// exact font is unavailable, composition exceeds its resource budget, or
    /// the canonical command is rejected. All failures occur before dispatch.
    pub fn update_text_box_sizing(
        &mut self,
        expected_revision: u64,
        object_id: &str,
        sizing_mode: TextBoxSizingModeInput,
    ) -> EditorResult<EditorSnapshot> {
        let snapshot = self.snapshot()?;
        if snapshot.project.revision.value() != expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The publication changed before the Text Box could be resized",
            ));
        }
        let object_id = ObjectId::parse(object_id.to_owned()).map_err(invalid_input)?;
        let object = snapshot
            .project
            .document
            .authored_objects()
            .find(|object| object.id == object_id)
            .ok_or_else(|| {
                EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "That Text Box is no longer available",
                )
            })?;
        let ObjectKind::Text { style, frame, .. } = &object.kind else {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "Only a Text Box can use Fit text or Fixed box",
            ));
        };
        let requested_mode = canonical_text_box_sizing_mode(sizing_mode);
        let size = if frame.sizing_mode == TextBoxSizingMode::Fixed
            && requested_mode == TextBoxSizingMode::Fixed
        {
            object.size
        } else {
            let bindings = required_fit_font_bindings(&snapshot.project, style);
            let referenced = self
                .referenced_font_faces(
                    &bindings,
                    MAX_TEXT_BOX_FONT_RESOURCES,
                    MAX_TEXT_BOX_FONT_BYTES_TOTAL,
                )
                .map_err(|_| {
                    EditorError::new(
                        EditorErrorCode::FontUnavailable,
                        "The Text Box font is unavailable, so its size was not changed",
                    )
                })?;
            if referenced.budget_exceeded {
                return Err(EditorError::new(
                    EditorErrorCode::LayoutFailed,
                    "The Text Box is too complex to resize safely; document content is unchanged",
                ));
            }
            let resources = referenced
                .faces
                .into_iter()
                .map(|resource| ExactFontResource {
                    source: resource.source,
                    face_index: resource.face_index,
                    bytes: resource.bytes,
                })
                .collect::<Vec<_>>();
            ResolvedPublication::resolve_fit_text_box(&snapshot.project, &resources, &object_id)
                .map_err(|error| {
                    let code = if error == PublicationLayoutError::ExactFontUnavailable {
                        EditorErrorCode::FontUnavailable
                    } else {
                        EditorErrorCode::LayoutFailed
                    };
                    EditorError::new(
                        code,
                        "The Text Box could not be resized; document content is unchanged",
                    )
                })?
                .size
        };
        validate_text_box_size(sizing_mode, size.width.value(), size.height.value())?;
        self.execute_saveable(
            Revision::new(expected_revision),
            Transaction::single(
                match requested_mode {
                    TextBoxSizingMode::Fit => "Fit text",
                    TextBoxSizingMode::Fixed => "Fix Text Box",
                },
                Command::UpdateTextBoxSizing {
                    object_id,
                    sizing_mode: requested_mode,
                    size,
                },
            ),
            std::iter::empty(),
        )?;
        self.snapshot()
    }

    /// Adds hash-bound raster bytes that were accepted by the dedicated worker.
    ///
    /// # Errors
    ///
    /// Returns a structured error when the worker report does not match the
    /// supplied bytes or media facts, or when placement is invalid or stale.
    pub fn import_verified_image(
        &mut self,
        input: VerifiedImageImport,
    ) -> EditorResult<EditorSnapshot> {
        self.import_verified_images(input.expected_revision, vec![input])
    }

    /// Places an ordered batch of already verified Pictures as one undoable edit.
    /// Native callers retain individual decoding failures outside this method.
    ///
    /// # Errors
    /// Rejects the entire batch without changing document, history, or assets when
    /// it is empty, exceeds 1,000 Pictures, has mismatched receipts or revisions,
    /// or its combined placement exceeds publication saveability budgets.
    pub fn import_verified_images(
        &mut self,
        expected_revision: u64,
        inputs: Vec<VerifiedImageImport>,
    ) -> EditorResult<EditorSnapshot> {
        self.import_verified_images_at(expected_revision, inputs, None)
    }

    /// Places validated Pictures in an explicit Master workspace.
    ///
    /// # Errors
    /// Refuses stale revisions, unavailable masters or invalid raster receipts.
    pub fn import_verified_images_into_master(
        &mut self,
        master_id: String,
        expected_revision: u64,
        inputs: Vec<VerifiedImageImport>,
    ) -> EditorResult<EditorSnapshot> {
        let master_id = master_pages::parse_master(master_id)?;
        let project = self.selection_project(expected_revision)?;
        if !project.document.masters.iter().any(|master| master.id == master_id) {
            return Err(invalid_input("That Master no longer exists"));
        }
        self.import_verified_images_at(expected_revision, inputs, Some(&master_id))
    }

    fn import_verified_images_at(
        &mut self,
        expected_revision: u64,
        inputs: Vec<VerifiedImageImport>,
        master_id: Option<&document_core::model::MasterId>,
    ) -> EditorResult<EditorSnapshot> {
        if inputs.is_empty() || inputs.len() > MAX_CANONICAL_COMMANDS {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "Place between 1 and 1,000 verified Pictures in one batch",
            ));
        }
        let mut commands = Vec::with_capacity(inputs.len());
        let mut pending: BTreeMap<AssetHash, AssetDescriptor> = BTreeMap::new();
        for input in &inputs {
            require_text(&input.name, MAX_TITLE_BYTES, "image name")?;
            if input.expected_revision != expected_revision
                || AssetHash::for_bytes(&input.bytes) != input.asset_hash
                || input.inspection.media_type != input.media_type
                || self.assets.get(&input.asset_hash).is_some_and(|asset| {
                    // Identical legacy bytes retain their original stored metadata;
                    // fresh worker inspection may recover a previously omitted profile.
                    asset.media_type != input.media_type
                })
                || pending.get(&input.asset_hash).is_some_and(|asset| {
                    asset.media_type != input.media_type
                        || asset.raster.as_ref() != Some(&input.inspection)
                })
            {
                return Err(EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "The validated raster report does not match the imported bytes or batch revision",
                ));
            }
            commands.push(Command::AddObject {
                location: master_id.map(|id| ObjectLocation::Master(id.clone())).map_or_else(|| location(input.page_id.clone()), Ok)?,
                object: object_draft(
                    input.name.clone(), input.x_um, input.y_um, input.width_um, input.height_um,
                    ObjectKind::Image {
                        asset: input.asset_hash.clone(),
                        wrap_mode: WrapMode::None,
                        content_kind: ImageContentKind::ContinuousTone,
                        crop: ImageCrop::default(),
                        flip: ImageFlip::default(),
                        lock_aspect_ratio: true,
                    },
                )?,
            });
            pending.entry(input.asset_hash.clone()).or_insert_with(|| AssetDescriptor {
                media_type: input.media_type.clone(),
                byte_length: input.bytes.len() as u64,
                raster: Some(input.inspection.clone()),
            });
        }
        self.execute_saveable(
            Revision::new(expected_revision),
            Transaction { label: "Place Pictures".into(), commands },
            pending,
        )?;
        for input in inputs {
            let inserted = self.assets.insert_raster(input.bytes, input.inspection);
            debug_assert_eq!(inserted, input.asset_hash);
        }
        self.snapshot()
    }

    /// Replaces a Picture's verified raster while preserving its layout and effects.
    ///
    /// # Errors
    /// Returns an error without mutation for mismatched bytes, a stale revision,
    /// a locked/non-Picture target, or a publication that cannot be saved.
    pub fn replace_verified_image(
        &mut self,
        input: VerifiedImageReplacement,
    ) -> EditorResult<EditorSnapshot> {
        require_text(&input.name, MAX_TITLE_BYTES, "image name")?;
        if AssetHash::for_bytes(&input.bytes) != input.asset_hash {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "The validated raster report does not match the replacement bytes",
            ));
        }
        self.execute_saveable(
            Revision::new(input.expected_revision),
            Transaction::single(
                "Replace Picture",
                Command::ReplaceImage {
                    object_id: parse_object_id(input.object_id)?,
                    asset: input.asset_hash.clone(),
                    name: input.name,
                },
            ),
            [(
                input.asset_hash.clone(),
                AssetDescriptor {
                    media_type: input.inspection.media_type.clone(),
                    byte_length: input.bytes.len() as u64,
                    raster: Some(input.inspection.clone()),
                },
            )],
        )?;
        let inserted = self.assets.insert_raster(input.bytes, input.inspection);
        debug_assert_eq!(inserted, input.asset_hash);
        self.snapshot()
    }

    /// Undoes one complete transaction at the expected revision.
    ///
    /// # Errors
    ///
    /// Returns a structured error if no document is open, the revision is stale,
    /// or no undo entry exists.
    pub fn undo(&mut self, expected_revision: u64) -> EditorResult<EditorSnapshot> {
        let descriptors = asset_descriptors(&self.assets);
        self.session
            .as_mut()
            .ok_or_else(no_document)?
            .undo_checked(Revision::new(expected_revision), |document, revision| {
                validate_reachable_saveability(document, revision, &descriptors).map_err(|error| {
                    document_core::commands::CommandError::Saveability(error.to_string())
                })
            })
            .map_err(command_rejected)?;
        // History navigation preserves older alternatives without granting deletion authority.
        self.retain_history_resources(descriptors);
        self.end_recovery_history_lineage();
        self.snapshot()
    }

    /// Redoes one complete transaction at the expected revision.
    ///
    /// # Errors
    ///
    /// Returns a structured error if no document is open, the revision is stale,
    /// or no redo entry exists.
    pub fn redo(&mut self, expected_revision: u64) -> EditorResult<EditorSnapshot> {
        let descriptors = asset_descriptors(&self.assets);
        self.session
            .as_mut()
            .ok_or_else(no_document)?
            .redo_checked(Revision::new(expected_revision), |document, revision| {
                validate_reachable_saveability(document, revision, &descriptors).map_err(|error| {
                    document_core::commands::CommandError::Saveability(error.to_string())
                })
            })
            .map_err(command_rejected)?;
        self.retain_history_resources(descriptors);
        self.end_recovery_history_lineage();
        self.snapshot()
    }

    /// Returns the immutable canonical snapshot and history availability.
    ///
    /// # Errors
    ///
    /// Returns `no_document` when the module does not own an open publication.
    pub fn snapshot(&self) -> EditorResult<EditorSnapshot> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        Ok(EditorSnapshot {
            project: ProjectSnapshot::from_session(session),
            can_undo: session.can_undo(),
            can_redo: session.can_redo(),
            older_history_removed: session.older_history_removed(),
            has_project_path: self.project.is_some(),
            source_schema_version: self.source_schema_version,
            requires_save_copy: self.requires_save_copy(),
            recovery_warning: self.recovery_warning.clone(),
            is_dirty: self
                .project
                .as_ref()
                .is_none_or(|binding| binding.revision != session.revision()),
        })
    }

    /// Returns cloned bytes for a validated raster that is still referenced by
    /// the open publication.
    ///
    /// This is the narrow read boundary used by the desktop image protocol. It
    /// deliberately does not expose source paths, generic embedded files, or
    /// assets left behind after their final image object is removed.
    ///
    /// # Errors
    ///
    /// Returns a structured error when no publication is open, the digest is
    /// malformed, or it does not identify a referenced validated raster.
    pub fn referenced_raster(&self, hash: &str) -> EditorResult<ReferencedRaster> {
        let asset = self.referenced_raster_asset(hash)?;
        Ok(ReferencedRaster {
            media_type: asset.media_type.clone(),
            bytes: asset.bytes.clone(),
        })
    }

    /// Inspects the type of a referenced raster without copying its source bytes.
    /// Callers can reserve bounded preview capacity before requesting those bytes.
    ///
    /// # Errors
    ///
    /// Returns the same reference and validation errors as [`Self::referenced_raster`].
    pub fn referenced_raster_media_type(&self, hash: &str) -> EditorResult<&str> {
        Ok(&self.referenced_raster_asset(hash)?.media_type)
    }

    /// Returns stored, validated source dimensions without parsing or copying encoded pixels.
    /// # Errors
    /// Refuses an unreferenced, missing or unvalidated raster.
    pub fn referenced_raster_inspection(&self, hash: &str) -> EditorResult<RasterInspection> {
        self.referenced_raster_asset(hash)?.raster.clone()
            .ok_or_else(|| invalid_input("The Picture has no validated source dimensions."))
    }

    fn referenced_raster_asset(&self, hash: &str) -> EditorResult<&EmbeddedAsset> {
        let hash = AssetHash::parse(hash.to_owned()).map_err(invalid_input)?;
        let document = self.session.as_ref().ok_or_else(no_document)?.document();
        let referenced = document.authored_objects()
            .any(
                |object| matches!(&object.kind, ObjectKind::Image { asset, .. } if asset == &hash),
            );
        if !referenced {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "That raster is not referenced by the open publication",
            ));
        }
        let asset = self.assets.get(&hash).ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::InvalidInput,
                "That raster is not embedded in the open publication",
            )
        })?;
        if asset.raster.is_none()
            || !matches!(
                asset.media_type.as_str(),
                "image/png" | "image/jpeg" | "image/tiff"
            )
        {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "That embedded asset is not a validated raster",
            ));
        }
        Ok(asset)
    }

    /// Returns the exact embedded font bytes still referenced by text objects.
    ///
    /// This is the narrow boundary between application-owned package resources
    /// and the native layout engine. Each returned resource is rechecked for
    /// content-address integrity, an allowed font media type, and a parseable
    /// requested face before its bytes leave the editing module.
    ///
    /// # Errors
    ///
    /// Returns a structured error when no publication is open or a referenced
    /// binding no longer resolves to a valid embedded font face.
    pub fn referenced_font_faces(
        &self,
        bindings: &BTreeSet<(AssetHash, u32)>,
        max_resources: usize,
        max_bytes: usize,
    ) -> EditorResult<ReferencedFontFaces> {
        self.session.as_ref().ok_or_else(no_document)?;
        let mut faces = Vec::new();
        let mut total_bytes = 0_usize;
        for (source, face_index) in bindings {
            let asset = self.assets.get(source).ok_or_else(|| {
                EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "A text object references a font that is not embedded in this publication",
                )
            })?;
            if !is_supported_font_media_type(&asset.media_type)
                || AssetHash::for_bytes(&asset.bytes) != *source
                || Face::parse(&asset.bytes, *face_index).is_err()
            {
                return Err(EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "A text object references an invalid embedded font face",
                ));
            }
            let Some(next_bytes) = total_bytes.checked_add(asset.bytes.len()) else {
                return Ok(ReferencedFontFaces {
                    faces,
                    budget_exceeded: true,
                });
            };
            if faces.len() >= max_resources || next_bytes > max_bytes {
                return Ok(ReferencedFontFaces {
                    faces,
                    budget_exceeded: true,
                });
            }
            total_bytes = next_bytes;
            faces.push(ReferencedFontFace {
                source: source.clone(),
                face_index: *face_index,
                bytes: asset.bytes.clone(),
            });
        }
        Ok(ReferencedFontFaces {
            faces,
            budget_exceeded: false,
        })
    }
}

pub type EditorResult<T> = Result<T, EditorError>;

fn required_fit_font_bindings(
    project: &ProjectSnapshot,
    style: &document_core::model::TextStyle,
) -> BTreeSet<(AssetHash, u32)> {
    let mut bindings = BTreeSet::new();
    if let Some(binding) = &style.font_face {
        bindings.insert((binding.source.clone(), binding.face_index));
    }
    for variant in &style.exact_font_variants {
        bindings.insert((variant.face.source.clone(), variant.face.face_index));
    }
    for variant in &style.exact_font_families {
        bindings.insert((variant.face.source.clone(), variant.face.face_index));
    }
    for chain in &project.document.exact_font_fallbacks {
        for face in &chain.faces {
            bindings.insert((face.source.clone(), face.face_index));
        }
    }
    bindings
}

/// Authoritative state for one document-lifecycle decision.
///
/// The revision must be presented again when releasing or discarding the
/// publication, preventing a prompt based on stale state from closing newer
/// edits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DocumentLifecycleState {
    Empty,
    Clean {
        revision: u64,
        has_project_path: bool,
        requires_save_copy: bool,
    },
    Dirty {
        revision: u64,
        has_project_path: bool,
        requires_save_copy: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
// These are independent bridge facts: history availability, path binding, and
// save dirtiness can change separately and are not one mutually exclusive state.
#[allow(clippy::struct_excessive_bools)]
pub struct EditorSnapshot {
    pub project: ProjectSnapshot,
    pub can_undo: bool,
    pub can_redo: bool,
    /// Sticky for this open session, including when no Undo/Redo steps remain.
    pub older_history_removed: bool,
    pub has_project_path: bool,
    /// Format read at Open, retained through recovery until verified Save Copy.
    pub source_schema_version: u32,
    pub requires_save_copy: bool,
    pub is_dirty: bool,
    pub recovery_warning: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferencedRaster {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// A validated, application-owned exact font resource for native composition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferencedFontFace {
    pub source: AssetHash,
    pub face_index: u32,
    pub bytes: Vec<u8>,
}

/// Bounded exact-font payload for one native preview request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferencedFontFaces {
    pub faces: Vec<ReferencedFontFace>,
    pub budget_exceeded: bool,
}

/// Immutable identity and canonical snapshot of one saved package generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportSourceSnapshot {
    pub project: ProjectSnapshot,
    pub package_generation: u64,
    pub package_fingerprint: PackageFingerprint,
}

/// Atomic current-work source for preflight, including unsaved embedded assets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreflightSourceSnapshot {
    pub project: ProjectSnapshot,
    pub assets: AssetStore,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDocumentInput {
    pub document_id: String,
    pub title: String,
    pub trim_width_um: i64,
    pub trim_height_um: i64,
    pub bleed_um: i64,
    pub page_count: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApplyTransactionInput {
    pub expected_revision: u64,
    pub label: String,
    pub commands: Vec<EditorCommandInput>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransformObjectInput {
    pub object_id: String,
    pub page_id: Option<String>,
    pub x_um: i64,
    pub y_um: i64,
    pub width_um: i64,
    pub height_um: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectCornersInput {
    pub object_id: String,
    pub top_left_um: i64,
    pub top_right_um: i64,
    pub bottom_right_um: i64,
    pub bottom_left_um: i64,
    pub linked: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotateObjectInput {
    pub object_id: String,
    pub page_id: Option<String>,
    pub x_um: i64,
    pub y_um: i64,
    pub rotation_microdegrees: i64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedRevisionInput {
    pub expected_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedImageImport {
    pub expected_revision: u64,
    pub page_id: Option<String>,
    pub name: String,
    pub x_um: i64,
    pub y_um: i64,
    pub width_um: i64,
    pub height_um: i64,
    pub media_type: String,
    pub asset_hash: AssetHash,
    pub inspection: RasterInspection,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedImageReplacement {
    pub expected_revision: u64,
    pub object_id: String,
    pub name: String,
    pub asset_hash: AssetHash,
    pub inspection: RasterInspection,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EditorCommandInput {
    AddMaster { name: String, trim_width_um: i64, trim_height_um: i64 },
    RenameMaster { master_id: String, name: String },
    DuplicateMaster { master_id: String, name: String },
    ResizeMaster { master_id: String, trim_width_um: i64, trim_height_um: i64 },
    AssignMaster { page_ids: Vec<String>, master_id: Option<String> },
    RemoveMaster { master_id: String, replacement: MasterRemovalInput },
    UpdatePageNumbering { page_id: String, section: Option<NumberingSectionInput> },
    SetPageNumberSuppression { page_ids: Vec<String>, suppressed: bool },
    SetTextField { object_id: String, field: Option<TextFieldInput> },
    AddPageNumber {
        page_id: Option<String>, name: String,
        #[serde(default)] style: Option<TextStyleInput>,
        x_um: i64, y_um: i64, width_um: i64, height_um: i64,
    },
    AddPage {
        after_page_id: Option<String>,
        trim_width_um: i64,
        trim_height_um: i64,
        bleed_um: i64,
    },
    DuplicatePage {
        page_id: String,
    },
    RenamePage {
        page_id: String,
        name: String,
    },
    DeletePage {
        page_id: String,
    },
    ReorderPage {
        page_id: String,
        new_index: usize,
    },
    UpdatePageBleed {
        page_id: String,
        bleed_um: i64,
    },
    UpdatePageTrim {
        page_id: String,
        trim_width_um: i64,
        trim_height_um: i64,
    },
    AddGuide {
        page_id: String,
        axis: GuideAxisInput,
        position_um: i64,
    },
    MoveGuide {
        page_id: String,
        guide_id: String,
        position_um: i64,
    },
    DeleteGuide {
        page_id: String,
        guide_id: String,
    },
    AddText {
        page_id: Option<String>,
        name: String,
        text: String,
        #[serde(default)]
        style: Option<TextStyleInput>,
        #[serde(default)]
        sizing_mode: TextBoxSizingModeInput,
        x_um: i64,
        y_um: i64,
        width_um: i64,
        height_um: i64,
    },
    AddShape {
        page_id: Option<String>,
        name: String,
        shape: ShapeInput,
        x_um: i64,
        y_um: i64,
        width_um: i64,
        height_um: i64,
        fill: ColorInput,
        #[serde(default)]
        stroke: Option<ShapeStrokeInput>,
        #[serde(default)]
        corner_rounding: Option<CornerRoundingInput>,
    },
    AddTable {
        page_id: Option<String>,
        name: String,
        rows: u16,
        columns: u16,
        #[serde(default)]
        style: Option<TextStyleInput>,
        x_um: i64,
        y_um: i64,
        width_um: i64,
        height_um: i64,
    },
    DuplicateObject {
        object_id: String,
        offset_x_um: i64,
        offset_y_um: i64,
    },
    PasteObjects {
        objects: Vec<PublicationObject>,
        destination_page_id: Option<String>,
        offset_x_um: i64,
        offset_y_um: i64,
    },
    GroupObjects {
        object_ids: Vec<String>,
    },
    UngroupObjects {
        object_ids: Vec<String>,
    },
    SetObjectLock {
        object_ids: Vec<String>,
        locked: bool,
    },
    MoveObject {
        object_id: String,
        page_id: Option<String>,
        x_um: i64,
        y_um: i64,
    },
    ReorderObject {
        object_id: String,
        new_index: usize,
    },
    ArrangeObject {
        object_id: String,
        operation: ArrangeOperationInput,
    },
    ArrangeSelection {
        object_ids: Vec<String>,
        operation: ArrangeOperationInput,
    },
    ArrangeGeometry {
        object_ids: Vec<String>,
        operation: ArrangeGeometryOperationInput,
    },
    ArrangeGeometryToPage {
        object_ids: Vec<String>,
        operation: ArrangeGeometryOperationInput,
    },
    MoveSelection {
        object_ids: Vec<String>,
        #[serde(default)]
        destination: SelectionDestinationInput,
        delta_x_um: i64,
        delta_y_um: i64,
    },
    DeleteSelection {
        object_ids: Vec<String>,
    },
    UpdateCornerRounding {
        object_id: String,
        top_left_um: i64,
        top_right_um: i64,
        bottom_right_um: i64,
        bottom_left_um: i64,
        linked: bool,
    },
    UpdateSelectionCornerRounding {
        object_ids: Vec<String>,
        top_left_um: i64,
        top_right_um: i64,
        bottom_right_um: i64,
        bottom_left_um: i64,
        linked: bool,
    },
    UpdateObjectCorners {
        objects: Vec<ObjectCornersInput>,
    },
    ResizeObject {
        object_id: String,
        width_um: i64,
        height_um: i64,
    },
    TransformObjects {
        objects: Vec<TransformObjectInput>,
    },
    RotateObjects {
        objects: Vec<RotateObjectInput>,
    },
    UpdateText {
        object_id: String,
        text: String,
    },
    ReplaceTextRange {
        object_id: String,
        start_byte: usize,
        end_byte: usize,
        text: String,
    },
    ApplyTextEdits {
        object_id: String,
        edits: Vec<TextEditInput>,
    },
    UpdateTextStyle {
        object_id: String,
        style: TextStyleInput,
    },
    SetInlineFontSize {
        object_id: String,
        start_byte: usize,
        end_byte: usize,
        font_size_millipoints: u32,
    },
    UpdateInlineTextStyle {
        object_id: String,
        start_byte: usize,
        end_byte: usize,
        #[serde(default)]
        bold: Option<bool>,
        #[serde(default)]
        italic: Option<bool>,
    },
    UpdateInlineTextAppearance {
        object_id: String,
        start_byte: usize,
        end_byte: usize,
        style: InlineTextStyleInput,
    },
    UpdateParagraphStyle {
        object_id: String,
        start_paragraph: u32,
        end_paragraph: u32,
        style: ParagraphStyleInput,
    },
    UpdateTextFrame {
        object_id: String,
        column_count: u16,
        column_gap_um: i64,
    },
    UpdateTextBoxInterior {
        object_id: String,
        margins: TextBoxMarginsInput,
        vertical_alignment: TextVerticalAlignment,
    },
    UpdateTextBoxSizing {
        object_id: String,
        sizing_mode: TextBoxSizingModeInput,
        width_um: i64,
        height_um: i64,
    },
    UpdateWrapMode {
        object_id: String,
        wrap_mode: WrapModeInput,
    },
    UpdateWrapSettings {
        object_id: String,
        distance_um: i64,
        side: WrapSide,
    },
    UpdateImageContentKind {
        object_id: String,
        content_kind: ImageContentKindInput,
    },
    UpdateImageCrop {
        object_id: String,
        crop: ImageCropInput,
    },
    SetImageFlip {
        object_id: String,
        horizontal: bool,
        vertical: bool,
    },
    ResetImageEditing {
        object_id: String,
    },
    SetImageAspectLock {
        object_id: String,
        locked: bool,
    },
    UpdateUntaggedRasterPolicy {
        policy: UntaggedRasterPolicyInput,
    },
    UpdateTableCell {
        object_id: String,
        row: u16,
        column: u16,
        text: String,
    },
    UpdateTableCells {
        object_id: String,
        row: u16,
        column: u16,
        cells: Vec<Vec<String>>,
    },
    UpdateTableDimensions {
        object_id: String,
        rows: u16,
        columns: u16,
    },
    InsertTableTrack {
        object_id: String,
        axis: TableTrackAxisInput,
        index: u16,
    },
    DeleteTableTrack {
        object_id: String,
        axis: TableTrackAxisInput,
        index: u16,
    },
    UpdateTableTextStyle {
        object_id: String,
        style: TextStyleInput,
    },
    UpdateTableGridBorder {
        object_id: String,
        border: TableGridBorderInput,
    },
    UpdateTableCellAlignment {
        object_id: String,
        row: u16,
        column: u16,
        horizontal_alignment: TextAlignmentInput,
        vertical_alignment: TableVerticalAlignmentInput,
    },
    UpdateTableCellFormatting {
        object_id: String,
        range: TableCellRangeInput,
        formatting: TableCellFormattingInput,
    },
    ResetTableCellFormatting {object_id: String, range: TableCellRangeInput},
    MergeTableCells {object_id: String, range: TableCellRangeInput},
    SplitTableCells {object_id: String, range: TableCellRangeInput},
    UpdateTableTrackSize {
        object_id: String,
        axis: TableTrackAxisInput,
        index: u16,
        weight: u32,
    },
    DistributeTableTracks {
        object_id: String,
        axis: TableTrackAxisInput,
    },
    SetOpacity {
        object_id: String,
        opacity_millionths: u32,
    },
    SetRotation {
        object_id: String,
        rotation_microdegrees: i64,
    },
    UpdateShape {
        object_id: String,
        shape: ShapeInput,
        fill: ColorInput,
        stroke: ShapeStrokeInput,
    },
    UpdateObjectEffects {
        object_id: String,
        effects: ObjectEffectsInput,
    },
    DeleteObject {
        object_id: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ColorInput {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageCropInput {
    pub left_millionths: u32,
    pub top_millionths: u32,
    pub right_millionths: u32,
    pub bottom_millionths: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ShapeInput {
    Rectangle,
    Ellipse,
    Cube,
    Line,
    Triangle,
    Arrow,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum GuideAxisInput {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum WrapModeInput {
    None,
    Rectangular,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextBoxMarginsInput {
    pub top_um: i64,
    pub right_um: i64,
    pub bottom_um: i64,
    pub left_um: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TextBoxSizingModeInput {
    Fit,
    #[default]
    Fixed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ImageContentKindInput {
    ContinuousTone,
    LineArt,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UntaggedRasterPolicyInput {
    RequireEmbeddedProfile,
    AssumeSrgb,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ArrangeOperationInput {
    BringForward,
    SendBackward,
    BringToFront,
    SendToBack,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ArrangeGeometryOperationInput {
    AlignLeft,
    AlignCenter,
    AlignRight,
    AlignTop,
    AlignMiddle,
    AlignBottom,
    DistributeHorizontally,
    DistributeVertically,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShapeStrokeInput {
    pub color: ColorInput,
    pub width_millipoints: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HardShadowInput {
    pub color: ColorInput,
    pub offset_x_um: i64,
    pub offset_y_um: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectEffectsInput {
    #[serde(default)]
    pub shadow: Option<HardShadowInput>,
    #[serde(default)]
    pub text_outline: Option<ShapeStrokeInput>,
    #[serde(default)]
    pub picture_border: Option<ShapeStrokeInput>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextStyleInput {
    /// Optional deliberate replacement of the exact embedded face. When absent,
    /// a style edit preserves the old face only if the family is unchanged.
    /// New text may reuse an unambiguous matching face already in the publication.
    #[serde(default)]
    pub font_face: Option<FontFaceBindingInput>,
    pub font_family: String,
    pub font_size_millipoints: u32,
    pub font_weight: u16,
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    pub alignment: TextAlignmentInput,
    pub color: ColorInput,
    pub line_height_per_thousand: u16,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ParagraphStyleInput {
    #[serde(default)]
    pub list_style: Option<ParagraphListStyleInput>,
    #[serde(default)]
    pub alignment: Option<TextAlignmentInput>,
    #[serde(default)]
    pub start_indent_um: Option<i64>,
    #[serde(default)]
    pub end_indent_um: Option<i64>,
    #[serde(default)]
    pub first_line_indent_um: Option<i64>,
    #[serde(default)]
    pub line_height_per_thousand: Option<u16>,
    #[serde(default)]
    pub space_before_millipoints: Option<u32>,
    #[serde(default)]
    pub space_after_millipoints: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "kind",
    deny_unknown_fields
)]
pub enum ParagraphListStyleInput {
    None,
    Bullet,
    Numbered { start_at: u32 },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TableGridBorderInput {
    pub color: ColorInput,
    pub width_millipoints: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TableVerticalAlignmentInput {
    Top,
    Middle,
    Bottom,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TableTrackAxisInput {
    Row,
    Column,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TextEditInput {
    pub start_byte: usize,
    pub end_byte: usize,
    pub text: String,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub style: Option<InlineTextStyleInput>,
}

/// Sparse formatting for a selected range or newly inserted text.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InlineTextStyleInput {
    #[serde(default)]
    pub font_family: Option<String>,
    #[serde(default)]
    pub font_weight: Option<u16>,
    #[serde(default)]
    pub font_size_millipoints: Option<u32>,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub color: Option<ColorInput>,
    #[serde(default)]
    pub underline: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FontFaceBindingInput {
    /// Lowercase SHA-256 of the already embedded OpenType resource.
    pub source: String,
    pub face_index: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TextAlignmentInput {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorErrorCode {
    NoDocument,
    InvalidInput,
    CommandRejected,
    StaleRevision,
    AuthoritativeLayoutRequired,
    FontUnavailable,
    LayoutFailed,
    PersistenceFailed,
    RecoveryFailed,
}

impl EditorErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoDocument => "no_document",
            Self::InvalidInput => "invalid_input",
            Self::CommandRejected => "command_rejected",
            Self::StaleRevision => "stale_revision",
            Self::AuthoritativeLayoutRequired => "authoritative_layout_required",
            Self::FontUnavailable => "font_unavailable",
            Self::LayoutFailed => "layout_failed",
            Self::PersistenceFailed => "persistence_failed",
            Self::RecoveryFailed => "recovery_failed",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorError {
    pub code: EditorErrorCode,
    pub message: String,
}

impl EditorError {
    fn new(code: EditorErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn canonical_commands(input: EditorCommandInput) -> EditorResult<Vec<Command>> {
    match input {
        EditorCommandInput::TransformObjects { objects } => transform_objects_commands(objects),
        EditorCommandInput::RotateObjects { objects } => rotate_objects_commands(objects),
        EditorCommandInput::UpdateSelectionCornerRounding {
            object_ids,
            top_left_um,
            top_right_um,
            bottom_right_um,
            bottom_left_um,
            linked,
        } => {
            let rounding = CornerRounding {
                top_left_um,
                top_right_um,
                bottom_right_um,
                bottom_left_um,
                linked,
            };
            selection_corner_commands(object_ids.into_iter().map(|id| (id, rounding)).collect())
        }
        EditorCommandInput::UpdateObjectCorners { objects } => selection_corner_commands(
            objects
                .into_iter()
                .map(|object| {
                    (object.object_id, CornerRounding {
                        top_left_um: object.top_left_um,
                        top_right_um: object.top_right_um,
                        bottom_right_um: object.bottom_right_um,
                        bottom_left_um: object.bottom_left_um,
                        linked: object.linked,
                    })
                })
                .collect(),
        ),
        input => Ok(vec![canonical_command(input)?]),
    }
}

fn selection_corner_commands(objects: Vec<(String, CornerRounding)>) -> EditorResult<Vec<Command>> {
    if !(1..=500).contains(&objects.len()) {
        return Err(invalid_input(
            "Select between 1 and 500 objects for corner rounding",
        ));
    }
    let objects = objects
        .into_iter()
        .map(|(id, rounding)| parse_object_id(id).map(|id| (id, rounding)))
        .collect::<EditorResult<Vec<_>>>()?;
    if objects.iter().map(|(id, _)| id).collect::<BTreeSet<_>>().len() != objects.len() {
        return Err(invalid_input(
            "Corner rounding cannot repeat an object identifier",
        ));
    }
    Ok(objects
        .into_iter()
        .map(|(object_id, rounding)| Command::UpdateCornerRounding { object_id, rounding })
        .collect())
}

fn canonical_transaction(input: ApplyTransactionInput) -> EditorResult<(Revision, Transaction)> {
    require_text(&input.label, MAX_LABEL_BYTES, "transaction label")?;
    if input.commands.is_empty() || input.commands.len() > MAX_COMMANDS {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "A transaction must contain between 1 and 100 commands",
        ));
    }
    let mut commands = Vec::new();
    for input in input.commands {
        commands.extend(canonical_commands(input)?);
        if commands.len() > MAX_CANONICAL_COMMANDS {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "A transaction expands beyond the 1000-command safety limit",
            ));
        }
    }
    Ok((
        Revision::new(input.expected_revision),
        Transaction {
            label: input.label,
            commands,
        },
    ))
}

fn validate_group_transform_targets<'a>(
    targets: impl Iterator<Item = (&'a str, &'a Option<String>)>,
) -> EditorResult<()> {
    let targets = targets.collect::<Vec<_>>();
    if !(2..=500).contains(&targets.len()) {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "A grouped transform must contain between 2 and 500 objects",
        ));
    }
    let unique_ids = targets.iter().map(|(id, _)| *id).collect::<BTreeSet<_>>();
    if unique_ids.len() != targets.len() {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "A grouped transform cannot contain duplicate object identifiers",
        ));
    }
    let destination = targets[0].1;
    if targets.iter().any(|(_, page_id)| *page_id != destination) {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Every object in a grouped transform must use the same destination",
        ));
    }
    Ok(())
}

fn rotate_objects_commands(objects: Vec<RotateObjectInput>) -> EditorResult<Vec<Command>> {
    validate_group_transform_targets(
        objects
            .iter()
            .map(|object| (object.object_id.as_str(), &object.page_id)),
    )?;
    let mut commands = Vec::with_capacity(objects.len() * 2);
    for object in objects {
        commands.push(set_rotation_command(
            object.object_id.clone(),
            object.rotation_microdegrees,
        )?);
        commands.push(move_object_command(
            object.object_id,
            object.page_id,
            object.x_um,
            object.y_um,
        )?);
    }
    Ok(commands)
}

fn transform_objects_commands(objects: Vec<TransformObjectInput>) -> EditorResult<Vec<Command>> {
    validate_group_transform_targets(
        objects
            .iter()
            .map(|object| (object.object_id.as_str(), &object.page_id)),
    )?;
    let mut commands = Vec::with_capacity(objects.len() * 2);
    for object in objects {
        commands.push(resize_object_command(
            object.object_id.clone(),
            object.width_um,
            object.height_um,
        )?);
        commands.push(move_object_command(
            object.object_id,
            object.page_id,
            object.x_um,
            object.y_um,
        )?);
    }
    Ok(commands)
}

#[allow(clippy::too_many_lines)]
fn canonical_command(input: EditorCommandInput) -> EditorResult<Command> {
    Ok(match input {
        command @ (EditorCommandInput::AddMaster { .. } | EditorCommandInput::RenameMaster { .. }
        | EditorCommandInput::DuplicateMaster { .. } | EditorCommandInput::ResizeMaster { .. }
        | EditorCommandInput::AssignMaster { .. } | EditorCommandInput::RemoveMaster { .. }
        | EditorCommandInput::UpdatePageNumbering { .. } | EditorCommandInput::SetPageNumberSuppression { .. }
        | EditorCommandInput::SetTextField { .. }) => master_pages::canonical_master_command(command)?,
        EditorCommandInput::AddPageNumber { page_id, name, style, x_um, y_um, width_um, height_um } =>
            add_text_command(page_id, name, String::new(), style, TextBoxSizingModeInput::Fit, x_um, y_um, width_um, height_um)?,
        EditorCommandInput::AddPage {
            after_page_id,
            trim_width_um,
            trim_height_um,
            bleed_um,
        } => Command::AddPage {
            after: optional_page_id(after_page_id)?,
            trim: physical_size(trim_width_um, trim_height_um)?,
            bleed: Insets::uniform(bleed_um).map_err(invalid_input)?,
        },
        EditorCommandInput::DuplicatePage { page_id } => Command::DuplicatePage {
            page_id: parse_page_id(page_id)?,
        },
        EditorCommandInput::RenamePage { page_id, name } => Command::RenamePage {
            page_id: parse_page_id(page_id)?,
            name,
        },
        EditorCommandInput::DeletePage { page_id } => Command::DeletePage {
            page_id: parse_page_id(page_id)?,
        },
        EditorCommandInput::ReorderPage { page_id, new_index } => {
            reorder_page_command(page_id, new_index)?
        }
        EditorCommandInput::UpdatePageBleed { page_id, bleed_um } => {
            update_page_bleed_command(page_id, bleed_um)?
        }
        EditorCommandInput::UpdatePageTrim {
            page_id,
            trim_width_um,
            trim_height_um,
        } => update_page_trim_command(page_id, trim_width_um, trim_height_um)?,
        EditorCommandInput::AddGuide {
            page_id,
            axis,
            position_um,
        } => Command::AddGuide {
            page_id: parse_page_id(page_id)?,
            axis: canonical_guide_axis(axis),
            position: Micrometres::new(position_um),
        },
        EditorCommandInput::MoveGuide {
            page_id,
            guide_id,
            position_um,
        } => Command::MoveGuide {
            page_id: parse_page_id(page_id)?,
            guide_id: parse_guide_id(guide_id)?,
            position: Micrometres::new(position_um),
        },
        EditorCommandInput::DeleteGuide { page_id, guide_id } => Command::DeleteGuide {
            page_id: parse_page_id(page_id)?,
            guide_id: parse_guide_id(guide_id)?,
        },
        EditorCommandInput::AddText {
            page_id,
            name,
            text,
            style,
            sizing_mode,
            x_um,
            y_um,
            width_um,
            height_um,
        } => add_text_command(
            page_id,
            name,
            text,
            style,
            sizing_mode,
            x_um,
            y_um,
            width_um,
            height_um,
        )?,
        EditorCommandInput::AddShape {
            page_id,
            name,
            shape,
            x_um,
            y_um,
            width_um,
            height_um,
            fill,
            stroke,
            corner_rounding: _,
        } => add_shape_command(
            page_id, name, shape, x_um, y_um, width_um, height_um, fill, stroke,
        )?,
        EditorCommandInput::AddTable {
            page_id,
            name,
            rows,
            columns,
            style,
            x_um,
            y_um,
            width_um,
            height_um,
        } => add_table_command(
            page_id, name, rows, columns, style, x_um, y_um, width_um, height_um,
        )?,
        EditorCommandInput::DuplicateObject {
            object_id,
            offset_x_um,
            offset_y_um,
        } => duplicate_object_command(object_id, offset_x_um, offset_y_um)?,
        EditorCommandInput::PasteObjects {
            objects,
            destination_page_id,
            offset_x_um,
            offset_y_um,
        } => paste_objects_command(objects, destination_page_id, offset_x_um, offset_y_um)?,
        EditorCommandInput::GroupObjects { object_ids } => Command::GroupObjects {
            object_ids: object_ids
                .into_iter()
                .map(parse_object_id)
                .collect::<EditorResult<Vec<_>>>()?,
        },
        EditorCommandInput::UngroupObjects { object_ids } => Command::UngroupObjects {
            object_ids: object_ids
                .into_iter()
                .map(parse_object_id)
                .collect::<EditorResult<Vec<_>>>()?,
        },
        EditorCommandInput::SetObjectLock { object_ids, locked } => Command::SetObjectLock {
            object_ids: object_ids
                .into_iter()
                .map(parse_object_id)
                .collect::<EditorResult<Vec<_>>>()?,
            locked,
        },
        EditorCommandInput::MoveObject {
            object_id,
            page_id,
            x_um,
            y_um,
        } => move_object_command(object_id, page_id, x_um, y_um)?,
        EditorCommandInput::ReorderObject {
            object_id,
            new_index,
        } => reorder_object_command(object_id, new_index)?,
        EditorCommandInput::ArrangeObject {
            object_id,
            operation,
        } => Command::ArrangeObject {
            object_id: parse_object_id(object_id)?,
            operation: match operation {
                ArrangeOperationInput::BringForward => ArrangeOperation::BringForward,
                ArrangeOperationInput::SendBackward => ArrangeOperation::SendBackward,
                ArrangeOperationInput::BringToFront => ArrangeOperation::BringToFront,
                ArrangeOperationInput::SendToBack => ArrangeOperation::SendToBack,
            },
        },
        EditorCommandInput::ArrangeGeometry {
            object_ids,
            operation,
        } => Command::ArrangeGeometry {
            object_ids: object_ids
                .into_iter()
                .map(parse_object_id)
                .collect::<EditorResult<Vec<_>>>()?,
            operation: match operation {
                ArrangeGeometryOperationInput::AlignLeft => ArrangeGeometryOperation::AlignLeft,
                ArrangeGeometryOperationInput::AlignCenter => ArrangeGeometryOperation::AlignCenter,
                ArrangeGeometryOperationInput::AlignRight => ArrangeGeometryOperation::AlignRight,
                ArrangeGeometryOperationInput::AlignTop => ArrangeGeometryOperation::AlignTop,
                ArrangeGeometryOperationInput::AlignMiddle => ArrangeGeometryOperation::AlignMiddle,
                ArrangeGeometryOperationInput::AlignBottom => ArrangeGeometryOperation::AlignBottom,
                ArrangeGeometryOperationInput::DistributeHorizontally => {
                    ArrangeGeometryOperation::DistributeHorizontally
                }
                ArrangeGeometryOperationInput::DistributeVertically => {
                    ArrangeGeometryOperation::DistributeVertically
                }
            },
        },
        EditorCommandInput::ArrangeSelection {
            object_ids,
            operation,
        } => Command::ArrangeSelection {
            object_ids: object_ids
                .into_iter()
                .map(parse_object_id)
                .collect::<EditorResult<Vec<_>>>()?,
            operation: match operation {
                ArrangeOperationInput::BringForward => ArrangeOperation::BringForward,
                ArrangeOperationInput::SendBackward => ArrangeOperation::SendBackward,
                ArrangeOperationInput::BringToFront => ArrangeOperation::BringToFront,
                ArrangeOperationInput::SendToBack => ArrangeOperation::SendToBack,
            },
        },
        EditorCommandInput::ArrangeGeometryToPage {
            object_ids,
            operation,
        } => Command::ArrangeGeometryToPage {
            object_ids: object_ids
                .into_iter()
                .map(parse_object_id)
                .collect::<EditorResult<Vec<_>>>()?,
            operation: selection::canonical_arrange_operation(operation),
        },
        EditorCommandInput::MoveSelection {
            object_ids,
            destination,
            delta_x_um,
            delta_y_um,
        } => {
            validate_coordinate(delta_x_um)?;
            validate_coordinate(delta_y_um)?;
            Command::MoveSelection {
                object_ids: object_ids
                    .into_iter()
                    .map(parse_object_id)
                    .collect::<EditorResult<Vec<_>>>()?,
                destination: match destination {
                    SelectionDestinationInput::Current => None,
                    SelectionDestinationInput::Pasteboard => Some(ObjectLocation::Pasteboard),
                    SelectionDestinationInput::Page { page_id } => Some(ObjectLocation::Page(parse_page_id(page_id)?)),
                },
                delta_x_um,
                delta_y_um,
            }
        }
        EditorCommandInput::DeleteSelection { object_ids } => Command::DeleteSelection {
            object_ids: object_ids
                .into_iter()
                .map(parse_object_id)
                .collect::<EditorResult<Vec<_>>>()?,
        },
        EditorCommandInput::UpdateCornerRounding {
            object_id,
            top_left_um,
            top_right_um,
            bottom_right_um,
            bottom_left_um,
            linked,
        } => Command::UpdateCornerRounding {
            object_id: parse_object_id(object_id)?,
            rounding: CornerRounding {
                top_left_um,
                top_right_um,
                bottom_right_um,
                bottom_left_um,
                linked,
            },
        },
        EditorCommandInput::ResizeObject {
            object_id,
            width_um,
            height_um,
        } => resize_object_command(object_id, width_um, height_um)?,
        EditorCommandInput::TransformObjects { .. } | EditorCommandInput::RotateObjects { .. }
        | EditorCommandInput::UpdateSelectionCornerRounding { .. } | EditorCommandInput::UpdateObjectCorners { .. } => {
            unreachable!("selection actions are expanded before single-command conversion")
        }
        EditorCommandInput::UpdateText { object_id, text } => update_text_command(object_id, text)?,
        EditorCommandInput::ReplaceTextRange {
            object_id,
            start_byte,
            end_byte,
            text,
        } => replace_text_range_command(object_id, start_byte, end_byte, text)?,
        EditorCommandInput::ApplyTextEdits { object_id, edits } => {
            apply_text_edits_command(object_id, edits)?
        }
        EditorCommandInput::UpdateTextStyle { object_id, style } => {
            update_text_style_command(object_id, style)?
        }
        EditorCommandInput::SetInlineFontSize {
            object_id,
            start_byte,
            end_byte,
            font_size_millipoints,
        } => Command::SetInlineFontSize {
            object_id: parse_object_id(object_id)?,
            range: TextSourceRange {
                start: start_byte,
                end: end_byte,
            },
            font_size_millipoints,
        },
        EditorCommandInput::UpdateInlineTextStyle {
            object_id,
            start_byte,
            end_byte,
            bold,
            italic,
        } => Command::UpdateInlineTextStyle {
            object_id: parse_object_id(object_id)?,
            range: TextSourceRange {
                start: start_byte,
                end: end_byte,
            },
            bold,
            italic,
        },
        EditorCommandInput::UpdateInlineTextAppearance {
            object_id,
            start_byte,
            end_byte,
            style,
        } => Command::UpdateInlineTextAppearance {
            object_id: parse_object_id(object_id)?,
            range: TextSourceRange {
                start: start_byte,
                end: end_byte,
            },
            style: inline_text_style(style)?,
        },
        EditorCommandInput::UpdateParagraphStyle {
            object_id,
            start_paragraph,
            end_paragraph,
            style,
        } => Command::UpdateParagraphStyle {
            object_id: parse_object_id(object_id)?,
            range: ParagraphRange {
                start: start_paragraph,
                end: end_paragraph,
            },
            style: paragraph_style(style),
        },
        EditorCommandInput::UpdateTextFrame {
            object_id,
            column_count,
            column_gap_um,
        } => update_text_frame_command(object_id, column_count, column_gap_um)?,
        EditorCommandInput::UpdateTextBoxInterior {
            object_id,
            margins,
            vertical_alignment,
        } => Command::UpdateTextBoxInterior {
            object_id: parse_object_id(object_id)?,
            margins: Insets {
                top: Micrometres::new(margins.top_um),
                right: Micrometres::new(margins.right_um),
                bottom: Micrometres::new(margins.bottom_um),
                left: Micrometres::new(margins.left_um),
            },
            vertical_alignment,
        },
        EditorCommandInput::UpdateTextBoxSizing {
            object_id: _,
            sizing_mode: _,
            width_um: _,
            height_um: _,
        } => {
            return Err(EditorError::new(
                EditorErrorCode::AuthoritativeLayoutRequired,
                "Use Auto-size box or Fixed box through authoritative Text Box sizing",
            ));
        }
        EditorCommandInput::UpdateWrapMode {
            object_id,
            wrap_mode,
        } => Command::UpdateWrapMode {
            object_id: parse_object_id(object_id)?,
            wrap_mode: match wrap_mode {
                WrapModeInput::None => WrapMode::None,
                WrapModeInput::Rectangular => WrapMode::Rectangular,
            },
        },
        EditorCommandInput::UpdateWrapSettings {
            object_id,
            distance_um,
            side,
        } => Command::UpdateWrapSettings {
            object_id: parse_object_id(object_id)?,
            settings: WrapSettings { distance_um, side },
        },
        EditorCommandInput::UpdateImageContentKind {
            object_id,
            content_kind,
        } => Command::UpdateImageContentKind {
            object_id: parse_object_id(object_id)?,
            content_kind: match content_kind {
                ImageContentKindInput::ContinuousTone => ImageContentKind::ContinuousTone,
                ImageContentKindInput::LineArt => ImageContentKind::LineArt,
            },
        },
        EditorCommandInput::UpdateImageCrop { object_id, crop } => Command::UpdateImageCrop {
            object_id: parse_object_id(object_id)?,
            crop: ImageCrop::new(
                crop.left_millionths,
                crop.top_millionths,
                crop.right_millionths,
                crop.bottom_millionths,
            )
            .map_err(invalid_input)?,
        },
        EditorCommandInput::SetImageAspectLock { object_id, locked } => {
            Command::SetImageAspectLock {
                object_id: parse_object_id(object_id)?,
                locked,
            }
        }
        EditorCommandInput::SetImageFlip { object_id, horizontal, vertical } => Command::SetImageFlip {
            object_id: parse_object_id(object_id)?,
            flip: ImageFlip { horizontal, vertical },
        },
        EditorCommandInput::ResetImageEditing { object_id } => Command::ResetImageEditing {
            object_id: parse_object_id(object_id)?,
        },
        EditorCommandInput::UpdateUntaggedRasterPolicy { policy } => {
            Command::UpdateUntaggedRasterPolicy {
                policy: match policy {
                    UntaggedRasterPolicyInput::RequireEmbeddedProfile => {
                        UntaggedRasterPolicy::RequireEmbeddedProfile
                    }
                    UntaggedRasterPolicyInput::AssumeSrgb => UntaggedRasterPolicy::AssumeSrgb,
                },
            }
        }
        EditorCommandInput::UpdateTableCell {
            object_id,
            row,
            column,
            text,
        } => update_table_cell_command(object_id, row, column, text)?,
        EditorCommandInput::UpdateTableCells {
            object_id,
            row,
            column,
            cells,
        } => {
            let width = cells.first().map_or(0, Vec::len);
            if cells.is_empty()
                || cells.len() > usize::from(TableData::MAX_ROWS)
                || width == 0
                || width > usize::from(TableData::MAX_COLUMNS)
                || cells.iter().any(|values| values.len() != width)
            {
                return Err(EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "Copied cells must form a rectangle of at most 500 rows and columns",
                ));
            }
            let mut bytes = 0_usize;
            for text in cells.iter().flatten() {
                require_bounded(text, MAX_TABLE_CELL_TEXT_BYTES, "table cell text")?;
                bytes = bytes.saturating_add(text.len());
            }
            if bytes > 1_048_576 {
                return Err(EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "Paste a smaller group of cells (up to 1 MiB of text).",
                ));
            }
            Command::UpdateTableCells {
                object_id: parse_object_id(object_id)?,
                row,
                column,
                cells,
            }
        }
        EditorCommandInput::UpdateTableDimensions {
            object_id,
            rows,
            columns,
        } => update_table_dimensions_command(object_id, rows, columns)?,
        EditorCommandInput::InsertTableTrack {
            object_id,
            axis,
            index,
        } => Command::InsertTableTrack {
            object_id: parse_object_id(object_id)?,
            axis: canonical_table_track_axis(axis),
            index,
        },
        EditorCommandInput::DeleteTableTrack {
            object_id,
            axis,
            index,
        } => Command::DeleteTableTrack {
            object_id: parse_object_id(object_id)?,
            axis: canonical_table_track_axis(axis),
            index,
        },
        EditorCommandInput::UpdateTableTextStyle { object_id, style } => {
            update_table_text_style_command(object_id, style)?
        }
        EditorCommandInput::UpdateTableGridBorder { object_id, border } => {
            update_table_grid_border_command(object_id, border)?
        }
        EditorCommandInput::UpdateTableCellAlignment {
            object_id,
            row,
            column,
            horizontal_alignment,
            vertical_alignment,
        } => Command::UpdateTableCellAlignment {
            object_id: parse_object_id(object_id)?,
            row,
            column,
            horizontal_alignment: Some(canonical_text_alignment(horizontal_alignment)),
            vertical_alignment: Some(canonical_table_vertical_alignment(vertical_alignment)),
        },
        EditorCommandInput::UpdateTableTrackSize {
            object_id,
            axis,
            index,
            weight,
        } => Command::UpdateTableTrackSize {
            object_id: parse_object_id(object_id)?,
            axis: canonical_table_track_axis(axis),
            index,
            weight,
        },
        EditorCommandInput::UpdateTableCellFormatting {object_id,range,formatting} => Command::UpdateTableCellFormatting {
            object_id: parse_object_id(object_id)?,range:range.into(),formatting:table_authoring::canonical_cell_formatting(formatting)?,
        },
        EditorCommandInput::ResetTableCellFormatting {object_id,range} => Command::ResetTableCellFormatting {object_id:parse_object_id(object_id)?,range:range.into()},
        EditorCommandInput::MergeTableCells {object_id,range} => Command::MergeTableCells {object_id:parse_object_id(object_id)?,range:range.into()},
        EditorCommandInput::SplitTableCells {object_id,range} => Command::SplitTableCells {object_id:parse_object_id(object_id)?,range:range.into()},
        EditorCommandInput::DistributeTableTracks { object_id, axis } => {
            Command::DistributeTableTracks {
                object_id: parse_object_id(object_id)?,
                axis: canonical_table_track_axis(axis),
            }
        }
        EditorCommandInput::SetOpacity {
            object_id,
            opacity_millionths,
        } => set_opacity_command(object_id, opacity_millionths)?,
        EditorCommandInput::SetRotation {
            object_id,
            rotation_microdegrees,
        } => set_rotation_command(object_id, rotation_microdegrees)?,
        EditorCommandInput::UpdateShape {
            object_id,
            shape,
            fill,
            stroke,
        } => update_shape_command(object_id, shape, fill, stroke)?,
        EditorCommandInput::UpdateObjectEffects { object_id, effects } => {
            update_object_effects_command(object_id, effects)?
        }
        EditorCommandInput::DeleteObject { object_id } => Command::DeleteObject {
            object_id: parse_object_id(object_id)?,
        },
    })
}

fn set_opacity_command(object_id: String, opacity_millionths: u32) -> EditorResult<Command> {
    if opacity_millionths > 1_000_000 {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Opacity must be between 0 and 1000000",
        ));
    }
    Ok(Command::SetOpacity {
        object_id: parse_object_id(object_id)?,
        opacity_millionths,
    })
}

fn set_rotation_command(object_id: String, rotation_microdegrees: i64) -> EditorResult<Command> {
    if !(-180_000_000..180_000_000).contains(&rotation_microdegrees) {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Rotation must be at least -180 and less than 180 degrees",
        ));
    }
    Ok(Command::SetRotation {
        object_id: parse_object_id(object_id)?,
        rotation_microdegrees,
    })
}

fn update_shape_command(
    object_id: String,
    shape: ShapeInput,
    fill: ColorInput,
    stroke: ShapeStrokeInput,
) -> EditorResult<Command> {
    Ok(Command::UpdateShape {
        object_id: parse_object_id(object_id)?,
        shape: canonical_shape(shape),
        fill: fill.into(),
        stroke: canonical_shape_stroke(stroke)?,
    })
}

fn canonical_shape(shape: ShapeInput) -> ShapeKind {
    match shape {
        ShapeInput::Rectangle => ShapeKind::Rectangle,
        ShapeInput::Ellipse => ShapeKind::Ellipse,
        ShapeInput::Cube => ShapeKind::Cube,
        ShapeInput::Line => ShapeKind::Line,
        ShapeInput::Triangle => ShapeKind::Triangle,
        ShapeInput::Arrow => ShapeKind::Arrow,
    }
}

fn canonical_shape_stroke(stroke: ShapeStrokeInput) -> EditorResult<ShapeStroke> {
    if stroke.width_millipoints > ShapeStroke::MAX_WIDTH_MILLIPOINTS {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Shape stroke width exceeds the supported limit",
        ));
    }
    Ok(ShapeStroke {
        color: stroke.color.into(),
        width_millipoints: stroke.width_millipoints,
    })
}

fn update_object_effects_command(
    object_id: String,
    effects: ObjectEffectsInput,
) -> EditorResult<Command> {
    let shadow = effects
        .shadow
        .map(|shadow| {
            if shadow.offset_x_um.unsigned_abs() > HardShadow::MAX_OFFSET_MICROMETRES as u64
                || shadow.offset_y_um.unsigned_abs() > HardShadow::MAX_OFFSET_MICROMETRES as u64
            {
                return Err(EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "Shadow offset exceeds the supported limit",
                ));
            }
            Ok(HardShadow {
                color: shadow.color.into(),
                offset_x: Micrometres::new(shadow.offset_x_um),
                offset_y: Micrometres::new(shadow.offset_y_um),
            })
        })
        .transpose()?;
    Ok(Command::UpdateObjectEffects {
        object_id: parse_object_id(object_id)?,
        effects: ObjectEffects {
            shadow,
            text_outline: effects
                .text_outline
                .map(canonical_shape_stroke)
                .transpose()?,
            picture_border: effects
                .picture_border
                .map(canonical_shape_stroke)
                .transpose()?,
        },
    })
}

fn update_page_bleed_command(page_id: String, bleed_um: i64) -> EditorResult<Command> {
    Ok(Command::UpdatePageBleed {
        page_id: parse_page_id(page_id)?,
        bleed: Insets::uniform(bleed_um).map_err(invalid_input)?,
    })
}

fn update_page_trim_command(
    page_id: String,
    trim_width_um: i64,
    trim_height_um: i64,
) -> EditorResult<Command> {
    Ok(Command::UpdatePageTrim {
        page_id: parse_page_id(page_id)?,
        trim: physical_size(trim_width_um, trim_height_um)?,
    })
}

fn reorder_page_command(page_id: String, new_index: usize) -> EditorResult<Command> {
    Ok(Command::ReorderPage {
        page_id: parse_page_id(page_id)?,
        new_index,
    })
}

fn reorder_object_command(object_id: String, new_index: usize) -> EditorResult<Command> {
    Ok(Command::ReorderObject {
        object_id: parse_object_id(object_id)?,
        new_index,
    })
}

fn resize_object_command(
    object_id: String,
    width_um: i64,
    height_um: i64,
) -> EditorResult<Command> {
    Ok(Command::ResizeObject {
        object_id: parse_object_id(object_id)?,
        size: physical_size(width_um, height_um)?,
    })
}

fn duplicate_object_command(
    object_id: String,
    horizontal_offset_um: i64,
    vertical_offset_um: i64,
) -> EditorResult<Command> {
    validate_coordinate(horizontal_offset_um)?;
    validate_coordinate(vertical_offset_um)?;
    Ok(Command::DuplicateObject {
        object_id: parse_object_id(object_id)?,
        offset_x_um: horizontal_offset_um,
        offset_y_um: vertical_offset_um,
    })
}

fn paste_objects_command(
    objects: Vec<PublicationObject>,
    destination_page_id: Option<String>,
    horizontal_offset_um: i64,
    vertical_offset_um: i64,
) -> EditorResult<Command> {
    if objects.is_empty() || objects.len() > 500 {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Paste must contain between one and 500 objects",
        ));
    }
    validate_coordinate(horizontal_offset_um)?;
    validate_coordinate(vertical_offset_um)?;
    Ok(Command::PasteObjects {
        objects,
        destination: optional_page_id(destination_page_id)?
            .map_or(ObjectLocation::Pasteboard, ObjectLocation::Page),
        offset_x_um: horizontal_offset_um,
        offset_y_um: vertical_offset_um,
    })
}

#[allow(clippy::too_many_arguments)]
fn add_text_command(
    page_id: Option<String>,
    name: String,
    text: String,
    style: Option<TextStyleInput>,
    sizing_mode: TextBoxSizingModeInput,
    x_um: i64,
    y_um: i64,
    width_um: i64,
    height_um: i64,
) -> EditorResult<Command> {
    require_text(&name, MAX_TITLE_BYTES, "object name")?;
    require_bounded(&text, MAX_TEXT_BYTES, "text")?;
    validate_text_box_size(sizing_mode, width_um, height_um)?;
    Ok(Command::AddObject {
        location: location(page_id)?,
        object: object_draft(
            name,
            x_um,
            y_um,
            width_um,
            height_um,
            ObjectKind::Text {
                text,
                style: style.map(text_style).transpose()?.unwrap_or_default(),
                frame: TextFrame {
                    sizing_mode: canonical_text_box_sizing_mode(sizing_mode),
                    ..TextFrame::default()
                },
                story: TextStoryFormatting::default(),
            },
        )?,
    })
}

fn update_table_cell_command(
    object_id: String,
    row: u16,
    column: u16,
    text: String,
) -> EditorResult<Command> {
    require_bounded(&text, MAX_TABLE_CELL_TEXT_BYTES, "table cell text")?;
    Ok(Command::UpdateTableCell {
        object_id: parse_object_id(object_id)?,
        row,
        column,
        text,
    })
}

fn update_table_dimensions_command(
    object_id: String,
    rows: u16,
    columns: u16,
) -> EditorResult<Command> {
    if rows == 0 || columns == 0 || rows > TableData::MAX_ROWS || columns > TableData::MAX_COLUMNS {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Table dimensions must be between 1 and 500",
        ));
    }
    Ok(Command::UpdateTableDimensions {
        object_id: parse_object_id(object_id)?,
        rows,
        columns,
    })
}

fn update_text_command(object_id: String, text: String) -> EditorResult<Command> {
    require_bounded(&text, MAX_TEXT_BYTES, "text")?;
    Ok(Command::UpdateText {
        object_id: parse_object_id(object_id)?,
        text,
    })
}

fn replace_text_range_command(
    object_id: String,
    start_byte: usize,
    end_byte: usize,
    text: String,
) -> EditorResult<Command> {
    require_bounded(&text, MAX_TEXT_BYTES, "replacement text")?;
    Ok(Command::ReplaceTextRange {
        object_id: parse_object_id(object_id)?,
        range: TextSourceRange {
            start: start_byte,
            end: end_byte,
        },
        text,
    })
}

fn inline_text_style(
    style: InlineTextStyleInput,
) -> EditorResult<document_core::model::InlineTextStyle> {
    if let Some(family) = &style.font_family {
        require_text(family, MAX_FONT_FAMILY_BYTES, "font family")?;
        if family.chars().count() > MAX_FONT_FAMILY_CHARS {
            return Err(invalid_input("Font family is too long"));
        }
    }
    Ok(document_core::model::InlineTextStyle {
        font_family: style.font_family,
        font_weight: style.font_weight,
        font_size_millipoints: style.font_size_millipoints,
        bold: style.bold,
        italic: style.italic,
        color: style.color.map(Into::into),
        underline: style.underline,
    })
}

fn apply_text_edits_command(object_id: String, edits: Vec<TextEditInput>) -> EditorResult<Command> {
    if edits.is_empty() || edits.len() > MAX_TEXT_EDIT_COUNT {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "A text edit session must contain between one and 10,000 edits",
        ));
    }
    let inserted_bytes = edits
        .iter()
        .try_fold(0usize, |total, edit| total.checked_add(edit.text.len()));
    if inserted_bytes.is_none_or(|total| total > MAX_TEXT_EDIT_INSERTED_BYTES) {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Text edit session is too large",
        ));
    }
    Ok(Command::ApplyTextEdits {
        object_id: parse_object_id(object_id)?,
        edits: edits
            .into_iter()
            .map(|edit| {
                Ok(TextEdit {
                    range: TextSourceRange {
                        start: edit.start_byte,
                        end: edit.end_byte,
                    },
                    text: edit.text,
                    bold: edit.bold,
                    italic: edit.italic,
                    style: edit.style.map(inline_text_style).transpose()?,
                })
            })
            .collect::<EditorResult<_>>()?,
    })
}

fn update_text_style_command(object_id: String, style: TextStyleInput) -> EditorResult<Command> {
    let canonical_style = text_style(style)?;
    let font_face_update = match &canonical_style.font_face {
        Some(binding) => FontFaceBindingUpdate::Replace(Some(binding.clone())),
        None => FontFaceBindingUpdate::PreserveIfFamilyUnchanged,
    };
    Ok(Command::UpdateTextStyle {
        object_id: parse_object_id(object_id)?,
        style: canonical_style,
        font_face_update,
    })
}

fn paragraph_style(style: ParagraphStyleInput) -> ParagraphStyle {
    ParagraphStyle {
        list_style: style.list_style.map(|list_style| match list_style {
            ParagraphListStyleInput::None => ParagraphListStyle::None,
            ParagraphListStyleInput::Bullet => ParagraphListStyle::Bullet,
            ParagraphListStyleInput::Numbered { start_at } => {
                ParagraphListStyle::Numbered { start_at }
            }
        }),
        alignment: style.alignment.map(canonical_text_alignment),
        start_indent_um: style.start_indent_um,
        end_indent_um: style.end_indent_um,
        first_line_indent_um: style.first_line_indent_um,
        line_height_per_thousand: style.line_height_per_thousand,
        space_before_millipoints: style.space_before_millipoints,
        space_after_millipoints: style.space_after_millipoints,
    }
}

fn update_table_grid_border_command(
    object_id: String,
    border: TableGridBorderInput,
) -> EditorResult<Command> {
    if border.width_millipoints > ShapeStroke::MAX_WIDTH_MILLIPOINTS {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Table border width is outside the supported range",
        ));
    }
    Ok(Command::UpdateTableGridBorder {
        object_id: parse_object_id(object_id)?,
        border: TableGridBorder {
            color: border.color.into(),
            width_millipoints: border.width_millipoints,
        },
    })
}

const fn canonical_table_vertical_alignment(
    alignment: TableVerticalAlignmentInput,
) -> TableVerticalAlignment {
    match alignment {
        TableVerticalAlignmentInput::Top => TableVerticalAlignment::Top,
        TableVerticalAlignmentInput::Middle => TableVerticalAlignment::Middle,
        TableVerticalAlignmentInput::Bottom => TableVerticalAlignment::Bottom,
    }
}

const fn canonical_table_track_axis(axis: TableTrackAxisInput) -> TableTrackAxis {
    match axis {
        TableTrackAxisInput::Row => TableTrackAxis::Row,
        TableTrackAxisInput::Column => TableTrackAxis::Column,
    }
}

const fn canonical_text_alignment(alignment: TextAlignmentInput) -> TextAlignment {
    match alignment {
        TextAlignmentInput::Left => TextAlignment::Left,
        TextAlignmentInput::Center => TextAlignment::Center,
        TextAlignmentInput::Right => TextAlignment::Right,
        TextAlignmentInput::Justify => TextAlignment::Justify,
    }
}

fn update_table_text_style_command(
    object_id: String,
    style: TextStyleInput,
) -> EditorResult<Command> {
    let canonical_style = text_style(style)?;
    let font_face_update = match &canonical_style.font_face {
        Some(binding) => FontFaceBindingUpdate::Replace(Some(binding.clone())),
        None => FontFaceBindingUpdate::PreserveIfFamilyUnchanged,
    };
    Ok(Command::UpdateTableTextStyle {
        object_id: parse_object_id(object_id)?,
        style: canonical_style,
        font_face_update,
    })
}

fn update_text_frame_command(
    object_id: String,
    column_count: u16,
    column_gap_um: i64,
) -> EditorResult<Command> {
    if !(1..=12).contains(&column_count) || !(0..=1_000_000).contains(&column_gap_um) {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Text columns or column gap are outside the supported range",
        ));
    }
    Ok(Command::UpdateTextFrame {
        object_id: parse_object_id(object_id)?,
        column_count,
        column_gap_um,
    })
}

fn validate_text_box_size(
    sizing_mode: TextBoxSizingModeInput,
    width_um: i64,
    height_um: i64,
) -> EditorResult<()> {
    if sizing_mode == TextBoxSizingModeInput::Fit
        && (width_um < TextFrame::MIN_FIT_WIDTH_MICROMETRES
            || height_um < TextFrame::MIN_FIT_HEIGHT_MICROMETRES)
    {
        Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Auto-size Text Box geometry is below the visible minimum",
        ))
    } else {
        Ok(())
    }
}

const fn canonical_text_box_sizing_mode(input: TextBoxSizingModeInput) -> TextBoxSizingMode {
    match input {
        TextBoxSizingModeInput::Fit => TextBoxSizingMode::Fit,
        TextBoxSizingModeInput::Fixed => TextBoxSizingMode::Fixed,
    }
}

fn text_style(style: TextStyleInput) -> EditorResult<TextStyle> {
    require_text(&style.font_family, MAX_FONT_FAMILY_BYTES, "font family")?;
    if style.font_family.chars().count() > MAX_FONT_FAMILY_CHARS
        || !(1_000..=1_000_000).contains(&style.font_size_millipoints)
        || !(1..=1_000).contains(&style.font_weight)
        || !(500..=5_000).contains(&style.line_height_per_thousand)
    {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Text style values are outside the supported range",
        ));
    }
    Ok(TextStyle {
        font_face: style
            .font_face
            .map(|binding| {
                Ok(FontFaceBinding {
                    source: AssetHash::parse(binding.source).map_err(invalid_input)?,
                    face_index: binding.face_index,
                })
            })
            .transpose()?,
        exact_font_variants: Vec::new(),
        exact_font_families: Vec::new(),
        font_family: style.font_family,
        font_size_millipoints: style.font_size_millipoints,
        font_weight: style.font_weight,
        italic: style.italic,
        underline: style.underline,
        alignment: canonical_text_alignment(style.alignment),
        color: style.color.into(),
        line_height_per_thousand: style.line_height_per_thousand,
    })
}

#[allow(clippy::too_many_arguments)]
fn add_shape_command(
    page_id: Option<String>,
    name: String,
    shape: ShapeInput,
    x_um: i64,
    y_um: i64,
    width_um: i64,
    height_um: i64,
    fill: ColorInput,
    stroke: Option<ShapeStrokeInput>,
) -> EditorResult<Command> {
    require_text(&name, MAX_TITLE_BYTES, "object name")?;
    Ok(Command::AddObject {
        location: location(page_id)?,
        object: object_draft(
            name,
            x_um,
            y_um,
            width_um,
            height_um,
            ObjectKind::Shape {
                shape: canonical_shape(shape),
                fill: fill.into(),
                stroke: stroke
                    .map(canonical_shape_stroke)
                    .transpose()?
                    .unwrap_or_default(),
                wrap_mode: WrapMode::None,
            },
        )?,
    })
}

#[allow(clippy::too_many_arguments)]
fn add_table_command(
    page_id: Option<String>,
    name: String,
    rows: u16,
    columns: u16,
    style: Option<TextStyleInput>,
    x_um: i64,
    y_um: i64,
    width_um: i64,
    height_um: i64,
) -> EditorResult<Command> {
    require_text(&name, MAX_TITLE_BYTES, "object name")?;
    let mut table = TableData::empty(rows, columns).map_err(invalid_input)?;
    if let Some(style) = style {
        table.style = text_style(style)?;
    }
    Ok(Command::AddObject {
        location: location(page_id)?,
        object: object_draft(
            name,
            x_um,
            y_um,
            width_um,
            height_um,
            ObjectKind::Table { table },
        )?,
    })
}

fn move_object_command(
    object_id: String,
    page_id: Option<String>,
    x_um: i64,
    y_um: i64,
) -> EditorResult<Command> {
    validate_coordinate(x_um)?;
    validate_coordinate(y_um)?;
    Ok(Command::SetObjectPosition {
        object_id: parse_object_id(object_id)?,
        destination: location(page_id)?,
        x: Micrometres::new(x_um),
        y: Micrometres::new(y_um),
    })
}

fn object_draft(
    name: String,
    x_um: i64,
    y_um: i64,
    width_um: i64,
    height_um: i64,
    kind: ObjectKind,
) -> EditorResult<ObjectDraft> {
    validate_coordinate(x_um)?;
    validate_coordinate(y_um)?;
    Ok(ObjectDraft {
        name,
        size: physical_size(width_um, height_um)?,
        transform: Transform::identity_at(x_um, y_um),
        opacity_millionths: 1_000_000,
        kind,
    })
}

fn physical_size(width_um: i64, height_um: i64) -> EditorResult<PhysicalSize> {
    if width_um > MAX_DIMENSION_UM || height_um > MAX_DIMENSION_UM {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Dimensions exceed the supported document limit",
        ));
    }
    PhysicalSize::new(width_um, height_um).map_err(invalid_input)
}

fn validate_coordinate(value: i64) -> EditorResult<()> {
    if (-MAX_COORDINATE_UM..=MAX_COORDINATE_UM).contains(&value) {
        Ok(())
    } else {
        Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "Coordinate exceeds the supported document limit",
        ))
    }
}

fn parse_page_id(value: String) -> EditorResult<PageId> {
    PageId::parse(value).map_err(invalid_input)
}

fn parse_object_id(value: String) -> EditorResult<ObjectId> {
    ObjectId::parse(value).map_err(invalid_input)
}

fn parse_guide_id(value: String) -> EditorResult<GuideId> {
    GuideId::parse(value).map_err(invalid_input)
}

const fn canonical_guide_axis(axis: GuideAxisInput) -> GuideAxis {
    match axis {
        GuideAxisInput::Horizontal => GuideAxis::Horizontal,
        GuideAxisInput::Vertical => GuideAxis::Vertical,
    }
}

fn optional_page_id(value: Option<String>) -> EditorResult<Option<PageId>> {
    value.map(parse_page_id).transpose()
}

fn location(page_id: Option<String>) -> EditorResult<ObjectLocation> {
    Ok(match optional_page_id(page_id)? {
        Some(page_id) => ObjectLocation::Page(page_id),
        None => ObjectLocation::Pasteboard,
    })
}

fn require_text(value: &str, maximum: usize, field: &str) -> EditorResult<()> {
    if value.trim().is_empty() {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            format!("{field} cannot be empty"),
        ));
    }
    require_bounded(value, maximum, field)
}

fn require_bounded(value: &str, maximum: usize, field: &str) -> EditorResult<()> {
    if value.len() <= maximum {
        Ok(())
    } else {
        Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            format!("{field} is too long"),
        ))
    }
}

fn no_document() -> EditorError {
    EditorError::new(EditorErrorCode::NoDocument, "No publication is open")
}

fn stale_document_transition() -> EditorError {
    EditorError::new(
        EditorErrorCode::CommandRejected,
        "The publication changed while the document action was open",
    )
}

fn invalid_input(error: impl std::fmt::Display) -> EditorError {
    EditorError::new(EditorErrorCode::InvalidInput, error.to_string())
}

fn command_rejected(error: impl std::fmt::Display) -> EditorError {
    EditorError::new(EditorErrorCode::CommandRejected, error.to_string())
}

// Opening has different repair guidance from saving: the selected file has not
// replaced the current session, and a rejected package cannot be edited in place.
#[allow(clippy::needless_pass_by_value)]
fn publication_open_failed(error: document_core::PackageError) -> EditorError {
    use document_core::PackageError;
    let message = match &error {
        PackageError::UnsupportedSchema { .. } => {
            "This publication uses a file version this app cannot open. Open it with a compatible version of Publisher V2."
        }
        PackageError::InvalidArchiveEntry
        | PackageError::MissingManifest
        | PackageError::UnexpectedArchiveEntry(_)
        | PackageError::DuplicateArchiveEntry(_)
        | PackageError::Zip(_)
        | PackageError::Json(_)
        | PackageError::InvalidModel(_)
        | PackageError::MissingAsset(_)
        | PackageError::UnexpectedAsset(_)
        | PackageError::AssetHashMismatch(_)
        | PackageError::AssetLengthMismatch { .. }
        | PackageError::ArchiveEntryLengthMismatch { .. } => {
            "This file is damaged or is not a Publisher V2 publication. Try another copy or open a recovery copy from the Start screen."
        }
        PackageError::ArchiveChangedDuringRead => {
            "This file changed while it was opening. Wait for the other program to finish, then try Open again."
        }
        PackageError::Io(source) if source.kind() == std::io::ErrorKind::NotFound => {
            "This publication could not be found. Check its location, then choose Open to find it again."
        }
        PackageError::Io(source) if source.kind() == std::io::ErrorKind::PermissionDenied => {
            "This publication could not be read. Check that you have access to the file, or try another copy."
        }
        _ => return persistence_failed(error),
    };
    EditorError::new(EditorErrorCode::PersistenceFailed, message)
}

// Own the error so this mapper can be passed directly to Result::map_err.
#[allow(clippy::needless_pass_by_value)]
fn persistence_failed(error: document_core::PackageError) -> EditorError {
    use document_core::PackageError;
    let message = match &error {
        PackageError::Io(source) | PackageError::WindowsAtomicReplace { source, .. }
            if matches!(source.kind(), std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidInput) =>
            "Choose a folder on a fixed local NTFS drive, then save a copy there.".to_owned(),
        PackageError::InvalidDestinationName | PackageError::DestinationHasNoParent =>
            "Choose a folder on a fixed local NTFS drive, then save a copy there.".to_owned(),
        PackageError::Io(source) | PackageError::WindowsAtomicReplace { source, .. }
            if matches!(source.raw_os_error(), Some(32 | 33)) =>
            "Another program is using this publication. Close it there, then try Save again. You can also save a copy to another location.".to_owned(),
        PackageError::DestinationExists(_) =>
            "A file already exists at that location. Choose a different name or folder in Save As, then try again. Your current work remains open.".to_owned(),
        PackageError::DestinationChanged | PackageError::InterruptedSaveRecoveryChanged =>
            "The saved publication or its pending save changed outside this session. Save a copy to another location to preserve your current work.".to_owned(),
        PackageError::InterruptedSavePresent(_) | PackageError::InvalidSaveJournal
        | PackageError::InterruptedSaveRecoveryAmbiguous
        | PackageError::InterruptedSaveRecoveryReadOnly
        | PackageError::InterruptedSaveRecoveryChoiceUnavailable
        | PackageError::InterruptedSaveRecoveryUnsupported =>
            "An earlier save needs recovery before this file can be changed. Save a copy to another location to preserve your current work.".to_owned(),
        _ => error.to_string(),
    };
    EditorError::new(EditorErrorCode::PersistenceFailed, message)
}

#[cfg(test)]
mod persistence_message_tests {
    use super::{EditorErrorCode, persistence_failed};
    use document_core::PackageError;
    use std::path::PathBuf;

    #[test]
    fn existing_save_as_destination_gives_repair_guidance_without_internal_path() {
        let error = persistence_failed(PackageError::DestinationExists(PathBuf::from(
            r"C:\Users\TestUser\Private\occupied.publisher-v2",
        )));

        assert_eq!(error.code, EditorErrorCode::PersistenceFailed);
        assert_eq!(
            error.message,
            "A file already exists at that location. Choose a different name or folder in Save As, then try again. Your current work remains open."
        );
        assert!(!error.message.contains("DestinationExists"));
        assert!(!error.message.contains("C:\\"));
    }
}

fn recovery_failed(error: impl std::fmt::Display) -> EditorError {
    EditorError::new(EditorErrorCode::RecoveryFailed, error.to_string())
}

impl From<ColorInput> for Color {
    fn from(value: ColorInput) -> Self {
        Self {
            red: value.red,
            green: value.green,
            blue: value.blue,
            alpha: value.alpha,
        }
    }
}

// Prospective metadata can include pending imports and resources retained only by
// undo. Count exactly the resources the resulting publication will serialize.
fn validate_reachable_saveability(
    document: &document_core::model::Document,
    revision: Revision,
    descriptors: &std::collections::BTreeMap<AssetHash, AssetDescriptor>,
) -> Result<(), document_core::PackageError> {
    let selected = referenced_asset_hashes(document)
        .into_iter()
        .filter_map(|hash| {
            descriptors
                .get(&hash)
                .map(|descriptor| (hash, descriptor.clone()))
        })
        .collect();
    validate_document_saveability(document, revision, &selected)
}

#[cfg(test)]
mod saveability_tests {
    use super::*;

    #[test]
    fn prospective_asset_budget_ignores_resources_retained_only_by_history() {
        let mut editor = EditorModule::new();
        editor
            .create(CreateDocumentInput {
                document_id: "history-budget".into(),
                title: "History".into(),
                trim_width_um: 100_000,
                trim_height_um: 100_000,
                bleed_um: 0,
                page_count: 1,
            })
            .unwrap();
        let before = editor.snapshot().unwrap();
        let page = &before.project.document.pages[0];
        editor
            .execute_saveable(
                before.project.revision,
                Transaction::single(
                    "Add Page",
                    Command::AddPage {
                        after: None,
                        trim: page.trim,
                        bleed: page.bleed,
                    },
                ),
                [(
                    AssetHash::for_bytes(b"unreferenced history"),
                    AssetDescriptor {
                        media_type: "font/ttf".into(),
                        byte_length: document_core::package::PACKAGE_LIMITS_V1
                            .aggregate_asset_bytes
                            + 1,
                        raster: None,
                    },
                )],
            )
            .unwrap();
        assert_eq!(editor.snapshot().unwrap().project.document.pages.len(), 2);
    }

    #[test]
    fn asset_admission_rejects_atomically_and_deduplicates_existing_hashes() {
        let mut editor = EditorModule::new();
        editor
            .create(CreateDocumentInput {
                document_id: "admission-test".into(),
                title: "Budget test".into(),
                trim_width_um: 100_000,
                trim_height_um: 100_000,
                bleed_um: 0,
                page_count: 1,
            })
            .unwrap();
        let hash = editor
            .assets
            .insert("font/ttf", b"existing trusted resource".to_vec());
        let before = editor.snapshot().unwrap();
        let assets = editor.assets.clone();
        let transaction = |source| {
            Transaction::single(
                "Set fallback",
                Command::SetExactFontFallbacks {
                    fallbacks: vec![document_core::model::ExactFontFallbackChain {
                        script: document_core::model::FontFallbackScript::Latin,
                        faces: vec![FontFaceBinding {
                            source,
                            face_index: 0,
                        }],
                    }],
                },
            )
        };
        let descriptor = AssetDescriptor {
            media_type: "font/ttf".into(),
            byte_length: document_core::package::PACKAGE_LIMITS_V1.aggregate_asset_bytes + 1,
            raster: None,
        };
        assert!(
            editor
                .execute_saveable(
                    before.project.revision,
                    transaction(AssetHash::for_bytes(b"new resource")),
                    [(AssetHash::for_bytes(b"new resource"), descriptor.clone())]
                )
                .is_err()
        );
        assert_eq!(editor.snapshot().unwrap(), before);
        assert_eq!(editor.assets, assets);
        // AssetStore insertion keeps the existing entry for identical content.
        editor
            .execute_saveable(
                before.project.revision,
                transaction(hash.clone()),
                [(hash, descriptor)],
            )
            .unwrap();
        assert_eq!(editor.assets, assets);
        assert_eq!(
            editor
                .snapshot()
                .unwrap()
                .project
                .document
                .exact_font_fallbacks
                .len(),
            1
        );
    }
}
