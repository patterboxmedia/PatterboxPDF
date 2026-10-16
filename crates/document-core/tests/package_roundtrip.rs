mod common;

use common::{
    TempDirectory, TestArchiveEntry, forge_archive_entry_count, forge_duplicate_archive_entry_name,
    letter_document, read_archive_entries, read_archive_entry, rectangle, replace_archive_entry,
    set_archive_comment, write_archive_entries,
};
use document_core::assets::AssetStore;
use document_core::commands::{Command, DocumentSession, ObjectLocation, Transaction};
use document_core::model::{
    ExactFontFamilyBinding, ExactFontVariantBinding, FontFaceBinding, GuideAxis, ImageContentKind, Micrometres,
    ObjectDraft, ObjectKind, PhysicalSize, Revision, TableData, TextFrame, TextStyle, Transform,
    UntaggedRasterPolicy, WrapMode,
};
use document_core::package::{
    PACKAGE_LIMITS_V1, ProjectSnapshot, open_package, save_existing_package, save_new_package,
};
use std::io::Write as _;

#[test]
fn current_package_reopens_page_guides_without_scene_state() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add page guides".to_owned(),
                commands: vec![
                    Command::AddGuide {
                        page_id: page_id.clone(),
                        axis: GuideAxis::Vertical,
                        position: Micrometres::new(25_400),
                    },
                    Command::AddGuide {
                        page_id,
                        axis: GuideAxis::Horizontal,
                        position: Micrometres::new(50_800),
                    },
                ],
            },
        )
        .expect("add guides");
    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("current-guides");
    let package = temp.path().join("guided.publisher-v2");
    save_new_package(&package, &snapshot, &AssetStore::default()).expect("save guided package");
    let reopened = open_package(&package).expect("reopen guided package");
    assert_eq!(reopened.snapshot, snapshot);
    assert_eq!(reopened.snapshot.document.pages[0].guides.len(), 2);
}

fn parseable_font() -> Vec<u8> {
    let table_count = 3_u16;
    let directory_len = 12 + usize::from(table_count) * 16;
    let mut font = vec![0_u8; directory_len];
    font[..4].copy_from_slice(&0x0001_0000_u32.to_be_bytes());
    font[4..6].copy_from_slice(&table_count.to_be_bytes());
    let mut head = vec![0_u8; 54];
    head[18..20].copy_from_slice(&1_000_u16.to_be_bytes());
    // The parser accepts a short `loca` format at this offset.
    head[50..52].copy_from_slice(&0_u16.to_be_bytes());
    let mut hhea = vec![0_u8; 36];
    hhea[34..36].copy_from_slice(&1_u16.to_be_bytes());
    let mut maxp = vec![0_u8; 6];
    maxp[..4].copy_from_slice(&0x0000_5000_u32.to_be_bytes());
    maxp[4..6].copy_from_slice(&1_u16.to_be_bytes());
    let tables = [(b"head", head), (b"hhea", hhea), (b"maxp", maxp)];
    for (index, (tag, table)) in tables.into_iter().enumerate() {
        while !font.len().is_multiple_of(4) {
            font.push(0);
        }
        let offset = u32::try_from(font.len()).expect("small test font");
        let length = u32::try_from(table.len()).expect("small test font");
        let record = 12 + index * 16;
        font[record..record + 4].copy_from_slice(tag);
        font[record + 8..record + 12].copy_from_slice(&offset.to_be_bytes());
        font[record + 12..record + 16].copy_from_slice(&length.to_be_bytes());
        font.extend_from_slice(&table);
    }
    font
}

fn snapshot_with_bound_font(source: document_core::AssetHash, face_index: u32) -> ProjectSnapshot {
    let style = TextStyle {
        font_face: Some(FontFaceBinding { source, face_index }),
        font_family: "Family label is not identity".to_owned(),
        ..TextStyle::default()
    };
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add exactly bound text",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Bound text".to_owned(),
                        size: PhysicalSize::new(20_000, 20_000).expect("positive size"),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Exact face".to_owned(),
                            style,
                            frame: TextFrame::default(),
                            story: document_core::model::TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .expect("add text");
    ProjectSnapshot::from_session(&session)
}

fn snapshot_with_bound_table_font(
    source: document_core::AssetHash,
    face_index: u32,
) -> ProjectSnapshot {
    let mut table = TableData::empty(2, 2).unwrap();
    table.cells[0] = "M".into();
    table.style.font_face = Some(FontFaceBinding { source, face_index });
    table.style.font_family = "Exact table family".into();
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add exactly bound table",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Word search".to_owned(),
                        size: PhysicalSize::new(20_000, 20_000).expect("positive size"),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .expect("add table");
    ProjectSnapshot::from_session(&session)
}

#[test]
fn schema_six_reopen_preserves_interleaved_unlocked_groups_verbatim() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add group members".to_owned(),
                commands: vec![
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle("Die", 10_000, 10_000),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object: rectangle("Outsider", 12_000, 12_000),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: rectangle("Pip", 15_000, 15_000),
                    },
                ],
            },
        )
        .expect("add objects");
    session
        .execute(
            added.revision,
            Transaction::single(
                "Group objects",
                Command::GroupObjects {
                    object_ids: vec![
                        added.created_objects[0].clone(),
                        added.created_objects[2].clone(),
                    ],
                },
            ),
        )
        .expect("group objects");
    let mut snapshot = ProjectSnapshot::from_session(&session);
    snapshot.document.pages[0].objects.swap(0, 1);
    let temp = TempDirectory::new("schema-six-groups");
    let package = temp.path().join("grouped.publisher-v2");
    save_new_package(&package, &snapshot, &AssetStore::default()).expect("save grouped package");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&package, "manifest.json"))
            .expect("parse manifest");
    manifest["schema_version"] = serde_json::Value::from(6);
    replace_archive_entry(
        &package,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    );
    let reopened = open_package(&package).expect("reopen grouped package");
    assert_eq!(reopened.snapshot, snapshot);
    let group_id = reopened.snapshot.document.pages[0].objects[0]
        .group_id
        .clone()
        .expect("persisted group");
    assert_eq!(
        reopened.snapshot.document.pages[0].objects[2]
            .group_id
            .as_ref(),
        Some(&group_id)
    );
    assert!(
        reopened.snapshot.document.pages[0]
            .objects
            .iter()
            .all(|object| !object.locked)
    );
}

