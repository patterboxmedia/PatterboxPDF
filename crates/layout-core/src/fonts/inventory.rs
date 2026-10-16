use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};

use fontique::{Collection, CollectionOptions, FontInfo, FontStyle, SourceInfo, SourceKind};
use sha2::{Digest, Sha256};

use super::{FontCatalogError, FontFaceFacts};

const MAX_RAW_FAMILY_NAMES: usize = 8_192;
const MAX_INSTALLED_FONT_FAMILIES: usize = 4_096;
const MAX_INSTALLED_FONT_FACES: usize = 16_384;
const MAX_INSTALLED_FONT_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_FONT_FAMILY_BYTES: usize = 1_024;
const MAX_FONT_FAMILY_CHARS: usize = 256;
const FONT_SELECTOR_DOMAIN: &[u8] = b"publisher-v2/installed-font-selector/v1\0";

/// A deterministic, path-free inventory of fonts exposed by one OS backend snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct InstalledFontInventory {
    pub backend_scope: InstalledFontBackendScope,
    pub families: Vec<String>,
    pub faces: Vec<InstalledFontFace>,
    pub issues: Vec<InstalledFontInventoryIssue>,
}

impl InstalledFontInventory {
    /// True when the bounded Fontique system-backend traversal completed.
    /// This does not claim that every Windows-installed font is representable.
    #[must_use]
    pub fn is_backend_complete(&self) -> bool {
        self.issues.is_empty()
    }

    /// Compatibility alias; completeness is limited to [`Self::backend_scope`].
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.is_backend_complete()
    }
}

/// The conservative backend boundary to which inventory completeness applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstalledFontBackendScope {
    /// Faces representable by Fontique's system-font backend. DirectWrite may
    /// expose additional sources that this backend cannot model.
    FontiqueSystemFonts,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InstalledFontInventoryIssue {
    FamilyMetadataUnavailable,
    RawFamilyLimitExceeded,
    FamilyLimitExceeded,
    FaceLimitExceeded,
    SelectorCollision,
}

