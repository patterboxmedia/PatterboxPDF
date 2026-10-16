//! Trust boundary for hostile raster decoding in the packaged sidecar.

use document_core::raster_capacity::{
    MAX_PRODUCTION_RASTER_PIXELS, RasterMemoryPermit, RasterWorkPermit, production_raster_pixels,
};
use document_core::{
    AssetHash, MAX_ICC_PROFILE_BYTES, MAX_RASTER_FILE_BYTES, RasterColorProfile, RasterInspection,
    UntaggedRasterPolicy,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    process::ExitStatus,
    thread,
    time::{Duration, Instant, SystemTime},
};

pub const RASTER_WORKER_PROTOCOL_VERSION: u16 = 2;
const PROTOCOL_VERSION: u16 = RASTER_WORKER_PROTOCOL_VERSION;
const MAX_WORKER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REPORT_BYTES: u64 = 64 * 1024;
const DECODE_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(windows)]
const WORKER_MEMORY_LIMIT: usize = document_core::raster_capacity::MAX_RASTER_DECODE_WORKER_BYTES;
pub const RASTER_DECODE_RULES_IDENTITY: &str = "publisher-v2/raster-decode/v3/oriented-rgb8/straight-alpha8/bounded-tiff-icc/embedded-or-explicit-srgb";
pub const CLIPBOARD_DIB_NORMALIZATION_RULES_IDENTITY: &str =
    "publisher-v2/clipboard-dib/v1/info40-v4-v5/rgb24-bgra32/bounded-rgb-icc/straight-alpha/png";
