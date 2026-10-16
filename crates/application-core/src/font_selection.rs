use super::{
    ApplyTransactionInput, AssetHash, Command, EditorCommandInput, EditorError, EditorErrorCode,
    EditorModule, EditorResult, EditorSnapshot, FontFaceBinding, FontFaceBindingUpdate,
    MAX_CANONICAL_COMMANDS, MAX_FONT_FAMILY_BYTES, MAX_FONT_FAMILY_CHARS, ObjectId, ObjectKind,
    Revision, TextSourceRange, Transaction, command_rejected, invalid_input,
    require_text,
};
use document_core::commands::DocumentSession;
use document_core::model::effective_font_variant_ranges;
use document_core::package::AssetDescriptor;
use font_boundary::{
    FontContainerKind, FontEmbeddingLicense, FontEmbeddingRights, FontFaceStyle, InspectedFontFace,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One exact installed-font choice that has already crossed the confined font worker.
pub struct VerifiedFontSelectionInput {
    pub expected_revision: u64,
    pub object_id: String,
    pub font_family: String,
    pub font_weight: u16,
    pub italic: bool,
    pub inspected_face: InspectedFontFace,
}

/// One revision-bound family, face, Bold, or Italic choice for a Text Box source range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineFontSelectionIntent {
    pub expected_revision: u64,
    pub object_id: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub font_family: Option<String>,
    pub font_weight: Option<u16>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
}

/// One effective family, weight, and slant that needs an exact installed face.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RequiredFontVariant {
    pub font_family: String,
    pub font_weight: u16,
    pub italic: bool,
}

/// Canonical, read-only work order for native installed-font inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineFontSelectionPlan {
    intent: InlineFontSelectionIntent,
    font_family: String,
    base_variant: RequiredFontVariant,
    required_variants: Vec<RequiredFontVariant>,
}

impl InlineFontSelectionPlan {
    #[must_use]
    pub fn font_family(&self) -> &str {
        &self.font_family
    }

    #[must_use]
    pub fn required_variants(&self) -> &[RequiredFontVariant] {
        &self.required_variants
    }
}

/// One plan-requested face that has crossed the confined font worker.
pub struct VerifiedInlineFontVariant {
    pub requested: RequiredFontVariant,
    pub inspected_face: InspectedFontFace,
}

/// Complete native evidence for one revision-bound inline-formatting plan.
pub struct VerifiedInlineFontSelectionInput {
    pub plan: InlineFontSelectionPlan,
    pub variants: Vec<VerifiedInlineFontVariant>,
}

/// Canonical work order for one transaction containing collapsed-caret styling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StyledTextTransactionPlan {
    input: ApplyTransactionInput,
    object_id: String,
    font_family: String,
    base_variant: RequiredFontVariant,
    required_variants: Vec<RequiredFontVariant>,
    final_style: document_core::model::TextStyle,
}

impl StyledTextTransactionPlan {
    #[must_use]
    pub fn font_family(&self) -> &str {
        &self.font_family
    }

    #[must_use]
    pub fn required_variants(&self) -> &[RequiredFontVariant] {
        &self.required_variants
    }
}

/// Complete native evidence for one atomic transaction containing styled typing.
pub struct VerifiedStyledTextTransactionInput {
    pub plan: StyledTextTransactionPlan,
    pub variants: Vec<VerifiedInlineFontVariant>,
}

/// Rehearsed Table appearance transaction and its exact face work order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableCellFontTransactionPlan {
    input: ApplyTransactionInput,
    object_id: String,
    font_family: String,
    base_variant: RequiredFontVariant,
    required_variants: Vec<RequiredFontVariant>,
    final_style: document_core::model::TextStyle,
}

impl TableCellFontTransactionPlan {
    #[must_use]
    pub fn font_family(&self) -> &str {
        &self.font_family
    }
    #[must_use]
    pub fn required_variants(&self) -> &[RequiredFontVariant] {
        &self.required_variants
    }
}

/// Confined font evidence for one atomic Table cell appearance transaction.
pub struct VerifiedTableCellFontTransactionInput {
    pub plan: TableCellFontTransactionPlan,
    pub variants: Vec<VerifiedInlineFontVariant>,
}

/// Whether the selected face travels with the publication or remains local to Windows.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FontSelectionDisposition {
    Embedded,
    WindowsOnly,
}

/// Canonical result of one revision-checked installed-font selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedFontSelectionOutcome {
    pub snapshot: EditorSnapshot,
    pub disposition: FontSelectionDisposition,
    pub message: String,
}

struct InspectedFaceEvidence {
    source_bytes: Vec<u8>,
    source_sha256: [u8; 32],
    face_index: u32,
    container_kind: FontContainerKind,
    face_count: u32,
    family_names: Vec<String>,
    weight: u16,
    style: FontFaceStyle,
    italic: bool,
    embedding_rights: FontEmbeddingRights,
}

struct PendingFontAsset {
    media_type: &'static str,
    bytes: Vec<u8>,
    hash: AssetHash,
}

impl From<InspectedFontFace> for InspectedFaceEvidence {
    fn from(face: InspectedFontFace) -> Self {
        let evidence = Self {
            source_sha256: face.source_sha256(),
            face_index: face.face_index(),
            container_kind: face.container_kind(),
            face_count: face.face_count(),
            family_names: face.family_names().to_vec(),
            weight: face.weight(),
            style: face.style(),
            italic: face.italic(),
            embedding_rights: face.embedding_rights(),
            source_bytes: Vec::new(),
        };
        Self {
            source_bytes: face.into_source_bytes(),
            ..evidence
        }
    }
}

impl EditorModule {
    /// Applies an installed font to one Text Box or Table without trusting the `WebView`.
    ///
    /// The selected source is embedded only when the worker proves that it is a
    /// single-font container licensed for installable or editable embedding.
    /// Preview-and-print, restricted, bitmap-only, unknown, and collection
    /// sources remain Windows-only. The exact asset is inserted only after the
    /// canonical style transaction succeeds.
    ///
    /// # Errors
    ///
    /// Returns a structured error for stale state, an unavailable or locked
    /// object, a non-text object, receipt/request drift, or invalid font bytes.
    pub fn apply_verified_font_selection(
        &mut self,
        input: VerifiedFontSelectionInput,
    ) -> EditorResult<VerifiedFontSelectionOutcome> {
        self.apply_inspected_font_selection(
            input.expected_revision,
            input.object_id,
            input.font_family,
            input.font_weight,
            input.italic,
            input.inspected_face.into(),
        )
    }

