use color_boundary::TrustedColorConversion;
use document_core::assets::MAX_ICC_PROFILE_BYTES;
use font_boundary::{FontOutlineKind, PreparedFontSubset};
use pdf_writer::types::{CidFontType, FontFlags, OutputIntentSubtype, SystemInfo, TrappingStatus};
use pdf_writer::writers::Resources;
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref, Str, TextStr};

use crate::{
    ArtifactHash, ColorRenderingIntent, EvaluationKey, FixtureContract, FontBoundary, PageBox,
    PageBoxes, ProfileError, ProfileHash, TaggedRgbBoundary,
};

const FONT_RESOURCE_NAME: Name<'static> = Name(b"PublicationFont");
const TAGGED_RGB_RESOURCE_NAME: Name<'static> = Name(b"TaggedRgbProbe");
const TAGGED_RGB_IMAGE_ID: Ref = Ref::new(15);
pub const MAX_TAGGED_RASTER_DIMENSION: u32 =
    document_core::raster_capacity::MAX_PRODUCTION_RASTER_DIMENSION;
pub const MAX_TAGGED_RASTER_PIXELS: usize =
    document_core::raster_capacity::MAX_PRODUCTION_RASTER_PIXELS;

/// Facts observed while building the candidate. These are intermediate
/// structural evidence and are never a conformance verdict.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct StructuralEvidence {
    pub evaluation_key: EvaluationKey,
    pub boxes: PageBoxes,
    pub device_cmyk_swatch_written: bool,
    pub pdfx_metadata_written: bool,
    pub output_intent_embedded: bool,
    pub output_intent_profile_hash: Option<ProfileHash>,
    pub tagged_rgb_raster_written: bool,
    pub tagged_rgb_source_profile_hash: Option<ProfileHash>,
    pub tagged_rgb_raw_pixel_hash: Option<ArtifactHash>,
    pub tagged_rgb_width: Option<u32>,
    pub tagged_rgb_height: Option<u32>,
    pub(crate) tagged_rgb_rendering_intent: Option<ColorRenderingIntent>,
    pub tagged_rgb_engine_identity_hash: Option<ArtifactHash>,
    pub tagged_rgb_rules_identity_hash: Option<ArtifactHash>,
    pub(crate) tagged_rgb_worker_executable_hash: Option<ArtifactHash>,
    pub converted_cmyk_raster_hash: Option<ArtifactHash>,
    pub live_transparency_written: bool,
    pub font_embedded: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidatePdf {
    bytes: Vec<u8>,
    artifact_hash: ArtifactHash,
    evidence: StructuralEvidence,
}

