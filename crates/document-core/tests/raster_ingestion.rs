use document_core::assets::{MAX_ICC_PROFILE_BYTES, tiff_embedded_icc_profile};
use document_core::{AssetHash, RasterColorProfile, RasterIngestError, inspect_raster};
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;

fn encoded(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let image = DynamicImage::new_rgb8(width, height);
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, format).expect("encode fixture");
    bytes.into_inner()
}

#[test]
fn supported_rasters_report_magic_derived_dimensions_and_color_facts() {
    for (format, media_type) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::Tiff, "image/tiff"),
    ] {
        let inspected = inspect_raster(&encoded(format, 17, 9)).expect("supported raster");
        assert_eq!(inspected.media_type, media_type);
        assert_eq!((inspected.width_pixels, inspected.height_pixels), (17, 9));
        assert_eq!(
            (
                inspected.oriented_width_pixels,
                inspected.oriented_height_pixels
            ),
            (17, 9)
        );
        assert_eq!(inspected.exif_orientation, 1);
        assert_eq!(inspected.color_profile, RasterColorProfile::Unspecified);
    }
}

#[test]
fn active_unsupported_corrupt_and_oversized_dimensions_are_rejected() {
    assert_eq!(inspect_raster(b""), Err(RasterIngestError::Empty));
    assert_eq!(
        inspect_raster(b"<svg xmlns='http://www.w3.org/2000/svg'></svg>"),
        Err(RasterIngestError::Unsupported)
    );
    assert_eq!(
        inspect_raster(b"%PDF-1.7\n"),
        Err(RasterIngestError::Unsupported)
    );
    assert_eq!(
        inspect_raster(b"\x89PNG\r\n\x1a\ncorrupt"),
        Err(RasterIngestError::Corrupt)
    );
    assert_eq!(
        inspect_raster(&encoded(ImageFormat::Png, 30_001, 1)),
        Err(RasterIngestError::DimensionsExceeded),
        "the oversized dimensions must reject before pixel decoding"
    );
}

const TAGGED_TIFF: &[u8] =
    include_bytes!("../../print-core/tests/fixtures/embedded-rgb-alpha-3x2.tif");
const UNTAGGED_TIFF: &[u8] =
    include_bytes!("../../print-core/tests/fixtures/oriented-alpha-3x2.tif");

fn icc_entry_offset(bytes: &[u8]) -> usize {
    assert_eq!(&bytes[..4], b"II*\0", "authored classic little-endian TIFF");
    let ifd = usize::try_from(u32::from_le_bytes(bytes[4..8].try_into().unwrap())).unwrap();
    let entries = usize::from(u16::from_le_bytes(bytes[ifd..ifd + 2].try_into().unwrap()));
    (0..entries)
        .map(|index| ifd + 2 + 12 * index)
        .find(|&offset| u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) == 34675)
        .expect("authored ICC tag")
}

#[test]
fn tiff_icc_tag_is_read_independently_of_small_pixel_allocation() {
    let profile = tiff_embedded_icc_profile(TAGGED_TIFF).unwrap().unwrap();
    assert_eq!(profile.len(), 588);
    assert_eq!(
        AssetHash::for_bytes(&profile).as_str(),
        "217fe48ec958c667f8eef725aa27198f465df95d7662593b90d0a1cc30114356"
    );
    assert_eq!(
        inspect_raster(TAGGED_TIFF).unwrap().color_profile,
        RasterColorProfile::Embedded {
            sha256: AssetHash::for_bytes(&profile),
            byte_length: 588,
        }
    );
    assert_eq!(tiff_embedded_icc_profile(UNTAGGED_TIFF), Ok(None));
    assert_eq!(
        inspect_raster(UNTAGGED_TIFF).unwrap().color_profile,
        RasterColorProfile::Unspecified
    );
}

#[test]
fn tiff_present_icc_tag_must_have_bounded_byte_value() {
    let entry = icc_entry_offset(TAGGED_TIFF);
    let mut byte_type = TAGGED_TIFF.to_vec();
    byte_type[entry + 2..entry + 4].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(
        tiff_embedded_icc_profile(&byte_type).unwrap().unwrap(),
        tiff_embedded_icc_profile(TAGGED_TIFF).unwrap().unwrap()
    );

    let mut invalid_type = TAGGED_TIFF.to_vec();
    invalid_type[entry + 2..entry + 4].copy_from_slice(&3u16.to_le_bytes());
    assert_eq!(
        tiff_embedded_icc_profile(&invalid_type),
        Err(RasterIngestError::Corrupt)
    );
    assert_eq!(
        inspect_raster(&invalid_type),
        Err(RasterIngestError::Corrupt)
    );

    let mut zero_count = TAGGED_TIFF.to_vec();
    zero_count[entry + 4..entry + 8].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        tiff_embedded_icc_profile(&zero_count),
        Err(RasterIngestError::Corrupt)
    );

    let mut oversized = TAGGED_TIFF.to_vec();
    oversized[entry + 4..entry + 8].copy_from_slice(
        &u32::try_from(MAX_ICC_PROFILE_BYTES + 1)
            .unwrap()
            .to_le_bytes(),
    );
    assert_eq!(
        tiff_embedded_icc_profile(&oversized),
        Err(RasterIngestError::ColorProfileExceeded)
    );

    let mut outside_file = TAGGED_TIFF.to_vec();
    outside_file[entry + 8..entry + 12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        tiff_embedded_icc_profile(&outside_file),
        Err(RasterIngestError::Corrupt)
    );

    let mut truncated = TAGGED_TIFF.to_vec();
    let profile_offset = usize::try_from(u32::from_le_bytes(
        truncated[entry + 8..entry + 12].try_into().unwrap(),
    ))
    .unwrap();
    let profile_length = usize::try_from(u32::from_le_bytes(
        truncated[entry + 4..entry + 8].try_into().unwrap(),
    ))
    .unwrap();
    truncated.truncate(profile_offset + profile_length - 1);
    assert_eq!(
        tiff_embedded_icc_profile(&truncated),
        Err(RasterIngestError::Corrupt)
    );
}
