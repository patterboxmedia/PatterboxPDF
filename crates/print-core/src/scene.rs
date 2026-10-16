//! Deterministic serialization of an immutable publication scene.
//!
//! Existing caller contracts validate immutable scene/resource evidence, then
//! lower into the neutral finished-page writer. Candidate envelopes are still
//! unvalidated output; independent checking must inspect their exact bytes.

#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "PDF operators use f32; every canonical fixed-point value is range-checked before the deliberate final conversion"
)]

use std::collections::{BTreeMap, BTreeSet};

use document_core::AssetHash;
use document_core::model::{EdgeExpectations, ShapeKind, Transform};
use layout_core::{
    PreviewBudgetState, ResolvedPageScene, ResolvedPublication, ResolvedSceneObject,
    ResolvedSceneObjectKind, TextBoxPreviewState, geometry::PhysicalBounds,
};
use pdf_writer::Rect;

use crate::{
    ProductionColorPalette, ProductionFont, ProductionFontEvidence, ProductionRaster,
    ProductionRasterEvidence,
    font_contract::{FontContractFailure, validate_layout_font_contract},
};

const POINTS_PER_MICROMETRE: f64 = 72.0 / 25_400.0;
const MAX_ABSOLUTE_PDF_POINTS: f64 = 10_000_000.0;

/// Print structure added around the exact scene traversal. Inputs reach this
/// type only after the production-candidate boundary validates them.
#[derive(Clone, Copy)]
pub(crate) struct ScenePrintEnvelope<'a> {
    pub output_profile: &'a [u8],
    pub metadata: &'a crate::PdfCandidateMetadata,
}

/// How unresolved raster identities are handled while serializing a scene.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MissingImagePolicy {
    /// Stop without producing bytes. This is the safe production default.
    Reject,
    /// Paint a conspicuous deterministic placeholder and record it in evidence.
    VisiblePlaceholder,
}

/// Configuration for scene serialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScenePdfOptions {
    pub missing_images: MissingImagePolicy,
}

impl Default for ScenePdfOptions {
    fn default() -> Self {
        Self {
            missing_images: MissingImagePolicy::Reject,
        }
    }
}

/// Per-page evidence gathered from the exact scene traversal used to paint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerializedPageEvidence {
    pub page_id: String,
    pub media_box: PhysicalBounds,
    pub bleed_box: PhysicalBounds,
    pub trim_box: PhysicalBounds,
    pub object_ids_in_paint_order: Vec<String>,
    pub placeholder_image_ids: Vec<String>,
    pub clipped_table_cells: Vec<SerializedTableCellAddress>,
    pub bleed_expectations: EdgeExpectations,
}

/// One table cell whose source text exceeded its padded layout region and was
/// clipped to the cell rectangle during the exact scene traversal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerializedTableCellAddress {
    pub object_id: String,
    pub row: u16,
    pub column: u16,
}

/// Evidence bound to the returned bytes. This is not a conformance verdict.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneSerializationEvidence {
    pub scene_digest: [u8; 32],
    pub pages: Vec<SerializedPageEvidence>,
    pub pasteboard_object_count_excluded: usize,
    pub uses_device_rgb: bool,
    pub text_object_count: usize,
    pub text_painted_as_native_outlines: bool,
    pub embedded_fonts: Vec<ProductionFontEvidence>,
    pub transparency: TransparencyEvidence,
    pub raster_placements: Vec<SerializedRasterPlacementEvidence>,
}

/// Exact supported transparency modes represented in the serialized scene.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransparencyEvidence {
    pub object_opacity: bool,
    pub paint_alpha: bool,
    pub raster_soft_mask: bool,
}

impl TransparencyEvidence {
    #[must_use]
    pub const fn has_live_transparency(self) -> bool {
        self.object_opacity || self.paint_alpha || self.raster_soft_mask
    }

    fn include(&mut self, other: Self) {
        self.object_opacity |= other.object_opacity;
        self.paint_alpha |= other.paint_alpha;
        self.raster_soft_mask |= other.raster_soft_mask;
    }
}

/// Exact scene placement bound to both worker attestations and embedded pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerializedRasterPlacementEvidence {
    pub page_id: String,
    pub object_id: String,
    pub scene_digest: [u8; 32],
    pub size: document_core::model::PhysicalSize,
    pub transform: Transform,
    pub crop: document_core::model::ImageCrop,
    pub flip: document_core::model::ImageFlip,
    pub raster: ProductionRasterEvidence,
}

/// A deterministic PDF containing the exact ordered export page scenes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerializedScenePdf {
    bytes: Vec<u8>,
    evidence: SceneSerializationEvidence,
}

impl SerializedScenePdf {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn evidence(&self) -> &SceneSerializationEvidence {
        &self.evidence
    }
}

