//! Revision-checked, transaction-scoped document commands and undo/redo.

use crate::assets::{AssetHash, referenced_asset_hashes};
use crate::model::{
    Color, CornerRounding, Document, EdgeExpectations, ExactFontFallbackChain, ExactFontFamilyBinding,
    ExactFontVariantBinding, FontFaceBinding, GroupId, GuideAxis, GuideId, ImageContentKind,
    ImageCrop, ImageFlip, InlineTextSpan, InlineTextStyle, Insets, MAX_EXACT_FONT_FAMILY_BINDINGS,
    MAX_EXACT_FONT_VARIANTS, Micrometres, ModelError, ObjectDraft, ObjectEffects, ObjectId,
    ObjectKind, Page, PageGuide, PageId, ParagraphRange, ParagraphStyle, ParagraphStyleRun,
    PhysicalSize, PublicationObject, Revision, ShapeKind, ShapeStroke, TableGridBorder,
    TableTrackAxis, TableVerticalAlignment, TextAlignment, TextBoxSizingMode, TextFrame,
    TextSourceRange, TextStoryFormatting, TextStyle, TextVerticalAlignment, Transform,
    UntaggedRasterPolicy, WrapMode, WrapSettings, paragraph_count,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
mod master_pages;

/// Session-only limits. Snapshot bytes measure serialized content, not heap usage.
/// Current publication content and its resources are always exempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryRetentionLimits {
    pub max_transactions: usize,
    pub max_snapshot_bytes: u64,
    pub max_history_asset_bytes: u64,
}

