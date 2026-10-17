//! Trust-owning boundary for the packaged independent PDF/X validator.

#[cfg(feature = "publisher-writer-profile")]
pub mod writer_profile;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    process::ExitStatus,
    thread,
    time::{Duration, Instant},
};

const PROTOCOL_VERSION: u16 = 5;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;
const MAX_WORKER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CANDIDATE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FINDINGS: usize = 512;
const VALIDATION_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(windows)]
const QPDF_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(windows)]
const QPDF_MAX_OUTPUT_BYTES: u64 = 64 * 1024;
const WORKER_MEMORY_LIMIT: usize = 768 * 1024 * 1024;
#[cfg(windows)]
const QPDF_LARGE_MEMORY_LIMIT: usize = 2 * 1024 * 1024 * 1024;
pub const VALIDATOR_NAME: &str = "Publisher V2 PDF/X-4 validation authority";
pub const VALIDATOR_VERSION: &str = "0.9.0";
pub const SOURCE_IDENTITY: &str = "publisher-v2:pdf-x-4-authority:0.9.0:free-composite+writer-profile-v5+worker-first+bounded-qpdf-2g";
pub const AUTHORITY_ID: &str = "publisher-v2/pdf-x-4-authority/v1";
pub const ACTIVE_ICC_NAME: &str = "LittleCMS and Publisher active color-space validator";
pub const ACTIVE_ICC_VERSION: &str = "6";
pub const ACTIVE_ICC_SOURCE_IDENTITY: &str =
    "publisher-v2:active-color-space-audit:v6+LittleCMS/lcms2-rs-6.2.0";
pub const ACTIVE_ICC_RULES_IDENTITY: &str =
    "publisher-v2/pdf-x-4/active-color-space-validity-and-spot-consistency/v6";
pub const PUBLIC_CORE_NAME: &str = "Publisher V2 public PDF/X object-rule audit";
pub const PUBLIC_CORE_VERSION: &str = "7";
pub const PUBLIC_CORE_SOURCE_IDENTITY: &str = "publisher-v2/pdf-x-4/public-object-rule-audit/v1";
pub const PUBLIC_CORE_RULES_IDENTITY: &str =
    "publisher-v2/pdf-x-4/object-interactive-metadata-safe-action-annotation-form-rules/v7";
pub const DIAGNOSTIC_NAME: &str = "PDFOxide PDF/X validator";
pub const DIAGNOSTIC_VERSION: &str = "0.3.73";
pub const DIAGNOSTIC_SOURCE_IDENTITY: &str = "crates.io:pdf_oxide:0.3.73:MIT-OR-Apache-2.0";
pub const DIAGNOSTIC_RULES_IDENTITY: &str = "publisher-v2/pdf-oxide-0.3.73/pdf-x-4/v13/blocking-xmp-id/exact-cmyk-dest-profile/nested-appearance-outline-safe-sfnt-cff-header-identity-cid-to-gid-stream-simple-and-type3-glyphs";
#[cfg(windows)]
pub const QPDF_VERSION: &str = "12.4.1";
#[cfg(windows)]
pub const QPDF_SOURCE_IDENTITY: &str = "github:qpdf/qpdf:v12.4.1:Apache-2.0";
const RULES_MANIFEST: &[u8] = include_bytes!("../rules/pdfx4-authority-v1.json");

#[must_use]
pub fn expected_rules_identity_hash() -> [u8; 32] {
    Hash32::of(RULES_MANIFEST).0
}

