//! Engine-neutral rich-text composition backed by Parley.
//!
//! All public geometry is integer millipoints. Parley's floating-point output
//! is an implementation detail and is canonicalized at this boundary.

use core::ops::Range;
use document_core::AssetHash;
use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontStyle, FontWeight, Layout,
    LayoutContext, LineHeight, StyleProperty, YieldData,
};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

/// Canonical text-layout unit: one thousandth of a typographic point.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Millipoints(i64);

impl Millipoints {
    /// Creates a canonical length from an integer millipoint count.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Returns the integer millipoint count.
    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }

    #[allow(clippy::cast_precision_loss)]
    fn to_points(self) -> f32 {
        self.0 as f32 / 1_000.0
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn from_parley(value: f32) -> Result<Self, TextLayoutError> {
        if !value.is_finite() {
            return Err(TextLayoutError::NonFiniteEngineGeometry);
        }
        let scaled = f64::from(value) * 1_000.0;
        if scaled < i64::MIN as f64 || scaled > i64::MAX as f64 {
            return Err(TextLayoutError::EngineGeometryOutOfRange);
        }
        Ok(Self(scaled.round() as i64))
    }
}

/// Axis-aligned rectangle in canonical text-layout units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextRect {
    pub x: Millipoints,
    pub y: Millipoints,
    pub width: Millipoints,
    pub height: Millipoints,
}

impl TextRect {
    /// Creates a positive-sized rectangle.
    ///
    /// # Errors
    ///
    /// Returns [`TextLayoutError::NonPositiveRegion`] when `width` or `height`
    /// is zero or negative.
    pub fn new(x: i64, y: i64, width: i64, height: i64) -> Result<Self, TextLayoutError> {
        if width <= 0 || height <= 0 {
            return Err(TextLayoutError::NonPositiveRegion);
        }
        Ok(Self {
            x: Millipoints::new(x),
            y: Millipoints::new(y),
            width: Millipoints::new(width),
            height: Millipoints::new(height),
        })
    }

    fn right(self) -> i64 {
        self.x.value().saturating_add(self.width.value())
    }

    fn bottom(self) -> i64 {
        self.y.value().saturating_add(self.height.value())
    }
}

/// A byte range in the source story.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRange {
    pub start: usize,
    pub end: usize,
}

impl From<Range<usize>> for SourceRange {
    fn from(value: Range<usize>) -> Self {
        Self {
            start: value.start,
            end: value.end,
        }
    }
}

impl SourceRange {
    fn as_range(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// Complete styling applied to a story span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoryStyle {
    /// Caller-owned stable identity retained in the resolved output.
    pub id: u32,
    /// CSS-like font-family source accepted by Parley, for example `"Arial"`.
    pub font_family: String,
    pub font_size: Millipoints,
    /// OpenType/CSS weight in the inclusive 1..=1000 range.
    pub font_weight: u16,
    pub italic: bool,
    pub color: document_core::model::Color,
    pub underline: bool,
    /// Line height as a per-thousand multiplier of the font size.
    pub line_height_per_thousand: u16,
}

impl StoryStyle {
    /// Creates an ordinary 12 pt style with a 1.2 line-height multiplier.
    #[must_use]
    pub fn new(id: u32, family: impl Into<String>) -> Self {
        Self {
            id,
            font_family: family.into(),
            font_size: Millipoints::new(12_000),
            font_weight: 400,
            italic: false,
            color: document_core::model::Color {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            underline: false,
            line_height_per_thousand: 1_200,
        }
    }

    fn validate(&self) -> Result<(), TextLayoutError> {
        if self.font_family.trim().is_empty() {
            return Err(TextLayoutError::EmptyFontFamily);
        }
        if self.font_size.value() <= 0 {
            return Err(TextLayoutError::NonPositiveFontSize);
        }
        if !(1..=1_000).contains(&self.font_weight) {
            return Err(TextLayoutError::FontWeightOutOfRange);
        }
        if self.line_height_per_thousand == 0 {
            return Err(TextLayoutError::NonPositiveLineHeight);
        }
        Ok(())
    }
}

/// One styled byte range within a story.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorySpan {
    pub source: SourceRange,
    pub style: StoryStyle,
}

/// Rich source content, independent of any text-region geometry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextStory {
    pub text: String,
    pub base_style: StoryStyle,
    pub spans: Vec<StorySpan>,
}

/// Bounded text-region geometry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextRegion {
    pub bounds: TextRect,
    pub column_count: u16,
    pub column_gap: Millipoints,
    /// Same-page rectangular image/shape wrap exclusions.
    pub exclusions: Vec<TextRect>,
}

/// Exact font resource used by a resolved run.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResolvedFont {
    /// SHA-256 of the exact font file/collection bytes.
    pub sha256: [u8; 32],
    /// Face index for TTC/OTC collections, zero for a single-font file.
    pub collection_index: u32,
}

/// Application-owned bytes for one exact embedded font face.
///
/// Layout never locates these bytes by path or by installed family name. The
/// caller must supply the package resource that was already validated at its
/// trust boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactFontResource {
    pub source: AssetHash,
    pub face_index: u32,
    pub bytes: Vec<u8>,
}

/// Why a supplied exact embedded font cannot be made selectable by Parley.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactFontRegistrationError {
    SourceHashMismatch,
    FaceUnavailable,
    NoFacesRegistered,
    InvalidFallbackScriptTag,
}

/// One canonical glyph placement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedGlyph {
    pub id: u32,
    pub x: Millipoints,
    pub y: Millipoints,
    pub advance: Millipoints,
    /// Canonical outline geometry in millipoints relative to the glyph origin.
    /// A missing outline is expected for whitespace and may occur for color glyphs.
    pub outline: Option<Arc<ResolvedGlyphOutline>>,
    /// A visible preview placeholder is required when the selected face cannot
    /// paint this source as a monochrome outline. The reason remains explicit
    /// so missing text is never confused with unsupported color artwork.
    pub fallback: Option<ResolvedGlyphFallback>,
}

/// Why a glyph needs an honest visible placeholder in outline-only previews.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolvedGlyphFallback {
    /// The shaping result used OpenType glyph zero (`.notdef`) for source text.
    Missing,
    /// The selected glyph is painted through OpenType COLR/CPAL layers.
    Colr,
    /// The selected glyph is painted through an embedded SVG document.
    Svg,
    /// The selected glyph is painted through an embedded bitmap strike.
    Bitmap,
}

/// Renderer-neutral vector outline for one resolved glyph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedGlyphOutline {
    pub commands: Vec<ResolvedOutlineCommand>,
}

/// One canonical vector path command. Font coordinates are scaled to the
/// resolved font size, but retain the font convention of positive Y pointing up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedOutlineCommand {
    MoveTo {
        x: Millipoints,
        y: Millipoints,
    },
    LineTo {
        x: Millipoints,
        y: Millipoints,
    },
    QuadTo {
        control_x: Millipoints,
        control_y: Millipoints,
        x: Millipoints,
        y: Millipoints,
    },
    CurveTo {
        control_1_x: Millipoints,
        control_1_y: Millipoints,
        control_2_x: Millipoints,
        control_2_y: Millipoints,
        x: Millipoints,
        y: Millipoints,
    },
    Close,
}

/// Paragraph alignment applied by the native layout engine after line breaking.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HorizontalAlignment {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// One generated paragraph marker. Markers are never inserted into the
/// author's UTF-8 story; they retain their own source identity so body ranges,
/// selections, and overset evidence remain canonical.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoryListMarker {
    Bullet,
    Numbered(u32),
}

impl StoryListMarker {
    fn text(self) -> String {
        match self {
            Self::Bullet => "\u{2022}".to_owned(),
            Self::Numbered(number) => format!("{number}."),
        }
    }
}

/// Fully resolved formatting for one source paragraph.
///
/// The document layer owns durable sparse overrides. Layout receives one
/// concrete entry per LF-delimited paragraph so paragraph interpretation is
/// settled before geometry is composed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoryParagraphStyle {
    pub list_marker: Option<StoryListMarker>,
    pub alignment: HorizontalAlignment,
    pub start_indent: Millipoints,
    pub end_indent: Millipoints,
    pub first_line_indent: Millipoints,
    pub line_height_per_thousand: u16,
    pub space_before: Millipoints,
    pub space_after: Millipoints,
}

impl StoryParagraphStyle {
    /// Resolves a paragraph with no indentation or spacing.
    #[must_use]
    pub const fn new(alignment: HorizontalAlignment, line_height_per_thousand: u16) -> Self {
        Self {
            list_marker: None,
            alignment,
            start_indent: Millipoints::new(0),
            end_indent: Millipoints::new(0),
            first_line_indent: Millipoints::new(0),
            line_height_per_thousand,
            space_before: Millipoints::new(0),
            space_after: Millipoints::new(0),
        }
    }

    fn validate(self) -> Result<(), TextLayoutError> {
        if self.start_indent.value() < 0
            || self.end_indent.value() < 0
            || self.space_before.value() < 0
            || self.space_after.value() < 0
        {
            return Err(TextLayoutError::NegativeParagraphGeometry);
        }
        if self.line_height_per_thousand == 0 {
            return Err(TextLayoutError::NonPositiveLineHeight);
        }
        Ok(())
    }
}

