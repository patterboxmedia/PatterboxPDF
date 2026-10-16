use application_core::{
    ApplyTransactionInput, ArrangeGeometryOperationInput, EditorCommandInput, EditorModule,
    EditorSnapshot, ResizeSelectionInput, RotateSelectionInput, SelectionScopeInput,
};
use document_core::model::{
    Color, FontFaceBinding, ImageContentKind, ImageCrop, ImageFlip, Insets, ObjectDraft,
    ObjectKind, PhysicalSize, ShapeKind, ShapeStroke, TableData, TextBoxSizingMode, TextFrame,
    TextStoryFormatting, TextStyle, Transform, WrapMode,
};
use document_core::{
    AssetStore, Command, Document, DocumentId, DocumentSession, ObjectLocation, ProjectSnapshot,
    Revision, Transaction, save_new_package,
};
use layout_core::{ExactFontResource, ResolvedPublication};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct TemporaryFolder(PathBuf);
impl TemporaryFolder {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "publisher-u14-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TemporaryFolder {
    fn drop(&mut self) {
        let _ignored = std::fs::remove_dir_all(&self.0);
    }
}

fn document() -> Document {
    Document::new(
        DocumentId::parse("selection-contract").unwrap(),
        "Composition",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::default(),
    )
    .unwrap()
}

#[test]
#[allow(clippy::too_many_lines)] // The explicit Edit Group journey covers admission and one-step history.
fn explicit_member_scope_preserves_siblings_and_default_group_safety() {
    let temp = TemporaryFolder::new();
    let mut session = DocumentSession::new(document()).unwrap();
    let location = ObjectLocation::Page(session.document().pages[0].id.clone());
    let added = session
        .execute(
            session.revision(),
            Transaction {
                label: "Add two members".into(),
                commands: vec![
                    Command::AddObject {
                        location: location.clone(),
                        object: shape(0),
                    },
                    Command::AddObject {
                        location,
                        object: shape(1),
                    },
                ],
            },
        )
        .unwrap();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Group",
                Command::GroupObjects {
                    object_ids: added.created_objects.clone(),
                },
            ),
        )
        .unwrap();
    let (mut editor, opened) = open_editor(
        &temp,
        &ProjectSnapshot::from_session(&session),
        &AssetStore::default(),
    );
    let ids = vec![added.created_objects[0].as_str().to_owned()];
    let revision = opened.project.revision.value();
    assert_eq!(
        editor
            .selection_geometry(revision, &ids)
            .unwrap()
            .object_ids
            .len(),
        2
    );
    let bounds = editor
        .selection_geometry_scoped(revision, &ids, SelectionScopeInput::Members)
        .unwrap();
    assert_eq!(bounds.object_ids, ids);
    let original = opened.project.document.pages[0].objects.clone();
    let resized = editor
        .resize_selection(ResizeSelectionInput {
            expected_revision: revision,
            object_ids: ids.clone(),
            scope: SelectionScopeInput::Members,
            x_um: bounds.bounds.x_um,
            y_um: bounds.bounds.y_um,
            width_um: bounds.bounds.width_um * 2,
            height_um: bounds.bounds.height_um,
        })
        .unwrap();
    assert_ne!(
        resized.project.document.pages[0].objects[0].transform,
        original[0].transform
    );
    assert_eq!(resized.project.document.pages[0].objects[1], original[1]);
    assert_eq!(
        resized.project.document.pages[0].objects[0].group_id,
        original[0].group_id
    );
    let rotated = editor
        .rotate_selection(RotateSelectionInput {
            expected_revision: resized.project.revision.value(),
            object_ids: ids.clone(),
            scope: SelectionScopeInput::Members,
            delta_microdegrees: 90_000_000,
        })
        .unwrap();
    assert_eq!(rotated.project.document.pages[0].objects[1], original[1]);
    let undone = editor.undo(rotated.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, resized.project.document);
    let before_invalid = editor.snapshot().unwrap();
    assert!(
        editor
            .selection_geometry_scoped(
                before_invalid.project.revision.value(),
                &[ids[0].clone(), ids[0].clone()],
                SelectionScopeInput::Members
            )
            .is_err()
    );
    assert!(
        editor
            .rotate_selection(RotateSelectionInput {
                expected_revision: revision,
                object_ids: ids.clone(),
                scope: SelectionScopeInput::Members,
                delta_microdegrees: 90_000_000,
            })
            .is_err()
    );
    assert_eq!(editor.snapshot().unwrap(), before_invalid);
    let locked = editor
        .apply(ApplyTransactionInput {
            expected_revision: before_invalid.project.revision.value(),
            label: "Lock member".into(),
            commands: vec![EditorCommandInput::SetObjectLock {
                object_ids: ids.clone(),
                locked: true,
            }],
        })
        .unwrap();
    assert!(
        editor
            .rotate_selection(RotateSelectionInput {
                expected_revision: locked.project.revision.value(),
                object_ids: ids.clone(),
                scope: SelectionScopeInput::Members,
                delta_microdegrees: 90_000_000,
            })
            .is_err()
    );
    assert_eq!(editor.snapshot().unwrap(), locked);
    let wire: ResizeSelectionInput = serde_json::from_value(serde_json::json!({
        "expectedRevision":revision,"objectIds":ids,"xUm":0,"yUm":0,"widthUm":1000,"heightUm":1000
    }))
    .unwrap();
    assert_eq!(wire.scope, SelectionScopeInput::Groups);
}
fn shape(index: usize) -> ObjectDraft {
    ObjectDraft {
        name: format!("Shape {index}"),
        size: PhysicalSize::new(20_000, 15_000).unwrap(),
        transform: Transform::identity_at(i64::try_from(index).unwrap() * 500, 25_000),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Shape {
            shape: ShapeKind::Rectangle,
            fill: Color {
                red: 30,
                green: 80,
                blue: 120,
                alpha: 255,
            },
            stroke: ShapeStroke::default(),
            wrap_mode: WrapMode::None,
        },
    }
}
fn open_editor(
    temp: &TemporaryFolder,
    project: &ProjectSnapshot,
    assets: &AssetStore,
) -> (EditorModule, EditorSnapshot) {
    let path = temp.0.join("source.publisher-v2");
    save_new_package(&path, project, assets).unwrap();
    let mut editor = EditorModule::new();
    let snapshot = editor.open(&path).unwrap();
    (editor, snapshot)
}
fn apply(
    editor: &mut EditorModule,
    revision: u64,
    commands: Vec<EditorCommandInput>,
) -> EditorSnapshot {
    editor
        .apply(ApplyTransactionInput {
            expected_revision: revision,
            label: "Arrange composition".into(),
            commands,
        })
        .unwrap()
}