const MAX_CLIPBOARD_PNG_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RasterSourceProfileOrigin {
    Embedded,
    AssumedSrgb,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkerBinding {
    path: PathBuf,
    executable_hash: Hash32,
}

/// Resolves and pins the packaged raster worker before hostile bytes are decoded.
pub struct TrustedRasterBoundary {
    binding: Result<WorkerBinding, RasterWorkerError>,
}

impl TrustedRasterBoundary {
    #[must_use]
    pub fn resolve_packaged() -> Self {
        Self {
            binding: resolve_worker(),
        }
    }

    /// Converts native clipboard DIB bytes to a hash-bound PNG inside the confined worker.
    /// Host and renderer code never parse bitmap headers or decode pixels.
    ///
    /// # Errors
    /// Refuses unsupported bitmaps, resource limits, stale worker identity, and receipt drift.
    pub fn normalize_clipboard_dib(
        &self,
        encoded: &[u8],
    ) -> Result<TrustedClipboardPng, RasterWorkerError> {
        if encoded.is_empty() || encoded.len() > MAX_CLIPBOARD_PNG_BYTES {
            return Err(RasterWorkerError::InvalidInput);
        }
        let source = AssetHash::for_bytes(encoded);
        let _work = RasterWorkPermit::acquire().map_err(|_| RasterWorkerError::ResourceLimit)?;
        let binding = self.binding.as_ref().map_err(Clone::clone)?;
        let worker = PreparedWorker::new(binding)?;
        let files = DecodeFiles::new(encoded)?;
        let report: ClipboardDibReport = run_worker_report(
            &worker,
            &files,
            &ClipboardDibRequest {
                protocol_version: PROTOCOL_VERSION,
                candidate_path: files.encoded.display().to_string(),
                expected_asset_sha256: source.as_str().to_owned(),
                untagged_raster_policy: UntaggedRasterPolicy::RequireEmbeddedProfile,
                clipboard_dib_output_path: files.png.display().to_string(),
            },
        )?;
        let evidence = report.normalized_png;
        let rules = Hash32::of(CLIPBOARD_DIB_NORMALIZATION_RULES_IDENTITY.as_bytes());
        if report.protocol_version != PROTOCOL_VERSION
            || report.asset_sha256 != source.as_str()
            || report.inspection.media_type != "image/png"
            || report.inspection.exif_orientation != 1
            || report.inspection.width_pixels != report.inspection.oriented_width_pixels
            || report.inspection.height_pixels != report.inspection.oriented_height_pixels
            || evidence.normalization_rules_identity != CLIPBOARD_DIB_NORMALIZATION_RULES_IDENTITY
            || evidence.normalization_rules_sha256 != rules.hex()
            || evidence.png_byte_length == 0
            || evidence.png_byte_length > MAX_CLIPBOARD_PNG_BYTES
        {
            return Err(RasterWorkerError::IdentityDrift);
        }
        checked_pixels(
            report.inspection.oriented_width_pixels,
            report.inspection.oriented_height_pixels,
        )?;
        let png = read_exact(&files.png, evidence.png_byte_length)?;
        let asset = AssetHash::for_bytes(&png);
        if asset.as_str() != evidence.png_sha256 {
            return Err(RasterWorkerError::IdentityDrift);
        }
        Ok(TrustedClipboardPng {
            bytes: png,
            asset,
            inspection: report.inspection,
            source_dib_hash: source,
            worker_executable_hash: worker.executable_hash.0,
        })
    }

    /// Decodes exact content-addressed bytes in the confined worker.
    ///
    /// # Errors
    ///
    /// Fails closed for unapproved missing ICC profiles, unsupported formats,
    /// resource exhaustion, executable drift, malformed reports, or any hash
    /// mismatch.
    pub fn decode(
        &self,
        encoded: &[u8],
        expected_asset: &AssetHash,
        untagged_raster_policy: UntaggedRasterPolicy,
    ) -> Result<TrustedDecodedRaster, RasterWorkerError> {
        if encoded.is_empty()
            || encoded.len() > MAX_RASTER_FILE_BYTES
            || &AssetHash::for_bytes(encoded) != expected_asset
        {
            return Err(RasterWorkerError::InvalidInput);
        }
        let binding = self.binding.as_ref().map_err(Clone::clone)?;
        let _work = RasterWorkPermit::acquire().map_err(|_| RasterWorkerError::ResourceLimit)?;
        let mut memory =
            RasterMemoryPermit::reserve(MAX_PRODUCTION_RASTER_PIXELS * 4 + MAX_ICC_PROFILE_BYTES)
                .map_err(|_| RasterWorkerError::ResourceLimit)?;
        let worker = PreparedWorker::new(binding)?;
        let files = DecodeFiles::new(encoded)?;
        let report = run_worker(
            &worker,
            &files,
            &WireRequest {
                protocol_version: PROTOCOL_VERSION,
                candidate_path: files.encoded.display().to_string(),
                expected_asset_sha256: expected_asset.as_str().to_owned(),
                untagged_raster_policy,
                rgb_output_path: files.rgb.display().to_string(),
                alpha_output_path: files.alpha.display().to_string(),
                source_profile_output_path: files.profile.display().to_string(),
            },
        )?;
        let decoded = report.decoded.ok_or(RasterWorkerError::IdentityDrift)?;
        let expected_rules = Hash32::of(RASTER_DECODE_RULES_IDENTITY.as_bytes());
        if report.protocol_version != PROTOCOL_VERSION
            || report.asset_sha256 != expected_asset.as_str()
            || decoded.decode_rules_identity != RASTER_DECODE_RULES_IDENTITY
            || decoded.decode_rules_sha256 != expected_rules.hex()
            || decoded.oriented_width != report.inspection.oriented_width_pixels
            || decoded.oriented_height != report.inspection.oriented_height_pixels
            || decoded.exif_orientation_applied != report.inspection.exif_orientation
        {
            return Err(RasterWorkerError::IdentityDrift);
        }
        let pixels = checked_pixels(decoded.oriented_width, decoded.oriented_height)?;
        let rgb = read_exact(&files.rgb, pixels.saturating_mul(3))?;
        let alpha = read_exact(&files.alpha, pixels)?;
        let profile = read_bounded(&files.profile, MAX_ICC_PROFILE_BYTES as u64)?;
        let rgb_hash = Hash32::of(&rgb);
        let alpha_hash = Hash32::of(&alpha);
        let profile_hash = Hash32::of(&profile);
        let profile_matches_inspection = match (
            decoded.source_profile_origin,
            &report.inspection.color_profile,
            untagged_raster_policy,
        ) {
            (
                RasterSourceProfileOrigin::Embedded,
                RasterColorProfile::Embedded {
                    sha256,
                    byte_length,
                },
                _,
            ) => {
                sha256.as_str() == decoded.source_profile_sha256
                    && *byte_length == decoded.source_profile_byte_length as u64
            }
            (
                RasterSourceProfileOrigin::AssumedSrgb,
                RasterColorProfile::Unspecified,
                UntaggedRasterPolicy::AssumeSrgb,
            ) => true,
            _ => false,
        };
        if decoded.rgb_byte_length != rgb.len()
            || decoded.alpha_byte_length != alpha.len()
            || decoded.source_profile_byte_length != profile.len()
            || decoded.rgb_sha256 != rgb_hash.hex()
            || decoded.alpha_sha256 != alpha_hash.hex()
            || decoded.source_profile_sha256 != profile_hash.hex()
            || decoded.has_transparency != alpha.iter().any(|value| *value != u8::MAX)
            || !profile_matches_inspection
        {
            return Err(RasterWorkerError::IdentityDrift);
        }
        memory
            .shrink_to(rgb.len() + alpha.len() + profile.len())
            .map_err(|_| RasterWorkerError::ResourceLimit)?;
        Ok(TrustedDecodedRaster {
            _memory: memory,
            asset: expected_asset.clone(),
            inspection: report.inspection,
            rgb,
            alpha,
            source_profile: profile,
            rgb_hash,
            alpha_hash,
            profile_hash,
            rules_hash: expected_rules,
            worker_executable_hash: worker.executable_hash,
            has_transparency: decoded.has_transparency,
            source_profile_origin: decoded.source_profile_origin,
        })
    }
}

/// Original-source and output identities attested by the pinned confined worker.
pub struct TrustedClipboardPng {
    bytes: Vec<u8>,
    asset: AssetHash,
    inspection: RasterInspection,
    source_dib_hash: AssetHash,
    worker_executable_hash: [u8; 32],
}
impl TrustedClipboardPng {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    #[must_use]
    pub const fn asset(&self) -> &AssetHash {
        &self.asset
    }
    #[must_use]
    pub const fn inspection(&self) -> &RasterInspection {
        &self.inspection
    }
    #[must_use]
    pub const fn source_dib_hash(&self) -> &AssetHash {
        &self.source_dib_hash
    }
    #[must_use]
    pub const fn worker_executable_hash(&self) -> [u8; 32] {
        self.worker_executable_hash
    }
    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, AssetHash, RasterInspection) {
        (self.bytes, self.asset, self.inspection)
    }
}

