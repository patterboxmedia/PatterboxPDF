//! Canonical document-to-scene composition for preview and print export.
//!
//! This deep module binds page geometry, object stacking, and resolved text to
//! one document revision. Presentation and print adapters consume the same
//! immutable scene without repeating line breaking or reconstructing document
//! intent.

use document_core::model::{
    Color, CornerRounding, Document, DocumentColorSettings, EdgeExpectations, EffectiveFontVariantRange,
    ImageContentKind, ImageCrop, ImageFlip, ObjectEffects, ObjectKind, Page, ParagraphListStyle,
    PhysicalSize, PublicationObject, ShapeKind, ShapeStroke, TableData, TableVerticalAlignment,
    TextAlignment, TextBoxSizingMode, TextFrame, TextSourceRange, TextStoryFormatting, TextStyle,
    TextVerticalAlignment, Transform, UntaggedRasterPolicy, WrapMode, WrapSide,
    effective_font_variant_ranges, paragraph_count,
};
use document_core::{AssetHash, ObjectId, PageId, ProjectSnapshot, Revision};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::geometry::PhysicalBounds;
use crate::text::{
    ExactFontResource, HorizontalAlignment, Millipoints, ResolvedFitTextLayout, ResolvedFont,
    ResolvedGlyphFallback, ResolvedGlyphOutline, ResolvedGlyphRun, ResolvedTextLayout,
    ResolvedTextLine, SourceRange, StoryListMarker, StoryParagraphStyle, StorySpan, StoryStyle,
    TextLayoutEngine, TextLayoutError, TextRect, TextRegion, TextStory,
};

const MAX_PREVIEW_TEXT_BOXES: usize = 256;
const MAX_PREVIEW_TEXT_BYTES_PER_BOX: usize = 32 * 1024;
const MAX_PREVIEW_TEXT_BYTES_TOTAL: usize = 64 * 1024;

mod table_authoring;
mod rounded_text;
pub use table_authoring::{ResolvedTableBorderSegment, ResolvedTableCellFill};

/// Whether the revision-bound preview represents every text box.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewBudgetState {
    Complete,
    Truncated,
}

/// Per-object preview availability. Budget-limited boxes remain explicit so
/// presentation layers never confuse omitted work with empty text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextBoxPreviewState {
    Complete,
    MissingFont,
    BudgetExceeded,
}

/// Exact font face selected by the native shaping engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFontFacePreview {
    pub sha256_hex: String,
    pub collection_index: u32,
}

/// Honest status for an author-selected family until the document model stores
/// an exact face binding that can be revalidated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FontSubstitutionStatus {
    Exact,
    Substituted,
    Unbound,
    Missing,
}

/// Future-compatible authoring binding: the requested family is retained now,
/// while `exact_face` remains empty until document-core owns that binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestedFontBindingPreview {
    pub requested_family: String,
    pub exact_face: Option<ResolvedFontFacePreview>,
    pub substitution_status: FontSubstitutionStatus,
}

/// One canonical source range and the exact face requested by its effective
/// inline weight/slant. These facts let presentation and print consumers
/// distinguish an unavailable bold or italic face from the Text Box's base
/// face without reconstructing rich-text formatting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestedFontRangeBindingPreview {
    pub source: SourceRange,
    pub requested_family: String,
    pub requested_weight: u16,
    pub requested_italic: bool,
    pub exact_face: Option<ResolvedFontFacePreview>,
    pub substitution_status: FontSubstitutionStatus,
}

/// One canonical glyph placement and optional vector outline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedGlyphPreview {
    pub id: u32,
    pub x: Millipoints,
    pub y: Millipoints,
    pub advance: Millipoints,
    pub outline: Option<Arc<ResolvedGlyphOutline>>,
    pub fallback: Option<ResolvedGlyphFallback>,
}

/// One source-addressable, resolved glyph run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedGlyphRunPreview {
    pub source: SourceRange,
    pub text: String,
    /// Full source text painted by this shaping cluster's glyphs.
    pub glyph_text: String,
    pub font_index: usize,
    pub font_size: Millipoints,
    pub color: Color,
    pub underline: Option<ResolvedTextUnderlinePreview>,
    pub requires_font_synthesis: bool,
    /// Present only for a generated bullet or number. Its byte range is local
    /// to `text`, never to the durable Text Box story.
    pub list_marker_paragraph: Option<u32>,
    pub right_to_left: bool,
    pub glyphs: Vec<ResolvedGlyphPreview>,
}

/// Shared physical underline geometry for preview and output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTextUnderlinePreview {
    pub x: Millipoints,
    pub y: Millipoints,
    pub width: Millipoints,
    pub thickness: Millipoints,
}

/// One paintable line fragment resolved from a canonical text box.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTextLinePreview {
    pub source: SourceRange,
    pub text: String,
    pub column_index: u16,
    pub x: Millipoints,
    pub y: Millipoints,
    pub width: Millipoints,
    pub height: Millipoints,
    pub baseline: Millipoints,
    pub glyph_runs: Vec<ResolvedGlyphRunPreview>,
}

/// All preview lines belonging to one canonical text box.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTextBoxPreview {
    pub object_id: ObjectId,
    pub sizing_mode: TextBoxSizingMode,
    pub composed_size: PhysicalSize,
    /// Physical interior relative to the Text Box origin, before alignment.
    pub content_bounds: PhysicalBounds,
    /// Accepted alignment offset inside that interior. Overset starts at its top.
    pub vertical_offset: Millipoints,
    pub font_binding: RequestedFontBindingPreview,
    /// Normalized, source-ordered effective bindings for authored text.
    pub font_bindings: Vec<RequestedFontRangeBindingPreview>,
    /// Document-authorized exact faces that may satisfy a missing glyph without
    /// becoming an uncontrolled machine-local substitution.
    pub permitted_fallback_fonts: Vec<ResolvedFontFacePreview>,
    pub resolved_fonts: Vec<ResolvedFontFacePreview>,
    pub lines: Vec<ResolvedTextLinePreview>,
    pub overset: bool,
    /// Exact unplaced UTF-8 source tail. This is retained independently of
    /// presentation so repair guidance never has to infer a character offset
    /// from rendered line text.
    pub overset_source: Option<SourceRange>,
    pub preview_state: TextBoxPreviewState,
}

/// Successful current-revision Fit-text composition and its shrink-wrapped bounds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFitTextBox {
    pub size: PhysicalSize,
    pub layout: ResolvedTextBoxPreview,
}

/// One populated table cell resolved by the same native shaping engine as a
/// text box. Coordinates in `layout` are local to the table object while the
/// row and column address remain explicit for clipping and repair guidance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTableCellPreview {
    pub row: u16,
    pub column: u16,
    pub row_span: u16,
    pub column_span: u16,
    pub text: String,
    pub layout: ResolvedTextBoxPreview,
    /// True when source text did not fit inside the padded cell region. A
    /// renderer must still clip every cell to its unpadded grid rectangle.
    pub clipped: bool,
}

/// Canonical table grid and populated-cell text in row-major order.
///
/// Boundaries are integer micrometres relative to the table object. Keeping
/// them in the scene prevents preview and print adapters from independently
/// rounding fractional row or column sizes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTablePreview {
    pub rows: u16,
    pub columns: u16,
    pub column_boundaries_um: Vec<i64>,
    pub row_boundaries_um: Vec<i64>,
    pub grid_color: Color,
    pub grid_width_millipoints: u32,
    pub text_color: Color,
    pub font_binding: RequestedFontBindingPreview,
    pub merged_cells: Vec<document_core::model::TableCellRange>,
    pub cell_fills: Vec<ResolvedTableCellFill>,
    pub border_segments: Vec<ResolvedTableBorderSegment>,
    pub populated_cells: Vec<ResolvedTableCellPreview>,
}

/// Renderer-neutral content for one object in an immutable resolved scene.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedSceneObjectKind {
    /// Text paint facts and the single authoritative resolved glyph layout.
    Text {
        color: Color,
        layout: ResolvedTextBoxPreview,
    },
    /// Content-addressed raster identity. Decode and color policy remain print concerns.
    Image {
        asset: AssetHash,
        content_kind: ImageContentKind,
        crop: ImageCrop,
        flip: ImageFlip,
    },
    /// Canonical vector primitive paint facts.
    Shape {
        shape: ShapeKind,
        fill: Color,
        stroke: ShapeStroke,
    },
    /// Native table grid and cell text layout in row-major order.
    Table { table: ResolvedTablePreview },
}

impl ResolvedSceneObjectKind {
    fn text_layout(&self) -> Option<&ResolvedTextBoxPreview> {
        match self {
            Self::Text { layout, .. } => Some(layout),
            Self::Image { .. } | Self::Shape { .. } | Self::Table { .. } => None,
        }
    }
}

/// One object in canonical stacking order, expressed only in physical units.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSceneObject {
    pub object_id: ObjectId,
    pub master_id: Option<document_core::model::MasterId>,
    pub name: String,
    pub size: PhysicalSize,
    pub transform: Transform,
    pub opacity_millionths: u32,
    pub effects: ObjectEffects,
    pub corner_rounding: CornerRounding,
    pub kind: ResolvedSceneObjectKind,
}

impl ResolvedSceneObject {
    /// Conservative page-space bounds for each independently painted region.
    ///
    /// Invisible fills and text without drawable glyph outlines contribute no
    /// region. Centered strokes expand only their own base geometry, while a
    /// hard shadow remains a separately shifted region. Keeping those regions
    /// separate prevents a gap between base artwork and its shadow from being
    /// mistaken for continuous bleed coverage.
    ///
    /// # Errors
    ///
    /// Returns a transform bounds error when an effect expansion or transformed
    /// page-space bound cannot be represented in canonical physical units.
    pub fn paint_regions(
        &self,
    ) -> Result<Vec<PhysicalBounds>, crate::geometry::TransformBoundsError> {
        if self.opacity_millionths == 0 {
            return Ok(Vec::new());
        }
        let has_text_glyphs = match &self.kind {
            ResolvedSceneObjectKind::Text { layout, .. } => text_has_drawable_glyphs(layout),
            ResolvedSceneObjectKind::Image { .. }
            | ResolvedSceneObjectKind::Shape { .. }
            | ResolvedSceneObjectKind::Table { .. } => false,
        };
        let mut local_regions = Vec::with_capacity(3);
        self.append_base_paint_regions(has_text_glyphs, &mut local_regions)?;
        self.append_effect_paint_regions(has_text_glyphs, &mut local_regions)?;

        let mut regions = Vec::with_capacity(local_regions.len());
        for bounds in local_regions {
            let transformed = crate::geometry::transformed_local_bounds(bounds, self.transform)?;
            if !regions.contains(&transformed) {
                regions.push(transformed);
            }
        }
        Ok(regions)
    }

    fn append_base_paint_regions(
        &self,
        has_text_glyphs: bool,
        local_regions: &mut Vec<PhysicalBounds>,
    ) -> Result<(), crate::geometry::TransformBoundsError> {
        let width = self.size.width.value();
        let height = self.size.height.value();
        match &self.kind {
            ResolvedSceneObjectKind::Text { layout, .. } => {
                if layout
                    .lines
                    .iter()
                    .flat_map(|line| &line.glyph_runs)
                    .any(|run| run.color.alpha > 0)
                    && has_text_glyphs
                {
                    local_regions.push(local_paint_bounds(width, height, 0, 0, 0)?);
                }
            }
            ResolvedSceneObjectKind::Image { .. } => {
                local_regions.push(local_paint_bounds(width, height, 0, 0, 0)?);
            }
            ResolvedSceneObjectKind::Shape { fill, stroke, .. } => {
                if fill.alpha > 0 {
                    local_regions.push(local_paint_bounds(width, height, 0, 0, 0)?);
                }
                if visible_stroke(*stroke) {
                    local_regions.push(local_paint_bounds(
                        width,
                        height,
                        0,
                        0,
                        stroke_half_width_micrometres(stroke.width_millipoints)?,
                    )?);
                }
            }
            ResolvedSceneObjectKind::Table { table } => {
                if let Some(max_width) = table.border_segments.iter().map(|edge| edge.width_millipoints).max() {
                    local_regions.push(local_paint_bounds(
                        width,
                        height,
                        0,
                        0,
                        stroke_half_width_micrometres(max_width)?,
                    )?);
                }
                if !table.cell_fills.is_empty() || table
                        .populated_cells
                        .iter()
                        .any(|cell| text_has_drawable_glyphs(&cell.layout))
                {
                    local_regions.push(local_paint_bounds(width, height, 0, 0, 0)?);
                }
            }
        }
        Ok(())
    }

    fn append_effect_paint_regions(
        &self,
        has_text_glyphs: bool,
        local_regions: &mut Vec<PhysicalBounds>,
    ) -> Result<(), crate::geometry::TransformBoundsError> {
        let width = self.size.width.value();
        let height = self.size.height.value();
        if let Some(outline) = self.effects.text_outline.filter(|stroke| {
            visible_stroke(*stroke) && matches!(&self.kind, ResolvedSceneObjectKind::Text { .. })
        }) && has_text_glyphs
        {
            local_regions.push(local_paint_bounds(
                width,
                height,
                0,
                0,
                stroke_half_width_micrometres(outline.width_millipoints)?,
            )?);
        }
        if let Some(border) = self.effects.picture_border.filter(|stroke| {
            visible_stroke(*stroke) && matches!(&self.kind, ResolvedSceneObjectKind::Image { .. })
        }) {
            local_regions.push(local_paint_bounds(
                width,
                height,
                0,
                0,
                stroke_half_width_micrometres(border.width_millipoints)?,
            )?);
        }
        let shadow_has_silhouette = match &self.kind {
            ResolvedSceneObjectKind::Text { .. } => has_text_glyphs,
            ResolvedSceneObjectKind::Image { .. }
            | ResolvedSceneObjectKind::Shape { .. }
            | ResolvedSceneObjectKind::Table { .. } => true,
        };
        if shadow_has_silhouette
            && let Some(shadow) = self.effects.shadow.filter(|shadow| shadow.color.alpha > 0)
        {
            local_regions.push(local_paint_bounds(
                width,
                height,
                shadow.offset_x.value(),
                shadow.offset_y.value(),
                0,
            )?);
        }
        Ok(())
    }
}

fn text_has_drawable_glyphs(layout: &ResolvedTextBoxPreview) -> bool {
    layout.lines.iter().any(|line| {
        line.glyph_runs.iter().any(|run| {
            run.underline.is_some() || run.glyphs.iter().any(|glyph| glyph.outline.is_some())
        })
    })
}

const fn visible_stroke(stroke: ShapeStroke) -> bool {
    stroke.width_millipoints > 0 && stroke.color.alpha > 0
}

fn local_paint_bounds(
    width: i64,
    height: i64,
    offset_x: i64,
    offset_y: i64,
    expansion: i64,
) -> Result<PhysicalBounds, crate::geometry::TransformBoundsError> {
    let left = offset_x
        .checked_sub(expansion)
        .ok_or(crate::geometry::TransformBoundsError::OutOfRange)?;
    let top = offset_y
        .checked_sub(expansion)
        .ok_or(crate::geometry::TransformBoundsError::OutOfRange)?;
    let right = offset_x
        .checked_add(width)
        .and_then(|value| value.checked_add(expansion))
        .ok_or(crate::geometry::TransformBoundsError::OutOfRange)?;
    let bottom = offset_y
        .checked_add(height)
        .and_then(|value| value.checked_add(expansion))
        .ok_or(crate::geometry::TransformBoundsError::OutOfRange)?;
    Ok(PhysicalBounds {
        x: document_core::model::Micrometres::new(left),
        y: document_core::model::Micrometres::new(top),
        width: document_core::model::Micrometres::new(
            right
                .checked_sub(left)
                .ok_or(crate::geometry::TransformBoundsError::OutOfRange)?,
        ),
        height: document_core::model::Micrometres::new(
            bottom
                .checked_sub(top)
                .ok_or(crate::geometry::TransformBoundsError::OutOfRange)?,
        ),
    })
}

