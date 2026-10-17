//! Neutral traversal using the imported exact font and raster resource emitter.
use crate::finished::{
    AffineTransform, BlendMode, FillRule, FinishedContent, FinishedDocument, FinishedItem,
    FinishedOutputMode, FinishedPath, FinishedPdfError, FinishedPdfOptions, FontResource,
    ImageColorSpace, ImageResource, LineCap, LineJoin, MAX_FINISHED_RESOURCE_ID_BYTES,
    MAX_FINISHED_TEXT_BYTES, Paint, PaintColor, PathCommand, PdfBox,
};
use crate::resources::{
    FontReferences, write_alpha_luminosity_mask, write_decoded_image, write_prepared_font,
};
use pdf_writer::types::{LineCapStyle, LineJoinStyle, OutputIntentSubtype, TrappingStatus};
use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, Str, TextStr};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_FINISHED_PAGES: usize = 10_000;
/// Maximum aggregate items, glyph positions, path commands, dash values,
/// gradient stops and interpolation functions.
pub const MAX_FINISHED_ITEMS: usize = 1_000_000;
pub const MAX_FINISHED_GROUP_DEPTH: usize = 64;
const MAX_PDFX_CONTENT_STRING_BYTES: usize = 32_767;
const MAX_PDFX_GRAPHICS_STATE_DEPTH: usize = 28;
const MAX_PDFX_INDIRECT_OBJECTS: i32 = 8_388_607;
const MAX_POINT: f32 = 10_000_000.0;
const PRODUCER: &str = "PDF Library finished-page writer";

#[derive(Debug)]
pub struct FinishedPdf {
    bytes: Vec<u8>,
}
impl FinishedPdf {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
pub struct FinishedPdfWriter;
#[derive(Default)]
struct StreamResources {
    fonts: BTreeMap<String, Ref>,
    x_objects: BTreeMap<String, Ref>,
    states: BTreeMap<String, Ref>,
    shadings: BTreeMap<String, Ref>,
}

struct References {
    next: i32,
    maximum: i32,
    fonts: BTreeMap<String, FontReferences>,
    images: BTreeMap<String, (Ref, Option<Ref>)>,
    alphas: BTreeMap<(u32, u32), Ref>,
    blends: BTreeMap<BlendMode, Ref>,
    image_masks: BTreeMap<String, (Ref, Ref)>,
}
impl References {
    fn alloc(&mut self) -> Result<Ref, FinishedPdfError> {
        if self.next > self.maximum {
            return Err(FinishedPdfError::ResourceLimit);
        }
        let reference = Ref::new(self.next);
        self.next = self
            .next
            .checked_add(1)
            .ok_or(FinishedPdfError::ResourceLimit)?;
        Ok(reference)
    }
}
struct Inputs<'a> {
    fonts: BTreeMap<&'a str, &'a FontResource>,
    images: BTreeMap<&'a str, &'a ImageResource>,
    alphas: BTreeSet<(u32, u32)>,
    blends: BTreeSet<BlendMode>,
    // Aggregate painted RGB is only a PDF/X admission fact. Page/Form group
    // spaces are selected from their own subtrees by paints_rgb below.
    rgb: bool,
    registered_rgb_images: bool,
    item_count: usize,
    text_bytes: usize,
    image_masks: BTreeSet<String>,
    content_string_bytes: usize,
    graphics_state_depth: usize,
}

impl FinishedPdfWriter {
    /// Serializes finished content in supplied page and paint order.
    /// Validation completes before any PDF bytes are returned. Ordinary output
    /// carries explicit metadata and no PDF/X claims. PDF/X-4 is a bounded candidate.
    /// # Errors
    /// Reports invalid geometry, paint, paths, resources, CIDs and budgets;
    /// PDF/X mode also rejects RGB, blank titles, oversized content strings,
    /// excessive executed graphics-state depth and unusable exact output profiles.
    pub fn write(
        document: &FinishedDocument,
        options: &FinishedPdfOptions<'_>,
    ) -> Result<FinishedPdf, FinishedPdfError> {
        Self::write_internal(document, options, false)
    }

