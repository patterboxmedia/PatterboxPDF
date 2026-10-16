//! Deterministic, UI-independent publication model and persistence boundary.
//!
//! The crate deliberately has no desktop-shell, renderer, or print dependencies.

pub mod assets;
pub mod commands;
pub mod geometry;
pub mod shape_geometry;
pub mod model;
pub mod package;
pub mod recovery;
pub use raster_capacity;
mod save_transaction;

pub use assets::{
    AssetHash, AssetStore, EmbeddedAsset, MAX_DECODED_RASTER_BYTES, MAX_ICC_PROFILE_BYTES,
    MAX_RASTER_FILE_BYTES, RasterColorProfile, RasterIngestError, RasterInspection, inspect_raster,
};
pub use commands::{
    ArrangeGeometryOperation, Command, CommandError, CommandOutcome, DocumentSession,
    MAX_TEXT_EDIT_COUNT, MAX_TEXT_EDIT_INSERTED_BYTES, ObjectLocation, TextEdit, Transaction,
};
pub use model::{
    Document, DocumentColorSettings, DocumentId, ExactFontFallbackChain, FontFallbackScript,
    ObjectId, PageId, Revision, UntaggedRasterPolicy,
};
pub use package::{
    OpenedPackage, PACKAGE_LIMITS_V1, PackageError, PackageFingerprint, PackageLimits,
    ProjectSnapshot, WindowsReplaceState, open_package, save_existing_package, save_new_package,
};
pub use recovery::{
    RecoveryCandidate, RecoveryError, discard_recovery, discover_recoveries, open_recovery_package,
    save_recovery,
};
pub use save_transaction::{
    InterruptedSaveCandidate, InterruptedSaveRecoveryChoice, InterruptedSaveRecoveryOutcome,
    InterruptedSaveState, InterruptedSaveStatus, SaveAttemptFailure, inspect_interrupted_save,
    open_package_for_editing, recover_interrupted_save, save_existing_package_retryable,
    save_existing_package_retryable_with_cancel,
};