#[allow(clippy::too_many_lines)] // One mixed fixture owns the exact resources and all four transformed object families.
fn mixed_project() -> (ProjectSnapshot, AssetStore, ExactFontResource) {
    let mut assets = AssetStore::default();
    let bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let font_hash = assets.insert("font/ttf", bytes.clone());
    let font = ExactFontResource {
        source: font_hash.clone(),
        face_index: 0,
        bytes,
    };
    let image = assets.insert(
        "image/png",
        include_bytes!("../../raster-boundary/tests/fixtures/valid-3x2.png").to_vec(),
    );
    let fit = ObjectDraft {
        name: "Fit caption".into(),
        size: PhysicalSize::new(6_350, 6_350).unwrap(),
        transform: Transform {
            rotation_microdegrees: 90_000_000,
            ..Transform::identity_at(110_000, 80_000)
        },
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Text {
            text: "A caption whose composed width is greater than its stored minimum.".into(),
            style: TextStyle {
                font_family: "DejaVu Sans".into(),
                font_face: Some(FontFaceBinding {
                    source: font_hash,
                    face_index: 0,
                }),
                ..TextStyle::default()
            },
            frame: TextFrame {
                sizing_mode: TextBoxSizingMode::Fit,
                ..TextFrame::default()
            },
            story: TextStoryFormatting::default(),
        },
    };
    let picture = ObjectDraft {
        name: "Picture".into(),
        size: PhysicalSize::new(35_000, 22_000).unwrap(),
        transform: Transform {
            rotation_microdegrees: -25_000_000,
            ..Transform::identity_at(30_000, 40_000)
        },
        opacity_millionths: 900_000,
        kind: ObjectKind::Image {
            asset: image,
            content_kind: ImageContentKind::ContinuousTone,
            crop: ImageCrop::default(),
            flip: ImageFlip::default(),
            lock_aspect_ratio: false,
            wrap_mode: WrapMode::None,
        },
    };
    let mut diagram = shape(0);
    diagram.transform = Transform {
        scale_x_nanos: -1_200_000_000,
        shear_x_nanos: 200_000_000,
        ..Transform::identity_at(95_000, 35_000)
    };
    let table = ObjectDraft {
        name: "Table".into(),
        size: PhysicalSize::new(45_000, 28_000).unwrap(),
        transform: Transform {
            rotation_microdegrees: 45_000_000,
            ..Transform::identity_at(35_000, 100_000)
        },
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Table {
            table: TableData::empty(2, 3).unwrap(),
        },
    };
    let mut session = DocumentSession::new(document()).unwrap();
    let page = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Composition".into(),
                commands: [fit, picture, diagram, table]
                    .into_iter()
                    .map(|object| Command::AddObject {
                        location: ObjectLocation::Page(page.clone()),
                        object,
                    })
                    .collect(),
            },
        )
        .unwrap();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Group",
                Command::GroupObjects {
                    object_ids: added.created_objects,
                },
            ),
        )
        .unwrap();
    (ProjectSnapshot::from_session(&session), assets, font)
}

