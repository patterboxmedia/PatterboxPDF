mod common;

use common::letter_document;
use document_core::commands::{Command, CommandError, DocumentSession, Transaction};
use document_core::model::{GuideAxis, Micrometres, Revision};

#[test]
fn page_guides_are_revision_checked_undoable_and_bounded_by_trim() {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add vertical guide",
                Command::AddGuide {
                    page_id: page_id.clone(),
                    axis: GuideAxis::Vertical,
                    position: Micrometres::new(25_400),
                },
            ),
        )
        .expect("add guide");
    let guide = session.document().pages[0].guides[0].clone();
    assert_eq!(guide.axis, GuideAxis::Vertical);
    assert_eq!(guide.position.value(), 25_400);

    let stale = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Stale guide move",
                Command::MoveGuide {
                    page_id: page_id.clone(),
                    guide_id: guide.id.clone(),
                    position: Micrometres::new(38_100),
                },
            ),
        )
        .expect_err("guide edits use the same revision check as object edits");
    assert!(matches!(stale, CommandError::RevisionConflict { .. }));

    let moved = session
        .execute(
            added.revision,
            Transaction::single(
                "Move guide",
                Command::MoveGuide {
                    page_id: page_id.clone(),
                    guide_id: guide.id.clone(),
                    position: Micrometres::new(50_800),
                },
            ),
        )
        .expect("move guide");
    assert_eq!(
        session.document().pages[0].guides[0].position.value(),
        50_800
    );
    session.undo(moved.revision).expect("undo move");
    assert_eq!(
        session.document().pages[0].guides[0].position.value(),
        25_400
    );

    let error = session
        .execute(
            session.revision(),
            Transaction::single(
                "Move beyond trim",
                Command::MoveGuide {
                    page_id,
                    guide_id: guide.id,
                    position: Micrometres::new(300_000),
                },
            ),
        )
        .expect_err("guide cannot leave its page");
    assert!(matches!(
        error,
        CommandError::GuidePositionOutOfRange { .. }
    ));

    let delete_page_id = session.document().pages[0].id.clone();
    let delete_guide_id = session.document().pages[0].guides[0].id.clone();
    let deleted = session
        .execute(
            session.revision(),
            Transaction::single(
                "Delete guide",
                Command::DeleteGuide {
                    page_id: delete_page_id,
                    guide_id: delete_guide_id,
                },
            ),
        )
        .expect("delete guide");
    assert!(session.document().pages[0].guides.is_empty());
    session.undo(deleted.revision).expect("undo guide deletion");
    assert_eq!(session.document().pages[0].guides.len(), 1);
}