/// A source-addressable glyph run. The spike intentionally emits cluster-sized
/// runs so the byte range is exact even for bidi text and ligatures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedGlyphRun {
    pub source: SourceRange,
    /// Complete shaping cluster represented by the glyphs, including source
    /// components that have no separate glyph (ligatures and composed accents).
    pub glyph_source: SourceRange,
    pub style_id: u32,
    pub font_index: usize,
    pub font_size: Millipoints,
    /// Fontique requested faux emboldening, skew, or variation coordinates
    /// that are not yet applied by the canonical outline extractor.
    pub requires_font_synthesis: bool,
    pub right_to_left: bool,
    pub glyphs: Vec<ResolvedGlyph>,
}

/// A contiguous usable horizontal fragment of a line box.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTextFragment {
    pub source: SourceRange,
    pub x: Millipoints,
    pub width: Millipoints,
    pub glyph_runs: Vec<ResolvedGlyphRun>,
}

/// One resolved line in visual placement order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTextLine {
    pub source: SourceRange,
    pub column_index: u16,
    pub y: Millipoints,
    pub height: Millipoints,
    pub baseline: Millipoints,
    pub fragments: Vec<ResolvedTextFragment>,
}

/// Exact unplaced tail of the source story.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OversetText {
    pub source: SourceRange,
}

/// Immutable, renderer-neutral text composition result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedTextLayout {
    pub source_len_bytes: usize,
    pub fonts: Vec<ResolvedFont>,
    pub lines: Vec<ResolvedTextLine>,
    /// Generated markers with marker-local source ranges and authoritative
    /// glyph geometry. Their paragraph indices bind them back to the story
    /// without pretending that marker bytes exist in the durable source.
    pub list_markers: Vec<ResolvedListMarker>,
    pub overset: Option<OversetText>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedListMarker {
    pub paragraph_index: u32,
    pub source_anchor: usize,
    pub text: String,
    pub layout: ResolvedTextLayout,
}

/// Complete unbounded composition used by headline-like Fit text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFitTextLayout {
    pub layout: ResolvedTextLayout,
    pub width: Millipoints,
    pub height: Millipoints,
}

/// Reusable shaping resources. It owns no publication state.
pub struct TextLayoutEngine {
    system_fonts_enabled: bool,
    fonts: FontContext,
    layout: LayoutContext<u32>,
    exact_font_families: BTreeMap<(AssetHash, u32), String>,
    // Fontique blobs created from owned immutable bytes carry unique IDs. Cache
    // their verified output digest for this engine, across independent cells.
    resolved_font_hashes: HashMap<u64, [u8; 32]>,
    exact_fallback_chains: BTreeMap<[u8; 4], Vec<(AssetHash, u32)>>,
}

impl Default for TextLayoutEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextLayoutEngine {
    /// Creates an engine with system-font discovery enabled by the pinned Parley route.
    #[must_use]
    pub fn new() -> Self {
        Self {
            system_fonts_enabled: true,
            fonts: FontContext::new(),
            layout: LayoutContext::new(),
            exact_font_families: BTreeMap::new(),
            resolved_font_hashes: HashMap::new(),
            exact_fallback_chains: BTreeMap::new(),
        }
    }

    /// Creates an engine whose results cannot depend on fonts installed on the
    /// host. Exact faces and any permitted fallback order must be registered
    /// explicitly before composition.
    #[must_use]
    pub fn new_without_system_fonts() -> Self {
        Self {
            system_fonts_enabled: false,
            fonts: FontContext {
                collection: fontique::Collection::new(fontique::CollectionOptions {
                    shared: false,
                    system_fonts: false,
                }),
                source_cache: fontique::SourceCache::default(),
            },
            layout: LayoutContext::new(),
            exact_font_families: BTreeMap::new(),
            resolved_font_hashes: HashMap::new(),
            exact_fallback_chains: BTreeMap::new(),
        }
    }

    pub(crate) const fn system_fonts_enabled(&self) -> bool {
        self.system_fonts_enabled
    }

    /// Registers an application-supplied exact font under a private family
    /// name and returns that name for use in one bound story style.
    ///
    /// The private family name prevents a bound story from falling back to a
    /// same-named installed font. Multi-face collections are still checked
    /// against the actual glyph-run identity after shaping by the publication
    /// resolver; Fontique has no direct collection-index selector.
    ///
    /// # Errors
    ///
    /// Returns an error instead of accepting bytes whose digest or requested
    /// face cannot be verified, or that Fontique declines to register.
    pub fn register_exact_font(
        &mut self,
        resource: &ExactFontResource,
    ) -> Result<String, ExactFontRegistrationError> {
        if AssetHash::for_bytes(&resource.bytes) != resource.source {
            return Err(ExactFontRegistrationError::SourceHashMismatch);
        }
        if ttf_parser::Face::parse(&resource.bytes, resource.face_index).is_err() {
            return Err(ExactFontRegistrationError::FaceUnavailable);
        }
        let key = (resource.source.clone(), resource.face_index);
        if let Some(family) = self.exact_font_families.get(&key) {
            return Ok(family.clone());
        }
        let family = format!(
            "Publisher V2 Bound {} {}",
            resource.source.as_str(),
            resource.face_index
        );
        let registered = self.fonts.collection.register_fonts(
            fontique::Blob::from(resource.bytes.clone()),
            Some(fontique::FontInfoOverride {
                family_name: Some(&family),
                ..fontique::FontInfoOverride::default()
            }),
        );
        if registered.is_empty() {
            return Err(ExactFontRegistrationError::NoFacesRegistered);
        }
        self.exact_font_families.insert(key, family.clone());
        Ok(family)
    }

    /// Registers one exact face as the sole fallback for an OpenType script
    /// tag in this engine. The fallback remains hash- and face-index-bound in
    /// resolved output; no installed font can enter an engine created with
    /// [`Self::new_without_system_fonts`].
    ///
    /// # Errors
    ///
    /// Returns the same validation failures as [`Self::register_exact_font`],
    /// or [`ExactFontRegistrationError::InvalidFallbackScriptTag`] when the
    /// supplied tag is not a four-letter OpenType script tag.
    pub fn register_exact_fallback_font(
        &mut self,
        resource: &ExactFontResource,
        script_tag: [u8; 4],
    ) -> Result<String, ExactFontRegistrationError> {
        self.set_exact_fallback_fonts(script_tag, core::iter::once(resource))?
            .into_iter()
            .next()
            .ok_or(ExactFontRegistrationError::NoFacesRegistered)
    }

