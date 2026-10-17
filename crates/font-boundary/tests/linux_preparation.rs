#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use font_boundary::{FontOutlineKind, TrustedFontBoundary};
use sha2::{Digest, Sha256};

fn checksum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0_u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

#[test]
fn actual_worker_rejects_synthetic_restricted_rights_without_prepared_receipt() {
    let mut bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let count = usize::from(u16::from_be_bytes(bytes[4..6].try_into().unwrap()));
    let records = (0..count).map(|index| 12 + 16 * index).collect::<Vec<_>>();
    let os2 = *records
        .iter()
        .find(|offset| &bytes[**offset..**offset + 4] == b"OS/2")
        .unwrap();
    let head = *records
        .iter()
        .find(|offset| &bytes[**offset..**offset + 4] == b"head")
        .unwrap();
    let offset = u32::from_be_bytes(bytes[os2 + 8..os2 + 12].try_into().unwrap()) as usize;
    let length = u32::from_be_bytes(bytes[os2 + 12..os2 + 16].try_into().unwrap()) as usize;
    let adjustment =
        u32::from_be_bytes(bytes[head + 8..head + 12].try_into().unwrap()) as usize + 8;
    // Synthetic rights metadata on an OFL fixture; preserve valid SFNT checksums.
    bytes[offset + 8..offset + 10].copy_from_slice(&2_u16.to_be_bytes());
    let table_checksum = checksum(&bytes[offset..offset + length]);
    bytes[os2 + 4..os2 + 8].copy_from_slice(&table_checksum.to_be_bytes());
    bytes[adjustment..adjustment + 4].fill(0);
    let whole_adjustment = 0xb1b0_afba_u32.wrapping_sub(checksum(&bytes));
    bytes[adjustment..adjustment + 4].copy_from_slice(&whole_adjustment.to_be_bytes());
    assert_eq!(checksum(&bytes), 0xb1b0_afba);
    let boundary = TrustedFontBoundary::resolve_packaged();
    boundary.inspect(&bytes, 0).unwrap();
    for result in [
        boundary.prepare(&bytes, 0, &[0, 36]).map(|_| ()),
        boundary
            .prepare_publication(&bytes, 0, &[0, 36])
            .map(|_| ()),
    ] {
        assert!(matches!(
            result,
            Err(font_boundary::FontWorkerError::EmbeddingDenied(
                font_boundary::FontEmbeddingDenial::Restricted
            ))
        ));
    }
}

#[test]
fn actual_truetype_and_cff_inspect_subset_and_publication_keep_exact_identities() {
    let boundary = TrustedFontBoundary::resolve_packaged();
    for (bytes, outline, glyph) in [
        (
            &include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf")[..],
            FontOutlineKind::TrueType,
            36,
        ),
        (
            &include_bytes!("../../layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf")[..],
            FontOutlineKind::Cff,
            34,
        ),
    ] {
        let source_hash: [u8; 32] = Sha256::digest(bytes).into();
        let inspected = boundary.inspect(bytes, 0).unwrap();
        let subset = boundary.prepare(bytes, 0, &[0, glyph]).unwrap();
        let publication = boundary.prepare_publication(bytes, 0, &[0, glyph]).unwrap();
        assert_eq!(inspected.source_sha256(), source_hash);
        assert_eq!(subset.source_sha256(), source_hash);
        assert_eq!(publication.source_sha256(), source_hash);
        assert_eq!(publication.outline_kind(), outline);
        assert_eq!(publication.prepared_bytes(), subset.subset_bytes());
        assert_eq!(publication.prepared_sha256(), subset.subset_sha256());
        assert_eq!(
            publication.worker_executable_sha256(),
            inspected.worker_executable_sha256()
        );
    }
}
