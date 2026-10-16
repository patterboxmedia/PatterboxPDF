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

#[cfg(test)]
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
        has_device_to_pcs |= signature == b"A2B0";
        has_pcs_to_device |= signature == b"B2A0";
    }

    if !has_device_to_pcs || !has_pcs_to_device {
        return Err(ProfileError::MissingRequiredOutputTransforms);
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

    #[cfg(not(windows))]
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