/// Concrete metadata for one installed face. No source path is returned.
#[derive(Clone, Debug, PartialEq)]
pub struct InstalledFontFace {
    /// Opaque, snapshot-local selector for a later exact-face capture request.
    pub selector: String,
    pub family: String,
    pub style_label: String,
    pub style: InstalledFontStyle,
    pub weight: f32,
    pub stretch_percent: f32,
    pub collection_index: u32,
    pub capture: InstalledFontCapture,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InstalledFontStyle {
    Normal,
    Italic,
    Oblique,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstalledFontCapture {
    NotCaptured,
    Captured(FontFaceFacts),
    Unavailable(InstalledFontCaptureUnavailableReason),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InstalledFontCaptureUnavailableReason {
    SourceCouldNotBeRead,
    SourceTooLarge,
    CatalogByteBudgetExceeded,
}

/// A path-free face choice for a requested Publisher-style family and B/I state.
#[derive(Clone, Debug, PartialEq)]
pub struct InstalledFontMatch {
    pub selector: String,
    pub family: String,
    pub style: InstalledFontStyle,
    pub weight: f32,
    pub collection_index: u32,
}

/// Exact source bytes materialized only after a user chooses a font.
/// The operating-system path remains private to the catalog snapshot.
pub struct InstalledFontSourceMaterial {
    bytes: Vec<u8>,
    face_index: u32,
}

impl InstalledFontSourceMaterial {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub const fn face_index(&self) -> u32 {
        self.face_index
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Sanitized failures for native-only installed-font materialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstalledFontMaterializationError {
    UnknownSelector,
    SourceUnavailable,
    SourceNotRegularFile,
    SourceTooLarge,
}

/// Private source authority retained alongside the path-free public inventory.
/// Intentionally has no `Debug` implementation or source accessor.
#[derive(Clone)]
pub struct InstalledFontSourceFace {
    source: SourceInfo,
    face_index: u32,
}

impl InstalledFontSourceFace {
    #[must_use]
    pub const fn face_index(&self) -> u32 {
        self.face_index
    }

    fn source_id(&self) -> u64 {
        self.source.id().to_u64()
    }
}

/// One catalog generation and its private exact-source lookup table.
/// Keep this alive while its selectors are offered to callers.
pub struct InstalledFontCatalogSnapshot {
    inventory: InstalledFontInventory,
    sources_by_selector: BTreeMap<String, InstalledFontSourceFace>,
}

impl InstalledFontCatalogSnapshot {
    #[must_use]
    pub const fn inventory(&self) -> &InstalledFontInventory {
        &self.inventory
    }

    #[must_use]
    pub fn into_inventory(self) -> InstalledFontInventory {
        self.inventory
    }

    #[must_use]
    pub fn resolve(&self, selector: &str) -> Option<&InstalledFontSourceFace> {
        self.sources_by_selector.get(selector)
    }

    /// Chooses the closest concrete face without exposing its source authority.
    #[must_use]
    pub fn match_face(
        &self,
        family: &str,
        requested_weight: u16,
        italic: bool,
    ) -> Option<InstalledFontMatch> {
        let family = family.trim();
        if family.is_empty() {
            return None;
        }
        let folded_family = family.to_lowercase();
        self.inventory
            .faces
            .iter()
            .filter(|face| face.family.to_lowercase() == folded_family)
            .filter(|face| face_style_rank(face.style, italic) < 2)
            .min_by(|left, right| compare_face_match(left, right, requested_weight, italic))
            .map(|face| InstalledFontMatch {
                selector: face.selector.clone(),
                family: face.family.clone(),
                style: face.style,
                weight: face.weight,
                collection_index: face.collection_index,
            })
    }

    /// Reads one explicitly selected source under a fixed byte ceiling.
    ///
    /// # Errors
    ///
    /// Returns a sanitized error when the selector is stale, the source cannot
    /// be read as a regular file or memory blob, or the source exceeds the
    /// worker's input ceiling.
    pub fn materialize(
        &self,
        selector: &str,
    ) -> Result<InstalledFontSourceMaterial, InstalledFontMaterializationError> {
        let source = self
            .sources_by_selector
            .get(selector)
            .ok_or(InstalledFontMaterializationError::UnknownSelector)?;
        materialize_source(source, MAX_INSTALLED_FONT_SOURCE_BYTES)
    }
}

fn compare_face_match(
    left: &InstalledFontFace,
    right: &InstalledFontFace,
    requested_weight: u16,
    italic: bool,
) -> Ordering {
    face_style_rank(left.style, italic)
        .cmp(&face_style_rank(right.style, italic))
        .then_with(|| {
            (left.weight - f32::from(requested_weight))
                .abs()
                .total_cmp(&(right.weight - f32::from(requested_weight)).abs())
        })
        .then_with(|| {
            (left.stretch_percent - 100.0)
                .abs()
                .total_cmp(&(right.stretch_percent - 100.0).abs())
        })
        .then_with(|| compare_faces(left, right))
}

const fn face_style_rank(style: InstalledFontStyle, italic: bool) -> u8 {
    match (italic, style) {
        (false, InstalledFontStyle::Normal) | (true, InstalledFontStyle::Italic) => 0,
        (true, InstalledFontStyle::Oblique) => 1,
        _ => 2,
    }
}

fn materialize_source(
    source: &InstalledFontSourceFace,
    byte_limit: usize,
) -> Result<InstalledFontSourceMaterial, InstalledFontMaterializationError> {
    let bytes = match source.source.kind() {
        SourceKind::Memory(blob) => {
            if blob.len() > byte_limit {
                return Err(InstalledFontMaterializationError::SourceTooLarge);
            }
            blob.as_ref().to_vec()
        }
        SourceKind::Path(path) => {
            let path_metadata = std::fs::symlink_metadata(path)
                .map_err(|_| InstalledFontMaterializationError::SourceUnavailable)?;
            if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
                return Err(InstalledFontMaterializationError::SourceNotRegularFile);
            }
            if path_metadata.len() > byte_limit as u64 {
                return Err(InstalledFontMaterializationError::SourceTooLarge);
            }
            let mut file = File::open(path)
                .map_err(|_| InstalledFontMaterializationError::SourceUnavailable)?;
            let opened_metadata = file
                .metadata()
                .map_err(|_| InstalledFontMaterializationError::SourceUnavailable)?;
            if !opened_metadata.is_file() {
                return Err(InstalledFontMaterializationError::SourceNotRegularFile);
            }
            if opened_metadata.len() > byte_limit as u64 {
                return Err(InstalledFontMaterializationError::SourceTooLarge);
            }
            let mut bytes =
                Vec::with_capacity(usize::try_from(opened_metadata.len()).unwrap_or(byte_limit));
            file.by_ref()
                .take(u64::try_from(byte_limit).expect("font byte limit fits u64") + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| InstalledFontMaterializationError::SourceUnavailable)?;
            if bytes.len() > byte_limit {
                return Err(InstalledFontMaterializationError::SourceTooLarge);
            }
            bytes
        }
    };
    Ok(InstalledFontSourceMaterial {
        bytes,
        face_index: source.face_index,
    })
}

/// Enumerates metadata without eagerly reading every font file.
/// Paths remain private and exact facts stay absent until explicit capture.
///
/// # Errors
///
/// Returns [`FontCatalogError`] when the system-font backend cannot be enumerated safely.
pub fn installed_font_catalog_snapshot() -> Result<InstalledFontCatalogSnapshot, FontCatalogError> {
    catch_unwind(AssertUnwindSafe(|| {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: true,
        });
        snapshot_from_collection(&mut collection)
    }))
    .map_err(|_| FontCatalogError::SystemEnumerationFailed)
}

/// Compatibility helper for callers that do not need later exact-face capture.
///
/// # Errors
///
/// Returns [`FontCatalogError`] when the system-font backend cannot be enumerated safely.
pub fn installed_font_inventory() -> Result<InstalledFontInventory, FontCatalogError> {
    installed_font_catalog_snapshot().map(InstalledFontCatalogSnapshot::into_inventory)
}

fn snapshot_from_collection(collection: &mut Collection) -> InstalledFontCatalogSnapshot {
    let mut issues = BTreeSet::new();
    let family_lookups = bounded_family_lookups(collection.family_names(), &mut issues);
    let mut builder = SnapshotBuilder::default();

    for lookup in family_lookups.values() {
        let Some(family) = collection.family_by_name(&lookup.query) else {
            issues.insert(InstalledFontInventoryIssue::FamilyMetadataUnavailable);
            continue;
        };
        for font in family.fonts() {
            if builder.faces.len() >= MAX_INSTALLED_FONT_FACES {
                issues.insert(InstalledFontInventoryIssue::FaceLimitExceeded);
                break;
            }
            builder.add_face(&lookup.display, font, &mut issues);
        }
        if builder.faces.len() >= MAX_INSTALLED_FONT_FACES {
            break;
        }
    }

    builder.faces.sort_by(compare_faces);
    let families = selectable_family_names(&builder.faces);
    InstalledFontCatalogSnapshot {
        inventory: InstalledFontInventory {
            backend_scope: InstalledFontBackendScope::FontiqueSystemFonts,
            families,
            faces: builder.faces,
            issues: issues.into_iter().collect(),
        },
        sources_by_selector: builder.sources_by_selector,
    }
}

fn selectable_family_names(faces: &[InstalledFontFace]) -> Vec<String> {
    faces
        .iter()
        .map(|face| face.family.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Default)]
struct SnapshotBuilder {
    seen_faces: BTreeSet<(String, u64, u32)>,
    faces: Vec<InstalledFontFace>,
    sources_by_selector: BTreeMap<String, InstalledFontSourceFace>,
}

impl SnapshotBuilder {
    fn add_face(
        &mut self,
        family_name: &str,
        font: &FontInfo,
        issues: &mut BTreeSet<InstalledFontInventoryIssue>,
    ) {
        let source_id = font.source().id().to_u64();
        let face_index = font.index();
        if !self
            .seen_faces
            .insert((family_name.to_lowercase(), source_id, face_index))
        {
            return;
        }
        let selector = opaque_selector(source_id, face_index);
        let source_face = InstalledFontSourceFace {
            source: font.source().clone(),
            face_index,
        };
        if let Some(existing) = self.sources_by_selector.get(&selector) {
            if existing.source_id() != source_id || existing.face_index != face_index {
                issues.insert(InstalledFontInventoryIssue::SelectorCollision);
                return;
            }
        } else {
            self.sources_by_selector
                .insert(selector.clone(), source_face);
        }

        let style = normalize_style(font.style());
        let weight = font.weight().value();
        let stretch_percent = font.width().percentage();
        self.faces.push(InstalledFontFace {
            selector,
            family: family_name.to_owned(),
            style_label: style_label(weight, style, stretch_percent),
            style,
            weight,
            stretch_percent,
            collection_index: face_index,
            capture: InstalledFontCapture::NotCaptured,
        });
    }
}

struct FamilyLookup {
    display: String,
    query: String,
}

fn bounded_family_lookups<'a>(
    names: impl Iterator<Item = &'a str>,
    issues: &mut BTreeSet<InstalledFontInventoryIssue>,
) -> BTreeMap<String, FamilyLookup> {
    let mut lookups = BTreeMap::new();
    for (index, raw_name) in names.enumerate() {
        if index >= MAX_RAW_FAMILY_NAMES {
            issues.insert(InstalledFontInventoryIssue::RawFamilyLimitExceeded);
            break;
        }
        let trimmed = raw_name.trim();
        if trimmed.is_empty()
            || trimmed.len() > MAX_FONT_FAMILY_BYTES
            || trimmed.chars().count() > MAX_FONT_FAMILY_CHARS
        {
            continue;
        }
        let folded = trimmed.to_lowercase();
        match lookups.entry(folded) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(FamilyLookup {
                    display: trimmed.to_owned(),
                    query: raw_name.to_owned(),
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if trimmed < entry.get().display.as_str() {
                    entry.insert(FamilyLookup {
                        display: trimmed.to_owned(),
                        query: raw_name.to_owned(),
                    });
                }
            }
        }
    }
    if lookups.len() > MAX_INSTALLED_FONT_FAMILIES {
        issues.insert(InstalledFontInventoryIssue::FamilyLimitExceeded);
        let split_key = lookups
            .keys()
            .nth(MAX_INSTALLED_FONT_FAMILIES)
            .expect("family limit split key exists")
            .clone();
        let _discarded = lookups.split_off(&split_key);
    }
    lookups
}

fn opaque_selector(source_id: u64, face_index: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FONT_SELECTOR_DOMAIN);
    hasher.update(source_id.to_be_bytes());
    hasher.update(face_index.to_be_bytes());
    let mut selector = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut selector, "{byte:02x}").expect("writing to a String cannot fail");
    }
    selector
}

