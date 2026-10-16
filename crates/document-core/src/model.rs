//! Canonical publication data in deterministic physical units.

use crate::assets::AssetHash;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};

mod table_authoring;
pub use table_authoring::{TableCellBorders, TableCellFormattingPatch, TableCellRange};
mod master_pages;
pub use master_pages::{MasterPage, PageNumberFormat, PageNumberingSection, PageNumberValue, TextField};

/// Stable identity for a canonical publication revision.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize,
)]
#[serde(transparent)]
pub struct Revision(u64);

impl Revision {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn initial() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
struct StableId(String);

impl StableId {
    fn parse(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 96
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            });
        valid
            .then_some(Self(value))
            .ok_or(ModelError::InvalidStableId)
    }

    fn generated(kind: &str, value: u64) -> Self {
        Self(format!("{kind}-{value:016x}"))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn generated_counter(&self, kind: &str) -> Option<u64> {
        self.0
            .strip_prefix(kind)
            .and_then(|suffix| suffix.strip_prefix('-'))
            .and_then(|suffix| (suffix.len() == 16).then_some(suffix))
            .and_then(|suffix| u64::from_str_radix(suffix, 16).ok())
    }
}

impl<'de> Deserialize<'de> for StableId {
    fn deserialize<DeserializerType>(
        deserializer: DeserializerType,
    ) -> Result<Self, DeserializerType::Error>
    where
        DeserializerType: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

macro_rules! typed_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(StableId);

        impl $name {
            /// Parses a stable identifier used by the canonical document model.
            ///
            /// # Errors
            ///
            /// Returns [`ModelError::InvalidStableId`] when `value` is empty,
            /// too long, or contains unsupported characters.
            pub fn parse(value: impl Into<String>) -> Result<Self, ModelError> {
                StableId::parse(value).map(Self)
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }
    };
}

typed_id!(DocumentId);
typed_id!(PageId);
typed_id!(MasterId);
typed_id!(ObjectId);
typed_id!(GroupId);
typed_id!(GuideId);

/// An integer count of micrometres. Floating-point screen coordinates never persist here.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Micrometres(i64);

impl Micrometres {
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PhysicalSize {
    pub width: Micrometres,
    pub height: Micrometres,
}

impl PhysicalSize {
    /// Creates a physical size from positive micrometre dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::NonPositiveSize`] if either dimension is zero or
    /// negative.
    pub fn new(width: i64, height: i64) -> Result<Self, ModelError> {
        if width <= 0 || height <= 0 {
            return Err(ModelError::NonPositiveSize);
        }
        Ok(Self {
            width: Micrometres::new(width),
            height: Micrometres::new(height),
        })
    }

