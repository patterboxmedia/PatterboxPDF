#![cfg(all(windows, not(debug_assertions)))]

#[test]
#[ignore = "requires an isolated release test directory without the adjacent sidecar"]
fn missing_packaged_sidecar_fails_closed() {
    let executable = std::env::current_exe().expect("test executable");
    assert!(
        !executable
            .with_file_name("publisher-raster-worker.exe")
            .exists()
    );
    // One authored RGB red pixel; valid input avoids a pre-launch input rejection.
    let bytes: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0,
        3, 1, 1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    assert!(matches!(
        raster_boundary::TrustedRasterBoundary::resolve_packaged().decode(
            bytes,
            &document_core::AssetHash::for_bytes(bytes),
            document_core::UntaggedRasterPolicy::AssumeSrgb,
        ),
        Err(raster_boundary::RasterWorkerError::NotInstalled)
    ));
}