fn stroke_half_width_micrometres(
    width_millipoints: u32,
) -> Result<i64, crate::geometry::TransformBoundsError> {
    // 1 millipoint = 127/360 micrometres. A centered stroke extends half
    // its width; round outward so preflight never understates painted bounds.
    let numerator = u64::from(width_millipoints)
        .checked_mul(127)
        .ok_or(crate::geometry::TransformBoundsError::OutOfRange)?;
    i64::try_from(numerator.div_ceil(720))
        .map_err(|_| crate::geometry::TransformBoundsError::OutOfRange)
}

/// One export page, including an intentional blank when `objects` is empty.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPageScene {
    pub page_id: PageId,
    pub trim_box: PhysicalBounds,
    pub bleed_box: PhysicalBounds,
    pub bleed_expectations: EdgeExpectations,
    pub objects: Vec<ResolvedSceneObject>,
}

/// Non-exported workspace objects retained for native preview and later authoring uses.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedPasteboardScene {
    pub objects: Vec<ResolvedSceneObject>,
}

/// Interactive composition of one original page and, optionally, the pasteboard.
///
/// This is deliberately a different type from [`ResolvedPublication`]: a complete
/// page preview says nothing about the completeness of the other publication pages
/// and cannot be supplied to a publication export API. Identity always describes
/// the full input snapshot, including objects outside the requested scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPagePreview {
    revision: Revision,
    source_project_digest: [u8; 32],
    page: ResolvedPageScene,
    pasteboard: Option<ResolvedPasteboardScene>,
    budget_state: PreviewBudgetState,
}

impl ResolvedPagePreview {
    /// Resolves an explicit editable Master workspace, without inherited copies.
    ///
    /// # Errors
    /// Refuses unavailable masters or invalid source geometry/text.
    pub fn resolve_master_with_font_resources(
        project: &ProjectSnapshot,
        master_id: &document_core::model::MasterId,
        resources: &[ExactFontResource],
    ) -> Result<Self, PublicationLayoutError> {
        let page = project.document.master_preview_page(master_id)
            .map_err(|_| PublicationLayoutError::InvalidOutputSource)?;
        let mut engine = TextLayoutEngine::new();
        let font_resources = resources.iter().map(|resource| ((resource.source.clone(),resource.face_index),resource)).collect::<BTreeMap<_,_>>();
        configure_exact_font_fallbacks(&mut engine, &project.document, &font_resources)?;
        let mut budget = PreviewBudget::default();
        let mut page = resolve_page_scene(&mut engine, &page, &font_resources, &mut budget)?;
        for object in &mut page.objects { object.master_id = Some(master_id.clone()); }
        Ok(Self {
            revision: project.revision,
            source_project_digest: project.semantic_digest().map_err(|_| PublicationLayoutError::ProjectIdentityUnavailable)?,
            page, pasteboard: None,
            budget_state: if budget.truncated {PreviewBudgetState::Truncated} else {PreviewBudgetState::Complete},
        })
    }
    /// Resolves the requested page with the existing per-request preview ceilings.
    /// Page objects consume the budget before optional pasteboard objects. Geometry,
    /// wrapping and font selection use the same authority as publication resolution.
    ///
    /// # Errors
    /// Returns an error for an unknown page, unavailable global fallback resources,
    /// or a canonical geometry, identity or text composition failure.
    pub fn resolve_with_font_resources(
        project: &ProjectSnapshot,
        page_id: &PageId,
        include_pasteboard: bool,
        resources: &[ExactFontResource],
    ) -> Result<Self, PublicationLayoutError> {
        let page = requested_page(project, page_id)?;
        let mut engine = TextLayoutEngine::new();
        let font_resources = resources
            .iter()
            .map(|resource| ((resource.source.clone(), resource.face_index), resource))
            .collect::<BTreeMap<_, _>>();
        configure_exact_font_fallbacks(&mut engine, &project.document, &font_resources)?;
        let mut budget = PreviewBudget::default();
        let page = resolve_document_page_scene(&mut engine, &project.document, page, &font_resources, &mut budget)?;
        let pasteboard = if include_pasteboard {
            Some(ResolvedPasteboardScene {
                objects: resolve_object_group(
                    &mut engine,
                    &project.document.preview_pasteboard_objects(),
                    &[],
                    &font_resources,
                    &mut budget,
                )?,
            })
        } else {
            None
        };
        Ok(Self {
            revision: project.revision,
            source_project_digest: project
                .semantic_digest()
                .map_err(|_| PublicationLayoutError::ProjectIdentityUnavailable)?,
            page,
            pasteboard,
            budget_state: if budget.truncated {
                PreviewBudgetState::Truncated
            } else {
                PreviewBudgetState::Complete
            },
        })
    }

    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Digest of the entire supplied project, not a page-filtered clone.
    #[must_use]
    pub const fn source_project_digest(&self) -> [u8; 32] {
        self.source_project_digest
    }

    #[must_use]
    pub const fn page(&self) -> &ResolvedPageScene {
        &self.page
    }

    /// `None` means the pasteboard was not requested, rather than being empty.
    #[must_use]
    pub const fn pasteboard(&self) -> Option<&ResolvedPasteboardScene> {
        self.pasteboard.as_ref()
    }

    /// Completeness within this page and its requested pasteboard only.
    #[must_use]
    pub const fn budget_state(&self) -> PreviewBudgetState {
        self.budget_state
    }
}

fn requested_page<'a>(
    project: &'a ProjectSnapshot,
    page_id: &PageId,
) -> Result<&'a Page, PublicationLayoutError> {
    project
        .document
        .pages
        .iter()
        .find(|page| &page.id == page_id)
        .ok_or(PublicationLayoutError::PageNotFound)
}

/// Revision-bound immutable publication scene resolved entirely by layout-core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPublication {
    revision: Revision,
    source_project_digest: [u8; 32],
    color_settings: DocumentColorSettings,
    pages: Vec<ResolvedPageScene>,
    pasteboard: ResolvedPasteboardScene,
    budget_state: PreviewBudgetState,
}

impl ResolvedPublication {
    /// Creates an empty resolved-publication marker for the supplied revision.
    #[must_use]
    pub const fn for_revision(revision: Revision) -> Self {
        Self {
            revision,
            source_project_digest: [0; 32],
            color_settings: DocumentColorSettings {
                untagged_raster_policy: UntaggedRasterPolicy::RequireEmbeddedProfile,
            },
            pages: Vec::new(),
            pasteboard: ResolvedPasteboardScene {
                objects: Vec::new(),
            },
            budget_state: PreviewBudgetState::Complete,
        }
    }

    /// Resolves every page and pasteboard text box against rectangular wrap objects.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationLayoutError`] when physical geometry cannot be converted or
    /// the text engine rejects a canonical text box.
    pub fn resolve(project: &ProjectSnapshot) -> Result<Self, PublicationLayoutError> {
        Self::resolve_with_font_resources(project, &[])
    }

    /// Resolves preview using validated application-owned exact font resources.
    ///
    /// A bound text object never falls back to a same-family installed font:
    /// absence or registration failure is surfaced as a missing exact face.
    /// Unbound legacy text keeps the normal system-family route.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationLayoutError`] when physical geometry cannot be converted or
    /// the text engine rejects a canonical text box.
    pub fn resolve_with_font_resources(
        project: &ProjectSnapshot,
        resources: &[ExactFontResource],
    ) -> Result<Self, PublicationLayoutError> {
        let mut engine = TextLayoutEngine::new();
        let font_resources = resources
            .iter()
            .map(|resource| ((resource.source.clone(), resource.face_index), resource))
            .collect::<BTreeMap<_, _>>();
        configure_exact_font_fallbacks(&mut engine, &project.document, &font_resources)?;
        let mut budget = PreviewBudget::default();
        let mut pages = Vec::with_capacity(project.document.pages.len());
        for page in &project.document.pages {
            pages.push(resolve_document_page_scene(
                &mut engine,
                &project.document,
                page,
                &font_resources,
                &mut budget,
            )?);
        }
        let pasteboard = ResolvedPasteboardScene {
            objects: resolve_object_group(
                &mut engine,
                &project.document.preview_pasteboard_objects(),
                &[],
                &font_resources,
                &mut budget,
            )?,
        };
        Ok(Self {
            revision: project.revision,
            source_project_digest: project
                .semantic_digest()
                .map_err(|_| PublicationLayoutError::ProjectIdentityUnavailable)?,
            color_settings: project.document.color_settings,
            pages,
            pasteboard,
            budget_state: if budget.truncated {
                PreviewBudgetState::Truncated
            } else {
                PreviewBudgetState::Complete
            },
        })
    }

    /// Composes every published page using a fresh bounded budget per page.
    ///
    /// Complete composition uses the package's bounded object and source-text
    /// ceilings, with 32 KiB per source cell/Text Box. Tables consume one object
    /// item and account for every cell byte. An over-budget page fails the whole
    /// operation; no partial publication is returned. Missing fonts remain
    /// explicit `MissingFont` facts, so completion is not print readiness.
    /// Pasteboard objects are excluded from painting and page budgets, but remain
    /// part of source validation and the full revision-bound project identity.
    /// Resources must already have passed the application's asset validation.
    ///
    /// # Errors
    /// Returns an invalid-source, page-budget, geometry, or shaping error.
    pub fn resolve_output_with_font_resources(
        project: &ProjectSnapshot,
        resources: &[ExactFontResource],
    ) -> Result<Self, PublicationLayoutError> {
        // Preflight every page with the same text traversal before shaping any
        // of them. Discovery validates the source and refuses partial budgets.
        output_font_face_bindings(project)?;
        let source_project_digest = project
            .semantic_digest()
            .map_err(|_| PublicationLayoutError::ProjectIdentityUnavailable)?;
        // Output must not substitute machine-installed faces ahead of the
        // document's explicit fallback chains (notably Windows emoji fonts).
        let mut engine = TextLayoutEngine::new_without_system_fonts();
        let font_resources = resources
            .iter()
            .map(|resource| ((resource.source.clone(), resource.face_index), resource))
            .collect::<BTreeMap<_, _>>();
        configure_exact_font_fallbacks(&mut engine, &project.document, &font_resources)?;
        let mut pages = Vec::with_capacity(project.document.pages.len());
        for page in &project.document.pages {
            let mut budget = PreviewBudget::complete();
            let scene = resolve_document_page_scene(&mut engine, &project.document, page, &font_resources, &mut budget)?;
            if budget.truncated {
                return Err(PublicationLayoutError::OutputPageBudgetExceeded(
                    page.id.clone(),
                ));
            }
            pages.push(scene);
        }
        Ok(Self {
            revision: project.revision,
            source_project_digest,
            color_settings: project.document.color_settings,
            pages,
            pasteboard: ResolvedPasteboardScene {
                objects: Vec::new(),
            },
            budget_state: PreviewBudgetState::Complete,
        })
    }

    /// Resolves one Text Box as headline-like Fit text using the same shaping
    /// authority as page preview and print. Page wrap objects and the stored
    /// frame height are deliberately excluded from this composition.
    ///
    /// # Errors
    ///
    /// Returns an error when the object is absent, is not text, its exact font
    /// resource is unavailable, or canonical composition cannot complete.
    pub fn resolve_fit_text_box(
        project: &ProjectSnapshot,
        resources: &[ExactFontResource],
        object_id: &ObjectId,
    ) -> Result<ResolvedFitTextBox, PublicationLayoutError> {
        let object = project
            .document
            .authored_objects()
            .find(|object| &object.id == object_id)
            .ok_or(PublicationLayoutError::ObjectNotFound)?;
        let mut field_object;
        let object = if object.text_field.is_some() {
            field_object = object.clone();
            let page_id = project.document.pages.iter().find(|page| page.objects.iter().any(|candidate| &candidate.id == object_id))
                .map(|page| &page.id);
            let display = if let Some(page_id) = page_id {
                project.document.page_numbers().map_err(|_| PublicationLayoutError::InvalidOutputSource)?
                    .into_iter().find(|number| &number.page_id == page_id).map_or_else(|| "1".into(), |number| number.display)
            } else { "1".into() };
            if let ObjectKind::Text {text,..} = &mut field_object.kind { *text = display; }
            field_object.text_field = None;
            &field_object
        } else { object };
        let ObjectKind::Text {
            text, style, story, ..
        } = &object.kind
        else {
            return Err(PublicationLayoutError::ObjectKindMismatch);
        };
        let font_resources = resources
            .iter()
            .map(|resource| ((resource.source.clone(), resource.face_index), resource))
            .collect::<BTreeMap<_, _>>();
        let mut engine = TextLayoutEngine::new();
        configure_exact_font_fallbacks(&mut engine, &project.document, &font_resources)?;
        compose_fit_text_box(&mut engine, &font_resources, object, text, style, story)
    }

    /// Returns the canonical revision represented by this layout result.
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Digest of the exact canonical project supplied to scene resolution.
    #[must_use]
    pub const fn source_project_digest(&self) -> [u8; 32] {
        self.source_project_digest
    }

    #[must_use]
    pub const fn untagged_raster_policy(&self) -> UntaggedRasterPolicy {
        self.color_settings.untagged_raster_policy
    }

    /// Returns a compatibility projection of text layouts in canonical page/object
    /// order, followed by pasteboard objects. Layout is stored only in the scenes.
    ///
    /// This allocates a bounded vector of shared references for legacy preview
    /// serialization. Scene consumers should traverse [`Self::pages`] directly;
    /// callers must not use this projection inside per-line or per-glyph loops.
    #[must_use]
    pub fn text_boxes(&self) -> Vec<&ResolvedTextBoxPreview> {
        self.pages
            .iter()
            .flat_map(|page| page.objects.iter())
            .chain(self.pasteboard.objects.iter())
            .filter_map(|object| object.kind.text_layout())
            .collect()
    }

    /// Export pages in canonical order. Blank pages are retained as empty scenes.
    #[must_use]
    pub fn pages(&self) -> &[ResolvedPageScene] {
        &self.pages
    }

    /// Workspace scene excluded from [`Self::pages`].
    #[must_use]
    pub const fn pasteboard(&self) -> &ResolvedPasteboardScene {
        &self.pasteboard
    }

    /// Reports whether native preview work was bounded before all text could be resolved.
    #[must_use]
    pub const fn budget_state(&self) -> PreviewBudgetState {
        self.budget_state
    }

    /// Returns a domain-separated digest of every paint- and layout-relevant
    /// fact in this immutable scene. The encoding uses explicit tags, lengths,
    /// and fixed-width integers so it remains independent of Rust's `Debug`
    /// output and platform word size.
    #[must_use]
    pub fn semantic_digest(&self) -> [u8; 32] {
        let mut digest = SceneDigest::new();
        digest.u64(self.revision.value());
        digest.tag(match self.budget_state {
            PreviewBudgetState::Complete => 0,
            PreviewBudgetState::Truncated => 1,
        });
        digest.tag(match self.color_settings.untagged_raster_policy {
            UntaggedRasterPolicy::RequireEmbeddedProfile => 0,
            UntaggedRasterPolicy::AssumeSrgb => 1,
        });
        digest.len(self.pages.len());
        for page in &self.pages {
            digest.string(page.page_id.as_str());
            digest.bounds(page.trim_box);
            digest.bounds(page.bleed_box);
            digest.edge_expectations(page.bleed_expectations);
            digest.objects(&page.objects);
        }
        digest.objects(&self.pasteboard.objects);
        digest.finish()
    }
}