/// A scene fact that cannot yet be represented honestly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneSerializationError {
    NoPages,
    PreviewBudgetIncomplete,
    InvalidPageBoxes,
    GeometryOutOfRange,
    MissingImageAsset,
    TextPreviewIncomplete,
    MissingGlyphOutline,
    UnsupportedGlyphFallback,
    UnsupportedFontSynthesis,
    FontBindingUnresolved,
    FontBindingIdentityMismatch,
    InvalidTableGeometry,
    InvalidRasterEvidence,
    InvalidFontEvidence,
    MissingPreparedFont,
    MissingPreparedColor,
    GlyphIdOutOfRange,
    FinishedInputRejected,
}

/// Serializes resolved page scenes without performing layout or reshaping.
pub struct ScenePdfWriter;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FontContractMode {
    Strict,
    DiagnosticAuthoring,
}

impl ScenePdfWriter {
    /// Paints the canonical scene in page/object order.
    ///
    /// # Errors
    ///
    /// Returns [`SceneSerializationError`] before any bytes are returned when
    /// geometry, text, images, or table content cannot be represented exactly
    /// under the supplied policy.
    pub fn write(
        publication: &ResolvedPublication,
        options: ScenePdfOptions,
    ) -> Result<SerializedScenePdf, SceneSerializationError> {
        Self::write_internal(
            publication,
            options,
            &[],
            None,
            None,
            None,
            FontContractMode::Strict,
        )
    }

    /// Paints the canonical scene using exact trusted CMYK raster resources.
    ///
    /// # Errors
    ///
    /// Rejects missing, duplicate, or unreferenced raster identities before
    /// exposing any PDF bytes.
    pub fn write_with_rasters(
        publication: &ResolvedPublication,
        options: ScenePdfOptions,
        rasters: &[ProductionRaster],
    ) -> Result<SerializedScenePdf, SceneSerializationError> {
        Self::write_internal(
            publication,
            options,
            rasters,
            None,
            None,
            None,
            FontContractMode::Strict,
        )
    }

    /// Paints exact scene text with worker-prepared embedded font subsets and
    /// exact trusted CMYK raster resources.
    ///
    /// # Errors
    ///
    /// Rejects missing, duplicate, unreferenced, or scene-drifted font and
    /// raster evidence before exposing PDF bytes.
    pub fn write_with_rasters_and_fonts(
        publication: &ResolvedPublication,
        options: ScenePdfOptions,
        rasters: &[ProductionRaster],
        fonts: &[ProductionFont],
    ) -> Result<SerializedScenePdf, SceneSerializationError> {
        Self::write_internal(
            publication,
            options,
            rasters,
            Some(fonts),
            None,
            None,
            FontContractMode::Strict,
        )
    }

    /// Paints exact scene resources with a worker-attested CMYK palette.
    ///
    /// # Errors
    ///
    /// Rejects any missing authored color or scene-drifted color evidence.
    pub fn write_with_rasters_fonts_and_colors(
        publication: &ResolvedPublication,
        options: ScenePdfOptions,
        rasters: &[ProductionRaster],
        fonts: Option<&[ProductionFont]>,
        colors: Option<&ProductionColorPalette>,
    ) -> Result<SerializedScenePdf, SceneSerializationError> {
        Self::write_internal(
            publication,
            options,
            rasters,
            fonts,
            colors,
            None,
            FontContractMode::Strict,
        )
    }

    pub(crate) fn write_candidate_with_resources(
        publication: &ResolvedPublication,
        options: ScenePdfOptions,
        rasters: &[ProductionRaster],
        fonts: Option<&[ProductionFont]>,
        colors: Option<&ProductionColorPalette>,
    ) -> Result<SerializedScenePdf, SceneSerializationError> {
        Self::write_internal(
            publication,
            options,
            rasters,
            fonts,
            colors,
            None,
            candidate_font_contract_mode(fonts),
        )
    }

    pub(crate) fn write_candidate_print_enveloped_with_resources(
        publication: &ResolvedPublication,
        options: ScenePdfOptions,
        rasters: &[ProductionRaster],
        fonts: Option<&[ProductionFont]>,
        colors: Option<&ProductionColorPalette>,
        envelope: ScenePrintEnvelope<'_>,
    ) -> Result<SerializedScenePdf, SceneSerializationError> {
        Self::write_internal(
            publication,
            options,
            rasters,
            fonts,
            colors,
            Some(envelope),
            candidate_font_contract_mode(fonts),
        )
    }