    /// Replaces one script's fallback order with exact application-supplied faces.
    ///
    /// Registration is completed before Fontique's active order changes. Resolved
    /// publication evidence can therefore distinguish these explicitly permitted
    /// faces from an uncontrolled machine-local substitution.
    ///
    /// # Errors
    ///
    /// Returns an error if the tag or any exact resource cannot be verified and
    /// registered by the pinned engine.
    pub fn set_exact_fallback_fonts<'a>(
        &mut self,
        script_tag: [u8; 4],
        resources: impl IntoIterator<Item = &'a ExactFontResource>,
    ) -> Result<Vec<String>, ExactFontRegistrationError> {
        if !script_tag.iter().all(u8::is_ascii_alphabetic) {
            return Err(ExactFontRegistrationError::InvalidFallbackScriptTag);
        }
        let mut families = Vec::new();
        let mut family_ids = Vec::new();
        let mut identities = Vec::new();
        for resource in resources {
            let family = self.register_exact_font(resource)?;
            let family_id = self
                .fonts
                .collection
                .family_id(&family)
                .ok_or(ExactFontRegistrationError::NoFacesRegistered)?;
            families.push(family);
            family_ids.push(family_id);
            identities.push((resource.source.clone(), resource.face_index));
        }
        if families.is_empty() {
            return Err(ExactFontRegistrationError::NoFacesRegistered);
        }
        if !self.fonts.collection.set_fallbacks(
            fontique::Script::from_bytes(script_tag),
            family_ids.into_iter(),
        ) {
            return Err(ExactFontRegistrationError::InvalidFallbackScriptTag);
        }
        self.exact_fallback_chains.insert(script_tag, identities);
        Ok(families)
    }

    /// Exact face identities currently permitted by any configured fallback chain.
    pub fn exact_fallback_faces(&self) -> impl Iterator<Item = &(AssetHash, u32)> {
        self.exact_fallback_chains.values().flatten()
    }

    /// Shapes and composes a story into columns and rectangular wrap exclusions.
    ///
    /// # Errors
    ///
    /// Returns [`TextLayoutError`] when the story or region is invalid, region
    /// geometry overflows, or the shaping engine yields unusable geometry.
    pub fn resolve(
        &mut self,
        story: &TextStory,
        region: &TextRegion,
    ) -> Result<ResolvedTextLayout, TextLayoutError> {
        self.resolve_aligned(story, region, HorizontalAlignment::Left)
    }

    /// Shapes and composes a story with canonical paragraph alignment.
    ///
    /// # Errors
    ///
    /// Returns [`TextLayoutError`] under the same conditions as [`Self::resolve`].
    pub fn resolve_aligned(
        &mut self,
        story: &TextStory,
        region: &TextRegion,
        alignment: HorizontalAlignment,
    ) -> Result<ResolvedTextLayout, TextLayoutError> {
        validate_inputs(story, region)?;
        let mut layout = self.shape(story);
        let placements = break_into_region(&mut layout, region)?;
        layout.align(
            match alignment {
                HorizontalAlignment::Left => Alignment::Left,
                HorizontalAlignment::Center => Alignment::Center,
                HorizontalAlignment::Right => Alignment::Right,
                HorizontalAlignment::Justify => Alignment::Justify,
            },
            AlignmentOptions::default(),
        );
        resolve_output(
            &layout,
            story.text.len(),
            &placements,
            &mut self.resolved_font_hashes,
        )
    }

    /// Composes LF-delimited paragraphs through the same native shaping,
    /// column, and exclusion pipeline while applying paragraph-local geometry.
    /// `paragraphs` must contain exactly one concrete style for every LF plus
    /// the final paragraph, including the final empty paragraph after a
    /// trailing LF.
    ///
    /// # Errors
    ///
    /// Returns [`TextLayoutError`] when the story, paragraph contract, or
    /// effective frame geometry is invalid.
    pub fn resolve_paragraphs(
        &mut self,
        story: &TextStory,
        region: &TextRegion,
        paragraphs: &[StoryParagraphStyle],
    ) -> Result<ResolvedTextLayout, TextLayoutError> {
        validate_inputs(story, region)?;
        let ranges = paragraph_ranges(&story.text);
        validate_paragraph_styles(paragraphs, ranges.len())?;
        let columns = columns(region)?;
        let mut flow = FlowCursor::new(&columns);
        let mut resolved = ResolvedTextLayout {
            source_len_bytes: story.text.len(),
            fonts: Vec::new(),
            lines: Vec::new(),
            list_markers: Vec::new(),
            overset: None,
        };

        for (index, range) in ranges.into_iter().enumerate() {
            let paragraph_style = paragraphs[index];
            advance_flow(&mut flow, &columns, paragraph_style.space_before.value())?;
            let paragraph_story = paragraph_story(story, range.clone(), paragraph_style)?;
            let marker = paragraph_style
                .list_marker
                .map(|marker| self.resolve_marker(&paragraph_story, marker))
                .transpose()?;
            let effective_style = paragraph_with_marker_gutter(
                paragraph_style,
                marker
                    .as_ref()
                    .map_or(0, |(_, layout)| layout.width.value()),
            )?;
            // A list marker is real visible paragraph content even when the
            // durable paragraph has no bytes. Shape one disposable space to
            // obtain the exact line box, then remove its source and glyphs.
            let marker_only = paragraph_story.text.is_empty() && marker.is_some();
            let composition_story = if marker_only {
                TextStory {
                    text: " ".to_owned(),
                    base_style: paragraph_story.base_style.clone(),
                    spans: Vec::new(),
                }
            } else {
                paragraph_story.clone()
            };
            let mut layout = self.shape(&composition_story);
            let placements =
                break_into_region_from(&mut layout, region, &columns, &mut flow, effective_style)?;
            layout.align(
                parley_alignment(effective_style.alignment),
                AlignmentOptions::default(),
            );
            let mut paragraph_resolved = resolve_output(
                &layout,
                composition_story.text.len(),
                &placements,
                &mut self.resolved_font_hashes,
            )?;
            if marker_only {
                sanitize_marker_only_body(&mut paragraph_resolved);
            }
            let paragraph_overset = paragraph_resolved.overset.clone();
            let line_start = resolved.lines.len();
            append_resolved_layout(&mut resolved, paragraph_resolved, range.start)?;
            if let Some((marker_text, mut marker_layout)) = marker
                && let Some(first_line) = resolved.lines.get(line_start)
            {
                position_marker(
                    &mut marker_layout.layout,
                    first_line,
                    marker_layout.width.value(),
                )?;
                resolved.list_markers.push(ResolvedListMarker {
                    paragraph_index: u32::try_from(index)
                        .map_err(|_| TextLayoutError::RegionGeometryOverflow)?,
                    source_anchor: range.start,
                    text: marker_text,
                    layout: marker_layout.layout,
                });
            }
            if let Some(overset) = paragraph_overset {
                resolved.overset = Some(OversetText {
                    source: SourceRange {
                        start: range
                            .start
                            .checked_add(overset.source.start)
                            .ok_or(TextLayoutError::RegionGeometryOverflow)?,
                        end: story.text.len(),
                    },
                });
                break;
            }
            advance_flow(&mut flow, &columns, paragraph_style.space_after.value())?;
        }
        Ok(resolved)
    }

    /// Shapes all source text without automatic wrapping, page exclusions, or
    /// a vertical limit and returns its canonical shrink-wrapped line box.
    /// Explicit paragraph breaks remain lines; Fit text never reports overset.
    ///
    /// # Errors
    ///
    /// Returns [`TextLayoutError`] when story styling or engine geometry is invalid.
    pub fn resolve_fit_aligned(
        &mut self,
        story: &TextStory,
        alignment: HorizontalAlignment,
    ) -> Result<ResolvedFitTextLayout, TextLayoutError> {
        validate_story(story)?;
        let mut layout = self.shape(story);
        layout.break_all_lines(None);
        let width = Millipoints::from_parley(layout.width())?;
        let height = Millipoints::from_parley(layout.height())?;
        let placements = layout
            .lines()
            .map(|_| LinePlacement {
                column_index: 0,
                segment: Segment {
                    x: 0,
                    width: width.value(),
                },
            })
            .collect::<Vec<_>>();
        layout.align(
            match alignment {
                HorizontalAlignment::Left => Alignment::Left,
                HorizontalAlignment::Center => Alignment::Center,
                HorizontalAlignment::Right => Alignment::Right,
                HorizontalAlignment::Justify => Alignment::Justify,
            },
            AlignmentOptions::default(),
        );
        let resolved = resolve_output(
            &layout,
            story.text.len(),
            &placements,
            &mut self.resolved_font_hashes,
        )?;
        if resolved.overset.is_some() {
            return Err(TextLayoutError::EngineLineCountMismatch);
        }
        Ok(ResolvedFitTextLayout {
            layout: resolved,
            width,
            height,
        })
    }

    fn resolve_marker(
        &mut self,
        paragraph_story: &TextStory,
        marker: StoryListMarker,
    ) -> Result<(String, ResolvedFitTextLayout), TextLayoutError> {
        let text = marker.text();
        let story = TextStory {
            text: text.clone(),
            base_style: paragraph_story.base_style.clone(),
            spans: Vec::new(),
        };
        let layout = self.resolve_fit_aligned(&story, HorizontalAlignment::Left)?;
        Ok((text, layout))
    }

    /// Composes paragraph-formatted Fit text without automatic wrapping or
    /// exclusions. Explicit LF paragraph boundaries, indentation, line height,
    /// spacing, and paragraph alignment remain authoritative.
    ///
    /// # Errors
    ///
    /// Returns [`TextLayoutError`] when the story or paragraph contract is
    /// invalid or its canonical geometry overflows.
    #[expect(
        clippy::too_many_lines,
        reason = "Fit composition deliberately keeps paragraph measurement, generated-marker geometry, alignment, and final source remapping in one ordered authority"
    )]
    pub fn resolve_fit_paragraphs(
        &mut self,
        story: &TextStory,
        paragraphs: &[StoryParagraphStyle],
    ) -> Result<ResolvedFitTextLayout, TextLayoutError> {
        validate_story(story)?;
        let ranges = paragraph_ranges(&story.text);
        validate_paragraph_styles(paragraphs, ranges.len())?;
        let mut resolved_paragraphs = Vec::with_capacity(ranges.len());
        let mut width = 0_i64;

        for (index, range) in ranges.iter().cloned().enumerate() {
            let paragraph_style = paragraphs[index];
            let paragraph_story = paragraph_story(story, range.clone(), paragraph_style)?;
            let marker = paragraph_style
                .list_marker
                .map(|marker| self.resolve_marker(&paragraph_story, marker))
                .transpose()?;
            let effective_style = paragraph_with_marker_gutter(
                paragraph_style,
                marker
                    .as_ref()
                    .map_or(0, |(_, layout)| layout.width.value()),
            )?;
            let marker_only = paragraph_story.text.is_empty() && marker.is_some();
            let composition_story = if marker_only {
                TextStory {
                    text: " ".to_owned(),
                    base_style: paragraph_story.base_style.clone(),
                    spans: Vec::new(),
                }
            } else {
                paragraph_story
            };
            let mut layout = self.shape(&composition_story);
            layout.break_all_lines(None);
            layout.align(Alignment::Left, AlignmentOptions::default());
            let natural_width = Millipoints::from_parley(layout.width())?.value();
            let adjusted_start = effective_style
                .start_indent
                .value()
                .checked_add(effective_style.first_line_indent.value())
                .ok_or(TextLayoutError::RegionGeometryOverflow)?;
            if adjusted_start < 0 {
                return Err(TextLayoutError::ParagraphIndentsDoNotFit);
            }
            let required_width = adjusted_start
                .checked_add(natural_width)
                .and_then(|value| value.checked_add(effective_style.end_indent.value()))
                .ok_or(TextLayoutError::RegionGeometryOverflow)?;
            width = width.max(required_width);
            resolved_paragraphs.push((
                index,
                range,
                effective_style,
                layout,
                natural_width,
                marker,
                marker_only,
                composition_story.text.len(),
            ));
        }

        let mut resolved = ResolvedTextLayout {
            source_len_bytes: story.text.len(),
            fonts: Vec::new(),
            lines: Vec::new(),
            list_markers: Vec::new(),
            overset: None,
        };
        let mut y = 0_i64;
        for (
            index,
            range,
            paragraph_style,
            layout,
            natural_width,
            marker,
            marker_only,
            composition_len,
        ) in resolved_paragraphs
        {
            y = y
                .checked_add(paragraph_style.space_before.value())
                .ok_or(TextLayoutError::RegionGeometryOverflow)?;
            let adjusted_start = paragraph_style
                .start_indent
                .value()
                .checked_add(paragraph_style.first_line_indent.value())
                .ok_or(TextLayoutError::RegionGeometryOverflow)?;
            let available_width = width
                .checked_sub(adjusted_start)
                .and_then(|value| value.checked_sub(paragraph_style.end_indent.value()))
                .ok_or(TextLayoutError::ParagraphIndentsDoNotFit)?;
            if available_width < natural_width {
                return Err(TextLayoutError::ParagraphIndentsDoNotFit);
            }
            let alignment_offset = match paragraph_style.alignment {
                HorizontalAlignment::Left | HorizontalAlignment::Justify => 0,
                HorizontalAlignment::Center => (available_width - natural_width) / 2,
                HorizontalAlignment::Right => available_width - natural_width,
            };
            let x = adjusted_start
                .checked_add(alignment_offset)
                .ok_or(TextLayoutError::RegionGeometryOverflow)?;
            let placements = layout
                .lines()
                .map(|_| LinePlacement {
                    column_index: 0,
                    segment: Segment {
                        x: 0,
                        width: available_width,
                    },
                })
                .collect::<Vec<_>>();
            let mut paragraph_resolved = resolve_output(
                &layout,
                composition_len,
                &placements,
                &mut self.resolved_font_hashes,
            )?;
            if marker_only {
                sanitize_marker_only_body(&mut paragraph_resolved);
            }
            translate_layout(&mut paragraph_resolved, x, y)?;
            let paragraph_height = paragraph_resolved.lines.last().map_or(0, |line| {
                line.y.value().saturating_add(line.height.value()) - y
            });
            let line_start = resolved.lines.len();
            append_resolved_layout(&mut resolved, paragraph_resolved, range.start)?;
            if let Some((marker_text, mut marker_layout)) = marker
                && let Some(first_line) = resolved.lines.get(line_start)
            {
                position_marker(
                    &mut marker_layout.layout,
                    first_line,
                    marker_layout.width.value(),
                )?;
                resolved.list_markers.push(ResolvedListMarker {
                    paragraph_index: u32::try_from(index)
                        .map_err(|_| TextLayoutError::RegionGeometryOverflow)?,
                    source_anchor: range.start,
                    text: marker_text,
                    layout: marker_layout.layout,
                });
            }
            y = y
                .checked_add(paragraph_height)
                .and_then(|value| value.checked_add(paragraph_style.space_after.value()))
                .ok_or(TextLayoutError::RegionGeometryOverflow)?;
        }
        Ok(ResolvedFitTextLayout {
            layout: resolved,
            width: Millipoints::new(width),
            height: Millipoints::new(y),
        })
    }

    fn shape(&mut self, story: &TextStory) -> Layout<u32> {
        let mut builder = self
            .layout
            .ranged_builder(&mut self.fonts, &story.text, 1.0, false);
        push_style_defaults(&mut builder, &story.base_style);
        for span in &story.spans {
            push_style_range(&mut builder, &span.style, span.source.as_range());
        }
        builder.build(&story.text)
    }
}

