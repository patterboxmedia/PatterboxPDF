//! Installed-font catalog shared by desktop authoring and layout.

mod inventory;

pub use inventory::{
    InstalledFontBackendScope, InstalledFontCapture, InstalledFontCaptureUnavailableReason,
    InstalledFontCatalogSnapshot, InstalledFontFace, InstalledFontInventory,
    InstalledFontInventoryIssue, InstalledFontMatch, InstalledFontMaterializationError,
    InstalledFontSourceFace, InstalledFontSourceMaterial, InstalledFontStyle,
    installed_font_catalog_snapshot, installed_font_inventory,
};

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::panic::{AssertUnwindSafe, catch_unwind};

use fontique::{Collection, CollectionOptions};
use sha2::{Digest, Sha256};
use ttf_parser::{Face, Permissions, Tag};

const FS_TYPE_PERMISSION_MASK: u16 = 0x000e;
const FS_TYPE_NO_SUBSETTING: u16 = 0x0100;
const FS_TYPE_BITMAP_ONLY: u16 = 0x0200;
const FS_TYPE_KNOWN_MASK: u16 =
    FS_TYPE_PERMISSION_MASK | FS_TYPE_NO_SUBSETTING | FS_TYPE_BITMAP_ONLY;
const OS2_FS_TYPE_OFFSET: usize = 8;
const FONT_FACE_FINGERPRINT_DOMAIN: &[u8] = b"publisher-v2/font-face-identity/v1\0";

/// Stable identity for one face in one exact font source.
///
/// A family name is deliberately absent: names are authoring labels and may
/// resolve to different files, faces, or versions on another machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontFaceIdentity {
    source_sha256: [u8; 32],
    face_index: u32,
    face_sha256: [u8; 32],
}

impl FontFaceIdentity {
    /// Hash of every byte in the supplied font file or collection.
    #[must_use]
    pub const fn source_sha256(&self) -> [u8; 32] {
        self.source_sha256
    }

    /// Zero-based face index used to address a standalone font or collection.
    #[must_use]
    pub const fn face_index(&self) -> u32 {
        self.face_index
    }

    /// Domain-separated digest binding the source hash to the face index.
    #[must_use]
    pub const fn face_sha256(&self) -> [u8; 32] {
        self.face_sha256
    }

    /// Confirms that later bytes and a face index still match this identity.
    #[must_use]
    pub fn matches_source(&self, font_bytes: &[u8], face_index: u32) -> bool {
        *self == font_face_identity(font_bytes, face_index)
    }
}

/// Exact-face facts produced from one immutable view of source bytes.
///
/// An `Unknown` eligibility remains useful evidence of which bytes failed, but
/// it must block font embedding. This structure is evidence for a future
/// export-input capsule; it is not an export result or a claim of font support.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontFaceFacts {
    identity: FontFaceIdentity,
    embedding_eligibility: FontEmbeddingEligibility,
}

impl FontFaceFacts {
    /// Exact source-and-index identity inspected for these facts.
    #[must_use]
    pub const fn identity(&self) -> FontFaceIdentity {
        self.identity
    }

    /// Fail-closed OpenType embedding decision for the exact identified face.
    #[must_use]
    pub const fn embedding_eligibility(&self) -> FontEmbeddingEligibility {
        self.embedding_eligibility
    }

    /// Confirms that later bytes still match the source inspected for these facts.
    #[must_use]
    pub fn matches_source(&self, font_bytes: &[u8], face_index: u32) -> bool {
        self.identity.matches_source(font_bytes, face_index)
    }
}

/// The license class encoded in an OpenType OS/2 `fsType` field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontEmbeddingLicense {
    /// The font permits installation and document embedding.
    Installable,
    /// The font permits embedding for viewing and printing.
    PreviewAndPrint,
    /// The font permits embedding in editable documents.
    Editable,
}

/// Why a font's embedding rights could not be determined safely.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontEmbeddingUnknownReason {
    /// The bytes or requested collection face cannot be parsed as a supported font.
    FaceUnavailableOrMalformed,
    /// The face has no valid OS/2 table from which licensing can be read.
    MissingOrMalformedOs2,
    /// The OS/2 `fsType` value uses an invalid or reserved flag combination.
    InvalidFsType,
}

