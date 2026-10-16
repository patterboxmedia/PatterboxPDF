//! Opt-in lowering from immutable Publisher scenes into the finished-page API.
//! Layout and resource preparation remain outside the neutral writer. Existing
//! scene attestations are checked before their exact shared resources are used.

#![expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "legacy fixed-point coordinates are bounded before conversion to PDF f32 points"
)]

use crate::finished::{
    AffineTransform, FillRule, FinishedContent, FinishedDocument, FinishedGroup, FinishedItem,
    FinishedPage, FinishedPageBoxes, FinishedPath, FinishedPathPaint, FinishedPdfError,
    FinishedResources, ImagePlacement, LineCap, LineJoin, Paint, PathCommand, PdfBox, Point,
    PositionedGlyph, PositionedGlyphRun, Stroke,
};
use crate::scene::{
    FontContractMode, hex_sha256, micrometres_to_points, millipoint_f64_to_points,
    millipoints_to_points, object_matrix, pdf_rect, publication_has_outlines,
    publication_transparency_evidence, trusted_font_map, trusted_raster_map, validate_publication,
};
use crate::{
    ProductionColorPalette, ProductionFont, ProductionRaster, ScenePdfOptions,
    SceneSerializationError, SceneSerializationEvidence, SerializedPageEvidence,
    SerializedRasterPlacementEvidence, SerializedTableCellAddress,
};
use document_core::{
    AssetHash,
    model::{Color, ShapeKind, ShapeStroke},
};
use layout_core::{
    ResolvedGlyphOutline, ResolvedOutlineCommand, ResolvedPublication, ResolvedSceneObject,
    ResolvedSceneObjectKind, ResolvedTablePreview, ResolvedTextBoxPreview,
};
use std::collections::BTreeMap;

