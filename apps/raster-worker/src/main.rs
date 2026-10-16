//! Bounded, machine-readable raster inspection worker.

mod clipboard_dib;

use document_core::raster_capacity::{MAX_PRODUCTION_RASTER_PIXELS, production_raster_pixels};
use document_core::{
    AssetHash, MAX_RASTER_FILE_BYTES, RasterIngestError, RasterInspection, UntaggedRasterPolicy,
    inspect_raster,
};
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{self, Cursor, Read, Write},
    path::Path,
    process::ExitCode,
};

const PROTOCOL_VERSION: u16 = 2;
const MAX_REQUEST_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerRequest {
    protocol_version: u16,
    candidate_path: String,
    expected_asset_sha256: String,
    untagged_raster_policy: UntaggedRasterPolicy,
    #[serde(default)]
    rgb_output_path: Option<String>,
    #[serde(default)]
    alpha_output_path: Option<String>,
    #[serde(default)]
    source_profile_output_path: Option<String>,
    #[serde(default)]
    clipboard_dib_output_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct WorkerReport {
    protocol_version: u16,
    asset_sha256: String,
    inspection: RasterInspection,
    #[serde(skip_serializing_if = "Option::is_none")]
    decoded: Option<DecodedReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    normalized_png: Option<NormalizedPngReport>,
}

#[derive(Debug, Serialize)]
struct NormalizedPngReport {
    png_sha256: String,
    png_byte_length: usize,
    normalization_rules_identity: &'static str,
    normalization_rules_sha256: String,
}

#[derive(Debug, Serialize)]
struct DecodedReport {
    oriented_width: u32,
    oriented_height: u32,
    exif_orientation_applied: u8,
    rgb_sha256: String,
    rgb_byte_length: usize,
    alpha_sha256: String,
    alpha_byte_length: usize,
    has_transparency: bool,
    source_profile_origin: SourceProfileOrigin,
    source_profile_sha256: String,
    source_profile_byte_length: usize,
    decode_rules_identity: &'static str,
    decode_rules_sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SourceProfileOrigin {
    Embedded,
    AssumedSrgb,
}

const DECODE_RULES_IDENTITY: &str = "publisher-v2/raster-decode/v3/oriented-rgb8/straight-alpha8/bounded-tiff-icc/embedded-or-explicit-srgb";

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
    Raster(RasterIngestError),
    MissingColorProfile,
    InvalidOutputRequest,
    DecodeFailed(String),
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
    serde_json::from_slice(&bytes).map_err(|error| WorkerError::InvalidRequest(error.to_string()))
}

fn process_request(request: &WorkerRequest) -> Result<WorkerReport, WorkerError> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(WorkerError::UnsupportedProtocol);
    }
    let expected = AssetHash::parse(request.expected_asset_sha256.clone())
        .map_err(|error| WorkerError::InvalidRequest(error.to_string()))?;
    let bytes = read_candidate(Path::new(&request.candidate_path))?;
    let actual = AssetHash::for_bytes(&bytes);
    if actual != expected {
        return Err(WorkerError::ArtifactMismatch);
    }
    if let Some(output) = &request.clipboard_dib_output_path {
        if request.rgb_output_path.is_some()
            || request.alpha_output_path.is_some()
            || request.source_profile_output_path.is_some()
        {
            return Err(WorkerError::InvalidOutputRequest);
        }
        let png = clipboard_dib::normalize(&bytes)?;
        let inspection = inspect_raster(&png).map_err(WorkerError::Raster)?;
        write_exclusive(Path::new(output), &png)?;
        return Ok(WorkerReport {
            protocol_version: PROTOCOL_VERSION,
            asset_sha256: actual.as_str().to_owned(),
            inspection,
            decoded: None,
            normalized_png: Some(NormalizedPngReport {
                png_sha256: hash_hex(&png),
                png_byte_length: png.len(),
                normalization_rules_identity: clipboard_dib::RULES_IDENTITY,
                normalization_rules_sha256: hash_hex(clipboard_dib::RULES_IDENTITY.as_bytes()),
            }),
        });
    }
    let inspection = inspect_raster(&bytes).map_err(WorkerError::Raster)?;
    let outputs = match (
        &request.rgb_output_path,
        &request.alpha_output_path,
        &request.source_profile_output_path,
    ) {
        (None, None, None) => None,
        (Some(rgb), Some(alpha), Some(profile)) => Some((rgb, alpha, profile)),
        _ => return Err(WorkerError::InvalidOutputRequest),
    };
    let decoded = outputs
        .map(|(rgb, alpha, profile)| {
            decode_and_write(&bytes, rgb, alpha, profile, request.untagged_raster_policy)
        })
        .transpose()?;
    Ok(WorkerReport {
        protocol_version: PROTOCOL_VERSION,
        asset_sha256: actual.as_str().to_owned(),
        inspection,
        decoded,
        normalized_png: None,
    })
}

