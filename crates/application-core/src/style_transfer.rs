//! Typed appearance values deliberately exclude source content, byte ranges and placement.
use super::session_clipboard::resources::rendered_faces;
use super::session_clipboard::{
    CaptureBinding, CapturedResources, ResourceCapturePlan, SessionClipboard, check_target,
    font_unavailable, merged_resources, rehearse,
};
use super::{
    EditorModule, EditorResult, EditorSnapshot, MAX_CANONICAL_COMMANDS, invalid_input, no_document,
    parse_object_id,
};
use document_core::commands::FontFaceBindingUpdate;
use document_core::model::{
    Color, CornerRounding, ExactFontFamilyBinding, ExactFontVariantBinding, HardShadow,
    InlineTextStyle, Insets, MAX_EXACT_FONT_VARIANTS, ObjectKind, ParagraphListStyle,
    ParagraphRange, ParagraphStyle, PublicationObject, Revision, ShapeKind, ShapeStroke,
    TableGridBorder, TextBoxSizingMode, TextStoryFormatting, TextStyle, TextVerticalAlignment,
    paragraph_count,
};
use document_core::{AssetHash, AssetStore, Command, ProjectSnapshot, Transaction};
use font_boundary::InspectedFontFace;
use layout_core::{ExactFontResource, ResolvedPublication};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureStyleClipboardInput {
    pub document_id: String,
    pub expected_revision: u64,
    pub object_id: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PasteStyleClipboardInput {
    pub document_id: String,
    pub expected_revision: u64,
    pub object_ids: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StyleClipboardSummary {
    pub source_kind: String,
    pub skipped_properties: Vec<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StylePasteOutcome {
    pub snapshot: EditorSnapshot,
    pub applied_object_ids: Vec<String>,
    pub skipped_properties: Vec<String>,
}

#[derive(Clone, Debug)]
struct StyleValues {
    source_kind: String,
    opacity: u32,
    shadow: Option<HardShadow>,
    corners: Option<CornerRounding>,
    text: Option<TextStyle>,
    mixed: BTreeSet<String>,
    paragraph: Option<ParagraphStyle>,
    interior: Option<(Insets, TextVerticalAlignment)>,
    text_outline: Option<ShapeStroke>,
    text_outline_supported: bool,
    border: Option<ShapeStroke>,
    fill: Option<Color>,
    table_grid: Option<TableGridBorder>,
}
#[derive(Clone, Debug)]
pub struct StyleClipboardCapturePlan {
    binding: CaptureBinding,
    values: StyleValues,
    resources: ResourceCapturePlan,
}
impl StyleClipboardCapturePlan {
    #[must_use]
    pub fn font_resources(&self) -> Vec<super::ClipboardFontWorkOrder> {
        self.resources.font_resources()
    }
}
#[derive(Clone, Debug)]
pub(super) struct CapturedStyle {
    values: StyleValues,
    resources: CapturedResources,
    summary: StyleClipboardSummary,
}
impl EditorModule {
    /// Captures typed appearance and immutable resource prerequisites without dirtying.
    /// # Errors
    /// Refuses stale state, unavailable source and missing exact homogeneous fonts.
    pub fn plan_style_clipboard_capture(
        &self,
        input: CaptureStyleClipboardInput,
    ) -> EditorResult<StyleClipboardCapturePlan> {
        let binding = CaptureBinding::new(self, &input.document_id, input.expected_revision)?;
        let id = parse_object_id(input.object_id)?;
        let object = self
            .session
            .as_ref()
            .ok_or_else(no_document)?
            .document()
            .authored_objects()
            .find(|object| object.id == id)
            .ok_or_else(|| invalid_input("The Style source no longer exists."))?;
        let values = StyleValues::capture(object)?;
        let font_style = values
            .text
            .as_ref()
            .filter(|_| !font_is_mixed(&values.mixed));
        let resources = ResourceCapturePlan::style(&self.assets, font_style)?;
        Ok(StyleClipboardCapturePlan {
            binding,
            values,
            resources,
        })
    }
}
impl SessionClipboard {
    /// Completes trusted existing-font inspection and replaces only the Style clipboard.
    /// # Errors
    /// Refuses stale source bindings and absent/denied required font evidence.
    pub fn capture_style(
        &mut self,
        editor: &EditorModule,
        plan: StyleClipboardCapturePlan,
        faces: Vec<InspectedFontFace>,
    ) -> EditorResult<StyleClipboardSummary> {
        plan.binding.validate(editor)?;
        let (resources, accepted) = plan.resources.verify(faces)?;
        let mut values = plan.values;
        if let Some(style) = &mut values.text {
            let before = style.exact_font_variants.len() + style.exact_font_families.len();
            style.exact_font_variants.retain(|variant| {
                accepted.get(&(variant.face.source.clone(), variant.face.face_index))
                    == Some(&(variant.font_weight, variant.italic))
            });
            style.exact_font_families.retain(|variant| {
                accepted.get(&(variant.face.source.clone(), variant.face.face_index))
                    == Some(&(variant.font_weight, variant.italic))
            });
            if !font_is_mixed(&values.mixed)
                && before != style.exact_font_variants.len() + style.exact_font_families.len()
            {
                values
                    .mixed
                    .insert("unavailable optional font variants".into());
            }
        }
        let summary = StyleClipboardSummary {
            source_kind: values.source_kind.clone(),
            skipped_properties: values.mixed.iter().cloned().collect(),
        };
        self.style = Some(CapturedStyle {
            values,
            resources,
            summary: summary.clone(),
        });
        Ok(summary)
    }
    #[must_use]
    pub fn style_summary(&self) -> Option<&StyleClipboardSummary> {
        self.style.as_ref().map(|style| &style.summary)
    }

    /// Applies compatible typed values to the complete target selection in one Undo.
    /// Content, transforms, crop, sizing mode, cells, tracks and local overrides survive.
    /// # Errors
    /// Refuses stale/locked targets, exact-font or resource failures and any change
    /// to visible Fit Text Box dimensions. Switch those targets to Fixed first.
    #[allow(clippy::needless_pass_by_value)] // Owned native intent matches other revision-bound editor APIs.
    pub fn paste_style(
        &self,
        editor: &mut EditorModule,
        input: PasteStyleClipboardInput,
    ) -> EditorResult<StylePasteOutcome> {
        check_target(editor, &input.document_id, input.expected_revision)?;
        let captured = self
            .style
            .as_ref()
            .ok_or_else(|| invalid_input("Copy a Style before pasting it."))?;
        let targets = style_targets(editor, &input.object_ids)?;
        let mut skipped = captured.values.mixed.clone();
        let mut commands = Vec::new();
        for target in &targets {
            captured
                .values
                .commands(target, &mut commands, &mut skipped)?;
        }
        if commands.len() > MAX_CANONICAL_COMMANDS {
            return Err(invalid_input(
                "This Style selection is too complex. Paste to fewer objects at a time.",
            ));
        }
        let transaction = Transaction {
            label: "Paste Style".into(),
            commands,
        };
        let after = rehearse(editor, input.expected_revision, &transaction)?;
        let resources = merged_resources(editor, &captured.resources);
        let prospective = after
            .document
            .authored_objects()
            .filter(|object| targets.iter().any(|target| target.id == object.id))
            .cloned()
            .collect::<Vec<_>>();
        if captured.values.text.is_some() && !font_is_mixed(&captured.values.mixed) {
            validate_rendered_resources(&prospective, &resources)?;
        }
        preserve_fit_geometry(editor, &targets, &after, &resources)?;
        editor.execute_saveable(
            Revision::new(input.expected_revision),
            transaction,
            captured.resources.descriptors(),
        )?;
        captured.resources.insert_into(&mut editor.assets);
        Ok(StylePasteOutcome {
            snapshot: editor.snapshot()?,
            applied_object_ids: targets
                .iter()
                .map(|target| target.id.as_str().into())
                .collect(),
            skipped_properties: skipped.into_iter().collect(),
        })
    }
}
fn font_is_mixed(mixed: &BTreeSet<String>) -> bool {
    mixed.contains("font family") || mixed.contains("font weight") || mixed.contains("font slant")
}
impl StyleValues {
    fn capture(object: &PublicationObject) -> EditorResult<Self> {
        let mut values = Self {
            source_kind: String::new(),
            opacity: object.opacity_millionths,
            shadow: object.effects.shadow,
            corners: None,
            text: None,
            mixed: BTreeSet::new(),
            paragraph: None,
            interior: None,
            text_outline: None,
            text_outline_supported: false,
            border: None,
            fill: None,
            table_grid: None,
        };
        match &object.kind {
            ObjectKind::Image { .. } => {
                values.source_kind = "Picture".into();
                values.corners = Some(object.corner_rounding);
                values.border = Some(object.effects.picture_border.unwrap_or_default());
            }
            ObjectKind::Shape {
                shape,
                fill,
                stroke,
                ..
            } => {
                values.source_kind = "Shape".into();
                values.fill = Some(*fill);
                values.border = Some(*stroke);
                if *shape == ShapeKind::Rectangle {
                    values.corners = Some(object.corner_rounding);
                }
            }
            ObjectKind::Text {
                text,
                style,
                story,
                frame,
                ..
            } => {
                values.source_kind = "Text Box".into();
                values.corners = Some(object.corner_rounding);
                values.text = Some(uniform_text_style(text, style, story, &mut values.mixed));
                values.paragraph = Some(uniform_paragraph(text, style, story, &mut values.mixed)?);
                values.interior = Some((frame.margins, frame.vertical_alignment));
                values.text_outline = object.effects.text_outline;
                values.text_outline_supported = true;
            }
            ObjectKind::Table { table } => {
                values.source_kind = "Table".into();
                values.corners = Some(object.corner_rounding);
                values.table_grid = Some(table.grid_border);
                let mut first = None;
                for row in 0..table.rows {
                    for column in 0..table.columns {
                        let style = table.effective_cell_style(row, column);
                        if let Some(first) = &first {
                            compare_text(first, &style, &mut values.mixed);
                        } else {
                            first = Some(style);
                        }
                    }
                }
                let mut style = first.unwrap_or_else(|| table.style.clone());
                rebuild_variants(&mut style, &table.style);
                values.text = Some(style);
                if table.cell_formatting.iter().any(|cell| cell.fill.is_some()) {
                    values.mixed.insert("cell fills".into());
                }
                if table
                    .cell_formatting
                    .iter()
                    .any(|cell| !cell.borders.is_default())
                {
                    values.mixed.insert("cell borders".into());
                }
                if table
                    .cell_formatting
                    .iter()
                    .any(|cell| cell.vertical_alignment.is_some())
                {
                    values.mixed.insert("cell vertical alignment".into());
                }
            }
        }
        if font_is_mixed(&values.mixed) {
            values.mixed.insert("font selection (mixed source)".into());
        }
        Ok(values)
    }
    #[allow(clippy::too_many_lines)] // Compatible typed-property dispatch stays together to expose preservation rules.
    fn commands(
        &self,
        target: &PublicationObject,
        commands: &mut Vec<Command>,
        skipped: &mut BTreeSet<String>,
    ) -> EditorResult<()> {
        let id = &target.id;
        commands.push(Command::SetOpacity {
            object_id: id.clone(),
            opacity_millionths: self.opacity,
        });
        let mut effects = target.effects;
        effects.shadow = self.shadow;
        match &target.kind {
            ObjectKind::Text { text, style, .. } => {
                if let Some(source) = &self.text {
                    let updated = self.target_text(style, source);
                    commands.push(Command::UpdateTextStyle {
                        object_id: id.clone(),
                        style: updated,
                        font_face_update: if font_is_mixed(&self.mixed) {
                            FontFaceBindingUpdate::PreserveIfFamilyUnchanged
                        } else {
                            FontFaceBindingUpdate::Replace(source.font_face.clone())
                        },
                    });
                    if !font_is_mixed(&self.mixed) {
                        font_commands(id, source, commands);
                    }
                }
                if let Some(style) = self.paragraph
                    && !style.is_default()
                {
                    commands.push(Command::UpdateParagraphStyle {
                        object_id: id.clone(),
                        range: ParagraphRange {
                            start: 0,
                            end: paragraph_count(text).map_err(invalid_input)?,
                        },
                        style,
                    });
                }
                if let Some((margins, vertical_alignment)) = self.interior {
                    commands.push(Command::UpdateTextBoxInterior {
                        object_id: id.clone(),
                        margins,
                        vertical_alignment,
                    });
                }
                if self.text_outline_supported {
                    effects.text_outline = self.text_outline;
                }
                if self.fill.is_some() {
                    skipped.insert("Shape fill on Text Box".into());
                }
                if self.border.is_some() {
                    skipped.insert("Picture or Shape border on Text Box".into());
                }
                if self.table_grid.is_some() {
                    skipped.insert("Table grid on Text Box".into());
                }
            }
            ObjectKind::Table { table } => {
                if let Some(source) = &self.text {
                    commands.push(Command::UpdateTableTextStyle {
                        object_id: id.clone(),
                        style: self.target_text(&table.style, source),
                        font_face_update: if font_is_mixed(&self.mixed) {
                            FontFaceBindingUpdate::PreserveIfFamilyUnchanged
                        } else {
                            FontFaceBindingUpdate::Replace(source.font_face.clone())
                        },
                    });
                    if !font_is_mixed(&self.mixed) {
                        font_commands(id, source, commands);
                    }
                }
                if let Some(border) = self.table_grid {
                    commands.push(Command::UpdateTableGridBorder {
                        object_id: id.clone(),
                        border,
                    });
                }
                self.skip_non_table(skipped);
            }
            ObjectKind::Image { .. } => {
                if let Some(border) = self.border {
                    effects.picture_border = if border.width_millipoints == 0 {
                        None
                    } else {
                        Some(border)
                    };
                }
                if self.fill.is_some() {
                    skipped.insert("Shape fill on Picture".into());
                }
                self.skip_text(skipped);
                if self.table_grid.is_some() {
                    skipped.insert("Table grid on Picture".into());
                }
            }
            ObjectKind::Shape {
                shape,
                fill,
                stroke,
                ..
            } => {
                if self.fill.is_some() || self.border.is_some() {
                    commands.push(Command::UpdateShape {
                        object_id: id.clone(),
                        shape: *shape,
                        fill: self.fill.unwrap_or(*fill),
                        stroke: self.border.unwrap_or(*stroke),
                    });
                }
                self.skip_text(skipped);
                if self.table_grid.is_some() {
                    skipped.insert("Table grid on Shape".into());
                }
            }
        }
        if let Some(rounding) = self.corners {
            if supports_corners(&target.kind) {
                commands.push(Command::UpdateCornerRounding {
                    object_id: id.clone(),
                    rounding,
                });
            } else {
                skipped.insert("corners on this Shape kind".into());
            }
        }
        commands.push(Command::UpdateObjectEffects {
            object_id: id.clone(),
            effects,
        });
        Ok(())
    }
    fn target_text(&self, target: &TextStyle, source: &TextStyle) -> TextStyle {
        let mut updated = target.clone();
        if !font_is_mixed(&self.mixed) {
            updated.font_family.clone_from(&source.font_family);
            updated.font_weight = source.font_weight;
            updated.italic = source.italic;
        }
        if !self.mixed.contains("font size") {
            updated.font_size_millipoints = source.font_size_millipoints;
        }
        if !self.mixed.contains("text color") {
            updated.color = source.color;
        }
        if !self.mixed.contains("underline") {
            updated.underline = source.underline;
        }
        if !self.mixed.contains("alignment") {
            updated.alignment = source.alignment;
        }
        if !self.mixed.contains("line spacing") {
            updated.line_height_per_thousand = source.line_height_per_thousand;
        }
        updated
    }
    fn skip_text(&self, skipped: &mut BTreeSet<String>) {
        if self.text.is_some() {
            skipped.insert("text formatting on Picture or Shape".into());
        }
        if self.paragraph.is_some() {
            skipped.insert("paragraph formatting on Picture or Shape".into());
        }
        if self.interior.is_some() {
            skipped.insert("Text Box interior on Picture or Shape".into());
        }
        if self.text_outline_supported {
            skipped.insert("text outline on Picture or Shape".into());
        }
    }
    fn skip_non_table(&self, skipped: &mut BTreeSet<String>) {
        if self.paragraph.is_some() {
            skipped.insert("paragraph formatting on Table".into());
        }
        if self.interior.is_some() {
            skipped.insert("Text Box interior on Table".into());
        }
        if self.text_outline_supported {
            skipped.insert("text outline on Table".into());
        }
        if self.fill.is_some() {
            skipped.insert("Shape fill on Table".into());
        }
        if self.border.is_some() {
            skipped.insert("Picture or Shape border on Table".into());
        }
    }
}
fn supports_corners(kind: &ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Image { .. }
            | ObjectKind::Text { .. }
            | ObjectKind::Table { .. }
            | ObjectKind::Shape {
                shape: ShapeKind::Rectangle,
                ..
            }
    )
}
fn font_commands(id: &document_core::ObjectId, style: &TextStyle, commands: &mut Vec<Command>) {
    for variant in &style.exact_font_variants {
        commands.push(Command::SetTextFontVariantBinding {
            object_id: id.clone(),
            font_weight: variant.font_weight,
            italic: variant.italic,
            font_face: Some(variant.face.clone()),
        });
    }
    for family in &style.exact_font_families {
        commands.push(Command::SetTextFontFamilyBinding {
            object_id: id.clone(),
            font_family: family.font_family.clone(),
            font_weight: family.font_weight,
            italic: family.italic,
            font_face: family.face.clone(),
        });
    }
}
fn style_targets(editor: &EditorModule, ids: &[String]) -> EditorResult<Vec<PublicationObject>> {
    if ids.is_empty() || ids.len() > 500 {
        return Err(invalid_input(
            "Select between one and 500 objects to paste Style.",
        ));
    }
    let ids_set = ids
        .iter()
        .cloned()
        .map(parse_object_id)
        .collect::<EditorResult<BTreeSet<_>>>()?;
    if ids_set.len() != ids.len() {
        return Err(invalid_input("Select each Style target only once."));
    }
    let targets = editor
        .session
        .as_ref()
        .ok_or_else(no_document)?
        .document()
        .authored_objects()
        .filter(|object| ids_set.contains(&object.id))
        .cloned()
        .collect::<Vec<_>>();
    if targets.len() != ids.len() {
        return Err(invalid_input("A Style target no longer exists."));
    }
    if targets.iter().any(|target| target.locked) {
        return Err(invalid_input(
            "Unlock every selected object before pasting Style.",
        ));
    }
    Ok(targets)
}
fn validate_rendered_resources(
    objects: &[PublicationObject],
    assets: &AssetStore,
) -> EditorResult<()> {
    for (face, _, _) in rendered_faces(objects)? {
        let asset=assets.get(&face.source).ok_or_else(||font_unavailable("The target's mixed formatting needs an unavailable exact face. Embed the face or choose compatible formatting first."))?;
        if AssetHash::for_bytes(&asset.bytes) != face.source
            || ttf_parser::Face::parse(&asset.bytes, face.face_index).is_err()
        {
            return Err(font_unavailable(
                "The target's exact font resource is invalid.",
            ));
        }
    }
    Ok(())
}
fn preserve_fit_geometry(
    editor: &EditorModule,
    targets: &[PublicationObject],
    after: &ProjectSnapshot,
    assets: &AssetStore,
) -> EditorResult<()> {
    let before = ProjectSnapshot {
        document: editor
            .session
            .as_ref()
            .ok_or_else(no_document)?
            .document()
            .clone(),
        revision: Revision::new(after.revision.value() - 1),
    };
    for target in targets {
        let ObjectKind::Text { frame, .. } = &target.kind else {
            continue;
        };
        if frame.sizing_mode != TextBoxSizingMode::Fit {
            continue;
        }
        let prospective = after
            .document
            .authored_objects()
            .find(|object| object.id == target.id)
            .expect("rehearsal preserves target identity");
        // Paint-only changes cannot alter Fit composition; missing unrelated
        // machine fonts need not block their independent opacity/shadow edits.
        if target.kind == prospective.kind && target.corner_rounding == prospective.corner_rounding
        {
            continue;
        }
        let mut bindings = BTreeSet::new();
        for project in [&before, after] {
            let object = project
                .document
                .authored_objects()
                .find(|object| object.id == target.id)
                .expect("rehearsal preserves target identity");
            if let ObjectKind::Text { style, .. } = &object.kind {
                bindings.extend(super::required_fit_font_bindings(project, style));
            }
        }
        if bindings.len() > super::MAX_TEXT_BOX_FONT_RESOURCES {
            return Err(font_unavailable(
                "Fit Style comparison exceeds the font resource budget.",
            ));
        }
        let mut bytes = 0_usize;
        let mut resources = Vec::new();
        for (source, face_index) in bindings {
            let asset = assets.get(&source).ok_or_else(|| {
                font_unavailable("Embed the Fit Text Box fonts before pasting Style.")
            })?;
            bytes = bytes.checked_add(asset.bytes.len()).ok_or_else(|| {
                font_unavailable("Fit Style comparison exceeds the font resource budget.")
            })?;
            if bytes > super::MAX_TEXT_BOX_FONT_BYTES_TOTAL {
                return Err(font_unavailable(
                    "Fit Style comparison exceeds the font resource budget.",
                ));
            }
            resources.push(ExactFontResource {
                source,
                face_index,
                bytes: asset.bytes.clone(),
            });
        }
        let resolve = |project| {
            ResolvedPublication::resolve_fit_text_box(project, &resources, &target.id).map_err(
                |_| font_unavailable("The Fit Text Box cannot be composed with its exact fonts."),
            )
        };
        if resolve(&before)?.size != resolve(after)?.size {
            return Err(invalid_input(
                "Paste Style would resize a Fit Text Box. Switch that target to Fixed box and paste again.",
            ));
        }
    }
    Ok(())
}
fn compare_text(first: &TextStyle, next: &TextStyle, mixed: &mut BTreeSet<String>) {
    macro_rules! compare {
        ($field:ident,$label:literal) => {
            if first.$field != next.$field {
                mixed.insert($label.into());
            }
        };
    }
    compare!(font_family, "font family");
    compare!(font_weight, "font weight");
    compare!(italic, "font slant");
    compare!(font_size_millipoints, "font size");
    compare!(color, "text color");
    compare!(underline, "underline");
    compare!(alignment, "alignment");
    compare!(line_height_per_thousand, "line spacing");
}
fn effective_inline(base: &TextStyle, patch: &InlineTextStyle) -> TextStyle {
    let mut result = base.clone();
    if let Some(family) = &patch.font_family {
        result.font_family.clone_from(family);
    }
    result.font_weight =
        patch
            .bold
            .map_or(patch.font_weight.unwrap_or(result.font_weight), |bold| {
                if bold { 700 } else { 400 }
            });
    result.italic = patch.italic.unwrap_or(result.italic);
    result.font_size_millipoints = patch
        .font_size_millipoints
        .unwrap_or(result.font_size_millipoints);
    result.color = patch.color.unwrap_or(result.color);
    result.underline = patch.underline.unwrap_or(result.underline);
    result.font_face = base
        .exact_font_face_for_family(&result.font_family, result.font_weight, result.italic)
        .cloned();
    result
}
fn uniform_text_style(
    text: &str,
    base: &TextStyle,
    story: &TextStoryFormatting,
    mixed: &mut BTreeSet<String>,
) -> TextStyle {
    let mut first = None;
    let mut position = 0;
    for span in &story.inline_spans {
        if span.range.start > position {
            if let Some(first) = &first {
                compare_text(first, base, mixed);
            } else {
                first = Some(base.clone());
            }
        }
        let style = effective_inline(base, &span.style);
        if let Some(first) = &first {
            compare_text(first, &style, mixed);
        } else {
            first = Some(style);
        }
        position = span.range.end;
    }
    if position < text.len() {
        if let Some(first) = &first {
            compare_text(first, base, mixed);
        } else {
            first = Some(base.clone());
        }
    }
    let mut style = first.unwrap_or_else(|| base.clone());
    rebuild_variants(&mut style, base);
    style
}
fn rebuild_variants(style: &mut TextStyle, base: &TextStyle) {
    let mut variants = Vec::new();
    if style.font_family == base.font_family {
        if let Some(face) = &base.font_face {
            variants.push(ExactFontVariantBinding {
                font_weight: base.font_weight,
                italic: base.italic,
                face: face.clone(),
            });
        }
        variants.extend(base.exact_font_variants.iter().cloned());
    }
    variants.extend(
        base.exact_font_families
            .iter()
            .filter(|entry| entry.font_family == style.font_family)
            .map(|entry| ExactFontVariantBinding {
                font_weight: entry.font_weight,
                italic: entry.italic,
                face: entry.face.clone(),
            }),
    );
    variants.retain(|entry| (entry.font_weight, entry.italic) != (style.font_weight, style.italic));
    variants.sort_by_key(|entry| (entry.font_weight, entry.italic));
    variants.dedup_by_key(|entry| (entry.font_weight, entry.italic));
    // Keep the canonical eight-variant ceiling. Additional exact keys remain
    // in the canonical family registry rather than truncating transferable faces.
    style.exact_font_families = if variants.len() > MAX_EXACT_FONT_VARIANTS {
        variants
            .split_off(MAX_EXACT_FONT_VARIANTS)
            .into_iter()
            .map(|entry| ExactFontFamilyBinding {
                font_family: style.font_family.clone(),
                font_weight: entry.font_weight,
                italic: entry.italic,
                face: entry.face,
            })
            .collect()
    } else {
        Vec::new()
    };
    style.exact_font_variants = variants;
}
fn uniform_paragraph(
    text: &str,
    base: &TextStyle,
    story: &TextStoryFormatting,
    mixed: &mut BTreeSet<String>,
) -> EditorResult<ParagraphStyle> {
    let mut first: Option<ParagraphStyle> = None;
    let mut result = ParagraphStyle::default();
    for index in 0..paragraph_count(text).map_err(invalid_input)? {
        let source = story
            .paragraph_runs
            .iter()
            .find(|run| run.range.start <= index && index < run.range.end)
            .map_or_else(ParagraphStyle::default, |run| run.style);
        let current = ParagraphStyle {
            list_style: Some(source.list_style.unwrap_or(ParagraphListStyle::None)),
            alignment: Some(source.alignment.unwrap_or(base.alignment)),
            start_indent_um: Some(source.start_indent_um.unwrap_or(0)),
            end_indent_um: Some(source.end_indent_um.unwrap_or(0)),
            first_line_indent_um: Some(source.first_line_indent_um.unwrap_or(0)),
            line_height_per_thousand: Some(
                source
                    .line_height_per_thousand
                    .unwrap_or(base.line_height_per_thousand),
            ),
            space_before_millipoints: Some(source.space_before_millipoints.unwrap_or(0)),
            space_after_millipoints: Some(source.space_after_millipoints.unwrap_or(0)),
        };
        if let Some(first) = first {
            macro_rules! compare {
                ($field:ident,$label:literal) => {
                    if first.$field != current.$field {
                        result.$field = None;
                        mixed.insert($label.into());
                    }
                };
            }
            compare!(list_style, "list formatting");
            compare!(alignment, "alignment");
            compare!(start_indent_um, "start indent");
            compare!(end_indent_um, "end indent");
            compare!(first_line_indent_um, "first line indent");
            compare!(line_height_per_thousand, "line spacing");
            compare!(space_before_millipoints, "space before paragraph");
            compare!(space_after_millipoints, "space after paragraph");
        } else {
            first = Some(current);
            result = current;
        }
    }
    Ok(result)
}