struct SceneDigest(Sha256);

impl SceneDigest {
    fn new() -> Self {
        let mut digest = Sha256::new();
        digest.update(b"publisher-v2/resolved-publication-semantics/v8\0");
        Self(digest)
    }

    fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }

    fn tag(&mut self, value: u8) {
        self.0.update([value]);
    }

    fn bool(&mut self, value: bool) {
        self.tag(u8::from(value));
    }

    fn u16(&mut self, value: u16) {
        self.0.update(value.to_be_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.0.update(value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.update(value.to_be_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.0.update(value.to_be_bytes());
    }

    fn len(&mut self, value: usize) {
        self.u64(u64::try_from(value).expect("supported scene sizes fit in u64"));
    }

    fn bytes(&mut self, value: &[u8]) {
        self.len(value.len());
        self.0.update(value);
    }

    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn bounds(&mut self, value: PhysicalBounds) {
        self.i64(value.x.value());
        self.i64(value.y.value());
        self.i64(value.width.value());
        self.i64(value.height.value());
    }

    fn color(&mut self, value: Color) {
        self.0
            .update([value.red, value.green, value.blue, value.alpha]);
    }

    fn edge_expectations(&mut self, value: EdgeExpectations) {
        use document_core::model::BleedExpectation;
        for edge in [value.top, value.right, value.bottom, value.left] {
            self.tag(match edge {
                BleedExpectation::Auto => 0,
                BleedExpectation::Required => 1,
                BleedExpectation::NotIntended => 2,
            });
        }
    }

    fn objects(&mut self, objects: &[ResolvedSceneObject]) {
        self.len(objects.len());
        for object in objects {
            self.string(object.object_id.as_str());
            self.string(&object.name);
            self.i64(object.size.width.value());
            self.i64(object.size.height.value());
            self.transform(object.transform);
            self.u32(object.opacity_millionths);
            self.effects(object.effects);
            self.i64(object.corner_rounding.top_left_um);
            self.i64(object.corner_rounding.top_right_um);
            self.i64(object.corner_rounding.bottom_right_um);
            self.i64(object.corner_rounding.bottom_left_um);
            self.tag(u8::from(object.corner_rounding.linked));
            self.object_kind(&object.kind);
        }
    }

    fn transform(&mut self, value: Transform) {
        self.i64(value.translation_x.value());
        self.i64(value.translation_y.value());
        self.i64(value.pivot_x.value());
        self.i64(value.pivot_y.value());
        self.i64(value.rotation_microdegrees);
        self.i64(value.scale_x_nanos);
        self.i64(value.scale_y_nanos);
        self.i64(value.shear_x_nanos);
        self.i64(value.shear_y_nanos);
    }

    fn effects(&mut self, value: ObjectEffects) {
        match value.shadow {
            Some(shadow) => {
                self.tag(1);
                self.color(shadow.color);
                self.i64(shadow.offset_x.value());
                self.i64(shadow.offset_y.value());
            }
            None => self.tag(0),
        }
        self.optional_stroke(value.text_outline);
        self.optional_stroke(value.picture_border);
    }

    fn optional_stroke(&mut self, value: Option<ShapeStroke>) {
        match value {
            Some(stroke) => {
                self.tag(1);
                self.color(stroke.color);
                self.u32(stroke.width_millipoints);
            }
            None => self.tag(0),
        }
    }

    fn object_kind(&mut self, value: &ResolvedSceneObjectKind) {
        match value {
            ResolvedSceneObjectKind::Text { color, layout } => {
                self.tag(0);
                self.color(*color);
                self.text_layout(layout);
            }
            ResolvedSceneObjectKind::Image {
                asset,
                content_kind,
                crop,
                flip,
            } => {
                self.tag(1);
                self.string(asset.as_str());
                self.tag(match content_kind {
                    ImageContentKind::ContinuousTone => 0,
                    ImageContentKind::LineArt => 1,
                });
                self.u32(crop.left_millionths);
                self.u32(crop.top_millionths);
                self.u32(crop.right_millionths);
                self.u32(crop.bottom_millionths);
                self.tag(u8::from(flip.horizontal));
                self.tag(u8::from(flip.vertical));
            }
            ResolvedSceneObjectKind::Shape {
                shape,
                fill,
                stroke,
            } => {
                self.tag(2);
                self.tag(match shape {
                    ShapeKind::Rectangle => 0,
                    ShapeKind::Ellipse => 1,
                    ShapeKind::Cube => 2,
                    ShapeKind::Line => 3,
                    ShapeKind::Triangle => 4,
                    ShapeKind::Arrow => 5,
                });
                self.color(*fill);
                self.color(stroke.color);
                self.u32(stroke.width_millipoints);
            }
            ResolvedSceneObjectKind::Table { table } => {
                self.tag(3);
                self.u16(table.rows);
                self.u16(table.columns);
                self.len(table.column_boundaries_um.len());
                for boundary in &table.column_boundaries_um {
                    self.i64(*boundary);
                }
                self.len(table.row_boundaries_um.len());
                for boundary in &table.row_boundaries_um {
                    self.i64(*boundary);
                }
                self.color(table.grid_color);
                self.u32(table.grid_width_millipoints);
                self.color(table.text_color);
                self.len(table.merged_cells.len());
                for span in &table.merged_cells {
                    self.u16(span.start_row); self.u16(span.end_row); self.u16(span.start_column); self.u16(span.end_column);
                }
                self.len(table.cell_fills.len());
                for fill in &table.cell_fills {
                    self.u16(fill.row); self.u16(fill.column); self.u16(fill.row_span); self.u16(fill.column_span); self.color(fill.color);
                }
                self.len(table.border_segments.len());
                for edge in &table.border_segments {
                    self.i64(edge.x1_um); self.i64(edge.y1_um); self.i64(edge.x2_um); self.i64(edge.y2_um); self.color(edge.color); self.u32(edge.width_millipoints); self.len(edge.outline.len());
                    for command in &edge.outline {
                        use document_core::shape_geometry::ShapePathCommand;
                        match *command {
                            ShapePathCommand::MoveTo(x,y) => {self.tag(0);self.i64(x);self.i64(y);},
                            ShapePathCommand::LineTo(x,y) => {self.tag(1);self.i64(x);self.i64(y);},
                            ShapePathCommand::CubicTo(x1,y1,x2,y2,x,y) => {self.tag(2);for value in [x1,y1,x2,y2,x,y] {self.i64(value);}},
                            ShapePathCommand::Close => self.tag(3),
                        }
                    }
                }
                self.len(table.populated_cells.len());
                for cell in &table.populated_cells {
                    self.u16(cell.row);
                    self.u16(cell.column);
                    self.u16(cell.row_span); self.u16(cell.column_span);
                    self.string(&cell.text);
                    self.text_layout(&cell.layout);
                    self.bool(cell.clipped);
                }
            }
        }
    }

    fn text_layout(&mut self, value: &ResolvedTextBoxPreview) {
        self.string(value.object_id.as_str());
        self.tag(match value.sizing_mode {
            TextBoxSizingMode::Fit => 0,
            TextBoxSizingMode::Fixed => 1,
        });
        self.i64(value.composed_size.width.value());
        self.i64(value.composed_size.height.value());
        self.font_binding(&value.font_binding);
        self.len(value.font_bindings.len());
        for binding in &value.font_bindings {
            self.font_range_binding(binding);
        }
        self.len(value.permitted_fallback_fonts.len());
        for face in &value.permitted_fallback_fonts {
            self.font_face(face);
        }
        self.len(value.resolved_fonts.len());
        for face in &value.resolved_fonts {
            self.font_face(face);
        }
        self.len(value.lines.len());
        for line in &value.lines {
            self.source_range(&line.source);
            self.string(&line.text);
            self.i64(line.x.value());
            self.i64(line.y.value());
            self.i64(line.width.value());
            self.i64(line.height.value());
            self.i64(line.baseline.value());
            self.len(line.glyph_runs.len());
            for run in &line.glyph_runs {
                self.source_range(&run.source);
                self.string(&run.text);
                self.string(&run.glyph_text);
                self.len(run.font_index);
                self.i64(run.font_size.value());
                self.color(run.color);
                if let Some(underline) = &run.underline {
                    self.tag(1);
                    self.i64(underline.x.value());
                    self.i64(underline.y.value());
                    self.i64(underline.width.value());
                    self.i64(underline.thickness.value());
                } else {
                    self.tag(0);
                }
                self.bool(run.requires_font_synthesis);
                match run.list_marker_paragraph {
                    Some(paragraph) => {
                        self.tag(1);
                        self.u32(paragraph);
                    }
                    None => self.tag(0),
                }
                self.bool(run.right_to_left);
                self.len(run.glyphs.len());
                for glyph in &run.glyphs {
                    self.u32(glyph.id);
                    self.i64(glyph.x.value());
                    self.i64(glyph.y.value());
                    self.i64(glyph.advance.value());
                    match &glyph.outline {
                        Some(outline) => {
                            self.tag(1);
                            self.len(outline.commands.len());
                            for command in &outline.commands {
                                self.outline_command(command);
                            }
                        }
                        None => self.tag(0),
                    }
                    self.tag(match glyph.fallback {
                        None => 0,
                        Some(ResolvedGlyphFallback::Missing) => 1,
                        Some(ResolvedGlyphFallback::Colr) => 2,
                        Some(ResolvedGlyphFallback::Svg) => 3,
                        Some(ResolvedGlyphFallback::Bitmap) => 4,
                    });
                }
            }
        }
        self.bool(value.overset);
        match &value.overset_source {
            Some(source) => {
                self.tag(1);
                self.source_range(source);
            }
            None => self.tag(0),
        }
        self.tag(match value.preview_state {
            TextBoxPreviewState::Complete => 0,
            TextBoxPreviewState::MissingFont => 1,
            TextBoxPreviewState::BudgetExceeded => 2,
        });
    }

    fn font_face(&mut self, value: &ResolvedFontFacePreview) {
        self.string(&value.sha256_hex);
        self.u32(value.collection_index);
    }

    fn font_binding(&mut self, value: &RequestedFontBindingPreview) {
        self.string(&value.requested_family);
        self.optional_font_face(value.exact_face.as_ref());
        self.font_substitution_status(value.substitution_status);
    }

    fn font_range_binding(&mut self, value: &RequestedFontRangeBindingPreview) {
        self.source_range(&value.source);
        self.string(&value.requested_family);
        self.u16(value.requested_weight);
        self.bool(value.requested_italic);
        self.optional_font_face(value.exact_face.as_ref());
        self.font_substitution_status(value.substitution_status);
    }

    fn optional_font_face(&mut self, value: Option<&ResolvedFontFacePreview>) {
        if let Some(face) = value {
            self.tag(1);
            self.font_face(face);
        } else {
            self.tag(0);
        }
    }

    fn font_substitution_status(&mut self, value: FontSubstitutionStatus) {
        self.tag(match value {
            FontSubstitutionStatus::Exact => 0,
            FontSubstitutionStatus::Substituted => 1,
            FontSubstitutionStatus::Unbound => 2,
            FontSubstitutionStatus::Missing => 3,
        });
    }

    fn source_range(&mut self, value: &SourceRange) {
        self.len(value.start);
        self.len(value.end);
    }

    fn outline_command(&mut self, value: &crate::text::ResolvedOutlineCommand) {
        use crate::text::ResolvedOutlineCommand;
        match value {
            ResolvedOutlineCommand::MoveTo { x, y } => {
                self.tag(0);
                self.i64(x.value());
                self.i64(y.value());
            }
            ResolvedOutlineCommand::LineTo { x, y } => {
                self.tag(1);
                self.i64(x.value());
                self.i64(y.value());
            }
            ResolvedOutlineCommand::QuadTo {
                control_x,
                control_y,
                x,
                y,
            } => {
                self.tag(2);
                for coordinate in [control_x, control_y, x, y] {
                    self.i64(coordinate.value());
                }
            }
            ResolvedOutlineCommand::CurveTo {
                control_1_x,
                control_1_y,
                control_2_x,
                control_2_y,
                x,
                y,
            } => {
                self.tag(3);
                for coordinate in [control_1_x, control_1_y, control_2_x, control_2_y, x, y] {
                    self.i64(coordinate.value());
                }
            }
            ResolvedOutlineCommand::Close => self.tag(4),
        }
    }
}

/// Returns only exact font bindings reachable within the canonical text-preview budget.
///
/// Callers use this inexpensive document-only pass before loading or cloning
/// embedded font bytes, so over-budget text cannot consume font resources.
#[must_use]
pub fn preview_font_face_bindings(
    project: &ProjectSnapshot,
) -> BTreeSet<(document_core::AssetHash, u32)> {
    scoped_preview_font_face_bindings(
        &project.document,
        project
            .document
            .pages
            .iter()
            .map(|page| page.objects.as_slice())
            .chain(core::iter::once(
                project.document.pasteboard.objects.as_slice(),
            )).chain(project.document.masters.iter().map(|master| master.objects.as_slice())),
    )
}

/// Discovers fonts using the same page-first traversal and budget as
/// [`ResolvedPagePreview::resolve_with_font_resources`]. Document-wide permitted
/// fallback chains apply to every scope; exact faces used only by other pages do not.
///
/// # Errors
/// Returns [`PublicationLayoutError::PageNotFound`] for an unknown page.
pub fn page_preview_font_face_bindings(
    project: &ProjectSnapshot,
    page_id: &PageId,
    include_pasteboard: bool,
) -> Result<BTreeSet<(AssetHash, u32)>, PublicationLayoutError> {
    requested_page(project, page_id)?;
    let page = project.document.composed_page(page_id).map_err(|_| PublicationLayoutError::InvalidOutputSource)?;
    Ok(scoped_preview_font_face_bindings(
        &project.document,
        core::iter::once(page.objects.as_slice())
            .chain(include_pasteboard.then_some(project.document.pasteboard.objects.as_slice())),
    ))
}

/// Discovers every exact face used on published pages, plus document fallback
/// chains, under the output per-page policy. Pasteboard-only faces are excluded.
///
/// # Errors
/// Returns an invalid-source or page-budget error instead of a partial font set.
pub fn output_font_face_bindings(
    project: &ProjectSnapshot,
) -> Result<BTreeSet<(AssetHash, u32)>, PublicationLayoutError> {
    validate_output_source(project)?;
    let mut bindings = BTreeSet::new();
    for page in &project.document.pages {
        let page = project.document.composed_page(&page.id).map_err(|_| PublicationLayoutError::InvalidOutputSource)?;
        let (page_bindings, truncated) = scoped_font_face_bindings_with_budget(
            &project.document,
            core::iter::once(page.objects.as_slice()),
            PreviewBudget::complete(),
        );
        if truncated {
            return Err(PublicationLayoutError::OutputPageBudgetExceeded(
                page.id.clone(),
            ));
        }
        bindings.extend(page_bindings);
    }
    Ok(bindings)
}

/// Discovers resources from an unassigned Master's editable source workspace.
///
/// # Errors
/// Refuses an unavailable Master or invalid source.
pub fn master_preview_font_face_bindings(project: &ProjectSnapshot, master_id: &document_core::model::MasterId)
    -> Result<BTreeSet<(AssetHash,u32)>,PublicationLayoutError> {
    let page = project.document.master_preview_page(master_id).map_err(|_| PublicationLayoutError::InvalidOutputSource)?;
    Ok(scoped_preview_font_face_bindings(&project.document, core::iter::once(page.objects.as_slice())))
}

fn validate_output_source(project: &ProjectSnapshot) -> Result<(), PublicationLayoutError> {
    // Reuse canonical source budgets before validation or hashing allocates
    // derived data. Asset bytes/descriptors remain the caller's responsibility.
    document_core::package::validate_document_saveability(
        &project.document,
        project.revision,
        &BTreeMap::new(),
    )
    .map_err(|_| PublicationLayoutError::InvalidOutputSource)?;
    project
        .document
        .validate()
        .map_err(|_| PublicationLayoutError::InvalidOutputSource)
}

fn scoped_preview_font_face_bindings<'a>(
    document: &Document,
    object_groups: impl IntoIterator<Item = &'a [PublicationObject]>,
) -> BTreeSet<(AssetHash, u32)> {
    scoped_font_face_bindings_with_budget(document, object_groups, PreviewBudget::default()).0
}

fn scoped_font_face_bindings_with_budget<'a>(
    document: &Document,
    object_groups: impl IntoIterator<Item = &'a [PublicationObject]>,
    mut budget: PreviewBudget,
) -> (BTreeSet<(AssetHash, u32)>, bool) {
    let mut bindings = BTreeSet::new();
    for chain in &document.exact_font_fallbacks {
        for face in &chain.faces {
            bindings.insert((face.source.clone(), face.face_index));
        }
    }
    for objects in object_groups {
        for object in objects {
            match &object.kind {
                ObjectKind::Text {
                    text, style, story, ..
                } => {
                    if budget.reserve(text.len()) {
                        if let Some(binding) = &style.font_face {
                            bindings.insert((binding.source.clone(), binding.face_index));
                        }
                        for range in effective_font_variant_ranges(
                            style,
                            story,
                            TextSourceRange {
                                start: 0,
                                end: text.len(),
                            },
                        ) {
                            if let Some(binding) = style.exact_font_face_for_family(
                                &range.font_family,
                                range.font_weight,
                                range.italic,
                            ) {
                                bindings.insert((binding.source.clone(), binding.face_index));
                            }
                        }
                    }
                }
                ObjectKind::Table { table } => {
                    if budget.reserve_table(table) {
                        if let Some(binding) = &table.style.font_face {bindings.insert((binding.source.clone(), binding.face_index));}
                        for row in 0..table.rows {for column in 0..table.columns {
                            let index=usize::from(row)*usize::from(table.columns)+usize::from(column);
                            if !table.cells[index].is_empty() && let Some(binding) = table.effective_cell_style(row,column).font_face {
                                bindings.insert((binding.source,binding.face_index));
                            }
                        }}
                    }
                }
                ObjectKind::Image { .. } | ObjectKind::Shape { .. } => {}
            }
        }
    }
    (bindings, budget.truncated)
}