    /// Retains the compatibility preflight probe: it may deliberately contain
    /// unmanaged RGB so its existing caller receives a blocking finding.
    /// This exception is private; public PDF/X generation always rejects RGB.
    #[cfg(feature = "publisher-adapter")]
    pub(crate) fn write_diagnostic_candidate(
        document: &FinishedDocument,
        options: &FinishedPdfOptions<'_>,
    ) -> Result<FinishedPdf, FinishedPdfError> {
        Self::write_internal(document, options, true)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one envelope writer pairs validation, pages and exact metadata references"
    )]
    fn write_internal(
        document: &FinishedDocument,
        options: &FinishedPdfOptions<'_>,
        diagnostic_rgb: bool,
    ) -> Result<FinishedPdf, FinishedPdfError> {
        let pdfx = matches!(options.mode, FinishedOutputMode::PdfX4 { .. });
        let mut inputs = validate(document, pdfx)?;
        count_text(&mut inputs, options.metadata.title_byte_len())?;
        let profile = match options.mode {
            FinishedOutputMode::Ordinary => None,
            FinishedOutputMode::PdfX4 { output_profile } => {
                if (inputs.rgb || inputs.registered_rgb_images) && !diagnostic_rgb {
                    return Err(FinishedPdfError::RgbInPdfX);
                }
                if options.metadata.title_is_blank() {
                    return Err(FinishedPdfError::EmptyPdfXTitle);
                }
                if inputs.content_string_bytes > MAX_PDFX_CONTENT_STRING_BYTES {
                    return Err(FinishedPdfError::PdfXContentStringTooLong {
                        bytes: inputs.content_string_bytes,
                        maximum: MAX_PDFX_CONTENT_STRING_BYTES,
                    });
                }
                if inputs.graphics_state_depth > MAX_PDFX_GRAPHICS_STATE_DEPTH {
                    return Err(FinishedPdfError::PdfXGraphicsStateDepthExceeded {
                        depth: inputs.graphics_state_depth,
                        maximum: MAX_PDFX_GRAPHICS_STATE_DEPTH,
                    });
                }
                crate::inspect_output_intent(output_profile)
                    .map_err(FinishedPdfError::InvalidOutputProfile)?;
                Some(output_profile)
            }
        };
        let mut pdf = Pdf::new();
        pdf.set_version(1, 6);
        let (mut refs, pages, metadata_ref, info_ref, profile_ref) =
            allocate_references(document, &inputs, profile.is_some())?;
        emit_resources(&mut pdf, &refs, &inputs)?;
        for (page, (page_ref, content_ref)) in document.pages.iter().zip(&pages) {
            let (bytes, stream) = paint_items(&mut pdf, &mut refs, &page.items, &inputs, pdfx)?;
            pdf.stream(*content_ref, &bytes);
            let mut target = pdf.page(*page_ref);
            target
                .parent(Ref::new(2))
                .media_box(rect(page.boxes.media))
                .bleed_box(rect(page.boxes.bleed))
                .trim_box(rect(page.boxes.trim))
                .contents(*content_ref);
            write_resources(&mut target.resources(), &stream);
            {
                let mut group = target.group();
                group.transparency().isolated(true).knockout(false);
                if !pdfx && paints_rgb(&page.items, &inputs) {
                    group.color_space().device_rgb();
                } else {
                    group.color_space().device_cmyk();
                }
            }
        }
        pdf.pages(Ref::new(2))
            .kids(pages.iter().map(|(page, _)| *page))
            .count(i32::try_from(pages.len()).map_err(|_| FinishedPdfError::ResourceLimit)?);
        {
            let mut catalog = pdf.catalog(Ref::new(1));
            catalog.pages(Ref::new(2)).metadata(metadata_ref);
            if let Some(reference) = profile_ref {
                let identity = hex(&Sha256::digest(
                    profile.expect("profile reference is paired"),
                ));
                let identifier = format!("PDF Library CMYK SHA-256 {identity}");
                let description = format!("Embedded CMYK printer profile; SHA-256 {identity}");
                catalog
                    .output_intents()
                    .push()
                    .subtype(OutputIntentSubtype::PDFX)
                    .output_condition_identifier(TextStr(&identifier))
                    .info(TextStr(&description))
                    .dest_output_profile(reference);
            }
        }
        if let (Some(reference), Some(bytes)) = (profile_ref, profile) {
            pdf.icc_profile(reference, bytes).n(4);
        }
        {
            let mut info = pdf.document_info(info_ref);
            options.metadata.write_info(&mut info);
            info.creator(TextStr("PDF Library"))
                .producer(TextStr(PRODUCER))
                .trapped(TrappingStatus::NotTrapped);
            if profile.is_some() {
                info.pair(Name(b"GTS_PDFXVersion"), TextStr("PDF/X-4"));
            }
        }
        let hash = Sha256::digest(pdf.as_bytes());
        let file_id = &hash[..16];
        pdf.set_file_id((file_id.to_vec(), file_id.to_vec()));
        pdf.metadata(
            metadata_ref,
            &options
                .metadata
                .xmp_for(file_id, PRODUCER, "PDF Library", profile.is_some()),
        );
        Ok(FinishedPdf {
            bytes: crate::pdf_lines::finish(&pdf.finish())?,
        })
    }
}

