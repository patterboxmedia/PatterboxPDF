//! Exact publication-font preparation for the resolved export scene.
//!
//! This boundary gathers glyphs from page content only, binds them to the
//! exact faces selected by layout-core, and delegates rights-aware program
//! preparation to the confined font worker. It does not paint PDF text; the
//! serializer consumes these opaque prepared assets in a later step.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use document_core::AssetHash;
use font_boundary::{
    FontPreparationKind, FontWorkerError, PreparedPublicationFont, TrustedFontBoundary,
};
use layout_core::{
    ExactFontResource, PreviewBudgetState, ResolvedPublication, ResolvedSceneObjectKind,
    ResolvedTextBoxPreview, TextBoxPreviewState,
};

use crate::font_contract::{FontContractFailure, validate_layout_font_contract};

/// Stable, non-font-data evidence for one scene-bound prepared font program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductionFontEvidence {
    pub scene_digest: [u8; 32],
    pub source_sha256: [u8; 32],
    pub face_index: u32,
    pub postscript_name_sha256: [u8; 32],
    pub preparation_kind: FontPreparationKind,
    pub glyph_set_sha256: [u8; 32],
    pub used_glyph_count: u16,
    pub embedded_glyph_count: u16,
    pub prepared_font_sha256: [u8; 32],
    pub rights_identity_sha256: [u8; 32],
    pub worker_executable_sha256: [u8; 32],
    pub engine_identity_sha256: [u8; 32],
    pub rules_identity_sha256: [u8; 32],
}

/// Exact worker-prepared font program accepted for one resolved publication.
pub struct ProductionFont {
    evidence: ProductionFontEvidence,
    program: Arc<PreparedPublicationFont>,
    to_unicode: Arc<BTreeMap<u16, Vec<char>>>,
}

impl ProductionFont {
    /// Moves the exact adapter-prepared resource into the neutral writer boundary.
    /// # Errors
    /// Rejects invalid neutral resource identities or Unicode mappings.
    pub fn into_finished_resource(
        self,
        id: String,
    ) -> Result<crate::finished::FontResource, crate::finished::FinishedPdfError> {
        self.finished_resource(id)
    }

    pub(crate) fn finished_resource(
        &self,
        id: String,
    ) -> Result<crate::finished::FontResource, crate::finished::FinishedPdfError> {
        if id.is_empty()
            || id.len() > crate::finished::MAX_FINISHED_RESOURCE_ID_BYTES
            || self.to_unicode.values().any(|s| {
                s.is_empty() || !crate::to_unicode::destination_within_limit(s.iter().copied())
            })
        {
            return Err(crate::finished::FinishedPdfError::InvalidFont(id));
        }
        Ok(crate::finished::FontResource {
            id,
            program: Arc::clone(&self.program),
            unicode: Arc::clone(&self.to_unicode),
        })
    }

    #[must_use]
    pub const fn evidence(&self) -> &ProductionFontEvidence {
        &self.evidence
    }

    /// Whether positive print still needs authored accented-glyph roles.
    #[must_use]
    pub fn requires_ascent_facts(&self) -> bool {
        self.program.requires_ascent_facts()
    }

    /// Whether positive print still needs the author's capital-height metric.
    #[must_use]
    pub fn requires_cap_height_fact(&self) -> bool {
        self.program.requires_cap_height_fact()
    }

    /// Source/face-bound identity of an explicitly supplied accent set.
    #[must_use]
    pub fn ascent_facts_sha256(&self) -> Option<[u8; 32]> {
        self.program.ascent_facts_sha256()
    }

    /// Source/face-bound identity of both optional authored font facts.
    #[must_use]
    pub fn font_facts_sha256(&self) -> Option<[u8; 32]> {
        self.program.font_facts_sha256()
    }

    pub(crate) fn program(&self) -> &PreparedPublicationFont {
        &self.program
    }

    pub(crate) fn to_unicode(&self) -> &BTreeMap<u16, Vec<char>> {
        &self.to_unicode
    }

    pub(crate) fn cid_for_source_glyph(&self, source_glyph_id: u16) -> Option<u16> {
        self.program
            .remapped_glyphs()
            .iter()
            .find(|mapping| mapping.source_glyph_id() == source_glyph_id)
            .map(|mapping| mapping.cid())
    }
}