/// Compatibility is explicit. The default library never imports authoring types.
pub struct PublisherSceneAdapter;
impl PublisherSceneAdapter {
    /// Converts ordered scenes and exact resources without relayout or reshaping.
    /// Borrowed resource views share their original admitted-memory leases.
    /// # Errors
    /// Rejects incomplete layout, invalid geometry and drifted resource evidence.
    pub fn lower(
        publication: &ResolvedPublication,
        options: ScenePdfOptions,
        rasters: &[ProductionRaster],
        fonts: Option<&[ProductionFont]>,
        colors: Option<&ProductionColorPalette>,
    ) -> Result<(FinishedDocument, SceneSerializationEvidence), SceneSerializationError> {
        lower(
            publication,
            options,
            rasters,
            fonts,
            colors,
            FontContractMode::Strict,
        )
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "keeps the ordered lowering and its exact scene receipts in one traversal"
)]
pub(crate) fn lower(
    publication: &ResolvedPublication,
    options: ScenePdfOptions,
    rasters: &[ProductionRaster],
    fonts: Option<&[ProductionFont]>,
    colors: Option<&ProductionColorPalette>,
    font_contract: FontContractMode,
) -> Result<(FinishedDocument, SceneSerializationEvidence), SceneSerializationError> {
    let rasters = trusted_raster_map(publication, rasters)?;
    let fonts = fonts
        .map(|values| trusted_font_map(publication, values))
        .transpose()?;
    if colors.is_some_and(|palette| !palette.matches_publication(publication)) {
        return Err(SceneSerializationError::MissingPreparedColor);
    }
    validate_publication(publication, options, &rasters, font_contract)?;
    let resources = FinishedResources {
        fonts: fonts
            .as_ref()
            .into_iter()
            .flat_map(|f| f.values())
            .map(|font| font.finished_resource(font_id(font)).map_err(map_error))
            .collect::<Result<_, _>>()?,
        images: rasters
            .values()
            .map(|raster| {
                raster
                    .finished_resource(image_id(&raster.evidence().asset))
                    .map_err(map_error)
            })
            .collect::<Result<_, _>>()?,
    };
    let paints = PaintResources {
        rasters: &rasters,
        fonts: fonts.as_ref(),
        colors,
    };
    let mut pages = Vec::new();
    let mut page_evidence = Vec::new();
    for page in publication.pages() {
        let media = pdf_rect(page.bleed_box, page.trim_box.height.value())?;
        let trim = pdf_rect(page.trim_box, page.trim_box.height.value())?;
        let boxes = FinishedPageBoxes {
            media: PdfBox::new(media.x1, media.y1, media.x2, media.y2),
            bleed: PdfBox::new(media.x1, media.y1, media.x2, media.y2),
            trim: PdfBox::new(trim.x1, trim.y1, trim.x2, trim.y2),
        };
        let height = micrometres_to_points(page.trim_box.height.value())?;
        let extent = pdf_rect(page.bleed_box, 0)?;
        let scene_bounds = PdfBox::new(extent.x1, -extent.y2, extent.x2, -extent.y1);
        let page_matrix = AffineTransform([1.0, 0.0, 0.0, -1.0, 0.0, height]);
        let mut items = Vec::new();
        let mut placeholders = Vec::new();
        for object in &page.objects {
            let matrix = AffineTransform(object_matrix(object.transform)?);
            let local_bounds = inverse_bounds(scene_bounds, matrix)?;
            let mut object_items = lower_object(object, &paints, local_bounds, &mut placeholders)?;
            // The object form is bounded in scene space, like the legacy writer.
            // Paint opacity stays inside; authored object opacity composites once.
            for item in &mut object_items {
                item.transform = compose(matrix, item.transform);
            }
            if object.opacity_millionths < 1_000_000 {
                let mut item = group(
                    scene_bounds,
                    object.opacity_millionths as f32 / 1_000_000.0,
                    object_items,
                    None,
                );
                item.transform = page_matrix;
                items.push(item);
            } else {
                for mut item in object_items {
                    item.transform = compose(page_matrix, item.transform);
                    items.push(item);
                }
            }
        }
        pages.push(FinishedPage { boxes, items });
        page_evidence.push(SerializedPageEvidence {
            page_id: page.page_id.as_str().to_owned(),
            media_box: page.bleed_box,
            bleed_box: page.bleed_box,
            trim_box: page.trim_box,
            object_ids_in_paint_order: page
                .objects
                .iter()
                .map(|object| object.object_id.as_str().to_owned())
                .collect(),
            placeholder_image_ids: placeholders,
            clipped_table_cells: page
                .objects
                .iter()
                .flat_map(|object| match &object.kind {
                    ResolvedSceneObjectKind::Table { table } => table
                        .populated_cells
                        .iter()
                        .filter(|cell| cell.clipped)
                        .map(|cell| SerializedTableCellAddress {
                            object_id: object.object_id.as_str().to_owned(),
                            row: cell.row,
                            column: cell.column,
                        })
                        .collect::<Vec<_>>(),
                    _ => Vec::new(),
                })
                .collect(),
            bleed_expectations: page.bleed_expectations,
        });
    }
    let uses_device_rgb = (colors.is_none()
        && !crate::color::publication_color_keys(publication).is_empty())
        || !page_evidence
            .iter()
            .all(|page| page.placeholder_image_ids.is_empty());
    let evidence = SceneSerializationEvidence {
        scene_digest: publication.semantic_digest(),
        pages: page_evidence,
        pasteboard_object_count_excluded: publication.pasteboard().objects.len(),
        uses_device_rgb,
        text_object_count: publication
            .pages()
            .iter()
            .flat_map(|page| &page.objects)
            .map(|object| match &object.kind {
                ResolvedSceneObjectKind::Text { .. } => 1,
                ResolvedSceneObjectKind::Table { table } => table.populated_cells.len(),
                _ => 0,
            })
            .sum(),
        text_painted_as_native_outlines: fonts.is_none() && publication_has_outlines(publication),
        embedded_fonts: fonts
            .as_ref()
            .into_iter()
            .flat_map(|fonts| fonts.values())
            .map(|font| font.evidence().clone())
            .collect(),
        transparency: publication_transparency_evidence(publication, &rasters),
        raster_placements: publication
            .pages()
            .iter()
            .flat_map(|page| {
                page.objects.iter().filter_map(|object| match &object.kind {
                    ResolvedSceneObjectKind::Image {
                        asset, crop, flip, ..
                    } => rasters
                        .get(asset)
                        .map(|raster| SerializedRasterPlacementEvidence {
                            page_id: page.page_id.as_str().to_owned(),
                            object_id: object.object_id.as_str().to_owned(),
                            scene_digest: publication.semantic_digest(),
                            size: object.size,
                            transform: object.transform,
                            crop: *crop,
                            flip: *flip,
                            raster: raster.evidence().clone(),
                        }),
                    _ => None,
                })
            })
            .collect(),
    };
    Ok((FinishedDocument { pages, resources }, evidence))
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "consumes the writer error as a Result::map_err callback"
)]
pub(crate) fn map_error(error: FinishedPdfError) -> SceneSerializationError {
    match error {
        FinishedPdfError::NoPages => SceneSerializationError::NoPages,
        FinishedPdfError::InvalidFont(_) | FinishedPdfError::InvalidGlyph { .. } => {
            SceneSerializationError::InvalidFontEvidence
        }
        FinishedPdfError::InvalidImage(_) => SceneSerializationError::InvalidRasterEvidence,
        FinishedPdfError::MissingResource(_) => SceneSerializationError::MissingPreparedFont,
        _ => SceneSerializationError::FinishedInputRejected,
    }
}

struct PaintResources<'a> {
    rasters: &'a BTreeMap<AssetHash, &'a ProductionRaster>,
    fonts: Option<&'a BTreeMap<(String, u32), &'a ProductionFont>>,
    colors: Option<&'a ProductionColorPalette>,
}

fn paint(
    color: Color,
    palette: Option<&ProductionColorPalette>,
) -> Result<Paint, SceneSerializationError> {
    let alpha = ((u32::from(color.alpha) * 1_000_000 + 127) / 255) as f32 / 1_000_000.0;
    Ok(if let Some(palette) = palette {
        let cmyk = palette
            .cmyk(color)
            .ok_or(SceneSerializationError::MissingPreparedColor)?;
        Paint::cmyk(cmyk.map(|c| f32::from(c) / 255.0), alpha)
    } else {
        Paint::rgb(
            [color.red, color.green, color.blue].map(|c| f32::from(c) / 255.0),
            alpha,
        )
    })
}