#[test]
fn schema_seven_round_trip_persists_lock_state() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add die",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: rectangle("Die", 0, 0),
                },
            ),
        )
        .expect("add objects");
    let ids = added.created_objects;
    session
        .execute(
            added.revision,
            Transaction::single(
                "Lock dice",
                Command::SetObjectLock {
                    object_ids: vec![ids[0].clone()],
                    locked: true,
                },
            ),
        )
        .expect("lock group");
    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("schema-seven-lock");
    let package = temp.path().join("locked.publisher-v2");
    save_new_package(&package, &snapshot, &AssetStore::default()).expect("save schema seven");
    let reopened = open_package(&package).expect("reopen schema seven");
    assert_eq!(reopened.snapshot, snapshot);
    assert!(reopened.snapshot.document.pages[0].objects[0].locked);
}

#[test]
fn pasted_group_round_trips_with_fresh_identity_and_editable_state() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction {
                label: "Add dice".to_owned(),
                commands: [
                    rectangle("Die", 10_000, 10_000),
                    rectangle("Pip", 15_000, 15_000),
                ]
                .into_iter()
                .map(|object| Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object,
                })
                .collect(),
            },
        )
        .expect("add dice");
    let grouped = session
        .execute(
            added.revision,
            Transaction::single(
                "Group dice",
                Command::GroupObjects {
                    object_ids: added.created_objects.clone(),
                },
            ),
        )
        .expect("group dice");
    let copied_objects = session.document().pages[0].objects.clone();
    session
        .execute(
            grouped.revision,
            Transaction::single(
                "Paste dice",
                Command::PasteObjects {
                    objects: copied_objects,
                    destination: ObjectLocation::Page(page_id),
                    offset_x_um: 12_000,
                    offset_y_um: 12_000,
                },
            ),
        )
        .expect("paste dice");
    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("pasted-group");
    let package = temp.path().join("pasted.publisher-v2");
    save_new_package(&package, &snapshot, &AssetStore::default()).expect("save pasted package");
    let reopened = open_package(&package).expect("reopen pasted package");
    assert_eq!(reopened.snapshot, snapshot);
    let source_group = &reopened.snapshot.document.pages[0].objects[0].group_id;
    let pasted_group = &reopened.snapshot.document.pages[0].objects[2].group_id;
    assert_ne!(pasted_group, source_group);
    assert!(
        reopened.snapshot.document.pages[0].objects[2..]
            .iter()
            .all(|object| !object.locked)
    );
}

#[test]
fn content_addressed_asset_and_pasteboard_round_trip_through_sibling_stage() {
    let mut assets = AssetStore::default();
    let original_bytes = b"inert raster fixture bytes".to_vec();
    let hash = assets.insert("image/png", original_bytes.clone());
    assert_eq!(assets.insert("image/png", original_bytes), hash);
    assert_eq!(assets.len(), 1, "content hashes deduplicate original bytes");

    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Park candidate image",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Alternate cover".to_owned(),
                        size: PhysicalSize::new(100_000, 120_000).expect("positive size"),
                        transform: Transform::identity_at(-110_000, 0),
                        opacity_millionths: 650_000,
                        kind: ObjectKind::Image {
                            asset: hash.clone(),
                            wrap_mode: WrapMode::Rectangular,
                            content_kind: ImageContentKind::LineArt,
                            crop: document_core::model::ImageCrop::new(
                                100_000, 50_000, 200_000, 150_000,
                            )
                            .unwrap(),
                            flip: document_core::model::ImageFlip { horizontal: true, vertical: false },
                            lock_aspect_ratio: false,
                        },
                    },
                },
            ),
        )
        .expect("add pasteboard image");
    let snapshot = ProjectSnapshot::from_session(&session);

    let temp = TempDirectory::new("roundtrip");
    let destination = temp.path().join("book.publisher-v2");
    save_new_package(&destination, &snapshot, &assets).expect("stage, reopen, and commit package");
    assert!(destination.is_file());
    assert!(
        temp.path()
            .read_dir()
            .expect("list temp parent")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .contains(".stage-")),
        "successful save leaves no stage sibling"
    );

    let opened = open_package(&destination).expect("open committed package");
    assert_eq!(opened.snapshot, snapshot);
    assert_eq!(opened.assets, assets);
    assert_eq!(opened.snapshot.revision, added.revision);
    assert_eq!(opened.snapshot.document.pages[0].objects.len(), 0);
    assert_eq!(opened.snapshot.document.pasteboard.objects.len(), 1);
    assert!(matches!(
        &opened.snapshot.document.pasteboard.objects[0].kind,
        ObjectKind::Image {
            wrap_mode: WrapMode::Rectangular,
            content_kind: ImageContentKind::LineArt,
            crop,
            flip,
            lock_aspect_ratio: false,
            ..
        } if *crop == document_core::model::ImageCrop::new(100_000, 50_000, 200_000, 150_000).unwrap() && flip.horizontal && !flip.vertical
    ));

    let reopened_session = opened
        .snapshot
        .into_session()
        .expect("valid reopened session");
    assert_eq!(reopened_session.revision(), added.revision);
    assert_eq!(reopened_session.document(), session.document());
}

