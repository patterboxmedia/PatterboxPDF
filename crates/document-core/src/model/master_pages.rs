//! Shared page definitions and numbering retain authored source identities.

use super::{
    Document, EdgeExpectations, Insets, MasterId, ModelError, ObjectKind, Page, PageId,
    PhysicalSize, PublicationObject, TextStoryFormatting,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MasterPage {
    pub id: MasterId,
    pub name: String,
    pub trim: PhysicalSize,
    pub objects: Vec<PublicationObject>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PageNumberFormat {
    #[default]
    Decimal,
    RomanLower,
    RomanUpper,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PageNumberingSection {
    pub format: PageNumberFormat,
    /// Absence continues the preceding sequence; a value starts a new sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
}

impl PageNumberingSection {
    pub const MAX_START: u32 = 1_000_000;

    pub(super) fn validate(self) -> Result<(), ModelError> {
        if self
            .start
            .is_some_and(|value| !(1..=Self::MAX_START).contains(&value))
        {
            return Err(ModelError::PageNumberStartOutOfRange);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TextField {
    PageNumber,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PageNumberValue {
    pub page_id: PageId,
    pub value: u32,
    pub format: PageNumberFormat,
    pub display: String,
    pub suppressed: bool,
    pub section_start: bool,
}

impl Document {
    pub const MAX_MASTERS: usize = Self::MAX_INITIAL_PAGES as usize;

    /// Every authored object exactly once, including unassigned masters.
    pub fn authored_objects(&self) -> impl Iterator<Item = &PublicationObject> {
        self.pages
            .iter()
            .flat_map(|page| &page.objects)
            .chain(&self.pasteboard.objects)
            .chain(self.masters.iter().flat_map(|master| &master.objects))
    }

    pub fn authored_objects_mut(&mut self) -> impl Iterator<Item = &mut PublicationObject> {
        self.pages
            .iter_mut()
            .flat_map(|page| &mut page.objects)
            .chain(&mut self.pasteboard.objects)
            .chain(
                self.masters
                    .iter_mut()
                    .flat_map(|master| &mut master.objects),
            )
    }

    /// Resolves the sequence after page insertion, removal or reordering.
    ///
    /// # Errors
    /// Returns a numbering error for invalid section starts or arithmetic overflow.
    pub fn page_numbers(&self) -> Result<Vec<PageNumberValue>, ModelError> {
        let mut value = 0_u32;
        let mut format = PageNumberFormat::Decimal;
        self.pages
            .iter()
            .map(|page| {
                value = value.checked_add(1).ok_or(ModelError::PageNumberOverflow)?;
                if let Some(section) = page.numbering_section {
                    section.validate()?;
                    format = section.format;
                    if let Some(start) = section.start {
                        value = start;
                    }
                }
                Ok(PageNumberValue {
                    page_id: page.id.clone(),
                    value,
                    format,
                    display: format_number(value, format),
                    suppressed: page.suppress_page_number,
                    section_start: page.numbering_section.is_some(),
                })
            })
            .collect()
    }

    /// Returns a disposable page instance for the shared layout authority.
    /// Definitions remain separate in the durable document.
    ///
    /// # Errors
    /// Returns an error for a missing page/master or mismatched physical size.
    pub fn composed_page(&self, page_id: &PageId) -> Result<Page, ModelError> {
        let source = self
            .pages
            .iter()
            .find(|page| &page.id == page_id)
            .ok_or_else(|| ModelError::PageReferenceNotFound(page_id.clone()))?;
        let number = self
            .page_numbers()?
            .into_iter()
            .find(|number| &number.page_id == page_id)
            .ok_or_else(|| ModelError::PageReferenceNotFound(page_id.clone()))?;
        let mut page = source.clone();
        let mut objects = if let Some(master_id) = &source.master_id {
            let master = self
                .masters
                .iter()
                .find(|master| &master.id == master_id)
                .ok_or_else(|| ModelError::MasterNotFound(master_id.clone()))?;
            if master.trim != source.trim {
                return Err(ModelError::MasterPageSizeMismatch);
            }
            master.objects.clone()
        } else {
            Vec::new()
        };
        objects.extend(source.objects.iter().cloned());
        page.objects = resolve_fields(objects, &number.display, number.suppressed);
        page.master_id = None;
        Ok(page)
    }

    /// Creates the explicit master workspace, with number fields shown as 1.
    ///
    /// # Errors
    /// Returns an error for a missing master or invalid stable identity.
    pub fn master_preview_page(&self, master_id: &MasterId) -> Result<Page, ModelError> {
        let master = self
            .masters
            .iter()
            .find(|master| &master.id == master_id)
            .ok_or_else(|| ModelError::MasterNotFound(master_id.clone()))?;
        Ok(Page {
            id: PageId::parse(master.id.as_str())?,
            name: master.name.clone(),
            trim: master.trim,
            bleed: Insets::default(),
            bleed_expectations: EdgeExpectations::default(),
            guides: Vec::new(),
            objects: resolve_fields(master.objects.clone(), "1", false),
            master_id: None,
            numbering_section: None,
            suppress_page_number: false,
        })
    }

    /// Parked number fields show a placeholder until moved onto a Page.
    #[must_use]
    pub fn preview_pasteboard_objects(&self) -> std::borrow::Cow<'_, [PublicationObject]> {
        if self
            .pasteboard
            .objects
            .iter()
            .any(|object| object.text_field.is_some())
        {
            std::borrow::Cow::Owned(resolve_fields(self.pasteboard.objects.clone(), "1", false))
        } else {
            std::borrow::Cow::Borrowed(&self.pasteboard.objects)
        }
    }
}

fn resolve_fields(
    objects: Vec<PublicationObject>,
    display: &str,
    suppressed: bool,
) -> Vec<PublicationObject> {
    objects
        .into_iter()
        .filter_map(|mut object| {
            if object.text_field.is_some() {
                if suppressed {
                    return None;
                }
                if let ObjectKind::Text { text, story, .. } = &mut object.kind {
                    *text = display.into();
                    *story = TextStoryFormatting::default();
                }
                object.text_field = None;
            }
            Some(object)
        })
        .collect()
}

fn format_number(value: u32, format: PageNumberFormat) -> String {
    if format == PageNumberFormat::Decimal {
        return value.to_string();
    }
    let mut remainder = value;
    let mut result = String::new();
    for (amount, letters) in [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ] {
        while remainder >= amount {
            result.push_str(letters);
            remainder -= amount;
        }
    }
    if format == PageNumberFormat::RomanLower {
        result.make_ascii_lowercase();
    }
    result
}