    fn validate(self) -> Result<(), ModelError> {
        if self.width.value() <= 0 || self.height.value() <= 0 {
            return Err(ModelError::NonPositiveSize);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Insets {
    pub top: Micrometres,
    pub right: Micrometres,
    pub bottom: Micrometres,
    pub left: Micrometres,
}

impl Insets {
    #[allow(clippy::trivially_copy_pass_by_ref)] // Serde's skip predicate receives `&T`.
    const fn is_zero(&self) -> bool {
        self.top.value() == 0
            && self.right.value() == 0
            && self.bottom.value() == 0
            && self.left.value() == 0
    }

    /// Creates equal non-negative insets on every edge.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::NegativeInset`] if `value` is negative.
    pub fn uniform(value: i64) -> Result<Self, ModelError> {
        if value < 0 {
            return Err(ModelError::NegativeInset);
        }
        let value = Micrometres::new(value);
        Ok(Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        })
    }

    fn validate(self) -> Result<(), ModelError> {
        if [self.top, self.right, self.bottom, self.left]
            .into_iter()
            .any(|value| value.value() < 0)
        {
            return Err(ModelError::NegativeInset);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BleedExpectation {
    #[default]
    Auto,
    Required,
    NotIntended,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EdgeExpectations {
    pub top: BleedExpectation,
    pub right: BleedExpectation,
    pub bottom: BleedExpectation,
    pub left: BleedExpectation,
}

/// Canonical decomposed transform. Scale and shear use fixed-point values at 1e-9.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Transform {
    pub translation_x: Micrometres,
    pub translation_y: Micrometres,
    pub pivot_x: Micrometres,
    pub pivot_y: Micrometres,
    pub rotation_microdegrees: i64,
    pub scale_x_nanos: i64,
    pub scale_y_nanos: i64,
    pub shear_x_nanos: i64,
    pub shear_y_nanos: i64,
}

impl Transform {
    pub const IDENTITY_SCALE: i64 = 1_000_000_000;

    #[must_use]
    pub const fn identity_at(x: i64, y: i64) -> Self {
        Self {
            translation_x: Micrometres::new(x),
            translation_y: Micrometres::new(y),
            pivot_x: Micrometres::new(0),
            pivot_y: Micrometres::new(0),
            rotation_microdegrees: 0,
            scale_x_nanos: Self::IDENTITY_SCALE,
            scale_y_nanos: Self::IDENTITY_SCALE,
            shear_x_nanos: 0,
            shear_y_nanos: 0,
        }
    }

    fn validate(self) -> Result<(), ModelError> {
        if self.scale_x_nanos == 0 || self.scale_y_nanos == 0 {
            return Err(ModelError::ZeroScale);
        }
        if !(-180_000_000..180_000_000).contains(&self.rotation_microdegrees) {
            return Err(ModelError::RotationNotNormalized);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ObjectKind {
    Text {
        text: String,
        #[serde(default)]
        style: TextStyle,
        #[serde(default)]
        frame: TextFrame,
        /// Selection-aware inline and paragraph overrides. Older publications
        /// omit this field and retain their whole-object Text Style behavior.
        #[serde(default, skip_serializing_if = "TextStoryFormatting::is_default")]
        story: TextStoryFormatting,
    },
    Image {
        asset: AssetHash,
        #[serde(default)]
        wrap_mode: WrapMode,
        #[serde(default)]
        content_kind: ImageContentKind,
        #[serde(default)]
        crop: ImageCrop,
        #[serde(default, skip_serializing_if = "ImageFlip::is_default")]
        flip: ImageFlip,
        #[serde(default = "default_true")]
        lock_aspect_ratio: bool,
    },
    Shape {
        shape: ShapeKind,
        fill: Color,
        #[serde(default)]
        stroke: ShapeStroke,
        #[serde(default)]
        wrap_mode: WrapMode,
    },
    Table {
        table: TableData,
    },
}

/// Author-declared picture intent used by print-resolution policy.
///
/// This is deliberately not guessed from appearance. `LineArt` is the user's
/// explicit assertion that the picture is hard-edged, monochrome artwork with
/// no shades; ordinary color, grayscale, and tonal drawings remain
/// `ContinuousTone`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageContentKind {
    #[default]
    ContinuousTone,
    LineArt,
}

/// Explicit document policy for raster files that carry no embedded ICC profile.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UntaggedRasterPolicy {
    #[default]
    RequireEmbeddedProfile,
    AssumeSrgb,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentColorSettings {
    pub untagged_raster_policy: UntaggedRasterPolicy,
}

impl ObjectKind {
    fn validate(&self) -> Result<(), ModelError> {
        match self {
            Self::Text {
                text,
                style,
                frame,
                story,
            } => {
                if text.len() > MAX_TEXT_BYTES {
                    return Err(ModelError::TextTooLarge);
                }
                style.validate()?;
                frame.validate()?;
                story.validate(text)?;
            }
            Self::Table { table } => table.validate()?,
            Self::Shape { stroke, .. } => stroke.validate()?,
            Self::Image { crop, .. } => crop.validate()?,
        }
        Ok(())
    }

    fn validate_for_size(&self, size: PhysicalSize) -> Result<(), ModelError> {
        self.validate()?;
        if let Self::Text { frame, .. } = self {
            frame.validate_for_size(size)?;
        }
        Ok(())
    }
}

/// Intrinsic mirroring within the Picture's cropped viewport.
/// Source pixels, placement, borders, and shadow offsets remain unchanged.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImageFlip {
    pub horizontal: bool,
    pub vertical: bool,
}

impl ImageFlip {
    #[must_use]
    pub const fn is_default(&self) -> bool {
        !self.horizontal && !self.vertical
    }
}

/// Source-relative crop in millionths of the oriented original picture on each edge.
/// Opposing edges must leave at least one millionth of the source visible.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImageCrop {
    pub left_millionths: u32,
    pub top_millionths: u32,
    pub right_millionths: u32,
    pub bottom_millionths: u32,
}

impl ImageCrop {
    /// Creates a bounded source-relative crop.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::ImageCropOutOfRange`] when an edge pair removes
    /// the entire source picture.
    pub fn new(left: u32, top: u32, right: u32, bottom: u32) -> Result<Self, ModelError> {
        let crop = Self {
            left_millionths: left,
            top_millionths: top,
            right_millionths: right,
            bottom_millionths: bottom,
        };
        crop.validate()?;
        Ok(crop)
    }

    fn validate(self) -> Result<(), ModelError> {
        if self.left_millionths >= 1_000_000
            || self.top_millionths >= 1_000_000
            || self.right_millionths >= 1_000_000
            || self.bottom_millionths >= 1_000_000
            || self.left_millionths + self.right_millionths >= 1_000_000
            || self.top_millionths + self.bottom_millionths >= 1_000_000
        {
            return Err(ModelError::ImageCropOutOfRange);
        }
        Ok(())
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TableData {
    pub rows: u16,
    pub columns: u16,
    pub cells: Vec<String>,
    #[serde(default = "default_table_text_style")]
    pub style: TextStyle,
    #[serde(default, skip_serializing_if = "TableGridBorder::is_default")]
    pub grid_border: TableGridBorder,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cell_formatting: Vec<TableCellFormatting>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_cells: Vec<TableCellRange>,
    #[serde(default, skip_serializing_if = "table_track_weights_are_default")]
    pub row_weights: Vec<u32>,
    #[serde(default, skip_serializing_if = "table_track_weights_are_default")]
    pub column_weights: Vec<u32>,
}

#[derive(Deserialize)]
struct SerializedTableData {
    rows: u16,
    columns: u16,
    cells: Vec<String>,
    #[serde(default = "default_table_text_style")]
    style: TextStyle,
    #[serde(default)]
    grid_border: TableGridBorder,
    #[serde(default)]
    cell_formatting: Vec<TableCellFormatting>,
    #[serde(default)]
    merged_cells: Vec<TableCellRange>,
    #[serde(default)]
    row_weights: OmittedTrackWeights,
    #[serde(default)]
    column_weights: OmittedTrackWeights,
}

/// Distinguishes a genuinely omitted legacy field from explicit JSON `null`.
/// Serde's ordinary `Option<Vec<_>>` intentionally treats both alike, which
/// would silently repair malformed current-schema packages.
#[derive(Default)]
struct OmittedTrackWeights(Option<Vec<u32>>);

impl<'de> Deserialize<'de> for OmittedTrackWeights {
    fn deserialize<DeserializerType>(
        deserializer: DeserializerType,
    ) -> Result<Self, DeserializerType::Error>
    where
        DeserializerType: Deserializer<'de>,
    {
        Vec::<u32>::deserialize(deserializer).map(|weights| Self(Some(weights)))
    }
}

impl<'de> Deserialize<'de> for TableData {
    fn deserialize<DeserializerType>(
        deserializer: DeserializerType,
    ) -> Result<Self, DeserializerType::Error>
    where
        DeserializerType: Deserializer<'de>,
    {
        let serialized = SerializedTableData::deserialize(deserializer)?;
        let row_weights = serialized
            .row_weights
            .0
            .unwrap_or_else(|| vec![Self::DEFAULT_TRACK_WEIGHT; usize::from(serialized.rows)]);
        let column_weights = serialized
            .column_weights
            .0
            .unwrap_or_else(|| vec![Self::DEFAULT_TRACK_WEIGHT; usize::from(serialized.columns)]);
        Ok(Self {
            rows: serialized.rows,
            columns: serialized.columns,
            cells: serialized.cells,
            style: serialized.style,
            grid_border: serialized.grid_border,
            cell_formatting: serialized.cell_formatting,
            merged_cells: serialized.merged_cells,
            row_weights,
            column_weights,
        })
    }
}

fn table_track_weights_are_default(weights: &[u32]) -> bool {
    weights
        .iter()
        .all(|weight| *weight == TableData::DEFAULT_TRACK_WEIGHT)
}

/// Grid-line paint shared by every row and column boundary. A zero width
/// deliberately removes the grid without losing its selected color.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TableGridBorder {
    pub color: Color,
    pub width_millipoints: u32,
}

impl TableGridBorder {
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.color.red == 0
            && self.color.green == 0
            && self.color.blue == 0
            && self.color.alpha == 255
            && self.width_millipoints == 500
    }

    fn validate(self) -> Result<(), ModelError> {
        if self.width_millipoints <= ShapeStroke::MAX_WIDTH_MILLIPOINTS {
            Ok(())
        } else {
            Err(ModelError::TableGridBorderWidthOutOfRange)
        }
    }
}

impl Default for TableGridBorder {
    fn default() -> Self {
        Self {
            color: Color {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            },
            width_millipoints: 500,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TableVerticalAlignment {
    Top,
    #[default]
    Middle,
    Bottom,
}

/// Sparse cell appearance. Missing fields inherit the Table defaults. Covered
/// merged cells retain their styles for Split and Undo; the anchor paints the merge.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TableCellFormatting {
    pub row: u16,
    pub column: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horizontal_alignment: Option<TextAlignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_alignment: Option<TableVerticalAlignment>,
    #[serde(default, skip_serializing_if = "InlineTextStyle::is_default")]
    pub style: InlineTextStyle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<Color>,
    #[serde(default, skip_serializing_if = "TableCellBorders::is_default")]
    pub borders: TableCellBorders,
}

impl TableCellFormatting {
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.horizontal_alignment.is_none() && self.vertical_alignment.is_none()
            && self.style.is_default() && self.fill.is_none() && self.borders.is_default()
    }
}

/// The direction of one relative table track size.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TableTrackAxis {
    Row,
    Column,
}

fn default_table_text_style() -> TextStyle {
    TextStyle {
        alignment: TextAlignment::Center,
        color: Color {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        },
        ..TextStyle::default()
    }
}

impl TableData {
    pub const MAX_ROWS: u16 = 500;
    pub const MAX_COLUMNS: u16 = 500;
    pub const DEFAULT_TRACK_WEIGHT: u32 = 1_000;
    pub const MAX_TRACK_WEIGHT: u32 = 1_000_000;

    /// Creates a bounded table with empty editable cells.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError::InvalidTableDimensions`] for zero or excessive dimensions.
    pub fn empty(rows: u16, columns: u16) -> Result<Self, ModelError> {
        if rows == 0 || columns == 0 || rows > Self::MAX_ROWS || columns > Self::MAX_COLUMNS {
            return Err(ModelError::InvalidTableDimensions);
        }
        Ok(Self {
            rows,
            columns,
            cells: vec![String::new(); usize::from(rows) * usize::from(columns)],
            style: default_table_text_style(),
            grid_border: TableGridBorder::default(),
            cell_formatting: Vec::new(),
            merged_cells: Vec::new(),
            row_weights: vec![Self::DEFAULT_TRACK_WEIGHT; usize::from(rows)],
            column_weights: vec![Self::DEFAULT_TRACK_WEIGHT; usize::from(columns)],
        })
    }

    pub(crate) fn set_cell(
        &mut self,
        row: u16,
        column: u16,
        text: String,
    ) -> Result<(), ModelError> {
        let index = self.cell_index(row, column)?;
        if self
            .cell_span(row, column)
            .is_some_and(|span| (span.start_row, span.start_column) != (row, column))
        {
            return Err(ModelError::InvalidTableMerge);
        }
        self.cells[index] = text;
        Ok(())
    }

    pub(crate) fn resize(&mut self, rows: u16, columns: u16) -> Result<(), ModelError> {
        let mut resized = Self::empty(rows, columns)?;
        let retained_rows = self.rows.min(rows);
        let retained_columns = self.columns.min(columns);
        for row in 0..retained_rows {
            for column in 0..retained_columns {
                let source = usize::from(row) * usize::from(self.columns) + usize::from(column);
                let destination = usize::from(row) * usize::from(columns) + usize::from(column);
                resized.cells[destination].clone_from(&self.cells[source]);
            }
        }
        resized.style = self.style.clone();
        resized.grid_border = self.grid_border;
        resized.cell_formatting = self
            .cell_formatting
            .iter()
            .filter(|formatting| formatting.row < rows && formatting.column < columns)
            .cloned()
            .collect();
        resized.row_weights[..usize::from(retained_rows)]
            .copy_from_slice(&self.row_weights[..usize::from(retained_rows)]);
        resized.column_weights[..usize::from(retained_columns)]
            .copy_from_slice(&self.column_weights[..usize::from(retained_columns)]);
        // A clipped merge becomes ordinary cells. Retained edited anchors and
        // their formatting stay in place instead of being discarded.
        resized.merged_cells = self
            .merged_cells
            .iter()
            .copied()
            .filter(|span| span.end_row <= rows && span.end_column <= columns)
            .collect();
        *self = resized;
        Ok(())
    }

    pub(crate) fn insert_track(
        &mut self,
        axis: TableTrackAxis,
        index: u16,
    ) -> Result<(), ModelError> {
        self.prepare_merged_track_edit(axis, index, false)?;
        match axis {
            TableTrackAxis::Row => self.insert_row(index),
            TableTrackAxis::Column => self.insert_column(index),
        }
    }

    pub(crate) fn delete_track(
        &mut self,
        axis: TableTrackAxis,
        index: u16,
    ) -> Result<(), ModelError> {
        self.prepare_merged_track_edit(axis, index, true)?;
        match axis {
            TableTrackAxis::Row => self.delete_row(index),
            TableTrackAxis::Column => self.delete_column(index),
        }
    }

    pub(crate) fn set_cell_alignment(
        &mut self,
        row: u16,
        column: u16,
        horizontal_alignment: Option<TextAlignment>,
        vertical_alignment: Option<TableVerticalAlignment>,
    ) -> Result<(), ModelError> {
        self.cell_index(row, column)?;
        let address = (row, column);
        match self
            .cell_formatting
            .binary_search_by_key(&address, |formatting| (formatting.row, formatting.column))
        {
            Ok(index) => {
                self.cell_formatting[index].horizontal_alignment = horizontal_alignment;
                self.cell_formatting[index].vertical_alignment = vertical_alignment;
                if self.cell_formatting[index].is_default() {
                    self.cell_formatting.remove(index);
                }
            }
            Err(_) if horizontal_alignment.is_none() && vertical_alignment.is_none() => {}
            Err(index) => self.cell_formatting.insert(
                index,
                TableCellFormatting {
                    row,
                    column,
                    horizontal_alignment,
                    vertical_alignment,
                    ..TableCellFormatting::default()
                },
            ),
        }
        Ok(())
    }

    pub(crate) fn set_track_weight(
        &mut self,
        axis: TableTrackAxis,
        index: u16,
        new_weight: u32,
    ) -> Result<(), ModelError> {
        if new_weight == 0 || new_weight > Self::MAX_TRACK_WEIGHT {
            return Err(ModelError::TableTrackWeightOutOfRange);
        }
        let weights = match axis {
            TableTrackAxis::Row => &mut self.row_weights,
            TableTrackAxis::Column => &mut self.column_weights,
        };
        let weight = weights
            .get_mut(usize::from(index))
            .ok_or(ModelError::TableTrackOutOfRange)?;
        *weight = new_weight;
        Ok(())
    }

    pub(crate) fn distribute_tracks(&mut self, axis: TableTrackAxis) {
        let weights = match axis {
            TableTrackAxis::Row => &mut self.row_weights,
            TableTrackAxis::Column => &mut self.column_weights,
        };
        weights.fill(Self::DEFAULT_TRACK_WEIGHT);
    }

    fn insert_row(&mut self, index: u16) -> Result<(), ModelError> {
        if index > self.rows {
            return Err(ModelError::TableTrackOutOfRange);
        }
        if self.rows == Self::MAX_ROWS {
            return Err(ModelError::InvalidTableDimensions);
        }
        let insertion = usize::from(index) * usize::from(self.columns);
        self.cells.splice(
            insertion..insertion,
            vec![String::new(); usize::from(self.columns)],
        );
        self.row_weights
            .insert(usize::from(index), Self::DEFAULT_TRACK_WEIGHT);
        self.rows += 1;
        for formatting in &mut self.cell_formatting {
            if formatting.row >= index {
                formatting.row += 1;
            }
        }
        Ok(())
    }

    fn insert_column(&mut self, index: u16) -> Result<(), ModelError> {
        if index > self.columns {
            return Err(ModelError::TableTrackOutOfRange);
        }
        if self.columns == Self::MAX_COLUMNS {
            return Err(ModelError::InvalidTableDimensions);
        }
        let old_columns = usize::from(self.columns);
        let insertion = usize::from(index);
        let mut source = std::mem::take(&mut self.cells).into_iter();
        let mut cells = Vec::with_capacity(usize::from(self.rows) * (old_columns + 1));
        for _row in 0..self.rows {
            for column in 0..=old_columns {
                if column == insertion {
                    cells.push(String::new());
                } else {
                    cells.push(
                        source
                            .next()
                            .expect("validated table cells remain rectangular"),
                    );
                }
            }
        }
        debug_assert!(source.next().is_none());
        self.cells = cells;
        self.column_weights
            .insert(insertion, Self::DEFAULT_TRACK_WEIGHT);
        self.columns += 1;
        for formatting in &mut self.cell_formatting {
            if formatting.column >= index {
                formatting.column += 1;
            }
        }
        Ok(())
    }

    fn delete_row(&mut self, index: u16) -> Result<(), ModelError> {
        if index >= self.rows {
            return Err(ModelError::TableTrackOutOfRange);
        }
        if self.rows == 1 {
            return Err(ModelError::InvalidTableDimensions);
        }
        let start = usize::from(index) * usize::from(self.columns);
        let end = start + usize::from(self.columns);
        self.cells.drain(start..end);
        self.row_weights.remove(usize::from(index));
        self.rows -= 1;
        self.cell_formatting = self
            .cell_formatting
            .iter()
            .cloned()
            .filter_map(|mut formatting| {
                if formatting.row == index {
                    return None;
                }
                if formatting.row > index {
                    formatting.row -= 1;
                }
                Some(formatting)
            })
            .collect();
        Ok(())
    }

    fn delete_column(&mut self, index: u16) -> Result<(), ModelError> {
        if index >= self.columns {
            return Err(ModelError::TableTrackOutOfRange);
        }
        if self.columns == 1 {
            return Err(ModelError::InvalidTableDimensions);
        }
        let old_columns = usize::from(self.columns);
        let deletion = usize::from(index);
        let mut source = std::mem::take(&mut self.cells).into_iter();
        let mut cells = Vec::with_capacity(usize::from(self.rows) * (old_columns - 1));
        for _row in 0..self.rows {
            for column in 0..old_columns {
                let cell = source
                    .next()
                    .expect("validated table cells remain rectangular");
                if column != deletion {
                    cells.push(cell);
                }
            }
        }
        debug_assert!(source.next().is_none());
        self.cells = cells;
        self.column_weights.remove(deletion);
        self.columns -= 1;
        self.cell_formatting = self
            .cell_formatting
            .iter()
            .cloned()
            .filter_map(|mut formatting| {
                if formatting.column == index {
                    return None;
                }
                if formatting.column > index {
                    formatting.column -= 1;
                }
                Some(formatting)
            })
            .collect();
        Ok(())
    }

    fn cell_index(&self, row: u16, column: u16) -> Result<usize, ModelError> {
        if row >= self.rows || column >= self.columns {
            return Err(ModelError::TableCellOutOfRange);
        }
        Ok(usize::from(row) * usize::from(self.columns) + usize::from(column))
    }

    fn validate(&self) -> Result<(), ModelError> {
        if self.rows == 0
            || self.columns == 0
            || self.rows > Self::MAX_ROWS
            || self.columns > Self::MAX_COLUMNS
            || self.cells.len() != usize::from(self.rows) * usize::from(self.columns)
        {
            return Err(ModelError::InvalidTableDimensions);
        }
        self.style.validate()?;
        self.grid_border.validate()?;
        self.validate_cell_formatting()?;
        self.validate_merged_cells()?;
        if self.cells.iter().any(|text| text.len() > MAX_TEXT_BYTES) {
            return Err(ModelError::TextTooLarge);
        }
        Self::validate_track_weights(&self.row_weights, usize::from(self.rows))?;
        Self::validate_track_weights(&self.column_weights, usize::from(self.columns))?;
        Ok(())
    }

    fn validate_cell_formatting(&self) -> Result<(), ModelError> {
        let mut previous = None;
        for formatting in &self.cell_formatting {
            if formatting.row >= self.rows
                || formatting.column >= self.columns
                || formatting.is_default()
            {
                return Err(ModelError::InvalidTableCellFormatting);
            }
            let address = (formatting.row, formatting.column);
            if previous.is_some_and(|previous| previous >= address) {
                return Err(ModelError::InvalidTableCellFormatting);
            }
            previous = Some(address);
            self.validate_cell_appearance(formatting)?;
        }
        Ok(())
    }

    fn validate_track_weights(weights: &[u32], expected: usize) -> Result<(), ModelError> {
        if weights.len() != expected {
            return Err(ModelError::InvalidTableTrackWeights);
        }
        weights.iter().try_fold(0_u64, |total, weight| {
            if *weight == 0 || *weight > Self::MAX_TRACK_WEIGHT {
                return Err(ModelError::TableTrackWeightOutOfRange);
            }
            total
                .checked_add(u64::from(*weight))
                .ok_or(ModelError::InvalidTableTrackWeights)
        })?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
    Cube,
    Triangle,
    Arrow,
    /// An open segment from the top-left to the bottom-right of its positive bounds.
    /// Fill is retained for changing shape type but is never painted for a line.
    Line,
}

/// Requested physical corner radii. Normalization is derived from current size.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct CornerRounding {
    pub top_left_um: i64,
    pub top_right_um: i64,
    pub bottom_right_um: i64,
    pub bottom_left_um: i64,
    pub linked: bool,
}

impl Default for CornerRounding {
    fn default() -> Self {
        Self {
            top_left_um: 0,
            top_right_um: 0,
            bottom_right_um: 0,
            bottom_left_um: 0,
            linked: true,
        }
    }
}

impl CornerRounding {
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// # Errors
    /// Rejects out-of-range radii or differing radii while corners are linked.
    pub fn validate(self) -> Result<(), ModelError> {
        let radii = [
            self.top_left_um,
            self.top_right_um,
            self.bottom_right_um,
            self.bottom_left_um,
        ];
        if radii.iter().any(|radius| !(0..=1_000_000).contains(radius)) {
            return Err(ModelError::CornerRadiusOutOfRange);
        }
        if self.linked && radii.iter().any(|radius| *radius != radii[0]) {
            return Err(ModelError::LinkedCornersDiffer);
        }
        Ok(())
    }
}

/// Whether an image or shape excludes text from its axis-aligned bounds.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapMode {
    #[default]
    None,
    Rectangular,
}

/// Which single clear fragment is available beside a rectangular obstacle.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapSide {
    #[default]
    Largest,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WrapSettings {
    pub distance_um: i64,
    pub side: WrapSide,
}

impl WrapSettings {
    #[allow(clippy::trivially_copy_pass_by_ref)] // Serde's skip predicate receives `&T`.
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn validate(self) -> Result<(), ModelError> {
        if !(0..=1_000_000).contains(&self.distance_um) {
            return Err(ModelError::WrapDistanceOutOfRange);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShapeStroke {
    pub color: Color,
    pub width_millipoints: u32,
}

impl ShapeStroke {
    pub const MAX_WIDTH_MILLIPOINTS: u32 = 72_000;

    fn validate(self) -> Result<(), ModelError> {
        if self.width_millipoints <= Self::MAX_WIDTH_MILLIPOINTS {
            Ok(())
        } else {
            Err(ModelError::ShapeStrokeWidthOutOfRange)
        }
    }
}

impl Default for ShapeStroke {
    fn default() -> Self {
        Self {
            color: Color {
                red: 31,
                green: 32,
                blue: 25,
                alpha: 255,
            },
            width_millipoints: 0,
        }
    }
}

/// A crisp, object-local shadow. Blur is deliberately unsupported in this schema.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HardShadow {
    pub color: Color,
    pub offset_x: Micrometres,
    pub offset_y: Micrometres,
}

impl HardShadow {
    pub const MAX_OFFSET_MICROMETRES: i64 = 1_000_000;

    fn validate(self) -> Result<(), ModelError> {
        if self.offset_x.value().unsigned_abs() > Self::MAX_OFFSET_MICROMETRES as u64
            || self.offset_y.value().unsigned_abs() > Self::MAX_OFFSET_MICROMETRES as u64
        {
            Err(ModelError::HardShadowOffsetOutOfRange)
        } else {
            Ok(())
        }
    }
}

/// Optional paint effects shared by the canonical object envelope.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectEffects {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<HardShadow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_outline: Option<ShapeStroke>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture_border: Option<ShapeStroke>,
}

impl ObjectEffects {
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.shadow.is_none() && self.text_outline.is_none() && self.picture_border.is_none()
    }

    pub(crate) fn validate_for(self, kind: &ObjectKind) -> Result<(), ModelError> {
        if let Some(shadow) = self.shadow {
            shadow.validate()?;
        }
        if let Some(outline) = self.text_outline {
            outline.validate()?;
            if !matches!(kind, ObjectKind::Text { .. }) {
                return Err(ModelError::TextOutlineKindMismatch);
            }
        }
        if let Some(border) = self.picture_border {
            border.validate()?;
            if !matches!(kind, ObjectKind::Image { .. }) {
                return Err(ModelError::PictureBorderKindMismatch);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlignment {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// Maximum UTF-8 storage for one Text Box story, aligned with the package's
/// persisted per-string budget.
pub const MAX_TEXT_BYTES: usize = 32 * 1024;

/// Half-open byte range in the canonical UTF-8 story source.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextSourceRange {
    pub start: usize,
    pub end: usize,
}

impl TextSourceRange {
    pub(crate) fn validate(self, text: &str) -> Result<(), ModelError> {
        if self.start >= self.end || self.end > text.len() {
            return Err(ModelError::TextSourceRangeOutOfRange);
        }
        self.validate_edit(text)
    }

    pub(crate) fn validate_edit(self, text: &str) -> Result<(), ModelError> {
        if self.start > self.end || self.end > text.len() {
            return Err(ModelError::TextSourceRangeOutOfRange);
        }
        if !text.is_char_boundary(self.start) || !text.is_char_boundary(self.end) {
            return Err(ModelError::TextSourceRangeNotUtf8Boundary);
        }
        Ok(())
    }
}

/// Inline overrides relative to the containing Text Box's base Text Style.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct InlineTextStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size_millipoints: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
}

impl InlineTextStyle {
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.font_family.is_none()
            && self.font_weight.is_none()
            && self.bold.is_none()
            && self.italic.is_none()
            && self.font_size_millipoints.is_none()
            && self.color.is_none()
            && self.underline.is_none()
    }
}

/// One normalized, non-overlapping inline formatting range.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InlineTextSpan {
    pub range: TextSourceRange,
    pub style: InlineTextStyle,
}

/// Half-open paragraph-index range. Paragraphs are separated only by LF;
/// trailing LF creates a final empty paragraph and CR remains source content.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParagraphRange {
    pub start: u32,
    pub end: u32,
}

impl ParagraphRange {
    pub(crate) fn validate(self, paragraph_count: u32) -> Result<(), ModelError> {
        if self.start >= self.end || self.end > paragraph_count {
            Err(ModelError::ParagraphRangeOutOfRange)
        } else {
            Ok(())
        }
    }
}

/// Publisher-familiar paragraph marker. `None` is explicit so a sparse style
/// patch can remove a previously applied list without conflating that action
/// with "leave this property unchanged."
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ParagraphListStyle {
    None,
    Bullet,
    Numbered { start_at: u32 },
}

impl ParagraphListStyle {
    pub const MAX_START_AT: u32 = 1_000_000;

    fn validate(self) -> Result<(), ModelError> {
        if let Self::Numbered { start_at } = self
            && !(1..=Self::MAX_START_AT).contains(&start_at)
        {
            return Err(ModelError::ParagraphListStartOutOfRange);
        }
        Ok(())
    }
}

/// Paragraph overrides relative to the containing Text Box's base Text Style.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParagraphStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_style: Option<ParagraphListStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alignment: Option<TextAlignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_indent_um: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_indent_um: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_line_indent_um: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_height_per_thousand: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_before_millipoints: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_after_millipoints: Option<u32>,
}

impl ParagraphStyle {
    pub const MAX_INDENT_MICROMETRES: i64 = 5_000_000;
    pub const MAX_SPACING_MILLIPOINTS: u32 = 1_000_000;

    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.list_style.is_none()
            && self.alignment.is_none()
            && self.start_indent_um.is_none()
            && self.end_indent_um.is_none()
            && self.first_line_indent_um.is_none()
            && self.line_height_per_thousand.is_none()
            && self.space_before_millipoints.is_none()
            && self.space_after_millipoints.is_none()
    }

    pub(crate) fn validate(self) -> Result<(), ModelError> {
        if self.is_default() {
            return Err(ModelError::EmptyParagraphStyle);
        }
        if let Some(list_style) = self.list_style {
            list_style.validate()?;
        }
        if self
            .start_indent_um
            .is_some_and(|value| !(0..=Self::MAX_INDENT_MICROMETRES).contains(&value))
            || self
                .end_indent_um
                .is_some_and(|value| !(0..=Self::MAX_INDENT_MICROMETRES).contains(&value))
            || self.first_line_indent_um.is_some_and(|value| {
                !(-Self::MAX_INDENT_MICROMETRES..=Self::MAX_INDENT_MICROMETRES).contains(&value)
            })
        {
            return Err(ModelError::ParagraphIndentOutOfRange);
        }
        if self
            .line_height_per_thousand
            .is_some_and(|value| !(500..=5_000).contains(&value))
        {
            return Err(ModelError::LineHeightOutOfRange);
        }
        if self
            .space_before_millipoints
            .is_some_and(|value| value > Self::MAX_SPACING_MILLIPOINTS)
            || self
                .space_after_millipoints
                .is_some_and(|value| value > Self::MAX_SPACING_MILLIPOINTS)
        {
            return Err(ModelError::ParagraphSpacingOutOfRange);
        }
        Ok(())
    }
}

/// One normalized, non-overlapping paragraph formatting range.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParagraphStyleRun {
    pub range: ParagraphRange,
    pub style: ParagraphStyle,
}

/// Durable selection-aware story formatting. Runs are ordered, disjoint, and
/// merged when adjacent runs have identical overrides.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextStoryFormatting {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline_spans: Vec<InlineTextSpan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paragraph_runs: Vec<ParagraphStyleRun>,
}

/// One source-ordered range with its effective Publisher-style weight and slant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveFontVariantRange {
    pub range: TextSourceRange,
    pub font_family: String,
    pub font_weight: u16,
    pub italic: bool,
}

