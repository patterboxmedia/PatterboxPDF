mod common;

use common::{TempDirectory, letter_document};
use document_core::model::{
    ObjectDraft, ObjectKind, PageNumberFormat, PageNumberingSection, PhysicalSize, TextField,
    TextFrame, TextStoryFormatting, TextStyle, Transform,
};
use document_core::{
    AssetStore, Command, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
    open_package, save_new_package,
};

fn values(session: &DocumentSession) -> Vec<String> {
    session
        .document()
        .page_numbers()
        .unwrap()
        .into_iter()
        .map(|number| number.display)
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)] // One section journey checks insertion, duplication, reorder, deletion and history.
fn sections_and_suppression_follow_page_order_without_accidental_restarts() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let trim = session.document().pages[0].trim;
    let bleed = session.document().pages[0].bleed;
    for _ in 0..4 {
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Append",
                    Command::AddPage {
                        after: None,
                        trim,
                        bleed,
                    },
                ),
            )
            .unwrap();
    }
    let pages = session
        .document()
        .pages
        .iter()
        .map(|page| page.id.clone())
        .collect::<Vec<_>>();
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Book numbering".into(),
                commands: vec![
                    Command::UpdatePageNumbering {
                        page_id: pages[0].clone(),
                        section: Some(PageNumberingSection {
                            format: PageNumberFormat::RomanLower,
                            start: Some(1),
                        }),
                    },
                    Command::UpdatePageNumbering {
                        page_id: pages[2].clone(),
                        section: Some(PageNumberingSection {
                            format: PageNumberFormat::Decimal,
                            start: Some(42),
                        }),
                    },
                    Command::SetPageNumberSuppression {
                        page_ids: vec![pages[0].clone(), pages[3].clone()],
                        suppressed: true,
                    },
                ],
            },
        )
        .unwrap();
    assert_eq!(values(&session), ["i", "ii", "42", "43", "44"]);
    assert!(session.document().page_numbers().unwrap()[3].suppressed);
    let inserted = session
        .execute(
            session.revision(),
            Transaction::single(
                "Insert in body",
                Command::AddPage {
                    after: Some(pages[2].clone()),
                    trim,
                    bleed,
                },
            ),
        )
        .unwrap()
        .created_pages[0]
        .clone();
    assert_eq!(values(&session), ["i", "ii", "42", "43", "44", "45"]);
    let copy = session
        .execute(
            session.revision(),
            Transaction::single(
                "Duplicate section start",
                Command::DuplicatePage {
                    page_id: pages[2].clone(),
                },
            ),
        )
        .unwrap()
        .created_pages[0]
        .clone();
    assert!(
        session
            .document()
            .pages
            .iter()
            .find(|page| page.id == copy)
            .unwrap()
            .numbering_section
            .is_none()
    );
    assert_eq!(values(&session), ["i", "ii", "42", "43", "44", "45", "46"]);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Move marker",
                Command::ReorderPage {
                    page_id: pages[2].clone(),
                    new_index: 4,
                },
            ),
        )
        .unwrap();
    assert_eq!(values(&session), ["i", "ii", "iii", "iv", "42", "43", "44"]);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Delete marker",
                Command::DeletePage {
                    page_id: pages[2].clone(),
                },
            ),
        )
        .unwrap();
    assert_eq!(values(&session), ["i", "ii", "iii", "iv", "42", "43"]);
    assert!(session.document().pages[4].numbering_section.is_some());
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document().pages[4].id, pages[2]);
    let before = session.document().clone();
    assert!(
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Invalid start",
                    Command::UpdatePageNumbering {
                        page_id: inserted,
                        section: Some(PageNumberingSection {
                            format: PageNumberFormat::RomanUpper,
                            start: Some(0)
                        }),
                    }
                )
            )
            .is_err()
    );
    assert_eq!(session.document(), &before);
    let temp = TempDirectory::new("section-roundtrip");
    let path = temp.path().join("numbered.publisher");
    let snapshot = ProjectSnapshot::from_session(&session);
    save_new_package(&path, &snapshot, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, snapshot);
}

#[test]
#[allow(clippy::too_many_lines)] // The same semantic field is composed, suppressed and parked without authored text replacement.
fn number_fields_remain_semantic_and_generated_text_cannot_be_overwritten() {
    let mut session = DocumentSession::new(letter_document()).unwrap();
    let page = session.document().pages[0].id.clone();
    let object = session
        .execute(
            session.revision(),
            Transaction::single(
                "Empty footer",
                Command::AddObject {
                    location: ObjectLocation::Page(page.clone()),
                    object: ObjectDraft {
                        name: "Number".into(),
                        size: PhysicalSize::new(25_400, 12_700).unwrap(),
                        transform: Transform::identity_at(0, 0),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: String::new(),
                            style: TextStyle::default(),
                            frame: TextFrame::default(),
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .unwrap()
        .created_objects[0]
        .clone();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Insert number",
                Command::SetTextField {
                    object_id: object.clone(),
                    field: Some(TextField::PageNumber),
                },
            ),
        )
        .unwrap();
    let source = &session.document().pages[0].objects[0];
    assert!(matches!(&source.kind,ObjectKind::Text {text,..} if text.is_empty()));
    assert!(
        matches!(&session.document().composed_page(&page).unwrap().objects[0].kind,ObjectKind::Text {text,..} if text=="1")
    );
    let before = session.document().clone();
    assert!(
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Overwrite",
                    Command::UpdateText {
                        object_id: object.clone(),
                        text: "typed number".into(),
                    }
                )
            )
            .is_err()
    );
    assert_eq!(session.document(), &before);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Hide number",
                Command::SetPageNumberSuppression {
                    page_ids: vec![page.clone()],
                    suppressed: true,
                },
            ),
        )
        .unwrap();
    assert!(
        session
            .document()
            .composed_page(&page)
            .unwrap()
            .objects
            .is_empty()
    );
    assert_eq!(values(&session), ["1"]);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Park number",
                Command::MoveObject {
                    object_id: object,
                    destination: ObjectLocation::Pasteboard,
                    transform: Transform::identity_at(-30_000, 0),
                },
            ),
        )
        .unwrap();
    let preview = session.document().preview_pasteboard_objects();
    assert!(matches!(&preview[0].kind, ObjectKind::Text { text, .. } if text == "1"));
    assert!(
        matches!(&session.document().pasteboard.objects[0].kind, ObjectKind::Text { text, .. } if text.is_empty())
    );
    assert_eq!(
        session.document().pasteboard.objects[0].text_field,
        Some(TextField::PageNumber)
    );
}