#[must_use]
pub fn expected_source_identity_hash() -> [u8; 32] {
    Hash32::of(SOURCE_IDENTITY.as_bytes()).0
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

#[must_use]
pub fn expected_tool_identity_hash() -> [u8; 32] {
    tool_identity_hash().0
}

fn tool_identity_hash() -> Hash32 {
    let mut digest = Sha256::new();
    digest.update(b"publisher-v2/validator-tool/v1\0");
    digest.update((VALIDATOR_NAME.len() as u64).to_be_bytes());
    digest.update(VALIDATOR_NAME.as_bytes());
    digest.update((VALIDATOR_VERSION.len() as u64).to_be_bytes());
    digest.update(VALIDATOR_VERSION.as_bytes());
    Hash32(digest.finalize().into())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkerBinding {
    path: PathBuf,
    executable_hash: Hash32,
    byte_limit: u64,
}

/// Owner of the packaged validator identity captured before validation starts.
pub struct TrustedValidatorBoundary {
    binding: Result<WorkerBinding, ValidatorWorkerError>,
    #[cfg(windows)]
    qpdf_binding: Result<QpdfBinding, ValidatorWorkerError>,
}

impl TrustedValidatorBoundary {
    #[must_use]
    pub fn resolve_packaged() -> Self {
        Self {
            binding: resolve_validator_worker(),
            #[cfg(windows)]
            qpdf_binding: resolve_qpdf_package(),
        }
    }

    /// Validates exact candidate bytes with the pinned, process-limited packaged worker.
    ///
    /// # Errors
    ///
    /// Fails closed for executable drift, process-limit setup failure, resource limits,
    /// malformed reports, or any candidate/evaluation/profile/rules identity drift.
    pub fn validate(
        &self,
        candidate_bytes: &[u8],
        evaluation_hash: [u8; 32],
        capsule_hash: [u8; 32],
        profile_hash: [u8; 32],
    ) -> Result<TrustedValidation, ValidatorWorkerError> {
        let binding = self.binding.as_ref().map_err(Clone::clone)?;
        // Validate the closed stream profile and bounded sample extents before
        // qpdf can materialize a supported full-page plane. Failed/unsupported
        // inputs retain the smaller existing structural-check job limit.
        let validation = run_validator_worker(
            binding,
            ValidatorWorkerRequest {
                candidate_bytes,
                expected_artifact_hash: Hash32::of(candidate_bytes),
                evaluation_hash: Hash32(evaluation_hash),
                capsule_hash: Hash32(capsule_hash),
                profile_hash: Hash32(profile_hash),
            },
            VALIDATION_TIMEOUT,
        );
        #[cfg(windows)]
        let structural_check = run_qpdf_check(
            self.qpdf_binding.as_ref().map_err(Clone::clone)?,
            candidate_bytes,
            QPDF_TIMEOUT,
            if validation
                .as_ref()
                .is_ok_and(|checked| checked.verdict == TrustedValidationVerdict::Passed)
            {
                QPDF_LARGE_MEMORY_LIMIT
            } else {
                WORKER_MEMORY_LIMIT
            },
        )?;
        #[cfg(windows)]
        if structural_check.status == StructuralCheckStatus::Failed {
            if let Ok(mut checked) = validation {
                checked.qpdf_package_hash = structural_check.package_hash;
                checked.structural_check_status = StructuralCheckStatus::Failed;
                checked.isolation_status = ValidatorIsolationStatus::ReleaseProven;
                checked.verdict = TrustedValidationVerdict::Failed;
                checked.findings.extend(structural_check.findings);
                return Ok(checked);
            }
            return Ok(TrustedValidation {
                artifact_hash: Hash32::of(candidate_bytes),
                evaluation_hash: Hash32(evaluation_hash),
                capsule_hash: Hash32(capsule_hash),
                profile_hash: Hash32(profile_hash),
                rules_hash: Hash32(expected_rules_identity_hash()),
                source_hash: Hash32(expected_source_identity_hash()),
                tool_identity_hash: tool_identity_hash(),
                worker_executable_hash: Hash32([0; 32]),
                qpdf_package_hash: structural_check.package_hash,
                active_icc_source_hash: Hash32([0; 32]),
                active_icc_rules_hash: Hash32([0; 32]),
                public_core_source_hash: Hash32([0; 32]),
                public_core_rules_hash: Hash32([0; 32]),
                rule_coverage_status: manifest_rule_coverage_status(),
                isolation_status: ValidatorIsolationStatus::ReleaseProven,
                structural_check_status: structural_check.status,
                verdict: TrustedValidationVerdict::Failed,
                findings: structural_check.findings,
            });
        }
        let validation = validation?;
        #[cfg(windows)]
        let validation = {
            let mut validation = validation;
            validation.qpdf_package_hash = structural_check.package_hash;
            validation.structural_check_status = structural_check.status;
            validation.isolation_status = ValidatorIsolationStatus::ReleaseProven;
            validation
        };
        Ok(validation)
    }
}

#[derive(Clone, Copy, Debug)]
struct ValidatorWorkerRequest<'a> {
    candidate_bytes: &'a [u8],
    expected_artifact_hash: Hash32,
    evaluation_hash: Hash32,
    capsule_hash: Hash32,
    profile_hash: Hash32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustedValidationVerdict {
    Passed,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidatorRuleCoverageStatus {
    Incomplete,
    ReleaseProven,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum ManifestCoverageStatus {
    Incomplete,
    ReleaseProven,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum RuleImplementationStatus {
    FixtureProven,
    Partial,
    Planned,
    NormativePending,
    InventoryAvailable,
    NotApplicableProven,
}

#[derive(Debug, Deserialize)]
struct AuthorityManifest {
    schema_version: u16,
    authority_id: String,
    standard: String,
    coverage_status: ManifestCoverageStatus,
    sources: Vec<ManifestSource>,
    rules: Vec<ManifestRule>,
}

#[derive(Debug, Deserialize)]
struct ManifestSource {
    source_id: String,
    role: String,
    url: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct ManifestRule {
    rule_id: String,
    source_id: String,
    requirement_summary: String,
    severity: String,
    evaluator: String,
    implementation_status: RuleImplementationStatus,
    fixtures: Vec<String>,
}

fn manifest_rule_coverage_status() -> ValidatorRuleCoverageStatus {
    let Ok(manifest) = serde_json::from_slice::<AuthorityManifest>(RULES_MANIFEST) else {
        return ValidatorRuleCoverageStatus::Incomplete;
    };
    let mut source_ids = HashSet::with_capacity(manifest.sources.len());
    let sources_are_valid = manifest.sources.iter().all(|source| {
        !source.source_id.is_empty()
            && !source.role.is_empty()
            && !source.url.is_empty()
            && !source.status.is_empty()
            && source_ids.insert(source.source_id.as_str())
    });
    let mut rule_ids = HashSet::with_capacity(manifest.rules.len());
    let rules_are_valid = manifest.rules.iter().all(|rule| {
        !rule.rule_id.is_empty()
            && !rule.requirement_summary.is_empty()
            && !rule.severity.is_empty()
            && !rule.evaluator.is_empty()
            && rule.fixtures.iter().all(|fixture| !fixture.is_empty())
            && source_ids.contains(rule.source_id.as_str())
            && rule_ids.insert(rule.rule_id.as_str())
    });
    let every_rule_is_proven = manifest.rules.iter().all(|rule| {
        matches!(
            rule.implementation_status,
            RuleImplementationStatus::FixtureProven | RuleImplementationStatus::NotApplicableProven
        )
    });
    if manifest.schema_version == 1
        && manifest.authority_id == AUTHORITY_ID
        && manifest.standard == "PDF/X-4"
        && manifest.coverage_status == ManifestCoverageStatus::ReleaseProven
        && !manifest.sources.is_empty()
        && !manifest.rules.is_empty()
        && sources_are_valid
        && rules_are_valid
        && every_rule_is_proven
    {
        ValidatorRuleCoverageStatus::ReleaseProven
    } else {
        ValidatorRuleCoverageStatus::Incomplete
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidatorIsolationStatus {
    /// The worker has bounded time, memory, input, and output, but no proven
    /// filesystem or network sandbox.
    ProcessLimitsOnly,
    ReleaseProven,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructuralCheckStatus {
    NotRunOnUnsupportedHost,
    Passed,
    Failed,
}

/// Opaque proof that exact candidate bytes crossed the pinned packaged validator.
///
/// This proves execution and identity binding, not complete PDF/X rule coverage.
/// Callers must inspect the evidence's isolation status rather than infer it
/// from successful worker execution.
///
/// ```compile_fail
/// use validator_boundary::TrustedValidation;
/// let forged = TrustedValidation { findings: Vec::new() };
/// ```
#[derive(Debug, Eq, PartialEq)]
pub struct TrustedValidation {
    artifact_hash: Hash32,
    evaluation_hash: Hash32,
    capsule_hash: Hash32,
    profile_hash: Hash32,
    rules_hash: Hash32,
    source_hash: Hash32,
    tool_identity_hash: Hash32,
    worker_executable_hash: Hash32,
    qpdf_package_hash: Hash32,
    active_icc_source_hash: Hash32,
    active_icc_rules_hash: Hash32,
    public_core_source_hash: Hash32,
    public_core_rules_hash: Hash32,
    structural_check_status: StructuralCheckStatus,
    rule_coverage_status: ValidatorRuleCoverageStatus,
    isolation_status: ValidatorIsolationStatus,
    verdict: TrustedValidationVerdict,
    findings: Vec<String>,
}

impl TrustedValidation {
    #[must_use]
    pub const fn artifact_hash(&self) -> [u8; 32] {
        self.artifact_hash.0
    }
    #[must_use]
    pub const fn evaluation_hash(&self) -> [u8; 32] {
        self.evaluation_hash.0
    }
    #[must_use]
    pub const fn capsule_hash(&self) -> [u8; 32] {
        self.capsule_hash.0
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
    pub const fn source_hash(&self) -> [u8; 32] {
        self.source_hash.0
    }
    #[must_use]
    pub const fn tool_identity_hash(&self) -> [u8; 32] {
        self.tool_identity_hash.0
    }
    #[must_use]
    pub const fn worker_executable_hash(&self) -> [u8; 32] {
        self.worker_executable_hash.0
    }
    #[must_use]
    pub const fn qpdf_package_hash(&self) -> [u8; 32] {
        self.qpdf_package_hash.0
    }
    #[must_use]
    pub const fn active_icc_source_hash(&self) -> [u8; 32] {
        self.active_icc_source_hash.0
    }
    #[must_use]
    pub const fn active_icc_rules_hash(&self) -> [u8; 32] {
        self.active_icc_rules_hash.0
    }

    #[must_use]
    pub const fn public_core_source_hash(&self) -> [u8; 32] {
        self.public_core_source_hash.0
    }

    #[must_use]
    pub const fn public_core_rules_hash(&self) -> [u8; 32] {
        self.public_core_rules_hash.0
    }
    #[must_use]
    pub const fn structural_check_status(&self) -> StructuralCheckStatus {
        self.structural_check_status
    }
    #[must_use]
    pub const fn rule_coverage_status(&self) -> ValidatorRuleCoverageStatus {
        self.rule_coverage_status
    }
    #[must_use]
    pub const fn isolation_status(&self) -> ValidatorIsolationStatus {
        self.isolation_status
    }
    #[must_use]
    pub const fn verdict(&self) -> TrustedValidationVerdict {
        self.verdict
    }
    #[must_use]
    pub fn findings(&self) -> &[String] {
        &self.findings
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidatorWorkerError {
    NotInstalled,
    InvalidExecutable(String),
    ExecutableIdentityDrift,
    WorkspaceUnavailable(String),
    CandidateUnavailable(String),
    CandidateTooLarge,
    RequestTooLarge,
    SpawnFailed(String),
    ProcessLimitSetupFailed(String),
    InputUnavailable,
    InputFailed(String),
    WaitFailed(String),
    TimedOut,
    OutputUnavailable,
    OutputFailed(String),
    OutputLimitExceeded,
    WorkerRejected { code: String, detail: String },
    MalformedReport(String),
    ProtocolMismatch,
    ArtifactIdentityDrift,
    EvaluationIdentityDrift,
    CapsuleIdentityDrift,
    ValidatorIdentityDrift(String),
    StructuralCheckerUnavailable(String),
    StructuralCheckerIdentityDrift,
    StructuralCheckerTimedOut,
    StructuralCheckerOutputLimitExceeded,
}

#[derive(Serialize)]
struct WireRequest {
    protocol_version: u16,
    expected_artifact_sha256: String,
    evaluation_sha256: String,
    capsule_sha256: String,
    profile_sha256: String,
    standard: &'static str,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReport {
    protocol_version: u16,
    artifact_sha256: String,
    evaluation_sha256: String,
    capsule_sha256: String,
    verdict: WireVerdict,
    findings: Vec<String>,
    validator: WireIdentity,
    authority_components: Vec<WireIdentity>,
    diagnostic_components: Vec<WireIdentity>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WireVerdict {
    Passed,
    Failed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireIdentity {
    name: String,
    version: String,
    source_sha256: String,
    rules_sha256: String,
    profile_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireFailure {
    protocol_version: u16,
    error: String,
    detail: String,
}

fn resolve_validator_worker() -> Result<WorkerBinding, ValidatorWorkerError> {
    if !cfg!(windows) {
        return Err(ValidatorWorkerError::ProcessLimitSetupFailed(
            "validator confinement is unsupported on this platform".into(),
        ));
    }
    let executable = std::env::current_exe()
        .map_err(|error| ValidatorWorkerError::InvalidExecutable(error.to_string()))?;
    let worker_name = if cfg!(windows) {
        "publisher-validator-worker.exe"
    } else {
        "publisher-validator-worker"
    };
    let sibling = executable.with_file_name(worker_name);
    if sibling.exists() {
        return if cfg!(debug_assertions) {
            bind_worker_with_limit(&sibling, 512 * 1024 * 1024)
        } else {
            bind_worker(&sibling)
        };
    }
    if cfg!(debug_assertions) {
        if let Some(development) = std::env::var_os("PUBLISHER_VALIDATOR_WORKER_PATH") {
            let development = PathBuf::from(development);
            if development.exists() {
                return bind_worker_with_limit(&development, 512 * 1024 * 1024);
            }
        }
        let development = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug")
            .join(worker_name);
        if development.exists() {
            return bind_worker_with_limit(&development, 512 * 1024 * 1024);
        }
    }
    Err(ValidatorWorkerError::NotInstalled)
}

fn bind_worker(path: &Path) -> Result<WorkerBinding, ValidatorWorkerError> {
    bind_worker_with_limit(path, MAX_WORKER_BYTES)
}

fn bind_worker_with_limit(
    path: &Path,
    byte_limit: u64,
) -> Result<WorkerBinding, ValidatorWorkerError> {
    let metadata = path
        .symlink_metadata()
        .map_err(|error| ValidatorWorkerError::InvalidExecutable(error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > byte_limit {
        return Err(ValidatorWorkerError::InvalidExecutable(
            "validator worker must be a non-empty regular file no larger than 64 MiB".into(),
        ));
    }
    let bytes =
        read_bounded_file(path, byte_limit).map_err(ValidatorWorkerError::InvalidExecutable)?;
    Ok(WorkerBinding {
        path: path.to_path_buf(),
        executable_hash: Hash32::of(&bytes),
        byte_limit,
    })
}

#[allow(clippy::too_many_lines)]
fn run_validator_worker(
    binding: &WorkerBinding,
    request: ValidatorWorkerRequest<'_>,
    timeout: Duration,
) -> Result<TrustedValidation, ValidatorWorkerError> {
    validate_candidate_bytes(request.candidate_bytes, request.expected_artifact_hash)?;
    let prepared_worker = prepare_verified_worker(binding)?;
    let input = serde_json::to_vec(&WireRequest {
        protocol_version: PROTOCOL_VERSION,
        expected_artifact_sha256: request.expected_artifact_hash.hex(),
        evaluation_sha256: request.evaluation_hash.hex(),
        capsule_sha256: request.capsule_hash.hex(),
        profile_sha256: request.profile_hash.hex(),
        standard: "pdf-x4",
    })
    .map_err(|error| ValidatorWorkerError::MalformedReport(error.to_string()))?;
    if input.len() > MAX_REQUEST_BYTES {
        return Err(ValidatorWorkerError::RequestTooLarge);
    }

    let mut child = RunningProcess::spawn(
        &prepared_worker.path,
        &[],
        &prepared_worker.directory,
        &[&prepared_worker.path],
        timeout,
        WORKER_MEMORY_LIMIT,
    )
    .map_err(ValidatorWorkerError::ProcessLimitSetupFailed)?;
    let Some(stdout) = child.take_stdout() else {
        child.terminate();
        return Err(ValidatorWorkerError::OutputUnavailable);
    };
    let Some(stderr) = child.take_stderr() else {
        child.terminate();
        return Err(ValidatorWorkerError::OutputUnavailable);
    };
    let output_reader = thread::spawn(move || {
        let mut output = Vec::new();
        stdout
            .take(MAX_OUTPUT_BYTES + 1)
            .read_to_end(&mut output)
            .map(|_| output)
            .map_err(|error| error.to_string())
    });
    let stderr_reader = thread::spawn(move || read_worker_stderr_bounded(stderr, MAX_OUTPUT_BYTES));
    let Some(stdin) = child.take_stdin() else {
        child.terminate();
        let _ignored = output_reader.join();
        let _ignored = stderr_reader.join();
        return Err(ValidatorWorkerError::InputUnavailable);
    };
    let deadline = Instant::now() + timeout;
    let status = thread::scope(|scope| {
        let input_writer = scope.spawn(move || {
            let mut stdin = stdin;
            stdin
                .write_all(&input)
                .and_then(|()| stdin.write_all(b"\n"))
                .and_then(|()| stdin.write_all(request.candidate_bytes))
                .map_err(|error| error.to_string())
        });
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    child.terminate();
                    let _ignored = input_writer.join();
                    return Err(ValidatorWorkerError::TimedOut);
                }
                Err(error) => {
                    child.terminate();
                    let _ignored = input_writer.join();
                    return Err(ValidatorWorkerError::WaitFailed(error.to_string()));
                }
            }
        };
        input_writer
            .join()
            .map_err(|_| ValidatorWorkerError::InputFailed("input writer panicked".into()))?
            .map_err(ValidatorWorkerError::InputFailed)?;
        Ok(status)
    });
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            let _ignored = output_reader.join();
            let _ignored = stderr_reader.join();
            return Err(error);
        }
    };

    let output = output_reader
        .join()
        .map_err(|_| ValidatorWorkerError::OutputFailed("output reader panicked".into()))?
        .map_err(ValidatorWorkerError::OutputFailed)?;
    stderr_reader
        .join()
        .map_err(|_| ValidatorWorkerError::OutputFailed("stderr reader panicked".into()))??;
    if output.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(ValidatorWorkerError::OutputLimitExceeded);
    }
    if !status.success() {
        let failure = serde_json::from_slice::<WireFailure>(&output).map_err(|error| {
            ValidatorWorkerError::MalformedReport(format!(
                "validator worker failed without a valid bounded report: {error}"
            ))
        })?;
        if failure.protocol_version != PROTOCOL_VERSION {
            return Err(ValidatorWorkerError::ProtocolMismatch);
        }
        return Err(ValidatorWorkerError::WorkerRejected {
            code: failure.error,
            detail: failure.detail,
        });
    }
    parse_worker_report(&output, request, prepared_worker.executable_hash)
}

fn read_worker_stderr_bounded(
    pipe: impl Read,
    limit: u64,
) -> Result<Vec<u8>, ValidatorWorkerError> {
    let mut output = Vec::new();
    pipe.take(limit + 1)
        .read_to_end(&mut output)
        .map_err(|error| ValidatorWorkerError::OutputFailed(error.to_string()))?;
    if output.len() as u64 > limit {
        return Err(ValidatorWorkerError::OutputLimitExceeded);
    }
    Ok(output)
}

fn validate_candidate_bytes(
    bytes: &[u8],
    expected_hash: Hash32,
) -> Result<(), ValidatorWorkerError> {
    if bytes.is_empty() {
        return Err(ValidatorWorkerError::CandidateUnavailable(
            "validation candidate is empty".into(),
        ));
    }
    if bytes.len() as u64 > MAX_CANDIDATE_BYTES {
        return Err(ValidatorWorkerError::CandidateTooLarge);
    }
    if Hash32::of(bytes) != expected_hash {
        return Err(ValidatorWorkerError::ArtifactIdentityDrift);
    }
    Ok(())
}

fn parse_worker_report(
    output: &[u8],
    request: ValidatorWorkerRequest<'_>,
    executable_hash: Hash32,
) -> Result<TrustedValidation, ValidatorWorkerError> {
    let report: WireReport = serde_json::from_slice(output)
        .map_err(|error| ValidatorWorkerError::MalformedReport(error.to_string()))?;
    if report.protocol_version != PROTOCOL_VERSION {
        return Err(ValidatorWorkerError::ProtocolMismatch);
    }
    if report.artifact_sha256 != request.expected_artifact_hash.hex() {
        return Err(ValidatorWorkerError::ArtifactIdentityDrift);
    }
    if report.evaluation_sha256 != request.evaluation_hash.hex() {
        return Err(ValidatorWorkerError::EvaluationIdentityDrift);
    }
    if report.capsule_sha256 != request.capsule_hash.hex() {
        return Err(ValidatorWorkerError::CapsuleIdentityDrift);
    }
    if report.findings.len() > MAX_FINDINGS {
        return Err(ValidatorWorkerError::OutputLimitExceeded);
    }
    let expected_source = Hash32(expected_source_identity_hash());
    let expected_rules = Hash32(expected_rules_identity_hash());
    if report.validator.name != VALIDATOR_NAME
        || report.validator.version != VALIDATOR_VERSION
        || report.validator.source_sha256 != expected_source.hex()
        || report.validator.rules_sha256 != expected_rules.hex()
        || report.validator.profile_sha256 != request.profile_hash.hex()
    {
        return Err(ValidatorWorkerError::ValidatorIdentityDrift(
            "validator name, version, rules, source, or profile identity changed".into(),
        ));
    }
    let expected_icc_source = Hash32::of(ACTIVE_ICC_SOURCE_IDENTITY.as_bytes());
    let expected_icc_rules = Hash32::of(ACTIVE_ICC_RULES_IDENTITY.as_bytes());
    let [active_icc, public_core] = report.authority_components.as_slice() else {
        return Err(ValidatorWorkerError::ValidatorIdentityDrift(
            "validator report lacks pinned authority components".into(),
        ));
    };
    if active_icc.name != ACTIVE_ICC_NAME
        || active_icc.version != ACTIVE_ICC_VERSION
        || active_icc.source_sha256 != expected_icc_source.hex()
        || active_icc.rules_sha256 != expected_icc_rules.hex()
        || active_icc.profile_sha256 != request.profile_hash.hex()
    {
        return Err(ValidatorWorkerError::ValidatorIdentityDrift(
            "active-color component identity changed".into(),
        ));
    }
    let expected_public_core_source = Hash32::of(PUBLIC_CORE_SOURCE_IDENTITY.as_bytes());
    let expected_public_core_rules = Hash32::of(PUBLIC_CORE_RULES_IDENTITY.as_bytes());
    if public_core.name != PUBLIC_CORE_NAME
        || public_core.version != PUBLIC_CORE_VERSION
        || public_core.source_sha256 != expected_public_core_source.hex()
        || public_core.rules_sha256 != expected_public_core_rules.hex()
        || public_core.profile_sha256 != request.profile_hash.hex()
    {
        return Err(ValidatorWorkerError::ValidatorIdentityDrift(
            "public-core component name, version, rules, source, or profile identity changed"
                .into(),
        ));
    }
    let expected_diagnostic_source = Hash32::of(DIAGNOSTIC_SOURCE_IDENTITY.as_bytes());
    let expected_diagnostic_rules = Hash32::of(DIAGNOSTIC_RULES_IDENTITY.as_bytes());
    let [diagnostic] = report.diagnostic_components.as_slice() else {
        return Err(ValidatorWorkerError::ValidatorIdentityDrift(
            "validator report must identify exactly one pinned diagnostic component".into(),
        ));
    };
    if diagnostic.name != DIAGNOSTIC_NAME
        || diagnostic.version != DIAGNOSTIC_VERSION
        || diagnostic.source_sha256 != expected_diagnostic_source.hex()
        || diagnostic.rules_sha256 != expected_diagnostic_rules.hex()
        || diagnostic.profile_sha256 != request.profile_hash.hex()
    {
        return Err(ValidatorWorkerError::ValidatorIdentityDrift(
            "diagnostic component name, version, rules, source, or profile identity changed".into(),
        ));
    }
    Ok(TrustedValidation {
        artifact_hash: request.expected_artifact_hash,
        evaluation_hash: request.evaluation_hash,
        capsule_hash: request.capsule_hash,
        profile_hash: request.profile_hash,
        rules_hash: expected_rules,
        source_hash: expected_source,
        tool_identity_hash: tool_identity_hash(),
        rule_coverage_status: manifest_rule_coverage_status(),
        isolation_status: ValidatorIsolationStatus::ProcessLimitsOnly,
        verdict: match report.verdict {
            WireVerdict::Passed => TrustedValidationVerdict::Passed,
            WireVerdict::Failed => TrustedValidationVerdict::Failed,
        },
        findings: report.findings,
        worker_executable_hash: executable_hash,
        qpdf_package_hash: Hash32([0; 32]),
        active_icc_source_hash: expected_icc_source,
        active_icc_rules_hash: expected_icc_rules,
        public_core_source_hash: expected_public_core_source,
        public_core_rules_hash: expected_public_core_rules,
        structural_check_status: StructuralCheckStatus::NotRunOnUnsupportedHost,
    })
}

#[cfg(windows)]
#[derive(Clone, Copy)]
struct QpdfRuntimeFile {
    name: &'static str,
    bytes: u64,
    sha256: &'static str,
}

#[cfg(windows)]
const QPDF_RUNTIME_FILES: &[QpdfRuntimeFile] = &[
    QpdfRuntimeFile {
        name: "concrt140.dll",
        bytes: 374_200,
        sha256: "54716F0738AF891F283D213B5C8D11B25896BB8EE3097D301EAE718560CF974E",
    },
    QpdfRuntimeFile {
        name: "msvcp140_1.dll",
        bytes: 35_768,
        sha256: "206C931BF90FDAD8816DE3B5E2EF80B2BCAA9406C89ECC05FE6FDDFFE251E982",
    },
    QpdfRuntimeFile {
        name: "msvcp140_2.dll",
        bytes: 274_872,
        sha256: "D50D7883F20D1DC6191768D3746F52DD9CAC89C346FFAED5BE1F110C2F34A838",
    },
    QpdfRuntimeFile {
        name: "msvcp140_atomic_wait.dll",
        bytes: 57_792,
        sha256: "3D0CBFAA1BF3EECF5A3F4491D2960EE803CB994F30292C6ADC4A07C498F60E2B",
    },
    QpdfRuntimeFile {
        name: "msvcp140_codecvt_ids.dll",
        bytes: 31_160,
        sha256: "8A65C7596EF2E6938731F5A1058E7E40145B6D97967CC649231A076B9A608D78",
    },
    QpdfRuntimeFile {
        name: "msvcp140.dll",
        bytes: 643_512,
        sha256: "7C26614E1D733892C2DEAC7E245CE115504B1D80592DD0A01B08E3E5A55F89CA",
    },
    QpdfRuntimeFile {
        name: "qpdf.exe",
        bytes: 19_456,
        sha256: "57C003E868FB66CD343FD5AFB91BE8C2277F56434EEA8635762499731BF9F60D",
    },
    QpdfRuntimeFile {
        name: "qpdf30.dll",
        bytes: 7_438_848,
        sha256: "36FE5B2023F244E5785D96B8372DBA26B75EA51B63ADF4D4F1E66AD0AA1C8A61",
    },
    QpdfRuntimeFile {
        name: "vcruntime140_1.dll",
        bytes: 50_112,
        sha256: "A7146C08F89FE5B04541AB507CDB59FF7B44534D4BA3C668A426C6450A03434E",
    },
    QpdfRuntimeFile {
        name: "vcruntime140.dll",
        bytes: 178_616,
        sha256: "D1F4225DF2CD877DBF130D5668A021DCE3F94118455FF5EC952061C30AFC9CE7",
    },
];

#[cfg(windows)]
#[derive(Clone, Debug)]
struct QpdfBinding {
    directory: PathBuf,
    package_hash: Hash32,
}

#[cfg(windows)]
struct QpdfCheckResult {
    package_hash: Hash32,
    status: StructuralCheckStatus,
    findings: Vec<String>,
}

#[cfg(windows)]
fn resolve_qpdf_package() -> Result<QpdfBinding, ValidatorWorkerError> {
    let executable = std::env::current_exe()
        .map_err(|error| ValidatorWorkerError::StructuralCheckerUnavailable(error.to_string()))?;
    let sibling = executable
        .parent()
        .ok_or_else(|| {
            ValidatorWorkerError::StructuralCheckerUnavailable(
                "application executable has no parent directory".into(),
            )
        })?
        .to_path_buf();
    let directory = if sibling.join("qpdf.exe").is_file() {
        sibling
    } else if cfg!(debug_assertions)
        && let Some(development) = std::env::var_os("PUBLISHER_QPDF_BIN_DIR")
    {
        PathBuf::from(development)
    } else if cfg!(debug_assertions) {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.tools/qpdf/12.4.1/qpdf-12.4.1-msvc64/bin")
    } else {
        return Err(ValidatorWorkerError::StructuralCheckerUnavailable(
            "the pinned qpdf runtime is not installed beside Publisher V2".into(),
        ));
    };
    let package_hash = verify_qpdf_package(&directory)?;
    Ok(QpdfBinding {
        directory,
        package_hash,
    })
}

#[cfg(windows)]
fn verify_qpdf_package(directory: &Path) -> Result<Hash32, ValidatorWorkerError> {
    let mut digest = Sha256::new();
    digest.update(b"publisher-v2/qpdf-package/v1\0");
    digest.update(QPDF_VERSION.as_bytes());
    for specification in QPDF_RUNTIME_FILES {
        let path = directory.join(specification.name);
        let metadata = path.symlink_metadata().map_err(|error| {
            ValidatorWorkerError::StructuralCheckerUnavailable(format!(
                "{}: {error}",
                specification.name
            ))
        })?;
        if !metadata.file_type().is_file() || metadata.len() != specification.bytes {
            return Err(ValidatorWorkerError::StructuralCheckerIdentityDrift);
        }
        let bytes = read_bounded_file(&path, specification.bytes)
            .map_err(ValidatorWorkerError::StructuralCheckerUnavailable)?;
        let hash = Hash32::of(&bytes);
        if !hash.hex().eq_ignore_ascii_case(specification.sha256) {
            return Err(ValidatorWorkerError::StructuralCheckerIdentityDrift);
        }
        digest.update((specification.name.len() as u64).to_be_bytes());
        digest.update(specification.name.as_bytes());
        digest.update(hash.0);
    }
    Ok(Hash32(digest.finalize().into()))
}

#[cfg(windows)]
struct PreparedQpdf {
    directory: PathBuf,
    executable: PathBuf,
    candidate: PathBuf,
    copied_files: Vec<PathBuf>,
    package_hash: Hash32,
}

#[cfg(windows)]
impl Drop for PreparedQpdf {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_file(&self.candidate);
        for path in self.copied_files.iter().rev() {
            let _ignored = std::fs::remove_file(path);
        }
        let _ignored = std::fs::remove_dir(&self.directory);
    }
}

#[cfg(windows)]
fn prepare_qpdf(
    binding: &QpdfBinding,
    candidate_bytes: &[u8],
) -> Result<PreparedQpdf, ValidatorWorkerError> {
    if verify_qpdf_package(&binding.directory)? != binding.package_hash {
        return Err(ValidatorWorkerError::StructuralCheckerIdentityDrift);
    }
    let directory = create_private_qpdf_directory()?;
    let mut prepared = PreparedQpdf {
        executable: directory.join("qpdf.exe"),
        candidate: directory.join("candidate.pdf"),
        directory,
        copied_files: Vec::with_capacity(QPDF_RUNTIME_FILES.len()),
        package_hash: binding.package_hash,
    };
    for specification in QPDF_RUNTIME_FILES {
        let source = binding.directory.join(specification.name);
        let destination = prepared.directory.join(specification.name);
        let bytes = read_bounded_file(&source, specification.bytes)
            .map_err(ValidatorWorkerError::StructuralCheckerUnavailable)?;
        write_new_file(&destination, &bytes)?;
        prepared.copied_files.push(destination);
    }
    if verify_qpdf_package(&prepared.directory)? != binding.package_hash {
        return Err(ValidatorWorkerError::StructuralCheckerIdentityDrift);
    }
    write_new_file(&prepared.candidate, candidate_bytes)?;
    Ok(prepared)
}

#[cfg(windows)]
fn create_private_qpdf_directory() -> Result<PathBuf, ValidatorWorkerError> {
    for attempt in 0..16_u32 {
        let directory = std::env::temp_dir().join(format!(
            "publisher-qpdf-{}-{}-{attempt}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| ValidatorWorkerError::InputFailed(error.to_string()))?
                .as_nanos()
        ));
        match std::fs::create_dir(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(ValidatorWorkerError::WorkspaceUnavailable(
                    error.to_string(),
                ));
            }
        }
    }
    Err(ValidatorWorkerError::WorkspaceUnavailable(
        "could not allocate a private qpdf directory".into(),
    ))
}

#[cfg(windows)]
fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), ValidatorWorkerError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| ValidatorWorkerError::WorkspaceUnavailable(error.to_string()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| ValidatorWorkerError::WorkspaceUnavailable(error.to_string()))
}

#[cfg(windows)]
fn run_qpdf_check(
    binding: &QpdfBinding,
    candidate_bytes: &[u8],
    timeout: Duration,
    memory_limit: usize,
) -> Result<QpdfCheckResult, ValidatorWorkerError> {
    validate_candidate_bytes(candidate_bytes, Hash32::of(candidate_bytes))?;
    let prepared = prepare_qpdf(binding, candidate_bytes)?;
    let arguments = [
        OsString::from("--suppress-recovery"),
        OsString::from("--check"),
        OsString::from("candidate.pdf"),
    ];
    let mut readable = prepared
        .copied_files
        .iter()
        .map(PathBuf::as_path)
        .collect::<Vec<_>>();
    readable.push(&prepared.candidate);
    let mut child = RunningProcess::spawn(
        &prepared.executable,
        &arguments,
        &prepared.directory,
        &readable,
        timeout,
        memory_limit,
    )
    .map_err(ValidatorWorkerError::ProcessLimitSetupFailed)?;
    drop(child.take_stdin());
    let stdout = child
        .take_stdout()
        .ok_or(ValidatorWorkerError::OutputUnavailable)?;
    let stderr = child
        .take_stderr()
        .ok_or(ValidatorWorkerError::OutputUnavailable)?;
    let stdout_reader = thread::spawn(move || read_pipe_bounded(stdout, QPDF_MAX_OUTPUT_BYTES));
    let stderr_reader = thread::spawn(move || read_pipe_bounded(stderr, QPDF_MAX_OUTPUT_BYTES));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                child.terminate();
                let _ignored = stdout_reader.join();
                let _ignored = stderr_reader.join();
                return Err(ValidatorWorkerError::StructuralCheckerTimedOut);
            }
            Err(error) => {
                child.terminate();
                let _ignored = stdout_reader.join();
                let _ignored = stderr_reader.join();
                return Err(ValidatorWorkerError::WaitFailed(error.to_string()));
            }
        }
    };
    let mut output = stdout_reader
        .join()
        .map_err(|_| ValidatorWorkerError::OutputFailed("qpdf stdout reader panicked".into()))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| ValidatorWorkerError::OutputFailed("qpdf stderr reader panicked".into()))??;
    output.extend_from_slice(&stderr);
    if output.len() as u64 > QPDF_MAX_OUTPUT_BYTES {
        return Err(ValidatorWorkerError::StructuralCheckerOutputLimitExceeded);
    }
    let status = if status.success() {
        StructuralCheckStatus::Passed
    } else {
        StructuralCheckStatus::Failed
    };
    let findings = if status == StructuralCheckStatus::Failed {
        vec![format!(
            "qpdf structural check failed: {}",
            bounded_qpdf_output(&output, &prepared.directory)
        )]
    } else {
        Vec::new()
    };
    Ok(QpdfCheckResult {
        package_hash: prepared.package_hash,
        status,
        findings,
    })
}

#[cfg(windows)]
fn read_pipe_bounded(pipe: impl Read, limit: u64) -> Result<Vec<u8>, ValidatorWorkerError> {
    let mut output = Vec::new();
    pipe.take(limit + 1)
        .read_to_end(&mut output)
        .map_err(|error| ValidatorWorkerError::OutputFailed(error.to_string()))?;
    if output.len() as u64 > limit {
        return Err(ValidatorWorkerError::StructuralCheckerOutputLimitExceeded);
    }
    Ok(output)
}

#[cfg(windows)]
fn bounded_qpdf_output(output: &[u8], private_directory: &Path) -> String {
    let sanitized = redact_qpdf_paths(&String::from_utf8_lossy(output), private_directory);
    let normalized = sanitized
        .replace(['\r', '\n'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut bounded = normalized.chars().take(512).collect::<String>();
    if normalized.chars().count() > 512 {
        bounded.push_str("...");
    }
    if bounded.is_empty() {
        "qpdf returned a nonzero exit status without diagnostics".into()
    } else {
        bounded
    }
}

#[cfg(windows)]
fn redact_qpdf_paths(output: &str, private_directory: &Path) -> String {
    let mut redacted_directory = output.to_owned();
    for candidate in private_path_candidates(&private_directory.to_string_lossy()) {
        redacted_directory = replace_normalized_ascii_matches(
            &redacted_directory,
            &candidate,
            "private validation workspace",
        );
    }
    replace_normalized_ascii_matches(&redacted_directory, "candidate.pdf", "candidate bytes")
}

#[cfg(windows)]
fn normalized_ascii_path(value: &str) -> String {
    String::from_utf8(
        value
            .bytes()
            .map(|byte| match byte {
                b'\\' => b'/',
                other => other.to_ascii_lowercase(),
            })
            .collect(),
    )
    .expect("ASCII path normalization preserves UTF-8")
}

#[cfg(windows)]
fn private_path_candidates(private_directory: &str) -> Vec<String> {
    let normalized = normalized_ascii_path(private_directory);
    let ordinary = if let Some(unc) = normalized.strip_prefix("//?/unc/") {
        format!("//{unc}")
    } else if let Some(drive) = normalized.strip_prefix("//?/") {
        drive.to_owned()
    } else {
        normalized
    };
    let extended = if let Some(unc) = ordinary.strip_prefix("//") {
        Some(format!("//?/unc/{unc}"))
    } else if ordinary.as_bytes().get(1) == Some(&b':') {
        Some(format!("//?/{ordinary}"))
    } else {
        None
    };
    extended
        .into_iter()
        .chain(std::iter::once(ordinary))
        .collect()
}

#[cfg(windows)]
fn replace_normalized_ascii_matches(value: &str, needle: &str, replacement: &str) -> String {
    let shadow = normalized_ascii_path(value).into_bytes();
    let normalized_needle = normalized_ascii_path(needle).into_bytes();
    if normalized_needle.is_empty() || normalized_needle.len() > shadow.len() {
        return value.to_owned();
    }

    let mut ranges = Vec::new();
    let mut cursor = 0;
    while cursor + normalized_needle.len() <= shadow.len() {
        let Some(relative) = shadow[cursor..]
            .windows(normalized_needle.len())
            .position(|candidate| candidate == normalized_needle)
        else {
            break;
        };
        let start = cursor + relative;
        let end = start + normalized_needle.len();
        ranges.push((start, end));
        cursor = end;
    }
    if ranges.is_empty() {
        return value.to_owned();
    }

    let mut redacted = String::with_capacity(value.len());
    let mut copied_through = 0;
    for (start, end) in ranges {
        redacted.push_str(&value[copied_through..start]);
        redacted.push_str(replacement);
        copied_through = end;
    }
    redacted.push_str(&value[copied_through..]);
    redacted
}

struct PreparedWorker {
    path: PathBuf,
    directory: PathBuf,
    executable_hash: Hash32,
    identity_lock: Option<File>,
}

impl Drop for PreparedWorker {
    fn drop(&mut self) {
        drop(self.identity_lock.take());
        let _ignored = std::fs::remove_file(&self.path);
        let _ignored = std::fs::remove_dir(&self.directory);
    }
}

fn prepare_verified_worker(
    binding: &WorkerBinding,
) -> Result<PreparedWorker, ValidatorWorkerError> {
    let source_bytes = read_bounded_file(&binding.path, binding.byte_limit)
        .map_err(ValidatorWorkerError::InvalidExecutable)?;
    if Hash32::of(&source_bytes) != binding.executable_hash {
        return Err(ValidatorWorkerError::ExecutableIdentityDrift);
    }
    let temp = std::env::temp_dir();
    for attempt in 0..16_u32 {
        let directory = temp.join(format!(
            "publisher-validator-{}-{}-{attempt}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| ValidatorWorkerError::InputFailed(error.to_string()))?
                .as_nanos()
        ));
        match std::fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ValidatorWorkerError::WorkspaceUnavailable(
                    error.to_string(),
                ));
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| ValidatorWorkerError::WorkspaceUnavailable(error.to_string()))?;
        }
        let path = directory.join(if cfg!(windows) {
            "publisher-validator-worker.exe"
        } else {
            "publisher-validator-worker"
        });
        let mut writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| ValidatorWorkerError::WorkspaceUnavailable(error.to_string()))?;
        writer
            .write_all(&source_bytes)
            .and_then(|()| writer.sync_all())
            .map_err(|error| ValidatorWorkerError::WorkspaceUnavailable(error.to_string()))?;
        drop(writer);
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_READ: u32 = 1;
            options.share_mode(FILE_SHARE_READ);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500))
                .map_err(|error| ValidatorWorkerError::WorkspaceUnavailable(error.to_string()))?;
        }
        let mut identity_lock = options
            .open(&path)
            .map_err(|error| ValidatorWorkerError::WorkspaceUnavailable(error.to_string()))?;
        identity_lock
            .rewind()
            .map_err(|error| ValidatorWorkerError::WorkspaceUnavailable(error.to_string()))?;
        let mut exact_bytes = Vec::new();
        Read::by_ref(&mut identity_lock)
            .take(binding.byte_limit + 1)
            .read_to_end(&mut exact_bytes)
            .map_err(|error| ValidatorWorkerError::WorkspaceUnavailable(error.to_string()))?;
        let executable_hash = Hash32::of(&exact_bytes);
        if exact_bytes.len() as u64 > binding.byte_limit
            || executable_hash != binding.executable_hash
        {
            return Err(ValidatorWorkerError::ExecutableIdentityDrift);
        }
        return Ok(PreparedWorker {
            path,
            directory,
            executable_hash,
            identity_lock: Some(identity_lock),
        });
    }
    Err(ValidatorWorkerError::WorkspaceUnavailable(
        "could not allocate a private validator directory".into(),
    ))
}