fn validate(document: &FinishedDocument, pdfx: bool) -> Result<Inputs<'_>, FinishedPdfError> {
    if document.pages.is_empty() {
        return Err(FinishedPdfError::NoPages);
    }
    if document.pages.len() > MAX_FINISHED_PAGES {
        return Err(FinishedPdfError::ResourceLimit);
    }
    let mut inputs = Inputs {
        fonts: BTreeMap::new(),
        images: BTreeMap::new(),
        alphas: BTreeSet::new(),
        blends: BTreeSet::new(),
        rgb: false,
        registered_rgb_images: false,
        item_count: 0,
        text_bytes: 0,
        image_masks: BTreeSet::new(),
        content_string_bytes: 0,
        graphics_state_depth: 0,
    };
    let mut ids = BTreeSet::new();
    let mut retained = 0_usize;
    for font in &document.resources.fonts {
        resource_identity(&font.id)?;
        count_text(&mut inputs, font.id.len())?;
        for chars in font.unicode.values() {
            count_text(&mut inputs, chars.iter().map(|c| c.len_utf8()).sum())?;
        }
        if !ids.insert(&font.id) {
            return Err(FinishedPdfError::DuplicateResource(font.id.clone()));
        }
        retained = retained
            .checked_add(font.program.prepared_bytes().len())
            .ok_or(FinishedPdfError::ResourceLimit)?;
        inputs.fonts.insert(&font.id, font);
    }
    let mut compressed = 0_usize;
    for image in &document.resources.images {
        resource_identity(&image.id)?;
        count_text(&mut inputs, image.id.len())?;
        if !ids.insert(&image.id) {
            return Err(FinishedPdfError::DuplicateResource(image.id.clone()));
        }
        compressed = compressed
            .checked_add(
                image.color.capacity() + image.alpha.as_ref().map_or(0, |plane| plane.capacity()),
            )
            .ok_or(FinishedPdfError::ResourceLimit)?;
        // Every registered image is emitted, including unused resources and
        // alpha-only sources. Its color space therefore participates in PDF/X
        // admission even when that color is not painted into a group.
        inputs.registered_rgb_images |= image.color_space == ImageColorSpace::Rgb;
        inputs.images.insert(&image.id, image);
    }
    if compressed > raster_capacity::MAX_PREPARED_RASTER_CACHE_BYTES
        || retained.saturating_add(compressed) > raster_capacity::MAX_RETAINED_RASTER_BYTES
    {
        return Err(FinishedPdfError::ResourceLimit);
    }
    for page in &document.pages {
        check_box(page.boxes.media)?;
        check_box(page.boxes.bleed)?;
        check_box(page.boxes.trim)?;
        if !contains(page.boxes.media, page.boxes.bleed)
            || !contains(page.boxes.bleed, page.boxes.trim)
        {
            return Err(FinishedPdfError::InvalidGeometry);
        }
        validate_items(&page.items, &mut inputs, 0, pdfx)?;
    }
    Ok(inputs)
}
#[expect(
    clippy::too_many_lines,
    reason = "one traversal validates all finished content kinds and their execution budgets"
)]
fn validate_items(
    items: &[FinishedItem],
    inputs: &mut Inputs<'_>,
    depth: usize,
    pdfx: bool,
) -> Result<(), FinishedPdfError> {
    if depth > MAX_FINISHED_GROUP_DEPTH {
        return Err(FinishedPdfError::ResourceLimit);
    }
    count_primitives(inputs, items.len())?;
    for item in items {
        // Each ancestor group retains its explicit item q and the implicit
        // Form invocation save (PDF Reference 1.6 section 4.9). This item's q
        // adds one more. Empty groups still execute their Form invocation.
        let item_depth = 2 * depth + 1;
        inputs.graphics_state_depth = inputs.graphics_state_depth.max(item_depth);
        matrix(item.transform)?;
        if let Some(clip) = &item.clip {
            count_primitives(inputs, clip.commands.len())?;
            path(clip)?;
        }
        match &item.content {
            FinishedContent::Gradient(fill) => {
                count_primitives(inputs, fill.path.commands.len())?;
                count_primitives(inputs, fill.gradient.stops.len())?;
                path(&fill.path)?;
                crate::finished_gradient::path_bounds(&fill.path)?;
                let functions =
                    crate::finished_gradient::validate(&fill.gradient, MAX_POINT, pdfx)?;
                count_primitives(inputs, functions)?;
                // All stops share the first stop's space; components and alpha
                // were checked by the focused gradient validator above.
                paint(fill.gradient.stops[0].paint, inputs)?;
                let constant = crate::finished_gradient::constant_alpha(&fill.gradient);
                if constant.is_none() {
                    // The mask Form paints one shading with no explicit q.
                    // Its invocation contributes exactly one implicit save.
                    inputs.graphics_state_depth = inputs.graphics_state_depth.max(item_depth + 1);
                }
            }
            FinishedContent::Path(value) => {
                count_primitives(inputs, value.path.commands.len())?;
                path(&value.path)?;
                if value.fill.is_none() && value.stroke.is_none() {
                    return Err(FinishedPdfError::InvalidPaint);
                }
                if let Some(fill) = value.fill {
                    paint(fill, inputs)?;
                }
                if let Some(stroke) = &value.stroke {
                    validate_stroke(stroke, inputs)?;
                }
            }
            FinishedContent::Glyphs(run) => {
                resource_identity(&run.font)?;
                count_text(inputs, run.font.len())?;
                count_text(inputs, run.actual_text.len())?;
                inputs.content_string_bytes = inputs
                    .content_string_bytes
                    .max(text_string_payload_bytes(&run.actual_text));
                count_primitives(inputs, run.glyphs.len())?;
                paint(run.paint, inputs)?;
                if !point(run.font_size)
                    || run.font_size <= 0.0
                    || run.glyphs.is_empty()
                    || run.actual_text.is_empty()
                    || run.actual_text.contains('\u{001b}')
                {
                    return Err(FinishedPdfError::InvalidFont(run.font.clone()));
                }
                let font = inputs
                    .fonts
                    .get(run.font.as_str())
                    .ok_or_else(|| FinishedPdfError::MissingResource(run.font.clone()))?;
                for glyph in &run.glyphs {
                    matrix(glyph.matrix)?;
                    if glyph.cid == 0
                        || glyph.cid >= font.program.embedded_glyph_count()
                        || !font.unicode.contains_key(&glyph.cid)
                    {
                        return Err(FinishedPdfError::InvalidGlyph {
                            font: run.font.clone(),
                            cid: glyph.cid,
                        });
                    }
                }
            }
            FinishedContent::Image(image) => {
                resource_identity(&image.image)?;
                count_text(inputs, image.image.len())?;
                alpha(image.alpha)?;
                let resource = inputs
                    .images
                    .get(image.image.as_str())
                    .ok_or_else(|| FinishedPdfError::MissingResource(image.image.clone()))?;
                inputs.rgb |= resource.color_space == ImageColorSpace::Rgb;
            }
            FinishedContent::ImageAlpha {
                image,
                paint: color,
            } => {
                // The emitted luminosity mask is a Form containing one image
                // Do and no q. Its implicit save adds one; an image Do adds none.
                inputs.graphics_state_depth = inputs.graphics_state_depth.max(item_depth + 1);
                resource_identity(image)?;
                count_text(inputs, image.len())?;
                paint(*color, inputs)?;
                let resource = inputs
                    .images
                    .get(image.as_str())
                    .ok_or_else(|| FinishedPdfError::MissingResource(image.clone()))?;
                if resource.alpha.is_none() {
                    return Err(FinishedPdfError::InvalidImage(image.clone()));
                }
                inputs.image_masks.insert(image.clone());
            }
            FinishedContent::Group(group) => {
                inputs.graphics_state_depth = inputs.graphics_state_depth.max(item_depth + 1);
                check_box(group.bounds)?;
                alpha(group.opacity)?;
                validate_items(&group.items, inputs, depth + 1, pdfx)?;
            }
        }
        inputs.alphas.insert(item_alpha_pair(item));
        inputs.blends.insert(item.blend_mode);
    }
    Ok(())
}
fn text_string_payload_bytes(value: &str) -> usize {
    // Match pinned pdf-writer TextStr: printable ASCII is byte-for-byte;
    // everything else is UTF-16BE with a two-byte BOM. Escaping/hex syntax
    // does not contribute to the decoded PDF string's byte length.
    if value.bytes().all(|b| matches!(b, 32..=126)) {
        value.len()
    } else {
        2 + 2 * value.encode_utf16().count()
    }
}
fn point(value: f32) -> bool {
    value.is_finite() && value.abs() <= MAX_POINT
}
fn check_box(b: PdfBox) -> Result<(), FinishedPdfError> {
    if [b.x1, b.y1, b.x2, b.y2].into_iter().all(point) && b.x2 > b.x1 && b.y2 > b.y1 {
        Ok(())
    } else {
        Err(FinishedPdfError::InvalidGeometry)
    }
}
fn contains(outer: PdfBox, inner: PdfBox) -> bool {
    outer.x1 <= inner.x1 && outer.y1 <= inner.y1 && outer.x2 >= inner.x2 && outer.y2 >= inner.y2
}
fn matrix(value: AffineTransform) -> Result<(), FinishedPdfError> {
    let [a, b, c, d, _, _] = value.0;
    let determinant = f64::from(a) * f64::from(d) - f64::from(b) * f64::from(c);
    if value.0.into_iter().all(point) && determinant != 0.0 {
        Ok(())
    } else {
        Err(FinishedPdfError::InvalidGeometry)
    }
}
fn alpha(a: f32) -> Result<(), FinishedPdfError> {
    if a.is_finite() && (0.0..=1.0).contains(&a) {
        Ok(())
    } else {
        Err(FinishedPdfError::InvalidPaint)
    }
}
fn paint(value: Paint, inputs: &mut Inputs<'_>) -> Result<(), FinishedPdfError> {
    alpha(value.alpha)?;
    let valid = match value.color {
        PaintColor::Gray(c) => unit(c),
        PaintColor::Rgb(c) => {
            inputs.rgb = true;
            c.into_iter().all(unit)
        }
        PaintColor::Cmyk(c) => c.into_iter().all(unit),
    };
    if valid {
        Ok(())
    } else {
        Err(FinishedPdfError::InvalidPaint)
    }
}
fn unit(v: f32) -> bool {
    v.is_finite() && (0.0..=1.0).contains(&v)
}
fn path(value: &FinishedPath) -> Result<(), FinishedPdfError> {
    if value.commands.is_empty() {
        return Err(FinishedPdfError::InvalidPath);
    }
    let mut started = false;
    for command in &value.commands {
        let valid = match command {
            PathCommand::MoveTo(p) => {
                started = true;
                point(p.x) && point(p.y)
            }
            PathCommand::LineTo(p) => started && point(p.x) && point(p.y),
            PathCommand::CubicTo(a, b, c) => {
                started && [a, b, c].into_iter().all(|p| point(p.x) && point(p.y))
            }
            PathCommand::Close => started,
        };
        if !valid {
            return Err(FinishedPdfError::InvalidPath);
        }
    }
    Ok(())
}
fn alpha_pair(stroke: f32, fill: f32) -> (u32, u32) {
    (
        if stroke == 0.0 { 0 } else { stroke.to_bits() },
        if fill == 0.0 { 0 } else { fill.to_bits() },
    )
}
fn item_alpha_pair(item: &FinishedItem) -> (u32, u32) {
    let alpha = match &item.content {
        FinishedContent::Path(p) => {
            return alpha_pair(
                p.stroke.as_ref().map_or(1.0, |s| s.paint.alpha),
                p.fill.map_or(1.0, |p| p.alpha),
            );
        }
        FinishedContent::Gradient(fill) => {
            crate::finished_gradient::constant_alpha(&fill.gradient).unwrap_or(1.0)
        }
        FinishedContent::Glyphs(run) => run.paint.alpha,
        FinishedContent::Image(image) => image.alpha,
        FinishedContent::ImageAlpha { paint, .. } => paint.alpha,
        FinishedContent::Group(group) => group.opacity,
    };
    alpha_pair(alpha, alpha)
}
fn paints_rgb(items: &[FinishedItem], inputs: &Inputs<'_>) -> bool {
    // Inspect only painted content in this page/group subtree. In particular,
    // ImageAlpha paints its supplied color, never its source image's color.
    items.iter().any(|item| match &item.content {
        FinishedContent::Path(p) => {
            p.fill.is_some_and(|p| is_rgb(p.color))
                || p.stroke.as_ref().is_some_and(|s| is_rgb(s.paint.color))
        }
        FinishedContent::Gradient(fill) => is_rgb(fill.gradient.stops[0].paint.color),
        FinishedContent::Glyphs(run) => is_rgb(run.paint.color),
        FinishedContent::Image(image) => {
            inputs.images[image.image.as_str()].color_space == ImageColorSpace::Rgb
        }
        FinishedContent::ImageAlpha { paint, .. } => is_rgb(paint.color),
        FinishedContent::Group(group) => paints_rgb(&group.items, inputs),
    })
}
fn is_rgb(color: PaintColor) -> bool {
    matches!(color, PaintColor::Rgb(_))
}
fn blend_name(mode: BlendMode) -> &'static [u8] {
    match mode {
        BlendMode::Normal => b"Normal",
        BlendMode::Multiply => b"Multiply",
        BlendMode::Screen => b"Screen",
        BlendMode::Overlay => b"Overlay",
        BlendMode::Darken => b"Darken",
        BlendMode::Lighten => b"Lighten",
        BlendMode::ColorDodge => b"ColorDodge",
        BlendMode::ColorBurn => b"ColorBurn",
        BlendMode::HardLight => b"HardLight",
        BlendMode::SoftLight => b"SoftLight",
        BlendMode::Difference => b"Difference",
        BlendMode::Exclusion => b"Exclusion",
        BlendMode::Hue => b"Hue",
        BlendMode::Saturation => b"Saturation",
        BlendMode::Color => b"Color",
        BlendMode::Luminosity => b"Luminosity",
    }
}
fn blend_state_name(mode: BlendMode) -> String {
    format!(
        "B{}",
        std::str::from_utf8(blend_name(mode)).expect("ASCII PDF blend name")
    )
}
fn rect(b: PdfBox) -> Rect {
    Rect::new(b.x1, b.y1, b.x2, b.y2)
}
fn font_name(id: &str) -> String {
    format!("F{}", hex(&Sha256::digest(id.as_bytes())))
}
fn image_name(id: &str) -> String {
    format!("I{}", hex(&Sha256::digest(id.as_bytes())))
}
fn state_name(pair: (u32, u32)) -> String {
    format!("A{:X}_{:X}", pair.0, pair.1)
}
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for byte in bytes {
        write!(s, "{byte:02X}").expect("String write");
    }
    s
}
fn write_resources(resources: &mut pdf_writer::writers::Resources<'_>, stream: &StreamResources) {
    if !stream.fonts.is_empty() {
        let mut fonts = resources.fonts();
        for (name, reference) in &stream.fonts {
            fonts.pair(Name(name.as_bytes()), *reference);
        }
    }
    if !stream.x_objects.is_empty() {
        let mut images = resources.x_objects();
        for (name, reference) in &stream.x_objects {
            images.pair(Name(name.as_bytes()), *reference);
        }
    }
    if !stream.states.is_empty() {
        let mut states = resources.ext_g_states();
        for (name, reference) in &stream.states {
            states.pair(Name(name.as_bytes()), *reference);
        }
    }
    if !stream.shadings.is_empty() {
        let mut shadings = resources.shadings();
        for (name, reference) in &stream.shadings {
            shadings.pair(Name(name.as_bytes()), *reference);
        }
    }
}
fn set_fill(content: &mut Content, paint: Paint) {
    match paint.color {
        PaintColor::Gray(gray) => {
            content.set_fill_gray(gray);
        }
        PaintColor::Rgb([r, g, b]) => {
            content.set_fill_rgb(r, g, b);
        }
        PaintColor::Cmyk([c, m, y, k]) => {
            content.set_fill_cmyk(c, m, y, k);
        }
    }
}
fn set_stroke(content: &mut Content, paint: Paint) {
    match paint.color {
        PaintColor::Gray(gray) => {
            content.set_stroke_gray(gray);
        }
        PaintColor::Rgb([r, g, b]) => {
            content.set_stroke_rgb(r, g, b);
        }
        PaintColor::Cmyk([c, m, y, k]) => {
            content.set_stroke_cmyk(c, m, y, k);
        }
    }
}
#[expect(
    clippy::float_cmp,
    reason = "optimize only exactly equal rectangle coordinates, preserving authored geometry"
)]
fn append_path(content: &mut Content, path: &FinishedPath) {
    // The ordered closed rectangle has the same winding as PDF's compact re
    // operator. Preserve exact rectangle geometry used by retained caller tests.
    if let [
        PathCommand::MoveTo(a),
        PathCommand::LineTo(b),
        PathCommand::LineTo(c),
        PathCommand::LineTo(d),
        PathCommand::Close,
    ] = path.commands.as_slice()
        && a.y == b.y
        && b.x == c.x
        && c.y == d.y
        && d.x == a.x
    {
        content.rect(a.x, a.y, b.x - a.x, d.y - a.y);
        return;
    }
    for command in &path.commands {
        match command {
            PathCommand::MoveTo(p) => {
                content.move_to(p.x, p.y);
            }
            PathCommand::LineTo(p) => {
                content.line_to(p.x, p.y);
            }
            PathCommand::CubicTo(a, b, c) => {
                content.cubic_to(a.x, a.y, b.x, b.y, c.x, c.y);
            }
            PathCommand::Close => {
                content.close_path();
            }
        }
    }
}
fn paint_items(
    pdf: &mut Pdf,
    refs: &mut References,
    items: &[FinishedItem],
    inputs: &Inputs<'_>,
    pdfx: bool,
) -> Result<(Vec<u8>, StreamResources), FinishedPdfError> {
    let mut content = Content::new();
    let mut resources = StreamResources::default();
    for item in items {
        content.save_state();
        // Select each child's own blend explicitly, including Normal. A group's
        // placement mode/opacity never becomes the mode/opacity of its paints.
        let blend = blend_state_name(item.blend_mode);
        content.set_parameters(Name(blend.as_bytes()));
        resources
            .states
            .insert(blend, refs.blends[&item.blend_mode]);
        let pair = item_alpha_pair(item);
        let alpha = state_name(pair);
        content.set_parameters(Name(alpha.as_bytes()));
        resources.states.insert(alpha, refs.alphas[&pair]);
        content.transform(item.transform.0);
        if let Some(clip) = &item.clip {
            clip_path(&mut content, clip);
        }
        match &item.content {
            FinishedContent::Path(p) => paint_path(&mut content, p),
            FinishedContent::Gradient(fill) => {
                let gradient = &fill.gradient;
                let bounds = crate::finished_gradient::path_bounds(&fill.path)?;
                let gradient_refs =
                    crate::finished_gradient::emit(pdf, gradient, bounds, || refs.alloc())?;
                if let Some(state) = gradient_refs.mask_state {
                    let name = crate::finished_gradient::mask_name(state);
                    content.set_parameters(Name(name.as_bytes()));
                    resources.states.insert(name, state);
                }
                clip_path(&mut content, &fill.path);
                let name = crate::finished_gradient::shading_name(gradient_refs.shading);
                content.shading(Name(name.as_bytes()));
                resources.shadings.insert(name, gradient_refs.shading);
            }
            FinishedContent::Glyphs(run) => {
                set_fill(&mut content, run.paint);
                content.begin_text();
                let name = font_name(&run.font);
                content.set_font(Name(name.as_bytes()), run.font_size);
                resources
                    .fonts
                    .insert(name, refs.fonts[&run.font].type0_font);
                {
                    let mut marked = content.begin_marked_content_with_properties(Name(b"Span"));
                    marked.properties().actual_text(TextStr(&run.actual_text));
                }
                for glyph in &run.glyphs {
                    content.set_text_matrix(glyph.matrix.0);
                    content.show(Str(&glyph.cid.to_be_bytes()));
                }
                content.end_marked_content();
                content.end_text();
            }
            FinishedContent::Image(image) => {
                let name = image_name(&image.image);
                content.x_object(Name(name.as_bytes()));
                resources
                    .x_objects
                    .insert(name, refs.images[&image.image].0);
            }
            FinishedContent::ImageAlpha { image, paint } => {
                set_fill(&mut content, *paint);
                let name = mask_name(image);
                content.set_parameters(Name(name.as_bytes()));
                resources.states.insert(name, refs.image_masks[image].1);
                content.rect(0.0, 0.0, 1.0, 1.0).fill_nonzero();
            }
            FinishedContent::Group(group) => {
                let reference = refs.alloc()?;
                let name = format!("G{}", reference.get());
                let (bytes, children) = paint_items(pdf, refs, &group.items, inputs, pdfx)?;
                let mut form = pdf.form_xobject(reference, &bytes);
                form.bbox(rect(group.bounds));
                {
                    let mut settings = form.group();
                    settings.transparency().isolated(true).knockout(false);
                    if !pdfx && paints_rgb(&group.items, inputs) {
                        settings.color_space().device_rgb();
                    } else {
                        settings.color_space().device_cmyk();
                    }
                }
                write_resources(&mut form.resources(), &children);
                form.finish();
                content.x_object(Name(name.as_bytes()));
                resources.x_objects.insert(name, reference);
            }
        }
        content.restore_state();
    }
    Ok((content.finish().into_vec(), resources))
}

