use std::collections::{BTreeMap, BTreeSet};

use color_boundary::{ColorWorkerError, TrustedColorBoundary};
use document_core::model::{Color, ShapeKind};
use layout_core::{ResolvedPublication, ResolvedSceneObjectKind};
use sha2::{Digest, Sha256};

/// Exact worker evidence for the authored sRGB colors used on publication pages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductionColorEvidence {
    pub scene_digest: [u8; 32],
    pub source_profile_hash: [u8; 32],
    pub output_profile_hash: [u8; 32],
    pub raw_palette_hash: [u8; 32],
    pub converted_palette_hash: [u8; 32],
    pub color_engine_hash: [u8; 32],
    pub color_rules_hash: [u8; 32],
    pub color_worker_executable_hash: [u8; 32],
    pub color_count: u32,
}

/// Trusted CMYK values for every authored page color in one immutable scene.
pub struct ProductionColorPalette {
    evidence: ProductionColorEvidence,
    colors: BTreeMap<[u8; 3], [u8; 4]>,
}

impl ProductionColorPalette {
    #[must_use]
    pub const fn evidence(&self) -> &ProductionColorEvidence {
        &self.evidence
    }

    pub(crate) fn cmyk(&self, color: Color) -> Option<[u8; 4]> {
        self.colors.get(&rgb_key(color)).copied()
    }

    pub(crate) fn matches_publication(&self, publication: &ResolvedPublication) -> bool {
        self.evidence.scene_digest == publication.semantic_digest()
            && self.colors.keys().copied().collect::<BTreeSet<_>>()
                == publication_color_keys(publication)
            && usize::try_from(self.evidence.color_count).ok() == Some(self.colors.len())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductionColorError {
    TooManyColors,
    ColorWorker(ColorWorkerError),
    IdentityDrift,
}

/// Converts the exact unique sRGB colors painted by page objects to the selected
/// CMYK output profile. Pasteboard-only colors are deliberately excluded.
///
/// # Errors
///
/// Fails closed if the sRGB profile cannot be created, the bounded worker rejects
/// the conversion, or any returned identity differs from the requested palette.
pub fn prepare_publication_colors(
    publication: &ResolvedPublication,
    output_profile: &[u8],
    boundary: &TrustedColorBoundary,
) -> Result<Option<ProductionColorPalette>, ProductionColorError> {
    let keys = publication_color_keys(publication);
    if keys.is_empty() {
        return Ok(None);
    }
    let width = u32::try_from(keys.len()).map_err(|_| ProductionColorError::TooManyColors)?;
    if width > color_boundary::MAX_TAGGED_RASTER_DIMENSION {
        return Err(ProductionColorError::TooManyColors);
    }
    let raw = keys.iter().flatten().copied().collect::<Vec<_>>();
    let converted = boundary
        .convert_srgb(&raw, width, 1, output_profile)
        .map_err(ProductionColorError::ColorWorker)?;
    if converted.width() != width
        || converted.height() != 1
        || converted.raw_pixel_hash() != hash(&raw)
        || converted.output_profile_hash() != hash(output_profile)
        || converted.cmyk_pixels().len() != keys.len().saturating_mul(4)
    {
        return Err(ProductionColorError::IdentityDrift);
    }
    let colors = keys
        .into_iter()
        .zip(converted.cmyk_pixels().as_chunks::<4>().0.iter().copied())
        .collect::<BTreeMap<_, _>>();
    if colors.len() != usize::try_from(width).unwrap_or(usize::MAX) {
        return Err(ProductionColorError::IdentityDrift);
    }
    Ok(Some(ProductionColorPalette {
        evidence: ProductionColorEvidence {
            scene_digest: publication.semantic_digest(),
            source_profile_hash: converted.source_profile_hash(),
            output_profile_hash: converted.output_profile_hash(),
            raw_palette_hash: converted.raw_pixel_hash(),
            converted_palette_hash: converted.converted_cmyk_hash(),
            color_engine_hash: converted.engine_identity_hash(),
            color_rules_hash: converted.rules_identity_hash(),
            color_worker_executable_hash: converted.worker_executable_hash(),
            color_count: width,
        },
        colors,
    }))
}

pub(crate) fn publication_color_keys(publication: &ResolvedPublication) -> BTreeSet<[u8; 3]> {
    let mut colors = BTreeSet::new();
    for object in publication.pages().iter().flat_map(|page| &page.objects) {
        match &object.kind {
            ResolvedSceneObjectKind::Text { color, layout } => {
                colors.insert(rgb_key(*color));
                for run in layout.lines.iter().flat_map(|line| &line.glyph_runs) {
                    colors.insert(rgb_key(run.color));
                }
            }
            ResolvedSceneObjectKind::Shape {
                shape,
                fill,
                stroke,
            } => {
                if *shape == ShapeKind::Cube {
                    for shade in [86, 70, 55] {
                        colors.insert(rgb_key(cube_face_color(*fill, shade)));
                    }
                } else {
                    colors.insert(rgb_key(*fill));
                }
                if stroke.width_millipoints > 0 {
                    colors.insert(rgb_key(stroke.color));
                }
            }
            ResolvedSceneObjectKind::Table { table } => {
                for fill in &table.cell_fills {colors.insert(rgb_key(fill.color));}
                for edge in &table.border_segments {colors.insert(rgb_key(edge.color));}
                if table.grid_width_millipoints > 0 {
                    colors.insert(rgb_key(table.grid_color));
                }
                if !table.populated_cells.is_empty() {
                    colors.insert(rgb_key(table.text_color));
                    for run in table
                        .populated_cells
                        .iter()
                        .flat_map(|cell| &cell.layout.lines)
                        .flat_map(|line| &line.glyph_runs)
                    {
                        colors.insert(rgb_key(run.color));
                    }
                }
            }
            ResolvedSceneObjectKind::Image { .. } => {}
        }
        if let Some(shadow) = object
            .effects
            .shadow
            .filter(|shadow| shadow.color.alpha > 0)
        {
            colors.insert(rgb_key(shadow.color));
        }
        if let Some(outline) = object
            .effects
            .text_outline
            .filter(|stroke| stroke.width_millipoints > 0 && stroke.color.alpha > 0)
        {
            colors.insert(rgb_key(outline.color));
        }
        if let Some(border) = object
            .effects
            .picture_border
            .filter(|stroke| stroke.width_millipoints > 0 && stroke.color.alpha > 0)
        {
            colors.insert(rgb_key(border.color));
        }
    }
    colors
}

const fn rgb_key(color: Color) -> [u8; 3] {
    [color.red, color.green, color.blue]
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

// Mix each face with white without making an opaque solid see-through.
// Alpha remains authored paint alpha, independent of intrinsic shading.
pub(crate) fn cube_face_color(color: Color, shade: u32) -> Color {
    let mix = |channel: u8| {
        u8::try_from((u32::from(channel) * shade + 255 * (100 - shade) + 50) / 100)
            .expect("cube shade is a percentage")
    };
    Color {
        red: mix(color.red),
        green: mix(color.green),
        blue: mix(color.blue),
        alpha: color.alpha,
    }
}
