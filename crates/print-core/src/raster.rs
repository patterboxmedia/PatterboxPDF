use document_core::raster_capacity::{MAX_PREPARED_RASTER_CACHE_BYTES, RasterMemoryPermit};
use std::collections::BTreeMap;
use std::sync::Arc;

use color_boundary::{ColorWorkerError, TrustedColorBoundary};
use document_core::{
    AssetHash, AssetStore, EmbeddedAsset, ObjectId, PageId, RasterColorProfile,
    UntaggedRasterPolicy,
};
use layout_core::{ResolvedPublication, ResolvedSceneObjectKind};
use raster_boundary::{
    RasterSourceProfileOrigin, RasterWorkerError, TrustedDecodedRaster, TrustedRasterBoundary,
};

/// Hash-bound proof for a placed production raster. These facts cover both
/// private workers and the exact bytes later embedded into the PDF.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductionRasterEvidence {
    pub asset: AssetHash,
    pub width: u32,
    pub height: u32,
    pub encoded_asset_hash: [u8; 32],
    pub decoded_rgb_hash: [u8; 32],
    pub alpha_hash: [u8; 32],
    pub source_profile_hash: [u8; 32],
    pub source_profile_origin: RasterSourceProfileOrigin,
    pub decode_rules_hash: [u8; 32],
    pub raster_worker_executable_hash: [u8; 32],
    pub output_profile_hash: [u8; 32],
    pub converted_cmyk_hash: [u8; 32],
    pub color_engine_hash: [u8; 32],
    pub color_rules_hash: [u8; 32],
    pub color_worker_executable_hash: [u8; 32],
    pub has_transparency: bool,
}

/// Losslessly compressed exact CMYK and optional soft-mask planes accepted from both workers.
pub struct ProductionRaster {
    evidence: ProductionRasterEvidence,
    cmyk: Arc<Vec<u8>>,
    alpha: Arc<Vec<u8>>,
    decoded_cmyk_bytes: usize,
    _memory: RasterMemoryPermit,
}

impl ProductionRaster {
    /// Moves the exact compressed planes and retained-byte lease into neutral resources.
    /// # Errors
    /// Rejects an empty caller resource identity.
    pub fn into_finished_resource(
        self,
        id: String,
    ) -> Result<crate::finished::ImageResource, crate::finished::FinishedPdfError> {
        self.finished_resource(id)
    }