    /// Plans the exact installed faces needed by one inline font choice.
    ///
    /// The canonical command is rehearsed against a cloned session so range,
    /// UTF-8, lock, and object-kind validation cannot drift from the eventual
    /// mutation. Existing exact bindings are deliberately omitted.
    ///
    /// # Errors
    ///
    /// Returns a structured error for stale state, an invalid or locked Text
    /// Box, a non-UTF-8 range boundary, or an empty formatting patch.
    pub fn plan_inline_font_selection(
        &self,
        intent: InlineFontSelectionIntent,
    ) -> EditorResult<InlineFontSelectionPlan> {
        let snapshot = self.snapshot()?;
        if snapshot.project.revision.value() != intent.expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The publication changed before selected text could be formatted",
            ));
        }
        let object_id = ObjectId::parse(intent.object_id.clone()).map_err(invalid_input)?;
        let range = TextSourceRange {
            start: intent.start_byte,
            end: intent.end_byte,
        };
        let mut rehearsal = DocumentSession::from_saved(
            snapshot.project.document.clone(),
            snapshot.project.revision,
        )
        .map_err(command_rejected)?;
        rehearsal
            .execute(
                snapshot.project.revision,
                Transaction::single(
                    "Plan selected text formatting",
                    Command::UpdateInlineTextAppearance {
                        object_id: object_id.clone(),
                        range,
                        style: inline_intent_style(&intent),
                    },
                ),
            )
            .map_err(command_rejected)?;
        let object = find_object(rehearsal.document(), &object_id).ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::InvalidInput,
                "That Text Box is no longer available",
            )
        })?;
        let ObjectKind::Text { style, story, .. } = &object.kind else {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "Only a Text Box selection can use inline formatting",
            ));
        };
        let base_variant = RequiredFontVariant {
            font_family: style.font_family.clone(),
            font_weight: style.font_weight,
            italic: style.italic,
        };
        let mut required_variants = effective_variants_in_range(style, story, range)
            .into_iter()
            .filter(|variant| {
                style
                    .exact_font_face_for_family(
                        &variant.font_family,
                        variant.font_weight,
                        variant.italic,
                    )
                    .is_none()
            })
            .collect::<BTreeSet<_>>();
        // A Publisher user can format selected text before explicitly choosing
        // a font. Capture the Text Box's base face in that same verified,
        // atomic operation so the unselected text does not remain silently
        // unbound while the selected range reports an embedded face.
        if style.font_face.is_none() {
            required_variants.insert(base_variant.clone());
        }
        Ok(InlineFontSelectionPlan {
            intent,
            font_family: style.font_family.clone(),
            base_variant,
            required_variants: required_variants.into_iter().collect(),
        })
    }

    /// Atomically binds verified faces and applies one inline font choice.
    ///
    /// # Errors
    ///
    /// Returns a structured error when the plan is stale, its evidence set is
    /// incomplete or inconsistent, a face receipt is invalid, or the complete
    /// canonical transaction is rejected. No asset is inserted on failure.
    pub fn apply_verified_inline_font_selection(
        &mut self,
        input: VerifiedInlineFontSelectionInput,
    ) -> EditorResult<VerifiedFontSelectionOutcome> {
        let evidence = input
            .variants
            .into_iter()
            .map(|variant| (variant.requested, variant.inspected_face.into()))
            .collect();
        self.apply_inspected_inline_font_selection(&input.plan, evidence)
    }

    /// Rehearses one Table cell formatting transaction before face inspection.
    /// Empty styled cells are included so later typing uses the chosen exact face.
    ///
    /// # Errors
    /// Returns an error for stale revisions, invalid ranges, locked objects,
    /// or transactions that do not format exactly one Table's font appearance.
    pub fn plan_table_cell_font_transaction(
        &self,
        input: ApplyTransactionInput,
    ) -> EditorResult<TableCellFontTransactionPlan> {
        let snapshot = self.snapshot()?;
        if snapshot.project.revision.value() != input.expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The publication changed before the Table could be formatted",
            ));
        }
        let ids = input
            .commands
            .iter()
            .filter_map(|command| match command {
                EditorCommandInput::UpdateTableCellFormatting {
                    object_id,
                    formatting,
                    ..
                } if formatting.style.font_family.is_some()
                    || formatting.style.font_weight.is_some()
                    || formatting.style.bold.is_some()
                    || formatting.style.italic.is_some() =>
                {
                    Some(object_id.as_str())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        if ids.len() != 1 {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "A cell font transaction must format exactly one Table",
            ));
        }
        let object_id = ids
            .first()
            .ok_or_else(|| {
                EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "Choose Table cells before formatting their font",
                )
            })?
            .to_string();
        let (revision, transaction) = self.canonical_creation_transaction(input.clone())?;
        let mut rehearsal = DocumentSession::from_saved(
            snapshot.project.document.clone(),
            snapshot.project.revision,
        )
        .map_err(command_rejected)?;
        rehearsal
            .execute(revision, transaction)
            .map_err(command_rejected)?;
        let parsed_id = ObjectId::parse(object_id.clone()).map_err(invalid_input)?;
        let object = find_object(rehearsal.document(), &parsed_id).ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::InvalidInput,
                "That Table is no longer available",
            )
        })?;
        let ObjectKind::Table { table } = &object.kind else {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "Cell formatting requires a Table",
            ));
        };
        let style = &table.style;
        let base_variant = RequiredFontVariant {
            font_family: style.font_family.clone(),
            font_weight: style.font_weight,
            italic: style.italic,
        };
        let mut effective = BTreeSet::from([base_variant.clone()]);
        for cell in &table.cell_formatting {
            let effective_style = table.effective_cell_style(cell.row, cell.column);
            effective.insert(RequiredFontVariant {
                font_family: effective_style.font_family,
                font_weight: effective_style.font_weight,
                italic: effective_style.italic,
            });
        }
        let required_variants = effective
            .into_iter()
            .filter(|variant| {
                style
                    .exact_font_face_for_family(
                        &variant.font_family,
                        variant.font_weight,
                        variant.italic,
                    )
                    .is_none()
            })
            .collect();
        Ok(TableCellFontTransactionPlan {
            input,
            object_id,
            font_family: style.font_family.clone(),
            base_variant,
            required_variants,
            final_style: style.clone(),
        })
    }

    /// Commits verified cell fonts and all accompanying Table edits in one Undo.
    ///
    /// # Errors
    /// Returns an error for stale plans, incomplete receipts or aggregate
    /// admission failure. Rejected transactions insert no font assets.
    pub fn apply_verified_table_cell_font_transaction(
        &mut self,
        input: VerifiedTableCellFontTransactionInput,
    ) -> EditorResult<VerifiedFontSelectionOutcome> {
        let evidence = input
            .variants
            .into_iter()
            .map(|variant| (variant.requested, variant.inspected_face.into()))
            .collect();
        self.apply_inspected_table_cell_font_transaction(&input.plan, evidence)
    }

    fn apply_inspected_table_cell_font_transaction(
        &mut self,
        plan: &TableCellFontTransactionPlan,
        evidence: Vec<(RequiredFontVariant, InspectedFaceEvidence)>,
    ) -> EditorResult<VerifiedFontSelectionOutcome> {
        if &self.plan_table_cell_font_transaction(plan.input.clone())? != plan {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The Table changed before its font faces could be applied",
            ));
        }
        let id = ObjectId::parse(plan.object_id.clone()).map_err(invalid_input)?;
        let (bindings, pending_assets, all_embedded) = prepare_variant_binding_commands(
            &plan.required_variants,
            &plan.base_variant,
            &plan.final_style,
            &id,
            evidence,
        )?;
        let (revision, mut transaction) = self.canonical_creation_transaction(plan.input.clone())?;
        transaction
            .commands
            .extend(bindings.into_iter().map(|command| match command {
                Command::UpdateTextStyle {
                    object_id,
                    style,
                    font_face_update,
                } => Command::UpdateTableTextStyle {
                    object_id,
                    style,
                    font_face_update,
                },
                other => other,
            }));
        if transaction.commands.len() > MAX_CANONICAL_COMMANDS {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "A transaction expands beyond the 1000-command safety limit",
            ));
        }
        self.execute_saveable(
            revision,
            transaction,
            pending_assets.iter().map(font_asset_descriptor),
        )?;
        for asset in pending_assets {
            let inserted = self.assets.insert(asset.media_type, asset.bytes);
            debug_assert_eq!(inserted, asset.hash);
        }
        Ok(VerifiedFontSelectionOutcome {
            snapshot: self.snapshot()?,
            disposition: if all_embedded {
                FontSelectionDisposition::Embedded
            } else {
                FontSelectionDisposition::WindowsOnly
            },
            message: if all_embedded {
                "Table cells use exact embedded font faces".to_owned()
            } else {
                "Formatting applied; one or more cell font faces remain available from Windows only"
                    .to_owned()
            },
        })
    }

    /// Plans one atomic transaction that includes collapsed-caret font styling.
    ///
    /// The final styled story is rehearsed before Windows font inspection. The
    /// plan binds all effective variants needed by that Text Box so the eventual
    /// transaction cannot commit text that the publication resolver cannot identify.
    ///
    /// # Errors
    ///
    /// Returns a structured error for stale state, invalid edits, a locked or
    /// non-text object, or a batch with no inserted typing style.
    pub fn plan_styled_text_transaction(
        &self,
        input: ApplyTransactionInput,
    ) -> EditorResult<StyledTextTransactionPlan> {
        let snapshot = self.snapshot()?;
        if snapshot.project.revision.value() != input.expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The publication changed before styled text could be committed",
            ));
        }
        let styled_object_ids = input
            .commands
            .iter()
            .filter_map(|command| match command {
                EditorCommandInput::ApplyTextEdits { object_id, edits }
                    if edits.iter().any(inserted_font_change) =>
                {
                    Some(object_id.as_str())
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        if styled_object_ids.len() != 1 {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "A styled text transaction must edit exactly one Text Box",
            ));
        }
        let object_id = styled_object_ids
            .first()
            .ok_or_else(|| {
                EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "A styled text transaction must identify one Text Box",
                )
            })?
            .to_string();
        let (expected_revision, transaction) = self.canonical_creation_transaction(input.clone())?;
        let mut rehearsal = DocumentSession::from_saved(
            snapshot.project.document.clone(),
            snapshot.project.revision,
        )
        .map_err(command_rejected)?;
        rehearsal
            .execute(expected_revision, transaction)
            .map_err(command_rejected)?;
        let parsed_object_id = ObjectId::parse(object_id.clone()).map_err(invalid_input)?;
        let object = find_object(rehearsal.document(), &parsed_object_id).ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::InvalidInput,
                "That Text Box is no longer available",
            )
        })?;
        let ObjectKind::Text {
            text, style, story, ..
        } = &object.kind
        else {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "Only a Text Box can commit styled text",
            ));
        };
        let base_variant = RequiredFontVariant {
            font_family: style.font_family.clone(),
            font_weight: style.font_weight,
            italic: style.italic,
        };
        let mut required_variants = effective_variants_in_range(
            style,
            story,
            TextSourceRange {
                start: 0,
                end: text.len(),
            },
        )
        .into_iter()
        .filter(|variant| {
            style
                .exact_font_face_for_family(
                    &variant.font_family,
                    variant.font_weight,
                    variant.italic,
                )
                .is_none()
        })
        .collect::<BTreeSet<_>>();
        if style.font_face.is_none() {
            required_variants.insert(base_variant.clone());
        }
        Ok(StyledTextTransactionPlan {
            input,
            object_id,
            font_family: style.font_family.clone(),
            base_variant,
            required_variants: required_variants.into_iter().collect(),
            final_style: style.clone(),
        })
    }

    /// Commits styled typing and its verified font bindings in one Undo step.
    ///
    /// # Errors
    ///
    /// Returns a structured error when the plan is stale, the inspected face
    /// set drifts, or the atomic canonical transaction is rejected.
    pub fn apply_verified_styled_text_transaction(
        &mut self,
        input: VerifiedStyledTextTransactionInput,
    ) -> EditorResult<EditorSnapshot> {
        let evidence = input
            .variants
            .into_iter()
            .map(|variant| (variant.requested, variant.inspected_face.into()))
            .collect();
        self.apply_inspected_styled_text_transaction(&input.plan, evidence)
    }

    fn apply_inspected_styled_text_transaction(
        &mut self,
        plan: &StyledTextTransactionPlan,
        evidence: Vec<(RequiredFontVariant, InspectedFaceEvidence)>,
    ) -> EditorResult<EditorSnapshot> {
        let current_plan = self.plan_styled_text_transaction(plan.input.clone())?;
        if &current_plan != plan {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The Text Box changed before styled text could be committed",
            ));
        }
        let object_id = ObjectId::parse(plan.object_id.clone()).map_err(invalid_input)?;
        let (binding_commands, pending_assets, _) = prepare_variant_binding_commands(
            &plan.required_variants,
            &plan.base_variant,
            &plan.final_style,
            &object_id,
            evidence,
        )?;
        let (expected_revision, mut transaction) = self.canonical_creation_transaction(plan.input.clone())?;
        transaction.commands.extend(binding_commands);
        if transaction.commands.len() > MAX_CANONICAL_COMMANDS {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "A transaction expands beyond the 1000-command safety limit",
            ));
        }
        self.execute_saveable(
            expected_revision,
            transaction,
            pending_assets.iter().map(font_asset_descriptor),
        )?;
        for asset in pending_assets {
            let inserted_hash = self.assets.insert(asset.media_type, asset.bytes);
            debug_assert_eq!(inserted_hash, asset.hash);
        }
        self.snapshot()
    }

    fn apply_inspected_inline_font_selection(
        &mut self,
        plan: &InlineFontSelectionPlan,
        evidence: Vec<(RequiredFontVariant, InspectedFaceEvidence)>,
    ) -> EditorResult<VerifiedFontSelectionOutcome> {
        let current_plan = self.plan_inline_font_selection(plan.intent.clone())?;
        if &current_plan != plan {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The selected text changed before its font faces could be applied",
            ));
        }
        let snapshot = self.snapshot()?;
        let object_id = ObjectId::parse(plan.intent.object_id.clone()).map_err(invalid_input)?;
        let object = find_object(&snapshot.project.document, &object_id).ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::InvalidInput,
                "That Text Box is no longer available",
            )
        })?;
        let ObjectKind::Text { style, .. } = &object.kind else {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "Only a Text Box selection can use inline formatting",
            ));
        };

        let (mut commands, pending_assets, all_embedded) = prepare_variant_binding_commands(
            &plan.required_variants,
            &plan.base_variant,
            style,
            &object_id,
            evidence,
        )?;
        commands.push(Command::UpdateInlineTextAppearance {
            object_id,
            range: TextSourceRange {
                start: plan.intent.start_byte,
                end: plan.intent.end_byte,
            },
            style: inline_intent_style(&plan.intent),
        });
        self.execute_saveable(
            Revision::new(plan.intent.expected_revision),
            Transaction {
                label: "Format selected text".to_owned(),
                commands,
            },
            pending_assets.iter().map(font_asset_descriptor),
        )?;
        for asset in pending_assets {
            let inserted_hash = self.assets.insert(asset.media_type, asset.bytes);
            debug_assert_eq!(inserted_hash, asset.hash);
        }
        let (disposition, message) = if all_embedded {
            (
                FontSelectionDisposition::Embedded,
                "Selected text uses exact embedded font faces".to_owned(),
            )
        } else {
            (
                FontSelectionDisposition::WindowsOnly,
                "Formatting applied; one or more font faces remain available from Windows only"
                    .to_owned(),
            )
        };
        Ok(VerifiedFontSelectionOutcome {
            snapshot: self.snapshot()?,
            disposition,
            message,
        })
    }

    fn apply_inspected_font_selection(
        &mut self,
        expected_revision: u64,
        object_id: String,
        font_family: String,
        font_weight: u16,
        italic: bool,
        evidence: InspectedFaceEvidence,
    ) -> EditorResult<VerifiedFontSelectionOutcome> {
        validate_selection_request(&font_family, font_weight, italic, &evidence)?;
        let snapshot = self.snapshot()?;
        if snapshot.project.revision.value() != expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The publication changed before the font could be applied",
            ));
        }
        let object_id = ObjectId::parse(object_id).map_err(invalid_input)?;
        let object = find_object(&snapshot.project.document, &object_id).ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::InvalidInput,
                "That Text Box or Table is no longer available",
            )
        })?;
        let mut style = match &object.kind {
            ObjectKind::Text { style, .. } => style.clone(),
            ObjectKind::Table { table } => table.style.clone(),
            _ => {
                return Err(EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "Only a Text Box or Table can use a font",
                ));
            }
        };
        style.font_family = font_family;
        style.font_weight = font_weight;
        style.italic = italic;

        let may_embed = editable_project_embedding_allowed(&evidence);
        let (binding, pending_asset) = if may_embed {
            let media_type = font_media_type(&evidence.source_bytes)?;
            let hash = AssetHash::for_bytes(&evidence.source_bytes);
            style.font_face = Some(FontFaceBinding {
                source: hash.clone(),
                face_index: evidence.face_index,
            });
            (
                style.font_face.clone(),
                Some((media_type, evidence.source_bytes, hash)),
            )
        } else {
            style.font_face = None;
            (None, None)
        };
        let command = match &object.kind {
            ObjectKind::Text { .. } => Command::UpdateTextStyle {
                object_id,
                style,
                font_face_update: FontFaceBindingUpdate::Replace(binding),
            },
            ObjectKind::Table { .. } => Command::UpdateTableTextStyle {
                object_id,
                style,
                font_face_update: FontFaceBindingUpdate::Replace(binding),
            },
            _ => unreachable!("object kind was validated above"),
        };
        self.execute_saveable(
            Revision::new(expected_revision),
            Transaction::single("Choose font", command),
            pending_asset.iter().map(|(media_type, bytes, hash)| {
                (
                    hash.clone(),
                    AssetDescriptor {
                        media_type: (*media_type).to_owned(),
                        byte_length: bytes.len() as u64,
                        raster: None,
                    },
                )
            }),
        )?;
        if let Some((media_type, bytes, expected_hash)) = pending_asset {
            let inserted_hash = self.assets.insert(media_type, bytes);
            debug_assert_eq!(inserted_hash, expected_hash);
        }
        let (disposition, message) = if may_embed {
            (
                FontSelectionDisposition::Embedded,
                "Font embedded with this publication".to_owned(),
            )
        } else {
            (
                FontSelectionDisposition::WindowsOnly,
                windows_only_message(evidence.embedding_rights, evidence.container_kind),
            )
        };
        Ok(VerifiedFontSelectionOutcome {
            snapshot: self.snapshot()?,
            disposition,
            message,
        })
    }
}

