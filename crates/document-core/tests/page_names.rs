mod common;
use common::{TempDirectory, letter_document, rectangle};
use document_core::model::{Page, PhysicalSize};
use document_core::{
    AssetStore, Command, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
    open_package, save_new_package,
};

#[test]
#[allow(clippy::too_many_lines)] // One Page reuse journey proves naming, independent content, history and sparse reopening.
fn page_names_follow_stable_identity_and_keep_copied_artwork_independent() {
    let document = letter_document();
    assert!(
        serde_json::to_value(&document).unwrap()["pages"][0]
            .get("name")
            .is_none()
    );
    let mut session = DocumentSession::new(document).unwrap();
    let original = session.document().pages[0].id.clone();
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Original Page".into(),
                commands: vec![
                    Command::RenamePage {
                        page_id: original.clone(),
                        name: "Maze original".into(),
                    },
                    Command::AddObject {
                        location: ObjectLocation::Page(original.clone()),
                        object: rectangle("Maze artwork", 0, 0),
                    },
                ],
            },
        )
        .unwrap();
    let copy = session
        .execute(
            session.revision(),
            Transaction::single(
                "Duplicate",
                Command::DuplicatePage {
                    page_id: original.clone(),
                },
            ),
        )
        .unwrap()
        .created_pages[0]
        .clone();
    assert_eq!(session.document().pages[1].name, "Maze original");
    assert_ne!(
        session.document().pages[0].objects[0].id,
        session.document().pages[1].objects[0].id
    );
    let duplicated = session.document().clone();
    let copied_art = session.document().pages[1].objects[0].id.clone();
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Reuse only the copy".into(),
                commands: vec![
                    Command::RenamePage {
                        page_id: copy.clone(),
                        name: "Maze experiment".into(),
                    },
                    Command::ResizeObject {
                        object_id: copied_art,
                        size: PhysicalSize::new(40_000, 25_400).unwrap(),
                    },
                    Command::ReorderPage {
                        page_id: copy.clone(),
                        new_index: 0,
                    },
                ],
            },
        )
        .unwrap();
    assert_eq!(session.document().pages[0].id, copy);
    assert_eq!(session.document().pages[0].name, "Maze experiment");
    assert_eq!(
        session.document().pages[0].objects[0].size.width.value(),
        40_000
    );
    assert_eq!(session.document().pages[1], duplicated.pages[0]);
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &duplicated);
    session.redo(session.revision()).unwrap();
    let before = session.document().clone();
    let revision = session.revision();
    assert!(
        session
            .execute(
                revision,
                Transaction::single(
                    "Too long",
                    Command::RenamePage {
                        page_id: copy.clone(),
                        name: "x".repeat(Page::MAX_NAME_BYTES + 1),
                    }
                )
            )
            .is_err()
    );
    assert_eq!(session.document(), &before);
    assert_eq!(session.revision(), revision);
    let temp = TempDirectory::new("page-names");
    let path = temp.path().join("named.publisher");
    let snapshot = ProjectSnapshot::from_session(&session);
    save_new_package(&path, &snapshot, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, snapshot);
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Use automatic name",
                Command::RenamePage {
                    page_id: copy.clone(),
                    name: String::new(),
                },
            ),
        )
        .unwrap();
    assert!(
        serde_json::to_value(session.document()).unwrap()["pages"][0]
            .get("name")
            .is_none()
    );
    session
        .execute(
            session.revision(),
            Transaction::single("Remove copy", Command::DeletePage { page_id: copy }),
        )
        .unwrap();
    let original_content = session.document().clone();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Delete last Page",
                Command::DeletePage {
                    page_id: original.clone(),
                },
            ),
        )
        .unwrap();
    assert_eq!(session.document().pages.len(), 1);
    assert_eq!(session.document().pages[0].id, original);
    assert_eq!(session.document().pages[0].name, "Maze original");
    assert!(session.document().pages[0].objects.is_empty());
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &original_content);
}
