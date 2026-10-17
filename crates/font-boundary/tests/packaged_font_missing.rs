#![cfg(all(
    any(windows, all(target_os = "linux", target_arch = "x86_64")),
    not(debug_assertions)
))]

// Run only in the isolated package fixture with this adjacent worker omitted.
#[test]
#[ignore = "requires an isolated release test directory without the adjacent sidecar"]
fn missing_packaged_sidecar_fails_closed() {
    let executable = std::env::current_exe().expect("test executable");
    assert!(
        !executable
            .with_file_name(if cfg!(windows) {
                "publisher-font-worker.exe"
            } else {
                "publisher-font-worker"
            })
            .exists()
    );
    let bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf");
    assert!(matches!(
        font_boundary::TrustedFontBoundary::resolve_packaged().inspect(bytes, 0),
        Err(font_boundary::FontWorkerError::NotInstalled)
    ));
}
