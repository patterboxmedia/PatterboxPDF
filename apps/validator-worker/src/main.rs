//! Constrained-process entry point for independent PDF/X-4 validation.

use std::{
    io::{self, BufRead, Read},
    process::ExitCode,
};

use print_core::{
    ACTIVE_ICC_NAME, ACTIVE_ICC_RULES_IDENTITY, ACTIVE_ICC_SOURCE_IDENTITY, ACTIVE_ICC_VERSION,
    ArtifactHash, IndependentValidator, PUBLIC_CORE_NAME, PUBLIC_CORE_RULES_IDENTITY,
    PUBLIC_CORE_SOURCE_IDENTITY, PUBLIC_CORE_VERSION, PdfOxideValidator, PrintStandard,
    ValidationError, ValidationRequest, ValidationVerdict, ValidatorIdentity,
    audit_active_icc_profiles, audit_public_pdfx_metadata, audit_public_pdfx_objects,
};
use serde::{Deserialize, Serialize};
use validator_boundary::{
    SOURCE_IDENTITY as AUTHORITY_SOURCE_IDENTITY, VALIDATOR_NAME as AUTHORITY_NAME,
    VALIDATOR_VERSION as AUTHORITY_VERSION, expected_rules_identity_hash,
    writer_profile::audit_publisher_writer_output,
};

const PROTOCOL_VERSION: u16 = 5;
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const MAX_CANDIDATE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FINDINGS: usize = 512;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerRequest {
    protocol_version: u16,
    expected_artifact_sha256: String,
    evaluation_sha256: String,
    capsule_sha256: String,
    profile_sha256: String,
    standard: Standard,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Standard {
    PdfX4,
}

#[derive(Debug, Serialize)]
struct WorkerIdentity {
    name: String,
    version: String,
    source_sha256: String,
    rules_sha256: String,
    profile_sha256: String,
}

#[derive(Debug, Serialize)]
struct WorkerReport {
    protocol_version: u16,
    artifact_sha256: String,
    evaluation_sha256: String,
    capsule_sha256: String,
    verdict: &'static str,
    findings: Vec<String>,
    validator: WorkerIdentity,
    authority_components: Vec<WorkerIdentity>,
    diagnostic_components: Vec<WorkerIdentity>,
}

#[derive(Debug, Serialize)]
struct WorkerFailure {
    protocol_version: u16,
    error: &'static str,
    detail: String,
}

#[derive(Debug)]
enum WorkerError {
    RequestTooLarge,
    InvalidRequest(String),
    UnsupportedProtocol,
    CandidateUnavailable(String),
    CandidateTooLarge,
    ArtifactMismatch,
    Validation(ValidationError),
}

fn main() -> ExitCode {
    match read_request().and_then(|(request, candidate)| process_request(&request, &candidate)) {
        Ok(report) => write_json(&report, ExitCode::SUCCESS),
        Err(error) => {
            let failure = WorkerFailure {
                protocol_version: PROTOCOL_VERSION,
                error: error.code(),
                detail: error.detail(),
            };
            write_json(&failure, ExitCode::from(2))
        }
    }
}

fn read_request() -> Result<(WorkerRequest, Vec<u8>), WorkerError> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut header = Vec::new();
    input
        .by_ref()
        .take(MAX_REQUEST_BYTES + 1)
        .read_until(b'\n', &mut header)
        .map_err(|error| WorkerError::InvalidRequest(error.to_string()))?;
    if header.len() as u64 > MAX_REQUEST_BYTES || header.last() != Some(&b'\n') {
        return Err(WorkerError::RequestTooLarge);
    }
    header.pop();
    let request = serde_json::from_slice(&header)
        .map_err(|error| WorkerError::InvalidRequest(error.to_string()))?;
    let mut candidate = Vec::new();
    input
        .take(MAX_CANDIDATE_BYTES + 1)
        .read_to_end(&mut candidate)
        .map_err(|error| WorkerError::CandidateUnavailable(error.to_string()))?;
    if candidate.len() as u64 > MAX_CANDIDATE_BYTES {
        return Err(WorkerError::CandidateTooLarge);
    }
    if candidate.is_empty() {
        return Err(WorkerError::CandidateUnavailable(
            "candidate bytes are empty".into(),
        ));
    }
    Ok((request, candidate))
}

