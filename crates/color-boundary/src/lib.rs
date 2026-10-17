//! Trust-owning boundary for confined `LittleCMS` raster conversion.

use raster_capacity::{RasterMemoryPermit, RasterWorkPermit, production_raster_pixels};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    process::ExitStatus,
    thread,
    time::{Duration, Instant, SystemTime},
};

const PROTOCOL_VERSION: u16 = 1;
const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TAGGED_RASTER_DIMENSION: u32 = raster_capacity::MAX_PRODUCTION_RASTER_DIMENSION;
pub const MAX_TAGGED_RASTER_PIXELS: usize = raster_capacity::MAX_PRODUCTION_RASTER_PIXELS;
pub const COLOR_ENGINE_IDENTITY: &str = "LittleCMS/lcms2-rs-6.2.0/RGB8-CMYK8";
pub const COLOR_RULES_IDENTITY: &str =
    "publisher-v2/color-conversion/v1/relative-colorimetric/no-proofing";
const MAX_WORKER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REPORT_BYTES: u64 = 64 * 1024;
#[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
const WORKER_MEMORY_LIMIT: usize = raster_capacity::MAX_RASTER_COLOR_WORKER_BYTES;
const CONVERSION_TIMEOUT: Duration = Duration::from_secs(120);
const INSPECTION_RULES: &str =
    "patterboxpdf/output-intent/v2/exact-edition-types/all-intents-bidirectional";

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkerBinding {
    path: PathBuf,
    executable_hash: Hash32,
}

/// A packaged worker identity pinned before any conversion begins.
///
/// There is intentionally no constructor accepting an arbitrary executable or
/// caller-supplied digest. The boundary resolves only the installed sidecar and
/// rechecks the exact bytes before executing a private copy.
pub struct TrustedColorBoundary {
    binding: Result<WorkerBinding, ColorWorkerError>,
}

impl TrustedColorBoundary {
    /// Inspects exact output-profile bytes inside the pinned, confined worker.
    ///
    /// # Errors
    /// Fails closed on malformed profiles, unavailable confinement, or identity drift.
    pub fn inspect_output_profile(
        &self,
        profile: &[u8],
    ) -> Result<TrustedOutputProfile, ColorWorkerError> {
        if profile.is_empty() || profile.len() > MAX_ICC_PROFILE_BYTES {
            return Err(ColorWorkerError::ResourceLimit);
        }
        let _work = RasterWorkPermit::acquire().map_err(|_| ColorWorkerError::ResourceLimit)?;
        let binding = self.binding.as_ref().map_err(Clone::clone)?;
        let prepared = PreparedWorker::new(binding)?;
        // Reuse the same private-run lifetime and permission grants as conversion.
        let files = ConversionFiles::new(&[], &[], profile)?;
        let request = InspectionRequest {
            protocol_version: PROTOCOL_VERSION,
            operation: "inspect-output-profile",
            output_profile_path: files.output_profile.display().to_string(),
            expected_output_profile_sha256: Hash32::of(profile).hex(),
            expected_byte_length: profile.len(),
        };
        let report: InspectionReport = run_worker_report(&prepared, &files, &request)?;
        accept_inspection_report(profile, &report, prepared.executable_hash)
    }

    #[must_use]
    pub fn resolve_packaged() -> Self {
        Self {
            binding: resolve_worker(),
        }
    }

