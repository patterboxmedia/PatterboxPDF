#![cfg(all(windows, not(debug_assertions)))]

use validator_boundary::{TrustedValidatorBoundary, ValidatorWorkerError};

fn adjacent(name: &str) -> std::path::PathBuf {
    std::env::current_exe()
        .expect("test executable")
        .with_file_name(name)
}

#[test]
#[ignore = "requires an isolated release package with the validator omitted"]
fn missing_validator_fails_closed() {
    assert!(!adjacent("publisher-validator-worker.exe").exists());
    assert!(matches!(
        TrustedValidatorBoundary::resolve_packaged().validate(
            b"not a PDF",
            [1; 32],
            [2; 32],
            [3; 32]
        ),
        Err(ValidatorWorkerError::NotInstalled)
    ));
}

#[test]
#[ignore = "requires an isolated release package with qpdf.exe omitted"]
fn missing_qpdf_fails_closed() {
    assert!(adjacent("publisher-validator-worker.exe").is_file());
    assert!(!adjacent("qpdf.exe").exists());
    assert!(matches!(
        TrustedValidatorBoundary::resolve_packaged().validate(
            b"not a PDF",
            [1; 32],
            [2; 32],
            [3; 32]
        ),
        Err(ValidatorWorkerError::StructuralCheckerUnavailable(_))
    ));
}

#[test]
#[ignore = "requires an isolated release package with qpdf30.dll omitted"]
fn missing_qpdf_library_fails_closed() {
    assert!(adjacent("publisher-validator-worker.exe").is_file());
    assert!(adjacent("qpdf.exe").is_file());
    assert!(!adjacent("qpdf30.dll").exists());
    assert!(matches!(
        TrustedValidatorBoundary::resolve_packaged().validate(
            b"not a PDF",
            [1; 32],
            [2; 32],
            [3; 32]
        ),
        Err(ValidatorWorkerError::StructuralCheckerUnavailable(_))
    ));
}

#[test]
#[ignore = "requires a complete isolated Windows release package"]
fn malformed_candidate_receives_failed_structural_verdict() {
    use sha2::{Digest, Sha256};

    const MALFORMED_PDF: &[u8] = b"not a PDF";
    const MALFORMED_PDF_SHA256: [u8; 32] = [
        0x6d, 0x68, 0x9b, 0xa9, 0x52, 0x21, 0x27, 0xc0, 0xc7, 0x59, 0x53, 0x5f, 0x5a, 0xdb, 0x7f,
        0x2c, 0x8f, 0x18, 0x2a, 0xc2, 0x80, 0xd9, 0x4e, 0xab, 0x93, 0xcb, 0x54, 0xe5, 0xec, 0x59,
        0x8f, 0xa0,
    ];
    assert_eq!(
        <[u8; 32]>::from(Sha256::digest(MALFORMED_PDF)),
        MALFORMED_PDF_SHA256
    );
    let report = TrustedValidatorBoundary::resolve_packaged()
        .validate(MALFORMED_PDF, [1; 32], [2; 32], [3; 32])
        .expect("complete package returns a structural rejection");
    assert_eq!(report.artifact_hash(), MALFORMED_PDF_SHA256);
    assert_eq!(
        report.structural_check_status(),
        validator_boundary::StructuralCheckStatus::Failed
    );
    assert_eq!(
        report.verdict(),
        validator_boundary::TrustedValidationVerdict::Failed
    );
    assert_ne!(report.qpdf_package_hash(), [0; 32]);
    assert_eq!(
        report.worker_executable_hash(),
        [0; 32],
        "structural rejection must stop before the PDF/X validator worker"
    );
    assert_eq!(report.findings().len(), 1);
    let finding = &report.findings()[0];
    assert!(finding.starts_with("qpdf structural check failed:"));
    assert!(finding.chars().count() <= 550);
    assert!(!finding.contains("publisher-qpdf-"));
    assert!(!finding.contains("candidate.pdf"));
    assert!(!finding.contains("\\Users\\"));
    assert!(!finding.contains(":\\"));
}

