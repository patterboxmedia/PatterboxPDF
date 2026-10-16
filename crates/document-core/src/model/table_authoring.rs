use super::{
    Color, InlineTextStyle, MAX_TEXT_BYTES, ModelError, TableCellFormatting, TableData,
    TableGridBorder, TableTrackAxis, TableVerticalAlignment, TextAlignment, TextStyle,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Half-open track coordinates. A merged cell uses this same rectangle and
/// stores all its editable text at the top-left address.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TableCellRange {
    pub start_row: u16,
    pub end_row: u16,
    pub start_column: u16,
    pub end_column: u16,
}

impl TableCellRange {
    /// # Errors
    /// Refuses empty or out-of-range rectangles before a Table mutation.
    pub fn validate(self, table: &TableData) -> Result<(), ModelError> {
        if self.start_row >= self.end_row
            || self.start_column >= self.end_column
            || self.end_row > table.rows
            || self.end_column > table.columns
        {
            return Err(ModelError::InvalidTableCellRange);
        }
        Ok(())
    }
    #[must_use]
    pub const fn contains(self, row: u16, column: u16) -> bool {
        row >= self.start_row
            && row < self.end_row
            && column >= self.start_column
            && column < self.end_column
    }
    #[must_use]
    pub const fn contains_range(self, other: Self) -> bool {
        self.start_row <= other.start_row
            && self.end_row >= other.end_row
            && self.start_column <= other.start_column
            && self.end_column >= other.end_column
    }
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.start_row < other.end_row
            && other.start_row < self.end_row
            && self.start_column < other.end_column
            && other.start_column < self.end_column
    }
}

