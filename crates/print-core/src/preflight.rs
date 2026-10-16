use crate::{
    ArtifactHash, CandidatePdf, EvaluationKey, FixtureContract, FontBoundary, IccColorSpace,
    IccDeviceClass, PublicationCandidatePreflight,
};
use validator_boundary::{
    TrustedValidation, TrustedValidationVerdict, ValidatorIsolationStatus,
    ValidatorRuleCoverageStatus, expected_rules_identity_hash, expected_source_identity_hash,
    expected_tool_identity_hash,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindingSeverity {
    Blocker,
    Warning,
    Information,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FindingCode {
    InvalidPageBoxes,
    CandidateEvaluationMismatch,
    CandidatePageBoxesMismatch,
    PdfxMetadataNotWritten,
    OutputIntentNotEmbedded,
    OutputIntentProfileMismatch,
    TaggedRgbRasterNotWritten,
    TaggedRgbIdentityMismatch,
    LiveTransparencyNotWritten,
    FontNotEmbedded,
    OutputIntentIsNotPrinterProfile,
    OutputIntentIsNotCmyk,
    OutputIntentColorimetricQualityUnverified,
    FontEmbeddingNotExercised,
    FontEmbeddingPermissionBlocked,
    FullFontEmbeddingUnresolved,
    FontSubsetPrepared,
    IndependentValidationRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreflightFinding {
    pub severity: FindingSeverity,
    pub code: FindingCode,
    pub message: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreflightReport {
    pub evaluation_key: EvaluationKey,
    pub artifact_hash: ArtifactHash,
    pub findings: Vec<PreflightFinding>,
    expected_profile_hash: ArtifactHash,
}

impl PreflightReport {
    #[must_use]
    pub fn for_candidate(fixture: &FixtureContract, candidate: &CandidatePdf) -> Self {
        let mut findings = Vec::new();
        if !fixture.boxes.is_nested() {
            findings.push(blocker(
                FindingCode::InvalidPageBoxes,
                "MediaBox, BleedBox, and TrimBox are not correctly nested.",
            ));
        }
        if candidate.evidence().evaluation_key != fixture.evaluation_key() {
            findings.push(blocker(
                FindingCode::CandidateEvaluationMismatch,
                "Candidate evidence belongs to a different fixture evaluation.",
            ));
        }
        if candidate.evidence().boxes != fixture.boxes {
            findings.push(blocker(
                FindingCode::CandidatePageBoxesMismatch,
                "Candidate page-box evidence does not match the fixture.",
            ));
        }
        if !candidate.evidence().pdfx_metadata_written {
            findings.push(blocker(
                FindingCode::PdfxMetadataNotWritten,
                "The candidate does not contain explicit PDF/X-4 metadata.",
            ));
        }
        if !candidate.evidence().output_intent_embedded {
            findings.push(blocker(
                FindingCode::OutputIntentNotEmbedded,
                "The candidate does not embed the required output intent.",
            ));
        }
        if candidate.evidence().output_intent_profile_hash != Some(fixture.profile_hash()) {
            findings.push(blocker(
                FindingCode::OutputIntentProfileMismatch,
                "The embedded output-intent profile does not match the evaluated profile bytes.",
            ));
        }
        check_tagged_rgb(fixture, candidate, &mut findings);
        if !candidate.evidence().live_transparency_written {
            findings.push(blocker(
                FindingCode::LiveTransparencyNotWritten,
                "The candidate has not exercised live transparency.",
            ));
        }
        if !candidate.evidence().font_embedded {
            findings.push(blocker(
                FindingCode::FontNotEmbedded,
                "The candidate does not embed the prepared font subset.",
            ));
        }
        if fixture.output_intent.device_class != IccDeviceClass::Printer {
            findings.push(blocker(
                FindingCode::OutputIntentIsNotPrinterProfile,
                "The output intent is not declared as an ICC printer profile.",
            ));
        }
        if fixture.output_intent.color_space != IccColorSpace::Cmyk {
            findings.push(blocker(
                FindingCode::OutputIntentIsNotCmyk,
                "The output-intent profile does not declare a CMYK device space.",
            ));
        }
        findings.push(information(
            FindingCode::OutputIntentColorimetricQualityUnverified,
            "The ICC profile parsed successfully, but its colorimetric quality is not independently characterized.",
        ));

        match fixture.font {
            FontBoundary::NotExercised => findings.push(blocker(
                FindingCode::FontEmbeddingNotExercised,
                "The fixture has not yet embedded a licensed font subset.",
            )),
            FontBoundary::PermissionBlocked => findings.push(blocker(
                FindingCode::FontEmbeddingPermissionBlocked,
                "Font embedding permission is absent or unknown.",
            )),
            FontBoundary::FullEmbeddingRequired { .. } => findings.push(blocker(
                FindingCode::FullFontEmbeddingUnresolved,
                "Subsetting failed; full-font embedding requires a separate decision.",
            )),
            FontBoundary::SubsetPrepared(_) => findings.push(information(
                FindingCode::FontSubsetPrepared,
                "A licensed subset was prepared and fingerprinted.",
            )),
        }
        findings.push(information(
            FindingCode::IndependentValidationRequired,
            "An independent PDF/X-4 validator must inspect these exact candidate bytes.",
        ));

        Self {
            evaluation_key: fixture.evaluation_key(),
            artifact_hash: candidate.artifact_hash(),
            findings,
            expected_profile_hash: ArtifactHash::from_bytes(fixture.profile_hash().bytes()),
        }
    }

    #[must_use]
    pub fn has_blockers(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.severity == FindingSeverity::Blocker)
    }

    #[must_use]
    pub fn ready_for_independent_validation(&self) -> bool {
        !self.has_blockers()
    }

    /// Exact output profile identity structurally evaluated for this candidate.
    #[must_use]
    pub const fn expected_profile_hash(&self) -> ArtifactHash {
        self.expected_profile_hash
    }
}

fn check_tagged_rgb(
    fixture: &FixtureContract,
    candidate: &CandidatePdf,
    findings: &mut Vec<PreflightFinding>,
) {
    let evidence = candidate.evidence();
    if !evidence.tagged_rgb_raster_written {
        findings.push(blocker(
            FindingCode::TaggedRgbRasterNotWritten,
            "The candidate has not exercised tagged RGB raster placement.",
        ));
    }
    let boundary = match (
        evidence.tagged_rgb_source_profile_hash,
        evidence.tagged_rgb_raw_pixel_hash,
        evidence.tagged_rgb_width,
        evidence.tagged_rgb_height,
        evidence.tagged_rgb_rendering_intent,
        evidence.tagged_rgb_engine_identity_hash,
        evidence.tagged_rgb_rules_identity_hash,
        evidence.tagged_rgb_worker_executable_hash,
    ) {
        (
            Some(source_profile_hash),
            Some(raw_pixel_hash),
            Some(width),
            Some(height),
            Some(rendering_intent),
            Some(engine_identity_hash),
            Some(rules_identity_hash),
            Some(worker_executable_hash),
        ) => Some(crate::TaggedRgbBoundary::WorkerAttested {
            source_profile_hash,
            raw_pixel_hash,
            width,
            height,
            rendering_intent,
            engine_identity_hash,
            rules_identity_hash,
            worker_executable_hash,
        }),
        _ => None,
    };
    if boundary != Some(fixture.tagged_rgb) {
        findings.push(blocker(
            FindingCode::TaggedRgbIdentityMismatch,
            "Tagged RGB evidence does not match the evaluated source profile, pixels, dimensions, intent, engine, and rules.",
        ));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationGate {
    pub allowed: bool,
    pub reason: &'static str,
    authorization: Option<PublicationAuthorization>,
    release_blockers: &'static [PublicationReleaseBlocker],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationReleaseBlocker {
    ValidatorRuleCoverageNotReleaseProven,
    ValidatorIsolationNotReleaseProven,
}

const ALL_VALIDATOR_RELEASE_BLOCKERS: &[PublicationReleaseBlocker] = &[
    PublicationReleaseBlocker::ValidatorRuleCoverageNotReleaseProven,
    PublicationReleaseBlocker::ValidatorIsolationNotReleaseProven,
];
const RULE_COVERAGE_RELEASE_BLOCKER: &[PublicationReleaseBlocker] =
    &[PublicationReleaseBlocker::ValidatorRuleCoverageNotReleaseProven];
const ISOLATION_RELEASE_BLOCKER: &[PublicationReleaseBlocker] =
    &[PublicationReleaseBlocker::ValidatorIsolationNotReleaseProven];

/// Opaque permission to commit only the exact candidate that passed both gates.
///
/// ```compile_fail
/// use print_core::PublicationAuthorization;
/// let forged = PublicationAuthorization { /* private authorization facts */ };
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationAuthorization {
    artifact_hash: ArtifactHash,
    evaluation_key: EvaluationKey,
    export_identity: ArtifactHash,
}

impl PublicationAuthorization {
    #[must_use]
    pub const fn artifact_hash(self) -> ArtifactHash {
        self.artifact_hash
    }

    #[must_use]
    pub const fn evaluation_key(self) -> EvaluationKey {
        self.evaluation_key
    }

    #[must_use]
    pub const fn export_identity(self) -> ArtifactHash {
        self.export_identity
    }

    #[cfg(test)]
    pub(crate) const fn for_test(artifact_hash: ArtifactHash) -> Self {
        Self {
            artifact_hash,
            evaluation_key: EvaluationKey::from_bytes([2; 32]),
            export_identity: ArtifactHash::from_bytes([3; 32]),
        }
    }
}

impl PublicationGate {
    const fn denied(reason: &'static str) -> Self {
        Self {
            allowed: false,
            reason,
            authorization: None,
            release_blockers: ALL_VALIDATOR_RELEASE_BLOCKERS,
        }
    }

    #[must_use]
    pub fn evaluate(
        report: &PublicationCandidatePreflight,
        validation: Option<&TrustedValidation>,
    ) -> Self {
        Self::evaluate_with_acknowledgement(report, validation, None)
    }

    #[must_use]
    pub fn evaluate_with_acknowledgement(
        report: &PublicationCandidatePreflight,
        validation: Option<&TrustedValidation>,
        acknowledgement: Option<&crate::PublicationWarningAcknowledgement>,
    ) -> Self {
        if report.has_blockers() {
            return Self::denied("structural preflight still contains blockers");
        }
        if !report.warnings_acknowledged(acknowledgement) {
            return Self::denied(
                "preflight warnings require acknowledgement for this exact evaluation",
            );
        }
        let Some(validation) = validation else {
            return Self::denied("independent PDF/X-4 validation is missing");
        };
        if validation.evaluation_hash() != report.evaluation_key.bytes() {
            return Self::denied("validator evidence belongs to a different evaluation");
        }
        if validation.artifact_hash() != report.artifact_hash.bytes() {
            return Self::denied("validator evidence belongs to different candidate bytes");
        }
        if validation.capsule_hash() != report.export_identity.bytes() {
            return Self::denied("validator evidence belongs to a different export capsule");
        }
        if validation.verdict() != TrustedValidationVerdict::Passed {
            return Self::denied("independent PDF/X-4 validation did not pass");
        }
        if validation.profile_hash() != report.profile_hash.bytes() {
            return Self::denied("validator evidence used a different output profile");
        }
        #[cfg(windows)]
        if validation.structural_check_status() != validator_boundary::StructuralCheckStatus::Passed
            || validation.qpdf_package_hash() == [0; 32]
        {
            return Self::denied(
                "validator evidence did not include the pinned qpdf structural check",
            );
        }
        if validation.rules_hash() != expected_rules_identity_hash()
            || validation.source_hash() != expected_source_identity_hash()
            || validation.tool_identity_hash() != expected_tool_identity_hash()
            || validation.worker_executable_hash() == [0; 32]
        {
            return Self::denied(
                "validator evidence did not cross the pinned packaged-worker trust boundary",
            );
        }
        let (release_blockers, release_reason) = match (
            validation.rule_coverage_status(),
            validation.isolation_status(),
        ) {
            (
                ValidatorRuleCoverageStatus::ReleaseProven,
                ValidatorIsolationStatus::ReleaseProven,
            ) => (&[][..], ""),
            (
                ValidatorRuleCoverageStatus::ReleaseProven,
                ValidatorIsolationStatus::ProcessLimitsOnly,
            ) => (
                ISOLATION_RELEASE_BLOCKER,
                "trusted validator evidence is recorded, but validator isolation is not release-proven",
            ),
            (ValidatorRuleCoverageStatus::Incomplete, ValidatorIsolationStatus::ReleaseProven) => (
                RULE_COVERAGE_RELEASE_BLOCKER,
                "trusted validator evidence is recorded, but validator rule coverage is not release-proven",
            ),
            (
                ValidatorRuleCoverageStatus::Incomplete,
                ValidatorIsolationStatus::ProcessLimitsOnly,
            ) => (
                ALL_VALIDATOR_RELEASE_BLOCKERS,
                "trusted validator evidence is recorded, but validator rule coverage and isolation are not release-proven",
            ),
        };
        if !release_blockers.is_empty() {
            return Self {
                allowed: false,
                reason: release_reason,
                authorization: None,
                release_blockers,
            };
        }
        Self {
            allowed: true,
            reason: "structural preflight and independent PDF/X-4 validation passed",
            authorization: Some(PublicationAuthorization {
                artifact_hash: report.artifact_hash,
                evaluation_key: report.evaluation_key,
                export_identity: report.export_identity,
            }),
            release_blockers,
        }
    }

    #[must_use]
    pub const fn authorization(&self) -> Option<&PublicationAuthorization> {
        self.authorization.as_ref()
    }

    #[must_use]
    pub const fn release_blockers(&self) -> &'static [PublicationReleaseBlocker] {
        self.release_blockers
    }
}

const fn blocker(code: FindingCode, message: &'static str) -> PreflightFinding {
    PreflightFinding {
        severity: FindingSeverity::Blocker,
        code,
        message,
    }
}

const fn information(code: FindingCode, message: &'static str) -> PreflightFinding {
    PreflightFinding {
        severity: FindingSeverity::Information,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use document_core::Revision;
    use layout_core::ResolvedPublication;

    use super::PreflightReport;
    use crate::{
        COLOR_ENGINE_IDENTITY, COLOR_RULES_IDENTITY, CandidatePdf, ColorRenderingIntent,
        FixtureContract, FontBoundary, IccColorSpace, IccDeviceClass, OutputIntentFacts,
        PreflightBasis, ProfileHash, StructuralEvidence, SubsetFontBoundary, TaggedRgbBoundary,
    };

    fn fixture(font: FontBoundary) -> FixtureContract {
        FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            OutputIntentFacts::from_test_parts(
                ProfileHash::from_bytes([3; 32]),
                132,
                132,
                IccDeviceClass::Printer,
                IccColorSpace::Cmyk,
                *b"Lab ",
            ),
            font,
        )
        .with_tagged_rgb(TaggedRgbBoundary::expected(
            ProfileHash::from_bytes([8; 32]),
            crate::ArtifactHash::from_bytes([7; 32]),
            2,
            2,
            crate::ArtifactHash::from_bytes([6; 32]),
        ))
    }

    fn complete_candidate_with_worker_hash(
        fixture: &FixtureContract,
        bytes: &[u8],
        worker_executable_hash: crate::ArtifactHash,
    ) -> CandidatePdf {
        CandidatePdf::from_test_evidence(
            bytes,
            StructuralEvidence {
                evaluation_key: fixture.evaluation_key(),
                boxes: fixture.boxes,
                device_cmyk_swatch_written: true,
                pdfx_metadata_written: true,
                output_intent_embedded: true,
                output_intent_profile_hash: Some(fixture.profile_hash()),
                tagged_rgb_raster_written: true,
                tagged_rgb_source_profile_hash: Some(ProfileHash::from_bytes([8; 32])),
                tagged_rgb_raw_pixel_hash: Some(crate::ArtifactHash::from_bytes([7; 32])),
                tagged_rgb_width: Some(2),
                tagged_rgb_height: Some(2),
                tagged_rgb_rendering_intent: Some(ColorRenderingIntent::RelativeColorimetric),
                tagged_rgb_engine_identity_hash: Some(crate::ArtifactHash::of(
                    COLOR_ENGINE_IDENTITY.as_bytes(),
                )),
                tagged_rgb_rules_identity_hash: Some(crate::ArtifactHash::of(
                    COLOR_RULES_IDENTITY.as_bytes(),
                )),
                tagged_rgb_worker_executable_hash: Some(worker_executable_hash),
                converted_cmyk_raster_hash: Some(crate::ArtifactHash::from_bytes([9; 32])),
                live_transparency_written: true,
                font_embedded: true,
            },
        )
    }

    #[test]
    fn preflight_rejects_worker_executable_identity_drift() {
        let fixture = fixture(FontBoundary::SubsetPrepared(Box::new(SubsetFontBoundary {
            source_sha256: [1; 32],
            face_index: 0,
            face_identity_sha256: [2; 32],
            glyph_set_sha256: [3; 32],
            glyph_count: 2,
            subset_sha256: [4; 32],
            rights_identity_sha256: [5; 32],
            worker_executable_sha256: [6; 32],
            engine_identity_sha256: [7; 32],
            rules_identity_sha256: [8; 32],
        })));
        let candidate = complete_candidate_with_worker_hash(
            &fixture,
            b"replacement-worker-candidate",
            crate::ArtifactHash::of(b"replacement worker"),
        );

        let report = PreflightReport::for_candidate(&fixture, &candidate);

        assert!(report.has_blockers());
        assert!(
            report
                .findings
                .iter()
                .any(|finding| { finding.code == super::FindingCode::TaggedRgbIdentityMismatch })
        );
    }
}
