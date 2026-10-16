//! Content-addressed embedded assets.

use crate::model::{Document, ObjectKind, PublicationObject};
use image::{ImageDecoder, ImageFormat, ImageReader, Limits, metadata::Orientation};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::io::Cursor;
use std::sync::Arc;
use tiff::{
    decoder::Decoder as TiffDecoder,
    tags::{Tag as TiffTag, Type as TiffType},
};

pub const MAX_RASTER_FILE_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_RASTER_DIMENSION: u32 = 30_000;
pub const MAX_DECODED_RASTER_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RasterInspection {
    pub media_type: String,
    pub width_pixels: u32,
    pub height_pixels: u32,
    pub oriented_width_pixels: u32,
    pub oriented_height_pixels: u32,
    pub exif_orientation: u8,
    pub decoded_bytes: u64,
    pub color_profile: RasterColorProfile,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RasterColorProfile {
    Embedded { sha256: AssetHash, byte_length: u64 },
    Unspecified,
}

/// Inspects only supported inert raster formats under hard encoded, decoded,
/// dimension, and metadata budgets. Original bytes are not rewritten.
///
/// # Errors
///
/// Returns a structured rejection for empty, excessive, unsupported, corrupt,
/// or metadata-heavy input.
pub fn inspect_raster(bytes: &[u8]) -> Result<RasterInspection, RasterIngestError> {
    if bytes.is_empty() {
        return Err(RasterIngestError::Empty);
    }
    if bytes.len() > MAX_RASTER_FILE_BYTES {
        return Err(RasterIngestError::EncodedBytesExceeded);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| RasterIngestError::Corrupt)?;
    let format = reader.format().ok_or(RasterIngestError::Unsupported)?;
    let media_type = match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Tiff => "image/tiff",
        _ => return Err(RasterIngestError::Unsupported),
    };
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODED_RASTER_BYTES);
    reader.limits(limits);
    let mut decoder = reader
        .into_decoder()
        .map_err(|_| RasterIngestError::Corrupt)?;
    let (width_pixels, height_pixels) = decoder.dimensions();
    if width_pixels == 0 || height_pixels == 0 {
        return Err(RasterIngestError::InvalidDimensions);
    }
    if width_pixels > MAX_RASTER_DIMENSION || height_pixels > MAX_RASTER_DIMENSION {
        return Err(RasterIngestError::DimensionsExceeded);
    }
    let decoded_bytes = decoder.total_bytes();
    if decoded_bytes > MAX_DECODED_RASTER_BYTES {
        return Err(RasterIngestError::DecodedBytesExceeded);
    }
    // image's TIFF decoder can silently omit an ICC tag when its metadata
    // allocation limit is smaller than the profile (notably on tiny images).
    let profile = if format == ImageFormat::Tiff {
        tiff_embedded_icc_profile(bytes)?
    } else {
        decoder
            .icc_profile()
            .map_err(|_| RasterIngestError::Corrupt)?
    };
    let color_profile = match profile {
        Some(profile) if profile.len() > MAX_ICC_PROFILE_BYTES => {
            return Err(RasterIngestError::ColorProfileExceeded);
        }
        Some(profile) => RasterColorProfile::Embedded {
            sha256: AssetHash::for_bytes(&profile),
            byte_length: u64::try_from(profile.len())
                .map_err(|_| RasterIngestError::ColorProfileExceeded)?,
        },
        None => RasterColorProfile::Unspecified,
    };
    let orientation = decoder
        .orientation()
        .map_err(|_| RasterIngestError::Corrupt)?;
    let swaps_dimensions = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    let (oriented_width_pixels, oriented_height_pixels) = if swaps_dimensions {
        (height_pixels, width_pixels)
    } else {
        (width_pixels, height_pixels)
    };
    Ok(RasterInspection {
        media_type: media_type.to_owned(),
        width_pixels,
        height_pixels,
        oriented_width_pixels,
        oriented_height_pixels,
        exif_orientation: orientation.to_exif(),
        decoded_bytes,
        color_profile,
    })
}

