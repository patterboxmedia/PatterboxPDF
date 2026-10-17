//! Shared exact resource emission used by neutral and compatibility traversal.
use flate2::{Compression, write::ZlibEncoder};
use font_boundary::{FontOutlineKind, PreparedPublicationFont};
use pdf_writer::types::{CidFontType, FontFlags, MaskType, SystemInfo};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref, Str};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
#[derive(Clone, Copy, Debug)]
pub(crate) enum ResourceEmissionError {
    InvalidFont,
    ResourceLimit,
}
#[derive(Clone, Copy)]
pub(crate) struct FontReferences {
    pub type0_font: Ref,
    pub cid_font: Ref,
    pub descriptor: Ref,
    pub font_file: Ref,
    pub cid_set: Ref,
    pub to_unicode: Ref,
}
#[expect(
    clippy::cast_precision_loss,
    reason = "the confined worker bounds font metrics to PDF-safe integer ranges and pdf-writer intentionally represents them as f32"
)]
pub(crate) fn write_prepared_font(
    pdf: &mut Pdf,
    program: &PreparedPublicationFont,
    unicode: &BTreeMap<u16, Vec<char>>,
    refs: FontReferences,
    subset_tags: &mut BTreeSet<[u8; 6]>,
) -> Result<(), ResourceEmissionError> {
    let cmap = crate::to_unicode::cmap(unicode).map_err(|()| ResourceEmissionError::InvalidFont)?;
    let postscript_name = pdf_font_name(
        program.source_sha256(),
        program.postscript_name(),
        program.preparation_kind(),
        subset_tags,
    )?;
    let base_font = Name(&postscript_name);
    // PDF 1.6 Table 5.18 recommends the CMap suffix for a Type0 parent
    // whose descendant is CIDFontType0. The descendant keeps the font name.
    let mut type0_name = postscript_name.clone();
    if program.outline_kind() == FontOutlineKind::Cff {
        type0_name.extend_from_slice(b"-Identity-H");
    }
    pdf.type0_font(refs.type0_font)
        .base_font(Name(&type0_name))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(refs.cid_font)
        .to_unicode(refs.to_unicode);
    {
        let mut cid_font = pdf.cid_font(refs.cid_font);
        cid_font
            .subtype(match program.outline_kind() {
                FontOutlineKind::TrueType => CidFontType::Type2,
                FontOutlineKind::Cff => CidFontType::Type0,
            })
            .base_font(base_font)
            .system_info(SystemInfo {
                registry: Str(b"Adobe"),
                ordering: Str(b"Identity"),
                supplement: 0,
            })
            .font_descriptor(refs.descriptor)
            .default_width(1_000.0);
        cid_font
            .widths()
            .consecutive(0, program.pdf_metrics().widths().iter().copied());
        if program.outline_kind() == FontOutlineKind::TrueType {
            cid_font.cid_to_gid_map_predefined(Name(b"Identity"));
        }
    }
    {
        let metrics = program.pdf_metrics();
        let bbox = metrics.bbox();
        let mut flags = FontFlags::SYMBOLIC;
        if program.fixed_pitch() {
            flags |= FontFlags::FIXED_PITCH;
        }
        if program.italic() {
            flags |= FontFlags::ITALIC;
        }
        let mut descriptor = pdf.font_descriptor(refs.descriptor);
        descriptor
            .name(base_font)
            .flags(flags)
            .bbox(Rect::new(bbox[0], bbox[1], bbox[2], bbox[3]))
            .italic_angle(program.italic_angle_millidegrees() as f32 / 1_000.0)
            .ascent(metrics.ascent())
            .descent(metrics.descent())
            .stem_v(metrics.stem_v())
            .cid_set(refs.cid_set);
        if let Some(cap_height) = metrics.cap_height() {
            descriptor.cap_height(cap_height);
        }
        match program.outline_kind() {
            FontOutlineKind::TrueType => {
                descriptor.font_file2(refs.font_file);
            }
            FontOutlineKind::Cff => {
                descriptor.font_file3(refs.font_file);
            }
        }
    }
    {
        let mut stream = pdf.stream(refs.font_file, program.prepared_bytes());
        match program.outline_kind() {
            FontOutlineKind::TrueType => {
                stream.pair(
                    Name(b"Length1"),
                    i32::try_from(program.prepared_bytes().len())
                        .map_err(|_| ResourceEmissionError::InvalidFont)?,
                );
            }
            FontOutlineKind::Cff => {
                stream.pair(Name(b"Subtype"), Name(b"CIDFontType0C"));
            }
        }
    }
    let mut cid_set = vec![0_u8; usize::from(program.embedded_glyph_count()).div_ceil(8)];
    for cid in 0..usize::from(program.embedded_glyph_count()) {
        cid_set[cid / 8] |= 0x80 >> (cid % 8);
    }
    pdf.stream(refs.cid_set, &cid_set);
    pdf.cmap(refs.to_unicode, cmap.as_slice());
    Ok(())
}

fn pdf_font_name(
    source_hash: [u8; 32],
    postscript_name: &str,
    preparation_kind: font_boundary::FontPreparationKind,
    subset_tags: &mut BTreeSet<[u8; 6]>,
) -> Result<Vec<u8>, ResourceEmissionError> {
    if preparation_kind != font_boundary::FontPreparationKind::Subset {
        return Ok(postscript_name.as_bytes().to_vec());
    }
    let mut tag = core::array::from_fn(|index| b'A' + source_hash[index] % 26);
    // Collection faces share a source hash. Preserve the preferred tag when
    // available, but keep every subset tag unique within this PDF (PDF 1.6 5.5.3).
    // Stable font-map order makes collision resolution repeatable.
    for _ in 0..=subset_tags.len() {
        if subset_tags.insert(tag) {
            let mut name = Vec::with_capacity(postscript_name.len() + 7);
            name.extend_from_slice(&tag);
            name.push(b'+');
            name.extend_from_slice(postscript_name.as_bytes());
            return Ok(name);
        }
        for letter in tag.iter_mut().rev() {
            if *letter < b'Z' {
                *letter += 1;
                break;
            }
            *letter = b'A';
        }
    }
    Err(ResourceEmissionError::InvalidFont)
}