/// Resolves normalized inline overrides into a linear, coalesced range sequence.
///
/// Installed-face selection and layout share this derivation so they agree on
/// exactly which concrete variants a rich-text selection needs.
#[must_use]
pub fn effective_font_variant_ranges(
    style: &TextStyle,
    formatting: &TextStoryFormatting,
    range: TextSourceRange,
) -> Vec<EffectiveFontVariantRange> {
    if range.start == range.end {
        return vec![EffectiveFontVariantRange {
            range,
            font_family: style.font_family.clone(),
            font_weight: style.font_weight,
            italic: style.italic,
        }];
    }

    let mut ranges = Vec::new();
    let mut cursor = range.start;
    for span in &formatting.inline_spans {
        if span.range.end <= range.start {
            continue;
        }
        if span.range.start >= range.end {
            break;
        }
        let span_start = span.range.start.max(range.start);
        let span_end = span.range.end.min(range.end);
        if cursor < span_start {
            push_effective_font_range(&mut ranges, cursor, span_start, style, None);
        }
        if span_start < span_end {
            push_effective_font_range(&mut ranges, span_start, span_end, style, Some(&span.style));
            cursor = span_end;
        }
    }
    if cursor < range.end {
        push_effective_font_range(&mut ranges, cursor, range.end, style, None);
    }
    ranges
}

