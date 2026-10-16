mod common;

use common::letter_document;
use document_core::commands::{
    Command, CommandError, DocumentSession, HistoryRetentionLimits, Transaction,
};
use document_core::model::Insets;

fn edit(session: &mut DocumentSession, bleed: i64) {
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Bleed",
                Command::UpdatePageBleed {
                    page_id: session.document().pages[0].id.clone(),
                    bleed: Insets::uniform(bleed).unwrap(),
                },
            ),
        )
        .unwrap();
}

#[test]
fn count_eviction_preserves_contiguous_undo_redo_and_clone_branches() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    session.set_history_retention_limits(HistoryRetentionLimits {
        max_transactions: 2,
        ..HistoryRetentionLimits::default()
    });
    edit(&mut session, 1);
    let oldest = session.document().clone();
    edit(&mut session, 2);
    edit(&mut session, 3);
    let latest = session.document().clone();
    assert_eq!(session.history_retention_status().undo_transactions, 2);
    assert!(session.history_retention_status().older_history_removed);
    let mut branch = session.clone();
    session.undo(session.revision()).unwrap();
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &oldest);
    assert!(matches!(
        session.undo(session.revision()),
        Err(CommandError::NothingToUndo)
    ));
    session.redo(session.revision()).unwrap();
    session.redo(session.revision()).unwrap();
    assert_eq!(session.document(), &latest);
    branch.undo(branch.revision()).unwrap();
    edit(&mut branch, 9);
    assert!(!branch.can_redo());
    assert_eq!(session.document(), &latest);
}

#[test]
fn snapshot_byte_limit_exempts_current_and_counts_shared_states_once() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    edit(&mut session, 1);
    let first_charge = session.history_retention_status().snapshot_bytes;
    assert_eq!(
        first_charge,
        serde_json::to_vec(&letter_document()).unwrap().len() as u64
    );
    edit(&mut session, 2);
    let second_charge = session.history_retention_status().snapshot_bytes;
    assert!(second_charge < first_charge * 3);
    session.set_history_retention_limits(HistoryRetentionLimits {
        max_snapshot_bytes: 0,
        ..HistoryRetentionLimits::default()
    });
    assert!(!session.can_undo());
    assert_eq!(session.history_retention_status().snapshot_bytes, 0);
    let current = session.document().clone();
    edit(&mut session, 3);
    assert_ne!(session.document(), &current);
    assert!(!session.can_undo());
}

#[test]
fn rejected_admission_preserves_history_revision_and_retention_status() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    session.set_history_retention_limits(HistoryRetentionLimits {
        max_transactions: 1,
        ..HistoryRetentionLimits::default()
    });
    edit(&mut session, 1);
    let document = session.document().clone();
    let revision = session.revision();
    let status = session.history_retention_status();
    let rejected = session.execute_checked(
        revision,
        Transaction::single(
            "Rejected",
            Command::UpdatePageBleed {
                page_id: document.pages[0].id.clone(),
                bleed: Insets::uniform(2).unwrap(),
            },
        ),
        |_, _| Err(CommandError::Saveability("injected".to_owned())),
    );
    assert!(rejected.is_err());
    assert!(
        session
            .undo_checked(revision, |_, _| Err(CommandError::Saveability(
                "injected".to_owned()
            )))
            .is_err()
    );
    assert_eq!(session.document(), &document);
    assert_eq!(session.revision(), revision);
    assert_eq!(session.history_retention_status(), status);
    session.undo(revision).unwrap();
    let status = session.history_retention_status();
    let revision = session.revision();
    assert!(
        session
            .redo_checked(revision, |_, _| Err(CommandError::Saveability(
                "injected".to_owned()
            )))
            .is_err()
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.history_retention_status(), status);
}

#[test]
fn exact_fonts_are_unique_history_resources_and_asset_limit_preserves_redo_order() {
    use document_core::AssetHash;
    use document_core::model::{ExactFontFallbackChain, FontFaceBinding, FontFallbackScript};
    let hash = AssetHash::for_bytes(b"font");
    let mut session = DocumentSession::new(letter_document()).unwrap();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Bind font",
                Command::SetExactFontFallbacks {
                    fallbacks: vec![ExactFontFallbackChain {
                        script: FontFallbackScript::Arabic,
                        faces: vec![FontFaceBinding {
                            source: hash.clone(),
                            face_index: 0,
                        }],
                    }],
                },
            ),
        )
        .unwrap();
    session.retain_history_resources(std::collections::BTreeMap::from([(hash.clone(), 100)]));
    edit(&mut session, 1);
    edit(&mut session, 2);
    assert_eq!(session.history_retention_status().history_asset_bytes, 0);
    session.undo(session.revision()).unwrap();
    session.undo(session.revision()).unwrap();
    session.undo(session.revision()).unwrap();
    assert_eq!(
        session.history_retention_status().history_asset_bytes,
        100,
        "shared font charged once across three future states"
    );
    assert_eq!(session.history_retention_status().redo_transactions, 3);
    session.set_history_retention_limits(HistoryRetentionLimits {
        max_transactions: 1,
        ..HistoryRetentionLimits::default()
    });
    assert_eq!(session.history_retention_status().redo_transactions, 1);
    session.redo(session.revision()).unwrap();
    assert_eq!(
        session.document().exact_font_fallbacks[0].faces[0].source,
        hash
    );
    assert!(
        !session.can_redo(),
        "closest future retained, farther edits evicted"
    );
    session.set_history_retention_limits(HistoryRetentionLimits {
        max_history_asset_bytes: 99,
        ..HistoryRetentionLimits::default()
    });
    session.undo(session.revision()).unwrap();
    assert!(!session.can_redo());
    assert!(session.retained_asset_hashes().is_empty());
}
