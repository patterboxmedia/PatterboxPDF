//! Immutable clipboard ownership belongs to the app session, beside its editor.
pub(super) mod resources;
use super::style_transfer::CapturedStyle;
use super::{
    CaptureClipboardInput, ClipboardSelection, EditorError, EditorErrorCode, EditorModule,
    EditorResult, EditorSnapshot, command_rejected, invalid_input, no_document,
    validate_coordinate,
};
use document_core::model::{ExactFontFallbackChain, MasterId, PageId, PublicationObject, Revision};
use document_core::{AssetStore, Command, ObjectLocation, ProjectSnapshot, Transaction};
use font_boundary::InspectedFontFace;
pub(super) use resources::{CapturedResources, ResourceCapturePlan, font_unavailable};
use serde::Deserialize;
use std::sync::Arc;

/// Read-only native work order. Receipt validation uses the private capture plan,
/// never values returned by the renderer or mutable copies of these fields.
#[derive(Clone, Debug)]
pub struct ClipboardFontWorkOrder {
    pub source: document_core::AssetHash,
    pub face_index: u32,
    pub bytes: Vec<u8>,
    pub required: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ClipboardDestinationInput {
    Page {
        #[serde(rename = "pageId")]
        page_id: String,
    },
    Pasteboard,
    Master {
        #[serde(rename = "masterId")]
        master_id: String,
    },
}
impl ClipboardDestinationInput {
    fn location(self) -> EditorResult<ObjectLocation> {
        Ok(match self {
            Self::Page { page_id } => {
                ObjectLocation::Page(PageId::parse(page_id).map_err(invalid_input)?)
            }
            Self::Pasteboard => ObjectLocation::Pasteboard,
            Self::Master { master_id } => {
                ObjectLocation::Master(MasterId::parse(master_id).map_err(invalid_input)?)
            }
        })
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PasteObjectClipboardInput {
    pub document_id: String,
    pub expected_revision: u64,
    pub destination: ClipboardDestinationInput,
    pub horizontal_offset_um: i64,
    pub vertical_offset_um: i64,
}

#[derive(Clone, Debug)]
pub(super) struct CaptureBinding {
    document_id: String,
    revision: Revision,
    token: Arc<()>,
}
impl CaptureBinding {
    pub(super) fn new(
        editor: &EditorModule,
        document_id: &str,
        revision: u64,
    ) -> EditorResult<Self> {
        check_target(editor, document_id, revision)?;
        Ok(Self {
            document_id: document_id.into(),
            revision: Revision::new(revision),
            token: Arc::clone(&editor.clipboard_binding),
        })
    }
    pub(super) fn validate(&self, editor: &EditorModule) -> EditorResult<()> {
        if !Arc::ptr_eq(&self.token, &editor.clipboard_binding) {
            return Err(stale_capture());
        }
        check_target(editor, &self.document_id, self.revision.value())
    }
}

#[derive(Clone, Debug)]
pub struct ObjectClipboardCapturePlan {
    binding: CaptureBinding,
    input: CaptureClipboardInput,
    selection: ClipboardSelection,
    resources: ResourceCapturePlan,
    fallbacks: Vec<ExactFontFallbackChain>,
}
impl ObjectClipboardCapturePlan {
    #[must_use]
    pub fn font_resources(&self) -> Vec<ClipboardFontWorkOrder> {
        self.resources.font_resources()
    }
}
#[derive(Clone, Debug)]
struct CapturedObjects {
    objects: Vec<PublicationObject>,
    resources: CapturedResources,
    fallbacks: Vec<ExactFontFallbackChain>,
}

/// Keep this value in the app's session service when replacing `EditorModule`.
/// Private captures cannot be deserialized or assembled from renderer data.
#[derive(Clone, Debug, Default)]
pub struct SessionClipboard {
    objects: Option<CapturedObjects>,
    pub(super) style: Option<CapturedStyle>,
}
impl SessionClipboard {
    #[must_use]
    pub fn has_objects(&self) -> bool {
        self.objects.is_some()
    }
    #[must_use]
    pub fn object_count(&self) -> usize {
        self.objects
            .as_ref()
            .map_or(0, |capture| capture.objects.len())
    }
    #[must_use]
    pub fn has_style(&self) -> bool {
        self.style.is_some()
    }

    /// Completes a read-only verified capture, or one atomic Cut, without replacing
    /// the previous clipboard when inspection, binding, locks or admission fail.
    /// # Errors
    /// Refuses stale capture bindings, invalid receipts and denied font embedding.
    pub fn capture_objects(
        &mut self,
        editor: &mut EditorModule,
        plan: ObjectClipboardCapturePlan,
        faces: Vec<InspectedFontFace>,
    ) -> EditorResult<ClipboardSelection> {
        plan.binding.validate(editor)?;
        let resources = plan.resources.verify(faces)?.0;
        let result = editor.capture_clipboard(plan.input)?;
        self.objects = Some(CapturedObjects {
            objects: plan.selection.objects,
            resources,
            fallbacks: plan.fallbacks,
        });
        Ok(result)
    }

    /// Pastes only the app-owned immutable capture. It carries exact resources,
    /// remaps object/group identity and deliberately targets Page/Pasteboard/Master.
    /// # Errors
    /// Refuses stale state, missing clipboard, conflicting fallback policy or
    /// aggregate resource/geometry admission. No resources enter on failure.
    pub fn paste_objects(
        &self,
        editor: &mut EditorModule,
        input: PasteObjectClipboardInput,
    ) -> EditorResult<EditorSnapshot> {
        check_target(editor, &input.document_id, input.expected_revision)?;
        let captured = self
            .objects
            .as_ref()
            .ok_or_else(|| invalid_input("Copy or cut objects before pasting."))?;
        validate_coordinate(input.horizontal_offset_um)?;
        validate_coordinate(input.vertical_offset_um)?;
        let mut commands = fallback_commands(editor, &captured.fallbacks)?;
        commands.push(Command::PasteObjects {
            objects: captured.objects.clone(),
            destination: input.destination.location()?,
            offset_x_um: input.horizontal_offset_um,
            offset_y_um: input.vertical_offset_um,
        });
        editor.execute_saveable(
            Revision::new(input.expected_revision),
            Transaction {
                label: "Paste objects".into(),
                commands,
            },
            captured.resources.descriptors(),
        )?;
        captured.resources.insert_into(&mut editor.assets);
        editor.snapshot()
    }
}

impl EditorModule {
    /// Captures canonical source and exact existing resources before native font
    /// inspection. No source mutation or clipboard replacement occurs here.
    /// # Errors
    /// Refuses stale input, partial groups, unavailable rendered fonts or resources.
    pub fn plan_object_clipboard_capture(
        &self,
        input: CaptureClipboardInput,
    ) -> EditorResult<ObjectClipboardCapturePlan> {
        let binding = CaptureBinding::new(self, &input.document_id, input.expected_revision)?;
        let selection = self.read_clipboard_selection(&input)?;
        let document = self.session.as_ref().ok_or_else(no_document)?.document();
        let fallbacks =
            resources::relevant_fallbacks(&selection.objects, &document.exact_font_fallbacks);
        let resources = ResourceCapturePlan::objects(&self.assets, &selection.objects, &fallbacks)?;
        Ok(ObjectClipboardCapturePlan {
            binding,
            input,
            selection,
            resources,
            fallbacks,
        })
    }
}

pub(super) fn check_target(
    editor: &EditorModule,
    document_id: &str,
    revision: u64,
) -> EditorResult<()> {
    let session = editor.session.as_ref().ok_or_else(no_document)?;
    if session.document().id.as_str() != document_id || session.revision().value() != revision {
        return Err(stale_capture());
    }
    Ok(())
}
fn stale_capture() -> EditorError {
    EditorError::new(
        EditorErrorCode::StaleRevision,
        "The publication changed. Copy or paste again from the current selection.",
    )
}
fn fallback_commands(
    editor: &EditorModule,
    captured: &[ExactFontFallbackChain],
) -> EditorResult<Vec<Command>> {
    let document = editor.session.as_ref().ok_or_else(no_document)?.document();
    let mut merged = document.exact_font_fallbacks.clone();
    for chain in captured {
        if let Some(existing) = merged
            .iter()
            .find(|existing| existing.script == chain.script)
        {
            if existing != chain {
                return Err(font_unavailable(
                    "This publication uses a different fallback font order. Choose matching fallback fonts before pasting.",
                ));
            }
        } else {
            merged.push(chain.clone());
        }
    }
    merged.sort_by_key(|chain| chain.script);
    Ok(if merged == document.exact_font_fallbacks {
        vec![]
    } else {
        vec![Command::SetExactFontFallbacks { fallbacks: merged }]
    })
}

pub(super) fn merged_resources(editor: &EditorModule, captured: &CapturedResources) -> AssetStore {
    let mut assets = editor.assets.clone();
    captured.insert_into(&mut assets);
    assets
}
pub(super) fn rehearse(
    editor: &EditorModule,
    revision: u64,
    transaction: &Transaction,
) -> EditorResult<ProjectSnapshot> {
    let session = editor.session.as_ref().ok_or_else(no_document)?;
    let mut rehearsal = document_core::DocumentSession::from_saved(
        session.document().clone(),
        Revision::new(revision),
    )
    .map_err(command_rejected)?;
    rehearsal
        .execute(Revision::new(revision), transaction.clone())
        .map_err(command_rejected)?;
    Ok(ProjectSnapshot {
        document: rehearsal.document().clone(),
        revision: rehearsal.revision(),
    })
}
