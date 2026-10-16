use document_core::model::{DocumentId, Insets, PhysicalSize};
use document_core::{Document, ProjectSnapshot, Revision};
use layout_core::ResolvedPublication;
use print_core::{
    FixtureContract, FontBoundary, PreflightBasis, PublicationCandidateFinding,
    PublicationCandidateFindingCode, PublicationCandidateFindingSeverity,
    PublicationCandidatePreflight, PublicationCandidateWriter, PublicationFindingFacts,
    PublicationGate, PublicationPageEdge, PublicationReleaseBlocker, PublicationRepairAction,
    PublicationRepairTarget, PublicationWarningAcknowledgement, inspect_output_intent,
};
use validator_boundary::{
    StructuralCheckStatus, TrustedValidation, TrustedValidationVerdict, TrustedValidatorBoundary,
    ValidatorIsolationStatus, ValidatorRuleCoverageStatus,
};

const PROFILE: &[u8] =
    include_bytes!("../../../crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc");

#[test]
fn packaged_worker_produces_bound_evidence_but_cannot_authorize_release() {
    let document = Document::new(
        DocumentId::parse("trusted-validator-book").unwrap(),
        "Trusted validator book",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let publication = ResolvedPublication::resolve(&ProjectSnapshot {
        document,
        revision: Revision::initial(),
    })
    .unwrap();
    let fixture = FixtureContract::new(
        PreflightBasis::from(publication.clone()),
        inspect_output_intent(PROFILE).unwrap(),
        FontBoundary::NotExercised,
    );
    let candidate = PublicationCandidateWriter::write(&publication, &fixture, PROFILE).unwrap();
    let preflight =
        PublicationCandidatePreflight::for_candidate(&publication, &fixture, &candidate);
    assert!(preflight.ready_for_independent_validation());
    assert!(!PublicationGate::evaluate(&preflight, None).allowed);

    let boundary = TrustedValidatorBoundary::resolve_packaged();
    let trusted = boundary
        .validate(
            candidate.bytes(),
            preflight.evaluation_key.bytes(),
            preflight.export_identity.bytes(),
            preflight.profile_hash.bytes(),
        )
        .expect("packaged validator returns an identity-bound verdict");
    let gate = PublicationGate::evaluate(&preflight, Some(&trusted));
    assert_eq!(trusted.verdict(), TrustedValidationVerdict::Passed);
    assert_ne!(trusted.active_icc_source_hash(), [0; 32]);
    assert_ne!(trusted.active_icc_rules_hash(), [0; 32]);
    assert_ne!(trusted.public_core_source_hash(), [0; 32]);
    assert_ne!(trusted.public_core_rules_hash(), [0; 32]);
    #[cfg(windows)]
    {
        assert_eq!(
            trusted.structural_check_status(),
            StructuralCheckStatus::Passed
        );
        assert_ne!(trusted.qpdf_package_hash(), [0; 32]);
        assert_structural_checker_rejects_malformed_pdf(&boundary, preflight.profile_hash.bytes());
    }
    #[cfg(not(windows))]
    assert_eq!(
        trusted.structural_check_status(),
        StructuralCheckStatus::NotRunOnUnsupportedHost
    );
    assert_eq!(
        trusted.rule_coverage_status(),
        ValidatorRuleCoverageStatus::Incomplete
    );
    #[cfg(windows)]
    assert_eq!(
        trusted.isolation_status(),
        ValidatorIsolationStatus::ReleaseProven
    );
    #[cfg(not(windows))]
    assert_eq!(
        trusted.isolation_status(),
        ValidatorIsolationStatus::ProcessLimitsOnly
    );
    assert!(!gate.allowed);
    assert!(gate.authorization().is_none());
    #[cfg(windows)]
    assert_eq!(
        gate.release_blockers(),
        &[PublicationReleaseBlocker::ValidatorRuleCoverageNotReleaseProven]
    );
    #[cfg(not(windows))]
    assert_eq!(
        gate.release_blockers(),
        &[
            PublicationReleaseBlocker::ValidatorRuleCoverageNotReleaseProven,
            PublicationReleaseBlocker::ValidatorIsolationNotReleaseProven,
        ]
    );

    assert_warning_acknowledgement_gate(
        &preflight,
        &trusted,
        publication.pages()[0].page_id.as_str(),
    );

    assert_gate_rejects_drift_and_structural_blockers(&preflight, &trusted);
}

fn assert_gate_rejects_drift_and_structural_blockers(
    preflight: &PublicationCandidatePreflight,
    trusted: &TrustedValidation,
) {
    let mut drifted = preflight.clone();
    drifted.artifact_hash = print_core::ArtifactHash::of(b"different candidate");
    assert!(!PublicationGate::evaluate(&drifted, Some(trusted)).allowed);

    let mut capsule_drifted = preflight.clone();
    capsule_drifted.export_identity = print_core::ArtifactHash::of(b"different export capsule");
    let capsule_gate = PublicationGate::evaluate(&capsule_drifted, Some(trusted));
    assert!(!capsule_gate.allowed);
    assert_eq!(
        capsule_gate.reason,
        "validator evidence belongs to a different export capsule"
    );

    let mut blocked = preflight.clone();
    blocked.findings.push(PublicationCandidateFinding {
        severity: PublicationCandidateFindingSeverity::Blocker,
        code: PublicationCandidateFindingCode::CandidateBytesChanged,
        message: "injected structural blocker",
        target: None,
        facts: print_core::PublicationFindingFacts::None,
    });
    assert!(!PublicationGate::evaluate(&blocked, Some(trusted)).allowed);
}

#[cfg(windows)]
fn assert_structural_checker_rejects_malformed_pdf(
    boundary: &TrustedValidatorBoundary,
    profile_hash: [u8; 32],
) {
    let malformed = b"%PDF-1.6\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
    let structural_failure = boundary
        .validate(malformed, [21; 32], [22; 32], profile_hash)
        .expect("a structural rejection is trustworthy failed evidence");
    assert_eq!(
        structural_failure.structural_check_status(),
        StructuralCheckStatus::Failed
    );
    assert_eq!(
        structural_failure.verdict(),
        TrustedValidationVerdict::Failed
    );
    assert_ne!(structural_failure.qpdf_package_hash(), [0; 32]);
    // The mandatory worker now runs first and binds its closed-profile refusal
    // before qpdf contributes the independent structural rejection.
    assert_ne!(structural_failure.worker_executable_hash(), [0; 32]);
    assert!(
        structural_failure
            .findings()
            .iter()
            .any(|finding| finding.starts_with("blocking Publisher writer profile v5:"))
    );
    assert!(
        structural_failure
            .findings()
            .iter()
            .any(|finding| finding.contains("qpdf structural check failed"))
    );
}

fn assert_warning_acknowledgement_gate(
    preflight: &PublicationCandidatePreflight,
    trusted: &TrustedValidation,
    page_id: &str,
) {
    let mut warning_report = preflight.clone();
    warning_report.findings.push(PublicationCandidateFinding {
        severity: PublicationCandidateFindingSeverity::Warning,
        code: PublicationCandidateFindingCode::PossibleBleedMissing,
        message: "review exact bleed intent",
        target: Some(PublicationRepairTarget {
            page_id: page_id.to_owned(),
            object_id: None,
            action: PublicationRepairAction::ReviewBleedIntent {
                edge: PublicationPageEdge::Right,
            },
        }),
        facts: PublicationFindingFacts::BleedEdge {
            edge: PublicationPageEdge::Right,
        },
    });
    let unacknowledged = PublicationGate::evaluate(&warning_report, Some(trusted));
    assert_eq!(
        unacknowledged.reason,
        "preflight warnings require acknowledgement for this exact evaluation"
    );
    let acknowledgement = PublicationWarningAcknowledgement::for_report(&warning_report)
        .expect("blocker-free warning report can be acknowledged");
    let acknowledged = PublicationGate::evaluate_with_acknowledgement(
        &warning_report,
        Some(trusted),
        Some(&acknowledgement),
    );
    #[cfg(windows)]
    assert_eq!(
        acknowledged.reason,
        "trusted validator evidence is recorded, but validator rule coverage is not release-proven"
    );
    #[cfg(not(windows))]
    assert_eq!(
        acknowledged.reason,
        "trusted validator evidence is recorded, but validator rule coverage and isolation are not release-proven"
    );
    let mut changed_warnings = warning_report.clone();
    changed_warnings
        .findings
        .last_mut()
        .expect("injected warning")
        .facts = PublicationFindingFacts::BleedEdge {
        edge: PublicationPageEdge::Left,
    };
    assert!(!changed_warnings.warnings_acknowledged(Some(&acknowledgement)));
}