#[test]
fn schema_seven_picture_defaults_to_uncropped_with_aspect_lock() {
    let mut assets = AssetStore::default();
    let hash = assets.insert("image/png", b"legacy inert raster".to_vec());
    let mut session = DocumentSession::new(letter_document()).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add legacy picture",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Legacy picture".into(),
                        size: PhysicalSize::new(100_000, 120_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Image {
                            asset: hash,
                            wrap_mode: WrapMode::None,
                            content_kind: ImageContentKind::ContinuousTone,
                            crop: document_core::model::ImageCrop::default(),
                            flip: document_core::model::ImageFlip::default(),
                            lock_aspect_ratio: true,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let temp = TempDirectory::new("schema-seven-picture");
    let destination = temp.path().join("legacy-picture.publisher-v2");
    save_new_package(
        &destination,
        &ProjectSnapshot::from_session(&session),
        &assets,
    )
    .unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&destination, "manifest.json")).unwrap();
    manifest["schema_version"] = serde_json::Value::from(7);
    let kind = manifest["snapshot"]["document"]["pasteboard"]["objects"][0]["kind"]
        .as_object_mut()
        .unwrap();
    assert!(!kind.contains_key("flip"), "default flip preserves legacy semantic encoding");
    kind.remove("crop");
    kind.remove("lock_aspect_ratio");
    replace_archive_entry(
        &destination,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).unwrap(),
    );

    let reopened = open_package(&destination).unwrap();
    assert!(matches!(
        reopened.snapshot.document.pasteboard.objects[0].kind,
        ObjectKind::Image { crop, flip, lock_aspect_ratio: true, .. }
            if crop == document_core::model::ImageCrop::default() && flip.is_default()
    ));
}

#[test]
fn missing_embedded_asset_refuses_save_without_creating_destination() {
    let hash = document_core::AssetHash::for_bytes(b"not embedded");
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Reference missing image",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: ObjectDraft {
                        name: "Missing".to_owned(),
                        size: PhysicalSize::new(1_000, 1_000).expect("positive size"),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Image {
                            asset: hash.clone(),
                            wrap_mode: WrapMode::None,
                            content_kind: ImageContentKind::ContinuousTone,
                            crop: document_core::model::ImageCrop::default(),
                            flip: document_core::model::ImageFlip::default(),
                            lock_aspect_ratio: true,
                        },
                    },
                },
            ),
        )
        .expect("canonical object may be assembled before asset arrives");
    let temp = TempDirectory::new("missing-asset");
    let destination = temp.path().join("book.publisher-v2");
    let error = save_new_package(
        &destination,
        &ProjectSnapshot::from_session(&session),
        &AssetStore::default(),
    )
    .expect_err("missing asset rejects save");

    assert!(matches!(error, document_core::PackageError::MissingAsset(found) if found == hash));
    assert!(!destination.exists());
}

#[test]
fn history_assets_do_not_consume_the_saved_archive_entry_budget() {
    let mut assets = AssetStore::default();
    for index in 0..=PACKAGE_LIMITS_V1.objects {
        assets.insert("application/octet-stream", index.to_le_bytes().to_vec());
    }
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    assert!(
        matches!(
            document_core::package::validate_document_saveability(
                &snapshot.document,
                snapshot.revision,
                &document_core::package::asset_descriptors(&assets),
            ),
            Err(document_core::PackageError::ArchiveEntryLimitExceeded)
        ),
        "the entry budget still rejects an oversized live descriptor set"
    );
    let temp = TempDirectory::new("asset-entry-limit");
    let destination = temp.path().join("history.publisher-v2");
    save_new_package(&destination, &snapshot, &assets).unwrap();
    assert!(open_package(&destination).unwrap().assets.is_empty());
    assert_eq!(read_archive_entries(&destination).len(), 1);
}

#[test]
fn exact_font_face_resource_round_trips_and_missing_resource_blocks_save() {
    let mut assets = AssetStore::default();
    let font_bytes = parseable_font();
    let source = assets.insert("font/otf", font_bytes);
    let snapshot = snapshot_with_bound_font(source.clone(), 0);
    let temp = TempDirectory::new("font-face-resource");
    let destination = temp.path().join("bound.publisher-v2");
    save_new_package(&destination, &snapshot, &assets).expect("font resource is packaged");
    let opened = open_package(&destination).expect("reopen package");
    assert_eq!(opened.snapshot, snapshot);

    let missing = save_new_package(
        &temp.path().join("missing.publisher-v2"),
        &snapshot,
        &AssetStore::default(),
    )
    .expect_err("an exact font binding cannot silently resolve by family");
    assert!(matches!(missing, document_core::PackageError::MissingAsset(found) if found == source));
}