fn stroke(paint: Paint, width: f32) -> Stroke {
    Stroke {
        paint,
        width,
        cap: LineCap::Butt,
        join: LineJoin::Miter,
        miter_limit: 10.0,
        dash: Vec::new(),
        dash_phase: 0.0,
    }
}
fn path_item(path: FinishedPath, fill: Option<Paint>, border: Option<Stroke>) -> FinishedItem {
    FinishedItem::new(FinishedContent::Path(FinishedPathPaint {
        path,
        fill,
        stroke: border,
    }))
}
fn group(
    bounds: PdfBox,
    opacity: f32,
    items: Vec<FinishedItem>,
    clip: Option<FinishedPath>,
) -> FinishedItem {
    FinishedItem {
        transform: AffineTransform::default(),
        clip,
        blend_mode: crate::finished::BlendMode::Normal,
        content: FinishedContent::Group(FinishedGroup {
            bounds,
            opacity,
            items,
        }),
    }
}
fn rounded(
    items: Vec<FinishedItem>,
    object: &ResolvedSceneObject,
    bounds: PdfBox,
) -> Result<Vec<FinishedItem>, SceneSerializationError> {
    if object.corner_rounding.is_default() {
        return Ok(items);
    }
    Ok(vec![group(bounds, 1.0, items, Some(rectangle(object)?))])
}

fn lower_object(
    object: &ResolvedSceneObject,
    resources: &PaintResources<'_>,
    bounds: PdfBox,
    placeholders: &mut Vec<String>,
) -> Result<Vec<FinishedItem>, SceneSerializationError> {
    let mut items = lower_shadow(object, resources, bounds)?;
    match &object.kind {
        ResolvedSceneObjectKind::Text { layout, .. } => {
            let mut text = Vec::new();
            if let Some(outline) = visible_stroke(object.effects.text_outline) {
                text.extend(lower_text(
                    layout,
                    resources,
                    Some((
                        paint(outline.color, resources.colors)?,
                        Some(outline.width_millipoints as f32 / 1_000.0),
                    )),
                )?);
            }
            text.extend(lower_text(layout, resources, None)?);
            items.extend(rounded(text, object, bounds)?);
        }
        ResolvedSceneObjectKind::Image {
            asset, crop, flip, ..
        } => {
            if resources.rasters.contains_key(asset) {
                items.push(placed_image(object, image_id(asset), *crop, *flip, None)?);
            } else {
                let placeholder = lower_placeholder(object, asset.as_str())?;
                items.extend(rounded(placeholder, object, bounds)?);
                placeholders.push(object.object_id.as_str().to_owned());
            }
            if let Some(border) = visible_stroke(object.effects.picture_border) {
                items.push(path_item(
                    rectangle(object)?,
                    None,
                    Some(stroke(
                        paint(border.color, resources.colors)?,
                        border.width_millipoints as f32 / 1_000.0,
                    )),
                ));
            }
        }
        ResolvedSceneObjectKind::Shape {
            shape,
            fill,
            stroke: border,
        } => {
            if *shape == ShapeKind::Cube {
                let width = micrometres_to_points(object.size.width.value())?;
                let height = micrometres_to_points(object.size.height.value())?;
                for (vertices, shade) in CUBE_FACES {
                    let mut path = PathBuffer::default();
                    append_polygon(&mut path, &vertices, width, height);
                    items.push(path_item(
                        path.finish(),
                        Some(paint(
                            crate::color::cube_face_color(*fill, shade),
                            resources.colors,
                        )?),
                        (border.width_millipoints > 0)
                            .then(|| {
                                paint(border.color, resources.colors)
                                    .map(|p| stroke(p, border.width_millipoints as f32 / 1_000.0))
                            })
                            .transpose()?,
                    ));
                }
            } else {
                items.push(path_item(
                    shape_path(*shape, object)?,
                    (*shape != ShapeKind::Line)
                        .then(|| paint(*fill, resources.colors))
                        .transpose()?,
                    (border.width_millipoints > 0)
                        .then(|| {
                            paint(border.color, resources.colors)
                                .map(|p| stroke(p, border.width_millipoints as f32 / 1_000.0))
                        })
                        .transpose()?,
                ));
            }
        }
        ResolvedSceneObjectKind::Table { table } => {
            items.extend(lower_table(object, table, resources, bounds)?);
        }
    }
    // A zero-width Line has no paint; the neutral API deliberately rejects empty paint.
    items.retain(|item| !matches!(&item.content, FinishedContent::Path(p) if p.fill.is_none() && p.stroke.is_none()));
    Ok(items)
}

fn visible_stroke(stroke: Option<ShapeStroke>) -> Option<ShapeStroke> {
    stroke.filter(|stroke| stroke.width_millipoints > 0 && stroke.color.alpha > 0)
}