fn push_style_defaults(builder: &mut parley::RangedBuilder<'_, u32>, style: &StoryStyle) {
    builder.push_default(StyleProperty::FontFamily(FontFamily::Source(
        Cow::Borrowed(style.font_family.as_str()),
    )));
    builder.push_default(StyleProperty::FontSize(style.font_size.to_points()));
    builder.push_default(StyleProperty::FontWeight(FontWeight::new(f32::from(
        style.font_weight,
    ))));
    builder.push_default(StyleProperty::FontStyle(if style.italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    }));
    builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
        f32::from(style.line_height_per_thousand) / 1_000.0,
    )));
    builder.push_default(StyleProperty::Brush(style.id));
}

fn push_style_range(
    builder: &mut parley::RangedBuilder<'_, u32>,
    style: &StoryStyle,
    range: Range<usize>,
) {
    builder.push(
        StyleProperty::FontFamily(FontFamily::Source(Cow::Borrowed(
            style.font_family.as_str(),
        ))),
        range.clone(),
    );
    builder.push(
        StyleProperty::FontSize(style.font_size.to_points()),
        range.clone(),
    );
    builder.push(
        StyleProperty::FontWeight(FontWeight::new(f32::from(style.font_weight))),
        range.clone(),
    );
    builder.push(
        StyleProperty::FontStyle(if style.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        }),
        range.clone(),
    );
    builder.push(
        StyleProperty::LineHeight(LineHeight::FontSizeRelative(
            f32::from(style.line_height_per_thousand) / 1_000.0,
        )),
        range.clone(),
    );
    builder.push(StyleProperty::Brush(style.id), range);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ColumnBox {
    x: i64,
    y: i64,
    width: i64,
    height: i64,
}

impl ColumnBox {
    fn right(self) -> i64 {
        self.x.saturating_add(self.width)
    }