#[test]
fn exact_font_variant_and_family_resources_round_trip_and_every_face_is_validated() {
    let mut assets = AssetStore::default();
    let base_source = assets.insert("font/otf", parseable_font());
    let mut variant_bytes = parseable_font();
    variant_bytes.push(0);
    let variant_source = assets.insert("font/otf", variant_bytes);
    let mut family_bytes = parseable_font();
    family_bytes.push(2);
    let family_source = assets.insert("font/otf", family_bytes);
    let mut snapshot = snapshot_with_bound_font(base_source, 0);
    let ObjectKind::Text { style, .. } = &mut snapshot.document.pasteboard.objects[0].kind else {
        panic!("fixture remains text");
    };
    style.exact_font_variants.push(ExactFontVariantBinding {
        font_weight: 700,
        italic: false,
        face: FontFaceBinding {
            source: variant_source.clone(),
            face_index: 0,
        },
    });
    style.exact_font_families.push(ExactFontFamilyBinding {
        font_family: "Additional range family".to_owned(),
        font_weight: 400,
        italic: false,
        face: FontFaceBinding {
            source: family_source.clone(),
            face_index: 0,
        },
    });

    let temp = TempDirectory::new("font-variant-resources");
    let destination = temp.path().join("bound-variants.publisher-v2");
    save_new_package(&destination, &snapshot, &assets).expect("variant resource is packaged");
    let reopened = open_package(&destination).unwrap();
    assert_eq!(reopened.snapshot, snapshot);
    assert!(reopened.assets.contains(&family_source), "range families must survive asset pruning");

    let mut base_only_assets = AssetStore::default();
    base_only_assets.insert("font/otf", parseable_font());
    let missing = save_new_package(
        &temp.path().join("missing-variant.publisher-v2"),
        &snapshot,
        &base_only_assets,
    )
    .expect_err("a missing variant cannot silently resolve by family");
    assert!(
        matches!(missing, document_core::PackageError::MissingAsset(found) if found == variant_source)
    );

    let ObjectKind::Text { style, .. } = &mut snapshot.document.pasteboard.objects[0].kind else {
        panic!("fixture remains text");
    };
    style.exact_font_variants[0].face.face_index = 1;
    let invalid = save_new_package(
        &temp.path().join("invalid-variant.publisher-v2"),
        &snapshot,
        &assets,
    )
    .expect_err("every variant face index is validated");
    assert!(matches!(
        invalid,
        document_core::PackageError::InvalidFontFace { source, face_index: 1 }
            if source == variant_source
    ));
    let ObjectKind::Text { style, .. } = &mut snapshot.document.pasteboard.objects[0].kind else {
        panic!("fixture remains text");
    };
    style.exact_font_variants[0].face.face_index = 0;
    style.exact_font_families[0].face.face_index = 1;
    let invalid = save_new_package(
        &temp.path().join("invalid-family.publisher-v2"),
        &snapshot,
        &assets,
    ).expect_err("range family face indices also require validation");
    assert!(matches!(invalid,
        document_core::PackageError::InvalidFontFace { source, face_index: 1 }
            if source == family_source
    ));
}

#[test]
fn schema_thirteen_text_style_defaults_to_no_exact_variants() {
    let mut assets = AssetStore::default();
    let source = assets.insert("font/otf", parseable_font());
    let snapshot = snapshot_with_bound_font(source, 0);
    let temp = TempDirectory::new("schema-thirteen-font-style");
    let destination = temp.path().join("schema-thirteen.publisher-v2");
    save_new_package(&destination, &snapshot, &assets).expect("save current package");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&destination, "manifest.json"))
            .expect("parse manifest");
    assert_eq!(manifest["schema_version"], document_core::package::SCHEMA_VERSION);
    manifest["schema_version"] = serde_json::json!(13);
    replace_archive_entry(
        &destination,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).expect("serialize legacy manifest"),
    );

    let reopened = open_package(&destination).expect("schema thirteen remains readable");
    let ObjectKind::Text { style, .. } = &reopened.snapshot.document.pasteboard.objects[0].kind
    else {
        panic!("fixture remains text");
    };
    assert!(style.exact_font_variants.is_empty());
}

#[test]
fn exact_table_font_resource_round_trips_and_missing_resource_blocks_save() {
    let mut assets = AssetStore::default();
    let source = assets.insert("font/otf", parseable_font());
    let snapshot = snapshot_with_bound_table_font(source.clone(), 0);
    let temp = TempDirectory::new("table-font-resource");
    let destination = temp.path().join("bound-table.publisher-v2");
    save_new_package(&destination, &snapshot, &assets).expect("table font resource is packaged");
    assert_eq!(open_package(&destination).unwrap().snapshot, snapshot);

    let error = save_new_package(
        &temp.path().join("missing-table-font.publisher-v2"),
        &snapshot,
        &AssetStore::default(),
    )
    .expect_err("a table's exact font binding cannot silently resolve by family");
    assert!(matches!(error, document_core::PackageError::MissingAsset(found) if found == source));
}

#[test]
fn schema_two_table_without_style_opens_with_the_legacy_centered_default() {
    let mut table = TableData::empty(1, 1).unwrap();
    table.cells[0] = "M".into();
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page_id = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add legacy table",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Legacy word search".to_owned(),
                        size: PhysicalSize::new(20_000, 20_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Table { table },
                    },
                },
            ),
        )
        .unwrap();
    let temp = TempDirectory::new("schema-two-table");
    let destination = temp.path().join("legacy-table.publisher-v2");
    save_new_package(
        &destination,
        &ProjectSnapshot::from_session(&session),
        &AssetStore::default(),
    )
    .unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&destination, "manifest.json")).unwrap();
    manifest["schema_version"] = serde_json::Value::from(2);
    manifest["snapshot"]["document"]["pages"][0]["objects"][0]["kind"]["table"]
        .as_object_mut()
        .expect("table object")
        .remove("style");
    replace_archive_entry(
        &destination,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).unwrap(),
    );

    let opened = open_package(&destination).expect("schema-two table remains readable");
    let ObjectKind::Table { table } = &opened.snapshot.document.pages[0].objects[0].kind else {
        panic!("expected table");
    };
    assert_eq!(table.style.font_family, "Arial");
    assert_eq!(
        table.style.alignment,
        document_core::model::TextAlignment::Center
    );
    assert_eq!(table.style.color.red, 0);
    assert_eq!(table.style.color.green, 0);
    assert_eq!(table.style.color.blue, 0);
}