fn process_request(request: &WorkerRequest, bytes: &[u8]) -> Result<WorkerReport, WorkerError> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(WorkerError::UnsupportedProtocol);
    }
    let expected_hash = parse_hash(&request.expected_artifact_sha256)?;
    let evaluation_key = parse_hash(&request.evaluation_sha256)?;
    let _capsule_hash = parse_hash(&request.capsule_sha256)?;
    let profile_hash = parse_hash(&request.profile_sha256)?;
    let actual_hash = ArtifactHash::of(bytes);
    if actual_hash != expected_hash {
        return Err(WorkerError::ArtifactMismatch);
    }

    let standard = match request.standard {
        Standard::PdfX4 => PrintStandard::PdfX4,
    };
    // This product-specific census runs only inside this confined worker. It
    // does not redefine the broader PDF/X diagnostics or complete rule coverage.
    let writer_audit = audit_publisher_writer_output(bytes)
        .map_err(|error| WorkerError::Validation(ValidationError::MalformedReport(error)))?;
    let writer_passed = writer_audit.findings.is_empty();
    let validator = PdfOxideValidator::new(profile_hash);
    if !writer_passed {
        // The first pass bounds lossless samples and rejects unsupported stream
        // codecs before broader diagnostics can attempt to interpret them.
        // Component identities describe the configured pipeline; a failed
        // report makes no claim that those later checks passed or ran.
        return Ok(report(
            request,
            actual_hash,
            profile_hash,
            "failed",
            writer_audit.findings,
            validator.identity(),
        ));
    }
    let response = validator
        .validate(ValidationRequest::new(
            print_core::EvaluationKey::from_bytes(evaluation_key.bytes()),
            bytes,
            standard,
        ))
        .map_err(WorkerError::Validation)?;
    let icc_audit = audit_active_icc_profiles(bytes).map_err(WorkerError::Validation)?;
    let icc_passed = icc_audit.findings().is_empty();
    let public_core_audit = audit_public_pdfx_objects(bytes).map_err(WorkerError::Validation)?;
    let public_core_passed = public_core_audit.findings().is_empty();
    let metadata_audit = audit_public_pdfx_metadata(bytes).map_err(WorkerError::Validation)?;
    let metadata_passed = metadata_audit.findings().is_empty();
    let total_findings = response
        .findings
        .len()
        .checked_add(icc_audit.findings().len())
        .and_then(|count| count.checked_add(public_core_audit.findings().len()))
        .and_then(|count| count.checked_add(metadata_audit.findings().len()))
        .and_then(|count| count.checked_add(writer_audit.findings.len()))
        .filter(|count| *count <= MAX_FINDINGS)
        .ok_or(WorkerError::Validation(
            ValidationError::OutputLimitExceeded,
        ))?;
    let mut findings = Vec::with_capacity(total_findings);
    findings.extend(response.findings);
    findings.extend(icc_audit.into_findings());
    findings.extend(public_core_audit.into_findings());
    findings.extend(metadata_audit.into_findings());
    findings.extend(writer_audit.findings);
    let diagnostic_identity = validator.identity();

    Ok(report(
        request,
        actual_hash,
        profile_hash,
        combined_verdict(
            response.verdict,
            icc_passed && public_core_passed && metadata_passed,
        ),
        findings,
        diagnostic_identity,
    ))
}