    fn bottom(self) -> i64 {
        self.y.saturating_add(self.height)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Segment {
    x: i64,
    width: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LinePlacement {
    column_index: u16,
    segment: Segment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FlowCursor {
    column_index: usize,
    y: i64,
}

impl FlowCursor {
    fn new(columns: &[ColumnBox]) -> Self {
        Self {
            column_index: 0,
            y: columns.first().map_or(0, |column| column.y),
        }
    }
}

fn columns(region: &TextRegion) -> Result<Vec<ColumnBox>, TextLayoutError> {
    let count = i64::from(region.column_count);
    let total_gap = region
        .column_gap
        .value()
        .checked_mul(count - 1)
        .ok_or(TextLayoutError::RegionGeometryOverflow)?;
    let usable = region
        .bounds
        .width
        .value()
        .checked_sub(total_gap)
        .ok_or(TextLayoutError::RegionGeometryOverflow)?;
    if usable < count {
        return Err(TextLayoutError::ColumnsDoNotFit);
    }
    let width = usable / count;
    let mut result = Vec::with_capacity(usize::from(region.column_count));
    for index in 0..region.column_count {
        let offset = i64::from(index)
            .checked_mul(
                width
                    .checked_add(region.column_gap.value())
                    .ok_or(TextLayoutError::RegionGeometryOverflow)?,
            )
            .ok_or(TextLayoutError::RegionGeometryOverflow)?;
        result.push(ColumnBox {
            x: region
                .bounds
                .x
                .value()
                .checked_add(offset)
                .ok_or(TextLayoutError::RegionGeometryOverflow)?,
            y: region.bounds.y.value(),
            width,
            height: region.bounds.height.value(),
        });
    }
    Ok(result)
}

fn paragraph_column(
    column: ColumnBox,
    paragraph: StoryParagraphStyle,
    first_line: bool,
) -> Result<ColumnBox, TextLayoutError> {
    let mut start = paragraph.start_indent.value();
    if first_line {
        start = start
            .checked_add(paragraph.first_line_indent.value())
            .ok_or(TextLayoutError::RegionGeometryOverflow)?;
    }
    if start < 0 {
        return Err(TextLayoutError::ParagraphIndentsDoNotFit);
    }
    let width = column
        .width
        .checked_sub(start)
        .and_then(|value| value.checked_sub(paragraph.end_indent.value()))
        .ok_or(TextLayoutError::RegionGeometryOverflow)?;
    if width <= 0 {
        return Err(TextLayoutError::ParagraphIndentsDoNotFit);
    }
    Ok(ColumnBox {
        x: column
            .x
            .checked_add(start)
            .ok_or(TextLayoutError::RegionGeometryOverflow)?,
        y: column.y,
        width,
        height: column.height,
    })
}

const LIST_MARKER_GAP_MILLIPOINTS: i64 = 4_000;
const LIST_MIN_GUTTER_MILLIPOINTS: i64 = 18_000;

fn paragraph_with_marker_gutter(
    mut paragraph: StoryParagraphStyle,
    marker_width: i64,
) -> Result<StoryParagraphStyle, TextLayoutError> {
    if paragraph.list_marker.is_none() {
        return Ok(paragraph);
    }
    let required = marker_width
        .checked_add(LIST_MARKER_GAP_MILLIPOINTS)
        .and_then(|value| value.checked_sub(paragraph.first_line_indent.value()))
        .ok_or(TextLayoutError::RegionGeometryOverflow)?;
    let gutter = LIST_MIN_GUTTER_MILLIPOINTS.max(required);
    paragraph.start_indent = Millipoints::new(
        paragraph
            .start_indent
            .value()
            .checked_add(gutter)
            .ok_or(TextLayoutError::RegionGeometryOverflow)?,
    );
    Ok(paragraph)
}

fn sanitize_marker_only_body(layout: &mut ResolvedTextLayout) {
    layout.source_len_bytes = 0;
    for line in &mut layout.lines {
        line.source = SourceRange { start: 0, end: 0 };
        for fragment in &mut line.fragments {
            fragment.source = SourceRange { start: 0, end: 0 };
            fragment.glyph_runs.clear();
        }
    }
    if let Some(overset) = &mut layout.overset {
        overset.source = SourceRange { start: 0, end: 0 };
    }
}

fn position_marker(
    marker: &mut ResolvedTextLayout,
    body_first_line: &ResolvedTextLine,
    marker_width: i64,
) -> Result<(), TextLayoutError> {
    let body_x = body_first_line
        .fragments
        .first()
        .map(|fragment| fragment.x.value())
        .ok_or(TextLayoutError::EngineLineCountMismatch)?;
    let marker_line = marker
        .lines
        .first()
        .ok_or(TextLayoutError::EngineLineCountMismatch)?;
    let marker_x = marker_line
        .fragments
        .first()
        .map(|fragment| fragment.x.value())
        .ok_or(TextLayoutError::EngineLineCountMismatch)?;
    let target_x = body_x
        .checked_sub(LIST_MARKER_GAP_MILLIPOINTS)
        .and_then(|value| value.checked_sub(marker_width))
        .ok_or(TextLayoutError::RegionGeometryOverflow)?;
    let x_offset = target_x
        .checked_sub(marker_x)
        .ok_or(TextLayoutError::RegionGeometryOverflow)?;
    let y_offset = body_first_line
        .baseline
        .value()
        .checked_sub(marker_line.baseline.value())
        .ok_or(TextLayoutError::RegionGeometryOverflow)?;
    translate_layout(marker, x_offset, y_offset)
}

fn advance_flow(
    flow: &mut FlowCursor,
    columns: &[ColumnBox],
    mut distance: i64,
) -> Result<(), TextLayoutError> {
    if distance < 0 {
        return Err(TextLayoutError::NegativeParagraphGeometry);
    }
    while distance > 0 && flow.column_index < columns.len() {
        let column = columns[flow.column_index];
        let available = column.bottom().saturating_sub(flow.y).max(0);
        if distance <= available {
            flow.y = flow
                .y
                .checked_add(distance)
                .ok_or(TextLayoutError::RegionGeometryOverflow)?;
            return Ok(());
        }
        distance -= available;
        flow.column_index += 1;
        if let Some(next) = columns.get(flow.column_index) {
            flow.y = next.y;
        }
    }
    Ok(())
}

fn paragraph_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::with_capacity(text.bytes().filter(|byte| *byte == b'\n').count() + 1);
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            ranges.push(start..index);
            start = index + 1;
        }
    }
    ranges.push(start..text.len());
    ranges
}

fn validate_paragraph_styles(
    paragraphs: &[StoryParagraphStyle],
    expected_count: usize,
) -> Result<(), TextLayoutError> {
    if paragraphs.len() != expected_count {
        return Err(TextLayoutError::ParagraphStyleCountMismatch);
    }
    for paragraph in paragraphs {
        paragraph.validate()?;
    }
    Ok(())
}

fn paragraph_story(
    story: &TextStory,
    range: Range<usize>,
    paragraph: StoryParagraphStyle,
) -> Result<TextStory, TextLayoutError> {
    let mut base_style = story.base_style.clone();
    base_style.line_height_per_thousand = paragraph.line_height_per_thousand;
    let mut spans = Vec::new();
    for span in &story.spans {
        let start = span.source.start.max(range.start);
        let end = span.source.end.min(range.end);
        if start >= end {
            continue;
        }
        let mut style = span.style.clone();
        style.line_height_per_thousand = paragraph.line_height_per_thousand;
        spans.push(StorySpan {
            source: SourceRange {
                start: start - range.start,
                end: end - range.start,
            },
            style,
        });
    }
    let text = story
        .text
        .get(range)
        .ok_or(TextLayoutError::InvalidSpanRange)?
        // The durable contract names LF as the sole paragraph delimiter. Parley
        // otherwise treats a retained CR as another mandatory line break. A
        // one-byte space preserves every UTF-8 byte offset while preventing a
        // second, renderer-owned paragraph interpretation.
        .replace('\r', " ");
    Ok(TextStory {
        text,
        base_style,
        spans,
    })
}

fn append_resolved_layout(
    target: &mut ResolvedTextLayout,
    mut source: ResolvedTextLayout,
    source_offset: usize,
) -> Result<(), TextLayoutError> {
    let mut font_indices = Vec::with_capacity(source.fonts.len());
    for font in source.fonts {
        let index = target
            .fonts
            .iter()
            .position(|candidate| candidate == &font)
            .unwrap_or_else(|| {
                let index = target.fonts.len();
                target.fonts.push(font);
                index
            });
        font_indices.push(index);
    }
    for line in &mut source.lines {
        offset_source_range(&mut line.source, source_offset)?;
        for fragment in &mut line.fragments {
            offset_source_range(&mut fragment.source, source_offset)?;
            for run in &mut fragment.glyph_runs {
                offset_source_range(&mut run.source, source_offset)?;
                offset_source_range(&mut run.glyph_source, source_offset)?;
                run.font_index = *font_indices
                    .get(run.font_index)
                    .ok_or(TextLayoutError::EngineLineCountMismatch)?;
            }
        }
    }
    target.lines.extend(source.lines);
    Ok(())
}

fn offset_source_range(range: &mut SourceRange, offset: usize) -> Result<(), TextLayoutError> {
    range.start = range
        .start
        .checked_add(offset)
        .ok_or(TextLayoutError::RegionGeometryOverflow)?;
    range.end = range
        .end
        .checked_add(offset)
        .ok_or(TextLayoutError::RegionGeometryOverflow)?;
    Ok(())
}

fn translate_layout(
    layout: &mut ResolvedTextLayout,
    x: i64,
    y: i64,
) -> Result<(), TextLayoutError> {
    for line in &mut layout.lines {
        line.y = Millipoints::new(
            line.y
                .value()
                .checked_add(y)
                .ok_or(TextLayoutError::RegionGeometryOverflow)?,
        );
        line.baseline = Millipoints::new(
            line.baseline
                .value()
                .checked_add(y)
                .ok_or(TextLayoutError::RegionGeometryOverflow)?,
        );
        for fragment in &mut line.fragments {
            fragment.x = Millipoints::new(
                fragment
                    .x
                    .value()
                    .checked_add(x)
                    .ok_or(TextLayoutError::RegionGeometryOverflow)?,
            );
            for run in &mut fragment.glyph_runs {
                for glyph in &mut run.glyphs {
                    glyph.x = Millipoints::new(
                        glyph
                            .x
                            .value()
                            .checked_add(x)
                            .ok_or(TextLayoutError::RegionGeometryOverflow)?,
                    );
                    glyph.y = Millipoints::new(
                        glyph
                            .y
                            .value()
                            .checked_add(y)
                            .ok_or(TextLayoutError::RegionGeometryOverflow)?,
                    );
                }
            }
        }
    }
    Ok(())
}

const fn parley_alignment(alignment: HorizontalAlignment) -> Alignment {
    match alignment {
        HorizontalAlignment::Left => Alignment::Left,
        HorizontalAlignment::Center => Alignment::Center,
        HorizontalAlignment::Right => Alignment::Right,
        HorizontalAlignment::Justify => Alignment::Justify,
    }
}

#[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
fn break_into_region(
    layout: &mut Layout<u32>,
    region: &TextRegion,
) -> Result<Vec<LinePlacement>, TextLayoutError> {
    let columns = columns(region)?;
    let mut flow = FlowCursor::new(&columns);
    break_into_region_from(
        layout,
        region,
        &columns,
        &mut flow,
        StoryParagraphStyle::new(HorizontalAlignment::Left, 1_200),
    )
}

#[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
fn break_into_region_from(
    layout: &mut Layout<u32>,
    region: &TextRegion,
    columns: &[ColumnBox],
    flow: &mut FlowCursor,
    paragraph: StoryParagraphStyle,
) -> Result<Vec<LinePlacement>, TextLayoutError> {
    let max_width = columns
        .iter()
        .map(|column| column.width)
        .max()
        .ok_or(TextLayoutError::ZeroColumns)?;
    let mut breaker = layout.break_lines();
    breaker
        .state_mut()
        .set_layout_max_advance(Millipoints::new(max_width).to_points());

    let mut placements = Vec::new();
    let mut first_line = true;

    'content: loop {
        if breaker.is_done() {
            break;
        }
        if flow.column_index >= columns.len() {
            break;
        }
        let column = columns[flow.column_index];
        if flow.y >= column.bottom() {
            flow.column_index += 1;
            if let Some(next) = columns.get(flow.column_index) {
                flow.y = next.y;
                continue;
            }
            break;
        }

        let paragraph_column = paragraph_column(column, paragraph, first_line)?;
        let full_column = Segment {
            x: paragraph_column.x,
            width: paragraph_column.width,
        };
        configure_line(&mut breaker, full_column, flow.y);
        let Some(yielded) = breaker.break_next() else {
            break;
        };
        let YieldData::LineBreak(line) = yielded else {
            return Err(TextLayoutError::UnexpectedEngineYield);
        };
        let provisional_height = Millipoints::from_parley(line.line_height)?.value();
        if provisional_height <= 0 {
            return Err(TextLayoutError::NonPositiveEngineLineHeight);
        }
        if !breaker.revert() {
            return Err(TextLayoutError::EngineRevertFailed);
        }
        if flow.y.saturating_add(provisional_height) > column.bottom() {
            flow.column_index += 1;
            if let Some(next) = columns.get(flow.column_index) {
                flow.y = next.y;
                continue;
            }
            break;
        }

        let Some(mut segment) = available_segment(
            paragraph_column,
            &region.exclusions,
            flow.y,
            flow.y.saturating_add(provisional_height),
        ) else {
            if let Some(next_y) = next_exclusion_boundary_y(
                paragraph_column,
                &region.exclusions,
                flow.y,
                flow.y.saturating_add(provisional_height),
            ) {
                flow.y = next_y;
                continue;
            }
            flow.column_index += 1;
            if let Some(next) = columns.get(flow.column_index) {
                flow.y = next.y;
                continue;
            }
            break;
        };

        for _ in 0..8 {
            configure_line(&mut breaker, segment, flow.y);
            let Some(yielded) = breaker.break_next() else {
                break 'content;
            };
            let YieldData::LineBreak(line) = yielded else {
                return Err(TextLayoutError::UnexpectedEngineYield);
            };
            let line_height = Millipoints::from_parley(line.line_height)?.value();
            if line_height <= 0 {
                return Err(TextLayoutError::NonPositiveEngineLineHeight);
            }
            if flow.y.saturating_add(line_height) > column.bottom() {
                if !breaker.revert() {
                    return Err(TextLayoutError::EngineRevertFailed);
                }
                flow.column_index += 1;
                if let Some(next) = columns.get(flow.column_index) {
                    flow.y = next.y;
                    continue 'content;
                }
                break 'content;
            }
            let exact = available_segment(
                paragraph_column,
                &region.exclusions,
                flow.y,
                flow.y.saturating_add(line_height),
            );
            if exact == Some(segment) {
                placements.push(LinePlacement {
                    column_index: flow.column_index as u16,
                    segment,
                });
                flow.y = flow.y.saturating_add(line_height);
                first_line = false;
                continue 'content;
            }
            if !breaker.revert() {
                return Err(TextLayoutError::EngineRevertFailed);
            }
            let Some(next_segment) = exact else {
                if let Some(next_y) = next_exclusion_boundary_y(
                    paragraph_column,
                    &region.exclusions,
                    flow.y,
                    flow.y.saturating_add(line_height),
                ) {
                    flow.y = next_y;
                    continue 'content;
                }
                flow.column_index += 1;
                if let Some(next) = columns.get(flow.column_index) {
                    flow.y = next.y;
                    continue 'content;
                }
                break 'content;
            };
            segment = next_segment;
        }
        return Err(TextLayoutError::UnstableExclusionResolution);
    }