    fn write_internal(
        publication: &ResolvedPublication,
        options: ScenePdfOptions,
        rasters: &[ProductionRaster],
        fonts: Option<&[ProductionFont]>,
        colors: Option<&ProductionColorPalette>,
        envelope: Option<ScenePrintEnvelope<'_>>,
        font_contract_mode: FontContractMode,
    ) -> Result<SerializedScenePdf, SceneSerializationError> {
        let (document, evidence) = crate::publisher_adapter::lower(
            publication,
            options,
            rasters,
            fonts,
            colors,
            font_contract_mode,
        )?;
        let options = crate::finished::FinishedPdfOptions {
            metadata: envelope
                .as_ref()
                .map_or_else(crate::PdfCandidateMetadata::validation_probe, |envelope| {
                    envelope.metadata.clone()
                }),
            mode: envelope.as_ref().map_or(
                crate::finished::FinishedOutputMode::Ordinary,
                |envelope| crate::finished::FinishedOutputMode::PdfX4 {
                    output_profile: envelope.output_profile,
                },
            ),
        };
        let bytes = if envelope.is_some() && evidence.uses_device_rgb {
            crate::FinishedPdfWriter::write_diagnostic_candidate(&document, &options)
        } else {
            crate::FinishedPdfWriter::write(&document, &options)
        }
        .map_err(crate::publisher_adapter::map_error)?
        .into_bytes();
        Ok(SerializedScenePdf { bytes, evidence })
    }
}

pub(crate) fn publication_transparency_evidence(
    publication: &ResolvedPublication,
    trusted_rasters: &BTreeMap<AssetHash, &ProductionRaster>,
) -> TransparencyEvidence {
    let mut evidence = TransparencyEvidence {
        raster_soft_mask: trusted_rasters
            .values()
            .any(|raster| raster.evidence().has_transparency),
        ..TransparencyEvidence::default()
    };
    for object in publication.pages().iter().flat_map(|page| &page.objects) {
        evidence.include(resolved_object_transparency_evidence(object));
    }
    evidence
}

pub(crate) fn resolved_object_transparency_evidence(
    object: &ResolvedSceneObject,
) -> TransparencyEvidence {
    let base_paint_alpha = match &object.kind {
        ResolvedSceneObjectKind::Text { color, layout } => {
            color.alpha < u8::MAX
                || layout
                    .lines
                    .iter()
                    .flat_map(|line| &line.glyph_runs)
                    .any(|run| run.color.alpha < u8::MAX)
        }
        ResolvedSceneObjectKind::Shape {
            shape,
            fill,
            stroke,
        } => {
            (*shape != ShapeKind::Line && fill.alpha < u8::MAX)
                || (stroke.width_millipoints > 0 && stroke.color.alpha < u8::MAX)
        }
        ResolvedSceneObjectKind::Table { table } => {
            table
                .border_segments
                .iter()
                .any(|edge| edge.color.alpha < u8::MAX)
                || table
                    .cell_fills
                    .iter()
                    .any(|fill| fill.color.alpha < u8::MAX)
                || table
                    .populated_cells
                    .iter()
                    .flat_map(|cell| &cell.layout.lines)
                    .flat_map(|line| &line.glyph_runs)
                    .any(|run| run.color.alpha < u8::MAX)
        }
        ResolvedSceneObjectKind::Image { .. } => false,
    };
    let effect_paint_alpha = object
        .effects
        .shadow
        .is_some_and(|shadow| shadow.color.alpha > 0 && shadow.color.alpha < u8::MAX)
        || object.effects.text_outline.is_some_and(|stroke| {
            stroke.width_millipoints > 0 && stroke.color.alpha > 0 && stroke.color.alpha < u8::MAX
        })
        || object.effects.picture_border.is_some_and(|stroke| {
            stroke.width_millipoints > 0 && stroke.color.alpha > 0 && stroke.color.alpha < u8::MAX
        });
    TransparencyEvidence {
        object_opacity: object.opacity_millionths < 1_000_000,
        paint_alpha: base_paint_alpha || effect_paint_alpha,
        raster_soft_mask: false,
    }
}

pub(crate) fn publication_has_outlines(publication: &ResolvedPublication) -> bool {
    publication.pages().iter().any(|page| {
        page.objects.iter().any(|object| match &object.kind {
            ResolvedSceneObjectKind::Text { layout, .. } => layout.lines.iter().any(|line| {
                line.glyph_runs
                    .iter()
                    .any(|run| run.glyphs.iter().any(|glyph| glyph.outline.is_some()))
            }),
            ResolvedSceneObjectKind::Table { table } => table.populated_cells.iter().any(|cell| {
                cell.layout.lines.iter().any(|line| {
                    line.glyph_runs
                        .iter()
                        .any(|run| run.glyphs.iter().any(|glyph| glyph.outline.is_some()))
                })
            }),
            ResolvedSceneObjectKind::Image { .. } | ResolvedSceneObjectKind::Shape { .. } => false,
        })
    })
}

const fn candidate_font_contract_mode(fonts: Option<&[ProductionFont]>) -> FontContractMode {
    if fonts.is_some() {
        FontContractMode::Strict
    } else {
        FontContractMode::DiagnosticAuthoring
    }
}

