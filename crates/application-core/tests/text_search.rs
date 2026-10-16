use application_core::{
    ApplyTransactionInput, CreateDocumentInput, EditorCommandInput, EditorModule, FindTextInput,
    ReplaceAllTextInput, ReplaceTextMatchInput, TextBoxSizingModeInput, TextSearchScope,
};

fn editor() -> EditorModule {
    let mut editor = EditorModule::new();
    editor
        .create(CreateDocumentInput {
            document_id: "search-book".into(),
            title: "Search".into(),
            trim_width_um: 215_900,
            trim_height_um: 279_400,
            bleed_um: 3_175,
            page_count: 1,
        })
        .unwrap();
    editor
        .apply(ApplyTransactionInput {
            expected_revision: 0,
            label: "Add text".into(),
            commands: vec![EditorCommandInput::AddText {
                page_id: None,
                name: "Pasteboard text".into(),
                text: "CAFÉ café 🦀 café".into(),
                style: None,
                sizing_mode: TextBoxSizingModeInput::Fixed,
                x_um: 0,
                y_um: 0,
                width_um: 80_000,
                height_um: 30_000,
            }],
        })
        .unwrap();
    editor
}

fn search(revision: u64) -> FindTextInput {
    FindTextInput {
        document_id: "search-book".into(),
        expected_revision: revision,
        query: "café".into(),
        match_case: false,
        whole_word: false,
        scope: TextSearchScope::Publication,
        cursor: None,
        limit: 2,
    }
}

#[test]
fn find_visits_canonical_unicode_and_replace_all_is_one_reversible_operation() {
    let mut editor = editor();
    let before = editor.snapshot().unwrap();
    let found = editor.find_text(search(1)).unwrap();
    assert_eq!(found.matches.len(), 2);
    assert!(!found.complete);
    assert_eq!(found.matches[0].text, "CAFÉ");
    let mut next = search(1);
    next.cursor = found.next_cursor;
    let tail = editor.find_text(next).unwrap();
    assert_eq!(tail.matches.len(), 1);
    assert!(tail.complete);
    let replaced = editor
        .replace_all_text(ReplaceAllTextInput {
            document_id: "search-book".into(),
            expected_revision: 1,
            query: "café".into(),
            match_case: false,
            whole_word: false,
            scope: TextSearchScope::Publication,
            replacement: "茶".into(),
        })
        .unwrap();
    assert_eq!(replaced.replaced, 3);
    let document_core::model::ObjectKind::Text { text, .. } =
        &replaced.snapshot.project.document.pasteboard.objects[0].kind
    else {
        panic!("Text Box");
    };
    assert_eq!(text, "茶 茶 🦀 茶");
    assert_eq!(
        editor
            .undo(replaced.snapshot.project.revision.value())
            .unwrap()
            .project
            .document,
        before.project.document
    );
    assert_eq!(
        editor
            .redo(editor.snapshot().unwrap().project.revision.value())
            .unwrap()
            .project
            .document,
        replaced.snapshot.project.document
    );
}

#[test]
fn stale_or_changed_match_refuses_without_mutating_text_or_history() {
    let mut editor = editor();
    let before = editor.snapshot().unwrap();
    let found = editor.find_text(search(1)).unwrap().matches.remove(0);
    let mut input = ReplaceTextMatchInput {
        document_id: "search-book".into(),
        expected_revision: 1,
        source: found.source,
        start_byte: found.start_byte,
        end_byte: found.end_byte,
        expected_text: "wrong".into(),
        replacement: "keep".into(),
    };
    assert!(editor.replace_text_match(input.clone()).is_err());
    assert_eq!(editor.snapshot().unwrap().project, before.project);
    input.expected_text = found.text;
    input.expected_revision = 0;
    assert!(editor.replace_text_match(input).is_err());
    assert_eq!(editor.snapshot().unwrap().project, before.project);
    assert_eq!(editor.snapshot().unwrap().can_undo, before.can_undo);
    let mut wrong_document = search(1);
    wrong_document.document_id = "other-book".into();
    assert!(editor.find_text(wrong_document).is_err());
}