#[cfg(test)]
fn reserve_empty_table_preview_slot(table: &TableData, budget: &mut PreviewBudget) -> Option<bool> {
    table
        .cells
        .iter()
        .all(String::is_empty)
        .then(|| budget.reserve(0))
}

struct PreviewBudget {
    text_boxes: usize,
    text_bytes: usize,
    truncated: bool,
    max_items: usize,
    max_total_bytes: usize,
}

impl Default for PreviewBudget {
    fn default() -> Self {
        Self {
            text_boxes: 0,
            text_bytes: 0,
            truncated: false,
            max_items: MAX_PREVIEW_TEXT_BOXES,
            max_total_bytes: MAX_PREVIEW_TEXT_BYTES_TOTAL,
        }
    }
}

impl PreviewBudget {
    fn complete() -> Self {
        Self {
            max_items: document_core::PACKAGE_LIMITS_V1.objects,
            max_total_bytes: document_core::PACKAGE_LIMITS_V1
                .story_characters
                .saturating_mul(4),
            ..Self::default()
        }
    }
    fn reserve(&mut self, text_len: usize) -> bool {
        self.reserve_item(text_len, text_len <= MAX_PREVIEW_TEXT_BYTES_PER_BOX)
    }
    fn reserve_table(&mut self, table: &TableData) -> bool {
        let valid_items = table
            .cells
            .iter()
            .all(|cell| cell.len() <= MAX_PREVIEW_TEXT_BYTES_PER_BOX);
        let total = table
            .cells
            .iter()
            .fold(0_usize, |sum, cell| sum.saturating_add(cell.len()));
        self.reserve_item(total, valid_items)
    }
    fn reserve_item(&mut self, text_len: usize, valid_item: bool) -> bool {
        self.text_boxes = self.text_boxes.saturating_add(1);
        let next_bytes = self.text_bytes.saturating_add(text_len);
        let allowed =
            self.text_boxes <= self.max_items && valid_item && next_bytes <= self.max_total_bytes;
        if allowed {
            self.text_bytes = next_bytes;
        } else {
            self.truncated = true;
        }
        allowed
    }
}

struct PreparedTextFonts {
    base_font: PreparedFontFamily,
    base_binding: RequestedFontBindingPreview,
    families: BTreeMap<(String, u16, bool), PreparedFontFamily>,
    bindings: Vec<RequestedFontRangeBindingPreview>,
    has_unavailable_face: bool,
}

#[derive(Clone)]
struct PreparedFontFamily {
    family: String,
    font_weight: u16,
    italic: bool,
}

fn face_preview(binding: &document_core::model::FontFaceBinding) -> ResolvedFontFacePreview {
    ResolvedFontFacePreview {
        sha256_hex: binding.source.as_str().to_owned(),
        collection_index: binding.face_index,
    }
}

fn used_permitted_fallback_previews(
    engine: &TextLayoutEngine,
    resolved_fonts: &[ResolvedFontFacePreview],
) -> Vec<ResolvedFontFacePreview> {
    let mut result = Vec::new();
    for (source, face_index) in engine.exact_fallback_faces() {
        let face = ResolvedFontFacePreview {
            sha256_hex: source.as_str().to_owned(),
            collection_index: *face_index,
        };
        if resolved_fonts.contains(&face) && !result.contains(&face) {
            result.push(face);
        }
    }
    result
}

fn configure_exact_font_fallbacks(
    engine: &mut TextLayoutEngine,
    document: &Document,
    font_resources: &BTreeMap<(document_core::AssetHash, u32), &ExactFontResource>,
) -> Result<(), PublicationLayoutError> {
    for chain in &document.exact_font_fallbacks {
        let resources = chain
            .faces
            .iter()
            .map(|face| {
                font_resources
                    .get(&(face.source.clone(), face.face_index))
                    .copied()
                    .ok_or(PublicationLayoutError::ExactFontUnavailable)
            })
            .collect::<Result<Vec<_>, _>>()?;
        engine
            .set_exact_fallback_fonts(chain.script.open_type_tag(), resources)
            .map_err(|_| PublicationLayoutError::ExactFontUnavailable)?;
    }
    Ok(())
}

fn prepare_text_fonts(
    engine: &mut TextLayoutEngine,
    text: &str,
    style: &TextStyle,
    formatting: &TextStoryFormatting,
    font_resources: &BTreeMap<(document_core::AssetHash, u32), &ExactFontResource>,
) -> PreparedTextFonts {
    let effective_ranges = effective_font_variant_ranges(
        style,
        formatting,
        TextSourceRange {
            start: 0,
            end: text.len(),
        },
    );
    let mut families = BTreeMap::new();
    let mut availability =
        BTreeMap::<(String, u16, bool), (PreparedFontFamily, FontSubstitutionStatus)>::new();
    let mut has_unavailable_face = false;
    let base_key = (style.font_family.clone(), style.font_weight, style.italic);

    let mut prepare_key = |family: &str, font_weight: u16, italic: bool| {
        let key = (family.to_owned(), font_weight, italic);
        if availability.contains_key(&key) {
            return;
        }
        let Some(binding) = style.exact_font_face_for_family(family, font_weight, italic) else {
            // Output cannot shape an unincluded family through machine fonts.
            // Keep the unbound fact so preflight can name the object to repair.
            has_unavailable_face |= !engine.system_fonts_enabled();
            let font = PreparedFontFamily {
                family: family.to_owned(),
                font_weight,
                italic,
            };
            availability.insert(key.clone(), (font.clone(), FontSubstitutionStatus::Unbound));
            families.insert(key, font);
            return;
        };
        let font = font_resources
            .get(&(binding.source.clone(), binding.face_index))
            .and_then(|resource| {
                let face = ttf_parser::Face::parse(&resource.bytes, resource.face_index).ok()?;
                let family = engine.register_exact_font(resource).ok()?;
                Some(PreparedFontFamily {
                    family,
                    font_weight: face.weight().to_number(),
                    italic: face.is_italic(),
                })
            });
        if let Some(font) = font {
            availability.insert(key.clone(), (font.clone(), FontSubstitutionStatus::Exact));
            families.insert(key, font);
        } else {
            has_unavailable_face = true;
            availability.insert(
                key,
                (
                    PreparedFontFamily {
                        family: String::new(),
                        font_weight,
                        italic,
                    },
                    FontSubstitutionStatus::Missing,
                ),
            );
        }
    };

    prepare_key(&style.font_family, style.font_weight, style.italic);
    for range in &effective_ranges {
        prepare_key(&range.font_family, range.font_weight, range.italic);
    }

    let (base_font, base_status) = availability
        .get(&base_key)
        .cloned()
        .expect("base key is prepared");
    let base_binding = RequestedFontBindingPreview {
        requested_family: style.font_family.clone(),
        exact_face: style.font_face.as_ref().map(face_preview),
        substitution_status: base_status,
    };
    let bindings = effective_ranges
        .iter()
        .map(|range| font_range_binding_preview(style, &availability, range))
        .collect();

    PreparedTextFonts {
        base_font,
        base_binding,
        families,
        bindings,
        has_unavailable_face,
    }
}

fn font_range_binding_preview(
    style: &TextStyle,
    availability: &BTreeMap<(String, u16, bool), (PreparedFontFamily, FontSubstitutionStatus)>,
    range: &EffectiveFontVariantRange,
) -> RequestedFontRangeBindingPreview {
    let status = availability
        .get(&(range.font_family.clone(), range.font_weight, range.italic))
        .map(|(_, status)| *status)
        .expect("effective key is prepared");
    RequestedFontRangeBindingPreview {
        source: SourceRange {
            start: range.range.start,
            end: range.range.end,
        },
        requested_family: range.font_family.clone(),
        requested_weight: range.font_weight,
        requested_italic: range.italic,
        exact_face: style
            .exact_font_face_for_family(&range.font_family, range.font_weight, range.italic)
            .map(face_preview),
        substitution_status: status,
    }
}

fn resolve_page_scene(
    engine: &mut TextLayoutEngine,
    page: &Page,
    font_resources: &BTreeMap<(document_core::AssetHash, u32), &ExactFontResource>,
    budget: &mut PreviewBudget,
) -> Result<ResolvedPageScene, PublicationLayoutError> {
    let (trim_box, bleed_box) = page_boxes(page)?;
    Ok(ResolvedPageScene {
        page_id: page.id.clone(),
        trim_box,
        bleed_box,
        bleed_expectations: page.bleed_expectations,
        objects: resolve_object_group(
            engine,
            &page.objects,
            &page.objects,
            font_resources,
            budget,
        )?,
    })
}

fn resolve_document_page_scene(
    engine: &mut TextLayoutEngine,
    document: &Document,
    source: &Page,
    font_resources: &BTreeMap<(document_core::AssetHash, u32), &ExactFontResource>,
    budget: &mut PreviewBudget,
) -> Result<ResolvedPageScene, PublicationLayoutError> {
    let page = document.composed_page(&source.id).map_err(|_| PublicationLayoutError::InvalidOutputSource)?;
    let mut scene = resolve_page_scene(engine, &page, font_resources, budget)?;
    if let Some(master_id) = &source.master_id {
        let master = document.masters.iter().find(|master| &master.id == master_id)
            .ok_or(PublicationLayoutError::InvalidOutputSource)?;
        let ids = master.objects.iter().map(|object| &object.id).collect::<BTreeSet<_>>();
        for object in &mut scene.objects {
            if ids.contains(&object.object_id) { object.master_id = Some(master_id.clone()); }
        }
    }
    Ok(scene)
}

fn page_boxes(page: &Page) -> Result<(PhysicalBounds, PhysicalBounds), PublicationLayoutError> {
    let trim_box = PhysicalBounds {
        x: document_core::model::Micrometres::new(0),
        y: document_core::model::Micrometres::new(0),
        width: page.trim.width,
        height: page.trim.height,
    };
    let bleed_width = page
        .trim
        .width
        .value()
        .checked_add(page.bleed.left.value())
        .and_then(|value| value.checked_add(page.bleed.right.value()))
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    let bleed_height = page
        .trim
        .height
        .value()
        .checked_add(page.bleed.top.value())
        .and_then(|value| value.checked_add(page.bleed.bottom.value()))
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    let bleed_x = page
        .bleed
        .left
        .value()
        .checked_neg()
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    let bleed_y = page
        .bleed
        .top
        .value()
        .checked_neg()
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    Ok((
        trim_box,
        PhysicalBounds {
            x: document_core::model::Micrometres::new(bleed_x),
            y: document_core::model::Micrometres::new(bleed_y),
            width: document_core::model::Micrometres::new(bleed_width),
            height: document_core::model::Micrometres::new(bleed_height),
        },
    ))
}

fn resolve_object_group(
    engine: &mut TextLayoutEngine,
    objects: &[PublicationObject],
    wrap_objects: &[PublicationObject],
    font_resources: &BTreeMap<(document_core::AssetHash, u32), &ExactFontResource>,
    budget: &mut PreviewBudget,
) -> Result<Vec<ResolvedSceneObject>, PublicationLayoutError> {
    objects
        .iter()
        .map(|object| {
            let mut resolved_size = object.size;
            let kind = match &object.kind {
                ObjectKind::Text {
                    text,
                    style,
                    frame,
                    story,
                } => {
                    let layout = if budget.reserve(text.len()) {
                        if frame.sizing_mode == TextBoxSizingMode::Fit {
                            match compose_fit_text_box(
                                engine,
                                font_resources,
                                object,
                                text,
                                style,
                                story,
                            ) {
                                Ok(fitted) => {
                                    resolved_size = fitted.size;
                                    fitted.layout
                                }
                                Err(PublicationLayoutError::ExactFontUnavailable) => {
                                    missing_font_text_box_for_story(
                                        engine,
                                        object,
                                        text,
                                        style,
                                        story,
                                        font_resources,
                                    )
                                }
                                Err(error) => return Err(error),
                            }
                        } else {
                            resolve_text_box(engine, wrap_objects, font_resources, object)?
                        }
                    } else {
                        budget_exceeded_text_box(object, style)
                    };
                    ResolvedSceneObjectKind::Text {
                        color: style.color,
                        layout,
                    }
                }
                ObjectKind::Image {
                    asset,
                    content_kind,
                    crop,
                    flip,
                    ..
                } => ResolvedSceneObjectKind::Image {
                    asset: asset.clone(),
                    content_kind: *content_kind,
                    crop: *crop,
                    flip: *flip,
                },
                ObjectKind::Shape {
                    shape,
                    fill,
                    stroke,
                    ..
                } => ResolvedSceneObjectKind::Shape {
                    shape: *shape,
                    fill: *fill,
                    stroke: *stroke,
                },
                ObjectKind::Table { table } => ResolvedSceneObjectKind::Table {
                    table: resolve_table(engine, font_resources, object, table, budget)?,
                },
            };
            Ok(ResolvedSceneObject {
                object_id: object.id.clone(),
                master_id: None,
                name: object.name.clone(),
                size: resolved_size,
                transform: object.transform,
                opacity_millionths: object.opacity_millionths,
                effects: object.effects,
                corner_rounding: object.corner_rounding,
                kind,
            })
        })
        .collect()
}