fn lower_shadow(
    object: &ResolvedSceneObject,
    resources: &PaintResources<'_>,
    bounds: PdfBox,
) -> Result<Vec<FinishedItem>, SceneSerializationError> {
    let Some(shadow) = object.effects.shadow.filter(|s| s.color.alpha > 0) else {
        return Ok(Vec::new());
    };
    let color = paint(shadow.color, resources.colors)?;
    let mut items = match &object.kind {
        ResolvedSceneObjectKind::Text { layout, .. } => rounded(
            lower_text(layout, resources, Some((color, None)))?,
            object,
            bounds,
        )?,
        ResolvedSceneObjectKind::Shape {
            shape: ShapeKind::Line,
            stroke: border,
            ..
        } => {
            if border.width_millipoints == 0 {
                Vec::new()
            } else {
                vec![path_item(
                    shape_path(ShapeKind::Line, object)?,
                    None,
                    Some(stroke(color, border.width_millipoints as f32 / 1_000.0)),
                )]
            }
        }
        ResolvedSceneObjectKind::Shape { shape, .. } => {
            vec![path_item(shape_path(*shape, object)?, Some(color), None)]
        }
        ResolvedSceneObjectKind::Image {
            asset, crop, flip, ..
        } if resources
            .rasters
            .get(asset)
            .is_some_and(|r| r.evidence().has_transparency) =>
        {
            vec![placed_image(
                object,
                image_id(asset),
                *crop,
                *flip,
                Some(color),
            )?]
        }
        _ => vec![path_item(rectangle(object)?, Some(color), None)],
    };
    let translation = AffineTransform([
        1.0,
        0.0,
        0.0,
        1.0,
        micrometres_to_points(shadow.offset_x.value())?,
        micrometres_to_points(shadow.offset_y.value())?,
    ]);
    for item in &mut items {
        item.transform = compose(translation, item.transform);
    }
    Ok(items)
}

fn placed_image(
    object: &ResolvedSceneObject,
    image: String,
    crop: document_core::model::ImageCrop,
    flip: document_core::model::ImageFlip,
    alpha_paint: Option<Paint>,
) -> Result<FinishedItem, SceneSerializationError> {
    let width = micrometres_to_points(object.size.width.value())?;
    let height = micrometres_to_points(object.size.height.value())?;
    let [w, h, x, y] = cropped_image_transform(width, height, crop, flip);
    let mut child = FinishedItem::new(if let Some(paint) = alpha_paint {
        FinishedContent::ImageAlpha { image, paint }
    } else {
        FinishedContent::Image(ImagePlacement { image, alpha: 1.0 })
    });
    child.transform = AffineTransform([w, 0.0, 0.0, h, x, y]);
    Ok(group(
        PdfBox::new(0.0, 0.0, width, height),
        1.0,
        vec![child],
        Some(rectangle(object)?),
    ))
}

fn lower_text(
    layout: &ResolvedTextBoxPreview,
    resources: &PaintResources<'_>,
    decoration: Option<(Paint, Option<f32>)>,
) -> Result<Vec<FinishedItem>, SceneSerializationError> {
    let mut items = Vec::new();
    // Decorative glyph paths are painted together, preserving joins between
    // overlapping outlines; their geometry never adds duplicate extracted text.
    let mut decorative = PathBuffer::default();
    for run in layout.lines.iter().flat_map(|line| &line.glyph_runs) {
        let color = if let Some((color, _)) = decoration {
            color
        } else {
            paint(run.color, resources.colors)?
        };
        if let Some(fonts) = resources
            .fonts
            .filter(|_| decoration.is_none() && !run.glyphs.is_empty())
        {
            let face = layout
                .resolved_fonts
                .get(run.font_index)
                .ok_or(SceneSerializationError::MissingPreparedFont)?;
            let font = fonts
                .get(&(face.sha256_hex.clone(), face.collection_index))
                .ok_or(SceneSerializationError::MissingPreparedFont)?;
            let glyphs = run
                .glyphs
                .iter()
                .map(|glyph| {
                    let source = u16::try_from(glyph.id)
                        .map_err(|_| SceneSerializationError::GlyphIdOutOfRange)?;
                    let cid = font
                        .cid_for_source_glyph(source)
                        .ok_or(SceneSerializationError::InvalidFontEvidence)?;
                    Ok(PositionedGlyph {
                        cid,
                        matrix: AffineTransform([
                            1.0,
                            0.0,
                            0.0,
                            -1.0,
                            millipoint_f64_to_points(glyph.x.value() as f64)?,
                            millipoint_f64_to_points(glyph.y.value() as f64)?,
                        ]),
                    })
                })
                .collect::<Result<_, SceneSerializationError>>()?;
            items.push(FinishedItem::new(FinishedContent::Glyphs(
                PositionedGlyphRun {
                    font: font_id(font),
                    font_size: millipoint_f64_to_points(run.font_size.value() as f64)?,
                    glyphs,
                    actual_text: run.glyph_text.clone(),
                    paint: color,
                },
            )));
        } else {
            let mut path = PathBuffer::default();
            for glyph in &run.glyphs {
                if let Some(outline) = &glyph.outline {
                    append_glyph_outline(&mut path, outline, glyph.x.value(), glyph.y.value())?;
                }
            }
            if decoration.is_some() {
                decorative.commands.extend(path.commands);
            } else if !path.commands.is_empty() {
                items.push(path_item(path.finish(), Some(color), None));
            }
        }
    }
    if let Some((color, outline_width)) = decoration
        && !decorative.commands.is_empty()
    {
        items.push(path_item(
            decorative.finish(),
            outline_width.is_none().then_some(color),
            outline_width.map(|width| stroke(color, width)),
        ));
    }
    for run in layout.lines.iter().flat_map(|line| &line.glyph_runs) {
        if let Some(underline) = &run.underline {
            let mut path = PathBuffer::default();
            path.rect(
                millipoint_f64_to_points(underline.x.value() as f64)?,
                millipoint_f64_to_points(underline.y.value() as f64)?,
                millipoint_f64_to_points(underline.width.value() as f64)?,
                millipoint_f64_to_points(underline.thickness.value() as f64)?,
            );
            let (color, border) = if let Some((color, width)) = decoration {
                (color, width.map(|w| stroke(color, w)))
            } else {
                (paint(run.color, resources.colors)?, None)
            };
            items.push(path_item(
                path.finish(),
                border.is_none().then_some(color),
                border,
            ));
        }
    }
    Ok(items)
}