fn clip_path(content: &mut Content, path: &FinishedPath) {
    append_path(content, path);
    match path.fill_rule {
        FillRule::NonZero => {
            content.clip_nonzero();
        }
        FillRule::EvenOdd => {
            content.clip_even_odd();
        }
    }
    content.end_path();
}

#[expect(
    clippy::type_complexity,
    reason = "one private allocation result keeps page and envelope references paired with their allocator"
)]
fn allocate_references(
    document: &FinishedDocument,
    inputs: &Inputs<'_>,
    has_profile: bool,
) -> Result<(References, Vec<(Ref, Ref)>, Ref, Ref, Option<Ref>), FinishedPdfError> {
    let mut refs = References {
        next: 3,
        maximum: if has_profile {
            MAX_PDFX_INDIRECT_OBJECTS
        } else {
            i32::MAX - 1
        },
        fonts: BTreeMap::new(),
        images: BTreeMap::new(),
        alphas: BTreeMap::new(),
        blends: BTreeMap::new(),
        image_masks: BTreeMap::new(),
    };
    let mut pages = Vec::with_capacity(document.pages.len());
    for _ in &document.pages {
        pages.push((refs.alloc()?, refs.alloc()?));
    }
    for font in inputs.fonts.keys() {
        let value = FontReferences {
            type0_font: refs.alloc()?,
            cid_font: refs.alloc()?,
            descriptor: refs.alloc()?,
            font_file: refs.alloc()?,
            cid_set: refs.alloc()?,
            to_unicode: refs.alloc()?,
        };
        refs.fonts.insert((*font).to_owned(), value);
    }
    for (id, image) in &inputs.images {
        let image_ref = refs.alloc()?;
        let mask = if image.alpha.is_some() {
            Some(refs.alloc()?)
        } else {
            None
        };
        refs.images.insert((*id).to_owned(), (image_ref, mask));
    }
    for id in &inputs.image_masks {
        let pair = (refs.alloc()?, refs.alloc()?);
        refs.image_masks.insert(id.clone(), pair);
    }
    for alpha in &inputs.alphas {
        let reference = refs.alloc()?;
        refs.alphas.insert(*alpha, reference);
    }
    for blend in &inputs.blends {
        let reference = refs.alloc()?;
        refs.blends.insert(*blend, reference);
    }
    let metadata_ref = refs.alloc()?;
    let info_ref = refs.alloc()?;
    let profile_ref = if has_profile {
        Some(refs.alloc()?)
    } else {
        None
    };
    Ok((refs, pages, metadata_ref, info_ref, profile_ref))
}

