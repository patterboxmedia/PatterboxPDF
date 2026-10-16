mod common;
use common::letter_document;
use document_core::commands::{Command, CommandError, DocumentSession, Transaction};
use document_core::model::Document;

#[test]
fn page_501_add_and_duplicate_are_rejected_without_changing_history() {
    let base = letter_document();
    let document = Document::new_with_page_count(
        base.id,
        base.title,
        base.pages[0].trim,
        base.pages[0].bleed,
        500,
    )
    .unwrap();
    let mut session = DocumentSession::new(document).unwrap();
    let page = session.document().pages[0].clone();
    for command in [
        Command::AddPage {
            after: None,
            trim: page.trim,
            bleed: page.bleed,
        },
        Command::DuplicatePage { page_id: page.id },
    ] {
        let before = session.document().clone();
        let revision = session.revision();
        let error = session
            .execute(revision, Transaction::single("Exceed page limit", command))
            .expect_err("page 501 must be rejected");
        assert_eq!(
            error,
            CommandError::PageLimitReached {
                maximum: Document::MAX_INITIAL_PAGES,
            }
        );
        assert_eq!(
            error.to_string(),
            "This publication supports at most 500 pages. Remove a page before adding or duplicating another."
        );
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), revision);
        assert!(!session.can_undo());
        assert!(!session.can_redo());
    }
}
use common::rectangle;
use document_core::AssetHash;
use document_core::commands::ObjectLocation;
use document_core::model::{ObjectKind, TextFrame, TextStoryFormatting, TextStyle};
use document_core::package::{
    AssetDescriptor, PACKAGE_LIMITS_V1, PackageError, validate_document_saveability,
};
use std::collections::BTreeMap;

fn text_transaction(count: usize, text: &str) -> Transaction {
    Transaction {
        label: "Add text".into(),
        commands: (0..count)
            .map(|_| {
                let mut object = rectangle("Text", 0, 0);
                object.kind = ObjectKind::Text {
                    text: text.into(),
                    style: TextStyle::default(),
                    frame: TextFrame::default(),
                    story: TextStoryFormatting::default(),
                };
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object,
                }
            })
            .collect(),
    }
}

#[test]
fn aggregate_text_rejection_preserves_existing_redo_history() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let text = "a".repeat(32 * 1024);
    session
        .execute(session.revision(), text_transaction(152, &text))
        .unwrap();
    session.undo(session.revision()).unwrap();
    let before = session.document().clone();
    let revision = session.revision();
    let error = session
        .execute(revision, text_transaction(153, &text))
        .unwrap_err();
    assert!(error.to_string().contains("5,000,000"));
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    assert!(session.can_redo());
    assert!(!session.can_undo());
}

#[test]
fn manifest_budget_counts_json_escaping_before_admission() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    // Control bytes expand to six JSON bytes each, despite being below story limits.
    let text = "\0".repeat(32 * 1024);
    let before = session.document().clone();
    let revision = session.revision();
    let error = session
        .execute(revision, text_transaction(44, &text))
        .unwrap_err();
    assert!(error.to_string().contains("saved document size limit"));
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    assert!(!session.can_undo());
}

#[test]
fn prospective_asset_limit_is_checked_without_allocating_gigabytes() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let hash = AssetHash::for_bytes(b"descriptor boundary");
    let mut descriptors = BTreeMap::from([(
        hash.clone(),
        AssetDescriptor {
            media_type: "font/ttf".into(),
            byte_length: PACKAGE_LIMITS_V1.aggregate_asset_bytes,
            raster: None,
        },
    )]);
    assert!(
        validate_document_saveability(session.document(), session.revision(), &descriptors).is_ok()
    );
    descriptors.get_mut(&hash).unwrap().byte_length += 1;
    let before = session.document().clone();
    let revision = session.revision();
    let result = session.execute_checked(
        revision,
        text_transaction(1, "retained"),
        |document, next_revision| {
            let error =
                validate_document_saveability(document, next_revision, &descriptors).unwrap_err();
            assert!(matches!(error, PackageError::AggregateAssetLimitExceeded));
            Err(document_core::commands::CommandError::Saveability(
                error.to_string(),
            ))
        },
    );
    assert!(result.is_err());
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    assert!(!session.can_undo());
}

#[test]
fn object_budget_counts_page_and_pasteboard_together() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    session
        .execute(session.revision(), text_transaction(1, ""))
        .unwrap();
    let mut document = session.document().clone();
    let object = document.pasteboard.objects[0].clone();
    // Budget counting is independent of identity validation; no fifty-thousand
    // command/history copies are needed to exercise its aggregate boundary.
    document.pages[0].objects = vec![object; PACKAGE_LIMITS_V1.objects];
    assert!(matches!(
        validate_document_saveability(&document, session.revision(), &BTreeMap::new()),
        Err(PackageError::ObjectLimitExceeded)
    ));
}
