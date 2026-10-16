//! Finished-page PDF generation from a saved publication.
//!
//! Both modes use the existing confined resource workers and local candidate
//! preflight. A successful receipt records generation, never PDF/X conformance
//! or permission from the professional publication gate.

use document_core::{AssetHash, OpenedPackage};
use layout_core::{ExactFontResource, ResolvedPublication, geometry::PhysicalBounds};
use print_core::{
    FixtureContract, FontBoundary, PreflightBasis, ProductionColorEvidence, ProductionFontEvidence,
    PublicationCandidateEvidence, PublicationCandidateFinding, PublicationCandidateFindingSeverity,
    PublicationCandidatePreflight, PublicationCandidateWriter, PublicationFindingFacts,
    ScenePdfOptions, ScenePdfWriter, SerializedRasterPlacementEvidence, TrustedColorBoundary,
    TrustedFontBoundary, inspect_output_intent, prepare_publication_colors,
    prepare_publication_fonts, prepare_publication_rasters,
};
use raster_boundary::TrustedRasterBoundary;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const PDF_FILE_NAME: &str = "publication.pdf";
pub const RECEIPT_FILE_NAME: &str = "receipt.json";

/// Ordinary scene PDF or the bounded print envelope awaiting independent checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum ExportMode {
    #[serde(rename = "pdf")]
    Pdf,
    #[serde(rename = "pdfx4-candidate")]
    Pdfx4Candidate,
}

/// The output is a new directory whose parent must already exist.
#[derive(Clone, Debug)]
pub struct ExportRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub profile: PathBuf,
    pub mode: ExportMode,
    pub allow_warnings: bool,
    /// Explicit creation time for reproducible PDF/X candidate bytes.
    pub metadata_utc: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FileReceipt {
    pub path: PathBuf,
    pub sha256: String,
}

/// Stable structured fields around an existing print finding. `facts` and
/// `target` preserve the current boundary's diagnostic details as JSON objects.
#[derive(Clone, Debug, Serialize)]
pub struct FindingReceipt {
    pub code: String,
    pub message: String,
    pub target: Value,
    pub facts: Value,
}

/// Completion requires `generate_pdf` to return `Ok` (or the command to exit
/// zero), plus a parseable receipt with a matching PDF hash. A final sync error
/// can leave complete-looking files after a failed run. Boundary evidence is
/// JSON because print-core evidence types are not serialization APIs.
#[derive(Clone, Debug, Serialize)]
pub struct ExportReceipt {
    pub schema_version: u32,
    pub generator_version: String,
    pub mode: ExportMode,
    pub status: String,
    pub independent_verification: String,
    pub verification_scope: String,
    pub source: FileReceipt,
    pub profile: FileReceipt,
    pub pdf: FileReceipt,
    pub source_schema_version: u32,
    pub source_revision: u64,
    pub source_project_digest: String,
    pub scene_digest: String,
    pub preflight_candidate_sha256: String,
    pub preflight_evaluation_key: String,
    pub preflight_export_identity: String,
    pub pages: Vec<Value>,
    pub pasteboard_object_count_excluded: usize,
    pub resources: Value,
    pub transparency: Value,
    pub warnings: Vec<FindingReceipt>,
    pub warnings_accepted: bool,
}

/// Failures never authorize publication. An I/O failure during the final write
/// may leave a partial output directory; it is retained for inspection and is
/// never replaced by a retry.
#[derive(Debug)]
pub enum ExportError {
    DestinationExists(PathBuf),
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Preparation {
        stage: &'static str,
        cause: String,
        guidance: &'static str,
    },
    PreflightBlocked(Vec<FindingReceipt>),
    WarningsRequireAcceptance(Vec<FindingReceipt>),
    Receipt(serde_json::Error),
}

impl ExportError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::DestinationExists(_) => "destination_exists",
            Self::Io { .. } => "io",
            Self::Preparation { stage, .. } => stage,
            Self::PreflightBlocked(_) => "preflight_blocked",
            Self::WarningsRequireAcceptance(_) => "warnings_require_acceptance",
            Self::Receipt(_) => "receipt_encoding",
        }
    }

    #[must_use]
    pub fn findings(&self) -> &[FindingReceipt] {
        match self {
            Self::PreflightBlocked(findings) | Self::WarningsRequireAcceptance(findings) => {
                findings
            }
            _ => &[],
        }
    }
}