impl CandidatePdf {
    fn new(bytes: Vec<u8>, evidence: StructuralEvidence) -> Self {
        let artifact_hash = ArtifactHash::of(&bytes);
        Self {
            bytes,
            artifact_hash,
            evidence,
        }
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn artifact_hash(&self) -> ArtifactHash {
        self.artifact_hash
    }

    #[must_use]
    pub const fn evidence(&self) -> &StructuralEvidence {
        &self.evidence
    }

    #[cfg(test)]
    pub(crate) fn from_test_evidence(bytes: &[u8], evidence: StructuralEvidence) -> Self {
        Self::new(bytes.to_vec(), evidence)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateError {
    InvalidPageBoxes,
    OutputProfileTooLarge,
    InvalidOutputProfile(ProfileError),
    OutputProfileMismatch,
    FontFixtureMismatch,
    TaggedRgbDimensionsInvalid,
    TaggedRgbPixelBudgetExceeded,
    TaggedRgbPixelLengthMismatch,
    TaggedRgbAttestationMismatch,
}

/// Produces a deliberately incomplete, deterministic structural probe.
///
/// It exercises explicit page boxes and a `DeviceCMYK` vector swatch. Output
/// intent, tagged RGB, transparency, and font embedding remain `false` in the
/// returned evidence until their adapters are implemented. The result must not
/// be offered as a PDF/X export.
pub struct CandidatePdfWriter;

impl CandidatePdfWriter {
    /// Writes the deterministic, deliberately incomplete structural probe.
    ///
    /// # Errors
    ///
    /// Returns [`CandidateError::InvalidPageBoxes`] when the fixture's media,
    /// bleed, and trim boxes are not nested with positive area.
    pub fn write_structural_probe(
        fixture: &FixtureContract,
    ) -> Result<CandidatePdf, CandidateError> {
        if !fixture.boxes.is_nested() {
            return Err(CandidateError::InvalidPageBoxes);
        }

        let catalog_id = Ref::new(1);
        let page_tree_id = Ref::new(2);
        let page_id = Ref::new(3);
        let content_id = Ref::new(4);
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        pdf.catalog(catalog_id).pages(page_tree_id);
        pdf.pages(page_tree_id).kids([page_id]).count(1);

        let mut page = pdf.page(page_id);
        page.parent(page_tree_id);
        page.media_box(as_rect(fixture.boxes.media));
        page.trim_box(as_rect(fixture.boxes.trim));
        page.bleed_box(as_rect(fixture.boxes.bleed));
        page.contents(content_id);
        page.finish();

        let mut content = Content::new();
        content.set_fill_cmyk(0.75, 0.25, 0.0, 0.1);
        content.rect(36.0, 36.0, 144.0, 72.0);
        content.fill_nonzero();
        pdf.stream(content_id, &content.finish());

        let bytes = pdf.finish();
        Ok(CandidatePdf::new(
            bytes,
            StructuralEvidence {
                evaluation_key: fixture.evaluation_key(),
                boxes: fixture.boxes,
                device_cmyk_swatch_written: true,
                pdfx_metadata_written: false,
                output_intent_embedded: false,
                output_intent_profile_hash: None,
                tagged_rgb_raster_written: false,
                tagged_rgb_source_profile_hash: None,
                tagged_rgb_raw_pixel_hash: None,
                tagged_rgb_width: None,
                tagged_rgb_height: None,
                tagged_rgb_rendering_intent: None,
                tagged_rgb_engine_identity_hash: None,
                tagged_rgb_rules_identity_hash: None,
                tagged_rgb_worker_executable_hash: None,
                converted_cmyk_raster_hash: None,
                live_transparency_written: false,
                font_embedded: false,
            },
        ))
    }

    /// Writes a bounded PDF/X-4-shaped candidate for structural and independent validation.
    ///
    /// The result contains explicit metadata, nested page boxes, the exact caller-supplied CMYK
    /// printer profile as an output intent, and live vector transparency. It remains an
    /// unvalidated candidate and must not be described or offered as conforming output.
    ///
    /// # Errors
    ///
    /// Returns [`CandidateError`] when page boxes are invalid, the profile exceeds 16 MiB, its
    /// header is unacceptable, or its exact inspected facts differ from the fixture contract.
    pub fn write_pdfx4_candidate_for_validation(
        fixture: &FixtureContract,
        output_profile: &[u8],
    ) -> Result<CandidatePdf, CandidateError> {
        Self::write_pdfx4_candidate(fixture, output_profile, None, None)
    }

    /// Writes a PDF/X-4-shaped validation candidate with one exact font subset.
    ///
    /// The current fixture contract represents one font boundary, so accepting
    /// exactly one subset keeps the candidate evidence bound to the evaluation
    /// key. The method prepares every font object before writing any PDF bytes;
    /// permission, identity, subsetting, parsing, or fixture mismatches therefore
    /// fail without producing a partially embedded candidate.
    ///
    /// # Errors
    ///
    /// Returns [`CandidateError`] for the page/profile errors documented by
    /// [`Self::write_pdfx4_candidate_for_validation`], or when the exact font
    /// cannot be legally and structurally subsetted or does not match the
    /// fixture's prepared-font boundary.
    pub fn write_pdfx4_candidate_with_font_for_validation(
        fixture: &FixtureContract,
        output_profile: &[u8],
        font: &PreparedFontSubset,
    ) -> Result<CandidatePdf, CandidateError> {
        Self::write_pdfx4_candidate(fixture, output_profile, Some(font), None)
    }

    /// Writes the complete production-shaped color/font probe for validation.
    ///
    /// The exact tagged RGB pixels are converted through the validated source
    /// profile and the fixture's exact CMYK output profile before the resulting
    /// `DeviceCMYK` image is embedded.
    ///
    /// # Errors
    ///
    /// Returns [`CandidateError`] when either profile, the raster dimensions or
    /// bytes, font preparation, color transform, or page contract is invalid.
    pub fn write_pdfx4_candidate_with_font_and_trusted_color_for_validation(
        fixture: &FixtureContract,
        output_profile: &[u8],
        font: &PreparedFontSubset,
        raster: &TrustedColorConversion,
    ) -> Result<CandidatePdf, CandidateError> {
        Self::write_pdfx4_candidate(fixture, output_profile, Some(font), Some(raster))
    }

    fn write_pdfx4_candidate(
        fixture: &FixtureContract,
        output_profile: &[u8],
        font: Option<&PreparedFontSubset>,
        raster: Option<&TrustedColorConversion>,
    ) -> Result<CandidatePdf, CandidateError> {
        let (profile_facts, prepared_font, prepared_raster) =
            validate_candidate_inputs(fixture, output_profile, font, raster)?;

        let catalog_id = Ref::new(1);
        let page_tree_id = Ref::new(2);
        let page_id = Ref::new(3);
        let content_id = Ref::new(4);
        let metadata_id = Ref::new(5);
        let profile_id = Ref::new(6);
        let info_id = Ref::new(7);
        let alpha_state_id = Ref::new(8);
        let font_refs = FontObjectRefs::candidate();
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        let mut file_id = fixture.evaluation_key().bytes()[..16].to_vec();
        if file_id.iter().all(|byte| *byte == 0) {
            file_id[0] = 1;
        }
        pdf.set_file_id((file_id.clone(), file_id.clone()));
        {
            let mut catalog = pdf.catalog(catalog_id);
            catalog.pages(page_tree_id).metadata(metadata_id);
            catalog
                .output_intents()
                .push()
                .subtype(OutputIntentSubtype::PDFX)
                .output_condition_identifier(TextStr("Publisher V2 CMYK output profile"))
                .info(TextStr("Embedded CMYK printer output profile"))
                .dest_output_profile(profile_id);
        }
        pdf.pages(page_tree_id).kids([page_id]).count(1);
        {
            let mut page = pdf.page(page_id);
            page.parent(page_tree_id);
            page.media_box(as_rect(fixture.boxes.media));
            page.bleed_box(as_rect(fixture.boxes.bleed));
            page.trim_box(as_rect(fixture.boxes.trim));
            page.contents(content_id);
            let mut resources = page.resources();
            resources
                .ext_g_states()
                .pair(Name(b"Alpha50"), alpha_state_id);
            if prepared_font.is_some() {
                resources
                    .fonts()
                    .pair(FONT_RESOURCE_NAME, font_refs.type0_font);
            }
            write_optional_tagged_rgb_resource(&mut resources, prepared_raster.as_ref());
        }

        let mut content = Content::new();
        content.set_fill_cmyk(0.75, 0.25, 0.0, 0.1);
        content.rect(36.0, 36.0, 144.0, 72.0);
        content.fill_nonzero();
        content.save_state();
        content.set_parameters(Name(b"Alpha50"));
        content.set_fill_cmyk(0.0, 0.75, 0.25, 0.05);
        content.rect(108.0, 54.0, 144.0, 72.0);
        content.fill_nonzero();
        content.restore_state();
        if let Some(prepared) = &prepared_font {
            prepared.write_exercise_text(&mut content);
        }
        write_optional_tagged_rgb_placement(&mut content, prepared_raster.as_ref());
        pdf.stream(content_id, &content.finish());
        pdf.ext_graphics(alpha_state_id).non_stroking_alpha(0.5);
        let metadata = crate::PdfCandidateMetadata::validation_probe();
        pdf.metadata(
            metadata_id,
            &metadata.xmp(&file_id, "Publisher V2 candidate writer"),
        );
        pdf.icc_profile(profile_id, output_profile).n(4);
        let mut info = pdf.document_info(info_id);
        metadata.write_info(&mut info);
        info.creator(TextStr("Publisher V2"))
            .producer(TextStr("Publisher V2 candidate writer"))
            .trapped(TrappingStatus::NotTrapped)
            .pair(Name(b"GTS_PDFXVersion"), TextStr("PDF/X-4"));
        info.finish();
        let mut font_exercise_written = false;
        if let Some(prepared) = &prepared_font {
            prepared.write(&mut pdf, font_refs);
            font_exercise_written = true;
        }
        let tagged_rgb_raster_written =
            write_optional_tagged_rgb_raster(&mut pdf, prepared_raster.as_ref());

        let bytes = pdf.finish();
        Ok(CandidatePdf::new(
            bytes,
            complete_structural_evidence(
                fixture,
                profile_facts.sha256,
                prepared_raster.as_ref(),
                tagged_rgb_raster_written,
                font_exercise_written,
            ),
        ))
    }
}

fn complete_structural_evidence(
    fixture: &FixtureContract,
    output_profile_hash: ProfileHash,
    raster: Option<&PreparedTaggedRgbRaster>,
    raster_written: bool,
    font_written: bool,
) -> StructuralEvidence {
    StructuralEvidence {
        evaluation_key: fixture.evaluation_key(),
        boxes: fixture.boxes,
        device_cmyk_swatch_written: true,
        pdfx_metadata_written: true,
        output_intent_embedded: true,
        output_intent_profile_hash: Some(output_profile_hash),
        tagged_rgb_raster_written: raster_written,
        tagged_rgb_source_profile_hash: raster.map(|value| value.source_profile_hash),
        tagged_rgb_raw_pixel_hash: raster.map(|value| value.raw_pixel_hash),
        tagged_rgb_width: raster.map(|value| value.width_u32),
        tagged_rgb_height: raster.map(|value| value.height_u32),
        tagged_rgb_rendering_intent: raster.map(|value| value.rendering_intent),
        tagged_rgb_engine_identity_hash: raster.map(|value| value.engine_identity_hash),
        tagged_rgb_rules_identity_hash: raster.map(|value| value.rules_identity_hash),
        tagged_rgb_worker_executable_hash: raster.map(|value| value.worker_executable_hash),
        converted_cmyk_raster_hash: raster.map(|value| value.converted_hash),
        live_transparency_written: true,
        font_embedded: font_written,
    }
}

fn validate_candidate_inputs(
    fixture: &FixtureContract,
    output_profile: &[u8],
    font: Option<&PreparedFontSubset>,
    raster: Option<&TrustedColorConversion>,
) -> Result<
    (
        crate::OutputIntentFacts,
        Option<PreparedCandidateFont>,
        Option<PreparedTaggedRgbRaster>,
    ),
    CandidateError,
> {
    if !fixture.boxes.is_nested() {
        return Err(CandidateError::InvalidPageBoxes);
    }
    if output_profile.len() > MAX_ICC_PROFILE_BYTES {
        return Err(CandidateError::OutputProfileTooLarge);
    }
    if output_profile.len() != fixture.output_intent.byte_len()
        || ProfileHash::of(output_profile) != fixture.profile_hash()
    {
        return Err(CandidateError::OutputProfileMismatch);
    }
    let profile_facts = fixture.output_intent;
    let prepared_font = font.map(PreparedCandidateFont::prepare);
    if let Some(prepared) = &prepared_font
        && prepared.boundary != fixture.font
    {
        return Err(CandidateError::FontFixtureMismatch);
    }
    let prepared_raster = raster
        .map(|raster| PreparedTaggedRgbRaster::prepare(raster, fixture, output_profile))
        .transpose()?;
    Ok((profile_facts, prepared_font, prepared_raster))
}

struct PreparedCandidateFont {
    subset_bytes: Vec<u8>,
    boundary: FontBoundary,
    postscript_name: Vec<u8>,
    outline_kind: FontOutlineKind,
    metrics: PreparedFontMetrics,
    cid_set: Vec<u8>,
    exercise_cid: u16,
    exercise_unicode: char,
    exercise_code: [u8; 2],
}

struct PreparedTaggedRgbRaster {
    width: i32,
    height: i32,
    width_u32: u32,
    height_u32: u32,
    cmyk_pixels: Vec<u8>,
    source_profile_hash: ProfileHash,
    raw_pixel_hash: ArtifactHash,
    converted_hash: ArtifactHash,
    rendering_intent: ColorRenderingIntent,
    engine_identity_hash: ArtifactHash,
    rules_identity_hash: ArtifactHash,
    worker_executable_hash: ArtifactHash,
}

impl PreparedTaggedRgbRaster {
    fn prepare(
        request: &TrustedColorConversion,
        fixture: &FixtureContract,
        output_profile: &[u8],
    ) -> Result<Self, CandidateError> {
        let width = request.width();
        let height = request.height();
        if width == 0
            || height == 0
            || width > MAX_TAGGED_RASTER_DIMENSION
            || height > MAX_TAGGED_RASTER_DIMENSION
        {
            return Err(CandidateError::TaggedRgbDimensionsInvalid);
        }
        let pixel_count = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(CandidateError::TaggedRgbPixelBudgetExceeded)?;
        let pixel_count = usize::try_from(pixel_count)
            .map_err(|_| CandidateError::TaggedRgbPixelBudgetExceeded)?;
        if pixel_count > MAX_TAGGED_RASTER_PIXELS {
            return Err(CandidateError::TaggedRgbPixelBudgetExceeded);
        }
        let expected_bytes = pixel_count
            .checked_mul(4)
            .ok_or(CandidateError::TaggedRgbPixelBudgetExceeded)?;
        if request.cmyk_pixels().len() != expected_bytes {
            return Err(CandidateError::TaggedRgbPixelLengthMismatch);
        }
        let source_profile_hash = ProfileHash::from_bytes(request.source_profile_hash());
        let raw_pixel_hash = ArtifactHash::from_bytes(request.raw_pixel_hash());
        let output_profile_hash = ProfileHash::from_bytes(request.output_profile_hash());
        let converted_cmyk_hash = ArtifactHash::from_bytes(request.converted_cmyk_hash());
        let engine_identity_hash = ArtifactHash::from_bytes(request.engine_identity_hash());
        let rules_identity_hash = ArtifactHash::from_bytes(request.rules_identity_hash());
        let worker_executable_hash = ArtifactHash::from_bytes(request.worker_executable_hash());
        let rendering_intent = ColorRenderingIntent::RelativeColorimetric;
        let converted_hash = ArtifactHash::of(request.cmyk_pixels());
        let expected_boundary = TaggedRgbBoundary::WorkerAttested {
            source_profile_hash,
            raw_pixel_hash,
            width,
            height,
            rendering_intent,
            engine_identity_hash,
            rules_identity_hash,
            worker_executable_hash,
        };
        if fixture.tagged_rgb != expected_boundary
            || output_profile_hash != fixture.profile_hash()
            || ProfileHash::of(output_profile) != output_profile_hash
            || converted_hash != converted_cmyk_hash
        {
            return Err(CandidateError::TaggedRgbAttestationMismatch);
        }

        Ok(Self {
            width: i32::try_from(width).map_err(|_| CandidateError::TaggedRgbDimensionsInvalid)?,
            height: i32::try_from(height)
                .map_err(|_| CandidateError::TaggedRgbDimensionsInvalid)?,
            width_u32: width,
            height_u32: height,
            cmyk_pixels: crate::raster::compress_raster_stream(
                request.cmyk_pixels(),
                document_core::raster_capacity::MAX_PREPARED_RASTER_CACHE_BYTES,
            )
            .map_err(|_| CandidateError::TaggedRgbPixelBudgetExceeded)?,
            source_profile_hash,
            raw_pixel_hash,
            converted_hash,
            rendering_intent,
            engine_identity_hash,
            rules_identity_hash,
            worker_executable_hash,
        })
    }

    fn write(&self, pdf: &mut Pdf) {
        let mut image = pdf.image_xobject(TAGGED_RGB_IMAGE_ID, &self.cmyk_pixels);
        image.filter(Filter::FlateDecode);
        image.width(self.width).height(self.height);
        image.color_space().device_cmyk();
        image.bits_per_component(8);
        image.finish();
    }
}

fn write_optional_tagged_rgb_resource(
    resources: &mut Resources<'_>,
    raster: Option<&PreparedTaggedRgbRaster>,
) {
    if raster.is_some() {
        resources
            .x_objects()
            .pair(TAGGED_RGB_RESOURCE_NAME, TAGGED_RGB_IMAGE_ID);
    }
}

fn write_optional_tagged_rgb_placement(
    content: &mut Content,
    raster: Option<&PreparedTaggedRgbRaster>,
) {
    if raster.is_some() {
        content.save_state();
        content.transform([72.0, 0.0, 0.0, 72.0, 270.0, 72.0]);
        content.x_object(TAGGED_RGB_RESOURCE_NAME);
        content.restore_state();
    }
}

fn write_optional_tagged_rgb_raster(
    pdf: &mut Pdf,
    raster: Option<&PreparedTaggedRgbRaster>,
) -> bool {
    let Some(raster) = raster else {
        return false;
    };
    raster.write(pdf);
    true
}

struct PreparedFontMetrics {
    bbox: Rect,
    ascent: f32,
    descent: f32,
    cap_height: f32,
    italic_angle: f32,
    flags: FontFlags,
    widths: Vec<f32>,
}

#[derive(Clone, Copy)]
struct FontObjectRefs {
    type0_font: Ref,
    cid_font: Ref,
    descriptor: Ref,
    font_file: Ref,
    cid_set: Ref,
    to_unicode: Ref,
}

impl FontObjectRefs {
    const fn candidate() -> Self {
        Self {
            type0_font: Ref::new(9),
            cid_font: Ref::new(10),
            descriptor: Ref::new(11),
            font_file: Ref::new(12),
            cid_set: Ref::new(13),
            to_unicode: Ref::new(14),
        }
    }
}

impl PreparedCandidateFont {
    #[expect(
        clippy::cast_precision_loss,
        reason = "the worker bounds font metrics to PDF-safe integer ranges and pdf-writer's geometry API is intentionally f32"
    )]
    fn prepare(request: &PreparedFontSubset) -> Self {
        let exercise_cid = request.exercise_cid();
        let exercise_unicode = request.exercise_unicode();
        let exercise_code = exercise_cid.to_be_bytes();
        let mut flags = FontFlags::SYMBOLIC;
        if request.fixed_pitch() {
            flags |= FontFlags::FIXED_PITCH;
        }
        if request.italic() {
            flags |= FontFlags::ITALIC;
        }
        let mut cid_set = vec![0_u8; usize::from(request.glyph_count()).div_ceil(8)];
        for cid in 0..usize::from(request.glyph_count()) {
            cid_set[cid / 8] |= 0x80 >> (cid % 8);
        }
        let source_hash = request.source_sha256();
        let bbox = request.bbox();
        let postscript_name = deterministic_subset_name(source_hash);
        let metrics = PreparedFontMetrics {
            bbox: Rect::new(
                bbox[0] as f32,
                bbox[1] as f32,
                bbox[2] as f32,
                bbox[3] as f32,
            ),
            ascent: request.ascent() as f32,
            descent: request.descent() as f32,
            cap_height: request.cap_height() as f32,
            italic_angle: request.italic_angle_millidegrees() as f32 / 1_000.0,
            flags,
            widths: request.widths().iter().map(|value| *value as f32).collect(),
        };
        Self {
            subset_bytes: request.subset_bytes().to_vec(),
            boundary: FontBoundary::from_prepared(request),
            postscript_name,
            outline_kind: request.outline_kind(),
            metrics,
            cid_set,
            exercise_cid,
            exercise_unicode,
            exercise_code,
        }
    }

    fn write_exercise_text(&self, content: &mut Content) {
        content.save_state();
        content.set_fill_cmyk(0.0, 0.0, 0.0, 1.0);
        content.begin_text();
        content.set_font(FONT_RESOURCE_NAME, 18.0);
        content.next_line(72.0, 180.0);
        content.show(Str(&self.exercise_code));
        content.end_text();
        content.restore_state();
    }

    fn write(&self, pdf: &mut Pdf, refs: FontObjectRefs) {
        let base_font = Name(&self.postscript_name);
        let mut type0_name = self.postscript_name.clone();
        if self.outline_kind == FontOutlineKind::Cff {
            type0_name.extend_from_slice(b"-Identity-H");
        }
        pdf.type0_font(refs.type0_font)
            .base_font(Name(&type0_name))
            .encoding_predefined(Name(b"Identity-H"))
            .descendant_font(refs.cid_font)
            .to_unicode(refs.to_unicode);
        {
            let mut cid_font = pdf.cid_font(refs.cid_font);
            cid_font
                .subtype(match self.outline_kind {
                    FontOutlineKind::TrueType => CidFontType::Type2,
                    FontOutlineKind::Cff => CidFontType::Type0,
                })
                .base_font(base_font)
                .system_info(SystemInfo {
                    registry: Str(b"Adobe"),
                    ordering: Str(b"Identity"),
                    supplement: 0,
                })
                .font_descriptor(refs.descriptor)
                .default_width(1_000.0);
            cid_font
                .widths()
                .consecutive(0, self.metrics.widths.iter().copied());
            if self.outline_kind == FontOutlineKind::TrueType {
                cid_font.cid_to_gid_map_predefined(Name(b"Identity"));
            }
        }
        {
            let mut descriptor = pdf.font_descriptor(refs.descriptor);
            descriptor
                .name(base_font)
                .flags(FontFlags::from_bits_retain(self.metrics.flags.bits()))
                .bbox(self.metrics.bbox)
                .italic_angle(self.metrics.italic_angle)
                .ascent(self.metrics.ascent)
                .descent(self.metrics.descent)
                .cap_height(self.metrics.cap_height)
                .stem_v(80.0)
                .cid_set(refs.cid_set);
            match self.outline_kind {
                FontOutlineKind::TrueType => {
                    descriptor.font_file2(refs.font_file);
                }
                FontOutlineKind::Cff => {
                    descriptor.font_file3(refs.font_file);
                }
            }
        }
        {
            let mut font_stream = pdf.stream(refs.font_file, &self.subset_bytes);
            match self.outline_kind {
                FontOutlineKind::TrueType => {
                    font_stream.pair(
                        Name(b"Length1"),
                        i32::try_from(self.subset_bytes.len())
                            .expect("subsetter output fits PDF stream limits"),
                    );
                }
                FontOutlineKind::Cff => {
                    font_stream.pair(Name(b"Subtype"), Name(b"CIDFontType0C"));
                }
            }
        }
        pdf.stream(refs.cid_set, &self.cid_set);
        let cmap_bytes = crate::to_unicode::cmap(&std::collections::BTreeMap::from([(
            self.exercise_cid,
            vec![self.exercise_unicode],
        )]))
        .expect("one Unicode scalar always fits the ToUnicode destination limit");
        pdf.cmap(refs.to_unicode, cmap_bytes.as_slice());
    }
}

fn deterministic_subset_name(source_hash: [u8; 32]) -> Vec<u8> {
    let mut name = Vec::with_capacity(6 + 1 + 22 + 1 + 12);
    for byte in &source_hash[..6] {
        name.push(b'A' + byte % 26);
    }
    name.extend_from_slice(b"+PublisherV2Exact-");
    for byte in &source_hash[..6] {
        name.extend_from_slice(format!("{byte:02X}").as_bytes());
    }
    name
}

fn as_rect(page_box: PageBox) -> Rect {
    Rect::new(
        micrometres_to_points(page_box.left_um),
        micrometres_to_points(page_box.bottom_um),
        micrometres_to_points(page_box.right_um),
        micrometres_to_points(page_box.top_um),
    )
}

#[allow(clippy::cast_precision_loss)]
fn micrometres_to_points(value: u32) -> f32 {
    value as f32 * 72.0 / 25_400.0
}

#[cfg(test)]
mod tests {
    use color_boundary::TrustedColorBoundary;
    use document_core::Revision;
    use layout_core::ResolvedPublication;

