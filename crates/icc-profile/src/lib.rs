//! Pure checks for the exact bytes of supported CMYK printer profiles.
//! Identity registration and original measurement provenance are external facts.
//! Optional characterization interchange files and retained PostScript operands
//! must be valid resources in their declared languages. This module checks their
//! ICC framing and roles, without interpreting those nested languages.

#[cfg(test)]
mod tests;

use md5::{Digest, Md5};

pub const MAX_PROFILE_BYTES: usize = 16 * 1024 * 1024;
const ICC_HEADER_LEN: usize = 128;
const ICC_TAG_COUNT_LEN: usize = 4;
const ICC_TAG_RECORD_LEN: usize = 12;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    TooShort,
    TooLarge,
    DeclaredSizeMismatch,
    BadSignature,
    InvalidVersionEncoding([u8; 4]),
    UnsupportedVersion([u8; 4]),
    Header,
    ProfileId,
    MissingTags,
    MalformedTagTable,
    MissingRequiredOutputTransforms,
    MissingRequiredOutputTags,
    NotPrinter,
    NotCmyk,
    UnsupportedPcs,
}

/// Cheap framing, header and role checks, before any untrusted body scan.
///
/// # Errors
/// Returns the first malformed container or unsupported output-profile field.
pub fn screen_output(profile: &[u8]) -> Result<(), Error> {
    validate_header(profile)?;
    validate_tag_table_with(profile, |_, _, _| Ok(()))
}

/// Complete byte checks. Call this inside the confined preparation worker.
///
/// # Errors
/// Returns an error for malformed retained standard data or a mismatched ID.
pub fn inspect_output(profile: &[u8]) -> Result<(), Error> {
    validate_header(profile)?;
    validate_tag_table(profile)?;
    validate_profile_id(profile)
}

fn validate_header(p: &[u8]) -> Result<(), Error> {
    if p.len() > MAX_PROFILE_BYTES {
        return Err(Error::TooLarge);
    }
    if p.len() < 132 {
        return Err(Error::TooShort);
    }
    if profile_u32(p, 0) != Some(p.len()) {
        return Err(Error::DeclaredSizeMismatch);
    }
    if &p[36..40] != b"acsp" {
        return Err(Error::BadSignature);
    }
    let v: [u8; 4] = p[8..12].try_into().map_err(|_| Error::TooShort)?;
    if v[..2].iter().any(|b| b >> 4 > 9 || b & 15 > 9) || v[2..] != [0, 0] {
        return Err(Error::InvalidVersionEncoding(v));
    }
    if !matches!(v, [2, 0x20, 0, 0] | [4, 0 | 0x10 | 0x20, 0, 0]) {
        return Err(Error::UnsupportedVersion(v));
    }
    if &p[12..16] != b"prtr" {
        return Err(Error::NotPrinter);
    }
    if &p[16..20] != b"CMYK" {
        return Err(Error::NotCmyk);
    }
    if !matches!(&p[20..24], b"Lab " | b"XYZ ") {
        return Err(Error::UnsupportedPcs);
    }
    let reserved = if v[0] == 2 { 84 } else { 100 };
    // Vendor flags occupy the upper halves. Only the defined low bits belong
    // to this format revision; vendor fields must remain intact.
    let attribute_mask = if v[0] == 2 { 3 } else { 15 };
    if p[reserved..128].iter().any(|b| *b != 0)
        || profile_u32(p, 44).is_none_or(|n| n & 0xfffc != 0)
        || profile_u32(p, 60).is_none_or(|n| n & !attribute_mask != 0)
        || profile_u32(p, 64).is_none_or(|n| (n & 0xffff) > 3 || (v == [4, 0x20, 0, 0] && n > 3))
        || p[68..80] != [0, 0, 0xf6, 0xd6, 0, 1, 0, 0, 0, 0, 0xd3, 0x2d]
        || !valid_date(&p[24..36])
    {
        return Err(Error::Header);
    }
    if !matches!(
        &p[40..44],
        b"\0\0\0\0" | b"APPL" | b"MSFT" | b"SGI " | b"SUNW"
    ) && !((p[8] == 2 || p[9] <= 0x10) && &p[40..44] == b"TGNT")
    {
        return Err(Error::Header);
    }
    Ok(())
}

fn valid_date(p: &[u8]) -> bool {
    if p.len() != 12 {
        return false;
    }
    let parts: Vec<_> = p
        .as_chunks::<2>()
        .0
        .iter()
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .collect();
    let (y, m, d) = (parts[0], parts[1], parts[2]);
    let leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let days = match m {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => 0,
    };
    y != 0 && d != 0 && d <= days && parts[3] <= 23 && parts[4] <= 59 && parts[5] <= 59
}

fn validate_profile_id(p: &[u8]) -> Result<(), Error> {
    if p[8] == 2 || p[84..100] == [0; 16] {
        return Ok(());
    }
    let mut digest = Md5::new();
    // The oldest supported v4 edition masks attributes. Later editions mask
    // flags instead. A zero ID retains its specified "not calculated" meaning.
    let mask = if p[9] == 0 { 56..64 } else { 44..48 };
    digest.update(&p[..mask.start]);
    digest.update(vec![0; mask.len()]);
    digest.update(&p[mask.end..64]);
    digest.update([0; 4]);
    digest.update(&p[68..84]);
    digest.update([0; 16]);
    digest.update(&p[100..]);
    if digest.finalize()[..] != p[84..100] {
        return Err(Error::ProfileId);
    }
    Ok(())
}

fn validate_tag_table(profile: &[u8]) -> Result<(), Error> {
    validate_tag_table_with(profile, validate_tag_payload)
}