fn resolve_table(
    engine: &mut TextLayoutEngine,
    font_resources: &BTreeMap<(document_core::AssetHash, u32), &ExactFontResource>,
    object: &PublicationObject,
    table: &TableData,
    budget: &mut PreviewBudget,
) -> Result<ResolvedTablePreview, PublicationLayoutError> {
    let reachable = budget.reserve_table(table);
    let column_boundaries_um =
        partition_weighted_boundaries(object.size.width.value(), &table.column_weights)?;
    let row_boundaries_um =
        partition_weighted_boundaries(object.size.height.value(), &table.row_weights)?;
    let mut paint = table_authoring::resolve_paint(table, &column_boundaries_um, &row_boundaries_um);
    table_authoring::round_outer_borders(&mut paint.borders, object.size, object.corner_rounding)
        .map_err(|_| PublicationLayoutError::GeometryOutOfRange)?;
    let style = &table.style;
    let font_family = exact_font_family(engine, style, font_resources);
    let mut cell_fonts = BTreeMap::from([(
        (style.font_family.clone(), style.font_weight, style.italic),
        font_family.clone(),
    )]);
    let mut populated_cells = Vec::new();
    for row in 0..table.rows {
        for column in 0..table.columns {
            let index = usize::from(row) * usize::from(table.columns) + usize::from(column);
            let text = &table.cells[index];
            if text.is_empty() || paint.owners[index] != (row, column) {
                continue;
            }
            let cell_style = table.effective_cell_style(row, column);
            let cell_font = cell_fonts
                .entry((
                    cell_style.font_family.clone(),
                    cell_style.font_weight,
                    cell_style.italic,
                ))
                .or_insert_with(|| exact_font_family(engine, &cell_style, font_resources))
                .clone();
            let span = table.cell_span(row, column);
            let row_span = span.map_or(1, |span| span.end_row - span.start_row);
            let column_span = span.map_or(1, |span| span.end_column - span.start_column);
            let formatting = table.cell_format(row, column);
            let horizontal_alignment = formatting
                .and_then(|value| value.horizontal_alignment)
                .unwrap_or(cell_style.alignment);
            let vertical_alignment = formatting
                .and_then(|value| value.vertical_alignment)
                .unwrap_or_default();
            let (layout, clipped) = if reachable {
                match &cell_font {
                    Some(font_family) => resolve_table_cell(
                        engine,
                        object,
                        text,
                        &cell_style,
                        font_family.clone(),
                        horizontal_alignment,
                        vertical_alignment,
                        row,
                        column,
                        row_span,
                        column_span,
                        &column_boundaries_um,
                        &row_boundaries_um,
                    )?,
                    None => (missing_font_text_box(object, &cell_style), false),
                }
            } else {
                (budget_exceeded_text_box(object, style), false)
            };
            populated_cells.push(ResolvedTableCellPreview {
                row,
                column,
                row_span,
                column_span,
                text: text.clone(),
                clipped,
                layout,
            });
        }
    }
    // The Table-level binding describes its default face. Each cell carries
    // its own independently checked binding; mixed cell faces are not a
    // substitution of the Table's default face.
    let font_binding = font_binding_preview(style, &[], font_family.is_some(), &[]);
    Ok(ResolvedTablePreview {
        rows: table.rows,
        columns: table.columns,
        column_boundaries_um,
        row_boundaries_um,
        grid_color: table.grid_border.color,
        grid_width_millipoints: table.grid_border.width_millipoints,
        text_color: style.color,
        font_binding,
        merged_cells: table.merged_cells.clone(),
        cell_fills: paint.fills,
        border_segments: paint.borders,
        populated_cells,
    })
}

fn partition_weighted_boundaries(
    total_um: i64,
    weights: &[u32],
) -> Result<Vec<i64>, PublicationLayoutError> {
    document_core::shape_geometry::table_track_boundaries(total_um, weights)
        .map_err(|_| PublicationLayoutError::TableCellGeometryTooSmall)
}

#[expect(
    clippy::too_many_arguments,
    reason = "A cell layout is bound to its table object, address, shared grid boundaries, style, and native engine in one fail-closed operation."
)]
fn resolve_table_cell(
    engine: &mut TextLayoutEngine,
    object: &PublicationObject,
    text: &str,
    style: &TextStyle,
    font_family: String,
    cell_horizontal_alignment: TextAlignment,
    vertical_alignment: TableVerticalAlignment,
    row: u16,
    column: u16,
    row_span: u16,
    column_span: u16,
    column_boundaries_um: &[i64],
    row_boundaries_um: &[i64],
) -> Result<(ResolvedTextBoxPreview, bool), PublicationLayoutError> {
    let left = column_boundaries_um[usize::from(column)];
    let right = column_boundaries_um[usize::from(column + column_span)];
    let top = row_boundaries_um[usize::from(row)];
    let bottom = row_boundaries_um[usize::from(row + row_span)];
    let width = right
        .checked_sub(left)
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    let height = bottom
        .checked_sub(top)
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    if width <= 0 || height <= 0 {
        return Err(PublicationLayoutError::TableCellGeometryTooSmall);
    }
    let clearance =
        document_core::shape_geometry::rounded_interior_insets(object.size, object.corner_rounding)
            .map_err(|_| PublicationLayoutError::GeometryOutOfRange)?;
    let [padded_left, padded_top, padded_right, padded_bottom] =
        document_core::shape_geometry::rounded_cell_text_bounds(
            object.size,
            clearance,
            [left, top, right, bottom],
        )
        .map_err(|_| PublicationLayoutError::TableCellGeometryTooSmall)?;
    let padded_width = padded_right - padded_left;
    let padded_height = padded_bottom - padded_top;
    let story = text_story(text, style, font_family);
    let region = TextRegion {
        bounds: TextRect::new(
            micrometres_to_millipoints(i128::from(padded_left))?,
            micrometres_to_millipoints(i128::from(padded_top))?,
            micrometres_to_millipoints(i128::from(padded_width))?,
            micrometres_to_millipoints(i128::from(padded_height))?,
        )?,
        column_count: 1,
        column_gap: Millipoints::new(0),
        exclusions: Vec::new(),
    };
    let resolved = engine.resolve_aligned(
        &story,
        &region,
        horizontal_alignment(cell_horizontal_alignment),
    )?;
    let clipped =
        resolved.overset.is_some() || text_layout_exceeds_region(&resolved.lines, &region);
    let overset_source = resolved
        .overset
        .as_ref()
        .map(|overset| overset.source.clone());
    let resolved_fonts = resolved_font_previews(&resolved.fonts);
    let permitted_fallback_fonts = used_permitted_fallback_previews(engine, &resolved_fonts);
    let mut lines = text_line_previews(&story, &resolved.lines)?;
    let original_top = lines.iter().map(|line| line.y.value()).min();
    if !clipped {
        vertically_align_lines(&mut lines, &region, vertical_alignment)?;
    }
    let vertical_offset = lines
        .iter()
        .map(|line| line.y.value())
        .min()
        .zip(original_top)
        .map_or(0, |(aligned, original)| aligned - original);
    Ok((
        ResolvedTextBoxPreview {
            object_id: object.id.clone(),
            sizing_mode: TextBoxSizingMode::Fixed,
            composed_size: PhysicalSize::new(padded_width, padded_height)
                .map_err(|_| PublicationLayoutError::TableCellGeometryTooSmall)?,
            content_bounds: PhysicalBounds {
                x: document_core::model::Micrometres::new(padded_left),
                y: document_core::model::Micrometres::new(padded_top),
                width: document_core::model::Micrometres::new(padded_width),
                height: document_core::model::Micrometres::new(padded_height),
            },
            vertical_offset: Millipoints::new(vertical_offset),
            font_binding: font_binding_preview(
                style,
                &resolved_fonts,
                true,
                &permitted_fallback_fonts,
            ),
            font_bindings: Vec::new(),
            permitted_fallback_fonts,
            resolved_fonts,
            lines,
            overset: overset_source.is_some(),
            overset_source,
            preview_state: TextBoxPreviewState::Complete,
        },
        clipped,
    ))
}

fn vertically_align_lines(
    lines: &mut [ResolvedTextLinePreview],
    region: &TextRegion,
    alignment: TableVerticalAlignment,
) -> Result<(), PublicationLayoutError> {
    let Some(first_top) = lines.iter().map(|line| line.y.value()).min() else {
        return Ok(());
    };
    let mut last_bottom = first_top;
    for line in lines.iter() {
        last_bottom = last_bottom.max(
            line.y
                .value()
                .checked_add(line.height.value())
                .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
        );
    }
    let used_height = last_bottom
        .checked_sub(first_top)
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    let remaining_height = region
        .bounds
        .height
        .value()
        .checked_sub(used_height)
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    let vertical_offset = match alignment {
        TableVerticalAlignment::Top => 0,
        TableVerticalAlignment::Middle => remaining_height / 2,
        TableVerticalAlignment::Bottom => remaining_height,
    };
    let aligned_top = region
        .bounds
        .y
        .value()
        .checked_add(vertical_offset)
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    let offset = aligned_top
        .checked_sub(first_top)
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    for line in lines {
        line.y = Millipoints::new(
            line.y
                .value()
                .checked_add(offset)
                .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
        );
        line.baseline = Millipoints::new(
            line.baseline
                .value()
                .checked_add(offset)
                .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
        );
        for run in &mut line.glyph_runs {
            if let Some(underline) = &mut run.underline {
                underline.y = Millipoints::new(
                    underline
                        .y
                        .value()
                        .checked_add(offset)
                        .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
                );
            }
            for glyph in &mut run.glyphs {
                glyph.y = Millipoints::new(
                    glyph
                        .y
                        .value()
                        .checked_add(offset)
                        .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
                );
            }
        }
    }
    Ok(())
}

fn text_layout_exceeds_region(lines: &[ResolvedTextLine], region: &TextRegion) -> bool {
    let left = region.bounds.x.value();
    let top = region.bounds.y.value();
    let right = left.saturating_add(region.bounds.width.value());
    let bottom = top.saturating_add(region.bounds.height.value());
    lines.iter().any(|line| {
        line.y.value() < top
            || line.y.value().saturating_add(line.height.value()) > bottom
            || line.fragments.iter().any(|fragment| {
                fragment.glyph_runs.iter().any(|run| {
                    run.glyphs.iter().any(|glyph| {
                        let glyph_start = glyph.x.value();
                        let glyph_end = glyph_start.saturating_add(glyph.advance.value());
                        glyph_start.min(glyph_end) < left
                            || glyph_start.max(glyph_end) > right
                            || glyph.outline.as_ref().is_some_and(|outline| {
                                outline.commands.iter().any(|command| {
                                    outline_command_exceeds_region(
                                        command,
                                        glyph.x.value(),
                                        glyph.y.value(),
                                        left,
                                        top,
                                        right,
                                        bottom,
                                    )
                                })
                            })
                    })
                })
            })
    })
}

fn outline_command_exceeds_region(
    command: &crate::text::ResolvedOutlineCommand,
    glyph_x: i64,
    glyph_y: i64,
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
) -> bool {
    use crate::text::ResolvedOutlineCommand;
    let outside = |x: Millipoints, y: Millipoints| {
        let painted_x = glyph_x.saturating_add(x.value());
        let painted_y = glyph_y.saturating_sub(y.value());
        painted_x < left || painted_x > right || painted_y < top || painted_y > bottom
    };
    match command {
        ResolvedOutlineCommand::MoveTo { x, y } | ResolvedOutlineCommand::LineTo { x, y } => {
            outside(*x, *y)
        }
        ResolvedOutlineCommand::QuadTo {
            control_x,
            control_y,
            x,
            y,
        } => outside(*control_x, *control_y) || outside(*x, *y),
        ResolvedOutlineCommand::CurveTo {
            control_1_x,
            control_1_y,
            control_2_x,
            control_2_y,
            x,
            y,
        } => {
            outside(*control_1_x, *control_1_y)
                || outside(*control_2_x, *control_2_y)
                || outside(*x, *y)
        }
        ResolvedOutlineCommand::Close => false,
    }
}

fn resolve_text_box(
    engine: &mut TextLayoutEngine,
    objects: &[PublicationObject],
    font_resources: &BTreeMap<(document_core::AssetHash, u32), &ExactFontResource>,
    text_box: &PublicationObject,
) -> Result<ResolvedTextBoxPreview, PublicationLayoutError> {
    let ObjectKind::Text {
        text,
        style,
        frame,
        story: story_formatting,
    } = &text_box.kind
    else {
        return Err(PublicationLayoutError::ObjectKindMismatch);
    };
    let mut prepared = prepare_text_fonts(engine, text, style, story_formatting, font_resources);
    if prepared.has_unavailable_face {
        return Ok(missing_text_box_from_prepared(text_box, prepared));
    }
    let (story, paragraphs) = formatted_text_story(
        text,
        style,
        story_formatting,
        &prepared.base_font,
        &prepared.families,
    )?;
    let region = text_region(text_box, frame, objects)?;
    let (resolved, vertical_offset) = resolve_text_box_paragraphs(
        engine,
        &story,
        &region,
        &paragraphs,
        frame.vertical_alignment,
    )?;
    let mut resolved_fonts = resolved_font_previews(&resolved.fonts);
    let overset_source = resolved
        .overset
        .as_ref()
        .map(|overset| overset.source.clone())
        .or_else(|| {
            // Rounded text is clipped to its outline. Report ink outside the
            // shared safe interior instead of silently hiding it.
            if text_box.corner_rounding.is_default() {
                return None;
            }
            resolved
                .lines
                .iter()
                .find(|line| text_layout_exceeds_region(std::slice::from_ref(*line), &region))
                .map(|line| line.source.clone())
        });
    let mut lines = text_line_previews(&story, &resolved.lines)?;
    append_list_marker_previews(
        &mut lines,
        &mut resolved_fonts,
        &resolved.list_markers,
        &story,
    )?;
    let permitted_fallback_fonts = used_permitted_fallback_previews(engine, &resolved_fonts);
    resolve_font_binding_facts(
        &mut prepared.base_binding,
        &mut prepared.bindings,
        &lines,
        &resolved_fonts,
        &permitted_fallback_fonts,
    );
    let font_binding = aggregate_font_binding(prepared.base_binding, &prepared.bindings);
    Ok(ResolvedTextBoxPreview {
        object_id: text_box.id.clone(),
        sizing_mode: TextBoxSizingMode::Fixed,
        composed_size: text_box.size,
        content_bounds: text_content_bounds(text_box, text_box.size),
        vertical_offset,
        font_binding,
        font_bindings: prepared.bindings,
        permitted_fallback_fonts,
        resolved_fonts,
        lines,
        overset: overset_source.is_some(),
        overset_source,
        preview_state: TextBoxPreviewState::Complete,
    })
}