pub(crate) fn validate_publication(
    publication: &ResolvedPublication,
    options: ScenePdfOptions,
    rasters: &BTreeMap<document_core::AssetHash, &ProductionRaster>,
    font_contract_mode: FontContractMode,
) -> Result<(), SceneSerializationError> {
    if publication.pages().is_empty() {
        return Err(SceneSerializationError::NoPages);
    }
    if publication.budget_state() != PreviewBudgetState::Complete {
        return Err(SceneSerializationError::PreviewBudgetIncomplete);
    }
    for page in publication.pages() {
        validate_page_boxes(page)?;
        for object in &page.objects {
            validate_object(object, options, rasters, font_contract_mode)?;
        }
    }
    Ok(())
}

fn validate_page_boxes(page: &ResolvedPageScene) -> Result<(), SceneSerializationError> {
    let trim = page.trim_box;
    let bleed = page.bleed_box;
    let trim_right = trim
        .x
        .value()
        .checked_add(trim.width.value())
        .ok_or(SceneSerializationError::InvalidPageBoxes)?;
    let trim_bottom = trim
        .y
        .value()
        .checked_add(trim.height.value())
        .ok_or(SceneSerializationError::InvalidPageBoxes)?;
    let bleed_right = bleed
        .x
        .value()
        .checked_add(bleed.width.value())
        .ok_or(SceneSerializationError::InvalidPageBoxes)?;
    let bleed_bottom = bleed
        .y
        .value()
        .checked_add(bleed.height.value())
        .ok_or(SceneSerializationError::InvalidPageBoxes)?;
    if trim.width.value() <= 0
        || trim.height.value() <= 0
        || bleed.width.value() <= 0
        || bleed.height.value() <= 0
        || bleed.x > trim.x
        || bleed.y > trim.y
        || bleed_right < trim_right
        || bleed_bottom < trim_bottom
    {
        return Err(SceneSerializationError::InvalidPageBoxes);
    }
    pdf_rect(trim, trim.height.value())?;
    pdf_rect(bleed, trim.height.value())?;
    Ok(())
}

fn validate_object(
    object: &ResolvedSceneObject,
    options: ScenePdfOptions,
    rasters: &BTreeMap<document_core::AssetHash, &ProductionRaster>,
    font_contract_mode: FontContractMode,
) -> Result<(), SceneSerializationError> {
    object_matrix(object.transform)?;
    micrometres_to_points(object.size.width.value())?;
    micrometres_to_points(object.size.height.value())?;
    object
        .corner_rounding
        .validate()
        .map_err(|_| SceneSerializationError::GeometryOutOfRange)?;
    if !object.corner_rounding.is_default()
        && !matches!(
            object.kind,
            ResolvedSceneObjectKind::Shape {
                shape: ShapeKind::Rectangle,
                ..
            } | ResolvedSceneObjectKind::Image { .. }
                | ResolvedSceneObjectKind::Text { .. }
                | ResolvedSceneObjectKind::Table { .. }
        )
    {
        return Err(SceneSerializationError::GeometryOutOfRange);
    }
    if let Some(shadow) = object.effects.shadow {
        micrometres_to_points(shadow.offset_x.value())?;
        micrometres_to_points(shadow.offset_y.value())?;
    }
    for stroke in [object.effects.text_outline, object.effects.picture_border]
        .into_iter()
        .flatten()
    {
        millipoint_f64_to_points(f64::from(stroke.width_millipoints))?;
    }
    match &object.kind {
        ResolvedSceneObjectKind::Text { layout, .. } => {
            validate_text_layout(layout, font_contract_mode)?;
        }
        ResolvedSceneObjectKind::Image { asset, .. }
            if options.missing_images == MissingImagePolicy::Reject
                && !rasters.contains_key(asset) =>
        {
            return Err(SceneSerializationError::MissingImageAsset);
        }
        ResolvedSceneObjectKind::Table { table } => {
            validate_table(object, table, font_contract_mode)?;
        }
        ResolvedSceneObjectKind::Image { .. } | ResolvedSceneObjectKind::Shape { .. } => {}
    }
    Ok(())
}

fn validate_text_layout(
    layout: &layout_core::ResolvedTextBoxPreview,
    font_contract_mode: FontContractMode,
) -> Result<(), SceneSerializationError> {
    require_complete_text_preview(layout)?;
    match validate_layout_font_contract(layout) {
        Ok(()) => {}
        Err(
            FontContractFailure::Synthesis { .. } | FontContractFailure::UnresolvedBinding { .. },
        ) if font_contract_mode == FontContractMode::DiagnosticAuthoring => {}
        Err(FontContractFailure::Synthesis { .. }) => {
            return Err(SceneSerializationError::UnsupportedFontSynthesis);
        }
        Err(FontContractFailure::IdentityMismatch { .. }) => {
            return Err(SceneSerializationError::FontBindingIdentityMismatch);
        }
        Err(FontContractFailure::MissingResolvedFont { .. }) => {
            return Err(SceneSerializationError::InvalidFontEvidence);
        }
        Err(
            FontContractFailure::NonCanonicalBindings { .. }
            | FontContractFailure::UnresolvedBinding { .. },
        ) => return Err(SceneSerializationError::FontBindingUnresolved),
    }
    validate_text_glyphs(layout, font_contract_mode)
}