/// Visit each distinct body after every directory and role check succeeds.
///
/// # Errors
/// Returns an invalid directory, missing required role, or body visitor error.
///
/// # Panics
/// The fixed slices are guarded by container bounds; no caller input panics.
#[allow(clippy::too_many_lines)] // One directory-first pass is the work-bound invariant.
pub fn validate_tag_table_with(
    profile: &[u8],
    mut validate_payload: impl FnMut(&[u8], &[u8], &[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    let count_end = ICC_HEADER_LEN
        .checked_add(ICC_TAG_COUNT_LEN)
        .ok_or(Error::MalformedTagTable)?;
    if profile.len() < count_end {
        return Err(Error::TooShort);
    }
    let tag_count = u32::from_be_bytes(
        profile[ICC_HEADER_LEN..count_end]
            .try_into()
            .expect("four byte slice"),
    );
    let tag_count = usize::try_from(tag_count).map_err(|_| Error::MalformedTagTable)?;
    if tag_count == 0 {
        return Err(Error::MissingTags);
    }
    let table_bytes = tag_count
        .checked_mul(ICC_TAG_RECORD_LEN)
        .ok_or(Error::MalformedTagTable)?;
    let table_end = count_end
        .checked_add(table_bytes)
        .ok_or(Error::MalformedTagTable)?;
    if table_end > profile.len() {
        return Err(Error::MalformedTagTable);
    }

    let mut has_device_to_pcs = false;
    let mut has_pcs_to_device = false;
    let mut signatures = std::collections::BTreeSet::new();
    let mut records = Vec::with_capacity(tag_count);
    for index in 0..tag_count {
        let record_start = count_end + index * ICC_TAG_RECORD_LEN;
        let signature = &profile[record_start..record_start + 4];
        let offset = u32::from_be_bytes(
            profile[record_start + 4..record_start + 8]
                .try_into()
                .expect("four byte slice"),
        );
        let size = u32::from_be_bytes(
            profile[record_start + 8..record_start + 12]
                .try_into()
                .expect("four byte slice"),
        );
        let offset = usize::try_from(offset).map_err(|_| Error::MalformedTagTable)?;
        let size = usize::try_from(size).map_err(|_| Error::MalformedTagTable)?;
        let end = offset.checked_add(size).ok_or(Error::MalformedTagTable)?;
        if offset < table_end || offset % 4 != 0 || size < 8 || end > profile.len() {
            return Err(Error::MalformedTagTable);
        }
        if !signatures.insert(signature) {
            return Err(Error::MalformedTagTable);
        }
        records.push((signature, offset, end));
        has_device_to_pcs |= signature == b"A2B0";
        has_pcs_to_device |= signature == b"B2A0";
    }

    if !has_device_to_pcs || !has_pcs_to_device {
        return Err(Error::MissingRequiredOutputTransforms);
    }
    // Every admitted edition requires the printer's description, copyright,
    // media white point, gamut and all three forward/reverse intent tables.
    // A native engine's ability to build one transform does not prove these
    // required resources exist in the exact profile being embedded.
    if [
        b"desc", b"cprt", b"wtpt", b"gamt", b"A2B0", b"A2B1", b"A2B2", b"B2A0", b"B2A1", b"B2A2",
    ]
    .iter()
    .any(|signature| !signatures.contains(signature.as_slice()))
    {
        return Err(Error::MissingRequiredOutputTags);
    }
    records.sort_unstable_by_key(|record| (record.1, record.2));
    // Complete aliases are valid; reject partial overlaps before payload scans.
    if records.windows(2).any(|pair| {
        let left = (pair[0].1, pair[0].2);
        let right = (pair[1].1, pair[1].2);
        left != right && left.1 > right.0
    }) {
        return Err(Error::MalformedTagTable);
    }
    if profile[8] == 4 {
        let mut end = table_end;
        let mut previous = None;
        for &(_, start, stop) in &records {
            if previous == Some((start, stop)) {
                continue;
            }
            if start != (end + 3) & !3 {
                return Err(Error::MalformedTagTable);
            }
            if profile[end..start].iter().any(|byte| *byte != 0) {
                return Err(Error::MalformedTagTable);
            }
            end = stop;
            previous = Some((start, stop));
        }
        if profile.len() != (end + 3) & !3 {
            return Err(Error::MalformedTagTable);
        }
        if profile[end..].iter().any(|byte| *byte != 0) {
            return Err(Error::MalformedTagTable);
        }
    }
    for &(signature, offset, end) in &records {
        validate_tag_role(signature, &profile[offset..end], profile)?;
    }
    if profile[8] == 4 {
        let illuminance_y = |tag: &[u8; 4]| {
            records.iter().find_map(|&(signature, offset, _)| {
                (signature == tag).then(|| &profile[offset + 12..offset + 16])
            })
        };
        if let (Some(view), Some(luminance)) = (illuminance_y(b"view"), illuminance_y(b"lumi"))
            && view != luminance
        {
            return Err(Error::MalformedTagTable);
        }
    }
    if profile[8] == 4
        && !signatures.contains(b"chad".as_slice())
        && records.iter().any(|(signature, start, end)| {
            *signature == b"meas"
                && end - start == 36
                && profile_u32(&profile[*start..*end], 32).is_some_and(|v| (2..=8).contains(&v))
        })
    {
        return Err(Error::MissingRequiredOutputTags);
    }
    let mut previous_range = None;
    for (signature, offset, end) in records {
        if previous_range != Some((offset, end)) {
            validate_payload(signature, &profile[offset..end], profile)?;
            previous_range = Some((offset, end));
        }
    }
    Ok(())
}

fn validate_tag_role(signature: &[u8], payload: &[u8], profile: &[u8]) -> Result<(), Error> {
    let malformed = Error::MalformedTagTable;
    let kind = &payload[..4];
    let expected = match signature {
        b"desc" | b"dmnd" | b"dmdd" | b"vued" => {
            Some(if profile[8] == 2 { b"desc" } else { b"mluc" }.as_slice())
        }
        b"cprt" => Some(if profile[8] == 2 { b"text" } else { b"mluc" }.as_slice()),
        b"wtpt" | b"bkpt" | b"rXYZ" | b"gXYZ" | b"bXYZ" | b"lumi" => Some(b"XYZ ".as_slice()),
        b"chad" if profile[8] == 4 => Some(b"sf32".as_slice()),
        b"calt" => Some(b"dtim".as_slice()),
        b"targ" => Some(b"text".as_slice()),
        b"meas" => Some(b"meas".as_slice()),
        b"view" => Some(b"view".as_slice()),
        b"tech" => Some(b"sig ".as_slice()),
        b"ncl2" => Some(b"ncl2".as_slice()),
        b"resp" => Some(b"rcs2".as_slice()),
        b"pseq" => Some(b"pseq".as_slice()),
        b"chrm" if profile[8] == 4 => Some(b"chrm".as_slice()),
        b"clro" if profile[8] == 4 => Some(b"clro".as_slice()),
        b"clrt" | b"clot" if profile[8] == 4 => Some(b"clrt".as_slice()),
        b"crdi" if profile[8] == 2 || profile[9] == 0 => Some(b"crdi".as_slice()),
        b"devs" if profile[8] == 2 || profile[9] == 0 => Some(b"devs".as_slice()),
        b"scrd" if profile[8] == 2 || profile[9] == 0 => {
            Some(if profile[8] == 2 { b"desc" } else { b"mluc" }.as_slice())
        }
        b"scrn" if profile[8] == 2 || profile[9] == 0 => Some(b"scrn".as_slice()),
        b"bfd " if profile[8] == 2 || profile[9] == 0 => Some(b"bfd ".as_slice()),
        b"ncol" if profile[8] == 2 => Some(b"ncol".as_slice()),
        b"psd0" | b"psd1" | b"psd2" | b"psd3" | b"ps2s" | b"ps2i"
            if profile[8] == 2 || profile[9] == 0 =>
        {
            Some(b"data".as_slice())
        }
        _ => None,
    };
    if expected.is_some_and(|expected| kind != expected)
        || (matches!(
            signature,
            b"wtpt" | b"bkpt" | b"rXYZ" | b"gXYZ" | b"bXYZ" | b"lumi"
        ) && payload.len() != 20)
        || (signature == b"view" && payload.len() != 36)
        || (profile[8] == 4 && signature == b"chad" && payload.len() != 44)
        || (matches!(signature, b"rTRC" | b"gTRC" | b"bTRC" | b"kTRC")
            && kind != b"curv"
            && (profile[8] != 4 || kind != b"para"))
    {
        return Err(malformed);
    }
    if profile[8] == 4 && signature == b"chad" {
        adaptation_matrix(payload)?;
    }
    if signature == b"tech" && (payload.len() != 12 || !technology(&payload[8..12])) {
        return Err(malformed);
    }
    if signature == b"targ" {
        characterization_target(payload)?;
    }
    let (channels, v4_kind) = match signature {
        b"A2B0" | b"A2B1" | b"A2B2" => (Some((4, 3)), b"mAB "),
        b"B2A0" | b"B2A1" | b"B2A2" => (Some((3, 4)), b"mBA "),
        b"gamt" => (Some((3, 1)), b"mBA "),
        b"pre0" | b"pre1" | b"pre2" => (Some((3, 3)), b"mBA "),
        _ => (None, b"    "),
    };
    if channels.is_some()
        && kind != b"mft1"
        && kind != b"mft2"
        && (profile[8] != 4 || kind != v4_kind)
    {
        return Err(malformed);
    }
    match kind {
        b"mft1" | b"mft2" if channels.is_some() => validate_lut_role(signature, payload, profile)?,
        b"mAB " | b"mBA "
            if profile[8] == 4
                && (payload.len() < 32
                    || channels.is_some_and(|expected| {
                        expected != (usize::from(payload[8]), usize::from(payload[9]))
                    })) =>
        {
            return Err(malformed);
        }
        _ => {}
    }
    Ok(())
}

/// Check one distinct data range. Directory and role checks precede this scan.
///
/// # Errors
/// Returns an error for an invalid known standard payload.
///
/// # Panics
/// Fixed-width conversions follow checked extents and cannot panic for input.
#[allow(clippy::too_many_lines)]
pub fn validate_tag_payload(signature: &[u8], p: &[u8], profile: &[u8]) -> Result<(), Error> {
    let malformed = Error::MalformedTagTable;
    if p.len() < 8 || profile.len() < 24 || p[4..8] != [0; 4] {
        return Err(malformed);
    }
    if !standard_type(&p[..4], profile) {
        return Ok(());
    }
    match &p[..4] {
        b"XYZ " => {
            if p.len() < 20 || !(p.len() - 8).is_multiple_of(12) || !xyz_nonnegative(&p[8..]) {
                return Err(malformed);
            }
        }
        b"text" => ascii_text(&p[8..])?,
        b"desc" => {
            if description_extent(p)? != p.len() {
                return Err(malformed);
            }
        }
        b"mluc" => {
            if mluc_extent(p, profile[9] != 0x20)? != p.len() {
                return Err(malformed);
            }
        }
        b"curv" | b"para" => {
            if curve_extent(p)? != p.len() {
                return Err(malformed);
            }
        }
        b"mft1" | b"mft2" => validate_lut_body(p)?,
        b"mAB " | b"mBA " => validate_multistage_lut(signature, p)?,
        b"sf32" | b"uf32" | b"ui32" => array(p, 4)?,
        b"ui16" => array(p, 2)?,
        b"ui64" => array(p, 8)?,
        b"sig " => {
            if p.len() != 12 {
                return Err(malformed);
            }
        }
        b"dtim" => {
            if p.len() != 20 || !valid_date(&p[8..]) {
                return Err(malformed);
            }
        }
        b"data" => match profile_u32(p, 8).ok_or(malformed)? {
            0 => ascii_text(&p[12..])?,
            1 => {}
            _ => return Err(malformed),
        },
        b"meas" => {
            if p.len() != 36
                || profile_u32(p, 8).is_none_or(|v| v > 2)
                || profile_u32(p, 24).is_none_or(|v| v > 2)
                || profile_u32(p, 28).is_none_or(|v| v > 65536)
                || profile_u32(p, 32).is_none_or(|v| v > 8)
                || !xyz_nonnegative(&p[12..24])
            {
                return Err(malformed);
            }
        }
        b"view" => {
            if p.len() != 36
                || !xyz_nonnegative(&p[8..32])
                || profile_u32(p, 32).is_none_or(|v| v > 8)
            {
                return Err(malformed);
            }
        }
        b"chrm" => chromaticity(p)?,
        b"clro" => {
            let n = profile_u32(p, 8).ok_or(malformed)?;
            if p.len() != 12 + n
                || n != 4
                || p[12..].iter().any(|v| usize::from(*v) >= n)
                || p[12..]
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != n
            {
                return Err(malformed);
            }
        }
        b"clrt" => colorant_table(p, profile)?,
        b"ncl2" => named_colors(p, profile)?,
        b"ncol" => legacy_named_colors(p)?,
        b"pseq" => profile_sequence(p, profile)?,
        b"rcs2" => response_curves(p)?,
        b"scrn" => screening(p)?,
        b"bfd " => ucr_bg(p)?,
        b"crdi" => crd_info(p)?,
        b"devs" => device_settings(p)?,
        _ => {} // Registered private types have no public proprietary schema.
    }
    Ok(())
}

fn standard_type(kind: &[u8], profile: &[u8]) -> bool {
    match kind {
        b"curv" | b"data" | b"dtim" | b"mft1" | b"mft2" | b"meas" | b"ncl2" | b"pseq" | b"rcs2"
        | b"sf32" | b"sig " | b"text" | b"uf32" | b"ui16" | b"ui32" | b"ui64" | b"ui08"
        | b"view" | b"XYZ " => true,
        b"desc" | b"ncol" => profile[8] == 2,
        b"devs" | b"crdi" | b"scrn" | b"bfd " => profile[8] == 2 || profile[9] == 0,
        b"chrm" | b"clro" | b"clrt" | b"mAB " | b"mBA " | b"mluc" | b"para" => profile[8] == 4,
        _ => false,
    }
}

fn adaptation_matrix(p: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    if p.len() != 44 || &p[..4] != b"sf32" {
        return Err(e);
    }
    let m: Vec<i128> = p[8..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|n| i128::from(i32::from_be_bytes(*n)))
        .collect();
    if m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
        + m[2] * (m[3] * m[7] - m[4] * m[6])
        == 0
    {
        return Err(e);
    }
    Ok(())
}

fn technology(value: &[u8]) -> bool {
    matches!(
        value,
        b"\0\0\0\0"
            | b"fscn"
            | b"dcam"
            | b"rscn"
            | b"ijet"
            | b"twax"
            | b"epho"
            | b"esta"
            | b"dsub"
            | b"rpho"
            | b"fprn"
            | b"vidm"
            | b"vidc"
            | b"pjtv"
            | b"CRT "
            | b"PMD "
            | b"AMD "
            | b"KPCD"
            | b"imgs"
            | b"grav"
            | b"offs"
            | b"silk"
            | b"flex"
    )
}

fn characterization_target(p: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    if p.len() < 16 || !p[8..15].iter().all(|v| (32..=126).contains(v)) {
        return Err(e);
    }
    if &p[8..15] == b"ICCHDAT" && (p.len() < 18 || p[15] != b' ' || p[16..p.len() - 1].contains(&0))
    {
        return Err(e);
    }
    // Reference-name registration is external. Other declared identifiers carry
    // their referenced interchange-format obligations, qualified separately.
    Ok(())
}

fn array(p: &[u8], stride: usize) -> Result<(), Error> {
    if !(p.len() - 8).is_multiple_of(stride) {
        return Err(Error::MalformedTagTable);
    }
    Ok(())
}
fn ascii_text(p: &[u8]) -> Result<(), Error> {
    if p.last() != Some(&0) || !p.is_ascii() {
        return Err(Error::MalformedTagTable);
    }
    Ok(())
}
fn xyz_nonnegative(p: &[u8]) -> bool {
    p.as_chunks::<4>()
        .0
        .iter()
        .all(|n| i32::from_be_bytes(*n) >= 0)
}
fn fixed_ascii(p: &[u8], zero_padding: bool) -> Result<(), Error> {
    let end = p
        .iter()
        .position(|b| *b == 0)
        .ok_or(Error::MalformedTagTable)?;
    if !p[..end].is_ascii() || (zero_padding && p[end..].iter().any(|b| *b != 0)) {
        return Err(Error::MalformedTagTable);
    }
    Ok(())
}
fn utf16(p: &[u8]) -> Result<(), Error> {
    if !p.len().is_multiple_of(2) {
        return Err(Error::MalformedTagTable);
    }
    let chars = p
        .as_chunks::<2>()
        .0
        .iter()
        .map(|n| u16::from_be_bytes([n[0], n[1]]));
    if char::decode_utf16(chars).any(|c| c.is_err()) {
        return Err(Error::MalformedTagTable);
    }
    Ok(())
}
fn description_extent(p: &[u8]) -> Result<usize, Error> {
    let e = Error::MalformedTagTable;
    let n = profile_u32(p, 8).ok_or(e)?;
    let end = 12_usize.checked_add(n).filter(|v| *v <= p.len()).ok_or(e)?;
    if n == 0 {
        return Err(e);
    }
    ascii_text(&p[12..end])?;
    let un = profile_u32(p, end + 4).ok_or(e)?;
    let ue = end
        .checked_add(8)
        .and_then(|v| un.checked_mul(2).and_then(|n| v.checked_add(n)))
        .filter(|v| *v <= p.len())
        .ok_or(e)?;
    utf16(&p[end + 8..ue])?;
    if un > 0 && p[ue - 2..ue] != [0, 0] {
        return Err(e);
    }
    let size = ue.checked_add(70).filter(|v| *v <= p.len()).ok_or(e)?;
    let script_n = usize::from(p[ue + 2]);
    if script_n > 67 || (script_n > 0 && p[ue + 3 + script_n - 1] != 0) {
        return Err(e);
    }
    Ok(size)
}
fn mluc_extent(p: &[u8], require_utf16: bool) -> Result<usize, Error> {
    // The older editions specify UTF-16BE. The later encoding recommendation
    // has no discriminator field: actual Unicode encoding/context is a named
    // valid-resource precondition there, while every storage range stays checked.
    let e = Error::MalformedTagTable;
    if p.len() < 16 || &p[..4] != b"mluc" || p[4..8] != [0; 4] {
        return Err(e);
    }
    let count = profile_u32(p, 8).ok_or(e)?;
    let stride = profile_u32(p, 12).ok_or(e)?;
    if count > 0 && stride < 12 {
        return Err(e);
    }
    let table = count
        .checked_mul(stride)
        .and_then(|n| n.checked_add(16))
        .filter(|n| *n <= p.len())
        .ok_or(e)?;
    let mut spans = [Vec::new(), Vec::new()];
    let mut extent = table;
    for index in 0..count {
        let record = &p[16 + index * stride..16 + index * stride + 12];
        let length = profile_u32(record, 4).ok_or(e)?;
        let offset = profile_u32(record, 8).ok_or(e)?;
        let end = offset
            .checked_add(length)
            .filter(|n| *n <= p.len())
            .ok_or(e)?;
        if offset < table || (require_utf16 && !length.is_multiple_of(2)) {
            return Err(e);
        }
        if require_utf16 && length > 0 {
            let first = u16::from_be_bytes([p[offset], p[offset + 1]]);
            let last = u16::from_be_bytes([p[end - 2], p[end - 1]]);
            // Omitting a final Unicode NUL is a recommendation, not a ban.
            if (0xdc00..=0xdfff).contains(&first) || (0xd800..=0xdbff).contains(&last) {
                return Err(e);
            }
        }
        extent = extent.max(end);
        if require_utf16 {
            spans[offset % 2].push((offset, end));
        }
    }
    // Shared substrings are legal. Merge code-unit spans with equal parity;
    // every storage byte is decoded at most twice, regardless of record count.
    for mut ranges in spans {
        ranges.sort_unstable();
        let mut merged: Option<(usize, usize)> = None;
        for (start, end) in ranges {
            match merged {
                Some((begin, stop)) if start <= stop => merged = Some((begin, stop.max(end))),
                Some((begin, stop)) => {
                    utf16(&p[begin..stop])?;
                    merged = Some((start, end));
                }
                None => merged = Some((start, end)),
            }
        }
        if let Some((start, end)) = merged {
            utf16(&p[start..end])?;
        }
    }
    Ok(extent)
}

#[allow(clippy::many_single_char_names)]
#[allow(clippy::cast_precision_loss)] // Active fixed-point products fit within 49 bits.
fn curve_extent(p: &[u8]) -> Result<usize, Error> {
    let e = Error::MalformedTagTable;
    if p.len() < 12 || p[4..8] != [0; 4] {
        return Err(e);
    }
    match &p[..4] {
        b"curv" => profile_u32(p, 8)
            .and_then(|n| n.checked_mul(2))
            .and_then(|n| n.checked_add(12))
            .filter(|n| *n <= p.len())
            .ok_or(e),
        b"para" => {
            let function = usize::from(u16::from_be_bytes([p[8], p[9]]));
            if function > 4 || p[10..12] != [0; 2] {
                return Err(e);
            }
            let end = 12 + 4 * [1, 3, 4, 5, 7][function];
            if end > p.len() {
                return Err(e);
            }
            let v: Vec<i32> = p[12..end]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|n| i32::from_be_bytes(*n))
                .collect();
            let g = v[0];
            if function == 0 {
                if g < 0 {
                    return Err(e);
                }
            } else {
                let (a, b) = (i64::from(v[1]), i64::from(v[2]));
                if function <= 2 && a == 0 {
                    return Err(e);
                }
                let threshold = if function <= 2 {
                    -f64::from(v[2]) / f64::from(v[1])
                } else {
                    f64::from(v[4]) / 65536.0
                };
                if threshold <= 1.0 {
                    // Exact fixed-point signs distinguish an interior pole
                    // from finite endpoint powers. The affine base has no
                    // other interior extrema on this closed active interval.
                    let first = if function <= 2 {
                        if threshold > 0.0 { 0 } else { b * 65536 }
                    } else {
                        a * i64::from(v[4].max(0)) + b * 65536
                    };
                    let last = (a + b) * 65536;
                    let (minimum, maximum) = (first.min(last), first.max(last));
                    if (g < 0 && minimum <= 0 && maximum >= 0) || (g % 65536 != 0 && minimum < 0) {
                        return Err(e);
                    }
                    for base in [first, last] {
                        if !(base as f64 / 4_294_967_296.0)
                            .powf(f64::from(g) / 65536.0)
                            .is_finite()
                        {
                            return Err(e);
                        }
                    }
                }
            }
            Ok(end)
        }
        _ => Err(e),
    }
}
fn validate_lut_role(signature: &[u8], p: &[u8], profile: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    if p.len() < 48 {
        return Err(e);
    }
    let expected = match signature {
        b"A2B0" | b"A2B1" | b"A2B2" => Some((4, 3)),
        b"B2A0" | b"B2A1" | b"B2A2" => Some((3, 4)),
        b"gamt" => Some((3, 1)),
        b"pre0" | b"pre1" | b"pre2" => Some((3, 3)),
        _ => None,
    };
    if expected.is_some_and(|v| v != (p[8], p[9])) {
        return Err(e);
    }
    let xyz_input =
        &profile[20..24] == b"XYZ " && !matches!(signature, b"A2B0" | b"A2B1" | b"A2B2");
    if !xyz_input {
        for i in 0..9 {
            if profile_u32(p, 12 + i * 4) != Some(if i % 4 == 0 { 65536 } else { 0 }) {
                return Err(e);
            }
        }
    }
    Ok(())
}
fn validate_lut_body(p: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    if p.len() < 48 {
        return Err(e);
    }
    let (inputs, outputs, grid) = (usize::from(p[8]), usize::from(p[9]), usize::from(p[10]));
    if !(1..=15).contains(&inputs) || !(1..=15).contains(&outputs) || grid < 2 || p[11] != 0 {
        return Err(e);
    }
    let (head, ni, no, bytes) = if &p[..4] == b"mft1" {
        (48, 256, 256, 1)
    } else {
        if p.len() < 52 {
            return Err(e);
        }
        let ni = usize::from(u16::from_be_bytes([p[48], p[49]]));
        let no = usize::from(u16::from_be_bytes([p[50], p[51]]));
        if !(2..=4096).contains(&ni) || !(2..=4096).contains(&no) {
            return Err(e);
        }
        (52, ni, no, 2)
    };
    let extent = grid
        .checked_pow(u32::try_from(inputs).map_err(|_| e)?)
        .and_then(|n| n.checked_mul(outputs))
        .and_then(|n| n.checked_add(inputs * ni + outputs * no))
        .and_then(|n| n.checked_mul(bytes))
        .and_then(|n| n.checked_add(head));
    if extent != Some(p.len()) {
        return Err(e);
    }
    Ok(())
}

fn chromaticity(p: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    if p.len() < 12 {
        return Err(e);
    }
    let n = usize::from(u16::from_be_bytes([p[8], p[9]]));
    let kind = usize::from(u16::from_be_bytes([p[10], p[11]]));
    if n == 0 || p.len() != 12 + 8 * n || kind > 4 {
        return Err(e);
    }
    if kind != 0 {
        if n != 3 {
            return Err(e);
        }
        let primaries = [
            [0.640, 0.330, 0.300, 0.600, 0.150, 0.060],
            [0.630, 0.340, 0.310, 0.595, 0.155, 0.070],
            [0.640, 0.330, 0.290, 0.600, 0.150, 0.060],
            [0.625, 0.340, 0.280, 0.605, 0.155, 0.070],
        ];
        for (i, v) in primaries[kind - 1].iter().enumerate() {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let expected = (v * 65536.0_f64).round() as usize;
            if profile_u32(p, 12 + 4 * i) != Some(expected) {
                return Err(e);
            }
        }
    }
    Ok(())
}

fn colorant_table(p: &[u8], profile: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    let n = profile_u32(p, 8).ok_or(e)?;
    if n != 4 || n.checked_mul(38).and_then(|n| n.checked_add(12)) != Some(p.len()) {
        return Err(e);
    }
    let mut names = std::collections::BTreeSet::new();
    for entry in p[12..].as_chunks::<38>().0 {
        fixed_ascii(&entry[..32], true)?;
        let len = entry[..32].iter().position(|v| *v == 0).ok_or(e)?;
        if !names.insert(&entry[..len]) {
            return Err(e);
        }
        if &profile[20..24] == b"Lab "
            && profile[8] == 2
            && u16::from_be_bytes([entry[32], entry[33]]) > 65280
        {
            return Err(e);
        }
    }
    Ok(())
}

fn named_colors(p: &[u8], _profile: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    if p.len() < 84 || profile_u32(p, 8).is_none_or(|n| n & 0xffff != 0) {
        return Err(e);
    }
    let n = profile_u32(p, 12).ok_or(e)?;
    let m = profile_u32(p, 16).ok_or(e)?;
    if m != 0 && m != 4 {
        return Err(e);
    }
    let size = 38 + 2 * m;
    if n.checked_mul(size).and_then(|n| n.checked_add(84)) != Some(p.len()) {
        return Err(e);
    }
    fixed_ascii(&p[20..52], false)?;
    fixed_ascii(&p[52..84], false)?;
    for item in p[84..].chunks_exact(size) {
        fixed_ascii(&item[..32], false)?;
    }
    Ok(())
}

fn legacy_named_colors(p: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    if p.len() < 12 || profile_u32(p, 8).is_none_or(|n| n & 0xffff != 0) {
        return Err(e);
    }
    // Legacy names are variable, NUL-delimited ASCII strings followed by
    // four device-coordinate bytes. Counts and names are decoded independently.
    let n = profile_u32(p, 12).ok_or(e)?;
    let mut at = 16;
    for _ in 0..2 {
        let tail = p.get(at..).ok_or(e)?;
        let end = tail.iter().position(|v| *v == 0).ok_or(e)?;
        if end > 31 || !tail[..end].is_ascii() {
            return Err(e);
        }
        at += end + 1;
    }
    for _ in 0..n {
        let tail = p.get(at..).ok_or(e)?;
        let end = tail.iter().position(|v| *v == 0).ok_or(e)?;
        if end > 31 || !tail[..end].is_ascii() {
            return Err(e);
        }
        at = at.checked_add(end + 5).filter(|n| *n <= p.len()).ok_or(e)?;
    }
    if at != p.len() {
        return Err(e);
    }
    Ok(())
}

fn profile_sequence(p: &[u8], profile: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    let count = profile_u32(p, 8).ok_or(e)?;
    let mut at = 12_usize;
    if count > (p.len() - 12) / 20 {
        return Err(e);
    }
    for _ in 0..count {
        let item = p.get(at..at + 20).ok_or(e)?;
        let mask = if profile[8] == 2 { 3 } else { 15 };
        if profile_u32(item, 12).is_none_or(|v| v & !mask != 0) {
            return Err(e);
        }
        at += 20;
        for _ in 0..2 {
            let tail = p.get(at..).ok_or(e)?;
            if tail.len() < 8 || tail[4..8] != [0; 4] {
                return Err(e);
            }
            let n = if profile[8] == 2 {
                if &tail[..4] != b"desc" {
                    return Err(e);
                }
                description_extent(tail)?
            } else {
                mluc_extent(tail, profile[9] != 0x20)?
            };
            at = at.checked_add(n).ok_or(e)?;
        }
    }
    if at != p.len() {
        return Err(e);
    }
    Ok(())
}

fn response_curves(p: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    if p.len() < 12 {
        return Err(e);
    }
    let channels = usize::from(u16::from_be_bytes([p[8], p[9]]));
    let count = usize::from(u16::from_be_bytes([p[10], p[11]]));
    if channels != 4 || count == 0 {
        return Err(e);
    }
    let table = 12 + 4 * count;
    if table > p.len() {
        return Err(e);
    }
    let mut ranges = Vec::with_capacity(count);
    let mut units = std::collections::BTreeSet::new();
    for i in 0..count {
        let start = profile_u32(p, 12 + i * 4).ok_or(e)?;
        let head = start
            .checked_add(4 + 16 * channels)
            .filter(|n| *n <= p.len())
            .ok_or(e)?;
        if start < table || !start.is_multiple_of(4) || !units.insert(&p[start..start + 4]) {
            return Err(e);
        }
        if !matches!(
            &p[start..start + 4],
            b"StaA" | b"StaE" | b"StaI" | b"StaT" | b"StaM" | b"DN  " | b"DN P" | b"DNN " | b"DNNP"
        ) {
            return Err(e);
        }
        if !xyz_nonnegative(&p[start + 4 + 4 * channels..head]) {
            return Err(e);
        }
        let mut end = head;
        for c in 0..channels {
            let n = profile_u32(p, start + 4 + c * 4).ok_or(e)?;
            let stop = n
                .checked_mul(8)
                .and_then(|n| end.checked_add(n))
                .filter(|n| *n <= p.len())
                .ok_or(e)?;
            let mut previous = None;
            for entry in p[end..stop].as_chunks::<8>().0 {
                let code = u16::from_be_bytes([entry[0], entry[1]]);
                if entry[2..4] != [0, 0] || previous.is_some_and(|v| code <= v) {
                    return Err(e);
                }
                previous = Some(code);
            }
            end = stop;
        }
        ranges.push((start, end));
    }
    ranges.sort_unstable();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0)
        || ranges.last().is_none_or(|r| r.1 != p.len())
    {
        return Err(e);
    }
    Ok(())
}

