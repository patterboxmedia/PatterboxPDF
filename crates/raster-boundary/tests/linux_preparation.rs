#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use document_core::{
    AssetHash, UntaggedRasterPolicy,
    raster_capacity::{MAX_RETAINED_RASTER_BYTES, RasterMemoryPermit},
};
use raster_boundary::{RasterSourceProfileOrigin, TrustedRasterBoundary};

#[test]
fn oriented_tiff_keeps_straight_alpha_exact_profile_and_receipt_memory_lifetime() {
    let boundary = TrustedRasterBoundary::resolve_packaged();
    let untagged = include_bytes!("../../print-core/tests/fixtures/oriented-alpha-3x2.tif");
    assert!(
        boundary
            .decode(
                untagged,
                &AssetHash::for_bytes(untagged),
                UntaggedRasterPolicy::RequireEmbeddedProfile
            )
            .is_err()
    );
    let tagged = include_bytes!("../../print-core/tests/fixtures/embedded-rgb-alpha-3x2.tif");
    let decoded = boundary
        .decode(
            tagged,
            &AssetHash::for_bytes(tagged),
            UntaggedRasterPolicy::RequireEmbeddedProfile,
        )
        .unwrap();
    assert_eq!((decoded.width(), decoded.height()), (2, 3));
    assert_eq!(decoded.inspection().exif_orientation, 6);
    assert_eq!(
        decoded.rgb_pixels(),
        [
            255, 255, 0, 255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0, 0, 255
        ]
    );
    assert_eq!(decoded.alpha_pixels(), [64, 255, 0, 192, 255, 128]);
    assert_eq!(
        decoded.source_profile_origin(),
        RasterSourceProfileOrigin::Embedded
    );
    assert_eq!(
        decoded.source_profile(),
        include_bytes!("../../print-core/tests/fixtures/generated-srgb.icc")
    );
    assert!(RasterMemoryPermit::reserve(MAX_RETAINED_RASTER_BYTES).is_err());
    drop(decoded);
    assert!(RasterMemoryPermit::reserve(MAX_RETAINED_RASTER_BYTES).is_ok());
}
