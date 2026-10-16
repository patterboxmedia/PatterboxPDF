use super::*;

const ORIENTED_TIFF: &[u8] =
    include_bytes!("../../print-core/tests/fixtures/oriented-alpha-3x2.tif");

fn editor() -> EditorModule {
    let mut editor = EditorModule::new();
    editor
        .create(CreateDocumentInput {
            document_id: "picture-batch".into(),
            title: "Pictures".into(),
            trim_width_um: 100_000,
            trim_height_um: 100_000,
            bleed_um: 0,
            page_count: 1,
        })
        .unwrap();
    editor
}

fn verified(
    name: &str,
    bytes: &[u8],
    revision: u64,
    page: Option<String>,
    x_um: i64,
) -> VerifiedImageImport {
    let inspection = document_core::inspect_raster(bytes).unwrap();
    VerifiedImageImport {
        expected_revision: revision,
        page_id: page,
        name: name.into(),
        x_um,
        y_um: 10_000,
        width_um: 20_000,
        height_um: 30_000,
        media_type: inspection.media_type.clone(),
        asset_hash: AssetHash::for_bytes(bytes),
        inspection,
        bytes: bytes.to_vec(),
    }
}

#[test]
fn ordered_successful_files_share_one_import_and_one_undo() {
    let mut editor = editor();
    let page = editor.snapshot().unwrap().project.document.pages[0]
        .id
        .as_str()
        .to_owned();
    // Native decoding reports individual failures before passing only admitted files to core.
    let files = [
        ("First.tif", ORIENTED_TIFF),
        ("Broken.png", b"bad".as_slice()),
        ("Second.tif", ORIENTED_TIFF),
    ];
    let mut failures = Vec::new();
    let successes = files
        .into_iter()
        .enumerate()
        .filter_map(|(index, (name, bytes))| {
            if document_core::inspect_raster(bytes).is_err() {
                failures.push(name);
                None
            } else {
                Some(verified(
                    name,
                    bytes,
                    0,
                    Some(page.clone()),
                    10_000 + i64::try_from(index).unwrap() * 5_000,
                ))
            }
        })
        .collect();
    let placed = editor.import_verified_images(0, successes).unwrap();
    assert_eq!(failures, ["Broken.png"]);
    assert_eq!(placed.project.revision.value(), 1);
    assert_eq!(
        placed.project.document.pages[0]
            .objects
            .iter()
            .map(|object| object.name.as_str())
            .collect::<Vec<_>>(),
        ["First.tif", "Second.tif"]
    );
    assert_eq!(
        placed.project.document.pages[0].objects[1]
            .transform
            .translation_x
            .value(),
        20_000
    );
    assert_eq!(
        editor.assets.len(),
        1,
        "duplicate exact bytes share one resource"
    );
    assert_eq!(
        editor
            .assets
            .get(&AssetHash::for_bytes(ORIENTED_TIFF))
            .unwrap()
            .bytes
            .as_slice(),
        ORIENTED_TIFF
    );
    let undone = editor.undo(1).unwrap();
    assert!(undone.project.document.pages[0].objects.is_empty());
    assert!(!undone.can_undo);
    let redone = editor.redo(2).unwrap();
    assert_eq!(redone.project.document, placed.project.document);
}