impl Default for HistoryRetentionLimits {
    fn default() -> Self {
        Self {
            max_transactions: 256,
            max_snapshot_bytes: 64 * 1024 * 1024,
            max_history_asset_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRetentionStatus {
    pub undo_transactions: usize,
    pub redo_transactions: usize,
    pub older_history_removed: bool,
    pub snapshot_bytes: u64,
    pub history_asset_bytes: u64,
    pub limits: HistoryRetentionLimits,
}

#[derive(Debug)]
struct HistorySnapshot {
    document: Document,
    serialized_bytes: u64,
    assets: BTreeSet<AssetHash>,
}

impl HistorySnapshot {
    fn new(document: Document) -> Result<Self, CommandError> {
        #[derive(Default)]
        struct Counter(u64);
        impl std::io::Write for Counter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self.0.saturating_add(bytes.len() as u64);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut counter = Counter::default();
        serde_json::to_writer(&mut counter, &document)
            .map_err(|error| CommandError::Saveability(error.to_string()))?;
        let assets = referenced_asset_hashes(&document);
        Ok(Self {
            document,
            serialized_bytes: counter.0,
            assets,
        })
    }
}

impl std::ops::Deref for HistorySnapshot {
    type Target = Document;
    fn deref(&self) -> &Document {
        &self.document
    }
}

/// Maximum ordered edits accepted in one atomic Text Box batch.
pub const MAX_TEXT_EDIT_COUNT: usize = 10_000;
/// Maximum aggregate UTF-8 replacement bytes accepted in one atomic batch.
pub const MAX_TEXT_EDIT_INSERTED_BYTES: usize = crate::model::MAX_TEXT_BYTES * 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObjectLocation {
    Page(PageId),
    Pasteboard,
    Master(crate::model::MasterId),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectTransformUpdate {
    pub object_id: ObjectId,
    pub transform: Transform,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArrangeOperation {
    BringForward,
    SendBackward,
    BringToFront,
    SendToBack,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArrangeGeometryOperation {
    AlignLeft,
    AlignCenter,
    AlignRight,
    AlignTop,
    AlignMiddle,
    AlignBottom,
    DistributeHorizontally,
    DistributeVertically,
}

impl ArrangeGeometryOperation {
    const fn minimum_units(self) -> usize {
        match self {
            Self::AlignLeft
            | Self::AlignCenter
            | Self::AlignRight
            | Self::AlignTop
            | Self::AlignMiddle
            | Self::AlignBottom => 2,
            Self::DistributeHorizontally | Self::DistributeVertically => 3,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::large_enum_variant, reason = "canonical commands retain owned value payloads; boxing AddObject would broaden the established public API")]
pub enum Command {
    AddMaster { name: String, trim: PhysicalSize },
    RenameMaster { master_id: crate::model::MasterId, name: String },
    DuplicateMaster { master_id: crate::model::MasterId, name: String },
    ResizeMaster { master_id: crate::model::MasterId, trim: PhysicalSize },
    AssignMaster { page_ids: Vec<PageId>, master_id: Option<crate::model::MasterId> },
    RemoveMaster { master_id: crate::model::MasterId, replacement: Option<crate::model::MasterId> },
    UpdatePageNumbering { page_id: PageId, section: Option<crate::model::PageNumberingSection> },
    SetPageNumberSuppression { page_ids: Vec<PageId>, suppressed: bool },
    SetTextField { object_id: ObjectId, field: Option<crate::model::TextField> },
    AddPage {
        after: Option<PageId>,
        trim: PhysicalSize,
        bleed: Insets,
    },
    DuplicatePage {
        page_id: PageId,
    },
    RenamePage {
        page_id: PageId,
        name: String,
    },
    DeletePage {
        page_id: PageId,
    },
    ReorderPage {
        page_id: PageId,
        new_index: usize,
    },
    UpdatePageBleed {
        page_id: PageId,
        bleed: Insets,
    },
    UpdatePageTrim {
        page_id: PageId,
        trim: PhysicalSize,
    },
    SetExactFontFallbacks {
        fallbacks: Vec<ExactFontFallbackChain>,
    },
    AddGuide {
        page_id: PageId,
        axis: GuideAxis,
        position: Micrometres,
    },
    MoveGuide {
        page_id: PageId,
        guide_id: GuideId,
        position: Micrometres,
    },
    DeleteGuide {
        page_id: PageId,
        guide_id: GuideId,
    },
    AddObject {
        location: ObjectLocation,
        object: ObjectDraft,
    },
    DuplicateObject {
        object_id: ObjectId,
        offset_x_um: i64,
        offset_y_um: i64,
    },
    PasteObjects {
        objects: Vec<PublicationObject>,
        destination: ObjectLocation,
        offset_x_um: i64,
        offset_y_um: i64,
    },
    GroupObjects {
        object_ids: Vec<ObjectId>,
    },
    UngroupObjects {
        object_ids: Vec<ObjectId>,
    },
    SetObjectLock {
        object_ids: Vec<ObjectId>,
        locked: bool,
    },
    MoveObject {
        object_id: ObjectId,
        destination: ObjectLocation,
        transform: Transform,
    },
    /// Change location/translation while preserving the current rotation,
    /// pivot, scale and shear, including earlier commands in this transaction.
    SetObjectPosition {
        object_id: ObjectId,
        destination: ObjectLocation,
        x: Micrometres,
        y: Micrometres,
    },
    ReorderObject {
        object_id: ObjectId,
        new_index: usize,
    },
    ArrangeObject {
        object_id: ObjectId,
        operation: ArrangeOperation,
    },
    ArrangeSelection {
        object_ids: Vec<ObjectId>,
        operation: ArrangeOperation,
    },
    ArrangeGeometry {
        object_ids: Vec<ObjectId>,
        operation: ArrangeGeometryOperation,
    },
    ArrangeGeometryToPage {
        object_ids: Vec<ObjectId>,
        operation: ArrangeGeometryOperation,
    },
    MoveSelection {
        object_ids: Vec<ObjectId>,
        destination: Option<ObjectLocation>,
        delta_x_um: i64,
        delta_y_um: i64,
    },
    DeleteSelection {
        object_ids: Vec<ObjectId>,
    },
    ApplySelectionTransforms {
        objects: Vec<ObjectTransformUpdate>,
    },
    /// Explicit Edit Group operation; only the requested member matrices change.
    ApplyMemberTransforms {
        objects: Vec<ObjectTransformUpdate>,
    },
    UpdateCornerRounding {
        object_id: ObjectId,
        rounding: CornerRounding,
    },
    ResizeObject {
        object_id: ObjectId,
        size: PhysicalSize,
    },
    UpdateText {
        object_id: ObjectId,
        text: String,
    },
    ReplaceTextRange {
        object_id: ObjectId,
        range: TextSourceRange,
        text: String,
    },
    ApplyTextEdits {
        object_id: ObjectId,
        edits: Vec<TextEdit>,
    },
    UpdateTextStyle {
        object_id: ObjectId,
        style: TextStyle,
        font_face_update: FontFaceBindingUpdate,
    },
    SetTextFontVariantBinding {
        object_id: ObjectId,
        font_weight: u16,
        italic: bool,
        font_face: Option<FontFaceBinding>,
    },
    SetTextFontFamilyBinding {
        object_id: ObjectId,
        font_family: String,
        font_weight: u16,
        italic: bool,
        font_face: FontFaceBinding,
    },
    SetInlineFontSize {
        object_id: ObjectId,
        range: TextSourceRange,
        font_size_millipoints: u32,
    },
    UpdateInlineTextStyle {
        object_id: ObjectId,
        range: TextSourceRange,
        bold: Option<bool>,
        italic: Option<bool>,
    },
    UpdateInlineTextAppearance {
        object_id: ObjectId,
        range: TextSourceRange,
        style: InlineTextStyle,
    },
    UpdateParagraphStyle {
        object_id: ObjectId,
        range: ParagraphRange,
        style: ParagraphStyle,
    },
    UpdateTextFrame {
        object_id: ObjectId,
        column_count: u16,
        column_gap_um: i64,
    },
    UpdateTextBoxInterior {
        object_id: ObjectId,
        margins: Insets,
        vertical_alignment: TextVerticalAlignment,
    },
    UpdateTextBoxSizing {
        object_id: ObjectId,
        sizing_mode: TextBoxSizingMode,
        size: PhysicalSize,
    },
    UpdateWrapMode {
        object_id: ObjectId,
        wrap_mode: WrapMode,
    },
    UpdateWrapSettings {
        object_id: ObjectId,
        settings: WrapSettings,
    },
    UpdateImageContentKind {
        object_id: ObjectId,
        content_kind: ImageContentKind,
    },
    UpdateImageCrop {
        object_id: ObjectId,
        crop: ImageCrop,
    },
    SetImageFlip {
        object_id: ObjectId,
        flip: ImageFlip,
    },
    ResetImageEditing {
        object_id: ObjectId,
    },
    ReplaceImage {
        object_id: ObjectId,
        asset: AssetHash,
        name: String,
    },
    SetImageAspectLock {
        object_id: ObjectId,
        locked: bool,
    },
    UpdateUntaggedRasterPolicy {
        policy: UntaggedRasterPolicy,
    },
    UpdateTableCell {
        object_id: ObjectId,
        row: u16,
        column: u16,
        text: String,
    },
    UpdateTableCells {
        object_id: ObjectId,
        row: u16,
        column: u16,
        cells: Vec<Vec<String>>,
    },
    UpdateTableDimensions {
        object_id: ObjectId,
        rows: u16,
        columns: u16,
    },
    InsertTableTrack {
        object_id: ObjectId,
        axis: TableTrackAxis,
        index: u16,
    },
    DeleteTableTrack {
        object_id: ObjectId,
        axis: TableTrackAxis,
        index: u16,
    },
    UpdateTableTextStyle {
        object_id: ObjectId,
        style: TextStyle,
        font_face_update: FontFaceBindingUpdate,
    },
    UpdateTableGridBorder {
        object_id: ObjectId,
        border: TableGridBorder,
    },
    UpdateTableCellAlignment {
        object_id: ObjectId,
        row: u16,
        column: u16,
        horizontal_alignment: Option<TextAlignment>,
        vertical_alignment: Option<TableVerticalAlignment>,
    },
    UpdateTableCellFormatting {
        object_id: ObjectId,
        range: crate::model::TableCellRange,
        formatting: crate::model::TableCellFormattingPatch,
    },
    ResetTableCellFormatting {
        object_id: ObjectId,
        range: crate::model::TableCellRange,
    },
    MergeTableCells {
        object_id: ObjectId,
        range: crate::model::TableCellRange,
    },
    SplitTableCells {
        object_id: ObjectId,
        range: crate::model::TableCellRange,
    },
    /// Changes one row or column's positive relative share of the Table's
    /// current object size. Resizing the object scales every track together.
    UpdateTableTrackSize {
        object_id: ObjectId,
        axis: TableTrackAxis,
        index: u16,
        weight: u32,
    },
    DistributeTableTracks {
        object_id: ObjectId,
        axis: TableTrackAxis,
    },
    SetOpacity {
        object_id: ObjectId,
        opacity_millionths: u32,
    },
    SetRotation {
        object_id: ObjectId,
        rotation_microdegrees: i64,
    },
    UpdateShape {
        object_id: ObjectId,
        shape: ShapeKind,
        fill: Color,
        stroke: ShapeStroke,
    },
    UpdateObjectEffects {
        object_id: ObjectId,
        effects: ObjectEffects,
    },
    DeleteObject {
        object_id: ObjectId,
    },
}

/// One exact UTF-8 source replacement. Its range is interpreted against the
/// story produced by every preceding edit in the same batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    pub range: TextSourceRange,
    pub text: String,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub style: Option<InlineTextStyle>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transaction {
    pub label: String,
    pub commands: Vec<Command>,
}

impl Transaction {
    #[must_use]
    pub fn single(label: impl Into<String>, command: Command) -> Self {
        Self {
            label: label.into(),
            commands: vec![command],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutcome {
    pub revision: Revision,
    pub created_pages: Vec<PageId>,
    pub created_objects: Vec<ObjectId>,
    pub created_masters: Vec<crate::model::MasterId>,
}

#[derive(Clone, Debug)]
struct HistoryEntry {
    before: Arc<HistorySnapshot>,
    after: Arc<HistorySnapshot>,
}

/// One open editing session. History is intentionally session-only in version one.
#[derive(Clone, Debug)]
pub struct DocumentSession {
    // Adjacent history entries and the current state share immutable snapshots.
    // A command edits a separate prospective document before publishing it.
    document: Arc<HistorySnapshot>,
    revision: Revision,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    history_limits: HistoryRetentionLimits,
    older_history_removed: bool,
    asset_sizes: BTreeMap<AssetHash, u64>,
}

impl DocumentSession {
    /// Starts an editing session at the initial revision.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] when the supplied document is invalid.
    pub fn new(document: Document) -> Result<Self, CommandError> {
        Self::from_saved(document, Revision::initial())
    }

    /// Restores an editing session at a previously saved revision.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] when the supplied document is invalid.
    pub fn from_saved(document: Document, revision: Revision) -> Result<Self, CommandError> {
        document.validate()?;
        Ok(Self {
            document: Arc::new(HistorySnapshot::new(document)?),
            revision,
            undo: Vec::new(),
            redo: Vec::new(),
            history_limits: HistoryRetentionLimits::default(),
            older_history_removed: false,
            asset_sizes: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn document(&self) -> &Document {
        &self.document
    }

    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Whether this session has released any history under its retention policy.
    #[must_use]
    pub const fn older_history_removed(&self) -> bool {
        self.older_history_removed
    }

    /// Applies a session policy immediately. Current work is never removed.
    pub fn set_history_retention_limits(&mut self, limits: HistoryRetentionLimits) {
        self.history_limits = limits;
        self.enforce_history_retention();
    }

    /// Registers resource sizes after successful admission, including pending imports.
    /// Hosts owning embedded bytes must call this before pruning their asset store.
    pub fn retain_history_resources(&mut self, sizes: BTreeMap<AssetHash, u64>) {
        self.asset_sizes = sizes;
        self.enforce_history_retention();
        let retained = self.retained_asset_hashes();
        self.asset_sizes.retain(|hash, _| retained.contains(hash));
    }

    /// Resources reachable by the current state or any remaining history state.
    #[must_use]
    pub fn retained_asset_hashes(&self) -> BTreeSet<AssetHash> {
        let mut assets = self.document.assets.clone();
        for entry in self.undo.iter().chain(&self.redo) {
            assets.extend(entry.before.assets.iter().cloned());
            assets.extend(entry.after.assets.iter().cloned());
        }
        assets
    }

    /// Unique shared snapshots and known history-only asset bytes, excluding current.
    /// Asset charges require the host to register sizes with `retain_history_resources`.
    #[must_use]
    pub fn history_retention_status(&self) -> HistoryRetentionStatus {
        let mut seen = HashSet::from([Arc::as_ptr(&self.document)]);
        let mut snapshot_bytes = 0_u64;
        for entry in self.undo.iter().chain(&self.redo) {
            for snapshot in [&entry.before, &entry.after] {
                if seen.insert(Arc::as_ptr(snapshot)) {
                    snapshot_bytes = snapshot_bytes.saturating_add(snapshot.serialized_bytes);
                }
            }
        }
        let history_asset_bytes = self
            .retained_asset_hashes()
            .difference(&self.document.assets)
            .map(|hash| self.asset_sizes.get(hash).copied().unwrap_or(0))
            .fold(0_u64, u64::saturating_add);
        HistoryRetentionStatus {
            undo_transactions: self.undo.len(),
            redo_transactions: self.redo.len(),
            older_history_removed: self.older_history_removed,
            snapshot_bytes,
            history_asset_bytes,
            limits: self.history_limits,
        }
    }

    fn enforce_history_retention(&mut self) {
        loop {
            let status = self.history_retention_status();
            if status.undo_transactions + status.redo_transactions
                <= self.history_limits.max_transactions
                && status.snapshot_bytes <= self.history_limits.max_snapshot_bytes
                && status.history_asset_bytes <= self.history_limits.max_history_asset_bytes
            {
                break;
            }
            // Oldest past first; then farthest future. Both preserve a contiguous
            // sequence adjacent to current, including after Undo changes exemptions.
            if !self.undo.is_empty() {
                self.undo.remove(0);
            } else if !self.redo.is_empty() {
                self.redo.remove(0);
            } else {
                break;
            }
            self.older_history_removed = true;
        }
    }

    /// Applies every command in a transaction as one undoable operation.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] if the expected revision is stale, the
    /// transaction is empty, a command is invalid, or the next revision cannot
    /// be represented.
    pub fn execute(
        &mut self,
        expected_revision: Revision,
        transaction: Transaction,
    ) -> Result<CommandOutcome, CommandError> {
        self.execute_checked(expected_revision, transaction, |document, revision| {
            crate::package::validate_document_saveability(
                document,
                revision,
                &std::collections::BTreeMap::new(),
            )
            .map_err(|error| CommandError::Saveability(error.to_string()))
        })
    }

    /// Validates prospective document resources before publishing history.
    ///
    /// # Errors
    /// Returns command or admission errors without changing the session.
    pub fn execute_checked(
        &mut self,
        expected_revision: Revision,
        transaction: Transaction,
        admit: impl FnOnce(&Document, Revision) -> Result<(), CommandError>,
    ) -> Result<CommandOutcome, CommandError> {
        self.require_revision(expected_revision)?;
        if transaction.commands.is_empty() {
            return Err(CommandError::EmptyTransaction);
        }

        let before = Arc::clone(&self.document);
        let mut after = before.document.clone();
        let mut created_pages = Vec::new();
        let mut created_objects = Vec::new();
        let mut created_masters = Vec::new();
        for command in transaction.commands {
            apply_command(
                &mut after,
                command,
                &mut created_pages,
                &mut created_objects,
                &mut created_masters,
            )?;
        }
        after.validate()?;
        let next_revision = self
            .revision
            .checked_next()
            .ok_or(CommandError::RevisionExhausted)?;

        admit(&after, next_revision)?;
        let after = Arc::new(HistorySnapshot::new(after)?);
        self.document = Arc::clone(&after);
        self.revision = next_revision;
        self.undo.push(HistoryEntry { before, after });
        self.redo.clear();
        self.enforce_history_retention();
        Ok(CommandOutcome {
            revision: self.revision,
            created_pages,
            created_objects,
            created_masters,
        })
    }

    /// Restores the document state before the most recent transaction.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] if the expected revision is stale, there is no
    /// transaction to undo, or the next revision cannot be represented.
    pub fn undo(&mut self, expected_revision: Revision) -> Result<Revision, CommandError> {
        self.undo_checked(expected_revision, |_, _| Ok(()))
    }

    /// Checks restored content against current resource budgets before changing history.
    ///
    /// # Errors
    /// Returns revision, history, or admission errors atomically.
    pub fn undo_checked(
        &mut self,
        expected_revision: Revision,
        admit: impl FnOnce(&Document, Revision) -> Result<(), CommandError>,
    ) -> Result<Revision, CommandError> {
        self.require_revision(expected_revision)?;
        let next_revision = self
            .revision
            .checked_next()
            .ok_or(CommandError::RevisionExhausted)?;
        let candidate = self.undo.last().ok_or(CommandError::NothingToUndo)?;
        admit(&candidate.before, next_revision)?;
        let entry = self.undo.pop().ok_or(CommandError::NothingToUndo)?;
        self.document = Arc::clone(&entry.before);
        self.redo.push(entry);
        self.revision = next_revision;
        self.enforce_history_retention();
        Ok(self.revision)
    }

    /// Reapplies the most recently undone transaction.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] if the expected revision is stale, there is no
    /// transaction to redo, or the next revision cannot be represented.
    pub fn redo(&mut self, expected_revision: Revision) -> Result<Revision, CommandError> {
        self.redo_checked(expected_revision, |_, _| Ok(()))
    }

    /// Checks restored content against current resource budgets before changing history.
    ///
    /// # Errors
    /// Returns revision, history, or admission errors atomically.
    pub fn redo_checked(
        &mut self,
        expected_revision: Revision,
        admit: impl FnOnce(&Document, Revision) -> Result<(), CommandError>,
    ) -> Result<Revision, CommandError> {
        self.require_revision(expected_revision)?;
        let next_revision = self
            .revision
            .checked_next()
            .ok_or(CommandError::RevisionExhausted)?;
        let candidate = self.redo.last().ok_or(CommandError::NothingToRedo)?;
        admit(&candidate.after, next_revision)?;
        let entry = self.redo.pop().ok_or(CommandError::NothingToRedo)?;
        self.document = Arc::clone(&entry.after);
        self.undo.push(entry);
        self.revision = next_revision;
        self.enforce_history_retention();
        Ok(self.revision)
    }

    fn require_revision(&self, expected: Revision) -> Result<(), CommandError> {
        if expected == self.revision {
            Ok(())
        } else {
            Err(CommandError::RevisionConflict {
                expected,
                actual: self.revision,
            })
        }
    }
}

#[allow(clippy::too_many_lines)] // Exhaustive command dispatch stays centralized and auditable.
fn apply_command(
    document: &mut Document,
    command: Command,
    created_pages: &mut Vec<PageId>,
    created_objects: &mut Vec<ObjectId>,
    created_masters: &mut Vec<crate::model::MasterId>,
) -> Result<(), CommandError> {
    ensure_command_targets_unlocked(document, &command)?;
    match command {
        Command::AddMaster { name, trim } => master_pages::add_master(document, name, trim, created_masters)?,
        Command::RenameMaster { master_id, name } => master_pages::rename_master(document, &master_id, name)?,
        Command::DuplicateMaster { master_id, name } => master_pages::duplicate_master(document, &master_id, name, created_masters, created_objects)?,
        Command::ResizeMaster { master_id, trim } => master_pages::resize_master(document, &master_id, trim)?,
        Command::AssignMaster { page_ids, master_id } => master_pages::assign_master(document, &page_ids, master_id.as_ref())?,
        Command::RemoveMaster { master_id, replacement } => master_pages::remove_master(document, &master_id, replacement.as_ref())?,
        Command::UpdatePageNumbering { page_id, section } => {
            let index = page_index(document, &page_id)?;
            document.pages[index].numbering_section = section;
        }
        Command::SetPageNumberSuppression { page_ids, suppressed } => {
            master_pages::suppress_numbers(document, &page_ids, suppressed)?;
        }
        Command::SetTextField { object_id, field } => { object_mut(document, &object_id)?.text_field = field; }
        Command::AddPage { after, trim, bleed } => {
            add_page(document, after.as_ref(), trim, bleed, created_pages)?;
        }
        Command::DuplicatePage { page_id } => {
            duplicate_page(document, &page_id, created_pages, created_objects)?;
        }
        Command::RenamePage { page_id, name } => {
            let index = page_index(document, &page_id)?;
            document.pages[index].name = name;
        }
        Command::DeletePage { page_id } => {
            let index = page_index(document, &page_id)?;
            master_pages::transfer_deleted_section(document, index);
            if document.pages.len() == 1 {
                document.pages[index].objects.clear();
                document.pages[index].guides.clear();
                document.pages[index].master_id = None;
                document.pages[index].numbering_section = None;
                document.pages[index].suppress_page_number = false;
            } else {
                document.pages.remove(index);
            }
        }
        Command::ReorderPage { page_id, new_index } => {
            if new_index >= document.pages.len() {
                return Err(CommandError::PageIndexOutOfRange(new_index));
            }
            let old_index = page_index(document, &page_id)?;
            if old_index != new_index {
                let page = document.pages.remove(old_index);
                document.pages.insert(new_index, page);
            }
        }
        Command::UpdatePageBleed { page_id, bleed } => {
            update_page_bleed(document, &page_id, bleed)?;
        }
        Command::UpdatePageTrim { page_id, trim } => {
            let index = page_index(document, &page_id)?;
            validate_guides_for_trim(&document.pages[index].guides, trim)?;
            document.pages[index].trim = trim;
        }
        Command::SetExactFontFallbacks { fallbacks } => {
            document.exact_font_fallbacks = fallbacks;
        }
        Command::AddGuide {
            page_id,
            axis,
            position,
        } => {
            let index = page_index(document, &page_id)?;
            validate_guide_position(document.pages[index].trim, axis, position)?;
            let id = document.allocate_guide_id();
            document.pages[index]
                .guides
                .push(PageGuide { id, axis, position });
        }
        Command::MoveGuide {
            page_id,
            guide_id,
            position,
        } => {
            let index = page_index(document, &page_id)?;
            let axis = document.pages[index]
                .guides
                .iter()
                .find(|guide| guide.id == guide_id)
                .ok_or_else(|| CommandError::GuideNotFound(guide_id.clone()))?
                .axis;
            validate_guide_position(document.pages[index].trim, axis, position)?;
            document.pages[index]
                .guides
                .iter_mut()
                .find(|guide| guide.id == guide_id)
                .expect("guide existence checked")
                .position = position;
        }
        Command::DeleteGuide { page_id, guide_id } => {
            let index = page_index(document, &page_id)?;
            let guide_index = document.pages[index]
                .guides
                .iter()
                .position(|guide| guide.id == guide_id)
                .ok_or(CommandError::GuideNotFound(guide_id))?;
            document.pages[index].guides.remove(guide_index);
        }
        Command::AddObject { location, object } => {
            object.validate()?;
            let object = document.object_from_draft(object);
            created_objects.push(object.id.clone());
            objects_mut(document, &location)?.push(object);
        }
        Command::DuplicateObject {
            object_id,
            offset_x_um,
            offset_y_um,
        } => duplicate_object(
            document,
            &object_id,
            offset_x_um,
            offset_y_um,
            created_objects,
        )?,
        Command::PasteObjects {
            objects,
            destination,
            offset_x_um,
            offset_y_um,
        } => paste_objects(
            document,
            objects,
            &destination,
            offset_x_um,
            offset_y_um,
            created_objects,
        )?,
        Command::GroupObjects { object_ids } => group_objects(document, &object_ids)?,
        Command::UngroupObjects { object_ids } => ungroup_objects(document, &object_ids)?,
        Command::SetObjectLock { object_ids, locked } => {
            set_object_lock(document, &object_ids, locked)?;
        }
        Command::MoveObject {
            object_id,
            destination,
            transform,
        } => {
            move_object(document, &object_id, &destination, transform)?;
        }
        Command::SetObjectPosition {
            object_id,
            destination,
            x,
            y,
        } => {
            let mut transform = object_with_location(document, &object_id)?.1.transform;
            transform.translation_x = x;
            transform.translation_y = y;
            move_object(document, &object_id, &destination, transform)?;
        }
        Command::ReorderObject {
            object_id,
            new_index,
        } => reorder_object(document, &object_id, new_index)?,
        Command::ArrangeObject {
            object_id,
            operation,
        } => arrange_object(document, &object_id, operation)?,
        Command::ArrangeSelection {
            object_ids,
            operation,
        } => arrange_selection(document, &object_ids, operation)?,
        Command::ArrangeGeometry {
            object_ids,
            operation,
        } => arrange_geometry(document, &object_ids, operation)?,
        Command::ArrangeGeometryToPage {
            object_ids,
            operation,
        } => {
            arrange_geometry_to_page(document, &object_ids, operation)?;
        }
        Command::MoveSelection {
            object_ids,
            destination,
            delta_x_um,
            delta_y_um,
        } => {
            for id in selected_object_ids(document, &object_ids)? {
                let (source, object) = object_with_location(document, &id)?;
                let mut transform = object.transform;
                transform.translation_x = Micrometres::new(
                    transform
                        .translation_x
                        .value()
                        .checked_add(delta_x_um)
                        .ok_or(CommandError::ArrangeGeometryOverflow)?,
                );
                transform.translation_y = Micrometres::new(
                    transform
                        .translation_y
                        .value()
                        .checked_add(delta_y_um)
                        .ok_or(CommandError::ArrangeGeometryOverflow)?,
                );
                move_object(document, &id, destination.as_ref().unwrap_or(&source), transform)?;
            }
        }
        Command::DeleteSelection { object_ids } => {
            for id in selected_object_ids(document, &object_ids)? {
                remove_object(document, &id)?;
            }
        }
        Command::ApplySelectionTransforms { objects } => {
            let ids = objects
                .iter()
                .map(|object| object.object_id.clone())
                .collect::<Vec<_>>();
            let normalized = selected_object_ids(document, &ids)?;
            if normalized.len() != objects.len() {
                return Err(CommandError::InvalidArrangeGeometrySelection);
            }
            for update in objects {
                object_mut(document, &update.object_id)?.transform = update.transform;
            }
        }
        Command::ApplyMemberTransforms { objects } => {
            let ids = objects.iter().map(|object| object.object_id.clone()).collect::<Vec<_>>();
            selected_member_ids(document, &ids)?;
            for update in objects {
                object_mut(document, &update.object_id)?.transform = update.transform;
            }
        }
        Command::UpdateCornerRounding {
            object_id,
            rounding,
        } => {
            object_mut(document, &object_id)?.corner_rounding = rounding;
        }
        Command::ResizeObject { object_id, size } => {
            let object = object_mut(document, &object_id)?;
            if let ObjectKind::Text { frame, .. } = &object.kind {
                frame.validate_usable_for_size(size)?;
            }
            object.size = size;
        }
        Command::UpdateText { object_id, text } => update_text(document, object_id, text)?,
        Command::ReplaceTextRange {
            object_id,
            range,
            text,
        } => replace_text_range(document, object_id, range, &text)?,
        Command::ApplyTextEdits { object_id, edits } => {
            apply_text_edits(document, object_id, edits)?;
        }
        Command::UpdateTextStyle {
            object_id,
            style,
            font_face_update,
        } => {
            update_text_style(document, object_id, style, font_face_update)?;
        }
        Command::SetTextFontVariantBinding {
            object_id,
            font_weight,
            italic,
            font_face,
        } => set_text_font_variant_binding(document, object_id, font_weight, italic, font_face)?,
        Command::SetTextFontFamilyBinding {
            object_id,
            font_family,
            font_weight,
            italic,
            font_face,
        } => {
            let object = object_mut(document, &object_id)?;
            let style = match &mut object.kind {
                ObjectKind::Text {style,..} => style,
                ObjectKind::Table {table} => &mut table.style,
                _ => return Err(CommandError::ObjectKindMismatch(object_id)),
            };
            set_font_family_binding(style, font_family, font_weight, italic, font_face)?;
        }
        Command::SetInlineFontSize {
            object_id,
            range,
            font_size_millipoints,
        } => {
            update_inline_style(
                document,
                object_id,
                range,
                &InlineTextStyle {
                    font_size_millipoints: Some(font_size_millipoints),
                    ..InlineTextStyle::default()
                },
            )?;
        }
        Command::UpdateInlineTextStyle {
            object_id,
            range,
            bold,
            italic,
        } => update_inline_text_style(document, object_id, range, bold, italic)?,
        Command::UpdateInlineTextAppearance {
            object_id,
            range,
            style,
        } => update_inline_style(document, object_id, range, &style)?,
        Command::UpdateParagraphStyle {
            object_id,
            range,
            style,
        } => update_paragraph_style(document, object_id, range, style)?,
        Command::UpdateTextFrame {
            object_id,
            column_count,
            column_gap_um,
        } => update_text_frame(document, object_id, column_count, column_gap_um)?,
        Command::UpdateTextBoxInterior {
            object_id,
            margins,
            vertical_alignment,
        } => {
            update_text_box_interior(document, object_id, margins, vertical_alignment)?;
        }
        Command::UpdateTextBoxSizing {
            object_id,
            sizing_mode,
            size,
        } => update_text_box_sizing(document, object_id, sizing_mode, size)?,
        Command::UpdateWrapMode {
            object_id,
            wrap_mode,
        } => update_wrap_mode(document, object_id, wrap_mode)?,
        Command::UpdateWrapSettings { object_id, settings } => {
            let object = object_mut(document, &object_id)?;
            if !matches!(object.kind, ObjectKind::Image { .. } | ObjectKind::Shape { .. }) {
                return Err(CommandError::ObjectKindMismatch(object_id));
            }
            settings.validate()?;
            object.wrap_settings = settings;
        }
        Command::UpdateImageContentKind {
            object_id,
            content_kind,
        } => update_image_content_kind(document, object_id, content_kind)?,
        Command::UpdateImageCrop { object_id, crop } => {
            update_image_crop(document, object_id, crop)?;
        }
        Command::SetImageFlip { object_id, flip } => {
            let object = object_mut(document, &object_id)?;
            let ObjectKind::Image { flip: current, .. } = &mut object.kind else {
                return Err(CommandError::ObjectKindMismatch(object_id));
            };
            *current = flip;
        }
        Command::ResetImageEditing { object_id } => {
            let object = object_mut(document, &object_id)?;
            let ObjectKind::Image { crop, flip, .. } = &mut object.kind else {
                return Err(CommandError::ObjectKindMismatch(object_id));
            };
            *crop = ImageCrop::default();
            *flip = ImageFlip::default();
        }
        Command::ReplaceImage {
            object_id,
            asset,
            name,
        } => {
            let object = object_mut(document, &object_id)?;
            let ObjectKind::Image { asset: current, .. } = &mut object.kind else {
                return Err(CommandError::ObjectKindMismatch(object_id));
            };
            *current = asset;
            object.name = name;
        }
        Command::SetImageAspectLock { object_id, locked } => {
            set_image_aspect_lock(document, object_id, locked)?;
        }
        Command::UpdateUntaggedRasterPolicy { policy } => {
            document.color_settings.untagged_raster_policy = policy;
        }
        Command::UpdateTableCell {
            object_id,
            row,
            column,
            text,
        } => {
            update_table_cell(document, object_id, row, column, text)?;
        }
        Command::UpdateTableDimensions {
            object_id,
            rows,
            columns,
        } => update_table_dimensions(document, object_id, rows, columns)?,
        Command::UpdateTableCells {
            object_id,
            row,
            column,
            cells,
        } => {
            let object = object_mut(document, &object_id)?;
            let ObjectKind::Table { table } = &mut object.kind else {
                return Err(CommandError::ObjectKindMismatch(object_id));
            };
            let width = cells.first().map_or(0, Vec::len);
            if cells.is_empty() || width == 0 || cells.iter().any(|values| values.len() != width) {
                return Err(ModelError::InvalidTableDimensions.into());
            }
            if usize::from(row) + cells.len() > usize::from(table.rows)
                || usize::from(column) + width > usize::from(table.columns)
            {
                return Err(ModelError::TableCellOutOfRange.into());
            }
            table.prepare_cell_paste(crate::model::TableCellRange {
                start_row: row,
                end_row: row + u16::try_from(cells.len()).map_err(|_| ModelError::TableCellOutOfRange)?,
                start_column: column,
                end_column: column + u16::try_from(width).map_err(|_| ModelError::TableCellOutOfRange)?,
            })?;
            for (row_offset, values) in cells.into_iter().enumerate() {
                for (column_offset, text) in values.into_iter().enumerate() {
                    table.set_cell(
                        row + u16::try_from(row_offset)
                            .map_err(|_| ModelError::TableCellOutOfRange)?,
                        column
                            + u16::try_from(column_offset)
                                .map_err(|_| ModelError::TableCellOutOfRange)?,
                        text,
                    )?;
                }
            }
        }
        Command::InsertTableTrack {
            object_id,
            axis,
            index,
        } => insert_table_track(document, object_id, axis, index)?,
        Command::DeleteTableTrack {
            object_id,
            axis,
            index,
        } => delete_table_track(document, object_id, axis, index)?,
        Command::UpdateTableTextStyle {
            object_id,
            style,
            font_face_update,
        } => update_table_text_style(document, object_id, style, font_face_update)?,
        Command::UpdateTableGridBorder { object_id, border } => {
            update_table_grid_border(document, object_id, border)?;
        }
        Command::UpdateTableCellAlignment {
            object_id,
            row,
            column,
            horizontal_alignment,
            vertical_alignment,
        } => update_table_cell_alignment(
            document,
            object_id,
            row,
            column,
            horizontal_alignment,
            vertical_alignment,
        )?,
        Command::UpdateTableTrackSize {
            object_id,
            axis,
            index,
            weight,
        } => update_table_track_size(document, object_id, axis, index, weight)?,
        Command::UpdateTableCellFormatting {object_id,range,formatting} => {
            require_table_mut(document, &object_id)?.update_cell_formatting(range, &formatting)?;
        }
        Command::ResetTableCellFormatting {object_id,range} => {
            require_table_mut(document, &object_id)?.reset_cell_formatting(range)?;
        }
        Command::MergeTableCells {object_id,range} => {
            require_table_mut(document, &object_id)?.merge_cells(range)?;
        }
        Command::SplitTableCells {object_id,range} => {
            require_table_mut(document, &object_id)?.split_cells(range)?;
        }
        Command::DistributeTableTracks { object_id, axis } => {
            distribute_table_tracks(document, object_id, axis)?;
        }
        Command::SetOpacity {
            object_id,
            opacity_millionths,
        } => set_opacity(document, &object_id, opacity_millionths)?,
        Command::SetRotation {
            object_id,
            rotation_microdegrees,
        } => set_rotation(document, &object_id, rotation_microdegrees)?,
        Command::UpdateShape {
            object_id,
            shape,
            fill,
            stroke,
        } => update_shape(document, object_id, shape, fill, stroke)?,
        Command::UpdateObjectEffects { object_id, effects } => {
            update_object_effects(document, &object_id, effects)?;
        }
        Command::DeleteObject { object_id } => {
            let removed = remove_object(document, &object_id)?;
            clear_singleton_group(document, removed.group_id.as_ref());
        }
    }
    Ok(())
}

fn add_page(
    document: &mut Document,
    after: Option<&PageId>,
    trim: PhysicalSize,
    bleed: Insets,
    created_pages: &mut Vec<PageId>,
) -> Result<(), CommandError> {
    let insertion = after
        .map(|id| page_index(document, id).map(|index| index + 1))
        .transpose()?
        .unwrap_or(document.pages.len());
    require_page_capacity(document)?;
    let id = document.allocate_page_id();
    document.pages.insert(
        insertion,
        Page {
            id: id.clone(),
            name: String::new(),
            trim,
            bleed,
            bleed_expectations: EdgeExpectations::default(),
            guides: Vec::new(),
            objects: Vec::new(),
            master_id: None,
            numbering_section: None,
            suppress_page_number: false,
        },
    );
    created_pages.push(id);
    Ok(())
}

fn duplicate_page(
    document: &mut Document,
    source_page_id: &PageId,
    created_pages: &mut Vec<PageId>,
    created_objects: &mut Vec<ObjectId>,
) -> Result<(), CommandError> {
    let index = page_index(document, source_page_id)?;
    require_page_capacity(document)?;
    let source = document.pages[index].clone();
    let page_id = document.allocate_page_id();
    let guides = source
        .guides
        .iter()
        .map(|guide| PageGuide {
            id: document.allocate_guide_id(),
            axis: guide.axis,
            position: guide.position,
        })
        .collect();
    let mut objects = Vec::with_capacity(source.objects.len());
    let mut groups = HashMap::<GroupId, GroupId>::new();
    for mut object in source.objects {
        let id = document.allocate_object_id();
        if let Some(source_group) = object.group_id.clone() {
            let group_id = if let Some(existing) = groups.get(&source_group) {
                existing.clone()
            } else {
                let allocated = document.allocate_group_id();
                groups.insert(source_group, allocated.clone());
                allocated
            };
            object.group_id = Some(group_id);
        }
        created_objects.push(id.clone());
        objects.push(PublicationObject { id, ..object });
    }
    document.pages.insert(
        index + 1,
        Page {
            id: page_id.clone(),
            guides,
            objects,
            numbering_section: None,
            ..source
        },
    );
    created_pages.push(page_id);
    Ok(())
}

fn require_page_capacity(document: &Document) -> Result<(), CommandError> {
    if document.pages.len() >= usize::from(Document::MAX_INITIAL_PAGES) {
        return Err(CommandError::PageLimitReached {
            maximum: Document::MAX_INITIAL_PAGES,
        });
    }
    Ok(())
}

fn duplicate_object(
    document: &mut Document,
    object_id: &ObjectId,
    horizontal_offset_um: i64,
    vertical_offset_um: i64,
    created_objects: &mut Vec<ObjectId>,
) -> Result<(), CommandError> {
    let located = document.pages.iter().find_map(|page| {
        page.objects
            .iter()
            .position(|object| &object.id == object_id)
            .map(|index| {
                (
                    ObjectLocation::Page(page.id.clone()),
                    index,
                    page.objects[index].clone(),
                )
            })
    });
    let (location, index, mut duplicate) = located
        .or_else(|| document.masters.iter().find_map(|master| {
            master.objects.iter().position(|object| &object.id == object_id)
                .map(|index| (ObjectLocation::Master(master.id.clone()), index, master.objects[index].clone()))
        }))
        .or_else(|| {
            document
                .pasteboard
                .objects
                .iter()
                .position(|object| &object.id == object_id)
                .map(|index| {
                    (
                        ObjectLocation::Pasteboard,
                        index,
                        document.pasteboard.objects[index].clone(),
                    )
                })
        })
        .ok_or_else(|| CommandError::ObjectNotFound(object_id.clone()))?;
    duplicate.transform.translation_x = crate::model::Micrometres::new(
        duplicate
            .transform
            .translation_x
            .value()
            .checked_add(horizontal_offset_um)
            .ok_or(ModelError::CoordinateOverflow)?,
    );
    duplicate.transform.translation_y = crate::model::Micrometres::new(
        duplicate
            .transform
            .translation_y
            .value()
            .checked_add(vertical_offset_um)
            .ok_or(ModelError::CoordinateOverflow)?,
    );
    let id = document.allocate_object_id();
    duplicate.id = id.clone();
    duplicate.group_id = None;
    objects_mut(document, &location)?.insert(index + 1, duplicate);
    created_objects.push(id);
    Ok(())
}

fn paste_objects(
    document: &mut Document,
    mut copies: Vec<PublicationObject>,
    destination: &ObjectLocation,
    horizontal_offset_um: i64,
    vertical_offset_um: i64,
    created_objects: &mut Vec<ObjectId>,
) -> Result<(), CommandError> {
    let selected_ids = copies
        .iter()
        .map(|object| object.id.clone())
        .collect::<HashSet<_>>();
    if copies.is_empty() || copies.len() > 500 || selected_ids.len() != copies.len() {
        return Err(CommandError::InvalidPasteSelection);
    }
    let mut group_counts = HashMap::<GroupId, usize>::new();
    for group_id in copies.iter().filter_map(|object| object.group_id.clone()) {
        *group_counts.entry(group_id).or_default() += 1;
    }
    if group_counts.values().any(|count| *count < 2) {
        return Err(CommandError::PartialGroupPaste);
    }
    let existing_group_counts = document.authored_objects()
        .filter_map(|object| object.group_id.clone())
        .fold(HashMap::<GroupId, usize>::new(), |mut counts, group_id| {
            *counts.entry(group_id).or_default() += 1;
            counts
        });
    if group_counts.iter().any(|(group_id, pasted_count)| {
        existing_group_counts
            .get(group_id)
            .is_some_and(|existing_count| pasted_count != existing_count)
    }) {
        return Err(CommandError::PartialGroupPaste);
    }
    if let ObjectLocation::Page(page_id) = destination {
        page_index(document, page_id)?;
    }
    if let ObjectLocation::Master(master_id) = destination { master_pages::master_index(document, master_id)?; }

    for copy in &mut copies {
        copy.transform.translation_x = Micrometres::new(
            copy.transform
                .translation_x
                .value()
                .checked_add(horizontal_offset_um)
                .ok_or(ModelError::CoordinateOverflow)?,
        );
        copy.transform.translation_y = Micrometres::new(
            copy.transform
                .translation_y
                .value()
                .checked_add(vertical_offset_um)
                .ok_or(ModelError::CoordinateOverflow)?,
        );
    }

    let mut group_ids = HashMap::<GroupId, GroupId>::new();
    for copy in &mut copies {
        let id = document.allocate_object_id();
        copy.id = id.clone();
        copy.locked = false;
        if let Some(source_group) = copy.group_id.clone() {
            let group_id = if let Some(existing) = group_ids.get(&source_group) {
                existing.clone()
            } else {
                let allocated = document.allocate_group_id();
                group_ids.insert(source_group, allocated.clone());
                allocated
            };
            copy.group_id = Some(group_id);
        }
        created_objects.push(id);
    }
    objects_mut(document, destination)?.extend(copies);
    Ok(())
}

fn update_page_bleed(
    document: &mut Document,
    page_id: &PageId,
    bleed: Insets,
) -> Result<(), CommandError> {
    let index = page_index(document, page_id)?;
    document.pages[index].bleed = bleed;
    Ok(())
}

fn move_object(
    document: &mut Document,
    object_id: &ObjectId,
    destination: &ObjectLocation,
    transform: Transform,
) -> Result<(), CommandError> {
    if !update_transform_if_already_there(document, object_id, destination, transform)? {
        let mut object = remove_object(document, object_id)?;
        object.transform = transform;
        objects_mut(document, destination)?.push(object);
    }
    Ok(())
}

fn update_transform_if_already_there(
    document: &mut Document,
    object_id: &ObjectId,
    location: &ObjectLocation,
    transform: Transform,
) -> Result<bool, CommandError> {
    let objects = objects_mut(document, location)?;
    if let Some(object) = objects.iter_mut().find(|object| &object.id == object_id) {
        object.transform = transform;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn page_index(document: &Document, page_id: &PageId) -> Result<usize, CommandError> {
    document
        .pages
        .iter()
        .position(|page| &page.id == page_id)
        .ok_or_else(|| CommandError::PageNotFound(page_id.clone()))
}

fn objects_mut<'a>(
    document: &'a mut Document,
    location: &ObjectLocation,
) -> Result<&'a mut Vec<PublicationObject>, CommandError> {
    match location {
        ObjectLocation::Page(page_id) => {
            let index = page_index(document, page_id)?;
            Ok(&mut document.pages[index].objects)
        }
        ObjectLocation::Pasteboard => Ok(&mut document.pasteboard.objects),
        ObjectLocation::Master(master_id) => {
            let index = master_pages::master_index(document, master_id)?;
            Ok(&mut document.masters[index].objects)
        }
    }
}

fn remove_object(
    document: &mut Document,
    object_id: &ObjectId,
) -> Result<PublicationObject, CommandError> {
    for page in &mut document.pages {
        if let Some(index) = page
            .objects
            .iter()
            .position(|object| &object.id == object_id)
        {
            return Ok(page.objects.remove(index));
        }
    }
    for master in &mut document.masters {
        if let Some(index) = master.objects.iter().position(|object| &object.id == object_id) {
            return Ok(master.objects.remove(index));
        }
    }
    if let Some(index) = document
        .pasteboard
        .objects
        .iter()
        .position(|object| &object.id == object_id)
    {
        return Ok(document.pasteboard.objects.remove(index));
    }
    Err(CommandError::ObjectNotFound(object_id.clone()))
}

fn group_objects(document: &mut Document, object_ids: &[ObjectId]) -> Result<(), CommandError> {
    let unique = object_ids.iter().cloned().collect::<HashSet<_>>();
    if unique.len() < 2 || unique.len() != object_ids.len() {
        return Err(CommandError::InvalidGroupSelection);
    }
    let mut location: Option<ObjectLocation> = None;
    for object_id in object_ids {
        let (candidate_location, object) = object_with_location(document, object_id)?;
        if object.group_id.is_some() {
            return Err(CommandError::ObjectAlreadyGrouped(object_id.clone()));
        }
        if let Some(expected) = &location {
            if expected != &candidate_location {
                return Err(CommandError::GroupObjectsInDifferentContainers);
            }
        } else {
            location = Some(candidate_location);
        }
    }
    let location = location.expect("a valid group has a container");
    let group_id = document.allocate_group_id();
    let objects = objects_mut(document, &location)?;
    let selected = objects
        .iter()
        .enumerate()
        .filter(|(_, object)| unique.contains(&object.id))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let highest = *selected.last().expect("a valid group has members");
    let insertion = highest + 1 - selected.len();
    let mut members = Vec::with_capacity(selected.len());
    for index in selected.into_iter().rev() {
        members.push(objects.remove(index));
    }
    members.reverse();
    for member in &mut members {
        member.group_id = Some(group_id.clone());
    }
    objects.splice(insertion..insertion, members);
    Ok(())
}

fn set_object_lock(
    document: &mut Document,
    object_ids: &[ObjectId],
    locked: bool,
) -> Result<(), CommandError> {
    let unique = object_ids.iter().cloned().collect::<HashSet<_>>();
    if unique.is_empty() || unique.len() != object_ids.len() {
        return Err(CommandError::InvalidLockSelection);
    }
    for object_id in object_ids {
        object_with_location(document, object_id)?;
    }
    for object_id in object_ids {
        object_mut(document, object_id)?.locked = locked;
    }
    Ok(())
}

fn ungroup_objects(document: &mut Document, object_ids: &[ObjectId]) -> Result<(), CommandError> {
    let unique = object_ids.iter().collect::<HashSet<_>>();
    if unique.is_empty() || unique.len() != object_ids.len() {
        return Err(CommandError::InvalidGroupSelection);
    }
    let mut groups = HashSet::new();
    for object_id in object_ids {
        let (_, object) = object_with_location(document, object_id)?;
        let group_id = object
            .group_id
            .clone()
            .ok_or_else(|| CommandError::ObjectNotGrouped(object_id.clone()))?;
        groups.insert(group_id);
    }
    for object in document.authored_objects_mut()
    {
        if object
            .group_id
            .as_ref()
            .is_some_and(|group_id| groups.contains(group_id))
        {
            object.group_id = None;
        }
    }
    Ok(())
}

fn object_with_location<'a>(
    document: &'a Document,
    object_id: &ObjectId,
) -> Result<(ObjectLocation, &'a PublicationObject), CommandError> {
    for page in &document.pages {
        if let Some(object) = page.objects.iter().find(|object| &object.id == object_id) {
            return Ok((ObjectLocation::Page(page.id.clone()), object));
        }
    }
    for master in &document.masters {
        if let Some(object) = master.objects.iter().find(|object| &object.id == object_id) {
            return Ok((ObjectLocation::Master(master.id.clone()), object));
        }
    }
    document
        .pasteboard
        .objects
        .iter()
        .find(|object| &object.id == object_id)
        .map(|object| (ObjectLocation::Pasteboard, object))
        .ok_or_else(|| CommandError::ObjectNotFound(object_id.clone()))
}

fn clear_singleton_group(document: &mut Document, removed_group: Option<&GroupId>) {
    let Some(group_id) = removed_group else {
        return;
    };
    let mut remaining = document.authored_objects_mut()
        .filter(|object| object.group_id.as_ref() == Some(group_id));
    if let Some(only) = remaining.next()
        && remaining.next().is_none()
    {
        only.group_id = None;
    }
}

fn reorder_object(
    document: &mut Document,
    object_id: &ObjectId,
    new_index: usize,
) -> Result<(), CommandError> {
    if object_with_location(document, object_id)?
        .1
        .group_id
        .is_some()
    {
        return Err(CommandError::GroupedObjectRequiresArrange(
            object_id.clone(),
        ));
    }
    for page in &mut document.pages {
        if let Some(old_index) = page
            .objects
            .iter()
            .position(|object| &object.id == object_id)
        {
            return reorder_in(&mut page.objects, old_index, new_index);
        }
    }
    for master in &mut document.masters {
        if let Some(old_index) = master.objects.iter().position(|object| &object.id == object_id) {
            return reorder_in(&mut master.objects, old_index, new_index);
        }
    }
    if let Some(old_index) = document
        .pasteboard
        .objects
        .iter()
        .position(|object| &object.id == object_id)
    {
        return reorder_in(&mut document.pasteboard.objects, old_index, new_index);
    }
    Err(CommandError::ObjectNotFound(object_id.clone()))
}

fn arrange_object(
    document: &mut Document,
    object_id: &ObjectId,
    operation: ArrangeOperation,
) -> Result<(), CommandError> {
    for page in &mut document.pages {
        if page.objects.iter().any(|object| &object.id == object_id) {
            return arrange_in(&mut page.objects, object_id, operation);
        }
    }
    for master in &mut document.masters {
        if master.objects.iter().any(|object| &object.id == object_id) {
            return arrange_in(&mut master.objects, object_id, operation);
        }
    }
    if document
        .pasteboard
        .objects
        .iter()
        .any(|object| &object.id == object_id)
    {
        return arrange_in(&mut document.pasteboard.objects, object_id, operation);
    }
    Err(CommandError::ObjectNotFound(object_id.clone()))
}

fn arrange_in(
    objects: &mut Vec<PublicationObject>,
    object_id: &ObjectId,
    operation: ArrangeOperation,
) -> Result<(), CommandError> {
    let selected = objects
        .iter()
        .find(|object| &object.id == object_id)
        .ok_or_else(|| CommandError::ObjectNotFound(object_id.clone()))?;
    let selected_group = selected.group_id.clone();

    let mut units: Vec<Vec<PublicationObject>> = Vec::new();
    let mut group_units = HashMap::<GroupId, usize>::new();
    for object in std::mem::take(objects) {
        if let Some(group_id) = object.group_id.clone() {
            if let Some(index) = group_units.get(&group_id).copied() {
                units[index].push(object);
            } else {
                group_units.insert(group_id, units.len());
                units.push(vec![object]);
            }
        } else {
            units.push(vec![object]);
        }
    }
    let current = units
        .iter()
        .position(|unit| {
            selected_group.as_ref().map_or_else(
                || unit.iter().any(|object| &object.id == object_id),
                |group_id| {
                    unit.first().and_then(|object| object.group_id.as_ref()) == Some(group_id)
                },
            )
        })
        .expect("selected arrange unit remains present");
    let target = match operation {
        ArrangeOperation::BringForward => (current + 1).min(units.len() - 1),
        ArrangeOperation::SendBackward => current.saturating_sub(1),
        ArrangeOperation::BringToFront => units.len() - 1,
        ArrangeOperation::SendToBack => 0,
    };
    if current != target {
        let unit = units.remove(current);
        units.insert(target, unit);
    }
    *objects = units.into_iter().flatten().collect();
    Ok(())
}

fn arrange_selection(
    document: &mut Document,
    object_ids: &[ObjectId],
    operation: ArrangeOperation,
) -> Result<(), CommandError> {
    let (location, selected) = arrange_units(document, object_ids, 1)?;
    let selected_keys = selected
        .into_iter()
        .map(|unit| unit.key)
        .collect::<HashSet<_>>();
    let objects = objects_mut(document, &location)?;
    let key = |object: &PublicationObject| {
        object.group_id.as_ref().map_or_else(
            || ArrangeUnitKey::Object(object.id.clone()),
            |id| ArrangeUnitKey::Group(id.clone()),
        )
    };
    let mut units: Vec<Vec<PublicationObject>> = Vec::new();
    let mut indexes = HashMap::<ArrangeUnitKey, usize>::new();
    for object in std::mem::take(objects) {
        let object_key = key(&object);
        if let Some(&index) = indexes.get(&object_key) {
            units[index].push(object);
        } else {
            indexes.insert(object_key, units.len());
            units.push(vec![object]);
        }
    }
    let selected = |unit: &[PublicationObject]| selected_keys.contains(&key(&unit[0]));
    match operation {
        ArrangeOperation::BringToFront => units.sort_by_key(|unit| selected(unit)),
        ArrangeOperation::SendToBack => units.sort_by_key(|unit| !selected(unit)),
        ArrangeOperation::BringForward => {
            for index in (0..units.len().saturating_sub(1)).rev() {
                if selected(&units[index]) && !selected(&units[index + 1]) {
                    units.swap(index, index + 1);
                }
            }
        }
        ArrangeOperation::SendBackward => {
            for index in 1..units.len() {
                if selected(&units[index]) && !selected(&units[index - 1]) {
                    units.swap(index, index - 1);
                }
            }
        }
    }
    *objects = units.into_iter().flatten().collect();
    Ok(())
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum ArrangeUnitKey {
    Object(ObjectId),
    Group(GroupId),
}

#[derive(Clone, Copy, Debug)]
struct ArrangeBounds {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl ArrangeBounds {
    fn from_object(object: &PublicationObject) -> Result<Self, CommandError> {
        let bounds = crate::geometry::transformed_bounds(object.size, object.transform)
            .map_err(|_| CommandError::ArrangeGeometryOverflow)?;
        Ok(Self {
            left: bounds.x.value(),
            top: bounds.y.value(),
            right: bounds
                .x
                .value()
                .checked_add(bounds.width.value())
                .ok_or(CommandError::ArrangeGeometryOverflow)?,
            bottom: bounds
                .y
                .value()
                .checked_add(bounds.height.value())
                .ok_or(CommandError::ArrangeGeometryOverflow)?,
        })
    }

    fn include(&mut self, object: &PublicationObject) -> Result<(), CommandError> {
        let bounds = Self::from_object(object)?;
        self.left = self.left.min(bounds.left);
        self.top = self.top.min(bounds.top);
        self.right = self.right.max(bounds.right);
        self.bottom = self.bottom.max(bounds.bottom);
        Ok(())
    }

    fn doubled_horizontal_center(self) -> Result<i64, CommandError> {
        self.left
            .checked_add(self.right)
            .ok_or(CommandError::ArrangeGeometryOverflow)
    }

    fn doubled_vertical_center(self) -> Result<i64, CommandError> {
        self.top
            .checked_add(self.bottom)
            .ok_or(CommandError::ArrangeGeometryOverflow)
    }
}

#[derive(Clone, Debug)]
struct ArrangeUnit {
    key: ArrangeUnitKey,
    member_ids: Vec<ObjectId>,
    bounds: ArrangeBounds,
    container_index: usize,
}

fn arrange_geometry(
    document: &mut Document,
    object_ids: &[ObjectId],
    operation: ArrangeGeometryOperation,
) -> Result<(), CommandError> {
    let (location, units) = arrange_units(document, object_ids, operation.minimum_units())?;
    let translations = arrange_translations(&units, operation)?;
    let objects = objects_mut(document, &location)?;
    for (unit, (delta_x, delta_y)) in units.iter().zip(translations) {
        for member_id in &unit.member_ids {
            let object = objects
                .iter_mut()
                .find(|object| &object.id == member_id)
                .expect("validated arrange member remains in its container");
            object.transform.translation_x = Micrometres::new(
                object
                    .transform
                    .translation_x
                    .value()
                    .checked_add(delta_x)
                    .ok_or(CommandError::ArrangeGeometryOverflow)?,
            );
            object.transform.translation_y = Micrometres::new(
                object
                    .transform
                    .translation_y
                    .value()
                    .checked_add(delta_y)
                    .ok_or(CommandError::ArrangeGeometryOverflow)?,
            );
        }
    }
    Ok(())
}

fn arrange_units(
    document: &Document,
    object_ids: &[ObjectId],
    minimum_units: usize,
) -> Result<(ObjectLocation, Vec<ArrangeUnit>), CommandError> {
    if object_ids.len() > 500 {
        return Err(CommandError::InvalidArrangeGeometrySelection);
    }
    let unique_ids = object_ids.iter().collect::<HashSet<_>>();
    if unique_ids.len() != object_ids.len() {
        return Err(CommandError::InvalidArrangeGeometrySelection);
    }
    let mut location = None;
    let mut keys = Vec::with_capacity(object_ids.len());
    let mut unique_keys = HashSet::new();
    for object_id in object_ids {
        let (candidate_location, object) = object_with_location(document, object_id)?;
        if location
            .as_ref()
            .is_some_and(|expected| expected != &candidate_location)
        {
            return Err(CommandError::ArrangeGeometryObjectsInDifferentContainers);
        }
        location.get_or_insert(candidate_location);
        let key = object.group_id.as_ref().map_or_else(
            || ArrangeUnitKey::Object(object.id.clone()),
            |group_id| ArrangeUnitKey::Group(group_id.clone()),
        );
        if !unique_keys.insert(key.clone()) {
            continue;
        }
        keys.push(key);
    }
    if keys.len() < minimum_units {
        return Err(CommandError::InvalidArrangeGeometrySelection);
    }
    let location = location.ok_or(CommandError::InvalidArrangeGeometrySelection)?;
    let objects = match &location {
        ObjectLocation::Page(page_id) => &document.pages[page_index(document, page_id)?].objects,
        ObjectLocation::Pasteboard => &document.pasteboard.objects,
        ObjectLocation::Master(id) => &document.masters[master_pages::master_index(document, id)?].objects,
    };
    let selected_keys = keys.iter().cloned().collect::<HashSet<_>>();
    let mut units: Vec<ArrangeUnit> = Vec::with_capacity(keys.len());
    for (index, object) in objects.iter().enumerate() {
        let key = object.group_id.as_ref().map_or_else(
            || ArrangeUnitKey::Object(object.id.clone()),
            |group_id| ArrangeUnitKey::Group(group_id.clone()),
        );
        if !selected_keys.contains(&key) {
            continue;
        }
        if let Some(unit) = units.iter_mut().find(|unit| unit.key == key) {
            unit.member_ids.push(object.id.clone());
            unit.bounds.include(object)?;
        } else {
            units.push(ArrangeUnit {
                key,
                member_ids: vec![object.id.clone()],
                bounds: ArrangeBounds::from_object(object)?,
                container_index: index,
            });
        }
    }
    if units.len() != keys.len() {
        return Err(CommandError::InvalidArrangeGeometrySelection);
    }
    if units.iter().map(|unit| unit.member_ids.len()).sum::<usize>() > 500 {
        return Err(CommandError::InvalidArrangeGeometrySelection);
    }
    Ok((location, units))
}

/// Expands flat groups as units, in canonical stacking order, within one container.
///
/// # Errors
/// Rejects missing or duplicate identifiers, mixed containers, or over 500 members.
pub fn selected_object_ids(
    document: &Document,
    object_ids: &[ObjectId],
) -> Result<Vec<ObjectId>, CommandError> {
    let (_, units) = arrange_units(document, object_ids, 1)?;
    Ok(units.into_iter().flat_map(|unit| unit.member_ids).collect())
}

/// Validates an explicit Edit Group selection without expanding its siblings.
///
/// # Errors
/// Refuses repeated, missing, mixed-container or excessive member selections.
pub fn selected_member_ids(document: &Document, object_ids: &[ObjectId]) -> Result<Vec<ObjectId>, CommandError> {
    if object_ids.is_empty() || object_ids.len() > 500 {
        return Err(CommandError::InvalidArrangeGeometrySelection);
    }
    let mut ids = HashSet::new();
    let mut location = None;
    for id in object_ids {
        if !ids.insert(id) { return Err(CommandError::InvalidArrangeGeometrySelection); }
        let (candidate, _) = object_with_location(document, id)?;
        if location.as_ref().is_some_and(|expected| expected != &candidate) {
            return Err(CommandError::ArrangeGeometryObjectsInDifferentContainers);
        }
        location = Some(candidate);
    }
    Ok(object_ids.to_vec())
}

fn arrange_geometry_to_page(
    document: &mut Document,
    object_ids: &[ObjectId],
    operation: ArrangeGeometryOperation,
) -> Result<(), CommandError> {
    let (location, units) = arrange_units(document, object_ids, 1)?;
    let trim = match location {
        ObjectLocation::Page(page_id) => document.pages[page_index(document, &page_id)?].trim,
        ObjectLocation::Master(id) => document.masters[master_pages::master_index(document, &id)?].trim,
        ObjectLocation::Pasteboard => return Err(CommandError::ArrangeGeometryRequiresPage),
    };
    for unit in units {
        let (x, y) = match operation {
            ArrangeGeometryOperation::AlignLeft => (checked_delta(0, unit.bounds.left)?, 0),
            ArrangeGeometryOperation::AlignCenter => (
                rounded_half(checked_delta(
                    trim.width.value(),
                    unit.bounds.doubled_horizontal_center()?,
                )?),
                0,
            ),
            ArrangeGeometryOperation::AlignRight => {
                (checked_delta(trim.width.value(), unit.bounds.right)?, 0)
            }
            ArrangeGeometryOperation::AlignTop => (0, checked_delta(0, unit.bounds.top)?),
            ArrangeGeometryOperation::AlignMiddle => (
                0,
                rounded_half(checked_delta(
                    trim.height.value(),
                    unit.bounds.doubled_vertical_center()?,
                )?),
            ),
            ArrangeGeometryOperation::AlignBottom => {
                (0, checked_delta(trim.height.value(), unit.bounds.bottom)?)
            }
            ArrangeGeometryOperation::DistributeHorizontally
            | ArrangeGeometryOperation::DistributeVertically => {
                return Err(CommandError::ArrangeGeometryRequiresAlignment);
            }
        };
        for id in unit.member_ids {
            let object = object_mut(document, &id)?;
            object.transform.translation_x = Micrometres::new(
                object
                    .transform
                    .translation_x
                    .value()
                    .checked_add(x)
                    .ok_or(CommandError::ArrangeGeometryOverflow)?,
            );
            object.transform.translation_y = Micrometres::new(
                object
                    .transform
                    .translation_y
                    .value()
                    .checked_add(y)
                    .ok_or(CommandError::ArrangeGeometryOverflow)?,
            );
        }
    }
    Ok(())
}

fn arrange_translations(
    units: &[ArrangeUnit],
    operation: ArrangeGeometryOperation,
) -> Result<Vec<(i64, i64)>, CommandError> {
    let mut translations = vec![(0, 0); units.len()];
    match operation {
        ArrangeGeometryOperation::AlignLeft => {
            let target = units.iter().map(|unit| unit.bounds.left).min().unwrap();
            for (index, unit) in units.iter().enumerate() {
                translations[index].0 = checked_delta(target, unit.bounds.left)?;
            }
        }
        ArrangeGeometryOperation::AlignCenter => {
            let left = units.iter().map(|unit| unit.bounds.left).min().unwrap();
            let right = units.iter().map(|unit| unit.bounds.right).max().unwrap();
            let target = left
                .checked_add(right)
                .ok_or(CommandError::ArrangeGeometryOverflow)?;
            for (index, unit) in units.iter().enumerate() {
                translations[index].0 = rounded_half(checked_delta(
                    target,
                    unit.bounds.doubled_horizontal_center()?,
                )?);
            }
        }
        ArrangeGeometryOperation::AlignRight => {
            let target = units.iter().map(|unit| unit.bounds.right).max().unwrap();
            for (index, unit) in units.iter().enumerate() {
                translations[index].0 = checked_delta(target, unit.bounds.right)?;
            }
        }
        ArrangeGeometryOperation::AlignTop => {
            let target = units.iter().map(|unit| unit.bounds.top).min().unwrap();
            for (index, unit) in units.iter().enumerate() {
                translations[index].1 = checked_delta(target, unit.bounds.top)?;
            }
        }
        ArrangeGeometryOperation::AlignMiddle => {
            let top = units.iter().map(|unit| unit.bounds.top).min().unwrap();
            let bottom = units.iter().map(|unit| unit.bounds.bottom).max().unwrap();
            let target = top
                .checked_add(bottom)
                .ok_or(CommandError::ArrangeGeometryOverflow)?;
            for (index, unit) in units.iter().enumerate() {
                translations[index].1 = rounded_half(checked_delta(
                    target,
                    unit.bounds.doubled_vertical_center()?,
                )?);
            }
        }
        ArrangeGeometryOperation::AlignBottom => {
            let target = units.iter().map(|unit| unit.bounds.bottom).max().unwrap();
            for (index, unit) in units.iter().enumerate() {
                translations[index].1 = checked_delta(target, unit.bounds.bottom)?;
            }
        }
        ArrangeGeometryOperation::DistributeHorizontally => {
            distribute_centers(units, true, &mut translations)?;
        }
        ArrangeGeometryOperation::DistributeVertically => {
            distribute_centers(units, false, &mut translations)?;
        }
    }
    Ok(translations)
}

fn distribute_centers(
    units: &[ArrangeUnit],
    horizontal: bool,
    translations: &mut [(i64, i64)],
) -> Result<(), CommandError> {
    let mut ordered = units
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            let center = if horizontal {
                unit.bounds.doubled_horizontal_center()
            } else {
                unit.bounds.doubled_vertical_center()
            }?;
            Ok((index, center, unit.container_index))
        })
        .collect::<Result<Vec<_>, CommandError>>()?;
    ordered.sort_by_key(|(_, center, container_index)| (*center, *container_index));
    let first = ordered
        .first()
        .expect("distribution has at least three units")
        .1;
    let last = ordered
        .last()
        .expect("distribution has at least three units")
        .1;
    let denominator = i64::try_from(ordered.len() - 1).expect("bounded object count fits i64");
    for (position, (unit_index, center, _)) in ordered.into_iter().enumerate() {
        let span = last
            .checked_sub(first)
            .ok_or(CommandError::ArrangeGeometryOverflow)?;
        let numerator = span
            .checked_mul(i64::try_from(position).expect("bounded object count fits i64"))
            .ok_or(CommandError::ArrangeGeometryOverflow)?;
        let target = first
            .checked_add(rounded_ratio(numerator, denominator))
            .ok_or(CommandError::ArrangeGeometryOverflow)?;
        let delta = rounded_half(checked_delta(target, center)?);
        if horizontal {
            translations[unit_index].0 = delta;
        } else {
            translations[unit_index].1 = delta;
        }
    }
    Ok(())
}

fn rounded_half(value: i64) -> i64 {
    value / 2 + value.signum() * i64::from(value % 2 != 0)
}

fn rounded_ratio(numerator: i64, denominator: i64) -> i64 {
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    quotient + remainder.signum() * i64::from(remainder.abs() * 2 >= denominator)
}

fn checked_delta(target: i64, current: i64) -> Result<i64, CommandError> {
    target
        .checked_sub(current)
        .ok_or(CommandError::ArrangeGeometryOverflow)
}

fn arrange_geometry_target_ids(
    document: &Document,
    object_ids: &[ObjectId],
) -> Result<Vec<ObjectId>, CommandError> {
    let (_, units) = arrange_units(document, object_ids, 1)?;
    Ok(units.into_iter().flat_map(|unit| unit.member_ids).collect())
}

fn page_object_ids(document: &Document, page_id: &PageId) -> Result<Vec<ObjectId>, CommandError> {
    let page = document
        .pages
        .iter()
        .find(|page| &page.id == page_id)
        .ok_or_else(|| CommandError::PageNotFound(page_id.clone()))?;
    Ok(page
        .objects
        .iter()
        .map(|object| object.id.clone())
        .collect())
}

#[allow(clippy::too_many_lines)] // Exhaustive lock targeting stays centralized and auditable.
fn ensure_command_targets_unlocked(
    document: &Document,
    command: &Command,
) -> Result<(), CommandError> {
    let targets: Vec<ObjectId> = match command {
        Command::SetObjectLock { .. }
        | Command::AddPage { .. }
        | Command::DuplicatePage { .. }
        | Command::RenamePage { .. }
        | Command::ReorderPage { .. }
        | Command::UpdatePageBleed { .. }
        | Command::UpdatePageTrim { .. }
        | Command::SetExactFontFallbacks { .. }
        | Command::AddGuide { .. }
        | Command::MoveGuide { .. }
        | Command::DeleteGuide { .. }
        | Command::AddObject { .. }
        | Command::PasteObjects { .. }
        | Command::UpdateUntaggedRasterPolicy { .. }
        | Command::AddMaster { .. } | Command::RenameMaster { .. } | Command::DuplicateMaster { .. }
        | Command::ResizeMaster { .. } | Command::AssignMaster { .. }
        | Command::UpdatePageNumbering { .. } | Command::SetPageNumberSuppression { .. } => return Ok(()),
        Command::DeletePage { page_id } => page_object_ids(document, page_id)?,
        Command::RemoveMaster { master_id, .. } => {
            document.masters[master_pages::master_index(document, master_id)?].objects.iter().map(|object| object.id.clone()).collect()
        }
        Command::GroupObjects { object_ids } => object_ids.clone(),
        Command::UngroupObjects { object_ids } => {
            let groups = object_ids
                .iter()
                .map(|object_id| {
                    object_with_location(document, object_id)?
                        .1
                        .group_id
                        .clone()
                        .ok_or_else(|| CommandError::ObjectNotGrouped(object_id.clone()))
                })
                .collect::<Result<HashSet<_>, _>>()?;
            document.authored_objects()
                .filter(|object| {
                    object
                        .group_id
                        .as_ref()
                        .is_some_and(|group_id| groups.contains(group_id))
                })
                .map(|object| object.id.clone())
                .collect()
        }
        Command::DuplicateObject { object_id, .. }
        | Command::MoveObject { object_id, .. }
        | Command::SetObjectPosition { object_id, .. }
        | Command::ReorderObject { object_id, .. }
        | Command::ResizeObject { object_id, .. }
        | Command::UpdateText { object_id, .. }
        | Command::ReplaceTextRange { object_id, .. }
        | Command::ApplyTextEdits { object_id, .. }
        | Command::UpdateTextStyle { object_id, .. }
        | Command::SetTextFontVariantBinding { object_id, .. }
        | Command::SetTextFontFamilyBinding { object_id, .. }
        | Command::SetInlineFontSize { object_id, .. }
        | Command::UpdateInlineTextStyle { object_id, .. }
        | Command::UpdateInlineTextAppearance { object_id, .. }
        | Command::UpdateParagraphStyle { object_id, .. }
        | Command::UpdateTextFrame { object_id, .. }
        | Command::UpdateTextBoxInterior { object_id, .. }
        | Command::UpdateTextBoxSizing { object_id, .. }
        | Command::UpdateWrapMode { object_id, .. }
        | Command::UpdateWrapSettings { object_id, .. }
        | Command::UpdateImageContentKind { object_id, .. }
        | Command::UpdateImageCrop { object_id, .. }
        | Command::SetImageFlip { object_id, .. }
        | Command::ResetImageEditing { object_id }
        | Command::ReplaceImage { object_id, .. }
        | Command::SetImageAspectLock { object_id, .. }
        | Command::UpdateTableCell { object_id, .. }
        | Command::UpdateTableCells { object_id, .. }
        | Command::UpdateTableDimensions { object_id, .. }
        | Command::InsertTableTrack { object_id, .. }
        | Command::DeleteTableTrack { object_id, .. }
        | Command::UpdateTableTextStyle { object_id, .. }
        | Command::UpdateTableGridBorder { object_id, .. }
        | Command::UpdateTableCellAlignment { object_id, .. }
        | Command::UpdateTableCellFormatting { object_id, .. }
        | Command::ResetTableCellFormatting { object_id, .. }
        | Command::MergeTableCells { object_id, .. }
        | Command::SplitTableCells { object_id, .. }
        | Command::UpdateTableTrackSize { object_id, .. }
        | Command::DistributeTableTracks { object_id, .. }
        | Command::SetOpacity { object_id, .. }
        | Command::SetRotation { object_id, .. }
        | Command::UpdateShape { object_id, .. }
        | Command::UpdateCornerRounding { object_id, .. }
        | Command::UpdateObjectEffects { object_id, .. }
        | Command::SetTextField { object_id, .. } => vec![object_id.clone()],
        Command::DeleteObject { object_id } => {
            let selected = object_with_location(document, object_id)?.1;
            if let Some(group_id) = &selected.group_id {
                document.authored_objects()
                    .filter(|object| object.group_id.as_ref() == Some(group_id))
                    .map(|object| object.id.clone())
                    .collect()
            } else {
                vec![object_id.clone()]
            }
        }
        Command::ArrangeObject { object_id, .. } => {
            let selected = object_with_location(document, object_id)?.1;
            if let Some(group_id) = &selected.group_id {
                document.authored_objects()
                    .filter(|object| object.group_id.as_ref() == Some(group_id))
                    .map(|object| object.id.clone())
                    .collect()
            } else {
                vec![object_id.clone()]
            }
        }
        Command::ArrangeSelection { object_ids, .. } | Command::ArrangeGeometry { object_ids, .. } | Command::ArrangeGeometryToPage { object_ids, .. }
        | Command::MoveSelection { object_ids, .. } | Command::DeleteSelection { object_ids } => {
            arrange_geometry_target_ids(document, object_ids)?
        }
        Command::ApplySelectionTransforms { objects } => selected_object_ids(document, &objects.iter().map(|object| object.object_id.clone()).collect::<Vec<_>>())?,
        Command::ApplyMemberTransforms { objects } => selected_member_ids(document, &objects.iter().map(|object| object.object_id.clone()).collect::<Vec<_>>())?,
    };
    for object_id in targets {
        if object_with_location(document, &object_id)?.1.locked {
            return Err(CommandError::ObjectLocked(object_id));
        }
    }
    Ok(())
}

fn validate_guide_position(
    trim: PhysicalSize,
    axis: GuideAxis,
    position: Micrometres,
) -> Result<(), CommandError> {
    let maximum = match axis {
        GuideAxis::Horizontal => trim.height.value(),
        GuideAxis::Vertical => trim.width.value(),
    };
    if (0..=maximum).contains(&position.value()) {
        Ok(())
    } else {
        Err(CommandError::GuidePositionOutOfRange {
            position: position.value(),
            maximum,
        })
    }
}

fn validate_guides_for_trim(guides: &[PageGuide], trim: PhysicalSize) -> Result<(), CommandError> {
    for guide in guides {
        validate_guide_position(trim, guide.axis, guide.position)?;
    }
    Ok(())
}

fn reorder_in(
    objects: &mut Vec<PublicationObject>,
    old_index: usize,
    new_index: usize,
) -> Result<(), CommandError> {
    if new_index >= objects.len() {
        return Err(CommandError::ObjectIndexOutOfRange(new_index));
    }
    if old_index != new_index {
        let object = objects.remove(old_index);
        objects.insert(new_index, object);
    }
    Ok(())
}

fn object_mut<'a>(
    document: &'a mut Document,
    object_id: &ObjectId,
) -> Result<&'a mut PublicationObject, CommandError> {
    document
        .authored_objects_mut()
        .find(|object| &object.id == object_id)
        .ok_or_else(|| CommandError::ObjectNotFound(object_id.clone()))
}

fn update_text(
    document: &mut Document,
    object_id: ObjectId,
    text: String,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Text {
        text: current_text,
        story,
        ..
    } = &mut object.kind
    else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    let replacement = TextReplacement {
        old_start: 0,
        old_end: current_text.len(),
        new_start: 0,
        new_end: text.len(),
    };
    let rebased_story = rebase_text_story(current_text, &text, story, replacement)?;
    *current_text = text;
    *story = rebased_story;
    Ok(())
}

fn replace_text_range(
    document: &mut Document,
    object_id: ObjectId,
    range: TextSourceRange,
    replacement_text: &str,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Text {
        text: current_text,
        story,
        ..
    } = &mut object.kind
    else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    range.validate_edit(current_text)?;
    let mut text = current_text.clone();
    text.replace_range(range.start..range.end, replacement_text);
    let replacement = TextReplacement {
        old_start: range.start,
        old_end: range.end,
        new_start: range.start,
        new_end: range.start + replacement_text.len(),
    };
    let rebased_story = rebase_text_story(current_text, &text, story, replacement)?;
    *current_text = text;
    *story = rebased_story;
    Ok(())
}

fn apply_text_edits(
    document: &mut Document,
    object_id: ObjectId,
    edits: Vec<TextEdit>,
) -> Result<(), CommandError> {
    if edits.is_empty() {
        return Err(CommandError::EmptyTextEdits);
    }
    if edits.len() > MAX_TEXT_EDIT_COUNT {
        return Err(CommandError::TextEditCountExceeded);
    }
    let inserted_bytes = edits
        .iter()
        .try_fold(0_usize, |total, edit| total.checked_add(edit.text.len()));
    if inserted_bytes.is_none_or(|total| total > MAX_TEXT_EDIT_INSERTED_BYTES) {
        return Err(CommandError::TextEditInsertedBytesExceeded);
    }
    for edit in edits {
        replace_text_range(document, object_id.clone(), edit.range, &edit.text)?;
        let inserted_end = edit.range.start + edit.text.len();
        let mut patch = edit.style.unwrap_or_default();
        patch.bold = edit.bold.or(patch.bold);
        patch.italic = edit.italic.or(patch.italic);
        if inserted_end > edit.range.start && !patch.is_default() {
            update_inline_style(
                document,
                object_id.clone(),
                TextSourceRange {
                    start: edit.range.start,
                    end: inserted_end,
                },
                &patch,
            )?;
        }
    }
    let ObjectKind::Text { text, .. } = &object_mut(document, &object_id)?.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    if text.len() > crate::model::MAX_TEXT_BYTES {
        return Err(ModelError::TextTooLarge.into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct TextReplacement {
    old_start: usize,
    old_end: usize,
    new_start: usize,
    new_end: usize,
}

fn rebase_text_story(
    old_text: &str,
    new_text: &str,
    story: &TextStoryFormatting,
    replacement: TextReplacement,
) -> Result<TextStoryFormatting, ModelError> {
    let inline_spans = rebase_inline_spans(story, replacement);
    let paragraph_runs = rebase_paragraph_runs(old_text, new_text, story, replacement)?;
    let rebased = TextStoryFormatting {
        inline_spans,
        paragraph_runs,
    };
    rebased.validate(new_text)?;
    Ok(rebased)
}

fn rebase_inline_spans(
    story: &TextStoryFormatting,
    replacement: TextReplacement,
) -> Vec<InlineTextSpan> {
    let mut fragments = Vec::with_capacity(story.inline_spans.len().saturating_mul(2) + 1);
    for span in &story.inline_spans {
        if span.range.start < replacement.old_start {
            fragments.push(InlineTextSpan {
                range: TextSourceRange {
                    start: span.range.start,
                    end: span.range.end.min(replacement.old_start),
                },
                style: span.style.clone(),
            });
        }
        if span.range.end > replacement.old_end {
            fragments.push(InlineTextSpan {
                range: TextSourceRange {
                    start: replacement.new_end + span.range.start.max(replacement.old_end)
                        - replacement.old_end,
                    end: replacement.new_end + span.range.end - replacement.old_end,
                },
                style: span.style.clone(),
            });
        }
    }

    if replacement.new_start < replacement.new_end {
        let insertion_style = insertion_inline_style(story, replacement);
        if !insertion_style.is_default() {
            fragments.push(InlineTextSpan {
                range: TextSourceRange {
                    start: replacement.new_start,
                    end: replacement.new_end,
                },
                style: insertion_style,
            });
        }
    }

    fragments.sort_by_key(|span| span.range.start);
    let mut normalized = Vec::with_capacity(fragments.len());
    for fragment in fragments {
        push_inline_span(&mut normalized, fragment.range, fragment.style);
    }
    normalized
}

fn insertion_inline_style(
    story: &TextStoryFormatting,
    replacement: TextReplacement,
) -> InlineTextStyle {
    let right = || {
        story
            .inline_spans
            .iter()
            .find(|span| {
                span.range.start <= replacement.old_start && replacement.old_start < span.range.end
            })
            .map_or_else(InlineTextStyle::default, |span| span.style.clone())
    };
    let left = || {
        story
            .inline_spans
            .iter()
            .find(|span| {
                span.range.start < replacement.old_start && replacement.old_start <= span.range.end
            })
            .map_or_else(InlineTextStyle::default, |span| span.style.clone())
    };
    // A replacement inherits its first matched character, including the base
    // style when that character has no override. Ordinary caret insertion still
    // prefers the preceding character's style.
    if replacement.old_start < replacement.old_end {
        return right();
    }
    let (preferred, fallback) = (left(), right());
    if preferred.is_default() {
        fallback
    } else {
        preferred
    }
}

fn rebase_paragraph_runs(
    old_text: &str,
    new_text: &str,
    story: &TextStoryFormatting,
    replacement: TextReplacement,
) -> Result<Vec<ParagraphStyleRun>, ModelError> {
    let old_paragraph_count = paragraph_count(old_text)? as usize;
    let mut old_styles = vec![ParagraphStyle::default(); old_paragraph_count];
    for run in &story.paragraph_runs {
        for style in &mut old_styles[run.range.start as usize..run.range.end as usize] {
            *style = run.style;
        }
    }

    let start_paragraph = old_text[..replacement.old_start].matches('\n').count();
    let end_paragraph = old_text[..replacement.old_end].matches('\n').count();
    let inserted_paragraph_breaks = new_text[replacement.new_start..replacement.new_end]
        .matches('\n')
        .count();
    let inherited = old_styles[start_paragraph];
    let expected_count = paragraph_count(new_text)? as usize;
    let mut new_styles = Vec::with_capacity(expected_count);
    new_styles.extend_from_slice(&old_styles[..start_paragraph]);
    new_styles.extend(std::iter::repeat_n(
        inherited,
        inserted_paragraph_breaks + 1,
    ));
    new_styles.extend_from_slice(&old_styles[end_paragraph + 1..]);
    if new_styles.len() != expected_count {
        return Err(ModelError::TextFormattingNotNormalized);
    }

    let mut normalized = Vec::new();
    for (index, style) in new_styles.into_iter().enumerate() {
        let start = u32::try_from(index).map_err(|_| ModelError::TooManyParagraphs)?;
        let end = start.checked_add(1).ok_or(ModelError::TooManyParagraphs)?;
        push_paragraph_run(&mut normalized, ParagraphRange { start, end }, style);
    }
    Ok(normalized)
}

fn update_inline_text_style(
    document: &mut Document,
    object_id: ObjectId,
    range: TextSourceRange,
    bold: Option<bool>,
    italic: Option<bool>,
) -> Result<(), CommandError> {
    update_inline_style(
        document,
        object_id,
        range,
        &InlineTextStyle {
            bold,
            italic,
            ..InlineTextStyle::default()
        },
    )
}

fn update_inline_style(
    document: &mut Document,
    object_id: ObjectId,
    range: TextSourceRange,
    patch: &InlineTextStyle,
) -> Result<(), CommandError> {
    if patch
        .font_family
        .as_ref()
        .is_some_and(|family| family.trim().is_empty())
    {
        return Err(ModelError::EmptyFontFamily.into());
    }
    if patch
        .font_weight
        .is_some_and(|weight| !(1..=1_000).contains(&weight))
    {
        return Err(ModelError::FontWeightOutOfRange.into());
    }
    if patch
        .font_size_millipoints
        .is_some_and(|size| !(1_000..=1_000_000).contains(&size))
    {
        return Err(ModelError::FontSizeOutOfRange.into());
    }
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Text { text, story, .. } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    range.validate(text)?;
    if patch.is_default() {
        return Err(ModelError::EmptyInlineTextStyle.into());
    }

    let mut boundaries = std::collections::BTreeSet::from([range.start, range.end]);
    for span in &story.inline_spans {
        boundaries.insert(span.range.start);
        boundaries.insert(span.range.end);
    }
    let boundaries = boundaries.into_iter().collect::<Vec<_>>();
    let mut normalized = Vec::new();
    for pair in boundaries.windows(2) {
        let segment = TextSourceRange {
            start: pair[0],
            end: pair[1],
        };
        let existing = story
            .inline_spans
            .iter()
            .find(|span| span.range.start <= segment.start && segment.end <= span.range.end)
            .map_or_else(InlineTextStyle::default, |span| span.style.clone());
        let style = if range.start <= segment.start && segment.end <= range.end {
            InlineTextStyle {
                font_family: patch.font_family.clone().or(existing.font_family),
                font_weight: patch.font_weight.or(existing.font_weight),
                font_size_millipoints: patch
                    .font_size_millipoints
                    .or(existing.font_size_millipoints),
                bold: if patch.font_weight.is_some() {
                    patch.bold
                } else {
                    patch.bold.or(existing.bold)
                },
                italic: patch.italic.or(existing.italic),
                color: patch.color.or(existing.color),
                underline: patch.underline.or(existing.underline),
            }
        } else {
            existing
        };
        push_inline_span(&mut normalized, segment, style);
    }
    story.inline_spans = normalized;
    Ok(())
}

fn push_inline_span(
    spans: &mut Vec<InlineTextSpan>,
    range: TextSourceRange,
    style: InlineTextStyle,
) {
    if style.is_default() {
        return;
    }
    if let Some(previous) = spans.last_mut()
        && previous.range.end == range.start
        && previous.style == style
    {
        previous.range.end = range.end;
        return;
    }
    spans.push(InlineTextSpan { range, style });
}

fn update_paragraph_style(
    document: &mut Document,
    object_id: ObjectId,
    range: ParagraphRange,
    patch: ParagraphStyle,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Text { text, story, .. } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    range.validate(paragraph_count(text)?)?;
    patch.validate()?;

    let mut boundaries = std::collections::BTreeSet::from([range.start, range.end]);
    for run in &story.paragraph_runs {
        boundaries.insert(run.range.start);
        boundaries.insert(run.range.end);
    }
    let boundaries = boundaries.into_iter().collect::<Vec<_>>();
    let mut normalized = Vec::new();
    for pair in boundaries.windows(2) {
        let segment = ParagraphRange {
            start: pair[0],
            end: pair[1],
        };
        let existing = story
            .paragraph_runs
            .iter()
            .find(|run| run.range.start <= segment.start && segment.end <= run.range.end)
            .map_or_else(ParagraphStyle::default, |run| run.style);
        let style = if range.start <= segment.start && segment.end <= range.end {
            overlay_paragraph_style(existing, patch)
        } else {
            existing
        };
        push_paragraph_run(&mut normalized, segment, style);
    }
    story.paragraph_runs = normalized;
    Ok(())
}

fn overlay_paragraph_style(existing: ParagraphStyle, patch: ParagraphStyle) -> ParagraphStyle {
    ParagraphStyle {
        list_style: patch.list_style.or(existing.list_style),
        alignment: patch.alignment.or(existing.alignment),
        start_indent_um: patch.start_indent_um.or(existing.start_indent_um),
        end_indent_um: patch.end_indent_um.or(existing.end_indent_um),
        first_line_indent_um: patch.first_line_indent_um.or(existing.first_line_indent_um),
        line_height_per_thousand: patch
            .line_height_per_thousand
            .or(existing.line_height_per_thousand),
        space_before_millipoints: patch
            .space_before_millipoints
            .or(existing.space_before_millipoints),
        space_after_millipoints: patch
            .space_after_millipoints
            .or(existing.space_after_millipoints),
    }
}

fn push_paragraph_run(
    runs: &mut Vec<ParagraphStyleRun>,
    range: ParagraphRange,
    style: ParagraphStyle,
) {
    if style.is_default() {
        return;
    }
    if let Some(previous) = runs.last_mut()
        && previous.range.end == range.start
        && previous.style == style
    {
        previous.range.end = range.end;
        return;
    }
    runs.push(ParagraphStyleRun { range, style });
}

fn update_text_style(
    document: &mut Document,
    object_id: ObjectId,
    mut style: TextStyle,
    font_face_update: FontFaceBindingUpdate,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Text {
        style: current_style,
        story,
        ..
    } = &mut object.kind
    else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    let retains_previous_family = current_style.font_family != style.font_family
        && story
            .inline_spans
            .iter()
            .any(|span| span.style.font_family.as_ref() == Some(&current_style.font_family));
    apply_font_face_update(current_style, &mut style, font_face_update)?;
    if retains_previous_family {
        if let Some(face) = &current_style.font_face {
            set_font_family_binding(
                &mut style,
                current_style.font_family.clone(),
                current_style.font_weight,
                current_style.italic,
                face.clone(),
            )?;
        }
        for variant in &current_style.exact_font_variants {
            set_font_family_binding(
                &mut style,
                current_style.font_family.clone(),
                variant.font_weight,
                variant.italic,
                variant.face.clone(),
            )?;
        }
    }
    *current_style = style;
    Ok(())
}

fn set_text_font_variant_binding(
    document: &mut Document,
    object_id: ObjectId,
    font_weight: u16,
    italic: bool,
    font_face: Option<FontFaceBinding>,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let style = match &mut object.kind {
        ObjectKind::Text { style, .. } => style,
        ObjectKind::Table { table } => &mut table.style,
        _ => return Err(CommandError::ObjectKindMismatch(object_id)),
    };
    if !(1..=1_000).contains(&font_weight) {
        return Err(ModelError::FontWeightOutOfRange.into());
    }
    let key = (font_weight, italic);
    if key == (style.font_weight, style.italic) {
        return Err(ModelError::ExactFontVariantMatchesBaseStyle.into());
    }
    match style
        .exact_font_variants
        .binary_search_by_key(&key, |variant| (variant.font_weight, variant.italic))
    {
        Ok(index) => match font_face {
            Some(face) => style.exact_font_variants[index].face = face,
            None => {
                style.exact_font_variants.remove(index);
            }
        },
        Err(index) => {
            let Some(face) = font_face else {
                return Ok(());
            };
            if style.exact_font_variants.len() >= MAX_EXACT_FONT_VARIANTS {
                return Err(ModelError::TooManyExactFontVariants.into());
            }
            style.exact_font_variants.insert(
                index,
                ExactFontVariantBinding {
                    font_weight,
                    italic,
                    face,
                },
            );
        }
    }
    Ok(())
}

fn apply_font_face_update(
    current_style: &TextStyle,
    style: &mut TextStyle,
    font_face_update: FontFaceBindingUpdate,
) -> Result<(), CommandError> {
    let family_unchanged = current_style.font_family == style.font_family;
    style
        .exact_font_families
        .clone_from(&current_style.exact_font_families);
    if !family_unchanged {
        style.font_face = match font_face_update {
            FontFaceBindingUpdate::PreserveIfFamilyUnchanged => None,
            FontFaceBindingUpdate::Replace(binding) => binding,
        };
        style.exact_font_variants.clear();
        return Ok(());
    }

    let current_key = (current_style.font_weight, current_style.italic);
    let next_key = (style.font_weight, style.italic);
    let mut variants = current_style.exact_font_variants.clone();
    if current_key == next_key {
        style.font_face = match font_face_update {
            FontFaceBindingUpdate::PreserveIfFamilyUnchanged => current_style.font_face.clone(),
            FontFaceBindingUpdate::Replace(binding) => binding,
        };
    } else {
        let promoted_face = variants
            .binary_search_by_key(&next_key, |variant| (variant.font_weight, variant.italic))
            .ok()
            .map(|index| variants.remove(index).face);
        style.font_face = match font_face_update {
            FontFaceBindingUpdate::PreserveIfFamilyUnchanged => promoted_face,
            FontFaceBindingUpdate::Replace(binding) => binding,
        };
        if let Some(face) = current_style.font_face.clone() {
            let insertion_index = variants
                .binary_search_by_key(&current_key, |variant| {
                    (variant.font_weight, variant.italic)
                })
                .expect_err("validated registry excludes the base style key");
            variants.insert(
                insertion_index,
                ExactFontVariantBinding {
                    font_weight: current_key.0,
                    italic: current_key.1,
                    face,
                },
            );
        }
    }
    if variants.len() > MAX_EXACT_FONT_VARIANTS {
        return Err(ModelError::TooManyExactFontVariants.into());
    }
    style.exact_font_variants = variants;
    Ok(())
}

fn set_font_family_binding(
    style: &mut TextStyle,
    font_family: String,
    font_weight: u16,
    italic: bool,
    face: FontFaceBinding,
) -> Result<(), CommandError> {
    if font_family.trim().is_empty() {
        return Err(ModelError::EmptyFontFamily.into());
    }
    if !(1..=1_000).contains(&font_weight) {
        return Err(ModelError::FontWeightOutOfRange.into());
    }
    let key = (font_family.as_str(), font_weight, italic);
    match style.exact_font_families.binary_search_by(|binding| {
        (
            binding.font_family.as_str(),
            binding.font_weight,
            binding.italic,
        )
            .cmp(&key)
    }) {
        Ok(index) => style.exact_font_families[index].face = face,
        Err(index) => {
            if style.exact_font_families.len() >= MAX_EXACT_FONT_FAMILY_BINDINGS {
                return Err(ModelError::TooManyExactFontVariants.into());
            }
            style.exact_font_families.insert(
                index,
                ExactFontFamilyBinding {
                    font_family,
                    font_weight,
                    italic,
                    face,
                },
            );
        }
    }
    Ok(())
}

/// How a text-style edit treats an existing exact font resource binding.
///
/// Ordinary formatting edits preserve exact faces only while retaining the
/// same family label. A base weight/slant change exchanges the old base and a
/// matching captured variant without losing either identity. A family change
/// clears every variant and clears the base unless the caller deliberately
/// supplies a replacement resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FontFaceBindingUpdate {
    PreserveIfFamilyUnchanged,
    Replace(Option<FontFaceBinding>),
}

fn update_text_frame(
    document: &mut Document,
    object_id: ObjectId,
    column_count: u16,
    column_gap_um: i64,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Text {
        frame: current_frame,
        ..
    } = &mut object.kind
    else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    let updated = TextFrame {
        column_count,
        column_gap_um,
        ..*current_frame
    };
    updated.validate_usable_for_size(object.size)?;
    *current_frame = updated;
    Ok(())
}

fn update_text_box_interior(
    document: &mut Document,
    object_id: ObjectId,
    margins: Insets,
    vertical_alignment: TextVerticalAlignment,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Text { frame, .. } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    let updated = TextFrame {
        margins,
        vertical_alignment,
        ..*frame
    };
    updated.validate_usable_for_size(object.size)?;
    *frame = updated;
    Ok(())
}

fn update_text_box_sizing(
    document: &mut Document,
    object_id: ObjectId,
    sizing_mode: TextBoxSizingMode,
    size: PhysicalSize,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Text { frame, .. } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    let updated_frame = TextFrame {
        sizing_mode,
        ..*frame
    };
    updated_frame.validate_usable_for_size(size)?;
    object.size = size;
    *frame = updated_frame;
    Ok(())
}

fn update_wrap_mode(
    document: &mut Document,
    object_id: ObjectId,
    wrap_mode: WrapMode,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    match &mut object.kind {
        ObjectKind::Image {
            wrap_mode: current, ..
        }
        | ObjectKind::Shape {
            wrap_mode: current, ..
        } => {
            *current = wrap_mode;
            Ok(())
        }
        ObjectKind::Text { .. } | ObjectKind::Table { .. } => {
            Err(CommandError::ObjectKindMismatch(object_id))
        }
    }
}

fn update_image_content_kind(
    document: &mut Document,
    object_id: ObjectId,
    content_kind: ImageContentKind,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Image {
        content_kind: current,
        ..
    } = &mut object.kind
    else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    *current = content_kind;
    Ok(())
}

fn update_image_crop(
    document: &mut Document,
    object_id: ObjectId,
    crop: ImageCrop,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Image { crop: current, .. } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    *current = crop;
    Ok(())
}

fn set_image_aspect_lock(
    document: &mut Document,
    object_id: ObjectId,
    locked: bool,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Image {
        lock_aspect_ratio, ..
    } = &mut object.kind
    else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    *lock_aspect_ratio = locked;
    Ok(())
}

fn update_table_cell(
    document: &mut Document,
    object_id: ObjectId,
    row: u16,
    column: u16,
    text: String,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Table { table } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    table.set_cell(row, column, text)?;
    Ok(())
}

fn update_table_dimensions(
    document: &mut Document,
    object_id: ObjectId,
    rows: u16,
    columns: u16,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Table { table } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    table.resize(rows, columns)?;
    Ok(())
}

fn insert_table_track(
    document: &mut Document,
    object_id: ObjectId,
    axis: TableTrackAxis,
    index: u16,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Table { table } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    table.insert_track(axis, index)?;
    Ok(())
}

fn delete_table_track(
    document: &mut Document,
    object_id: ObjectId,
    axis: TableTrackAxis,
    index: u16,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Table { table } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    table.delete_track(axis, index)?;
    Ok(())
}

fn update_table_text_style(
    document: &mut Document,
    object_id: ObjectId,
    mut style: TextStyle,
    font_face_update: FontFaceBindingUpdate,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Table { table } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    let retains_previous_family = table.style.font_family != style.font_family
        && table.cell_formatting.iter().any(|formatting| {
            formatting.style.font_family.as_ref() == Some(&table.style.font_family)
        });
    apply_font_face_update(&table.style, &mut style, font_face_update)?;
    if retains_previous_family {
        if let Some(face) = &table.style.font_face {
            set_font_family_binding(
                &mut style,
                table.style.font_family.clone(),
                table.style.font_weight,
                table.style.italic,
                face.clone(),
            )?;
        }
        for variant in &table.style.exact_font_variants {
            set_font_family_binding(
                &mut style,
                table.style.font_family.clone(),
                variant.font_weight,
                variant.italic,
                variant.face.clone(),
            )?;
        }
    }
    table.style = style;
    Ok(())
}

fn require_table_mut<'a>(document: &'a mut Document, object_id: &ObjectId) -> Result<&'a mut crate::model::TableData, CommandError> {
    let object = object_mut(document, object_id)?;
    let ObjectKind::Table {table} = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id.clone()));
    };
    Ok(table)
}

fn update_table_grid_border(
    document: &mut Document,
    object_id: ObjectId,
    border: TableGridBorder,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Table { table } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    table.grid_border = border;
    Ok(())
}

fn update_table_cell_alignment(
    document: &mut Document,
    object_id: ObjectId,
    row: u16,
    column: u16,
    horizontal_alignment: Option<TextAlignment>,
    vertical_alignment: Option<TableVerticalAlignment>,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Table { table } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    table.set_cell_alignment(row, column, horizontal_alignment, vertical_alignment)?;
    Ok(())
}

fn update_table_track_size(
    document: &mut Document,
    object_id: ObjectId,
    axis: TableTrackAxis,
    index: u16,
    weight: u32,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Table { table } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    table.set_track_weight(axis, index, weight)?;
    Ok(())
}

fn distribute_table_tracks(
    document: &mut Document,
    object_id: ObjectId,
    axis: TableTrackAxis,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Table { table } = &mut object.kind else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    table.distribute_tracks(axis);
    Ok(())
}

fn update_shape(
    document: &mut Document,
    object_id: ObjectId,
    shape: ShapeKind,
    fill: Color,
    stroke: ShapeStroke,
) -> Result<(), CommandError> {
    let object = object_mut(document, &object_id)?;
    let ObjectKind::Shape {
        shape: current_shape,
        fill: current_fill,
        stroke: current_stroke,
        ..
    } = &mut object.kind
    else {
        return Err(CommandError::ObjectKindMismatch(object_id));
    };
    *current_shape = shape;
    *current_fill = fill;
    *current_stroke = stroke;
    if shape != ShapeKind::Rectangle { object.corner_rounding = CornerRounding::default(); }
    Ok(())
}

fn update_object_effects(
    document: &mut Document,
    object_id: &ObjectId,
    effects: ObjectEffects,
) -> Result<(), CommandError> {
    let object = object_mut(document, object_id)?;
    effects.validate_for(&object.kind)?;
    object.effects = effects;
    Ok(())
}

fn set_rotation(
    document: &mut Document,
    object_id: &ObjectId,
    rotation_microdegrees: i64,
) -> Result<(), CommandError> {
    object_mut(document, object_id)?
        .transform
        .rotation_microdegrees = rotation_microdegrees;
    Ok(())
}

fn set_opacity(
    document: &mut Document,
    object_id: &ObjectId,
    opacity_millionths: u32,
) -> Result<(), CommandError> {
    object_mut(document, object_id)?.opacity_millionths = opacity_millionths;
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandError {
    RevisionConflict {
        expected: Revision,
        actual: Revision,
    },
    EmptyTransaction,
    NothingToUndo,
    NothingToRedo,
    PageNotFound(PageId),
    PageLimitReached {
        maximum: u16,
    },
    ObjectNotFound(ObjectId),
    GuideNotFound(GuideId),
    GuidePositionOutOfRange {
        position: i64,
        maximum: i64,
    },
    ObjectLocked(ObjectId),
    GroupedObjectRequiresArrange(ObjectId),
    ObjectKindMismatch(ObjectId),
    PageIndexOutOfRange(usize),
    ObjectIndexOutOfRange(usize),
    InvalidGroupSelection,
    InvalidLockSelection,
    InvalidArrangeGeometrySelection,
    InvalidPasteSelection,
    InvalidPageSelection,
    ObjectAlreadyGrouped(ObjectId),
    ObjectNotGrouped(ObjectId),
    GroupObjectsInDifferentContainers,
    ArrangeGeometryObjectsInDifferentContainers,
    PartialGroupPaste,
    ArrangeGeometryOverflow,
    ArrangeGeometryRequiresPage,
    ArrangeGeometryRequiresAlignment,
    RevisionExhausted,
    EmptyTextEdits,
    TextEditCountExceeded,
    TextEditInsertedBytesExceeded,
    InvalidModel(ModelError),
    Saveability(String),
}

impl From<ModelError> for CommandError {
    fn from(error: ModelError) -> Self {
        Self::InvalidModel(error)
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Saveability(message) => formatter.write_str(message),
            Self::InvalidModel(error) => error.fmt(formatter),
            Self::InvalidArrangeGeometrySelection => formatter.write_str("Select complete objects or groups in one container, with at most 500 members."),
            Self::InvalidPageSelection => formatter.write_str("Select between 1 and 500 distinct Pages."),
            Self::ArrangeGeometryRequiresPage => formatter.write_str("Place this selection on a Page before aligning it to the Page."),
            Self::ArrangeGeometryRequiresAlignment => formatter.write_str("Choose an alignment command when arranging relative to the Page."),
            Self::PageLimitReached { maximum } => write!(
                formatter,
                "This publication supports at most {maximum} pages. Remove a page before adding or duplicating another."
            ),
            _ => write!(formatter, "document command rejected: {self:?}"),
        }
    }
}

impl std::error::Error for CommandError {}
