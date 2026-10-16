//! Bounded, machine-readable font inspection and publication preparation worker.

use layout_core::fonts::{FontEmbeddingEligibility, FontEmbeddingLicense, inspect_font_face};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    process::ExitCode,
};
use subsetter::GlyphRemapper;
use ttf_parser::{Face, GlyphId, Style, Tag, name_id};

const PROTOCOL_VERSION: u16 = 5;
const MAX_REQUEST_BYTES: u64 = 256 * 1024;
const MAX_FONT_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_FONT_GLYPHS_PER_SUBSET: usize = 16_384;
const MAX_FONT_FAMILY_BYTES: usize = 1_024;
const MAX_FONT_FAMILY_CHARS: usize = 256;
const MAX_FONT_FAMILY_NAMES: usize = 256;
const ENGINE_IDENTITY: &str = "subsetter-0.2.6/ttf-parser-0.25.1";
const RULES_IDENTITY: &str =
    "publisher-v2/font-preparation/v5/inspect-family-aliases-and-publication/cid-cff/fail-closed";
const GLYPH_SET_DOMAIN: &[u8] = b"publisher-v2/font-glyph-set/v1\0";
const RIGHTS_DOMAIN: &[u8] = b"publisher-v2/font-rights/v1\0";
const POSTSCRIPT_NAME_DOMAIN: &[u8] = b"publisher-v2/font-postscript-name/v1\0";