fn normalize_style(style: FontStyle) -> InstalledFontStyle {
    match style {
        FontStyle::Normal => InstalledFontStyle::Normal,
        FontStyle::Italic => InstalledFontStyle::Italic,
        FontStyle::Oblique(_) => InstalledFontStyle::Oblique,
    }
}

fn style_label(weight: f32, style: InstalledFontStyle, stretch_percent: f32) -> String {
    let weight_label = [
        (100.0, "Thin"),
        (200.0, "Extra Light"),
        (300.0, "Light"),
        (350.0, "Semi Light"),
        (400.0, "Regular"),
        (500.0, "Medium"),
        (600.0, "Semi Bold"),
        (700.0, "Bold"),
        (800.0, "Extra Bold"),
        (900.0, "Black"),
        (950.0, "Extra Black"),
    ]
    .into_iter()
    .find_map(|(candidate, label)| weight.total_cmp(&candidate).is_eq().then_some(label));
    let weight = weight_label.map_or_else(|| format!("Weight {weight}"), str::to_owned);
    let slope = match style {
        InstalledFontStyle::Normal => "",
        InstalledFontStyle::Italic => " Italic",
        InstalledFontStyle::Oblique => " Oblique",
    };
    if stretch_percent.total_cmp(&100.0).is_eq() {
        format!("{weight}{slope}")
    } else {
        format!("{weight}{slope} ({stretch_percent}% width)")
    }
}