#[expect(clippy::too_many_lines, reason = "One Fit composition binds exact faces, preserves native ink, solves rounded clearance and emits the same final geometry for authoring and output")]
fn compose_fit_text_box(
    engine: &mut TextLayoutEngine,
    font_resources: &BTreeMap<(document_core::AssetHash, u32), &ExactFontResource>,
    text_box: &PublicationObject,
    text: &str,
    style: &TextStyle,
    story_formatting: &TextStoryFormatting,
) -> Result<ResolvedFitTextBox, PublicationLayoutError> {
    let ObjectKind::Text { frame, .. } = &text_box.kind else {
        return Err(PublicationLayoutError::ObjectKindMismatch);
    };
    let mut prepared = prepare_text_fonts(engine, text, style, story_formatting, font_resources);
    if prepared.has_unavailable_face {
        return Err(PublicationLayoutError::ExactFontUnavailable);
    }
    let has_visible_list_marker = story_formatting.paragraph_runs.iter().any(|run| {
        matches!(
            run.style.list_style,
            Some(ParagraphListStyle::Bullet | ParagraphListStyle::Numbered { .. })
        )
    });
    if text.trim().is_empty() && !has_visible_list_marker {
        return empty_fit_text_box(text_box, prepared, style);
    }
    let (story, paragraphs) = formatted_text_story(
        text,
        style,
        story_formatting,
        &prepared.base_font,
        &prepared.families,
    )?;
    let ResolvedFitTextLayout {
        layout: resolved,
        width,
        height,
    } = engine.resolve_fit_paragraphs(&story, &paragraphs)?;
    let mut resolved_fonts = resolved_font_previews(&resolved.fonts);
    let mut lines = text_line_previews(&story, &resolved.lines)?;
    append_list_marker_previews(
        &mut lines,
        &mut resolved_fonts,
        &resolved.list_markers,
        &story,
    )?;
    let [left, top, right, bottom] = if text_box.corner_rounding.is_default() {
        [0, 0, width.value(), height.value()]
    } else {
        rounded_text::fit_ink_bounds(&lines, width, height)
    };
    let content_size = fit_text_size(
        Millipoints::new(right - left),
        Millipoints::new(bottom - top),
    )?;
    let (size, effective_margins) = document_core::shape_geometry::fit_rounded_text_size(
        content_size,
        text_box.corner_rounding,
        frame.margins,
    )
    .map_err(|_| PublicationLayoutError::GeometryOutOfRange)?;
    offset_text_lines(
        &mut lines,
        micrometres_to_millipoints(i128::from(effective_margins.left.value()))? - left,
        micrometres_to_millipoints(i128::from(effective_margins.top.value()))? - top,
    )?;
    let interior = text_content_bounds(text_box, size);
    let region = TextRegion {
        bounds: physical_text_rect(interior)?,
        column_count: 1,
        column_gap: Millipoints::new(0),
        exclusions: Vec::new(),
    };
    let before_y = lines
        .iter()
        .map(|line| line.y.value())
        .min()
        .unwrap_or(region.bounds.y.value());
    vertically_align_lines(
        &mut lines,
        &region,
        table_vertical_alignment(frame.vertical_alignment),
    )?;
    let vertical_offset = Millipoints::new(
        lines
            .iter()
            .map(|line| line.y.value())
            .min()
            .unwrap_or(before_y)
            - before_y,
    );
    let permitted_fallback_fonts = used_permitted_fallback_previews(engine, &resolved_fonts);
    resolve_font_binding_facts(
        &mut prepared.base_binding,
        &mut prepared.bindings,
        &lines,
        &resolved_fonts,
        &permitted_fallback_fonts,
    );
    let font_binding = aggregate_font_binding(prepared.base_binding, &prepared.bindings);
    Ok(ResolvedFitTextBox {
        size,
        layout: ResolvedTextBoxPreview {
            object_id: text_box.id.clone(),
            sizing_mode: TextBoxSizingMode::Fit,
            composed_size: size,
            content_bounds: interior,
            vertical_offset,
            font_binding,
            font_bindings: prepared.bindings,
            permitted_fallback_fonts,
            resolved_fonts,
            lines,
            overset: false,
            overset_source: None,
            preview_state: TextBoxPreviewState::Complete,
        },
    })
}

fn fit_text_size(
    width: Millipoints,
    height: Millipoints,
) -> Result<PhysicalSize, PublicationLayoutError> {
    let width_um =
        millipoints_to_micrometres_ceil(width.value())?.max(TextFrame::MIN_FIT_WIDTH_MICROMETRES);
    let height_um =
        millipoints_to_micrometres_ceil(height.value())?.max(TextFrame::MIN_FIT_HEIGHT_MICROMETRES);
    PhysicalSize::new(width_um, height_um).map_err(|_| PublicationLayoutError::GeometryOutOfRange)
}

fn empty_fit_text_box(
    text_box: &PublicationObject,
    prepared: PreparedTextFonts,
    style: &TextStyle,
) -> Result<ResolvedFitTextBox, PublicationLayoutError> {
    let ObjectKind::Text { frame, .. } = &text_box.kind else {
        return Err(PublicationLayoutError::ObjectKindMismatch);
    };
    let minimum_content = PhysicalSize::new(
        TextFrame::MIN_FIT_WIDTH_MICROMETRES,
        TextFrame::MIN_FIT_HEIGHT_MICROMETRES,
    )
    .map_err(|_| PublicationLayoutError::GeometryOutOfRange)?;
    let (size, _) = document_core::shape_geometry::fit_rounded_text_size(
        minimum_content,
        text_box.corner_rounding,
        frame.margins,
    )
    .map_err(|_| PublicationLayoutError::GeometryOutOfRange)?;
    let resolved_fonts = style
        .font_face
        .as_ref()
        .map(face_preview)
        .into_iter()
        .collect();
    Ok(ResolvedFitTextBox {
        size,
        layout: ResolvedTextBoxPreview {
            object_id: text_box.id.clone(),
            sizing_mode: TextBoxSizingMode::Fit,
            composed_size: size,
            content_bounds: text_content_bounds(text_box, size),
            vertical_offset: Millipoints::new(0),
            font_binding: aggregate_font_binding(prepared.base_binding, &prepared.bindings),
            font_bindings: prepared.bindings,
            permitted_fallback_fonts: Vec::new(),
            resolved_fonts,
            lines: Vec::new(),
            overset: false,
            overset_source: None,
            preview_state: TextBoxPreviewState::Complete,
        },
    })
}

fn exact_font_family(
    engine: &mut TextLayoutEngine,
    style: &TextStyle,
    font_resources: &BTreeMap<(document_core::AssetHash, u32), &ExactFontResource>,
) -> Option<String> {
    let Some(binding) = &style.font_face else {
        return engine
            .system_fonts_enabled()
            .then(|| style.font_family.clone());
    };
    let resource = font_resources.get(&(binding.source.clone(), binding.face_index))?;
    engine.register_exact_font(resource).ok()
}

fn missing_font_text_box_for_story(
    engine: &mut TextLayoutEngine,
    text_box: &PublicationObject,
    text: &str,
    style: &TextStyle,
    formatting: &TextStoryFormatting,
    font_resources: &BTreeMap<(document_core::AssetHash, u32), &ExactFontResource>,
) -> ResolvedTextBoxPreview {
    let prepared = prepare_text_fonts(engine, text, style, formatting, font_resources);
    missing_text_box_from_prepared(text_box, prepared)
}

fn missing_text_box_from_prepared(
    text_box: &PublicationObject,
    prepared: PreparedTextFonts,
) -> ResolvedTextBoxPreview {
    ResolvedTextBoxPreview {
        object_id: text_box.id.clone(),
        sizing_mode: object_text_sizing_mode(text_box),
        composed_size: text_box.size,
        content_bounds: text_content_bounds(text_box, text_box.size),
        vertical_offset: Millipoints::new(0),
        font_binding: aggregate_font_binding(prepared.base_binding, &prepared.bindings),
        font_bindings: prepared.bindings,
        permitted_fallback_fonts: Vec::new(),
        resolved_fonts: Vec::new(),
        lines: Vec::new(),
        overset: false,
        overset_source: None,
        preview_state: TextBoxPreviewState::MissingFont,
    }
}

fn missing_font_text_box(
    text_box: &PublicationObject,
    style: &TextStyle,
) -> ResolvedTextBoxPreview {
    ResolvedTextBoxPreview {
        object_id: text_box.id.clone(),
        sizing_mode: object_text_sizing_mode(text_box),
        composed_size: text_box.size,
        content_bounds: text_content_bounds(text_box, text_box.size),
        vertical_offset: Millipoints::new(0),
        font_binding: font_binding_preview(style, &[], false, &[]),
        font_bindings: Vec::new(),
        permitted_fallback_fonts: Vec::new(),
        resolved_fonts: Vec::new(),
        lines: Vec::new(),
        overset: false,
        overset_source: None,
        preview_state: TextBoxPreviewState::MissingFont,
    }
}

fn budget_exceeded_text_box(
    text_box: &PublicationObject,
    style: &TextStyle,
) -> ResolvedTextBoxPreview {
    ResolvedTextBoxPreview {
        object_id: text_box.id.clone(),
        sizing_mode: object_text_sizing_mode(text_box),
        composed_size: text_box.size,
        content_bounds: text_content_bounds(text_box, text_box.size),
        vertical_offset: Millipoints::new(0),
        font_binding: font_binding_preview(style, &[], false, &[]),
        font_bindings: Vec::new(),
        permitted_fallback_fonts: Vec::new(),
        resolved_fonts: Vec::new(),
        lines: Vec::new(),
        overset: false,
        overset_source: None,
        preview_state: TextBoxPreviewState::BudgetExceeded,
    }
}

fn object_text_sizing_mode(object: &PublicationObject) -> TextBoxSizingMode {
    match &object.kind {
        ObjectKind::Text { frame, .. } => frame.sizing_mode,
        ObjectKind::Image { .. } | ObjectKind::Shape { .. } | ObjectKind::Table { .. } => {
            TextBoxSizingMode::Fixed
        }
    }
}

fn text_story(text: &str, style: &TextStyle, font_family: String) -> TextStory {
    TextStory {
        text: text.to_owned(),
        base_style: StoryStyle {
            id: 0,
            font_family,
            font_size: Millipoints::new(i64::from(style.font_size_millipoints)),
            font_weight: style.font_weight,
            italic: style.italic,
            color: style.color,
            underline: style.underline,
            line_height_per_thousand: style.line_height_per_thousand,
        },
        spans: Vec::new(),
    }
}

fn formatted_text_story(
    text: &str,
    style: &TextStyle,
    formatting: &TextStoryFormatting,
    base_font: &PreparedFontFamily,
    font_families: &BTreeMap<(String, u16, bool), PreparedFontFamily>,
) -> Result<(TextStory, Vec<StoryParagraphStyle>), PublicationLayoutError> {
    let story = formatted_inline_story(text, style, formatting, base_font, font_families);

    let paragraph_count = usize::try_from(
        paragraph_count(text).map_err(|_| PublicationLayoutError::GeometryOutOfRange)?,
    )
    .map_err(|_| PublicationLayoutError::GeometryOutOfRange)?;
    let base_paragraph = StoryParagraphStyle::new(
        horizontal_alignment(style.alignment),
        style.line_height_per_thousand,
    );
    let mut paragraphs = vec![base_paragraph; paragraph_count];
    for run in &formatting.paragraph_runs {
        let start = usize::try_from(run.range.start)
            .map_err(|_| PublicationLayoutError::GeometryOutOfRange)?;
        let end = usize::try_from(run.range.end)
            .map_err(|_| PublicationLayoutError::GeometryOutOfRange)?;
        let paragraph_slice = paragraphs
            .get_mut(start..end)
            .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
        for paragraph in paragraph_slice {
            if let Some(list_style) = run.style.list_style {
                paragraph.list_marker = match list_style {
                    ParagraphListStyle::None => None,
                    ParagraphListStyle::Bullet => Some(StoryListMarker::Bullet),
                    // The declared start is resolved to concrete sequential
                    // markers in the source-order pass below.
                    ParagraphListStyle::Numbered { start_at } => {
                        Some(StoryListMarker::Numbered(start_at))
                    }
                };
            }
            if let Some(alignment) = run.style.alignment {
                paragraph.alignment = horizontal_alignment(alignment);
            }
            if let Some(value) = run.style.start_indent_um {
                paragraph.start_indent =
                    Millipoints::new(micrometres_to_millipoints(i128::from(value))?);
            }
            if let Some(value) = run.style.end_indent_um {
                paragraph.end_indent =
                    Millipoints::new(micrometres_to_millipoints(i128::from(value))?);
            }
            if let Some(value) = run.style.first_line_indent_um {
                paragraph.first_line_indent =
                    Millipoints::new(micrometres_to_millipoints(i128::from(value))?);
            }
            if let Some(value) = run.style.line_height_per_thousand {
                paragraph.line_height_per_thousand = value;
            }
            if let Some(value) = run.style.space_before_millipoints {
                paragraph.space_before = Millipoints::new(i64::from(value));
            }
            if let Some(value) = run.style.space_after_millipoints {
                paragraph.space_after = Millipoints::new(i64::from(value));
            }
        }
    }
    let mut numbering: Option<(u32, u32)> = None;
    for paragraph in &mut paragraphs {
        match paragraph.list_marker {
            Some(StoryListMarker::Numbered(declared_start)) => {
                let number = match numbering {
                    Some((active_start, next)) if active_start == declared_start => next,
                    _ => declared_start,
                };
                paragraph.list_marker = Some(StoryListMarker::Numbered(number));
                numbering = Some((
                    declared_start,
                    number
                        .checked_add(1)
                        .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
                ));
            }
            Some(StoryListMarker::Bullet) | None => numbering = None,
        }
    }
    Ok((story, paragraphs))
}

fn formatted_inline_story(
    text: &str,
    style: &TextStyle,
    formatting: &TextStoryFormatting,
    base_font: &PreparedFontFamily,
    font_families: &BTreeMap<(String, u16, bool), PreparedFontFamily>,
) -> TextStory {
    let mut story = text_story(text, style, base_font.family.clone());
    story.base_style.font_weight = base_font.font_weight;
    story.base_style.italic = base_font.italic;
    story.spans = formatting
        .inline_spans
        .iter()
        .enumerate()
        .map(|(index, span)| {
            let mut span_style = story.base_style.clone();
            if let Some(size) = span.style.font_size_millipoints {
                span_style.font_size = Millipoints::new(i64::from(size));
            }
            let requested_weight = span.style.bold.map_or_else(
                || span.style.font_weight.unwrap_or(style.font_weight),
                |bold| if bold { 700 } else { 400 },
            );
            let requested_family = span
                .style
                .font_family
                .as_ref()
                .unwrap_or(&style.font_family);
            let requested_italic = span.style.italic.unwrap_or(style.italic);
            span_style.font_weight = requested_weight;
            span_style.italic = requested_italic;
            span_style.id =
                1 + u32::try_from(index).expect("story spans fit the canonical text budget");
            span_style.color = span.style.color.unwrap_or(style.color);
            span_style.underline = span.style.underline.unwrap_or(style.underline);
            if let Some(font) =
                font_families.get(&(requested_family.clone(), requested_weight, requested_italic))
            {
                span_style.font_family.clone_from(&font.family);
                span_style.font_weight = font.font_weight;
                span_style.italic = font.italic;
            } else {
                span_style.font_family.clone_from(requested_family);
            }
            StorySpan {
                source: SourceRange {
                    start: span.range.start,
                    end: span.range.end,
                },
                style: span_style,
            }
        })
        .collect();
    story
}