fn lower_table(
    object: &ResolvedSceneObject,
    table: &ResolvedTablePreview,
    resources: &PaintResources<'_>,
    bounds: PdfBox,
) -> Result<Vec<FinishedItem>, SceneSerializationError> {
    let mut inner = Vec::new();
    for fill in &table.cell_fills {
        let [l, t, r, b] = table_cell_bounds(
            table,
            fill.row,
            fill.column,
            fill.row_span,
            fill.column_span,
        )?;
        let mut path = PathBuffer::default();
        path.rect(l, t, r - l, b - t);
        inner.push(path_item(
            path.finish(),
            Some(paint(fill.color, resources.colors)?),
            None,
        ));
    }
    inner.extend(lower_table_borders(table, resources, false)?);
    for cell in &table.populated_cells {
        let [l, t, r, b] = table_cell_bounds(
            table,
            cell.row,
            cell.column,
            cell.row_span,
            cell.column_span,
        )?;
        let mut path = PathBuffer::default();
        path.rect(l, t, r - l, b - t);
        inner.push(group(
            PdfBox::new(l, t, r, b),
            1.0,
            lower_text(&cell.layout, resources, None)?,
            Some(path.finish()),
        ));
    }
    let mut items = rounded(inner, object, bounds)?;
    items.extend(lower_table_borders(table, resources, true)?);
    Ok(items)
}

fn lower_table_borders(
    table: &ResolvedTablePreview,
    resources: &PaintResources<'_>,
    outer: bool,
) -> Result<Vec<FinishedItem>, SceneSerializationError> {
    let mut items = Vec::new();
    let mut active = None;
    let mut path = PathBuffer::default();
    for edge in table
        .border_segments
        .iter()
        .filter(|edge| edge.outline.is_empty() != outer)
    {
        let appearance = (edge.color, edge.width_millipoints);
        if active != Some(appearance) {
            if let Some((color, width)) = active {
                items.push(path_item(
                    std::mem::take(&mut path).finish(),
                    None,
                    Some(stroke(
                        paint(color, resources.colors)?,
                        width as f32 / 1_000.0,
                    )),
                ));
            }
            active = Some(appearance);
        }
        if edge.outline.is_empty() {
            path.move_to(
                micrometres_to_points(edge.x1_um)?,
                micrometres_to_points(edge.y1_um)?,
            )
            .line_to(
                micrometres_to_points(edge.x2_um)?,
                micrometres_to_points(edge.y2_um)?,
            );
        } else {
            append_physical_path(&mut path, &edge.outline)?;
        }
    }
    if let Some((color, width)) = active {
        items.push(path_item(
            path.finish(),
            None,
            Some(stroke(
                paint(color, resources.colors)?,
                width as f32 / 1_000.0,
            )),
        ));
    }
    Ok(items)
}

fn lower_placeholder(
    object: &ResolvedSceneObject,
    asset: &str,
) -> Result<Vec<FinishedItem>, SceneSerializationError> {
    let w = micrometres_to_points(object.size.width.value())?;
    let h = micrometres_to_points(object.size.height.value())?;
    let mut bg = PathBuffer::default();
    bg.rect(0.0, 0.0, w, h);
    let mut cross = PathBuffer::default();
    cross
        .rect(0.0, 0.0, w, h)
        .move_to(0.0, 0.0)
        .line_to(w, h)
        .move_to(w, 0.0)
        .line_to(0.0, h);
    let mut bars = PathBuffer::default();
    for (index, byte) in asset.as_bytes().iter().take(8).enumerate() {
        let x = w * (index as f32 + 0.2) / 8.0;
        let height = h * (0.1 + 0.2 * (f32::from(*byte) / 255.0));
        bars.rect(x, h - height, w / 16.0, height);
    }
    Ok(vec![
        path_item(bg.finish(), Some(Paint::rgb([0.92; 3], 1.0)), None),
        path_item(
            cross.finish(),
            None,
            Some(stroke(Paint::rgb([0.35; 3], 1.0), 0.75)),
        ),
        path_item(bars.finish(), Some(Paint::rgb([0.35; 3], 1.0)), None),
    ])
}