/// Opaque evidence and bounded decoded resources produced by the packaged worker.
pub struct TrustedDecodedRaster {
    _memory: RasterMemoryPermit,
    asset: AssetHash,
    inspection: RasterInspection,
    rgb: Vec<u8>,
    alpha: Vec<u8>,
    source_profile: Vec<u8>,
    rgb_hash: Hash32,
    alpha_hash: Hash32,
    profile_hash: Hash32,
    rules_hash: Hash32,
    worker_executable_hash: Hash32,
    has_transparency: bool,
    source_profile_origin: RasterSourceProfileOrigin,
}

impl TrustedDecodedRaster {
    #[must_use]
    pub const fn asset(&self) -> &AssetHash {
        &self.asset
    }
    #[must_use]
    pub const fn inspection(&self) -> &RasterInspection {
        &self.inspection
    }
    #[must_use]
    pub fn rgb_pixels(&self) -> &[u8] {
        &self.rgb
    }
    #[must_use]
    pub fn alpha_pixels(&self) -> &[u8] {
        &self.alpha
    }
    #[must_use]
    pub fn source_profile(&self) -> &[u8] {
        &self.source_profile
    }
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.inspection.oriented_width_pixels
    }
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.inspection.oriented_height_pixels
    }
    #[must_use]
    pub const fn has_transparency(&self) -> bool {
        self.has_transparency
    }
    #[must_use]
    pub const fn source_profile_origin(&self) -> RasterSourceProfileOrigin {
        self.source_profile_origin
    }
    #[must_use]
    pub const fn rgb_hash(&self) -> [u8; 32] {
        self.rgb_hash.0
    }
    #[must_use]
    pub const fn alpha_hash(&self) -> [u8; 32] {
        self.alpha_hash.0
    }
    #[must_use]
    pub const fn profile_hash(&self) -> [u8; 32] {
        self.profile_hash.0
    }
    #[must_use]
    pub const fn rules_hash(&self) -> [u8; 32] {
        self.rules_hash.0
    }
    #[must_use]
    pub const fn worker_executable_hash(&self) -> [u8; 32] {
        self.worker_executable_hash.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RasterWorkerError {
    NotInstalled,
    InvalidInput,
    ResourceLimit,
    ExecutableIdentityDrift,
    Io(String),
    SpawnFailed(String),
    ConfinementFailed(String),
    TimedOut,
    WorkerRejected(String),
    MalformedReport(String),
    IdentityDrift,
}

#[derive(Serialize)]
struct WireRequest {
    protocol_version: u16,
    candidate_path: String,
    expected_asset_sha256: String,
    untagged_raster_policy: UntaggedRasterPolicy,
    rgb_output_path: String,
    alpha_output_path: String,
    source_profile_output_path: String,
}

#[derive(Serialize)]
struct ClipboardDibRequest {
    protocol_version: u16,
    candidate_path: String,
    expected_asset_sha256: String,
    untagged_raster_policy: UntaggedRasterPolicy,
    clipboard_dib_output_path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipboardDibReport {
    protocol_version: u16,
    asset_sha256: String,
    inspection: RasterInspection,
    normalized_png: NormalizedPngReport,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalizedPngReport {
    png_sha256: String,
    png_byte_length: usize,
    normalization_rules_identity: String,
    normalization_rules_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReport {
    protocol_version: u16,
    asset_sha256: String,
    inspection: RasterInspection,
    decoded: Option<DecodedReport>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecodedReport {
    oriented_width: u32,
    oriented_height: u32,
    exif_orientation_applied: u8,
    rgb_sha256: String,
    rgb_byte_length: usize,
    alpha_sha256: String,
    alpha_byte_length: usize,
    has_transparency: bool,
    source_profile_origin: RasterSourceProfileOrigin,
    source_profile_sha256: String,
    source_profile_byte_length: usize,
    decode_rules_identity: String,
    decode_rules_sha256: String,
}

#[derive(Deserialize)]
struct WireFailure {
    protocol_version: u16,
    error: String,
    detail: String,
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

fn checked_pixels(width: u32, height: u32) -> Result<usize, RasterWorkerError> {
    production_raster_pixels(width, height).map_err(|_| RasterWorkerError::ResourceLimit)
}

fn resolve_worker() -> Result<WorkerBinding, RasterWorkerError> {
    let executable = std::env::current_exe().map_err(io_error)?;
    resolve_worker_from(&executable)
}

fn resolve_worker_from(executable: &Path) -> Result<WorkerBinding, RasterWorkerError> {
    for path in worker_candidates(executable) {
        if path.is_file() {
            let bytes = read_bounded(&path, MAX_WORKER_BYTES)?;
            return Ok(WorkerBinding {
                path,
                executable_hash: Hash32::of(&bytes),
            });
        }
    }
    Err(RasterWorkerError::NotInstalled)
}

fn worker_candidates(executable: &Path) -> Vec<PathBuf> {
    let name = if cfg!(windows) {
        "publisher-raster-worker.exe"
    } else {
        "publisher-raster-worker"
    };
    let mut candidates = vec![executable.with_file_name(name)];
    if cfg!(debug_assertions) {
        candidates.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug")
                .join(name),
        );
    }
    candidates
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
        Err(RasterWorkerError::NotInstalled)
    ));
    write_exclusive(&candidates[0], b"pinned fixture bytes").unwrap();
    let binding = resolve_worker_from(&host).unwrap();
    assert_eq!(binding.path, candidates[0]);
    assert_eq!(binding.executable_hash, Hash32::of(b"pinned fixture bytes"));
    std::fs::remove_file(&candidates[0]).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

struct PreparedWorker {
    path: PathBuf,
    directory: PathBuf,
    executable_hash: Hash32,
    lock: Option<File>,
}

impl PreparedWorker {
    fn new(binding: &WorkerBinding) -> Result<Self, RasterWorkerError> {
        let bytes = read_bounded(&binding.path, MAX_WORKER_BYTES)?;
        if Hash32::of(&bytes) != binding.executable_hash {
            return Err(RasterWorkerError::ExecutableIdentityDrift);
        }
        let directory = private_directory("publisher-raster-executable")?;
        let path = directory.join(if cfg!(windows) {
            "publisher-raster-worker.exe"
        } else {
            "publisher-raster-worker"
        });
        write_exclusive(&path, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500))
                .map_err(io_error)?;
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let mut lock = options.open(&path).map_err(io_error)?;
        lock.rewind().map_err(io_error)?;
        let mut exact = Vec::new();
        Read::by_ref(&mut lock)
            .take(MAX_WORKER_BYTES + 1)
            .read_to_end(&mut exact)
            .map_err(io_error)?;
        if Hash32::of(&exact) != binding.executable_hash {
            return Err(RasterWorkerError::ExecutableIdentityDrift);
        }
        Ok(Self {
            path,
            directory,
            executable_hash: binding.executable_hash,
            lock: Some(lock),
        })
    }
}

impl Drop for PreparedWorker {
    fn drop(&mut self) {
        drop(self.lock.take());
        let _ignored = std::fs::remove_file(&self.path);
        let _ignored = std::fs::remove_dir(&self.directory);
    }
}

struct DecodeFiles {
    directory: PathBuf,
    encoded: PathBuf,
    rgb: PathBuf,
    alpha: PathBuf,
    profile: PathBuf,
    png: PathBuf,
}

impl DecodeFiles {
    fn new(encoded: &[u8]) -> Result<Self, RasterWorkerError> {
        let directory = private_directory("publisher-raster-decode")?;
        let source = directory.join("source.raster");
        write_exclusive(&source, encoded)?;
        Ok(Self {
            rgb: directory.join("decoded.rgb"),
            alpha: directory.join("decoded.alpha"),
            profile: directory.join("source.icc"),
            png: directory.join("clipboard.png"),
            encoded: source,
            directory,
        })
    }
}

impl Drop for DecodeFiles {
    fn drop(&mut self) {
        for path in [
            &self.encoded,
            &self.rgb,
            &self.alpha,
            &self.profile,
            &self.png,
        ] {
            let _ignored = std::fs::remove_file(path);
        }
        let _ignored = std::fs::remove_dir(&self.directory);
    }
}

fn run_worker(
    worker: &PreparedWorker,
    files: &DecodeFiles,
    request: &WireRequest,
) -> Result<WireReport, RasterWorkerError> {
    run_worker_report(worker, files, request)
}

fn run_worker_report<Request: Serialize, Report: serde::de::DeserializeOwned>(
    worker: &PreparedWorker,
    files: &DecodeFiles,
    request: &Request,
) -> Result<Report, RasterWorkerError> {
    let input = serde_json::to_vec(request)
        .map_err(|error| RasterWorkerError::MalformedReport(error.to_string()))?;
    let mut child = RunningProcess::spawn(worker, files, DECODE_TIMEOUT)
        .map_err(RasterWorkerError::ConfinementFailed)?;
    child
        .take_stdin()
        .ok_or_else(|| RasterWorkerError::SpawnFailed("raster worker stdin unavailable".into()))?
        .write_all(&input)
        .map_err(io_error)?;
    let deadline = Instant::now() + DECODE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                child.terminate();
                return Err(RasterWorkerError::TimedOut);
            }
            Err(error) => {
                child.terminate();
                return Err(RasterWorkerError::Io(error.to_string()));
            }
        }
    };
    let mut output = Vec::new();
    child
        .take_stdout()
        .ok_or_else(|| RasterWorkerError::MalformedReport("stdout unavailable".into()))?
        .take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut output)
        .map_err(io_error)?;
    if output.len() as u64 > MAX_REPORT_BYTES {
        return Err(RasterWorkerError::ResourceLimit);
    }
    if !status.success() {
        let failure: WireFailure = serde_json::from_slice(&output)
            .map_err(|error| RasterWorkerError::MalformedReport(error.to_string()))?;
        if failure.protocol_version != PROTOCOL_VERSION {
            return Err(RasterWorkerError::IdentityDrift);
        }
        return Err(RasterWorkerError::WorkerRejected(format!(
            "{}: {}",
            failure.error, failure.detail
        )));
    }
    serde_json::from_slice(&output)
        .map_err(|error| RasterWorkerError::MalformedReport(error.to_string()))
}

