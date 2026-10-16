//! Immutable Save work separated from the live editing session and its history.
use super::{
    EditorError, EditorErrorCode, EditorModule, EditorResult, EditorSnapshot, ProjectBinding,
    no_document,
};
use document_core::{DocumentId, ProjectSnapshot};

pub struct EditorSaveJob {
    document_id: DocumentId,
    original: Option<ProjectBinding>,
    session_identity: std::sync::Arc<()>,
    staged: EditorModule,
}

pub struct EditorSaveJobOutcome {
    document_id: DocumentId,
    original: Option<ProjectBinding>,
    session_identity: std::sync::Arc<()>,
    saved: Option<ProjectBinding>,
    saved_schema: u32,
    result: EditorResult<()>,
}

impl EditorModule {
    /// Capture a Save without transferring ownership of current edits or Undo history.
    ///
    /// # Errors
    /// Returns the normal Save admission error if no compatible bound publication is open.
    pub fn prepare_save_job(&self) -> EditorResult<EditorSaveJob> {
        if self.requires_save_copy() {
            return Err(EditorError::new(
                EditorErrorCode::PersistenceFailed,
                "Save a copy of this earlier-format publication first",
            ));
        }
        self.project.as_ref().ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::PersistenceFailed,
                "Save this publication to a new location first",
            )
        })?;
        self.prepare_save_new_job()
    }

    /// Capture a first Save or Save Copy without changing the editable session.
    ///
    /// # Errors
    /// Returns a no-document or snapshot admission error.
    pub fn prepare_save_new_job(&self) -> EditorResult<EditorSaveJob> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        let original = self.project.clone();
        let project = ProjectSnapshot::from_session(session);
        let document_id = project.document.id.clone();
        let mut staged = Self::new();
        staged.session = Some(project.into_session().map_err(super::command_rejected)?);
        staged.assets = self.assets.for_document(session.document());
        staged.project.clone_from(&original);
        staged.source_schema_version = self.source_schema_version;
        Ok(EditorSaveJob {
            document_id,
            original,
            session_identity: std::sync::Arc::clone(&self.clipboard_binding),
            staged,
        })
    }

    /// Accept only file-binding metadata; edits made during Save remain live and dirty.
    ///
    /// # Errors
    /// Refuses a result for a replaced document or changed binding, or returns the Save error.
    pub fn complete_save_job(&mut self, outcome: EditorSaveJobOutcome) -> EditorResult<()> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        let binding_matches = match (self.project.as_ref(), outcome.original.as_ref()) {
            (None, None) => true,
            (Some(current), Some(expected)) => same_binding(current, expected),
            _ => false,
        };
        if session.document().id != outcome.document_id
            || !binding_matches
            || !std::sync::Arc::ptr_eq(&self.clipboard_binding, &outcome.session_identity)
        {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The publication changed before its Save result could be accepted",
            ));
        }
        self.project = outcome.saved;
        self.source_schema_version = outcome.saved_schema;
        outcome.result
    }
}

fn same_binding(current: &ProjectBinding, expected: &ProjectBinding) -> bool {
    current.path == expected.path
        && current.fingerprint == expected.fingerprint
        && current.generation == expected.generation
        && current.revision == expected.revision
        && current.failed_save == expected.failed_save
}

impl EditorSaveJob {
    /// Logical embedded bytes retained by this captured Save; assets are shared immutable data.
    #[must_use]
    pub fn retained_asset_bytes(&self) -> u64 {
        let Some(session) = self.staged.session.as_ref() else {
            return 0;
        };
        document_core::assets::referenced_asset_hashes(session.document())
            .iter()
            .filter_map(|hash| self.staged.assets.get(hash))
            .map(|asset| asset.bytes.len() as u64)
            .sum()
    }

    /// Capture metadata for local diagnostics; assets remain shared immutable storage.
    ///
    /// # Errors
    /// Returns a snapshot error if the captured publication is unavailable.
    pub fn snapshot(&self) -> EditorResult<EditorSnapshot> {
        self.staged.snapshot()
    }

    /// Runs without borrowing the editable document. Captured document/assets drop on return.
    #[must_use]
    pub fn run(mut self, cancelled: impl Fn() -> bool) -> EditorSaveJobOutcome {
        let result = self.staged.save_current_cancellable(cancelled);
        self.finish(result)
    }

    /// Runs first Save/Save Copy staging and no-overwrite publication outside the editing lock.
    #[must_use]
    pub fn run_new(
        mut self,
        path: &std::path::Path,
        cancelled: impl Fn() -> bool,
    ) -> EditorSaveJobOutcome {
        let result = self.staged.save_new_cancellable(path, cancelled);
        self.finish(result)
    }

    fn finish(mut self, result: EditorResult<()>) -> EditorSaveJobOutcome {
        EditorSaveJobOutcome {
            document_id: self.document_id,
            original: self.original,
            session_identity: self.session_identity,
            saved: self.staged.project.take(),
            saved_schema: self.staged.source_schema_version,
            result,
        }
    }
}