/// Missing edges inherit the Table grid. An explicit zero-width edge removes
/// that border. At a shared edge, the cell below/right wins conflicting explicit
/// overrides; a lone explicit override wins over the uniform grid.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TableCellBorders {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<TableGridBorder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<TableGridBorder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bottom: Option<TableGridBorder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<TableGridBorder>,
}
impl TableCellBorders {
    #[must_use]
    pub const fn is_default(&self) -> bool {
        self.top.is_none() && self.right.is_none() && self.bottom.is_none() && self.left.is_none()
    }
    fn validate(self) -> Result<(), ModelError> {
        for border in [self.top, self.right, self.bottom, self.left]
            .into_iter()
            .flatten()
        {
            border.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TableCellFormattingPatch {
    pub horizontal_alignment: Option<TextAlignment>,
    pub vertical_alignment: Option<TableVerticalAlignment>,
    pub style: InlineTextStyle,
    pub fill: Option<Color>,
    pub borders: TableCellBorders,
}

impl TableData {
    #[must_use]
    pub fn cell_format(&self, row: u16, column: u16) -> Option<&TableCellFormatting> {
        self.cell_formatting
            .binary_search_by_key(&(row, column), |value| (value.row, value.column))
            .ok()
            .map(|index| &self.cell_formatting[index])
    }
    #[must_use]
    pub fn cell_span(&self, row: u16, column: u16) -> Option<TableCellRange> {
        self.merged_cells
            .iter()
            .find(|span| span.contains(row, column))
            .copied()
    }
    /// Effective uniform cell text shares the same explicit face and Bold/weight
    /// precedence as selected Text Box text. Exact faces remain in the Table registry.
    #[must_use]
    pub fn effective_cell_style(&self, row: u16, column: u16) -> TextStyle {
        let mut style = self.style.clone();
        let Some(formatting) = self.cell_format(row, column) else {
            return style;
        };
        let inline = &formatting.style;
        if let Some(family) = &inline.font_family {
            style.font_family.clone_from(family);
        }
        style.font_weight =
            inline
                .bold
                .map_or(inline.font_weight.unwrap_or(style.font_weight), |bold| {
                    if bold { 700 } else { 400 }
                });
        style.italic = inline.italic.unwrap_or(style.italic);
        style.font_size_millipoints = inline
            .font_size_millipoints
            .unwrap_or(style.font_size_millipoints);
        style.color = inline.color.unwrap_or(style.color);
        style.underline = inline.underline.unwrap_or(style.underline);
        style.alignment = formatting.horizontal_alignment.unwrap_or(style.alignment);
        style.font_face = self
            .style
            .exact_font_face_for_family(&style.font_family, style.font_weight, style.italic)
            .cloned();
        style.exact_font_variants.clear();
        style.exact_font_families.clear();
        style
    }
    pub(super) fn validate_cell_appearance(
        &self,
        formatting: &TableCellFormatting,
    ) -> Result<(), ModelError> {
        let inline = &formatting.style;
        if inline
            .font_family
            .as_ref()
            .is_some_and(|family| family.trim().is_empty())
        {
            return Err(ModelError::EmptyFontFamily);
        }
        if inline
            .font_weight
            .is_some_and(|weight| !(1..=1_000).contains(&weight))
        {
            return Err(ModelError::FontWeightOutOfRange);
        }
        if inline
            .font_size_millipoints
            .is_some_and(|size| !(1_000..=1_000_000).contains(&size))
        {
            return Err(ModelError::FontSizeOutOfRange);
        }
        self.effective_cell_style(formatting.row, formatting.column)
            .validate()?;
        formatting.borders.validate()
    }
    pub(super) fn validate_merged_cells(&self) -> Result<(), ModelError> {
        let mut occupied = vec![false; self.cells.len()];
        let mut previous = None;
        for span in &self.merged_cells {
            span.validate(self)?;
            let address = (span.start_row, span.start_column);
            if previous.is_some_and(|previous| previous >= address)
                || (span.end_row - span.start_row == 1 && span.end_column - span.start_column == 1)
            {
                return Err(ModelError::InvalidTableMerge);
            }
            previous = Some(address);
            for row in span.start_row..span.end_row {
                for column in span.start_column..span.end_column {
                    let index = self.cell_index(row, column)?;
                    if occupied[index]
                        || ((row, column) != address && !self.cells[index].is_empty())
                    {
                        return Err(ModelError::InvalidTableMerge);
                    }
                    occupied[index] = true;
                }
            }
        }
        Ok(())
    }
    fn validate_whole_merges(&self, range: TableCellRange) -> Result<(), ModelError> {
        range.validate(self)?;
        if self
            .merged_cells
            .iter()
            .any(|span| range.intersects(*span) && !range.contains_range(*span))
        {
            return Err(ModelError::InvalidTableMerge);
        }
        Ok(())
    }
    /// Editable content result shown before a populated merge. Nonempty source
    /// cells join in row-major order with LF; existing multiline text stays exact.
    /// # Errors
    /// Refuses partial merged selections and results beyond one cell's text limit.
    pub fn merge_preview(&self, range: TableCellRange) -> Result<String, ModelError> {
        self.validate_whole_merges(range)?;
        let mut result = String::new();
        for row in range.start_row..range.end_row {
            for column in range.start_column..range.end_column {
                let text = &self.cells[self.cell_index(row, column)?];
                if !text.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    if result
                        .len()
                        .checked_add(text.len())
                        .is_none_or(|length| length > MAX_TEXT_BYTES)
                    {
                        return Err(ModelError::TextTooLarge);
                    }
                    result.push_str(text);
                }
            }
        }
        Ok(result)
    }
    pub(crate) fn merge_cells(&mut self, range: TableCellRange) -> Result<(), ModelError> {
        if range.end_row.saturating_sub(range.start_row) == 1
            && range.end_column.saturating_sub(range.start_column) == 1
        {
            return Err(ModelError::InvalidTableMerge);
        }
        let text = self.merge_preview(range)?;
        for row in range.start_row..range.end_row {
            for column in range.start_column..range.end_column {
                let index = self.cell_index(row, column)?;
                self.cells[index].clear();
            }
        }
        let anchor = self.cell_index(range.start_row, range.start_column)?;
        self.cells[anchor] = text;
        self.merged_cells
            .retain(|span| !range.contains_range(*span));
        self.merged_cells.push(range);
        self.merged_cells
            .sort_by_key(|span| (span.start_row, span.start_column));
        Ok(())
    }
    pub(crate) fn split_cells(&mut self, range: TableCellRange) -> Result<(), ModelError> {
        self.validate_whole_merges(range)?;
        // Text already lives at the anchor. Removing the span never discards or
        // guesses how edited multiline content should be divided among tracks.
        self.merged_cells
            .retain(|span| !range.contains_range(*span));
        Ok(())
    }
    pub(crate) fn update_cell_formatting(
        &mut self,
        range: TableCellRange,
        patch: &TableCellFormattingPatch,
    ) -> Result<(), ModelError> {
        self.validate_whole_merges(range)?;
        if patch.style.is_default()
            && patch.horizontal_alignment.is_none()
            && patch.vertical_alignment.is_none()
            && patch.fill.is_none()
            && patch.borders.is_default()
        {
            return Err(ModelError::InvalidTableCellFormatting);
        }
        let mut formats = std::mem::take(&mut self.cell_formatting)
            .into_iter()
            .map(|formatting| ((formatting.row, formatting.column), formatting))
            .collect::<BTreeMap<_, _>>();
        for row in range.start_row..range.end_row {
            for column in range.start_column..range.end_column {
                let format = formats
                    .entry((row, column))
                    .or_insert_with(|| TableCellFormatting {
                        row,
                        column,
                        ..TableCellFormatting::default()
                    });
                format.horizontal_alignment =
                    patch.horizontal_alignment.or(format.horizontal_alignment);
                format.vertical_alignment = patch.vertical_alignment.or(format.vertical_alignment);
                format.fill = patch.fill.or(format.fill);
                format.borders = TableCellBorders {
                    top: patch.borders.top.or(format.borders.top),
                    right: patch.borders.right.or(format.borders.right),
                    bottom: patch.borders.bottom.or(format.borders.bottom),
                    left: patch.borders.left.or(format.borders.left),
                };
                let previous = &format.style;
                format.style = InlineTextStyle {
                    font_family: patch
                        .style
                        .font_family
                        .clone()
                        .or_else(|| previous.font_family.clone()),
                    font_weight: if patch.style.font_weight.is_some() {
                        patch.style.font_weight
                    } else if patch.style.bold.is_some() {
                        None
                    } else {
                        previous.font_weight
                    },
                    bold: if patch.style.font_weight.is_some() {
                        None
                    } else {
                        patch.style.bold.or(previous.bold)
                    },
                    italic: patch.style.italic.or(previous.italic),
                    font_size_millipoints: patch
                        .style
                        .font_size_millipoints
                        .or(previous.font_size_millipoints),
                    color: patch.style.color.or(previous.color),
                    underline: patch.style.underline.or(previous.underline),
                };
            }
        }
        self.cell_formatting = formats
            .into_values()
            .filter(|formatting| !formatting.is_default())
            .collect();
        Ok(())
    }
    pub(crate) fn reset_cell_formatting(
        &mut self,
        range: TableCellRange,
    ) -> Result<(), ModelError> {
        self.validate_whole_merges(range)?;
        self.cell_formatting
            .retain(|formatting| !range.contains(formatting.row, formatting.column));
        Ok(())
    }
    pub(crate) fn prepare_cell_paste(&mut self, range: TableCellRange) -> Result<(), ModelError> {
        range.validate(self)?;
        if range.end_row - range.start_row == 1 && range.end_column - range.start_column == 1 {
            range.validate(self)?;
            if self
                .cell_span(range.start_row, range.start_column)
                .is_none_or(|span| {
                    (span.start_row, span.start_column) == (range.start_row, range.start_column)
                })
            {
                return Ok(());
            }
        }
        self.split_cells(range)
    }
    pub(super) fn prepare_merged_track_edit(
        &mut self,
        axis: TableTrackAxis,
        index: u16,
        deleting: bool,
    ) -> Result<(), ModelError> {
        let total = match axis {
            TableTrackAxis::Row => self.rows,
            TableTrackAxis::Column => self.columns,
        };
        if index > total || (deleting && index == total) {
            return Err(ModelError::TableTrackOutOfRange);
        }
        if (deleting && total == 1) || (!deleting && total == 500) {
            return Err(ModelError::InvalidTableDimensions);
        }
        let mut retained = Vec::new();
        for mut span in std::mem::take(&mut self.merged_cells) {
            let (start, end) = match axis {
                TableTrackAxis::Row => (span.start_row, span.end_row),
                TableTrackAxis::Column => (span.start_column, span.end_column),
            };
            if index < end && (index > start || (deleting && index == start)) {
                // Unmerge the touched span. If deleting its anchor track leaves
                // surviving tracks, move its edited text to their first cell.
                if deleting && index == start && end - start > 1 {
                    let source = self.cell_index(span.start_row, span.start_column)?;
                    let destination = match axis {
                        TableTrackAxis::Row => {
                            self.cell_index(span.start_row + 1, span.start_column)?
                        }
                        TableTrackAxis::Column => {
                            self.cell_index(span.start_row, span.start_column + 1)?
                        }
                    };
                    self.cells[destination] = std::mem::take(&mut self.cells[source]);
                    let (row, column) = match axis {
                        TableTrackAxis::Row => (span.start_row + 1, span.start_column),
                        TableTrackAxis::Column => (span.start_row, span.start_column + 1),
                    };
                    let anchor_format =
                        self.cell_format(span.start_row, span.start_column).cloned();
                    self.cell_formatting
                        .retain(|value| (value.row, value.column) != (row, column));
                    if let Some(mut formatting) = anchor_format {
                        formatting.row = row;
                        formatting.column = column;
                        self.cell_formatting.push(formatting);
                        self.cell_formatting
                            .sort_by_key(|value| (value.row, value.column));
                    }
                }
                continue;
            }
            if start >= index {
                let shift = |value: u16| if deleting { value - 1 } else { value + 1 };
                match axis {
                    TableTrackAxis::Row => {
                        span.start_row = shift(span.start_row);
                        span.end_row = shift(span.end_row);
                    }
                    TableTrackAxis::Column => {
                        span.start_column = shift(span.start_column);
                        span.end_column = shift(span.end_column);
                    }
                }
            }
            retained.push(span);
        }
        self.merged_cells = retained;
        Ok(())
    }
}