    /// Converts exact tagged-RGB bytes with the pinned, confined packaged worker.
    ///
    /// # Errors
    ///
    /// Fails closed for invalid resources, executable drift, confinement failure,
    /// timeout, malformed output, or any identity mismatch.
    pub fn convert(
        &self,
        rgb_pixels: &[u8],
        width: u32,
        height: u32,
        source_profile: &[u8],
        output_profile: &[u8],
    ) -> Result<TrustedColorConversion, ColorWorkerError> {
        validate_inputs(rgb_pixels, width, height, source_profile, output_profile)?;
        let _work = RasterWorkPermit::acquire().map_err(|_| ColorWorkerError::ResourceLimit)?;
        let pixels =
            production_raster_pixels(width, height).map_err(|_| ColorWorkerError::ResourceLimit)?;
        let memory =
            RasterMemoryPermit::reserve(pixels * 4).map_err(|_| ColorWorkerError::ResourceLimit)?;
        let binding = self.binding.as_ref().map_err(Clone::clone)?;
        let prepared = PreparedWorker::new(binding)?;
        let work = ConversionFiles::new(rgb_pixels, source_profile, output_profile)?;
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            rgb_pixels_path: work.rgb.display().to_string(),
            expected_rgb_pixels_sha256: Hash32::of(rgb_pixels).hex(),
            source_profile_path: work.source_profile.display().to_string(),
            expected_source_profile_sha256: Hash32::of(source_profile).hex(),
            output_profile_path: work.output_profile.display().to_string(),
            expected_output_profile_sha256: Hash32::of(output_profile).hex(),
            converted_output_path: work.converted.display().to_string(),
            width,
            height,
            rendering_intent: "relative-colorimetric",
        };
        let report = run_worker(&prepared, &work, &request)?;
        let expected_engine = Hash32::of(COLOR_ENGINE_IDENTITY.as_bytes());
        let expected_rules = Hash32::of(COLOR_RULES_IDENTITY.as_bytes());
        if report.protocol_version != PROTOCOL_VERSION
            || report.raw_pixels_sha256 != request.expected_rgb_pixels_sha256
            || report.source_profile_sha256 != request.expected_source_profile_sha256
            || report.output_profile_sha256 != request.expected_output_profile_sha256
            || report.width != width
            || report.height != height
            || report.rendering_intent != request.rendering_intent
            || report.engine_identity != COLOR_ENGINE_IDENTITY
            || report.rules_identity != COLOR_RULES_IDENTITY
            || report.engine_identity_sha256 != expected_engine.hex()
            || report.rules_identity_sha256 != expected_rules.hex()
        {
            return Err(ColorWorkerError::IdentityDrift);
        }
        let expected_len = usize::try_from(u64::from(width) * u64::from(height) * 4)
            .map_err(|_| ColorWorkerError::ResourceLimit)?;
        let converted = read_exact_bounded(&work.converted, expected_len)?;
        let converted_hash = Hash32::of(&converted);
        if report.converted_sha256 != converted_hash.hex()
            || report.converted_byte_length != expected_len
        {
            return Err(ColorWorkerError::IdentityDrift);
        }
        Ok(TrustedColorConversion {
            _memory: memory,
            cmyk_pixels: converted,
            source_profile_hash: Hash32::of(source_profile),
            raw_pixel_hash: Hash32::of(rgb_pixels),
            output_profile_hash: Hash32::of(output_profile),
            converted_cmyk_hash: converted_hash,
            width,
            height,
            engine_identity_hash: expected_engine,
            rules_identity_hash: expected_rules,
            worker_executable_hash: prepared.executable_hash,
        })
    }

    /// Converts authored sRGB pixels using a deterministic, normalized source
    /// profile owned by this trust boundary.
    ///
    /// # Errors
    ///
    /// Fails closed if the canonical profile cannot be produced or the normal
    /// confined conversion rejects any exact input.
    pub fn convert_srgb(
        &self,
        rgb_pixels: &[u8],
        width: u32,
        height: u32,
        output_profile: &[u8],
    ) -> Result<TrustedColorConversion, ColorWorkerError> {
        let source_profile = canonical_srgb_profile()?;
        self.convert(rgb_pixels, width, height, &source_profile, output_profile)
    }
}

/// Opaque evidence that the exact ICC bytes passed native parsing and both
/// required transforms inside the confined worker. Callers cannot mint receipts.
/// ```compile_fail
/// use color_boundary::TrustedOutputProfile;
/// let forged = TrustedOutputProfile { byte_len: 128 };
/// ```
pub struct TrustedOutputProfile {
    hash: Hash32,
    byte_len: usize,
    worker_executable_hash: Hash32,
}

impl TrustedOutputProfile {
    #[must_use]
    pub const fn profile_hash(&self) -> [u8; 32] {
        self.hash.0
    }
    #[must_use]
    pub const fn byte_len(&self) -> usize {
        self.byte_len
    }
    #[must_use]
    pub const fn worker_executable_hash(&self) -> [u8; 32] {
        self.worker_executable_hash.0
    }
}