    breaker.finish();
    Ok(placements)
}

fn configure_line(breaker: &mut parley::BreakLines<'_, u32>, segment: Segment, y: i64) {
    let state = breaker.state_mut();
    state.set_line_x(Millipoints::new(segment.x).to_points());
    state.set_line_y(f64::from(Millipoints::new(y).to_points()));
    state.set_line_max_advance(Millipoints::new(segment.width).to_points());
}

fn available_segment(
    column: ColumnBox,
    exclusions: &[TextRect],
    y_start: i64,
    y_end: i64,
) -> Option<Segment> {
    let mut segments = vec![(column.x, column.right())];
    for exclusion in exclusions
        .iter()
        .copied()
        .filter(|exclusion| exclusion.y.value() < y_end && exclusion.bottom() > y_start)
    {
        let cut_start = exclusion.x.value().max(column.x);
        let cut_end = exclusion.right().min(column.right());
        if cut_start >= cut_end {
            continue;
        }
        let mut next = Vec::with_capacity(segments.len() + 1);
        for (start, end) in segments {
            if cut_end <= start || cut_start >= end {
                next.push((start, end));
                continue;
            }
            if start < cut_start {
                next.push((start, cut_start));
            }
            if cut_end < end {
                next.push((cut_end, end));
            }
        }
        segments = next;
    }
    segments
        .into_iter()
        .filter_map(|(start, end)| {
            let width = end.saturating_sub(start);
            (width > 0).then_some(Segment { x: start, width })
        })
        .max_by(|left, right| {
            left.width
                .cmp(&right.width)
                .then_with(|| right.x.cmp(&left.x))
        })
}

fn next_exclusion_boundary_y(
    column: ColumnBox,
    exclusions: &[TextRect],
    y_start: i64,
    y_end: i64,
) -> Option<i64> {
    exclusions
        .iter()
        .filter(|exclusion| {
            exclusion.y.value() < y_end
                && exclusion.bottom() > y_start
                && exclusion.x.value() < column.right()
                && exclusion.right() > column.x
        })
        .map(|exclusion| exclusion.bottom().min(column.bottom()))
        .filter(|next| *next > y_start)
        .min()
}

fn resolve_output(
    layout: &Layout<u32>,
    source_len: usize,
    placements: &[LinePlacement],
    font_hashes: &mut HashMap<u64, [u8; 32]>,
) -> Result<ResolvedTextLayout, TextLayoutError> {
    if layout.len() != placements.len() {
        return Err(TextLayoutError::EngineLineCountMismatch);
    }
    let mut fonts = Vec::<ResolvedFont>::new();
    let mut font_indices = HashMap::<ResolvedFont, usize>::new();
    let mut outline_cache = HashMap::<(usize, u32, i64), Arc<ResolvedGlyphOutline>>::new();
    let mut lines = Vec::with_capacity(layout.len());

    for (line, placement) in layout.lines().zip(placements.iter().copied()) {
        lines.push(resolve_line_output(
            line,
            source_len,
            placement,
            &mut fonts,
            &mut font_indices,
            &mut outline_cache,
            font_hashes,
        )?);
    }

    let placed_end = lines.iter().map(|line| line.source.end).max().unwrap_or(0);
    let overset = (placed_end < source_len).then_some(OversetText {
        source: SourceRange {
            start: placed_end,
            end: source_len,
        },
    });
    Ok(ResolvedTextLayout {
        source_len_bytes: source_len,
        fonts,
        lines,
        list_markers: Vec::new(),
        overset,
    })
}