fn text_region(
    text_box: &PublicationObject,
    frame: &TextFrame,
    objects: &[PublicationObject],
) -> Result<TextRegion, PublicationLayoutError> {
    let bounds = physical_text_rect(text_content_bounds(text_box, text_box.size))?;
    let exclusions = objects
        .iter()
        .filter(|candidate| supports_rectangular_wrap(candidate))
        .map(|candidate| {
            let mut exclusion = exclusion_rect(candidate, text_box.transform)?;
            let right = bounds.x.value() + bounds.width.value();
            let exclusion_right = exclusion.x.value() + exclusion.width.value();
            if exclusion.x.value() < right && exclusion_right > bounds.x.value() {
                match candidate.wrap_settings.side {
                    WrapSide::Largest => {}
                    WrapSide::Left => {
                        exclusion.width =
                            Millipoints::new(right.max(exclusion_right) - exclusion.x.value());
                    }
                    WrapSide::Right => {
                        let left = bounds.x.value().min(exclusion.x.value());
                        exclusion.x = Millipoints::new(left);
                        exclusion.width = Millipoints::new(exclusion_right - left);
                    }
                }
            }
            Ok::<_, PublicationLayoutError>(exclusion)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TextRegion {
        bounds,
        column_count: frame.column_count,
        column_gap: Millipoints::new(micrometres_to_millipoints(i128::from(frame.column_gap_um))?),
        exclusions,
    })
}

fn text_content_bounds(object: &PublicationObject, size: PhysicalSize) -> PhysicalBounds {
    let margins = match &object.kind {
        ObjectKind::Text { frame, .. } => frame.margins,
        _ => document_core::model::Insets::default(),
    };
    let margins =
        document_core::shape_geometry::rounded_text_margins(size, object.corner_rounding, margins)
            .unwrap_or(margins);
    PhysicalBounds {
        x: margins.left,
        y: margins.top,
        width: document_core::model::Micrometres::new(
            size.width.value() - margins.left.value() - margins.right.value(),
        ),
        height: document_core::model::Micrometres::new(
            size.height.value() - margins.top.value() - margins.bottom.value(),
        ),
    }
}

fn physical_text_rect(bounds: PhysicalBounds) -> Result<TextRect, PublicationLayoutError> {
    TextRect::new(
        micrometres_to_millipoints(i128::from(bounds.x.value()))?,
        micrometres_to_millipoints(i128::from(bounds.y.value()))?,
        micrometres_to_millipoints(i128::from(bounds.width.value()))?,
        micrometres_to_millipoints(i128::from(bounds.height.value()))?,
    )
    .map_err(Into::into)
}

const fn table_vertical_alignment(value: TextVerticalAlignment) -> TableVerticalAlignment {
    match value {
        TextVerticalAlignment::Top => TableVerticalAlignment::Top,
        TextVerticalAlignment::Middle => TableVerticalAlignment::Middle,
        TextVerticalAlignment::Bottom => TableVerticalAlignment::Bottom,
    }
}

fn offset_text_lines(
    lines: &mut [ResolvedTextLinePreview],
    x: i64,
    y: i64,
) -> Result<(), PublicationLayoutError> {
    let shifted = |value: Millipoints, offset| {
        value
            .value()
            .checked_add(offset)
            .map(Millipoints::new)
            .ok_or(PublicationLayoutError::GeometryOutOfRange)
    };
    for line in lines {
        line.x = shifted(line.x, x)?;
        line.y = shifted(line.y, y)?;
        line.baseline = shifted(line.baseline, y)?;
        for run in &mut line.glyph_runs {
            if let Some(underline) = &mut run.underline {
                underline.x = shifted(underline.x, x)?;
                underline.y = shifted(underline.y, y)?;
            }
            for glyph in &mut run.glyphs {
                glyph.x = shifted(glyph.x, x)?;
                glyph.y = shifted(glyph.y, y)?;
            }
        }
    }
    Ok(())
}

/// Search only physical vertical starts, using the existing native compositor.
/// Every accepted candidate keeps the same obstacles and exact UTF-8 story.
fn resolve_text_box_paragraphs(
    engine: &mut TextLayoutEngine,
    story: &TextStory,
    region: &TextRegion,
    paragraphs: &[StoryParagraphStyle],
    alignment: TextVerticalAlignment,
) -> Result<(ResolvedTextLayout, Millipoints), PublicationLayoutError> {
    let initial = engine.resolve_paragraphs(story, region, paragraphs)?;
    if alignment == TextVerticalAlignment::Top
        || initial.overset.is_some()
        || initial.lines.is_empty()
    {
        return Ok((initial, Millipoints::new(0)));
    }
    let resolve_at = |engine: &mut TextLayoutEngine, offset| {
        let mut candidate = region.clone();
        candidate.bounds.y = Millipoints::new(region.bounds.y.value() + offset);
        candidate.bounds.height = Millipoints::new(region.bounds.height.value() - offset);
        engine
            .resolve_paragraphs(story, &candidate, paragraphs)
            .map_err(PublicationLayoutError::from)
    };
    if region.exclusions.is_empty() {
        let bottom = initial
            .lines
            .iter()
            .map(|line| line.y.value() + line.height.value())
            .max()
            .unwrap_or(region.bounds.y.value());
        let free = (region.bounds.y.value() + region.bounds.height.value() - bottom).max(0);
        let offset = if alignment == TextVerticalAlignment::Middle {
            free / 2
        } else {
            free
        };
        let resolved = resolve_at(engine, offset)?;
        if resolved.overset.is_some() {
            return Err(PublicationLayoutError::VerticalAlignmentDidNotSettle);
        }
        return Ok((resolved, Millipoints::new(offset)));
    }
    let mut lower = 0;
    let mut upper = region.bounds.height.value();
    let mut best = initial;
    for _ in 0..32 {
        if upper - lower <= 1 {
            break;
        }
        let midpoint = lower + (upper - lower) / 2;
        let candidate = resolve_at(engine, midpoint)?;
        if candidate.overset.is_none() {
            lower = midpoint;
            best = candidate;
        } else {
            upper = midpoint;
        }
    }
    if upper - lower > 1 {
        return Err(PublicationLayoutError::VerticalAlignmentDidNotSettle);
    }
    let offset = if alignment == TextVerticalAlignment::Middle {
        lower / 2
    } else {
        lower
    };
    let resolved = if offset == lower {
        best
    } else {
        resolve_at(engine, offset)?
    };
    if resolved.overset.is_some() || layout_overlaps_exclusions(&resolved, &region.exclusions) {
        return Err(PublicationLayoutError::VerticalAlignmentDidNotSettle);
    }
    Ok((resolved, Millipoints::new(offset)))
}

fn layout_overlaps_exclusions(layout: &ResolvedTextLayout, exclusions: &[TextRect]) -> bool {
    layout.lines.iter().any(|line| {
        exclusions.iter().any(|exclusion| {
            line.y.value() < exclusion.y.value() + exclusion.height.value()
                && line.y.value() + line.height.value() > exclusion.y.value()
                && line.fragments.iter().any(|fragment| {
                    fragment.x.value() < exclusion.x.value() + exclusion.width.value()
                        && fragment.x.value() + fragment.width.value() > exclusion.x.value()
                })
        })
    })
}

const fn horizontal_alignment(alignment: TextAlignment) -> HorizontalAlignment {
    match alignment {
        TextAlignment::Left => HorizontalAlignment::Left,
        TextAlignment::Center => HorizontalAlignment::Center,
        TextAlignment::Right => HorizontalAlignment::Right,
        TextAlignment::Justify => HorizontalAlignment::Justify,
    }
}

fn resolved_font_previews(fonts: &[ResolvedFont]) -> Vec<ResolvedFontFacePreview> {
    fonts
        .iter()
        .map(|font| ResolvedFontFacePreview {
            sha256_hex: encode_sha256(font.sha256),
            collection_index: font.collection_index,
        })
        .collect()
}

fn font_binding_preview(
    style: &TextStyle,
    resolved_fonts: &[ResolvedFontFacePreview],
    binding_available: bool,
    permitted_fallback_fonts: &[ResolvedFontFacePreview],
) -> RequestedFontBindingPreview {
    let exact_face = style.font_face.as_ref().map(face_preview);
    let substitution_status = match &exact_face {
        None => FontSubstitutionStatus::Unbound,
        Some(_) if !binding_available => FontSubstitutionStatus::Missing,
        Some(bound)
            if resolved_fonts.iter().all(|resolved| {
                resolved == bound || permitted_fallback_fonts.contains(resolved)
            }) =>
        {
            FontSubstitutionStatus::Exact
        }
        Some(_) => FontSubstitutionStatus::Substituted,
    };
    RequestedFontBindingPreview {
        requested_family: style.font_family.clone(),
        exact_face,
        substitution_status,
    }
}

fn resolved_run_matches_face(
    run: &ResolvedGlyphRunPreview,
    resolved_fonts: &[ResolvedFontFacePreview],
    expected: &ResolvedFontFacePreview,
    permitted_fallback_fonts: &[ResolvedFontFacePreview],
) -> bool {
    // Synthesis is a separate print refusal; it does not change font identity.
    resolved_fonts
        .get(run.font_index)
        .is_some_and(|actual| actual == expected || permitted_fallback_fonts.contains(actual))
}

fn resolve_font_binding_facts(
    base_binding: &mut RequestedFontBindingPreview,
    bindings: &mut [RequestedFontRangeBindingPreview],
    lines: &[ResolvedTextLinePreview],
    resolved_fonts: &[ResolvedFontFacePreview],
    permitted_fallback_fonts: &[ResolvedFontFacePreview],
) {
    for run in lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .filter(|run| run.list_marker_paragraph.is_none() && run.source.start < run.source.end)
    {
        let Some(index) = bindings
            .partition_point(|binding| binding.source.start <= run.source.start)
            .checked_sub(1)
        else {
            continue;
        };
        let binding = &mut bindings[index];
        if run.source.end <= binding.source.end
            && binding.substitution_status == FontSubstitutionStatus::Exact
        {
            if let Some(expected) = binding.exact_face.as_ref() {
                if !resolved_run_matches_face(
                    run,
                    resolved_fonts,
                    expected,
                    permitted_fallback_fonts,
                ) {
                    binding.substitution_status = FontSubstitutionStatus::Substituted;
                }
            } else {
                binding.substitution_status = FontSubstitutionStatus::Unbound;
            }
        }
    }

    if base_binding.substitution_status == FontSubstitutionStatus::Exact
        && let Some(expected) = base_binding.exact_face.as_ref()
        && lines
            .iter()
            .flat_map(|line| &line.glyph_runs)
            .filter(|run| run.list_marker_paragraph.is_some())
            .any(|run| {
                !resolved_run_matches_face(run, resolved_fonts, expected, permitted_fallback_fonts)
            })
    {
        base_binding.substitution_status = FontSubstitutionStatus::Substituted;
    }
}

const fn font_status_rank(status: FontSubstitutionStatus) -> u8 {
    match status {
        FontSubstitutionStatus::Exact => 0,
        FontSubstitutionStatus::Unbound => 1,
        FontSubstitutionStatus::Substituted => 2,
        FontSubstitutionStatus::Missing => 3,
    }
}

fn aggregate_font_binding(
    mut base: RequestedFontBindingPreview,
    bindings: &[RequestedFontRangeBindingPreview],
) -> RequestedFontBindingPreview {
    if let Some(worst) = bindings
        .iter()
        .map(|binding| binding.substitution_status)
        .max_by_key(|status| font_status_rank(*status))
        && font_status_rank(worst) > font_status_rank(base.substitution_status)
    {
        base.substitution_status = worst;
    }
    base
}

fn text_line_previews(
    story: &TextStory,
    lines: &[ResolvedTextLine],
) -> Result<Vec<ResolvedTextLinePreview>, PublicationLayoutError> {
    let mut previews = Vec::new();
    for line in lines {
        for fragment in &line.fragments {
            let source = fragment.source.clone();
            previews.push(ResolvedTextLinePreview {
                text: source_text(&story.text, &source)?
                    .trim_end_matches(['\r', '\n'])
                    .to_owned(),
                glyph_runs: fragment
                    .glyph_runs
                    .iter()
                    .map(|run| {
                        let style = story
                            .spans
                            .iter()
                            .find(|span| span.style.id == run.style_id)
                            .map_or(&story.base_style, |span| &span.style);
                        glyph_run_preview(&story.text, run, None, style, line.baseline)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                source,
                column_index: line.column_index,
                x: fragment.x,
                y: line.y,
                width: fragment.width,
                height: line.height,
                baseline: line.baseline,
            });
        }
    }
    Ok(previews)
}

fn append_list_marker_previews(
    lines: &mut [ResolvedTextLinePreview],
    resolved_fonts: &mut Vec<ResolvedFontFacePreview>,
    markers: &[crate::text::ResolvedListMarker],
    story: &TextStory,
) -> Result<(), PublicationLayoutError> {
    for marker in markers {
        let local_faces = resolved_font_previews(&marker.layout.fonts);
        let font_indices = local_faces
            .into_iter()
            .map(|face| {
                resolved_fonts
                    .iter()
                    .position(|candidate| candidate == &face)
                    .unwrap_or_else(|| {
                        let index = resolved_fonts.len();
                        resolved_fonts.push(face);
                        index
                    })
            })
            .collect::<Vec<_>>();
        let marker_line = marker
            .layout
            .lines
            .first()
            .ok_or(PublicationLayoutError::InvalidResolvedSource)?;
        let marker_baseline = marker_line.baseline;
        let target = lines
            .iter_mut()
            .find(|line| {
                line.baseline == marker_baseline
                    && line.source.start <= marker.source_anchor
                    && marker.source_anchor <= line.source.end
            })
            .ok_or(PublicationLayoutError::InvalidResolvedSource)?;
        let mut marker_runs = marker_line
            .fragments
            .iter()
            .flat_map(|fragment| &fragment.glyph_runs)
            .map(|run| {
                let style = story
                    .spans
                    .iter()
                    .find(|span| {
                        span.source.start <= marker.source_anchor
                            && marker.source_anchor < span.source.end
                    })
                    .map_or(&story.base_style, |span| &span.style);
                let mut preview = glyph_run_preview(
                    &marker.text,
                    run,
                    Some(marker.paragraph_index),
                    style,
                    marker_baseline,
                )?;
                preview.underline = None;
                preview.font_index = *font_indices
                    .get(preview.font_index)
                    .ok_or(PublicationLayoutError::InvalidResolvedSource)?;
                Ok(preview)
            })
            .collect::<Result<Vec<_>, PublicationLayoutError>>()?;
        marker_runs.append(&mut target.glyph_runs);
        target.glyph_runs = marker_runs;
    }
    Ok(())
}

fn glyph_run_preview(
    story: &str,
    run: &ResolvedGlyphRun,
    list_marker_paragraph: Option<u32>,
    style: &StoryStyle,
    baseline: Millipoints,
) -> Result<ResolvedGlyphRunPreview, PublicationLayoutError> {
    let underline = if style.underline {
        let start = run.glyphs.iter().map(|glyph| glyph.x.value()).min();
        let end = run
            .glyphs
            .iter()
            .map(|glyph| {
                glyph
                    .x
                    .value()
                    .checked_add(glyph.advance.value())
                    .ok_or(PublicationLayoutError::GeometryOutOfRange)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max();
        match (start, end) {
            (Some(start), Some(end)) if end > start => Some(ResolvedTextUnderlinePreview {
                x: Millipoints::new(start),
                y: Millipoints::new(
                    baseline
                        .value()
                        .checked_add(run.font_size.value() / 10)
                        .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
                ),
                width: Millipoints::new(
                    end.checked_sub(start)
                        .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
                ),
                thickness: Millipoints::new((run.font_size.value() / 16).max(250)),
            }),
            _ => None,
        }
    } else {
        None
    };
    Ok(ResolvedGlyphRunPreview {
        source: run.source.clone(),
        text: source_text(story, &run.source)?.to_owned(),
        glyph_text: source_text(story, &run.glyph_source)?.to_owned(),
        font_index: run.font_index,
        font_size: run.font_size,
        color: style.color,
        underline,
        requires_font_synthesis: run.requires_font_synthesis,
        list_marker_paragraph,
        right_to_left: run.right_to_left,
        glyphs: run
            .glyphs
            .iter()
            .map(|glyph| ResolvedGlyphPreview {
                id: glyph.id,
                x: glyph.x,
                y: glyph.y,
                advance: glyph.advance,
                outline: glyph.outline.clone(),
                fallback: glyph.fallback,
            })
            .collect(),
    })
}

fn source_text<'a>(
    story: &'a str,
    source: &SourceRange,
) -> Result<&'a str, PublicationLayoutError> {
    story
        .get(source.start..source.end)
        .ok_or(PublicationLayoutError::InvalidResolvedSource)
}

fn supports_rectangular_wrap(object: &PublicationObject) -> bool {
    matches!(
        &object.kind,
        ObjectKind::Image {
            wrap_mode: WrapMode::Rectangular,
            ..
        } | ObjectKind::Shape {
            wrap_mode: WrapMode::Rectangular,
            ..
        }
    )
}

fn exclusion_rect(
    object: &PublicationObject,
    text_transform: document_core::model::Transform,
) -> Result<TextRect, PublicationLayoutError> {
    let mut page_bounds = crate::geometry::transformed_bounds(object.size, object.transform)
        .map_err(|_| PublicationLayoutError::GeometryOutOfRange)?;
    let distance = object.wrap_settings.distance_um;
    page_bounds.x = document_core::model::Micrometres::new(
        page_bounds
            .x
            .value()
            .checked_sub(distance)
            .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
    );
    page_bounds.y = document_core::model::Micrometres::new(
        page_bounds
            .y
            .value()
            .checked_sub(distance)
            .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
    );
    page_bounds.width = document_core::model::Micrometres::new(
        page_bounds
            .width
            .value()
            .checked_add(distance * 2)
            .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
    );
    page_bounds.height = document_core::model::Micrometres::new(
        page_bounds
            .height
            .value()
            .checked_add(distance * 2)
            .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
    );
    let bounds = crate::geometry::inverse_transformed_bounds(page_bounds, text_transform)
        .map_err(|_| PublicationLayoutError::GeometryOutOfRange)?;
    let right_um = bounds
        .x
        .value()
        .checked_add(bounds.width.value())
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    let bottom_um = bounds
        .y
        .value()
        .checked_add(bounds.height.value())
        .ok_or(PublicationLayoutError::GeometryOutOfRange)?;
    let left = micrometres_to_millipoints_floor(i128::from(bounds.x.value()))?;
    let top = micrometres_to_millipoints_floor(i128::from(bounds.y.value()))?;
    let right = micrometres_to_millipoints_ceil(i128::from(right_um))?;
    let bottom = micrometres_to_millipoints_ceil(i128::from(bottom_um))?;
    TextRect::new(
        left,
        top,
        right
            .checked_sub(left)
            .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
        bottom
            .checked_sub(top)
            .ok_or(PublicationLayoutError::GeometryOutOfRange)?,
    )
    .map_err(Into::into)
}

fn encode_sha256(bytes: [u8; 32]) -> String {
    use core::fmt::Write;

    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn micrometres_to_millipoints(value: i128) -> Result<i64, PublicationLayoutError> {
    value
        .checked_mul(360)
        .and_then(|scaled| scaled.checked_div(127))
        .and_then(|converted| i64::try_from(converted).ok())
        .ok_or(PublicationLayoutError::GeometryOutOfRange)
}

fn micrometres_to_millipoints_floor(value: i128) -> Result<i64, PublicationLayoutError> {
    value
        .checked_mul(360)
        .map(|scaled| scaled.div_euclid(127))
        .and_then(|converted| i64::try_from(converted).ok())
        .ok_or(PublicationLayoutError::GeometryOutOfRange)
}

fn micrometres_to_millipoints_ceil(value: i128) -> Result<i64, PublicationLayoutError> {
    value
        .checked_mul(360)
        .map(|scaled| -(-scaled).div_euclid(127))
        .and_then(|converted| i64::try_from(converted).ok())
        .ok_or(PublicationLayoutError::GeometryOutOfRange)
}

fn millipoints_to_micrometres_ceil(value: i64) -> Result<i64, PublicationLayoutError> {
    i128::from(value)
        .checked_mul(127)
        .map(|scaled| -(-scaled).div_euclid(360))
        .and_then(|converted| i64::try_from(converted).ok())
        .ok_or(PublicationLayoutError::GeometryOutOfRange)
}

/// Failures while resolving revision-bound publication preview geometry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicationLayoutError {
    InvalidOutputSource,
    OutputPageBudgetExceeded(PageId),
    GeometryOutOfRange,
    InvalidResolvedSource,
    ObjectNotFound,
    PageNotFound,
    ObjectKindMismatch,
    ExactFontUnavailable,
    ProjectIdentityUnavailable,
    TableCellGeometryTooSmall,
    VerticalAlignmentDidNotSettle,
    Text(TextLayoutError),
}

impl From<TextLayoutError> for PublicationLayoutError {
    fn from(error: TextLayoutError) -> Self {
        Self::Text(error)
    }
}

impl core::fmt::Display for PublicationLayoutError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "publication layout failed: {self:?}")
    }
}