/// A publication font fact that cannot be prepared honestly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductionFontError {
    PreviewBudgetIncomplete,
    TextPreviewIncomplete {
        object_id: String,
    },
    MissingResolvedFont {
        object_id: String,
        font_index: usize,
    },
    NonCanonicalFontBindings {
        object_id: String,
        source_start: usize,
        source_end: usize,
    },
    FontBindingUnresolved {
        object_id: String,
        source_start: usize,
        source_end: usize,
    },
    FontBindingIdentityMismatch {
        object_id: String,
        source_start: usize,
        source_end: usize,
    },
    UnsupportedFontSynthesis {
        object_id: String,
        source_start: usize,
        source_end: usize,
    },
    UnsupportedGlyphFallback {
        object_id: String,
        glyph_id: u32,
    },
    GlyphIdOutOfRange {
        object_id: String,
        glyph_id: u32,
    },
    /// One retained `ToUnicode` destination exceeds 512 UTF-16BE bytes.
    UnicodeMappingTooLong {
        object_id: String,
        glyph_id: u16,
    },
    InvalidResourceHash {
        source: AssetHash,
        face_index: u32,
    },
    DuplicateResource {
        source: AssetHash,
        face_index: u32,
    },
    MissingResource {
        source_sha256: String,
        face_index: u32,
    },
    FontWorker {
        source_sha256: String,
        face_index: u32,
        error: FontWorkerError,
    },
    IdentityDrift {
        source_sha256: String,
        face_index: u32,
    },
}

/// Prepares every font used by export pages from exact package resources.
/// Pasteboard-only text and unreferenced resources are deliberately excluded.
///
/// # Errors
///
/// Fails closed when the scene is incomplete, a run is not bound to supplied
/// exact bytes, a glyph cannot be represented by the preparation protocol, or
/// the confined worker's returned identity differs from the resolved scene.
pub fn prepare_publication_fonts(
    publication: &ResolvedPublication,
    resources: &[ExactFontResource],
    boundary: &TrustedFontBoundary,
) -> Result<Vec<ProductionFont>, ProductionFontError> {
    prepare_publication_fonts_with_font_facts(publication, resources, boundary, |_| (None, None))
}