fn apply(
    editor: &mut EditorModule,
    commands: Vec<EditorCommandInput>,
) -> application_core::EditorSnapshot {
    editor
        .apply(ApplyTransactionInput {
            expected_revision: editor.snapshot().unwrap().project.revision.value(),
            label: "Prepare search".into(),
            commands,
        })
        .unwrap()
}

#[test]
fn cross_style_replace_preserves_surroundings_and_first_character_including_base_style() {
    let mut editor = editor();
    let id = editor
        .snapshot()
        .unwrap()
        .project
        .document
        .pasteboard
        .objects[0]
        .id
        .as_str()
        .to_owned();
    apply(
        &mut editor,
        vec![EditorCommandInput::UpdateText {
            object_id: id.clone(),
            text: "Aplain tail".into(),
        }],
    );
    let before = apply(
        &mut editor,
        vec![
            EditorCommandInput::UpdateInlineTextStyle {
                object_id: id.clone(),
                start_byte: 0,
                end_byte: 1,
                bold: Some(true),
                italic: None,
            },
            EditorCommandInput::UpdateInlineTextStyle {
                object_id: id.clone(),
                start_byte: 3,
                end_byte: 6,
                bold: None,
                italic: Some(true),
            },
            EditorCommandInput::UpdateInlineTextStyle {
                object_id: id.clone(),
                start_byte: 7,
                end_byte: 11,
                bold: Some(true),
                italic: None,
            },
        ],
    );
    let changed = editor
        .replace_all_text(ReplaceAllTextInput {
            document_id: "search-book".into(),
            expected_revision: before.project.revision.value(),
            query: "plain".into(),
            match_case: true,
            whole_word: false,
            scope: TextSearchScope::Object { object_id: id },
            replacement: "new".into(),
        })
        .unwrap();
    let document_core::model::ObjectKind::Text { text, story, .. } =
        &changed.snapshot.project.document.pasteboard.objects[0].kind
    else {
        panic!("text");
    };
    assert_eq!(text, "Anew tail");
    assert_eq!(story.inline_spans.len(), 2);
    assert_eq!(
        (
            story.inline_spans[0].range.start,
            story.inline_spans[0].range.end
        ),
        (0, 1)
    );
    assert_eq!(
        (
            story.inline_spans[1].range.start,
            story.inline_spans[1].range.end
        ),
        (5, 9)
    );
    assert!(
        story
            .inline_spans
            .iter()
            .all(|span| span.style.bold == Some(true))
    );
    assert_eq!(
        editor
            .undo(changed.snapshot.project.revision.value())
            .unwrap()
            .project
            .document,
        before.project.document
    );
}

#[test]
fn whole_word_respects_unicode_boundaries_and_rejects_partial_case_expansion() {
    let mut editor = editor();
    let id = editor
        .snapshot()
        .unwrap()
        .project
        .document
        .pasteboard
        .objects[0]
        .id
        .as_str()
        .to_owned();
    let snapshot = apply(
        &mut editor,
        vec![EditorCommandInput::UpdateText {
            object_id: id,
            text: "café cafés _café café_ CAFÉ 🦀 İ café\u{301} café's café’s café‿word".into(),
        }],
    );
    let mut input = search(snapshot.project.revision.value());
    input.whole_word = true;
    input.limit = 500;
    let found = editor.find_text(input.clone()).unwrap();
    assert_eq!(found.matches.len(), 2);
    input.query = "i".into();
    input.whole_word = false;
    assert!(editor.find_text(input.clone()).unwrap().matches.is_empty());
    input.query = "i\u{307}".into();
    assert_eq!(editor.find_text(input).unwrap().matches[0].text, "İ");
}