fn resolve_line_output(
    line: parley::layout::Line<'_, u32>,
    source_len: usize,
    placement: LinePlacement,
    fonts: &mut Vec<ResolvedFont>,
    font_indices: &mut HashMap<ResolvedFont, usize>,
    outline_cache: &mut HashMap<(usize, u32, i64), Arc<ResolvedGlyphOutline>>,
    font_hashes: &mut HashMap<u64, [u8; 32]>,
) -> Result<ResolvedTextLine, TextLayoutError> {
    let metrics = line.metrics();
    let mut glyph_runs = Vec::new();
    let mut cursor = metrics.inline_min_coord + metrics.offset;
    for run in line.runs() {
        let run_font = run.font();
        let requires_font_synthesis = run.synthesis().any();
        let font = ResolvedFont {
            sha256: *font_hashes
                .entry(run_font.data.id())
                .or_insert_with(|| Sha256::digest(run_font.data.data()).into()),
            collection_index: run_font.index,
        };
        let outline_face = ttf_parser::Face::parse(run_font.data.data(), run_font.index).ok();
        let next_font_index = fonts.len();
        let font_index = *font_indices.entry(font.clone()).or_insert_with(|| {
            fonts.push(font);
            next_font_index
        });
        // Parley splits ligatures into editing components, sometimes carrying
        // the glyph on the last logical component for RTL. Keep that component
        // range for editing and independently retain the full shaping range.
        let mut shaping_ranges: Vec<std::ops::Range<usize>> = Vec::new();
        for cluster in run.visual_clusters() {
            let source = cluster.text_range();
            if cluster.is_ligature_continuation()
                && let Some(range) = shaping_ranges.last_mut()
            {
                range.start = range.start.min(source.start);
                range.end = range.end.max(source.end);
                continue;
            }
            shaping_ranges.push(source);
        }
        shaping_ranges.sort_by_key(|range| range.start);
        for cluster in run.visual_clusters() {
            let cluster_source = cluster.text_range();
            let shaping_index = shaping_ranges
                .partition_point(|range| range.start <= cluster_source.start)
                .saturating_sub(1);
            let glyph_source = shaping_ranges[shaping_index].clone();
            let source_bearing = cluster_source.start < cluster_source.end;
            let mut glyph_cursor = cursor;
            let mut glyphs = Vec::new();
            let font_size = Millipoints::from_parley(run.font_size())?;
            for glyph in cluster.glyphs() {
                let fallback =
                    resolve_glyph_fallback(outline_face.as_ref(), glyph.id, source_bearing);
                let outline_key = (font_index, glyph.id, font_size.value());
                let outline = if fallback.is_some() {
                    None
                } else if let Some(cached) = outline_cache.get(&outline_key) {
                    Some(Arc::clone(cached))
                } else {
                    outline_face.as_ref().and_then(|face| {
                        resolve_glyph_outline(face, glyph.id, run.font_size()).map(|outline| {
                            let outline = Arc::new(outline);
                            outline_cache.insert(outline_key, Arc::clone(&outline));
                            outline
                        })
                    })
                };
                glyphs.push(ResolvedGlyph {
                    id: glyph.id,
                    x: Millipoints::from_parley(glyph_cursor + glyph.x)?,
                    y: Millipoints::from_parley(metrics.baseline + glyph.y)?,
                    advance: Millipoints::from_parley(glyph.advance)?,
                    outline,
                    fallback,
                });
                glyph_cursor += glyph.advance;
            }
            glyph_runs.push(ResolvedGlyphRun {
                source: cluster_source.into(),
                glyph_source: glyph_source.into(),
                style_id: cluster.first_style().brush,
                font_index,
                font_size,
                requires_font_synthesis,
                right_to_left: cluster.is_rtl(),
                glyphs,
            });
            cursor += cluster.advance();
        }
    }

    // Parley retains a synthetic 0..1 sentinel line range for an empty
    // layout. Empty trailing paragraphs are real layout lines but own no
    // source byte, so canonicalize that engine sentinel at our boundary.
    let source: SourceRange = if source_len == 0 {
        SourceRange { start: 0, end: 0 }
    } else {
        line.text_range().into()
    };
    let fragment = ResolvedTextFragment {
        source: source.clone(),
        x: Millipoints::new(placement.segment.x),
        width: Millipoints::new(placement.segment.width),
        glyph_runs,
    };
    Ok(ResolvedTextLine {
        source,
        column_index: placement.column_index,
        y: Millipoints::from_parley(metrics.block_min_coord)?,
        height: Millipoints::from_parley(metrics.line_height)?,
        baseline: Millipoints::from_parley(metrics.baseline)?,
        fragments: vec![fragment],
    })
}

fn resolve_glyph_fallback(
    face: Option<&ttf_parser::Face<'_>>,
    glyph_id: u32,
    source_bearing: bool,
) -> Option<ResolvedGlyphFallback> {
    let glyph_id = u16::try_from(glyph_id).ok().map(ttf_parser::GlyphId);
    let missing = source_bearing && glyph_id.is_none_or(|glyph_id| glyph_id.0 == 0);
    let Some(face) = face else {
        return missing.then_some(ResolvedGlyphFallback::Missing);
    };
    let Some(glyph_id) = glyph_id else {
        return Some(ResolvedGlyphFallback::Missing);
    };
    classify_glyph_fallback(
        missing,
        GlyphTechnologyFacts {
            colr: face.is_color_glyph(glyph_id),
            svg: face.glyph_svg_image(glyph_id).is_some(),
            bitmap: face.glyph_raster_image(glyph_id, u16::MAX).is_some(),
        },
    )
}

#[derive(Clone, Copy)]
struct GlyphTechnologyFacts {
    colr: bool,
    svg: bool,
    bitmap: bool,
}

const fn classify_glyph_fallback(
    missing: bool,
    technology: GlyphTechnologyFacts,
) -> Option<ResolvedGlyphFallback> {
    if missing {
        Some(ResolvedGlyphFallback::Missing)
    } else if technology.colr {
        Some(ResolvedGlyphFallback::Colr)
    } else if technology.svg {
        Some(ResolvedGlyphFallback::Svg)
    } else if technology.bitmap {
        Some(ResolvedGlyphFallback::Bitmap)
    } else {
        None
    }
}

fn resolve_glyph_outline(
    face: &ttf_parser::Face<'_>,
    glyph_id: u32,
    font_size_points: f32,
) -> Option<ResolvedGlyphOutline> {
    let glyph_id = u16::try_from(glyph_id).ok().map(ttf_parser::GlyphId)?;
    let scale = f64::from(font_size_points) * 1_000.0 / f64::from(face.units_per_em());
    let mut builder = CanonicalOutlineBuilder {
        scale,
        commands: Vec::new(),
    };
    face.outline_glyph(glyph_id, &mut builder)?;
    Some(ResolvedGlyphOutline {
        commands: builder.commands,
    })
}

struct CanonicalOutlineBuilder {
    scale: f64,
    commands: Vec<ResolvedOutlineCommand>,
}

impl CanonicalOutlineBuilder {
    #[allow(clippy::cast_possible_truncation)]
    fn coordinate(&self, value: f32) -> Millipoints {
        Millipoints::new((f64::from(value) * self.scale).round() as i64)
    }
}

impl ttf_parser::OutlineBuilder for CanonicalOutlineBuilder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.commands.push(ResolvedOutlineCommand::MoveTo {
            x: self.coordinate(x),
            y: self.coordinate(y),
        });
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.commands.push(ResolvedOutlineCommand::LineTo {
            x: self.coordinate(x),
            y: self.coordinate(y),
        });
    }

    fn quad_to(&mut self, control_x: f32, control_y: f32, x: f32, y: f32) {
        self.commands.push(ResolvedOutlineCommand::QuadTo {
            control_x: self.coordinate(control_x),
            control_y: self.coordinate(control_y),
            x: self.coordinate(x),
            y: self.coordinate(y),
        });
    }

    fn curve_to(
        &mut self,
        control_1_x: f32,
        control_1_y: f32,
        control_2_x: f32,
        control_2_y: f32,
        x: f32,
        y: f32,
    ) {
        self.commands.push(ResolvedOutlineCommand::CurveTo {
            control_1_x: self.coordinate(control_1_x),
            control_1_y: self.coordinate(control_1_y),
            control_2_x: self.coordinate(control_2_x),
            control_2_y: self.coordinate(control_2_y),
            x: self.coordinate(x),
            y: self.coordinate(y),
        });
    }

    fn close(&mut self) {
        self.commands.push(ResolvedOutlineCommand::Close);
    }
}

fn validate_inputs(story: &TextStory, region: &TextRegion) -> Result<(), TextLayoutError> {
    validate_story(story)?;
    if region.column_count == 0 {
        return Err(TextLayoutError::ZeroColumns);
    }
    if region.column_gap.value() < 0 {
        return Err(TextLayoutError::NegativeColumnGap);
    }
    if region.bounds.width.value() <= 0 || region.bounds.height.value() <= 0 {
        return Err(TextLayoutError::NonPositiveRegion);
    }
    columns(region).map(|_| ())
}

fn validate_story(story: &TextStory) -> Result<(), TextLayoutError> {
    story.base_style.validate()?;
    for span in &story.spans {
        span.style.validate()?;
        if span.source.start > span.source.end
            || span.source.end > story.text.len()
            || !story.text.is_char_boundary(span.source.start)
            || !story.text.is_char_boundary(span.source.end)
        {
            return Err(TextLayoutError::InvalidSpanRange);
        }
    }
    Ok(())
}

/// Text feasibility failures surfaced without renderer or DOM dependencies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextLayoutError {
    EmptyFontFamily,
    NonPositiveFontSize,
    FontWeightOutOfRange,
    NonPositiveLineHeight,
    InvalidSpanRange,
    ParagraphStyleCountMismatch,
    NegativeParagraphGeometry,
    ParagraphIndentsDoNotFit,
    ZeroColumns,
    NegativeColumnGap,
    NonPositiveRegion,
    ColumnsDoNotFit,
    RegionGeometryOverflow,
    NonFiniteEngineGeometry,
    EngineGeometryOutOfRange,
    NonPositiveEngineLineHeight,
    UnexpectedEngineYield,
    EngineRevertFailed,
    EngineLineCountMismatch,
    UnstableExclusionResolution,
}

impl core::fmt::Display for TextLayoutError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "text layout failed: {self:?}")
    }
}

impl std::error::Error for TextLayoutError {}

#[cfg(test)]
mod geometry_tests {
    use super::{ColumnBox, Segment, TextRect, available_segment};

