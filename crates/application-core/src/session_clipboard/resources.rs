//! Only existing canonical assets can enter an immutable session capture.
use super::{
    super::{
        EditorError, EditorErrorCode, EditorResult, MAX_TEXT_BOX_FONT_BYTES_TOTAL,
        MAX_TEXT_BOX_FONT_RESOURCES, invalid_input,
    },
    ClipboardFontWorkOrder,
};
use document_core::model::{
    ExactFontFallbackChain, FontFaceBinding, FontFallbackScript, ObjectKind, PublicationObject,
    TextSourceRange, TextStyle, effective_font_variant_ranges,
};
use document_core::package::{
    AssetDescriptor, PACKAGE_LIMITS_V1, asset_descriptors, is_supported_font_media_type,
};
use document_core::{AssetHash, AssetStore};
use font_boundary::{
    FontContainerKind, FontEmbeddingLicense, FontEmbeddingRights, InspectedFontFace,
};
use icu_properties::{CodePointMapData, props::Script};
use std::collections::{BTreeMap, BTreeSet};

type FaceKey = (AssetHash, u32);
type VerifiedFaceVariants = BTreeMap<FaceKey, (u16, bool)>;
#[derive(Clone, Debug, Default)]
struct FaceRequirement {
    required: bool,
    required_variants: BTreeSet<(u16, bool)>,
    optional_variants: BTreeSet<(u16, bool)>,
}
#[derive(Clone, Debug)]
pub(crate) struct ResourceCapturePlan {
    assets: AssetStore,
    faces: BTreeMap<FaceKey, FaceRequirement>,
    non_fonts: BTreeSet<AssetHash>,
}
#[derive(Clone, Debug)]
pub(crate) struct CapturedResources {
    assets: AssetStore,
}
impl CapturedResources {
    pub(crate) fn descriptors(&self) -> impl Iterator<Item = (AssetHash, AssetDescriptor)> {
        asset_descriptors(&self.assets).into_iter()
    }
    pub(crate) fn insert_into(&self, target: &mut AssetStore) {
        for hash in asset_descriptors(&self.assets).into_keys() {
            if target.contains(&hash) {
                continue;
            }
            let asset = self
                .assets
                .get(&hash)
                .expect("captured descriptor belongs to immutable store");
            if let Some(raster) = &asset.raster {
                target.insert_raster(asset.bytes.clone(), raster.clone());
            } else {
                target.insert(asset.media_type.clone(), asset.bytes.clone());
            }
        }
    }
}
impl ResourceCapturePlan {
    pub(crate) fn objects(
        assets: &AssetStore,
        objects: &[PublicationObject],
        fallbacks: &[ExactFontFallbackChain],
    ) -> EditorResult<Self> {
        let mut plan = Self {
            assets: assets.clone(),
            faces: BTreeMap::new(),
            non_fonts: BTreeSet::new(),
        };
        for object in objects {
            match &object.kind {
                ObjectKind::Image { asset, .. } => {
                    plan.non_fonts.insert(asset.clone());
                }
                ObjectKind::Text { style, .. } => plan.style_faces(style, true)?,
                ObjectKind::Table { table } => plan.style_faces(&table.style, true)?,
                ObjectKind::Shape { .. } => {}
            }
        }
        for (face, weight, italic) in rendered_faces(objects)? {
            plan.add_face(&face, true, Some((weight, italic)))?;
        }
        for chain in fallbacks {
            for face in &chain.faces {
                plan.add_face(face, true, None)?;
            }
        }
        plan.finish()?;
        Ok(plan)
    }
    pub(crate) fn style(assets: &AssetStore, style: Option<&TextStyle>) -> EditorResult<Self> {
        let mut plan = Self {
            assets: assets.clone(),
            faces: BTreeMap::new(),
            non_fonts: BTreeSet::new(),
        };
        if let Some(style) = style {
            let face = style.font_face.as_ref().ok_or_else(|| {
                font_unavailable("Embed the source font before copying its Style.")
            })?;
            plan.add_face(face, true, Some((style.font_weight, style.italic)))?;
            for variant in &style.exact_font_variants {
                plan.add_face(
                    &variant.face,
                    false,
                    Some((variant.font_weight, variant.italic)),
                )?;
            }
            for family in &style.exact_font_families {
                plan.add_face(
                    &family.face,
                    false,
                    Some((family.font_weight, family.italic)),
                )?;
            }
        }
        plan.finish()?;
        Ok(plan)
    }
    fn style_faces(&mut self, style: &TextStyle, required: bool) -> EditorResult<()> {
        if let Some(face) = &style.font_face {
            self.add_face(face, required, Some((style.font_weight, style.italic)))?;
        }
        for variant in &style.exact_font_variants {
            self.add_face(
                &variant.face,
                required,
                Some((variant.font_weight, variant.italic)),
            )?;
        }
        for family in &style.exact_font_families {
            self.add_face(
                &family.face,
                required,
                Some((family.font_weight, family.italic)),
            )?;
        }
        Ok(())
    }
    fn add_face(
        &mut self,
        face: &FontFaceBinding,
        required: bool,
        variant: Option<(u16, bool)>,
    ) -> EditorResult<()> {
        let Some(asset) = self.assets.get(&face.source) else {
            return if required {
                Err(font_unavailable(
                    "A captured font is unavailable. Embed its exact face before copying.",
                ))
            } else {
                Ok(())
            };
        };
        if !is_supported_font_media_type(&asset.media_type)
            || AssetHash::for_bytes(&asset.bytes) != face.source
            || ttf_parser::Face::parse(&asset.bytes, face.face_index).is_err()
        {
            return if required {
                Err(font_unavailable("A captured font asset is invalid."))
            } else {
                Ok(())
            };
        }
        let entry = self
            .faces
            .entry((face.source.clone(), face.face_index))
            .or_default();
        entry.required |= required;
        if let Some(variant) = variant {
            if required {
                entry.required_variants.insert(variant);
            } else {
                entry.optional_variants.insert(variant);
            }
        }
        Ok(())
    }
    fn finish(&mut self) -> EditorResult<()> {
        let mut retained = self.non_fonts.clone();
        retained.extend(self.faces.keys().map(|key| key.0.clone()));
        let mut total = 0_u64;
        for hash in &retained {
            let asset = self
                .assets
                .get(hash)
                .ok_or_else(|| invalid_input("A captured Picture resource is unavailable."))?;
            if AssetHash::for_bytes(&asset.bytes) != *hash
                || (self.non_fonts.contains(hash) && asset.raster.is_none())
            {
                return Err(invalid_input("A captured Picture resource is invalid."));
            }
            total = total.checked_add(asset.bytes.len() as u64).ok_or_else(|| {
                invalid_input("Clipboard resources exceed the publication limit.")
            })?;
        }
        if total > PACKAGE_LIMITS_V1.aggregate_asset_bytes {
            return Err(invalid_input(
                "Clipboard resources exceed the publication limit.",
            ));
        }
        let font_bytes = self
            .faces
            .keys()
            .try_fold(0_usize, |total, key| {
                total.checked_add(self.assets.get(&key.0)?.bytes.len())
            })
            .ok_or_else(|| {
                font_unavailable("Clipboard fonts exceed the supported resource budget.")
            })?;
        if self.faces.len() > MAX_TEXT_BOX_FONT_RESOURCES
            || font_bytes > MAX_TEXT_BOX_FONT_BYTES_TOTAL
        {
            return Err(font_unavailable(
                "Clipboard fonts exceed the supported resource budget.",
            ));
        }
        self.assets.retain_hashes(&retained);
        Ok(())
    }
    pub(crate) fn font_resources(&self) -> Vec<ClipboardFontWorkOrder> {
        self.faces
            .iter()
            .map(
                |((source, face_index), requirement)| ClipboardFontWorkOrder {
                    source: source.clone(),
                    face_index: *face_index,
                    bytes: self
                        .assets
                        .get(source)
                        .expect("capture admitted exact bytes")
                        .bytes
                        .clone(),
                    required: requirement.required,
                },
            )
            .collect()
    }
    pub(crate) fn verify(
        self,
        faces: Vec<InspectedFontFace>,
    ) -> EditorResult<(CapturedResources, VerifiedFaceVariants)> {
        let mut accepted = BTreeMap::new();
        let mut received = BTreeSet::new();
        for face in faces {
            let key = (AssetHash::for_bytes(face.source_bytes()), face.face_index());
            let requirement = self.faces.get(&key).ok_or_else(|| {
                invalid_input("Font evidence does not belong to this clipboard capture.")
            })?;
            if !received.insert(key.clone()) {
                return Err(invalid_input("Duplicate clipboard font evidence."));
            }
            let valid = face.container_kind() == FontContainerKind::SingleFont
                && face.face_count() == 1
                && editable_rights(face.embedding_rights())
                && requirement
                    .required_variants
                    .iter()
                    .all(|&(weight, italic)| face.weight() == weight && face.italic() == italic);
            if !valid && requirement.required {
                return Err(font_unavailable(
                    "The captured font cannot be embedded in an editable publication. The clipboard and publication are unchanged.",
                ));
            }
            if valid
                && (requirement.required
                    || requirement
                        .optional_variants
                        .contains(&(face.weight(), face.italic())))
            {
                accepted.insert(key, (face.weight(), face.italic()));
            }
        }
        if self
            .faces
            .iter()
            .any(|(key, value)| value.required && !accepted.contains_key(key))
        {
            return Err(font_unavailable(
                "Verify every required captured font before copying or cutting.",
            ));
        }
        let mut retained = self.non_fonts;
        retained.extend(accepted.keys().map(|key| key.0.clone()));
        let mut assets = self.assets;
        assets.retain_hashes(&retained);
        Ok((CapturedResources { assets }, accepted))
    }
}
fn editable_rights(rights: FontEmbeddingRights) -> bool {
    matches!(
        rights,
        FontEmbeddingRights::SubsetAllowed {
            license: FontEmbeddingLicense::Installable | FontEmbeddingLicense::Editable,
            ..
        } | FontEmbeddingRights::FullFontRequired {
            license: FontEmbeddingLicense::Installable | FontEmbeddingLicense::Editable,
            ..
        }
    )
}
pub(crate) fn font_unavailable(message: &str) -> EditorError {
    EditorError::new(EditorErrorCode::FontUnavailable, message)
}
/// Actual rendered combinations, with no installed-name or synthetic face fallback.
pub(crate) fn rendered_faces(
    objects: &[PublicationObject],
) -> EditorResult<Vec<(FontFaceBinding, u16, bool)>> {
    let mut faces = Vec::new();
    for object in objects {
        match &object.kind {
            ObjectKind::Text {
                text, style, story, ..
            } => {
                for range in effective_font_variant_ranges(
                    style,
                    story,
                    TextSourceRange {
                        start: 0,
                        end: text.len(),
                    },
                ) {
                    let face=style.exact_font_face_for_family(&range.font_family,range.font_weight,range.italic).ok_or_else(||font_unavailable("An effective text face is unavailable. Embed it before copying or pasting."))?;
                    faces.push((face.clone(), range.font_weight, range.italic));
                }
            }
            ObjectKind::Table { table } => {
                for row in 0..table.rows {
                    for column in 0..table.columns {
                        if !table.cells
                            [usize::from(row) * usize::from(table.columns) + usize::from(column)]
                        .is_empty()
                        {
                            let style = table.effective_cell_style(row, column);
                            let face=style.font_face.ok_or_else(||font_unavailable("An effective Table face is unavailable. Embed it before copying or pasting."))?;
                            faces.push((face, style.font_weight, style.italic));
                        }
                    }
                }
            }
            ObjectKind::Image { .. } | ObjectKind::Shape { .. } => {}
        }
    }
    Ok(faces)
}
pub(crate) fn relevant_fallbacks(
    objects: &[PublicationObject],
    chains: &[ExactFontFallbackChain],
) -> Vec<ExactFontFallbackChain> {
    let mut scripts = BTreeSet::new();
    for object in objects {
        match &object.kind {
            ObjectKind::Text { text, .. } => collect_scripts(text, &mut scripts),
            ObjectKind::Table { table } => {
                for text in &table.cells {
                    collect_scripts(text, &mut scripts);
                }
            }
            ObjectKind::Shape { .. } | ObjectKind::Image { .. } => {}
        }
    }
    chains
        .iter()
        .filter(|chain| scripts.contains(&chain.script))
        .cloned()
        .collect()
}
fn collect_scripts(text: &str, scripts: &mut BTreeSet<FontFallbackScript>) {
    let data = CodePointMapData::<Script>::new();
    for character in text.chars() {
        let script = data.get(character);
        let mapped = match script {
            Script::Arabic => FontFallbackScript::Arabic,
            Script::Bengali => FontFallbackScript::Bengali,
            Script::Cyrillic => FontFallbackScript::Cyrillic,
            Script::Devanagari => FontFallbackScript::Devanagari,
            Script::Greek => FontFallbackScript::Greek,
            Script::Gujarati => FontFallbackScript::Gujarati,
            Script::Gurmukhi => FontFallbackScript::Gurmukhi,
            Script::Han => FontFallbackScript::Han,
            Script::Hangul => FontFallbackScript::Hangul,
            Script::Hebrew => FontFallbackScript::Hebrew,
            Script::Hiragana => FontFallbackScript::Hiragana,
            Script::Kannada => FontFallbackScript::Kannada,
            Script::Katakana => FontFallbackScript::Katakana,
            Script::Latin => FontFallbackScript::Latin,
            Script::Malayalam => FontFallbackScript::Malayalam,
            Script::Tamil => FontFallbackScript::Tamil,
            Script::Telugu => FontFallbackScript::Telugu,
            Script::Thai => FontFallbackScript::Thai,
            _ => continue,
        };
        scripts.insert(mapped);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn script_classification_uses_unicode_properties_for_extended_and_supplementary_text() {
        let mut scripts = BTreeSet::new();
        collect_scripts("𐀀𠀀 ἀ Ж ا বাংলা ש देवनागरी कन्नಡ ไทย 한국 あ ア", &mut scripts);
        assert!(scripts.contains(&FontFallbackScript::Han));
        assert!(scripts.contains(&FontFallbackScript::Greek));
        assert!(scripts.contains(&FontFallbackScript::Cyrillic));
        assert!(scripts.contains(&FontFallbackScript::Bengali));
        assert!(scripts.contains(&FontFallbackScript::Hangul));
        assert!(!scripts.contains(&FontFallbackScript::Latin));
        let mut common = BTreeSet::new();
        collect_scripts("123 !", &mut common);
        assert!(common.is_empty());
    }
}
