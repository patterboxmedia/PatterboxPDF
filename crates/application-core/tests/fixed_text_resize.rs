use application_core::{
    ApplyTransactionInput, EditorCommandInput, EditorModule, ResizeSelectionInput,
    SelectionScopeInput,
};
use document_core::geometry::{PhysicalBounds, transformed_bounds, transformed_point_um};
use document_core::model::{
    Color, CornerRounding, FontFaceBinding, Insets, ObjectDraft, ObjectKind, PhysicalSize,
    TextBoxSizingMode, TextFrame, TextStoryFormatting, TextStyle, Transform,
};
use document_core::{
    AssetStore, Command, Document, DocumentId, DocumentSession, ObjectLocation, ProjectSnapshot,
    Transaction, save_new_package,
};
use layout_core::{ExactFontResource, ResolvedPublication};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct TemporaryFolder(PathBuf);
impl TemporaryFolder {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "publisher-fixed-resize-{}-{}",
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

fn fixture(
    transform: Transform,
    frame: TextFrame,
    corners: CornerRounding,
) -> (TemporaryFolder, EditorModule, ExactFontResource) {
    let temp = TemporaryFolder::new();
    // The normal portable regression uses its checked-in exact face. A native
    // receipt can supply the hash-pinned Georgia bytes from the read-only
    // ordinary witness, without editing that publication or Windows fonts.
    let supplied = std::env::var_os("PUBLISHER_FIXED_RESIZE_FONT");
    let bytes = supplied.as_ref().map_or_else(
        || include_bytes!("../../layout-core/tests/fixtures/DejaVuSans-Bold.ttf").to_vec(),
        |path| std::fs::read(path).unwrap(),
    );
    let mut assets = AssetStore::default();
    let source = assets.insert("font/ttf", bytes.clone());
    let font = ExactFontResource {
        source: source.clone(),
        face_index: 0,
        bytes,
    };
    let document = Document::new(
        DocumentId::parse("fixed-resize").unwrap(),
        "Header",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::default(),
    )
    .unwrap();
    let mut session = DocumentSession::new(document).unwrap();
    let page = session.document().pages[0].id.clone();
    let added = session
        .execute(
            session.revision(),
            Transaction::single(
                "Header",
                Command::AddObject {
                    location: ObjectLocation::Page(page),
                    object: ObjectDraft {
                        name: "Word Search".into(),
                        size: PhysicalSize::new(52_070, 8_606).unwrap(),
                        transform,
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Word Search".into(),
                            style: TextStyle {
                                font_family: if supplied.is_some() {
                                    "Georgia"
                                } else {
                                    "DejaVu Sans"
                                }
                                .into(),
                                font_face: Some(FontFaceBinding {
                                    source,
                                    face_index: 0,
                                }),
                                font_size_millipoints: 32_000,
                                font_weight: 700,
                                alignment: document_core::model::TextAlignment::Center,
                                color: Color {
                                    red: 23,
                                    green: 63,
                                    blue: 49,
                                    alpha: 255,
                                },
                                line_height_per_thousand: 1_050,
                                ..TextStyle::default()
                            },
                            frame,
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .unwrap();
    if !corners.is_default() {
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Corners",
                    Command::UpdateCornerRounding {
                        object_id: added.created_objects[0].clone(),
                        rounding: corners,
                    },
                ),
            )
            .unwrap();
    }
    let path = temp.0.join("source.publisher-v2");
    save_new_package(&path, &ProjectSnapshot::from_session(&session), &assets).unwrap();
    let mut editor = EditorModule::new();
    editor.open(&path).unwrap();
    (temp, editor, font)
}

fn header_transform() -> Transform {
    Transform {
        scale_x_nanos: 1_487_292_164,
        scale_y_nanos: 2_133_743_906,
        ..Transform::identity_at(70_261, 23_618)
    }
}
fn input(editor: &EditorModule, bounds: PhysicalBounds) -> ResizeSelectionInput {
    let snapshot = editor.snapshot().unwrap();
    ResizeSelectionInput {
        expected_revision: snapshot.project.revision.value(),
        object_ids: vec![
            snapshot.project.document.pages[0].objects[0]
                .id
                .as_str()
                .to_owned(),
        ],
        scope: SelectionScopeInput::Groups,
        x_um: bounds.x.value(),
        y_um: bounds.y.value(),
        width_um: bounds.width.value(),
        height_um: bounds.height.value(),
    }
}
fn assert_bounds(actual: PhysicalBounds, expected: PhysicalBounds) {
    for delta in [
        actual.x.value() - expected.x.value(),
        actual.y.value() - expected.y.value(),
        actual.width.value() - expected.width.value(),
        actual.height.value() - expected.height.value(),
    ] {
        assert!(delta.abs() <= 3, "physical resize mismatch {delta}um");
    }
}

#[test]
fn single_fixed_numeric_side_and_corner_resize_reflows_with_one_undo_and_reopen() {
    for (width, height) in [(100_000, 18_364), (77_444, 25_000), (108_422, 25_710)] {
        let (temp, mut editor, font) = fixture(
            header_transform(),
            TextFrame::default(),
            CornerRounding::default(),
        );
        let before = editor.snapshot().unwrap();
        let prior = &before.project.document.pages[0].objects[0];
        let old_layout = ResolvedPublication::resolve_with_font_resources(
            &before.project,
            std::slice::from_ref(&font),
        )
        .unwrap();
        assert!(old_layout.text_boxes()[0].overset);
        assert!(old_layout.text_boxes()[0].lines.is_empty());
        let target = PhysicalBounds {
            x: prior.transform.translation_x,
            y: prior.transform.translation_y,
            width: document_core::model::Micrometres::new(width),
            height: document_core::model::Micrometres::new(height),
        };
        let resized = editor.resize_selection(input(&editor, target)).unwrap();
        let object = &resized.project.document.pages[0].objects[0];
        assert!((object.size.width.value() - width).abs() <= 3);
        assert!((object.size.height.value() - height).abs() <= 3);
        assert_eq!(object.transform.scale_x_nanos, Transform::IDENTITY_SCALE);
        assert_eq!(object.transform.scale_y_nanos, Transform::IDENTITY_SCALE);
        assert_eq!(object.kind, prior.kind);
        assert_eq!(object.id, prior.id);
        assert_bounds(
            transformed_bounds(object.size, object.transform).unwrap(),
            target,
        );
        let layout = ResolvedPublication::resolve_with_font_resources(
            &resized.project,
            std::slice::from_ref(&font),
        )
        .unwrap();
        assert!(!layout.text_boxes()[0].overset);
        assert!(!layout.text_boxes()[0].lines.is_empty());
        assert_eq!(
            editor.history_retention_status().unwrap().undo_transactions,
            1
        );
        let undone = editor.undo(resized.project.revision.value()).unwrap();
        assert_eq!(undone.project.document, before.project.document);
        let redone = editor.redo(undone.project.revision.value()).unwrap();
        assert_eq!(redone.project.document, resized.project.document);
        let saved = temp.0.join("resized.publisher-v2");
        editor.save_new(&saved).unwrap();
        let mut reopened = EditorModule::new();
        assert_eq!(
            reopened.open(&saved).unwrap().project.document,
            resized.project.document
        );
    }
}

#[test]
#[expect(
    clippy::cast_precision_loss,
    reason = "The regression independently compares the bounded native affine corners within3um"
)]
fn transformed_fixed_frame_preserves_reflection_pivot_corners_and_kind() {
    let transform = Transform {
        rotation_microdegrees: 27_000_000,
        pivot_x: document_core::model::Micrometres::new(7_000),
        pivot_y: document_core::model::Micrometres::new(1_500),
        scale_x_nanos: -1_200_000_000,
        scale_y_nanos: 1_700_000_000,
        shear_x_nanos: 200_000_000,
        shear_y_nanos: -100_000_000,
        ..Transform::identity_at(80_000, 50_000)
    };
    let corners = CornerRounding {
        top_left_um: 800,
        top_right_um: 700,
        bottom_right_um: 600,
        bottom_left_um: 500,
        linked: false,
    };
    let (_temp, mut editor, _font) = fixture(transform, TextFrame::default(), corners);
    let before = editor.snapshot().unwrap();
    let old = &before.project.document.pages[0].objects[0];
    let bounds = transformed_bounds(old.size, old.transform).unwrap();
    let target = PhysicalBounds {
        width: document_core::model::Micrometres::new(bounds.width.value() * 3 / 2),
        height: document_core::model::Micrometres::new(bounds.height.value() * 2),
        ..bounds
    };
    let resized = editor.resize_selection(input(&editor, target)).unwrap();
    let object = &resized.project.document.pages[0].objects[0];
    assert_eq!(object.transform.scale_x_nanos, -Transform::IDENTITY_SCALE);
    assert_eq!(object.transform.scale_y_nanos, Transform::IDENTITY_SCALE);
    assert_eq!(
        object.transform.rotation_microdegrees,
        transform.rotation_microdegrees
    );
    assert_eq!(object.kind, old.kind);
    assert_eq!(object.corner_rounding, old.corner_rounding);
    assert_bounds(
        transformed_bounds(object.size, object.transform).unwrap(),
        target,
    );
    for (x, y) in [
        (0, 0),
        (old.size.width.value(), 0),
        (0, old.size.height.value()),
        (old.size.width.value(), old.size.height.value()),
    ] {
        let original = transformed_point_um(old.transform, x, y).unwrap();
        let nx = if x == 0 { 0 } else { object.size.width.value() };
        let ny = if y == 0 {
            0
        } else {
            object.size.height.value()
        };
        let actual = transformed_point_um(object.transform, nx, ny).unwrap();
        let expected_x = target.x.value() as f64
            + (original.0 - bounds.x.value() as f64) * target.width.value() as f64
                / bounds.width.value() as f64;
        let expected_y = target.y.value() as f64
            + (original.1 - bounds.y.value() as f64) * target.height.value() as f64
                / bounds.height.value() as f64;
        assert!((actual.0 - expected_x).abs() <= 3.0);
        assert!((actual.1 - expected_y).abs() <= 3.0);
    }
}

fn apply(editor: &mut EditorModule, commands: Vec<EditorCommandInput>) {
    editor
        .apply(ApplyTransactionInput {
            expected_revision: editor.snapshot().unwrap().project.revision.value(),
            label: "Fixture setup".into(),
            commands,
        })
        .unwrap();
}

#[test]
fn fixed_frame_refusals_preserve_snapshot_history_and_exact_assets() {
    let frame = TextFrame {
        column_count: 2,
        margins: Insets {
            left: document_core::model::Micrometres::new(800),
            right: document_core::model::Micrometres::new(800),
            ..Insets::default()
        },
        ..TextFrame::default()
    };
    let (_temp, mut editor, _font) = fixture(header_transform(), frame, CornerRounding::default());
    let snapshot = editor.snapshot().unwrap();
    let before = editor
        .snapshot_for_preflight(snapshot.project.revision.value())
        .unwrap();
    let history = editor.history_retention_status().unwrap();
    let bounds = transformed_bounds(
        snapshot.project.document.pages[0].objects[0].size,
        snapshot.project.document.pages[0].objects[0].transform,
    )
    .unwrap();
    let mut stale = input(&editor, bounds);
    stale.expected_revision -= 1;
    assert!(editor.resize_selection(stale).is_err());
    for (width, height) in [(1000, 20_000), (80_000, 0)] {
        let mut invalid = input(&editor, bounds);
        invalid.width_um = width;
        invalid.height_um = height;
        assert!(editor.resize_selection(invalid).is_err());
    }
    assert_eq!(editor.snapshot().unwrap(), snapshot);
    assert_eq!(
        editor
            .snapshot_for_preflight(snapshot.project.revision.value())
            .unwrap(),
        before
    );
    assert_eq!(editor.history_retention_status().unwrap(), history);
    apply(
        &mut editor,
        vec![EditorCommandInput::SetObjectLock {
            object_ids: vec![
                snapshot.project.document.pages[0].objects[0]
                    .id
                    .as_str()
                    .to_owned(),
            ],
            locked: true,
        }],
    );
    let locked = editor.snapshot().unwrap();
    let mut requested = input(&editor, bounds);
    requested.height_um *= 2;
    assert!(editor.resize_selection(requested).is_err());
    assert_eq!(editor.snapshot().unwrap(), locked);
}

#[test]
fn fit_grouped_fixed_and_explicit_group_members_retain_intrinsic_stretch() {
    let (_temp, mut editor, _font) = fixture(
        header_transform(),
        TextFrame {
            sizing_mode: TextBoxSizingMode::Fit,
            ..TextFrame::default()
        },
        CornerRounding::default(),
    );
    let before = editor.snapshot().unwrap();
    let ids = vec![
        before.project.document.pages[0].objects[0]
            .id
            .as_str()
            .to_owned(),
    ];
    let geometry = editor
        .selection_geometry(before.project.revision.value(), &ids)
        .unwrap();
    let stretched = editor
        .resize_selection(ResizeSelectionInput {
            expected_revision: before.project.revision.value(),
            object_ids: ids,
            scope: SelectionScopeInput::Groups,
            x_um: geometry.bounds.x_um,
            y_um: geometry.bounds.y_um,
            width_um: geometry.bounds.width_um * 2,
            height_um: geometry.bounds.height_um * 2,
        })
        .unwrap();
    assert_eq!(
        stretched.project.document.pages[0].objects[0].size,
        before.project.document.pages[0].objects[0].size
    );
    let (_temp, mut editor, _font) = fixture(
        header_transform(),
        TextFrame::default(),
        CornerRounding::default(),
    );
    let first = editor.snapshot().unwrap().project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    apply(
        &mut editor,
        vec![EditorCommandInput::DuplicateObject {
            object_id: first.clone(),
            offset_x_um: 80_000,
            offset_y_um: 0,
        }],
    );
    let ids = editor.snapshot().unwrap().project.document.pages[0]
        .objects
        .iter()
        .map(|object| object.id.as_str().to_owned())
        .collect();
    apply(
        &mut editor,
        vec![EditorCommandInput::GroupObjects { object_ids: ids }],
    );
    for scope in [SelectionScopeInput::Groups, SelectionScopeInput::Members] {
        let before = editor.snapshot().unwrap();
        let bounds = editor
            .selection_geometry_scoped(
                before.project.revision.value(),
                std::slice::from_ref(&first),
                scope,
            )
            .unwrap()
            .bounds;
        let resized = editor
            .resize_selection(ResizeSelectionInput {
                expected_revision: before.project.revision.value(),
                object_ids: vec![first.clone()],
                scope,
                x_um: bounds.x_um,
                y_um: bounds.y_um,
                width_um: bounds.width_um * 3 / 2,
                height_um: bounds.height_um * 4 / 3,
            })
            .unwrap();
        for (old, new) in before.project.document.pages[0]
            .objects
            .iter()
            .zip(&resized.project.document.pages[0].objects)
        {
            assert_eq!(new.size, old.size);
            assert_eq!(new.kind, old.kind);
        }
    }
}
