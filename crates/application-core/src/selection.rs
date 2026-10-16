//! Revision-bound selection geometry and atomic page-space transforms.

use super::{
    ArrangeGeometryOperationInput, EditorError, EditorErrorCode, EditorModule, EditorResult,
    EditorSnapshot, MAX_TEXT_BOX_FONT_BYTES_TOTAL, MAX_TEXT_BOX_FONT_RESOURCES, command_rejected,
    no_document, parse_object_id, physical_size, required_fit_font_bindings, validate_coordinate,
};
use document_core::commands::{
    ArrangeGeometryOperation, Command, DocumentSession, ObjectLocation, ObjectTransformUpdate,
    Transaction, selected_member_ids, selected_object_ids,
};
use document_core::geometry::{
    PhysicalBounds, resize_transform_in_bounds, rotate_transform_in_bounds, transformed_bounds,
    transformed_point_um, union_bounds,
};
use document_core::model::{
    Document, Micrometres, ObjectId, ObjectKind, PhysicalSize, PublicationObject, Revision,
    ShapeKind, TextBoxSizingMode, Transform,
};
use document_core::package::ProjectSnapshot;
use layout_core::{ExactFontResource, PublicationLayoutError, ResolvedPublication};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionBounds {
    pub x_um: i64,
    pub y_um: i64,
    pub width_um: i64,
    pub height_um: i64,
}