impl fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DestinationExists(path) => write!(
                formatter,
                "Output destination already exists: {}. Choose a new directory; no files were replaced.",
                path.display()
            ),
            Self::Io {
                action,
                path,
                source,
            } => write!(formatter, "Could not {action} {}: {source}", path.display()),
            Self::Preparation {
                stage,
                cause,
                guidance,
            } => write!(formatter, "{stage}: {cause}. {guidance}"),
            Self::PreflightBlocked(findings) => write!(
                formatter,
                "Local preflight found {} blocking issue(s). Repair the listed page or object before generating PDF.",
                findings.len()
            ),
            Self::WarningsRequireAcceptance(findings) => write!(
                formatter,
                "Local preflight found {} warning(s). Review them, then pass --allow-warnings to record acceptance.",
                findings.len()
            ),
            Self::Receipt(source) => write!(
                formatter,
                "Could not encode the completion receipt: {source}"
            ),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Receipt(source) => Some(source),
            _ => None,
        }
    }
}

/// Generates PDF using exact saved resources and writes a new completion bundle.
/// All layout, resource, preflight and receipt-encoding checks finish before the
/// output directory is created. The PDF is synced before the receipt is written.
/// Neither source publications nor existing destinations are opened for writing.
///
/// # Errors
/// Returns a stage-specific error for invalid packages/profiles, unavailable
/// workers, incomplete resources/layout, blocking findings, unaccepted warnings,
/// existing destinations or failed writes. Failed or interrupted output must be
/// inspected separately, even if its receipt parses and its PDF hash matches.
/// A retry must use a new destination.
pub fn generate_pdf(request: &ExportRequest) -> Result<ExportReceipt, ExportError> {
    require_new_destination(&request.output)?;
    let opened = document_core::open_package(&request.input).map_err(|error| {
        preparation(
            "open_publication",
            error,
            "Open and repair the saved publication or choose a valid .publisher-v2 package.",
        )
    })?;
    let profile = read_profile(&request.profile)?;
    let resources = exact_fonts(&opened)?;
    let publication =
        ResolvedPublication::resolve_output_with_font_resources(&opened.snapshot, &resources)
            .map_err(|error| {
                preparation(
                    "resolve_pages",
                    error,
                    "Finish every page layout and use the publication's exact embedded fonts.",
                )
            })?;
    let (bytes, evidence, preflight) = prepare_pdf(
        &publication,
        &resources,
        &opened,
        &profile,
        request.mode,
        request.metadata_utc.as_deref(),
    )?;
    if preflight.has_blockers() {
        return Err(ExportError::PreflightBlocked(findings(
            &preflight,
            PublicationCandidateFindingSeverity::Blocker,
        )));
    }
    let warnings = findings(&preflight, PublicationCandidateFindingSeverity::Warning);
    if !warnings.is_empty() && !request.allow_warnings {
        return Err(ExportError::WarningsRequireAcceptance(warnings));
    }
    let receipt = receipt(
        request, &opened, &profile, &bytes, &evidence, &preflight, warnings,
    );
    let mut receipt_bytes = serde_json::to_vec_pretty(&receipt).map_err(ExportError::Receipt)?;
    receipt_bytes.push(b'\n');
    fs::create_dir(&request.output).map_err(|source| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            ExportError::DestinationExists(request.output.clone())
        } else {
            io_error(
                "create new output directory (its parent must already exist)",
                &request.output,
                source,
            )
        }
    })?;
    write_new(&request.output.join(PDF_FILE_NAME), &bytes)?;
    // Written last, after the PDF sync. Receipt sync can still fail after all
    // JSON bytes are visible; callers must also observe our successful return.
    write_new(&request.output.join(RECEIPT_FILE_NAME), &receipt_bytes)?;
    Ok(receipt)
}

fn require_new_destination(path: &Path) -> Result<(), ExportError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(ExportError::DestinationExists(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect output destination", path, source)),
    }
}