/// Reads the first TIFF image's ICC tag without decoding it into a list of
/// per-byte values. The declared byte count is checked before allocation.
///
/// # Errors
///
/// Rejects excessive encoded bytes or an invalid recognized field type,
/// truncated value, or oversized present ICC tag. The TIFF decoder omits
/// entries with unknown field-type codes before this helper can inspect them.
pub fn tiff_embedded_icc_profile(bytes: &[u8]) -> Result<Option<Vec<u8>>, RasterIngestError> {
    if bytes.is_empty() {
        return Err(RasterIngestError::Empty);
    }
    if bytes.len() > MAX_RASTER_FILE_BYTES {
        return Err(RasterIngestError::EncodedBytesExceeded);
    }
    let mut decoder =
        TiffDecoder::new(Cursor::new(bytes)).map_err(|_| RasterIngestError::Corrupt)?;
    let mut ifd = decoder.image_ifd();
    let Some(entry) = ifd.find_entry(TiffTag::IccProfile) else {
        return Ok(None);
    };
    if !matches!(entry.field_type(), TiffType::BYTE | TiffType::UNDEFINED) || entry.count() == 0 {
        return Err(RasterIngestError::Corrupt);
    }
    let count =
        usize::try_from(entry.count()).map_err(|_| RasterIngestError::ColorProfileExceeded)?;
    if count > MAX_ICC_PROFILE_BYTES {
        return Err(RasterIngestError::ColorProfileExceeded);
    }
    let mut profile = vec![0; count];
    let read = ifd
        .find_tag_bytes(TiffTag::IccProfile, &mut profile, 0)
        .map_err(|_| RasterIngestError::Corrupt)?
        .ok_or(RasterIngestError::Corrupt)?;
    if read != count {
        return Err(RasterIngestError::Corrupt);
    }
    Ok(Some(profile))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RasterIngestError {
    Empty,
    EncodedBytesExceeded,
    Unsupported,
    Corrupt,
    InvalidDimensions,
    DimensionsExceeded,
    DecodedBytesExceeded,
    ColorProfileExceeded,
}

impl std::fmt::Display for RasterIngestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "raster import rejected: {self:?}")
    }
}

impl std::error::Error for RasterIngestError {}

/// A lowercase SHA-256 digest used as the durable identity of embedded bytes.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct AssetHash(String);

impl AssetHash {
    /// Hashes bytes without interpreting or executing them.
    #[must_use]
    pub fn for_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self(format!("{digest:x}"))
    }

    /// Parses a canonical lowercase SHA-256 digest.
    ///
    /// # Errors
    ///
    /// Returns [`AssetHashError`] when `value` is not exactly 64 lowercase
    /// hexadecimal characters.
    pub fn parse(value: impl Into<String>) -> Result<Self, AssetHashError> {
        let value = value.into();
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value))
        } else {
            Err(AssetHashError)
        }
    }

    /// Returns the canonical filename-safe digest.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for AssetHash {
    fn deserialize<DeserializerType>(
        deserializer: DeserializerType,
    ) -> Result<Self, DeserializerType::Error>
    where
        DeserializerType: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// A malformed content digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AssetHashError;

impl std::fmt::Display for AssetHashError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("asset hash must be 64 lowercase hexadecimal characters")
    }
}

impl std::error::Error for AssetHashError {}

/// Original asset bytes plus their declared inert media type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddedAsset {
    pub media_type: String,
    pub bytes: Vec<u8>,
    pub raster: Option<RasterInspection>,
}

/// A deterministic, de-duplicated collection of embedded assets.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AssetStore {
    assets: BTreeMap<AssetHash, Arc<EmbeddedAsset>>,
}