#[test]
fn schema_three_picture_without_content_kind_opens_as_continuous_tone() {
    let mut assets = AssetStore::default();
    let bytes = b"legacy raster".to_vec();
    let asset = assets.insert("image/png", bytes);
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page_id = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add legacy picture",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Legacy picture".into(),
                        size: PhysicalSize::new(20_000, 20_000).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Image {
                            asset,
                            wrap_mode: WrapMode::None,
                            content_kind: ImageContentKind::LineArt,
                            crop: document_core::model::ImageCrop::default(),
                            flip: document_core::model::ImageFlip::default(),
                            lock_aspect_ratio: true,
                        },
                    },
                },
            ),
        )
        .unwrap();
    let temp = TempDirectory::new("schema-three-picture");
    let destination = temp.path().join("legacy-picture.publisher-v2");
    save_new_package(
        &destination,
        &ProjectSnapshot::from_session(&session),
        &assets,
    )
    .unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&destination, "manifest.json")).unwrap();
    manifest["schema_version"] = serde_json::Value::from(3);
    manifest["snapshot"]["document"]["pages"][0]["objects"][0]["kind"]
        .as_object_mut()
        .expect("image object")
        .remove("content_kind");
    replace_archive_entry(
        &destination,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).unwrap(),
    );

    let opened = open_package(&destination).expect("schema-three picture remains readable");
    assert!(matches!(
        opened.snapshot.document.pages[0].objects[0].kind,
        ObjectKind::Image {
            content_kind: ImageContentKind::ContinuousTone,
            ..
        }
    ));
}

#[test]
fn schema_four_document_without_color_settings_stays_fail_closed() {
    let assets = AssetStore::default();
    let session = DocumentSession::new(letter_document()).unwrap();
    let temp = TempDirectory::new("schema-four-color-settings");
    let destination = temp.path().join("legacy-color-settings.publisher-v2");
    save_new_package(
        &destination,
        &ProjectSnapshot::from_session(&session),
        &assets,
    )
    .unwrap();

    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&destination, "manifest.json")).unwrap();
    assert_eq!(
        manifest["snapshot"]["document"]["color_settings"]["untagged_raster_policy"],
        "require_embedded_profile"
    );
    manifest["schema_version"] = serde_json::Value::from(4);
    manifest["snapshot"]["document"]
        .as_object_mut()
        .expect("document object")
        .remove("color_settings");
    replace_archive_entry(
        &destination,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).unwrap(),
    );

    let opened = open_package(&destination).expect("schema-four document remains readable");
    assert_eq!(
        opened
            .snapshot
            .document
            .color_settings
            .untagged_raster_policy,
        UntaggedRasterPolicy::RequireEmbeddedProfile
    );
}

#[test]
fn exact_font_binding_rejects_untrusted_media_bytes_and_invalid_face_index() {
    let temp = TempDirectory::new("invalid-font-resource");

    let mut wrong_media = AssetStore::default();
    let wrong_media_source = wrong_media.insert("image/png", parseable_font());
    let error = save_new_package(
        &temp.path().join("wrong-media.publisher-v2"),
        &snapshot_with_bound_font(wrong_media_source.clone(), 0),
        &wrong_media,
    )
    .expect_err("a non-font declared resource cannot back a font face");
    assert!(
        matches!(error, document_core::PackageError::InvalidFontMediaType { source, .. } if source == wrong_media_source)
    );

    let mut arbitrary = AssetStore::default();
    let arbitrary_source = arbitrary.insert("font/otf", b"not a font".to_vec());
    let error = save_new_package(
        &temp.path().join("arbitrary.publisher-v2"),
        &snapshot_with_bound_font(arbitrary_source.clone(), 0),
        &arbitrary,
    )
    .expect_err("arbitrary bytes cannot back a font face");
    assert!(
        matches!(error, document_core::PackageError::InvalidFontFace { source, face_index: 0 } if source == arbitrary_source)
    );

    let mut out_of_range = AssetStore::default();
    let out_of_range_source = out_of_range.insert("font/otf", parseable_font());
    let error = save_new_package(
        &temp.path().join("out-of-range.publisher-v2"),
        &snapshot_with_bound_font(out_of_range_source.clone(), 1),
        &out_of_range,
    )
    .expect_err("a single-face font has no collection index one");
    assert!(
        matches!(error, document_core::PackageError::InvalidFontFace { source, face_index: 1 } if source == out_of_range_source)
    );
}

#[test]
fn changed_asset_bytes_are_detected_on_reopen() {
    let mut assets = AssetStore::default();
    let hash = assets.insert("image/png", b"original bytes".to_vec());
    let snapshot = snapshot_with_picture(hash.clone());
    let temp = TempDirectory::new("changed-asset");
    let destination = temp.path().join("book.publisher-v2");
    save_new_package(&destination, &snapshot, &assets).expect("save valid package");
    replace_archive_entry(
        &destination,
        &format!("assets/{}.bin", hash.as_str()),
        b"tampered bytes".to_vec(),
    );

    let error = open_package(&destination).expect_err("tampered bytes must reject");
    assert!(matches!(
        error,
        document_core::PackageError::AssetHashMismatch(found) if found == hash
    ));
}

#[test]
fn oversized_manifest_stops_before_json_deserialization() {
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let temp = TempDirectory::new("oversized-manifest");
    let destination = temp.path().join("book.publisher-v2");
    save_new_package(&destination, &snapshot, &AssetStore::default()).expect("save valid package");
    replace_archive_entry(
        &destination,
        "manifest.json",
        vec![b' '; usize::try_from(PACKAGE_LIMITS_V1.manifest_bytes + 1).expect("test size fits")],
    );

    let error = open_package(&destination).expect_err("oversized manifest must reject");
    assert!(matches!(
        error,
        document_core::PackageError::ManifestTooLarge
    ));
}