fn push_effective_font_range(
    ranges: &mut Vec<EffectiveFontVariantRange>,
    start: usize,
    end: usize,
    base: &TextStyle,
    inline: Option<&InlineTextStyle>,
) {
    let font_family = inline
        .and_then(|value| value.font_family.as_ref())
        .unwrap_or(&base.font_family);
    let font_weight = inline.and_then(|value| value.bold).map_or_else(
        || {
            inline
                .and_then(|value| value.font_weight)
                .unwrap_or(base.font_weight)
        },
        |bold| if bold { 700 } else { 400 },
    );
    let italic = inline.and_then(|value| value.italic).unwrap_or(base.italic);
    if let Some(previous) = ranges.last_mut()
        && previous.range.end == start
        && previous.font_family == *font_family
        && previous.font_weight == font_weight
        && previous.italic == italic
    {
        previous.range.end = end;
    } else {
        ranges.push(EffectiveFontVariantRange {
            range: TextSourceRange { start, end },
            font_family: font_family.clone(),
            font_weight,
            italic,
        });
    }
}

impl TextStoryFormatting {
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.inline_spans.is_empty() && self.paragraph_runs.is_empty()
    }

    pub(crate) fn validate(&self, text: &str) -> Result<(), ModelError> {
        let mut previous_inline: Option<&InlineTextSpan> = None;
        for span in &self.inline_spans {
            span.range.validate(text)?;
            if span
                .style
                .font_family
                .as_ref()
                .is_some_and(|family| family.trim().is_empty())
            {
                return Err(ModelError::EmptyFontFamily);
            }
            if span
                .style
                .font_weight
                .is_some_and(|weight| !(1..=1_000).contains(&weight))
            {
                return Err(ModelError::FontWeightOutOfRange);
            }
            if span
                .style
                .font_size_millipoints
                .is_some_and(|size| !(1_000..=1_000_000).contains(&size))
            {
                return Err(ModelError::FontSizeOutOfRange);
            }
            if span.style.is_default() {
                return Err(ModelError::EmptyInlineTextStyle);
            }
            if let Some(previous) = previous_inline {
                if span.range.start < previous.range.end {
                    return Err(ModelError::OverlappingInlineTextSpans);
                }
                if span.range.start == previous.range.end && span.style == previous.style {
                    return Err(ModelError::TextFormattingNotNormalized);
                }
            }
            previous_inline = Some(span);
        }

        let paragraph_count = paragraph_count(text)?;
        let mut previous_paragraph: Option<&ParagraphStyleRun> = None;
        for run in &self.paragraph_runs {
            run.range.validate(paragraph_count)?;
            run.style.validate()?;
            if let Some(previous) = previous_paragraph {
                if run.range.start < previous.range.end {
                    return Err(ModelError::OverlappingParagraphStyleRuns);
                }
                if run.range.start == previous.range.end && run.style == previous.style {
                    return Err(ModelError::TextFormattingNotNormalized);
                }
            }
            previous_paragraph = Some(run);
        }
        Ok(())
    }
}

