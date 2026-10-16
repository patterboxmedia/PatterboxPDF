//! Professional-print boundary.
//!
//! The PDF emitted here is an **unvalidated candidate**, never proof of PDF/X
//! conformance. Structural inspection and the optional `LittleCMS` adapter are
//! intermediate evidence. A separate independent validator must inspect the
//! exact candidate bytes before any caller describes an export as compliant.
//!
//! Color-worker proof is intentionally not a caller-construction API:
//! ```compile_fail
//! use color_boundary::TrustedColorConversion;
//! let forged = TrustedColorConversion { /* private trust facts */ };
//! ```
//!
//! Font-worker proof is equally opaque:
//! ```compile_fail
//! use font_boundary::PreparedFontSubset;
//! let forged = PreparedFontSubset { /* private trust facts */ };
//! ```

#[cfg(feature = "publisher-adapter")]
mod candidate;
mod candidate_metadata;
mod to_unicode;
#[cfg(feature = "publisher-adapter")]
mod color;
#[cfg(feature = "publisher-adapter")]
mod export;
#[cfg(feature = "publisher-adapter")]
mod fixture;
#[cfg(feature = "publisher-adapter")]
mod font;
#[cfg(feature = "publisher-adapter")]
mod font_contract;
#[cfg(feature = "publisher-adapter")]
mod preflight;
#[cfg(feature = "publisher-adapter")]
mod production;
mod profile;
#[cfg(feature = "publisher-adapter")]
mod publication_font;
#[cfg(feature = "publisher-adapter")]
mod publisher_adapter;
#[cfg(feature = "publisher-adapter")]
mod raster;
#[cfg(feature = "publisher-adapter")]
mod scene;
#[cfg(feature = "publisher-adapter")]
mod validator;
#[cfg(feature = "pdf-oxide-validator")]
mod validator_cff;
#[cfg(feature = "pdf-oxide-validator")]
mod validator_glyphs;
#[cfg(feature = "pdf-oxide-validator")]
mod validator_icc;
#[cfg(feature = "pdf-oxide-validator")]
mod validator_metadata;
#[cfg(feature = "pdf-oxide-validator")]
mod validator_pdf_oxide;
#[cfg(feature = "pdf-oxide-validator")]
mod validator_public;