fn validate_text_glyphs(
    layout: &layout_core::ResolvedTextBoxPreview,
    font_contract_mode: FontContractMode,
) -> Result<(), SceneSerializationError> {
    for line in &layout.lines {
        for run in &line.glyph_runs {
            if run.requires_font_synthesis && font_contract_mode == FontContractMode::Strict {
                return Err(SceneSerializationError::UnsupportedFontSynthesis);
            }
            for glyph in &run.glyphs {
                if glyph.fallback.is_some() {
                    return Err(SceneSerializationError::UnsupportedGlyphFallback);
                }
                if glyph.outline.is_none() && !run.text.chars().all(char::is_whitespace) {
                    return Err(SceneSerializationError::MissingGlyphOutline);
                }
            }
        }
    }
    Ok(())
}

fn require_complete_text_preview(
    layout: &layout_core::ResolvedTextBoxPreview,
) -> Result<(), SceneSerializationError> {
    if layout.preview_state != TextBoxPreviewState::Complete {
        return Err(SceneSerializationError::TextPreviewIncomplete);
    }
    Ok(())
}

fn validate_table(
    object: &ResolvedSceneObject,
    table: &layout_core::ResolvedTablePreview,
    font_contract_mode: FontContractMode,
) -> Result<(), SceneSerializationError> {
    if table.column_boundaries_um.len() != usize::from(table.columns) + 1
        || table.row_boundaries_um.len() != usize::from(table.rows) + 1
        || table.column_boundaries_um.first() != Some(&0)
        || table.row_boundaries_um.first() != Some(&0)
        || table.column_boundaries_um.last() != Some(&object.size.width.value())
        || table.row_boundaries_um.last() != Some(&object.size.height.value())
        || table
            .column_boundaries_um
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || table
            .row_boundaries_um
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err(SceneSerializationError::InvalidTableGeometry);
    }
    let mut previous_address = None;
    for cell in &table.populated_cells {
        let address = (cell.row, cell.column);
        if cell.row >= table.rows
            || cell.column >= table.columns
            || cell.row_span == 0
            || cell.column_span == 0
            || usize::from(cell.row) + usize::from(cell.row_span) > usize::from(table.rows)
            || usize::from(cell.column) + usize::from(cell.column_span) > usize::from(table.columns)
            || cell.text.is_empty()
            || previous_address.is_some_and(|previous| previous >= address)
            || cell.layout.object_id != object.object_id
            || (cell.layout.overset && !cell.clipped)
        {
            return Err(SceneSerializationError::InvalidTableGeometry);
        }
        let left = table.column_boundaries_um[usize::from(cell.column)];
        let right = table.column_boundaries_um[usize::from(cell.column + cell.column_span)];
        let top = table.row_boundaries_um[usize::from(cell.row)];
        let bottom = table.row_boundaries_um[usize::from(cell.row + cell.row_span)];
        if right <= left || bottom <= top {
            return Err(SceneSerializationError::InvalidTableGeometry);
        }
        for value in [left, right, top, bottom] {
            micrometres_to_points(value)?;
        }
        validate_text_layout(&cell.layout, font_contract_mode)?;
        previous_address = Some(address);
    }
    for fill in &table.cell_fills {
        if fill.row_span == 0
            || fill.column_span == 0
            || usize::from(fill.row) + usize::from(fill.row_span) > usize::from(table.rows)
            || usize::from(fill.column) + usize::from(fill.column_span) > usize::from(table.columns)
        {
            return Err(SceneSerializationError::InvalidTableGeometry);
        }
    }
    for edge in &table.border_segments {
        if edge.x1_um < 0
            || edge.y1_um < 0
            || edge.x2_um > object.size.width.value()
            || edge.y2_um > object.size.height.value()
            || edge.x1_um > edge.x2_um
            || edge.y1_um > edge.y2_um
            || (edge.x1_um == edge.x2_um && edge.y1_um == edge.y2_um)
            || (edge.x1_um != edge.x2_um && edge.y1_um != edge.y2_um)
            || edge.width_millipoints > document_core::model::ShapeStroke::MAX_WIDTH_MILLIPOINTS
            || !valid_table_border_outline(&edge.outline, object.size)
        {
            return Err(SceneSerializationError::InvalidTableGeometry);
        }
    }
    Ok(())
}

fn valid_table_border_outline(
    path: &[document_core::shape_geometry::ShapePathCommand],
    size: document_core::model::PhysicalSize,
) -> bool {
    use document_core::shape_geometry::ShapePathCommand;
    if path.is_empty() {
        return true;
    }
    if path.len() > 4 || !matches!(path[0], ShapePathCommand::MoveTo(..)) {
        return false;
    }
    let inside =
        |x, y| (0..=size.width.value()).contains(&x) && (0..=size.height.value()).contains(&y);
    path.iter().all(|command| match *command {
        ShapePathCommand::MoveTo(x, y) | ShapePathCommand::LineTo(x, y) => inside(x, y),
        ShapePathCommand::CubicTo(x1, y1, x2, y2, x, y) => {
            inside(x1, y1) && inside(x2, y2) && inside(x, y)
        }
        ShapePathCommand::Close => false,
    })
}