    #[expect(
        clippy::used_underscore_binding,
        reason = "retain the shared admitted-memory lease"
    )]
    pub(crate) fn finished_resource(
        &self,
        id: String,
    ) -> Result<crate::finished::ImageResource, crate::finished::FinishedPdfError> {
        if id.len() > crate::finished::MAX_FINISHED_RESOURCE_ID_BYTES {
            return Err(crate::finished::FinishedPdfError::ResourceLimit);
        }
        if id.is_empty() {
            return Err(crate::finished::FinishedPdfError::InvalidImage(id));
        }
        Ok(crate::finished::ImageResource {
            id,
            width: self.evidence.width,
            height: self.evidence.height,
            color_space: crate::finished::ImageColorSpace::Cmyk,
            color: Arc::clone(&self.cmyk),
            alpha: self
                .evidence
                .has_transparency
                .then(|| Arc::clone(&self.alpha)),
            _memory: self._memory.clone(),
        })
    }

    #[must_use]
    pub const fn evidence(&self) -> &ProductionRasterEvidence {
        &self.evidence
    }
    pub(crate) fn cmyk(&self) -> &[u8] {
        &self.cmyk
    }
    pub(crate) const fn decoded_cmyk_bytes(&self) -> usize {
        self.decoded_cmyk_bytes
    }
    pub(crate) fn alpha(&self) -> Option<&[u8]> {
        self.evidence
            .has_transparency
            .then_some(self.alpha.as_slice())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductionRasterError {
    MissingAsset(AssetHash),
    UnvalidatedAsset(AssetHash),
    StoredInspectionDrift(AssetHash),
    UntaggedRasterRequiresPolicy {
        asset: AssetHash,
        page_id: PageId,
        object_id: ObjectId,
    },
    RasterWorker(RasterWorkerError),
    ColorWorker(ColorWorkerError),
    IdentityDrift(AssetHash),
    ResourceLimit,
}

pub(crate) fn compress_raster_stream(
    bytes: &[u8],
    maximum: usize,
) -> Result<Vec<u8>, ProductionRasterError> {
    crate::resources::compress_raster_stream(bytes, maximum)
        .map_err(|_| ProductionRasterError::ResourceLimit)
}

/// Runs every referenced page raster through decode and color workers.
/// Pasteboard-only assets are deliberately excluded.
///
/// # Errors
///
/// Fails closed when an asset/profile policy is missing, stored import evidence
/// has drifted, either worker rejects, or any decoded/conversion identity
/// differs.
pub fn prepare_publication_rasters(
    publication: &ResolvedPublication,
    assets: &AssetStore,
    output_profile: &[u8],
    raster_boundary: &TrustedRasterBoundary,
    color_boundary: &TrustedColorBoundary,
) -> Result<Vec<ProductionRaster>, ProductionRasterError> {
    let mut identities = BTreeMap::new();
    for page in publication.pages() {
        for object in &page.objects {
            if let ResolvedSceneObjectKind::Image { asset, .. } = &object.kind {
                identities
                    .entry(asset.clone())
                    .or_insert_with(|| (page.page_id.clone(), object.object_id.clone()));
            }
        }
    }
    let mut result = Vec::with_capacity(identities.len());
    let mut retained_cache_bytes = 0_usize;
    for (asset_hash, (page_id, object_id)) in identities {
        let asset = assets
            .get(&asset_hash)
            .ok_or_else(|| ProductionRasterError::MissingAsset(asset_hash.clone()))?;
        let decoded = decode_publication_raster(
            &asset_hash,
            asset,
            page_id,
            object_id,
            publication.untagged_raster_policy(),
            raster_boundary,
        )?;
        let converted = color_boundary
            .convert(
                decoded.rgb_pixels(),
                decoded.width(),
                decoded.height(),
                decoded.source_profile(),
                output_profile,
            )
            .map_err(ProductionRasterError::ColorWorker)?;
        if converted.width() != decoded.width()
            || converted.height() != decoded.height()
            || converted.raw_pixel_hash() != decoded.rgb_hash()
            || converted.source_profile_hash() != decoded.profile_hash()
        {
            return Err(ProductionRasterError::IdentityDrift(asset_hash));
        }
        let mut encoded = [0_u8; 32];
        decode_asset_hash(&asset_hash, &mut encoded)
            .map_err(|()| ProductionRasterError::IdentityDrift(asset_hash.clone()))?;
        let remaining = MAX_PREPARED_RASTER_CACHE_BYTES - retained_cache_bytes;
        let mut memory = RasterMemoryPermit::reserve(remaining)
            .map_err(|_| ProductionRasterError::ResourceLimit)?;
        let cmyk = compress_raster_stream(converted.cmyk_pixels(), remaining)?;
        let alpha = if decoded.has_transparency() {
            let alpha_budget = remaining
                .checked_sub(cmyk.capacity())
                .ok_or(ProductionRasterError::ResourceLimit)?;
            compress_raster_stream(decoded.alpha_pixels(), alpha_budget)?
        } else {
            Vec::new()
        };
        let cache_bytes = cmyk.capacity() + alpha.capacity();
        retained_cache_bytes += cache_bytes;
        memory
            .shrink_to(cache_bytes)
            .map_err(|_| ProductionRasterError::ResourceLimit)?;
        result.push(ProductionRaster {
            evidence: ProductionRasterEvidence {
                asset: asset_hash,
                width: decoded.width(),
                height: decoded.height(),
                encoded_asset_hash: encoded,
                decoded_rgb_hash: decoded.rgb_hash(),
                alpha_hash: decoded.alpha_hash(),
                source_profile_hash: decoded.profile_hash(),
                source_profile_origin: decoded.source_profile_origin(),
                decode_rules_hash: decoded.rules_hash(),
                raster_worker_executable_hash: decoded.worker_executable_hash(),
                output_profile_hash: converted.output_profile_hash(),
                converted_cmyk_hash: converted.converted_cmyk_hash(),
                color_engine_hash: converted.engine_identity_hash(),
                color_rules_hash: converted.rules_identity_hash(),
                color_worker_executable_hash: converted.worker_executable_hash(),
                has_transparency: decoded.has_transparency(),
            },
            cmyk: Arc::new(cmyk),
            alpha: Arc::new(alpha),
            decoded_cmyk_bytes: converted.cmyk_pixels().len(),
            _memory: memory,
        });
    }
    Ok(result)
}

fn decode_publication_raster(
    asset_hash: &AssetHash,
    asset: &EmbeddedAsset,
    page_id: PageId,
    object_id: ObjectId,
    policy: UntaggedRasterPolicy,
    raster_boundary: &TrustedRasterBoundary,
) -> Result<TrustedDecodedRaster, ProductionRasterError> {
    let stored = asset
        .raster
        .as_ref()
        .ok_or_else(|| ProductionRasterError::UnvalidatedAsset(asset_hash.clone()))?;
    let unspecified = matches!(stored.color_profile, RasterColorProfile::Unspecified);
    let refresh_tiff =
        unspecified && asset.media_type == "image/tiff" && stored.media_type == "image/tiff";
    let requires_profile = policy == UntaggedRasterPolicy::RequireEmbeddedProfile;
    if unspecified && requires_profile && !refresh_tiff {
        return Err(ProductionRasterError::UntaggedRasterRequiresPolicy {
            asset: asset_hash.clone(),
            page_id,
            object_id,
        });
    }
    // Older TIFF inspection could lose an embedded ICC profile. Gather current
    // hash-bound evidence even when the saved metadata says it was untagged;
    // enforce the publication's actual policy before any color conversion.
    let decode_policy = if refresh_tiff {
        UntaggedRasterPolicy::AssumeSrgb
    } else {
        policy
    };
    let decoded = raster_boundary
        .decode(&asset.bytes, asset_hash, decode_policy)
        .map_err(ProductionRasterError::RasterWorker)?;
    let mut expected = stored.clone();
    if refresh_tiff && decoded.source_profile_origin() == RasterSourceProfileOrigin::Embedded {
        expected.color_profile = decoded.inspection().color_profile.clone();
    }
    if decoded.inspection() != &expected {
        return Err(ProductionRasterError::StoredInspectionDrift(
            asset_hash.clone(),
        ));
    }
    if requires_profile && decoded.source_profile_origin() == RasterSourceProfileOrigin::AssumedSrgb
    {
        return Err(ProductionRasterError::UntaggedRasterRequiresPolicy {
            asset: asset_hash.clone(),
            page_id,
            object_id,
        });
    }
    Ok(decoded)
}

fn decode_asset_hash(hash: &AssetHash, output: &mut [u8; 32]) -> Result<(), ()> {
    for (index, pair) in hash
        .as_str()
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .enumerate()
    {
        let text = std::str::from_utf8(pair).map_err(|_| ())?;
        output[index] = u8::from_str_radix(text, 16).map_err(|_| ())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ProductionRasterError, compress_raster_stream};
    use std::io::Read;
    #[test]
    fn lossless_streams_preserve_pixels_and_refuse_before_excessive_cache_growth() {
        let pixels = (0..65_536)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect::<Vec<_>>();
        let compressed = compress_raster_stream(&pixels, 4_096).unwrap();
        let mut decoded = Vec::new();
        flate2::read::ZlibDecoder::new(compressed.as_slice())
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, pixels);
        assert_eq!(
            compress_raster_stream(&pixels, 16),
            Err(ProductionRasterError::ResourceLimit)
        );
    }
}