fn screening(p: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    let n = profile_u32(p, 12).ok_or(e)?;
    if profile_u32(p, 8).is_none_or(|v| v & 0xfffc != 0)
        || n.checked_mul(12).and_then(|n| n.checked_add(16)) != Some(p.len())
    {
        return Err(e);
    }
    for item in p[16..].as_chunks::<12>().0 {
        if profile_u32(item, 8).is_none_or(|v| v > 7) {
            return Err(e);
        }
    }
    Ok(())
}

fn ucr_bg(p: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    let mut at = 8_usize;
    for _ in 0..2 {
        let n = profile_u32(p, at).ok_or(e)?;
        at = n
            .checked_mul(2)
            .and_then(|n| at.checked_add(4 + n))
            .filter(|v| *v <= p.len())
            .ok_or(e)?;
    }
    ascii_text(&p[at..])
}

fn crd_info(p: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    let mut at = 8_usize;
    for _ in 0..5 {
        let n = profile_u32(p, at).ok_or(e)?;
        let end = at
            .checked_add(4)
            .and_then(|v| v.checked_add(n))
            .filter(|v| *v <= p.len())
            .ok_or(e)?;
        if n > 0 {
            ascii_text(&p[at + 4..end])?;
        }
        at = end;
    }
    if at != p.len() {
        return Err(e);
    }
    Ok(())
}