/// A fail-closed decision for embedding one exact font face in a PDF.
///
/// OpenType does not define a "subset only" license bit. It defines the
/// inverse: a no-subsetting bit that requires embedding the complete font.
/// Keeping that distinction explicit prevents a future exporter from silently
/// treating every generally embeddable font as subsettable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontEmbeddingEligibility {
    /// Outline embedding and subsetting are both permitted.
    SubsetAllowed {
        license: FontEmbeddingLicense,
        fs_type: u16,
    },
    /// Outline embedding is permitted, but the complete font must be embedded.
    FullFontRequired {
        license: FontEmbeddingLicense,
        fs_type: u16,
    },
    /// The font explicitly denies document embedding.
    Restricted { fs_type: u16 },
    /// Only embedded bitmaps are permitted; PDF outline embedding is not.
    BitmapOnly {
        license: FontEmbeddingLicense,
        fs_type: u16,
    },
    /// The decision cannot be established from trusted metadata and must block export.
    Unknown {
        reason: FontEmbeddingUnknownReason,
        fs_type: Option<u16>,
    },
}

/// Captures exact-face identity and embedding rights from the same byte view.
///
/// The returned facts retain a fingerprint, not the source bytes. A future
/// immutable export capsule must own or lease those exact bytes and call
/// [`FontFaceFacts::matches_source`] before use.
#[must_use]
pub fn inspect_font_face(font_bytes: &[u8], face_index: u32) -> FontFaceFacts {
    FontFaceFacts {
        identity: font_face_identity(font_bytes, face_index),
        embedding_eligibility: inspect_font_embedding_eligibility(font_bytes, face_index),
    }
}

fn font_face_identity(font_bytes: &[u8], face_index: u32) -> FontFaceIdentity {
    let source_sha256: [u8; 32] = Sha256::digest(font_bytes).into();
    let mut face_hasher = Sha256::new();
    face_hasher.update(FONT_FACE_FINGERPRINT_DOMAIN);
    face_hasher.update(source_sha256);
    face_hasher.update(face_index.to_be_bytes());
    FontFaceIdentity {
        source_sha256,
        face_index,
        face_sha256: face_hasher.finalize().into(),
    }
}

/// Inspects the OpenType embedding rights for one face in caller-supplied bytes.
///
/// This pure parser is intended to run inside the future constrained font
/// worker. It returns facts only; it neither installs a font nor claims that a
/// PDF exporter exists or that the font technology can actually be written.
#[must_use]
pub fn inspect_font_embedding_eligibility(
    font_bytes: &[u8],
    face_index: u32,
) -> FontEmbeddingEligibility {
    let Ok(face) = Face::parse(font_bytes, face_index) else {
        return FontEmbeddingEligibility::Unknown {
            reason: FontEmbeddingUnknownReason::FaceUnavailableOrMalformed,
            fs_type: None,
        };
    };
    let Some(os2_bytes) = face.raw_face().table(Tag::from_bytes(b"OS/2")) else {
        return FontEmbeddingEligibility::Unknown {
            reason: FontEmbeddingUnknownReason::MissingOrMalformedOs2,
            fs_type: None,
        };
    };
    let Some(fs_type_bytes) = os2_bytes.get(OS2_FS_TYPE_OFFSET..OS2_FS_TYPE_OFFSET + 2) else {
        return FontEmbeddingEligibility::Unknown {
            reason: FontEmbeddingUnknownReason::MissingOrMalformedOs2,
            fs_type: None,
        };
    };
    let fs_type = u16::from_be_bytes([fs_type_bytes[0], fs_type_bytes[1]]);
    let Some(os2) = face.tables().os2 else {
        return FontEmbeddingEligibility::Unknown {
            reason: FontEmbeddingUnknownReason::MissingOrMalformedOs2,
            fs_type: Some(fs_type),
        };
    };
    if fs_type & !FS_TYPE_KNOWN_MASK != 0
        || (os2.version <= 1 && fs_type & (FS_TYPE_NO_SUBSETTING | FS_TYPE_BITMAP_ONLY) != 0)
    {
        return FontEmbeddingEligibility::Unknown {
            reason: FontEmbeddingUnknownReason::InvalidFsType,
            fs_type: Some(fs_type),
        };
    }

    let Some(permission) = os2.permissions() else {
        return FontEmbeddingEligibility::Unknown {
            reason: FontEmbeddingUnknownReason::InvalidFsType,
            fs_type: Some(fs_type),
        };
    };
    let license = match permission {
        Permissions::Installable => FontEmbeddingLicense::Installable,
        Permissions::PreviewAndPrint => FontEmbeddingLicense::PreviewAndPrint,
        Permissions::Editable => FontEmbeddingLicense::Editable,
        Permissions::Restricted => return FontEmbeddingEligibility::Restricted { fs_type },
    };

    if !os2.is_outline_embedding_allowed() {
        FontEmbeddingEligibility::BitmapOnly { license, fs_type }
    } else if !os2.is_subsetting_allowed() {
        FontEmbeddingEligibility::FullFontRequired { license, fs_type }
    } else {
        FontEmbeddingEligibility::SubsetAllowed { license, fs_type }
    }
}

