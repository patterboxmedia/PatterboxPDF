use super::{
    EditorError, EditorErrorCode, EditorModule, EditorResult, EditorSnapshot, invalid_input,
    no_document, parse_object_id,
};
use document_core::assets::referenced_object_asset_hashes;
use document_core::commands::{Command, Transaction};
use document_core::model::{MasterId, PageId, PublicationObject, Revision};
use document_core::package::asset_descriptors;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureClipboardInput {
    pub document_id: String,
    pub expected_revision: u64,
    pub object_ids: Vec<String>,
    pub cut: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardSelection {
    pub objects: Vec<PublicationObject>,
    pub source_page_id: Option<PageId>,
    pub source_master_id: Option<MasterId>,
    pub snapshot: Option<EditorSnapshot>,
}

impl EditorModule {
    /// Captures a complete selection, optionally removing it in one Undo step.
    /// The session retains its picture/font bytes until another capture or document
    /// replaces this clipboard, independently of old Undo steps being retired.
    /// Save and recovery still contain only resources used by their own documents.
    ///
    /// # Errors
    /// Rejects stale documents/revisions, incomplete groups, mixed locations and
    /// locked cuts. A rejected cut preserves the document and previous clipboard.
    #[allow(clippy::needless_pass_by_value)] // Owned public intent stays compatible with existing host callers.
    pub fn capture_clipboard(
        &mut self,
        input: CaptureClipboardInput,
    ) -> EditorResult<ClipboardSelection> {
        let mut selection = self.read_clipboard_selection(&input)?;
        let objects = &selection.objects;
        // Pin before executing: even a zero-step history budget may retire the
        // original resources immediately. Failed admission has not pruned assets.
        let previous = std::mem::replace(
            &mut self.clipboard_resources,
            referenced_object_asset_hashes(objects.iter()),
        );
        let snapshot = if input.cut {
            let transaction = Transaction {
                label: "Cut objects".to_owned(),
                commands: objects
                    .iter()
                    .map(|object| Command::DeleteObject {
                        object_id: object.id.clone(),
                    })
                    .collect(),
            };
            if let Err(error) = self.execute_saveable(
                Revision::new(input.expected_revision),
                transaction,
                std::iter::empty(),
            ) {
                self.clipboard_resources = previous;
                return Err(error);
            }
            Some(self.snapshot()?)
        } else {
            self.retain_history_resources(asset_descriptors(&self.assets));
            None
        };
        selection.snapshot = snapshot;
        Ok(selection)
    }

    /// Shared read-only canonical membership validation for session captures.
    #[allow(clippy::too_many_lines)] // Group/location/lock validation forms one selection admission.
    pub(super) fn read_clipboard_selection(
        &self,
        input: &CaptureClipboardInput,
    ) -> EditorResult<ClipboardSelection> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        let document = session.document();
        if document.id.as_str() != input.document_id
            || session.revision().value() != input.expected_revision
        {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The publication changed. Select the objects again before copying or cutting.",
            ));
        }
        let count = input.object_ids.len();
        if count == 0 || count > 500 {
            return Err(invalid_input(
                "Select between one and 500 objects to copy or cut.",
            ));
        }
        let ids = input
            .object_ids
            .iter()
            .cloned()
            .map(parse_object_id)
            .collect::<EditorResult<BTreeSet<_>>>()?;
        if ids.len() != count {
            return Err(invalid_input("Select each object only once."));
        }
        let mut containers = document
            .pages
            .iter()
            .map(|page| (Some(page.id.clone()), None, &page.objects))
            .chain(std::iter::once((None, None, &document.pasteboard.objects)))
            .chain(
                document
                    .masters
                    .iter()
                    .map(|master| (None, Some(master.id.clone()), &master.objects)),
            )
            .filter(|(_, _, objects)| objects.iter().any(|object| ids.contains(&object.id)));
        let (source_page_id, source_master_id, container) = containers
            .next()
            .ok_or_else(|| invalid_input("The selected objects no longer exist."))?;
        if containers.next().is_some() {
            return Err(invalid_input(
                "Copy or cut objects from one page or the pasteboard at a time.",
            ));
        }
        let objects: Vec<_> = container
            .iter()
            .filter(|object| ids.contains(&object.id))
            .cloned()
            .collect();
        if objects.len() != ids.len() {
            return Err(invalid_input("The selected objects no longer exist."));
        }
        let groups: BTreeSet<_> = objects
            .iter()
            .filter_map(|object| object.group_id.as_ref())
            .collect();
        if container.iter().any(|object| {
            object
                .group_id
                .as_ref()
                .is_some_and(|group| groups.contains(group))
                && !ids.contains(&object.id)
        }) {
            return Err(invalid_input(
                "Select the whole group before copying or cutting it.",
            ));
        }
        if input.cut && objects.iter().any(|object| object.locked) {
            return Err(invalid_input(
                "Unlock the selected objects before cutting them.",
            ));
        }
        Ok(ClipboardSelection {
            objects,
            source_page_id,
            source_master_id,
            snapshot: None,
        })
    }
}