/// Counts canonical paragraphs. Only LF separates paragraphs, so CR remains
/// source content and a trailing LF creates one final empty paragraph.
///
/// # Errors
///
/// Returns [`ModelError::TooManyParagraphs`] if the count cannot fit in the
/// persisted paragraph-index representation.
pub fn paragraph_count(text: &str) -> Result<u32, ModelError> {
    let delimiters = text.matches('\n').count();
    u32::try_from(delimiters.saturating_add(1)).map_err(|_| ModelError::TooManyParagraphs)
}

/// Whole-object text formatting retained independently from frame geometry.
pub const MAX_EXACT_FONT_VARIANTS: usize = 8;
pub const MAX_EXACT_FONT_FAMILY_BINDINGS: usize = 32;

/// Exact face for an additional family used by a selected source range.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExactFontFamilyBinding {
    pub font_family: String,
    pub font_weight: u16,
    pub italic: bool,
    pub face: FontFaceBinding,
}

/// Exact embedded face for one non-base weight/slant combination in a Text Style.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExactFontVariantBinding {
    pub font_weight: u16,
    pub italic: bool,
    pub face: FontFaceBinding,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextStyle {
    /// Exact embedded font resource selected for this text.
    ///
    /// The family remains an authoring label only. When this binding is
    /// present, composition and export must use these exact bytes and face
    /// rather than resolving by family on the current machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_face: Option<FontFaceBinding>,
    /// Exact embedded faces for inline weight/slant overrides.
    ///
    /// Entries are sorted and unique by `(font_weight, italic)`. The base
    /// combination remains represented by `font_face`, not this registry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exact_font_variants: Vec<ExactFontVariantBinding>,
    /// Additional range families, ordered by family, weight, and slant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exact_font_families: Vec<ExactFontFamilyBinding>,
    pub font_family: String,
    pub font_size_millipoints: u32,
    pub font_weight: u16,
    pub italic: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub underline: bool,
    pub alignment: TextAlignment,
    pub color: Color,
    pub line_height_per_thousand: u16,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_face: None,
            exact_font_variants: Vec::new(),
            exact_font_families: Vec::new(),
            font_family: "Arial".to_owned(),
            font_size_millipoints: 12_000,
            font_weight: 400,
            italic: false,
            underline: false,
            alignment: TextAlignment::Left,
            color: Color {
                red: 23,
                green: 63,
                blue: 49,
                alpha: 255,
            },
            line_height_per_thousand: 1_200,
        }
    }
}

/// Durable address of one face inside an embedded OpenType resource.
///
/// `source` is the SHA-256 of the exact bytes stored with the project; a
/// collection uses `face_index` to distinguish its faces. It intentionally
/// contains no family name because installed family names are not stable font
/// identities.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FontFaceBinding {
    pub source: AssetHash,
    pub face_index: u32,
}

/// Script whose missing characters may use one explicitly embedded fallback chain.
///
/// The list is intentionally finite and serialized by familiar script name rather
/// than exposing OpenType tags in the publication package or eventual interface.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FontFallbackScript {
    Arabic,
    Bengali,
    Cyrillic,
    Devanagari,
    Greek,
    Gujarati,
    Gurmukhi,
    Han,
    Hangul,
    Hebrew,
    Hiragana,
    Kannada,
    Katakana,
    Latin,
    Malayalam,
    Tamil,
    Telugu,
    Thai,
}

impl FontFallbackScript {
    /// Returns the corresponding four-byte OpenType script tag.
    #[must_use]
    pub const fn open_type_tag(self) -> [u8; 4] {
        match self {
            Self::Arabic => *b"Arab",
            Self::Bengali => *b"Beng",
            Self::Cyrillic => *b"Cyrl",
            Self::Devanagari => *b"Deva",
            Self::Greek => *b"Grek",
            Self::Gujarati => *b"Gujr",
            Self::Gurmukhi => *b"Guru",
            Self::Han => *b"Hani",
            Self::Hangul => *b"Hang",
            Self::Hebrew => *b"Hebr",
            Self::Hiragana => *b"Hira",
            Self::Kannada => *b"Knda",
            Self::Katakana => *b"Kana",
            Self::Latin => *b"Latn",
            Self::Malayalam => *b"Mlym",
            Self::Tamil => *b"Taml",
            Self::Telugu => *b"Telu",
            Self::Thai => *b"Thai",
        }
    }
}