fn device_settings(p: &[u8]) -> Result<(), Error> {
    let e = Error::MalformedTagTable;
    let count = profile_u32(p, 8).ok_or(e)?;
    let mut at = 12_usize;
    if count > (p.len() - 12) / 12 {
        return Err(e);
    }
    for _ in 0..count {
        let size = profile_u32(p, at + 4).ok_or(e)?;
        let n = profile_u32(p, at + 8).ok_or(e)?;
        let end = at.checked_add(size).filter(|v| *v <= p.len()).ok_or(e)?;
        if size < 12 {
            return Err(e);
        }
        let platform = p.get(at..at + 4).ok_or(e)?;
        let mut next = at + 12;
        if n > (end - next) / 8 {
            return Err(e);
        }
        for _ in 0..n {
            let combo_size = profile_u32(p, next).ok_or(e)?;
            let settings = profile_u32(p, next + 4).ok_or(e)?;
            let stop = next
                .checked_add(combo_size)
                .filter(|v| *v <= end)
                .ok_or(e)?;
            if combo_size < 8 || settings > (combo_size - 8) / 12 {
                return Err(e);
            }
            let mut setting = next + 8;
            for _ in 0..settings {
                let width = profile_u32(p, setting + 4).ok_or(e)?;
                let values = profile_u32(p, setting + 8).ok_or(e)?;
                let limit = width
                    .checked_mul(values)
                    .and_then(|n| setting.checked_add(12 + n))
                    .filter(|v| *v <= stop)
                    .ok_or(e)?;
                if matches!(platform, b"MSFT" | b"msft") {
                    match &p[setting..setting + 4] {
                        b"rsln" if width != 8 => return Err(e),
                        b"mtyp" | b"hftn" if width != 4 => return Err(e),
                        _ => {}
                    }
                    if &p[setting..setting + 4] == b"mtyp"
                        && p[setting + 12..limit].as_chunks::<4>().0.iter().any(|v| {
                            profile_u32(v, 0).is_none_or(|n| !(0..=3).contains(&n) && n < 256)
                        })
                    {
                        return Err(e);
                    }
                    if &p[setting..setting + 4] == b"hftn"
                        && p[setting + 12..limit].as_chunks::<4>().0.iter().any(|v| {
                            profile_u32(v, 0).is_none_or(|n| !(0..=10).contains(&n) && n < 256)
                        })
                    {
                        return Err(e);
                    }
                }
                setting = limit;
            }
            if setting != stop {
                return Err(e);
            }
            next = stop;
        }
        if next != end {
            return Err(e);
        }
        at = end;
    }
    if at != p.len() {
        return Err(e);
    }
    Ok(())
}