/// Prepares scene-bound fonts with optional facts from each resource's author.
/// The callback receives the validated exact resource and supplies an optional
/// complete accented-source-GID set and an optional capital height in PDF glyph
/// units above the transformed baseline. Missing facts differ from an empty set
/// or zero. Their semantic truth remains the resource author's responsibility.
/// The wrapper retains control of the exact source bytes, face and scene GIDs.
/// Pasteboard-only text and unreferenced resources are excluded as before.
///
/// # Errors
///
/// Preserves automatic preparation's scene/resource/identity errors and native
/// fact validation errors. Contradictory facts cannot override computable metrics.
pub fn prepare_publication_fonts_with_font_facts<F>(
    publication: &ResolvedPublication,
    resources: &[ExactFontResource],
    boundary: &TrustedFontBoundary,
    mut facts_for: F,
) -> Result<Vec<ProductionFont>, ProductionFontError>
where
    F: FnMut(&ExactFontResource) -> (Option<Vec<u16>>, Option<f32>),
{
    if publication.budget_state() != PreviewBudgetState::Complete {
        return Err(ProductionFontError::PreviewBudgetIncomplete);
    }

    let resource_map = exact_resource_map(resources)?;
    let glyph_sets = collect_page_glyph_sets(publication)?;
    let scene_digest = publication.semantic_digest();
    let mut prepared = Vec::with_capacity(glyph_sets.len());

    for ((source_sha256, face_index), usage) in glyph_sets {
        let resource = resource_map
            .get(&(source_sha256.clone(), face_index))
            .ok_or_else(|| ProductionFontError::MissingResource {
                source_sha256: source_sha256.clone(),
                face_index,
            })?;
        let glyph_ids = usage.glyph_ids.into_iter().collect::<Vec<_>>();
        let (accented_glyph_ids, cap_height_pdf_units) = facts_for(resource);
        let program = boundary
            .prepare_publication_with_font_facts(
                &resource.bytes,
                face_index,
                &glyph_ids,
                accented_glyph_ids.as_deref(),
                cap_height_pdf_units,
            )
            .map_err(|error| ProductionFontError::FontWorker {
                source_sha256: source_sha256.clone(),
                face_index,
                error,
            })?;
        let expected_source =
            decode_sha256(&source_sha256).ok_or_else(|| ProductionFontError::IdentityDrift {
                source_sha256: source_sha256.clone(),
                face_index,
            })?;
        if program.source_sha256() != expected_source
            || program.face_index() != face_index
            || usize::from(program.used_glyph_count()) != glyph_ids.len()
        {
            return Err(ProductionFontError::IdentityDrift {
                source_sha256,
                face_index,
            });
        }
        let evidence = ProductionFontEvidence {
            scene_digest,
            source_sha256: program.source_sha256(),
            face_index,
            postscript_name_sha256: program.postscript_name_sha256(),
            preparation_kind: program.preparation_kind(),
            glyph_set_sha256: program.glyph_set_sha256(),
            used_glyph_count: program.used_glyph_count(),
            embedded_glyph_count: program.embedded_glyph_count(),
            prepared_font_sha256: program.prepared_sha256(),
            rights_identity_sha256: program.rights_identity_sha256(),
            worker_executable_sha256: program.worker_executable_sha256(),
            engine_identity_sha256: program.engine_identity_sha256(),
            rules_identity_sha256: program.rules_identity_sha256(),
        };
        let to_unicode = usage
            .unicode_by_source_glyph
            .into_iter()
            .map(|(source_glyph, codepoints)| {
                program
                    .remapped_glyphs()
                    .iter()
                    .find(|mapping| mapping.source_glyph_id() == source_glyph)
                    .map(|mapping| (mapping.cid(), codepoints))
                    .ok_or_else(|| ProductionFontError::IdentityDrift {
                        source_sha256: source_sha256.clone(),
                        face_index,
                    })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        prepared.push(ProductionFont {
            evidence,
            program: Arc::new(program),
            to_unicode: Arc::new(to_unicode),
        });
    }

    Ok(prepared)
}

fn exact_resource_map(
    resources: &[ExactFontResource],
) -> Result<BTreeMap<(String, u32), &ExactFontResource>, ProductionFontError> {
    let mut result = BTreeMap::new();
    for resource in resources {
        if AssetHash::for_bytes(&resource.bytes) != resource.source {
            return Err(ProductionFontError::InvalidResourceHash {
                source: resource.source.clone(),
                face_index: resource.face_index,
            });
        }
        let key = (resource.source.as_str().to_owned(), resource.face_index);
        if result.insert(key, resource).is_some() {
            return Err(ProductionFontError::DuplicateResource {
                source: resource.source.clone(),
                face_index: resource.face_index,
            });
        }
    }
    Ok(result)
}

fn collect_page_glyph_sets(
    publication: &ResolvedPublication,
) -> Result<BTreeMap<(String, u32), CollectedFontUsage>, ProductionFontError> {
    let mut result = BTreeMap::new();
    for object in publication.pages().iter().flat_map(|page| &page.objects) {
        match &object.kind {
            ResolvedSceneObjectKind::Text { layout, .. } => {
                collect_layout_glyphs(object.object_id.as_str(), layout, &mut result)?;
            }
            ResolvedSceneObjectKind::Table { table } => {
                for cell in &table.populated_cells {
                    collect_layout_glyphs(object.object_id.as_str(), &cell.layout, &mut result)?;
                }
            }
            ResolvedSceneObjectKind::Image { .. } | ResolvedSceneObjectKind::Shape { .. } => {}
        }
    }
    Ok(result)
}

fn validate_layout_readiness(
    object_id: &str,
    layout: &ResolvedTextBoxPreview,
) -> Result<(), ProductionFontError> {
    if layout.preview_state == TextBoxPreviewState::MissingFont {
        let unbound = layout.font_bindings.iter().find(|binding| {
            binding.substitution_status == layout_core::FontSubstitutionStatus::Unbound
        });
        if unbound.is_some()
            || layout.font_binding.substitution_status
                == layout_core::FontSubstitutionStatus::Unbound
        {
            return Err(ProductionFontError::FontBindingUnresolved {
                object_id: object_id.to_owned(),
                source_start: unbound.map_or(0, |binding| binding.source.start),
                source_end: unbound.map_or(0, |binding| binding.source.end),
            });
        }
    }
    if layout.preview_state != TextBoxPreviewState::Complete {
        return Err(ProductionFontError::TextPreviewIncomplete {
            object_id: object_id.to_owned(),
        });
    }
    Ok(())
}

fn collect_layout_glyphs(
    object_id: &str,
    layout: &ResolvedTextBoxPreview,
    result: &mut BTreeMap<(String, u32), CollectedFontUsage>,
) -> Result<(), ProductionFontError> {
    validate_layout_readiness(object_id, layout)?;
    validate_layout_font_contract(layout).map_err(|failure| match failure {
        FontContractFailure::Synthesis {
            source_start,
            source_end,
        } => ProductionFontError::UnsupportedFontSynthesis {
            object_id: object_id.to_owned(),
            source_start,
            source_end,
        },
        FontContractFailure::IdentityMismatch {
            source_start,
            source_end,
        } => ProductionFontError::FontBindingIdentityMismatch {
            object_id: object_id.to_owned(),
            source_start,
            source_end,
        },
        FontContractFailure::MissingResolvedFont { font_index } => {
            ProductionFontError::MissingResolvedFont {
                object_id: object_id.to_owned(),
                font_index,
            }
        }
        FontContractFailure::NonCanonicalBindings {
            source_start,
            source_end,
        } => ProductionFontError::NonCanonicalFontBindings {
            object_id: object_id.to_owned(),
            source_start,
            source_end,
        },
        FontContractFailure::UnresolvedBinding {
            source_start,
            source_end,
        } => ProductionFontError::FontBindingUnresolved {
            object_id: object_id.to_owned(),
            source_start,
            source_end,
        },
    })?;
    for run in layout.lines.iter().flat_map(|line| &line.glyph_runs) {
        let face = layout.resolved_fonts.get(run.font_index).ok_or_else(|| {
            ProductionFontError::MissingResolvedFont {
                object_id: object_id.to_owned(),
                font_index: run.font_index,
            }
        })?;
        let usage = result
            .entry((face.sha256_hex.clone(), face.collection_index))
            .or_default();
        // CID-keyed PDF fonts require the mandatory .notdef glyph even when
        // no shaped run references it directly. Include it in the attested
        // request so the requested glyph set and worker remap stay identical.
        usage.glyph_ids.insert(0);
        for glyph in &run.glyphs {
            if glyph.fallback.is_some() {
                return Err(ProductionFontError::UnsupportedGlyphFallback {
                    object_id: object_id.to_owned(),
                    glyph_id: glyph.id,
                });
            }
            let glyph_id =
                u16::try_from(glyph.id).map_err(|_| ProductionFontError::GlyphIdOutOfRange {
                    object_id: object_id.to_owned(),
                    glyph_id: glyph.id,
                })?;
            usage.glyph_ids.insert(glyph_id);
        }
        if let Some(first_glyph) = run.glyphs.first() {
            let glyph_id = u16::try_from(first_glyph.id).map_err(|_| {
                ProductionFontError::GlyphIdOutOfRange {
                    object_id: object_id.to_owned(),
                    glyph_id: first_glyph.id,
                }
            })?;
            let codepoints = run.glyph_text.chars().collect::<Vec<_>>();
            if !codepoints.is_empty() {
                // ToUnicode is a compatibility fallback keyed by PDF CID, so
                // one subset glyph can retain only one representative mapping.
                // The scene writer additionally wraps every shaped cluster in
                // occurrence-specific ActualText, which preserves the exact
                // source sequence when the same glyph represents another one.
                record_compatibility_unicode(
                    &mut usage.unicode_by_source_glyph,
                    glyph_id,
                    codepoints,
                )
                .map_err(|()| ProductionFontError::UnicodeMappingTooLong {
                    object_id: object_id.to_owned(),
                    glyph_id,
                })?;
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct CollectedFontUsage {
    glyph_ids: BTreeSet<u16>,
    unicode_by_source_glyph: BTreeMap<u16, Vec<char>>,
}

fn record_compatibility_unicode(
    mappings: &mut BTreeMap<u16, Vec<char>>,
    glyph_id: u16,
    codepoints: Vec<char>,
) -> Result<(), ()> {
    if let std::collections::btree_map::Entry::Vacant(entry) = mappings.entry(glyph_id) {
        if !crate::to_unicode::destination_within_limit(codepoints.iter().copied()) {
            return Err(());
        }
        entry.insert(codepoints);
    }
    Ok(())
}

fn decode_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut result = [0_u8; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        result[index] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::record_compatibility_unicode;

    #[test]
    fn one_glyph_can_serve_different_actual_text_sequences() {
        let mut mappings = BTreeMap::new();
        record_compatibility_unicode(&mut mappings, 36, vec!['a']).unwrap();
        record_compatibility_unicode(&mut mappings, 36, vec!['a', '\u{301}']).unwrap();

        assert_eq!(mappings.get(&36), Some(&vec!['a']));
    }

    #[test]
    fn compatibility_mapping_bounds_only_the_retained_unicode_destination() {
        for (c, count) in [('a', 256), ('😀', 128)] {
            let mut mappings = BTreeMap::new();
            assert!(record_compatibility_unicode(&mut mappings, 36, vec![c; count + 1]).is_err());
            assert!(mappings.is_empty());
            record_compatibility_unicode(&mut mappings, 36, vec![c; count]).unwrap();
            // A later occurrence can carry longer ActualText independently;
            // the already retained, bounded CID fallback does not change.
            record_compatibility_unicode(&mut mappings, 36, vec![c; count + 1]).unwrap();
            assert_eq!(mappings[&36], vec![c; count]);
        }
    }
}