fn report(
    request: &WorkerRequest,
    actual_hash: ArtifactHash,
    profile_hash: ArtifactHash,
    verdict: &'static str,
    findings: Vec<String>,
    diagnostic_identity: ValidatorIdentity,
) -> WorkerReport {
    WorkerReport {
        protocol_version: PROTOCOL_VERSION,
        artifact_sha256: actual_hash.hex(),
        evaluation_sha256: request.evaluation_sha256.clone(),
        capsule_sha256: request.capsule_sha256.clone(),
        verdict,
        findings,
        validator: WorkerIdentity {
            name: AUTHORITY_NAME.into(),
            version: AUTHORITY_VERSION.into(),
            source_sha256: ArtifactHash::of(AUTHORITY_SOURCE_IDENTITY.as_bytes()).hex(),
            rules_sha256: ArtifactHash::from_bytes(expected_rules_identity_hash()).hex(),
            profile_sha256: profile_hash.hex(),
        },
        authority_components: vec![
            WorkerIdentity {
                name: ACTIVE_ICC_NAME.into(),
                version: ACTIVE_ICC_VERSION.into(),
                source_sha256: ArtifactHash::of(ACTIVE_ICC_SOURCE_IDENTITY.as_bytes()).hex(),
                rules_sha256: ArtifactHash::of(ACTIVE_ICC_RULES_IDENTITY.as_bytes()).hex(),
                profile_sha256: profile_hash.hex(),
            },
            WorkerIdentity {
                name: PUBLIC_CORE_NAME.into(),
                version: PUBLIC_CORE_VERSION.into(),
                source_sha256: ArtifactHash::of(PUBLIC_CORE_SOURCE_IDENTITY.as_bytes()).hex(),
                rules_sha256: ArtifactHash::of(PUBLIC_CORE_RULES_IDENTITY.as_bytes()).hex(),
                profile_sha256: profile_hash.hex(),
            },
        ],
        diagnostic_components: vec![WorkerIdentity {
            name: diagnostic_identity.name,
            version: diagnostic_identity.version,
            source_sha256: diagnostic_identity.executable_or_source_hash.hex(),
            rules_sha256: diagnostic_identity.rules_hash.hex(),
            profile_sha256: diagnostic_identity.profile_hash.hex(),
        }],
    }
}

const fn combined_verdict(
    diagnostic_verdict: ValidationVerdict,
    authority_checks_passed: bool,
) -> &'static str {
    if matches!(diagnostic_verdict, ValidationVerdict::Passed) && authority_checks_passed {
        "passed"
    } else {
        "failed"
    }
}

fn parse_hash(value: &str) -> Result<ArtifactHash, WorkerError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(WorkerError::InvalidRequest(
            "SHA-256 fields must contain exactly 64 hexadecimal characters".into(),
        ));
    }
    let mut bytes = [0_u8; 32];
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    for (index, chunk) in pairs.iter().enumerate() {
        let pair = std::str::from_utf8(chunk)
            .map_err(|error| WorkerError::InvalidRequest(error.to_string()))?;
        bytes[index] = u8::from_str_radix(pair, 16)
            .map_err(|error| WorkerError::InvalidRequest(error.to_string()))?;
    }
    Ok(ArtifactHash::from_bytes(bytes))
}

fn write_json(value: &impl Serialize, exit: ExitCode) -> ExitCode {
    match serde_json::to_writer(io::stdout().lock(), value) {
        Ok(()) => exit,
        Err(_) => ExitCode::from(3),
    }
}