    #[test]
    fn exclusion_chooses_widest_segment_then_leftmost() {
        let column = ColumnBox {
            x: 0,
            y: 0,
            width: 100_000,
            height: 100_000,
        };
        let exclusion = TextRect::new(40_000, 0, 20_000, 20_000).unwrap();
        assert_eq!(
            available_segment(column, &[exclusion], 0, 12_000),
            Some(Segment {
                x: 0,
                width: 40_000
            })
        );
        assert_eq!(
            available_segment(column, &[exclusion], 21_000, 33_000),
            Some(Segment {
                x: 0,
                width: 100_000
            })
        );
    }
}

#[cfg(test)]
mod glyph_fallback_tests {
    use super::{
        GlyphTechnologyFacts, ResolvedGlyphFallback, classify_glyph_fallback,
        resolve_glyph_fallback,
    };

    const FONT: &[u8] = include_bytes!("../../tests/fixtures/DejaVuSans.ttf");

    fn read_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_be_bytes(
            bytes[offset..offset + 2]
                .try_into()
                .expect("font fixture contains a complete u16"),
        )
    }

    fn read_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_be_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("font fixture contains a complete u32"),
        )
    }

    fn push_u16(bytes: &mut Vec<u8>, value: u16) {
        bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn font_with_extra_tables(extras: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
        let table_count = usize::from(read_u16(FONT, 4));
        let mut tables = Vec::with_capacity(table_count + extras.len());
        for index in 0..table_count {
            let record = 12 + index * 16;
            let tag: [u8; 4] = FONT[record..record + 4]
                .try_into()
                .expect("font fixture table tag is complete");
            if extras.iter().any(|(extra_tag, _)| *extra_tag == tag) {
                continue;
            }
            let offset = usize::try_from(read_u32(FONT, record + 8))
                .expect("font fixture table offset fits usize");
            let length = usize::try_from(read_u32(FONT, record + 12))
                .expect("font fixture table length fits usize");
            tables.push((tag, FONT[offset..offset + length].to_vec()));
        }
        tables.extend(extras.iter().cloned());
        tables.sort_by_key(|(tag, _)| *tag);

        let count = u16::try_from(tables.len()).expect("test font table count fits u16");
        let mut font = vec![0; 12 + tables.len() * 16];
        font[..4].copy_from_slice(&FONT[..4]);
        font[4..6].copy_from_slice(&count.to_be_bytes());
        let max_power = 1_u16 << count.ilog2();
        font[6..8].copy_from_slice(&(max_power * 16).to_be_bytes());
        font[8..10].copy_from_slice(
            &u16::try_from(count.ilog2())
                .expect("test font search selector fits u16")
                .to_be_bytes(),
        );
        font[10..12].copy_from_slice(&(count * 16 - max_power * 16).to_be_bytes());

        for (index, (tag, bytes)) in tables.into_iter().enumerate() {
            while !font.len().is_multiple_of(4) {
                font.push(0);
            }
            let offset = u32::try_from(font.len()).expect("test font offset fits u32");
            let length = u32::try_from(bytes.len()).expect("test font table length fits u32");
            let record = 12 + index * 16;
            font[record..record + 4].copy_from_slice(&tag);
            font[record + 8..record + 12].copy_from_slice(&offset.to_be_bytes());
            font[record + 12..record + 16].copy_from_slice(&length.to_be_bytes());
            font.extend_from_slice(&bytes);
        }
        font
    }

    fn colr_tables(glyph_id: ttf_parser::GlyphId) -> [([u8; 4], Vec<u8>); 2] {
        let mut colr = Vec::new();
        push_u16(&mut colr, 0);
        push_u16(&mut colr, 1);
        push_u32(&mut colr, 14);
        push_u32(&mut colr, 20);
        push_u16(&mut colr, 1);
        push_u16(&mut colr, glyph_id.0);
        push_u16(&mut colr, 0);
        push_u16(&mut colr, 1);
        push_u16(&mut colr, glyph_id.0);
        push_u16(&mut colr, 0);

        let mut cpal = Vec::new();
        push_u16(&mut cpal, 0);
        push_u16(&mut cpal, 1);
        push_u16(&mut cpal, 1);
        push_u16(&mut cpal, 1);
        push_u32(&mut cpal, 14);
        push_u16(&mut cpal, 0);
        cpal.extend_from_slice(&[0, 0, 255, 255]);
        [(*b"COLR", colr), (*b"CPAL", cpal)]
    }

    fn svg_table(glyph_id: ttf_parser::GlyphId) -> Vec<u8> {
        let document = b"<svg xmlns='http://www.w3.org/2000/svg'/>";
        let mut svg = Vec::new();
        push_u16(&mut svg, 0);
        push_u32(&mut svg, 10);
        push_u32(&mut svg, 0);
        push_u16(&mut svg, 1);
        push_u16(&mut svg, glyph_id.0);
        push_u16(&mut svg, glyph_id.0);
        push_u32(&mut svg, 14);
        push_u32(
            &mut svg,
            u32::try_from(document.len()).expect("test SVG length fits u32"),
        );
        svg.extend_from_slice(document);
        svg
    }

    fn sbix_table(face: &ttf_parser::Face<'_>, glyph_id: ttf_parser::GlyphId) -> Vec<u8> {
        let offset_count = u32::from(face.number_of_glyphs()) + 1;
        let glyph_data_start = 4 + offset_count * 4;
        let mut image = vec![0; 24];
        image[16..20].copy_from_slice(&1_u32.to_be_bytes());
        image[20..24].copy_from_slice(&1_u32.to_be_bytes());
        let glyph_data_end = glyph_data_start + 8 + u32::try_from(image.len()).unwrap_or(0);

        let mut sbix = Vec::new();
        push_u16(&mut sbix, 1);
        push_u16(&mut sbix, 0);
        push_u32(&mut sbix, 1);
        push_u32(&mut sbix, 12);
        push_u16(&mut sbix, 16);
        push_u16(&mut sbix, 72);
        for index in 0..offset_count {
            push_u32(
                &mut sbix,
                if index <= u32::from(glyph_id.0) {
                    glyph_data_start
                } else {
                    glyph_data_end
                },
            );
        }
        push_u16(&mut sbix, 0);
        push_u16(&mut sbix, 0);
        sbix.extend_from_slice(b"png ");
        sbix.extend_from_slice(&image);
        sbix
    }

    #[test]
    fn unsupported_glyph_technology_is_classified_with_stable_precedence() {
        assert_eq!(
            classify_glyph_fallback(
                true,
                GlyphTechnologyFacts {
                    colr: true,
                    svg: true,
                    bitmap: true,
                }
            ),
            Some(ResolvedGlyphFallback::Missing)
        );
        assert_eq!(
            classify_glyph_fallback(
                false,
                GlyphTechnologyFacts {
                    colr: true,
                    svg: true,
                    bitmap: true,
                }
            ),
            Some(ResolvedGlyphFallback::Colr)
        );
        assert_eq!(
            classify_glyph_fallback(
                false,
                GlyphTechnologyFacts {
                    colr: false,
                    svg: true,
                    bitmap: true,
                }
            ),
            Some(ResolvedGlyphFallback::Svg)
        );
        assert_eq!(
            classify_glyph_fallback(
                false,
                GlyphTechnologyFacts {
                    colr: false,
                    svg: false,
                    bitmap: true,
                }
            ),
            Some(ResolvedGlyphFallback::Bitmap)
        );
        assert_eq!(
            classify_glyph_fallback(
                false,
                GlyphTechnologyFacts {
                    colr: false,
                    svg: false,
                    bitmap: false,
                }
            ),
            None
        );
    }

    #[test]
    fn parser_facts_distinguish_colr_svg_and_bitmap_glyphs() {
        let base_face = ttf_parser::Face::parse(FONT, 0).expect("licensed fixture must parse");
        let glyph_id = base_face
            .glyph_index('A')
            .expect("licensed fixture must contain A");

        let colr_font = font_with_extra_tables(&colr_tables(glyph_id));
        let colr_face = ttf_parser::Face::parse(&colr_font, 0).expect("COLR test face must parse");
        assert_eq!(
            resolve_glyph_fallback(Some(&colr_face), u32::from(glyph_id.0), true),
            Some(ResolvedGlyphFallback::Colr)
        );

        let svg_font = font_with_extra_tables(&[(*b"SVG ", svg_table(glyph_id))]);
        let svg_face = ttf_parser::Face::parse(&svg_font, 0).expect("SVG test face must parse");
        assert_eq!(
            resolve_glyph_fallback(Some(&svg_face), u32::from(glyph_id.0), true),
            Some(ResolvedGlyphFallback::Svg)
        );

        let bitmap_font = font_with_extra_tables(&[(*b"sbix", sbix_table(&base_face, glyph_id))]);
        let bitmap_face =
            ttf_parser::Face::parse(&bitmap_font, 0).expect("bitmap test face must parse");
        assert_eq!(
            resolve_glyph_fallback(Some(&bitmap_face), u32::from(glyph_id.0), true),
            Some(ResolvedGlyphFallback::Bitmap)
        );
    }
}