fn font_asset_descriptor(asset: &PendingFontAsset) -> (AssetHash, AssetDescriptor) {
    (
        asset.hash.clone(),
        AssetDescriptor {
            media_type: asset.media_type.to_owned(),
            byte_length: asset.bytes.len() as u64,
            raster: None,
        },
    )
}

fn prepare_variant_binding_commands(
    required_variants: &[RequiredFontVariant],
    base_variant: &RequiredFontVariant,
    style: &document_core::model::TextStyle,
    object_id: &ObjectId,
    evidence: Vec<(RequiredFontVariant, InspectedFaceEvidence)>,
) -> EditorResult<(Vec<Command>, Vec<PendingFontAsset>, bool)> {
    let mut evidence_by_variant = BTreeMap::new();
    for (requested, inspected) in evidence {
        if evidence_by_variant.insert(requested, inspected).is_some() {
            return Err(invalid_variant_evidence());
        }
    }
    if evidence_by_variant.len() != required_variants.len()
        || required_variants
            .iter()
            .any(|variant| !evidence_by_variant.contains_key(variant))
    {
        return Err(invalid_variant_evidence());
    }

    let mut commands = Vec::with_capacity(required_variants.len() + 1);
    let mut pending_assets = Vec::new();
    let mut all_embedded = true;
    for requested in required_variants {
        let inspected = evidence_by_variant
            .remove(requested)
            .expect("the exact evidence set was validated");
        validate_selection_request(
            &requested.font_family,
            requested.font_weight,
            requested.italic,
            &inspected,
        )?;
        if !editable_project_embedding_allowed(&inspected) {
            all_embedded = false;
            continue;
        }
        let media_type = font_media_type(&inspected.source_bytes)?;
        let hash = AssetHash::for_bytes(&inspected.source_bytes);
        let binding = FontFaceBinding {
            source: hash.clone(),
            face_index: inspected.face_index,
        };
        if requested == base_variant {
            commands.push(Command::UpdateTextStyle {
                object_id: object_id.clone(),
                style: style.clone(),
                font_face_update: FontFaceBindingUpdate::Replace(Some(binding)),
            });
        } else if requested.font_family == style.font_family {
            commands.push(Command::SetTextFontVariantBinding {
                object_id: object_id.clone(),
                font_weight: requested.font_weight,
                italic: requested.italic,
                font_face: Some(binding),
            });
        } else {
            commands.push(Command::SetTextFontFamilyBinding {
                object_id: object_id.clone(),
                font_family: requested.font_family.clone(),
                font_weight: requested.font_weight,
                italic: requested.italic,
                font_face: binding,
            });
        }
        pending_assets.push(PendingFontAsset {
            media_type,
            bytes: inspected.source_bytes,
            hash,
        });
    }
    Ok((commands, pending_assets, all_embedded))
}