    use super::{CandidateError, CandidatePdfWriter};
    use crate::{
        ArtifactHash, FixtureContract, FontBoundary, IccColorSpace, IccDeviceClass,
        OutputIntentFacts, PreflightBasis, PreflightReport, PreparedFontSubset, ProfileHash,
        TrustedFontBoundary, inspect_output_intent,
    };

    fn synthetic_cmyk_output_profile() -> Vec<u8> {
        let mut profile = vec![0_u8; 132];
        profile[0..4].copy_from_slice(&132_u32.to_be_bytes());
        profile[12..16].copy_from_slice(b"prtr");
        profile[16..20].copy_from_slice(b"CMYK");
        profile[20..24].copy_from_slice(b"Lab ");
        profile[36..40].copy_from_slice(b"acsp");
        profile
    }

    fn fixture() -> FixtureContract {
        FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            OutputIntentFacts::from_test_parts(
                ProfileHash::from_bytes([4; 32]),
                132,
                132,
                IccDeviceClass::Printer,
                IccColorSpace::Cmyk,
                *b"Lab ",
            ),
            FontBoundary::NotExercised,
        )
    }

    #[test]
    fn probe_is_deterministic_and_truthfully_incomplete() {
        let first = CandidatePdfWriter::write_structural_probe(&fixture()).expect("valid boxes");
        let second = CandidatePdfWriter::write_structural_probe(&fixture()).expect("valid boxes");
        assert_eq!(first.bytes(), second.bytes());
        assert!(first.evidence().device_cmyk_swatch_written);
        assert!(!first.evidence().pdfx_metadata_written);
        assert!(!first.evidence().output_intent_embedded);
        assert_eq!(first.evidence().output_intent_profile_hash, None);
        assert!(!first.evidence().font_embedded);
    }