fn private_directory(label: &str) -> Result<PathBuf, RasterWorkerError> {
    for attempt in 0..16_u32 {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| RasterWorkerError::Io(error.to_string()))?
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
    Err(RasterWorkerError::Io(
        "could not allocate private raster-worker directory".into(),
    ))
}

fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<(), RasterWorkerError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_error)
}

fn read_exact(path: &Path, expected: usize) -> Result<Vec<u8>, RasterWorkerError> {
    let bytes = read_bounded(path, expected as u64)?;
    if bytes.len() != expected {
        return Err(RasterWorkerError::IdentityDrift);
    }
    Ok(bytes)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, RasterWorkerError> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err(RasterWorkerError::ResourceLimit);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(
            usize::try_from(metadata.len()).map_err(|_| RasterWorkerError::ResourceLimit)?,
        )
        .map_err(|_| RasterWorkerError::ResourceLimit)?;
    File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(RasterWorkerError::ResourceLimit);
    }
    Ok(bytes)
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(error: std::io::Error) -> RasterWorkerError {
    RasterWorkerError::Io(error.to_string())
}

struct RunningProcess {
    #[cfg(windows)]
    inner: publisher_worker_job::SandboxedProcess,
    #[cfg(not(windows))]
    inner: std::process::Child,
}