fn decode_and_write(
    bytes: &[u8],
    rgb_output: &str,
    alpha_output: &str,
    profile_output: &str,
    untagged_raster_policy: UntaggedRasterPolicy,
) -> Result<DecodedReport, WorkerError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| WorkerError::DecodeFailed(error.to_string()))?;
    let format = reader
        .format()
        .ok_or_else(|| WorkerError::DecodeFailed("missing raster format".into()))?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::Tiff
    ) {
        return Err(WorkerError::DecodeFailed(
            "unsupported raster format".into(),
        ));
    }
    let mut limits = Limits::default();
    limits.max_alloc = Some((MAX_PRODUCTION_RASTER_PIXELS * 4) as u64);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| WorkerError::DecodeFailed(error.to_string()))?;
    let (encoded_width, encoded_height) = decoder.dimensions();
    production_raster_pixels(encoded_width, encoded_height).map_err(|_| {
        WorkerError::DecodeFailed("decoded raster exceeds Letter1200+bleed budget".into())
    })?;
    let orientation = decoder
        .orientation()
        .map_err(|error| WorkerError::DecodeFailed(error.to_string()))?;
    // The image wrapper can silently lose TIFF metadata after setting pixel
    // limits. Share inspection's bounded, strict tag read before applying policy.
    let embedded_profile = if format == image::ImageFormat::Tiff {
        document_core::assets::tiff_embedded_icc_profile(bytes).map_err(WorkerError::Raster)?
    } else {
        decoder
            .icc_profile()
            .map_err(|error| WorkerError::DecodeFailed(error.to_string()))?
            .filter(|profile| !profile.is_empty())
    };
    let (source_profile, source_profile_origin) = match embedded_profile {
        Some(profile) => (profile, SourceProfileOrigin::Embedded),
        None if untagged_raster_policy == UntaggedRasterPolicy::AssumeSrgb => {
            (canonical_srgb_profile()?, SourceProfileOrigin::AssumedSrgb)
        }
        None => return Err(WorkerError::MissingColorProfile),
    };
    if source_profile.len() > document_core::MAX_ICC_PROFILE_BYTES {
        return Err(WorkerError::Raster(RasterIngestError::ColorProfileExceeded));
    }
    let mut image = DynamicImage::from_decoder(decoder)
        .map_err(|error| WorkerError::DecodeFailed(error.to_string()))?;
    image.apply_orientation(orientation);
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    let pixel_count = production_raster_pixels(width, height).map_err(|_| {
        WorkerError::DecodeFailed("decoded raster exceeds production pixel budget".into())
    })?;
    let mut rgb = Vec::new();
    rgb.try_reserve_exact(pixel_count * 3)
        .map_err(|_| WorkerError::DecodeFailed("RGB allocation refused".into()))?;
    let mut alpha = Vec::new();
    alpha
        .try_reserve_exact(pixel_count)
        .map_err(|_| WorkerError::DecodeFailed("alpha allocation refused".into()))?;
    let mut has_transparency = false;
    for pixel in rgba.pixels() {
        rgb.extend_from_slice(&pixel.0[..3]);
        alpha.push(pixel.0[3]);
        has_transparency |= pixel.0[3] != u8::MAX;
    }
    write_exclusive(Path::new(rgb_output), &rgb)?;
    write_exclusive(Path::new(alpha_output), &alpha)?;
    write_exclusive(Path::new(profile_output), &source_profile)?;
    Ok(DecodedReport {
        oriented_width: width,
        oriented_height: height,
        exif_orientation_applied: orientation.to_exif(),
        rgb_sha256: hash_hex(&rgb),
        rgb_byte_length: rgb.len(),
        alpha_sha256: hash_hex(&alpha),
        alpha_byte_length: alpha.len(),
        has_transparency,
        source_profile_origin,
        source_profile_sha256: hash_hex(&source_profile),
        source_profile_byte_length: source_profile.len(),
        decode_rules_identity: DECODE_RULES_IDENTITY,
        decode_rules_sha256: hash_hex(DECODE_RULES_IDENTITY.as_bytes()),
    })
}