fn emit_resources(
    pdf: &mut Pdf,
    refs: &References,
    inputs: &Inputs<'_>,
) -> Result<(), FinishedPdfError> {
    let mut tags = BTreeSet::new();
    for (id, font) in &inputs.fonts {
        write_prepared_font(
            pdf,
            &font.program,
            &font.unicode,
            refs.fonts[*id],
            &mut tags,
        )
        .map_err(|_| FinishedPdfError::InvalidFont((*id).to_owned()))?;
    }
    for (id, image) in &inputs.images {
        let (image_ref, mask_ref) = refs.images[*id];
        write_decoded_image(pdf, image_ref, mask_ref, image)
            .map_err(|_| FinishedPdfError::ResourceLimit)?;
    }
    for (id, (form, state)) in &refs.image_masks {
        let alpha_image = refs.images[id].1.expect("mask plane validated");
        write_alpha_luminosity_mask(pdf, alpha_image, *form, *state);
    }
    for ((stroke, fill), reference) in &refs.alphas {
        pdf.ext_graphics(*reference)
            .stroking_alpha(f32::from_bits(*stroke))
            .non_stroking_alpha(f32::from_bits(*fill));
    }
    for (mode, reference) in &refs.blends {
        pdf.ext_graphics(*reference)
            .pair(Name(b"BM"), Name(blend_name(*mode)));
    }
    Ok(())
}