    #[test]
    fn probe_rejects_zero_area_page_boxes() {
        let mut fixture = fixture();
        fixture.boxes.trim.right_um = fixture.boxes.trim.left_um;
        assert_eq!(
            CandidatePdfWriter::write_structural_probe(&fixture),
            Err(CandidateError::InvalidPageBoxes)
        );
    }

    #[test]
    fn validation_candidate_rejects_a_header_only_profile() {
        let profile = synthetic_cmyk_output_profile();
        assert_eq!(
            CandidatePdfWriter::write_pdfx4_candidate_for_validation(&fixture(), &profile),
            Err(CandidateError::OutputProfileMismatch)
        );
    }

    #[test]
    fn validation_candidate_rejects_a_truncated_profile() {
        let profile = synthetic_cmyk_output_profile();
        let truncated = &profile[..127];
        assert_eq!(
            CandidatePdfWriter::write_pdfx4_candidate_for_validation(&fixture(), truncated),
            Err(CandidateError::OutputProfileMismatch)
        );
    }

    #[test]
    fn validation_candidate_embeds_the_exact_prepared_subset() {
        let profile = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        let font = usable_font_fixture();
        let glyphs = mapped_glyphs(['A', 'B']);
        let prepared = prepare_font(&font, &glyphs);
        let fixture = FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            inspect_output_intent(profile).expect("fixture ICC profile is accepted"),
            FontBoundary::from_prepared(&prepared),
        );