fn validate_selection_request(
    requested_family: &str,
    requested_weight: u16,
    requested_italic: bool,
    evidence: &InspectedFaceEvidence,
) -> EditorResult<()> {
    require_text(requested_family, MAX_FONT_FAMILY_BYTES, "font family")?;
    require_text(
        evidence.family_names.first().map_or("", String::as_str),
        MAX_FONT_FAMILY_BYTES,
        "inspected font family",
    )?;
    let requested_family = requested_family.trim().to_lowercase();
    let family_matches = evidence
        .family_names
        .iter()
        .any(|family| family.trim().to_lowercase() == requested_family);
    if requested_family.chars().count() > MAX_FONT_FAMILY_CHARS
        || !(1..=1_000).contains(&requested_weight)
        || !family_matches
        || !(1..=1_000).contains(&evidence.weight)
        || evidence.italic != requested_italic
        || evidence.italic
            != matches!(
                evidence.style,
                FontFaceStyle::Italic | FontFaceStyle::Oblique
            )
        || (evidence.container_kind == FontContainerKind::SingleFont
            && (evidence.face_count != 1 || evidence.face_index != 0))
        || evidence.face_count == 0
        || evidence.face_index >= evidence.face_count
        || AssetHash::for_bytes(&evidence.source_bytes).as_str()
            != sha256_hex(evidence.source_sha256)
    {
        return Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "The inspected font does not match the selected face",
        ));
    }
    Ok(())
}

fn invalid_variant_evidence() -> EditorError {
    EditorError::new(
        EditorErrorCode::InvalidInput,
        "The inspected font faces do not match the selected text request",
    )
}

fn find_object<'a>(
    document: &'a document_core::model::Document,
    object_id: &ObjectId,
) -> Option<&'a document_core::model::PublicationObject> {
    document.authored_objects()
        .find(|object| &object.id == object_id)
}

fn effective_variants_in_range(
    style: &document_core::model::TextStyle,
    story: &document_core::model::TextStoryFormatting,
    range: TextSourceRange,
) -> BTreeSet<RequiredFontVariant> {
    effective_font_variant_ranges(style, story, range)
        .into_iter()
        .map(|range| RequiredFontVariant {
            font_family: range.font_family,
            font_weight: range.font_weight,
            italic: range.italic,
        })
        .collect()
}

fn inline_intent_style(
    intent: &InlineFontSelectionIntent,
) -> document_core::model::InlineTextStyle {
    document_core::model::InlineTextStyle {
        font_family: intent.font_family.clone(),
        font_weight: intent.font_weight,
        bold: intent.bold,
        italic: intent.italic,
        ..document_core::model::InlineTextStyle::default()
    }
}

fn inserted_font_change(edit: &super::TextEditInput) -> bool {
    !edit.text.is_empty()
        && (edit.bold.is_some()
            || edit.italic.is_some()
            || edit.style.as_ref().is_some_and(|style| {
                style.font_family.is_some()
                    || style.font_weight.is_some()
                    || style.bold.is_some()
                    || style.italic.is_some()
            }))
}

fn editable_project_embedding_allowed(evidence: &InspectedFaceEvidence) -> bool {
    evidence.container_kind == FontContainerKind::SingleFont
        && matches!(
            evidence.embedding_rights,
            FontEmbeddingRights::SubsetAllowed {
                license: FontEmbeddingLicense::Installable | FontEmbeddingLicense::Editable,
                ..
            } | FontEmbeddingRights::FullFontRequired {
                license: FontEmbeddingLicense::Installable | FontEmbeddingLicense::Editable,
                ..
            }
        )
}

fn font_media_type(bytes: &[u8]) -> EditorResult<&'static str> {
    match bytes.get(..4) {
        Some(b"OTTO") => Ok("font/otf"),
        Some([0, 1, 0, 0] | b"true") => Ok("font/ttf"),
        _ => Err(EditorError::new(
            EditorErrorCode::InvalidInput,
            "The inspected font has an unsupported OpenType signature",
        )),
    }
}

fn windows_only_message(rights: FontEmbeddingRights, container: FontContainerKind) -> String {
    if container == FontContainerKind::Collection {
        return "Used from Windows only; font collections are not embedded yet".to_owned();
    }
    match rights {
        FontEmbeddingRights::SubsetAllowed {
            license: FontEmbeddingLicense::PreviewAndPrint,
            ..
        }
        | FontEmbeddingRights::FullFontRequired {
            license: FontEmbeddingLicense::PreviewAndPrint,
            ..
        } => "Used from Windows only; its license does not permit embedding in an editable publication".to_owned(),
        FontEmbeddingRights::Restricted { .. } => {
            "Used from Windows only; the font does not permit embedding".to_owned()
        }
        FontEmbeddingRights::BitmapOnly { .. } => {
            "Used from Windows only; the font permits bitmap embedding only".to_owned()
        }
        FontEmbeddingRights::Unknown { .. } => {
            "Used from Windows only; embedding rights could not be verified".to_owned()
        }
        FontEmbeddingRights::SubsetAllowed { .. }
        | FontEmbeddingRights::FullFontRequired { .. } => {
            "Used from Windows only; this font cannot be safely embedded".to_owned()
        }
    }
}