#[test]
fn archive_rejects_missing_duplicate_and_unexpected_entries() {
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let temp = TempDirectory::new("hostile-entry-set");
    let valid = temp.path().join("valid.publisher-v2");
    save_new_package(&valid, &snapshot, &AssetStore::default()).expect("save valid archive");
    let entries = read_archive_entries(&valid);

    let missing = temp.path().join("missing-manifest.publisher-v2");
    write_archive_entries(&missing, &[]);
    assert!(matches!(
        open_package(&missing).expect_err("manifest is required"),
        document_core::PackageError::MissingManifest
    ));

    let duplicate = temp.path().join("duplicate-manifest.publisher-v2");
    forge_duplicate_archive_entry_name(&duplicate, &entries, "manifest.json");
    assert!(matches!(
        open_package(&duplicate).expect_err("duplicate normalized name rejects"),
        document_core::PackageError::DuplicateArchiveEntry(path) if path == "manifest.json"
    ));

    let extra_hash = document_core::AssetHash::for_bytes(b"unexpected bytes");
    let unexpected_asset = temp.path().join("unexpected-asset.publisher-v2");
    let mut with_asset = entries.clone();
    with_asset.push(TestArchiveEntry {
        name: format!("assets/{}.bin", extra_hash.as_str()),
        bytes: b"unexpected bytes".to_vec(),
    });
    write_archive_entries(&unexpected_asset, &with_asset);
    assert!(matches!(
        open_package(&unexpected_asset).expect_err("undeclared asset rejects"),
        document_core::PackageError::UnexpectedAsset(hash) if hash == extra_hash
    ));

    let missing_asset = temp.path().join("missing-asset.publisher-v2");
    let mut assets = AssetStore::default();
    let missing_hash = assets.insert("image/png", b"declared bytes".to_vec());
    let snapshot = snapshot_with_picture(missing_hash.clone());
    save_new_package(&missing_asset, &snapshot, &assets).expect("save declared asset");
    let entries_without_asset: Vec<_> = read_archive_entries(&missing_asset)
        .into_iter()
        .filter(|entry| entry.name == "manifest.json")
        .collect();
    write_archive_entries(&missing_asset, &entries_without_asset);
    assert!(matches!(
        open_package(&missing_asset).expect_err("declared asset is required"),
        document_core::PackageError::MissingAsset(hash) if hash == missing_hash
    ));
}

#[test]
fn identical_package_inputs_produce_identical_archive_bytes_and_fingerprints() {
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let mut assets = AssetStore::default();
    assets.insert("image/png", b"stable bytes".to_vec());
    let temp = TempDirectory::new("deterministic-archive");
    let first = temp.path().join("first.publisher-v2");
    let second = temp.path().join("second.publisher-v2");

    let first_fingerprint =
        save_new_package(&first, &snapshot, &assets).expect("save first archive");
    let second_fingerprint =
        save_new_package(&second, &snapshot, &assets).expect("save second archive");

    assert_eq!(first_fingerprint, second_fingerprint);
    assert_eq!(
        std::fs::read(first).expect("read first archive"),
        std::fs::read(second).expect("read second archive")
    );
}

#[test]
fn archive_entry_count_is_bounded_before_zip_parser_allocation() {
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let temp = TempDirectory::new("hostile-entry-count");
    let archive = temp.path().join("forged-count.publisher-v2");
    save_new_package(&archive, &snapshot, &AssetStore::default()).expect("save valid archive");
    forge_archive_entry_count(&archive, 60_000);

    assert!(matches!(
        open_package(&archive).expect_err("declared entry count must be preflight bounded"),
        document_core::PackageError::ArchiveEntryLimitExceeded
    ));
}

#[test]
fn archive_rejects_traversal_absolute_backslash_ads_and_unexpected_paths() {
    let temp = TempDirectory::new("hostile-paths");
    for (index, path) in [
        "../manifest.json",
        "/manifest.json",
        "assets\\bad.bin",
        "manifest.json:stream",
        "assets/./bad.bin",
        "notes.txt",
    ]
    .into_iter()
    .enumerate()
    {
        let archive = temp.path().join(format!("bad-path-{index}.publisher-v2"));
        write_archive_entries(
            &archive,
            &[TestArchiveEntry {
                name: path.to_owned(),
                bytes: b"hostile".to_vec(),
            }],
        );
        assert!(
            matches!(
                open_package(&archive).expect_err("hostile path rejects"),
                document_core::PackageError::InvalidArchiveEntry
                    | document_core::PackageError::UnexpectedArchiveEntry(_)
            ),
            "path should reject: {path}"
        );
    }
}

#[test]
fn archive_rejects_special_entries_and_prepended_payloads() {
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let temp = TempDirectory::new("hostile-entry-types");
    let valid = temp.path().join("valid.publisher-v2");
    save_new_package(&valid, &snapshot, &AssetStore::default()).expect("save valid archive");

    let symlink = temp.path().join("symlink.publisher-v2");
    let file = std::fs::File::create(&symlink).expect("create symlink archive");
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o600);
    writer
        .start_file("manifest.json", options)
        .expect("start manifest");
    writer
        .write_all(&read_archive_entry(&valid, "manifest.json"))
        .expect("write manifest");
    writer
        .add_symlink("assets/link.bin", "../outside", options)
        .expect("add symlink");
    writer.finish().expect("finish symlink archive");
    assert!(matches!(
        open_package(&symlink).expect_err("symlink rejects"),
        document_core::PackageError::InvalidArchiveEntry
    ));

    let prepended = temp.path().join("prepended.publisher-v2");
    let mut bytes = b"hidden-prefix".to_vec();
    bytes.extend_from_slice(&std::fs::read(&valid).expect("read valid archive"));
    std::fs::write(&prepended, bytes).expect("write prepended archive");
    assert!(matches!(
        open_package(&prepended).expect_err("prepended payload rejects"),
        document_core::PackageError::InvalidArchiveEntry | document_core::PackageError::Zip(_)
    ));
}

#[test]
fn zip64_size_metadata_is_accepted_without_accepting_arbitrary_extra_fields() {
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let temp = TempDirectory::new("zip64-metadata");
    let ordinary = temp.path().join("ordinary.publisher-v2");
    save_new_package(&ordinary, &snapshot, &AssetStore::default()).expect("save ordinary archive");
    let zip64 = temp.path().join("zip64.publisher-v2");
    let file = std::fs::File::create(&zip64).expect("create zip64 archive");
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o600)
        .large_file(true);
    writer
        .start_file("manifest.json", options)
        .expect("start zip64 entry");
    writer
        .write_all(&read_archive_entry(&ordinary, "manifest.json"))
        .expect("write zip64 manifest");
    writer.finish().expect("finish zip64 archive");
    assert_eq!(
        open_package(&zip64)
            .expect("open bounded zip64 metadata")
            .snapshot,
        snapshot
    );
}