/// Ordered exact fallback faces permitted for one script throughout a publication.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExactFontFallbackChain {
    pub script: FontFallbackScript,
    /// First face has highest priority. Every face remains content-hash bound.
    pub faces: Vec<FontFaceBinding>,
}

pub const MAX_EXACT_FONT_FALLBACK_SCRIPTS: usize = 18;
pub const MAX_EXACT_FONT_FALLBACK_FACES_PER_SCRIPT: usize = 8;
/// Keeps publication-wide fallback resources below the native preview/export
/// boundary while leaving room for the document's directly authored faces.
pub const MAX_EXACT_FONT_FALLBACK_FACES_TOTAL: usize = 32;

impl TextStyle {
    /// Resolves a captured family without substituting another authored family.
    #[must_use]
    pub fn exact_font_face_for_family(
        &self,
        family: &str,
        font_weight: u16,
        italic: bool,
    ) -> Option<&FontFaceBinding> {
        if family == self.font_family
            && let Some(face) = self.exact_font_face_for(font_weight, italic)
        {
            return Some(face);
        }
        self.exact_font_families
            .binary_search_by(|binding| {
                (
                    binding.font_family.as_str(),
                    binding.font_weight,
                    binding.italic,
                )
                    .cmp(&(family, font_weight, italic))
            })
            .ok()
            .map(|index| &self.exact_font_families[index].face)
    }
    /// Returns the exact embedded face for one effective weight/slant key.
    ///
    /// Missing variants deliberately return `None`; callers must not silently
    /// substitute the base face or resolve a machine-local family instead.
    #[must_use]
    pub fn exact_font_face_for(&self, font_weight: u16, italic: bool) -> Option<&FontFaceBinding> {
        if (font_weight, italic) == (self.font_weight, self.italic) {
            return self.font_face.as_ref();
        }
        self.exact_font_variants
            .binary_search_by_key(&(font_weight, italic), |variant| {
                (variant.font_weight, variant.italic)
            })
            .ok()
            .map(|index| &self.exact_font_variants[index].face)
    }

    fn validate(&self) -> Result<(), ModelError> {
        if self.font_family.trim().is_empty() {
            return Err(ModelError::EmptyFontFamily);
        }
        if !(1_000..=1_000_000).contains(&self.font_size_millipoints) {
            return Err(ModelError::FontSizeOutOfRange);
        }
        if !(1..=1_000).contains(&self.font_weight) {
            return Err(ModelError::FontWeightOutOfRange);
        }
        if self.exact_font_variants.len() > MAX_EXACT_FONT_VARIANTS {
            return Err(ModelError::TooManyExactFontVariants);
        }
        if self.exact_font_families.len() > MAX_EXACT_FONT_FAMILY_BINDINGS {
            return Err(ModelError::TooManyExactFontVariants);
        }
        let mut previous_family_key = None;
        for binding in &self.exact_font_families {
            if binding.font_family.trim().is_empty() {
                return Err(ModelError::EmptyFontFamily);
            }
            if !(1..=1_000).contains(&binding.font_weight) {
                return Err(ModelError::FontWeightOutOfRange);
            }
            let key = (
                binding.font_family.as_str(),
                binding.font_weight,
                binding.italic,
            );
            if previous_family_key.is_some_and(|previous| previous >= key) {
                return Err(ModelError::ExactFontVariantsNotNormalized);
            }
            previous_family_key = Some(key);
        }
        let mut previous_key = None;
        for variant in &self.exact_font_variants {
            let key = (variant.font_weight, variant.italic);
            if !(1..=1_000).contains(&variant.font_weight) {
                return Err(ModelError::FontWeightOutOfRange);
            }
            if key == (self.font_weight, self.italic) {
                return Err(ModelError::ExactFontVariantMatchesBaseStyle);
            }
            if previous_key.is_some_and(|previous| previous >= key) {
                return Err(ModelError::ExactFontVariantsNotNormalized);
            }
            previous_key = Some(key);
        }
        if !(500..=5_000).contains(&self.line_height_per_thousand) {
            return Err(ModelError::LineHeightOutOfRange);
        }
        Ok(())
    }
}

/// Whether a Text Box follows its composed content or retains authored bounds.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextBoxSizingMode {
    Fit,
    #[default]
    Fixed,
}

impl TextBoxSizingMode {
    #[allow(clippy::trivially_copy_pass_by_ref)] // Serde's skip predicate receives `&T`.
    const fn is_fixed(&self) -> bool {
        matches!(self, Self::Fixed)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextVerticalAlignment {
    #[default]
    Top,
    Middle,
    Bottom,
}

impl TextVerticalAlignment {
    #[allow(clippy::trivially_copy_pass_by_ref)] // Serde's skip predicate receives `&T`.
    const fn is_top(&self) -> bool {
        matches!(self, Self::Top)
    }
}

/// Whole-frame composition settings kept separate from font formatting.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextFrame {
    /// Defaults to Fixed so publications written before schema 11 retain their
    /// authored Text Box geometry.
    #[serde(default, skip_serializing_if = "TextBoxSizingMode::is_fixed")]
    pub sizing_mode: TextBoxSizingMode,
    pub column_count: u16,
    pub column_gap_um: i64,
    #[serde(default, skip_serializing_if = "Insets::is_zero")]
    pub margins: Insets,
    #[serde(default, skip_serializing_if = "TextVerticalAlignment::is_top")]
    pub vertical_alignment: TextVerticalAlignment,
}

impl Default for TextFrame {
    fn default() -> Self {
        Self {
            sizing_mode: TextBoxSizingMode::Fixed,
            column_count: 1,
            column_gap_um: 6_350,
            margins: Insets::default(),
            vertical_alignment: TextVerticalAlignment::Top,
        }
    }
}

impl TextFrame {
    /// Empty and whitespace-only Fit Text Boxes use a visible quarter-inch
    /// fallback instead of persisting zero-sized geometry.
    pub const MIN_FIT_WIDTH_MICROMETRES: i64 = 6_350;
    pub const MIN_FIT_HEIGHT_MICROMETRES: i64 = 6_350;

    fn validate(self) -> Result<(), ModelError> {
        if !(1..=12).contains(&self.column_count) {
            return Err(ModelError::TextColumnCountOutOfRange);
        }
        if !(0..=1_000_000).contains(&self.column_gap_um) {
            return Err(ModelError::TextColumnGapOutOfRange);
        }
        self.margins.validate()?;
        if [
            self.margins.top,
            self.margins.right,
            self.margins.bottom,
            self.margins.left,
        ]
        .into_iter()
        .any(|value| value.value() > 1_000_000)
        {
            return Err(ModelError::TextMarginOutOfRange);
        }
        Ok(())
    }

    pub(crate) fn validate_for_size(self, size: PhysicalSize) -> Result<(), ModelError> {
        self.validate()?;
        if self.sizing_mode == TextBoxSizingMode::Fit
            && (size.width.value() < Self::MIN_FIT_WIDTH_MICROMETRES
                || size.height.value() < Self::MIN_FIT_HEIGHT_MICROMETRES)
        {
            return Err(ModelError::FitTextSizeBelowMinimum);
        }
        Ok(())
    }

