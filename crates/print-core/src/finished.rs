//! Already composed page content. Coordinates are PDF points, with the origin
//! at the bottom left and positive y pointing up. No layout or ingestion runs here.

use crate::{PdfCandidateMetadata, ProfileError};
use font_boundary::PreparedPublicationFont;
use raster_capacity::{
    MAX_PREPARED_RASTER_CACHE_BYTES, RasterMemoryPermit, production_raster_pixels,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Stable caller-chosen resource identity, independent of application asset types.
pub type ResourceId = String;
/// Maximum aggregate UTF-8 resource identities, Unicode mappings and `ActualText`,
/// plus the metadata title, admitted for one finished document.
pub const MAX_FINISHED_TEXT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum byte length of one caller resource identity.
pub const MAX_FINISHED_RESOURCE_ID_BYTES: usize = 1_024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
impl Point {
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Rectangle represented by its lower-left and upper-right coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfBox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}
impl PdfBox {
    #[must_use]
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AffineTransform(pub [f32; 6]);
impl Default for AffineTransform {
    fn default() -> Self {
        Self([1.0, 0.0, 0.0, 1.0, 0.0, 0.0])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCommand {
    MoveTo(Point),
    LineTo(Point),
    CubicTo(Point, Point, Point),
    Close,
}
#[derive(Clone, Debug, PartialEq)]
pub struct FinishedPath {
    pub commands: Vec<PathCommand>,
    pub fill_rule: FillRule,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PaintColor {
    /// Device gray: zero is black and one is white.
    Gray(f32),
    Rgb([f32; 3]),
    Cmyk([f32; 4]),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paint {
    pub color: PaintColor,
    pub alpha: f32,
}
impl Paint {
    #[must_use]
    pub const fn gray(color: f32, alpha: f32) -> Self {
        Self {
            color: PaintColor::Gray(color),
            alpha,
        }
    }
    #[must_use]
    pub const fn cmyk(color: [f32; 4], alpha: f32) -> Self {
        Self {
            color: PaintColor::Cmyk(color),
            alpha,
        }
    }
    #[must_use]
    pub const fn rgb(color: [f32; 3], alpha: f32) -> Self {
        Self {
            color: PaintColor::Rgb(color),
            alpha,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub paint: Paint,
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f32,
    pub dash: Vec<f32>,
    pub dash_phase: f32,
}
#[derive(Clone, Debug, PartialEq)]
pub struct FinishedPathPaint {
    pub path: FinishedPath,
    pub fill: Option<Paint>,
    pub stroke: Option<Stroke>,
}

/// Native endpoint-padded gradient geometry, in the fill item's local coordinates.
/// A nonuniform item transform turns radial circles into elliptical gradients.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientGeometry {
    Axial {
        start: Point,
        end: Point,
    },
    Radial {
        start_center: Point,
        start_radius: f32,
        end_center: Point,
        end_radius: f32,
    },
}
/// One authored color and straight opacity at a gradient parameter in [0, 1].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub offset: f32,
    pub paint: Paint,
}
/// Native linear interpolation of each authored color component and opacity.
/// Stops must share one device color space, be ordered, and have unique endpoints
/// at 0 and 1. Equal interior offsets create hard transitions: the first color
/// is the left limit and the last color is used at and to the right of the stop.
/// Colors and opacity interpolate independently, rather than as premultiplied
/// web-gradient colors. The endpoint colors pad beyond both ends of the geometry.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub geometry: GradientGeometry,
    pub stops: Vec<GradientStop>,
}
/// A native gradient clipped to this path, respecting its fill rule. Item
/// transforms and an optional additional item clip apply to color and opacity.
#[derive(Clone, Debug, PartialEq)]
pub struct FinishedGradientFill {
    pub path: FinishedPath,
    pub gradient: Gradient,
}

/// CID is the prepared program's CID, not a source font glyph number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionedGlyph {
    pub cid: u16,
    pub matrix: AffineTransform,
}
#[derive(Clone, Debug, PartialEq)]
pub struct PositionedGlyphRun {
    pub font: ResourceId,
    pub font_size: f32,
    pub glyphs: Vec<PositionedGlyph>,
    /// Logical text; U+001B language escape inputs are unsupported.
    pub actual_text: String,
    pub paint: Paint,
}
/// Image content paints the resource's unit square; the item matrix places it.
#[derive(Clone, Debug, PartialEq)]
pub struct ImagePlacement {
    pub image: ResourceId,
    pub alpha: f32,
}
/// Isolated group bounds are in its local coordinates and include its visible paints.
/// Group opacity is applied once after the overlapping inner paints are combined.
#[derive(Clone, Debug, PartialEq)]
pub struct FinishedGroup {
    pub bounds: PdfBox,
    pub opacity: f32,
    pub items: Vec<FinishedItem>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum FinishedContent {
    Path(FinishedPathPaint),
    Gradient(FinishedGradientFill),
    Glyphs(PositionedGlyphRun),
    Image(ImagePlacement),
    /// Paints a constant color through the referenced image's exact alpha plane.
    /// Like Image, this paints a unit square; transforms and clips belong to the item.
    ImageAlpha {
        image: ResourceId,
        paint: Paint,
    },
    Group(FinishedGroup),
}
/// Standard PDF blend modes, applied to an item's completed paint.
/// For an isolated group, the mode and group opacity apply at placement;
/// its children use their own modes, whose default is Normal.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}
#[derive(Clone, Debug, PartialEq)]
pub struct FinishedItem {
    pub transform: AffineTransform,
    pub clip: Option<FinishedPath>,
    pub blend_mode: BlendMode,
    pub content: FinishedContent,
}
impl FinishedItem {
    #[must_use]
    pub fn new(content: FinishedContent) -> Self {
        Self {
            transform: AffineTransform::default(),
            clip: None,
            blend_mode: BlendMode::Normal,
            content,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FinishedPageBoxes {
    pub media: PdfBox,
    pub bleed: PdfBox,
    pub trim: PdfBox,
}
#[derive(Clone, Debug, PartialEq)]
pub struct FinishedPage {
    pub boxes: FinishedPageBoxes,
    pub items: Vec<FinishedItem>,
}
#[derive(Default)]
pub struct FinishedResources {
    pub fonts: Vec<FontResource>,
    pub images: Vec<ImageResource>,
}
pub struct FinishedDocument {
    pub pages: Vec<FinishedPage>,
    pub resources: FinishedResources,
}

pub struct FontResource {
    pub(crate) id: ResourceId,
    pub(crate) program: Arc<PreparedPublicationFont>,
    pub(crate) unicode: Arc<BTreeMap<u16, Vec<char>>>,
}
impl FontResource {
    /// Retains the exact rights-aware worker-prepared program and CID mappings.
    /// # Errors
    /// Rejects empty identities, empty Unicode strings, .notdef and CIDs outside
    /// the prepared mappings. Each Unicode destination is limited to 512
    /// UTF-16BE bytes, without a byte-order mark (PDF 1.6 section 5.9.2).
    pub fn from_prepared(
        id: ResourceId,
        program: PreparedPublicationFont,
        unicode: BTreeMap<u16, String>,
    ) -> Result<Self, FinishedPdfError> {
        if id.len() > MAX_FINISHED_RESOURCE_ID_BYTES
            || unicode
                .values()
                .try_fold(id.len(), |sum, s| sum.checked_add(s.len()))
                .is_none_or(|sum| sum > MAX_FINISHED_TEXT_BYTES)
        {
            return Err(FinishedPdfError::ResourceLimit);
        }
        let prepared_cids: BTreeSet<_> = program
            .remapped_glyphs()
            .iter()
            .map(|mapping| mapping.cid())
            .collect();
        if id.is_empty()
            || unicode.iter().any(|(cid, s)| {
                *cid == 0
                    || *cid >= program.embedded_glyph_count()
                    || s.is_empty()
                    || !crate::to_unicode::destination_within_limit(s.chars())
                    || !prepared_cids.contains(cid)
            })
        {
            return Err(FinishedPdfError::InvalidFont(id));
        }
        Ok(Self {
            id,
            program: Arc::new(program),
            unicode: Arc::new(
                unicode
                    .into_iter()
                    .map(|(cid, s)| (cid, s.chars().collect()))
                    .collect(),
            ),
        })
    }
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    #[must_use]
    pub fn program(&self) -> &PreparedPublicationFont {
        &self.program
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImageColorSpace {
    Gray,
    Rgb,
    Cmyk,
}
impl ImageColorSpace {
    const fn components(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
        }
    }
}

pub struct ImageResource {
    pub(crate) id: ResourceId,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) color_space: ImageColorSpace,
    pub(crate) color: Arc<Vec<u8>>,
    pub(crate) alpha: Option<Arc<Vec<u8>>>,
    // Every view keeps both compressed planes and their admitted-memory lease alive.
    pub(crate) _memory: RasterMemoryPermit,
}
impl ImageResource {
    /// Compresses exact interleaved decoded 8-bit RGB and optional straight alpha.
    /// Alpha is an independent plane; color samples must not be premultiplied.
    /// # Errors
    /// Rejects invalid dimensions, plane lengths, identity and resource budgets.
    pub fn from_decoded_rgb(
        id: ResourceId,
        width: u32,
        height: u32,
        rgb: &[u8],
        alpha: Option<&[u8]>,
    ) -> Result<Self, FinishedPdfError> {
        Self::from_decoded(id, width, height, ImageColorSpace::Rgb, rgb, alpha)
    }

    /// Compresses exact decoded 8-bit gray and optional straight alpha.
    /// Zero is black and 255 is white; alpha is an independent plane.
    /// # Errors
    /// Rejects invalid dimensions, plane lengths, identity and resource budgets.
    pub fn from_decoded_gray(
        id: ResourceId,
        width: u32,
        height: u32,
        gray: &[u8],
        alpha: Option<&[u8]>,
    ) -> Result<Self, FinishedPdfError> {
        Self::from_decoded(id, width, height, ImageColorSpace::Gray, gray, alpha)
    }

    /// Compresses exact decoded 8-bit CMYK and optional 8-bit alpha planes.
    /// Memory admission and pixel bounds are shared with the raster workers.
    /// Alpha is straight and independent; color samples must not be premultiplied.
    /// # Errors
    /// Rejects invalid dimensions, plane lengths, identity and resource budgets.
    pub fn from_decoded_cmyk(
        id: ResourceId,
        width: u32,
        height: u32,
        cmyk: &[u8],
        alpha: Option<&[u8]>,
    ) -> Result<Self, FinishedPdfError> {
        Self::from_decoded(id, width, height, ImageColorSpace::Cmyk, cmyk, alpha)
    }

    fn from_decoded(
        id: ResourceId,
        width: u32,
        height: u32,
        color_space: ImageColorSpace,
        color: &[u8],
        alpha: Option<&[u8]>,
    ) -> Result<Self, FinishedPdfError> {
        if id.len() > MAX_FINISHED_RESOURCE_ID_BYTES {
            return Err(FinishedPdfError::ResourceLimit);
        }
        let pixels =
            production_raster_pixels(width, height).map_err(|_| FinishedPdfError::ResourceLimit)?;
        let expected_color_bytes = pixels
            .checked_mul(color_space.components())
            .ok_or(FinishedPdfError::ResourceLimit)?;
        if id.is_empty()
            || color.len() != expected_color_bytes
            || alpha.is_some_and(|a| a.len() != pixels)
        {
            return Err(FinishedPdfError::InvalidImage(id));
        }
        let reserved_bytes = color
            .len()
            .checked_add(alpha.map_or(0, <[u8]>::len))
            .and_then(|bytes| bytes.checked_add(MAX_PREPARED_RASTER_CACHE_BYTES))
            .ok_or(FinishedPdfError::ResourceLimit)?;
        let mut memory = RasterMemoryPermit::reserve(reserved_bytes)
            .map_err(|_| FinishedPdfError::ResourceLimit)?;
        let color =
            crate::resources::compress_raster_stream(color, MAX_PREPARED_RASTER_CACHE_BYTES)
                .map_err(|_| FinishedPdfError::ResourceLimit)?;
        let alpha = alpha
            .map(|a| {
                crate::resources::compress_raster_stream(
                    a,
                    MAX_PREPARED_RASTER_CACHE_BYTES - color.capacity(),
                )
            })
            .transpose()
            .map_err(|_| FinishedPdfError::ResourceLimit)?;
        // The compressor retains spare allocation; admit capacity, including
        // both planes, rather than only the bytes currently used in them.
        memory
            .shrink_to(color.capacity() + alpha.as_ref().map_or(0, Vec::capacity))
            .map_err(|_| FinishedPdfError::ResourceLimit)?;
        Ok(Self {
            id,
            width,
            height,
            color_space,
            color: Arc::new(color),
            alpha: alpha.map(Arc::new),
            _memory: memory,
        })
    }
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Bounded PDF/X-4 candidate generation requires an exact usable CMYK output profile.
/// Independent checking of the returned bytes remains necessary for conformance.
#[derive(Clone, Copy, Debug)]
pub enum FinishedOutputMode<'a> {
    Ordinary,
    PdfX4 { output_profile: &'a [u8] },
}
#[derive(Clone, Debug)]
pub struct FinishedPdfOptions<'a> {
    pub metadata: PdfCandidateMetadata,
    pub mode: FinishedOutputMode<'a>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum FinishedPdfError {
    NoPages,
    InvalidGeometry,
    InvalidPath,
    InvalidPaint,
    InvalidGradient,
    InvalidGlyph { font: ResourceId, cid: u16 },
    MissingResource(ResourceId),
    DuplicateResource(ResourceId),
    InvalidFont(ResourceId),
    InvalidImage(ResourceId),
    ResourceLimit,
    InvalidOutputProfile(ProfileError),
    RgbInPdfX,
    EmptyPdfXTitle,
    PdfXContentStringTooLong { bytes: usize, maximum: usize },
    PdfXGraphicsStateDepthExceeded { depth: usize, maximum: usize },
}
impl std::fmt::Display for FinishedPdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Finished PDF input rejected: {self:?}")
    }
}
impl std::error::Error for FinishedPdfError {}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_capacity::MAX_RETAINED_RASTER_BYTES;

    #[test]
    fn decoded_image_lease_accounts_for_allocated_planes() {
        let mut state = 123_456_789_u32;
        let cmyk: Vec<u8> = (0..256 * 256 * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state.to_le_bytes()[0]
            })
            .collect();
        for color_space in [
            ImageColorSpace::Gray,
            ImageColorSpace::Rgb,
            ImageColorSpace::Cmyk,
        ] {
            let image = ImageResource::from_decoded(
                "allocated-planes".into(),
                256,
                256,
                color_space,
                &cmyk[..256 * 256 * color_space.components()],
                Some(&cmyk[..256 * 256]),
            )
            .unwrap();
            let allocated = image.color.capacity() + image.alpha.as_ref().unwrap().capacity();
            assert!(allocated > image.color.len() + image.alpha.as_ref().unwrap().len());
            // Reserve the remaining budget without allocating it. Even one more
            // admitted byte must fail while these allocated planes remain alive.
            let remainder =
                RasterMemoryPermit::reserve(MAX_RETAINED_RASTER_BYTES - allocated).unwrap();
            assert!(RasterMemoryPermit::reserve(1).is_err());
            drop(remainder);
            drop(image);
            drop(RasterMemoryPermit::reserve(MAX_RETAINED_RASTER_BYTES).unwrap());
        }
    }
}
