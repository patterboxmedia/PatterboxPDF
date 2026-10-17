#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

#[test]
fn linux_prepares_actual_profile_through_public_boundary() {
    let profile = include_bytes!("../../print-core/tests/fixtures/SWOP2006coated_300_mth.icc");
    let inspected = color_boundary::TrustedColorBoundary::resolve_packaged()
        .inspect_output_profile(profile)
        .expect("qualified Linux must run the confined profile worker");
    assert_eq!(inspected.byte_len(), profile.len());
    let rgb = [255, 0, 0, 0, 255, 0];
    let converted = color_boundary::TrustedColorBoundary::resolve_packaged()
        .convert_srgb(&rgb, 2, 1, profile)
        .expect("authored sRGB uses the same confined conversion");
    assert_eq!(converted.cmyk_pixels().len(), 8);
    assert_eq!(converted.width(), 2);
    assert_eq!(converted.height(), 1);
    assert_eq!(
        converted.worker_executable_hash(),
        inspected.worker_executable_hash()
    );
}