#[test]
fn archive_declared_and_actual_asset_lengths_must_match() {
    let mut assets = AssetStore::default();
    let hash = assets.insert("image/png", b"length-bound".to_vec());
    let snapshot = snapshot_with_picture(hash.clone());
    let temp = TempDirectory::new("length-mismatch");
    let destination = temp.path().join("book.publisher-v2");
    save_new_package(&destination, &snapshot, &assets).expect("save valid archive");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&destination, "manifest.json")).unwrap();
    manifest["assets"][hash.as_str()]["byte_length"] = serde_json::Value::from(1_u64);
    replace_archive_entry(
        &destination,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).unwrap(),
    );
    assert!(matches!(
        open_package(&destination).expect_err("declared length mismatch rejects"),
        document_core::PackageError::AssetLengthMismatch { .. }
    ));
}

#[test]
fn asset_entry_must_remain_a_regular_file() {
    let mut assets = AssetStore::default();
    let hash = assets.insert("image/png", b"original bytes".to_vec());
    let snapshot = snapshot_with_picture(hash.clone());
    let temp = TempDirectory::new("non-file-asset");
    let destination = temp.path().join("book.publisher-v2");
    save_new_package(&destination, &snapshot, &assets).expect("save valid package");
    let mut entries = read_archive_entries(&destination);
    entries.retain(|entry| entry.name != format!("assets/{}.bin", hash.as_str()));
    entries.push(TestArchiveEntry {
        name: format!("assets/{}/", hash.as_str()),
        bytes: Vec::new(),
    });
    write_archive_entries(&destination, &entries);

    let error = open_package(&destination).expect_err("non-file asset must reject");
    assert!(matches!(
        error,
        document_core::PackageError::InvalidArchiveEntry
            | document_core::PackageError::UnexpectedArchiveEntry(_)
    ));
}

#[test]
fn existing_save_preserves_the_prior_generation_and_commits_validated_edits() {
    let temp = TempDirectory::new("replace");
    let destination = temp.path().join("book.publisher-v2");
    let assets = AssetStore::default();
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let initial = ProjectSnapshot::from_session(&session);
    let initial_fingerprint =
        save_new_package(&destination, &initial, &assets).expect("save initial package");
    let page_id = session.document().pages[0].id.clone();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add revision",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: rectangle("Saved revision", 10_000, 20_000),
                },
            ),
        )
        .expect("edit publication");
    let revised = ProjectSnapshot::from_session(&session);

    let revised_fingerprint =
        save_existing_package(&destination, &initial_fingerprint, &revised, &assets)
            .expect("replace with validated revision");
    assert_ne!(revised_fingerprint, initial_fingerprint);
    assert_eq!(
        open_package(&destination)
            .expect("open replacement")
            .snapshot,
        revised
    );

    let prior = temp
        .path()
        .read_dir()
        .expect("list package siblings")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with(".publisher-v2-save-") && name.ends_with(".prior")
            })
        })
        .expect("one transaction-unique prior remains");
    assert_eq!(
        open_package(&prior)
            .expect("prior generation remains valid")
            .snapshot,
        initial
    );
}

#[test]
fn existing_save_refuses_an_external_byte_change_without_mutating_it() {
    let temp = TempDirectory::new("conflict");
    let destination = temp.path().join("book.publisher-v2");
    let assets = AssetStore::default();
    let snapshot = ProjectSnapshot {
        document: letter_document(),
        revision: Revision::initial(),
    };
    let opened_fingerprint =
        save_new_package(&destination, &snapshot, &assets).expect("save initial package");
    set_archive_comment(&destination, "externally rewritten");
    let changed_fingerprint = open_package(&destination)
        .expect("commented archive remains structurally valid")
        .fingerprint;
    assert_ne!(changed_fingerprint, opened_fingerprint);
    let externally_changed = std::fs::read(&destination).expect("read changed archive bytes");

    let error = save_existing_package(&destination, &opened_fingerprint, &snapshot, &assets)
        .expect_err("external change must refuse overwrite");
    assert!(matches!(
        error,
        document_core::PackageError::DestinationChanged
    ));
    assert_eq!(
        std::fs::read(&destination).expect("read unchanged external bytes"),
        externally_changed
    );
}

#[test]
fn save_omits_deleted_font_but_history_can_restore_it() {
    let mut assets = AssetStore::default();
    let hash = assets.insert("font/otf", parseable_font());
    let snapshot = snapshot_with_bound_font(hash.clone(), 0);
    let mut session = snapshot.into_session().unwrap();
    let object_id = session.document().pasteboard.objects[0].id.clone();
    session
        .execute(
            session.revision(),
            Transaction::single("Delete text", Command::DeleteObject { object_id }),
        )
        .unwrap();
    let temp = TempDirectory::new("reachable-history");
    let path = temp.path().join("history.publisher-v2");
    let fingerprint =
        save_new_package(&path, &ProjectSnapshot::from_session(&session), &assets).unwrap();
    assert!(
        open_package(&path).unwrap().assets.is_empty(),
        "deleted font must not be packaged"
    );
    session.undo(session.revision()).unwrap();
    let restored = ProjectSnapshot::from_session(&session);
    let fingerprint =
        document_core::save_existing_package_retryable(&path, &fingerprint, &restored, &assets)
            .unwrap();
    assert!(open_package(&path).unwrap().assets.contains(&hash));
    session.redo(session.revision()).unwrap();
    document_core::save_existing_package_retryable(
        &path,
        &fingerprint,
        &ProjectSnapshot::from_session(&session),
        &assets,
    )
    .unwrap();
    assert!(open_package(&path).unwrap().assets.is_empty());
    assert!(
        assets.contains(&hash),
        "saving never retires undo resources"
    );
}