impl AssetStore {
    /// Inserts original bytes, de-duplicating identical content.
    pub fn insert(&mut self, media_type: impl Into<String>, bytes: Vec<u8>) -> AssetHash {
        let hash = AssetHash::for_bytes(&bytes);
        if let Entry::Vacant(entry) = self.assets.entry(hash.clone()) {
            entry.insert(Arc::new(EmbeddedAsset {
                media_type: media_type.into(),
                bytes,
                raster: None,
            }));
        }
        hash
    }

    /// Inserts a raster whose bytes have been hash-bound to a validated worker report.
    pub fn insert_raster(&mut self, bytes: Vec<u8>, raster: RasterInspection) -> AssetHash {
        let hash = AssetHash::for_bytes(&bytes);
        if let Entry::Vacant(entry) = self.assets.entry(hash.clone()) {
            entry.insert(Arc::new(EmbeddedAsset {
                media_type: raster.media_type.clone(),
                bytes,
                raster: Some(raster),
            }));
        }
        hash
    }

    /// Inserts an asset whose expected hash came from a validated manifest.
    pub(crate) fn insert_verified(
        &mut self,
        hash: AssetHash,
        asset: EmbeddedAsset,
    ) -> Result<(), AssetIntegrityError> {
        if AssetHash::for_bytes(&asset.bytes) != hash {
            return Err(AssetIntegrityError { hash });
        }
        self.assets.insert(hash, Arc::new(asset));
        Ok(())
    }

    #[must_use]
    pub fn get(&self, hash: &AssetHash) -> Option<&EmbeddedAsset> {
        self.assets.get(hash).map(AsRef::as_ref)
    }

    #[must_use]
    pub fn contains(&self, hash: &AssetHash) -> bool {
        self.assets.contains_key(hash)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.assets.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    /// Releases only entries unreachable from the host's current and retained history.
    /// Other cloned stores keep their immutable asset bytes alive independently.
    pub fn retain_hashes(&mut self, retained: &BTreeSet<AssetHash>) {
        self.assets.retain(|hash, _| retained.contains(hash));
    }

    /// Selects resources named by this snapshot, sharing immutable bytes with history.
    /// Missing references remain the package validator's responsibility.
    #[must_use]
    pub fn for_document(&self, document: &Document) -> Self {
        let assets = referenced_asset_hashes(document)
            .into_iter()
            .filter_map(|hash| {
                self.assets
                    .get(&hash)
                    .map(|asset| (hash, Arc::clone(asset)))
            })
            .collect();
        Self { assets }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&AssetHash, &EmbeddedAsset)> {
        self.assets
            .iter()
            .map(|(hash, asset)| (hash, asset.as_ref()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AssetIntegrityError {
    pub hash: AssetHash,
}

/// Every durable resource reference, including unassigned masters, pasteboard and exact
/// font bindings. Shared resources appear once; undo history is intentionally absent.
#[must_use]
pub fn referenced_asset_hashes(document: &Document) -> BTreeSet<AssetHash> {
    let mut hashes = referenced_object_asset_hashes(document.authored_objects());
    for chain in &document.exact_font_fallbacks {
        hashes.extend(chain.faces.iter().map(|face| face.source.clone()));
    }
    hashes
}

/// Resources needed by selected objects, including exact variants and range families.
#[must_use]
pub fn referenced_object_asset_hashes<'a>(
    objects: impl IntoIterator<Item = &'a PublicationObject>,
) -> BTreeSet<AssetHash> {
    let mut hashes = BTreeSet::new();
    for object in objects {
        let style = match &object.kind {
            ObjectKind::Image { asset, .. } => {
                hashes.insert(asset.clone());
                continue;
            }
            ObjectKind::Text { style, .. } => style,
            ObjectKind::Table { table } => &table.style,
            ObjectKind::Shape { .. } => continue,
        };
        hashes.extend(style.font_face.iter().map(|face| face.source.clone()));
        hashes.extend(
            style
                .exact_font_variants
                .iter()
                .map(|variant| variant.face.source.clone()),
        );
        hashes.extend(
            style
                .exact_font_families
                .iter()
                .map(|binding| binding.face.source.clone()),
        );
    }
    hashes
}