#[derive(Serialize)]
struct InspectionRequest {
    protocol_version: u16,
    operation: &'static str,
    output_profile_path: String,
    expected_output_profile_sha256: String,
    expected_byte_length: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectionReport {
    protocol_version: u16,
    operation: String,
    output_profile_sha256: String,
    byte_length: usize,
    engine_identity: String,
    rules_identity: String,
}

fn accept_inspection_report(
    profile: &[u8],
    report: &InspectionReport,
    worker_executable_hash: Hash32,
) -> Result<TrustedOutputProfile, ColorWorkerError> {
    let hash = Hash32::of(profile);
    if report.protocol_version != PROTOCOL_VERSION
        || report.operation != "inspect-output-profile"
        || report.output_profile_sha256 != hash.hex()
        || report.byte_length != profile.len()
        || report.engine_identity != COLOR_ENGINE_IDENTITY
        || report.rules_identity != INSPECTION_RULES
    {
        return Err(ColorWorkerError::IdentityDrift);
    }
    Ok(TrustedOutputProfile {
        hash,
        byte_len: profile.len(),
        worker_executable_hash,
    })
}

/// Opaque proof that exact color resources crossed the confined worker boundary.
///
/// Fields and constructors are private so downstream crates can inspect and
/// consume this value but cannot mint accepted evidence themselves.
///
/// ```compile_fail
/// use color_boundary::TrustedColorConversion;
/// let forged = TrustedColorConversion { cmyk_pixels: Vec::new() };
/// ```
pub struct TrustedColorConversion {
    _memory: RasterMemoryPermit,
    cmyk_pixels: Vec<u8>,
    source_profile_hash: Hash32,
    raw_pixel_hash: Hash32,
    output_profile_hash: Hash32,
    converted_cmyk_hash: Hash32,
    width: u32,
    height: u32,
    engine_identity_hash: Hash32,
    rules_identity_hash: Hash32,
    worker_executable_hash: Hash32,
}

impl TrustedColorConversion {
    #[must_use]
    pub fn cmyk_pixels(&self) -> &[u8] {
        &self.cmyk_pixels
    }
    #[must_use]
    pub const fn source_profile_hash(&self) -> [u8; 32] {
        self.source_profile_hash.0
    }
    #[must_use]
    pub const fn raw_pixel_hash(&self) -> [u8; 32] {
        self.raw_pixel_hash.0
    }
    #[must_use]
    pub const fn output_profile_hash(&self) -> [u8; 32] {
        self.output_profile_hash.0
    }
    #[must_use]
    pub const fn converted_cmyk_hash(&self) -> [u8; 32] {
        self.converted_cmyk_hash.0
    }
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }
    #[must_use]
    pub const fn engine_identity_hash(&self) -> [u8; 32] {
        self.engine_identity_hash.0
    }
    #[must_use]
    pub const fn rules_identity_hash(&self) -> [u8; 32] {
        self.rules_identity_hash.0
    }
    #[must_use]
    pub const fn worker_executable_hash(&self) -> [u8; 32] {
        self.worker_executable_hash.0
    }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ColorWorkerError {
    NotInstalled,
    InvalidExecutable(String),
    ExecutableIdentityDrift,
    ResourceLimit,
    Io(String),
    SpawnFailed(String),
    ConfinementFailed(String),
    TimedOut,
    WorkerRejected(String),
    MalformedReport(String),
    IdentityDrift,
    SourceProfile(String),
}

fn canonical_srgb_profile() -> Result<Vec<u8>, ColorWorkerError> {
    let mut profile = lcms2::Profile::new_srgb()
        .icc()
        .map_err(|error| ColorWorkerError::SourceProfile(error.to_string()))?;
    if profile.len() < 128 || profile.get(36..40) != Some(b"acsp") {
        return Err(ColorWorkerError::SourceProfile(
            "LittleCMS returned an invalid sRGB profile".to_owned(),
        ));
    }
    // LittleCMS stamps virtual profiles with time and host fields. They are
    // informational, so normalize them to keep evidence deterministic.
    profile[24..36].copy_from_slice(&[
        0x07, 0xd0, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);
    profile[40..44].copy_from_slice(b"MSFT");
    profile[84..100].fill(0);
    Ok(profile)
}

#[derive(Serialize)]
struct WireRequest {
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
    rendering_intent: &'static str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReport {
    protocol_version: u16,
    raw_pixels_sha256: String,
    source_profile_sha256: String,
    output_profile_sha256: String,
    converted_sha256: String,
    converted_byte_length: usize,
    width: u32,
    height: u32,
    rendering_intent: String,
    engine_identity: String,
    engine_identity_sha256: String,
    rules_identity: String,
    rules_identity_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFailure {
    protocol_version: u16,
    error: String,
    detail: String,
}

fn validate_inputs(
    rgb: &[u8],
    width: u32,
    height: u32,
    source_profile: &[u8],
    output_profile: &[u8],
) -> Result<(), ColorWorkerError> {
    if width == 0
        || height == 0
        || width > MAX_TAGGED_RASTER_DIMENSION
        || height > MAX_TAGGED_RASTER_DIMENSION
    {
        return Err(ColorWorkerError::ResourceLimit);
    }
    let pixels = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| ColorWorkerError::ResourceLimit)?;
    if pixels > MAX_TAGGED_RASTER_PIXELS || rgb.len() != pixels.saturating_mul(3) {
        return Err(ColorWorkerError::ResourceLimit);
    }
    if source_profile.is_empty()
        || output_profile.is_empty()
        || source_profile.len() > MAX_ICC_PROFILE_BYTES
        || output_profile.len() > MAX_ICC_PROFILE_BYTES
    {
        return Err(ColorWorkerError::ResourceLimit);
    }
    Ok(())
}

fn resolve_worker() -> Result<WorkerBinding, ColorWorkerError> {
    let executable = std::env::current_exe()
        .map_err(|error| ColorWorkerError::InvalidExecutable(error.to_string()))?;
    resolve_worker_from(&executable)
}

fn resolve_worker_from(executable: &Path) -> Result<WorkerBinding, ColorWorkerError> {
    for path in worker_candidates(executable) {
        if path.exists() {
            let bytes = read_bounded(&path, MAX_WORKER_BYTES)?;
            return Ok(WorkerBinding {
                path,
                executable_hash: Hash32::of(&bytes),
            });
        }
    }
    Err(ColorWorkerError::NotInstalled)
}

fn worker_candidates(executable: &Path) -> Vec<PathBuf> {
    let name = if cfg!(windows) {
        "publisher-color-worker.exe"
    } else {
        "publisher-color-worker"
    };
    // Only debug builds may discover development executables. Packaged releases
    // fail closed when their adjacent sidecar is missing.
    let cargo_test_worker = cfg!(debug_assertions)
        .then(|| option_env!("CARGO_BIN_EXE_publisher-color-worker").map(PathBuf::from))
        .flatten();
    [
        Some(executable.with_file_name(name)),
        cargo_test_worker,
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
        Err(ColorWorkerError::NotInstalled)
    ));
    write_exclusive(&candidates[0], b"pinned fixture bytes").unwrap();
    let binding = resolve_worker_from(&host).unwrap();
    assert_eq!(binding.path, candidates[0]);
    assert_eq!(binding.executable_hash, Hash32::of(b"pinned fixture bytes"));
    std::fs::remove_file(&candidates[0]).unwrap();
    std::fs::remove_dir(directory).unwrap();
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
    fn new(binding: &WorkerBinding) -> Result<Self, ColorWorkerError> {
        let bytes = read_bounded(&binding.path, MAX_WORKER_BYTES)?;
        if Hash32::of(&bytes) != binding.executable_hash {
            return Err(ColorWorkerError::ExecutableIdentityDrift);
        }
        let directory = private_directory("publisher-color-executable")?;
        let path = directory.join(if cfg!(windows) {
            "publisher-color-worker.exe"
        } else {
            "publisher-color-worker"
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
        let mut identity_lock = options.open(&prepared.path).map_err(io_error)?;
        identity_lock.rewind().map_err(io_error)?;
        let mut exact = Vec::new();
        Read::by_ref(&mut identity_lock)
            .take(MAX_WORKER_BYTES + 1)
            .read_to_end(&mut exact)
            .map_err(io_error)?;
        if Hash32::of(&exact) != binding.executable_hash {
            return Err(ColorWorkerError::ExecutableIdentityDrift);
        }
        prepared.identity_lock = Some(identity_lock);
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
        let _ignored = std::fs::remove_file(&self.path);
        let _ignored = std::fs::remove_dir(&self.directory);
    }
}

struct ConversionFiles {
    output_directory: PathBuf,
    directory: PathBuf,
    rgb: PathBuf,
    source_profile: PathBuf,
    output_profile: PathBuf,
    converted: PathBuf,
}

impl ConversionFiles {
    fn new(rgb: &[u8], source: &[u8], output: &[u8]) -> Result<Self, ColorWorkerError> {
        let directory = private_directory("publisher-color-conversion")?;
        let output_directory = directory.join("outputs");
        let files = Self {
            rgb: directory.join("pixels.rgb"),
            source_profile: directory.join("source.icc"),
            output_profile: directory.join("output.icc"),
            converted: output_directory.join("converted.cmyk"),
            output_directory,
            directory,
        };
        write_exclusive(&files.rgb, rgb)?;
        write_exclusive(&files.source_profile, source)?;
        write_exclusive(&files.output_profile, output)?;
        std::fs::create_dir(&files.output_directory).map_err(io_error)?;
        Ok(files)
    }
}

impl Drop for ConversionFiles {
    fn drop(&mut self) {
        for path in [
            &self.rgb,
            &self.source_profile,
            &self.output_profile,
            &self.converted,
        ] {
            let _ignored = std::fs::remove_file(path);
        }
        let _ignored = std::fs::remove_dir_all(&self.output_directory);
        let _ignored = std::fs::remove_dir(&self.directory);
    }
}

fn run_worker(
    worker: &PreparedWorker,
    files: &ConversionFiles,
    request: &WireRequest,
) -> Result<WireReport, ColorWorkerError> {
    run_worker_report(worker, files, request)
}

fn run_worker_report<T: serde::de::DeserializeOwned>(
    worker: &PreparedWorker,
    files: &ConversionFiles,
    request: &impl Serialize,
) -> Result<T, ColorWorkerError> {
    let input = serde_json::to_vec(request)
        .map_err(|error| ColorWorkerError::MalformedReport(error.to_string()))?;
    let mut child = RunningProcess::spawn(worker, files, CONVERSION_TIMEOUT)
        .map_err(ColorWorkerError::ConfinementFailed)?;
    let mut stdin = child
        .take_stdin()
        .ok_or_else(|| ColorWorkerError::SpawnFailed("worker stdin unavailable".into()))?;
    let stdout = child
        .take_stdout()
        .ok_or_else(|| ColorWorkerError::MalformedReport("stdout unavailable".into()))?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| ColorWorkerError::MalformedReport("stderr unavailable".into()))?;
    let writer = thread::spawn(move || stdin.write_all(&input));
    // Drain during execution so a bounded or hostile report cannot fill a pipe
    // and hide the worker's termination from the owning boundary.
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
    let deadline = Instant::now() + CONVERSION_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                child.terminate();
                let _ = writer.join();
                let _ = reader.join();
                let _ = stderr_reader.join();
                return Err(ColorWorkerError::TimedOut);
            }
            Err(error) => {
                child.terminate();
                let _ = writer.join();
                let _ = reader.join();
                let _ = stderr_reader.join();
                return Err(if error.kind() == io::ErrorKind::TimedOut {
                    ColorWorkerError::TimedOut
                } else {
                    ColorWorkerError::Io(error.to_string())
                });
            }
        }
    };
    let written = writer
        .join()
        .map_err(|_| ColorWorkerError::Io("worker input writer panicked".into()))?;
    let output = reader
        .join()
        .map_err(|_| ColorWorkerError::Io("worker output reader panicked".into()))?
        .map_err(io_error)?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| ColorWorkerError::Io("worker stderr reader panicked".into()))?
        .map_err(io_error)?;
    if written.is_err() && status.success() {
        return Err(ColorWorkerError::Io("worker input pipe closed".into()));
    }
    if output.len() as u64 > MAX_REPORT_BYTES || stderr.len() as u64 > MAX_REPORT_BYTES {
        return Err(ColorWorkerError::ResourceLimit);
    }
    if !status.success() {
        let failure: WireFailure = serde_json::from_slice(&output)
            .map_err(|error| ColorWorkerError::MalformedReport(error.to_string()))?;
        if failure.protocol_version != PROTOCOL_VERSION {
            return Err(ColorWorkerError::IdentityDrift);
        }
        return Err(ColorWorkerError::WorkerRejected(format!(
            "{}: {}",
            failure.error, failure.detail
        )));
    }
    serde_json::from_slice(&output)
        .map_err(|error| ColorWorkerError::MalformedReport(error.to_string()))
}

