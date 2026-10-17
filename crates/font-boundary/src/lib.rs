//! Trust-owning boundary for confined font inspection and publication preparation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    process::ExitStatus,
    thread,
    time::{Duration, Instant, SystemTime},
};

const PROTOCOL_VERSION: u16 = 9;
pub const MAX_FONT_FILE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_FONT_GLYPHS_PER_SUBSET: usize = 16_384;
pub const FONT_ENGINE_IDENTITY: &str =
    "subsetter-0.2.6/ttf-parser-0.25.1/native-type2-path-exact16-enclosure-v1";
pub const FONT_RULES_IDENTITY: &str = "publisher-v2/font-preparation/v9/complete-retained-programs/effective-affine-curve-metrics-v5/cid-matrix-normalization-v1/authored-ascent-facts-v1/cap-height-facts-v1";
const FACE_IDENTITY_DOMAIN: &[u8] = b"publisher-v2/font-face-identity/v1\0";
const GLYPH_SET_DOMAIN: &[u8] = b"publisher-v2/font-glyph-set/v1\0";
const ASCENT_FACTS_DOMAIN: &[u8] = b"publisher-v2/font-ascent-facts/v1\0";
const FONT_FACTS_DOMAIN: &[u8] = b"publisher-v2/font-design-facts/v1\0";
const RIGHTS_DOMAIN: &[u8] = b"publisher-v2/font-rights/v1\0";
const POSTSCRIPT_NAME_DOMAIN: &[u8] = b"publisher-v2/font-postscript-name/v1\0";
#[cfg(not(debug_assertions))]
const MAX_WORKER_BYTES: u64 = 64 * 1024 * 1024;
// Static debug symbols make the harness-free workspace worker much larger
// than the packaged release sidecar. This allowance is absent in release.
#[cfg(debug_assertions)]
const MAX_WORKER_BYTES: u64 = 256 * 1024 * 1024;
const MAX_REPORT_BYTES: u64 = 4 * 1024 * 1024;
const PREPARATION_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FONT_FAMILY_BYTES: usize = 1_024;
const MAX_FONT_FAMILY_CHARS: usize = 256;
const MAX_FONT_FAMILY_NAMES: usize = 256;
const MAX_FONT_COLLECTION_FACES: u32 = 16_384;
#[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
const WORKER_MEMORY_LIMIT: usize = 512 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkerBinding {
    path: PathBuf,
    executable_hash: Hash32,
}

/// A packaged worker identity pinned before any font bytes are inspected.
pub struct TrustedFontBoundary {
    binding: Result<WorkerBinding, FontWorkerError>,
}

impl TrustedFontBoundary {
    #[must_use]
    pub fn resolve_packaged() -> Self {
        Self {
            binding: resolve_worker(),
        }
    }

    /// Inspects one exact face and returns an opaque receipt bound to the exact
    /// source bytes, face index, installed-face metadata, container, and
    /// embedding rights reported by the confined worker.
    ///
    /// # Errors
    ///
    /// Fails closed for empty or oversized input, malformed faces, executable
    /// identity drift, confinement failures, timeouts, or malformed evidence.
    pub fn inspect(
        &self,
        font_bytes: &[u8],
        face_index: u32,
    ) -> Result<InspectedFontFace, FontWorkerError> {
        validate_source(font_bytes)?;
        let binding = self.binding.as_ref().map_err(Clone::clone)?;
        let prepared_worker = PreparedWorker::new(binding)?;
        let files = PreparationFiles::new(font_bytes)?;
        let source_hash = Hash32::of(font_bytes);
        let request = WireRequest::Inspect {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: source_hash.hex(),
            face_index,
        };
        let report = run_worker(&prepared_worker, &files, &request)?;
        let WireReport::Inspect(report) = report else {
            return Err(FontWorkerError::IdentityDrift);
        };
        validate_inspection_report(font_bytes, face_index, &report)?;
        Ok(InspectedFontFace {
            source_bytes: font_bytes.to_vec(),
            source_hash,
            face_index,
            face_identity_hash: face_identity_hash(source_hash, face_index),
            container_kind: report.container_kind.into(),
            face_count: report.face_count,
            family_names: report.family_names,
            weight: report.weight,
            style: report.style.into(),
            italic: report.italic,
            embedding_rights: report.embedding_rights.into(),
            rights_identity_hash: domain_hash(RIGHTS_DOMAIN, report.rights_identity.as_bytes()),
            worker_executable_hash: prepared_worker.executable_hash,
            engine_identity_hash: Hash32::of(FONT_ENGINE_IDENTITY.as_bytes()),
            rules_identity_hash: Hash32::of(FONT_RULES_IDENTITY.as_bytes()),
        })
    }

    /// Revalidates rights and prepares an exact subset in the confined worker.
    ///
    /// # Errors
    ///
    /// Fails closed for resource-limit, permission, executable-identity,
    /// confinement, timeout, malformed-output, or attestation failures.
    pub fn prepare(
        &self,
        font_bytes: &[u8],
        face_index: u32,
        glyph_ids: &[u16],
    ) -> Result<PreparedFontSubset, FontWorkerError> {
        let prepared = self.prepare_with_policy(
            font_bytes,
            face_index,
            glyph_ids,
            WirePreparationPolicy::SubsetOnly,
            None,
            None,
        )?;
        if prepared.preparation_kind != FontPreparationKind::Subset {
            return Err(FontWorkerError::IdentityDrift);
        }
        Ok(PreparedFontSubset { prepared })
    }

    /// Revalidates rights and prepares the exact program required for a
    /// publication: a subset when permitted, otherwise the untouched full font.
    ///
    /// # Errors
    ///
    /// Fails closed for denied rights, unsupported full-font containers,
    /// resource limits, identity drift, confinement failures, or malformed
    /// worker evidence.
    pub fn prepare_publication(
        &self,
        font_bytes: &[u8],
        face_index: u32,
        glyph_ids: &[u16],
    ) -> Result<PreparedPublicationFont, FontWorkerError> {
        self.prepare_with_policy(
            font_bytes,
            face_index,
            glyph_ids,
            WirePreparationPolicy::Publication,
            None,
            None,
        )
    }

    /// Supplies complete source-GID roles excluded as accented-only characters
    /// from PDF Ascent. An empty set affirmatively includes every retained role.
    /// Facts cover all retained components, and every glyph for full embedding;
    /// valid nonretained source GIDs may also be included. Native checks validate
    /// identity and transport, not the semantic truth of authored classification.
    ///
    /// # Errors
    /// Rejects GID0, duplicate or unsorted facts, out-of-range source GIDs,
    /// and every existing publication preparation failure.
    pub fn prepare_publication_with_ascent_facts(
        &self,
        font_bytes: &[u8],
        face_index: u32,
        glyph_ids: &[u16],
        accented_source_glyph_ids: &[u16],
    ) -> Result<PreparedPublicationFont, FontWorkerError> {
        self.prepare_publication_with_font_facts(
            font_bytes,
            face_index,
            glyph_ids,
            Some(accented_source_glyph_ids),
            None,
        )
    }

    /// Supplies independent source-author facts for unresolved descriptor design.
    /// `CapHeight` is in final PDF glyph units relative to the transformed baseline.
    /// None requests inference; Some(0) asserts a zero/no-capital design. Signed
    /// zero is canonicalized to +0. Native checks cannot prove semantic truth.
    ///
    /// # Errors
    /// Rejects malformed accent sets, nonfinite/out-of-range cap values, a cap
    /// value conflicting with a computable metric, and existing preparation errors.
    pub fn prepare_publication_with_font_facts(
        &self,
        font_bytes: &[u8],
        face_index: u32,
        glyph_ids: &[u16],
        accented_source_glyph_ids: Option<&[u16]>,
        cap_height_pdf_units: Option<f32>,
    ) -> Result<PreparedPublicationFont, FontWorkerError> {
        self.prepare_with_policy(
            font_bytes,
            face_index,
            glyph_ids,
            WirePreparationPolicy::Publication,
            accented_source_glyph_ids,
            cap_height_pdf_units,
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One preparation handshake validates the exact report before minting opaque authority"
    )]
    fn prepare_with_policy(
        &self,
        font_bytes: &[u8],
        face_index: u32,
        glyph_ids: &[u16],
        preparation_policy: WirePreparationPolicy,
        ascent_facts: Option<&[u16]>,
        cap_fact: Option<f32>,
    ) -> Result<PreparedPublicationFont, FontWorkerError> {
        let glyph_ids = validate_inputs(font_bytes, glyph_ids)?;
        let cap_fact = cap_fact.map(validate_cap_fact).transpose()?;
        if let Some(facts) = ascent_facts {
            validate_ascent_facts_shape(facts)?;
        }
        let binding = self.binding.as_ref().map_err(Clone::clone)?;
        let prepared_worker = PreparedWorker::new(binding)?;
        let files = PreparationFiles::new(font_bytes)?;
        let source_hash = Hash32::of(font_bytes);
        let glyph_hash = glyph_set_hash(&glyph_ids);
        let expected_ascent_hash =
            ascent_facts.map(|gids| ascent_facts_hash(source_hash, face_index, gids));
        let expected_font_hash = font_facts_hash(source_hash, face_index, ascent_facts, cap_fact);
        let request = WireRequest::Prepare {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: source_hash.hex(),
            prepared_output_path: files.prepared.display().to_string(),
            preparation_policy,
            face_index,
            glyph_ids: glyph_ids.clone(),
            accented_source_glyph_ids: ascent_facts.map(<[u16]>::to_vec),
            expected_ascent_facts_sha256: expected_ascent_hash.map(Hash32::hex),
            cap_height_pdf_units: cap_fact,
            expected_font_facts_sha256: expected_font_hash.map(Hash32::hex),
            expected_glyph_set_sha256: glyph_hash.hex(),
        };
        let report = run_worker(&prepared_worker, &files, &request)?;
        let WireReport::Prepare(report) = report else {
            return Err(FontWorkerError::IdentityDrift);
        };
        let expected_face_hash = face_identity_hash(source_hash, face_index);
        let expected_engine_hash = Hash32::of(FONT_ENGINE_IDENTITY.as_bytes());
        let expected_rules_hash = Hash32::of(FONT_RULES_IDENTITY.as_bytes());
        let expected_rights_hash = domain_hash(RIGHTS_DOMAIN, report.rights_identity.as_bytes());
        let expected_postscript_name_hash =
            domain_hash(POSTSCRIPT_NAME_DOMAIN, report.postscript_name.as_bytes());
        let identity_valid = report.protocol_version == PROTOCOL_VERSION
            && report.source_sha256 == source_hash.hex()
            && report.face_index == face_index
            && report.face_identity_sha256 == expected_face_hash.hex()
            && report.postscript_name_sha256 == expected_postscript_name_hash.hex()
            && valid_postscript_name(&report.postscript_name)
            && report.glyph_set_sha256 == glyph_hash.hex()
            && report.engine_identity == FONT_ENGINE_IDENTITY
            && report.engine_identity_sha256 == expected_engine_hash.hex()
            && report.rules_identity == FONT_RULES_IDENTITY
            && report.rules_identity_sha256 == expected_rules_hash.hex()
            && report.rights_identity_sha256 == expected_rights_hash.hex()
            && valid_rights_identity(&report.rights_identity, report.preparation_kind)
            && (!matches!(preparation_policy, WirePreparationPolicy::SubsetOnly)
                || report.preparation_kind == FontPreparationKind::Subset);
        if !identity_valid {
            #[cfg(test)]
            eprintln!("font worker identity drift: {report:#?}");
            return Err(FontWorkerError::IdentityDrift);
        }
        validate_ascent_facts_binding(&report, expected_ascent_hash)?;
        validate_font_facts_binding(&report, expected_font_hash, cap_fact)?;
        validate_report(&report, &glyph_ids)?;
        let prepared_bytes = read_exact_bounded(&files.prepared, report.prepared_byte_length)?;
        let prepared_hash = Hash32::of(&prepared_bytes);
        if report.prepared_sha256 != prepared_hash.hex()
            || (report.preparation_kind == FontPreparationKind::Full
                && prepared_hash != source_hash)
        {
            return Err(FontWorkerError::IdentityDrift);
        }
        Ok(PreparedPublicationFont {
            prepared_bytes,
            preparation_kind: report.preparation_kind,
            source_hash,
            face_index,
            face_identity_hash: expected_face_hash,
            postscript_name: report.postscript_name,
            postscript_name_hash: expected_postscript_name_hash,
            glyph_set_hash: glyph_hash,
            used_glyph_count: report.used_glyph_count,
            embedded_glyph_count: report.embedded_glyph_count,
            prepared_hash,
            rights_identity_hash: expected_rights_hash,
            worker_executable_hash: prepared_worker.executable_hash,
            engine_identity_hash: expected_engine_hash,
            rules_identity_hash: expected_rules_hash,
            outline_kind: report.outline_kind,
            bbox: report.bbox,
            ascent: report.ascent,
            descent: report.descent,
            cap_height: report.cap_height,
            italic_angle_millidegrees: report.italic_angle_millidegrees,
            fixed_pitch: report.fixed_pitch,
            italic: report.italic,
            widths: report.widths,
            requires_ascent_facts: report.requires_ascent_facts,
            ascent_facts_hash: expected_ascent_hash,
            requires_cap_height_fact: report.requires_cap_height_fact,
            font_facts_hash: expected_font_hash,
            pdf_metrics: report.pdf_metrics.into(),
            remapped_glyphs: report.remapped_glyphs,
            exercise_cid: report.exercise_cid,
            exercise_unicode: char::from_u32(report.exercise_unicode)
                .ok_or(FontWorkerError::IdentityDrift)?,
        })
    }
}