    /// Prospective authoring edits must leave a positive interior and at least
    /// one physical unit per column. Legacy boxes remain readable even when
    /// their existing column geometry cannot compose.
    pub(crate) fn validate_usable_for_size(self, size: PhysicalSize) -> Result<(), ModelError> {
        self.validate_for_size(size)?;
        if self.sizing_mode == TextBoxSizingMode::Fit {
            return Ok(());
        }
        let width = size.width.value() - self.margins.left.value() - self.margins.right.value();
        let height = size.height.value() - self.margins.top.value() - self.margins.bottom.value();
        if width <= 0 || height <= 0 {
            return Err(ModelError::TextInteriorDoesNotFit);
        }
        let gaps = self.column_gap_um * i64::from(self.column_count - 1);
        if width - gaps < i64::from(self.column_count) {
            return Err(ModelError::TextColumnsDoNotFit);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ObjectDraft {
    pub name: String,
    pub size: PhysicalSize,
    pub transform: Transform,
    pub opacity_millionths: u32,
    pub kind: ObjectKind,
}

impl ObjectDraft {
    /// Validates the persisted constraints of an object draft.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when opacity or transform values are outside the
    /// canonical model's accepted ranges.
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.opacity_millionths > 1_000_000 {
            return Err(ModelError::OpacityOutOfRange);
        }
        self.size.validate()?;
        self.transform.validate()?;
        self.kind.validate_for_size(self.size)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PublicationObject {
    pub id: ObjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<GroupId>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub locked: bool,
    pub name: String,
    pub size: PhysicalSize,
    pub transform: Transform,
    pub opacity_millionths: u32,
    #[serde(default, skip_serializing_if = "ObjectEffects::is_default")]
    pub effects: ObjectEffects,
    #[serde(default, skip_serializing_if = "WrapSettings::is_default")]
    pub wrap_settings: WrapSettings,
    #[serde(default, skip_serializing_if = "CornerRounding::is_default")]
    pub corner_rounding: CornerRounding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_field: Option<TextField>,
    pub kind: ObjectKind,
}

/// The direction of a page guide. A vertical guide measures from the page's
/// left trim edge; a horizontal guide measures from its top trim edge.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuideAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PageGuide {
    pub id: GuideId,
    pub axis: GuideAxis,
    pub position: Micrometres,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // Serde's skip predicate receives `&T`.
fn is_false(value: &bool) -> bool {
    !*value
}

impl PublicationObject {
    fn from_draft(id: ObjectId, draft: ObjectDraft) -> Self {
        Self {
            id,
            group_id: None,
            locked: false,
            name: draft.name,
            size: draft.size,
            transform: draft.transform,
            opacity_millionths: draft.opacity_millionths,
            effects: ObjectEffects::default(),
            wrap_settings: WrapSettings::default(),
            corner_rounding: CornerRounding::default(),
            text_field: None,
            kind: draft.kind,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Page {
    pub id: PageId,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub trim: PhysicalSize,
    pub bleed: Insets,
    pub bleed_expectations: EdgeExpectations,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guides: Vec<PageGuide>,
    pub objects: Vec<PublicationObject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_id: Option<MasterId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numbering_section: Option<PageNumberingSection>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub suppress_page_number: bool,
}

impl Page {
    pub const MAX_GUIDES: usize = 200;
    pub const MAX_NAME_BYTES: usize = 256;

    fn validate_name(&self) -> Result<(), ModelError> {
        if self.name.len() > Self::MAX_NAME_BYTES || (!self.name.is_empty() && self.name.trim().is_empty()) {
            return Err(ModelError::InvalidPageName);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Pasteboard {
    pub objects: Vec<PublicationObject>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Document {
    pub id: DocumentId,
    pub title: String,
    #[serde(default)]
    pub color_settings: DocumentColorSettings,
    /// Publication-wide, explicitly permitted exact fallback chains.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exact_font_fallbacks: Vec<ExactFontFallbackChain>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masters: Vec<MasterPage>,
    pub pages: Vec<Page>,
    pub pasteboard: Pasteboard,
    next_id: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ModelGroupContainer {
    Page(PageId),
    Pasteboard,
    Master(MasterId),
}

impl Document {
    pub const MAX_INITIAL_PAGES: u16 = 500;

    /// Creates a publication with one initial page.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the trim size or bleed insets are invalid.
    pub fn new(
        id: DocumentId,
        title: impl Into<String>,
        trim: PhysicalSize,
        bleed: Insets,
    ) -> Result<Self, ModelError> {
        Self::new_with_page_count(id, title, trim, bleed, 1)
    }

    /// Creates a publication with its complete initial page set and no edit history.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when the page count, trim size, or bleed insets are invalid.
    pub fn new_with_page_count(
        id: DocumentId,
        title: impl Into<String>,
        trim: PhysicalSize,
        bleed: Insets,
        page_count: u16,
    ) -> Result<Self, ModelError> {
        if !(1..=Self::MAX_INITIAL_PAGES).contains(&page_count) {
            return Err(ModelError::InitialPageCountOutOfRange);
        }
        trim.validate()?;
        bleed.validate()?;
        let mut document = Self {
            id,
            title: title.into(),
            color_settings: DocumentColorSettings::default(),
            exact_font_fallbacks: Vec::new(),
            masters: Vec::new(),
            pages: Vec::new(),
            pasteboard: Pasteboard {
                objects: Vec::new(),
            },
            next_id: 1,
        };
        for _ in 0..page_count {
            let page_id = document.allocate_page_id();
            document.pages.push(Page {
                id: page_id,
                name: String::new(),
                trim,
                bleed,
                bleed_expectations: EdgeExpectations::default(),
                guides: Vec::new(),
                objects: Vec::new(),
                master_id: None,
                numbering_section: None,
                suppress_page_number: false,
            });
        }
        Ok(document)
    }

    /// Validates document identity, geometry, object, and allocation invariants.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when any canonical document invariant is broken.
    #[allow(clippy::too_many_lines)] // Identity, geometry, master assignment and group topology are admitted together.
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.pages.is_empty() {
            return Err(ModelError::DocumentHasNoPages);
        }
        if self.next_id == 0 {
            return Err(ModelError::InvalidNextId);
        }
        if self.exact_font_fallbacks.len() > MAX_EXACT_FONT_FALLBACK_SCRIPTS {
            return Err(ModelError::TooManyExactFontFallbackScripts);
        }
        if self
            .exact_font_fallbacks
            .iter()
            .map(|chain| chain.faces.len())
            .sum::<usize>()
            > MAX_EXACT_FONT_FALLBACK_FACES_TOTAL
        {
            return Err(ModelError::TooManyExactFontFallbackFaces);
        }
        let mut previous_script = None;
        for chain in &self.exact_font_fallbacks {
            if previous_script.is_some_and(|previous| previous >= chain.script) {
                return Err(ModelError::ExactFontFallbacksNotNormalized);
            }
            if chain.faces.is_empty()
                || chain.faces.len() > MAX_EXACT_FONT_FALLBACK_FACES_PER_SCRIPT
                || chain
                    .faces
                    .iter()
                    .enumerate()
                    .any(|(index, face)| chain.faces[..index].contains(face))
            {
                return Err(ModelError::InvalidExactFontFallbackChain);
            }
            previous_script = Some(chain.script);
        }
        let mut page_ids = HashSet::new();
        let mut object_ids = HashSet::new();
        let mut guide_ids = HashSet::new();
        let mut groups: HashMap<GroupId, (ModelGroupContainer, usize)> = HashMap::new();
        let mut greatest_generated_id = 0;
        for page in &self.pages {
            if !page_ids.insert(page.id.clone()) {
                return Err(ModelError::DuplicatePageId(page.id.clone()));
            }
            greatest_generated_id =
                greatest_generated_id.max(page.id.0.generated_counter("page").unwrap_or_default());
            page.trim.validate()?;
            page.validate_name()?;
            if let Some(section) = page.numbering_section { section.validate()?; }
            if let Some(master_id) = &page.master_id {
                let master = self.masters.iter().find(|master| &master.id == master_id)
                    .ok_or_else(|| ModelError::MasterNotFound(master_id.clone()))?;
                if master.trim != page.trim { return Err(ModelError::MasterPageSizeMismatch); }
            }
            page.bleed.validate()?;
            if page.guides.len() > Page::MAX_GUIDES {
                return Err(ModelError::TooManyGuides);
            }
            for guide in &page.guides {
                if !guide_ids.insert(guide.id.clone()) {
                    return Err(ModelError::DuplicateGuideId(guide.id.clone()));
                }
                let maximum = match guide.axis {
                    GuideAxis::Horizontal => page.trim.height.value(),
                    GuideAxis::Vertical => page.trim.width.value(),
                };
                if !(0..=maximum).contains(&guide.position.value()) {
                    return Err(ModelError::GuidePositionOutOfRange);
                }
                greatest_generated_id = greatest_generated_id
                    .max(guide.id.0.generated_counter("guide").unwrap_or_default());
            }
            for object in &page.objects {
                Self::validate_object(object, &mut object_ids)?;
                greatest_generated_id = greatest_generated_id
                    .max(object.id.0.generated_counter("object").unwrap_or_default());
                Self::record_group(
                    object,
                    &ModelGroupContainer::Page(page.id.clone()),
                    &mut groups,
                    &mut greatest_generated_id,
                )?;
            }
        }
        for object in &self.pasteboard.objects {
            Self::validate_object(object, &mut object_ids)?;
            greatest_generated_id = greatest_generated_id
                .max(object.id.0.generated_counter("object").unwrap_or_default());
            Self::record_group(object, &ModelGroupContainer::Pasteboard, &mut groups, &mut greatest_generated_id)?;
        }
        if self.masters.len() > Self::MAX_MASTERS { return Err(ModelError::TooManyMasters); }
        let mut master_ids = HashSet::new();
        for master in &self.masters {
            if !master_ids.insert(&master.id) { return Err(ModelError::DuplicateMasterId(master.id.clone())); }
            if master.name.trim().is_empty() || master.name.len() > 256 { return Err(ModelError::InvalidMasterName); }
            master.trim.validate()?;
            greatest_generated_id = greatest_generated_id.max(master.id.0.generated_counter("master").unwrap_or_default());
            for object in &master.objects {
                Self::validate_object(object, &mut object_ids)?;
                greatest_generated_id = greatest_generated_id.max(object.id.0.generated_counter("object").unwrap_or_default());
                Self::record_group(object, &ModelGroupContainer::Master(master.id.clone()), &mut groups, &mut greatest_generated_id)?;
            }
        }
        self.page_numbers()?;
        if groups.values().any(|(_, member_count)| *member_count < 2) {
            return Err(ModelError::GroupHasFewerThanTwoMembers);
        }
        if self.next_id <= greatest_generated_id {
            return Err(ModelError::InvalidNextId);
        }
        Ok(())
    }

    fn validate_object(
        object: &PublicationObject,
        ids: &mut HashSet<ObjectId>,
    ) -> Result<(), ModelError> {
        if !ids.insert(object.id.clone()) {
            return Err(ModelError::DuplicateObjectId(object.id.clone()));
        }
        if object.opacity_millionths > 1_000_000 {
            return Err(ModelError::OpacityOutOfRange);
        }
        object.size.validate()?;
        object.transform.validate()?;
        object.kind.validate_for_size(object.size)?;
        if object.text_field.is_some()
            && !matches!(&object.kind, ObjectKind::Text { text, story, .. } if text.is_empty() && story.is_default())
        {
            return Err(ModelError::TextFieldRequiresEmptyText);
        }
        object.wrap_settings.validate()?;
        crate::shape_geometry::validate_corner_interior(object)?;
        if !object.wrap_settings.is_default()
            && !matches!(object.kind, ObjectKind::Image { .. } | ObjectKind::Shape { .. })
        {
            return Err(ModelError::WrapSettingsNotSupported);
        }
        object.effects.validate_for(&object.kind)
    }

    fn record_group(
        object: &PublicationObject,
        location: &ModelGroupContainer,
        groups: &mut HashMap<GroupId, (ModelGroupContainer, usize)>,
        greatest_generated_id: &mut u64,
    ) -> Result<(), ModelError> {
        let Some(group_id) = &object.group_id else {
            return Ok(());
        };
        *greatest_generated_id =
            (*greatest_generated_id).max(group_id.0.generated_counter("group").unwrap_or_default());
        let entry = groups
            .entry(group_id.clone())
            .or_insert_with(|| (location.clone(), 0));
        if &entry.0 != location {
            return Err(ModelError::GroupSpansContainers(group_id.clone()));
        }
        entry.1 += 1;
        Ok(())
    }

    pub(crate) fn allocate_page_id(&mut self) -> PageId {
        let id = PageId(StableId::generated("page", self.next_id));
        self.next_id += 1;
        id
    }

    pub(crate) fn allocate_object_id(&mut self) -> ObjectId {
        let id = ObjectId(StableId::generated("object", self.next_id));
        self.next_id += 1;
        id
    }

    pub(crate) fn allocate_master_id(&mut self) -> MasterId {
        let id = MasterId(StableId::generated("master", self.next_id));
        self.next_id += 1;
        id
    }

    pub(crate) fn allocate_group_id(&mut self) -> GroupId {
        let id = GroupId(StableId::generated("group", self.next_id));
        self.next_id += 1;
        id
    }

    pub(crate) fn allocate_guide_id(&mut self) -> GuideId {
        let id = GuideId(StableId::generated("guide", self.next_id));
        self.next_id += 1;
        id
    }

    pub(crate) fn object_from_draft(&mut self, draft: ObjectDraft) -> PublicationObject {
        PublicationObject::from_draft(self.allocate_object_id(), draft)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelError {
    InvalidStableId,
    NonPositiveSize,
    NegativeInset,
    ZeroScale,
    RotationNotNormalized,
    OpacityOutOfRange,
    DocumentHasNoPages,
    InitialPageCountOutOfRange,
    DuplicatePageId(PageId),
    DuplicateMasterId(MasterId),
    MasterNotFound(MasterId),
    PageReferenceNotFound(PageId),
    MasterPageSizeMismatch,
    MasterRemovalReplacementIsSame,
    InvalidMasterName,
    InvalidPageName,
    TooManyMasters,
    PageNumberStartOutOfRange,
    PageNumberOverflow,
    TextFieldRequiresEmptyText,
    DuplicateObjectId(ObjectId),
    DuplicateGuideId(GuideId),
    GroupSpansContainers(GroupId),
    GroupHasFewerThanTwoMembers,
    InvalidNextId,
    InvalidTableDimensions,
    TableCellOutOfRange,
    TableGridBorderWidthOutOfRange,
    InvalidTableCellFormatting,
    InvalidTableCellRange,
    InvalidTableMerge,
    InvalidTableTrackWeights,
    TableTrackOutOfRange,
    TableTrackWeightOutOfRange,
    EmptyFontFamily,
    FontSizeOutOfRange,
    FontWeightOutOfRange,
    TooManyExactFontVariants,
    ExactFontVariantsNotNormalized,
    ExactFontVariantMatchesBaseStyle,
    TooManyExactFontFallbackScripts,
    TooManyExactFontFallbackFaces,
    ExactFontFallbacksNotNormalized,
    InvalidExactFontFallbackChain,
    LineHeightOutOfRange,
    TextTooLarge,
    TextSourceRangeOutOfRange,
    TextSourceRangeNotUtf8Boundary,
    EmptyInlineTextStyle,
    OverlappingInlineTextSpans,
    ParagraphRangeOutOfRange,
    EmptyParagraphStyle,
    ParagraphIndentOutOfRange,
    ParagraphSpacingOutOfRange,
    ParagraphListStartOutOfRange,
    OverlappingParagraphStyleRuns,
    TextFormattingNotNormalized,
    TooManyParagraphs,
    TextColumnCountOutOfRange,
    TextColumnGapOutOfRange,
    TextMarginOutOfRange,
    TextInteriorDoesNotFit,
    TextColumnsDoNotFit,
    WrapDistanceOutOfRange,
    WrapSettingsNotSupported,
    CornerRadiusOutOfRange,
    LinkedCornersDiffer,
    CornerRoundingNotSupported,
    CornerInteriorDoesNotFit,
    FitTextSizeBelowMinimum,
    CoordinateOverflow,
    ShapeStrokeWidthOutOfRange,
    HardShadowOffsetOutOfRange,
    TextOutlineKindMismatch,
    PictureBorderKindMismatch,
    ImageCropOutOfRange,
    GuidePositionOutOfRange,
    TooManyGuides,
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guidance = match self {
            Self::MasterNotFound(_) => Some("That Master no longer exists. Choose an available Master or None."),
            Self::DuplicateMasterId(_) => Some("Two Masters use the same identity. This publication cannot be opened safely."),
            Self::MasterPageSizeMismatch => Some("The Page and Master must have the same physical size. Duplicate and Resize Master, then apply the resized copy."),
            Self::MasterRemovalReplacementIsSame => Some("Choose another Master or None before removing this Master."),
            Self::InvalidMasterName => Some("Give the Master a name between one and 256 bytes."),
            Self::InvalidPageName => Some("Give the Page a name up to 256 bytes, or clear its name to use the automatic label."),
            Self::TooManyMasters => Some("This publication supports at most 500 Masters. Remove an unused Master before creating another."),
            Self::PageNumberStartOutOfRange => Some("Choose a section starting number between 1 and 1,000,000."),
            Self::PageNumberOverflow => Some("The page-number sequence is too large. Restart the section at a smaller number."),
            Self::TextFieldRequiresEmptyText => Some("Automatic page numbers use an empty Text Box. Keep authored text in another Text Box."),
            Self::InvalidTableCellRange => Some("Choose a nonempty range inside the Table."),
            Self::InvalidTableMerge => Some("Select the whole merged cell before changing its cells, formatting, or merge."),
            Self::TextInteriorDoesNotFit => Some(
                "The margins leave no usable space. Reduce the margins or enlarge the Text Box.",
            ),
            Self::TextColumnsDoNotFit => {
                Some("The column gap leaves no usable width. Reduce the gap or widen the Text Box.")
            }
            Self::TextMarginOutOfRange => Some("A Text Box margin is outside the supported range."),
            Self::WrapDistanceOutOfRange => Some("Wrap distance is outside the supported range."),
            Self::WrapSettingsNotSupported => {
                Some("Wrap Text settings apply to Pictures and Shapes.")
            }
            Self::CornerRadiusOutOfRange => Some("A corner radius is outside the supported range."),
            Self::LinkedCornersDiffer => Some("Linked corners must use the same radius."),
            Self::CornerRoundingNotSupported => Some("Corner rounding applies to Pictures, Text Boxes, Tables and rectangular Shapes."),
            Self::CornerInteriorDoesNotFit => Some("The rounded corners leave no usable space for text or cells. Reduce the corner radii or enlarge the object."),
            _ => None,
        };
        if let Some(guidance) = guidance {
            return formatter.write_str(guidance);
        }
        write!(formatter, "invalid canonical document: {self:?}")
    }
}

impl std::error::Error for ModelError {}
