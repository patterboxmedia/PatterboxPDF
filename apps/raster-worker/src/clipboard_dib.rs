//! Finite clipboard DIB interpretation, kept inside the confined raster worker.

use super::{WorkerError, canonical_srgb_profile};
use image::{
    DynamicImage, ImageDecoder, ImageEncoder, Limits,
    codecs::{bmp::BmpDecoder, png::PngEncoder},
};
use std::io::{self, Cursor, Write};

pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024 * 1024;
pub const RULES_IDENTITY: &str =
    "publisher-v2/clipboard-dib/v1/info40-v4-v5/rgb24-bgra32/bounded-rgb-icc/straight-alpha/png";

fn rejected() -> WorkerError {
    WorkerError::DecodeFailed("unsupported, invalid, or oversized clipboard bitmap".into())
}
fn word(bytes: &[u8], offset: usize) -> Result<u32, WorkerError> {
    let bytes = bytes.get(offset..offset + 4).ok_or_else(rejected)?;
    Ok(u32::from_le_bytes(
        bytes.try_into().map_err(|_| rejected())?,
    ))
}

struct BoundedPng(Vec<u8>);
impl Write for BoundedPng {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_CLIPBOARD_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "clipboard PNG exceeds 64 MiB",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn profile_and_pixel_offset(
    bytes: &[u8],
    header: usize,
    base: usize,
    pixel_bytes: usize,
) -> Result<(Option<Vec<u8>>, usize), WorkerError> {
    if header < 108 {
        return Ok((None, base));
    }
    let color_space = word(bytes, 56)?;
    if color_space != 0x4d42_4544
        && header == 124
        && (word(bytes, 112)? != 0 || word(bytes, 116)? != 0)
    {
        return Err(rejected());
    }
    match color_space {
        0x7352_4742 => Ok((Some(canonical_srgb_profile()?), base)), // LCS_sRGB
        0 if bytes[60..108].iter().all(|byte| *byte == 0) => Ok((None, base)),
        0x4d42_4544 if header == 124 => {
            // PROFILE_EMBEDDED may precede or follow packed DIB pixels.
            let offset = usize::try_from(word(bytes, 112)?).map_err(|_| rejected())?;
            let length = usize::try_from(word(bytes, 116)?).map_err(|_| rejected())?;
            let end = offset.checked_add(length).ok_or_else(rejected)?;
            if !(128..=document_core::MAX_ICC_PROFILE_BYTES).contains(&length)
                || offset < base
                || end > bytes.len()
                || (offset != base && offset < base + pixel_bytes)
            {
                return Err(rejected());
            }
            let profile = &bytes[offset..end];
            let parsed = lcms2::Profile::new_icc(profile).map_err(|_| rejected())?;
            if parsed.color_space() != lcms2::ColorSpaceSignature::RgbData {
                return Err(rejected());
            }
            Ok((
                Some(profile.to_vec()),
                if offset == base { end } else { base },
            ))
        }
        _ => Err(rejected()), // Includes PROFILE_LINKED: never follow clipboard paths.
    }
}

/// Supports uncompressed Windows INFO/V4/V5 RGB24 and RGB32/bitfield BGRA32.
/// File-linked profiles, calibrated color spaces, RLE, palettes, and other masks refuse.
pub(super) fn normalize(bytes: &[u8]) -> Result<Vec<u8>, WorkerError> {
    if bytes.len() < 40 || bytes.len() > MAX_CLIPBOARD_BYTES {
        return Err(rejected());
    }
    let header = usize::try_from(word(bytes, 0)?).map_err(|_| rejected())?;
    if !matches!(header, 40 | 108 | 124) || bytes.len() < header {
        return Err(rejected());
    }
    let width = i32::from_le_bytes(bytes[4..8].try_into().map_err(|_| rejected())?);
    let signed_height = i32::from_le_bytes(bytes[8..12].try_into().map_err(|_| rejected())?);
    let height = signed_height.checked_abs().ok_or_else(rejected)?;
    if width <= 0
        || height <= 0
        || width > 8_192
        || height > 8_192
        || i64::from(width) * i64::from(height) > 16 * 1024 * 1024
        || bytes[12..14] != 1_u16.to_le_bytes()
        || word(bytes, 32)? != 0
    {
        return Err(rejected());
    }
    let depth = u16::from_le_bytes(bytes[14..16].try_into().map_err(|_| rejected())?);
    let compression = word(bytes, 16)?;
    if !matches!((depth, compression), (24 | 32, 0) | (32, 3)) {
        return Err(rejected());
    }
    let masks_after_header = if header == 40 && compression == 3 {
        12
    } else {
        0
    };
    let base = header + masks_after_header;
    let stride =
        (usize::try_from(width).map_err(|_| rejected())? * usize::from(depth)).div_ceil(32) * 4;
    let pixel_bytes = stride
        .checked_mul(usize::try_from(height).map_err(|_| rejected())?)
        .ok_or_else(rejected)?;
    let (embedded_profile, pixel_offset) =
        profile_and_pixel_offset(bytes, header, base, pixel_bytes)?;
    if pixel_offset.checked_add(pixel_bytes).ok_or_else(rejected)? > bytes.len() {
        return Err(rejected());
    }
    let mut dib = bytes.to_vec();
    if compression == 3 || (depth == 32 && header >= 108 && word(bytes, 52)? != 0) {
        if word(bytes, 40)? != 0x00ff_0000
            || word(bytes, 44)? != 0x0000_ff00
            || word(bytes, 48)? != 0x0000_00ff
            || (header >= 108 && !matches!(word(bytes, 52)?, 0 | 0xff00_0000))
        {
            return Err(rejected());
        }
        // BI_RGB ignores its reserved high byte. An explicit V4/V5 alpha mask
        // selects bitfields so the decoder retains authored straight alpha.
        dib[16..20].copy_from_slice(&3_u32.to_le_bytes());
    }
    let file_length = u32::try_from(dib.len() + 14).map_err(|_| rejected())?;
    let mut bmp = Vec::with_capacity(dib.len() + 14);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_length.to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(
        &u32::try_from(pixel_offset + 14)
            .map_err(|_| rejected())?
            .to_le_bytes(),
    );
    bmp.extend_from_slice(&dib);
    let mut decoder = BmpDecoder::new(Cursor::new(bmp)).map_err(|_| rejected())?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_CLIPBOARD_BYTES as u64);
    decoder.set_limits(limits).map_err(|_| rejected())?;
    let rgba = DynamicImage::from_decoder(decoder)
        .map_err(|_| rejected())?
        .into_rgba8();
    let mut png = BoundedPng(Vec::new());
    let mut encoder = PngEncoder::new(&mut png);
    if let Some(profile) = embedded_profile {
        encoder.set_icc_profile(profile).map_err(|_| rejected())?;
    }
    encoder
        .write_image(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|_| rejected())?;
    Ok(png.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dib(header: usize, depth: u16, top_down: bool, alpha: bool) -> Vec<u8> {
        let mut bytes = vec![0; header];
        bytes[0..4].copy_from_slice(&u32::try_from(header).unwrap().to_le_bytes());
        bytes[4..8].copy_from_slice(&2_i32.to_le_bytes());
        bytes[8..12].copy_from_slice(&(if top_down { -2_i32 } else { 2_i32 }).to_le_bytes());
        bytes[12..14].copy_from_slice(&1_u16.to_le_bytes());
        bytes[14..16].copy_from_slice(&depth.to_le_bytes());
        if header >= 108 {
            bytes[56..60].copy_from_slice(&0x7352_4742_u32.to_le_bytes());
            if alpha {
                bytes[16..20].copy_from_slice(&3_u32.to_le_bytes());
                for (offset, mask) in [
                    (40, 0x00ff_0000_u32),
                    (44, 0x0000_ff00),
                    (48, 0x0000_00ff),
                    (52, 0xff00_0000),
                ] {
                    bytes[offset..offset + 4].copy_from_slice(&mask.to_le_bytes());
                }
            }
        }
        let (top, bottom): ([u8; 8], [u8; 8]) = if depth == 24 {
            (
                [0, 0, 255, 0, 255, 0, 0, 0],
                [255, 0, 0, 255, 255, 255, 0, 0],
            )
        } else {
            (
                [0, 0, 255, 128, 0, 255, 0, 255],
                [255, 0, 0, 255, 255, 255, 255, 255],
            )
        };
        bytes.extend_from_slice(if top_down { &top } else { &bottom });
        bytes.extend_from_slice(if top_down { &bottom } else { &top });
        bytes
    }
    #[test]
    fn info_v4_v5_top_down_and_bottom_up_normalize_pixels_and_explicit_alpha() {
        for header in [40, 108, 124] {
            for depth in [24, 32] {
                for top_down in [false, true] {
                    let png = normalize(&dib(header, depth, top_down, depth == 32 && header > 40))
                        .unwrap();
                    let rgba = image::load_from_memory(&png).unwrap().into_rgba8();
                    assert_eq!(rgba.dimensions(), (2, 2));
                    assert_eq!(
                        rgba.get_pixel(0, 0).0,
                        [
                            255,
                            0,
                            0,
                            if depth == 32 && header > 40 { 128 } else { 255 }
                        ]
                    );
                    assert_eq!(rgba.get_pixel(1, 0).0, [0, 255, 0, 255]);
                    assert_eq!(rgba.get_pixel(0, 1).0, [0, 0, 255, 255]);
                    assert_eq!(rgba.get_pixel(1, 1).0, [255, 255, 255, 255]);
                    assert_eq!(
                        document_core::inspect_raster(&png).unwrap().media_type,
                        "image/png"
                    );
                }
            }
        }
        let mut info_bitfields = dib(40, 32, true, false);
        info_bitfields[16..20].copy_from_slice(&3_u32.to_le_bytes());
        let masks = [0x00ff_0000_u32, 0x0000_ff00, 0x0000_00ff]
            .into_iter()
            .flat_map(u32::to_le_bytes);
        info_bitfields.splice(40..40, masks);
        let rgba = image::load_from_memory(&normalize(&info_bitfields).unwrap())
            .unwrap()
            .into_rgba8();
        assert_eq!(rgba.get_pixel(0, 0).0, [255, 0, 0, 255]);
        let mut v4_explicit_alpha = dib(108, 32, true, true);
        v4_explicit_alpha[16..20].copy_from_slice(&0_u32.to_le_bytes());
        let rgba = image::load_from_memory(&normalize(&v4_explicit_alpha).unwrap())
            .unwrap()
            .into_rgba8();
        assert_eq!(rgba.get_pixel(0, 0).0, [255, 0, 0, 128]);
    }
    #[test]
    fn corrupt_excessive_linked_profile_and_unsupported_bitmap_refuse() {
        let valid = dib(124, 32, true, true);
        let mut cases = vec![
            vec![],
            valid[..123].to_vec(),
            valid[..valid.len() - 1].to_vec(),
        ];
        for (offset, value) in [
            (0, 12),
            (4, 8_193),
            (8, i32::MIN.cast_unsigned()),
            (16, 1),
            (40, 0x0f),
            (56, 0x4c49_4e4b),
            (112, u32::MAX),
        ] {
            let mut invalid = valid.clone();
            invalid[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            cases.push(invalid);
        }
        for bytes in cases {
            assert!(normalize(&bytes).is_err());
        }
    }
    #[test]
    fn embedded_rgb_icc_is_retained_at_either_packed_profile_position() {
        let profile = canonical_srgb_profile().unwrap();
        for before_pixels in [false, true] {
            let mut bytes = dib(124, 32, true, true);
            bytes[56..60].copy_from_slice(&0x4d42_4544_u32.to_le_bytes());
            let offset = if before_pixels { 124 } else { bytes.len() };
            bytes[112..116].copy_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
            bytes[116..120].copy_from_slice(&u32::try_from(profile.len()).unwrap().to_le_bytes());
            bytes.splice(offset..offset, profile.iter().copied());
            let png = normalize(&bytes).unwrap();
            let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(png)).unwrap();
            assert_eq!(decoder.icc_profile().unwrap(), Some(profile.clone()));
        }
    }
}
