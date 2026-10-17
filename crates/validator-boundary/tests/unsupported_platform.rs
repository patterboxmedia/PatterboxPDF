#![cfg(not(windows))]

#[test]
fn public_validator_explicitly_refuses_the_unqualified_platform() {
    let result = validator_boundary::TrustedValidatorBoundary::resolve_packaged().validate(
        b"candidate",
        [1; 32],
        [2; 32],
        [3; 32],
    );
    assert!(matches!(result,
        Err(validator_boundary::ValidatorWorkerError::ProcessLimitSetupFailed(detail))
            if detail == "validator confinement is unsupported on this platform"));
}