fn rectangle(object: &ResolvedSceneObject) -> Result<FinishedPath, SceneSerializationError> {
    let mut path = PathBuffer::default();
    append_rectangle_path(&mut path, object)?;
    Ok(path.finish())
}
fn shape_path(
    shape: ShapeKind,
    object: &ResolvedSceneObject,
) -> Result<FinishedPath, SceneSerializationError> {
    let mut path = PathBuffer::default();
    append_shape_path(&mut path, shape, object)?;
    Ok(path.finish())
}
fn font_id(font: &ProductionFont) -> String {
    format!(
        "{}_{:X}",
        hex_sha256(font.evidence().source_sha256),
        font.evidence().face_index
    )
}
fn image_id(asset: &AssetHash) -> String {
    asset.as_str().to_owned()
}
#[expect(
    clippy::many_single_char_names,
    reason = "standard six-component affine matrix notation"
)]
fn compose(outer: AffineTransform, inner: AffineTransform) -> AffineTransform {
    let [a, b, c, d, e, f] = outer.0;
    let [g, h, i, j, k, l] = inner.0;
    AffineTransform([
        a * g + c * h,
        b * g + d * h,
        a * i + c * j,
        b * i + d * j,
        a * k + c * l + e,
        b * k + d * l + f,
    ])
}
#[expect(
    clippy::many_single_char_names,
    reason = "standard six-component affine matrix notation"
)]
fn inverse_bounds(
    bounds: PdfBox,
    matrix: AffineTransform,
) -> Result<PdfBox, SceneSerializationError> {
    let [a, b, c, d, e, f] = matrix.0.map(f64::from);
    let determinant = a * d - b * c;
    if !determinant.is_finite() || determinant == 0.0 {
        return Err(SceneSerializationError::GeometryOutOfRange);
    }
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (x, y) in [
        (bounds.x1, bounds.y1),
        (bounds.x1, bounds.y2),
        (bounds.x2, bounds.y1),
        (bounds.x2, bounds.y2),
    ] {
        let x = f64::from(x) - e;
        let y = f64::from(y) - f;
        xs.push((d * x - c * y) / determinant);
        ys.push((-b * x + a * y) / determinant);
    }
    let coords = [
        xs.iter().copied().fold(f64::INFINITY, f64::min),
        ys.iter().copied().fold(f64::INFINITY, f64::min),
        xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
    ];
    if coords
        .iter()
        .any(|v| !v.is_finite() || v.abs() > 10_000_000.0)
    {
        return Err(SceneSerializationError::GeometryOutOfRange);
    }
    Ok(PdfBox::new(
        coords[0] as f32,
        coords[1] as f32,
        coords[2] as f32,
        coords[3] as f32,
    ))
}

/// Only builds ordered geometry. It has no PDF dictionaries or emission state.
#[derive(Default)]
struct PathBuffer {
    commands: Vec<PathCommand>,
}
impl PathBuffer {
    fn finish(self) -> FinishedPath {
        FinishedPath {
            commands: self.commands,
            fill_rule: FillRule::NonZero,
        }
    }
    fn move_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.commands.push(PathCommand::MoveTo(Point::new(x, y)));
        self
    }
    fn line_to(&mut self, x: f32, y: f32) -> &mut Self {
        self.commands.push(PathCommand::LineTo(Point::new(x, y)));
        self
    }
    fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) -> &mut Self {
        self.commands.push(PathCommand::CubicTo(
            Point::new(x1, y1),
            Point::new(x2, y2),
            Point::new(x, y),
        ));
        self
    }
    fn close_path(&mut self) -> &mut Self {
        self.commands.push(PathCommand::Close);
        self
    }
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) -> &mut Self {
        self.move_to(x, y)
            .line_to(x + w, y)
            .line_to(x + w, y + h)
            .line_to(x, y + h)
            .close_path()
    }
}

fn append_glyph_outline(
    content: &mut PathBuffer,
    outline: &ResolvedGlyphOutline,
    glyph_x: i64,
    glyph_y: i64,
) -> Result<(), SceneSerializationError> {
    let mut current = (0_i64, 0_i64);
    for command in &outline.commands {
        match *command {
            ResolvedOutlineCommand::MoveTo { x, y } => {
                current = (x.value(), y.value());
                content.move_to(
                    millipoints_to_points(glyph_x, current.0)?,
                    millipoints_to_points(glyph_y, -current.1)?,
                );
            }
            ResolvedOutlineCommand::LineTo { x, y } => {
                current = (x.value(), y.value());
                content.line_to(
                    millipoints_to_points(glyph_x, current.0)?,
                    millipoints_to_points(glyph_y, -current.1)?,
                );
            }
            ResolvedOutlineCommand::QuadTo {
                control_x,
                control_y,
                x,
                y,
            } => {
                let end = (x.value(), y.value());
                let control = (control_x.value(), control_y.value());
                let c1 = (
                    current.0 as f64 + (2.0 / 3.0) * (control.0 - current.0) as f64,
                    current.1 as f64 + (2.0 / 3.0) * (control.1 - current.1) as f64,
                );
                let c2 = (
                    end.0 as f64 + (2.0 / 3.0) * (control.0 - end.0) as f64,
                    end.1 as f64 + (2.0 / 3.0) * (control.1 - end.1) as f64,
                );
                content.cubic_to(
                    millipoint_f64_to_points(glyph_x as f64 + c1.0)?,
                    millipoint_f64_to_points(glyph_y as f64 - c1.1)?,
                    millipoint_f64_to_points(glyph_x as f64 + c2.0)?,
                    millipoint_f64_to_points(glyph_y as f64 - c2.1)?,
                    millipoints_to_points(glyph_x, end.0)?,
                    millipoints_to_points(glyph_y, -end.1)?,
                );
                current = end;
            }
            ResolvedOutlineCommand::CurveTo {
                control_1_x,
                control_1_y,
                control_2_x,
                control_2_y,
                x,
                y,
            } => {
                current = (x.value(), y.value());
                content.cubic_to(
                    millipoints_to_points(glyph_x, control_1_x.value())?,
                    millipoints_to_points(glyph_y, -control_1_y.value())?,
                    millipoints_to_points(glyph_x, control_2_x.value())?,
                    millipoints_to_points(glyph_y, -control_2_y.value())?,
                    millipoints_to_points(glyph_x, current.0)?,
                    millipoints_to_points(glyph_y, -current.1)?,
                );
            }
            ResolvedOutlineCommand::Close => {
                content.close_path();
            }
        }
    }
    Ok(())
}