#[test]
fn stale_invalid_and_aggregate_batch_refusals_preserve_publication_assets_and_redo() {
    let mut editor = editor();
    editor
        .import_verified_image(verified("Retained.tif", ORIENTED_TIFF, 0, None, -10_000))
        .unwrap();
    editor.undo(1).unwrap();
    let before = editor.snapshot().unwrap();
    let assets_before = editor.assets.clone();
    let mut mismatched = verified("Mismatch.tif", ORIENTED_TIFF, 2, None, 0);
    mismatched.inspection.media_type = "image/png".into();
    let mut mixed_revision = verified("Wrong session.tif", ORIENTED_TIFF, 1, None, 0);
    mixed_revision.name = "Mixed revision".into();
    let escaped_name = "\0".repeat(MAX_TITLE_BYTES);
    for (revision, inputs) in [
        (1, vec![verified("Stale.tif", ORIENTED_TIFF, 1, None, 0)]),
        (2, vec![mismatched]),
        (
            2,
            vec![
                verified("Valid.tif", ORIENTED_TIFF, 2, None, 0),
                mixed_revision,
            ],
        ),
        (
            2,
            (0..45)
                .map(|_| verified(&escaped_name, ORIENTED_TIFF, 2, None, 0))
                .collect(),
        ),
        (2, Vec::new()),
        (
            2,
            vec![verified("Too many.tif", ORIENTED_TIFF, 2, None, 0); MAX_CANONICAL_COMMANDS + 1],
        ),
    ] {
        assert!(editor.import_verified_images(revision, inputs).is_err());
        assert_eq!(editor.snapshot().unwrap(), before);
        assert_eq!(editor.assets, assets_before);
    }
    assert_eq!(
        editor
            .redo(2)
            .unwrap()
            .project
            .document
            .pasteboard
            .objects
            .len(),
        1
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one Picture journey verifies nondefault appearance, intrinsic crop/flip, reset, source retention, and Undo/Redo together"
)]
fn intrinsic_picture_flip_and_reset_preserve_placement_effects_and_source() {
    let mut editor = editor();
    let placed = editor
        .import_verified_image(verified("Oriented.tif", ORIENTED_TIFF, 0, None, -10_000))
        .unwrap();
    editor
        .execute_saveable(
            placed.project.revision,
            Transaction {
                label: "Picture appearance".into(),
                commands: vec![
                    Command::UpdateObjectEffects {
                        object_id: placed.project.document.pasteboard.objects[0].id.clone(),
                        effects: ObjectEffects {
                            shadow: Some(HardShadow {
                                offset_x: Micrometres::new(2_000),
                                offset_y: Micrometres::new(3_000),
                                color: Color {
                                    red: 0,
                                    green: 0,
                                    blue: 0,
                                    alpha: 128,
                                },
                            }),
                            picture_border: Some(ShapeStroke {
                                color: Color {
                                    red: 200,
                                    green: 50,
                                    blue: 20,
                                    alpha: 255,
                                },
                                width_millipoints: 2_000,
                            }),
                            text_outline: None,
                        },
                    },
                    Command::SetImageAspectLock {
                        object_id: placed.project.document.pasteboard.objects[0].id.clone(),
                        locked: false,
                    },
                ],
            },
            std::iter::empty(),
        )
        .unwrap();
    let placed = editor.snapshot().unwrap();
    let object = &placed.project.document.pasteboard.objects[0];
    let id = object.id.as_str().to_owned();
    let crop = ImageCropInput {
        left_millionths: 100_000,
        top_millionths: 200_000,
        right_millionths: 250_000,
        bottom_millionths: 50_000,
    };
    let cropped = editor
        .apply(ApplyTransactionInput {
            expected_revision: 2,
            label: "Crop and flip".into(),
            commands: vec![
                EditorCommandInput::UpdateImageCrop {
                    object_id: id.clone(),
                    crop,
                },
                EditorCommandInput::SetImageFlip {
                    object_id: id.clone(),
                    horizontal: true,
                    vertical: true,
                },
            ],
        })
        .unwrap();
    let edited = &cropped.project.document.pasteboard.objects[0];
    assert_eq!(edited.size, object.size);
    assert_eq!(edited.transform, object.transform);
    assert_eq!(edited.effects, object.effects);
    assert!(
        matches!(edited.kind, ObjectKind::Image { flip, .. } if flip.horizontal && flip.vertical)
    );
    let reset = editor
        .apply(ApplyTransactionInput {
            expected_revision: 3,
            label: "Reset Picture".into(),
            commands: vec![EditorCommandInput::ResetImageEditing { object_id: id }],
        })
        .unwrap();
    assert_eq!(reset.project.document, placed.project.document);
    assert_eq!(
        editor
            .assets
            .get(&AssetHash::for_bytes(ORIENTED_TIFF))
            .unwrap()
            .bytes
            .as_slice(),
        ORIENTED_TIFF
    );
    assert_eq!(
        editor.undo(4).unwrap().project.document,
        cropped.project.document
    );
    assert_eq!(
        editor.redo(5).unwrap().project.document,
        placed.project.document
    );
}

#[test]
fn locked_picture_editing_commands_refuse_without_changing_history() {
    let mut editor = editor();
    let placed = editor
        .import_verified_image(verified("Locked.tif", ORIENTED_TIFF, 0, None, 0))
        .unwrap();
    let id = placed.project.document.pasteboard.objects[0].id.clone();
    editor
        .execute_saveable(
            placed.project.revision,
            Transaction::single(
                "Lock Picture",
                Command::SetObjectLock {
                    object_ids: vec![id.clone()],
                    locked: true,
                },
            ),
            std::iter::empty(),
        )
        .unwrap();
    let before = editor.snapshot().unwrap();
    let assets_before = editor.assets.clone();
    for command in [
        EditorCommandInput::SetImageFlip {
            object_id: id.as_str().into(),
            horizontal: true,
            vertical: false,
        },
        EditorCommandInput::ResetImageEditing {
            object_id: id.as_str().into(),
        },
    ] {
        assert!(
            editor
                .apply(ApplyTransactionInput {
                    expected_revision: 2,
                    label: "Locked edit".into(),
                    commands: vec![command]
                })
                .is_err()
        );
        assert_eq!(editor.snapshot().unwrap(), before);
        assert_eq!(editor.assets, assets_before);
    }
}

#[test]
fn reimporting_identical_legacy_pixels_keeps_the_existing_asset_metadata() {
    let bytes =
        include_bytes!("../../print-core/tests/fixtures/embedded-rgb-alpha-3x2.tif");
    let mut editor = editor();
    let mut legacy = document_core::inspect_raster(bytes).unwrap();
    legacy.color_profile = document_core::assets::RasterColorProfile::Unspecified;
    let hash = editor.assets.insert_raster(bytes.to_vec(), legacy.clone());
    editor
        .import_verified_image(verified("Legacy pixels.tif", bytes, 0, None, 0))
        .unwrap();
    assert_eq!(editor.assets.len(), 1);
    assert_eq!(
        editor.assets.get(&hash).unwrap().raster.as_ref(),
        Some(&legacy)
    );
}