fn read_profile(path: &Path) -> Result<Vec<u8>, ExportError> {
    let file =
        File::open(path).map_err(|error| io_error("read CMYK output profile", path, error))?;
    let mut bytes = Vec::new();
    // Preserve the existing profile boundary's byte budget before allocating.
    file.take(document_core::MAX_ICC_PROFILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read CMYK output profile", path, error))?;
    Ok(bytes)
}

fn exact_fonts(opened: &OpenedPackage) -> Result<Vec<ExactFontResource>, ExportError> {
    layout_core::output_font_face_bindings(&opened.snapshot)
        .map_err(|error| {
            preparation(
                "resolve_font_bindings",
                error,
                "Repair the publication's exact font bindings.",
            )
        })?
        .into_iter()
        .map(|(source, face_index)| {
            let asset = opened
                .assets
                .get(&source)
                .filter(|asset| AssetHash::for_bytes(&asset.bytes) == source)
                .ok_or_else(|| {
                    preparation(
                        "embedded_font_missing",
                        source.as_str(),
                        "Embed the exact requested font in the saved publication.",
                    )
                })?;
            Ok(ExactFontResource {
                source,
                face_index,
                bytes: asset.bytes.clone(),
            })
        })
        .collect()
}

fn prepare_pdf(
    publication: &ResolvedPublication,
    resources: &[ExactFontResource],
    opened: &OpenedPackage,
    profile: &[u8],
    mode: ExportMode,
    metadata_utc: Option<&str>,
) -> Result<
    (
        Vec<u8>,
        PublicationCandidateEvidence,
        PublicationCandidatePreflight,
    ),
    ExportError,
> {
    let facts = inspect_output_intent(profile).map_err(|error| preparation("inspect_profile", error,
        "Choose a valid printer CMYK ICC profile and keep the packaged color worker beside publisher-pdf."))?;
    let contract = FixtureContract::new(
        PreflightBasis::from(publication.clone()),
        facts,
        FontBoundary::NotExercised,
    );
    let fonts = prepare_publication_fonts(publication, resources, &TrustedFontBoundary::resolve_packaged())
        .map_err(|error| preparation("prepare_fonts", error,
            "Use exact embeddable fonts with supported glyphs, and keep the packaged font worker beside publisher-pdf."))?;
    let color_boundary = TrustedColorBoundary::resolve_packaged();
    let colors =
        prepare_publication_colors(publication, profile, &color_boundary).map_err(|error| {
            preparation(
                "prepare_colors",
                error,
                "Check the CMYK profile and packaged color worker.",
            )
        })?;
    let rasters = prepare_publication_rasters(publication, &opened.assets, profile,
        &TrustedRasterBoundary::resolve_packaged(), &color_boundary)
        .map_err(|error| preparation("prepare_pictures", error,
            "Use validated Pictures with embedded color profiles or an explicit saved sRGB policy; keep packaged raster and color workers beside publisher-pdf."))?;
    let title = &opened.snapshot.document.title;
    let metadata = if let Some(timestamp) = metadata_utc {
        print_core::PdfCandidateMetadata::at_utc(title, timestamp).ok_or_else(|| {
            preparation(
                "metadata_date",
                "Invalid PDF creation timestamp",
                "Use a valid RFC 3339 timestamp with a four-digit year.",
            )
        })?
    } else {
        print_core::PdfCandidateMetadata::for_export(title)
    };
    let candidate = PublicationCandidateWriter::write_with_resources_and_metadata(
        publication,
        &contract,
        profile,
        &rasters,
        Some(&fonts),
        colors.as_ref(),
        &metadata,
    )
    .map_err(|error| {
        preparation(
            "build_candidate",
            error,
            "Repair incomplete layout or unsupported page resources before generating PDF.",
        )
    })?;
    let preflight = PublicationCandidatePreflight::for_candidate_with_font_resources(
        publication,
        &contract,
        &candidate,
        resources,
    );
    // Preflight can report overset that the ordinary strict serializer refuses.
    // Preserve those actionable findings instead of replacing them with a generic
    // serialization error, and never expose candidate bytes when blockers exist.
    let bytes = if preflight.has_blockers() || mode == ExportMode::Pdfx4Candidate {
        candidate.bytes().to_vec()
    } else {
        ScenePdfWriter::write_with_rasters_fonts_and_colors(
            publication,
            ScenePdfOptions::default(),
            &rasters,
            Some(&fonts),
            colors.as_ref(),
        )
        .map_err(|error| {
            preparation(
                "write_scene",
                error,
                "Repair incomplete layout or unsupported page resources before generating PDF.",
            )
        })?
        .bytes()
        .to_vec()
    };
    Ok((bytes, candidate.evidence().clone(), preflight))
}

fn preparation(stage: &'static str, cause: impl fmt::Debug, guidance: &'static str) -> ExportError {
    ExportError::Preparation {
        stage,
        cause: format!("{cause:?}"),
        guidance,
    }
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> ExportError {
    ExportError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), ExportError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| io_error("create new bundle file", path, error))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            io_error(
                "write and sync bundle file (partial output may remain)",
                path,
                error,
            )
        })
}

