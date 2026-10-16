use application_core::{
    ApplyTransactionInput, CreateDocumentInput, EditorCommandInput, EditorModule,
    VerifiedImageImport,
};
use document_core::commands::HistoryRetentionLimits;
use document_core::{AssetHash, inspect_raster};
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;

fn editor() -> EditorModule {
    let mut editor = EditorModule::new();
    editor
        .create(CreateDocumentInput {
            document_id: "history-assets".to_owned(),
            title: "History".to_owned(),
            trim_width_um: 215_900,
            trim_height_um: 279_400,
            bleed_um: 0,
            page_count: 1,
        })
        .unwrap();
    editor
}

#[test]
fn serialized_history_notice_survives_empty_history_and_clears_for_new_session() {
    let mut editor = editor();
    assert_eq!(
        serde_json::to_value(editor.snapshot().unwrap()).unwrap()["olderHistoryRemoved"],
        false
    );
    editor
        .set_history_retention_limits(HistoryRetentionLimits {
            max_transactions: 0,
            ..HistoryRetentionLimits::default()
        })
        .unwrap();
    import(&mut editor);
    let snapshot = editor.snapshot().unwrap();
    assert!(!snapshot.can_undo && !snapshot.can_redo);
    assert!(snapshot.older_history_removed);
    assert_eq!(
        serde_json::to_value(snapshot).unwrap()["olderHistoryRemoved"],
        true
    );
    editor
        .create(CreateDocumentInput {
            document_id: "new-history-session".to_owned(),
            title: "New publication".to_owned(),
            trim_width_um: 215_900,
            trim_height_um: 279_400,
            bleed_um: 0,
            page_count: 1,
        })
        .unwrap();
    assert!(!editor.snapshot().unwrap().older_history_removed);
}

fn import(editor: &mut EditorModule) -> (AssetHash, String) {
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(2, 2)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    let bytes = encoded.into_inner();
    let hash = AssetHash::for_bytes(&bytes);
    let inspection = inspect_raster(&bytes).unwrap();
    let before = editor.snapshot().unwrap();
    let imported = editor
        .import_verified_image(VerifiedImageImport {
            expected_revision: before.project.revision.value(),
            page_id: Some(before.project.document.pages[0].id.as_str().to_owned()),
            name: "Image".to_owned(),
            x_um: 0,
            y_um: 0,
            width_um: 10_000,
            height_um: 10_000,
            media_type: inspection.media_type.clone(),
            asset_hash: hash.clone(),
            inspection,
            bytes,
        })
        .unwrap();
    (
        hash,
        imported.project.document.pages[0].objects[0]
            .id
            .as_str()
            .to_owned(),
    )
}

#[test]
fn pending_import_survives_zero_history_budget_and_evicted_asset_is_released() {
    let mut editor = editor();
    editor
        .set_history_retention_limits(HistoryRetentionLimits {
            max_transactions: 0,
            max_history_asset_bytes: 0,
            ..HistoryRetentionLimits::default()
        })
        .unwrap();
    let (hash, object_id) = import(&mut editor);
    assert!(editor.referenced_raster(hash.as_str()).is_ok());
    let independent = editor.snapshot_for_preflight(1).unwrap().assets;
    let before = editor.history_retention_status().unwrap();
    assert!(
        editor
            .apply(ApplyTransactionInput {
                expected_revision: 0,
                label: "Stale delete".to_owned(),
                commands: vec![EditorCommandInput::DeleteObject {
                    object_id: object_id.clone()
                }]
            })
            .is_err()
    );
    assert_eq!(editor.history_retention_status().unwrap(), before);
    assert!(editor.referenced_raster(hash.as_str()).is_ok());
    editor
        .apply(ApplyTransactionInput {
            expected_revision: 1,
            label: "Delete".to_owned(),
            commands: vec![EditorCommandInput::DeleteObject { object_id }],
        })
        .unwrap();
    assert!(editor.snapshot_for_preflight(2).unwrap().assets.is_empty());
    assert!(
        independent.contains(&hash),
        "preflight clones retain independent immutable bytes"
    );
    assert!(!editor.snapshot().unwrap().can_undo);
    assert_eq!(
        editor
            .history_retention_status()
            .unwrap()
            .history_asset_bytes,
        0
    );
}

#[test]
fn history_resources_survive_undo_redo_until_current_exemption_changes() {
    let mut editor = editor();
    let (hash, _) = import(&mut editor);
    editor.undo(1).unwrap();
    assert!(
        editor
            .history_retention_status()
            .unwrap()
            .history_asset_bytes
            > 0
    );
    editor.redo(2).unwrap();
    assert!(editor.referenced_raster(hash.as_str()).is_ok());
    editor
        .set_history_retention_limits(HistoryRetentionLimits {
            max_history_asset_bytes: 0,
            ..HistoryRetentionLimits::default()
        })
        .unwrap();
    assert!(
        editor.referenced_raster(hash.as_str()).is_ok(),
        "current assets are exempt"
    );
    editor.undo(3).unwrap();
    assert!(
        !editor.snapshot().unwrap().can_redo,
        "over-budget farthest future is retired after Undo"
    );
    assert_eq!(
        editor
            .history_retention_status()
            .unwrap()
            .history_asset_bytes,
        0
    );
    assert!(
        editor
            .history_retention_status()
            .unwrap()
            .older_history_removed
    );
}