impl RunningProcess {
    fn spawn(
        worker: &PreparedWorker,
        files: &DecodeFiles,
        timeout: Duration,
    ) -> Result<Self, String> {
        #[cfg(windows)]
        {
            let profile =
                publisher_worker_job::AppContainerProfile::create_unique("PublisherV2.Raster")
                    .map_err(|error| error.to_string())?;
            profile
                .grant_read_execute(&worker.directory)
                .map_err(|error| error.to_string())?;
            // Grants come from host-owned run files, never from request paths.
            profile
                .grant_read(&files.encoded)
                .map_err(|error| error.to_string())?;
            profile
                .grant_private_working_directory(&files.directory)
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
        #[cfg(not(windows))]
        {
            let _ = (worker, files, timeout);
            Err("worker confinement is supported only on Windows".into())
        }
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        #[cfg(windows)]
        {
            self.inner
                .take_stdin()
                .map(|pipe| Box::new(pipe) as Box<dyn Write + Send>)
        }
        #[cfg(not(windows))]
        {
            self.inner
                .stdin
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn Write + Send>)
        }
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        #[cfg(windows)]
        {
            self.inner
                .take_stdout()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>)
        }
        #[cfg(not(windows))]
        {
            self.inner
                .stdout
                .take()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>)
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.inner.try_wait()
    }

    fn terminate(&mut self) {
        #[cfg(windows)]
        {
            let _ignored = self.inner.terminate();
        }
        #[cfg(not(windows))]
        {
            let _ignored = self.inner.kill();
            let _ignored = self.inner.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RasterWorkerError, TrustedRasterBoundary, checked_pixels};
    use document_core::{AssetHash, UntaggedRasterPolicy};

    #[test]
    fn host_rejects_excessive_dimensions_and_asset_drift_before_decode() {
        assert_eq!(
            checked_pixels(13_501, 1),
            Err(RasterWorkerError::ResourceLimit)
        );
        assert_eq!(
            checked_pixels(13_500, 10_501),
            Err(RasterWorkerError::ResourceLimit)
        );
        let boundary = TrustedRasterBoundary::resolve_packaged();
        assert!(matches!(
            boundary.normalize_clipboard_dib(&[]),
            Err(RasterWorkerError::InvalidInput)
        ));
        assert!(matches!(
            boundary.decode(
                b"not the expected bytes",
                &AssetHash::for_bytes(b"different"),
                UntaggedRasterPolicy::RequireEmbeddedProfile,
            ),
            Err(RasterWorkerError::InvalidInput)
        ));
    }
    // One authored RGB red pixel, with no embedded profile.
    #[cfg(windows)]
    const RED_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0,
        3, 1, 1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    #[cfg(windows)]
    fn authored_clipboard_dib() -> Vec<u8> {
        let mut bytes = vec![0; 124];
        bytes[..4].copy_from_slice(&124_u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&1_i32.to_le_bytes());
        bytes[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
        bytes[12..14].copy_from_slice(&1_u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&32_u16.to_le_bytes());
        bytes[16..20].copy_from_slice(&3_u32.to_le_bytes());
        for (offset, mask) in [
            (40, 0x00ff_0000_u32),
            (44, 0x0000_ff00),
            (48, 0x0000_00ff),
            (52, 0xff00_0000),
            (56, 0x7352_4742),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&mask.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0, 255, 128]);
        bytes
    }

    #[cfg(windows)]
    #[test]
    fn confined_clipboard_normalization_binds_source_png_pixels_and_worker() {
        let dib = authored_clipboard_dib();
        let boundary = TrustedRasterBoundary::resolve_packaged();
        let png = boundary
            .normalize_clipboard_dib(&dib)
            .expect("confined DIB normalization");
        assert_eq!(png.source_dib_hash(), &AssetHash::for_bytes(&dib));
        assert_eq!(png.asset(), &AssetHash::for_bytes(png.bytes()));
        assert_eq!(png.inspection().media_type, "image/png");
        assert_eq!(png.inspection().exif_orientation, 1);
        assert_eq!(
            (
                png.inspection().width_pixels,
                png.inspection().height_pixels
            ),
            (1, 1)
        );
        assert_eq!(
            png.inspection(),
            &document_core::inspect_raster(png.bytes()).unwrap()
        );
        let decoded = boundary
            .decode(
                png.bytes(),
                png.asset(),
                UntaggedRasterPolicy::RequireEmbeddedProfile,
            )
            .expect("normalized PNG uses ordinary verified decode");
        assert_eq!(decoded.rgb_pixels(), [255, 0, 0]);
        assert_eq!(decoded.alpha_pixels(), [128]);
        assert_eq!(
            png.worker_executable_hash(),
            decoded.worker_executable_hash()
        );
        let (bytes, asset, inspection) = png.into_parts();
        assert_eq!(asset, AssetHash::for_bytes(&bytes));
        assert_eq!(inspection.media_type, "image/png");
        assert!(matches!(
            boundary.normalize_clipboard_dib(b"malformed DIB"),
            Err(RasterWorkerError::WorkerRejected(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn clipboard_normalization_cannot_read_or_write_outside_private_run() {
        use super::{
            ClipboardDibReport, ClipboardDibRequest, DecodeFiles, PROTOCOL_VERSION, PreparedWorker,
            resolve_worker, run_worker_report,
        };
        let dib = authored_clipboard_dib();
        let worker = PreparedWorker::new(&resolve_worker().expect("packaged worker"))
            .expect("private worker");
        let files = DecodeFiles::new(&dib).unwrap();
        let outside = DecodeFiles::new(&dib).unwrap();
        let mut request = ClipboardDibRequest {
            protocol_version: PROTOCOL_VERSION,
            candidate_path: outside.encoded.display().to_string(),
            expected_asset_sha256: AssetHash::for_bytes(&dib).as_str().to_owned(),
            untagged_raster_policy: UntaggedRasterPolicy::RequireEmbeddedProfile,
            clipboard_dib_output_path: files.png.display().to_string(),
        };
        assert!(
            matches!(run_worker_report::<_, ClipboardDibReport>(&worker, &files, &request), Err(RasterWorkerError::WorkerRejected(detail)) if detail.starts_with("candidate-unavailable:"))
        );
        request.candidate_path = files.encoded.display().to_string();
        request.clipboard_dib_output_path = outside.png.display().to_string();
        assert!(
            matches!(run_worker_report::<_, ClipboardDibReport>(&worker, &files, &request), Err(RasterWorkerError::WorkerRejected(detail)) if detail.starts_with("output-unavailable:"))
        );
        assert!(!files.png.exists());
        assert!(!outside.png.exists());
        assert_eq!(std::fs::read(&outside.encoded).unwrap(), dib);
    }

    #[cfg(windows)]
    #[test]
    fn packaged_raster_worker_decodes_exact_authored_pixel() {
        let result = TrustedRasterBoundary::resolve_packaged()
            .decode(
                RED_PNG,
                &AssetHash::for_bytes(RED_PNG),
                UntaggedRasterPolicy::AssumeSrgb,
            )
            .expect("valid confined decode");
        assert_eq!(result.rgb_pixels(), [255, 0, 0]);
        assert_eq!(result.alpha_pixels(), [255]);
    }

    #[cfg(windows)]
    #[test]
    fn packaged_raster_worker_cannot_read_or_write_outside_its_private_run() {
        use super::{
            DecodeFiles, PROTOCOL_VERSION, PreparedWorker, WireRequest, resolve_worker, run_worker,
        };
        let worker = PreparedWorker::new(&resolve_worker().expect("packaged worker"))
            .expect("private worker");
        let files = DecodeFiles::new(RED_PNG).expect("private input");
        let outside = DecodeFiles::new(RED_PNG).expect("ungranted sentinel directory");
        let mut request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            candidate_path: outside.encoded.display().to_string(),
            expected_asset_sha256: AssetHash::for_bytes(RED_PNG).as_str().to_owned(),
            untagged_raster_policy: UntaggedRasterPolicy::AssumeSrgb,
            rgb_output_path: files.rgb.display().to_string(),
            alpha_output_path: files.alpha.display().to_string(),
            source_profile_output_path: files.profile.display().to_string(),
        };
        assert!(
            matches!(run_worker(&worker, &files, &request), Err(RasterWorkerError::WorkerRejected(detail)) if detail.starts_with("candidate-unavailable:"))
        );
        request.candidate_path = files.encoded.display().to_string();
        request.rgb_output_path = outside.rgb.display().to_string();
        assert!(
            matches!(run_worker(&worker, &files, &request), Err(RasterWorkerError::WorkerRejected(detail)) if detail.starts_with("output-unavailable:"))
        );
        assert!(!outside.rgb.exists());
        assert_eq!(
            std::fs::read(&outside.encoded).expect("sentinel remains"),
            RED_PNG
        );
    }

    #[cfg(windows)]
    #[test]
    fn packaged_raster_worker_rejects_exact_malformed_raster_without_outputs_or_paths() {
        use super::{
            DecodeFiles, Hash32, PROTOCOL_VERSION, PreparedWorker, WireRequest, resolve_worker,
            run_worker,
        };

        const MALFORMED_RASTER: &[u8] = b"not a raster";
        const MALFORMED_RASTER_SHA256: &str =
            "34a5b642009c01c39c2422d9ab3ec7f4aab64b687496cbf694825abe04f828a2";

        assert_eq!(Hash32::of(MALFORMED_RASTER).hex(), MALFORMED_RASTER_SHA256);
        let worker = PreparedWorker::new(&resolve_worker().expect("packaged worker"))
            .expect("private worker");
        let files = DecodeFiles::new(MALFORMED_RASTER).expect("stage exact malformed raster");
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            candidate_path: files.encoded.display().to_string(),
            expected_asset_sha256: MALFORMED_RASTER_SHA256.to_owned(),
            untagged_raster_policy: UntaggedRasterPolicy::AssumeSrgb,
            rgb_output_path: files.rgb.display().to_string(),
            alpha_output_path: files.alpha.display().to_string(),
            source_profile_output_path: files.profile.display().to_string(),
        };

        let Err(error) = run_worker(&worker, &files, &request) else {
            panic!("malformed raster must fail");
        };
        let RasterWorkerError::WorkerRejected(detail) = error else {
            panic!("expected structured worker rejection, got {error:?}");
        };
        assert!(
            detail.starts_with("raster-rejected:"),
            "stable rejection code: {detail}"
        );
        assert!(detail.len() <= 256, "failure detail must remain bounded");
        assert!(!detail.contains(&files.directory.display().to_string()));
        assert!(!detail.contains("source.raster"));
        assert!(!detail.contains("decoded.rgb"));
        assert!(!detail.contains("decoded.alpha"));
        assert!(!detail.contains("source.icc"));
        assert!(!files.rgb.exists());
        assert!(!files.alpha.exists());
        assert!(!files.profile.exists());
    }

    #[cfg(windows)]
    #[test]
    fn packaged_raster_worker_decodes_supported_formats_and_rejects_malformed_payloads() {
        use super::{
            DecodeFiles, PROTOCOL_VERSION, PreparedWorker, WireRequest, resolve_worker, run_worker,
        };

        let cases: [(&str, &[u8], &[u8], &str); 3] = [
            (
                "PNG",
                include_bytes!("../tests/fixtures/valid-3x2.png"),
                include_bytes!("../tests/fixtures/truncated-pixels-3x2.png"),
                "image/png",
            ),
            (
                "JPEG",
                include_bytes!("../tests/fixtures/valid-3x2.jpg"),
                include_bytes!("../tests/fixtures/invalid-huffman-3x2.jpg"),
                "image/jpeg",
            ),
            (
                "TIFF",
                include_bytes!("../tests/fixtures/valid-3x2.tif"),
                include_bytes!("../tests/fixtures/truncated-pixels-3x2.tif"),
                "image/tiff",
            ),
        ];
        let worker = PreparedWorker::new(&resolve_worker().expect("packaged worker"))
            .expect("private worker");

        for (format, valid, malformed, media_type) in cases {
            let decoded = TrustedRasterBoundary::resolve_packaged()
                .decode(
                    valid,
                    &AssetHash::for_bytes(valid),
                    UntaggedRasterPolicy::AssumeSrgb,
                )
                .unwrap_or_else(|error| panic!("{format} control must decode: {error:?}"));
            assert_eq!(decoded.inspection().media_type, media_type);
            assert_eq!((decoded.width(), decoded.height()), (3, 2));
            assert_eq!(decoded.rgb_pixels().len(), 18);
            assert_eq!(decoded.alpha_pixels(), [255; 6]);

            // The bytes still identify a supported format and dimensions. The
            // worker must reach pixel decoding, reject, and leave no output.
            let inspection = document_core::inspect_raster(malformed)
                .unwrap_or_else(|error| panic!("{format} header must inspect: {error:?}"));
            assert_eq!(inspection.media_type, media_type);
            assert_eq!((inspection.width_pixels, inspection.height_pixels), (3, 2));
            let files = DecodeFiles::new(malformed).expect("stage exact malformed raster");
            let request = WireRequest {
                protocol_version: PROTOCOL_VERSION,
                candidate_path: files.encoded.display().to_string(),
                expected_asset_sha256: AssetHash::for_bytes(malformed).as_str().to_owned(),
                untagged_raster_policy: UntaggedRasterPolicy::AssumeSrgb,
                rgb_output_path: files.rgb.display().to_string(),
                alpha_output_path: files.alpha.display().to_string(),
                source_profile_output_path: files.profile.display().to_string(),
            };
            let Err(error) = run_worker(&worker, &files, &request) else {
                panic!("{format} malformed payload must fail in confined worker");
            };
            let RasterWorkerError::WorkerRejected(detail) = error else {
                panic!("{format} expected structured worker rejection, got {error:?}");
            };
            assert!(
                detail.starts_with("decode-failed:"),
                "{format} must reach pixel decoding: {detail}"
            );
            assert!(
                detail.len() <= 256,
                "{format} failure detail must be bounded"
            );
            for private_path in [
                files.directory.display().to_string(),
                files.encoded.display().to_string(),
                files.rgb.display().to_string(),
                files.alpha.display().to_string(),
                files.profile.display().to_string(),
            ] {
                assert!(
                    !detail.contains(&private_path),
                    "{format} failure must not reveal private paths"
                );
            }
            assert!(!files.rgb.exists(), "{format} must leave no RGB output");
            assert!(!files.alpha.exists(), "{format} must leave no alpha output");
            assert!(
                !files.profile.exists(),
                "{format} must leave no profile output"
            );
        }
    }
    #[test]
    fn worker_replacement_after_pinning_is_rejected() {
        use super::{Hash32, PreparedWorker, WorkerBinding, private_directory, write_exclusive};
        let directory = private_directory("publisher-raster-pin-test").expect("test directory");
        let path = directory.join("worker");
        write_exclusive(&path, b"original executable fixture").expect("stage fixture");
        let binding = WorkerBinding {
            path: path.clone(),
            executable_hash: Hash32::of(b"original executable fixture"),
        };
        std::fs::write(&path, b"replacement executable fixture").expect("replace fixture");
        assert!(matches!(
            PreparedWorker::new(&binding),
            Err(RasterWorkerError::ExecutableIdentityDrift)
        ));
        std::fs::remove_file(path).expect("remove fixture");
        std::fs::remove_dir(directory).expect("remove fixture directory");
    }
    #[cfg(not(windows))]
    #[test]
    fn unsupported_platform_refuses_to_launch_a_worker() {
        use super::{
            DecodeFiles, Hash32, PreparedWorker, RunningProcess, WorkerBinding, private_directory,
            write_exclusive,
        };
        let directory =
            private_directory("publisher-unsupported-worker-test").expect("test directory");
        let path = directory.join("worker");
        write_exclusive(&path, b"inert fixture").expect("stage fixture");
        let worker = PreparedWorker::new(&WorkerBinding {
            path: path.clone(),
            executable_hash: Hash32::of(b"inert fixture"),
        })
        .expect("private copy");
        let files = DecodeFiles::new(b"source").expect("private files");
        assert!(
            matches!(RunningProcess::spawn(&worker, &files, std::time::Duration::from_secs(1)), Err(message) if message.contains("supported only on Windows"))
        );
        std::fs::remove_file(path).expect("remove fixture");
        std::fs::remove_dir(directory).expect("remove test directory");
    }
}
