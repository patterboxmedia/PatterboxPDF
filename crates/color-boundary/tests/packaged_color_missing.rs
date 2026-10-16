#![cfg(all(windows, not(debug_assertions)))]

// Run only in the isolated package fixture with this adjacent worker omitted.
#[test]
#[ignore = "requires an isolated release test directory without the adjacent sidecar"]
fn missing_packaged_sidecar_fails_closed() {
    let executable = std::env::current_exe().expect("test executable");
    assert!(
        !executable
            .with_file_name("publisher-color-worker.exe")
            .exists()
    );
    let bytes = include_bytes!("../../print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
    assert!(matches!(
        color_boundary::TrustedColorBoundary::resolve_packaged().inspect_output_profile(bytes),
        Err(color_boundary::ColorWorkerError::NotInstalled)
    ));
}
