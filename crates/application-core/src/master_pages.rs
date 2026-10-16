//! Explicit shared-object editing and semantic page-number commands.

use super::{
    ApplyTransactionInput, EditorCommandInput, EditorModule, EditorResult, EditorSnapshot,
    command_rejected, embedded_font_reuse, invalid_input, parse_object_id, parse_page_id,
    physical_size,
};
use document_core::commands::{Command, ObjectLocation};
use document_core::model::{
    MasterId, PageNumberFormat, PageNumberValue, PageNumberingSection, TextField,
};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MasterTransactionInput {
    pub master_id: String,
    pub transaction: ApplyTransactionInput,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum PageNumberFormatInput {
    Decimal,
    RomanLower,
    RomanUpper,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NumberingSectionInput {
    pub format: PageNumberFormatInput,
    pub start: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum TextFieldInput {
    PageNumber,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MasterRemovalInput {
    None,
    Master { master_id: String },
}

pub(super) fn canonical_master_command(input: EditorCommandInput) -> EditorResult<Command> {
    Ok(match input {
        EditorCommandInput::AddMaster {
            name,
            trim_width_um,
            trim_height_um,
        } => Command::AddMaster {
            name,
            trim: physical_size(trim_width_um, trim_height_um)?,
        },
        EditorCommandInput::RenameMaster { master_id, name } => Command::RenameMaster {
            master_id: parse_master(master_id)?,
            name,
        },
        EditorCommandInput::DuplicateMaster { master_id, name } => Command::DuplicateMaster {
            master_id: parse_master(master_id)?,
            name,
        },
        EditorCommandInput::ResizeMaster {
            master_id,
            trim_width_um,
            trim_height_um,
        } => Command::ResizeMaster {
            master_id: parse_master(master_id)?,
            trim: physical_size(trim_width_um, trim_height_um)?,
        },
        EditorCommandInput::AssignMaster {
            page_ids,
            master_id,
        } => Command::AssignMaster {
            page_ids: page_ids
                .into_iter()
                .map(parse_page_id)
                .collect::<EditorResult<_>>()?,
            master_id: master_id.map(parse_master).transpose()?,
        },
        EditorCommandInput::RemoveMaster {
            master_id,
            replacement,
        } => Command::RemoveMaster {
            master_id: parse_master(master_id)?,
            replacement: match replacement {
                MasterRemovalInput::None => None,
                MasterRemovalInput::Master { master_id } => Some(parse_master(master_id)?),
            },
        },
        EditorCommandInput::UpdatePageNumbering { page_id, section } => {
            Command::UpdatePageNumbering {
                page_id: parse_page_id(page_id)?,
                section: section.map(|section| PageNumberingSection {
                    format: match section.format {
                        PageNumberFormatInput::Decimal => PageNumberFormat::Decimal,
                        PageNumberFormatInput::RomanLower => PageNumberFormat::RomanLower,
                        PageNumberFormatInput::RomanUpper => PageNumberFormat::RomanUpper,
                    },
                    start: section.start,
                }),
            }
        }
        EditorCommandInput::SetPageNumberSuppression {
            page_ids,
            suppressed,
        } => Command::SetPageNumberSuppression {
            page_ids: page_ids
                .into_iter()
                .map(parse_page_id)
                .collect::<EditorResult<_>>()?,
            suppressed,
        },
        EditorCommandInput::SetTextField { object_id, field } => Command::SetTextField {
            object_id: parse_object_id(object_id)?,
            field: field.map(|TextFieldInput::PageNumber| TextField::PageNumber),
        },
        _ => {
            return Err(invalid_input(
                "Choose a supported Master or page-number command",
            ));
        }
    })
}

pub(super) fn parse_master(value: String) -> EditorResult<MasterId> {
    MasterId::parse(value).map_err(invalid_input)
}

pub(super) fn redirect_master_commands(commands: &mut [Command], master_id: &MasterId) {
    for command in commands {
        match command {
            Command::AddObject { location, .. } => {
                *location = ObjectLocation::Master(master_id.clone());
            }
            Command::PasteObjects { destination, .. } | Command::MoveObject { destination, .. } => {
                *destination = ObjectLocation::Master(master_id.clone());
            }
            Command::MoveSelection {
                destination: Some(destination),
                ..
            } => *destination = ObjectLocation::Master(master_id.clone()),
            _ => {}
        }
    }
}

impl EditorModule {
    /// Edits shared objects in the explicit master workspace as one Undo step.
    ///
    /// # Errors
    /// Refuses stale context or transactions that would modify another workspace.
    pub fn apply_master(&mut self, input: MasterTransactionInput) -> EditorResult<EditorSnapshot> {
        let master_id = parse_master(input.master_id)?;
        let (revision, mut transaction) =
            self.canonical_creation_transaction_in_master(input.transaction, Some(&master_id))?;
        if let Some(session) = &self.session {
            embedded_font_reuse::bind_new_text(session.document(), &self.assets, &mut transaction);
        }
        self.prepare_arrange_geometry(revision, &mut transaction)?;
        self.execute_saveable(revision, transaction, std::iter::empty())?;
        self.snapshot()
    }

    /// Returns the derived number sequence for a revision-bound Pages preview.
    ///
    /// # Errors
    /// Refuses stale revisions or invalid section settings.
    pub fn page_numbers(&self, expected_revision: u64) -> EditorResult<Vec<PageNumberValue>> {
        self.selection_project(expected_revision)?
            .document
            .page_numbers()
            .map_err(command_rejected)
    }
}