impl From<PhysicalBounds> for SelectionBounds {
    fn from(value: PhysicalBounds) -> Self {
        Self {
            x_um: value.x.value(),
            y_um: value.y.value(),
            width_um: value.width.value(),
            height_um: value.height.value(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionGeometry {
    pub revision: u64,
    pub object_ids: Vec<String>,
    pub bounds: SelectionBounds,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResizeSelectionInput {
    pub expected_revision: u64,
    pub object_ids: Vec<String>,
    #[serde(default)]
    pub scope: SelectionScopeInput,
    pub x_um: i64,
    pub y_um: i64,
    pub width_um: i64,
    pub height_um: i64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotateSelectionInput {
    pub expected_revision: u64,
    pub object_ids: Vec<String>,
    #[serde(default)]
    pub scope: SelectionScopeInput,
    pub delta_microdegrees: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum SelectionScopeInput {
    #[default]
    Groups,
    Members,
}

struct SelectionContext {
    ids: Vec<ObjectId>,
    sizes: BTreeMap<ObjectId, PhysicalSize>,
    bounds: PhysicalBounds,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SelectionDestinationInput {
    #[default]
    Current,
    Pasteboard,
    Page {
        page_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetLineEndpointsInput {
    pub expected_revision: u64,
    pub object_id: String,
    pub start_x_um: i64,
    pub start_y_um: i64,
    pub end_x_um: i64,
    pub end_y_um: i64,
}

impl EditorModule {
    /// Sets a Line's two physical endpoints as one admitted, reversible gesture.
    /// Intrinsic geometry becomes a positive-width, one-micrometre-high segment;
    /// pivot, scale and shear reset while style, effects and identity remain.
    ///
    /// # Errors
    /// Rejects stale, locked or non-Line targets, collapsed/over-bound endpoints
    /// and transforms whose actual endpoint displacement exceeds three micrometres.
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "Input coordinates and length are bounded before deliberate float trigonometry; shared endpoint evaluation verifies the rounded transform"
    )]
    #[allow(clippy::too_many_lines)] // One atomic endpoint gesture validates and proves the normalized transform.
    pub fn set_line_endpoints(
        &mut self,
        input: SetLineEndpointsInput,
    ) -> EditorResult<EditorSnapshot> {
        for coordinate in [
            input.start_x_um,
            input.start_y_um,
            input.end_x_um,
            input.end_y_um,
        ] {
            validate_coordinate(coordinate)?;
        }
        let project = self.selection_project(input.expected_revision)?;
        let object_id = parse_object_id(input.object_id)?;
        let located = project
            .document
            .pages
            .iter()
            .find_map(|page| {
                page.objects
                    .iter()
                    .find(|object| object.id == object_id)
                    .map(|object| (ObjectLocation::Page(page.id.clone()), object))
            })
            .or_else(|| {
                project.document.masters.iter().find_map(|master| {
                    master
                        .objects
                        .iter()
                        .find(|object| object.id == object_id)
                        .map(|object| (ObjectLocation::Master(master.id.clone()), object))
                })
            })
            .or_else(|| {
                project
                    .document
                    .pasteboard
                    .objects
                    .iter()
                    .find(|object| object.id == object_id)
                    .map(|object| (ObjectLocation::Pasteboard, object))
            });
        let (destination, object) = located.ok_or_else(|| {
            EditorError::new(
                EditorErrorCode::InvalidInput,
                "That Line is no longer available",
            )
        })?;
        if !matches!(
            object.kind,
            ObjectKind::Shape {
                shape: ShapeKind::Line,
                ..
            }
        ) {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "Endpoint editing applies to Lines",
            ));
        }
        if object.locked {
            return Err(EditorError::new(
                EditorErrorCode::CommandRejected,
                "Unlock the Line before changing its endpoints",
            ));
        }
        let dx = (input.end_x_um - input.start_x_um) as f64;
        let dy = (input.end_y_um - input.start_y_um) as f64;
        let length = dx.hypot(dy);
        if length < 1.0 || length > super::MAX_DIMENSION_UM as f64 {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "Choose two different endpoints within the supported Line length",
            ));
        }
        let size = physical_size(length.round() as i64, 1)?;
        let angle = ((dy.atan2(dx) - 1.0_f64.atan2(size.width.value() as f64)).to_degrees()
            * 1_000_000.0)
            .round() as i64;
        let transform = Transform {
            rotation_microdegrees: (angle + 180_000_000).rem_euclid(360_000_000) - 180_000_000,
            ..Transform::identity_at(input.start_x_um, input.start_y_um)
        };
        let actual = transformed_point_um(transform, size.width.value(), size.height.value())
            .map_err(geometry_failed)?;
        if (actual.0 - input.end_x_um as f64).abs() > 3.0
            || (actual.1 - input.end_y_um as f64).abs() > 3.0
        {
            return Err(geometry_failed(
                document_core::geometry::TransformBoundsError::NonRepresentableResize,
            ));
        }
        self.execute_saveable(
            project.revision,
            Transaction {
                label: "Change Line endpoints".into(),
                commands: vec![
                    Command::ResizeObject {
                        object_id: object_id.clone(),
                        size,
                    },
                    Command::MoveObject {
                        object_id,
                        destination,
                        transform,
                    },
                ],
            },
            std::iter::empty(),
        )?;
        self.snapshot()
    }
    /// Returns canonical transformed bounds, including native Fit text size.
    ///
    /// # Errors
    /// Rejects stale, mixed-container or excessive selections and unavailable layout.
    pub fn selection_geometry(
        &self,
        expected_revision: u64,
        object_ids: &[String],
    ) -> EditorResult<SelectionGeometry> {
        self.selection_geometry_scoped(expected_revision, object_ids, SelectionScopeInput::Groups)
    }

    /// Returns member bounds for an explicit Edit Group context.
    ///
    /// # Errors
    /// Refuses stale, missing, repeated, mixed-container or excessive selections.
    pub fn selection_geometry_scoped(
        &self,
        expected_revision: u64,
        object_ids: &[String],
        scope: SelectionScopeInput,
    ) -> EditorResult<SelectionGeometry> {
        let project = self.selection_project(expected_revision)?;
        let context = self.selection_context(&project, &parse_ids(object_ids.to_vec())?, scope)?;
        Ok(SelectionGeometry {
            revision: expected_revision,
            object_ids: context
                .ids
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect(),
            bounds: context.bounds.into(),
        })
    }

    /// Reflows one ungrouped Fixed Text Box by resizing its physical frame.
    /// Other selections stretch in page axes, retaining intrinsic size and contents.
    ///
    /// # Errors
    /// Refuses stale revision, unavailable Fit layout or unsafe transform decomposition.
    pub fn resize_selection(
        &mut self,
        input: ResizeSelectionInput,
    ) -> EditorResult<EditorSnapshot> {
        validate_coordinate(input.x_um)?;
        validate_coordinate(input.y_um)?;
        let size = physical_size(input.width_um, input.height_um)?;
        let target = PhysicalBounds {
            x: Micrometres::new(input.x_um),
            y: Micrometres::new(input.y_um),
            width: size.width,
            height: size.height,
        };
        let project = self.selection_project(input.expected_revision)?;
        let context =
            self.selection_context(&project, &parse_ids(input.object_ids)?, input.scope)?;
        if context.ids.len() == 1 {
            let object = find_object(&project.document, &context.ids[0]);
            if object.group_id.is_none()
                && matches!(&object.kind, ObjectKind::Text { frame, .. } if frame.sizing_mode == TextBoxSizingMode::Fixed)
            {
                let (size, transform) = fixed_text_frame_resize(object, context.bounds, target)?;
                return self.commit_fixed_text_resize(
                    project.revision,
                    object.id.clone(),
                    size,
                    transform,
                );
            }
        }
        let objects = context
            .ids
            .iter()
            .map(|id| {
                let object = find_object(&project.document, id);
                let transform = resize_transform_in_bounds(
                    object.transform,
                    context.sizes[id],
                    context.bounds,
                    target,
                )
                .map_err(geometry_failed)?;
                validate_coordinate(transform.translation_x.value())?;
                validate_coordinate(transform.translation_y.value())?;
                Ok(ObjectTransformUpdate {
                    object_id: id.clone(),
                    transform,
                })
            })
            .collect::<EditorResult<Vec<_>>>()?;
        let actual = union_bounds(
            &objects
                .iter()
                .map(|update| {
                    transformed_bounds(context.sizes[&update.object_id], update.transform)
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(geometry_failed)?,
        )
        .map_err(geometry_failed)?;
        if [
            actual.x.value() - target.x.value(),
            actual.y.value() - target.y.value(),
            actual.width.value() - target.width.value(),
            actual.height.value() - target.height.value(),
        ]
        .iter()
        .any(|delta| delta.abs() > 3)
        {
            return Err(geometry_failed(
                document_core::geometry::TransformBoundsError::NonRepresentableResize,
            ));
        }
        self.commit_selection_transforms(
            input.expected_revision,
            "Resize selection",
            objects,
            input.scope,
        )
    }

    /// Rotates the selection rigidly about its native visible center.
    ///
    /// # Errors
    /// Refuses stale revision, unavailable Fit layout or unsafe rounded geometry.
    pub fn rotate_selection(
        &mut self,
        input: RotateSelectionInput,
    ) -> EditorResult<EditorSnapshot> {
        if !(-360_000_000..=360_000_000).contains(&input.delta_microdegrees) {
            return Err(EditorError::new(
                EditorErrorCode::InvalidInput,
                "Rotation is outside the supported range",
            ));
        }
        let project = self.selection_project(input.expected_revision)?;
        let context =
            self.selection_context(&project, &parse_ids(input.object_ids)?, input.scope)?;
        let objects = context
            .ids
            .iter()
            .map(|id| {
                let object = find_object(&project.document, id);
                let transform = rotate_transform_in_bounds(
                    object.transform,
                    context.sizes[id],
                    context.bounds,
                    input.delta_microdegrees,
                )
                .map_err(geometry_failed)?;
                validate_coordinate(transform.translation_x.value())?;
                validate_coordinate(transform.translation_y.value())?;
                Ok(ObjectTransformUpdate {
                    object_id: id.clone(),
                    transform,
                })
            })
            .collect::<EditorResult<Vec<_>>>()?;
        self.commit_selection_transforms(
            input.expected_revision,
            "Rotate selection",
            objects,
            input.scope,
        )
    }

    fn commit_selection_transforms(
        &mut self,
        revision: u64,
        label: &str,
        objects: Vec<ObjectTransformUpdate>,
        scope: SelectionScopeInput,
    ) -> EditorResult<EditorSnapshot> {
        let command = match scope {
            SelectionScopeInput::Groups => Command::ApplySelectionTransforms { objects },
            SelectionScopeInput::Members => Command::ApplyMemberTransforms { objects },
        };
        self.execute_saveable(
            Revision::new(revision),
            Transaction::single(label, command),
            std::iter::empty(),
        )?;
        self.snapshot()
    }

    fn commit_fixed_text_resize(
        &mut self,
        revision: Revision,
        object_id: ObjectId,
        size: PhysicalSize,
        transform: Transform,
    ) -> EditorResult<EditorSnapshot> {
        // Existing canonical admission checks locks, columns, margins,
        // rounded interiors and saveability before changing the session.
        self.execute_saveable(
            revision,
            Transaction {
                label: "Resize Text Box".into(),
                commands: vec![
                    Command::ResizeObject {
                        object_id: object_id.clone(),
                        size,
                    },
                    Command::ApplySelectionTransforms {
                        objects: vec![ObjectTransformUpdate {
                            object_id,
                            transform,
                        }],
                    },
                ],
            },
            std::iter::empty(),
        )?;
        self.snapshot()
    }

    pub(super) fn selection_project(
        &self,
        expected_revision: u64,
    ) -> EditorResult<ProjectSnapshot> {
        let session = self.session.as_ref().ok_or_else(no_document)?;
        if session.revision().value() != expected_revision {
            return Err(EditorError::new(
                EditorErrorCode::StaleRevision,
                "The publication changed before the selection could be updated",
            ));
        }
        Ok(ProjectSnapshot {
            document: session.document().clone(),
            revision: session.revision(),
        })
    }

    fn selection_context(
        &self,
        project: &ProjectSnapshot,
        requested_ids: &[ObjectId],
        scope: SelectionScopeInput,
    ) -> EditorResult<SelectionContext> {
        let ids = match scope {
            SelectionScopeInput::Groups => selected_object_ids(&project.document, requested_ids),
            SelectionScopeInput::Members => selected_member_ids(&project.document, requested_ids),
        }
        .map_err(command_rejected)?;
        let mut bindings = BTreeSet::new();
        for id in &ids {
            if let ObjectKind::Text { style, frame, .. } = &find_object(&project.document, id).kind
                && frame.sizing_mode == TextBoxSizingMode::Fit
            {
                bindings.extend(required_fit_font_bindings(project, style));
            }
        }
        let referenced = self
            .referenced_font_faces(
                &bindings,
                MAX_TEXT_BOX_FONT_RESOURCES,
                MAX_TEXT_BOX_FONT_BYTES_TOTAL,
            )
            .map_err(|_| {
                EditorError::new(
                    EditorErrorCode::FontUnavailable,
                    "A selected Text Box font is unavailable; the selection was not changed",
                )
            })?;
        if referenced.budget_exceeded {
            return Err(EditorError::new(
                EditorErrorCode::LayoutFailed,
                "The selection is too complex to resize safely",
            ));
        }
        let resources = referenced
            .faces
            .into_iter()
            .map(|resource| ExactFontResource {
                source: resource.source,
                face_index: resource.face_index,
                bytes: resource.bytes,
            })
            .collect::<Vec<_>>();
        let mut sizes = BTreeMap::new();
        let mut bounds = Vec::with_capacity(ids.len());
        for id in &ids {
            let object = find_object(&project.document, id);
            let size = if matches!(&object.kind, ObjectKind::Text { frame, .. } if frame.sizing_mode == TextBoxSizingMode::Fit)
            {
                ResolvedPublication::resolve_fit_text_box(project, &resources, id).map_err(|error| EditorError::new(if error == PublicationLayoutError::ExactFontUnavailable { EditorErrorCode::FontUnavailable } else { EditorErrorCode::LayoutFailed }, "A selected Text Box could not be composed; the selection was not changed"))?.size
            } else {
                object.size
            };
            sizes.insert(id.clone(), size);
            bounds.push(transformed_bounds(size, object.transform).map_err(geometry_failed)?);
        }
        Ok(SelectionContext {
            ids,
            sizes,
            bounds: union_bounds(&bounds).map_err(geometry_failed)?,
        })
    }

    // Plan Arrange on a disposable document with resolved Fit dimensions, then
    // commit only transforms to the real document. Prior commands retain order.
    pub(super) fn prepare_arrange_geometry(
        &self,
        revision: Revision,
        transaction: &mut Transaction,
    ) -> EditorResult<()> {
        if !transaction.commands.iter().any(|command| {
            matches!(
                command,
                Command::ArrangeGeometry { .. }
                    | Command::ArrangeGeometryToPage { .. }
                    | Command::MoveSelection { .. }
            )
        }) {
            return Ok(());
        }
        let project = self.selection_project(revision.value())?;
        let mut scratch =
            DocumentSession::from_saved(project.document, revision).map_err(command_rejected)?;
        for command in &mut transaction.commands {
            if let Command::MoveSelection {
                object_ids,
                delta_x_um,
                delta_y_um,
                ..
            } = command
            {
                validate_selection_move(scratch.document(), object_ids, *delta_x_um, *delta_y_um)?;
            }
            if let Command::ArrangeGeometry { object_ids, .. }
            | Command::ArrangeGeometryToPage { object_ids, .. } = command
            {
                let mut fitted = ProjectSnapshot {
                    document: scratch.document().clone(),
                    revision: scratch.revision(),
                };
                let context =
                    self.selection_context(&fitted, object_ids, SelectionScopeInput::Groups)?;
                for object in fitted.document.authored_objects_mut() {
                    if let Some(size) = context.sizes.get(&object.id) {
                        object.size = *size;
                    }
                }
                let mut geometry = DocumentSession::from_saved(fitted.document, fitted.revision)
                    .map_err(command_rejected)?;
                geometry
                    .execute(
                        fitted.revision,
                        Transaction::single("Resolve Arrange geometry", command.clone()),
                    )
                    .map_err(command_rejected)?;
                *command = Command::ApplySelectionTransforms {
                    objects: context
                        .ids
                        .into_iter()
                        .map(|id| ObjectTransformUpdate {
                            transform: find_object(geometry.document(), &id).transform,
                            object_id: id,
                        })
                        .collect(),
                };
            }
            scratch
                .execute(
                    scratch.revision(),
                    Transaction::single("Prepare Arrange transaction", command.clone()),
                )
                .map_err(command_rejected)?;
        }
        Ok(())
    }
}

fn find_object<'a>(document: &'a Document, id: &ObjectId) -> &'a PublicationObject {
    document
        .authored_objects()
        .find(|object| &object.id == id)
        .expect("normalized selection member remains present")
}

fn validate_selection_move(
    document: &Document,
    ids: &[ObjectId],
    delta_x: i64,
    delta_y: i64,
) -> EditorResult<()> {
    for id in selected_object_ids(document, ids).map_err(command_rejected)? {
        let object = find_object(document, &id);
        for (position, delta) in [
            (object.transform.translation_x.value(), delta_x),
            (object.transform.translation_y.value(), delta_y),
        ] {
            validate_coordinate(position.checked_add(delta).ok_or_else(|| {
                EditorError::new(
                    EditorErrorCode::InvalidInput,
                    "The selection position is outside the supported range",
                )
            })?)?;
        }
    }
    Ok(())
}

fn parse_ids(ids: Vec<String>) -> EditorResult<Vec<ObjectId>> {
    ids.into_iter().map(parse_object_id).collect()
}
fn geometry_failed(error: document_core::geometry::TransformBoundsError) -> EditorError {
    use document_core::geometry::TransformBoundsError;
    let message = match error {
        TransformBoundsError::NonRepresentableResize => error.to_string(),
        TransformBoundsError::NonInvertible => "This selection has a collapsed transform. Reset the object transforms before resizing.".to_owned(),
        TransformBoundsError::NonFinite | TransformBoundsError::OutOfRange => "The requested selection geometry exceeds the supported range. Use a smaller size or reset the object transforms.".to_owned(),
    };
    EditorError::new(EditorErrorCode::InvalidInput, message)
}

// Absorb scale into the Fixed frame, leaving its canonical font size and
// physical interior available to reflow. R*H*sign(S) acting on |S|*point keeps
// the same frame corners; T + oldPivot - newPivot keeps their world origin.
#[expect(clippy::cast_precision_loss, reason = "Bounded physical coordinates are evaluated by the existing affine engine, with every final corner checked within3um")]
fn fixed_text_frame_resize(
    object: &PublicationObject,
    source: PhysicalBounds,
    target: PhysicalBounds,
) -> EditorResult<(PhysicalSize, Transform)> {
    use document_core::geometry::TransformBoundsError;
    let resized = resize_transform_in_bounds(object.transform, object.size, source, target)
        .map_err(geometry_failed)?;
    let scaled = |value: i64, scale: i64| -> EditorResult<i64> {
        let product = i128::from(value) * i128::from(scale).abs();
        let rounded = (product + product.signum() * i128::from(Transform::IDENTITY_SCALE / 2))
            / i128::from(Transform::IDENTITY_SCALE);
        i64::try_from(rounded).map_err(|_| geometry_failed(TransformBoundsError::OutOfRange))
    };
    let size = physical_size(
        scaled(object.size.width.value(), resized.scale_x_nanos)?,
        scaled(object.size.height.value(), resized.scale_y_nanos)?,
    )?;
    let pivot_x = scaled(resized.pivot_x.value(), resized.scale_x_nanos)?;
    let pivot_y = scaled(resized.pivot_y.value(), resized.scale_y_nanos)?;
    let translation = |position: i64, old: i64, new: i64| -> EditorResult<Micrometres> {
        let value = i64::try_from(i128::from(position) + i128::from(old) - i128::from(new))
            .map_err(|_| geometry_failed(TransformBoundsError::OutOfRange))?;
        validate_coordinate(value)?;
        Ok(Micrometres::new(value))
    };
    let transform = Transform {
        translation_x: translation(
            resized.translation_x.value(),
            resized.pivot_x.value(),
            pivot_x,
        )?,
        translation_y: translation(
            resized.translation_y.value(),
            resized.pivot_y.value(),
            pivot_y,
        )?,
        pivot_x: Micrometres::new(pivot_x),
        pivot_y: Micrometres::new(pivot_y),
        scale_x_nanos: resized.scale_x_nanos.signum() * Transform::IDENTITY_SCALE,
        scale_y_nanos: resized.scale_y_nanos.signum() * Transform::IDENTITY_SCALE,
        ..resized
    };
    let actual = transformed_bounds(size, transform).map_err(geometry_failed)?;
    if [
        actual.x.value() - target.x.value(),
        actual.y.value() - target.y.value(),
        actual.width.value() - target.width.value(),
        actual.height.value() - target.height.value(),
    ]
    .iter()
    .any(|delta| delta.abs() > 3)
    {
        return Err(geometry_failed(
            TransformBoundsError::NonRepresentableResize,
        ));
    }
    for (x, y, new_x, new_y) in [
        (0, 0, 0, 0),
        (object.size.width.value(), 0, size.width.value(), 0),
        (0, object.size.height.value(), 0, size.height.value()),
        (
            object.size.width.value(),
            object.size.height.value(),
            size.width.value(),
            size.height.value(),
        ),
    ] {
        let before = transformed_point_um(object.transform, x, y).map_err(geometry_failed)?;
        let after = transformed_point_um(transform, new_x, new_y).map_err(geometry_failed)?;
        let expected_x = target.x.value() as f64
            + (before.0 - source.x.value() as f64) * target.width.value() as f64
                / source.width.value() as f64;
        let expected_y = target.y.value() as f64
            + (before.1 - source.y.value() as f64) * target.height.value() as f64
                / source.height.value() as f64;
        if (after.0 - expected_x).abs() > 3.0 || (after.1 - expected_y).abs() > 3.0 {
            return Err(geometry_failed(
                TransformBoundsError::NonRepresentableResize,
            ));
        }
    }
    Ok((size, transform))
}

pub(super) const fn canonical_arrange_operation(
    operation: ArrangeGeometryOperationInput,
) -> ArrangeGeometryOperation {
    match operation {
        ArrangeGeometryOperationInput::AlignLeft => ArrangeGeometryOperation::AlignLeft,
        ArrangeGeometryOperationInput::AlignCenter => ArrangeGeometryOperation::AlignCenter,
        ArrangeGeometryOperationInput::AlignRight => ArrangeGeometryOperation::AlignRight,
        ArrangeGeometryOperationInput::AlignTop => ArrangeGeometryOperation::AlignTop,
        ArrangeGeometryOperationInput::AlignMiddle => ArrangeGeometryOperation::AlignMiddle,
        ArrangeGeometryOperationInput::AlignBottom => ArrangeGeometryOperation::AlignBottom,
        ArrangeGeometryOperationInput::DistributeHorizontally => {
            ArrangeGeometryOperation::DistributeHorizontally
        }
        ArrangeGeometryOperationInput::DistributeVertically => {
            ArrangeGeometryOperation::DistributeVertically
        }
    }
}