#[cfg(test)]
mod table_font_contract_tests {
    use std::collections::BTreeMap;

    use document_core::ObjectId;
    use document_core::model::{Color, ObjectEffects, PhysicalSize, Transform};
    use layout_core::text::{Millipoints, SourceRange};
    use layout_core::{
        FontSubstitutionStatus, RequestedFontBindingPreview, ResolvedFontFacePreview,
        ResolvedGlyphRunPreview, ResolvedSceneObject, ResolvedSceneObjectKind,
        ResolvedTableCellPreview, ResolvedTablePreview, ResolvedTextBoxPreview,
        ResolvedTextLinePreview, TextBoxPreviewState,
    };

    use super::{FontContractMode, ScenePdfOptions, SceneSerializationError, validate_object};

    fn face(value: char) -> ResolvedFontFacePreview {
        ResolvedFontFacePreview {
            sha256_hex: value.to_string().repeat(64),
            collection_index: 0,
        }
    }

    fn table_with_cell_binding(
        status: FontSubstitutionStatus,
        expected: Option<ResolvedFontFacePreview>,
        actual: ResolvedFontFacePreview,
        synthesis: bool,
    ) -> ResolvedSceneObject {
        let object_id = ObjectId::parse("table-font-contract").unwrap();
        let cell_layout = ResolvedTextBoxPreview {
            object_id: object_id.clone(),
            sizing_mode: document_core::model::TextBoxSizingMode::Fixed,
            composed_size: PhysicalSize::new(8_000, 8_000).unwrap(),
            content_bounds: layout_core::geometry::PhysicalBounds {
                x: document_core::model::Micrometres::new(0),
                y: document_core::model::Micrometres::new(0),
                width: document_core::model::Micrometres::new(8_000),
                height: document_core::model::Micrometres::new(8_000),
            },
            vertical_offset: Millipoints::new(0),
            font_binding: RequestedFontBindingPreview {
                requested_family: "Test family".to_owned(),
                exact_face: expected,
                substitution_status: status,
            },
            font_bindings: Vec::new(),
            permitted_fallback_fonts: Vec::new(),
            resolved_fonts: vec![actual],
            lines: vec![ResolvedTextLinePreview {
                source: SourceRange { start: 0, end: 1 },
                text: "A".to_owned(),
                column_index: 0,
                x: Millipoints::new(0),
                y: Millipoints::new(0),
                width: Millipoints::new(1_000),
                height: Millipoints::new(1_000),
                baseline: Millipoints::new(800),
                glyph_runs: vec![ResolvedGlyphRunPreview {
                    source: SourceRange { start: 0, end: 1 },
                    text: "A".to_owned(),
                    glyph_text: "A".to_owned(),
                    font_index: 0,
                    font_size: Millipoints::new(12_000),
                    color: Color {
                        red: 0,
                        green: 0,
                        blue: 0,
                        alpha: 255,
                    },
                    underline: None,
                    requires_font_synthesis: synthesis,
                    list_marker_paragraph: None,
                    right_to_left: false,
                    glyphs: Vec::new(),
                }],
            }],
            overset: false,
            overset_source: None,
            preview_state: TextBoxPreviewState::Complete,
        };
        let color = Color {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        };
        ResolvedSceneObject {
            object_id,
            master_id: None,
            name: "Table".to_owned(),
            size: PhysicalSize::new(10_000, 10_000).unwrap(),
            transform: Transform::identity_at(0, 0),
            opacity_millionths: 1_000_000,
            effects: ObjectEffects::default(),
            corner_rounding: document_core::model::CornerRounding::default(),
            kind: ResolvedSceneObjectKind::Table {
                table: ResolvedTablePreview {
                    rows: 1,
                    columns: 1,
                    merged_cells: Vec::new(),
                    cell_fills: Vec::new(),
                    border_segments: Vec::new(),
                    column_boundaries_um: vec![0, 10_000],
                    row_boundaries_um: vec![0, 10_000],
                    grid_color: color,
                    grid_width_millipoints: 0,
                    text_color: color,
                    font_binding: RequestedFontBindingPreview {
                        requested_family: "Test family".to_owned(),
                        exact_face: None,
                        substitution_status: FontSubstitutionStatus::Unbound,
                    },
                    populated_cells: vec![ResolvedTableCellPreview {
                        row: 0,
                        column: 0,
                        row_span: 1,
                        column_span: 1,
                        text: "A".to_owned(),
                        layout: cell_layout,
                        clipped: false,
                    }],
                },
            },
        }
    }