/// Opaque proof that exact source bytes and one selected face crossed the
/// confined inspection boundary and matched the returned metadata and rights.
///
/// This type has no public constructor or deserializer. Downstream code can
/// consume its exact bytes and facts, but cannot mint trusted inspection proof.
///
/// ```compile_fail
/// use font_boundary::InspectedFontFace;
/// let forged = InspectedFontFace { source_bytes: Vec::new() };
/// ```
pub struct InspectedFontFace {
    source_bytes: Vec<u8>,
    source_hash: Hash32,
    face_index: u32,
    face_identity_hash: Hash32,
    container_kind: FontContainerKind,
    face_count: u32,
    family_names: Vec<String>,
    weight: u16,
    style: FontFaceStyle,
    italic: bool,
    embedding_rights: FontEmbeddingRights,
    rights_identity_hash: Hash32,
    worker_executable_hash: Hash32,
    engine_identity_hash: Hash32,
    rules_identity_hash: Hash32,
}

impl InspectedFontFace {
    #[must_use]
    pub fn source_bytes(&self) -> &[u8] {
        &self.source_bytes
    }
    #[must_use]
    pub fn into_source_bytes(self) -> Vec<u8> {
        self.source_bytes
    }
    #[must_use]
    pub const fn source_sha256(&self) -> [u8; 32] {
        self.source_hash.0
    }
    #[must_use]
    pub const fn face_index(&self) -> u32 {
        self.face_index
    }
    #[must_use]
    pub const fn face_identity_sha256(&self) -> [u8; 32] {
        self.face_identity_hash.0
    }
    #[must_use]
    pub const fn container_kind(&self) -> FontContainerKind {
        self.container_kind
    }
    #[must_use]
    pub const fn face_count(&self) -> u32 {
        self.face_count
    }
    #[must_use]
    pub fn family_name(&self) -> &str {
        &self.family_names[0]
    }
    #[must_use]
    pub fn family_names(&self) -> &[String] {
        &self.family_names
    }
    #[must_use]
    pub const fn weight(&self) -> u16 {
        self.weight
    }
    #[must_use]
    pub const fn style(&self) -> FontFaceStyle {
        self.style
    }
    #[must_use]
    pub const fn italic(&self) -> bool {
        self.italic
    }
    #[must_use]
    pub const fn embedding_rights(&self) -> FontEmbeddingRights {
        self.embedding_rights
    }
    #[must_use]
    pub const fn rights_identity_sha256(&self) -> [u8; 32] {
        self.rights_identity_hash.0
    }
    #[must_use]
    pub const fn worker_executable_sha256(&self) -> [u8; 32] {
        self.worker_executable_hash.0
    }
    #[must_use]
    pub const fn engine_identity_sha256(&self) -> [u8; 32] {
        self.engine_identity_hash.0
    }
    #[must_use]
    pub const fn rules_identity_sha256(&self) -> [u8; 32] {
        self.rules_identity_hash.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontContainerKind {
    SingleFont,
    Collection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontFaceStyle {
    Normal,
    Italic,
    Oblique,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontEmbeddingLicense {
    Installable,
    PreviewAndPrint,
    Editable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontEmbeddingUnknownReason {
    FaceUnavailableOrMalformed,
    MissingOrMalformedOs2,
    InvalidFsType,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontEmbeddingRights {
    SubsetAllowed {
        license: FontEmbeddingLicense,
        fs_type: u16,
    },
    FullFontRequired {
        license: FontEmbeddingLicense,
        fs_type: u16,
    },
    Restricted {
        fs_type: u16,
    },
    BitmapOnly {
        license: FontEmbeddingLicense,
        fs_type: u16,
    },
    Unknown {
        reason: FontEmbeddingUnknownReason,
        fs_type: Option<u16>,
    },
}

/// Opaque proof that exact font bytes crossed the confined worker boundary and
/// were prepared according to their publication rights.
///
/// This type has no public constructor or deserializer. Downstream code can
/// embed its prepared bytes and bind its identity, but cannot mint proof.
///
/// ```compile_fail
/// use font_boundary::PreparedPublicationFont;
/// let forged = PreparedPublicationFont { prepared_bytes: Vec::new() };
/// ```
#[expect(
    clippy::struct_excessive_bools,
    reason = "Attested style and descriptor authority are independent observations"
)]
pub struct PreparedPublicationFont {
    prepared_bytes: Vec<u8>,
    preparation_kind: FontPreparationKind,
    source_hash: Hash32,
    face_index: u32,
    face_identity_hash: Hash32,
    postscript_name: String,
    postscript_name_hash: Hash32,
    glyph_set_hash: Hash32,
    used_glyph_count: u16,
    embedded_glyph_count: u16,
    prepared_hash: Hash32,
    rights_identity_hash: Hash32,
    worker_executable_hash: Hash32,
    engine_identity_hash: Hash32,
    rules_identity_hash: Hash32,
    outline_kind: FontOutlineKind,
    bbox: [i32; 4],
    ascent: i32,
    descent: i32,
    cap_height: i32,
    italic_angle_millidegrees: i32,
    fixed_pitch: bool,
    italic: bool,
    widths: Vec<i32>,
    requires_ascent_facts: bool,
    ascent_facts_hash: Option<Hash32>,
    requires_cap_height_fact: bool,
    font_facts_hash: Option<Hash32>,
    pdf_metrics: PdfFontMetrics,
    remapped_glyphs: Vec<RemappedGlyph>,
    exercise_cid: u16,
    exercise_unicode: char,
}

impl PreparedPublicationFont {
    /// True when automatic evidence cannot establish PDF Ascent independently
    /// of unknown glyph roles. Ordinary output retains its compatibility
    /// heuristic; positive print generation requires authored facts.
    #[must_use]
    pub const fn requires_ascent_facts(&self) -> bool {
        self.requires_ascent_facts
    }
    /// Identity of the explicit complete facts, bound to source bytes and face.
    /// None requests inference; an explicit empty set has a present hash.
    #[must_use]
    pub const fn ascent_facts_sha256(&self) -> Option<[u8; 32]> {
        match self.ascent_facts_hash {
            Some(hash) => Some(hash.0),
            None => None,
        }
    }

    /// True when no source declaration, established reference or absence proof
    /// establishes `CapHeight`. Positive print generation requires an author fact.
    #[must_use]
    pub const fn requires_cap_height_fact(&self) -> bool {
        self.requires_cap_height_fact
    }
    /// Combined facts identity. Optional-field presence and canonical binary32
    /// cap bits are bound to source bytes and face; legacy Ascent identity is unchanged.
    #[must_use]
    pub const fn font_facts_sha256(&self) -> Option<[u8; 32]> {
        match self.font_facts_hash {
            Some(hash) => Some(hash.0),
            None => None,
        }
    }

    /// Precise attested metrics for PDF serialization, in 1000-em units.
    /// Existing integer accessors remain available for compatibility.
    #[must_use]
    pub const fn pdf_metrics(&self) -> &PdfFontMetrics {
        &self.pdf_metrics
    }
    /// Exact attested PDF program: TrueType SFNT or raw CID-keyed CFF,
    /// according to `outline_kind`. All parsing stays in the font worker.
    #[must_use]
    pub fn prepared_bytes(&self) -> &[u8] {
        &self.prepared_bytes
    }
    #[must_use]
    pub const fn preparation_kind(&self) -> FontPreparationKind {
        self.preparation_kind
    }
    #[must_use]
    pub const fn source_sha256(&self) -> [u8; 32] {
        self.source_hash.0
    }
    #[must_use]
    pub const fn face_index(&self) -> u32 {
        self.face_index
    }
    #[must_use]
    pub const fn face_identity_sha256(&self) -> [u8; 32] {
        self.face_identity_hash.0
    }
    #[must_use]
    pub fn postscript_name(&self) -> &str {
        &self.postscript_name
    }
    #[must_use]
    pub const fn postscript_name_sha256(&self) -> [u8; 32] {
        self.postscript_name_hash.0
    }
    #[must_use]
    pub const fn glyph_set_sha256(&self) -> [u8; 32] {
        self.glyph_set_hash.0
    }
    #[must_use]
    pub const fn used_glyph_count(&self) -> u16 {
        self.used_glyph_count
    }
    #[must_use]
    pub const fn embedded_glyph_count(&self) -> u16 {
        self.embedded_glyph_count
    }
    #[must_use]
    pub const fn prepared_sha256(&self) -> [u8; 32] {
        self.prepared_hash.0
    }
    #[must_use]
    pub const fn rights_identity_sha256(&self) -> [u8; 32] {
        self.rights_identity_hash.0
    }
    #[must_use]
    pub const fn worker_executable_sha256(&self) -> [u8; 32] {
        self.worker_executable_hash.0
    }
    #[must_use]
    pub const fn engine_identity_sha256(&self) -> [u8; 32] {
        self.engine_identity_hash.0
    }
    #[must_use]
    pub const fn rules_identity_sha256(&self) -> [u8; 32] {
        self.rules_identity_hash.0
    }
    #[must_use]
    pub const fn outline_kind(&self) -> FontOutlineKind {
        self.outline_kind
    }
    #[must_use]
    pub const fn bbox(&self) -> [i32; 4] {
        self.bbox
    }
    #[must_use]
    pub const fn ascent(&self) -> i32 {
        self.ascent
    }
    #[must_use]
    pub const fn descent(&self) -> i32 {
        self.descent
    }
    #[must_use]
    pub const fn cap_height(&self) -> i32 {
        self.cap_height
    }
    #[must_use]
    pub const fn italic_angle_millidegrees(&self) -> i32 {
        self.italic_angle_millidegrees
    }
    #[must_use]
    pub const fn fixed_pitch(&self) -> bool {
        self.fixed_pitch
    }
    #[must_use]
    pub const fn italic(&self) -> bool {
        self.italic
    }
    #[must_use]
    pub fn widths(&self) -> &[i32] {
        &self.widths
    }
    #[must_use]
    pub fn remapped_glyphs(&self) -> &[RemappedGlyph] {
        &self.remapped_glyphs
    }
    #[must_use]
    pub const fn exercise_cid(&self) -> u16 {
        self.exercise_cid
    }
    #[must_use]
    pub const fn exercise_unicode(&self) -> char {
        self.exercise_unicode
    }
}

/// Metrics derived inside confinement from the exact prepared outlines and
/// original-face design information. A certified numeric bbox enclosure is
/// encoded outward in whole PDF glyph units; uncertainty can add an adjacent
/// unit. This is not a mathematically minimal integer rectangle claim. Other
/// metrics retain fractional precision.
pub struct PdfFontMetrics {
    bbox: [f32; 4],
    ascent: f32,
    descent: f32,
    cap_height: Option<f32>,
    italic_angle: f32,
    stem_v: f32,
    widths: Vec<f32>,
}
impl PdfFontMetrics {
    #[must_use]
    pub const fn bbox(&self) -> [f32; 4] {
        self.bbox
    }
    #[must_use]
    pub const fn ascent(&self) -> f32 {
        self.ascent
    }
    #[must_use]
    pub const fn descent(&self) -> f32 {
        self.descent
    }
    #[must_use]
    pub const fn cap_height(&self) -> Option<f32> {
        self.cap_height
    }
    #[must_use]
    pub const fn stem_v(&self) -> f32 {
        self.stem_v
    }
    #[must_use]
    pub const fn italic_angle(&self) -> f32 {
        self.italic_angle
    }
    #[must_use]
    pub fn widths(&self) -> &[f32] {
        &self.widths
    }
}
impl From<WirePdfFontMetrics> for PdfFontMetrics {
    fn from(value: WirePdfFontMetrics) -> Self {
        Self {
            bbox: value.bbox,
            ascent: value.ascent,
            descent: value.descent,
            cap_height: value.cap_height,
            italic_angle: value.italic_angle,
            stem_v: value.stem_v,
            widths: value.widths,
        }
    }
}

/// Opaque subset-only compatibility wrapper used by lower-level callers.
pub struct PreparedFontSubset {
    prepared: PreparedPublicationFont,
}

impl std::ops::Deref for PreparedFontSubset {
    type Target = PreparedPublicationFont;

    fn deref(&self) -> &Self::Target {
        &self.prepared
    }
}

impl PreparedFontSubset {
    #[must_use]
    pub fn subset_bytes(&self) -> &[u8] {
        self.prepared.prepared_bytes()
    }

    #[must_use]
    pub const fn glyph_count(&self) -> u16 {
        self.prepared.embedded_glyph_count()
    }

    #[must_use]
    pub const fn subset_sha256(&self) -> [u8; 32] {
        self.prepared.prepared_sha256()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FontPreparationKind {
    Subset,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FontOutlineKind {
    /// The prepared program is a TrueType SFNT for `FontFile2`.
    TrueType,
    /// The prepared program is raw CFF with identity CIDs for `CIDFontType0C`.
    Cff,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
pub struct RemappedGlyph {
    source_glyph_id: u16,
    cid: u16,
}

impl RemappedGlyph {
    #[must_use]
    pub const fn source_glyph_id(self) -> u16 {
        self.source_glyph_id
    }
    #[must_use]
    pub const fn cid(self) -> u16 {
        self.cid
    }
}

/// A worker-confirmed font-rights state that prevents the requested preparation.
///
/// Keeping this typed prevents integrity and protocol failures from being
/// mistaken for an author-fixable licensing decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontEmbeddingDenial {
    FullFontRequired,
    Restricted,
    BitmapOnly,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FontWorkerError {
    NotInstalled,
    InvalidExecutable(String),
    ExecutableIdentityDrift,
    SourceTooLarge { actual: usize, maximum: usize },
    TooManyGlyphs { actual: usize, maximum: usize },
    DuplicateGlyph(u16),
    NoGlyphs,
    InvalidCapHeightFact,
    InvalidAscentFacts { reason: &'static str },
    ResourceLimit,
    Io(String),
    SpawnFailed(String),
    ConfinementFailed(String),
    TimedOut,
    EmbeddingDenied(FontEmbeddingDenial),
    WorkerRejected { code: String, detail: String },
    MalformedReport(String),
    IdentityDrift,
}

#[derive(Serialize)]
#[serde(
    tag = "operation",
    rename_all = "kebab-case",
    rename_all_fields = "snake_case"
)]
enum WireRequest {
    Inspect {
        protocol_version: u16,
        font_path: String,
        expected_source_sha256: String,
        face_index: u32,
    },
    Prepare {
        protocol_version: u16,
        font_path: String,
        expected_source_sha256: String,
        prepared_output_path: String,
        preparation_policy: WirePreparationPolicy,
        face_index: u32,
        glyph_ids: Vec<u16>,
        expected_glyph_set_sha256: String,
        accented_source_glyph_ids: Option<Vec<u16>>,
        expected_ascent_facts_sha256: Option<String>,
        cap_height_pdf_units: Option<f32>,
        expected_font_facts_sha256: Option<String>,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum WirePreparationPolicy {
    SubsetOnly,
    Publication,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", content = "report", rename_all = "kebab-case")]
enum WireReport {
    Inspect(Box<WireInspectionReport>),
    Prepare(Box<WirePreparationReport>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireInspectionReport {
    protocol_version: u16,
    source_sha256: String,
    source_byte_length: usize,
    face_index: u32,
    face_identity_sha256: String,
    container_kind: WireContainerKind,
    face_count: u32,
    family_names: Vec<String>,
    weight: u16,
    style: WireFaceStyle,
    italic: bool,
    embedding_rights: WireEmbeddingRights,
    rights_identity: String,
    rights_identity_sha256: String,
    engine_identity: String,
    engine_identity_sha256: String,
    rules_identity: String,
    rules_identity_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Wire fields independently describe font style and descriptor authority"
)]
struct WirePreparationReport {
    protocol_version: u16,
    source_sha256: String,
    face_index: u32,
    face_identity_sha256: String,
    postscript_name: String,
    postscript_name_sha256: String,
    glyph_set_sha256: String,
    preparation_kind: FontPreparationKind,
    used_glyph_count: u16,
    embedded_glyph_count: u16,
    prepared_sha256: String,
    prepared_byte_length: usize,
    rights_identity: String,
    rights_identity_sha256: String,
    engine_identity: String,
    engine_identity_sha256: String,
    rules_identity: String,
    rules_identity_sha256: String,
    outline_kind: FontOutlineKind,
    bbox: [i32; 4],
    ascent: i32,
    descent: i32,
    cap_height: i32,
    italic_angle_millidegrees: i32,
    fixed_pitch: bool,
    italic: bool,
    widths: Vec<i32>,
    requires_ascent_facts: bool,
    ascent_facts_sha256: Option<String>,
    requires_cap_height_fact: bool,
    font_facts_sha256: Option<String>,
    pdf_metrics: WirePdfFontMetrics,
    remapped_glyphs: Vec<RemappedGlyph>,
    exercise_cid: u16,
    exercise_unicode: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePdfFontMetrics {
    bbox: [f32; 4],
    ascent: f32,
    descent: f32,
    cap_height: Option<f32>,
    italic_angle: f32,
    stem_v: f32,
    widths: Vec<f32>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum WireContainerKind {
    SingleFont,
    Collection,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum WireFaceStyle {
    Normal,
    Italic,
    Oblique,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum WireEmbeddingLicense {
    Installable,
    PreviewAndPrint,
    Editable,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum WireUnknownRightsReason {
    FaceUnavailableOrMalformed,
    MissingOrMalformedOs2,
    InvalidFsType,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "kebab-case",
    rename_all_fields = "snake_case"
)]
enum WireEmbeddingRights {
    SubsetAllowed {
        license: WireEmbeddingLicense,
        fs_type: u16,
    },
    FullFontRequired {
        license: WireEmbeddingLicense,
        fs_type: u16,
    },
    Restricted {
        fs_type: u16,
    },
    BitmapOnly {
        license: WireEmbeddingLicense,
        fs_type: u16,
    },
    Unknown {
        reason: WireUnknownRightsReason,
        fs_type: Option<u16>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFailure {
    protocol_version: u16,
    error: String,
    detail: String,
}

fn classify_worker_failure(failure: WireFailure) -> FontWorkerError {
    if failure.error == "embedding-denied" {
        let denial = match failure.detail.as_str() {
            "full-font-required" => Some(FontEmbeddingDenial::FullFontRequired),
            "restricted" => Some(FontEmbeddingDenial::Restricted),
            "bitmap-only" => Some(FontEmbeddingDenial::BitmapOnly),
            "unknown" => Some(FontEmbeddingDenial::Unknown),
            _ => None,
        };
        if let Some(denial) = denial {
            return FontWorkerError::EmbeddingDenied(denial);
        }
    }
    FontWorkerError::WorkerRejected {
        code: failure.error,
        detail: failure.detail,
    }
}

impl From<WireContainerKind> for FontContainerKind {
    fn from(value: WireContainerKind) -> Self {
        match value {
            WireContainerKind::SingleFont => Self::SingleFont,
            WireContainerKind::Collection => Self::Collection,
        }
    }
}

impl From<WireFaceStyle> for FontFaceStyle {
    fn from(value: WireFaceStyle) -> Self {
        match value {
            WireFaceStyle::Normal => Self::Normal,
            WireFaceStyle::Italic => Self::Italic,
            WireFaceStyle::Oblique => Self::Oblique,
        }
    }
}

impl From<WireEmbeddingLicense> for FontEmbeddingLicense {
    fn from(value: WireEmbeddingLicense) -> Self {
        match value {
            WireEmbeddingLicense::Installable => Self::Installable,
            WireEmbeddingLicense::PreviewAndPrint => Self::PreviewAndPrint,
            WireEmbeddingLicense::Editable => Self::Editable,
        }
    }
}

impl From<WireUnknownRightsReason> for FontEmbeddingUnknownReason {
    fn from(value: WireUnknownRightsReason) -> Self {
        match value {
            WireUnknownRightsReason::FaceUnavailableOrMalformed => Self::FaceUnavailableOrMalformed,
            WireUnknownRightsReason::MissingOrMalformedOs2 => Self::MissingOrMalformedOs2,
            WireUnknownRightsReason::InvalidFsType => Self::InvalidFsType,
        }
    }
}

impl From<WireEmbeddingRights> for FontEmbeddingRights {
    fn from(value: WireEmbeddingRights) -> Self {
        match value {
            WireEmbeddingRights::SubsetAllowed { license, fs_type } => Self::SubsetAllowed {
                license: license.into(),
                fs_type,
            },
            WireEmbeddingRights::FullFontRequired { license, fs_type } => Self::FullFontRequired {
                license: license.into(),
                fs_type,
            },
            WireEmbeddingRights::Restricted { fs_type } => Self::Restricted { fs_type },
            WireEmbeddingRights::BitmapOnly { license, fs_type } => Self::BitmapOnly {
                license: license.into(),
                fs_type,
            },
            WireEmbeddingRights::Unknown { reason, fs_type } => Self::Unknown {
                reason: reason.into(),
                fs_type,
            },
        }
    }
}

fn validate_inspection_report(
    font: &[u8],
    face_index: u32,
    report: &WireInspectionReport,
) -> Result<(), FontWorkerError> {
    let source_hash = Hash32::of(font);
    let expected_rights_identity = inspection_rights_identity(report.embedding_rights);
    let expected_rights_hash = domain_hash(RIGHTS_DOMAIN, expected_rights_identity.as_bytes());
    let valid_container = match report.container_kind {
        WireContainerKind::SingleFont => report.face_count == 1 && face_index == 0,
        WireContainerKind::Collection => {
            report.face_count > 0
                && report.face_count <= MAX_FONT_COLLECTION_FACES
                && face_index < report.face_count
        }
    };
    let valid_style = !matches!(report.style, WireFaceStyle::Italic) || report.italic;
    let valid = report.protocol_version == PROTOCOL_VERSION
        && report.source_sha256 == source_hash.hex()
        && report.source_byte_length == font.len()
        && report.face_index == face_index
        && report.face_identity_sha256 == face_identity_hash(source_hash, face_index).hex()
        && valid_container
        && valid_family_names(&report.family_names)
        && (1..=1_000).contains(&report.weight)
        && valid_style
        && report.rights_identity == expected_rights_identity
        && report.rights_identity_sha256 == expected_rights_hash.hex()
        && report.engine_identity == FONT_ENGINE_IDENTITY
        && report.engine_identity_sha256 == Hash32::of(FONT_ENGINE_IDENTITY.as_bytes()).hex()
        && report.rules_identity == FONT_RULES_IDENTITY
        && report.rules_identity_sha256 == Hash32::of(FONT_RULES_IDENTITY.as_bytes()).hex();
    if valid {
        Ok(())
    } else {
        Err(FontWorkerError::IdentityDrift)
    }
}

fn valid_family_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_FONT_FAMILY_BYTES
        && value.chars().count() <= MAX_FONT_FAMILY_CHARS
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_family_names(values: &[String]) -> bool {
    !values.is_empty()
        && values.len() <= MAX_FONT_FAMILY_NAMES
        && values.iter().all(|value| valid_family_name(value))
        && values.windows(2).all(|pair| pair[0] < pair[1])
}

fn inspection_rights_identity(rights: WireEmbeddingRights) -> String {
    match rights {
        WireEmbeddingRights::SubsetAllowed { license, fs_type } => format!(
            "subset-allowed/{}/{fs_type:04x}",
            inspection_license_name(license)
        ),
        WireEmbeddingRights::FullFontRequired { license, fs_type } => format!(
            "full-font-required/{}/{fs_type:04x}",
            inspection_license_name(license)
        ),
        WireEmbeddingRights::Restricted { fs_type } => format!("restricted/{fs_type:04x}"),
        WireEmbeddingRights::BitmapOnly { license, fs_type } => format!(
            "bitmap-only/{}/{fs_type:04x}",
            inspection_license_name(license)
        ),
        WireEmbeddingRights::Unknown { reason, fs_type } => format!(
            "unknown/{}/{}",
            match reason {
                WireUnknownRightsReason::FaceUnavailableOrMalformed => {
                    "face-unavailable-or-malformed"
                }
                WireUnknownRightsReason::MissingOrMalformedOs2 => "missing-or-malformed-os2",
                WireUnknownRightsReason::InvalidFsType => "invalid-fs-type",
            },
            fs_type.map_or_else(|| "none".to_owned(), |value| format!("{value:04x}"))
        ),
    }
}

const fn inspection_license_name(license: WireEmbeddingLicense) -> &'static str {
    match license {
        WireEmbeddingLicense::Installable => "installable",
        WireEmbeddingLicense::PreviewAndPrint => "preview-and-print",
        WireEmbeddingLicense::Editable => "editable",
    }
}

fn validate_source(font: &[u8]) -> Result<(), FontWorkerError> {
    if font.is_empty() || font.len() > MAX_FONT_FILE_BYTES {
        return Err(FontWorkerError::SourceTooLarge {
            actual: font.len(),
            maximum: MAX_FONT_FILE_BYTES,
        });
    }
    Ok(())
}

fn validate_inputs(font: &[u8], glyphs: &[u16]) -> Result<Vec<u16>, FontWorkerError> {
    validate_source(font)?;
    if glyphs.is_empty() {
        return Err(FontWorkerError::NoGlyphs);
    }
    if glyphs.len() > MAX_FONT_GLYPHS_PER_SUBSET {
        return Err(FontWorkerError::TooManyGlyphs {
            actual: glyphs.len(),
            maximum: MAX_FONT_GLYPHS_PER_SUBSET,
        });
    }
    let unique = glyphs.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != glyphs.len() {
        let duplicate = glyphs
            .iter()
            .copied()
            .find(|glyph| {
                glyphs
                    .iter()
                    .filter(|candidate| *candidate == glyph)
                    .count()
                    > 1
            })
            .unwrap_or(0);
        return Err(FontWorkerError::DuplicateGlyph(duplicate));
    }
    Ok(unique.into_iter().collect())
}

fn validate_report(
    report: &WirePreparationReport,
    requested: &[u16],
) -> Result<(), FontWorkerError> {
    let used_glyph_count = usize::from(report.used_glyph_count);
    let embedded_glyph_count = usize::from(report.embedded_glyph_count);
    if used_glyph_count == 0
        || used_glyph_count != requested.len()
        || embedded_glyph_count == 0
        || report.prepared_byte_length == 0
        || report.prepared_byte_length > MAX_FONT_FILE_BYTES
        || report.widths.len() != embedded_glyph_count
        || report.pdf_metrics.widths.len() != embedded_glyph_count
        || report
            .pdf_metrics
            .bbox
            .iter()
            .chain([
                &report.pdf_metrics.ascent,
                &report.pdf_metrics.descent,
                &report.pdf_metrics.stem_v,
                &report.pdf_metrics.italic_angle,
            ])
            .chain(report.pdf_metrics.cap_height.iter())
            .chain(report.pdf_metrics.widths.iter())
            .any(|value| !value.is_finite() || value.abs() > 10_000_000.0)
        || report.pdf_metrics.bbox[0] > report.pdf_metrics.bbox[2]
        || report.pdf_metrics.bbox[1] > report.pdf_metrics.bbox[3]
        || report.pdf_metrics.stem_v < 0.0
        || report
            .bbox
            .iter()
            .any(|value| value.unsigned_abs() > 10_000_000)
        || [report.ascent, report.descent, report.cap_height]
            .iter()
            .any(|value| value.unsigned_abs() > 10_000_000)
        || report.italic_angle_millidegrees.unsigned_abs() > 360_000
        || char::from_u32(report.exercise_unicode).is_none()
    {
        return Err(FontWorkerError::IdentityDrift);
    }
    match report.preparation_kind {
        FontPreparationKind::Subset => {
            // Subsetter appends composite dependencies after the caller's mapping.
            // Only requested glyphs plus .notdef have public text CIDs; all final
            // glyphs still require widths and the exact prepared-byte hash.
            if !valid_subset_mappings(&report.remapped_glyphs, requested, embedded_glyph_count) {
                return Err(FontWorkerError::IdentityDrift);
            }
        }
        FontPreparationKind::Full => {
            if report.remapped_glyphs.len() != used_glyph_count
                || report.remapped_glyphs.iter().any(|remapped| {
                    remapped.cid != remapped.source_glyph_id
                        || usize::from(remapped.cid) >= embedded_glyph_count
                })
            {
                return Err(FontWorkerError::IdentityDrift);
            }
        }
    }
    let remapped_sources = report
        .remapped_glyphs
        .iter()
        .map(|item| item.source_glyph_id)
        .collect::<BTreeSet<_>>();
    if remapped_sources.len() != report.remapped_glyphs.len()
        || requested
            .iter()
            .any(|glyph| !remapped_sources.contains(glyph))
        || !report
            .remapped_glyphs
            .iter()
            .any(|item| item.cid == report.exercise_cid)
    {
        return Err(FontWorkerError::IdentityDrift);
    }
    Ok(())
}

fn valid_subset_mappings(mappings: &[RemappedGlyph], requested: &[u16], embedded: usize) -> bool {
    let expected = std::iter::once(0).chain(requested.iter().copied().filter(|glyph| *glyph != 0));
    let expected_count = requested.len() + usize::from(!requested.contains(&0));
    mappings.len() == expected_count
        && embedded >= expected_count
        && mappings
            .iter()
            .zip(expected)
            .enumerate()
            .all(|(cid, (mapping, source))| {
                mapping.source_glyph_id == source && usize::from(mapping.cid) == cid
            })
}

fn valid_rights_identity(value: &str, kind: FontPreparationKind) -> bool {
    let Some((prefix, fs_type)) = value.rsplit_once('/') else {
        return false;
    };
    let valid_prefix = match kind {
        FontPreparationKind::Subset => matches!(
            prefix,
            "subset-allowed/installable"
                | "subset-allowed/preview-and-print"
                | "subset-allowed/editable"
        ),
        FontPreparationKind::Full => matches!(
            prefix,
            "full-font-required/installable"
                | "full-font-required/preview-and-print"
                | "full-font-required/editable"
        ),
    };
    valid_prefix && fs_type.len() == 4 && fs_type.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_postscript_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !b"[](){}<>/%".contains(&byte))
}

fn resolve_worker() -> Result<WorkerBinding, FontWorkerError> {
    let executable = std::env::current_exe()
        .map_err(|error| FontWorkerError::InvalidExecutable(error.to_string()))?;
    resolve_worker_from(&executable)
}

fn resolve_worker_from(executable: &Path) -> Result<WorkerBinding, FontWorkerError> {
    for path in worker_candidates(executable) {
        if path.exists() {
            let bytes = read_bounded(&path, MAX_WORKER_BYTES)?;
            return Ok(WorkerBinding {
                path,
                executable_hash: Hash32::of(&bytes),
            });
        }
    }
    #[cfg(debug_assertions)]
    if let Some(path) = debug_worker_candidate() {
        let bytes = read_bounded(&path, MAX_WORKER_BYTES)?;
        return Ok(WorkerBinding {
            path,
            executable_hash: Hash32::of(&bytes),
        });
    }
    Err(FontWorkerError::NotInstalled)
}

fn worker_candidates(executable: &Path) -> Vec<PathBuf> {
    let name = if cfg!(windows) {
        "publisher-font-worker.exe"
    } else {
        "publisher-font-worker"
    };
    let adjacent_worker = executable.parent().map(|directory| {
        #[cfg(debug_assertions)]
        if directory
            .file_name()
            .is_some_and(|value| value.eq_ignore_ascii_case("deps"))
        {
            return directory.parent().unwrap_or(directory).join(name);
        }
        directory.join(name)
    });
    let cargo_worker = cfg!(debug_assertions)
        .then(|| option_env!("CARGO_BIN_EXE_publisher-font-worker").map(PathBuf::from))
        .flatten();
    [
        adjacent_worker,
        cargo_worker,
        cfg!(debug_assertions).then(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug")
                .join(name)
        }),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(all(test, not(debug_assertions)))]
#[test]
fn release_worker_lookup_is_confined_to_the_application_directory() {
    let directory = private_directory("publisher-release-lookup").unwrap();
    let host = directory.join("Publisher V2.exe");
    let candidates = worker_candidates(&host);
    assert_eq!(
        candidates.len(),
        1,
        "release must not search development executables"
    );
    assert_eq!(candidates[0].parent(), host.parent());
    assert!(matches!(
        resolve_worker_from(&host),
        Err(FontWorkerError::NotInstalled)
    ));
    write_exclusive(&candidates[0], b"pinned fixture bytes").unwrap();
    let binding = resolve_worker_from(&host).unwrap();
    assert_eq!(binding.path, candidates[0]);
    assert_eq!(binding.executable_hash, Hash32::of(b"pinned fixture bytes"));
    std::fs::remove_file(&candidates[0]).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

/// Cargo places workspace binary test targets under `target/debug/deps` with a
/// hash suffix. This debug-only discovery keeps release trust restricted to the
/// adjacent packaged sidecar while allowing cross-crate tests to exercise the
/// real protocol boundary.
#[cfg(debug_assertions)]
fn debug_worker_candidate() -> Option<PathBuf> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/deps");
    let prefix = "publisher_font_worker-";
    let mut candidates = std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                return false;
            };
            let extension = path.extension().and_then(|value| value.to_str());
            name.starts_with(prefix)
                && !extension.is_some_and(|value| value.eq_ignore_ascii_case("d"))
                && (!cfg!(windows)
                    || extension.is_some_and(|value| value.eq_ignore_ascii_case("exe")))
                && path.is_file()
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| {
        path.metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    candidates.pop()
}

struct PreparedWorker {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    verified_bytes: Vec<u8>,
    path: PathBuf,
    directory: PathBuf,
    executable_hash: Hash32,
    identity_lock: Option<File>,
}

impl PreparedWorker {
    fn new(binding: &WorkerBinding) -> Result<Self, FontWorkerError> {
        let bytes = read_bounded(&binding.path, MAX_WORKER_BYTES)?;
        if Hash32::of(&bytes) != binding.executable_hash {
            return Err(FontWorkerError::ExecutableIdentityDrift);
        }
        let directory = private_directory("publisher-font-executable")?;
        let path = directory.join(if cfg!(windows) {
            "publisher-font-worker.exe"
        } else {
            "publisher-font-worker"
        });
        let mut prepared = Self {
            path,
            directory,
            executable_hash: binding.executable_hash,
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            verified_bytes: Vec::new(),
            identity_lock: None,
        };
        write_exclusive(&prepared.path, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&prepared.path, std::fs::Permissions::from_mode(0o500))
                .map_err(io_error)?;
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let mut lock = options.open(&prepared.path).map_err(io_error)?;
        lock.rewind().map_err(io_error)?;
        let mut exact = Vec::new();
        Read::by_ref(&mut lock)
            .take(MAX_WORKER_BYTES + 1)
            .read_to_end(&mut exact)
            .map_err(io_error)?;
        if Hash32::of(&exact) != binding.executable_hash {
            return Err(FontWorkerError::ExecutableIdentityDrift);
        }
        prepared.identity_lock = Some(lock);
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            prepared.verified_bytes = exact;
        }
        Ok(prepared)
    }
}

impl Drop for PreparedWorker {
    fn drop(&mut self) {
        drop(self.identity_lock.take());
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

struct PreparationFiles {
    output_directory: PathBuf,
    directory: PathBuf,
    source: PathBuf,
    prepared: PathBuf,
}
impl PreparationFiles {
    fn new(font: &[u8]) -> Result<Self, FontWorkerError> {
        let directory = private_directory("publisher-font-preparation")?;
        let output_directory = directory.join("outputs");
        // Establish cleanup ownership before the first fallible file operation.
        let files = Self {
            source: directory.join("source.font"),
            prepared: output_directory.join("prepared.font"),
            output_directory,
            directory,
        };
        write_exclusive(&files.source, font)?;
        std::fs::create_dir(&files.output_directory).map_err(io_error)?;
        Ok(files)
    }
}

impl Drop for PreparationFiles {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.source);
        let _ = std::fs::remove_file(&self.prepared);
        let _ = std::fs::remove_dir_all(&self.output_directory);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

fn run_worker(
    worker: &PreparedWorker,
    files: &PreparationFiles,
    request: &WireRequest,
) -> Result<WireReport, FontWorkerError> {
    run_worker_with_timeout(worker, files, request, PREPARATION_TIMEOUT)
}

fn run_worker_with_timeout(
    worker: &PreparedWorker,
    files: &PreparationFiles,
    request: &WireRequest,
    timeout: Duration,
) -> Result<WireReport, FontWorkerError> {
    let input = serde_json::to_vec(request)
        .map_err(|error| FontWorkerError::MalformedReport(error.to_string()))?;
    let mut child = RunningProcess::spawn(worker, files, timeout)
        .map_err(FontWorkerError::ConfinementFailed)?;
    let mut stdin = child
        .take_stdin()
        .ok_or_else(|| FontWorkerError::SpawnFailed("font worker stdin unavailable".into()))?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| FontWorkerError::SpawnFailed("font worker stdout unavailable".into()))?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| FontWorkerError::SpawnFailed("font worker stderr unavailable".into()))?;
    let writer = thread::spawn(move || stdin.write_all(&input));
    // Drain while the worker runs. Waiting for process exit before reading can
    // deadlock once the bounded report exceeds the operating-system pipe
    // buffer, even though the report remains below our 4 MiB protocol ceiling.
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        stdout
            .take(MAX_REPORT_BYTES + 1)
            .read_to_end(&mut output)
            .map(|_| output)
    });
    let stderr_reader = thread::spawn(move || {
        let mut output = Vec::new();
        stderr
            .take(MAX_REPORT_BYTES + 1)
            .read_to_end(&mut output)
            .map(|_| output)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                child.terminate();
                // Both supported backends kill and reap before pipes are joined.
                let _ = writer.join();
                let _ = reader.join();
                let _ = stderr_reader.join();
                return Err(FontWorkerError::TimedOut);
            }
            Err(error) => {
                child.terminate();
                let _ = writer.join();
                let _ = reader.join();
                let _ = stderr_reader.join();
                return Err(if error.kind() == io::ErrorKind::TimedOut {
                    FontWorkerError::TimedOut
                } else {
                    FontWorkerError::Io(error.to_string())
                });
            }
        }
    };
    if writer
        .join()
        .map_err(|_| FontWorkerError::Io("font worker input writer panicked".into()))?
        .is_err()
        && status.success()
    {
        return Err(FontWorkerError::Io("font worker input pipe closed".into()));
    }
    let output = reader
        .join()
        .map_err(|_| FontWorkerError::Io("font worker output reader panicked".into()))?
        .map_err(io_error)?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| FontWorkerError::Io("font worker stderr reader panicked".into()))?
        .map_err(io_error)?;
    if output.len() as u64 > MAX_REPORT_BYTES || stderr.len() as u64 > MAX_REPORT_BYTES {
        return Err(FontWorkerError::ResourceLimit);
    }
    if !status.success() {
        let failure: WireFailure = serde_json::from_slice(&output)
            .map_err(|error| FontWorkerError::MalformedReport(error.to_string()))?;
        if failure.protocol_version != PROTOCOL_VERSION {
            return Err(FontWorkerError::IdentityDrift);
        }
        return Err(classify_worker_failure(failure));
    }
    serde_json::from_slice(&output)
        .map_err(|error| FontWorkerError::MalformedReport(error.to_string()))
}

fn private_directory(label: &str) -> Result<PathBuf, FontWorkerError> {
    for attempt in 0..16_u32 {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| FontWorkerError::Io(error.to_string()))?
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("{label}-{}-{nonce}-{attempt}", std::process::id()));
        match std::fs::create_dir(&path) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                        .map_err(io_error)?;
                }
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(error)),
        }
    }
    Err(FontWorkerError::Io(
        "could not allocate private font-worker directory".into(),
    ))
}

fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<(), FontWorkerError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_error)
}
fn read_exact_bounded(path: &Path, expected: usize) -> Result<Vec<u8>, FontWorkerError> {
    if expected == 0 || expected > MAX_FONT_FILE_BYTES {
        return Err(FontWorkerError::ResourceLimit);
    }
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if !metadata.file_type().is_file() || metadata.len() != expected as u64 {
        return Err(FontWorkerError::IdentityDrift);
    }
    let bytes = read_bounded(path, expected as u64)?;
    if bytes.len() != expected {
        return Err(FontWorkerError::IdentityDrift);
    }
    Ok(bytes)
}
fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, FontWorkerError> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err(FontWorkerError::ResourceLimit);
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(FontWorkerError::ResourceLimit);
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Hash32([u8; 32]);
impl Hash32 {
    fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
    fn hex(self) -> String {
        use std::fmt::Write as _;
        let mut value = String::with_capacity(64);
        for byte in self.0 {
            write!(&mut value, "{byte:02x}").expect("string writes cannot fail");
        }
        value
    }
}
fn face_identity_hash(source: Hash32, face_index: u32) -> Hash32 {
    let mut hasher = Sha256::new();
    hasher.update(FACE_IDENTITY_DOMAIN);
    hasher.update(source.0);
    hasher.update(face_index.to_be_bytes());
    Hash32(hasher.finalize().into())
}
fn validate_ascent_facts_shape(gids: &[u16]) -> Result<(), FontWorkerError> {
    let reason = if gids.len() > usize::from(u16::MAX) {
        Some("facts exceed source GID capacity")
    } else if gids.contains(&0) {
        Some("GID0 cannot be an accented-only role")
    } else if gids.windows(2).any(|w| w[0] == w[1]) {
        Some("duplicate accented source GID")
    } else if gids.windows(2).any(|w| w[0] > w[1]) {
        Some("accented source GIDs must be sorted")
    } else {
        None
    };
    if let Some(reason) = reason {
        Err(FontWorkerError::InvalidAscentFacts { reason })
    } else {
        Ok(())
    }
}
fn ascent_facts_hash(source: Hash32, face: u32, gids: &[u16]) -> Hash32 {
    let mut hasher = Sha256::new();
    hasher.update(ASCENT_FACTS_DOMAIN);
    hasher.update(source.0);
    hasher.update(face.to_be_bytes());
    hasher.update(u32::try_from(gids.len()).unwrap_or(u32::MAX).to_be_bytes());
    for gid in gids {
        hasher.update(gid.to_be_bytes());
    }
    Hash32(hasher.finalize().into())
}
fn canonical_zero(value: f32) -> f32 {
    if value == 0. { 0. } else { value }
}
fn validate_cap_fact(value: f32) -> Result<f32, FontWorkerError> {
    if !value.is_finite() || value.abs() > 10_000_000. {
        return Err(FontWorkerError::InvalidCapHeightFact);
    }
    Ok(canonical_zero(value))
}
fn font_facts_hash(
    source: Hash32,
    face: u32,
    accented: Option<&[u16]>,
    cap: Option<f32>,
) -> Option<Hash32> {
    if accented.is_none() && cap.is_none() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(FONT_FACTS_DOMAIN);
    hasher.update(source.0);
    hasher.update(face.to_be_bytes());
    hasher.update([u8::from(accented.is_some())]);
    if let Some(gids) = accented {
        hasher.update(u32::try_from(gids.len()).unwrap_or(u32::MAX).to_be_bytes());
        for gid in gids {
            hasher.update(gid.to_be_bytes());
        }
    }
    hasher.update([u8::from(cap.is_some())]);
    if let Some(value) = cap {
        hasher.update(canonical_zero(value).to_bits().to_be_bytes());
    }
    Some(Hash32(hasher.finalize().into()))
}
fn validate_font_facts_binding(
    report: &WirePreparationReport,
    expected: Option<Hash32>,
    cap: Option<f32>,
) -> Result<(), FontWorkerError> {
    if report.font_facts_sha256 != expected.map(Hash32::hex)
        || report.requires_cap_height_fact != report.pdf_metrics.cap_height.is_none()
        || cap.is_some_and(|value| {
            report.requires_cap_height_fact
                || report.pdf_metrics.cap_height.map(f32::to_bits) != Some(value.to_bits())
        })
    {
        Err(FontWorkerError::IdentityDrift)
    } else {
        Ok(())
    }
}
fn validate_ascent_facts_binding(
    report: &WirePreparationReport,
    expected: Option<Hash32>,
) -> Result<(), FontWorkerError> {
    if report.ascent_facts_sha256 != expected.map(Hash32::hex)
        || (expected.is_some() && report.requires_ascent_facts)
    {
        Err(FontWorkerError::IdentityDrift)
    } else {
        Ok(())
    }
}
fn glyph_set_hash(glyphs: &[u16]) -> Hash32 {
    let mut hasher = Sha256::new();
    hasher.update(GLYPH_SET_DOMAIN);
    hasher.update(
        u32::try_from(glyphs.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    for glyph in glyphs {
        hasher.update(glyph.to_be_bytes());
    }
    Hash32(hasher.finalize().into())
}
fn domain_hash(domain: &[u8], value: &[u8]) -> Hash32 {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(value);
    Hash32(hasher.finalize().into())
}
#[allow(clippy::needless_pass_by_value)]
fn io_error(error: std::io::Error) -> FontWorkerError {
    FontWorkerError::Io(error.to_string())
}

struct RunningProcess {
    #[cfg(windows)]
    inner: publisher_worker_job::SandboxedProcess,
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    inner: publisher_linux_worker_job::SandboxedProcess,
    #[cfg(not(any(windows, all(target_os = "linux", target_arch = "x86_64"))))]
    inner: std::convert::Infallible,
}

impl RunningProcess {
    fn spawn(
        worker: &PreparedWorker,
        files: &PreparationFiles,
        timeout: Duration,
    ) -> Result<Self, String> {
        #[cfg(windows)]
        {
            let profile =
                publisher_worker_job::AppContainerProfile::create_unique("PublisherV2.Font")
                    .map_err(|error| error.to_string())?;
            profile
                .grant_read_execute(&worker.directory)
                .map_err(|error| error.to_string())?;
            // Source access is read-only. Grant the pre-created private output
            // directory itself: Windows inheritance does not retroactively
            // authorize this existing child through a later parent-directory grant.
            profile
                .grant_read(&files.source)
                .map_err(|error| error.to_string())?;
            profile
                .grant_private_working_directory(&files.output_directory)
                .map_err(|error| error.to_string())?;
            let inner = profile
                .spawn(
                    &worker.path,
                    &[],
                    &worker.directory,
                    publisher_worker_job::JobLimits {
                        process_memory_bytes: WORKER_MEMORY_LIMIT,
                        job_memory_bytes: WORKER_MEMORY_LIMIT,
                        cpu_time: timeout,
                    },
                )
                .map_err(|error| error.to_string())?;
            Ok(Self { inner })
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            // The bytes were read and hashed together during preparation. Never
            // reopen the executable path between identity verification and exec.
            let executable =
                publisher_linux_worker_job::PinnedExecutable::from_bytes(&worker.verified_bytes)
                    .map_err(|error| error.to_string())?;
            let profile = publisher_linux_worker_job::SandboxProfile::new()
                .map_err(|error| error.to_string())?;
            profile
                .grant_read(&files.source)
                .map_err(|error| error.to_string())?;
            profile
                .grant_private_working_directory(&files.output_directory)
                .map_err(|error| error.to_string())?;
            let inner = profile
                .spawn_pinned(
                    &executable,
                    &[],
                    &files.output_directory,
                    publisher_linux_worker_job::JobLimits {
                        process_memory_bytes: WORKER_MEMORY_LIMIT,
                        job_memory_bytes: WORKER_MEMORY_LIMIT,
                        cpu_time: timeout,
                        wall_time: timeout,
                    },
                )
                .map_err(|error| error.to_string())?;
            Ok(Self { inner })
        }
        #[cfg(not(any(windows, all(target_os = "linux", target_arch = "x86_64"))))]
        {
            let _ = (worker, files, timeout);
            Err("worker confinement requires Windows or qualified x86_64 Linux".into())
        }
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
        {
            self.inner
                .take_stdin()
                .map(|pipe| Box::new(pipe) as Box<dyn Write + Send>)
        }
        #[cfg(not(any(windows, all(target_os = "linux", target_arch = "x86_64"))))]
        {
            match self.inner {}
        }
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
        {
            self.inner
                .take_stdout()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>)
        }
        #[cfg(not(any(windows, all(target_os = "linux", target_arch = "x86_64"))))]
        {
            match self.inner {}
        }
    }

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
        {
            self.inner
                .take_stderr()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>)
        }
        #[cfg(not(any(windows, all(target_os = "linux", target_arch = "x86_64"))))]
        {
            match self.inner {}
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        #[cfg(windows)]
        {
            self.inner.try_wait()
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let status = self.inner.try_wait()?;
            if status.is_some() && self.inner.deadline_terminated() {
                Err(io::ErrorKind::TimedOut.into())
            } else {
                Ok(status)
            }
        }
        #[cfg(not(any(windows, all(target_os = "linux", target_arch = "x86_64"))))]
        {
            match self.inner {}
        }
    }

    fn terminate(&mut self) {
        #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
        {
            let _ignored = self.inner.terminate();
        }
        #[cfg(not(any(windows, all(target_os = "linux", target_arch = "x86_64"))))]
        {
            match self.inner {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_ascent_public_api_preserves_fact_presence_and_authority() {
        let boundary = TrustedFontBoundary::resolve_packaged();
        let high = include_bytes!(
            "../../../apps/font-worker/src/tests/fixtures/ascent-unencoded-high.otf"
        );
        let automatic = boundary.prepare_publication(high, 0, &[0, 1, 2]).unwrap();
        assert!(automatic.requires_ascent_facts());
        assert!(automatic.ascent_facts_sha256().is_none());
        let authored = boundary
            .prepare_publication_with_ascent_facts(high, 0, &[0, 1, 2], &[2])
            .unwrap();
        assert!(!authored.requires_ascent_facts());
        assert_eq!(
            authored.ascent_facts_sha256(),
            Some(ascent_facts_hash(Hash32::of(high), 0, &[2]).0)
        );
        assert_eq!(authored.pdf_metrics().ascent().to_bits(), 880_f32.to_bits());
        assert_eq!(authored.prepared_bytes(), automatic.prepared_bytes());
        let legacy = boundary.prepare(high, 0, &[0, 1, 2]).unwrap();
        assert!(
            legacy.requires_ascent_facts(),
            "legacy wrapper preserves unresolved state"
        );
        let plain = include_bytes!("../../../apps/font-worker/src/tests/fixtures/default.otf");
        let empty = boundary
            .prepare_publication_with_ascent_facts(plain, 0, &[0, 1], &[])
            .unwrap();
        assert!(!empty.requires_ascent_facts());
        assert!(empty.ascent_facts_sha256().is_some());
        let legacy = boundary.prepare(plain, 0, &[0, 1]).unwrap();
        assert!(
            !legacy.requires_ascent_facts(),
            "legacy authoritative preparation remains authoritative"
        );
        for facts in [&[0][..], &[2, 2][..], &[2, 1][..]] {
            assert!(matches!(
                boundary.prepare_publication_with_ascent_facts(high, 0, &[0, 1, 2], facts),
                Err(FontWorkerError::InvalidAscentFacts { .. })
            ));
        }
        assert!(
            matches!(boundary.prepare_publication_with_ascent_facts(high,0,&[0,1,2],&[3]),Err(FontWorkerError::WorkerRejected {code,..}) if code=="invalid-ascent-facts")
        );
    }
    #[test]
    fn authored_ascent_report_hash_and_authority_tampering_is_rejected() {
        let boundary = TrustedFontBoundary::resolve_packaged();
        let binding = boundary.binding.as_ref().unwrap();
        let worker = PreparedWorker::new(binding).unwrap();
        let font = include_bytes!("../../../apps/font-worker/src/tests/fixtures/default.otf");
        let files = PreparationFiles::new(font).unwrap();
        let source = Hash32::of(font);
        let expected = ascent_facts_hash(source, 0, &[]);
        let request = WireRequest::Prepare {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: source.hex(),
            prepared_output_path: files.prepared.display().to_string(),
            preparation_policy: WirePreparationPolicy::Publication,
            face_index: 0,
            glyph_ids: vec![0, 1],
            expected_glyph_set_sha256: glyph_set_hash(&[0, 1]).hex(),
            accented_source_glyph_ids: Some(vec![]),
            expected_ascent_facts_sha256: Some(expected.hex()),
            cap_height_pdf_units: None,
            expected_font_facts_sha256: font_facts_hash(source, 0, Some(&[]), None)
                .map(Hash32::hex),
        };
        let WireReport::Prepare(mut report) = run_worker(&worker, &files, &request).unwrap() else {
            panic!("preparation");
        };
        assert!(validate_ascent_facts_binding(&report, Some(expected)).is_ok());
        assert!(validate_ascent_facts_binding(&report, None).is_err());
        for changed in [
            ascent_facts_hash(Hash32::of(b"other source"), 0, &[]),
            ascent_facts_hash(source, 1, &[]),
            ascent_facts_hash(source, 0, &[1]),
        ] {
            assert!(validate_ascent_facts_binding(&report, Some(changed)).is_err());
        }
        report.requires_ascent_facts = true;
        assert!(validate_ascent_facts_binding(&report, Some(expected)).is_err());
        report.requires_ascent_facts = false;
        report.ascent_facts_sha256 = None;
        assert!(validate_ascent_facts_binding(&report, Some(expected)).is_err());
    }
    #[test]
    fn authored_ascent_fact_capacity_uses_source_face_and_large_request_transport() {
        let font = include_bytes!("../../layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf");
        // Range/transport evidence only; native validation does not assert the
        // semantic truth of these authored memberships. The request exceeds
        // the old256KiB frame and smaller16,384 requested-GID capacity.
        let facts = (1..60001_u16).collect::<Vec<_>>();
        let prepared = TrustedFontBoundary::resolve_packaged()
            .prepare_publication_with_ascent_facts(font, 0, &[0, 41], &facts)
            .unwrap();
        assert!(!prepared.requires_ascent_facts());
        assert!(prepared.ascent_facts_sha256().is_some());
    }

    #[test]
    fn authored_cap_height_public_api_preserves_legacy_and_canonical_presence() {
        let boundary = TrustedFontBoundary::resolve_packaged();
        let plain = include_bytes!("../../../apps/font-worker/src/tests/fixtures/default.otf");
        let automatic = boundary.prepare_publication(plain, 0, &[1]).unwrap();
        assert!(!automatic.requires_cap_height_fact());
        assert!(automatic.font_facts_sha256().is_none());
        let cap = automatic.pdf_metrics().cap_height().unwrap();
        let authored = boundary
            .prepare_publication_with_font_facts(plain, 0, &[1], Some(&[]), Some(cap))
            .unwrap();
        let legacy = boundary
            .prepare_publication_with_ascent_facts(plain, 0, &[1], &[])
            .unwrap();
        assert_eq!(authored.prepared_bytes(), automatic.prepared_bytes());
        assert_eq!(authored.ascent_facts_sha256(), legacy.ascent_facts_sha256());
        assert_eq!(
            authored.font_facts_sha256(),
            font_facts_hash(Hash32::of(plain), 0, Some(&[]), Some(cap)).map(|h| h.0)
        );
        assert_ne!(authored.font_facts_sha256(), legacy.font_facts_sha256());
        assert!(
            matches!(boundary.prepare_publication_with_font_facts(plain,0,&[1],None,Some(cap+1.)),Err(FontWorkerError::WorkerRejected{code,..}) if code=="cap-height-fact-conflict")
        );
        for cap in [
            f32::NAN,
            f32::INFINITY,
            -f32::INFINITY,
            10_000_001.,
            -10_000_001.,
        ] {
            assert!(matches!(
                boundary.prepare_publication_with_font_facts(plain, 0, &[1], None, Some(cap)),
                Err(FontWorkerError::InvalidCapHeightFact)
            ));
        }
        let empty = include_bytes!("../../../apps/font-worker/src/tests/fixtures/empty.otf");
        let positive_zero = boundary
            .prepare_publication_with_font_facts(empty, 0, &[1], None, Some(0.))
            .unwrap();
        let negative_zero = boundary
            .prepare_publication_with_font_facts(empty, 0, &[1], None, Some(-0.))
            .unwrap();
        assert_eq!(
            positive_zero.font_facts_sha256(),
            negative_zero.font_facts_sha256()
        );
        assert_eq!(
            negative_zero.pdf_metrics().cap_height().map(f32::to_bits),
            Some(0_f32.to_bits())
        );
    }

    #[test]
    fn authored_cap_height_report_binding_rejects_hash_presence_value_and_authority_tampering() {
        let boundary = TrustedFontBoundary::resolve_packaged();
        let worker = PreparedWorker::new(boundary.binding.as_ref().unwrap()).unwrap();
        let font = include_bytes!("../../../apps/font-worker/src/tests/fixtures/default.otf");
        let files = PreparationFiles::new(font).unwrap();
        let source = Hash32::of(font);
        let expected = font_facts_hash(source, 0, None, Some(733.));
        let request = WireRequest::Prepare {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: source.hex(),
            prepared_output_path: files.prepared.display().to_string(),
            preparation_policy: WirePreparationPolicy::Publication,
            face_index: 0,
            glyph_ids: vec![1],
            expected_glyph_set_sha256: glyph_set_hash(&[1]).hex(),
            accented_source_glyph_ids: None,
            expected_ascent_facts_sha256: None,
            cap_height_pdf_units: Some(733.),
            expected_font_facts_sha256: expected.map(Hash32::hex),
        };
        let WireReport::Prepare(mut report) = run_worker(&worker, &files, &request).unwrap() else {
            panic!("prepare");
        };
        assert!(validate_font_facts_binding(&report, expected, Some(733.)).is_ok());
        for wrong in [
            None,
            font_facts_hash(Hash32::of(b"other"), 0, None, Some(733.)),
            font_facts_hash(source, 1, None, Some(733.)),
            font_facts_hash(source, 0, Some(&[]), Some(733.)),
            font_facts_hash(source, 0, None, Some(734.)),
        ] {
            assert!(validate_font_facts_binding(&report, wrong, Some(733.)).is_err());
        }
        report.requires_cap_height_fact = true;
        assert!(validate_font_facts_binding(&report, expected, Some(733.)).is_err());
        report.requires_cap_height_fact = false;
        report.pdf_metrics.cap_height = None;
        assert!(validate_font_facts_binding(&report, expected, Some(733.)).is_err());
        report.pdf_metrics.cap_height = Some(734.);
        assert!(validate_font_facts_binding(&report, expected, Some(733.)).is_err());
    }

    #[test]
    fn worker_failures_preserve_authoring_denials_without_hiding_integrity_codes() {
        for (detail, expected) in [
            ("full-font-required", FontEmbeddingDenial::FullFontRequired),
            ("restricted", FontEmbeddingDenial::Restricted),
            ("bitmap-only", FontEmbeddingDenial::BitmapOnly),
            ("unknown", FontEmbeddingDenial::Unknown),
        ] {
            assert_eq!(
                classify_worker_failure(WireFailure {
                    protocol_version: PROTOCOL_VERSION,
                    error: "embedding-denied".to_owned(),
                    detail: detail.to_owned(),
                }),
                FontWorkerError::EmbeddingDenied(expected)
            );
        }

        for code in [
            "source-hash-mismatch",
            "glyph-set-hash-mismatch",
            "unsupported-protocol",
            "output-unavailable",
            "subset-failed",
        ] {
            assert_eq!(
                classify_worker_failure(WireFailure {
                    protocol_version: PROTOCOL_VERSION,
                    error: code.to_owned(),
                    detail: "bounded detail".to_owned(),
                }),
                FontWorkerError::WorkerRejected {
                    code: code.to_owned(),
                    detail: "bounded detail".to_owned(),
                }
            );
        }
        assert!(matches!(
            classify_worker_failure(WireFailure {
                protocol_version: PROTOCOL_VERSION,
                error: "embedding-denied".to_owned(),
                detail: "unexpected".to_owned(),
            }),
            FontWorkerError::WorkerRejected { code, detail }
                if code == "embedding-denied" && detail == "unexpected"
        ));
    }

    #[test]
    fn input_budgets_and_duplicates_fail_before_worker_launch() {
        assert!(matches!(
            validate_inputs(&vec![0; MAX_FONT_FILE_BYTES + 1], &[1]),
            Err(FontWorkerError::SourceTooLarge { .. })
        ));
        assert_eq!(
            validate_inputs(b"font", &[]),
            Err(FontWorkerError::NoGlyphs)
        );
        assert_eq!(
            validate_inputs(b"font", &[7, 7]),
            Err(FontWorkerError::DuplicateGlyph(7))
        );
    }

    #[test]
    fn subset_mapping_allows_component_tail_but_rejects_inconsistent_cids() {
        let valid = vec![
            RemappedGlyph {
                source_glyph_id: 0,
                cid: 0,
            },
            RemappedGlyph {
                source_glyph_id: 171,
                cid: 1,
            },
        ];
        assert!(valid_subset_mappings(&valid, &[171], 4));
        assert!(valid_subset_mappings(&valid, &[0, 171], 4));
        assert!(!valid_subset_mappings(&valid, &[171], 1));
        assert!(!valid_subset_mappings(&valid[..1], &[171], 4));
        for bad in [
            vec![
                RemappedGlyph {
                    source_glyph_id: 0,
                    cid: 0,
                },
                RemappedGlyph {
                    source_glyph_id: 171,
                    cid: 0,
                },
            ],
            vec![
                RemappedGlyph {
                    source_glyph_id: 0,
                    cid: 0,
                },
                RemappedGlyph {
                    source_glyph_id: 171,
                    cid: 4,
                },
            ],
            vec![
                RemappedGlyph {
                    source_glyph_id: 0,
                    cid: 0,
                },
                RemappedGlyph {
                    source_glyph_id: 72,
                    cid: 1,
                },
            ],
            vec![
                RemappedGlyph {
                    source_glyph_id: 171,
                    cid: 0,
                },
                RemappedGlyph {
                    source_glyph_id: 0,
                    cid: 1,
                },
            ],
        ] {
            assert!(!valid_subset_mappings(&bad, &[171], 4));
        }
    }

    #[test]
    fn confined_worker_prepares_accented_composite_subset() {
        let font = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
        // Fixture cmap: e=72, eacute=171, idieresis=177. Accents add component glyphs.
        for glyphs in [&[171][..], &[177][..], &[72, 171, 177][..]] {
            let prepared = TrustedFontBoundary::resolve_packaged()
                .prepare(font, 0, glyphs)
                .expect("composite subset");
            assert!(usize::from(prepared.embedded_glyph_count()) > glyphs.len() + 1);
            assert_eq!(prepared.source_sha256(), Hash32::of(font).0);
            assert_eq!(
                prepared.subset_sha256(),
                Hash32::of(prepared.subset_bytes()).0
            );
        }
    }

    #[test]
    fn confined_worker_prepares_hash_bound_subset() {
        let font = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
        let prepared = TrustedFontBoundary::resolve_packaged()
            .prepare(font, 0, &[36, 37])
            .expect("worker prepares font");
        assert_eq!(prepared.source_sha256(), Hash32::of(font).0);
        assert_eq!(
            prepared.subset_sha256(),
            Hash32::of(prepared.subset_bytes()).0
        );
        assert_ne!(prepared.worker_executable_sha256(), [0; 32]);
        assert_eq!(prepared.face_index(), 0);
        assert_eq!(prepared.postscript_name(), "DejaVuSans");
        assert!(prepared.glyph_count() >= 2);
    }

    #[test]
    fn confined_worker_inspects_an_exact_face_into_an_opaque_receipt() {
        let font = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");

        let inspected = TrustedFontBoundary::resolve_packaged()
            .inspect(font, 0)
            .expect("worker inspects exact face");

        assert_eq!(inspected.source_sha256(), Hash32::of(font).0);
        assert_eq!(
            inspected.face_identity_sha256(),
            face_identity_hash(Hash32::of(font), 0).0
        );
        assert_eq!(inspected.face_index(), 0);
        assert_eq!(inspected.container_kind(), FontContainerKind::SingleFont);
        assert_eq!(inspected.face_count(), 1);
        assert_eq!(inspected.family_name(), "DejaVu Sans");
        assert_eq!(inspected.family_names(), ["DejaVu Sans"]);
        assert_eq!(inspected.weight(), 400);
        assert_eq!(inspected.style(), FontFaceStyle::Normal);
        assert!(!inspected.italic());
        assert_eq!(
            inspected.embedding_rights(),
            FontEmbeddingRights::SubsetAllowed {
                license: FontEmbeddingLicense::Installable,
                fs_type: 0,
            }
        );
        assert_ne!(inspected.worker_executable_sha256(), [0; 32]);
        assert_eq!(inspected.into_source_bytes(), font);
    }

    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    #[test]
    fn packaged_font_worker_cannot_read_or_write_outside_its_private_run() {
        let font = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
        let binding = resolve_worker().expect("test worker is built");
        let worker = PreparedWorker::new(&binding).expect("prepare worker");
        let files = PreparationFiles::new(font).expect("stage exact font");
        let outside =
            private_directory("publisher-font-denied-path-test").expect("create outside directory");
        let denied_source = outside.join("denied-source.font");
        write_exclusive(&denied_source, font).expect("write denied source sentinel");

        let read_request = WireRequest::Inspect {
            protocol_version: PROTOCOL_VERSION,
            font_path: denied_source.display().to_string(),
            expected_source_sha256: Hash32::of(font).hex(),
            face_index: 0,
        };
        assert!(matches!(
            run_worker(&worker, &files, &read_request),
            Err(FontWorkerError::WorkerRejected { code, .. }) if code == "input-unavailable"
        ));

        let denied_output = outside.join("denied-output.font");
        let write_request = WireRequest::Prepare {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: Hash32::of(font).hex(),
            prepared_output_path: denied_output.display().to_string(),
            preparation_policy: WirePreparationPolicy::SubsetOnly,
            face_index: 0,
            glyph_ids: vec![36],
            accented_source_glyph_ids: None,
            expected_ascent_facts_sha256: None,
            cap_height_pdf_units: None,
            expected_font_facts_sha256: None,
            expected_glyph_set_sha256: glyph_set_hash(&[36]).hex(),
        };
        assert!(matches!(
            run_worker(&worker, &files, &write_request),
            Err(FontWorkerError::WorkerRejected { code, .. }) if code == "output-unavailable"
        ));
        assert!(!denied_output.exists());
        assert_eq!(
            std::fs::read(&denied_source).expect("denied source sentinel remains"),
            font
        );

        std::fs::remove_file(denied_source).expect("remove denied source sentinel");
        std::fs::remove_dir(outside).expect("remove outside directory");
    }

    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    #[test]
    fn packaged_font_worker_returns_a_bounded_resource_failure() {
        let font = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
        let binding = resolve_worker().expect("test worker is built");
        let worker = PreparedWorker::new(&binding).expect("prepare worker");
        let files = PreparationFiles::new(font).expect("stage exact font");
        let glyph_ids = (0..=u16::try_from(MAX_FONT_GLYPHS_PER_SUBSET)
            .expect("font glyph limit fits u16"))
            .collect::<Vec<_>>();
        let request = WireRequest::Prepare {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: Hash32::of(font).hex(),
            prepared_output_path: files.prepared.display().to_string(),
            preparation_policy: WirePreparationPolicy::SubsetOnly,
            face_index: 0,
            accented_source_glyph_ids: None,
            expected_ascent_facts_sha256: None,
            cap_height_pdf_units: None,
            expected_font_facts_sha256: None,
            expected_glyph_set_sha256: glyph_set_hash(&glyph_ids).hex(),
            glyph_ids,
        };

        assert!(matches!(
            run_worker(&worker, &files, &request),
            Err(FontWorkerError::WorkerRejected { code, detail })
                if code == "resource-limit" && detail.len() <= 64
        ));
        assert!(!files.prepared.exists());
    }

    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    #[test]
    fn packaged_font_worker_rejects_exact_malformed_font_without_output_or_paths() {
        const MALFORMED_FONT: &[u8] = b"not a font";
        const MALFORMED_FONT_SHA256: &str =
            "af4d9eef04c124f5f634d62e071cf4d46c82f08181210b1d0368ccf351e17ebb";
        const TRUNCATED_TTF: &[u8] = include_bytes!("../tests/fixtures/truncated-dejavu-sfnt.ttf");
        const TRUNCATED_TTF_SHA256: &str =
            "c5fa9d096bff817f76cfafc5bc5b7b10d48823986d663d2c803c66998c00e46d";

        let valid_font = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
        let table_count = usize::from(u16::from_be_bytes([valid_font[4], valid_font[5]]));
        let directory_end = 12 + table_count * 16;
        assert_eq!(TRUNCATED_TTF.len(), directory_end + 4);
        assert_eq!(TRUNCATED_TTF, &valid_font[..TRUNCATED_TTF.len()]);
        assert_eq!(
            TrustedFontBoundary::resolve_packaged()
                .inspect(valid_font, 0)
                .expect("licensed complete-font control")
                .family_name(),
            "DejaVu Sans"
        );
        let binding = resolve_worker().expect("test worker is built");
        let worker = PreparedWorker::new(&binding).expect("prepare worker");

        for (label, font, expected_hash) in [
            ("unrecognized bytes", MALFORMED_FONT, MALFORMED_FONT_SHA256),
            ("truncated SFNT", TRUNCATED_TTF, TRUNCATED_TTF_SHA256),
        ] {
            assert_eq!(Hash32::of(font).hex(), expected_hash);
            let files = PreparationFiles::new(font).expect("stage exact malformed font");
            let request = WireRequest::Inspect {
                protocol_version: PROTOCOL_VERSION,
                font_path: files.source.display().to_string(),
                expected_source_sha256: expected_hash.to_owned(),
                face_index: 0,
            };

            let error =
                run_worker(&worker, &files, &request).expect_err("malformed font must fail");
            let FontWorkerError::WorkerRejected { code, detail } = error else {
                panic!("{label}: expected structured worker rejection, got {error:?}");
            };
            assert_eq!(code, "malformed-font", "{label}");
            assert!(
                detail.len() <= 64,
                "{label}: failure detail must be bounded"
            );
            assert!(!detail.contains(&files.directory.display().to_string()));
            assert!(!detail.contains("source.font"));
            assert!(!detail.contains("prepared.font"));
            assert!(!files.prepared.exists(), "{label}: no output may remain");
        }
    }

    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    #[test]
    fn confined_worker_rejects_malformed_cff_name_index_without_output() {
        const VALID_CFF: &[u8] =
            include_bytes!("../../layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf");
        const VALID_SHA256: &str =
            "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b";
        const MALFORMED_SHA256: &str =
            "5d619400b6f7259355d050c6fe9f78e90604ca501f7cbf1ba2045a4b5dfad087";
        const CFF_TABLE_OFFSET: usize = 239_496;
        const NAME_INDEX_OFF_SIZE: usize = CFF_TABLE_OFFSET + 6;

        assert_eq!(Hash32::of(VALID_CFF).hex(), VALID_SHA256);
        assert_eq!(&VALID_CFF[..4], b"OTTO");
        assert_eq!(&VALID_CFF[28..32], b"CFF ");
        assert_eq!(
            u32::from_be_bytes(VALID_CFF[36..40].try_into().unwrap()),
            u32::try_from(CFF_TABLE_OFFSET).unwrap()
        );
        assert_eq!(
            &VALID_CFF[CFF_TABLE_OFFSET..CFF_TABLE_OFFSET + 7],
            b"\x01\x00\x04\x03\x00\x01\x01"
        );
        let mut malformed = VALID_CFF.to_vec();
        malformed[NAME_INDEX_OFF_SIZE] = 0; // CFF 1 Name INDEX requires an offset width of 1..=4.
        assert_eq!(&VALID_CFF[32..36], &0x338e_7374_u32.to_be_bytes());
        assert_eq!(&VALID_CFF[292..296], &0xbc90_609b_u32.to_be_bytes());
        // Keep the CFF table and whole SFNT checksums valid after the payload edit.
        malformed[32..36].copy_from_slice(&0x338e_7274_u32.to_be_bytes());
        malformed[292..296].copy_from_slice(&0xbc90_629b_u32.to_be_bytes());
        assert_eq!(Hash32::of(&malformed).hex(), MALFORMED_SHA256);
        let changed_offsets = VALID_CFF
            .iter()
            .zip(&malformed)
            .enumerate()
            .filter_map(|(index, (before, after))| (before != after).then_some(index))
            .collect::<Vec<_>>();
        assert_eq!(changed_offsets, vec![34, 294, NAME_INDEX_OFF_SIZE]);

        let valid_subset = TrustedFontBoundary::resolve_packaged()
            .prepare(VALID_CFF, 0, &[0, 34])
            .expect("licensed CFF control prepares through confined worker");
        assert_eq!(valid_subset.outline_kind(), FontOutlineKind::Cff);
        assert_eq!(valid_subset.source_sha256(), Hash32::of(VALID_CFF).0);
        // Protocol v5 attests the raw CFF program, never an incomplete OTTO wrapper.
        assert_eq!(&valid_subset.subset_bytes()[..3], b"\x01\x00\x04");
        assert_eq!(
            valid_subset.subset_sha256(),
            Hash32::of(valid_subset.subset_bytes()).0
        );
        assert_eq!(valid_subset.remapped_glyphs()[0].cid(), 0);
        assert_eq!(valid_subset.remapped_glyphs()[1].cid(), 1);

        let binding = resolve_worker().expect("test worker is built");
        let worker = PreparedWorker::new(&binding).expect("prepare worker");
        let files = PreparationFiles::new(&malformed).expect("stage malformed CFF font");
        let request = WireRequest::Prepare {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: MALFORMED_SHA256.to_owned(),
            prepared_output_path: files.prepared.display().to_string(),
            preparation_policy: WirePreparationPolicy::SubsetOnly,
            face_index: 0,
            glyph_ids: vec![0, 34],
            accented_source_glyph_ids: None,
            expected_ascent_facts_sha256: None,
            cap_height_pdf_units: None,
            expected_font_facts_sha256: None,
            expected_glyph_set_sha256: glyph_set_hash(&[0, 34]).hex(),
        };
        let error = run_worker(&worker, &files, &request)
            .expect_err("invalid CFF Name INDEX must fail preparation");
        let FontWorkerError::WorkerRejected { code, detail } = error else {
            panic!("expected structured CFF rejection, got {error:?}");
        };
        assert_eq!(code, "subset-failed");
        assert!(detail.len() <= 64);
        assert!(!detail.contains(&files.directory.display().to_string()));
        assert!(!detail.contains("source.font"));
        assert!(!detail.contains("prepared.font"));
        assert!(!files.prepared.exists());
    }

    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    #[test]
    fn confined_worker_rejects_two_cff_names_without_output() {
        const FONT: &[u8] =
            include_bytes!("../../layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf");
        const CFF_TABLE_OFFSET: usize = 239_496;
        const NAME_INDEX: usize = CFF_TABLE_OFFSET + 4;
        const SFNT_CHECKSUM_MAGIC: u32 = 0xb1b0_afba;

        let checksum = |bytes: &[u8]| {
            bytes.chunks(4).fold(0_u32, |sum, chunk| {
                let mut word = [0_u8; 4];
                word[..chunk.len()].copy_from_slice(chunk);
                sum.wrapping_add(u32::from_be_bytes(word))
            })
        };
        assert_eq!(&FONT[..4], b"OTTO");
        assert_eq!(&FONT[28..32], b"CFF ");
        assert_eq!(
            u32::from_be_bytes(FONT[36..40].try_into().unwrap()),
            u32::try_from(CFF_TABLE_OFFSET).unwrap()
        );
        let cff_length =
            usize::try_from(u32::from_be_bytes(FONT[40..44].try_into().unwrap())).unwrap();
        assert_eq!(
            checksum(&FONT[CFF_TABLE_OFFSET..CFF_TABLE_OFFSET + cff_length]),
            0x338e_7374
        );
        assert_eq!(&FONT[292..296], &0xbc90_609b_u32.to_be_bytes());
        assert_eq!(&FONT[NAME_INDEX..NAME_INDEX + 6], b"\0\x01\x01\x01\x16N");

        let mut two_names = FONT.to_vec();
        // Count two, with offsets 1, 2, 21. The original 21-byte name loses
        // its first byte to the extra offset, leaving two nonempty names in
        // the same 26-byte INDEX span; the following Top DICT does not move.
        two_names[NAME_INDEX..NAME_INDEX + 6].copy_from_slice(b"\0\x02\x01\x01\x02\x15");
        assert_eq!(two_names.len(), FONT.len());
        assert_eq!(
            &two_names[NAME_INDEX + 6..NAME_INDEX + 26],
            &FONT[NAME_INDEX + 6..NAME_INDEX + 26]
        );
        let cff_checksum = checksum(&two_names[CFF_TABLE_OFFSET..CFF_TABLE_OFFSET + cff_length]);
        two_names[32..36].copy_from_slice(&cff_checksum.to_be_bytes());
        two_names[292..296].fill(0);
        let adjustment = SFNT_CHECKSUM_MAGIC.wrapping_sub(checksum(&two_names));
        two_names[292..296].copy_from_slice(&adjustment.to_be_bytes());
        assert_eq!(checksum(&two_names), SFNT_CHECKSUM_MAGIC);
        let changed_offsets = FONT
            .iter()
            .zip(&two_names)
            .enumerate()
            .filter_map(|(index, (before, after))| (before != after).then_some(index))
            .collect::<Vec<_>>();
        assert!(changed_offsets.iter().all(|index| {
            (32..36).contains(index)
                || (292..296).contains(index)
                || [NAME_INDEX + 1, NAME_INDEX + 4, NAME_INDEX + 5].contains(index)
        }));
        assert_eq!(
            TrustedFontBoundary::resolve_packaged()
                .inspect(&two_names, 0)
                .expect("unchanged SFNT face remains inspectable")
                .family_name(),
            "Noto Sans CJK SC"
        );

        let binding = resolve_worker().expect("test worker is built");
        let worker = PreparedWorker::new(&binding).expect("prepare worker");
        let files = PreparationFiles::new(&two_names).expect("stage two-name CFF font");
        let request = WireRequest::Prepare {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: Hash32::of(&two_names).hex(),
            prepared_output_path: files.prepared.display().to_string(),
            preparation_policy: WirePreparationPolicy::SubsetOnly,
            face_index: 0,
            glyph_ids: vec![0, 34],
            accented_source_glyph_ids: None,
            expected_ascent_facts_sha256: None,
            cap_height_pdf_units: None,
            expected_font_facts_sha256: None,
            expected_glyph_set_sha256: glyph_set_hash(&[0, 34]).hex(),
        };
        let error = run_worker(&worker, &files, &request)
            .expect_err("two CFF Name INDEX entries must fail preparation");
        let FontWorkerError::WorkerRejected { code, detail } = error else {
            panic!("expected structured two-name CFF rejection, got {error:?}");
        };
        assert_eq!(code, "subset-failed");
        assert!(detail.len() <= 64);
        assert!(!detail.contains(&files.directory.display().to_string()));
        assert!(!detail.contains("source.font"));
        assert!(!detail.contains("prepared.font"));
        assert!(!files.prepared.exists());
    }

    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    #[test]
    fn confined_worker_refuses_cff_full_embedding_without_output() {
        const CFF: &[u8] =
            include_bytes!("../../layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf");
        const SOURCE_SHA256: &str =
            "2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b";
        const NO_SUBSETTING_SHA256: &str =
            "95364d48c7fdd34f19de64f24aa7092667a01b7ad7cd6ec6d60dc594e4ce8063";

        assert_eq!(Hash32::of(CFF).hex(), SOURCE_SHA256);
        assert_eq!(&CFF[..4], b"OTTO");
        assert_eq!(&CFF[108..112], b"OS/2");
        assert_eq!(&CFF[116..120], &384_u32.to_be_bytes());
        assert_eq!(&CFF[384..386], &3_u16.to_be_bytes());
        assert_eq!(&CFF[392..394], &0_u16.to_be_bytes());
        assert_eq!(&CFF[112..116], &0x9fe3_17ee_u32.to_be_bytes());
        assert_eq!(&CFF[292..296], &0xbc90_609b_u32.to_be_bytes());
        // Synthetic restriction metadata on an OFL fixture, not its actual license.
        // Preserve table layout and repair OS/2 and whole-SFNT checksums.
        let mut no_subsetting = CFF.to_vec();
        no_subsetting[392..394].copy_from_slice(&0x0100_u16.to_be_bytes());
        no_subsetting[112..116].copy_from_slice(&0xa0e3_17ee_u32.to_be_bytes());
        no_subsetting[292..296].copy_from_slice(&0xba90_609b_u32.to_be_bytes());
        assert_eq!(Hash32::of(&no_subsetting).hex(), NO_SUBSETTING_SHA256);
        let changed_offsets = CFF
            .iter()
            .zip(&no_subsetting)
            .enumerate()
            .filter_map(|(index, (before, after))| (before != after).then_some(index))
            .collect::<Vec<_>>();
        assert_eq!(changed_offsets, vec![112, 292, 392]);

        let boundary = TrustedFontBoundary::resolve_packaged();
        let control = boundary
            .prepare_publication(CFF, 0, &[0, 34])
            .expect("licensed CFF control prepares through confined worker");
        assert_eq!(control.preparation_kind(), FontPreparationKind::Subset);
        assert_eq!(control.outline_kind(), FontOutlineKind::Cff);
        assert_eq!(control.source_sha256(), Hash32::of(CFF).0);
        let inspected = boundary
            .inspect(&no_subsetting, 0)
            .expect("synthetic no-subsetting CFF remains inspectable");
        assert_eq!(inspected.source_sha256(), Hash32::of(&no_subsetting).0);
        assert_eq!(inspected.container_kind(), FontContainerKind::SingleFont);
        assert_eq!(
            inspected.embedding_rights(),
            FontEmbeddingRights::FullFontRequired {
                license: FontEmbeddingLicense::Installable,
                fs_type: 0x0100,
            }
        );

        let binding = resolve_worker().expect("test worker is built");
        let worker = PreparedWorker::new(&binding).expect("prepare worker");
        let files = PreparationFiles::new(&no_subsetting).expect("stage no-subsetting CFF");
        let request = WireRequest::Prepare {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: NO_SUBSETTING_SHA256.to_owned(),
            prepared_output_path: files.prepared.display().to_string(),
            preparation_policy: WirePreparationPolicy::Publication,
            face_index: 0,
            glyph_ids: vec![0, 34],
            accented_source_glyph_ids: None,
            expected_ascent_facts_sha256: None,
            cap_height_pdf_units: None,
            expected_font_facts_sha256: None,
            expected_glyph_set_sha256: glyph_set_hash(&[0, 34]).hex(),
        };
        let error = run_worker(&worker, &files, &request)
            .expect_err("full CFF embedding is explicitly unsupported");
        let FontWorkerError::WorkerRejected { code, detail } = error else {
            panic!("expected unsupported full-font container, got {error:?}");
        };
        assert_eq!(code, "unsupported-full-font-container");
        assert!(detail.len() <= 64);
        assert!(!detail.contains(&files.directory.display().to_string()));
        assert!(!detail.contains("source.font"));
        assert!(!detail.contains("prepared.font"));
        assert!(!files.prepared.exists());
    }

    #[test]
    fn host_rejects_a_worker_inspection_identity_mismatch() {
        let font = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
        let binding = resolve_worker().expect("test worker is built");
        let worker = PreparedWorker::new(&binding).expect("prepare worker");
        let files = PreparationFiles::new(font).expect("stage exact font");
        let request = WireRequest::Inspect {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: Hash32::of(font).hex(),
            face_index: 0,
        };
        let WireReport::Inspect(mut report) =
            run_worker(&worker, &files, &request).expect("worker returns inspection report")
        else {
            panic!("expected inspection report");
        };
        report.source_sha256 = "00".repeat(32);

        assert_eq!(
            validate_inspection_report(font, 0, &report),
            Err(FontWorkerError::IdentityDrift)
        );

        report.source_sha256 = Hash32::of(font).hex();
        report.family_names = vec!["Alias".to_owned(), "Alias".to_owned()];
        assert_eq!(
            validate_inspection_report(font, 0, &report),
            Err(FontWorkerError::IdentityDrift)
        );
    }

    #[test]
    fn publication_mode_preserves_a_full_font_when_subsetting_is_forbidden() {
        // This original coherent control includes an unused point-attached
        // composite. Its rights require the entire valid program unchanged.
        let font = include_bytes!("../../../apps/font-worker/src/tests/fixtures/original.ttf");
        let prepared = TrustedFontBoundary::resolve_packaged()
            .prepare_publication(font, 0, &[0, 1])
            .expect("worker preserves full font");
        assert_eq!(prepared.preparation_kind(), FontPreparationKind::Full);
        assert_eq!(prepared.postscript_name(), "PatterboxU2Original");
        assert_eq!(prepared.prepared_bytes(), font);
        assert_eq!(prepared.prepared_sha256(), prepared.source_sha256());
        assert_eq!(prepared.used_glyph_count(), 2);
        assert_eq!(prepared.embedded_glyph_count(), 4);
    }
    #[test]
    fn source_replacement_after_pinning_is_rejected() {
        let packaged = resolve_worker().expect("test worker is built");
        let bytes = read_bounded(&packaged.path, MAX_WORKER_BYTES).expect("read worker");
        let directory = private_directory("publisher-font-pin-test").expect("private directory");
        let source = directory.join("worker");
        write_exclusive(&source, &bytes).expect("stage source");
        let binding = WorkerBinding {
            path: source.clone(),
            executable_hash: Hash32::of(&bytes),
        };
        let mut replacement = bytes;
        replacement[0] ^= 0xff;
        std::fs::write(&source, replacement).expect("replace source");
        assert!(matches!(
            PreparedWorker::new(&binding),
            Err(FontWorkerError::ExecutableIdentityDrift)
        ));
        std::fs::remove_file(source).expect("remove source");
        std::fs::remove_dir(directory).expect("remove directory");
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn linux_probe() -> PreparedWorker {
        let executable = std::env::current_exe().unwrap();
        let path = executable
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/linux-boundary-probe");
        let bytes = read_bounded(&path, MAX_WORKER_BYTES)
            .expect("build the linux-boundary-probe example before this native suite");
        PreparedWorker::new(&WorkerBinding {
            path,
            executable_hash: Hash32::of(&bytes),
        })
        .unwrap()
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn probe_request(files: &PreparationFiles, mode: &str) -> WireRequest {
        WireRequest::Inspect {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: mode.into(),
            face_index: 0,
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linux_launch_uses_verified_bytes_after_private_path_replacement() {
        use std::os::unix::fs::PermissionsExt;
        let worker = PreparedWorker::new(&resolve_worker().unwrap()).unwrap();
        std::fs::set_permissions(&worker.path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&worker.path, b"replaced after the digest check").unwrap();
        let font = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
        let files = PreparationFiles::new(font).unwrap();
        let request = WireRequest::Inspect {
            protocol_version: PROTOCOL_VERSION,
            font_path: files.source.display().to_string(),
            expected_source_sha256: Hash32::of(font).hex(),
            face_index: 0,
        };
        let WireReport::Inspect(report) = run_worker(&worker, &files, &request).unwrap() else {
            panic!("inspection report")
        };
        validate_inspection_report(font, 0, &report).unwrap();
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linux_real_elf_protocol_failures_clean_up_and_allow_fresh_retry() {
        let worker = linux_probe();
        let executable_directory = worker.directory.clone();
        for mode in [
            "malformed",
            "oversized",
            "stderr",
            "crash",
            "early-close",
            "denials",
        ] {
            let files = PreparationFiles::new(b"exact staged input").unwrap();
            let directory = files.directory.clone();
            let request_mode = if mode == "early-close" {
                format!("early-close{}", "x".repeat(128 * 1024))
            } else {
                mode.into()
            };
            let error = run_worker_with_timeout(
                &worker,
                &files,
                &probe_request(&files, &request_mode),
                Duration::from_secs(2),
            )
            .expect_err("hostile protocol cannot mint a receipt");
            if mode == "oversized" {
                assert_eq!(error, FontWorkerError::ResourceLimit);
            } else {
                assert!(
                    matches!(error, FontWorkerError::MalformedReport(_)),
                    "{mode}: {error:?}"
                );
            }
            if mode == "denials" {
                assert_eq!(
                    std::fs::read(files.output_directory.join("denials-proven")).unwrap(),
                    b"yes"
                );
            }
            if mode == "early-close" {
                let consumed: usize =
                    std::fs::read_to_string(files.output_directory.join("early-close-proven"))
                        .unwrap()
                        .parse()
                        .unwrap();
                assert!(
                    consumed < request_mode.len(),
                    "probe must exit before consuming the complete input"
                );
            }
            assert_eq!(std::fs::read(&files.source).unwrap(), b"exact staged input");
            drop(files);
            assert!(!directory.exists(), "all private outputs must be removed");
        }
        drop(worker);
        assert!(!executable_directory.exists());
        TrustedFontBoundary::resolve_packaged()
            .inspect(
                include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf"),
                0,
            )
            .unwrap();
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linux_partial_grant_setup_never_runs_and_cleans_up() {
        let worker = linux_probe();
        let files = PreparationFiles::new(b"source").unwrap();
        let directory = files.directory.clone();
        std::fs::remove_file(&files.source).unwrap();
        assert!(matches!(
            run_worker(&worker, &files, &probe_request(&files, "sleep")),
            Err(FontWorkerError::ConfinementFailed(_))
        ));
        assert!(!files.output_directory.join("pid").exists());
        drop(files);
        assert!(!directory.exists());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linux_active_cancellation_reaps_and_removes_partial_outputs() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let worker = linux_probe();
        let files = PreparationFiles::new(b"exact input").unwrap();
        let directory = files.directory.clone();
        let marker = files.output_directory.join("pid");
        let watcher_marker = marker.clone();
        let flag = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::clone(&flag);
        let watcher = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !watcher_marker.exists() {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(5));
            }
            cancellation.store(true, Ordering::Release);
        });
        let scope = publisher_linux_worker_job::WorkerCancellationScope::enter(flag);
        let started = Instant::now();
        assert!(
            run_worker_with_timeout(
                &worker,
                &files,
                &probe_request(&files, "sleep"),
                Duration::from_secs(5)
            )
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        watcher.join().unwrap();
        drop(scope);
        let pid = std::fs::read_to_string(marker).unwrap();
        assert!(
            !Path::new("/proc").join(pid).exists(),
            "worker must be reaped"
        );
        assert!(files.output_directory.join("partial").exists());
        drop(files);
        assert!(!directory.exists());
        TrustedFontBoundary::resolve_packaged()
            .inspect(
                include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf"),
                0,
            )
            .unwrap();
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linux_deadline_kills_sleeping_worker_and_cleans_partial_output() {
        let worker = linux_probe();
        let files = PreparationFiles::new(b"input").unwrap();
        let directory = files.directory.clone();
        let started = Instant::now();
        assert!(matches!(
            run_worker_with_timeout(
                &worker,
                &files,
                &probe_request(&files, "sleep"),
                Duration::from_millis(250)
            ),
            Err(FontWorkerError::TimedOut)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = std::fs::read_to_string(files.output_directory.join("pid")).unwrap();
        assert!(!Path::new("/proc").join(pid).exists());
        drop(files);
        assert!(!directory.exists());
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    fn linux_deadline_remains_a_timeout_after_the_owner_has_already_reaped() {
        let worker = linux_probe();
        let files = PreparationFiles::new(b"input").unwrap();
        let mut child = RunningProcess::spawn(&worker, &files, Duration::from_millis(250)).unwrap();
        let input = serde_json::to_vec(&probe_request(&files, "sleep")).unwrap();
        let mut stdin = child.take_stdin().unwrap();
        stdin.write_all(&input).unwrap();
        drop(stdin);
        // Do not poll: the owner must kill and reap independently.
        thread::sleep(Duration::from_millis(600));
        let error = child
            .try_wait()
            .expect_err("a reaped deadline must retain its reason");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let pid = std::fs::read_to_string(files.output_directory.join("pid")).unwrap();
        assert!(!Path::new("/proc").join(pid).exists());
        child.terminate();
        let directory = files.directory.clone();
        drop(files);
        assert!(!directory.exists());
    }
}