fn paint_path(content: &mut Content, p: &crate::finished::FinishedPathPaint) {
    if let Some(fill) = p.fill {
        set_fill(content, fill);
    }
    if let Some(stroke) = &p.stroke {
        set_stroke(content, stroke.paint);
        content.set_line_width(stroke.width);
        content.set_line_cap(match stroke.cap {
            LineCap::Butt => LineCapStyle::ButtCap,
            LineCap::Round => LineCapStyle::RoundCap,
            LineCap::Square => LineCapStyle::ProjectingSquareCap,
        });
        content.set_line_join(match stroke.join {
            LineJoin::Miter => LineJoinStyle::MiterJoin,
            LineJoin::Round => LineJoinStyle::RoundJoin,
            LineJoin::Bevel => LineJoinStyle::BevelJoin,
        });
        content.set_miter_limit(stroke.miter_limit);
        content.set_dash_pattern(stroke.dash.iter().copied(), stroke.dash_phase);
    }
    append_path(content, &p.path);
    match (p.fill.is_some(), p.stroke.is_some(), p.path.fill_rule) {
        (true, true, FillRule::NonZero) => {
            content.fill_nonzero_and_stroke();
        }
        (true, true, FillRule::EvenOdd) => {
            content.fill_even_odd_and_stroke();
        }
        (true, false, FillRule::NonZero) => {
            content.fill_nonzero();
        }
        (true, false, FillRule::EvenOdd) => {
            content.fill_even_odd();
        }
        (false, true, _) => {
            content.stroke();
        }
        _ => unreachable!("validated paint"),
    }
}

