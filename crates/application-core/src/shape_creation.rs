//! Initial appearance resolves the canonical created ID before one real transaction.

use super::{
    ApplyTransactionInput, EditorCommandInput, EditorError, EditorErrorCode, EditorModule,
    EditorResult, MAX_CANONICAL_COMMANDS, MAX_COMMANDS, MAX_LABEL_BYTES, canonical_commands,
    canonical_transaction, command_rejected, require_text,
};
use document_core::commands::{Command, DocumentSession, Transaction};
use document_core::model::{CornerRounding, MasterId, Revision, TextField};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CornerRoundingInput {
    pub top_left_um: i64,
    pub top_right_um: i64,
    pub bottom_right_um: i64,
    pub bottom_left_um: i64,
    pub linked: bool,
}

impl From<CornerRoundingInput> for CornerRounding {
    fn from(value: CornerRoundingInput) -> Self {
        Self {
            top_left_um: value.top_left_um,
            top_right_um: value.top_right_um,
            bottom_right_um: value.bottom_right_um,
            bottom_left_um: value.bottom_left_um,
            linked: value.linked,
        }
    }
}

impl EditorModule {
    pub(super) fn canonical_creation_transaction(
        &self,
        input: ApplyTransactionInput,
    ) -> EditorResult<(Revision, Transaction)> {
        self.canonical_creation_transaction_in_master(input, None)
    }

    #[allow(clippy::too_many_lines)] // One receipt preparation admits creation and context together.
    pub(super) fn canonical_creation_transaction_in_master(
        &self,
        input: ApplyTransactionInput,
        master_id: Option<&MasterId>,
    ) -> EditorResult<(Revision, Transaction)> {
        if master_id.is_none()
            && !input.commands.iter().any(|command| {
                matches!(
                    command,
                    EditorCommandInput::AddShape {
                        corner_rounding: Some(_),
                        ..
                    } | EditorCommandInput::AddPageNumber { .. }
                )
            })
        {
            return canonical_transaction(input);
        }
        require_text(&input.label, MAX_LABEL_BYTES, "transaction label")?;
        if input.commands.is_empty() || input.commands.len() > MAX_COMMANDS {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "A transaction must contain between 1 and 100 commands",
            ));
        }
        let project = self.selection_project(input.expected_revision)?;
        if master_id.is_some_and(|id| {
            !project
                .document
                .masters
                .iter()
                .any(|master| &master.id == id)
        }) {
            return Err(super::invalid_input("That Master no longer exists"));
        }
        let expected = project.revision;
        let original = project.document.clone();
        let mut scratch =
            DocumentSession::from_saved(project.document, expected).map_err(command_rejected)?;
        let mut prepared = Vec::new();
        for command in input.commands {
            let initial = match &command {
                EditorCommandInput::AddShape {
                    corner_rounding, ..
                } => *corner_rounding,
                _ => None,
            };
            let is_page_number = matches!(command, EditorCommandInput::AddPageNumber { .. });
            let mut commands = canonical_commands(command)?;
            if let Some(master_id) = master_id {
                super::master_pages::redirect_master_commands(&mut commands, master_id);
            }
            let receipt = scratch
                .execute(
                    scratch.revision(),
                    Transaction {
                        label: "Prepare initial appearance".into(),
                        commands: commands.clone(),
                    },
                )
                .map_err(command_rejected)?;
            if initial.is_some() || is_page_number {
                let object_id = receipt.created_objects.first().cloned().ok_or_else(|| {
                    EditorError::new(
                        EditorErrorCode::InvalidInput,
                        "The object could not be created",
                    )
                })?;
                let setter = if let Some(initial) = initial {
                    Command::UpdateCornerRounding {
                        object_id,
                        rounding: initial.into(),
                    }
                } else {
                    Command::SetTextField {
                        object_id,
                        field: Some(TextField::PageNumber),
                    }
                };
                scratch
                    .execute(
                        scratch.revision(),
                        Transaction::single("Prepare initial corners", setter.clone()),
                    )
                    .map_err(command_rejected)?;
                commands.push(setter);
            }
            prepared.extend(commands);
            if prepared.len() > MAX_CANONICAL_COMMANDS {
                return Err(EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "A transaction expands beyond the 1000-command safety limit",
                ));
            }
        }
        if let Some(master_id) = master_id {
            let after = scratch.document();
            let outside_unchanged = original.pages == after.pages
                && original.pasteboard == after.pasteboard
                && original.color_settings == after.color_settings
                && original.exact_font_fallbacks == after.exact_font_fallbacks
                && original.title == after.title
                && original.masters.len() == after.masters.len()
                && original
                    .masters
                    .iter()
                    .zip(&after.masters)
                    .all(|(before, after)| {
                        if &before.id == master_id {
                            before.id == after.id
                                && before.name == after.name
                                && before.trim == after.trim
                        } else {
                            before == after
                        }
                    });
            if !outside_unchanged {
                return Err(super::invalid_input(
                    "Edit only this Master's objects in the Master workspace",
                ));
            }
        }
        Ok((
            expected,
            Transaction {
                label: input.label,
                commands: prepared,
            },
        ))
    }
}