fn hex(bytes: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(64);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn findings(
    report: &PublicationCandidatePreflight,
    severity: PublicationCandidateFindingSeverity,
) -> Vec<FindingReceipt> {
    report
        .findings
        .iter()
        .filter(|finding| finding.severity == severity)
        .map(finding_receipt)
        .collect()
}

fn finding_receipt(finding: &PublicationCandidateFinding) -> FindingReceipt {
    let facts = match finding.facts {
        PublicationFindingFacts::None => Value::Null,
        PublicationFindingFacts::ImageResolution {
            content_kind,
            effective_ppi_milli,
            target_ppi,
            minimum_ppi,
        } => {
            json!({"content_kind": format!("{content_kind:?}"), "effective_ppi_milli": effective_ppi_milli,
                "target_ppi": target_ppi, "minimum_ppi": minimum_ppi})
        }
        PublicationFindingFacts::BleedEdge { edge } => json!({"edge": format!("{edge:?}")}),
        PublicationFindingFacts::TableCell { row, column } => json!({"row": row, "column": column}),
        PublicationFindingFacts::FontEmbedding { state } => {
            json!({"font_embedding": format!("{state:?}")})
        }
    };
    FindingReceipt {
        code: format!("{:?}", finding.code), message: finding.message.into(), facts,
        target: finding.target.as_ref().map_or(Value::Null, |target| json!({
            "page_id": target.page_id, "object_id": target.object_id, "repair_action": format!("{:?}", target.action),
        })),
    }
}

fn bounds(bounds: PhysicalBounds) -> Value {
    json!({"x": bounds.x.value(), "y": bounds.y.value(),
        "width": bounds.width.value(), "height": bounds.height.value()})
}

fn receipt(
    request: &ExportRequest,
    opened: &OpenedPackage,
    profile: &[u8],
    bytes: &[u8],
    evidence: &PublicationCandidateEvidence,
    report: &PublicationCandidatePreflight,
    warnings: Vec<FindingReceipt>,
) -> ExportReceipt {
    ExportReceipt {
        schema_version: 1,
        generator_version: env!("CARGO_PKG_VERSION").into(),
        mode: request.mode,
        status: match request.mode { ExportMode::Pdf => "pdf_generated", ExportMode::Pdfx4Candidate => "candidate_generated" }.into(),
        independent_verification: "pending".into(),
        verification_scope: match request.mode {
            ExportMode::Pdf => "Ordinary PDF with no PDF/X declaration. Local candidate preflight checked the shared layout and resources; independent conformance verification is pending.",
            ExportMode::Pdfx4Candidate => "Bounded PDF/X-4 candidate. Local preflight passed; independent PDF/X-4 conformance verification is pending.",
        }.into(),
        source: FileReceipt { path: request.input.clone(), sha256: opened.fingerprint.as_str().into() },
        profile: FileReceipt { path: request.profile.clone(), sha256: hex(Sha256::digest(profile).into()) },
        pdf: FileReceipt { path: request.output.join(PDF_FILE_NAME), sha256: hex(Sha256::digest(bytes).into()) },
        source_schema_version: opened.schema_version,
        source_revision: opened.snapshot.revision.value(),
        source_project_digest: hex(evidence.source_project_digest),
        scene_digest: hex(evidence.scene_digest),
        preflight_candidate_sha256: hex(report.artifact_hash.bytes()),
        preflight_evaluation_key: hex(report.evaluation_key.bytes()),
        preflight_export_identity: hex(report.export_identity.bytes()),
        pages: evidence.pages.iter().map(|page| json!({
            "page_id": page.page_id, "media_box_um": bounds(page.media_box),
            "bleed_box_um": bounds(page.bleed_box), "trim_box_um": bounds(page.trim_box),
            "object_ids_in_paint_order": page.object_ids_in_paint_order,
            "placeholder_image_ids": page.placeholder_image_ids,
            "clipped_table_cells": page.clipped_table_cells.iter().map(|cell| json!({
                "object_id": cell.object_id, "row": cell.row, "column": cell.column,
            })).collect::<Vec<_>>(),
        })).collect(),
        // Output-only layout deliberately empties the resolved Pasteboard.
        // Count the saved objects it excluded before reading scene evidence.
        pasteboard_object_count_excluded: opened.snapshot.document.pasteboard.objects.len(),
        resources: json!({
            "fonts": evidence.embedded_fonts.iter().map(font_receipt).collect::<Vec<_>>(),
            "pictures": evidence.raster_placements.iter().map(raster_receipt).collect::<Vec<_>>(),
            "vector_colors": evidence.vector_colors.as_ref().map(color_receipt),
        }),
        transparency: json!({"object_opacity": evidence.transparency.object_opacity,
            "paint_alpha": evidence.transparency.paint_alpha, "raster_soft_mask": evidence.transparency.raster_soft_mask}),
        warnings_accepted: !warnings.is_empty() && request.allow_warnings,
        warnings,
    }
}

fn font_receipt(font: &ProductionFontEvidence) -> Value {
    json!({"source_sha256": hex(font.source_sha256), "face_index": font.face_index,
        "postscript_name_sha256": hex(font.postscript_name_sha256),
        "preparation_kind": format!("{:?}", font.preparation_kind),
        "glyph_set_sha256": hex(font.glyph_set_sha256), "used_glyph_count": font.used_glyph_count,
        "embedded_glyph_count": font.embedded_glyph_count,
        "prepared_font_sha256": hex(font.prepared_font_sha256), "rights_identity_sha256": hex(font.rights_identity_sha256),
        "worker_executable_sha256": hex(font.worker_executable_sha256),
        "engine_identity_sha256": hex(font.engine_identity_sha256), "rules_identity_sha256": hex(font.rules_identity_sha256)})
}

fn color_receipt(color: &ProductionColorEvidence) -> Value {
    json!({"source_profile_sha256": hex(color.source_profile_hash),
        "output_profile_sha256": hex(color.output_profile_hash), "raw_palette_sha256": hex(color.raw_palette_hash),
        "converted_palette_sha256": hex(color.converted_palette_hash), "color_count": color.color_count,
        "worker_executable_sha256": hex(color.color_worker_executable_hash),
        "engine_identity_sha256": hex(color.color_engine_hash), "rules_identity_sha256": hex(color.color_rules_hash)})
}

fn raster_receipt(placement: &SerializedRasterPlacementEvidence) -> Value {
    let raster = &placement.raster;
    json!({"page_id": placement.page_id, "object_id": placement.object_id,
        "size_um": {"width": placement.size.width.value(), "height": placement.size.height.value()},
        "transform": placement.transform, "crop": placement.crop, "flip": placement.flip,
        "asset": raster.asset.as_str(), "width": raster.width, "height": raster.height,
        "encoded_asset_sha256": hex(raster.encoded_asset_hash), "decoded_rgb_sha256": hex(raster.decoded_rgb_hash),
        "alpha_sha256": hex(raster.alpha_hash), "source_profile_sha256": hex(raster.source_profile_hash),
        "source_profile_origin": format!("{:?}", raster.source_profile_origin), "decode_rules_sha256": hex(raster.decode_rules_hash),
        "raster_worker_executable_sha256": hex(raster.raster_worker_executable_hash),
        "output_profile_sha256": hex(raster.output_profile_hash), "converted_cmyk_sha256": hex(raster.converted_cmyk_hash),
        "color_engine_sha256": hex(raster.color_engine_hash), "color_rules_sha256": hex(raster.color_rules_hash),
        "color_worker_executable_sha256": hex(raster.color_worker_executable_hash), "has_transparency": raster.has_transparency})
}
