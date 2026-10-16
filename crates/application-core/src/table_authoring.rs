use super::{
    ColorInput, EditorResult, InlineTextStyleInput, TableGridBorderInput,
    TableVerticalAlignmentInput, TextAlignmentInput, canonical_table_vertical_alignment,
    canonical_text_alignment, inline_text_style,
};
use document_core::model::{
    InlineTextStyle, TableCellBorders, TableCellFormattingPatch, TableCellRange, TableGridBorder,
};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TableCellRangeInput {
    pub start_row: u16,
    pub end_row: u16,
    pub start_column: u16,
    pub end_column: u16,
}
impl From<TableCellRangeInput> for TableCellRange {
    fn from(value: TableCellRangeInput) -> Self {
        Self {
            start_row: value.start_row,
            end_row: value.end_row,
            start_column: value.start_column,
            end_column: value.end_column,
        }
    }
}
impl From<TableCellRange> for TableCellRangeInput {
    fn from(value: TableCellRange) -> Self {
        Self {
            start_row: value.start_row,
            end_row: value.end_row,
            start_column: value.start_column,
            end_column: value.end_column,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TableCellBordersInput {
    #[serde(default)]
    pub top: Option<TableGridBorderInput>,
    #[serde(default)]
    pub right: Option<TableGridBorderInput>,
    #[serde(default)]
    pub bottom: Option<TableGridBorderInput>,
    #[serde(default)]
    pub left: Option<TableGridBorderInput>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TableCellFormattingInput {
    #[serde(default)]
    pub horizontal_alignment: Option<TextAlignmentInput>,
    #[serde(default)]
    pub vertical_alignment: Option<TableVerticalAlignmentInput>,
    #[serde(default)]
    pub style: InlineTextStyleInput,
    #[serde(default)]
    pub fill: Option<ColorInput>,
    #[serde(default)]
    pub borders: TableCellBordersInput,
}

pub(super) fn canonical_cell_formatting(
    value: TableCellFormattingInput,
) -> EditorResult<TableCellFormattingPatch> {
    let border = |value: TableGridBorderInput| TableGridBorder {
        color: value.color.into(),
        width_millipoints: value.width_millipoints,
    };
    Ok(TableCellFormattingPatch {
        horizontal_alignment: value.horizontal_alignment.map(canonical_text_alignment),
        vertical_alignment: value
            .vertical_alignment
            .map(canonical_table_vertical_alignment),
        style: if value.style == InlineTextStyleInput::default() {
            InlineTextStyle::default()
        } else {
            inline_text_style(value.style)?
        },
        fill: value.fill.map(Into::into),
        borders: TableCellBorders {
            top: value.borders.top.map(border),
            right: value.borders.right.map(border),
            bottom: value.borders.bottom.map(border),
            left: value.borders.left.map(border),
        },
    })
}