struct BoundedCompressedBytes {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for BoundedCompressedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let required = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|length| *length <= self.maximum)
            .ok_or_else(|| {
                std::io::Error::other("Prepared Pictures exceed the 192 MiB compressed cache limit")
            })?;
        if required > self.bytes.capacity() {
            let target = required
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.maximum);
            self.bytes
                .try_reserve_exact(target - self.bytes.len())
                .map_err(|_| std::io::Error::other("Picture compression allocation refused"))?;
            if self.bytes.capacity() > self.maximum {
                return Err(std::io::Error::other(
                    "Picture compression capacity refused",
                ));
            }
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn compress_raster_stream(
    bytes: &[u8],
    maximum: usize,
) -> Result<Vec<u8>, ResourceEmissionError> {
    let output = BoundedCompressedBytes {
        bytes: Vec::new(),
        maximum,
    };
    let mut encoder = ZlibEncoder::new(output, Compression::fast());
    encoder
        .write_all(bytes)
        .map_err(|_| ResourceEmissionError::ResourceLimit)?;
    encoder
        .finish()
        .map(|output| output.bytes)
        .map_err(|_| ResourceEmissionError::ResourceLimit)
}

pub(crate) fn write_decoded_image(
    pdf: &mut Pdf,
    image_ref: Ref,
    mask_ref: Option<Ref>,
    resource: &crate::finished::ImageResource,
) -> Result<(), ResourceEmissionError> {
    let width = i32::try_from(resource.width).map_err(|_| ResourceEmissionError::ResourceLimit)?;
    let height =
        i32::try_from(resource.height).map_err(|_| ResourceEmissionError::ResourceLimit)?;
    let mut image = pdf.image_xobject(image_ref, &resource.color);
    image.filter(Filter::FlateDecode);
    image.width(width).height(height);
    match resource.color_space {
        crate::finished::ImageColorSpace::Gray => {
            image.color_space().device_gray();
        }
        crate::finished::ImageColorSpace::Rgb => {
            image.color_space().device_rgb();
        }
        crate::finished::ImageColorSpace::Cmyk => {
            image.color_space().device_cmyk();
        }
    }
    image.bits_per_component(8);
    if let Some(reference) = mask_ref {
        image.s_mask(reference);
    }
    image.finish();
    if let (Some(reference), Some(bytes)) = (mask_ref, resource.alpha.as_ref()) {
        let mut mask = pdf.image_xobject(reference, bytes);
        mask.filter(Filter::FlateDecode);
        mask.width(width).height(height);
        mask.color_space().device_gray();
        mask.bits_per_component(8);
        mask.finish();
    }
    Ok(())
}

/// Unit-square luminosity mask from the exact compressed alpha plane.
/// Placement, clipping and color remain with the caller's paint traversal.
pub(crate) fn write_alpha_luminosity_mask(
    pdf: &mut Pdf,
    alpha_image_ref: Ref,
    form_ref: Ref,
    state_ref: Ref,
) {
    // Reuse the attested alpha bytes as a unit-square luminosity mask.
    // Color stays in the existing prepared palette; crop and stretch
    // are applied when the shadow is painted, just like the picture.
    let mut mask_content = Content::new();
    mask_content.x_object(Name(b"AlphaPixels"));
    let bytes = mask_content.finish();
    let mut group = pdf.form_xobject(form_ref, &bytes);
    group.bbox(Rect::new(0.0, 0.0, 1.0, 1.0));
    group
        .resources()
        .x_objects()
        .pair(Name(b"AlphaPixels"), alpha_image_ref);
    {
        let mut settings = group.group();
        settings.transparency().isolated(true).knockout(false);
        settings.color_space().device_gray();
    }
    group.finish();
    pdf.ext_graphics(state_ref)
        .soft_mask()
        .subtype(MaskType::Luminosity)
        .group(form_ref);
}

#[cfg(test)]
mod tests {
    use super::pdf_font_name;
    use font_boundary::FontPreparationKind;
    use std::collections::BTreeSet;

    #[test]
    fn subset_tags_are_unique_for_collection_faces_and_colliding_hashes() {
        let mut tags = BTreeSet::new();
        for (hash, face, expected) in [
            ([25; 32], "Regular", b"ZZZZZZ+Regular".as_slice()),
            ([25; 32], "Bold", b"AAAAAA+Bold".as_slice()),
            ([51; 32], "Other", b"AAAAAB+Other".as_slice()),
        ] {
            assert_eq!(
                pdf_font_name(hash, face, FontPreparationKind::Subset, &mut tags).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn uncontended_subset_and_full_font_names_keep_their_existing_identity() {
        let mut tags = BTreeSet::new();
        let mut hash = [0; 32];
        hash[..6].copy_from_slice(&[0, 1, 2, 3, 4, 5]);
        assert_eq!(
            pdf_font_name(hash, "Regular", FontPreparationKind::Full, &mut tags).unwrap(),
            b"Regular"
        );
        assert!(tags.is_empty());
        assert_eq!(
            pdf_font_name(hash, "Regular", FontPreparationKind::Subset, &mut tags).unwrap(),
            b"ABCDEF+Regular"
        );
    }
}