fn sha256_hex(hash: [u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut value = String::with_capacity(64);
    for byte in hash {
        write!(&mut value, "{byte:02x}").expect("string writes cannot fail");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{
        FontContainerKind, FontEmbeddingLicense, FontEmbeddingRights, FontFaceStyle,
        FontSelectionDisposition, InlineFontSelectionIntent, InspectedFaceEvidence,
        RequiredFontVariant, font_media_type, validate_selection_request,
    };
    use crate::{
        ApplyTransactionInput, ColorInput, CreateDocumentInput, EditorCommandInput,
        EditorErrorCode, EditorModule, EditorResult, EditorSnapshot, ShapeInput,
        TextBoxSizingModeInput, TextEditInput, TextStyle, VerifiedFontSelectionOutcome,
    };
    use document_core::{AssetHash, model::ObjectKind};
    use font_boundary::FontEmbeddingUnknownReason;
    use std::time::{SystemTime, UNIX_EPOCH};

    const FAMILY: &str = "DejaVu Sans";

    fn create_input() -> CreateDocumentInput {
        CreateDocumentInput {
            document_id: "document-font-selection".to_owned(),
            title: "Font selection".to_owned(),
            trim_width_um: 215_900,
            trim_height_um: 279_400,
            bleed_um: 3_175,
            page_count: 1,
        }
    }

    fn transaction(
        expected_revision: u64,
        label: &str,
        commands: Vec<EditorCommandInput>,
    ) -> ApplyTransactionInput {
        ApplyTransactionInput {
            expected_revision,
            label: label.to_owned(),
            commands,
        }
    }

    fn add_text(editor: &mut EditorModule) -> String {
        let page_id = editor.snapshot().unwrap().project.document.pages[0]
            .id
            .as_str()
            .to_owned();
        let snapshot = editor
            .apply(transaction(
                0,
                "Add Text Box",
                vec![EditorCommandInput::AddText {
                    page_id: Some(page_id),
                    name: "Instructions".to_owned(),
                    text: "Make a coin vanish.".to_owned(),
                    style: None,
                    sizing_mode: TextBoxSizingModeInput::Fixed,
                    x_um: 10_000,
                    y_um: 10_000,
                    width_um: 100_000,
                    height_um: 40_000,
                }],
            ))
            .unwrap();
        snapshot.project.document.pages[0].objects[0]
            .id
            .as_str()
            .to_owned()
    }

    fn add_table(editor: &mut EditorModule) -> String {
        let page_id = editor.snapshot().unwrap().project.document.pages[0]
            .id
            .as_str()
            .to_owned();
        let snapshot = editor
            .apply(transaction(
                0,
                "Add Table",
                vec![EditorCommandInput::AddTable {
                    page_id: Some(page_id),
                    name: "Word search".to_owned(),
                    rows: 2,
                    columns: 2,
                    style: None,
                    x_um: 10_000,
                    y_um: 10_000,
                    width_um: 100_000,
                    height_um: 100_000,
                }],
            ))
            .unwrap();
        snapshot.project.document.pages[0].objects[0]
            .id
            .as_str()
            .to_owned()
    }

    fn add_shape(editor: &mut EditorModule) -> String {
        let page_id = editor.snapshot().unwrap().project.document.pages[0]
            .id
            .as_str()
            .to_owned();
        let snapshot = editor
            .apply(transaction(
                0,
                "Add Shape",
                vec![EditorCommandInput::AddShape {
                    page_id: Some(page_id),
                    name: "Not text".to_owned(),
                    shape: ShapeInput::Rectangle,
                    fill: ColorInput {
                        red: 0,
                        green: 0,
                        blue: 0,
                        alpha: 255,
                    },
                    x_um: 10_000,
                    y_um: 10_000,
                    width_um: 20_000,
                    height_um: 20_000,
                    stroke: None,
                    corner_rounding: None,
                }],
            ))
            .unwrap();
        snapshot.project.document.pages[0].objects[0]
            .id
            .as_str()
            .to_owned()
    }

    fn evidence(rights: FontEmbeddingRights) -> InspectedFaceEvidence {
        let source_bytes =
            include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
        let hash = AssetHash::for_bytes(&source_bytes);
        InspectedFaceEvidence {
            source_sha256: hash_bytes(&hash),
            source_bytes,
            face_index: 0,
            container_kind: FontContainerKind::SingleFont,
            face_count: 1,
            family_names: vec![FAMILY.to_owned()],
            weight: 400,
            style: FontFaceStyle::Normal,
            italic: false,
            embedding_rights: rights,
        }
    }

    fn variant_evidence(
        weight: u16,
        italic: bool,
        rights: FontEmbeddingRights,
    ) -> InspectedFaceEvidence {
        let mut evidence = evidence(rights);
        evidence.family_names = vec!["Arial".to_owned()];
        evidence.weight = weight;
        evidence.italic = italic;
        evidence.style = if italic {
            FontFaceStyle::Italic
        } else {
            FontFaceStyle::Normal
        };
        evidence
    }

    #[test]
    fn trusted_family_aliases_and_multibyte_names_are_selectable() {
        let mut aliases = evidence(installable());
        aliases.family_names.push("Famille localisee".to_owned());
        assert!(validate_selection_request("Famille localisee", 400, false, &aliases).is_ok());

        let multibyte = "é".repeat(256);
        aliases.family_names = vec![multibyte.clone()];
        assert!(validate_selection_request(&multibyte, 400, false, &aliases).is_ok());

        let too_many_characters = format!("{multibyte}é");
        aliases.family_names = vec![too_many_characters.clone()];
        assert_eq!(
            validate_selection_request(&too_many_characters, 400, false, &aliases)
                .expect_err("257-character family is rejected")
                .code,
            EditorErrorCode::InvalidInput
        );
    }

    fn hash_bytes(hash: &AssetHash) -> [u8; 32] {
        let mut bytes = [0; 32];
        let encoded = hash.as_str().as_bytes();
        for (index, byte) in bytes.iter_mut().enumerate() {
            let offset = index * 2;
            *byte = (hex_digit(encoded[offset]) << 4) | hex_digit(encoded[offset + 1]);
        }
        bytes
    }

    const fn hex_digit(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("test hash is canonical lowercase hex"),
        }
    }

    fn installable() -> FontEmbeddingRights {
        FontEmbeddingRights::SubsetAllowed {
            license: FontEmbeddingLicense::Installable,
            fs_type: 0,
        }
    }

    fn select(
        editor: &mut EditorModule,
        revision: u64,
        object_id: String,
        evidence: InspectedFaceEvidence,
    ) -> EditorResult<VerifiedFontSelectionOutcome> {
        editor.apply_inspected_font_selection(
            revision,
            object_id,
            FAMILY.to_owned(),
            400,
            false,
            evidence,
        )
    }

    fn selected_style(snapshot: &EditorSnapshot) -> &TextStyle {
        match &snapshot.project.document.pages[0].objects[0].kind {
            ObjectKind::Text { style, .. } => style,
            ObjectKind::Table { table } => &table.style,
            _ => panic!("fixture must be text-bearing"),
        }
    }

    fn selected_story(snapshot: &EditorSnapshot) -> &document_core::model::TextStoryFormatting {
        match &snapshot.project.document.pages[0].objects[0].kind {
            ObjectKind::Text { story, .. } => story,
            _ => panic!("fixture must be a Text Box"),
        }
    }

    fn editor_with_mixed_italic_text() -> (EditorModule, String) {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let object_id = add_text(&mut editor);
        editor
            .apply(transaction(
                1,
                "Italic first word",
                vec![EditorCommandInput::UpdateInlineTextStyle {
                    object_id: object_id.clone(),
                    start_byte: 0,
                    end_byte: 4,
                    bold: None,
                    italic: Some(true),
                }],
            ))
            .unwrap();
        (editor, object_id)
    }

    fn mixed_italic_bold_plan(
        editor: &EditorModule,
        object_id: String,
    ) -> super::InlineFontSelectionPlan {
        editor
            .plan_inline_font_selection(InlineFontSelectionIntent {
                font_family: None,
                font_weight: None,
                expected_revision: 2,
                object_id,
                start_byte: 0,
                end_byte: 8,
                bold: Some(true),
                italic: None,
            })
            .unwrap()
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Capture/refusal and Undo share one revision-bound fixture.
    fn selected_family_captures_each_family_and_preserves_the_other_words() {
        let (mut editor, object_id) = editor_with_mixed_italic_text();
        let before = editor.snapshot().unwrap();
        let plan = editor
            .plan_inline_font_selection(InlineFontSelectionIntent {
                expected_revision: 2,
                object_id: object_id.clone(),
                start_byte: 0,
                end_byte: 4,
                font_family: Some(FAMILY.to_owned()),
                font_weight: Some(400),
                bold: None,
                italic: Some(false),
            })
            .unwrap();
        assert_eq!(
            plan.required_variants(),
            &[
                RequiredFontVariant {
                    font_family: "Arial".to_owned(),
                    font_weight: 400,
                    italic: false
                },
                RequiredFontVariant {
                    font_family: FAMILY.to_owned(),
                    font_weight: 400,
                    italic: false
                },
            ]
        );
        assert!(
            editor
                .apply_inspected_inline_font_selection(&plan, vec![])
                .is_err()
        );
        assert_eq!(editor.snapshot().unwrap().project, before.project);
        assert_eq!(editor.assets.len(), 0);
        let selected = editor
            .apply_inspected_inline_font_selection(
                &plan,
                vec![
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 400,
                            italic: false,
                        },
                        variant_evidence(400, false, installable()),
                    ),
                    (
                        RequiredFontVariant {
                            font_family: FAMILY.to_owned(),
                            font_weight: 400,
                            italic: false,
                        },
                        evidence(installable()),
                    ),
                ],
            )
            .unwrap();
        assert_eq!(selected_style(&selected.snapshot).font_family, "Arial");
        assert_eq!(selected_story(&selected.snapshot).inline_spans.len(), 1);
        let span = &selected_story(&selected.snapshot).inline_spans[0];
        assert_eq!((span.range.start, span.range.end), (0, 4));
        assert_eq!(span.style.font_family.as_deref(), Some(FAMILY));
        assert_eq!(span.style.font_weight, Some(400));
        assert!(
            selected_style(&selected.snapshot)
                .exact_font_face_for_family(FAMILY, 400, false)
                .is_some()
        );
        let second_plan = editor
            .plan_inline_font_selection(InlineFontSelectionIntent {
                expected_revision: 3,
                object_id,
                start_byte: 0,
                end_byte: 8,
                font_family: None,
                font_weight: None,
                bold: Some(true),
                italic: None,
            })
            .unwrap();
        assert_eq!(
            second_plan.required_variants(),
            &[
                RequiredFontVariant {
                    font_family: "Arial".to_owned(),
                    font_weight: 700,
                    italic: false
                },
                RequiredFontVariant {
                    font_family: FAMILY.to_owned(),
                    font_weight: 700,
                    italic: false
                },
            ]
        );
        assert_eq!(
            editor.undo(3).unwrap().project.document,
            before.project.document
        );
        assert_eq!(
            editor.redo(4).unwrap().project.document,
            selected.snapshot.project.document
        );
    }

    #[test]
    fn mixed_selection_captures_unbound_base_and_distinct_effective_variants_once() {
        let (mut editor, object_id) = editor_with_mixed_italic_text();
        let before = editor.snapshot().unwrap();
        let plan = mixed_italic_bold_plan(&editor, object_id);
        assert_eq!(plan.font_family(), "Arial");
        assert_eq!(
            plan.required_variants(),
            &[
                RequiredFontVariant {
                    font_family: "Arial".to_owned(),
                    font_weight: 400,
                    italic: false,
                },
                RequiredFontVariant {
                    font_family: "Arial".to_owned(),
                    font_weight: 700,
                    italic: false,
                },
                RequiredFontVariant {
                    font_family: "Arial".to_owned(),
                    font_weight: 700,
                    italic: true,
                },
            ]
        );

        let selected = editor
            .apply_inspected_inline_font_selection(
                &plan,
                vec![
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 400,
                            italic: false,
                        },
                        variant_evidence(400, false, installable()),
                    ),
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 700,
                            italic: false,
                        },
                        variant_evidence(700, false, installable()),
                    ),
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 700,
                            italic: true,
                        },
                        variant_evidence(700, true, installable()),
                    ),
                ],
            )
            .unwrap();
        assert_eq!(selected.disposition, FontSelectionDisposition::Embedded);
        assert_eq!(selected.snapshot.project.revision.value(), 3);
        assert_eq!(selected_style(&selected.snapshot).font_family, "Arial");
        assert!(selected_style(&selected.snapshot).font_face.is_some());
        assert_eq!(
            selected_style(&selected.snapshot)
                .exact_font_variants
                .iter()
                .map(|variant| (variant.font_weight, variant.italic))
                .collect::<Vec<_>>(),
            vec![(700, false), (700, true)]
        );
        assert_eq!(
            selected_story(&selected.snapshot)
                .inline_spans
                .iter()
                .map(|span| {
                    (
                        span.range.start,
                        span.range.end,
                        span.style.bold,
                        span.style.italic,
                    )
                })
                .collect::<Vec<_>>(),
            vec![(0, 4, Some(true), Some(true)), (4, 8, Some(true), None)]
        );

        let undone = editor.undo(3).unwrap();
        assert_eq!(undone.project.document, before.project.document);
        let redone = editor.redo(4).unwrap();
        assert_eq!(
            selected_style(&redone).exact_font_variants.len(),
            2,
            "one redo restores bindings and formatting together"
        );
    }

    #[test]
    fn styled_typing_binds_variants_and_undoes_with_inserted_text() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let object_id = add_text(&mut editor);
        let before = editor.snapshot().unwrap();
        let plan = editor
            .plan_styled_text_transaction(transaction(
                1,
                "Edit styled text and rotate",
                vec![
                    EditorCommandInput::ApplyTextEdits {
                        object_id: object_id.clone(),
                        edits: vec![TextEditInput {
                            start_byte: 19,
                            end_byte: 19,
                            text: " Bold".to_owned(),
                            bold: Some(true),
                            italic: Some(false),
                            style: None,
                        }],
                    },
                    EditorCommandInput::SetRotation {
                        object_id,
                        rotation_microdegrees: 5_000_000,
                    },
                ],
            ))
            .unwrap();
        assert_eq!(
            plan.required_variants(),
            &[
                RequiredFontVariant {
                    font_family: "Arial".to_owned(),
                    font_weight: 400,
                    italic: false,
                },
                RequiredFontVariant {
                    font_family: "Arial".to_owned(),
                    font_weight: 700,
                    italic: false,
                },
            ]
        );

        let edited = editor
            .apply_inspected_styled_text_transaction(
                &plan,
                vec![
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 400,
                            italic: false,
                        },
                        variant_evidence(400, false, installable()),
                    ),
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 700,
                            italic: false,
                        },
                        variant_evidence(700, false, installable()),
                    ),
                ],
            )
            .unwrap();
        let ObjectKind::Text { text, story, .. } =
            &edited.project.document.pages[0].objects[0].kind
        else {
            panic!("fixture remains a Text Box");
        };
        assert_eq!(text, "Make a coin vanish. Bold");
        assert_eq!(
            story
                .inline_spans
                .iter()
                .map(|span| (
                    span.range.start,
                    span.range.end,
                    span.style.bold,
                    span.style.italic
                ))
                .collect::<Vec<_>>(),
            vec![(19, 24, Some(true), Some(false))]
        );
        assert!(selected_style(&edited).font_face.is_some());
        assert_eq!(selected_style(&edited).exact_font_variants.len(), 1);
        assert_eq!(
            edited.project.document.pages[0].objects[0]
                .transform
                .rotation_microdegrees,
            5_000_000
        );

        let undone = editor.undo(edited.project.revision.value()).unwrap();
        assert_eq!(undone.project.document, before.project.document);
        let redone = editor.redo(undone.project.revision.value()).unwrap();
        assert_eq!(redone.project.document, edited.project.document);
    }

    #[test]
    fn denied_variant_is_an_honest_windows_only_result_without_partial_failure() {
        let (mut editor, object_id) = editor_with_mixed_italic_text();
        let plan = mixed_italic_bold_plan(&editor, object_id);
        let selected = editor
            .apply_inspected_inline_font_selection(
                &plan,
                vec![
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 400,
                            italic: false,
                        },
                        variant_evidence(400, false, installable()),
                    ),
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 700,
                            italic: false,
                        },
                        variant_evidence(700, false, installable()),
                    ),
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 700,
                            italic: true,
                        },
                        variant_evidence(700, true, FontEmbeddingRights::Restricted { fs_type: 2 }),
                    ),
                ],
            )
            .unwrap();

        assert_eq!(selected.disposition, FontSelectionDisposition::WindowsOnly);
        assert_eq!(selected.snapshot.project.revision.value(), 3);
        assert_eq!(
            selected_style(&selected.snapshot)
                .exact_font_variants
                .iter()
                .map(|variant| (variant.font_weight, variant.italic))
                .collect::<Vec<_>>(),
            vec![(700, false)]
        );
        assert_eq!(editor.snapshot_for_preflight(3).unwrap().assets.len(), 1);
        assert_eq!(selected_story(&selected.snapshot).inline_spans.len(), 2);
    }

    #[test]
    fn every_inline_receipt_is_validated_before_document_or_assets_change() {
        let (mut editor, object_id) = editor_with_mixed_italic_text();
        let before = editor.snapshot().unwrap();
        let plan = mixed_italic_bold_plan(&editor, object_id);
        let error = editor
            .apply_inspected_inline_font_selection(
                &plan,
                vec![
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 400,
                            italic: false,
                        },
                        variant_evidence(400, false, installable()),
                    ),
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 700,
                            italic: false,
                        },
                        variant_evidence(700, false, installable()),
                    ),
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 700,
                            italic: true,
                        },
                        variant_evidence(700, false, installable()),
                    ),
                ],
            )
            .expect_err("the second receipt has the wrong slant");

        assert_eq!(error.code, EditorErrorCode::InvalidInput);
        assert_eq!(editor.snapshot().unwrap(), before);
        assert!(editor.snapshot_for_preflight(2).unwrap().assets.is_empty());
    }

    #[test]
    fn inline_plan_rejects_missing_receipts_and_stale_revisions() {
        let (mut editor, object_id) = editor_with_mixed_italic_text();
        let plan = mixed_italic_bold_plan(&editor, object_id.clone());
        let before = editor.snapshot().unwrap();
        let missing = editor
            .apply_inspected_inline_font_selection(&plan, Vec::new())
            .expect_err("every planned face requires one receipt");
        assert_eq!(missing.code, EditorErrorCode::InvalidInput);
        assert_eq!(editor.snapshot().unwrap(), before);

        editor
            .apply(transaction(
                2,
                "Edit text",
                vec![EditorCommandInput::UpdateText {
                    object_id,
                    text: "Changed after planning".to_owned(),
                }],
            ))
            .unwrap();
        let stale = editor
            .apply_inspected_inline_font_selection(&plan, Vec::new())
            .expect_err("revision-bound plan must not cross another edit");
        assert_eq!(stale.code, EditorErrorCode::StaleRevision);
        assert!(editor.snapshot_for_preflight(3).unwrap().assets.is_empty());
    }

    #[test]
    fn permitted_exact_face_is_atomic_undoable_and_persistent() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let object_id = add_text(&mut editor);
        let expected_hash = AssetHash::for_bytes(&evidence(installable()).source_bytes);

        let selected = select(&mut editor, 1, object_id, evidence(installable())).unwrap();
        assert_eq!(selected.disposition, FontSelectionDisposition::Embedded);
        assert_eq!(selected.snapshot.project.revision.value(), 2);
        assert!(
            selected_style(&selected.snapshot)
                .font_face
                .as_ref()
                .is_some_and(|binding| binding.source == expected_hash && binding.face_index == 0)
        );
        let assets = editor.snapshot_for_preflight(2).unwrap().assets;
        assert_eq!(assets.len(), 1);
        assert_eq!(assets.get(&expected_hash).unwrap().media_type, "font/ttf");

        let undone = editor.undo(2).unwrap();
        assert!(selected_style(&undone).font_face.is_none());
        let redone = editor.redo(3).unwrap();
        assert_eq!(
            selected_style(&redone).font_face.as_ref().unwrap().source,
            expected_hash
        );

        let path = std::env::temp_dir().join(format!(
            "publisher-v2-font-selection-{}-{}.pubv2",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        editor.save_new(&path).unwrap();
        let mut reopened_editor = EditorModule::new();
        let reopened = reopened_editor.open(&path).unwrap();
        assert_eq!(
            selected_style(&reopened).font_face.as_ref().unwrap().source,
            expected_hash
        );
        let reopened_assets = reopened_editor
            .snapshot_for_preflight(reopened.project.revision.value())
            .unwrap()
            .assets;
        assert!(reopened_assets.contains(&expected_hash));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn tables_use_the_same_exact_font_boundary() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let object_id = add_table(&mut editor);
        let selected = select(&mut editor, 1, object_id, evidence(installable())).unwrap();
        assert_eq!(selected.disposition, FontSelectionDisposition::Embedded);
        assert!(selected_style(&selected.snapshot).font_face.is_some());
        assert_eq!(editor.snapshot_for_preflight(2).unwrap().assets.len(), 1);
    }

    #[test]
    fn every_non_editable_rights_class_remains_windows_only_without_an_asset() {
        let denied = [
            FontEmbeddingRights::SubsetAllowed {
                license: FontEmbeddingLicense::PreviewAndPrint,
                fs_type: 4,
            },
            FontEmbeddingRights::Restricted { fs_type: 2 },
            FontEmbeddingRights::BitmapOnly {
                license: FontEmbeddingLicense::Editable,
                fs_type: 0x200,
            },
            FontEmbeddingRights::Unknown {
                reason: FontEmbeddingUnknownReason::MissingOrMalformedOs2,
                fs_type: None,
            },
        ];
        for rights in denied {
            let mut editor = EditorModule::new();
            editor.create(create_input()).unwrap();
            let object_id = add_text(&mut editor);
            let selected = select(&mut editor, 1, object_id, evidence(rights)).unwrap();
            assert_eq!(selected.disposition, FontSelectionDisposition::WindowsOnly);
            assert!(selected_style(&selected.snapshot).font_face.is_none());
            assert!(editor.snapshot_for_preflight(2).unwrap().assets.is_empty());
        }
    }

    #[test]
    fn collections_remain_windows_only_even_with_installable_rights() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let object_id = add_text(&mut editor);
        let mut collection = evidence(installable());
        collection.container_kind = FontContainerKind::Collection;
        collection.face_count = 2;
        let selected = select(&mut editor, 1, object_id, collection).unwrap();
        assert_eq!(selected.disposition, FontSelectionDisposition::WindowsOnly);
        assert!(selected_style(&selected.snapshot).font_face.is_none());
        assert!(editor.snapshot_for_preflight(2).unwrap().assets.is_empty());
    }

    #[test]
    fn stale_locked_and_wrong_kind_fail_without_leaving_an_asset() {
        let mut stale_editor = EditorModule::new();
        stale_editor.create(create_input()).unwrap();
        let stale_id = add_text(&mut stale_editor);
        let stale = select(&mut stale_editor, 0, stale_id, evidence(installable())).unwrap_err();
        assert_eq!(stale.code, EditorErrorCode::StaleRevision);
        assert!(
            stale_editor
                .snapshot_for_preflight(1)
                .unwrap()
                .assets
                .is_empty()
        );

        let mut locked_editor = EditorModule::new();
        locked_editor.create(create_input()).unwrap();
        let locked_id = add_text(&mut locked_editor);
        locked_editor
            .apply(transaction(
                1,
                "Lock Text Box",
                vec![EditorCommandInput::SetObjectLock {
                    object_ids: vec![locked_id.clone()],
                    locked: true,
                }],
            ))
            .unwrap();
        let locked = select(&mut locked_editor, 2, locked_id, evidence(installable())).unwrap_err();
        assert_eq!(locked.code, EditorErrorCode::CommandRejected);
        assert!(
            locked_editor
                .snapshot_for_preflight(2)
                .unwrap()
                .assets
                .is_empty()
        );

        let mut shape_editor = EditorModule::new();
        shape_editor.create(create_input()).unwrap();
        let shape_id = add_shape(&mut shape_editor);
        let wrong_kind =
            select(&mut shape_editor, 1, shape_id, evidence(installable())).unwrap_err();
        assert_eq!(wrong_kind.code, EditorErrorCode::InvalidInput);
        assert!(
            shape_editor
                .snapshot_for_preflight(1)
                .unwrap()
                .assets
                .is_empty()
        );
    }

    #[test]
    fn moving_from_exact_to_windows_only_clears_the_binding() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let object_id = add_text(&mut editor);
        let selected = select(&mut editor, 1, object_id.clone(), evidence(installable())).unwrap();
        assert!(selected_style(&selected.snapshot).font_face.is_some());

        let denied = select(
            &mut editor,
            2,
            object_id,
            evidence(FontEmbeddingRights::Restricted { fs_type: 2 }),
        )
        .unwrap();
        assert_eq!(denied.disposition, FontSelectionDisposition::WindowsOnly);
        assert!(selected_style(&denied.snapshot).font_face.is_none());
    }

    #[test]
    fn no_subsetting_still_permits_the_full_exact_source() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let object_id = add_text(&mut editor);
        let selected = select(
            &mut editor,
            1,
            object_id,
            evidence(FontEmbeddingRights::FullFontRequired {
                license: FontEmbeddingLicense::Editable,
                fs_type: 0x100,
            }),
        )
        .unwrap();
        assert_eq!(selected.disposition, FontSelectionDisposition::Embedded);
        assert!(selected_style(&selected.snapshot).font_face.is_some());
    }

    #[test]
    fn nearest_face_receipt_does_not_replace_the_requested_canonical_style() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let object_id = add_text(&mut editor);
        let mut nearest_face = evidence(installable());
        nearest_face.weight = 600;
        let selected = editor
            .apply_inspected_font_selection(
                1,
                object_id,
                FAMILY.to_owned(),
                700,
                false,
                nearest_face,
            )
            .unwrap();
        let style = selected_style(&selected.snapshot);
        assert_eq!(style.font_weight, 700);
        assert!(!style.italic);
        assert!(style.font_face.is_some());
    }

    #[test]
    fn requested_slant_must_match_the_trusted_face_receipt() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let object_id = add_text(&mut editor);
        let before = editor.snapshot().unwrap();

        let error = editor
            .apply_inspected_font_selection(
                1,
                object_id,
                FAMILY.to_owned(),
                400,
                true,
                evidence(installable()),
            )
            .expect_err("a normal face cannot satisfy an italic request");

        assert_eq!(error.code, EditorErrorCode::InvalidInput);
        assert_eq!(editor.snapshot().unwrap(), before);
        assert!(editor.snapshot_for_preflight(1).unwrap().assets.is_empty());
    }

    #[test]
    fn receipt_identity_drift_is_rejected_before_document_or_assets_change() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let object_id = add_text(&mut editor);
        let before = editor.snapshot().unwrap();
        let mut drifted = evidence(installable());
        drifted.source_sha256[0] ^= 0xff;
        let error = select(&mut editor, 1, object_id, drifted).unwrap_err();
        assert_eq!(error.code, EditorErrorCode::InvalidInput);
        assert_eq!(editor.snapshot().unwrap(), before);
        assert!(editor.snapshot_for_preflight(1).unwrap().assets.is_empty());
    }

    #[test]
    fn supported_single_font_signatures_are_classified_without_extension_guessing() {
        assert_eq!(font_media_type(b"OTTOrest").unwrap(), "font/otf");
        assert_eq!(font_media_type(b"\0\x01\0\0rest").unwrap(), "font/ttf");
        assert_eq!(font_media_type(b"truerest").unwrap(), "font/ttf");
        assert_eq!(
            font_media_type(b"ttcfrest").unwrap_err().code,
            EditorErrorCode::InvalidInput
        );
    }
    fn cell_font_input(id: &str, revision: u64) -> ApplyTransactionInput {
        transaction(
            revision,
            "Format empty and populated Table cells",
            vec![
                EditorCommandInput::UpdateTableCells {
                    object_id: id.to_owned(),
                    row: 0,
                    column: 0,
                    cells: vec![vec!["two\nlines".to_owned()]],
                },
                EditorCommandInput::UpdateTableCellFormatting {
                    object_id: id.to_owned(),
                    range: crate::TableCellRangeInput {
                        start_row: 0,
                        end_row: 1,
                        start_column: 0,
                        end_column: 2,
                    },
                    formatting: crate::TableCellFormattingInput {
                        style: crate::InlineTextStyleInput {
                            font_family: Some(FAMILY.to_owned()),
                            underline: Some(true),
                            ..crate::InlineTextStyleInput::default()
                        },
                        ..crate::TableCellFormattingInput::default()
                    },
                },
            ],
        )
    }

    #[test]
    fn verified_table_cell_fonts_commit_typing_and_empty_cell_styles_in_one_undo() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let id = add_table(&mut editor);
        let before = editor.snapshot().unwrap();
        let plan = editor
            .plan_table_cell_font_transaction(cell_font_input(&id, 1))
            .unwrap();
        assert_eq!(plan.required_variants().len(), 2);
        let selected = editor
            .apply_inspected_table_cell_font_transaction(
                &plan,
                vec![
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 400,
                            italic: false,
                        },
                        variant_evidence(400, false, installable()),
                    ),
                    (
                        RequiredFontVariant {
                            font_family: FAMILY.to_owned(),
                            font_weight: 400,
                            italic: false,
                        },
                        evidence(installable()),
                    ),
                ],
            )
            .unwrap();
        assert_eq!(selected.disposition, FontSelectionDisposition::Embedded);
        let ObjectKind::Table { table } =
            &selected.snapshot.project.document.pages[0].objects[0].kind
        else {
            panic!("Table")
        };
        assert_eq!(table.cells[0], "two\nlines");
        assert!(table.cells[1].is_empty());
        for column in 0..2 {
            let style = table.effective_cell_style(0, column);
            assert_eq!(style.font_family, FAMILY);
            assert!(style.underline);
            assert!(style.font_face.is_some());
        }
        assert!(table.style.font_face.is_some());
        assert_eq!(editor.snapshot_for_preflight(2).unwrap().assets.len(), 1);
        assert_eq!(
            editor.undo(2).unwrap().project.document,
            before.project.document
        );
        let redone = editor.redo(3).unwrap();
        assert_eq!(redone.project.document, selected.snapshot.project.document);
        assert!(
            editor
                .plan_table_cell_font_transaction(cell_font_input(&id, 4))
                .unwrap()
                .required_variants()
                .is_empty()
        );
    }

    #[test]
    fn incomplete_drifted_and_stale_table_face_plans_preserve_document_and_assets() {
        let mut editor = EditorModule::new();
        editor.create(create_input()).unwrap();
        let id = add_table(&mut editor);
        let before = editor.snapshot().unwrap();
        let plan = editor
            .plan_table_cell_font_transaction(cell_font_input(&id, 1))
            .unwrap();
        assert!(
            editor
                .apply_inspected_table_cell_font_transaction(&plan, Vec::new())
                .is_err()
        );
        let mut drifted = evidence(installable());
        drifted.source_sha256[0] ^= 1;
        let error = editor
            .apply_inspected_table_cell_font_transaction(
                &plan,
                vec![
                    (
                        RequiredFontVariant {
                            font_family: "Arial".to_owned(),
                            font_weight: 400,
                            italic: false,
                        },
                        variant_evidence(400, false, installable()),
                    ),
                    (
                        RequiredFontVariant {
                            font_family: FAMILY.to_owned(),
                            font_weight: 400,
                            italic: false,
                        },
                        drifted,
                    ),
                ],
            )
            .unwrap_err();
        assert_eq!(error.code, EditorErrorCode::InvalidInput);
        assert_eq!(editor.snapshot().unwrap(), before);
        assert!(editor.snapshot_for_preflight(1).unwrap().assets.is_empty());
        editor
            .apply(transaction(
                1,
                "Concurrent Table edit",
                vec![EditorCommandInput::UpdateTableCell {
                    object_id: id,
                    row: 1,
                    column: 1,
                    text: "concurrent".to_owned(),
                }],
            ))
            .unwrap();
        let concurrent = editor.snapshot().unwrap();
        assert_eq!(
            editor
                .apply_inspected_table_cell_font_transaction(&plan, Vec::new())
                .unwrap_err()
                .code,
            EditorErrorCode::StaleRevision
        );
        assert_eq!(editor.snapshot().unwrap(), concurrent);
        assert!(editor.snapshot_for_preflight(2).unwrap().assets.is_empty());
    }
}