/// Enumerates the font families exposed by the current operating system.
///
/// On Windows, Fontique obtains this catalog through DirectWrite. The returned
/// names are trimmed, case-insensitively deduplicated, and sorted for stable UI
/// presentation. This is discovery only: exact face fingerprints and embedding
/// permissions are resolved separately when the publication is laid out.
///
/// # Errors
///
/// Returns [`FontCatalogError`] when the operating-system font backend cannot
/// construct or enumerate its catalog.
pub fn installed_font_family_names() -> Result<Vec<String>, FontCatalogError> {
    catch_unwind(AssertUnwindSafe(|| {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: true,
        });
        normalize_font_family_names(collection.family_names().map(String::from))
    }))
    .map_err(|_| FontCatalogError::SystemEnumerationFailed)
}

/// Recoverable failure while asking the operating system for installed fonts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontCatalogError {
    /// The platform font backend could not construct or enumerate its catalog.
    SystemEnumerationFailed,
}

impl Display for FontCatalogError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SystemEnumerationFailed => formatter.write_str("system font enumeration failed"),
        }
    }
}

impl Error for FontCatalogError {}

fn normalize_font_family_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut by_folded_name = BTreeMap::new();
    for raw_name in names {
        let name = raw_name.trim();
        if name.is_empty() || name.chars().count() > 256 {
            continue;
        }
        by_folded_name
            .entry(name.to_lowercase())
            .or_insert_with(|| name.to_owned());
    }
    by_folded_name.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::{
        FontEmbeddingEligibility, FontEmbeddingLicense, FontEmbeddingUnknownReason,
        inspect_font_embedding_eligibility, inspect_font_face, normalize_font_family_names,
    };

    fn make_font(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let directory_len = 12 + tables.len() * 16;
        let mut font = vec![0; directory_len];
        font[..4].copy_from_slice(&0x0001_0000_u32.to_be_bytes());
        let table_count = u16::try_from(tables.len()).expect("test font has a bounded table count");
        font[4..6].copy_from_slice(&table_count.to_be_bytes());

        for (index, (tag, bytes)) in tables.iter().enumerate() {
            while !font.len().is_multiple_of(4) {
                font.push(0);
            }
            let offset = font.len();
            font.extend_from_slice(bytes);
            let record = 12 + index * 16;
            font[record..record + 4].copy_from_slice(tag);
            let offset = u32::try_from(offset).expect("test font offset fits in u32");
            let length = u32::try_from(bytes.len()).expect("test font table length fits in u32");
            font[record + 8..record + 12].copy_from_slice(&offset.to_be_bytes());
            font[record + 12..record + 16].copy_from_slice(&length.to_be_bytes());
        }
        font
    }

    fn font_with_os2(version_and_fs_type: Option<(u16, u16)>) -> Vec<u8> {
        let mut head = vec![0; 54];
        head[18..20].copy_from_slice(&1_000_u16.to_be_bytes());
        let hhea = vec![0; 36];
        let mut maxp = vec![0; 6];
        maxp[..4].copy_from_slice(&0x0000_5000_u32.to_be_bytes());
        maxp[4..6].copy_from_slice(&1_u16.to_be_bytes());
        let mut tables = vec![(*b"head", head), (*b"hhea", hhea), (*b"maxp", maxp)];
        if let Some((version, fs_type)) = version_and_fs_type {
            let mut os2 = vec![0; 96];
            os2[..2].copy_from_slice(&version.to_be_bytes());
            os2[8..10].copy_from_slice(&fs_type.to_be_bytes());
            tables.push((*b"OS/2", os2));
        }
        tables.sort_by_key(|(tag, _)| *tag);
        make_font(&tables)
    }

    fn font_with_fs_type(fs_type: Option<u16>) -> Vec<u8> {
        font_with_os2(fs_type.map(|fs_type| (3, fs_type)))
    }

    #[test]
    fn catalog_names_are_trimmed_sorted_and_case_insensitively_unique() {
        let names = normalize_font_family_names([
            "  Zeta  ".to_owned(),
            "arial".to_owned(),
            "Arial".to_owned(),
            String::new(),
            "Beta".to_owned(),
        ]);

        assert_eq!(names, ["arial", "Beta", "Zeta"]);
    }

    #[test]
    fn catalog_rejects_unreasonable_family_names() {
        let names =
            normalize_font_family_names(["Readable".to_owned(), "x".repeat(257), "   ".to_owned()]);

        assert_eq!(names, ["Readable"]);
    }

    #[test]
    fn embedding_metadata_distinguishes_subset_full_restricted_and_bitmap_only() {
        assert_eq!(
            inspect_font_embedding_eligibility(&font_with_fs_type(Some(0x0004)), 0),
            FontEmbeddingEligibility::SubsetAllowed {
                license: FontEmbeddingLicense::PreviewAndPrint,
                fs_type: 0x0004,
            }
        );
        assert_eq!(
            inspect_font_embedding_eligibility(&font_with_fs_type(Some(0x0108)), 0),
            FontEmbeddingEligibility::FullFontRequired {
                license: FontEmbeddingLicense::Editable,
                fs_type: 0x0108,
            }
        );
        assert_eq!(
            inspect_font_embedding_eligibility(&font_with_fs_type(Some(0x0002)), 0),
            FontEmbeddingEligibility::Restricted { fs_type: 0x0002 }
        );
        assert_eq!(
            inspect_font_embedding_eligibility(&font_with_fs_type(Some(0x0200)), 0),
            FontEmbeddingEligibility::BitmapOnly {
                license: FontEmbeddingLicense::Installable,
                fs_type: 0x0200,
            }
        );
    }

    #[test]
    fn missing_malformed_and_reserved_metadata_fail_closed() {
        assert_eq!(
            inspect_font_embedding_eligibility(b"not a font", 0),
            FontEmbeddingEligibility::Unknown {
                reason: FontEmbeddingUnknownReason::FaceUnavailableOrMalformed,
                fs_type: None,
            }
        );
        assert_eq!(
            inspect_font_embedding_eligibility(&font_with_fs_type(None), 0),
            FontEmbeddingEligibility::Unknown {
                reason: FontEmbeddingUnknownReason::MissingOrMalformedOs2,
                fs_type: None,
            }
        );
        assert_eq!(
            inspect_font_embedding_eligibility(&font_with_fs_type(Some(0x0400)), 0),
            FontEmbeddingEligibility::Unknown {
                reason: FontEmbeddingUnknownReason::InvalidFsType,
                fs_type: Some(0x0400),
            }
        );
        assert_eq!(
            inspect_font_embedding_eligibility(&font_with_fs_type(Some(0x0001)), 0),
            FontEmbeddingEligibility::Unknown {
                reason: FontEmbeddingUnknownReason::InvalidFsType,
                fs_type: Some(0x0001),
            }
        );
        assert_eq!(
            inspect_font_embedding_eligibility(&font_with_os2(Some((1, 0x0104))), 0),
            FontEmbeddingEligibility::Unknown {
                reason: FontEmbeddingUnknownReason::InvalidFsType,
                fs_type: Some(0x0104),
            }
        );
    }

    #[test]
    fn exact_face_identity_binds_all_source_bytes_and_face_index() {
        let font = font_with_fs_type(Some(0x0004));
        let first = inspect_font_face(&font, 0);
        let repeated = inspect_font_face(&font, 0);
        let other_face = inspect_font_face(&font, 1);
        let mut changed_font = font.clone();
        let last = changed_font.last_mut().expect("test font is not empty");
        *last ^= 1;
        let changed = inspect_font_face(&changed_font, 0);

        assert_eq!(first.identity(), repeated.identity());
        assert_eq!(
            first.identity().source_sha256(),
            other_face.identity().source_sha256()
        );
        assert_ne!(
            first.identity().face_sha256(),
            other_face.identity().face_sha256()
        );
        assert_ne!(
            first.identity().source_sha256(),
            changed.identity().source_sha256()
        );
        assert_ne!(
            first.identity().face_sha256(),
            changed.identity().face_sha256()
        );
        assert!(first.matches_source(&font, 0));
        assert!(!first.matches_source(&font, 1));
        assert!(!first.matches_source(&changed_font, 0));
    }

    #[test]
    fn exact_face_facts_keep_identity_when_eligibility_is_unknown() {
        let facts = inspect_font_face(b"not a font", 7);

        assert_eq!(facts.identity().face_index(), 7);
        assert_eq!(
            facts.embedding_eligibility(),
            FontEmbeddingEligibility::Unknown {
                reason: FontEmbeddingUnknownReason::FaceUnavailableOrMalformed,
                fs_type: None,
            }
        );
        assert!(facts.matches_source(b"not a font", 7));
    }

    #[test]
    fn face_fingerprint_encoding_is_stable_and_versioned() {
        let identity = inspect_font_face(b"font bytes", 3).identity();

        assert_eq!(
            identity.source_sha256(),
            [
                238, 46, 136, 48, 134, 16, 1, 157, 206, 56, 86, 3, 95, 181, 114, 183, 103, 93, 201,
                212, 201, 144, 9, 19, 116, 52, 148, 118, 181, 190, 236, 177,
            ]
        );
        assert_eq!(
            identity.face_sha256(),
            [
                222, 243, 205, 208, 43, 223, 139, 33, 172, 129, 6, 81, 225, 236, 149, 34, 40, 175,
                243, 137, 90, 220, 30, 125, 44, 177, 113, 162, 119, 184, 77, 107,
            ]
        );
    }
}
