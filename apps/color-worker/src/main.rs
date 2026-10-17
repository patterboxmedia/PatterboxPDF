//! Bounded, machine-readable ICC color-conversion worker.

use lcms2::{ColorSpaceSignature, Intent, PixelFormat, Profile, ProfileClassSignature, Transform};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    process::ExitCode,
};

const PROTOCOL_VERSION: u16 = 1;
const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;
const RGB_CHANNELS: usize = 3;
const CMYK_CHANNELS: usize = 4;
const ICC_HEADER_BYTES: usize = 128;
const ICC_TAG_COUNT_BYTES: usize = 4;
const ICC_TAG_RECORD_BYTES: usize = 12;

const ENGINE_IDENTITY: &str = "LittleCMS/lcms2-rs-6.2.0/RGB8-CMYK8";
const RULES_IDENTITY: &str = "publisher-v2/color-conversion/v1/relative-colorimetric/no-proofing";
const INSPECTION_RULES: &str =
    "patterboxpdf/output-intent/v2/exact-edition-types/all-intents-bidirectional";

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Request {
    Conversion(WorkerRequest),
    Inspection(InspectionRequest),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectionRequest {
    protocol_version: u16,
    operation: String,
    output_profile_path: String,
    expected_output_profile_sha256: String,
    expected_byte_length: usize,
}

#[derive(Debug, Serialize)]
struct InspectionReport {
    protocol_version: u16,
    operation: &'static str,
    output_profile_sha256: String,
    byte_length: usize,
    engine_identity: &'static str,
    rules_identity: &'static str,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Report {
    Conversion(WorkerReport),
    Inspection(InspectionReport),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerRequest {
    protocol_version: u16,
    rgb_pixels_path: String,
    expected_rgb_pixels_sha256: String,
    source_profile_path: String,
    expected_source_profile_sha256: String,
    output_profile_path: String,
    expected_output_profile_sha256: String,
    converted_output_path: String,
    width: u32,
    height: u32,
    rendering_intent: RenderingIntent,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RenderingIntent {
    RelativeColorimetric,
}

#[derive(Debug, Serialize)]
struct WorkerReport {
    protocol_version: u16,
    source_profile_sha256: String,
    raw_pixels_sha256: String,
    output_profile_sha256: String,
    converted_sha256: String,
    converted_byte_length: usize,
    width: u32,
    height: u32,
    rendering_intent: RenderingIntent,
    engine_identity: &'static str,
    engine_identity_sha256: String,
    rules_identity: &'static str,
    rules_identity_sha256: String,
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
    InvalidDimensions,
    PixelBudgetExceeded,
    InputUnavailable(&'static str, String),
    InputTooLarge(&'static str),
    InputLengthMismatch,
    HashMismatch(&'static str),
    InvalidIcc(&'static str, String),
    UnsupportedColorSpace(&'static str),
    TransformRejected,
    OutputUnavailable(String),
}

fn main() -> ExitCode {
    match read_request().and_then(|request| match request {
        Request::Conversion(request) => process_request(&request).map(Report::Conversion),
        Request::Inspection(request) => inspect_profile(&request).map(Report::Inspection),
    }) {
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

fn read_request() -> Result<Request, WorkerError> {
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

fn inspect_profile(request: &InspectionRequest) -> Result<InspectionReport, WorkerError> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(WorkerError::UnsupportedProtocol);
    }
    if request.operation != "inspect-output-profile" {
        return Err(WorkerError::InvalidRequest(
            "unsupported inspection operation".into(),
        ));
    }
    if request.expected_byte_length == 0 || request.expected_byte_length > MAX_ICC_PROFILE_BYTES {
        return Err(WorkerError::InputTooLarge("output profile"));
    }
    let expected_hash = parse_hash(&request.expected_output_profile_sha256)?;
    let bytes = read_input_bounded(
        Path::new(&request.output_profile_path),
        MAX_ICC_PROFILE_BYTES,
        "output profile",
    )?;
    verify_hash(&bytes, expected_hash, "output profile")?;
    if bytes.len() != request.expected_byte_length {
        return Err(WorkerError::InvalidIcc(
            "output profile",
            "ICC byte length changed".into(),
        ));
    }
    validate_output_profile(&bytes)?;
    let profile = Profile::new_icc(&bytes).map_err(|_| {
        WorkerError::InvalidIcc("output profile", "LittleCMS rejected the profile".into())
    })?;
    if profile.color_space() != ColorSpaceSignature::CmykData
        || profile.device_class() != ProfileClassSignature::OutputClass
    {
        return Err(WorkerError::UnsupportedColorSpace("output profile"));
    }
    probe_output_profile(&profile)?;
    Ok(InspectionReport {
        protocol_version: PROTOCOL_VERSION,
        operation: "inspect-output-profile",
        output_profile_sha256: hex(sha256(&bytes)),
        byte_length: bytes.len(),
        engine_identity: ENGINE_IDENTITY,
        rules_identity: INSPECTION_RULES,
    })
}

fn probe_output_profile(profile: &Profile) -> Result<(), WorkerError> {
    let pcs_profile = Profile::new_xyz();
    for intent in [
        Intent::Perceptual,
        Intent::RelativeColorimetric,
        Intent::Saturation,
        Intent::AbsoluteColorimetric,
    ] {
        let device_to_pcs = Transform::<[u8; 4], [u16; 3]>::new(
            profile,
            PixelFormat::CMYK_8,
            &pcs_profile,
            PixelFormat::XYZ_16,
            intent,
        )
        .map_err(|_| {
            WorkerError::InvalidIcc("output profile", "device-to-PCS transform rejected".into())
        })?;
        let mut pcs_sample = [[0_u16; 3]];
        device_to_pcs.transform_pixels(&[[0_u8; 4]], &mut pcs_sample);
        let pcs_to_device = Transform::<[u16; 3], [u8; 4]>::new(
            &pcs_profile,
            PixelFormat::XYZ_16,
            profile,
            PixelFormat::CMYK_8,
            intent,
        )
        .map_err(|_| {
            WorkerError::InvalidIcc("output profile", "PCS-to-device transform rejected".into())
        })?;
        pcs_to_device.transform_pixels(&pcs_sample, &mut [[0_u8; 4]]);
    }
    Ok(())
}

fn process_request(request: &WorkerRequest) -> Result<WorkerReport, WorkerError> {
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(WorkerError::UnsupportedProtocol);
    }

    let pixel_count = checked_pixel_count(request.width, request.height)?;
    let expected_rgb_bytes = pixel_count
        .checked_mul(RGB_CHANNELS)
        .ok_or(WorkerError::PixelBudgetExceeded)?;

    let expected_pixels_hash = parse_hash(&request.expected_rgb_pixels_sha256)?;
    let expected_source_hash = parse_hash(&request.expected_source_profile_sha256)?;
    let expected_output_hash = parse_hash(&request.expected_output_profile_sha256)?;

    let rgb_pixels = read_input_exact(
        Path::new(&request.rgb_pixels_path),
        expected_rgb_bytes,
        "RGB pixels",
    )?;
    verify_hash(&rgb_pixels, expected_pixels_hash, "RGB pixels")?;

    let source_profile_bytes = read_input_bounded(
        Path::new(&request.source_profile_path),
        MAX_ICC_PROFILE_BYTES,
        "source profile",
    )?;
    verify_hash(
        &source_profile_bytes,
        expected_source_hash,
        "source profile",
    )?;
    validate_icc_container(&source_profile_bytes, "source profile")?;

    let output_profile_bytes = read_input_bounded(
        Path::new(&request.output_profile_path),
        MAX_ICC_PROFILE_BYTES,
        "output profile",
    )?;
    verify_hash(
        &output_profile_bytes,
        expected_output_hash,
        "output profile",
    )?;
    validate_output_profile(&output_profile_bytes)?;

    let source_profile = Profile::new_icc(&source_profile_bytes).map_err(|_| {
        WorkerError::InvalidIcc("source profile", "LittleCMS rejected the profile".into())
    })?;
    if source_profile.color_space() != ColorSpaceSignature::RgbData {
        return Err(WorkerError::UnsupportedColorSpace("source profile"));
    }

    let output_profile = Profile::new_icc(&output_profile_bytes).map_err(|_| {
        WorkerError::InvalidIcc("output profile", "LittleCMS rejected the profile".into())
    })?;
    if output_profile.color_space() != ColorSpaceSignature::CmykData
        || output_profile.device_class() != ProfileClassSignature::OutputClass
    {
        return Err(WorkerError::UnsupportedColorSpace("output profile"));
    }

    probe_output_profile(&output_profile)?;

    let intent = match request.rendering_intent {
        RenderingIntent::RelativeColorimetric => Intent::RelativeColorimetric,
    };
    let transform = Transform::<[u8; RGB_CHANNELS], [u8; CMYK_CHANNELS]>::new(
        &source_profile,
        PixelFormat::RGB_8,
        &output_profile,
        PixelFormat::CMYK_8,
        intent,
    )
    .map_err(|_| WorkerError::TransformRejected)?;

    let (source_pixels, remainder) = rgb_pixels.as_chunks::<RGB_CHANNELS>();
    debug_assert!(remainder.is_empty());
    let mut converted = Vec::new();
    converted
        .try_reserve_exact(pixel_count)
        .map_err(|_| WorkerError::PixelBudgetExceeded)?;
    converted.resize(pixel_count, [0_u8; CMYK_CHANNELS]);
    transform.transform_pixels(source_pixels, &mut converted);
    let converted = converted.into_flattened();
    let converted_hash = sha256(&converted);
    write_exclusive(Path::new(&request.converted_output_path), &converted)?;

    Ok(WorkerReport {
        protocol_version: PROTOCOL_VERSION,
        source_profile_sha256: hex(expected_source_hash),
        raw_pixels_sha256: hex(expected_pixels_hash),
        output_profile_sha256: hex(expected_output_hash),
        converted_sha256: hex(converted_hash),
        converted_byte_length: converted.len(),
        width: request.width,
        height: request.height,
        rendering_intent: request.rendering_intent,
        engine_identity: ENGINE_IDENTITY,
        engine_identity_sha256: hex(sha256(ENGINE_IDENTITY.as_bytes())),
        rules_identity: RULES_IDENTITY,
        rules_identity_sha256: hex(sha256(RULES_IDENTITY.as_bytes())),
    })
}

fn validate_output_profile(bytes: &[u8]) -> Result<(), WorkerError> {
    icc_profile::inspect_output(bytes).map_err(|error| {
        WorkerError::InvalidIcc(
            "output profile",
            format!("exact ICC byte validation: {error:?}"),
        )
    })
}

fn checked_pixel_count(width: u32, height: u32) -> Result<usize, WorkerError> {
    use raster_capacity::{MAX_PRODUCTION_RASTER_DIMENSION, production_raster_pixels};
    if width == 0
        || height == 0
        || width > MAX_PRODUCTION_RASTER_DIMENSION
        || height > MAX_PRODUCTION_RASTER_DIMENSION
    {
        return Err(WorkerError::InvalidDimensions);
    }
    production_raster_pixels(width, height).map_err(|_| WorkerError::PixelBudgetExceeded)
}

fn read_input_exact(
    path: &Path,
    expected_len: usize,
    label: &'static str,
) -> Result<Vec<u8>, WorkerError> {
    let bytes = read_input_bounded(path, expected_len, label)?;
    if bytes.len() != expected_len {
        return Err(WorkerError::InputLengthMismatch);
    }
    Ok(bytes)
}

fn read_input_bounded(
    path: &Path,
    maximum: usize,
    label: &'static str,
) -> Result<Vec<u8>, WorkerError> {
    let path_metadata = path
        .symlink_metadata()
        .map_err(|error| WorkerError::InputUnavailable(label, error.to_string()))?;
    if !path_metadata.file_type().is_file() {
        return Err(WorkerError::InputUnavailable(
            label,
            "path is not a regular file".into(),
        ));
    }
    if path_metadata.len() > maximum as u64 {
        return Err(WorkerError::InputTooLarge(label));
    }

    let file = File::open(path)
        .map_err(|error| WorkerError::InputUnavailable(label, error.to_string()))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| WorkerError::InputUnavailable(label, error.to_string()))?;
    if !opened_metadata.file_type().is_file() {
        return Err(WorkerError::InputUnavailable(
            label,
            "opened input is not a regular file".into(),
        ));
    }
    if opened_metadata.len() > maximum as u64 {
        return Err(WorkerError::InputTooLarge(label));
    }

    let mut bytes = Vec::with_capacity(usize::try_from(opened_metadata.len()).unwrap_or(0));
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| WorkerError::InputUnavailable(label, error.to_string()))?;
    if bytes.len() > maximum {
        return Err(WorkerError::InputTooLarge(label));
    }
    Ok(bytes)
}

fn validate_icc_container(bytes: &[u8], label: &'static str) -> Result<(), WorkerError> {
    if bytes.len() < ICC_HEADER_BYTES + ICC_TAG_COUNT_BYTES || &bytes[36..40] != b"acsp" {
        return Err(WorkerError::InvalidIcc(
            label,
            "missing or malformed ICC header".into(),
        ));
    }
    let declared = u32::from_be_bytes(bytes[0..4].try_into().map_err(
        |error: std::array::TryFromSliceError| WorkerError::InvalidIcc(label, error.to_string()),
    )?);
    let declared = usize::try_from(declared)
        .map_err(|_| WorkerError::InvalidIcc(label, "declared size is unsupported".into()))?;
    if declared != bytes.len() {
        return Err(WorkerError::InvalidIcc(
            label,
            "declared ICC size does not match the file".into(),
        ));
    }

    let tag_count = u32::from_be_bytes(
        bytes[ICC_HEADER_BYTES..ICC_HEADER_BYTES + ICC_TAG_COUNT_BYTES]
            .try_into()
            .map_err(|error: std::array::TryFromSliceError| {
                WorkerError::InvalidIcc(label, error.to_string())
            })?,
    );
    let tag_count = usize::try_from(tag_count)
        .map_err(|_| WorkerError::InvalidIcc(label, "tag count is unsupported".into()))?;
    let table_bytes = tag_count
        .checked_mul(ICC_TAG_RECORD_BYTES)
        .ok_or_else(|| WorkerError::InvalidIcc(label, "tag table overflows".into()))?;
    let table_end = (ICC_HEADER_BYTES + ICC_TAG_COUNT_BYTES)
        .checked_add(table_bytes)
        .ok_or_else(|| WorkerError::InvalidIcc(label, "tag table overflows".into()))?;
    if table_end > bytes.len() {
        return Err(WorkerError::InvalidIcc(
            label,
            "tag table extends beyond the profile".into(),
        ));
    }

    for index in 0..tag_count {
        let record = ICC_HEADER_BYTES + ICC_TAG_COUNT_BYTES + index * ICC_TAG_RECORD_BYTES;
        let offset = u32::from_be_bytes(bytes[record + 4..record + 8].try_into().map_err(
            |error: std::array::TryFromSliceError| {
                WorkerError::InvalidIcc(label, error.to_string())
            },
        )?);
        let size = u32::from_be_bytes(bytes[record + 8..record + 12].try_into().map_err(
            |error: std::array::TryFromSliceError| {
                WorkerError::InvalidIcc(label, error.to_string())
            },
        )?);
        let offset = usize::try_from(offset)
            .map_err(|_| WorkerError::InvalidIcc(label, "tag offset is unsupported".into()))?;
        let size = usize::try_from(size)
            .map_err(|_| WorkerError::InvalidIcc(label, "tag size is unsupported".into()))?;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| WorkerError::InvalidIcc(label, "tag extent overflows".into()))?;
        if offset < table_end || offset % 4 != 0 || size < 8 || end > bytes.len() {
            return Err(WorkerError::InvalidIcc(
                label,
                "tag payload is outside the profile or malformed".into(),
            ));
        }
    }
    Ok(())
}

fn parse_hash(value: &str) -> Result<[u8; 32], WorkerError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(WorkerError::InvalidRequest(
            "SHA-256 fields must contain exactly 64 hexadecimal characters".into(),
        ));
    }
    let mut bytes = [0_u8; 32];
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    for (index, pair) in pairs.iter().enumerate() {
        let pair = std::str::from_utf8(pair)
            .map_err(|error| WorkerError::InvalidRequest(error.to_string()))?;
        bytes[index] = u8::from_str_radix(pair, 16)
            .map_err(|error| WorkerError::InvalidRequest(error.to_string()))?;
    }
    Ok(bytes)
}

fn verify_hash(bytes: &[u8], expected: [u8; 32], label: &'static str) -> Result<(), WorkerError> {
    if sha256(bytes) != expected {
        return Err(WorkerError::HashMismatch(label));
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: [u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<(), WorkerError> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| WorkerError::OutputUnavailable(error.to_string()))?;
    if let Err(error) = output.write_all(bytes).and_then(|()| output.sync_all()) {
        drop(output);
        let _ignored = fs::remove_file(path);
        return Err(WorkerError::OutputUnavailable(error.to_string()));
    }
    Ok(())
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
            Self::InvalidDimensions => "invalid-dimensions",
            Self::PixelBudgetExceeded => "pixel-budget-exceeded",
            Self::InputUnavailable(_, _) => "input-unavailable",
            Self::InputTooLarge(_) => "input-too-large",
            Self::InputLengthMismatch => "input-length-mismatch",
            Self::HashMismatch(_) => "hash-mismatch",
            Self::InvalidIcc(_, _) => "invalid-icc",
            Self::UnsupportedColorSpace(_) => "unsupported-color-space",
            Self::TransformRejected => "transform-rejected",
            Self::OutputUnavailable(_) => "output-unavailable",
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::RequestTooLarge => "request exceeds the 64 KiB protocol limit".into(),
            Self::InvalidRequest(detail) | Self::OutputUnavailable(detail) => detail.clone(),
            Self::UnsupportedProtocol => "unsupported color-worker protocol version".into(),
            Self::InvalidDimensions => format!(
                "width and height must each be between 1 and {} pixels",
                raster_capacity::MAX_PRODUCTION_RASTER_DIMENSION
            ),
            Self::PixelBudgetExceeded => format!(
                "raster exceeds the {} pixel conversion limit",
                raster_capacity::MAX_PRODUCTION_RASTER_PIXELS
            ),
            Self::InputUnavailable(label, detail) | Self::InvalidIcc(label, detail) => {
                format!("{label}: {detail}")
            }
            Self::InputTooLarge(label) => format!("{label} exceeds its byte limit"),
            Self::InputLengthMismatch => "RGB byte length does not equal width * height * 3".into(),
            Self::HashMismatch(label) => format!("{label} bytes do not match the expected hash"),
            Self::UnsupportedColorSpace("source profile") => {
                "source profile must declare an RGB device space".into()
            }
            Self::UnsupportedColorSpace("output profile") => {
                "output profile must be a CMYK printer profile".into()
            }
            Self::UnsupportedColorSpace(label) => format!("unsupported color space: {label}"),
            Self::TransformRejected => {
                "LittleCMS could not construct the requested RGB8-to-CMYK8 transform".into()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ENGINE_IDENTITY, PROTOCOL_VERSION, RULES_IDENTITY, RenderingIntent, WorkerError,
        WorkerRequest, hex, process_request, sha256,
    };
    use lcms2::Profile;
    use std::{
        fs,
        path::{Path, PathBuf},
        time::SystemTime,
    };

    const CMYK_PROFILE: &[u8] =
        include_bytes!("../../../crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc");

    struct Fixture {
        directory: PathBuf,
        pixels: Vec<u8>,
        source_profile: Vec<u8>,
        output: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock is after epoch")
                .as_nanos();
            let directory = std::env::temp_dir().join(format!(
                "publisher-color-worker-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&directory).expect("create fixture directory");
            let pixels = vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
            let source_profile = Profile::new_srgb()
                .icc()
                .expect("serialize LittleCMS sRGB profile");
            fs::write(directory.join("pixels.rgb"), &pixels).expect("write RGB pixels");
            fs::write(directory.join("source.icc"), &source_profile).expect("write source profile");
            fs::write(directory.join("output.icc"), CMYK_PROFILE).expect("write output profile");
            let output = directory.join("converted.cmyk");
            Self {
                directory,
                pixels,
                source_profile,
                output,
            }
        }

        fn request(&self) -> WorkerRequest {
            WorkerRequest {
                protocol_version: PROTOCOL_VERSION,
                rgb_pixels_path: self.directory.join("pixels.rgb").display().to_string(),
                expected_rgb_pixels_sha256: hex(sha256(&self.pixels)),
                source_profile_path: self.directory.join("source.icc").display().to_string(),
                expected_source_profile_sha256: hex(sha256(&self.source_profile)),
                output_profile_path: self.directory.join("output.icc").display().to_string(),
                expected_output_profile_sha256: hex(sha256(CMYK_PROFILE)),
                converted_output_path: self.output.display().to_string(),
                width: 2,
                height: 2,
                rendering_intent: RenderingIntent::RelativeColorimetric,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ignored = fs::remove_dir_all(&self.directory);
        }
    }

    fn read(path: &Path) -> Vec<u8> {
        fs::read(path).expect("read fixture output")
    }

    #[test]
    fn exact_inputs_produce_deterministic_hash_bound_cmyk() {
        let first = Fixture::new();
        let first_report = process_request(&first.request()).expect("convert exact inputs");
        let first_bytes = read(&first.output);

        let second = Fixture::new();
        let second_report = process_request(&second.request()).expect("repeat conversion");
        let second_bytes = read(&second.output);

        assert_eq!(first_bytes.len(), 2 * 2 * 4);
        assert_eq!(first_bytes, second_bytes);
        assert_eq!(first_report.converted_sha256, hex(sha256(&first_bytes)));
        assert_eq!(first_report.converted_byte_length, first_bytes.len());
        assert_eq!(
            first_report.converted_sha256,
            second_report.converted_sha256
        );
        assert_eq!(first_report.raw_pixels_sha256, hex(sha256(&first.pixels)));
        assert_eq!(
            first_report.source_profile_sha256,
            hex(sha256(&first.source_profile))
        );
        assert_eq!(
            first_report.output_profile_sha256,
            hex(sha256(CMYK_PROFILE))
        );
        assert_eq!(first_report.width, 2);
        assert_eq!(first_report.height, 2);
        assert_eq!(first_report.engine_identity, ENGINE_IDENTITY);
        assert_eq!(first_report.rules_identity, RULES_IDENTITY);
    }

    #[test]
    fn mismatched_input_hash_creates_no_output() {
        let fixture = Fixture::new();
        let mut request = fixture.request();
        request.expected_rgb_pixels_sha256 = "0".repeat(64);

        let error = process_request(&request).expect_err("mismatched pixels must reject");

        assert!(matches!(error, WorkerError::HashMismatch("RGB pixels")));
        assert!(!fixture.output.exists());
    }

    #[test]
    fn existing_output_is_never_overwritten() {
        let fixture = Fixture::new();
        fs::write(&fixture.output, b"prior bytes").expect("write existing destination");

        let error = process_request(&fixture.request()).expect_err("existing output must reject");

        assert!(matches!(error, WorkerError::OutputUnavailable(_)));
        assert_eq!(read(&fixture.output), b"prior bytes");
    }

    #[test]
    fn dimensions_are_checked_before_opening_inputs() {
        let fixture = Fixture::new();
        let mut request = fixture.request();
        request.width = 10_501;
        request.height = 13_500;

        let error = process_request(&request).expect_err("pixel budget must reject");

        assert!(matches!(error, WorkerError::PixelBudgetExceeded));
        assert!(!fixture.output.exists());
    }

    #[test]
    fn request_rejects_unknown_fields() {
        let request = r#"{
            "protocol_version":1,
            "rgb_pixels_path":"pixels.rgb",
            "expected_rgb_pixels_sha256":"0000000000000000000000000000000000000000000000000000000000000000",
            "source_profile_path":"source.icc",
            "expected_source_profile_sha256":"0000000000000000000000000000000000000000000000000000000000000000",
            "output_profile_path":"output.icc",
            "expected_output_profile_sha256":"0000000000000000000000000000000000000000000000000000000000000000",
            "converted_output_path":"converted.cmyk",
            "width":1,
            "height":1,
            "rendering_intent":"relative-colorimetric",
            "untrusted_extension":true
        }"#;

        assert!(serde_json::from_str::<WorkerRequest>(request).is_err());
    }

    fn inspection_request(fixture: &Fixture) -> super::InspectionRequest {
        super::InspectionRequest {
            protocol_version: PROTOCOL_VERSION,
            operation: "inspect-output-profile".into(),
            output_profile_path: fixture.directory.join("output.icc").display().to_string(),
            expected_output_profile_sha256: hex(sha256(CMYK_PROFILE)),
            expected_byte_length: CMYK_PROFILE.len(),
        }
    }

    #[test]
    fn inspection_accepts_exact_bidirectional_printer_profile_without_conversion_output() {
        let fixture = Fixture::new();
        let report = super::inspect_profile(&inspection_request(&fixture)).expect("valid profile");
        assert_eq!(report.output_profile_sha256, hex(sha256(CMYK_PROFILE)));
        assert_eq!(report.byte_length, CMYK_PROFILE.len());
        assert_eq!(report.rules_identity, super::INSPECTION_RULES);
        assert!(!fixture.output.exists());
    }

    #[test]
    fn inspection_rejects_wrong_hash_and_length() {
        let fixture = Fixture::new();
        let mut request = inspection_request(&fixture);
        request.expected_output_profile_sha256 = "0".repeat(64);
        assert!(matches!(
            super::inspect_profile(&request),
            Err(WorkerError::HashMismatch(_))
        ));
        request.expected_output_profile_sha256 = hex(sha256(CMYK_PROFILE));
        request.expected_byte_length -= 1;
        assert!(matches!(
            super::inspect_profile(&request),
            Err(WorkerError::InvalidIcc(_, _))
        ));
    }

    #[test]
    fn inspection_rejects_bounded_but_unusable_transform_tags() {
        let fixture = Fixture::new();
        let mut bytes = CMYK_PROFILE.to_vec();
        let count = u32::from_be_bytes(bytes[128..132].try_into().unwrap()) as usize;
        for index in 0..count {
            let record = 132 + index * 12;
            if &bytes[record..record + 4] == b"A2B0" || &bytes[record..record + 4] == b"B2A0" {
                let offset =
                    u32::from_be_bytes(bytes[record + 4..record + 8].try_into().unwrap()) as usize;
                bytes[offset..offset + 4].copy_from_slice(b"BAD!");
            }
        }
        super::validate_icc_container(&bytes, "output profile").expect("container remains bounded");
        fs::write(fixture.directory.join("output.icc"), &bytes).unwrap();
        let mut request = inspection_request(&fixture);
        request.expected_output_profile_sha256 = hex(sha256(&bytes));
        assert!(matches!(
            super::inspect_profile(&request),
            Err(WorkerError::InvalidIcc(_, _))
        ));
    }

    #[test]
    fn inspection_requires_both_output_transform_tags() {
        let fixture = Fixture::new();
        let mut bytes = CMYK_PROFILE.to_vec();
        let count = u32::from_be_bytes(bytes[128..132].try_into().unwrap()) as usize;
        for index in 0..count {
            let record = 132 + index * 12;
            if &bytes[record..record + 4] == b"B2A0" {
                bytes[record..record + 4].copy_from_slice(b"xxxx");
            }
        }
        fs::write(fixture.directory.join("output.icc"), &bytes).unwrap();
        let mut request = inspection_request(&fixture);
        request.expected_output_profile_sha256 = hex(sha256(&bytes));
        assert!(matches!(
            super::inspect_profile(&request),
            Err(WorkerError::InvalidIcc(_, _))
        ));
    }

    #[test]
    fn inspection_rejects_tolerated_raw_header_errors_before_attestation() {
        let fixture = Fixture::new();
        for (offset, value) in [(127, 1), (27, 13)] {
            let mut bytes = CMYK_PROFILE.to_vec();
            bytes[offset] = value;
            assert!(lcms2::Profile::new_icc(&bytes).is_ok());
            fs::write(fixture.directory.join("output.icc"), &bytes).unwrap();
            let mut request = inspection_request(&fixture);
            request.expected_output_profile_sha256 = hex(sha256(&bytes));
            assert!(matches!(
                super::inspect_profile(&request),
                Err(WorkerError::InvalidIcc(_, _))
            ));
        }
    }

    fn extended_mluc_record(bytes: &[u8]) -> Vec<u8> {
        let mut extended = bytes.to_vec();
        let offset = u32::from_be_bytes(bytes[136..140].try_into().unwrap()) as usize;
        assert_eq!(&bytes[offset..offset + 4], b"mluc");
        assert_eq!(&bytes[offset + 8..offset + 12], &1_u32.to_be_bytes());
        let insert = offset + 28;
        extended.splice(insert..insert, *b"extn");
        extended[offset + 12..offset + 16].copy_from_slice(&16_u32.to_be_bytes());
        let text_offset = u32::from_be_bytes(bytes[offset + 24..offset + 28].try_into().unwrap());
        extended[offset + 24..offset + 28].copy_from_slice(&(text_offset + 4).to_be_bytes());
        let count = u32::from_be_bytes(bytes[128..132].try_into().unwrap()) as usize;
        for index in 0..count {
            let at = 132 + 12 * index;
            let start = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap());
            let size = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap());
            if start as usize == offset {
                extended[at + 8..at + 12].copy_from_slice(&(size + 4).to_be_bytes());
            } else if start as usize >= insert {
                extended[at + 4..at + 8].copy_from_slice(&(start + 4).to_be_bytes());
            }
        }
        let length = u32::try_from(extended.len()).unwrap();
        extended[..4].copy_from_slice(&length.to_be_bytes());
        extended[84..100].fill(0);
        extended
    }

    #[test]
    fn genuine_serialized_editions_pass_exact_checks_and_every_intent_probe() {
        use lcms2::{ColorSpaceSignature, Profile, ProfileClassSignature, TagSignature};
        let fixture = Fixture::new();
        let source = Profile::new_icc(CMYK_PROFILE).unwrap();
        for (name, edition) in [("v22", 2.2), ("v40", 4.0), ("v41", 4.1), ("v42", 4.2)] {
            // Construct and serialize through typed native tags. Version changes
            // determine the serialized tag representations, never a byte patch.
            let mut profile = Profile::new_placeholder();
            profile.set_version(edition);
            profile.set_device_class(ProfileClassSignature::OutputClass);
            profile.set_color_space(ColorSpaceSignature::CmykData);
            profile.set_pcs(ColorSpaceSignature::LabData);
            for tag in [
                TagSignature::ProfileDescriptionTag,
                TagSignature::CopyrightTag,
                TagSignature::MediaWhitePointTag,
                TagSignature::GamutTag,
                TagSignature::AToB0Tag,
                TagSignature::AToB1Tag,
                TagSignature::AToB2Tag,
                TagSignature::BToA0Tag,
                TagSignature::BToA1Tag,
                TagSignature::BToA2Tag,
            ] {
                assert!(profile.write_tag(tag, source.read_tag(tag)));
            }
            let serialized = profile.icc().unwrap();
            let bytes = if name == "v22" && super::validate_output_profile(&serialized).is_err() {
                // This engine's legacy description serializer includes trailing
                // padding in the tag size. Keep the original exact valid v2
                // control rather than declaring that reserialization valid.
                if let Some(directory) = std::env::var_os("PDF_LIBRARY_ICC_ARTIFACTS") {
                    fs::write(
                        std::path::Path::new(&directory)
                            .join("native-v22-invalid-description-padding.icc"),
                        &serialized,
                    )
                    .unwrap();
                }
                CMYK_PROFILE.to_vec()
            } else {
                serialized
            };
            if let Some(directory) = std::env::var_os("PDF_LIBRARY_ICC_ARTIFACTS") {
                fs::write(
                    std::path::Path::new(&directory).join(format!("native-{name}.icc")),
                    &bytes,
                )
                .unwrap();
            }
            super::validate_output_profile(&bytes).unwrap();
            fs::write(fixture.directory.join("output.icc"), &bytes).unwrap();
            let mut request = inspection_request(&fixture);
            request.expected_output_profile_sha256 = hex(sha256(&bytes));
            request.expected_byte_length = bytes.len();
            let report = super::inspect_profile(&request).unwrap();
            assert_eq!(report.output_profile_sha256, hex(sha256(&bytes)));
            assert_eq!(
                fs::read(fixture.directory.join("output.icc")).unwrap(),
                bytes
            );
            if name == "v42" {
                let extended = extended_mluc_record(&bytes);
                super::validate_output_profile(&extended).unwrap();
                fs::write(fixture.directory.join("output.icc"), &extended).unwrap();
                request.expected_byte_length = extended.len();
                request.expected_output_profile_sha256 = hex(sha256(&extended));
                assert!(super::inspect_profile(&request).is_ok());
                if let Some(directory) = std::env::var_os("PDF_LIBRARY_ICC_ARTIFACTS") {
                    fs::write(
                        std::path::Path::new(&directory).join("native-v42-mluc-stride16.icc"),
                        &extended,
                    )
                    .unwrap();
                }
            }
        }
    }
}
