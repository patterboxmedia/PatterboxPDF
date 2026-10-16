//! Production-shaped PDF candidate construction from an immutable scene.
//!
//! The boundary deliberately consumes `layout-core`'s resolved publication and
//! `ScenePdfWriter`'s exact traversal. It adds print structure without relayout,
//! reshaping, or silently replacing unresolved assets. The result is still only
//! a candidate: it is not accepted by [`crate::PublicationGate`].

use document_core::model::{BleedExpectation, ImageContentKind};
use layout_core::{
    ExactFontResource, FontSubstitutionStatus, PreviewBudgetState, RequestedFontBindingPreview,
    ResolvedPageScene, ResolvedPublication, ResolvedSceneObject, ResolvedSceneObjectKind,
    ResolvedTextBoxPreview, fonts::FontEmbeddingEligibility,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::PdfCandidateMetadata;
use crate::scene::{ScenePrintEnvelope, resolved_object_transparency_evidence};
use crate::{
    ArtifactHash, EvaluationKey, FixtureContract, MissingImagePolicy, ProfileError, ProfileHash,
    RasterResolutionPreset, ScenePdfOptions, ScenePdfWriter, SceneSerializationError,
    SceneSerializationEvidence, SerializedPageEvidence, SerializedRasterPlacementEvidence,
    SerializedTableCellAddress, TransparencyEvidence, inspect_output_intent,
};
use crate::{
    ProductionColorEvidence, ProductionColorPalette, ProductionFont, ProductionFontEvidence,
    ProductionRaster,
};

/// Exact scene and print facts bound to the candidate bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationCandidateEvidence {
    pub fixture_evaluation_key: EvaluationKey,
    pub publication_evaluation_key: EvaluationKey,
    pub source_scene_artifact_hash: ArtifactHash,
    pub source_project_digest: [u8; 32],
    pub scene_digest: [u8; 32],
    pub pages: Vec<SerializedPageEvidence>,
    pub pasteboard_object_count_excluded: usize,
    pub output_intent_profile_hash: ProfileHash,
    pub text: PublicationTextEvidence,
    pub embedded_fonts: Vec<ProductionFontEvidence>,
    pub transparency: TransparencyEvidence,
    pub color: PublicationColorEvidence,
    pub vector_colors: Option<ProductionColorEvidence>,
    pub raster_placements: Vec<SerializedRasterPlacementEvidence>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationTextEvidence {
    NoText,
    NativeOutlines {
        source_count: usize,
    },
    EmbeddedFonts {
        source_count: usize,
        font_count: usize,
        full_font_count: usize,
    },
    MissingOutlines {
        source_count: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationColorEvidence {
    NoPaintedColor,
    ManagedDeviceCmyk,
    UnmanagedDeviceRgb,
}

/// A deterministic, publication-shaped PDF awaiting color completion and
/// independent validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationCandidatePdf {
    bytes: Vec<u8>,
    artifact_hash: ArtifactHash,
    export_identity: ArtifactHash,
    evidence: PublicationCandidateEvidence,
}

impl PublicationCandidatePdf {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn artifact_hash(&self) -> ArtifactHash {
        self.artifact_hash
    }

    /// Identity for the exact scene, print inputs, and resulting candidate
    /// bytes. This is suitable for a later export capsule but is not permission
    /// to publish the file.
    #[must_use]
    pub const fn export_identity(&self) -> ArtifactHash {
        self.export_identity
    }

    #[must_use]
    pub const fn evidence(&self) -> &PublicationCandidateEvidence {
        &self.evidence
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationCandidateError {
    RevisionMismatch,
    SceneBasisMismatch,
    OutputProfile(ProfileError),
    OutputProfileMismatch,
    RasterOutputProfileMismatch,
    ColorOutputProfileMismatch,
    Scene(SceneSerializationError),
    SceneEvidenceDrift,
}

/// Builds a print-enveloped candidate from the authoritative ordered scene.
pub struct PublicationCandidateWriter;

impl PublicationCandidateWriter {
    /// Builds a fail-closed candidate without resolving layout or reshaping text.
    ///
    /// The base scene is first serialized without print decoration. Its exact
    /// bytes and evidence are then bound into the publication evaluation key.
    /// The same scene traversal is repeated with metadata and the exact CMYK
    /// output profile; any evidence drift aborts the operation.
    ///
    /// # Errors
    ///
    /// Returns an error before candidate bytes are exposed when the revision or
    /// output profile differs from the fixture, or when the scene contains any
    /// fact that the exact serializer cannot yet represent (including unresolved
    /// images or invalid table/text geometry).
    pub fn write(
        publication: &ResolvedPublication,
        fixture: &FixtureContract,
        output_profile: &[u8],
    ) -> Result<PublicationCandidatePdf, PublicationCandidateError> {
        Self::write_internal(publication, fixture, output_profile, &[], None, None)
    }

    /// Builds the same candidate with exact trusted page-raster resources.
    ///
    /// # Errors
    ///
    /// Fails before exposing bytes for any missing raster or evidence drift.
    pub fn write_with_rasters(
        publication: &ResolvedPublication,
        fixture: &FixtureContract,
        output_profile: &[u8],
        rasters: &[ProductionRaster],
    ) -> Result<PublicationCandidatePdf, PublicationCandidateError> {
        Self::write_internal(publication, fixture, output_profile, rasters, None, None)
    }

    /// Builds the production candidate with exact trusted rasters and
    /// scene-bound worker-prepared font subsets.
    ///
    /// # Errors
    ///
    /// Fails before exposing bytes for any missing resource, identity drift,
    /// or incomplete font coverage.
    pub fn write_with_rasters_and_fonts(
        publication: &ResolvedPublication,
        fixture: &FixtureContract,
        output_profile: &[u8],
        rasters: &[ProductionRaster],
        fonts: &[ProductionFont],
    ) -> Result<PublicationCandidatePdf, PublicationCandidateError> {
        Self::write_internal(
            publication,
            fixture,
            output_profile,
            rasters,
            Some(fonts),
            None,
        )
    }

    /// Builds a production candidate with every supported external resource and
    /// the exact worker-attested CMYK vector palette.
    ///
    /// # Errors
    ///
    /// Fails before exposing bytes for resource, profile, or scene-identity drift.
    pub fn write_with_resources(
        publication: &ResolvedPublication,
        fixture: &FixtureContract,
        output_profile: &[u8],
        rasters: &[ProductionRaster],
        fonts: Option<&[ProductionFont]>,
        colors: Option<&ProductionColorPalette>,
    ) -> Result<PublicationCandidatePdf, PublicationCandidateError> {
        Self::write_internal(publication, fixture, output_profile, rasters, fonts, colors)
    }

    fn write_internal(
        publication: &ResolvedPublication,
        fixture: &FixtureContract,
        output_profile: &[u8],
        rasters: &[ProductionRaster],
        fonts: Option<&[ProductionFont]>,
        colors: Option<&ProductionColorPalette>,
    ) -> Result<PublicationCandidatePdf, PublicationCandidateError> {
        Self::write_with_resources_and_metadata(
            publication,
            fixture,
            output_profile,
            rasters,
            fonts,
            colors,
            &PdfCandidateMetadata::validation_probe(),
        )
    }

    /// Builds a candidate with explicit title and PDF creation time. Reusing
    /// identical metadata preserves deterministic bytes; ordinary fixture APIs
    /// use a synthetic reference epoch. Saved-publication export uses this API.
    ///
    /// # Errors
    /// Fails before exposing bytes for resource, profile or scene-identity drift.
    pub fn write_with_resources_and_metadata(
        publication: &ResolvedPublication,
        fixture: &FixtureContract,
        output_profile: &[u8],
        rasters: &[ProductionRaster],
        fonts: Option<&[ProductionFont]>,
        colors: Option<&ProductionColorPalette>,
        metadata: &PdfCandidateMetadata,
    ) -> Result<PublicationCandidatePdf, PublicationCandidateError> {
        if publication.revision() != fixture.basis.revision() {
            return Err(PublicationCandidateError::RevisionMismatch);
        }
        if publication.semantic_digest() != fixture.basis.scene_digest() {
            return Err(PublicationCandidateError::SceneBasisMismatch);
        }
        // Recheck the exact bytes through the confined color worker. A prior
        // fixture cannot authorize native ICC parsing in the publishing process.
        let profile_facts = inspect_output_intent(output_profile)
            .map_err(PublicationCandidateError::OutputProfile)?;
        if profile_facts != fixture.output_intent {
            return Err(PublicationCandidateError::OutputProfileMismatch);
        }
        if rasters.iter().any(|raster| {
            raster.evidence().output_profile_hash != profile_facts.profile_hash().bytes()
        }) {
            return Err(PublicationCandidateError::RasterOutputProfileMismatch);
        }
        if colors.is_some_and(|palette| {
            palette.evidence().output_profile_hash != profile_facts.profile_hash().bytes()
        }) {
            return Err(PublicationCandidateError::ColorOutputProfileMismatch);
        }

        let options = ScenePdfOptions {
            missing_images: MissingImagePolicy::Reject,
        };
        let source = ScenePdfWriter::write_candidate_with_resources(
            publication,
            options,
            rasters,
            fonts,
            colors,
        )
        .map_err(PublicationCandidateError::Scene)?;
        let source_scene_artifact_hash = ArtifactHash::of(source.bytes());
        let publication_evaluation_key = publication_evaluation_key(
            fixture.evaluation_key(),
            source_scene_artifact_hash,
            publication,
            source.evidence(),
            profile_facts.profile_hash(),
            colors.map(ProductionColorPalette::evidence),
        );
        let envelope = ScenePrintEnvelope {
            output_profile,
            metadata,
        };
        let enveloped = ScenePdfWriter::write_candidate_print_enveloped_with_resources(
            publication,
            options,
            rasters,
            fonts,
            colors,
            envelope,
        )
        .map_err(PublicationCandidateError::Scene)?;
        if enveloped.evidence() != source.evidence() {
            return Err(PublicationCandidateError::SceneEvidenceDrift);
        }

        let bytes = enveloped.bytes().to_vec();
        let artifact_hash = ArtifactHash::of(&bytes);
        let export_identity = export_identity(publication_evaluation_key, artifact_hash);
        let scene = enveloped.evidence();
        Ok(PublicationCandidatePdf {
            bytes,
            artifact_hash,
            export_identity,
            evidence: PublicationCandidateEvidence {
                fixture_evaluation_key: fixture.evaluation_key(),
                publication_evaluation_key,
                source_scene_artifact_hash,
                source_project_digest: publication.source_project_digest(),
                scene_digest: scene.scene_digest,
                pages: scene.pages.clone(),
                pasteboard_object_count_excluded: scene.pasteboard_object_count_excluded,
                output_intent_profile_hash: profile_facts.profile_hash(),
                text: publication_text_evidence(scene),
                embedded_fonts: scene.embedded_fonts.clone(),
                transparency: scene.transparency,
                color: if scene.uses_device_rgb {
                    PublicationColorEvidence::UnmanagedDeviceRgb
                } else if colors.is_some() {
                    PublicationColorEvidence::ManagedDeviceCmyk
                } else {
                    PublicationColorEvidence::NoPaintedColor
                },
                vector_colors: colors.map(|palette| palette.evidence().clone()),
                raster_placements: scene.raster_placements.clone(),
            },
        })
    }
}

fn publication_evaluation_key(
    fixture_key: EvaluationKey,
    source_scene_hash: ArtifactHash,
    publication: &ResolvedPublication,
    scene: &SceneSerializationEvidence,
    profile_hash: ProfileHash,
    vector_colors: Option<&ProductionColorEvidence>,
) -> EvaluationKey {
    let mut digest = Sha256::new();
    digest.update(b"publisher-v2/publication-candidate-evaluation/v3\0");
    digest.update(fixture_key.bytes());
    digest.update(source_scene_hash.bytes());
    digest.update(publication.source_project_digest());
    digest.update(scene.scene_digest);
    digest.update(profile_hash.bytes());
    match vector_colors {
        Some(colors) => {
            digest.update([1]);
            digest.update(colors.scene_digest);
            digest.update(colors.source_profile_hash);
            digest.update(colors.output_profile_hash);
            digest.update(colors.raw_palette_hash);
            digest.update(colors.converted_palette_hash);
            digest.update(colors.color_engine_hash);
            digest.update(colors.color_rules_hash);
            digest.update(colors.color_worker_executable_hash);
            digest.update(colors.color_count.to_be_bytes());
        }
        None => digest.update([0]),
    }
    digest.update([
        u8::from(scene.transparency.object_opacity),
        u8::from(scene.transparency.paint_alpha),
        u8::from(scene.transparency.raster_soft_mask),
    ]);
    hash_page_evidence(&mut digest, &scene.pages);
    digest.update((scene.raster_placements.len() as u64).to_be_bytes());
    for placement in &scene.raster_placements {
        hash_string(&mut digest, &placement.page_id);
        hash_string(&mut digest, &placement.object_id);
        digest.update(placement.scene_digest);
        digest.update(placement.raster.encoded_asset_hash);
        digest.update(placement.raster.decoded_rgb_hash);
        digest.update(placement.raster.alpha_hash);
        digest.update(placement.raster.source_profile_hash);
        digest.update([match placement.raster.source_profile_origin {
            raster_boundary::RasterSourceProfileOrigin::Embedded => 0,
            raster_boundary::RasterSourceProfileOrigin::AssumedSrgb => 1,
        }]);
        digest.update(placement.raster.decode_rules_hash);
        digest.update(placement.raster.raster_worker_executable_hash);
        digest.update(placement.raster.output_profile_hash);
        digest.update(placement.raster.converted_cmyk_hash);
        digest.update(placement.raster.color_engine_hash);
        digest.update(placement.raster.color_rules_hash);
        digest.update(placement.raster.color_worker_executable_hash);
        digest.update(placement.raster.width.to_be_bytes());
        digest.update(placement.raster.height.to_be_bytes());
        digest.update([u8::from(placement.raster.has_transparency)]);
        digest.update(placement.size.width.value().to_be_bytes());
        digest.update(placement.size.height.value().to_be_bytes());
        digest.update(placement.crop.left_millionths.to_be_bytes());
        digest.update(placement.crop.top_millionths.to_be_bytes());
        digest.update(placement.crop.right_millionths.to_be_bytes());
        digest.update(placement.crop.bottom_millionths.to_be_bytes());
        digest.update(placement.transform.translation_x.value().to_be_bytes());
        digest.update(placement.transform.translation_y.value().to_be_bytes());
        digest.update(placement.transform.rotation_microdegrees.to_be_bytes());
        digest.update(placement.transform.scale_x_nanos.to_be_bytes());
        digest.update(placement.transform.scale_y_nanos.to_be_bytes());
        digest.update(placement.transform.shear_x_nanos.to_be_bytes());
        digest.update(placement.transform.shear_y_nanos.to_be_bytes());
        digest.update(placement.transform.pivot_x.value().to_be_bytes());
        digest.update(placement.transform.pivot_y.value().to_be_bytes());
    }
    digest.update((scene.embedded_fonts.len() as u64).to_be_bytes());
    for font in &scene.embedded_fonts {
        digest.update(font.scene_digest);
        digest.update(font.source_sha256);
        digest.update(font.face_index.to_be_bytes());
        digest.update(font.postscript_name_sha256);
        digest.update([match font.preparation_kind {
            font_boundary::FontPreparationKind::Subset => 0,
            font_boundary::FontPreparationKind::Full => 1,
        }]);
        digest.update(font.glyph_set_sha256);
        digest.update(font.used_glyph_count.to_be_bytes());
        digest.update(font.embedded_glyph_count.to_be_bytes());
        digest.update(font.prepared_font_sha256);
        digest.update(font.rights_identity_sha256);
        digest.update(font.worker_executable_sha256);
        digest.update(font.engine_identity_sha256);
        digest.update(font.rules_identity_sha256);
    }
    EvaluationKey::from_bytes(digest.finalize().into())
}

fn publication_text_evidence(scene: &SceneSerializationEvidence) -> PublicationTextEvidence {
    match (
        scene.text_object_count,
        scene.text_painted_as_native_outlines,
        scene.embedded_fonts.len(),
    ) {
        (0, _, _) => PublicationTextEvidence::NoText,
        (source_count, false, font_count) if font_count > 0 => {
            PublicationTextEvidence::EmbeddedFonts {
                source_count,
                font_count,
                full_font_count: scene
                    .embedded_fonts
                    .iter()
                    .filter(|font| {
                        font.preparation_kind == font_boundary::FontPreparationKind::Full
                    })
                    .count(),
            }
        }
        (source_count, true, _) => PublicationTextEvidence::NativeOutlines { source_count },
        (source_count, false, _) => PublicationTextEvidence::MissingOutlines { source_count },
    }
}

fn hash_page_evidence(digest: &mut Sha256, pages: &[SerializedPageEvidence]) {
    digest.update((pages.len() as u64).to_be_bytes());
    for page in pages {
        hash_string(digest, &page.page_id);
        for bounds in [page.media_box, page.bleed_box, page.trim_box] {
            for value in [
                bounds.x.value(),
                bounds.y.value(),
                bounds.width.value(),
                bounds.height.value(),
            ] {
                digest.update(value.to_be_bytes());
            }
        }
        digest.update((page.object_ids_in_paint_order.len() as u64).to_be_bytes());
        for object_id in &page.object_ids_in_paint_order {
            hash_string(digest, object_id);
        }
        digest.update((page.placeholder_image_ids.len() as u64).to_be_bytes());
        for object_id in &page.placeholder_image_ids {
            hash_string(digest, object_id);
        }
        digest.update((page.clipped_table_cells.len() as u64).to_be_bytes());
        for cell in &page.clipped_table_cells {
            hash_string(digest, &cell.object_id);
            digest.update(cell.row.to_be_bytes());
            digest.update(cell.column.to_be_bytes());
        }
        for expectation in [
            page.bleed_expectations.top,
            page.bleed_expectations.right,
            page.bleed_expectations.bottom,
            page.bleed_expectations.left,
        ] {
            digest.update([bleed_expectation_tag(expectation)]);
        }
    }
}

fn hash_string(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

const fn bleed_expectation_tag(expectation: BleedExpectation) -> u8 {
    match expectation {
        BleedExpectation::Auto => 0,
        BleedExpectation::Required => 1,
        BleedExpectation::NotIntended => 2,
    }
}

fn export_identity(evaluation_key: EvaluationKey, artifact_hash: ArtifactHash) -> ArtifactHash {
    let mut digest = Sha256::new();
    digest.update(b"publisher-v2/publication-candidate-export-identity/v1\0");
    digest.update(evaluation_key.bytes());
    digest.update(artifact_hash.bytes());
    ArtifactHash::from_bytes(digest.finalize().into())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationCandidateFindingCode {
    CandidateBytesChanged,
    FixtureEvaluationMismatch,
    SourceProjectMismatch,
    SceneDigestMismatch,
    PageEvidenceMismatch,
    OutputIntentProfileMismatch,
    PdfxStructureMissing,
    UnmanagedDeviceRgb,
    TextOutlineEvidenceMissing,
    PreviewBudgetIncomplete,
    TextOverset,
    TableCellOverset,
    FontEmbeddingUnresolved,
    FontEmbeddingPermissionBlocked,
    FullFontEmbeddingRequired,
    ImageResolutionBelowPreset,
    ImageResolutionBelowMinimum,
    RequiredBleedMissing,
    PossibleBleedMissing,
    TransparencyEvidenceMismatch,
    IndependentValidationRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationCandidateFindingSeverity {
    Blocker,
    Warning,
    Information,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PublicationPageEdge {
    Top,
    Right,
    Bottom,
    Left,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationRepairAction {
    ResolveTextOverflow,
    ResolveTableCellOverflow { row: u16, column: u16 },
    PrepareEmbeddableFont,
    ReplaceFont,
    EmbedFullFont,
    ReducePlacedImageSize,
    ReplaceImageOrReducePlacedSize,
    ExtendObjectToBleed { edge: PublicationPageEdge },
    ReviewBleedIntent { edge: PublicationPageEdge },
    RebuildResolvedPreview,
    ReviewTransparency,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationRepairTarget {
    pub page_id: String,
    pub object_id: Option<String>,
    pub action: PublicationRepairAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationFindingFacts {
    None,
    ImageResolution {
        content_kind: ImageContentKind,
        effective_ppi_milli: u32,
        target_ppi: u16,
        minimum_ppi: u16,
    },
    BleedEdge {
        edge: PublicationPageEdge,
    },
    TableCell {
        row: u16,
        column: u16,
    },
    FontEmbedding {
        state: PublicationFontEmbeddingState,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationFontEmbeddingState {
    SubsetAllowed,
    FullFontRequired,
    Restricted,
    BitmapOnly,
    Unknown,
    ExactBytesMissing,
    Substituted,
    Unbound,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationCandidateFinding {
    pub severity: PublicationCandidateFindingSeverity,
    pub code: PublicationCandidateFindingCode,
    pub message: &'static str,
    pub target: Option<PublicationRepairTarget>,
    pub facts: PublicationFindingFacts,
}

/// Preflight evidence for a real publication candidate. This intentionally
/// cannot be passed to `PublicationGate`; independent validation and the trusted
/// final handoff remain separate unfinished boundaries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationCandidatePreflight {
    pub evaluation_key: EvaluationKey,
    pub artifact_hash: ArtifactHash,
    pub export_identity: ArtifactHash,
    pub profile_hash: ProfileHash,
    pub findings: Vec<PublicationCandidateFinding>,
}

/// Opaque acknowledgement of every warning in one exact preflight result.
///
/// Any change to the evaluated inputs, candidate bytes, export capsule, or
/// warning details invalidates the acknowledgement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationWarningAcknowledgement {
    evaluation_key: EvaluationKey,
    artifact_hash: ArtifactHash,
    export_identity: ArtifactHash,
    warnings_hash: ArtifactHash,
}

impl PublicationWarningAcknowledgement {
    /// Records acknowledgement only for a blocker-free report that actually
    /// contains warnings. A clean report needs no acknowledgement.
    #[must_use]
    pub fn for_report(report: &PublicationCandidatePreflight) -> Option<Self> {
        (!report.has_blockers() && report.has_warnings()).then(|| Self {
            evaluation_key: report.evaluation_key,
            artifact_hash: report.artifact_hash,
            export_identity: report.export_identity,
            warnings_hash: warning_set_hash(&report.findings),
        })
    }

    #[must_use]
    pub fn matches(self, report: &PublicationCandidatePreflight) -> bool {
        report.has_warnings()
            && self.evaluation_key == report.evaluation_key
            && self.artifact_hash == report.artifact_hash
            && self.export_identity == report.export_identity
            && self.warnings_hash == warning_set_hash(&report.findings)
    }
}

impl PublicationCandidatePreflight {
    #[must_use]
    pub fn for_candidate(
        publication: &ResolvedPublication,
        fixture: &FixtureContract,
        candidate: &PublicationCandidatePdf,
    ) -> Self {
        Self::for_candidate_with_font_resources(publication, fixture, candidate, &[])
    }

    #[must_use]
    pub fn for_candidate_with_font_resources(
        publication: &ResolvedPublication,
        fixture: &FixtureContract,
        candidate: &PublicationCandidatePdf,
        font_resources: &[ExactFontResource],
    ) -> Self {
        let evidence = candidate.evidence();
        let mut findings = Vec::new();
        if ArtifactHash::of(candidate.bytes()) != candidate.artifact_hash() {
            findings.push(blocking(
                PublicationCandidateFindingCode::CandidateBytesChanged,
                "Candidate bytes no longer match their frozen artifact identity.",
            ));
        }
        let fixture_matches = evidence.fixture_evaluation_key == fixture.evaluation_key();
        if !fixture_matches {
            findings.push(blocking(
                PublicationCandidateFindingCode::FixtureEvaluationMismatch,
                "Candidate print inputs belong to a different fixture evaluation.",
            ));
        }
        let scene_matches = evidence.scene_digest == publication.semantic_digest();
        if !scene_matches {
            findings.push(blocking(
                PublicationCandidateFindingCode::SceneDigestMismatch,
                "Candidate scene identity differs from the resolved publication.",
            ));
        }
        let source_project_matches =
            evidence.source_project_digest == publication.source_project_digest();
        if !source_project_matches {
            findings.push(blocking(
                PublicationCandidateFindingCode::SourceProjectMismatch,
                "Candidate source-project identity differs from the resolved publication.",
            ));
        }
        let page_facts_match = evidence.pasteboard_object_count_excluded
            == publication.pasteboard().objects.len()
            && page_evidence_matches(publication, &evidence.pages);
        if !page_facts_match {
            findings.push(blocking(
                PublicationCandidateFindingCode::PageEvidenceMismatch,
                "Candidate page order, boxes, or paint order differs from the resolved publication.",
            ));
        }
        let output_intent_matches = evidence.output_intent_profile_hash == fixture.profile_hash();
        if !output_intent_matches {
            findings.push(blocking(
                PublicationCandidateFindingCode::OutputIntentProfileMismatch,
                "Candidate output intent differs from the evaluated profile.",
            ));
        }
        if !contains_print_structure(candidate.bytes()) {
            findings.push(blocking(
                PublicationCandidateFindingCode::PdfxStructureMissing,
                "Candidate lacks the required PDF/X metadata or output-intent structure.",
            ));
        }
        if evidence.color == PublicationColorEvidence::UnmanagedDeviceRgb {
            findings.push(blocking(
                PublicationCandidateFindingCode::UnmanagedDeviceRgb,
                "Scene colors are still emitted as unmanaged DeviceRGB and require the trusted color boundary.",
            ));
        }
        if matches!(
            evidence.text,
            PublicationTextEvidence::MissingOutlines { .. }
        ) {
            findings.push(blocking(
                PublicationCandidateFindingCode::TextOutlineEvidenceMissing,
                "Text exists but the candidate has no native-outline painting evidence.",
            ));
        }
        if fixture_matches
            && scene_matches
            && source_project_matches
            && page_facts_match
            && output_intent_matches
        {
            collect_authoring_findings(
                publication,
                candidate,
                font_resources,
                fixture.raster_resolution,
                &mut findings,
            );
        }
        findings.push(information(
            PublicationCandidateFindingCode::IndependentValidationRequired,
            "An independent PDF/X-4 validator must inspect these exact candidate bytes.",
        ));
        Self {
            evaluation_key: evidence.publication_evaluation_key,
            artifact_hash: candidate.artifact_hash(),
            export_identity: candidate.export_identity(),
            profile_hash: evidence.output_intent_profile_hash,
            findings,
        }
    }

    #[must_use]
    pub fn has_blockers(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.severity == PublicationCandidateFindingSeverity::Blocker)
    }

    #[must_use]
    pub fn has_warnings(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.severity == PublicationCandidateFindingSeverity::Warning)
    }

    #[must_use]
    pub fn warnings_acknowledged(
        &self,
        acknowledgement: Option<&PublicationWarningAcknowledgement>,
    ) -> bool {
        !self.has_warnings() || acknowledgement.is_some_and(|value| value.matches(self))
    }

    #[must_use]
    pub fn ready_for_independent_validation(&self) -> bool {
        !self.has_blockers()
    }
}

fn warning_set_hash(findings: &[PublicationCandidateFinding]) -> ArtifactHash {
    let mut digest = Sha256::new();
    digest.update(b"publisher-v2/publication-warning-set/v1\0");
    let warnings = findings
        .iter()
        .filter(|finding| finding.severity == PublicationCandidateFindingSeverity::Warning)
        .collect::<Vec<_>>();
    digest.update(
        u64::try_from(warnings.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for finding in warnings {
        digest.update(finding_code_tag(finding.code).to_be_bytes());
        digest_string(&mut digest, finding.message);
        digest_target(&mut digest, finding.target.as_ref());
        digest_facts(&mut digest, finding.facts);
    }
    ArtifactHash::from_bytes(digest.finalize().into())
}

fn digest_target(digest: &mut Sha256, target: Option<&PublicationRepairTarget>) {
    let Some(target) = target else {
        digest.update([0]);
        return;
    };
    digest.update([1]);
    digest_string(digest, &target.page_id);
    match &target.object_id {
        Some(object_id) => {
            digest.update([1]);
            digest_string(digest, object_id);
        }
        None => digest.update([0]),
    }
    digest_repair_action(digest, target.action);
}

fn digest_repair_action(digest: &mut Sha256, action: PublicationRepairAction) {
    match action {
        PublicationRepairAction::ResolveTextOverflow => digest.update([0]),
        PublicationRepairAction::ResolveTableCellOverflow { row, column } => {
            digest.update([1]);
            digest.update(row.to_be_bytes());
            digest.update(column.to_be_bytes());
        }
        PublicationRepairAction::PrepareEmbeddableFont => digest.update([2]),
        PublicationRepairAction::ReplaceFont => digest.update([3]),
        PublicationRepairAction::EmbedFullFont => digest.update([4]),
        PublicationRepairAction::ReducePlacedImageSize => digest.update([5]),
        PublicationRepairAction::ReplaceImageOrReducePlacedSize => digest.update([10]),
        PublicationRepairAction::ExtendObjectToBleed { edge } => {
            digest.update([6]);
            digest.update([page_edge_tag(edge)]);
        }
        PublicationRepairAction::ReviewBleedIntent { edge } => {
            digest.update([7]);
            digest.update([page_edge_tag(edge)]);
        }
        PublicationRepairAction::RebuildResolvedPreview => digest.update([8]),
        PublicationRepairAction::ReviewTransparency => digest.update([9]),
    }
}

fn digest_facts(digest: &mut Sha256, facts: PublicationFindingFacts) {
    match facts {
        PublicationFindingFacts::None => digest.update([0]),
        PublicationFindingFacts::ImageResolution {
            content_kind,
            effective_ppi_milli,
            target_ppi,
            minimum_ppi,
        } => {
            digest.update([1]);
            digest.update([image_content_kind_tag(content_kind)]);
            digest.update(effective_ppi_milli.to_be_bytes());
            digest.update(target_ppi.to_be_bytes());
            digest.update(minimum_ppi.to_be_bytes());
        }
        PublicationFindingFacts::BleedEdge { edge } => {
            digest.update([2]);
            digest.update([page_edge_tag(edge)]);
        }
        PublicationFindingFacts::TableCell { row, column } => {
            digest.update([3]);
            digest.update(row.to_be_bytes());
            digest.update(column.to_be_bytes());
        }
        PublicationFindingFacts::FontEmbedding { state } => {
            digest.update([4]);
            digest.update([font_embedding_state_tag(state)]);
        }
    }
}

fn digest_string(digest: &mut Sha256, value: &str) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(value.as_bytes());
}

const fn page_edge_tag(edge: PublicationPageEdge) -> u8 {
    match edge {
        PublicationPageEdge::Top => 0,
        PublicationPageEdge::Right => 1,
        PublicationPageEdge::Bottom => 2,
        PublicationPageEdge::Left => 3,
    }
}

const fn finding_code_tag(code: PublicationCandidateFindingCode) -> u16 {
    match code {
        PublicationCandidateFindingCode::CandidateBytesChanged => 0,
        PublicationCandidateFindingCode::FixtureEvaluationMismatch => 1,
        PublicationCandidateFindingCode::SourceProjectMismatch => 2,
        PublicationCandidateFindingCode::SceneDigestMismatch => 3,
        PublicationCandidateFindingCode::PageEvidenceMismatch => 4,
        PublicationCandidateFindingCode::OutputIntentProfileMismatch => 5,
        PublicationCandidateFindingCode::PdfxStructureMissing => 6,
        PublicationCandidateFindingCode::UnmanagedDeviceRgb => 7,
        PublicationCandidateFindingCode::TextOutlineEvidenceMissing => 8,
        PublicationCandidateFindingCode::PreviewBudgetIncomplete => 9,
        PublicationCandidateFindingCode::TextOverset => 10,
        PublicationCandidateFindingCode::TableCellOverset => 11,
        PublicationCandidateFindingCode::FontEmbeddingUnresolved => 12,
        PublicationCandidateFindingCode::FontEmbeddingPermissionBlocked => 13,
        PublicationCandidateFindingCode::FullFontEmbeddingRequired => 14,
        PublicationCandidateFindingCode::ImageResolutionBelowPreset => 15,
        PublicationCandidateFindingCode::ImageResolutionBelowMinimum => 20,
        PublicationCandidateFindingCode::RequiredBleedMissing => 16,
        PublicationCandidateFindingCode::PossibleBleedMissing => 17,
        PublicationCandidateFindingCode::TransparencyEvidenceMismatch => 18,
        PublicationCandidateFindingCode::IndependentValidationRequired => 19,
    }
}

const fn image_content_kind_tag(kind: ImageContentKind) -> u8 {
    match kind {
        ImageContentKind::ContinuousTone => 0,
        ImageContentKind::LineArt => 1,
    }
}

const fn font_embedding_state_tag(state: PublicationFontEmbeddingState) -> u8 {
    match state {
        PublicationFontEmbeddingState::SubsetAllowed => 0,
        PublicationFontEmbeddingState::FullFontRequired => 1,
        PublicationFontEmbeddingState::Restricted => 2,
        PublicationFontEmbeddingState::BitmapOnly => 3,
        PublicationFontEmbeddingState::Unknown => 4,
        PublicationFontEmbeddingState::ExactBytesMissing => 5,
        PublicationFontEmbeddingState::Substituted => 6,
        PublicationFontEmbeddingState::Unbound => 7,
    }
}

#[derive(Clone, Default)]
struct BleedEdgeState {
    reaches_bleed: bool,
    first_shortfall_object_id: Option<String>,
}

fn collect_authoring_findings(
    publication: &ResolvedPublication,
    candidate: &PublicationCandidatePdf,
    font_resources: &[ExactFontResource],
    raster_resolution: RasterResolutionPreset,
    findings: &mut Vec<PublicationCandidateFinding>,
) {
    if publication.budget_state() != PreviewBudgetState::Complete {
        let target = publication
            .pages()
            .first()
            .map(|page| PublicationRepairTarget {
                page_id: page.page_id.as_str().to_owned(),
                object_id: None,
                action: PublicationRepairAction::RebuildResolvedPreview,
            });
        findings.push(targeted_blocker(
            PublicationCandidateFindingCode::PreviewBudgetIncomplete,
            "The resolved publication omitted text because its preview budget was exceeded.",
            target,
            PublicationFindingFacts::None,
        ));
    }

    let raster_placements = candidate
        .evidence()
        .raster_placements
        .iter()
        .map(|placement| {
            (
                (placement.page_id.as_str(), placement.object_id.as_str()),
                placement,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut transparency_expected = TransparencyEvidence {
        raster_soft_mask: raster_placements
            .values()
            .any(|placement| placement.raster.has_transparency),
        ..TransparencyEvidence::default()
    };
    let font_resources = font_resources
        .iter()
        .map(|resource| {
            (
                (resource.source.as_str(), resource.face_index),
                resource.bytes.as_slice(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let embedded_fonts = candidate
        .evidence()
        .embedded_fonts
        .iter()
        .map(|font| (hex_sha256(font.source_sha256), font.face_index))
        .collect::<BTreeSet<_>>();

    for page in publication.pages() {
        let mut bleed_state = std::array::from_fn(|_| BleedEdgeState::default());
        for object in &page.objects {
            collect_object_findings(
                page,
                object,
                &raster_placements,
                &font_resources,
                &embedded_fonts,
                raster_resolution,
                findings,
            );
            let object_transparency = resolved_object_transparency_evidence(object);
            transparency_expected.object_opacity |= object_transparency.object_opacity;
            transparency_expected.paint_alpha |= object_transparency.paint_alpha;
            collect_bleed_findings(page, object, &mut bleed_state);
        }
        collect_uncovered_bleed(page, &bleed_state, findings);
    }

    if transparency_expected != candidate.evidence().transparency {
        findings.push(targeted_blocker(
            PublicationCandidateFindingCode::TransparencyEvidenceMismatch,
            "Candidate transparency evidence does not match the resolved publication.",
            publication
                .pages()
                .first()
                .map(|page| PublicationRepairTarget {
                    page_id: page.page_id.as_str().to_owned(),
                    object_id: None,
                    action: PublicationRepairAction::ReviewTransparency,
                }),
            PublicationFindingFacts::None,
        ));
    }
}

fn collect_object_findings(
    page: &ResolvedPageScene,
    object: &ResolvedSceneObject,
    raster_placements: &BTreeMap<(&str, &str), &SerializedRasterPlacementEvidence>,
    font_resources: &BTreeMap<(&str, u32), &[u8]>,
    embedded_fonts: &BTreeSet<(String, u32)>,
    raster_resolution: RasterResolutionPreset,
    findings: &mut Vec<PublicationCandidateFinding>,
) {
    let target = |action| PublicationRepairTarget {
        page_id: page.page_id.as_str().to_owned(),
        object_id: Some(object.object_id.as_str().to_owned()),
        action,
    };
    match &object.kind {
        ResolvedSceneObjectKind::Text { layout, .. } => {
            if layout.overset {
                findings.push(targeted_blocker(
                    PublicationCandidateFindingCode::TextOverset,
                    "Text continues beyond the visible text box.",
                    Some(target(PublicationRepairAction::ResolveTextOverflow)),
                    PublicationFindingFacts::None,
                ));
            }
            if !layout.lines.is_empty() || layout.overset {
                collect_text_font_findings(
                    page,
                    object,
                    layout,
                    font_resources,
                    embedded_fonts,
                    findings,
                );
            }
        }
        ResolvedSceneObjectKind::Table { table } => {
            for cell in table
                .populated_cells
                .iter()
                .filter(|cell| cell.clipped || cell.layout.overset)
            {
                findings.push(targeted_blocker(
                    PublicationCandidateFindingCode::TableCellOverset,
                    "Text does not fit inside this table cell.",
                    Some(target(PublicationRepairAction::ResolveTableCellOverflow {
                        row: cell.row,
                        column: cell.column,
                    })),
                    PublicationFindingFacts::TableCell {
                        row: cell.row,
                        column: cell.column,
                    },
                ));
            }
            if let Some(cell) = table
                .populated_cells
                .iter()
                .find(|cell| !binding_is_embedded(&cell.layout.font_binding, embedded_fonts))
            {
                findings.push(font_embedding_finding(
                    page,
                    object,
                    &cell.layout.font_binding,
                    font_resources,
                ));
            }
        }
        ResolvedSceneObjectKind::Image { content_kind, .. } => {
            if let Some(placement) =
                raster_placements.get(&(page.page_id.as_str(), object.object_id.as_str()))
                && let Some(effective_ppi_milli) = effective_ppi_milli(placement)
            {
                let rule = match content_kind {
                    ImageContentKind::ContinuousTone => raster_resolution.continuous_tone(),
                    ImageContentKind::LineArt => raster_resolution.line_art(),
                };
                if effective_ppi_milli < u32::from(rule.minimum_ppi()) * 1_000 {
                    findings.push(PublicationCandidateFinding {
                        severity: PublicationCandidateFindingSeverity::Blocker,
                        code: PublicationCandidateFindingCode::ImageResolutionBelowMinimum,
                        message: "Placed image resolution is below the selected print preset's minimum.",
                        target: Some(target(
                            PublicationRepairAction::ReplaceImageOrReducePlacedSize,
                        )),
                        facts: PublicationFindingFacts::ImageResolution {
                            content_kind: *content_kind,
                            effective_ppi_milli,
                            target_ppi: rule.target_ppi(),
                            minimum_ppi: rule.minimum_ppi(),
                        },
                    });
                } else if effective_ppi_milli < u32::from(rule.target_ppi()) * 1_000 {
                    findings.push(PublicationCandidateFinding {
                        severity: PublicationCandidateFindingSeverity::Warning,
                        code: PublicationCandidateFindingCode::ImageResolutionBelowPreset,
                        message: "Placed image resolution is below the selected print preset's target.",
                        target: Some(target(PublicationRepairAction::ReducePlacedImageSize)),
                        facts: PublicationFindingFacts::ImageResolution {
                            content_kind: *content_kind,
                            effective_ppi_milli,
                            target_ppi: rule.target_ppi(),
                            minimum_ppi: rule.minimum_ppi(),
                        },
                    });
                }
            }
        }
        ResolvedSceneObjectKind::Shape { .. } => {}
    }
}

fn collect_text_font_findings(
    page: &ResolvedPageScene,
    object: &ResolvedSceneObject,
    layout: &ResolvedTextBoxPreview,
    font_resources: &BTreeMap<(&str, u32), &[u8]>,
    embedded_fonts: &BTreeSet<(String, u32)>,
    findings: &mut Vec<PublicationCandidateFinding>,
) {
    for binding in distinct_text_font_bindings(layout)
        .iter()
        .filter(|binding| !binding_is_embedded(binding, embedded_fonts))
    {
        findings.push(font_embedding_finding(
            page,
            object,
            binding,
            font_resources,
        ));
    }
}

fn distinct_text_font_bindings(
    layout: &ResolvedTextBoxPreview,
) -> Vec<RequestedFontBindingPreview> {
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for binding in &layout.font_bindings {
        let key = (
            binding.requested_family.clone(),
            binding
                .exact_face
                .as_ref()
                .map(|face| face.sha256_hex.clone()),
            binding
                .exact_face
                .as_ref()
                .map(|face| face.collection_index),
            font_substitution_status_tag(binding.substitution_status),
        );
        if seen.insert(key) {
            result.push(RequestedFontBindingPreview {
                requested_family: binding.requested_family.clone(),
                exact_face: binding.exact_face.clone(),
                substitution_status: binding.substitution_status,
            });
        }
    }
    let has_list_marker = layout
        .lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .any(|run| run.list_marker_paragraph.is_some());
    if (layout.font_bindings.is_empty() || has_list_marker)
        && !result.contains(&layout.font_binding)
    {
        result.push(layout.font_binding.clone());
    }
    result
}

const fn font_substitution_status_tag(status: FontSubstitutionStatus) -> u8 {
    match status {
        FontSubstitutionStatus::Exact => 0,
        FontSubstitutionStatus::Substituted => 1,
        FontSubstitutionStatus::Unbound => 2,
        FontSubstitutionStatus::Missing => 3,
    }
}

fn binding_is_embedded(
    binding: &layout_core::RequestedFontBindingPreview,
    embedded_fonts: &BTreeSet<(String, u32)>,
) -> bool {
    binding.substitution_status == FontSubstitutionStatus::Exact
        && binding.exact_face.as_ref().is_some_and(|face| {
            embedded_fonts.contains(&(face.sha256_hex.clone(), face.collection_index))
        })
}

fn hex_sha256(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

fn font_embedding_finding(
    page: &ResolvedPageScene,
    object: &ResolvedSceneObject,
    binding: &layout_core::RequestedFontBindingPreview,
    resources: &BTreeMap<(&str, u32), &[u8]>,
) -> PublicationCandidateFinding {
    let (code, message, action, state) = match binding.substitution_status {
        FontSubstitutionStatus::Exact => match binding.exact_face.as_ref().and_then(|face| {
            resources
                .get(&(face.sha256_hex.as_str(), face.collection_index))
                .map(|bytes| layout_core::fonts::inspect_font_face(bytes, face.collection_index))
        }) {
            Some(facts) => match facts.embedding_eligibility() {
                FontEmbeddingEligibility::SubsetAllowed { .. } => (
                    PublicationCandidateFindingCode::FontEmbeddingUnresolved,
                    "This permitted exact font is not yet embedded in the publication PDF.",
                    PublicationRepairAction::PrepareEmbeddableFont,
                    PublicationFontEmbeddingState::SubsetAllowed,
                ),
                FontEmbeddingEligibility::FullFontRequired { .. } => (
                    PublicationCandidateFindingCode::FullFontEmbeddingRequired,
                    "This font permits embedding only when the complete font is included.",
                    PublicationRepairAction::EmbedFullFont,
                    PublicationFontEmbeddingState::FullFontRequired,
                ),
                FontEmbeddingEligibility::Restricted { .. } => (
                    PublicationCandidateFindingCode::FontEmbeddingPermissionBlocked,
                    "This font explicitly prohibits document embedding.",
                    PublicationRepairAction::ReplaceFont,
                    PublicationFontEmbeddingState::Restricted,
                ),
                FontEmbeddingEligibility::BitmapOnly { .. } => (
                    PublicationCandidateFindingCode::FontEmbeddingPermissionBlocked,
                    "This font permits bitmap embedding but not the required outlines.",
                    PublicationRepairAction::ReplaceFont,
                    PublicationFontEmbeddingState::BitmapOnly,
                ),
                FontEmbeddingEligibility::Unknown { .. } => (
                    PublicationCandidateFindingCode::FontEmbeddingPermissionBlocked,
                    "Embedding permission cannot be established for this exact font.",
                    PublicationRepairAction::ReplaceFont,
                    PublicationFontEmbeddingState::Unknown,
                ),
            },
            None => (
                PublicationCandidateFindingCode::FontEmbeddingUnresolved,
                "The exact font bytes needed for export are unavailable.",
                PublicationRepairAction::ReplaceFont,
                PublicationFontEmbeddingState::ExactBytesMissing,
            ),
        },
        FontSubstitutionStatus::Substituted => (
            PublicationCandidateFindingCode::FontEmbeddingUnresolved,
            "Text uses a substituted font and cannot be exported as compliant.",
            PublicationRepairAction::ReplaceFont,
            PublicationFontEmbeddingState::Substituted,
        ),
        FontSubstitutionStatus::Unbound => (
            PublicationCandidateFindingCode::FontEmbeddingUnresolved,
            "Text is not bound to an exact embeddable font face.",
            PublicationRepairAction::ReplaceFont,
            PublicationFontEmbeddingState::Unbound,
        ),
        FontSubstitutionStatus::Missing => (
            PublicationCandidateFindingCode::FontEmbeddingUnresolved,
            "The exact font required by this text is unavailable.",
            PublicationRepairAction::ReplaceFont,
            PublicationFontEmbeddingState::ExactBytesMissing,
        ),
    };
    targeted_blocker(
        code,
        message,
        Some(PublicationRepairTarget {
            page_id: page.page_id.as_str().to_owned(),
            object_id: Some(object.object_id.as_str().to_owned()),
            action,
        }),
        PublicationFindingFacts::FontEmbedding { state },
    )
}

fn collect_bleed_findings(
    page: &ResolvedPageScene,
    object: &ResolvedSceneObject,
    state: &mut [BleedEdgeState; 4],
) {
    let Ok(regions) = object.paint_regions() else {
        return;
    };
    let trim_right = page.trim_box.width.value();
    let trim_bottom = page.trim_box.height.value();
    let bleed_right = page
        .bleed_box
        .x
        .value()
        .saturating_add(page.bleed_box.width.value());
    let bleed_bottom = page
        .bleed_box
        .y
        .value()
        .saturating_add(page.bleed_box.height.value());
    for bounds in regions {
        let Some(right) = bounds.x.value().checked_add(bounds.width.value()) else {
            continue;
        };
        let Some(bottom) = bounds.y.value().checked_add(bounds.height.value()) else {
            continue;
        };
        let edges = [
            (
                bounds.y.value() <= 0
                    && bottom >= 0
                    && intervals_overlap(bounds.x.value(), right, 0, trim_right),
                page.bleed_box.y.value() < 0 && bounds.y.value() <= page.bleed_box.y.value(),
                page.bleed_expectations.top,
            ),
            (
                bounds.x.value() <= trim_right
                    && right >= trim_right
                    && intervals_overlap(bounds.y.value(), bottom, 0, trim_bottom),
                bleed_right > trim_right && right >= bleed_right,
                page.bleed_expectations.right,
            ),
            (
                bounds.y.value() <= trim_bottom
                    && bottom >= trim_bottom
                    && intervals_overlap(bounds.x.value(), right, 0, trim_right),
                bleed_bottom > trim_bottom && bottom >= bleed_bottom,
                page.bleed_expectations.bottom,
            ),
            (
                bounds.x.value() <= 0
                    && right >= 0
                    && intervals_overlap(bounds.y.value(), bottom, 0, trim_bottom),
                page.bleed_box.x.value() < 0 && bounds.x.value() <= page.bleed_box.x.value(),
                page.bleed_expectations.left,
            ),
        ];
        for (index, (touches, reaches, expectation)) in edges.into_iter().enumerate() {
            if !touches {
                continue;
            }
            state[index].reaches_bleed |= reaches;
            if !reaches && expectation != BleedExpectation::NotIntended {
                state[index]
                    .first_shortfall_object_id
                    .get_or_insert_with(|| object.object_id.as_str().to_owned());
            }
        }
    }
}

const fn intervals_overlap(
    first_start: i64,
    first_end: i64,
    second_start: i64,
    second_end: i64,
) -> bool {
    first_start <= second_end && first_end >= second_start
}

fn collect_uncovered_bleed(
    page: &ResolvedPageScene,
    state: &[BleedEdgeState; 4],
    findings: &mut Vec<PublicationCandidateFinding>,
) {
    for (index, (edge, expectation)) in [
        (PublicationPageEdge::Top, page.bleed_expectations.top),
        (PublicationPageEdge::Right, page.bleed_expectations.right),
        (PublicationPageEdge::Bottom, page.bleed_expectations.bottom),
        (PublicationPageEdge::Left, page.bleed_expectations.left),
    ]
    .into_iter()
    .enumerate()
    {
        if expectation == BleedExpectation::NotIntended || state[index].reaches_bleed {
            continue;
        }
        let object_id = state[index].first_shortfall_object_id.clone();
        if expectation == BleedExpectation::Auto {
            let Some(object_id) = object_id else {
                continue;
            };
            findings.push(PublicationCandidateFinding {
                severity: PublicationCandidateFindingSeverity::Warning,
                code: PublicationCandidateFindingCode::PossibleBleedMissing,
                message: "An object touches the trim edge but may need to extend through bleed.",
                target: Some(PublicationRepairTarget {
                    page_id: page.page_id.as_str().to_owned(),
                    object_id: Some(object_id),
                    action: PublicationRepairAction::ReviewBleedIntent { edge },
                }),
                facts: PublicationFindingFacts::BleedEdge { edge },
            });
            continue;
        }
        let message = if object_id.is_some() {
            "Artwork touching this required trim edge does not extend through bleed."
        } else {
            "This required page edge has no artwork extending through bleed."
        };
        findings.push(targeted_blocker(
            PublicationCandidateFindingCode::RequiredBleedMissing,
            message,
            Some(PublicationRepairTarget {
                page_id: page.page_id.as_str().to_owned(),
                object_id,
                action: PublicationRepairAction::ExtendObjectToBleed { edge },
            }),
            PublicationFindingFacts::BleedEdge { edge },
        ));
    }
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounded fixed-point transform and raster dimensions are converted only to calculate a conservative diagnostic PPI value"
)]
fn effective_ppi_milli(placement: &SerializedRasterPlacementEvidence) -> Option<u32> {
    let scale_x = (placement.transform.scale_x_nanos as f64
        / document_core::model::Transform::IDENTITY_SCALE as f64)
        .abs();
    let scale_y = (placement.transform.scale_y_nanos as f64
        / document_core::model::Transform::IDENTITY_SCALE as f64)
        .abs();
    let shear_x = placement.transform.shear_x_nanos as f64
        / document_core::model::Transform::IDENTITY_SCALE as f64;
    let shear_y = placement.transform.shear_y_nanos as f64
        / document_core::model::Transform::IDENTITY_SCALE as f64;
    let placed_width_um =
        placement.size.width.value() as f64 * scale_x * (1.0 + shear_y * shear_y).sqrt();
    let placed_height_um =
        placement.size.height.value() as f64 * scale_y * (1.0 + shear_x * shear_x).sqrt();
    if placed_width_um <= 0.0 || placed_height_um <= 0.0 {
        return None;
    }
    let visible_width =
        f64::from(1_000_000 - placement.crop.left_millionths - placement.crop.right_millionths)
            / 1_000_000.0;
    let visible_height =
        f64::from(1_000_000 - placement.crop.top_millionths - placement.crop.bottom_millionths)
            / 1_000_000.0;
    let horizontal = f64::from(placement.raster.width) * visible_width * 25_400.0 / placed_width_um;
    let vertical =
        f64::from(placement.raster.height) * visible_height * 25_400.0 / placed_height_um;
    let milli = horizontal.min(vertical) * 1_000.0;
    (milli.is_finite() && milli >= 0.0 && milli <= f64::from(u32::MAX))
        .then_some(milli.floor() as u32)
}

fn targeted_blocker(
    code: PublicationCandidateFindingCode,
    message: &'static str,
    target: Option<PublicationRepairTarget>,
    facts: PublicationFindingFacts,
) -> PublicationCandidateFinding {
    PublicationCandidateFinding {
        severity: PublicationCandidateFindingSeverity::Blocker,
        code,
        message,
        target,
        facts,
    }
}

fn contains_print_structure(bytes: &[u8]) -> bool {
    [
        b"/OutputIntents".as_slice(),
        b"/GTS_PDFXVersion".as_slice(),
        b"/Metadata".as_slice(),
        b"/DestOutputProfile".as_slice(),
    ]
    .iter()
    .all(|needle| bytes.windows(needle.len()).any(|window| window == *needle))
}

fn page_evidence_matches(
    publication: &ResolvedPublication,
    evidence: &[SerializedPageEvidence],
) -> bool {
    publication.pages().len() == evidence.len()
        && publication
            .pages()
            .iter()
            .zip(evidence)
            .all(|(page, observed)| {
                let expected_clipped_cells = page
                    .objects
                    .iter()
                    .filter_map(|object| match &object.kind {
                        ResolvedSceneObjectKind::Table { table } => Some(
                            table
                                .populated_cells
                                .iter()
                                .filter(|cell| cell.clipped)
                                .map(|cell| SerializedTableCellAddress {
                                    object_id: object.object_id.as_str().to_owned(),
                                    row: cell.row,
                                    column: cell.column,
                                }),
                        ),
                        ResolvedSceneObjectKind::Text { .. }
                        | ResolvedSceneObjectKind::Image { .. }
                        | ResolvedSceneObjectKind::Shape { .. } => None,
                    })
                    .flatten()
                    .collect::<Vec<_>>();
                observed.page_id == page.page_id.as_str()
                    && observed.media_box == page.bleed_box
                    && observed.bleed_box == page.bleed_box
                    && observed.trim_box == page.trim_box
                    && observed.bleed_expectations == page.bleed_expectations
                    && observed.placeholder_image_ids.is_empty()
                    && observed.clipped_table_cells == expected_clipped_cells
                    && observed.object_ids_in_paint_order
                        == page
                            .objects
                            .iter()
                            .map(|object| object.object_id.as_str().to_owned())
                            .collect::<Vec<_>>()
            })
}

const fn blocking(
    code: PublicationCandidateFindingCode,
    message: &'static str,
) -> PublicationCandidateFinding {
    PublicationCandidateFinding {
        severity: PublicationCandidateFindingSeverity::Blocker,
        code,
        message,
        target: None,
        facts: PublicationFindingFacts::None,
    }
}

const fn information(
    code: PublicationCandidateFindingCode,
    message: &'static str,
) -> PublicationCandidateFinding {
    PublicationCandidateFinding {
        severity: PublicationCandidateFindingSeverity::Information,
        code,
        message,
        target: None,
        facts: PublicationFindingFacts::None,
    }
}