fn read_bounded_file(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("file exceeded its configured byte limit".into());
    }
    Ok(bytes)
}

struct RunningProcess {
    #[cfg(windows)]
    inner: publisher_worker_job::SandboxedProcess,
    #[cfg(not(windows))]
    inner: std::process::Child,
}

impl RunningProcess {
    fn spawn(
        executable: &Path,
        arguments: &[OsString],
        current_directory: &Path,
        readable_paths: &[&Path],
        timeout: Duration,
        memory_limit: usize,
    ) -> Result<Self, String> {
        #[cfg(windows)]
        {
            let profile =
                publisher_worker_job::AppContainerProfile::create_unique("PublisherV2.Validator")
                    .map_err(|error| error.to_string())?;
            profile
                .grant_read_execute(current_directory)
                .map_err(|error| error.to_string())?;
            for path in readable_paths {
                profile
                    .grant_read_execute(path)
                    .map_err(|error| error.to_string())?;
            }
            let inner = profile
                .spawn(
                    executable,
                    arguments,
                    current_directory,
                    publisher_worker_job::JobLimits {
                        process_memory_bytes: memory_limit,
                        job_memory_bytes: memory_limit,
                        cpu_time: timeout,
                    },
                )
                .map_err(|error| error.to_string())?;
            Ok(Self { inner })
        }
        #[cfg(not(windows))]
        {
            let _ = (
                executable,
                arguments,
                current_directory,
                readable_paths,
                timeout,
                memory_limit,
            );
            Err("validator confinement is unsupported on this platform".into())
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

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        #[cfg(windows)]
        {
            self.inner
                .take_stderr()
                .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>)
        }
        #[cfg(not(windows))]
        {
            self.inner
                .stderr
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
    use super::{
        ACTIVE_ICC_NAME, ACTIVE_ICC_RULES_IDENTITY, ACTIVE_ICC_SOURCE_IDENTITY, ACTIVE_ICC_VERSION,
        DIAGNOSTIC_NAME, DIAGNOSTIC_RULES_IDENTITY, DIAGNOSTIC_SOURCE_IDENTITY, DIAGNOSTIC_VERSION,
        PROTOCOL_VERSION, PUBLIC_CORE_NAME, PUBLIC_CORE_RULES_IDENTITY,
        PUBLIC_CORE_SOURCE_IDENTITY, PUBLIC_CORE_VERSION, SOURCE_IDENTITY, VALIDATOR_NAME,
        VALIDATOR_VERSION, ValidatorWorkerError, ValidatorWorkerRequest, bind_worker,
        bind_worker_with_limit, expected_rules_identity_hash, manifest_rule_coverage_status,
        parse_worker_report, prepare_verified_worker, run_validator_worker,
    };
    use super::{Hash32, TrustedValidationVerdict, ValidatorRuleCoverageStatus};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::{fs, time::Duration, time::SystemTime};

    #[cfg(windows)]
    #[test]
    fn qpdf_diagnostics_redact_windows_path_variants_and_keep_useful_detail() {
        use super::bounded_qpdf_output;
        use std::path::Path;

        let private = Path::new(r"C:\Users\TestUser\AppData\Local\Temp\publisher-qpdf-123");
        for exposed in [
            r"C:\Users\TestUser\AppData\Local\Temp\publisher-qpdf-123\candidate.pdf",
            r"C:/Users/TestUser/AppData/Local/Temp/publisher-qpdf-123/CANDIDATE.PDF",
            r"C:\Users/TestUser\AppData/Local\Temp/publisher-qpdf-123\candidate.Pdf",
            r"c:\uSeRs\tEsTuSeR\aPpDaTa\lOcAl\tEmP\PuBlIsHeR-QpDf-123\CaNdIdAtE.pDf",
            r"\\?\C:\Users\TestUser\AppData\Local\Temp\publisher-qpdf-123\candidate.pdf",
        ] {
            let output = format!("qpdf: {exposed}: damaged trailer");
            let redacted = bounded_qpdf_output(output.as_bytes(), private);
            assert!(redacted.contains("private validation workspace"));
            assert!(redacted.contains("candidate bytes"));
            assert!(redacted.contains("damaged trailer"));
            assert!(!redacted.to_ascii_lowercase().contains("publisher-qpdf-"));
            assert!(!redacted.to_ascii_lowercase().contains("candidate.pdf"));
            assert!(!redacted.contains("//?/"));
            assert!(!redacted.contains(r"\\?\"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn qpdf_diagnostic_redaction_preserves_the_output_cap() {
        use super::bounded_qpdf_output;
        use std::path::Path;

        let private = Path::new(r"C:\Temp\publisher-qpdf-1");
        let output = format!(
            r"qpdf: \\?\C:\Temp\publisher-qpdf-1\candidate.pdf: damaged {}",
            "x".repeat(1_000)
        );
        let redacted = bounded_qpdf_output(output.as_bytes(), private);
        assert!(redacted.starts_with("qpdf: private validation workspace"));
        assert!(redacted.contains("candidate bytes: damaged"));
        assert!(redacted.chars().count() <= 515);
        assert!(redacted.ends_with("..."));
    }

    #[cfg(windows)]
    #[test]
    fn qpdf_diagnostics_redact_unc_and_extended_unc_variants() {
        use super::bounded_qpdf_output;
        use std::path::Path;

        let private = Path::new(r"\\server\share\publisher-qpdf-456");
        for exposed in [
            r"\\server\share\publisher-qpdf-456\candidate.pdf",
            r"//SERVER/share/PUBLISHER-QPDF-456/CANDIDATE.PDF",
            r"\\?\UNC\server\share\publisher-qpdf-456\candidate.pdf",
            r"//?/uNc/SeRvEr\ShArE/pUbLiShEr-QpDf-456\CaNdIdAtE.pDf",
        ] {
            let output = format!("qpdf: {exposed}: invalid object stream");
            let redacted = bounded_qpdf_output(output.as_bytes(), private);
            assert!(redacted.contains("private validation workspace"));
            assert!(redacted.contains("candidate bytes"));
            assert!(redacted.contains("invalid object stream"));
            let lowercase = redacted.to_ascii_lowercase();
            assert!(!lowercase.contains("publisher-qpdf-"));
            assert!(!lowercase.contains("candidate.pdf"));
            assert!(!lowercase.contains("server"));
            assert!(!lowercase.contains("share"));
            assert!(!lowercase.contains("//?/unc/"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn qpdf_diagnostics_redact_ordinary_paths_when_private_path_is_extended() {
        use super::{bounded_qpdf_output, private_path_candidates};
        use std::path::Path;

        assert_eq!(
            private_path_candidates(r"\\?\C:\Temp\publisher-qpdf-789"),
            [
                "//?/c:/temp/publisher-qpdf-789",
                "c:/temp/publisher-qpdf-789"
            ]
        );
        assert_eq!(
            private_path_candidates(r"\\?\UNC\server\share\publisher-qpdf-987"),
            [
                "//?/unc/server/share/publisher-qpdf-987",
                "//server/share/publisher-qpdf-987"
            ]
        );
        for (private, exposed) in [
            (
                Path::new(r"\\?\C:\Temp\publisher-qpdf-789"),
                r"C:\Temp\publisher-qpdf-789\candidate.pdf",
            ),
            (
                Path::new(r"\\?\UNC\server\share\publisher-qpdf-987"),
                r"\\server\share\publisher-qpdf-987\candidate.pdf",
            ),
        ] {
            let output = format!("qpdf: {exposed}: damaged cross-reference table");
            let redacted = bounded_qpdf_output(output.as_bytes(), private);
            assert!(redacted.contains("private validation workspace"));
            assert!(redacted.contains("candidate bytes"));
            assert!(redacted.contains("damaged cross-reference table"));
            let lowercase = redacted.to_ascii_lowercase();
            assert!(!lowercase.contains("publisher-qpdf-"));
            assert!(!lowercase.contains("candidate.pdf"));
            assert!(!lowercase.contains("server"));
            assert!(!lowercase.contains("share"));
            assert!(!lowercase.contains("//?/"));
        }
    }

    fn temp_file(label: &str, bytes: &[u8]) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("publisher-{label}-{nonce}"));
        fs::write(&path, bytes).expect("write temp file");
        path
    }

    fn valid_report(
        artifact: Hash32,
        evaluation: Hash32,
        capsule: Hash32,
        profile: Hash32,
    ) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "protocol_version": PROTOCOL_VERSION,
            "artifact_sha256": artifact.hex(),
            "evaluation_sha256": evaluation.hex(),
            "capsule_sha256": capsule.hex(),
            "verdict": "failed",
            "findings": ["synthetic finding"],
            "validator": {
                "name": VALIDATOR_NAME,
                "version": VALIDATOR_VERSION,
                "source_sha256": Hash32::of(SOURCE_IDENTITY.as_bytes()).hex(),
                "rules_sha256": Hash32(expected_rules_identity_hash()).hex(),
                "profile_sha256": profile.hex()
            },
            "authority_components": [
                {
                    "name": ACTIVE_ICC_NAME,
                    "version": ACTIVE_ICC_VERSION,
                    "source_sha256": Hash32::of(ACTIVE_ICC_SOURCE_IDENTITY.as_bytes()).hex(),
                    "rules_sha256": Hash32::of(ACTIVE_ICC_RULES_IDENTITY.as_bytes()).hex(),
                    "profile_sha256": profile.hex()
                },
                {
                    "name": PUBLIC_CORE_NAME,
                    "version": PUBLIC_CORE_VERSION,
                    "source_sha256": Hash32::of(PUBLIC_CORE_SOURCE_IDENTITY.as_bytes()).hex(),
                    "rules_sha256": Hash32::of(PUBLIC_CORE_RULES_IDENTITY.as_bytes()).hex(),
                    "profile_sha256": profile.hex()
                }
            ],
            "diagnostic_components": [{
                "name": DIAGNOSTIC_NAME,
                "version": DIAGNOSTIC_VERSION,
                "source_sha256": Hash32::of(DIAGNOSTIC_SOURCE_IDENTITY.as_bytes()).hex(),
                "rules_sha256": Hash32::of(DIAGNOSTIC_RULES_IDENTITY.as_bytes()).hex(),
                "profile_sha256": profile.hex()
            }]
        }))
        .expect("serialize report")
    }

    #[test]
    fn checked_in_manifest_fails_closed_until_every_rule_is_release_proven() {
        assert_eq!(
            manifest_rule_coverage_status(),
            ValidatorRuleCoverageStatus::Incomplete
        );
        assert_ne!(expected_rules_identity_hash(), [0; 32]);
    }

    #[test]
    fn executable_binding_detects_replacement_after_resolution() {
        let path = temp_file("validator-worker", b"first executable generation");
        let binding = bind_worker(&path).expect("bind worker");
        fs::write(&path, b"replacement executable generation").expect("replace worker");

        assert_eq!(
            prepare_verified_worker(&binding).map(|_| ()),
            Err(ValidatorWorkerError::ExecutableIdentityDrift)
        );
        fs::remove_file(path).expect("remove temp worker");
    }

    #[test]
    fn report_is_bound_to_artifact_profile_rules_and_source_identity() {
        let candidate = b"candidate";
        let artifact = Hash32::of(b"candidate");
        let profile = Hash32::of(b"profile");
        let evaluation = Hash32::of(b"evaluation");
        let capsule = Hash32::of(b"capsule");
        let request = ValidatorWorkerRequest {
            candidate_bytes: candidate,
            expected_artifact_hash: artifact,
            evaluation_hash: evaluation,
            capsule_hash: capsule,
            profile_hash: profile,
        };
        let executable = Hash32::of(b"worker");

        let evidence = parse_worker_report(
            &valid_report(artifact, evaluation, capsule, profile),
            request,
            executable,
        )
        .expect("valid bound report");
        assert_eq!(evidence.verdict, TrustedValidationVerdict::Failed);
        assert_eq!(evidence.worker_executable_hash, executable);
        assert_eq!(
            evidence.active_icc_source_hash,
            Hash32::of(ACTIVE_ICC_SOURCE_IDENTITY.as_bytes())
        );
        assert_eq!(
            evidence.active_icc_rules_hash,
            Hash32::of(ACTIVE_ICC_RULES_IDENTITY.as_bytes())
        );
        assert_eq!(
            evidence.public_core_source_hash,
            Hash32::of(PUBLIC_CORE_SOURCE_IDENTITY.as_bytes())
        );
        assert_eq!(
            evidence.public_core_rules_hash,
            Hash32::of(PUBLIC_CORE_RULES_IDENTITY.as_bytes())
        );

        let drifted = valid_report(Hash32::of(b"other"), evaluation, capsule, profile);
        assert_eq!(
            parse_worker_report(&drifted, request, executable),
            Err(ValidatorWorkerError::ArtifactIdentityDrift)
        );
        let evaluation_drifted =
            valid_report(artifact, Hash32::of(b"other evaluation"), capsule, profile);
        assert_eq!(
            parse_worker_report(&evaluation_drifted, request, executable),
            Err(ValidatorWorkerError::EvaluationIdentityDrift)
        );
        let capsule_drifted =
            valid_report(artifact, evaluation, Hash32::of(b"other capsule"), profile);
        assert_eq!(
            parse_worker_report(&capsule_drifted, request, executable),
            Err(ValidatorWorkerError::CapsuleIdentityDrift)
        );
    }

    #[test]
    fn diagnostic_component_identity_drift_is_rejected() {
        let artifact = Hash32::of(b"candidate");
        let evaluation = Hash32::of(b"evaluation");
        let capsule = Hash32::of(b"capsule");
        let profile = Hash32::of(b"profile");
        let request = ValidatorWorkerRequest {
            candidate_bytes: b"candidate",
            expected_artifact_hash: artifact,
            evaluation_hash: evaluation,
            capsule_hash: capsule,
            profile_hash: profile,
        };
        let mut report: serde_json::Value =
            serde_json::from_slice(&valid_report(artifact, evaluation, capsule, profile))
                .expect("valid report JSON");
        report["diagnostic_components"][0]["name"] = "substituted parser".into();
        let bytes = serde_json::to_vec(&report).expect("serialize drifted report");

        assert!(matches!(
            parse_worker_report(&bytes, request, Hash32::of(b"worker")),
            Err(ValidatorWorkerError::ValidatorIdentityDrift(_))
        ));
    }

    #[test]
    fn active_icc_component_identity_drift_is_rejected() {
        let artifact = Hash32::of(b"candidate");
        let evaluation = Hash32::of(b"evaluation");
        let capsule = Hash32::of(b"capsule");
        let profile = Hash32::of(b"profile");
        let request = ValidatorWorkerRequest {
            candidate_bytes: b"candidate",
            expected_artifact_hash: artifact,
            evaluation_hash: evaluation,
            capsule_hash: capsule,
            profile_hash: profile,
        };
        let mut report: serde_json::Value =
            serde_json::from_slice(&valid_report(artifact, evaluation, capsule, profile))
                .expect("valid report JSON");
        report["authority_components"][0]["rules_sha256"] = "substituted rules".into();
        let bytes = serde_json::to_vec(&report).expect("serialize drifted report");

        assert!(matches!(
            parse_worker_report(&bytes, request, Hash32::of(b"worker")),
            Err(ValidatorWorkerError::ValidatorIdentityDrift(_))
        ));
    }

    #[test]
    fn public_core_component_identity_drift_is_rejected() {
        let artifact = Hash32::of(b"candidate");
        let evaluation = Hash32::of(b"evaluation");
        let capsule = Hash32::of(b"capsule");
        let profile = Hash32::of(b"profile");
        let request = ValidatorWorkerRequest {
            candidate_bytes: b"candidate",
            expected_artifact_hash: artifact,
            evaluation_hash: evaluation,
            capsule_hash: capsule,
            profile_hash: profile,
        };
        let mut report: serde_json::Value =
            serde_json::from_slice(&valid_report(artifact, evaluation, capsule, profile))
                .expect("valid report JSON");
        report["authority_components"][1]["source_sha256"] = "substituted source".into();
        let bytes = serde_json::to_vec(&report).expect("serialize drifted report");

        assert!(matches!(
            parse_worker_report(&bytes, request, Hash32::of(b"worker")),
            Err(ValidatorWorkerError::ValidatorIdentityDrift(_))
        ));
    }

    #[test]
    fn unknown_report_fields_are_rejected() {
        let candidate = b"candidate";
        let request = ValidatorWorkerRequest {
            candidate_bytes: candidate,
            expected_artifact_hash: Hash32::of(b"candidate"),
            evaluation_hash: Hash32::of(b"evaluation"),
            capsule_hash: Hash32::of(b"capsule"),
            profile_hash: Hash32::of(b"profile"),
        };
        let report = br#"{"protocol_version":5,"artifact_sha256":"00","evaluation_sha256":"00","capsule_sha256":"00","verdict":"failed","findings":[],"validator":{"name":"x","version":"x","source_sha256":"x","rules_sha256":"x","profile_sha256":"x"},"authority_components":[],"diagnostic_components":[],"unexpected":true}"#;

        assert!(matches!(
            parse_worker_report(report, request, Hash32::of(b"worker")),
            Err(ValidatorWorkerError::MalformedReport(_))
        ));
    }

    #[test]
    fn private_worker_copy_remains_bound_after_the_packaged_source_is_replaced() {
        let path = temp_file("validator-worker-copy", b"first executable generation");
        let binding = bind_worker(&path).expect("bind worker");
        let prepared = prepare_verified_worker(&binding).expect("prepare exact private copy");
        fs::write(&path, b"replacement executable generation").expect("replace source worker");

        assert_eq!(prepared.executable_hash, binding.executable_hash);
        assert_eq!(
            Hash32::of(&fs::read(&prepared.path).expect("read private executable")),
            binding.executable_hash
        );
        fs::remove_file(path).expect("remove source worker");
    }

    #[test]
    fn packaged_worker_failure_crosses_the_bounded_typed_process_boundary() {
        let Some(worker_path) = option_env!("CARGO_BIN_EXE_publisher-validator-worker") else {
            return;
        };
        // Cargo debug binaries can exceed the production sidecar's strict
        // 64 MiB packaging limit. Preserve that production limit while
        // allowing this test-only binding to exercise the real process path.
        let binding = bind_worker_with_limit(std::path::Path::new(worker_path), 512 * 1024 * 1024)
            .expect("bind test worker");
        let candidate = b"not a PDF candidate";
        let request = ValidatorWorkerRequest {
            candidate_bytes: candidate,
            expected_artifact_hash: Hash32::of(b"not a PDF candidate"),
            evaluation_hash: Hash32::of(b"evaluation"),
            capsule_hash: Hash32::of(b"capsule"),
            profile_hash: Hash32::of(b"profile"),
        };

        let result = run_validator_worker(&binding, request, Duration::from_secs(10));
        assert!(matches!(
            result,
            Err(ValidatorWorkerError::WorkerRejected { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_validator_fails_before_a_script_can_create_a_marker() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let marker_name = format!("publisher-validator-executed-{nonce}");
        let marker = std::env::temp_dir().join(&marker_name);
        let worker = temp_file(
            "unsupported-validator",
            format!("#!/bin/sh\nprintf executed > {marker_name}\n").as_bytes(),
        );
        let mut permissions = fs::metadata(&worker).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&worker, permissions).unwrap();
        let binding = bind_worker(&worker).unwrap();
        let request = ValidatorWorkerRequest {
            candidate_bytes: b"candidate",
            expected_artifact_hash: Hash32::of(b"candidate"),
            evaluation_hash: Hash32::of(b"evaluation"),
            capsule_hash: Hash32::of(b"capsule"),
            profile_hash: Hash32::of(b"profile"),
        };
        assert_eq!(
            run_validator_worker(&binding, request, Duration::from_secs(1)),
            Err(ValidatorWorkerError::ProcessLimitSetupFailed(
                "validator confinement is unsupported on this platform".into()
            ))
        );
        assert!(
            super::RunningProcess::spawn(
                &worker,
                &[],
                worker.parent().unwrap(),
                &[],
                Duration::from_secs(1),
                super::WORKER_MEMORY_LIMIT
            )
            .is_err()
        );
        assert!(!marker.exists(), "unsupported validator must never execute");
        fs::remove_file(worker).unwrap();
    }
}