#[derive(Debug, Deserialize)]
#[serde(
    tag = "operation",
    rename_all = "kebab-case",
    rename_all_fields = "snake_case",
    deny_unknown_fields
)]
enum WorkerRequest {
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
        preparation_policy: PreparationPolicy,
        face_index: u32,
        glyph_ids: Vec<u16>,
        expected_glyph_set_sha256: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PreparationPolicy {
    SubsetOnly,
    Publication,
}

#[derive(Debug, Serialize)]
#[serde(tag = "operation", content = "report", rename_all = "kebab-case")]
enum WorkerReport {
    Inspect(InspectionReport),
    Prepare(PreparationReport),
}

#[derive(Debug, Serialize)]
struct InspectionReport {
    protocol_version: u16,
    source_sha256: String,
    source_byte_length: usize,
    face_index: u32,
    face_identity_sha256: String,
    container_kind: ContainerKind,
    face_count: u32,
    family_names: Vec<String>,
    weight: u16,
    style: FaceStyle,
    italic: bool,
    embedding_rights: EmbeddingRights,
    rights_identity: String,
    rights_identity_sha256: String,
    engine_identity: &'static str,
    engine_identity_sha256: String,
    rules_identity: &'static str,
    rules_identity_sha256: String,
}

#[derive(Debug, Serialize)]
struct PreparationReport {
    protocol_version: u16,
    source_sha256: String,
    face_index: u32,
    face_identity_sha256: String,
    postscript_name: String,
    postscript_name_sha256: String,
    glyph_set_sha256: String,
    preparation_kind: PreparationKind,
    used_glyph_count: u16,
    embedded_glyph_count: u16,
    prepared_sha256: String,
    prepared_byte_length: usize,
    rights_identity: String,
    rights_identity_sha256: String,
    engine_identity: &'static str,
    engine_identity_sha256: String,
    rules_identity: &'static str,
    rules_identity_sha256: String,
    outline_kind: OutlineKind,
    bbox: [i32; 4],
    ascent: i32,
    descent: i32,
    cap_height: i32,
    italic_angle_millidegrees: i32,
    fixed_pitch: bool,
    italic: bool,
    widths: Vec<i32>,
    remapped_glyphs: Vec<RemappedGlyph>,
    exercise_cid: u16,
    exercise_unicode: u32,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ContainerKind {
    SingleFont,
    Collection,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum FaceStyle {
    Normal,
    Italic,
    Oblique,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(
    tag = "status",
    rename_all = "kebab-case",
    rename_all_fields = "snake_case"
)]
enum EmbeddingRights {
    SubsetAllowed {
        license: EmbeddingLicense,
        fs_type: u16,
    },
    FullFontRequired {
        license: EmbeddingLicense,
        fs_type: u16,
    },
    Restricted {
        fs_type: u16,
    },
    BitmapOnly {
        license: EmbeddingLicense,
        fs_type: u16,
    },
    Unknown {
        reason: UnknownRightsReason,
        fs_type: Option<u16>,
    },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum EmbeddingLicense {
    Installable,
    PreviewAndPrint,
    Editable,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum UnknownRightsReason {
    FaceUnavailableOrMalformed,
    MissingOrMalformedOs2,
    InvalidFsType,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum PreparationKind {
    Subset,
    Full,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum OutlineKind {
    TrueType,
    Cff,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct RemappedGlyph {
    source_glyph_id: u16,
    cid: u16,
}

#[derive(Debug, Serialize)]
struct WorkerFailure {
    protocol_version: u16,
    error: &'static str,
    detail: String,
}

#[derive(Debug)]
enum WorkerError {
    NoRequest,
    RequestTooLarge,
    InvalidRequest(String),
    UnsupportedProtocol,
    ResourceLimit,
    InvalidGlyphSet,
    InputUnavailable(String),
    SourceHashMismatch,
    GlyphSetHashMismatch,
    EmbeddingDenied(&'static str),
    MalformedFont,
    SubsetFailed,
    NoUnicodeGlyph,
    UnsupportedFullFontContainer,
    OutputUnavailable(String),
}

fn main() -> ExitCode {
    match read_request().and_then(|request| process_request(&request)) {
        Ok(report) => write_json(&report, ExitCode::SUCCESS),
        Err(error) => write_json(
            &WorkerFailure {
                protocol_version: PROTOCOL_VERSION,
                error: error.code(),
                detail: error.detail(),
            },
            ExitCode::from(2),
        ),
    }
}

fn read_request() -> Result<WorkerRequest, WorkerError> {
    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| WorkerError::InvalidRequest(error.to_string()))?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(WorkerError::RequestTooLarge);
    }
    if bytes.is_empty() {
        return Err(WorkerError::NoRequest);
    }
    serde_json::from_slice(&bytes).map_err(|error| WorkerError::InvalidRequest(error.to_string()))
}

#[allow(clippy::too_many_lines)]
fn process_request(request: &WorkerRequest) -> Result<WorkerReport, WorkerError> {
    match request {
        WorkerRequest::Inspect {
            protocol_version,
            font_path,
            expected_source_sha256,
            face_index,
        } => {
            if *protocol_version != PROTOCOL_VERSION {
                return Err(WorkerError::UnsupportedProtocol);
            }
            process_inspection(font_path, expected_source_sha256, *face_index)
                .map(WorkerReport::Inspect)
        }
        WorkerRequest::Prepare {
            protocol_version,
            font_path,
            expected_source_sha256,
            prepared_output_path,
            preparation_policy,
            face_index,
            glyph_ids,
            expected_glyph_set_sha256,
        } => {
            if *protocol_version != PROTOCOL_VERSION {
                return Err(WorkerError::UnsupportedProtocol);
            }
            let request = PreparationRequest {
                font_path,
                expected_source_sha256,
                prepared_output_path,
                preparation_policy: *preparation_policy,
                face_index: *face_index,
                glyph_ids,
                expected_glyph_set_sha256,
            };
            process_preparation(&request).map(WorkerReport::Prepare)
        }
    }
}

struct PreparationRequest<'a> {
    font_path: &'a str,
    expected_source_sha256: &'a str,
    prepared_output_path: &'a str,
    preparation_policy: PreparationPolicy,
    face_index: u32,
    glyph_ids: &'a [u16],
    expected_glyph_set_sha256: &'a str,
}

fn process_inspection(
    font_path: &str,
    expected_source_sha256: &str,
    face_index: u32,
) -> Result<InspectionReport, WorkerError> {
    let source = read_bounded(Path::new(font_path), MAX_FONT_FILE_BYTES)?;
    let source_hash = sha256(&source);
    if hex(source_hash) != expected_source_sha256 {
        return Err(WorkerError::SourceHashMismatch);
    }
    let face = Face::parse(&source, face_index).map_err(|_| WorkerError::MalformedFont)?;
    let (container_kind, face_count) = match ttf_parser::fonts_in_collection(&source) {
        Some(face_count) if face_count > 0 && face_index < face_count => {
            (ContainerKind::Collection, face_count)
        }
        None if face_index == 0 => (ContainerKind::SingleFont, 1),
        Some(_) | None => return Err(WorkerError::MalformedFont),
    };
    let facts = inspect_font_face(&source, face_index);
    if facts.identity().source_sha256() != source_hash
        || facts.identity().face_index() != face_index
    {
        return Err(WorkerError::SourceHashMismatch);
    }
    let embedding_rights = embedding_rights(facts.embedding_eligibility());
    let family_names = exact_family_names(&face)?;
    let style = match face.style() {
        Style::Normal => FaceStyle::Normal,
        Style::Italic => FaceStyle::Italic,
        Style::Oblique => FaceStyle::Oblique,
    };
    let rights_identity = inspection_rights_identity(embedding_rights);
    let rights_hash = domain_hash(RIGHTS_DOMAIN, rights_identity.as_bytes());
    Ok(InspectionReport {
        protocol_version: PROTOCOL_VERSION,
        source_sha256: hex(source_hash),
        source_byte_length: source.len(),
        face_index,
        face_identity_sha256: hex(facts.identity().face_sha256()),
        container_kind,
        face_count,
        family_names,
        weight: face.weight().to_number(),
        style,
        italic: face.is_italic(),
        embedding_rights,
        rights_identity,
        rights_identity_sha256: hex(rights_hash),
        engine_identity: ENGINE_IDENTITY,
        engine_identity_sha256: hex(sha256(ENGINE_IDENTITY.as_bytes())),
        rules_identity: RULES_IDENTITY,
        rules_identity_sha256: hex(sha256(RULES_IDENTITY.as_bytes())),
    })
}

#[allow(clippy::too_many_lines)]
fn process_preparation(request: &PreparationRequest<'_>) -> Result<PreparationReport, WorkerError> {
    if request.glyph_ids.is_empty() || request.glyph_ids.len() > MAX_FONT_GLYPHS_PER_SUBSET {
        return Err(WorkerError::ResourceLimit);
    }
    let unique = request.glyph_ids.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != request.glyph_ids.len()
        || unique.iter().copied().ne(request.glyph_ids.iter().copied())
    {
        return Err(WorkerError::InvalidGlyphSet);
    }
    let source = read_bounded(Path::new(request.font_path), MAX_FONT_FILE_BYTES)?;
    let source_hash = sha256(&source);
    if hex(source_hash) != request.expected_source_sha256 {
        return Err(WorkerError::SourceHashMismatch);
    }
    let glyph_set_hash = glyph_set_hash(request.glyph_ids);
    if hex(glyph_set_hash) != request.expected_glyph_set_sha256 {
        return Err(WorkerError::GlyphSetHashMismatch);
    }

    let facts = inspect_font_face(&source, request.face_index);
    if facts.identity().source_sha256() != source_hash
        || facts.identity().face_index() != request.face_index
    {
        return Err(WorkerError::SourceHashMismatch);
    }
    let (preparation_kind, license, fs_type) = match facts.embedding_eligibility() {
        FontEmbeddingEligibility::SubsetAllowed { license, fs_type } => {
            (PreparationKind::Subset, license, fs_type)
        }
        FontEmbeddingEligibility::FullFontRequired { license, fs_type }
            if matches!(request.preparation_policy, PreparationPolicy::Publication) =>
        {
            (PreparationKind::Full, license, fs_type)
        }
        FontEmbeddingEligibility::FullFontRequired { .. } => {
            return Err(WorkerError::EmbeddingDenied("full-font-required"));
        }
        FontEmbeddingEligibility::Restricted { .. } => {
            return Err(WorkerError::EmbeddingDenied("restricted"));
        }
        FontEmbeddingEligibility::BitmapOnly { .. } => {
            return Err(WorkerError::EmbeddingDenied("bitmap-only"));
        }
        FontEmbeddingEligibility::Unknown { .. } => {
            return Err(WorkerError::EmbeddingDenied("unknown"));
        }
    };
    let rights_identity = format!(
        "{}/{}/{fs_type:04x}",
        match preparation_kind {
            PreparationKind::Subset => "subset-allowed",
            PreparationKind::Full => "full-font-required",
        },
        license_name(license)
    );
    let rights_hash = domain_hash(RIGHTS_DOMAIN, rights_identity.as_bytes());

    let source_face =
        Face::parse(&source, request.face_index).map_err(|_| WorkerError::MalformedFont)?;
    let postscript_name = exact_postscript_name(&source_face)?;
    let postscript_name_hash = domain_hash(POSTSCRIPT_NAME_DOMAIN, postscript_name.as_bytes());
    if request
        .glyph_ids
        .iter()
        .any(|glyph_id| *glyph_id >= source_face.number_of_glyphs())
    {
        return Err(WorkerError::InvalidGlyphSet);
    }
    let (prepared, remapped_glyphs) = match preparation_kind {
        PreparationKind::Subset => {
            let mut remapper = GlyphRemapper::new();
            for glyph_id in request.glyph_ids {
                remapper.remap(*glyph_id);
            }
            let subset = subsetter::subset(&source, request.face_index, &remapper)
                .map_err(|_| WorkerError::SubsetFailed)?;
            let mappings = remapper
                .remapped_gids()
                .zip(0_u16..)
                .map(|(source_glyph_id, cid)| RemappedGlyph {
                    source_glyph_id,
                    cid,
                })
                .collect::<Vec<_>>();
            (subset, mappings)
        }
        PreparationKind::Full => {
            if request.face_index != 0
                || source_face.tables().glyf.is_none()
                || !matches!(source.get(..4), Some(b"\0\x01\0\0" | b"true" | b"typ1"))
            {
                return Err(WorkerError::UnsupportedFullFontContainer);
            }
            let mappings = request
                .glyph_ids
                .iter()
                .copied()
                .map(|source_glyph_id| RemappedGlyph {
                    source_glyph_id,
                    cid: source_glyph_id,
                })
                .collect::<Vec<_>>();
            (source.clone(), mappings)
        }
    };
    if prepared.is_empty() || prepared.len() > MAX_FONT_FILE_BYTES {
        return Err(WorkerError::SubsetFailed);
    }
    let (exercise_cid, exercise_unicode) =
        select_exercise_glyph(&source_face, &remapped_glyphs).ok_or(WorkerError::NoUnicodeGlyph)?;
    let face = Face::parse(&prepared, 0).map_err(|_| WorkerError::MalformedFont)?;
    let outline_kind = if face.tables().glyf.is_some() {
        OutlineKind::TrueType
    } else if face.tables().cff.is_some() {
        OutlineKind::Cff
    } else {
        return Err(WorkerError::MalformedFont);
    };
    let units_per_em = i64::from(face.units_per_em());
    let scaled = |value: i16| scale_1000(i64::from(value), units_per_em);
    let bbox = face.global_bounding_box();
    let widths = (0..face.number_of_glyphs())
        .map(|gid| {
            scale_1000(
                i64::from(face.glyph_hor_advance(GlyphId(gid)).unwrap_or(0)),
                units_per_em,
            )
        })
        .collect::<Vec<_>>();
    #[allow(clippy::cast_possible_truncation)]
    let italic_angle_millidegrees = (face.italic_angle() * 1_000.0).round() as i32;
    // PDF CIDFontType0C embeds the CFF payload directly. The subsetter omits
    // cmap, so its SFNT wrapper cannot be emitted as FontFile3/OpenType.
    // Inspect and extract here, inside confinement; metrics still come from
    // the prepared face and the attestation binds the exact emitted bytes.
    let font_program = match outline_kind {
        OutlineKind::TrueType => prepared.as_slice(),
        OutlineKind::Cff => {
            let cff = face.tables().cff.ok_or(WorkerError::MalformedFont)?;
            if cff.number_of_glyphs() != face.number_of_glyphs()
                || (0..cff.number_of_glyphs()).any(|gid| cff.glyph_cid(GlyphId(gid)) != Some(gid))
            {
                return Err(WorkerError::SubsetFailed);
            }
            let program = face
                .raw_face()
                .table(Tag::from_bytes(b"CFF "))
                .ok_or(WorkerError::MalformedFont)?;
            // The subsetter writes one Top DICT but copies the Name INDEX.
            // CIDFontType0C requires exactly one font, including that index.
            let header_size = usize::from(*program.get(2).ok_or(WorkerError::MalformedFont)?);
            if header_size < 4 || program.get(header_size..header_size + 2) != Some(b"\0\x01") {
                return Err(WorkerError::SubsetFailed);
            }
            program
        }
    };
    let prepared_hash = sha256(font_program);
    write_exclusive(Path::new(request.prepared_output_path), font_program)?;

    Ok(PreparationReport {
        protocol_version: PROTOCOL_VERSION,
        source_sha256: hex(source_hash),
        face_index: request.face_index,
        face_identity_sha256: hex(facts.identity().face_sha256()),
        postscript_name,
        postscript_name_sha256: hex(postscript_name_hash),
        glyph_set_sha256: hex(glyph_set_hash),
        preparation_kind,
        used_glyph_count: u16::try_from(request.glyph_ids.len())
            .map_err(|_| WorkerError::ResourceLimit)?,
        embedded_glyph_count: face.number_of_glyphs(),
        prepared_sha256: hex(prepared_hash),
        prepared_byte_length: font_program.len(),
        rights_identity: rights_identity.clone(),
        rights_identity_sha256: hex(rights_hash),
        engine_identity: ENGINE_IDENTITY,
        engine_identity_sha256: hex(sha256(ENGINE_IDENTITY.as_bytes())),
        rules_identity: RULES_IDENTITY,
        rules_identity_sha256: hex(sha256(RULES_IDENTITY.as_bytes())),
        outline_kind,
        bbox: [
            scaled(bbox.x_min),
            scaled(bbox.y_min),
            scaled(bbox.x_max),
            scaled(bbox.y_max),
        ],
        ascent: scaled(face.ascender()),
        descent: scaled(face.descender()),
        cap_height: scaled(face.capital_height().unwrap_or_else(|| face.ascender())),
        italic_angle_millidegrees,
        fixed_pitch: face.is_monospaced(),
        italic: face.is_italic(),
        widths,
        remapped_glyphs,
        exercise_cid,
        exercise_unicode,
    })
}

fn embedding_rights(eligibility: FontEmbeddingEligibility) -> EmbeddingRights {
    match eligibility {
        FontEmbeddingEligibility::SubsetAllowed { license, fs_type } => {
            EmbeddingRights::SubsetAllowed {
                license: embedding_license(license),
                fs_type,
            }
        }
        FontEmbeddingEligibility::FullFontRequired { license, fs_type } => {
            EmbeddingRights::FullFontRequired {
                license: embedding_license(license),
                fs_type,
            }
        }
        FontEmbeddingEligibility::Restricted { fs_type } => EmbeddingRights::Restricted { fs_type },
        FontEmbeddingEligibility::BitmapOnly { license, fs_type } => EmbeddingRights::BitmapOnly {
            license: embedding_license(license),
            fs_type,
        },
        FontEmbeddingEligibility::Unknown { reason, fs_type } => EmbeddingRights::Unknown {
            reason: match reason {
                layout_core::fonts::FontEmbeddingUnknownReason::FaceUnavailableOrMalformed => {
                    UnknownRightsReason::FaceUnavailableOrMalformed
                }
                layout_core::fonts::FontEmbeddingUnknownReason::MissingOrMalformedOs2 => {
                    UnknownRightsReason::MissingOrMalformedOs2
                }
                layout_core::fonts::FontEmbeddingUnknownReason::InvalidFsType => {
                    UnknownRightsReason::InvalidFsType
                }
            },
            fs_type,
        },
    }
}

const fn embedding_license(license: FontEmbeddingLicense) -> EmbeddingLicense {
    match license {
        FontEmbeddingLicense::Installable => EmbeddingLicense::Installable,
        FontEmbeddingLicense::PreviewAndPrint => EmbeddingLicense::PreviewAndPrint,
        FontEmbeddingLicense::Editable => EmbeddingLicense::Editable,
    }
}

fn inspection_rights_identity(rights: EmbeddingRights) -> String {
    match rights {
        EmbeddingRights::SubsetAllowed { license, fs_type } => {
            format!(
                "subset-allowed/{}/{fs_type:04x}",
                inspection_license_name(license)
            )
        }
        EmbeddingRights::FullFontRequired { license, fs_type } => format!(
            "full-font-required/{}/{fs_type:04x}",
            inspection_license_name(license)
        ),
        EmbeddingRights::Restricted { fs_type } => format!("restricted/{fs_type:04x}"),
        EmbeddingRights::BitmapOnly { license, fs_type } => {
            format!(
                "bitmap-only/{}/{fs_type:04x}",
                inspection_license_name(license)
            )
        }
        EmbeddingRights::Unknown { reason, fs_type } => format!(
            "unknown/{}/{}",
            match reason {
                UnknownRightsReason::FaceUnavailableOrMalformed => "face-unavailable-or-malformed",
                UnknownRightsReason::MissingOrMalformedOs2 => "missing-or-malformed-os2",
                UnknownRightsReason::InvalidFsType => "invalid-fs-type",
            },
            fs_type.map_or_else(|| "none".to_owned(), |value| format!("{value:04x}"))
        ),
    }
}

const fn inspection_license_name(license: EmbeddingLicense) -> &'static str {
    match license {
        EmbeddingLicense::Installable => "installable",
        EmbeddingLicense::PreviewAndPrint => "preview-and-print",
        EmbeddingLicense::Editable => "editable",
    }
}

fn exact_postscript_name(face: &Face<'_>) -> Result<String, WorkerError> {
    let names = face
        .names()
        .into_iter()
        .filter(|name| name.name_id == name_id::POST_SCRIPT_NAME)
        .filter_map(|name| name.to_string())
        .filter(|name| valid_postscript_name(name))
        .collect::<BTreeSet<_>>();
    if names.len() != 1 {
        return Err(WorkerError::MalformedFont);
    }
    names.into_iter().next().ok_or(WorkerError::MalformedFont)
}

fn exact_family_names(face: &Face<'_>) -> Result<Vec<String>, WorkerError> {
    let mut names = BTreeSet::new();
    for name_id in [
        name_id::TYPOGRAPHIC_FAMILY,
        name_id::WWS_FAMILY,
        name_id::FAMILY,
    ] {
        for name in face
            .names()
            .into_iter()
            .filter(|name| name.name_id == name_id)
            .filter_map(|name| name.to_string())
            .map(|name| name.trim().to_owned())
            .filter(|name| {
                !name.is_empty()
                    && name.len() <= MAX_FONT_FAMILY_BYTES
                    && name.chars().count() <= MAX_FONT_FAMILY_CHARS
            })
        {
            names.insert(name);
            if names.len() > MAX_FONT_FAMILY_NAMES {
                return Err(WorkerError::ResourceLimit);
            }
        }
    }
    if names.is_empty() {
        Err(WorkerError::MalformedFont)
    } else {
        Ok(names.into_iter().collect())
    }
}

fn valid_postscript_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && !b"[](){}<>/%".contains(&byte))
}

fn scale_1000(value: i64, units_per_em: i64) -> i32 {
    i32::try_from((value * 1_000).div_euclid(units_per_em)).unwrap_or_else(|_| {
        if value.is_negative() {
            i32::MIN
        } else {
            i32::MAX
        }
    })
}

fn select_exercise_glyph(face: &Face<'_>, remapped: &[RemappedGlyph]) -> Option<(u16, u32)> {
    let by_source = remapped
        .iter()
        .filter(|glyph| glyph.source_glyph_id != 0)
        .map(|glyph| (glyph.source_glyph_id, glyph.cid))
        .collect::<BTreeMap<_, _>>();
    (0x20..=0x7e)
        .chain(0xa0..=0x10_ffff)
        .filter_map(char::from_u32)
        .find_map(|character| {
            face.glyph_index(character)
                .and_then(|glyph| by_source.get(&glyph.0).copied())
                .map(|cid| (cid, u32::from(character)))
        })
}

fn license_name(license: FontEmbeddingLicense) -> &'static str {
    match license {
        FontEmbeddingLicense::Installable => "installable",
        FontEmbeddingLicense::PreviewAndPrint => "preview-and-print",
        FontEmbeddingLicense::Editable => "editable",
    }
}

fn glyph_set_hash(glyphs: &[u16]) -> [u8; 32] {
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
    hasher.finalize().into()
}

fn domain_hash(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    hasher.finalize().into()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(hash: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(64);
    for byte in hash {
        write!(&mut value, "{byte:02x}").expect("string writes cannot fail");
    }
    value
}

fn read_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>, WorkerError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| WorkerError::InputUnavailable(error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > maximum as u64 {
        return Err(WorkerError::ResourceLimit);
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(maximum as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| WorkerError::InputUnavailable(error.to_string()))?;
    if bytes.len() > maximum {
        return Err(WorkerError::ResourceLimit);
    }
    Ok(bytes)
}

fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<(), WorkerError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| WorkerError::OutputUnavailable(error.to_string()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| WorkerError::OutputUnavailable(error.to_string()))
}

fn write_json(value: &impl Serialize, code: ExitCode) -> ExitCode {
    if serde_json::to_writer(io::stdout().lock(), value).is_err() {
        return ExitCode::from(3);
    }
    code
}

impl WorkerError {
    const fn code(&self) -> &'static str {
        match self {
            Self::NoRequest => "no-request",
            Self::RequestTooLarge => "request-too-large",
            Self::InvalidRequest(_) => "invalid-request",
            Self::UnsupportedProtocol => "unsupported-protocol",
            Self::ResourceLimit => "resource-limit",
            Self::InvalidGlyphSet => "invalid-glyph-set",
            Self::InputUnavailable(_) => "input-unavailable",
            Self::SourceHashMismatch => "source-hash-mismatch",
            Self::GlyphSetHashMismatch => "glyph-set-hash-mismatch",
            Self::EmbeddingDenied(_) => "embedding-denied",
            Self::MalformedFont => "malformed-font",
            Self::SubsetFailed => "subset-failed",
            Self::NoUnicodeGlyph => "no-unicode-glyph",
            Self::UnsupportedFullFontContainer => "unsupported-full-font-container",
            Self::OutputUnavailable(_) => "output-unavailable",
        }
    }
    fn detail(&self) -> String {
        match self {
            Self::InvalidRequest(value) => value.clone(),
            Self::InputUnavailable(value) => {
                debug_assert!(!value.is_empty());
                "source unavailable".to_owned()
            }
            Self::OutputUnavailable(value) => {
                debug_assert!(!value.is_empty());
                "output unavailable".to_owned()
            }
            Self::EmbeddingDenied(value) => (*value).to_owned(),
            other => format!("{other:?}"),
        }
    }
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    const FONT: &[u8] = include_bytes!("../../../crates/layout-core/tests/fixtures/DejaVuSans.ttf");

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: std::path::PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "publisher-font-worker-test-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create isolated font-worker test directory");
            Self { path }
        }