#[must_use]
pub fn profile_u32(bytes: &[u8], offset: usize) -> Option<usize> {
    let value = u32::from_be_bytes(bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?);
    usize::try_from(value).ok()
}

/// Validate stage roles and all referenced curve/CLUT/matrix ranges.
///
/// # Errors
/// Returns an invalid stage combination, curve, channel count or range.
///
/// # Panics
/// Fixed-size header conversions follow checked extents.
pub fn validate_multistage_lut(signature: &[u8], payload: &[u8]) -> Result<(), Error> {
    let malformed = Error::MalformedTagTable;
    if payload.len() < 32 || payload[10..12] != [0; 2] {
        return Err(malformed);
    }
    let inputs = usize::from(payload[8]);
    let outputs = usize::from(payload[9]);
    let expected = match signature {
        b"A2B0" | b"A2B1" | b"A2B2" => Some((4, 3)),
        b"B2A0" | b"B2A1" | b"B2A2" => Some((3, 4)),
        b"gamt" => Some((3, 1)),
        _ => None,
    };
    if !(1..=15).contains(&inputs)
        || !(1..=15).contains(&outputs)
        || expected.is_some_and(|value| value != (inputs, outputs))
    {
        return Err(malformed);
    }
    let [b, matrix, m, clut, a] = [12, 16, 20, 24, 28]
        .map(|offset| profile_u32(payload, offset).expect("complete multistage header"));
    if b == 0
        || (a == 0) != (clut == 0)
        || (m == 0) != (matrix == 0)
        || (inputs != outputs && clut == 0)
    {
        return Err(malformed);
    }
    let forward = &payload[..4] == b"mAB ";
    let side = if forward { outputs } else { inputs };
    let mut ranges = Vec::new();
    multistage_curves(payload, b, side, &mut ranges)?;
    if a != 0 {
        multistage_curves(
            payload,
            a,
            if forward { inputs } else { outputs },
            &mut ranges,
        )?;
    }
    if m != 0 {
        if side != 3 {
            return Err(malformed);
        }
        multistage_curves(payload, m, side, &mut ranges)?;
        ranges.push(multistage_range(payload, matrix, 48)?);
    }
    if clut != 0 {
        multistage_range(payload, clut, 20)?;
        let grids = &payload[clut..clut + 16];
        let precision = usize::from(payload[clut + 16]);
        if grids[..inputs].iter().any(|value| *value < 2)
            || grids[inputs..].iter().any(|value| *value != 0)
            || !matches!(precision, 1 | 2)
            || payload[clut + 17..clut + 20] != [0; 3]
        {
            return Err(malformed);
        }
        let clut_extent = grids[..inputs]
            .iter()
            .try_fold(outputs * precision, |total, grid| {
                total.checked_mul(usize::from(*grid))
            })
            .and_then(|samples| samples.checked_add(20))
            .ok_or(malformed)?;
        let range = multistage_range(payload, clut, clut_extent)?;
        let padded = (range.1 + 3) & !3;
        if payload[range.1..padded.min(payload.len())]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(malformed);
        }
        ranges.push(range);
    }
    ranges.sort_unstable();
    ranges.dedup();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(malformed);
    }
    Ok(())
}

fn multistage_range(payload: &[u8], start: usize, size: usize) -> Result<(usize, usize), Error> {
    let end = start.checked_add(size).ok_or(Error::MalformedTagTable)?;
    if start < 32 || !start.is_multiple_of(4) || end > payload.len() {
        return Err(Error::MalformedTagTable);
    }
    Ok((start, end))
}

fn multistage_curves(
    payload: &[u8],
    mut start: usize,
    count: usize,
    ranges: &mut Vec<(usize, usize)>,
) -> Result<(), Error> {
    let malformed = Error::MalformedTagTable;
    for _ in 0..count {
        multistage_range(payload, start, 12)?;
        let size = curve_extent(&payload[start..])?;
        let range = multistage_range(payload, start, size)?;
        ranges.push(range);
        start = range
            .1
            .checked_add(3)
            .map(|end| end & !3)
            .ok_or(malformed)?;
        if payload[range.1..start.min(payload.len())]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(malformed);
        }
    }
    Ok(())
}