#[test]
#[ignore = "requires a complete isolated Windows release package"]
fn ordinary_pdf_reaches_validator_and_is_rejected_as_pdfx() {
    use pdf_writer::{Pdf, Rect, Ref};
    let mut pdf = Pdf::new();
    let catalog = Ref::new(1);
    let pages = Ref::new(2);
    let page = Ref::new(3);
    pdf.catalog(catalog).pages(pages);
    pdf.pages(pages).kids([page]).count(1);
    pdf.page(page)
        .parent(pages)
        .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
        .resources();
    let candidate = pdf.finish();
    let report = TrustedValidatorBoundary::resolve_packaged()
        .validate(&candidate, [1; 32], [2; 32], [3; 32])
        .expect("structurally valid ordinary PDF reaches the validator");
    assert_eq!(
        report.structural_check_status(),
        validator_boundary::StructuralCheckStatus::Passed,
        "{:?}",
        report.findings()
    );
    assert_eq!(
        report.verdict(),
        validator_boundary::TrustedValidationVerdict::Failed
    );
    assert_ne!(report.worker_executable_hash(), [0; 32]);
    assert_ne!(report.qpdf_package_hash(), [0; 32]);
    assert!(!report.findings().is_empty());
}

fn pdf_with_active_page_icc(profile: &[u8]) -> Vec<u8> {
    use pdf_writer::writers::ColorSpace;
    use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref};

    let catalog = Ref::new(1);
    let pages = Ref::new(2);
    let page = Ref::new(3);
    let content = Ref::new(4);
    let profile_ref = Ref::new(5);
    let mut pdf = Pdf::new();
    pdf.set_version(1, 6);
    pdf.catalog(catalog).pages(pages);
    pdf.pages(pages).kids([page]).count(1);
    let mut page_writer = pdf.page(page);
    page_writer
        .parent(pages)
        .media_box(Rect::new(0.0, 0.0, 612.0, 792.0))
        .contents(content);
    page_writer
        .resources()
        .color_spaces()
        .insert(Name(b"PageIcc"))
        .start::<ColorSpace>()
        .icc_based(profile_ref);
    page_writer.finish();
    let mut operators = Content::new();
    operators.set_fill_color_space(Name(b"PageIcc"));
    operators.set_fill_color([0.1, 0.2, 0.3, 0.4]);
    operators.rect(10.0, 10.0, 20.0, 20.0);
    operators.fill_nonzero();
    pdf.stream(content, &operators.finish());
    pdf.icc_profile(profile_ref, profile).n(4);
    pdf.finish()
}

#[test]
#[ignore = "requires a complete isolated Windows release package"]
fn unsupported_active_page_icc_is_rejected_before_general_diagnostics() {
    use sha2::{Digest, Sha256};
    use validator_boundary::{StructuralCheckStatus, TrustedValidationVerdict};

    let valid_profile =
        include_bytes!("../../print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
    for (label, profile) in [
        ("valid", valid_profile.as_slice()),
        ("malformed", b"not an ICC profile".as_slice()),
    ] {
        let candidate = pdf_with_active_page_icc(profile);
        let expected_hash = <[u8; 32]>::from(Sha256::digest(&candidate));
        println!(
            "{label} candidate SHA-256: {:x}",
            Sha256::digest(&candidate)
        );
        let report = TrustedValidatorBoundary::resolve_packaged()
            .validate(&candidate, [1; 32], [2; 32], [3; 32])
            .expect("qpdf-valid page ICC candidate receives a packaged verdict");
        assert_eq!(report.artifact_hash(), expected_hash, "{label}");
        assert_eq!(
            report.structural_check_status(),
            StructuralCheckStatus::Passed,
            "{label}"
        );
        assert_ne!(report.qpdf_package_hash(), [0; 32], "{label}");
        assert_ne!(report.worker_executable_hash(), [0; 32], "{label}");
        assert_ne!(report.active_icc_source_hash(), [0; 32], "{label}");
        assert_ne!(report.active_icc_rules_hash(), [0; 32], "{label}");
        // Publisher emits converted DeviceCMYK content, not named ICCBased
        // page color spaces. The closed writer census must refuse both inputs
        // before general diagnostics decode their profiles. Component hashes
        // above identify the configured pipeline; they do not prove it ran.
        assert_eq!(
            report.verdict(),
            TrustedValidationVerdict::Failed,
            "{label}"
        );
        assert!(!report.findings().is_empty(), "{label}");
        assert!(
            report
                .findings()
                .iter()
                .all(|finding| { finding.starts_with("blocking Publisher writer profile v5:") }),
            "{label}: {:?}",
            report.findings()
        );
        assert!(
            report.findings().iter().any(|finding| {
                finding.contains("Resources/ColorSpace: entry is not emitted by Publisher")
            }),
            "{label}: {:?}",
            report.findings()
        );
        assert!(
            report.findings().iter().all(|finding| {
                finding.chars().count() <= 550
                    && !finding.contains("candidate.pdf")
                    && !finding.contains("publisher-qpdf-")
                    && !finding.contains("\\Users\\")
                    && !finding.contains(":\\")
            }),
            "{label}: {:?}",
            report.findings()
        );
    }
}