    #[test]
    fn table_cell_rejects_an_actual_face_with_another_identity() {
        let object = table_with_cell_binding(
            FontSubstitutionStatus::Exact,
            Some(face('a')),
            face('b'),
            false,
        );

        assert_eq!(
            validate_object(
                &object,
                ScenePdfOptions::default(),
                &BTreeMap::new(),
                FontContractMode::Strict,
            )
            .unwrap_err(),
            SceneSerializationError::FontBindingIdentityMismatch
        );
    }

    #[test]
    fn diagnostic_candidate_tolerates_synthesis_that_strict_export_rejects() {
        let object = table_with_cell_binding(
            FontSubstitutionStatus::Exact,
            Some(face('a')),
            face('a'),
            true,
        );

        assert_eq!(
            validate_object(
                &object,
                ScenePdfOptions::default(),
                &BTreeMap::new(),
                FontContractMode::Strict,
            )
            .unwrap_err(),
            SceneSerializationError::UnsupportedFontSynthesis
        );
        validate_object(
            &object,
            ScenePdfOptions::default(),
            &BTreeMap::new(),
            FontContractMode::DiagnosticAuthoring,
        )
        .expect("diagnostic candidates must preserve authoring evidence for preflight");
    }
}

pub(crate) fn object_matrix(transform: Transform) -> Result<[f32; 6], SceneSerializationError> {
    let angle = (transform.rotation_microdegrees as f64).to_radians() / 1_000_000.0;
    let (sin, cos) = angle.sin_cos();
    let scale_x = transform.scale_x_nanos as f64 / Transform::IDENTITY_SCALE as f64;
    let scale_y = transform.scale_y_nanos as f64 / Transform::IDENTITY_SCALE as f64;
    let shear_x = transform.shear_x_nanos as f64 / Transform::IDENTITY_SCALE as f64;
    let shear_y = transform.shear_y_nanos as f64 / Transform::IDENTITY_SCALE as f64;
    let a = scale_x * (cos - sin * shear_y);
    let b = scale_x * (sin + cos * shear_y);
    let c = scale_y * (cos * shear_x - sin);
    let d = scale_y * (sin * shear_x + cos);
    let pivot_x = transform.pivot_x.value() as f64 * POINTS_PER_MICROMETRE;
    let pivot_y = transform.pivot_y.value() as f64 * POINTS_PER_MICROMETRE;
    let tx = transform.translation_x.value() as f64 * POINTS_PER_MICROMETRE + pivot_x
        - a * pivot_x
        - c * pivot_y;
    let ty = transform.translation_y.value() as f64 * POINTS_PER_MICROMETRE + pivot_y
        - b * pivot_x
        - d * pivot_y;
    checked_matrix([a, b, c, d, tx, ty])
}

fn checked_matrix(values: [f64; 6]) -> Result<[f32; 6], SceneSerializationError> {
    let mut converted = [0.0_f32; 6];
    for (target, value) in converted.iter_mut().zip(values) {
        if !value.is_finite() || value.abs() > MAX_ABSOLUTE_PDF_POINTS {
            return Err(SceneSerializationError::GeometryOutOfRange);
        }
        *target = value as f32;
    }
    Ok(converted)
}

pub(crate) fn pdf_rect(
    bounds: PhysicalBounds,
    trim_height_um: i64,
) -> Result<Rect, SceneSerializationError> {
    let right = bounds
        .x
        .value()
        .checked_add(bounds.width.value())
        .ok_or(SceneSerializationError::GeometryOutOfRange)?;
    let bottom = bounds
        .y
        .value()
        .checked_add(bounds.height.value())
        .ok_or(SceneSerializationError::GeometryOutOfRange)?;
    let pdf_bottom = trim_height_um
        .checked_sub(bottom)
        .ok_or(SceneSerializationError::GeometryOutOfRange)?;
    let pdf_top = trim_height_um
        .checked_sub(bounds.y.value())
        .ok_or(SceneSerializationError::GeometryOutOfRange)?;
    Ok(Rect::new(
        micrometres_to_points(bounds.x.value())?,
        micrometres_to_points(pdf_bottom)?,
        micrometres_to_points(right)?,
        micrometres_to_points(pdf_top)?,
    ))
}

pub(crate) fn micrometres_to_points(value: i64) -> Result<f32, SceneSerializationError> {
    checked_point(value as f64 * POINTS_PER_MICROMETRE)
}

pub(crate) fn millipoints_to_points(
    base: i64,
    offset: i64,
) -> Result<f32, SceneSerializationError> {
    let value = base
        .checked_add(offset)
        .ok_or(SceneSerializationError::GeometryOutOfRange)?;
    millipoint_f64_to_points(value as f64)
}

pub(crate) fn millipoint_f64_to_points(value: f64) -> Result<f32, SceneSerializationError> {
    checked_point(value / 1_000.0)
}