        fn path(&self, name: &str) -> std::path::PathBuf {
            self.path.join(name)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn request(
        directory: &TestDirectory,
        font: &[u8],
        policy: PreparationPolicy,
        glyph_ids: Vec<u16>,
    ) -> WorkerRequest {
        let font_path = directory.path("source.font");
        std::fs::write(&font_path, font).expect("write exact source font");
        WorkerRequest::Prepare {
            protocol_version: PROTOCOL_VERSION,
            font_path: font_path.display().to_string(),
            expected_source_sha256: hex(sha256(font)),
            prepared_output_path: directory.path("prepared.font").display().to_string(),
            preparation_policy: policy,
            face_index: 0,
            expected_glyph_set_sha256: hex(glyph_set_hash(&glyph_ids)),
            glyph_ids,
        }
    }

    fn inspection_request(directory: &TestDirectory, font: &[u8]) -> WorkerRequest {
        let font_path = directory.path("source.font");
        std::fs::write(&font_path, font).expect("write exact source font");
        WorkerRequest::Inspect {
            protocol_version: PROTOCOL_VERSION,
            font_path: font_path.display().to_string(),
            expected_source_sha256: hex(sha256(font)),
            face_index: 0,
        }
    }

    fn preparation_report(report: WorkerReport) -> PreparationReport {
        let WorkerReport::Prepare(report) = report else {
            panic!("expected preparation report");
        };
        report
    }

    fn inspection_report(report: WorkerReport) -> InspectionReport {
        let WorkerReport::Inspect(report) = report else {
            panic!("expected inspection report");
        };
        report
    }

    fn font_with_fs_type(fs_type: u16) -> Vec<u8> {
        let mut font = FONT.to_vec();
        let table_count = usize::from(u16::from_be_bytes([font[4], font[5]]));
        let os2_record = (0..table_count)
            .map(|index| 12 + index * 16)
            .find(|offset| font[*offset..*offset + 4] == *b"OS/2")
            .expect("fixture has OS/2 table");
        let table_offset = usize::try_from(u32::from_be_bytes([
            font[os2_record + 8],
            font[os2_record + 9],
            font[os2_record + 10],
            font[os2_record + 11],
        ]))
        .expect("OS/2 offset fits usize");
        let table_length = usize::try_from(u32::from_be_bytes([
            font[os2_record + 12],
            font[os2_record + 13],
            font[os2_record + 14],
            font[os2_record + 15],
        ]))
        .expect("OS/2 length fits usize");
        let old_padded_end = table_offset + table_length.next_multiple_of(4);
        let new_length = table_length.max(96);
        let shift = new_length.next_multiple_of(4) - table_length.next_multiple_of(4);
        if shift != 0 {
            font.splice(old_padded_end..old_padded_end, vec![0; shift]);
            for index in 0..table_count {
                let record = 12 + index * 16;
                let offset = usize::try_from(u32::from_be_bytes([
                    font[record + 8],
                    font[record + 9],
                    font[record + 10],
                    font[record + 11],
                ]))
                .expect("table offset fits usize");
                if offset >= old_padded_end {
                    font[record + 8..record + 12].copy_from_slice(
                        &u32::try_from(offset + shift)
                            .expect("shifted table offset fits u32")
                            .to_be_bytes(),
                    );
                }
            }
        }
        if new_length != table_length {
            font[os2_record + 12..os2_record + 16].copy_from_slice(
                &u32::try_from(new_length)
                    .expect("expanded OS/2 table length fits u32")
                    .to_be_bytes(),
            );
        }
        font[table_offset..table_offset + 2].copy_from_slice(&3_u16.to_be_bytes());
        font[table_offset + 8..table_offset + 10].copy_from_slice(&fs_type.to_be_bytes());
        font
    }

    fn assert_error(error: &WorkerError, code: &str, detail: Option<&str>) {
        assert_eq!(error.code(), code);
        if let Some(detail) = detail {
            assert_eq!(error.detail(), detail);
        }
    }

    #[test]
    fn supported_font_produces_a_hash_bound_subset() {
        let directory = TestDirectory::new();
        let request = request(
            &directory,
            FONT,
            PreparationPolicy::SubsetOnly,
            vec![0, 36, 37],
        );

        let report =
            preparation_report(process_request(&request).expect("prepare supported subset"));
        assert!(matches!(report.preparation_kind, PreparationKind::Subset));
        assert_eq!(report.source_sha256, hex(sha256(FONT)));
        let WorkerRequest::Prepare {
            expected_glyph_set_sha256,
            ..
        } = &request
        else {
            unreachable!();
        };
        assert_eq!(report.glyph_set_sha256, *expected_glyph_set_sha256);
        assert_eq!(report.used_glyph_count, 3);
        let prepared = std::fs::read(directory.path("prepared.font")).expect("read subset output");
        assert_eq!(report.prepared_sha256, hex(sha256(&prepared)));
        assert_eq!(report.prepared_byte_length, prepared.len());
    }

    #[test]
    fn no_subsetting_rights_require_publication_policy_and_preserve_full_font() {
        let font = font_with_fs_type(0x0108);
        let directory = TestDirectory::new();
        let subset_request = request(
            &directory,
            &font,
            PreparationPolicy::SubsetOnly,
            vec![0, 36, 37],
        );
        let error =
            process_request(&subset_request).expect_err("subset-only must reject full font");
        assert_error(&error, "embedding-denied", Some("full-font-required"));

        let publication_request = request(
            &directory,
            &font,
            PreparationPolicy::Publication,
            vec![0, 36, 37],
        );
        let report = preparation_report(
            process_request(&publication_request).expect("publication keeps full font"),
        );
        assert!(matches!(report.preparation_kind, PreparationKind::Full));
        assert_eq!(report.rights_identity, "full-font-required/editable/0108");
        assert_eq!(
            std::fs::read(directory.path("prepared.font")).expect("read full font output"),
            font
        );
    }

    #[test]
    fn restricted_bitmap_only_and_unknown_rights_fail_closed() {
        for (fs_type, expected_detail) in [
            (0x0002, "restricted"),
            (0x0200, "bitmap-only"),
            (0x0400, "unknown"),
        ] {
            let directory = TestDirectory::new();
            let font = font_with_fs_type(fs_type);
            let request = request(
                &directory,
                &font,
                PreparationPolicy::Publication,
                vec![0, 36],
            );
            let error = process_request(&request).expect_err("rights must fail closed");
            assert_error(&error, "embedding-denied", Some(expected_detail));
            assert!(!directory.path("prepared.font").exists());
        }
    }

    #[test]
    fn malformed_source_and_identity_mismatches_are_rejected_without_output() {
        let malformed_directory = TestDirectory::new();
        let malformed = b"not a font";
        let malformed_request = request(
            &malformed_directory,
            malformed,
            PreparationPolicy::Publication,
            vec![1],
        );
        let error = process_request(&malformed_request).expect_err("malformed font must fail");
        assert_error(&error, "embedding-denied", Some("unknown"));
        assert!(!malformed_directory.path("prepared.font").exists());

        let source_directory = TestDirectory::new();
        let mut source_request = request(
            &source_directory,
            FONT,
            PreparationPolicy::SubsetOnly,
            vec![36],
        );
        let WorkerRequest::Prepare {
            expected_source_sha256,
            ..
        } = &mut source_request
        else {
            unreachable!();
        };
        *expected_source_sha256 = "00".repeat(32);
        let error = process_request(&source_request).expect_err("wrong source hash must fail");
        assert_error(&error, "source-hash-mismatch", None);

        let glyph_directory = TestDirectory::new();
        let mut glyph_request = request(
            &glyph_directory,
            FONT,
            PreparationPolicy::SubsetOnly,
            vec![36],
        );
        let WorkerRequest::Prepare {
            expected_glyph_set_sha256,
            ..
        } = &mut glyph_request
        else {
            unreachable!();
        };
        *expected_glyph_set_sha256 = "00".repeat(32);
        let error = process_request(&glyph_request).expect_err("wrong glyph hash must fail");
        assert_error(&error, "glyph-set-hash-mismatch", None);
    }

    #[test]
    fn empty_duplicate_unsorted_and_over_budget_glyph_sets_are_rejected_first() {
        for glyph_ids in [Vec::new(), vec![36, 36], vec![37, 36]] {
            let directory = TestDirectory::new();
            let request = request(&directory, FONT, PreparationPolicy::SubsetOnly, glyph_ids);
            let error = process_request(&request).expect_err("invalid glyph set must fail");
            let WorkerRequest::Prepare { glyph_ids, .. } = &request else {
                unreachable!();
            };
            let expected = if glyph_ids.is_empty() {
                "resource-limit"
            } else {
                "invalid-glyph-set"
            };
            assert_error(&error, expected, None);
        }

        let directory = TestDirectory::new();
        let glyph_ids = (0..=u16::try_from(MAX_FONT_GLYPHS_PER_SUBSET)
            .expect("worker glyph limit fits u16"))
            .collect::<Vec<_>>();
        let request = request(&directory, FONT, PreparationPolicy::SubsetOnly, glyph_ids);
        let error = process_request(&request).expect_err("over-budget glyph set must fail");
        assert_error(&error, "resource-limit", None);
    }

    #[test]
    fn unsupported_protocol_is_rejected_before_source_processing() {
        let directory = TestDirectory::new();
        let mut request = request(&directory, FONT, PreparationPolicy::SubsetOnly, vec![36]);
        let WorkerRequest::Prepare {
            protocol_version,
            font_path,
            ..
        } = &mut request
        else {
            unreachable!();
        };
        *protocol_version = PROTOCOL_VERSION + 1;
        std::fs::remove_file(font_path).expect("remove source after request construction");

        let error = process_request(&request).expect_err("unsupported protocol must fail first");
        assert_error(&error, "unsupported-protocol", None);
    }

    #[test]
    fn existing_output_path_fails_without_overwriting_existing_bytes() {
        let directory = TestDirectory::new();
        let request = request(&directory, FONT, PreparationPolicy::SubsetOnly, vec![0, 36]);
        let WorkerRequest::Prepare {
            prepared_output_path,
            ..
        } = &request
        else {
            unreachable!();
        };
        std::fs::write(prepared_output_path, b"keep me").expect("create existing output sentinel");

        let error = process_request(&request).expect_err("exclusive output must fail");
        assert_error(&error, "output-unavailable", None);
        assert_eq!(
            std::fs::read(prepared_output_path).expect("read output sentinel"),
            b"keep me"
        );
    }

    #[test]
    fn inspect_reports_hash_bound_container_and_rights_without_output() {
        let directory = TestDirectory::new();
        let request = inspection_request(&directory, FONT);

        let report = inspection_report(process_request(&request).expect("inspect exact font"));

        assert_eq!(report.source_sha256, hex(sha256(FONT)));
        assert_eq!(report.source_byte_length, FONT.len());
        assert_eq!(report.face_index, 0);
        assert_eq!(
            report.face_identity_sha256,
            hex(inspect_font_face(FONT, 0).identity().face_sha256())
        );
        assert!(matches!(report.container_kind, ContainerKind::SingleFont));
        assert_eq!(report.face_count, 1);
        assert_eq!(report.family_names, ["DejaVu Sans"]);
        assert!(matches!(
            report.embedding_rights,
            EmbeddingRights::SubsetAllowed {
                license: EmbeddingLicense::Installable,
                fs_type: 0
            }
        ));
        assert!(!directory.path("prepared.font").exists());
    }

    #[test]
    fn inspect_returns_denied_rights_as_facts_but_rejects_malformed_sources() {
        let restricted_directory = TestDirectory::new();
        let restricted = font_with_fs_type(0x0002);
        let request = inspection_request(&restricted_directory, &restricted);
        let report = inspection_report(process_request(&request).expect("inspect restricted font"));
        assert!(matches!(
            report.embedding_rights,
            EmbeddingRights::Restricted { fs_type: 0x0002 }
        ));

        let malformed_directory = TestDirectory::new();
        let malformed_request = inspection_request(&malformed_directory, b"not a font");
        let error = process_request(&malformed_request).expect_err("malformed source must fail");
        assert_error(&error, "malformed-font", None);
    }
}
