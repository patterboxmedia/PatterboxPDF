use crate::EvaluationKey;
use sha2::{Digest, Sha256};

/// SHA-256 identity of the exact candidate bytes sent for validation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ArtifactHash([u8; 32]);

impl ArtifactHash {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    #[must_use]
    pub fn hex(self) -> String {
        let mut hex = String::with_capacity(64);
        for byte in self.0 {
            use std::fmt::Write;
            write!(&mut hex, "{byte:02x}").expect("writing to a string cannot fail");
        }
        hex
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrintStandard {
    PdfX4,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorIdentity {
    pub name: String,
    pub version: String,
    pub executable_or_source_hash: ArtifactHash,
    pub rules_hash: ArtifactHash,
    pub profile_hash: ArtifactHash,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationVerdict {
    Passed,
    Failed,
}

/// Independent evidence for the exact candidate fingerprint supplied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndependentValidation {
    evaluation_key: EvaluationKey,
    artifact_hash: ArtifactHash,
    validator: ValidatorIdentity,
    standard: PrintStandard,
    verdict: ValidationVerdict,
    findings: Vec<String>,
}

impl IndependentValidation {
    #[must_use]
    pub const fn evaluation_key(&self) -> EvaluationKey {
        self.evaluation_key
    }

    #[must_use]
    pub const fn artifact_hash(&self) -> ArtifactHash {
        self.artifact_hash
    }

    #[must_use]
    pub const fn standard(&self) -> PrintStandard {
        self.standard
    }

    #[must_use]
    pub const fn verdict(&self) -> ValidationVerdict {
        self.verdict
    }

    #[must_use]
    pub fn validator(&self) -> &ValidatorIdentity {
        &self.validator
    }

    #[must_use]
    pub fn findings(&self) -> &[String] {
        &self.findings
    }
}

/// Unbound facts returned by a configured external validator adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatorResponse {
    pub verdict: ValidationVerdict,
    pub findings: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct ValidationRequest<'a> {
    evaluation_key: EvaluationKey,
    artifact_hash: ArtifactHash,
    candidate_bytes: &'a [u8],
    standard: PrintStandard,
}

impl<'a> ValidationRequest<'a> {
    #[must_use]
    pub fn new(
        evaluation_key: EvaluationKey,
        candidate_bytes: &'a [u8],
        standard: PrintStandard,
    ) -> Self {
        Self {
            evaluation_key,
            artifact_hash: ArtifactHash::of(candidate_bytes),
            candidate_bytes,
            standard,
        }
    }

    #[must_use]
    pub const fn evaluation_key(self) -> EvaluationKey {
        self.evaluation_key
    }

    #[must_use]
    pub const fn artifact_hash(self) -> ArtifactHash {
        self.artifact_hash
    }

    #[must_use]
    pub const fn candidate_bytes(self) -> &'a [u8] {
        self.candidate_bytes
    }

    #[must_use]
    pub const fn standard(self) -> PrintStandard {
        self.standard
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    NotConfigured,
    EmptyCandidate,
    Unavailable(String),
    SpawnFailed(String),
    TimedOut,
    Cancelled,
    OutputLimitExceeded,
    MalformedReport(String),
    IdentityDrift(String),
}

/// Vendor-neutral, read-only seam for an independent validator.
///
/// A validator may report only findings and a verdict. Byte rewriting belongs
/// behind a separate normalizer port, and any resulting bytes require a fresh
/// independent validation request.
pub trait IndependentValidator {
    fn identity(&self) -> ValidatorIdentity;

    /// Inspects the exact candidate bytes without modifying them.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] when the validator cannot run or cannot
    /// produce a trustworthy report for the request.
    fn validate(
        &self,
        request: ValidationRequest<'_>,
    ) -> Result<ValidatorResponse, ValidationError>;
}

/// Runs the configured adapter and binds its response to the exact request.
/// Callers cannot manufacture [`IndependentValidation`] directly.
///
/// # Errors
///
/// Returns [`ValidationError::EmptyCandidate`] for empty input, or propagates
/// the configured validator's execution and report errors.
pub fn run_independent_validation(
    validator: &dyn IndependentValidator,
    request: ValidationRequest<'_>,
) -> Result<IndependentValidation, ValidationError> {
    if request.candidate_bytes().is_empty() {
        return Err(ValidationError::EmptyCandidate);
    }
    let identity = validator.identity();
    let response = validator.validate(request)?;
    Ok(IndependentValidation {
        evaluation_key: request.evaluation_key(),
        artifact_hash: request.artifact_hash(),
        validator: identity,
        standard: request.standard(),
        verdict: response.verdict,
        findings: response.findings,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ArtifactHash, IndependentValidator, PrintStandard, ValidationError, ValidationRequest,
        ValidationVerdict, ValidatorIdentity, ValidatorResponse, run_independent_validation,
    };
    use crate::EvaluationKey;

    struct RecordingValidator;

    impl IndependentValidator for RecordingValidator {
        fn identity(&self) -> ValidatorIdentity {
            ValidatorIdentity {
                name: "test double".into(),
                version: "1".into(),
                executable_or_source_hash: ArtifactHash::of(b"test-validator"),
                rules_hash: ArtifactHash::of(b"test-rules"),
                profile_hash: ArtifactHash::of(b"test-profile"),
            }
        }

        fn validate(
            &self,
            request: ValidationRequest<'_>,
        ) -> Result<ValidatorResponse, ValidationError> {
            assert!(!request.candidate_bytes().is_empty());
            Ok(ValidatorResponse {
                verdict: ValidationVerdict::Passed,
                findings: Vec::new(),
            })
        }
    }

    #[test]
    fn adapter_result_is_bound_to_requested_evaluation() {
        let key = EvaluationKey::from_bytes([9; 32]);
        let result = run_independent_validation(
            &RecordingValidator,
            ValidationRequest::new(key, b"candidate", PrintStandard::PdfX4),
        )
        .expect("test adapter succeeds");
        assert_eq!(result.evaluation_key(), key);
        assert_eq!(result.artifact_hash(), ArtifactHash::of(b"candidate"));
    }

    #[test]
    fn empty_candidate_is_rejected_before_adapter_execution() {
        let result = run_independent_validation(
            &RecordingValidator,
            ValidationRequest::new(
                EvaluationKey::from_bytes([4; 32]),
                b"",
                PrintStandard::PdfX4,
            ),
        );

        assert_eq!(result, Err(ValidationError::EmptyCandidate));
    }
}