        let candidate = CandidatePdfWriter::write_pdfx4_candidate_with_font_for_validation(
            &fixture, profile, &prepared,
        )
        .expect("exact licensed subset embeds");

        assert!(candidate.evidence().font_embedded);
        assert!(
            candidate
                .bytes()
                .windows(prepared.subset_bytes().len())
                .any(|window| window == prepared.subset_bytes()),
            "the PDF must contain the exact subset bytes whose hash is in the fixture"
        );
        let pdf_text = String::from_utf8_lossy(candidate.bytes());
        assert!(pdf_text.contains("/Subtype /Type0"));
        assert!(pdf_text.contains("/Subtype /CIDFontType2"));
        assert!(pdf_text.contains("/FontFile2 12 0 R"));
        assert!(pdf_text.contains("/CIDToGIDMap /Identity"));
        assert!(pdf_text.contains("/PublicationFont 9 0 R"));
        assert!(pdf_text.contains("/ToUnicode 14 0 R"));
        assert!(pdf_text.contains("/CMapType 2 def"));
        assert!(pdf_text.contains("/Encoding /Identity-H"));
        assert!(pdf_text.contains("begincmap"));
        assert!(pdf_text.contains("BT"));
        assert!(pdf_text.contains("Tj"));
    }

    #[test]
    fn validation_candidate_embeds_attested_cff_without_an_opentype_wrapper() {
        let profile = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        let font = include_bytes!("../../layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf");
        let prepared = TrustedFontBoundary::resolve_packaged()
            .prepare(font, 0, &[0, 34])
            .expect("licensed CFF prepares through the confined worker");
        let fixture = FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            inspect_output_intent(profile).expect("fixture ICC profile is accepted"),
            FontBoundary::from_prepared(&prepared),
        );
        let candidate = CandidatePdfWriter::write_pdfx4_candidate_with_font_for_validation(
            &fixture, profile, &prepared,
        )
        .expect("attested CFF program embeds");
        assert_eq!(&prepared.subset_bytes()[..3], b"\x01\x00\x04");
        assert!(
            candidate
                .bytes()
                .windows(prepared.subset_bytes().len())
                .any(|window| window == prepared.subset_bytes())
        );
        let text = String::from_utf8_lossy(candidate.bytes());
        assert!(text.contains("/Subtype /CIDFontType0C"));
        assert!(text.contains("/FontFile3 12 0 R"));
        assert!(!text.contains("/Subtype /OpenType"));
        let name = String::from_utf8(super::deterministic_subset_name(prepared.source_sha256()))
            .expect("generated name is ASCII");
        assert!(text.contains(&format!("/BaseFont /{name}-Identity-H\n")));
        assert!(text.contains(&format!("/BaseFont /{name}\n")));
        assert!(text.contains(&format!("/FontName /{name}\n")));
    }

    #[test]
    fn validation_candidate_rejects_font_bytes_changed_after_inspection() {
        let profile = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        let font = usable_font_fixture();
        let glyphs = mapped_glyphs(['A']);
        let prepared = prepare_font(&font, &glyphs);
        let fixture = FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            inspect_output_intent(profile).expect("fixture ICC profile is accepted"),
            FontBoundary::from_prepared(&prepared),
        );
        let mut changed_font = font;
        changed_font.push(0);
        let changed = prepare_font(&changed_font, &glyphs);

        assert_eq!(
            CandidatePdfWriter::write_pdfx4_candidate_with_font_for_validation(
                &fixture, profile, &changed,
            ),
            Err(CandidateError::FontFixtureMismatch)
        );
    }

    #[test]
    fn validation_candidate_rejects_a_subset_not_bound_to_the_fixture() {
        let profile = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        let font = usable_font_fixture();
        let fixture_glyphs = mapped_glyphs(['A']);
        let candidate_glyphs = mapped_glyphs(['B']);
        let fixture_font = prepare_font(&font, &fixture_glyphs);
        let candidate_font = prepare_font(&font, &candidate_glyphs);
        let fixture = FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            inspect_output_intent(profile).expect("fixture ICC profile is accepted"),
            FontBoundary::from_prepared(&fixture_font),
        );

        assert_eq!(
            CandidatePdfWriter::write_pdfx4_candidate_with_font_for_validation(
                &fixture,
                profile,
                &candidate_font,
            ),
            Err(CandidateError::FontFixtureMismatch)
        );
    }

    #[test]
    fn tagged_rgb_probe_is_converted_embedded_and_hash_bound_deterministically() {
        let output_profile = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        let source_profile = lcms2::Profile::new_srgb()
            .icc()
            .expect("LittleCMS serializes sRGB");
        let font = usable_font_fixture();
        let glyphs = mapped_glyphs(['A']);
        let prepared = prepare_font(&font, &glyphs);
        let raw_pixels = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
        let conversion = TrustedColorBoundary::resolve_packaged()
            .convert(&raw_pixels, 2, 2, &source_profile, output_profile)
            .expect("confined packaged worker converts exact inputs");
        let fixture = FixtureContract::new(
            PreflightBasis::from(ResolvedPublication::for_revision(Revision::initial())),
            inspect_output_intent(output_profile).expect("fixture ICC profile is accepted"),
            FontBoundary::from_prepared(&prepared),
        )
        .with_trusted_color(&conversion);
        let write = || {
            CandidatePdfWriter::write_pdfx4_candidate_with_font_and_trusted_color_for_validation(
                &fixture,
                output_profile,
                &prepared,
                &conversion,
            )
            .expect("tagged RGB probe converts and embeds")
        };

        let first = write();
        let second = write();
        assert_eq!(first.bytes(), second.bytes());
        assert!(first.evidence().tagged_rgb_raster_written);
        assert_eq!(
            first.evidence().tagged_rgb_source_profile_hash,
            Some(ProfileHash::of(&source_profile))
        );
        assert_eq!(
            first.evidence().tagged_rgb_raw_pixel_hash,
            Some(ArtifactHash::of(&raw_pixels))
        );
        assert_eq!(
            first.evidence().converted_cmyk_raster_hash,
            Some(ArtifactHash::of(conversion.cmyk_pixels()))
        );
        assert_eq!(
            first.evidence().tagged_rgb_worker_executable_hash,
            Some(ArtifactHash::from_bytes(
                conversion.worker_executable_hash()
            ))
        );
        assert!(
            PreflightReport::for_candidate(&fixture, &first).ready_for_independent_validation()
        );
        let pdf_text = String::from_utf8_lossy(first.bytes());
        assert!(pdf_text.contains("/TaggedRgbProbe 15 0 R"));
        assert!(pdf_text.contains("/Subtype /Image"));
        assert!(pdf_text.contains("/Width 2"));
        assert!(pdf_text.contains("/Height 2"));
        assert!(pdf_text.contains("/ColorSpace /DeviceCMYK"));
        assert!(pdf_text.contains("/BitsPerComponent 8"));
        assert!(pdf_text.contains("/TaggedRgbProbe Do"));
    }

    fn usable_font_fixture() -> Vec<u8> {
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec()
    }

    fn mapped_glyphs<const N: usize>(characters: [char; N]) -> Vec<u16> {
        let mut glyphs = vec![0];
        glyphs.extend(characters.map(|character| match character {
            'A' => 36,
            'B' => 37,
            _ => panic!("test helper supports only A and B"),
        }));
        glyphs
    }

    fn prepare_font(font: &[u8], glyphs: &[u16]) -> PreparedFontSubset {
        TrustedFontBoundary::resolve_packaged()
            .prepare(font, 0, glyphs)
            .expect("confined font worker prepares exact subset")
    }
}