#[cfg(feature = "publisher-adapter")]
pub use candidate::{
    CandidateError, CandidatePdf, CandidatePdfWriter, MAX_TAGGED_RASTER_DIMENSION,
    MAX_TAGGED_RASTER_PIXELS, StructuralEvidence,
};
pub use candidate_metadata::PdfCandidateMetadata;
#[cfg(feature = "publisher-adapter")]
pub use color::{
    ProductionColorError, ProductionColorEvidence, ProductionColorPalette,
    prepare_publication_colors,
};
pub use color_boundary::{
    COLOR_ENGINE_IDENTITY, COLOR_RULES_IDENTITY, ColorWorkerError, TrustedColorBoundary,
};
#[cfg(feature = "publisher-adapter")]
pub use export::{
    DestinationExpectation, ExportArtifactObservation, ExportArtifactState, ExportCommitError,
    ExportCommitFailure, ExportCommitOutcome, ExportFailureState, commit_authorized_pdf,
    commit_authorized_pdf_with_report,
};
#[cfg(windows)]
#[cfg(feature = "publisher-adapter")]
pub use export::{
    InterruptedExportRecovery, InterruptedExportRecoveryOutcome, InterruptedExportState,
    InterruptedExportStatus, inspect_interrupted_export, recover_interrupted_export,
};
#[cfg(feature = "publisher-adapter")]
pub use fixture::{
    ColorFact, EvaluationKey, FixtureContract, PageBox, PageBoxes, PreflightBasis,
    RasterResolutionPreset, RasterResolutionRule, TransparencyFact,
};
#[cfg(feature = "publisher-adapter")]
pub(crate) use fixture::{ColorRenderingIntent, TaggedRgbBoundary};
#[cfg(feature = "publisher-adapter")]
pub use font::{FontBoundary, SubsetFontBoundary};
pub use font_boundary::{
    FONT_ENGINE_IDENTITY, FONT_RULES_IDENTITY, FontEmbeddingDenial, FontOutlineKind,
    FontPreparationKind, FontWorkerError, MAX_FONT_FILE_BYTES, MAX_FONT_GLYPHS_PER_SUBSET,
    PreparedFontSubset, PreparedPublicationFont, RemappedGlyph, TrustedFontBoundary,
};
#[cfg(feature = "publisher-adapter")]
pub use preflight::{
    FindingCode, FindingSeverity, PreflightFinding, PreflightReport, PublicationAuthorization,
    PublicationGate, PublicationReleaseBlocker,
};
#[cfg(feature = "publisher-adapter")]
pub use production::{
    PublicationCandidateError, PublicationCandidateEvidence, PublicationCandidateFinding,
    PublicationCandidateFindingCode, PublicationCandidateFindingSeverity, PublicationCandidatePdf,
    PublicationCandidatePreflight, PublicationCandidateWriter, PublicationColorEvidence,
    PublicationFindingFacts, PublicationFontEmbeddingState, PublicationPageEdge,
    PublicationRepairAction, PublicationRepairTarget, PublicationTextEvidence,
    PublicationWarningAcknowledgement,
};
pub use profile::{
    IccColorSpace, IccDeviceClass, OutputIntentFacts, ProfileError, ProfileHash,
    inspect_output_intent,
};
#[cfg(feature = "publisher-adapter")]
pub use publication_font::{
    ProductionFont, ProductionFontError, ProductionFontEvidence, prepare_publication_fonts,
};
#[cfg(feature = "publisher-adapter")]
pub use publisher_adapter::PublisherSceneAdapter;
#[cfg(feature = "publisher-adapter")]
pub use raster::{
    ProductionRaster, ProductionRasterError, ProductionRasterEvidence, prepare_publication_rasters,
};
#[cfg(feature = "publisher-adapter")]
pub use raster_boundary::RasterSourceProfileOrigin;
#[cfg(feature = "publisher-adapter")]
pub use scene::{
    MissingImagePolicy, ScenePdfOptions, ScenePdfWriter, SceneSerializationError,
    SceneSerializationEvidence, SerializedPageEvidence, SerializedRasterPlacementEvidence,
    SerializedScenePdf, SerializedTableCellAddress, TransparencyEvidence,
};
#[cfg(feature = "publisher-adapter")]
pub use validator::{
    ArtifactHash, IndependentValidation, IndependentValidator, PrintStandard, ValidationError,
    ValidationRequest, ValidationVerdict, ValidatorIdentity, ValidatorResponse,
    run_independent_validation,
};
#[cfg(feature = "pdf-oxide-validator")]
pub use validator_icc::{
    ACTIVE_ICC_NAME, ACTIVE_ICC_RULES_IDENTITY, ACTIVE_ICC_SOURCE_IDENTITY, ACTIVE_ICC_VERSION,
    ActiveIccProfileAudit, audit_active_icc_profiles,
};
#[cfg(feature = "pdf-oxide-validator")]
pub use validator_metadata::{PublicPdfxMetadataAudit, audit_public_pdfx_metadata};
#[cfg(feature = "pdf-oxide-validator")]
pub use validator_pdf_oxide::PdfOxideValidator;
#[cfg(feature = "pdf-oxide-validator")]
pub use validator_public::{
    PUBLIC_CORE_NAME, PUBLIC_CORE_RULES_IDENTITY, PUBLIC_CORE_SOURCE_IDENTITY, PUBLIC_CORE_VERSION,
    PublicPdfxObjectAudit, audit_public_pdfx_objects,
};

/// Application-neutral finished-page inputs, all geometry in PDF points.
pub mod finished;
mod finished_gradient;
mod finished_writer;
mod resources;
pub use finished_writer::{
    FinishedPdf, FinishedPdfWriter, MAX_FINISHED_GROUP_DEPTH, MAX_FINISHED_ITEMS,
    MAX_FINISHED_PAGES,
};
