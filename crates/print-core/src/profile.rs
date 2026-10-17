/// Maximum accepted exact output-profile size.
pub const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;
use sha2::{Digest, Sha256};

const ICC_HEADER_LEN: usize = 128;
const ICC_TAG_COUNT_LEN: usize = 4;
const ICC_TAG_RECORD_LEN: usize = 12;

/// SHA-256 identity for the exact ICC bytes used by an evaluation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProfileHash([u8; 32]);

impl ProfileHash {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    #[must_use]
    pub fn of(profile: &[u8]) -> Self {
        Self(Sha256::digest(profile).into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IccDeviceClass {
    Printer,
    Display,
    ColorSpace,
    Unknown([u8; 4]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IccColorSpace {
    Cmyk,
    Rgb,
    Gray,
    Unknown([u8; 4]),
}

/// Immutable facts from an output-intent profile accepted by the complete
/// parser boundary. Fields are intentionally not caller-constructible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputIntentFacts {
    pub(crate) sha256: ProfileHash,
    pub(crate) byte_len: usize,
    pub(crate) declared_size: usize,
    pub(crate) device_class: IccDeviceClass,
    pub(crate) color_space: IccColorSpace,
    pub(crate) profile_connection_space: [u8; 4],
}

#[cfg(all(test, feature = "publisher-adapter"))]
impl OutputIntentFacts {
    pub(crate) const fn from_test_parts(
        sha256: ProfileHash,
        byte_len: usize,
        declared_size: usize,
        device_class: IccDeviceClass,
        color_space: IccColorSpace,
        profile_connection_space: [u8; 4],
    ) -> Self {
        Self {
            sha256,
            byte_len,
            declared_size,
            device_class,
            color_space,
            profile_connection_space,
        }
    }
}

impl OutputIntentFacts {
    #[must_use]
    pub const fn profile_hash(self) -> ProfileHash {
        self.sha256
    }

    #[must_use]
    pub const fn byte_len(self) -> usize {
        self.byte_len
    }

    #[must_use]
    pub const fn device_class(self) -> IccDeviceClass {
        self.device_class
    }

    #[must_use]
    pub const fn color_space(self) -> IccColorSpace {
        self.color_space
    }

    #[must_use]
    pub const fn profile_connection_space(self) -> [u8; 4] {
        self.profile_connection_space
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileError {
    TooShort,
    TooLarge { actual: usize, maximum: usize },
    DeclaredSizeMismatch { declared: usize, actual: usize },
    BadSignature,
    InvalidVersionEncoding([u8; 4]),
    UnsupportedVersion([u8; 4]),
    MissingTags,
    MalformedTagTable,
    MissingRequiredOutputTransforms,
    MissingRequiredOutputTags,
    ColorWorkerUnavailable,
    ColorWorkerIdentityMismatch,
    ColorWorkerConfinementFailed,
    ColorWorkerTimedOut,
    ColorWorkerProtocolFailure,
    ColorWorkerIo,
    ColorEngineRejected,
    NotPrinter,
    NotCmyk,
    UnsupportedProfileConnectionSpace([u8; 4]),
}

/// Screens the immutable bytes intended for `/DestOutputProfile`.
///
/// It validates the complete ICC container with confined `LittleCMS`, verifies every tag
/// record is bounded by the immutable source bytes, and requires both device-to-PCS
/// and PCS-to-device transforms used by a printer output profile. This is a
/// conservative acceptance boundary: a successful result means the profile is
/// parseable and structurally usable, not that its colorimetric quality has been
/// characterized or independently certified.
/// Header and tag bounds are checked in safe Rust before launching the worker;
/// the version must encode reviewed ICC 2.2.0 or 4.0.0–4.2.0 with zero reserved
/// bytes. This explicit edition screen is not a complete ICC conformance proof.
/// Native parsing and transform probes require the packaged Windows sidecar.
/// There is no in-process fallback when that worker is unavailable.
///
/// # Errors
///
/// Returns [`ProfileError`] when the header is truncated, internally
/// inconsistent, does not describe a printer-class CMYK output profile, or the
/// confined worker cannot establish the exact profile's usability.
///
/// # Panics
///
/// Panics only on a target whose `usize` cannot represent a `u32`; this library
/// does not support such targets. The fixed-size slice conversions are guarded
/// by the header-length check.
pub fn inspect_output_intent(profile: &[u8]) -> Result<OutputIntentFacts, ProfileError> {
    if profile.len() > MAX_ICC_PROFILE_BYTES {
        return Err(ProfileError::TooLarge {
            actual: profile.len(),
            maximum: MAX_ICC_PROFILE_BYTES,
        });
    }
    if profile.len() < ICC_HEADER_LEN {
        return Err(ProfileError::TooShort);
    }

    let declared_size = u32::from_be_bytes(profile[0..4].try_into().expect("four byte slice"));
    let declared_size =
        usize::try_from(declared_size).expect("u32 fits usize on supported targets");
    if declared_size != profile.len() {
        return Err(ProfileError::DeclaredSizeMismatch {
            declared: declared_size,
            actual: profile.len(),
        });
    }
    if &profile[36..40] != b"acsp" {
        return Err(ProfileError::BadSignature);
    }
    validate_referenced_version(profile[8..12].try_into().expect("four byte slice"))?;

    let device_class_bytes: [u8; 4] = profile[12..16].try_into().expect("four byte slice");
    let device_class = match &device_class_bytes {
        b"prtr" => IccDeviceClass::Printer,
        b"mntr" => IccDeviceClass::Display,
        b"spac" => IccDeviceClass::ColorSpace,
        _ => IccDeviceClass::Unknown(device_class_bytes),
    };
    if device_class != IccDeviceClass::Printer {
        return Err(ProfileError::NotPrinter);
    }

    let color_space_bytes: [u8; 4] = profile[16..20].try_into().expect("four byte slice");
    let color_space = match &color_space_bytes {
        b"CMYK" => IccColorSpace::Cmyk,
        b"RGB " => IccColorSpace::Rgb,
        b"GRAY" => IccColorSpace::Gray,
        _ => IccColorSpace::Unknown(color_space_bytes),
    };
    if color_space != IccColorSpace::Cmyk {
        return Err(ProfileError::NotCmyk);
    }

    let pcs: [u8; 4] = profile[20..24].try_into().expect("four byte slice");
    if &pcs != b"Lab " && &pcs != b"XYZ " {
        return Err(ProfileError::UnsupportedProfileConnectionSpace(pcs));
    }

    validate_tag_table(profile)?;
    let receipt = color_boundary::TrustedColorBoundary::resolve_packaged()
        .inspect_output_profile(profile)
        .map_err(|error| profile_worker_error(&error))?;
    if receipt.profile_hash() != ProfileHash::of(profile).bytes()
        || receipt.byte_len() != profile.len()
    {
        return Err(ProfileError::ColorWorkerIdentityMismatch);
    }

    Ok(OutputIntentFacts {
        sha256: ProfileHash::of(profile),
        byte_len: profile.len(),
        declared_size,
        device_class,
        color_space,
        profile_connection_space: pcs,
    })
}

fn validate_referenced_version(version: [u8; 4]) -> Result<(), ProfileError> {
    if version[0] >> 4 > 9
        || version[0] & 15 > 9
        || version[1] >> 4 > 9
        || version[1] & 15 > 9
        || version[2] != 0
        || version[3] != 0
    {
        return Err(ProfileError::InvalidVersionEncoding(version));
    }
    // Supported PDF/X-4 reference editions: ICC.1:1998-09 (2.2.0),
    // ICC.1:2001-12 (4.0.0), ICC.1:2003-09 (4.1.0), and
    // ICC.1:2004-10 / ISO 15076-1:2005 (4.2.0). Older v2 minors and
    // newer/bug-fix versions are outside this explicitly reviewed boundary.
    match version {
        [2, 0x20, 0, 0] | [4, 0x00 | 0x10 | 0x20, 0, 0] => Ok(()),
        _ => Err(ProfileError::UnsupportedVersion(version)),
    }
}

fn profile_worker_error(error: &color_boundary::ColorWorkerError) -> ProfileError {
    use color_boundary::ColorWorkerError;
    match error {
        ColorWorkerError::NotInstalled | ColorWorkerError::InvalidExecutable(_) => {
            ProfileError::ColorWorkerUnavailable
        }
        ColorWorkerError::ExecutableIdentityDrift | ColorWorkerError::IdentityDrift => {
            ProfileError::ColorWorkerIdentityMismatch
        }
        ColorWorkerError::ConfinementFailed(_) => ProfileError::ColorWorkerConfinementFailed,
        ColorWorkerError::TimedOut => ProfileError::ColorWorkerTimedOut,
        ColorWorkerError::MalformedReport(_) => ProfileError::ColorWorkerProtocolFailure,
        ColorWorkerError::WorkerRejected(_) => ProfileError::ColorEngineRejected,
        ColorWorkerError::Io(_)
        | ColorWorkerError::SpawnFailed(_)
        | ColorWorkerError::ResourceLimit
        | ColorWorkerError::SourceProfile(_) => ProfileError::ColorWorkerIo,
    }
}

fn validate_tag_table(profile: &[u8]) -> Result<(), ProfileError> {
    validate_tag_table_with(profile, validate_tag_payload)
}

fn validate_tag_table_with(
    profile: &[u8],
    mut validate_payload: impl FnMut(&[u8], &[u8], &[u8]) -> Result<(), ProfileError>,
) -> Result<(), ProfileError> {
    let count_end = ICC_HEADER_LEN
        .checked_add(ICC_TAG_COUNT_LEN)
        .ok_or(ProfileError::MalformedTagTable)?;
    if profile.len() < count_end {
        return Err(ProfileError::TooShort);
    }
    let tag_count = u32::from_be_bytes(
        profile[ICC_HEADER_LEN..count_end]
            .try_into()
            .expect("four byte slice"),
    );
    let tag_count = usize::try_from(tag_count).map_err(|_| ProfileError::MalformedTagTable)?;
    if tag_count == 0 {
        return Err(ProfileError::MissingTags);
    }
    let table_bytes = tag_count
        .checked_mul(ICC_TAG_RECORD_LEN)
        .ok_or(ProfileError::MalformedTagTable)?;
    let table_end = count_end
        .checked_add(table_bytes)
        .ok_or(ProfileError::MalformedTagTable)?;
    if table_end > profile.len() {
        return Err(ProfileError::MalformedTagTable);
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
        let offset = usize::try_from(offset).map_err(|_| ProfileError::MalformedTagTable)?;
        let size = usize::try_from(size).map_err(|_| ProfileError::MalformedTagTable)?;
        let end = offset
            .checked_add(size)
            .ok_or(ProfileError::MalformedTagTable)?;
        if offset < table_end || offset % 4 != 0 || size < 8 || end > profile.len() {
            return Err(ProfileError::MalformedTagTable);
        }
        if !signatures.insert(signature) {
            return Err(ProfileError::MalformedTagTable);
        }
        records.push((signature, offset, end));
        has_device_to_pcs |= signature == b"A2B0";
        has_pcs_to_device |= signature == b"B2A0";
    }

    if !has_device_to_pcs || !has_pcs_to_device {
        return Err(ProfileError::MissingRequiredOutputTransforms);
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
        return Err(ProfileError::MissingRequiredOutputTags);
    }
    records.sort_unstable_by_key(|record| (record.1, record.2));
    // Complete aliases are valid; reject partial overlaps before payload scans.
    if records.windows(2).any(|pair| {
        let left = (pair[0].1, pair[0].2);
        let right = (pair[1].1, pair[1].2);
        left != right && left.1 > right.0
    }) {
        return Err(ProfileError::MalformedTagTable);
    }
    for &(signature, offset, end) in &records {
        validate_tag_role(signature, &profile[offset..end], profile)?;
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

fn validate_tag_role(signature: &[u8], payload: &[u8], profile: &[u8]) -> Result<(), ProfileError> {
    let malformed = ProfileError::MalformedTagTable;
    let kind = &payload[..4];
    let expected = match signature {
        b"desc" => Some(if profile[8] == 2 { b"desc" } else { b"mluc" }.as_slice()),
        b"cprt" => Some(if profile[8] == 2 { b"text" } else { b"mluc" }.as_slice()),
        b"wtpt" | b"bkpt" => Some(b"XYZ ".as_slice()),
        _ => None,
    };
    if expected.is_some_and(|expected| kind != expected)
        || (matches!(signature, b"wtpt" | b"bkpt") && payload.len() != 20)
    {
        return Err(malformed);
    }
    let (channels, v4_kind) = match signature {
        b"A2B0" | b"A2B1" | b"A2B2" => (Some((4, 3)), b"mAB "),
        b"B2A0" | b"B2A1" | b"B2A2" => (Some((3, 4)), b"mBA "),
        b"gamt" => (Some((3, 1)), b"mBA "),
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
        // This parser checks only a fixed header, extent and nine matrix values.
        b"mft2" => validate_lut16(signature, payload, profile)?,
        b"mAB " | b"mBA "
            if payload.len() < 32
                || channels.is_some_and(|expected| {
                    expected != (usize::from(payload[8]), usize::from(payload[9]))
                }) =>
        {
            return Err(malformed);
        }
        _ => {}
    }
    Ok(())
}

fn validate_tag_payload(
    signature: &[u8],
    payload: &[u8],
    profile: &[u8],
) -> Result<(), ProfileError> {
    let malformed = ProfileError::MalformedTagTable;
    if payload[4..8].iter().any(|byte| *byte != 0) {
        return Err(malformed);
    }
    let kind = &payload[..4];
    match kind {
        b"XYZ " => {
            if payload.len() < 20
                || !(payload.len() - 8).is_multiple_of(12)
                || ((signature == b"wtpt" || signature == b"bkpt") && payload.len() != 20)
                || payload[8..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|value| i32::from_be_bytes(*value) < 0)
            {
                return Err(malformed);
            }
        }
        b"text" => {
            if payload.len() < 9 || payload.last() != Some(&0) || !payload[8..].is_ascii() {
                return Err(malformed);
            }
        }
        b"desc" => {
            let ascii_count = profile_u32(payload, 8).ok_or(malformed)?;
            let unicode_start = 12_usize.checked_add(ascii_count).ok_or(malformed)?;
            let unicode_count = profile_u32(payload, unicode_start + 4).ok_or(malformed)?;
            let script_start = unicode_count
                .checked_mul(2)
                .and_then(|size| unicode_start.checked_add(8 + size))
                .ok_or(malformed)?;
            if ascii_count == 0
                || unicode_start > payload.len()
                || payload[unicode_start - 1] != 0
                || !payload[12..unicode_start].is_ascii()
                || script_start
                    .checked_add(70)
                    .is_none_or(|end| end != payload.len())
                || payload[script_start + 2] > 67
            {
                return Err(malformed);
            }
        }
        b"mft2" => validate_lut16(signature, payload, profile)?,
        b"mAB " | b"mBA " => validate_multistage_lut(signature, payload)?,
        b"sf32" if payload.len() < 12 || !(payload.len() - 8).is_multiple_of(4) => {
            return Err(malformed);
        }
        _ => {} // Other admitted standard/private types are decoded by the confined native engine.
    }
    Ok(())
}

fn profile_u32(bytes: &[u8], offset: usize) -> Option<usize> {
    let value = u32::from_be_bytes(bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?);
    usize::try_from(value).ok()
}

fn validate_multistage_lut(signature: &[u8], payload: &[u8]) -> Result<(), ProfileError> {
    let malformed = ProfileError::MalformedTagTable;
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
        ranges.push(multistage_range(payload, clut, clut_extent)?);
    }
    ranges.sort_unstable();
    ranges.dedup();
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(malformed);
    }
    Ok(())
}

fn multistage_range(
    payload: &[u8],
    start: usize,
    size: usize,
) -> Result<(usize, usize), ProfileError> {
    let end = start
        .checked_add(size)
        .ok_or(ProfileError::MalformedTagTable)?;
    if start < 32 || !start.is_multiple_of(4) || end > payload.len() {
        return Err(ProfileError::MalformedTagTable);
    }
    Ok((start, end))
}

fn multistage_curves(
    payload: &[u8],
    mut start: usize,
    count: usize,
    ranges: &mut Vec<(usize, usize)>,
) -> Result<(), ProfileError> {
    let malformed = ProfileError::MalformedTagTable;
    for _ in 0..count {
        multistage_range(payload, start, 12)?;
        if payload[start + 4..start + 8] != [0; 4] {
            return Err(malformed);
        }
        let size = match &payload[start..start + 4] {
            b"curv" => profile_u32(payload, start + 8)
                .and_then(|count| count.checked_mul(2))
                .and_then(|samples| samples.checked_add(12))
                .ok_or(malformed)?,
            b"para" => {
                let function =
                    usize::from(u16::from_be_bytes([payload[start + 8], payload[start + 9]]));
                if function > 4 || payload[start + 10..start + 12] != [0; 2] {
                    return Err(malformed);
                }
                12 + 4 * [1, 3, 4, 5, 7][function]
            }
            _ => return Err(malformed),
        };
        let range = multistage_range(payload, start, size)?;
        ranges.push(range);
        start = range
            .1
            .checked_add(3)
            .map(|end| end & !3)
            .ok_or(malformed)?;
        if start > payload.len() || payload[range.1..start].iter().any(|byte| *byte != 0) {
            return Err(malformed);
        }
    }
    Ok(())
}

fn validate_lut16(signature: &[u8], payload: &[u8], profile: &[u8]) -> Result<(), ProfileError> {
    let malformed = ProfileError::MalformedTagTable;
    if payload.len() < 52 {
        return Err(malformed);
    }
    let inputs = usize::from(payload[8]);
    let outputs = usize::from(payload[9]);
    let grid = usize::from(payload[10]);
    let input_entries = usize::from(u16::from_be_bytes([payload[48], payload[49]]));
    let output_entries = usize::from(u16::from_be_bytes([payload[50], payload[51]]));
    let expected_channels = match signature {
        b"A2B0" | b"A2B1" | b"A2B2" => Some((4, 3)),
        b"B2A0" | b"B2A1" | b"B2A2" => Some((3, 4)),
        b"gamt" => Some((3, 1)),
        _ => None,
    };
    if !(1..=15).contains(&inputs)
        || !(1..=15).contains(&outputs)
        || grid < 2
        || payload[11] != 0
        || !(2..=4096).contains(&input_entries)
        || !(2..=4096).contains(&output_entries)
        || expected_channels.is_some_and(|expected| expected != (inputs, outputs))
    {
        return Err(malformed);
    }
    let expected_size = grid
        .checked_pow(u32::try_from(inputs).map_err(|_| malformed)?)
        .and_then(|clut| clut.checked_mul(outputs))
        .and_then(|samples| samples.checked_add(inputs * input_entries + outputs * output_entries))
        .and_then(|samples| samples.checked_mul(2))
        .and_then(|bytes| bytes.checked_add(52));
    if expected_size != Some(payload.len()) {
        return Err(malformed);
    }
    let xyz_input = &profile[20..24] == b"XYZ "
        && signature != b"A2B0"
        && signature != b"A2B1"
        && signature != b"A2B2";
    if !xyz_input {
        for index in 0..9 {
            let expected = if index % 4 == 0 { 65536 } else { 0 };
            if profile_u32(payload, 12 + index * 4) != Some(expected) {
                return Err(malformed);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MAX_ICC_PROFILE_BYTES, ProfileError, inspect_output_intent};

    fn synthetic_cmyk_output_profile() -> Vec<u8> {
        let mut profile = vec![0; 132];
        let declared_size = u32::try_from(profile.len()).expect("synthetic profile fits u32");
        profile[0..4].copy_from_slice(&declared_size.to_be_bytes());
        profile[8..12].copy_from_slice(&[2, 0x20, 0, 0]);
        profile[12..16].copy_from_slice(b"prtr");
        profile[16..20].copy_from_slice(b"CMYK");
        profile[20..24].copy_from_slice(b"Lab ");
        profile[36..40].copy_from_slice(b"acsp");
        profile
    }

    #[test]
    fn referenced_versions_reach_tag_checks_and_bad_encodings_fail_before_worker_work() {
        for version in [
            [2, 0x20, 0, 0],
            [4, 0, 0, 0],
            [4, 0x10, 0, 0],
            [4, 0x20, 0, 0],
        ] {
            let mut profile = synthetic_cmyk_output_profile();
            profile[8..12].copy_from_slice(&version);
            assert_eq!(
                inspect_output_intent(&profile),
                Err(ProfileError::MissingTags)
            );
        }
        for version in [
            [2, 0x20, 1, 0],
            [4, 0x20, 0, 1],
            [4, 0xA0, 0, 0],
            [0xA4, 0x20, 0, 0],
            [4, 0x2A, 0, 0],
        ] {
            let mut profile = synthetic_cmyk_output_profile();
            profile[8..12].copy_from_slice(&version);
            assert_eq!(
                inspect_output_intent(&profile),
                Err(ProfileError::InvalidVersionEncoding(version))
            );
        }
        for version in [
            [2, 0, 0, 0],
            [2, 0x10, 0, 0],
            [2, 0x30, 0, 0],
            [4, 0x30, 0, 0],
            [4, 0x21, 0, 0],
            [5, 0, 0, 0],
        ] {
            let mut profile = synthetic_cmyk_output_profile();
            profile[8..12].copy_from_slice(&version);
            assert_eq!(
                inspect_output_intent(&profile),
                Err(ProfileError::UnsupportedVersion(version))
            );
        }
    }

    #[cfg(not(any(windows, all(target_os = "linux", target_arch = "x86_64"))))]
    #[test]
    fn valid_profile_cannot_fall_back_to_native_parsing_on_an_unsupported_host() {
        let profile = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        assert!(matches!(
            inspect_output_intent(profile),
            Err(ProfileError::ColorWorkerUnavailable | ProfileError::ColorWorkerConfinementFailed)
        ));
    }

    #[test]
    fn rejects_a_header_only_profile() {
        let profile = synthetic_cmyk_output_profile();
        assert_eq!(
            inspect_output_intent(&profile),
            Err(ProfileError::MissingTags)
        );
    }

    #[test]
    fn rejects_missing_required_printer_tags_before_native_parsing() {
        let original = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        for required in [
            b"desc", b"cprt", b"wtpt", b"A2B0", b"A2B1", b"A2B2", b"B2A0", b"B2A1", b"B2A2",
            b"gamt",
        ] {
            let mut changed = original.to_vec();
            let records = u32::from_be_bytes(changed[128..132].try_into().unwrap()) as usize;
            let start = (0..records)
                .map(|index| 132 + index * 12)
                .find(|start| &changed[*start..*start + 4] == required)
                .unwrap();
            changed[start..start + 4].copy_from_slice(b"test");
            assert!(
                super::validate_tag_table(&changed).is_err(),
                "missing required printer tag {required:?} was admitted"
            );
        }
        assert!(super::validate_tag_table(original).is_ok());
    }

    fn profile_with_shared_xyz_aliases(alias_count: usize) -> (Vec<u8>, usize) {
        let original = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        let original_count = u32::from_be_bytes(original[128..132].try_into().unwrap()) as usize;
        let gap = alias_count * 12;
        let gap_u32 = u32::try_from(gap).unwrap();
        let mut profile = original.to_vec();
        profile.splice(132..132, std::iter::repeat_n(0, gap));
        for index in 0..original_count {
            let record = 132 + gap + index * 12;
            let offset = u32::from_be_bytes(profile[record + 4..record + 8].try_into().unwrap());
            profile[record + 4..record + 8].copy_from_slice(&(offset + gap_u32).to_be_bytes());
        }
        let offset = (profile.len() + 3) & !3;
        let size = 8 + 12 * 8192;
        profile.resize(offset + size, 0);
        profile[offset..offset + 4].copy_from_slice(b"XYZ ");
        let offset_bytes = u32::try_from(offset).unwrap().to_be_bytes();
        let size_bytes = u32::try_from(size).unwrap().to_be_bytes();
        for index in 0..alias_count {
            let record = 132 + index * 12;
            profile[record..record + 4]
                .copy_from_slice(&(0x7000_0000_u32 + u32::try_from(index).unwrap()).to_be_bytes());
            profile[record + 4..record + 8].copy_from_slice(&offset_bytes);
            profile[record + 8..record + 12].copy_from_slice(&size_bytes);
        }
        profile[128..132].copy_from_slice(
            &u32::try_from(original_count + alias_count)
                .unwrap()
                .to_be_bytes(),
        );
        let size = u32::try_from(profile.len()).unwrap();
        profile[..4].copy_from_slice(&size.to_be_bytes());
        (profile, offset)
    }

    #[test]
    fn shared_tag_body_work_is_bounded_by_unique_ranges() {
        let (profile, large_offset) = profile_with_shared_xyz_aliases(4096);
        let mut large_visits = 0;
        let mut large_bytes = 0;
        super::validate_tag_table_with(&profile, |signature, payload, profile| {
            if payload.as_ptr() == profile[large_offset..].as_ptr() {
                large_visits += 1;
                large_bytes += payload.len();
            }
            super::validate_tag_payload(signature, payload, profile)
        })
        .unwrap();
        assert_eq!(large_visits, 1);
        assert_eq!(large_bytes, profile.len() - large_offset);
    }

    #[test]
    fn directory_failures_precede_all_body_work() {
        let (profile, _) = profile_with_shared_xyz_aliases(64);
        let count = u32::from_be_bytes(profile[128..132].try_into().unwrap()) as usize;
        let record = |signature: &[u8]| {
            (0..count)
                .map(|i| 132 + i * 12)
                .find(|start| &profile[*start..*start + 4] == signature)
                .unwrap()
        };
        let mut missing = profile.clone();
        missing[record(b"A2B0")..record(b"A2B0") + 4].copy_from_slice(b"gone");
        let mut duplicate = profile.clone();
        duplicate[144..148].copy_from_slice(&profile[132..136]);
        let mut overlap = profile.clone();
        let offset = u32::from_be_bytes(profile[136..140].try_into().unwrap());
        overlap[148..152].copy_from_slice(&(offset + 4).to_be_bytes());
        for invalid in [missing, duplicate, overlap] {
            let mut body_visits = 0;
            assert!(
                super::validate_tag_table_with(&invalid, |_, _, _| {
                    body_visits += 1;
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(body_visits, 0);
        }
    }

    #[test]
    fn shared_bodies_still_obey_each_signature_role() {
        let original = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        let count = u32::from_be_bytes(original[128..132].try_into().unwrap()) as usize;
        let record = |signature: &[u8]| {
            (0..count)
                .map(|i| 132 + i * 12)
                .find(|start| &original[*start..*start + 4] == signature)
                .unwrap()
        };
        for (target, source) in [(b"B2A0", b"A2B0"), (b"gamt", b"B2A0"), (b"cprt", b"desc")] {
            let mut changed = original.to_vec();
            changed[record(target) + 4..record(target) + 12]
                .copy_from_slice(&original[record(source) + 4..record(source) + 12]);
            assert!(super::validate_tag_table(&changed).is_err());
        }
        let (mut changed, offset) = profile_with_shared_xyz_aliases(1);
        let white = (0..=count)
            .map(|i| 132 + i * 12)
            .find(|start| &changed[*start..*start + 4] == b"wtpt")
            .unwrap();
        let size = changed.len() - offset;
        changed[white + 4..white + 8]
            .copy_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
        changed[white + 8..white + 12].copy_from_slice(&u32::try_from(size).unwrap().to_be_bytes());
        assert!(super::validate_tag_table(&changed).is_err());
    }

    #[test]
    fn rejects_duplicate_tags_and_partial_payload_overlap_but_allows_shared_payloads() {
        let original = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        assert!(super::validate_tag_table(original).is_ok());
        let mut duplicate = original.to_vec();
        duplicate[144..148].copy_from_slice(b"desc");
        assert!(super::validate_tag_table(&duplicate).is_err());
        let mut overlap = original.to_vec();
        let first_offset = u32::from_be_bytes(original[136..140].try_into().unwrap());
        overlap[148..152].copy_from_slice(&(first_offset + 4).to_be_bytes());
        assert!(super::validate_tag_table(&overlap).is_err());
    }

    #[test]
    fn rejects_malformed_present_tag_payloads_before_native_parsing() {
        let original = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        for signature in [b"desc", b"cprt", b"wtpt", b"A2B0", b"arts"] {
            let mut changed = original.to_vec();
            let records = u32::from_be_bytes(changed[128..132].try_into().unwrap()) as usize;
            let start = (0..records)
                .map(|index| 132 + index * 12)
                .find(|start| &changed[*start..*start + 4] == signature)
                .unwrap();
            let offset =
                u32::from_be_bytes(changed[start + 4..start + 8].try_into().unwrap()) as usize;
            changed[offset + 4] = 1;
            assert!(
                super::validate_tag_table(&changed).is_err(),
                "nonzero tag reserved bytes admitted"
            );
        }
        let mut wrong_type = original.to_vec();
        let description_offset =
            u32::from_be_bytes(original[136..140].try_into().unwrap()) as usize;
        wrong_type[description_offset..description_offset + 4].copy_from_slice(b"text");
        assert!(super::validate_tag_table(&wrong_type).is_err());
    }

    #[test]
    fn rejects_invalid_lut16_sample_layout_before_native_parsing() {
        let original = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        let mut changed = original.to_vec();
        let records = u32::from_be_bytes(changed[128..132].try_into().unwrap()) as usize;
        let start = (0..records)
            .map(|index| 132 + index * 12)
            .find(|start| &changed[*start..*start + 4] == b"A2B0")
            .unwrap();
        let offset = u32::from_be_bytes(changed[start + 4..start + 8].try_into().unwrap()) as usize;
        changed[offset + 48..offset + 50].copy_from_slice(&1_u16.to_be_bytes());
        assert!(super::validate_tag_table(&changed).is_err());
    }

    #[test]
    fn rejects_transform_payload_types_and_negative_xyz_before_native_parsing() {
        let original = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        for tag in [*b"A2B0", *b"B2A0", *b"gamt", *b"wtpt"] {
            let mut changed = original.to_vec();
            let count = u32::from_be_bytes(changed[128..132].try_into().unwrap()) as usize;
            let record = (0..count)
                .map(|index| 132 + index * 12)
                .find(|record| changed[*record..*record + 4] == tag)
                .unwrap();
            let offset =
                u32::from_be_bytes(changed[record + 4..record + 8].try_into().unwrap()) as usize;
            if tag == *b"wtpt" {
                changed[offset + 8..offset + 12].copy_from_slice(&(-1_i32).to_be_bytes());
            } else {
                changed[offset..offset + 4].copy_from_slice(b"bad!");
            }
            assert!(super::validate_tag_table(&changed).is_err());
        }
    }

    #[test]
    fn multistage_luts_check_stage_pairs_curves_and_clut_extents() {
        fn lut(inputs: u8, outputs: u8, forward: bool) -> Vec<u8> {
            let mut data = vec![0; 32];
            data[..4].copy_from_slice(if forward { b"mAB " } else { b"mBA " });
            data[8] = inputs;
            data[9] = outputs;
            data[12..16].copy_from_slice(&32_u32.to_be_bytes());
            for _ in 0..if forward { outputs } else { inputs } {
                data.extend_from_slice(b"curv\0\0\0\0\0\0\0\0");
            }
            let clut = u32::try_from(data.len()).unwrap();
            data[24..28].copy_from_slice(&clut.to_be_bytes());
            data.extend(std::iter::repeat_n(2, usize::from(inputs)));
            data.extend(std::iter::repeat_n(0, 16 - usize::from(inputs)));
            data.extend_from_slice(&[1, 0, 0, 0]);
            data.extend(std::iter::repeat_n(
                0,
                2_usize.pow(u32::from(inputs)) * usize::from(outputs),
            ));
            while !data.len().is_multiple_of(4) {
                data.push(0);
            }
            let a = u32::try_from(data.len()).unwrap();
            data[28..32].copy_from_slice(&a.to_be_bytes());
            for _ in 0..if forward { inputs } else { outputs } {
                data.extend_from_slice(b"curv\0\0\0\0\0\0\0\0");
            }
            data
        }
        for (signature, data) in [
            (*b"A2B0", lut(4, 3, true)),
            (*b"B2A0", lut(3, 4, false)),
            (*b"gamt", lut(3, 1, false)),
        ] {
            assert!(super::validate_multistage_lut(&signature, &data).is_ok());
            let clut = super::profile_u32(&data, 24).unwrap();
            for (position, value) in [
                (10, 1),
                (15, 0),
                (31, 0),
                (clut + 16, 3),
                (clut + usize::from(data[8]), 9),
                (clut, 255),
                (32, b'x'),
            ] {
                let mut changed = data.clone();
                changed[position] = value;
                assert!(super::validate_multistage_lut(&signature, &changed).is_err());
            }
        }
        // A and B can reuse identical individual curve blocks.
        let mut shared = vec![0; 44];
        shared[..4].copy_from_slice(b"mAB ");
        shared[8..10].copy_from_slice(&[1, 1]);
        shared[12..16].copy_from_slice(&32_u32.to_be_bytes());
        shared[32..36].copy_from_slice(b"curv");
        assert!(super::validate_multistage_lut(b"test", &shared).is_ok());
        shared[28..32].copy_from_slice(&32_u32.to_be_bytes());
        shared[24..28].copy_from_slice(&44_u32.to_be_bytes());
        shared.extend_from_slice(&[
            2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 255,
        ]);
        assert!(super::validate_multistage_lut(b"test", &shared).is_ok());
    }

    #[test]
    fn rejects_oversized_input_before_color_engine_parsing() {
        let profile = vec![0_u8; MAX_ICC_PROFILE_BYTES + 1];
        assert_eq!(
            inspect_output_intent(&profile),
            Err(ProfileError::TooLarge {
                actual: profile.len(),
                maximum: MAX_ICC_PROFILE_BYTES,
            })
        );
    }

    #[cfg(windows)]
    #[test]
    fn accepts_a_real_cmyk_profile_with_bidirectional_transforms() {
        let profile = include_bytes!("../tests/fixtures/SWOP2006coated_300_mth.icc");
        assert_eq!(&profile[8..12], &[2, 0x20, 0, 0]);
        let facts = inspect_output_intent(profile).expect("real CMYK profile is transformable");
        assert_eq!(facts.device_class(), super::IccDeviceClass::Printer);
        assert_eq!(facts.color_space(), super::IccColorSpace::Cmyk);
        assert_eq!(facts.byte_len(), profile.len());
    }

    #[test]
    fn rejects_declared_size_mismatch() {
        let mut profile = synthetic_cmyk_output_profile();
        profile[0..4].copy_from_slice(&999_u32.to_be_bytes());
        assert_eq!(
            inspect_output_intent(&profile),
            Err(ProfileError::DeclaredSizeMismatch {
                declared: 999,
                actual: 132,
            })
        );
    }

    #[test]
    fn rejects_a_truncated_tag_table() {
        let mut profile = synthetic_cmyk_output_profile();
        profile[128..132].copy_from_slice(&1_u32.to_be_bytes());
        assert_eq!(
            inspect_output_intent(&profile),
            Err(ProfileError::MalformedTagTable)
        );
    }

    #[test]
    fn rejects_a_tag_payload_outside_the_profile() {
        let mut profile = vec![0_u8; 160];
        profile[0..4].copy_from_slice(&160_u32.to_be_bytes());
        profile[8..12].copy_from_slice(&[2, 0x20, 0, 0]);
        profile[12..16].copy_from_slice(b"prtr");
        profile[16..20].copy_from_slice(b"CMYK");
        profile[20..24].copy_from_slice(b"Lab ");
        profile[36..40].copy_from_slice(b"acsp");
        profile[128..132].copy_from_slice(&1_u32.to_be_bytes());
        profile[132..136].copy_from_slice(b"A2B0");
        profile[136..140].copy_from_slice(&156_u32.to_be_bytes());
        profile[140..144].copy_from_slice(&8_u32.to_be_bytes());
        assert_eq!(
            inspect_output_intent(&profile),
            Err(ProfileError::MalformedTagTable)
        );
    }
}