#[test]
fn publication_replace_above_one_thousand_cells_is_atomic_and_locked_targets_refuse() {
    let mut editor = editor();
    let page_id = editor.snapshot().unwrap().project.document.pages[0]
        .id
        .as_str()
        .to_owned();
    let added = apply(
        &mut editor,
        vec![EditorCommandInput::AddTable {
            page_id: Some(page_id.clone()),
            name: "Large Table".into(),
            rows: 31,
            columns: 33,
            style: None,
            x_um: 10_000,
            y_um: 10_000,
            width_um: 190_000,
            height_um: 240_000,
        }],
    );
    let id = added.project.document.pages[0].objects[0]
        .id
        .as_str()
        .to_owned();
    let before = apply(
        &mut editor,
        vec![EditorCommandInput::UpdateTableCells {
            object_id: id.clone(),
            row: 0,
            column: 0,
            cells: vec![vec!["café".into(); 33]; 31],
        }],
    );
    let mut query = search(before.project.revision.value());
    query.scope = TextSearchScope::Page {
        page_id: page_id.clone(),
    };
    query.limit = 500;
    let found = editor.find_text(query).unwrap();
    assert_eq!(found.matches.len(), 500);
    assert!(found.matches.iter().all(|item| matches!(
        item.source,
        application_core::TextSourceLocation::TableCell { .. }
    )));
    let mut input = ReplaceAllTextInput {
        document_id: "search-book".into(),
        expected_revision: before.project.revision.value(),
        query: "café".into(),
        match_case: false,
        whole_word: true,
        scope: TextSearchScope::Page { page_id },
        replacement: "茶".into(),
    };
    let changed = editor.replace_all_text(input.clone()).unwrap();
    assert_eq!(changed.replaced, 1_023);
    let document_core::model::ObjectKind::Table { table } =
        &changed.snapshot.project.document.pages[0].objects[0].kind
    else {
        panic!("table");
    };
    assert!(table.cells.iter().all(|text| text == "茶"));
    let undone = editor
        .undo(changed.snapshot.project.revision.value())
        .unwrap();
    assert_eq!(undone.project.document, before.project.document);
    let locked = apply(
        &mut editor,
        vec![EditorCommandInput::SetObjectLock {
            object_ids: vec![id],
            locked: true,
        }],
    );
    input.expected_revision = locked.project.revision.value();
    input.scope = TextSearchScope::Publication;
    assert!(editor.replace_all_text(input).is_err());
    assert_eq!(editor.snapshot().unwrap().project, locked.project);
}

#[test]
fn authored_master_text_is_visited_once_and_generated_number_fields_are_excluded() {
    let mut editor = editor();
    let page_id = editor.snapshot().unwrap().project.document.pages[0].id.as_str().to_owned();
    let snapshot = apply(&mut editor, vec![EditorCommandInput::AddTable { page_id: Some(page_id), name: "Master table".into(), rows: 2, columns: 2, style: None,
        x_um: 10_000, y_um: 10_000, width_um: 80_000, height_um: 30_000 }]);
    let mut document = snapshot.project.document;
    let text = document.pasteboard.objects.remove(0);
    let table = document.pages[0].objects.remove(0);
    let mut field = text.clone(); field.id = document_core::ObjectId::parse("page-number-field").unwrap();
    field.text_field = Some(document_core::model::TextField::PageNumber);
    let document_core::model::ObjectKind::Text { text: field_text, .. } = &mut field.kind else { unreachable!() }; field_text.clear();
    let master_id = document_core::model::MasterId::parse("shared-footer").unwrap();
    document.masters.push(document_core::model::MasterPage { id: master_id.clone(), name: "Footer".into(), trim: document.pages[0].trim, objects: vec![text, table, field] });
    document.pages[0].master_id = Some(master_id.clone());
    let mut second = document.pages[0].clone(); second.id = document_core::PageId::parse("second-page").unwrap(); document.pages.push(second);
    document.validate().unwrap();
    let sources = application_core::authored_text_sources(&document).collect::<Vec<_>>();
    assert_eq!(sources.len(), 5, "one authored Text Box and all four authored cells, independent of master assignment count");
    assert!(sources.iter().all(|source| source.location.master_id() == Some(master_id.as_str()) && source.location.page_id().is_none()));
    assert_eq!(sources.iter().filter(|source| source.text.contains("CAFÉ")).count(), 1);
    assert!(!sources.iter().any(|source| source.location.object_id() == "page-number-field"));
}