impl std::error::Error for PublicationLayoutError {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use document_core::model::{
        Color, FontFaceBinding, InlineTextSpan, InlineTextStyle, ObjectEffects, ObjectKind,
        PhysicalSize, PublicationObject, ShapeKind, ShapeStroke, TableData, TextSourceRange,
        TextStoryFormatting, TextStyle, Transform, WrapMode,
    };
    use document_core::{AssetHash, ObjectId};

    use super::{
        ExactFontResource, FontSubstitutionStatus, MAX_PREVIEW_TEXT_BOXES, PreparedFontFamily,
        PreviewBudget, ResolvedFontFacePreview, TextLayoutEngine, exclusion_rect,
        font_binding_preview, formatted_inline_story, micrometres_to_millipoints_ceil,
        micrometres_to_millipoints_floor, prepare_text_fonts, reserve_empty_table_preview_slot,
    };

    #[test]
    fn unbound_inline_variant_keeps_the_authored_family_beside_an_exact_base() {
        let bytes = include_bytes!("../tests/fixtures/DejaVuSans.ttf").to_vec();
        let source = AssetHash::for_bytes(&bytes);
        let resource = ExactFontResource {
            source: source.clone(),
            face_index: 0,
            bytes,
        };
        let style = TextStyle {
            font_face: Some(FontFaceBinding {
                source: source.clone(),
                face_index: 0,
            }),
            font_family: "Authored family".to_owned(),
            ..TextStyle::default()
        };
        let formatting = TextStoryFormatting {
            inline_spans: vec![InlineTextSpan {
                range: TextSourceRange { start: 0, end: 6 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: None,
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            }],
            paragraph_runs: Vec::new(),
        };
        let resources = BTreeMap::from([((source, 0), &resource)]);
        let mut engine = TextLayoutEngine::new();
        let prepared = prepare_text_fonts(&mut engine, "Italic", &style, &formatting, &resources);

        let story = formatted_inline_story(
            "Italic",
            &style,
            &formatting,
            &prepared.base_font,
            &prepared.families,
        );

        assert_ne!(story.base_style.font_family, style.font_family);
        assert_eq!(story.spans[0].style.font_family, style.font_family);
    }

    #[test]
    fn inline_lookup_uses_authored_weight_before_applying_the_concrete_face() {
        let style = TextStyle {
            font_family: "Authored family".to_owned(),
            font_weight: 650,
            ..TextStyle::default()
        };
        let base_font = PreparedFontFamily {
            family: "Publisher V2 Bound nearest-regular".to_owned(),
            font_weight: 700,
            italic: false,
        };
        let italic_font = PreparedFontFamily {
            family: "Publisher V2 Bound nearest-italic".to_owned(),
            font_weight: 700,
            italic: true,
        };
        let font_families = BTreeMap::from([
            ((style.font_family.clone(), 650, false), base_font.clone()),
            ((style.font_family.clone(), 650, true), italic_font.clone()),
        ]);
        let formatting = TextStoryFormatting {
            inline_spans: vec![InlineTextSpan {
                range: TextSourceRange { start: 0, end: 6 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: None,
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            }],
            paragraph_runs: Vec::new(),
        };

        let story =
            formatted_inline_story("Italic", &style, &formatting, &base_font, &font_families);
        let inline_style = &story.spans[0].style;

        assert_eq!(inline_style.font_family, italic_font.family);
        assert_eq!(inline_style.font_weight, 700);
        assert!(inline_style.italic);
    }

    #[test]
    fn selected_font_size_reaches_layout_without_changing_body_or_face() {
        let style = TextStyle::default();
        let base_font = PreparedFontFamily {
            family: "Embedded regular".to_owned(),
            font_weight: 400,
            italic: false,
        };
        let formatting = TextStoryFormatting {
            inline_spans: vec![InlineTextSpan {
                range: TextSourceRange { start: 0, end: 4 },
                style: InlineTextStyle {
                    font_size_millipoints: Some(20_000),
                    ..InlineTextStyle::default()
                },
            }],
            paragraph_runs: Vec::new(),
        };
        let families =
            BTreeMap::from([((style.font_family.clone(), 400, false), base_font.clone())]);
        let story =
            formatted_inline_story("Head\nBody", &style, &formatting, &base_font, &families);
        assert_eq!(story.base_style.font_size.value(), 12_000);
        assert_eq!(story.spans[0].style.font_size.value(), 20_000);
        assert_eq!(story.spans[0].style.font_family, base_font.family);
        assert_eq!(story.spans[0].source.end, 4);
    }

    #[test]
    fn empty_tables_consume_the_same_preview_budget_slot_in_both_passes() {
        let table = TableData::empty(1, 1).unwrap();
        let mut resource_prepass = PreviewBudget::default();
        let mut layout_pass = PreviewBudget::default();
        for _ in 0..MAX_PREVIEW_TEXT_BOXES {
            assert_eq!(
                reserve_empty_table_preview_slot(&table, &mut resource_prepass),
                reserve_empty_table_preview_slot(&table, &mut layout_pass)
            );
        }
        assert_eq!(resource_prepass.reserve(1), layout_pass.reserve(1));
        assert!(resource_prepass.truncated);
        assert!(layout_pass.truncated);
    }

    #[test]
    fn exact_font_status_requires_every_resolved_run_to_use_the_bound_face() {
        let source = AssetHash::for_bytes(b"bound font");
        let style = TextStyle {
            font_face: Some(FontFaceBinding {
                source: source.clone(),
                face_index: 2,
            }),
            ..TextStyle::default()
        };
        let bound = ResolvedFontFacePreview {
            sha256_hex: source.as_str().to_owned(),
            collection_index: 2,
        };
        let fallback = ResolvedFontFacePreview {
            sha256_hex: AssetHash::for_bytes(b"fallback font").as_str().to_owned(),
            collection_index: 0,
        };

        assert_eq!(
            font_binding_preview(&style, &[], true, &[]).substitution_status,
            FontSubstitutionStatus::Exact,
            "an empty story can still have a validated exact binding"
        );
        assert_eq!(
            font_binding_preview(&style, core::slice::from_ref(&bound), true, &[])
                .substitution_status,
            FontSubstitutionStatus::Exact
        );
        assert_eq!(
            font_binding_preview(&style, &[bound.clone(), fallback.clone()], true, &[])
                .substitution_status,
            FontSubstitutionStatus::Substituted
        );
        assert_eq!(
            font_binding_preview(
                &style,
                &[bound, fallback.clone()],
                true,
                core::slice::from_ref(&fallback),
            )
            .substitution_status,
            FontSubstitutionStatus::Exact
        );
        assert_eq!(
            font_binding_preview(&style, &[], false, &[]).substitution_status,
            FontSubstitutionStatus::Missing
        );
    }

    #[test]
    fn rotated_wrap_uses_transformed_bounds_relative_to_text_origin() {
        let mut transform = Transform::identity_at(50_000, 60_000);
        transform.rotation_microdegrees = 90_000_000;
        let object = PublicationObject {
            id: ObjectId::parse("rotated-wrap").unwrap(),
            group_id: None,
            locked: false,
            name: "Rotated Picture".to_owned(),
            size: PhysicalSize::new(30_000, 20_000).unwrap(),
            transform,
            opacity_millionths: 1_000_000,
        effects: ObjectEffects::default(),
            wrap_settings: document_core::model::WrapSettings::default(),
        corner_rounding: document_core::model::CornerRounding::default(),
        text_field: None,
            kind: ObjectKind::Shape {
                shape: ShapeKind::Rectangle,
                fill: Color {
                    red: 0,
                    green: 0,
                    blue: 0,
                    alpha: 255,
                },
                stroke: ShapeStroke::default(),
                wrap_mode: WrapMode::Rectangular,
            },
        };

        let exclusion = exclusion_rect(&object, Transform::identity_at(10_000, 10_000)).unwrap();

        let expected_left = micrometres_to_millipoints_floor(20_000).unwrap();
        let expected_top = micrometres_to_millipoints_floor(50_000).unwrap();
        let expected_right = micrometres_to_millipoints_ceil(40_000).unwrap();
        let expected_bottom = micrometres_to_millipoints_ceil(80_000).unwrap();
        let actual_right = exclusion.x.value() + exclusion.width.value();
        let actual_bottom = exclusion.y.value() + exclusion.height.value();

        assert!((expected_left - 3..=expected_left).contains(&exclusion.x.value()));
        assert!((expected_top - 3..=expected_top).contains(&exclusion.y.value()));
        assert!((expected_right..=expected_right + 3).contains(&actual_right));
        assert!((expected_bottom..=expected_bottom + 3).contains(&actual_bottom));
    }

    #[test]
    fn rotated_text_box_inverse_maps_page_exclusion_before_wrapping() {
        let object = PublicationObject {
            id: ObjectId::parse("rotated-text-wrap").unwrap(),
            group_id: None,
            locked: false,
            name: "Picture".to_owned(),
            size: PhysicalSize::new(20_000, 30_000).unwrap(),
            transform: Transform::identity_at(-30_000, 20_000),
            opacity_millionths: 1_000_000,
        effects: ObjectEffects::default(),
            wrap_settings: document_core::model::WrapSettings::default(),
        corner_rounding: document_core::model::CornerRounding::default(),
        text_field: None,
            kind: ObjectKind::Shape {
                shape: ShapeKind::Rectangle,
                fill: Color {
                    red: 0,
                    green: 0,
                    blue: 0,
                    alpha: 255,
                },
                stroke: ShapeStroke::default(),
                wrap_mode: WrapMode::Rectangular,
            },
        };
        let text_transform = Transform {
            rotation_microdegrees: 90_000_000,
            ..Transform::identity_at(10_000, 10_000)
        };

        let exclusion = exclusion_rect(&object, text_transform).unwrap();

        let expected_left = micrometres_to_millipoints_floor(10_000).unwrap();
        let expected_top = micrometres_to_millipoints_floor(20_000).unwrap();
        let expected_right = micrometres_to_millipoints_ceil(40_000).unwrap();
        let expected_bottom = micrometres_to_millipoints_ceil(40_000).unwrap();
        let actual_right = exclusion.x.value() + exclusion.width.value();
        let actual_bottom = exclusion.y.value() + exclusion.height.value();

        // The inverse transform and physical-unit conversion both round
        // outward. Assert the meaningful contract: the wrap exclusion fully
        // encloses the mathematical result with only a tiny rounding margin.
        assert!((expected_left - 3..=expected_left).contains(&exclusion.x.value()));
        assert!((expected_top - 3..=expected_top).contains(&exclusion.y.value()));
        assert!((expected_right..=expected_right + 3).contains(&actual_right));
        assert!((expected_bottom..=expected_bottom + 3).contains(&actual_bottom));
    }
}