fn canonical_srgb_profile() -> Result<Vec<u8>, WorkerError> {
    let mut profile = lcms2::Profile::new_srgb()
        .icc()
        .map_err(|error| WorkerError::DecodeFailed(error.to_string()))?;
    if profile.len() < 128 || profile.get(36..40) != Some(b"acsp") {
        return Err(WorkerError::DecodeFailed(
            "LittleCMS returned an invalid sRGB profile".to_owned(),
        ));
    }
    // LittleCMS stamps virtual profiles with the current time and host
    // platform. Normalize those informational header fields so the explicit
    // assumed-sRGB policy produces identical evidence on every supported host.
    profile[24..36].copy_from_slice(&[
        0x07, 0xd0, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);
    profile[40..44].copy_from_slice(b"MSFT");
    profile[84..100].fill(0);
    Ok(profile)
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

fn hash_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn read_candidate(path: &Path) -> Result<Vec<u8>, WorkerError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| WorkerError::CandidateUnavailable(error.to_string()))?;
    if !metadata.file_type().is_file() {
        return Err(WorkerError::CandidateUnavailable(
            "candidate path is not a regular file".to_owned(),
        ));
    }
    if metadata.len() > MAX_RASTER_FILE_BYTES as u64 {
        return Err(WorkerError::CandidateTooLarge);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)
        .and_then(|file| {
            file.take(MAX_RASTER_FILE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| WorkerError::CandidateUnavailable(error.to_string()))?;
    if bytes.len() > MAX_RASTER_FILE_BYTES {
        return Err(WorkerError::CandidateTooLarge);
    }
    Ok(bytes)
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
            Self::Raster(_) => "raster-rejected",
            Self::MissingColorProfile => "missing-color-profile",
            Self::InvalidOutputRequest => "invalid-output-request",
            Self::DecodeFailed(_) => "decode-failed",
            Self::OutputUnavailable(_) => "output-unavailable",
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::RequestTooLarge => "request exceeds the 64 KiB protocol limit".to_owned(),
            Self::InvalidRequest(detail)
            | Self::CandidateUnavailable(detail)
            | Self::DecodeFailed(detail)
            | Self::OutputUnavailable(detail) => detail.clone(),
            Self::UnsupportedProtocol => "unsupported raster-worker protocol version".to_owned(),
            Self::CandidateTooLarge => "candidate exceeds the 256 MiB import limit".to_owned(),
            Self::ArtifactMismatch => {
                "candidate bytes do not match expected_asset_sha256".to_owned()
            }
            Self::Raster(error) => error.to_string(),
            Self::MissingColorProfile => {
                "production raster decoding requires an embedded RGB ICC profile".to_owned()
            }
            Self::InvalidOutputRequest => {
                "all decoded output paths must be supplied together".to_owned()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PROTOCOL_VERSION, SourceProfileOrigin, WorkerError, WorkerRequest, canonical_srgb_profile,
        hash_hex, process_request,
    };
    use document_core::{AssetHash, UntaggedRasterPolicy};
    use image::{
        DynamicImage, ExtendedColorType, ImageEncoder, ImageFormat, codecs::png::PngEncoder,
    };
    use std::{fs, io::Cursor, path::PathBuf, time::SystemTime};

    fn temp_png() -> (PathBuf, Vec<u8>) {
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(23, 11)
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode PNG");
        let bytes = encoded.into_inner();
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("publisher-raster-{nonce}.png"));
        fs::write(&path, &bytes).expect("write raster fixture");
        (path, bytes)
    }

    #[test]
    fn exact_bytes_produce_bounded_machine_readable_metadata() {
        let (path, bytes) = temp_png();
        let report = process_request(&WorkerRequest {
            protocol_version: PROTOCOL_VERSION,
            candidate_path: path.display().to_string(),
            expected_asset_sha256: AssetHash::for_bytes(&bytes).as_str().to_owned(),
            untagged_raster_policy: UntaggedRasterPolicy::RequireEmbeddedProfile,
            rgb_output_path: None,
            alpha_output_path: None,
            source_profile_output_path: None,
            clipboard_dib_output_path: None,
        })
        .expect("inspect exact raster");
        assert_eq!(report.inspection.width_pixels, 23);
        assert_eq!(report.inspection.height_pixels, 11);
        let _ignored = fs::remove_file(path);
    }

    #[test]
    fn hash_mismatch_stops_before_raster_acceptance() {
        let (path, _bytes) = temp_png();
        let error = process_request(&WorkerRequest {
            protocol_version: PROTOCOL_VERSION,
            candidate_path: path.display().to_string(),
            expected_asset_sha256: "0".repeat(64),
            untagged_raster_policy: UntaggedRasterPolicy::RequireEmbeddedProfile,
            rgb_output_path: None,
            alpha_output_path: None,
            source_profile_output_path: None,
            clipboard_dib_output_path: None,
        })
        .expect_err("mismatched bytes must reject");
        assert!(matches!(error, WorkerError::ArtifactMismatch));
        let _ignored = fs::remove_file(path);
    }

    #[test]
    fn production_decode_outputs_oriented_rgb_alpha_and_exact_profile() {
        let profile = lcms2::Profile::new_srgb()
            .icc()
            .expect("serialize sRGB profile");
        let pixels = [255, 0, 0, 255, 0, 255, 0, 64];
        let mut bytes = Vec::new();
        let mut encoder = PngEncoder::new(&mut bytes);
        encoder
            .set_icc_profile(profile.clone())
            .expect("PNG supports ICC");
        encoder
            .set_exif_metadata(exif_orientation(6))
            .expect("PNG supports EXIF");
        encoder
            .write_image(&pixels, 2, 1, ExtendedColorType::Rgba8)
            .expect("encode tagged PNG");
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("publisher-raster-decode-test-{nonce}"));
        fs::create_dir(&directory).expect("create test directory");
        let source = directory.join("source.png");
        let rgb = directory.join("decoded.rgb");
        let alpha = directory.join("decoded.alpha");
        let icc = directory.join("source.icc");
        fs::write(&source, &bytes).expect("write source");
        let report = process_request(&WorkerRequest {
            protocol_version: PROTOCOL_VERSION,
            candidate_path: source.display().to_string(),
            expected_asset_sha256: AssetHash::for_bytes(&bytes).as_str().to_owned(),
            untagged_raster_policy: UntaggedRasterPolicy::RequireEmbeddedProfile,
            rgb_output_path: Some(rgb.display().to_string()),
            alpha_output_path: Some(alpha.display().to_string()),
            source_profile_output_path: Some(icc.display().to_string()),
            clipboard_dib_output_path: None,
        })
        .expect("decode tagged raster");
        let decoded = report.decoded.expect("decoded evidence");
        assert_eq!(decoded.oriented_width, 1);
        assert_eq!(decoded.oriented_height, 2);
        assert_eq!(decoded.exif_orientation_applied, 6);
        assert!(decoded.has_transparency);
        assert_eq!(decoded.source_profile_origin, SourceProfileOrigin::Embedded);
        assert_eq!(fs::read(&rgb).expect("read RGB"), [255, 0, 0, 0, 255, 0]);
        assert_eq!(fs::read(&alpha).expect("read alpha"), [255, 64]);
        assert_eq!(fs::read(&icc).expect("read ICC"), profile);
        for path in [source, rgb, alpha, icc] {
            let _ignored = fs::remove_file(path);
        }
        let _ignored = fs::remove_dir(directory);
    }

    fn exif_orientation(value: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"II\x2a\0");
        bytes.extend_from_slice(&8_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&274_u16.to_le_bytes());
        bytes.extend_from_slice(&3_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&value.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes
    }

    #[test]
    fn production_decode_rejects_missing_profile_and_partial_outputs() {
        let (source, bytes) = temp_png();
        let directory = source.parent().expect("temporary parent");
        let rgb = directory.join("publisher-raster-missing-profile.rgb");
        let alpha = directory.join("publisher-raster-missing-profile.alpha");
        let profile = directory.join("publisher-raster-missing-profile.icc");
        let error = process_request(&WorkerRequest {
            protocol_version: PROTOCOL_VERSION,
            candidate_path: source.display().to_string(),
            expected_asset_sha256: AssetHash::for_bytes(&bytes).as_str().to_owned(),
            untagged_raster_policy: UntaggedRasterPolicy::RequireEmbeddedProfile,
            rgb_output_path: Some(rgb.display().to_string()),
            alpha_output_path: Some(alpha.display().to_string()),
            source_profile_output_path: Some(profile.display().to_string()),
            clipboard_dib_output_path: None,
        })
        .expect_err("untagged production raster must reject");
        assert!(matches!(error, WorkerError::MissingColorProfile));
        let partial = process_request(&WorkerRequest {
            protocol_version: PROTOCOL_VERSION,
            candidate_path: source.display().to_string(),
            expected_asset_sha256: AssetHash::for_bytes(&bytes).as_str().to_owned(),
            untagged_raster_policy: UntaggedRasterPolicy::RequireEmbeddedProfile,
            rgb_output_path: Some(rgb.display().to_string()),
            alpha_output_path: None,
            source_profile_output_path: None,
            clipboard_dib_output_path: None,
        })
        .expect_err("partial output contract must reject");
        assert!(matches!(partial, WorkerError::InvalidOutputRequest));
        let _ignored = fs::remove_file(source);
    }

    #[test]
    fn explicit_policy_decodes_untagged_raster_with_canonical_srgb_profile() {
        let (source, bytes) = temp_png();
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("publisher-raster-srgb-test-{nonce}"));
        fs::create_dir(&directory).expect("create test directory");
        let rgb = directory.join("decoded.rgb");
        let alpha = directory.join("decoded.alpha");
        let profile = directory.join("source.icc");
        let report = process_request(&WorkerRequest {
            protocol_version: PROTOCOL_VERSION,
            candidate_path: source.display().to_string(),
            expected_asset_sha256: AssetHash::for_bytes(&bytes).as_str().to_owned(),
            untagged_raster_policy: UntaggedRasterPolicy::AssumeSrgb,
            rgb_output_path: Some(rgb.display().to_string()),
            alpha_output_path: Some(alpha.display().to_string()),
            source_profile_output_path: Some(profile.display().to_string()),
            clipboard_dib_output_path: None,
        })
        .expect("explicit sRGB policy decodes untagged raster");
        let decoded = report.decoded.expect("decoded evidence");
        assert_eq!(
            decoded.source_profile_origin,
            SourceProfileOrigin::AssumedSrgb
        );
        let profile_bytes = fs::read(&profile).expect("read assumed profile");
        lcms2::Profile::new_icc(&profile_bytes).expect("assumed profile is valid ICC");
        assert_eq!(decoded.source_profile_sha256, hash_hex(&profile_bytes));
        assert_eq!(profile_bytes, canonical_srgb_profile().unwrap());
        assert_eq!(
            &profile_bytes[24..36],
            &[0x07, 0xd0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(&profile_bytes[40..44], b"MSFT");

        let _ignored = fs::remove_file(source);
        for path in [rgb, alpha, profile] {
            let _ignored = fs::remove_file(path);
        }
        let _ignored = fs::remove_dir(directory);
    }
}