#[test]
#[allow(clippy::too_many_lines)] // One real mixed composition exercises the whole geometry boundary.
fn mixed_rotated_group_uses_native_fit_bounds_and_preserves_contents_through_resize_undo_and_save()
{
    let temp = TemporaryFolder::new();
    let (project, assets, font) = mixed_project();
    let native = ResolvedPublication::resolve_with_font_resources(&project, &[font]).unwrap();
    let native_bounds = document_core::geometry::union_bounds(
        &native.pages()[0]
            .objects
            .iter()
            .map(|object| {
                document_core::geometry::transformed_bounds(object.size, object.transform).unwrap()
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let (mut editor, opened) = open_editor(&temp, &project, &assets);
    let selected = vec![
        opened.project.document.pages[0].objects[0]
            .id
            .as_str()
            .to_owned(),
    ];
    let before = editor
        .selection_geometry(opened.project.revision.value(), &selected)
        .unwrap();
    assert_eq!(
        before.object_ids.len(),
        4,
        "one member expands the complete group"
    );
    assert_eq!(before.bounds.width_um, native_bounds.width.value());
    assert_eq!(before.bounds.height_um, native_bounds.height.value());
    let mut input = ResizeSelectionInput {
        expected_revision: opened.project.revision.value(),
        object_ids: selected.clone(),
        scope: SelectionScopeInput::Groups,
        x_um: -10_000,
        y_um: 20_000,
        width_um: before.bounds.width_um * 3 / 2,
        height_um: before.bounds.height_um * 2 / 3,
    };
    let resized = editor.resize_selection(input.clone()).unwrap();
    let after = editor
        .selection_geometry(resized.project.revision.value(), &selected)
        .unwrap();
    for delta in [
        after.bounds.x_um - input.x_um,
        after.bounds.y_um - input.y_um,
        after.bounds.width_um - input.width_um,
        after.bounds.height_um - input.height_um,
    ] {
        assert!(delta.abs() <= 3);
    }
    for (original, changed) in opened.project.document.pages[0]
        .objects
        .iter()
        .zip(&resized.project.document.pages[0].objects)
    {
        assert_eq!(changed.size, original.size);
        assert_eq!(changed.kind, original.kind);
        assert_eq!(changed.group_id, original.group_id);
    }
    assert_eq!(
        editor.history_retention_status().unwrap().undo_transactions,
        1
    );
    input.expected_revision = resized.project.revision.value();
    input.width_um = 0;
    assert!(editor.resize_selection(input).is_err());
    assert_eq!(editor.snapshot().unwrap().project, resized.project);
    let undone = editor.undo(resized.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, opened.project.document);
    let redone = editor.redo(undone.project.revision.value()).unwrap();
    let rotated = editor
        .rotate_selection(RotateSelectionInput {
            expected_revision: redone.project.revision.value(),
            object_ids: selected.clone(),
            scope: SelectionScopeInput::Groups,
            delta_microdegrees: 90_000_000,
        })
        .unwrap();
    assert!(
        rotated.project.document.pages[0]
            .objects
            .iter()
            .zip(&resized.project.document.pages[0].objects)
            .all(|(a, b)| a.kind == b.kind && a.size == b.size)
    );
    let aligned = apply(
        &mut editor,
        rotated.project.revision.value(),
        vec![
            EditorCommandInput::ArrangeGeometryToPage {
                object_ids: selected.clone(),
                operation: ArrangeGeometryOperationInput::AlignRight,
            },
            EditorCommandInput::ArrangeGeometryToPage {
                object_ids: selected.clone(),
                operation: ArrangeGeometryOperationInput::AlignTop,
            },
        ],
    );
    let bounds = editor
        .selection_geometry(aligned.project.revision.value(), &selected)
        .unwrap()
        .bounds;
    assert_eq!(
        bounds.x_um + bounds.width_um,
        aligned.project.document.pages[0].trim.width.value()
    );
    assert_eq!(bounds.y_um, 0);
    let copy = temp.0.join("finished.publisher-v2");
    editor.save_new(&copy).unwrap();
    let reopened = EditorModule::new().open(&copy).unwrap();
    assert_eq!(reopened.project.document, aligned.project.document);
}

#[test]
#[allow(clippy::too_many_lines)] // One capacity fixture checks the member limit, range refusal and Undo.
fn five_hundred_member_actions_are_atomic_and_larger_selections_are_refused() {
    let temp = TemporaryFolder::new();
    let mut session = DocumentSession::new(document()).unwrap();
    let page = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Many objects".into(),
                commands: (0..501)
                    .map(|index| Command::AddObject {
                        location: ObjectLocation::Page(page.clone()),
                        object: shape(index),
                    })
                    .collect(),
            },
        )
        .unwrap();
    let (mut editor, opened) = open_editor(
        &temp,
        &ProjectSnapshot::from_session(&session),
        &AssetStore::default(),
    );
    let ids = opened.project.document.pages[0]
        .objects
        .iter()
        .map(|object| object.id.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(
        editor
            .apply(ApplyTransactionInput {
                expected_revision: opened.project.revision.value(),
                label: "Too many".into(),
                commands: vec![EditorCommandInput::MoveSelection {
                    object_ids: ids.clone(),
                    destination: application_core::SelectionDestinationInput::Current,
                    delta_x_um: 100,
                    delta_y_um: 200
                }]
            })
            .is_err()
    );
    assert_eq!(editor.snapshot().unwrap().project, opened.project);
    let moved = apply(
        &mut editor,
        opened.project.revision.value(),
        vec![EditorCommandInput::MoveSelection {
            object_ids: ids[..500].to_vec(),
            destination: application_core::SelectionDestinationInput::Current,
            delta_x_um: 100,
            delta_y_um: 200,
        }],
    );
    assert_eq!(
        editor.history_retention_status().unwrap().undo_transactions,
        1
    );
    assert_eq!(
        moved.project.document.pages[0].objects[499]
            .transform
            .translation_y
            .value(),
        25_200
    );
    assert_eq!(
        moved.project.document.pages[0].objects[500],
        opened.project.document.pages[0].objects[500]
    );
    assert!(
        editor
            .apply(ApplyTransactionInput {
                expected_revision: moved.project.revision.value(),
                label: "Out of range move".into(),
                commands: vec![EditorCommandInput::MoveSelection {
                    object_ids: vec![ids[0].clone()],
                    destination: application_core::SelectionDestinationInput::Current,
                    delta_x_um: 10_000_000,
                    delta_y_um: 0
                }],
            })
            .is_err()
    );
    assert_eq!(editor.snapshot().unwrap().project, moved.project);
    let pasted = apply(
        &mut editor,
        moved.project.revision.value(),
        vec![EditorCommandInput::MoveSelection {
            object_ids: ids[..500].to_vec(),
            destination: application_core::SelectionDestinationInput::Pasteboard,
            delta_x_um: -100,
            delta_y_um: -200,
        }],
    );
    assert_eq!(pasted.project.document.pages[0].objects.len(), 1);
    assert_eq!(pasted.project.document.pasteboard.objects.len(), 500);
    assert_eq!(
        pasted.project.document.pasteboard.objects[499],
        opened.project.document.pages[0].objects[499]
    );
    let restored = editor.undo(pasted.project.revision.value()).unwrap();
    assert_eq!(restored.project.document, moved.project.document);
    let deleted = apply(
        &mut editor,
        restored.project.revision.value(),
        vec![EditorCommandInput::DeleteSelection {
            object_ids: ids[..500].to_vec(),
        }],
    );
    assert_eq!(deleted.project.document.pages[0].objects.len(), 1);
    let undone = editor.undo(deleted.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, moved.project.document);
}

#[test]
fn rounded_shape_creation_and_updates_share_one_transaction_and_typed_wire() {
    let temp = TemporaryFolder::new();
    let empty = ProjectSnapshot {
        document: document(),
        revision: Revision::initial(),
    };
    let (mut editor, opened) = open_editor(&temp, &empty, &AssetStore::default());
    let page = opened.project.document.pages[0].id.as_str();
    let next_id = serde_json::to_value(&opened.project.document).unwrap()["next_id"]
        .as_u64()
        .unwrap();
    let object_id = format!("object-{:016x}", next_id + 1);
    let add: EditorCommandInput = serde_json::from_value(serde_json::json!({"type":"addShape","pageId":page,"name":"Rounded rectangle","shape":"rectangle","xUm":12000,"yUm":14000,"widthUm":25400,"heightUm":12700,"fill":{"red":20,"green":60,"blue":110,"alpha":0},"stroke":{"color":{"red":20,"green":60,"blue":110,"alpha":255},"widthMillipoints":1000},"cornerRounding":{"topLeftUm":8000,"topRightUm":8000,"bottomRightUm":8000,"bottomLeftUm":8000,"linked":true}})).unwrap();
    let prior_allocation = EditorCommandInput::AddPage {
        after_page_id: Some(page.to_owned()),
        trim_width_um: 215_900,
        trim_height_um: 279_400,
        bleed_um: 0,
    };
    let composed = apply(&mut editor, 0, vec![prior_allocation, add]);
    assert_eq!(
        editor.history_retention_status().unwrap().undo_transactions,
        1
    );
    let object = &composed.project.document.pages[0].objects[0];
    assert_eq!(object.id.as_str(), object_id);
    assert_eq!(
        object.corner_rounding.top_left_um, 8000,
        "requested radii stay authored"
    );
    assert_eq!(
        document_core::shape_geometry::normalized_corner_radii(object.size, object.corner_rounding)
            .unwrap(),
        [6350; 4]
    );
    assert!(
        matches!(&object.kind, ObjectKind::Shape { fill, stroke, .. } if fill.alpha == 0 && stroke.width_millipoints == 1000)
    );
    let before = editor.snapshot().unwrap();
    let invalid: EditorCommandInput = serde_json::from_value(serde_json::json!({"type":"updateCornerRounding","objectId":object_id,"topLeftUm":-1,"topRightUm":0,"bottomRightUm":0,"bottomLeftUm":0,"linked":false})).unwrap();
    assert!(
        editor
            .apply(ApplyTransactionInput {
                expected_revision: before.project.revision.value(),
                label: "Invalid radius".into(),
                commands: vec![invalid]
            })
            .is_err()
    );
    assert_eq!(editor.snapshot().unwrap().project, before.project);
    let undone = editor.undo(before.project.revision.value()).unwrap();
    assert_eq!(undone.project.document, opened.project.document);
    assert!(
        !serde_json::to_value(undone.project.document)
            .unwrap()
            .to_string()
            .contains("corner_rounding")
    );
}

#[test]
#[allow(clippy::too_many_lines)] // One endpoint fixture covers arbitrary old transforms, directions and admission failures.
fn line_endpoints_normalize_arbitrary_old_transforms_without_changing_appearance() {
    use application_core::SetLineEndpointsInput;
    let temp = TemporaryFolder::new();
    let mut line = shape(0);
    if let ObjectKind::Shape { shape, stroke, .. } = &mut line.kind {
        *shape = ShapeKind::Line;
        stroke.width_millipoints = 1_500;
    }
    line.transform = Transform {
        scale_x_nanos: -1_500_000_000,
        shear_x_nanos: 400_000_000,
        rotation_microdegrees: 87_000_000,
        pivot_x: document_core::model::Micrometres::new(4_000),
        pivot_y: document_core::model::Micrometres::new(6_000),
        ..line.transform
    };
    let mut session = DocumentSession::new(document()).unwrap();
    let page = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Line",
                Command::AddObject {
                    location: ObjectLocation::Page(page),
                    object: line,
                },
            ),
        )
        .unwrap();
    let (mut editor, opened) = open_editor(
        &temp,
        &ProjectSnapshot::from_session(&session),
        &AssetStore::default(),
    );
    let original = opened.project.document.pages[0].objects[0].clone();
    for (end_x, end_y) in [(50_000, 15_000), (12_000, 45_000), (-30_000, -20_000)] {
        let before = editor.snapshot().unwrap();
        let input = SetLineEndpointsInput {
            expected_revision: before.project.revision.value(),
            object_id: original.id.as_str().to_owned(),
            start_x_um: 12_000,
            start_y_um: 15_000,
            end_x_um: end_x,
            end_y_um: end_y,
        };
        let changed = editor.set_line_endpoints(input.clone()).unwrap();
        let object = &changed.project.document.pages[0].objects[0];
        assert_eq!(object.kind, original.kind);
        assert_eq!(object.effects, original.effects);
        assert_eq!(object.opacity_millionths, original.opacity_millionths);
        assert_eq!(object.size.height.value(), 1);
        let start = document_core::geometry::transformed_point_um(object.transform, 0, 0).unwrap();
        let end = document_core::geometry::transformed_point_um(
            object.transform,
            object.size.width.value(),
            object.size.height.value(),
        )
        .unwrap();
        assert_eq!(start, (12_000.0, 15_000.0));
        assert!((end.0 - f64::from(i32::try_from(end_x).unwrap())).abs() <= 3.0);
        assert!((end.1 - f64::from(i32::try_from(end_y).unwrap())).abs() <= 3.0);
        let mut stale = input;
        stale.end_x_um += 500;
        assert!(editor.set_line_endpoints(stale).is_err());
        assert_eq!(editor.snapshot().unwrap().project, changed.project);
        let undone = editor.undo(changed.project.revision.value()).unwrap();
        assert_eq!(undone.project.document, before.project.document);
    }
    let before = editor.snapshot().unwrap();
    let mut invalid = SetLineEndpointsInput {
        expected_revision: before.project.revision.value(),
        object_id: original.id.as_str().to_owned(),
        start_x_um: 12_000,
        start_y_um: 15_000,
        end_x_um: 12_000,
        end_y_um: 15_000,
    };
    assert!(editor.set_line_endpoints(invalid.clone()).is_err());
    invalid.end_x_um = 20_000_000;
    assert!(editor.set_line_endpoints(invalid).is_err());
    assert_eq!(editor.snapshot().unwrap().project, before.project);
    let locked = apply(
        &mut editor,
        before.project.revision.value(),
        vec![EditorCommandInput::SetObjectLock {
            object_ids: vec![original.id.as_str().to_owned()],
            locked: true,
        }],
    );
    assert!(
        editor
            .set_line_endpoints(SetLineEndpointsInput {
                expected_revision: locked.project.revision.value(),
                object_id: original.id.as_str().to_owned(),
                start_x_um: 0,
                start_y_um: 0,
                end_x_um: 10_000,
                end_y_um: 10_000
            })
            .is_err()
    );
    assert_eq!(editor.snapshot().unwrap().project, locked.project);
}