fn checked_point(value: f64) -> Result<f32, SceneSerializationError> {
    if !value.is_finite() || value.abs() > MAX_ABSOLUTE_PDF_POINTS {
        return Err(SceneSerializationError::GeometryOutOfRange);
    }
    Ok(value as f32)
}

pub(crate) fn trusted_raster_map<'a>(
    publication: &ResolvedPublication,
    rasters: &'a [ProductionRaster],
) -> Result<BTreeMap<document_core::AssetHash, &'a ProductionRaster>, SceneSerializationError> {
    let referenced = publication
        .pages()
        .iter()
        .flat_map(|page| &page.objects)
        .filter_map(|object| match &object.kind {
            ResolvedSceneObjectKind::Image { asset, .. } => Some(asset.clone()),
            ResolvedSceneObjectKind::Text { .. }
            | ResolvedSceneObjectKind::Shape { .. }
            | ResolvedSceneObjectKind::Table { .. } => None,
        })
        .collect::<BTreeSet<_>>();
    let mut result = BTreeMap::new();
    for raster in rasters {
        let asset = raster.evidence().asset.clone();
        if !referenced.contains(&asset) || result.insert(asset, raster).is_some() {
            return Err(SceneSerializationError::InvalidRasterEvidence);
        }
        let expected_cmyk = usize::try_from(
            u64::from(raster.evidence().width)
                .saturating_mul(u64::from(raster.evidence().height))
                .saturating_mul(4),
        )
        .map_err(|_| SceneSerializationError::InvalidRasterEvidence)?;
        if raster.decoded_cmyk_bytes() != expected_cmyk
            || raster.cmyk().is_empty()
            || raster.alpha().is_some_and(<[u8]>::is_empty)
        {
            return Err(SceneSerializationError::InvalidRasterEvidence);
        }
    }
    Ok(result)
}

pub(crate) fn trusted_font_map<'a>(
    publication: &ResolvedPublication,
    fonts: &'a [ProductionFont],
) -> Result<BTreeMap<(String, u32), &'a ProductionFont>, SceneSerializationError> {
    let referenced = publication_font_keys(publication);
    let scene_digest = publication.semantic_digest();
    let mut result = BTreeMap::new();
    for font in fonts {
        let evidence = font.evidence();
        let key = (hex_sha256(evidence.source_sha256), evidence.face_index);
        let program = font.program();
        if evidence.scene_digest != scene_digest
            || !referenced.contains(&key)
            || evidence.source_sha256 != program.source_sha256()
            || evidence.face_index != program.face_index()
            || evidence.postscript_name_sha256 != program.postscript_name_sha256()
            || evidence.preparation_kind != program.preparation_kind()
            || evidence.glyph_set_sha256 != program.glyph_set_sha256()
            || evidence.used_glyph_count != program.used_glyph_count()
            || evidence.embedded_glyph_count != program.embedded_glyph_count()
            || evidence.prepared_font_sha256 != program.prepared_sha256()
            || evidence.rights_identity_sha256 != program.rights_identity_sha256()
            || evidence.worker_executable_sha256 != program.worker_executable_sha256()
            || evidence.engine_identity_sha256 != program.engine_identity_sha256()
            || evidence.rules_identity_sha256 != program.rules_identity_sha256()
            || font
                .to_unicode()
                .keys()
                .any(|cid| *cid >= evidence.embedded_glyph_count)
            || result.insert(key, font).is_some()
        {
            return Err(SceneSerializationError::InvalidFontEvidence);
        }
    }
    if result.keys().ne(referenced.iter()) {
        return Err(SceneSerializationError::MissingPreparedFont);
    }
    Ok(result)
}

fn publication_font_keys(publication: &ResolvedPublication) -> BTreeSet<(String, u32)> {
    publication
        .pages()
        .iter()
        .flat_map(page_font_keys)
        .collect()
}

fn page_font_keys(page: &ResolvedPageScene) -> BTreeSet<(String, u32)> {
    let mut result = BTreeSet::new();
    for object in &page.objects {
        match &object.kind {
            ResolvedSceneObjectKind::Text { layout, .. } => {
                insert_layout_font_keys(layout, &mut result);
            }
            ResolvedSceneObjectKind::Table { table } => {
                for cell in &table.populated_cells {
                    insert_layout_font_keys(&cell.layout, &mut result);
                }
            }
            ResolvedSceneObjectKind::Image { .. } | ResolvedSceneObjectKind::Shape { .. } => {}
        }
    }
    result
}

fn insert_layout_font_keys(
    layout: &layout_core::ResolvedTextBoxPreview,
    result: &mut BTreeSet<(String, u32)>,
) {
    for run in layout.lines.iter().flat_map(|line| &line.glyph_runs) {
        if let Some(face) = layout.resolved_fonts.get(run.font_index) {
            result.insert((face.sha256_hex.clone(), face.collection_index));
        }
    }
}

pub(crate) fn hex_sha256(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}