fn snapshot_with_picture(hash: document_core::AssetHash) -> ProjectSnapshot {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let mut object = rectangle("Picture", 0, 0);
    object.kind = ObjectKind::Image {
        asset: hash,
        wrap_mode: WrapMode::None,
        content_kind: ImageContentKind::ContinuousTone,
        crop: document_core::model::ImageCrop::default(),
        flip: document_core::model::ImageFlip::default(),
        lock_aspect_ratio: true,
    };
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Add Picture",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object,
                },
            ),
        )
        .unwrap();
    ProjectSnapshot::from_session(&session)
}

#[test]
#[allow(clippy::too_many_lines)] // One resource-pruning journey covers shared Pictures and every font source.
fn reachable_resources_include_shared_pictures_table_variants_families_and_fallbacks() {
    use document_core::model::{ExactFontFallbackChain, FontFallbackScript};
    let mut assets = AssetStore::default();
    let picture = assets.insert("image/png", b"shared picture".to_vec());
    let base = assets.insert("font/ttf", parseable_font());
    let mut bytes = parseable_font();
    bytes.push(1);
    let variant = assets.insert("font/ttf", bytes);
    let mut bytes = parseable_font();
    bytes.push(2);
    let fallback = assets.insert("font/ttf", bytes);
    let mut bytes = parseable_font();
    bytes.push(3);
    let family = assets.insert("font/ttf", bytes);
    let orphan = assets.insert("font/ttf", vec![0; 16_384]);
    let mut snapshot = snapshot_with_picture(picture.clone());
    let mut session = snapshot.clone().into_session().unwrap();
    let object = &snapshot.document.pasteboard.objects[0];
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Shared page picture",
                Command::AddObject {
                    location: ObjectLocation::Page(snapshot.document.pages[0].id.clone()),
                    object: ObjectDraft {
                        name: "Shared".into(),
                        size: object.size,
                        transform: object.transform,
                        opacity_millionths: object.opacity_millionths,
                        kind: object.kind.clone(),
                    },
                },
            ),
        )
        .unwrap();
    let mut table = TableData::empty(1, 1).unwrap();
    table.style.font_face = Some(FontFaceBinding {
        source: base.clone(),
        face_index: 0,
    });
    table
        .style
        .exact_font_variants
        .push(ExactFontVariantBinding {
            font_weight: 700,
            italic: false,
            face: FontFaceBinding {
                source: variant.clone(),
                face_index: 0,
            },
        });
    table.style.exact_font_families.push(ExactFontFamilyBinding {
        font_family: "Table range family".to_owned(),
        font_weight: 400,
        italic: false,
        face: FontFaceBinding {
            source: family.clone(),
            face_index: 0,
        },
    });
    let mut table_object = rectangle("Table", 0, 0);
    table_object.kind = ObjectKind::Table { table };
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Table",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: table_object,
                },
            ),
        )
        .unwrap();
    snapshot = ProjectSnapshot::from_session(&session);
    snapshot.document.exact_font_fallbacks = vec![ExactFontFallbackChain {
        script: FontFallbackScript::Latin,
        faces: vec![FontFaceBinding {
            source: fallback.clone(),
            face_index: 0,
        }],
    }];
    let temp = TempDirectory::new("reachable-resources");
    let path = temp.path().join("resources.publisher-v2");
    save_new_package(&path, &snapshot, &assets).unwrap();
    let opened = open_package(&path).unwrap();
    assert_eq!(opened.assets.len(), 5);
    for hash in [&picture, &base, &variant, &family, &fallback] {
        assert_eq!(opened.assets.get(hash), assets.get(hash));
    }
    assert!(!opened.assets.contains(&orphan));
    let entries = read_archive_entries(&path);
    assert_eq!(entries.len(), 6, "manifest plus five unique resources");
    let archive_asset_bytes: usize = entries
        .iter()
        .filter(|entry| entry.name.starts_with("assets/"))
        .map(|entry| entry.bytes.len())
        .sum();
    let expected_bytes: usize = [&picture, &base, &variant, &family, &fallback]
        .iter()
        .map(|hash| assets.get(hash).unwrap().bytes.len())
        .sum();
    assert_eq!(archive_asset_bytes, expected_bytes);
    // Sharing a selected store must not copy large history resources or selected bytes.
    let selected = assets.for_document(&snapshot.document);
    assert!(std::ptr::eq(
        selected.get(&picture).unwrap().bytes.as_ptr(),
        assets.get(&picture).unwrap().bytes.as_ptr()
    ));
}

#[test]
fn line_identity_stroke_and_positive_bounds_survive_save_reopen_and_undo() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let mut line = rectangle("Line", 12_000, 18_000);
    if let ObjectKind::Shape { shape, stroke, .. } = &mut line.kind {
        *shape = document_core::model::ShapeKind::Line;
        stroke.width_millipoints = 1_500;
    }
    let added = session.execute(Revision::initial(), Transaction::single("Add line", Command::AddObject {
        location: ObjectLocation::Page(session.document().pages[0].id.clone()), object: line,
    })).unwrap();
    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("line-roundtrip");
    let path = temp.path().join("line.publisher-v2");
    save_new_package(&path, &snapshot, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, snapshot);
    let manifest: serde_json::Value = serde_json::from_slice(&read_archive_entry(&path, "manifest.json")).unwrap();
    assert_eq!(manifest["schema_version"], document_core::package::SCHEMA_VERSION);
    session.undo(added.revision).unwrap();
    assert!(session.document().pages[0].objects.is_empty());
    session.redo(session.revision()).unwrap();
    assert_eq!(session.document(), &snapshot.document);
}