fn compare_faces(left: &InstalledFontFace, right: &InstalledFontFace) -> Ordering {
    left.family
        .cmp(&right.family)
        .then_with(|| left.stretch_percent.total_cmp(&right.stretch_percent))
        .then_with(|| left.style.cmp(&right.style))
        .then_with(|| left.weight.total_cmp(&right.weight))
        .then_with(|| left.collection_index.cmp(&right.collection_index))
        .then_with(|| left.selector.cmp(&right.selector))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicU64, Ordering};

    use fontique::{Blob, Collection, CollectionOptions, FontInfoOverride, FontStyle, FontWeight};

    use super::{
        InstalledFontBackendScope, InstalledFontCapture, InstalledFontCatalogSnapshot,
        InstalledFontInventoryIssue, InstalledFontMaterializationError, InstalledFontStyle,
        MAX_INSTALLED_FONT_FAMILIES, MAX_RAW_FAMILY_NAMES, SnapshotBuilder, bounded_family_lookups,
        materialize_source, selectable_family_names, snapshot_from_collection,
    };

    const FONT: &[u8] = include_bytes!("../../tests/fixtures/DejaVuSans.ttf");
    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn empty_collection() -> Collection {
        Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        })
    }

    fn two_face_collection(font: &[u8]) -> Vec<u8> {
        fn relocated_face(font: &[u8], base: usize) -> Vec<u8> {
            let mut face = font.to_vec();
            let table_count = usize::from(u16::from_be_bytes([face[4], face[5]]));
            for table_index in 0..table_count {
                let record = 12 + table_index * 16;
                let offset = u32::from_be_bytes([
                    face[record + 8],
                    face[record + 9],
                    face[record + 10],
                    face[record + 11],
                ]);
                let relocated = offset
                    .checked_add(u32::try_from(base).expect("test collection offset fits u32"))
                    .expect("test collection table offset remains bounded");
                face[record + 8..record + 12].copy_from_slice(&relocated.to_be_bytes());
            }
            face
        }

        const HEADER_LEN: usize = 20;
        let first_offset = HEADER_LEN;
        let second_offset = first_offset + font.len();
        let mut collection = Vec::with_capacity(HEADER_LEN + font.len() * 2);
        collection.extend_from_slice(b"ttcf");
        collection.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
        collection.extend_from_slice(&2_u32.to_be_bytes());
        collection.extend_from_slice(&u32::try_from(first_offset).unwrap().to_be_bytes());
        collection.extend_from_slice(&u32::try_from(second_offset).unwrap().to_be_bytes());
        collection.extend_from_slice(&relocated_face(font, first_offset));
        collection.extend_from_slice(&relocated_face(font, second_offset));
        collection
    }

    #[test]
    fn inventory_preserves_distinct_sources_with_identical_metadata() {
        let mut collection = empty_collection();
        for family_name in ["Zeta", "Alpha", "Alpha"] {
            collection.register_fonts(
                Blob::from(FONT.to_vec()),
                Some(FontInfoOverride {
                    family_name: Some(family_name),
                    ..FontInfoOverride::default()
                }),
            );
        }

        let snapshot = snapshot_from_collection(&mut collection);
        let inventory = snapshot.inventory();
        assert_eq!(
            inventory.backend_scope,
            InstalledFontBackendScope::FontiqueSystemFonts
        );
        assert_eq!(inventory.families, ["Alpha", "Zeta"]);
        assert_eq!(inventory.faces.len(), 3);
        assert_eq!(
            inventory
                .faces
                .iter()
                .filter(|face| face.family == "Alpha")
                .count(),
            2
        );
        assert_eq!(
            inventory
                .faces
                .iter()
                .map(|face| &face.selector)
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
        assert!(inventory.is_backend_complete());
        assert!(
            inventory
                .faces
                .iter()
                .all(|face| matches!(face.capture, InstalledFontCapture::NotCaptured))
        );
        for face in &inventory.faces {
            assert_eq!(
                snapshot
                    .resolve(&face.selector)
                    .map(super::InstalledFontSourceFace::face_index),
                Some(face.collection_index)
            );
        }
    }

    #[test]
    fn collection_faces_keep_exact_indices_and_private_resolvers() {
        let mut collection = empty_collection();
        let registered = collection.register_fonts(
            Blob::from(two_face_collection(FONT)),
            Some(FontInfoOverride {
                family_name: Some("Collection Test"),
                ..FontInfoOverride::default()
            }),
        );
        assert_eq!(registered[0].1.len(), 2);
        let snapshot = snapshot_from_collection(&mut collection);
        let inventory = snapshot.inventory();
        assert_eq!(
            inventory
                .faces
                .iter()
                .map(|face| face.collection_index)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_ne!(inventory.faces[0].selector, inventory.faces[1].selector);
    }

    #[test]
    fn identical_source_and_face_index_is_deduplicated() {
        let mut collection = empty_collection();
        let registered = collection.register_fonts(
            Blob::from(FONT.to_vec()),
            Some(FontInfoOverride {
                family_name: Some("Same Source"),
                ..FontInfoOverride::default()
            }),
        );
        let font = &registered[0].1[0];
        let mut builder = SnapshotBuilder::default();
        let mut issues = BTreeSet::new();
        builder.add_face("Same Source", font, &mut issues);
        builder.add_face("Same Source", font, &mut issues);
        assert_eq!(builder.faces.len(), 1);
        assert_eq!(builder.sources_by_selector.len(), 1);
        assert!(issues.is_empty());
    }

    #[test]
    fn aliases_for_one_source_face_remain_independently_selectable() {
        let mut collection = empty_collection();
        let registered = collection.register_fonts(
            Blob::from(FONT.to_vec()),
            Some(FontInfoOverride {
                family_name: Some("Localized Family"),
                ..FontInfoOverride::default()
            }),
        );
        let font = &registered[0].1[0];
        let mut builder = SnapshotBuilder::default();
        let mut issues = BTreeSet::new();
        builder.add_face("Localized Family", font, &mut issues);
        builder.add_face("Famille localisee", font, &mut issues);
        let snapshot = InstalledFontCatalogSnapshot {
            inventory: super::InstalledFontInventory {
                backend_scope: InstalledFontBackendScope::FontiqueSystemFonts,
                families: vec![
                    "Famille localisee".to_owned(),
                    "Localized Family".to_owned(),
                ],
                faces: builder.faces,
                issues: issues.into_iter().collect(),
            },
            sources_by_selector: builder.sources_by_selector,
        };

        let localized = snapshot
            .match_face("Famille localisee", 400, false)
            .expect("localized alias match");
        let default = snapshot
            .match_face("Localized Family", 400, false)
            .expect("default alias match");
        assert_eq!(localized.selector, default.selector);
        assert_eq!(snapshot.sources_by_selector.len(), 1);
    }

    #[test]
    fn exposed_families_are_derived_only_from_selectable_face_rows() {
        let faces = vec![super::InstalledFontFace {
            selector: "selector".to_owned(),
            family: "Selectable".to_owned(),
            style_label: "Regular".to_owned(),
            style: InstalledFontStyle::Normal,
            weight: 400.0,
            stretch_percent: 100.0,
            collection_index: 0,
            capture: InstalledFontCapture::NotCaptured,
        }];
        assert_eq!(selectable_family_names(&faces), ["Selectable"]);
        assert!(!selectable_family_names(&faces).contains(&"Unvisited after limit".to_owned()));
    }

    #[test]
    fn raw_family_iteration_is_bounded_before_allocation() {
        let names = (0..=MAX_RAW_FAMILY_NAMES)
            .map(|index| format!("Family {:02}", index % 10))
            .collect::<Vec<_>>();
        let mut issues = BTreeSet::new();
        let lookups = bounded_family_lookups(names.iter().map(String::as_str), &mut issues);
        assert_eq!(lookups.len(), 10);
        assert!(issues.contains(&InstalledFontInventoryIssue::RawFamilyLimitExceeded));
        assert!(!issues.contains(&InstalledFontInventoryIssue::FamilyLimitExceeded));
    }

    #[test]
    fn unique_family_count_is_bounded_after_raw_names_are_bounded() {
        let names = (0..MAX_RAW_FAMILY_NAMES)
            .map(|index| format!("Family {index:05}"))
            .collect::<Vec<_>>();
        let mut issues = BTreeSet::new();
        let lookups = bounded_family_lookups(names.iter().map(String::as_str), &mut issues);
        assert_eq!(lookups.len(), MAX_INSTALLED_FONT_FAMILIES);
        assert!(issues.contains(&InstalledFontInventoryIssue::FamilyLimitExceeded));
    }

    #[test]
    fn metadata_snapshot_does_not_eagerly_reopen_source_files() {
        let unique = format!(
            "publisher-v2-font-inventory-{}-{}.ttf",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, FONT).expect("write temporary font");
        let mut collection = empty_collection();
        collection.load_fonts_from_paths([&path]);
        std::fs::remove_file(&path).expect("remove temporary font");
        let snapshot = snapshot_from_collection(&mut collection);
        assert_eq!(snapshot.inventory().faces.len(), 1);
        assert!(matches!(
            snapshot.inventory().faces[0].capture,
            InstalledFontCapture::NotCaptured
        ));
        assert!(snapshot.inventory().issues.is_empty());
    }

    #[test]
    fn private_snapshot_is_safe_to_retain_in_desktop_state() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InstalledFontCatalogSnapshot>();
    }

    #[test]
    fn requested_family_weight_and_italic_choose_one_deterministic_face() {
        let mut collection = empty_collection();
        for (weight, style) in [
            (FontWeight::NORMAL, FontStyle::Normal),
            (FontWeight::BOLD, FontStyle::Normal),
            (FontWeight::NORMAL, FontStyle::Italic),
        ] {
            collection.register_fonts(
                Blob::from(FONT.to_vec()),
                Some(FontInfoOverride {
                    family_name: Some("Magic Sans"),
                    weight: Some(weight),
                    style: Some(style),
                    ..FontInfoOverride::default()
                }),
            );
        }
        let snapshot = snapshot_from_collection(&mut collection);

        let bold = snapshot
            .match_face(" magic sans ", 700, false)
            .expect("bold family match");
        assert!(bold.weight.total_cmp(&700.0).is_eq());
        assert_eq!(bold.style, InstalledFontStyle::Normal);
        let italic = snapshot
            .match_face("MAGIC SANS", 400, true)
            .expect("italic family match");
        assert!(italic.weight.total_cmp(&400.0).is_eq());
        assert_eq!(italic.style, InstalledFontStyle::Italic);
        assert!(snapshot.match_face("Unknown", 400, false).is_none());
    }

    #[test]
    fn face_matching_never_crosses_the_requested_slant() {
        let mut normal_only = empty_collection();
        normal_only.register_fonts(
            Blob::from(FONT.to_vec()),
            Some(FontInfoOverride {
                family_name: Some("Normal Only"),
                weight: Some(FontWeight::NORMAL),
                style: Some(FontStyle::Normal),
                ..FontInfoOverride::default()
            }),
        );
        let normal_only = snapshot_from_collection(&mut normal_only);
        assert!(normal_only.match_face("Normal Only", 400, true).is_none());

        let mut italic_only = empty_collection();
        italic_only.register_fonts(
            Blob::from(FONT.to_vec()),
            Some(FontInfoOverride {
                family_name: Some("Italic Only"),
                weight: Some(FontWeight::NORMAL),
                style: Some(FontStyle::Italic),
                ..FontInfoOverride::default()
            }),
        );
        let italic_only = snapshot_from_collection(&mut italic_only);
        assert!(italic_only.match_face("Italic Only", 400, false).is_none());
    }

    #[test]
    fn selected_memory_source_materializes_exact_bytes_only_on_request() {
        let mut collection = empty_collection();
        collection.register_fonts(
            Blob::from(FONT.to_vec()),
            Some(FontInfoOverride {
                family_name: Some("Memory Face"),
                ..FontInfoOverride::default()
            }),
        );
        let snapshot = snapshot_from_collection(&mut collection);
        let selected = snapshot
            .match_face("Memory Face", 400, false)
            .expect("memory face match");
        let material = snapshot
            .materialize(&selected.selector)
            .expect("materialize selected memory face");
        assert_eq!(material.bytes(), FONT);
        assert_eq!(material.face_index(), selected.collection_index);
        assert_eq!(
            snapshot.materialize("unknown-selector").err(),
            Some(InstalledFontMaterializationError::UnknownSelector)
        );
    }

    #[test]
    fn materialization_is_bounded_and_disappearing_paths_fail_without_path_text() {
        let unique = format!(
            "publisher-v2-font-materialize-{}-{}.ttf",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::write(&path, FONT).expect("write temporary font");
        let mut collection = empty_collection();
        collection.load_fonts_from_paths([&path]);
        let snapshot = snapshot_from_collection(&mut collection);
        let selector = snapshot.inventory().faces[0].selector.clone();
        let source = snapshot
            .resolve(&selector)
            .expect("private selected source");
        assert_eq!(
            materialize_source(source, FONT.len() - 1).err(),
            Some(InstalledFontMaterializationError::SourceTooLarge)
        );

        std::fs::remove_file(&path).expect("remove temporary font");
        let error = snapshot
            .materialize(&selector)
            .err()
            .expect("vanished source must fail");
        assert_eq!(error, InstalledFontMaterializationError::SourceUnavailable);
        assert!(!format!("{error:?}").contains(path.to_string_lossy().as_ref()));
    }
}