fn table_cell_bounds(
    table: &layout_core::ResolvedTablePreview,
    row: u16,
    column: u16,
    row_span: u16,
    column_span: u16,
) -> Result<[f32; 4], SceneSerializationError> {
    Ok([
        micrometres_to_points(table.column_boundaries_um[usize::from(column)])?,
        micrometres_to_points(table.row_boundaries_um[usize::from(row)])?,
        micrometres_to_points(table.column_boundaries_um[usize::from(column + column_span)])?,
        micrometres_to_points(table.row_boundaries_um[usize::from(row + row_span)])?,
    ])
}

fn append_rectangle_path(
    content: &mut PathBuffer,
    object: &ResolvedSceneObject,
) -> Result<(), SceneSerializationError> {
    if object.corner_rounding.is_default() {
        content.rect(
            0.0,
            0.0,
            micrometres_to_points(object.size.width.value())?,
            micrometres_to_points(object.size.height.value())?,
        );
    } else {
        let path =
            document_core::shape_geometry::rectangle_outline(object.size, object.corner_rounding)
                .map_err(|_| SceneSerializationError::GeometryOutOfRange)?;
        append_physical_path(content, &path)?;
    }
    Ok(())
}
fn append_physical_path(
    content: &mut PathBuffer,
    path: &[document_core::shape_geometry::ShapePathCommand],
) -> Result<(), SceneSerializationError> {
    use document_core::shape_geometry::ShapePathCommand;
    let p = micrometres_to_points;
    for command in path {
        match *command {
            ShapePathCommand::MoveTo(x, y) => {
                content.move_to(p(x)?, p(y)?);
            }
            ShapePathCommand::LineTo(x, y) => {
                content.line_to(p(x)?, p(y)?);
            }
            ShapePathCommand::CubicTo(x1, y1, x2, y2, x, y) => {
                content.cubic_to(p(x1)?, p(y1)?, p(x2)?, p(y2)?, p(x)?, p(y)?);
            }
            ShapePathCommand::Close => {
                content.close_path();
            }
        }
    }
    Ok(())
}

fn append_shape_path(
    content: &mut PathBuffer,
    shape: ShapeKind,
    object: &ResolvedSceneObject,
) -> Result<(), SceneSerializationError> {
    let width = micrometres_to_points(object.size.width.value())?;
    let height = micrometres_to_points(object.size.height.value())?;
    if matches!(shape, ShapeKind::Triangle | ShapeKind::Arrow)
        || (shape == ShapeKind::Rectangle && !object.corner_rounding.is_default())
    {
        let path = document_core::shape_geometry::shape_outline(
            shape,
            object.size,
            object.corner_rounding,
        )
        .map_err(|_| SceneSerializationError::GeometryOutOfRange)?;
        append_physical_path(content, &path)?;
        return Ok(());
    }
    match shape {
        ShapeKind::Line => {
            content.move_to(0.0, 0.0).line_to(width, height);
        }
        ShapeKind::Rectangle => {
            content.rect(0.0, 0.0, width, height);
        }
        ShapeKind::Ellipse => {
            const KAPPA: f32 = 0.552_284_8;
            let rx = width / 2.0;
            let ry = height / 2.0;
            content
                .move_to(width, ry)
                .cubic_to(width, ry + KAPPA * ry, rx + KAPPA * rx, height, rx, height)
                .cubic_to(rx - KAPPA * rx, height, 0.0, ry + KAPPA * ry, 0.0, ry)
                .cubic_to(0.0, ry - KAPPA * ry, rx - KAPPA * rx, 0.0, rx, 0.0)
                .cubic_to(rx + KAPPA * rx, 0.0, width, ry - KAPPA * ry, width, ry)
                .close_path();
        }
        ShapeKind::Cube => {
            // Union silhouette for the hard shadow; face seams are not holes.
            append_polygon(
                content,
                &[
                    (10.0, 30.0),
                    (50.0, 8.0),
                    (90.0, 30.0),
                    (90.0, 70.0),
                    (50.0, 92.0),
                    (10.0, 70.0),
                ],
                width,
                height,
            );
        }
        ShapeKind::Triangle | ShapeKind::Arrow => {
            unreachable!("shared shape outline handled above")
        }
    }
    Ok(())
}