fn private_directory(label: &str) -> Result<PathBuf, ColorWorkerError> {
    for attempt in 0..16_u32 {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| ColorWorkerError::Io(error.to_string()))?
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
    Err(ColorWorkerError::Io(
        "could not allocate private color-worker directory".into(),
    ))
}

fn write_exclusive(path: &Path, bytes: &[u8]) -> Result<(), ColorWorkerError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(io_error)
}

fn read_exact_bounded(path: &Path, expected: usize) -> Result<Vec<u8>, ColorWorkerError> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if !metadata.file_type().is_file() || metadata.len() != expected as u64 {
        return Err(ColorWorkerError::IdentityDrift);
    }
    let bytes = read_bounded(path, expected as u64)?;
    if bytes.len() != expected {
        return Err(ColorWorkerError::IdentityDrift);
    }
    Ok(bytes)
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ColorWorkerError> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > limit {
        return Err(ColorWorkerError::ResourceLimit);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(
            usize::try_from(metadata.len()).map_err(|_| ColorWorkerError::ResourceLimit)?,
        )
        .map_err(|_| ColorWorkerError::ResourceLimit)?;
    File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(ColorWorkerError::ResourceLimit);
    }
    Ok(bytes)
}

#[allow(clippy::needless_pass_by_value)]
fn io_error(error: std::io::Error) -> ColorWorkerError {
    ColorWorkerError::Io(error.to_string())
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
        files: &ConversionFiles,
        timeout: Duration,
    ) -> Result<Self, String> {
        #[cfg(windows)]
        {
            let profile =
                publisher_worker_job::AppContainerProfile::create_unique("PublisherV2.Color")
                    .map_err(|error| error.to_string())?;
            profile
                .grant_read_execute(&worker.directory)
                .map_err(|error| error.to_string())?;
            // Grants come from host-owned run files, never from request paths.
            for input in [&files.rgb, &files.source_profile, &files.output_profile] {
                profile
                    .grant_read(input)
                    .map_err(|error| error.to_string())?;
            }
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
                .grant_read(&files.rgb)
                .map_err(|error| error.to_string())?;
            profile
                .grant_read(&files.source_profile)
                .map_err(|error| error.to_string())?;
            profile
                .grant_read(&files.output_profile)
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
    #[test]
    fn inspection_fails_closed_without_a_worker() {
        let boundary = super::TrustedColorBoundary {
            binding: Err(super::ColorWorkerError::NotInstalled),
        };
        assert!(matches!(
            boundary.inspect_output_profile(b"profile"),
            Err(super::ColorWorkerError::NotInstalled)
        ));
    }

    #[test]
    fn inspection_report_rejects_each_identity_mismatch() {
        use super::{
            COLOR_ENGINE_IDENTITY, Hash32, INSPECTION_RULES, InspectionReport, PROTOCOL_VERSION,
            accept_inspection_report,
        };
        for field in 0..6 {
            let mut report = InspectionReport {
                protocol_version: PROTOCOL_VERSION,
                operation: "inspect-output-profile".into(),
                output_profile_sha256: Hash32::of(b"profile").hex(),
                byte_length: 7,
                engine_identity: COLOR_ENGINE_IDENTITY.into(),
                rules_identity: INSPECTION_RULES.into(),
            };
            match field {
                0 => report.protocol_version += 1,
                1 => report.operation = "convert".into(),
                2 => report.output_profile_sha256 = "0".repeat(64),
                3 => report.byte_length += 1,
                4 => report.engine_identity.push('!'),
                _ => report.rules_identity.push('!'),
            }
            assert!(matches!(
                accept_inspection_report(b"profile", &report, Hash32::of(b"worker")),
                Err(super::ColorWorkerError::IdentityDrift)
            ));
        }
    }

    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    #[test]
    fn packaged_worker_inspects_profile_and_rejects_native_transform_corruption() {
        let profile = include_bytes!("../../print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
        let boundary = super::TrustedColorBoundary::resolve_packaged();
        let receipt = boundary
            .inspect_output_profile(profile)
            .expect("confined profile inspection");
        assert_eq!(receipt.profile_hash(), super::Hash32::of(profile).0);
        assert_eq!(receipt.byte_len(), profile.len());
        assert_ne!(receipt.worker_executable_hash(), [0; 32]);
        let mut corrupted = profile.to_vec();
        let count = u32::from_be_bytes(corrupted[128..132].try_into().unwrap()) as usize;
        for index in 0..count {
            let record = 132 + index * 12;
            let offset =
                u32::from_be_bytes(corrupted[record + 4..record + 8].try_into().unwrap()) as usize;
            corrupted[offset..offset + 4].copy_from_slice(b"BAD!");
        }
        assert_eq!(corrupted.len(), 2_730_944);
        assert_eq!(
            super::Hash32::of(&corrupted).hex(),
            "1dcf6a28e91f8654adc0adc23ded043686ad1344a35a83c80ed44a3c27092a9e"
        );
        let Err(error) = boundary.inspect_output_profile(&corrupted) else {
            panic!("corrupted ICC profile must fail");
        };
        let super::ColorWorkerError::WorkerRejected(detail) = error else {
            panic!("expected structured worker rejection, got {error:?}");
        };
        assert!(
            detail.starts_with("invalid-icc:"),
            "stable rejection code: {detail}"
        );
        assert!(detail.len() <= 256, "failure detail must remain bounded");
        assert!(!detail.contains('\\'));
        assert!(!detail.contains("publisher-color"));
    }

    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    use super::TrustedColorBoundary;
    use super::{
        ColorWorkerError, Hash32, MAX_WORKER_BYTES, PreparedWorker, WorkerBinding,
        canonical_srgb_profile, private_directory, read_bounded, resolve_worker, validate_inputs,
        write_exclusive,
    };

    #[test]
    fn host_rejects_invalid_sizes_before_starting_native_code() {
        assert_eq!(
            validate_inputs(&[1, 2], 1, 1, b"profile", b"output"),
            Err(ColorWorkerError::ResourceLimit)
        );
        assert_eq!(
            validate_inputs(&[], 0, 1, b"profile", b"output"),
            Err(ColorWorkerError::ResourceLimit)
        );
    }

    #[test]
    fn authored_srgb_profile_is_canonical_across_repeated_preparation() {
        let first = canonical_srgb_profile().expect("canonical sRGB profile");
        let second = canonical_srgb_profile().expect("repeated canonical sRGB profile");

        assert_eq!(first, second);
        assert_eq!(&first[24..36], &[0x07, 0xd0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&first[40..44], b"MSFT");
        assert!(first[84..100].iter().all(|byte| *byte == 0));
    }

    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    #[test]
    fn confined_worker_converts_valid_profile_and_rejects_out_of_bounds_icc_tag() {
        use super::{ConversionFiles, PROTOCOL_VERSION, WireRequest, run_worker};

        let source_profile = lcms2::Profile::new_srgb()
            .icc()
            .expect("LittleCMS serializes sRGB");
        let output_profile =
            include_bytes!("../../print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
        let rgb = [255, 0, 0, 0, 255, 0];
        let conversion = TrustedColorBoundary::resolve_packaged()
            .convert(&rgb, 2, 1, &source_profile, output_profile)
            .expect("packaged color worker converts exact inputs");

        assert_eq!(conversion.cmyk_pixels.len(), 8);
        assert_eq!(conversion.raw_pixel_hash, Hash32::of(&rgb));
        assert_ne!(conversion.worker_executable_hash(), [0; 32]);
        assert_eq!(
            conversion.source_profile_hash(),
            Hash32::of(&source_profile).0
        );
        assert_eq!(
            conversion.output_profile_hash(),
            Hash32::of(output_profile).0
        );
        assert_eq!(
            conversion.converted_cmyk_hash(),
            Hash32::of(conversion.cmyk_pixels()).0
        );

        // Keep the real ICC header, tag signature and payload offset. Only the
        // first tag's declared length extends one byte beyond the file.
        let mut malformed = output_profile.to_vec();
        assert_eq!(&malformed[36..40], b"acsp");
        assert_eq!(
            usize::try_from(u32::from_be_bytes(malformed[0..4].try_into().unwrap())).unwrap(),
            malformed.len()
        );
        assert_eq!(&malformed[132..136], b"desc");
        let tag_offset =
            usize::try_from(u32::from_be_bytes(malformed[136..140].try_into().unwrap())).unwrap();
        let out_of_bounds_length = u32::try_from(malformed.len() - tag_offset + 1).unwrap();
        malformed[140..144].copy_from_slice(&out_of_bounds_length.to_be_bytes());
        assert_eq!(
            Hash32::of(&malformed).hex(),
            "156984d3aea7032a8c1bf33adc108b10cc9cb0d081d9fd6fccc2ad19378affb4"
        );

        let worker = PreparedWorker::new(&resolve_worker().expect("packaged worker"))
            .expect("private worker");
        let files = ConversionFiles::new(&rgb, &source_profile, &malformed)
            .expect("stage exact malformed ICC");
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            rgb_pixels_path: files.rgb.display().to_string(),
            expected_rgb_pixels_sha256: Hash32::of(&rgb).hex(),
            source_profile_path: files.source_profile.display().to_string(),
            expected_source_profile_sha256: Hash32::of(&source_profile).hex(),
            output_profile_path: files.output_profile.display().to_string(),
            expected_output_profile_sha256: Hash32::of(&malformed).hex(),
            converted_output_path: files.converted.display().to_string(),
            width: 2,
            height: 1,
            rendering_intent: "relative-colorimetric",
        };
        let Err(error) = run_worker(&worker, &files, &request) else {
            panic!("out-of-bounds ICC tag must reject before conversion output");
        };
        let ColorWorkerError::WorkerRejected(detail) = error else {
            panic!("expected structured ICC rejection, got {error:?}");
        };
        assert_eq!(
            detail,
            "invalid-icc: output profile: exact ICC byte validation: MalformedTagTable",
            "exact byte validation rejects the malformed table before conversion"
        );
        assert!(detail.len() <= 256, "failure detail must be bounded");
        assert!(!detail.contains(&files.directory.display().to_string()));
        assert!(!detail.contains("output.icc"));
        assert!(!detail.contains("converted.cmyk"));
        assert!(!files.converted.exists(), "no converted output may remain");
    }

    #[test]
    fn source_replacement_after_pinning_is_rejected() {
        let packaged = resolve_worker().expect("test worker is built");
        let bytes = read_bounded(&packaged.path, MAX_WORKER_BYTES).expect("read worker");
        let directory = private_directory("publisher-color-pin-test").expect("private directory");
        let source = directory.join("worker");
        write_exclusive(&source, &bytes).expect("stage source");
        let binding = WorkerBinding {
            path: source.clone(),
            executable_hash: Hash32::of(&bytes),
        };
        let mut replacement = bytes;
        replacement[0] ^= 0xff;
        std::fs::write(&source, replacement).expect("replace source after pinning");

        assert!(matches!(
            PreparedWorker::new(&binding),
            Err(ColorWorkerError::ExecutableIdentityDrift)
        ));
        std::fs::remove_file(source).expect("remove test source");
        std::fs::remove_dir(directory).expect("remove test directory");
    }

    #[test]
    fn private_executed_copy_remains_bound_if_source_changes() {
        let packaged = resolve_worker().expect("test worker is built");
        let bytes = read_bounded(&packaged.path, MAX_WORKER_BYTES).expect("read worker");
        let directory = private_directory("publisher-color-copy-test").expect("private directory");
        let source = directory.join("worker");
        write_exclusive(&source, &bytes).expect("stage source");
        let binding = WorkerBinding {
            path: source.clone(),
            executable_hash: Hash32::of(&bytes),
        };
        let prepared = PreparedWorker::new(&binding).expect("prepare exact private copy");
        let mut replacement = bytes;
        replacement[0] ^= 0xff;
        std::fs::write(&source, replacement).expect("replace original source");

        let executed = read_bounded(&prepared.path, MAX_WORKER_BYTES).expect("read private copy");
        assert_eq!(Hash32::of(&executed), prepared.executable_hash);
        drop(prepared);
        std::fs::remove_file(source).expect("remove test source");
        std::fs::remove_dir(directory).expect("remove test directory");
    }
    #[cfg(any(windows, all(target_os = "linux", target_arch = "x86_64")))]
    #[test]
    fn packaged_color_worker_cannot_read_or_write_outside_its_private_run() {
        use super::{ConversionFiles, PROTOCOL_VERSION, WireRequest, run_worker};
        let source = canonical_srgb_profile().expect("sRGB");
        let output = include_bytes!("../../print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
        let rgb = [255, 0, 0];
        let worker = PreparedWorker::new(&resolve_worker().expect("packaged worker"))
            .expect("private worker");
        let files = ConversionFiles::new(&rgb, &source, output).expect("private inputs");
        let outside =
            ConversionFiles::new(&rgb, &source, output).expect("ungranted sentinel directory");
        let mut request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            rgb_pixels_path: outside.rgb.display().to_string(),
            expected_rgb_pixels_sha256: Hash32::of(&rgb).hex(),
            source_profile_path: files.source_profile.display().to_string(),
            expected_source_profile_sha256: Hash32::of(&source).hex(),
            output_profile_path: files.output_profile.display().to_string(),
            expected_output_profile_sha256: Hash32::of(output).hex(),
            converted_output_path: files.converted.display().to_string(),
            width: 1,
            height: 1,
            rendering_intent: "relative-colorimetric",
        };
        assert!(
            matches!(run_worker(&worker, &files, &request), Err(ColorWorkerError::WorkerRejected(detail)) if detail.starts_with("input-unavailable:"))
        );
        request.rgb_pixels_path = files.rgb.display().to_string();
        request.converted_output_path = outside.converted.display().to_string();
        assert!(
            matches!(run_worker(&worker, &files, &request), Err(ColorWorkerError::WorkerRejected(detail)) if detail.starts_with("output-unavailable:"))
        );
        assert!(!outside.converted.exists());
        assert_eq!(std::fs::read(&outside.rgb).expect("sentinel remains"), rgb);
    }
    #[cfg(not(any(windows, all(target_os = "linux", target_arch = "x86_64"))))]
    #[test]
    fn unsupported_platform_refuses_to_launch_a_worker() {
        use super::{
            ConversionFiles, Hash32, PreparedWorker, RunningProcess, WorkerBinding,
            private_directory, write_exclusive,
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
        let files = ConversionFiles::new(&[1, 2, 3], b"source", b"output").expect("private files");
        assert!(
            matches!(RunningProcess::spawn(&worker, &files, std::time::Duration::from_secs(1)), Err(message) if message.contains("requires Windows or qualified x86_64 Linux"))
        );
        std::fs::remove_file(path).expect("remove fixture");
        std::fs::remove_dir(directory).expect("remove test directory");
    }
}