impl WorkerError {
    const fn code(&self) -> &'static str {
        match self {
            Self::RequestTooLarge => "request-too-large",
            Self::InvalidRequest(_) => "invalid-request",
            Self::UnsupportedProtocol => "unsupported-protocol",
            Self::CandidateUnavailable(_) => "candidate-unavailable",
            Self::CandidateTooLarge => "candidate-too-large",
            Self::ArtifactMismatch => "artifact-mismatch",
            Self::Validation(_) => "validation-unavailable",
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::RequestTooLarge => "request exceeds the 64 KiB protocol limit".into(),
            Self::InvalidRequest(detail) | Self::CandidateUnavailable(detail) => detail.clone(),
            Self::UnsupportedProtocol => "unsupported validator-worker protocol version".into(),
            Self::CandidateTooLarge => "candidate exceeds the 256 MiB validation limit".into(),
            Self::ArtifactMismatch => {
                "streamed candidate bytes do not match expected_artifact_sha256".into()
            }
            Self::Validation(error) => format!("{error:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    mod calibration;

    use pdf_writer::{Pdf, Ref};

    use super::{
        PROTOCOL_VERSION, Standard, WorkerError, WorkerRequest, combined_verdict, process_request,
    };
    use print_core::{ArtifactHash, ValidationVerdict};
    use validator_boundary::{ACTIVE_ICC_NAME, DIAGNOSTIC_NAME, PUBLIC_CORE_NAME, VALIDATOR_NAME};

    fn parseable_nonconformant_pdf() -> Vec<u8> {
        let catalog = Ref::new(1);
        let pages = Ref::new(2);
        let page = Ref::new(3);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog).pages(pages);
        pdf.pages(pages).kids([page]).count(1);
        pdf.page(page)
            .parent(pages)
            .media_box(pdf_writer::Rect::new(0.0, 0.0, 612.0, 792.0));
        pdf.finish()
    }

    fn request(bytes: &[u8]) -> WorkerRequest {
        WorkerRequest {
            protocol_version: PROTOCOL_VERSION,
            expected_artifact_sha256: ArtifactHash::of(bytes).hex(),
            evaluation_sha256: ArtifactHash::of(b"evaluation").hex(),
            capsule_sha256: ArtifactHash::of(b"capsule").hex(),
            profile_sha256: ArtifactHash::of(b"profile").hex(),
            standard: Standard::PdfX4,
        }
    }

    #[test]
    fn hash_mismatch_stops_before_validation() {
        let bytes = parseable_nonconformant_pdf();
        let mut request = request(&bytes);
        request.expected_artifact_sha256 = ArtifactHash::of(b"different").hex();

        let result = process_request(&request, &bytes);

        assert!(matches!(result, Err(WorkerError::ArtifactMismatch)));
    }

    #[test]
    fn parseable_candidate_returns_bounded_machine_readable_findings() {
        let bytes = parseable_nonconformant_pdf();

        let request = request(&bytes);
        let result = process_request(&request, &bytes).expect("validator returns verdict");

        assert_eq!(result.protocol_version, PROTOCOL_VERSION);
        assert_eq!(result.artifact_sha256, ArtifactHash::of(&bytes).hex());
        assert_eq!(result.evaluation_sha256, request.evaluation_sha256);
        assert_eq!(result.capsule_sha256, request.capsule_sha256);
        assert_eq!(result.verdict, "failed");
        assert!(!result.findings.is_empty());
        assert_eq!(result.validator.name, VALIDATOR_NAME);
        assert_eq!(result.authority_components.len(), 2);
        assert_eq!(result.authority_components[0].name, ACTIVE_ICC_NAME);
        assert_eq!(result.authority_components[1].name, PUBLIC_CORE_NAME);
        assert_eq!(result.diagnostic_components.len(), 1);
        assert_eq!(result.diagnostic_components[0].name, DIAGNOSTIC_NAME);
    }

    #[test]
    fn active_icc_failure_overrides_a_passing_diagnostic() {
        assert_eq!(combined_verdict(ValidationVerdict::Passed, false), "failed");
        assert_eq!(combined_verdict(ValidationVerdict::Passed, true), "passed");
    }

    fn grouped_publication() -> layout_core::ResolvedPublication {
        use document_core::model::{
            Color, DocumentId, Insets, ObjectDraft, ObjectKind, PhysicalSize, Revision, ShapeKind,
            ShapeStroke, Transform, WrapMode,
        };
        use document_core::{
            Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
        };
        use layout_core::ResolvedPublication;
        let document = Document::new(
            DocumentId::parse("writer-census").unwrap(),
            "Writer census",
            PhysicalSize::new(215_900, 279_400).unwrap(),
            Insets::uniform(3_175).unwrap(),
        )
        .unwrap();
        let page = document.pages[0].id.clone();
        let mut session = DocumentSession::new(document).unwrap();
        session
            .execute(
                Revision::initial(),
                Transaction::single(
                    "Translucent shape",
                    Command::AddObject {
                        location: ObjectLocation::Page(page),
                        object: ObjectDraft {
                            name: "Grouped ellipse".into(),
                            size: PhysicalSize::new(60_000, 30_000).unwrap(),
                            transform: Transform::identity_at(12_000, 15_000),
                            opacity_millionths: 500_000,
                            kind: ObjectKind::Shape {
                                shape: ShapeKind::Ellipse,
                                fill: Color {
                                    red: 180,
                                    green: 40,
                                    blue: 70,
                                    alpha: 220,
                                },
                                stroke: ShapeStroke {
                                    color: Color {
                                        red: 20,
                                        green: 30,
                                        blue: 40,
                                        alpha: 255,
                                    },
                                    width_millipoints: 750,
                                },
                                wrap_mode: WrapMode::None,
                            },
                        },
                    },
                ),
            )
            .unwrap();
        ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap()
    }

    #[test]
    fn publisher_profile_censuses_actual_scene_with_object_group() {
        let scene = print_core::ScenePdfWriter::write(
            &grouped_publication(),
            print_core::ScenePdfOptions::default(),
        )
        .unwrap();
        let audit = super::audit_publisher_writer_output(scene.bytes()).unwrap();
        assert!(audit.findings.is_empty(), "{:?}", audit.findings);
        assert_eq!(
            audit.artifact_sha256,
            ArtifactHash::of(scene.bytes()).bytes()
        );
        assert!(String::from_utf8_lossy(scene.bytes()).contains("/Group"));
    }

    #[test]
    fn publisher_profile_failure_is_bound_in_machine_report() {
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(Ref::new(1))
            .pages(Ref::new(2))
            .pair(pdf_writer::Name(b"UnexpectedPublisherEntry"), true);
        pdf.pages(Ref::new(2)).kids([Ref::new(3)]).count(1);
        pdf.page(Ref::new(3))
            .parent(Ref::new(2))
            .media_box(pdf_writer::Rect::new(0.0, 0.0, 100.0, 100.0))
            .contents(Ref::new(4));
        pdf.stream(Ref::new(4), b"q Q");
        let bytes = pdf.finish();
        let report = process_request(&request(&bytes), &bytes).unwrap();
        assert_eq!(report.verdict, "failed");
        assert_eq!(report.artifact_sha256, ArtifactHash::of(&bytes).hex());
        assert!(report.findings.iter().any(|finding| {
            finding.contains("blocking Publisher writer profile v5")
                && finding.contains("trailer/Root")
                && finding.contains("UnexpectedPublisherEntry")
        }));
        // A rejected first pass must return before broader PDF/ICC decoding;
        // component identity is retained, but no later findings claim a run.
        assert!(
            report
                .findings
                .iter()
                .all(|finding| { finding.starts_with("blocking Publisher writer profile v5:") })
        );
        assert_eq!(report.authority_components.len(), 2);
        assert_eq!(report.diagnostic_components.len(), 1);
    }

    // Profile inspection is deliberately Windows-confined; do not substitute a
    // fabricated authority token just to make these route checks run on Linux.
    #[cfg(windows)]
    #[test]
    fn publisher_profile_censuses_actual_candidate_routes() {
        use print_core::{
            CandidatePdfWriter, FixtureContract, FontBoundary, PreflightBasis,
            PublicationCandidateWriter, inspect_output_intent,
        };
        let publication = grouped_publication();
        let profile =
            include_bytes!("../../../crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
        let fixture = FixtureContract::new(
            PreflightBasis::from(publication.clone()),
            inspect_output_intent(profile).unwrap(),
            FontBoundary::NotExercised,
        );
        let structural = CandidatePdfWriter::write_structural_probe(&fixture).unwrap();
        let exercise =
            CandidatePdfWriter::write_pdfx4_candidate_for_validation(&fixture, profile).unwrap();
        let production =
            PublicationCandidateWriter::write(&publication, &fixture, profile).unwrap();
        for (route, bytes) in [
            ("structural", structural.bytes()),
            ("exercise", exercise.bytes()),
            ("production", production.bytes()),
        ] {
            if route != "structural" {
                let serialized = String::from_utf8_lossy(bytes);
                for key in ["/Title ", "/CreationDate ", "/ModDate "] {
                    assert!(serialized.contains(key), "{route}: missing emitted {key}");
                }
            }
            let audit = super::audit_publisher_writer_output(bytes).unwrap();
            assert!(audit.findings.is_empty(), "{route}: {:?}", audit.findings);
            assert_eq!(audit.artifact_sha256, ArtifactHash::of(bytes).bytes());
        }
    }

    #[cfg(windows)]
    #[test]
    fn publisher_profile_censuses_actual_production_picture_shadow() {
        let candidate = production_picture_shadow_candidate();
        let candidate_text = String::from_utf8_lossy(candidate.bytes());
        assert!(candidate_text.contains("/S /Luminosity"));
        assert!(candidate_text.contains("/AlphaPixels"));
        assert!(candidate_text.contains("/PV2Shadow"));

        let audit = super::audit_publisher_writer_output(candidate.bytes()).unwrap();
        assert!(audit.findings.is_empty(), "{:?}", audit.findings);
        assert_eq!(
            audit.artifact_sha256,
            ArtifactHash::of(candidate.bytes()).bytes()
        );
    }

    #[cfg(windows)]
    #[allow(
        clippy::too_many_lines,
        reason = "one shared fixture keeps Picture import, preparation and production emission in one chain"
    )]
    fn production_picture_shadow_candidate() -> print_core::PublicationCandidatePdf {
        use color_boundary::TrustedColorBoundary;
        use document_core::model::{
            Color, DocumentId, HardShadow, ImageContentKind, Insets, Micrometres, ObjectDraft,
            ObjectEffects, ObjectKind, PhysicalSize, Revision, Transform, UntaggedRasterPolicy,
            WrapMode,
        };
        use document_core::{
            AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot,
            Transaction, inspect_raster,
        };
        use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
        use layout_core::ResolvedPublication;
        use print_core::{
            FixtureContract, FontBoundary, PreflightBasis, PublicationCandidateWriter,
            inspect_output_intent, prepare_publication_colors, prepare_publication_rasters,
        };
        use raster_boundary::TrustedRasterBoundary;

        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(
                &[255, 0, 0, 255, 0, 120, 255, 64],
                2,
                1,
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        let inspection = inspect_raster(&png).unwrap();
        let mut assets = AssetStore::default();
        let asset = assets.insert_raster(png, inspection);
        let document = Document::new(
            DocumentId::parse("writer-picture-shadow-census").unwrap(),
            "Writer Picture shadow census",
            PhysicalSize::new(215_900, 279_400).unwrap(),
            Insets::uniform(3_175).unwrap(),
        )
        .unwrap();
        let page = document.pages[0].id.clone();
        let mut session = DocumentSession::new(document).unwrap();
        let policy = session
            .execute(
                Revision::initial(),
                Transaction::single(
                    "Use sRGB for the test Picture",
                    Command::UpdateUntaggedRasterPolicy {
                        policy: UntaggedRasterPolicy::AssumeSrgb,
                    },
                ),
            )
            .unwrap();
        let placed = session
            .execute(
                policy.revision,
                Transaction::single(
                    "Place transparent Picture",
                    Command::AddObject {
                        location: ObjectLocation::Page(page),
                        object: ObjectDraft {
                            name: "Transparent Picture".into(),
                            size: PhysicalSize::new(50_800, 25_400).unwrap(),
                            transform: Transform::identity_at(12_000, 18_000),
                            opacity_millionths: 750_000,
                            kind: ObjectKind::Image {
                                asset,
                                wrap_mode: WrapMode::None,
                                content_kind: ImageContentKind::ContinuousTone,
                                crop: document_core::model::ImageCrop::default(),
                                flip: document_core::model::ImageFlip::default(),
                                lock_aspect_ratio: true,
                            },
                        },
                    },
                ),
            )
            .unwrap();
        session
            .execute(
                placed.revision,
                Transaction::single(
                    "Add visible Picture shadow",
                    Command::UpdateObjectEffects {
                        object_id: placed.created_objects[0].clone(),
                        effects: ObjectEffects {
                            shadow: Some(HardShadow {
                                color: Color {
                                    red: 20,
                                    green: 30,
                                    blue: 40,
                                    alpha: 192,
                                },
                                offset_x: Micrometres::new(2_540),
                                offset_y: Micrometres::new(1_270),
                            }),
                            text_outline: None,
                            picture_border: None,
                        },
                    },
                ),
            )
            .unwrap();
        let publication =
            ResolvedPublication::resolve(&ProjectSnapshot::from_session(&session)).unwrap();
        let profile =
            include_bytes!("../../../crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
        let rasters = prepare_publication_rasters(
            &publication,
            &assets,
            profile,
            &TrustedRasterBoundary::resolve_packaged(),
            &TrustedColorBoundary::resolve_packaged(),
        )
        .unwrap();
        let colors = prepare_publication_colors(
            &publication,
            profile,
            &TrustedColorBoundary::resolve_packaged(),
        )
        .unwrap()
        .unwrap();
        let fixture = FixtureContract::new(
            PreflightBasis::from(publication.clone()),
            inspect_output_intent(profile).unwrap(),
            FontBoundary::NotExercised,
        );
        PublicationCandidateWriter::write_with_resources(
            &publication,
            &fixture,
            profile,
            &rasters,
            None,
            Some(&colors),
        )
        .unwrap()
    }

    #[cfg(windows)]
    fn actual_font_scene_candidate(bytes: &[u8]) -> print_core::SerializedScenePdf {
        use document_core::model::{
            DocumentId, FontFaceBinding, Insets, ObjectDraft, ObjectKind, PhysicalSize, Revision,
            TextFrame, TextStoryFormatting, TextStyle, Transform,
        };
        use document_core::{
            AssetHash, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot,
            Transaction,
        };
        use layout_core::{ExactFontResource, ResolvedPublication};
        use print_core::{
            ScenePdfOptions, ScenePdfWriter, TrustedFontBoundary, prepare_publication_fonts,
        };
        let resource = ExactFontResource {
            source: AssetHash::for_bytes(bytes),
            face_index: 0,
            bytes: bytes.to_vec(),
        };
        let document = Document::new(
            DocumentId::parse("font-census").unwrap(),
            "Font census",
            PhysicalSize::new(215_900, 279_400).unwrap(),
            Insets::uniform(3_175).unwrap(),
        )
        .unwrap();
        let page = document.pages[0].id.clone();
        let mut session = DocumentSession::new(document).unwrap();
        session
            .execute(
                Revision::initial(),
                Transaction::single(
                    "Exact text",
                    Command::AddObject {
                        location: ObjectLocation::Page(page),
                        object: ObjectDraft {
                            name: "Exact text".into(),
                            size: PhysicalSize::new(120_000, 40_000).unwrap(),
                            transform: Transform::identity_at(10_000, 15_000),
                            opacity_millionths: 500_000,
                            kind: ObjectKind::Text {
                                text: "Magic".into(),
                                style: TextStyle {
                                    font_face: Some(FontFaceBinding {
                                        source: resource.source.clone(),
                                        face_index: 0,
                                    }),
                                    font_family: "Exact fixture".into(),
                                    font_size_millipoints: 24_000,
                                    ..TextStyle::default()
                                },
                                frame: TextFrame::default(),
                                story: TextStoryFormatting::default(),
                            },
                        },
                    },
                ),
            )
            .unwrap();
        let publication = ResolvedPublication::resolve_with_font_resources(
            &ProjectSnapshot::from_session(&session),
            core::slice::from_ref(&resource),
        )
        .unwrap();
        let fonts = prepare_publication_fonts(
            &publication,
            core::slice::from_ref(&resource),
            &TrustedFontBoundary::resolve_packaged(),
        )
        .unwrap();
        ScenePdfWriter::write_with_rasters_and_fonts(
            &publication,
            ScenePdfOptions::default(),
            &[],
            &fonts,
        )
        .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn publisher_profile_censuses_actual_truetype_and_cff_font_graphs() {
        for bytes in [
            include_bytes!("../../../crates/layout-core/tests/fixtures/DejaVuSans.ttf").as_slice(),
            include_bytes!("../../../crates/layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf")
                .as_slice(),
        ] {
            let scene = actual_font_scene_candidate(bytes);
            let audit = super::audit_publisher_writer_output(scene.bytes()).unwrap();
            assert!(audit.findings.is_empty(), "{:?}", audit.findings);
        }
    }

    #[cfg(windows)]
    #[test]
    fn actual_cff_scene_embeds_raw_cid_program_with_matching_names() {
        let font =
            include_bytes!("../../../crates/layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf");
        let scene = actual_font_scene_candidate(font);
        assert!(
            scene.bytes() == actual_font_scene_candidate(font).bytes(),
            "actual CFF scene output must be repeatable"
        );
        let text = String::from_utf8_lossy(scene.bytes());
        let references = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("/FontFile3 "))
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 1);
        let fields = references[0].split_ascii_whitespace().collect::<Vec<_>>();
        assert_eq!(&fields[1..], &["0", "R"]);
        let program_object = fields[0].parse::<u32>().unwrap();
        let (range, header) = calibration::emitted_stream(scene.bytes(), program_object);
        assert!(header.contains("/Subtype /CIDFontType0C"));
        assert!(!header.contains("/Subtype /OpenType"));
        assert!(!header.contains("/Length1"));
        let program = &scene.bytes()[range];
        assert_eq!(
            program.first(),
            Some(&1),
            "FontFile3 starts with a CFF 1 header"
        );
        assert!(
            !program.starts_with(b"OTTO"),
            "FontFile3 is raw CFF, not SFNT"
        );
        assert_eq!(scene.evidence().embedded_fonts.len(), 1);
        assert_eq!(
            ArtifactHash::of(program).bytes(),
            scene.evidence().embedded_fonts[0].prepared_font_sha256
        );

        let names = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("/FontName /"))
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 1);
        let name = names[0].trim();
        assert!(name.len() > 7 && name.as_bytes()[..6].iter().all(u8::is_ascii_uppercase));
        assert_eq!(name.as_bytes()[6], b'+');
        assert_eq!(text.matches(&format!("/BaseFont /{name}\n")).count(), 1);
        assert_eq!(
            text.matches(&format!("/BaseFont /{name}-Identity-H\n"))
                .count(),
            1
        );

        let profile = super::audit_publisher_writer_output(scene.bytes()).unwrap();
        assert!(profile.findings.is_empty(), "{:?}", profile.findings);
        let report = process_request(&request(scene.bytes()), scene.bytes()).unwrap();
        assert_eq!(report.verdict, "failed", "scene has no PDF/X envelope");
        assert_eq!(
            report.artifact_sha256,
            ArtifactHash::of(scene.bytes()).hex()
        );
        assert!(
            report
                .findings
                .iter()
                .all(|finding| { !finding.contains("[XFONT-") && !finding.contains("font /") }),
            "{:?}",
            report.findings
        );
    }
}