fn mask_name(id: &str) -> String {
    // Caller identities remain full length; PDF Names are bounded to 65 bytes.
    format!("M{}", hex(&Sha256::digest(id.as_bytes())))
}

fn count_primitives(inputs: &mut Inputs<'_>, count: usize) -> Result<(), FinishedPdfError> {
    inputs.item_count = inputs
        .item_count
        .checked_add(count)
        .filter(|n| *n <= MAX_FINISHED_ITEMS)
        .ok_or(FinishedPdfError::ResourceLimit)?;
    Ok(())
}

fn count_text(inputs: &mut Inputs<'_>, bytes: usize) -> Result<(), FinishedPdfError> {
    inputs.text_bytes = inputs
        .text_bytes
        .checked_add(bytes)
        .filter(|sum| *sum <= MAX_FINISHED_TEXT_BYTES)
        .ok_or(FinishedPdfError::ResourceLimit)?;
    Ok(())
}
fn resource_identity(id: &str) -> Result<(), FinishedPdfError> {
    if id.len() > MAX_FINISHED_RESOURCE_ID_BYTES {
        Err(FinishedPdfError::ResourceLimit)
    } else {
        Ok(())
    }
}

fn validate_stroke(
    stroke: &crate::finished::Stroke,
    inputs: &mut Inputs<'_>,
) -> Result<(), FinishedPdfError> {
    count_primitives(inputs, stroke.dash.len())?;
    paint(stroke.paint, inputs)?;
    if !point(stroke.width)
        || stroke.width < 0.0
        || !point(stroke.miter_limit)
        || stroke.miter_limit < 1.0
        || !point(stroke.dash_phase)
        || stroke.dash_phase < 0.0
        || stroke.dash.iter().any(|v| !point(*v) || *v < 0.0)
        || (!stroke.dash.is_empty() && stroke.dash.iter().all(|v| *v == 0.0))
    {
        return Err(FinishedPdfError::InvalidPaint);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdfx_reference_allocator_allows_last_object_and_rejects_the_next() {
        let document = FinishedDocument {
            pages: vec![],
            resources: crate::finished::FinishedResources::default(),
        };
        let inputs = Inputs {
            fonts: BTreeMap::new(),
            images: BTreeMap::new(),
            alphas: BTreeSet::new(),
            blends: BTreeSet::new(),
            rgb: false,
            registered_rgb_images: false,
            item_count: 0,
            text_bytes: 0,
            image_masks: BTreeSet::new(),
            content_string_bytes: 0,
            graphics_state_depth: 0,
        };
        let (mut refs, _, _, _, _) = allocate_references(&document, &inputs, true).unwrap();
        refs.next = 8_388_607;
        assert_eq!(refs.alloc().unwrap().get(), 8_388_607);
        assert!(refs.alloc().is_err());
        let (mut ordinary, _, _, _, _) = allocate_references(&document, &inputs, false).unwrap();
        ordinary.next = 8_388_608;
        assert_eq!(ordinary.alloc().unwrap().get(), 8_388_608);
    }
}