// Percent coordinates and color-mix percentages match ShapeGraphic's
// familiar three-face Cube, including independently stretched dimensions.
const CUBE_FACES: [([(f32, f32); 4], u32); 3] = [
    ([(10.0, 30.0), (50.0, 8.0), (90.0, 30.0), (50.0, 52.0)], 86),
    ([(10.0, 30.0), (50.0, 52.0), (50.0, 92.0), (10.0, 70.0)], 70),
    ([(50.0, 52.0), (90.0, 30.0), (90.0, 70.0), (50.0, 92.0)], 55),
];

fn append_polygon(content: &mut PathBuffer, vertices: &[(f32, f32)], width: f32, height: f32) {
    let (x, y) = vertices[0];
    content.move_to(x * width / 100.0, y * height / 100.0);
    for &(x, y) in &vertices[1..] {
        content.line_to(x * width / 100.0, y * height / 100.0);
    }
    content.close_path();
}

#[expect(
    clippy::cast_precision_loss,
    reason = "bounded millionth crop fractions are converted only to PDF's f32 paint coordinates"
)]
fn cropped_image_transform(
    width: f32,
    height: f32,
    crop: document_core::model::ImageCrop,
    flip: document_core::model::ImageFlip,
) -> [f32; 4] {
    let visible_width =
        (1_000_000 - crop.left_millionths - crop.right_millionths) as f32 / 1_000_000.0;
    let visible_height =
        (1_000_000 - crop.top_millionths - crop.bottom_millionths) as f32 / 1_000_000.0;
    let source_width = width / visible_width;
    let source_height = height / visible_height;
    let source_x = -(crop.left_millionths as f32) / 1_000_000.0 * source_width;
    // PDF image samples run from unit y=1 (first row) to y=0 (last row).
    // The scene already points down, so invert the image's unit y axis and
    // position its first row above the viewport by the authored top crop.
    // The same matrix keeps the image and its alpha-shadow mask aligned.
    let source_y = (1.0 - crop.top_millionths as f32 / 1_000_000.0) * source_height;
    // Mirror the already cropped source around its viewport, so intrinsic flip
    // does not move the Picture, its border, or its authored shadow offset.
    [
        if flip.horizontal {
            -source_width
        } else {
            source_width
        },
        if flip.vertical {
            source_height
        } else {
            -source_height
        },
        if flip.horizontal {
            width - source_x
        } else {
            source_x
        },
        if flip.vertical {
            height - source_y
        } else {
            source_y
        },
    ]
}

#[cfg(test)]
mod crop_tests {
    use super::cropped_image_transform;

    #[test]
    fn crop_expands_and_offsets_source_inside_the_placed_viewport() {
        let crop = document_core::model::ImageCrop::new(100_000, 50_000, 200_000, 150_000).unwrap();
        let [width, height, x, y] =
            cropped_image_transform(70.0, 80.0, crop, document_core::model::ImageFlip::default());
        assert!((width - 100.0).abs() < 0.001);
        assert!((height + 100.0).abs() < 0.001);
        assert!((x + 10.0).abs() < 0.001);
        // PDF image rows start at unit y=1; the scene's y axis points down.
        // Top and bottom crop boundaries must land on the viewport edges.
        assert!((height * 0.95 + y).abs() < 0.001);
        assert!((height * 0.15 + y - 80.0).abs() < 0.001);
    }

    #[test]
    fn uncropped_image_top_row_stays_at_the_top_of_the_scene() {
        let [width, height, x, y] = cropped_image_transform(
            70.0,
            80.0,
            document_core::model::ImageCrop::default(),
            document_core::model::ImageFlip::default(),
        );
        assert!((width - 70.0).abs() < 0.001);
        assert!(x.abs() < 0.001);
        assert!((height + y).abs() < 0.001);
        assert!((y - 80.0).abs() < 0.001);
    }

    #[test]
    fn intrinsic_flip_mirrors_the_visible_crop_inside_the_same_viewport() {
        let crop = document_core::model::ImageCrop::new(100_000, 50_000, 200_000, 150_000).unwrap();
        for horizontal in [false, true] {
            for vertical in [false, true] {
                let flip = document_core::model::ImageFlip {
                    horizontal,
                    vertical,
                };
                let [width, height, x, y] = cropped_image_transform(70.0, 80.0, crop, flip);
                let expected_left = if horizontal { 70.0 } else { 0.0 };
                let expected_right = if horizontal { 0.0 } else { 70.0 };
                let expected_top = if vertical { 80.0 } else { 0.0 };
                let expected_bottom = if vertical { 0.0 } else { 80.0 };
                assert!((width * 0.1 + x - expected_left).abs() < 0.001);
                assert!((width * 0.8 + x - expected_right).abs() < 0.001);
                assert!((height * 0.95 + y - expected_top).abs() < 0.001);
                assert!((height * 0.15 + y - expected_bottom).abs() < 0.001);
            }
        }
    }
}
