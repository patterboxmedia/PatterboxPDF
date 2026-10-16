mod common;

use common::{TempDirectory, letter_document, read_archive_entry, replace_archive_entry};
use document_core::assets::AssetStore;
use document_core::commands::{
    Command, CommandError, DocumentSession, MAX_TEXT_EDIT_COUNT, MAX_TEXT_EDIT_INSERTED_BYTES,
    ObjectLocation, TextEdit, Transaction,
};
use document_core::model::{
    InlineTextSpan, InlineTextStyle, MAX_TEXT_BYTES, ModelError, ObjectDraft, ObjectKind,
    ParagraphRange, ParagraphStyle, ParagraphStyleRun, PhysicalSize, Revision, TextAlignment,
    TextFrame, TextSourceRange, TextStoryFormatting, TextStyle, Transform,
};
use document_core::package::{PackageError, ProjectSnapshot, open_package, save_new_package};

fn text_session(text: &str) -> (DocumentSession, document_core::model::ObjectId) {
    let mut session = DocumentSession::new(letter_document()).expect("valid session");
    let page_id = session.document().pages[0].id.clone();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add story",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Story".to_owned(),
                        size: PhysicalSize::new(100_000, 100_000).expect("positive size"),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: text.to_owned(),
                            style: TextStyle::default(),
                            frame: TextFrame::default(),
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .expect("add text object");
    (session, added.created_objects[0].clone())
}

fn story(session: &DocumentSession) -> &TextStoryFormatting {
    let ObjectKind::Text { story, .. } = &session.document().pages[0].objects[0].kind else {
        panic!("fixture remains text");
    };
    story
}

fn paragraph_style_at(story: &TextStoryFormatting, index: u32) -> ParagraphStyle {
    story
        .paragraph_runs
        .iter()
        .find(|run| run.range.start <= index && index < run.range.end)
        .map_or_else(ParagraphStyle::default, |run| run.style)
}

#[test]
#[allow(clippy::too_many_lines)] // One authoring/Undo/reopen journey retains its shared state.
fn selected_appearance_and_styled_typing_preserve_utf8_neighbors_and_reopen() {
    let (mut session, object_id) = text_session("Aé🙂Z");
    let red = document_core::model::Color {
        red: 190,
        green: 20,
        blue: 30,
        alpha: 255,
    };
    let selected_style = InlineTextStyle {
        font_family: Some("DejaVu Sans".to_owned()),
        font_weight: Some(700),
        color: Some(red),
        underline: Some(true),
        ..InlineTextStyle::default()
    };
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Format selected phrase",
                Command::UpdateInlineTextAppearance {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 1, end: 7 },
                    style: selected_style.clone(),
                },
            ),
        )
        .unwrap();
    assert_eq!(
        story(&session).inline_spans,
        vec![InlineTextSpan {
            range: TextSourceRange { start: 1, end: 7 },
            style: selected_style.clone()
        }]
    );
    let selected = session.document().clone();
    let typing_style = InlineTextStyle {
        font_family: Some("DejaVu Sans".to_owned()),
        font_weight: Some(400),
        font_size_millipoints: Some(16_000),
        color: Some(document_core::model::Color {
            red: 10,
            green: 40,
            blue: 180,
            alpha: 255,
        }),
        underline: Some(false),
        ..InlineTextStyle::default()
    };
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Type with insertion style",
                Command::ApplyTextEdits {
                    object_id: object_id.clone(),
                    edits: vec![TextEdit {
                        range: TextSourceRange { start: 7, end: 7 },
                        text: "Q".to_owned(),
                        bold: None,
                        italic: None,
                        style: Some(typing_style.clone()),
                    }],
                },
            ),
        )
        .unwrap();
    let ObjectKind::Text {
        text, style: base, ..
    } = &session.document().pages[0].objects[0].kind
    else {
        panic!("text");
    };
    assert_eq!(text, "Aé🙂QZ");
    assert_eq!(base.font_family, "Arial");
    assert_eq!(
        story(&session).inline_spans,
        vec![
            InlineTextSpan {
                range: TextSourceRange { start: 1, end: 7 },
                style: selected_style
            },
            InlineTextSpan {
                range: TextSourceRange { start: 7, end: 8 },
                style: typing_style
            },
        ]
    );
    let typed = session.document().clone();
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &selected);
    session.redo(session.revision()).unwrap();
    assert_eq!(session.document(), &typed);
    let before_invalid = session.document().clone();
    assert!(
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Invalid UTF8 selection",
                    Command::UpdateInlineTextAppearance {
                        object_id,
                        range: TextSourceRange { start: 2, end: 3 },
                        style: InlineTextStyle {
                            underline: Some(false),
                            ..InlineTextStyle::default()
                        },
                    }
                )
            )
            .is_err()
    );
    assert_eq!(session.document(), &before_invalid);
    let directory = TempDirectory::new("selected-appearance");
    let path = directory.path().join("selected-appearance.publisher-v2");
    let snapshot = ProjectSnapshot::from_session(&session);
    save_new_package(&path, &snapshot, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, snapshot);
}

#[test]
fn explicit_face_weight_and_later_bold_toggles_follow_the_latest_choice() {
    let (mut session, object_id) = text_session("Styled body");
    for (patch, expected_weight) in [
        (
            InlineTextStyle {
                bold: Some(true),
                ..InlineTextStyle::default()
            },
            700,
        ),
        (
            InlineTextStyle {
                font_weight: Some(600),
                ..InlineTextStyle::default()
            },
            600,
        ),
        (
            InlineTextStyle {
                bold: Some(true),
                ..InlineTextStyle::default()
            },
            700,
        ),
        (
            InlineTextStyle {
                bold: Some(false),
                ..InlineTextStyle::default()
            },
            400,
        ),
        (
            InlineTextStyle {
                font_weight: Some(400),
                ..InlineTextStyle::default()
            },
            400,
        ),
    ] {
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Choose weight",
                    Command::UpdateInlineTextAppearance {
                        object_id: object_id.clone(),
                        range: TextSourceRange { start: 0, end: 6 },
                        style: patch,
                    },
                ),
            )
            .unwrap();
        let ObjectKind::Text { style, story, .. } = &session.document().pages[0].objects[0].kind
        else {
            panic!("text");
        };
        let ranges = document_core::model::effective_font_variant_ranges(
            style,
            story,
            TextSourceRange { start: 0, end: 11 },
        );
        assert_eq!(ranges[0].font_weight, expected_weight);
        assert_eq!(
            ranges
                .iter()
                .find(|range| range.range.start <= 6 && 6 < range.range.end)
                .unwrap()
                .font_weight,
            400,
            "unselected body keeps its default"
        );
    }
}

#[test]
fn inline_styles_use_exact_utf8_byte_ranges_and_are_one_undo_step() {
    let (mut session, object_id) = text_session("Aé🙂Z");
    let added_revision = session.revision();
    let bold = session
        .execute(
            added_revision,
            Transaction::single(
                "Bold accented text and emoji",
                Command::UpdateInlineTextStyle {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 1, end: 7 },
                    bold: Some(true),
                    italic: None,
                },
            ),
        )
        .expect("valid UTF-8 boundaries");
    assert_eq!(
        story(&session).inline_spans,
        vec![InlineTextSpan {
            range: TextSourceRange { start: 1, end: 7 },
            style: InlineTextStyle {
                font_size_millipoints: None,
                bold: Some(true),
                italic: None,
                ..InlineTextStyle::default()
            },
        }]
    );

    let italic = session
        .execute(
            bold.revision,
            Transaction::single(
                "Italicize emoji",
                Command::UpdateInlineTextStyle {
                    object_id,
                    range: TextSourceRange { start: 3, end: 7 },
                    bold: None,
                    italic: Some(true),
                },
            ),
        )
        .expect("format nested range");
    assert_eq!(
        story(&session).inline_spans,
        vec![
            InlineTextSpan {
                range: TextSourceRange { start: 1, end: 3 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(true),
                    italic: None,
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: TextSourceRange { start: 3, end: 7 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(true),
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            },
        ]
    );

    let undo_revision = session
        .undo(italic.revision)
        .expect("undo one style command");
    assert_eq!(story(&session).inline_spans.len(), 1);
    let redo_revision = session.redo(undo_revision).expect("redo one style command");
    assert_eq!(story(&session).inline_spans.len(), 2);

    session
        .execute(
            redo_revision,
            Transaction::single(
                "Italicize adjacent accented text",
                Command::UpdateInlineTextStyle {
                    object_id: session.document().pages[0].objects[0].id.clone(),
                    range: TextSourceRange { start: 1, end: 3 },
                    bold: None,
                    italic: Some(true),
                },
            ),
        )
        .expect("format adjacent range");
    assert_eq!(
        story(&session).inline_spans,
        vec![InlineTextSpan {
            range: TextSourceRange { start: 1, end: 7 },
            style: InlineTextStyle {
                font_size_millipoints: None,
                bold: Some(true),
                italic: Some(true),
                ..InlineTextStyle::default()
            },
        }],
        "adjacent identical ranges normalize into one span"
    );
}

#[test]
fn invalid_inline_range_is_rejected_without_mutating_state_or_revision() {
    let (mut session, object_id) = text_session("Aé🙂Z");
    let revision = session.revision();
    let before = session.document().clone();
    let error = session
        .execute(
            revision,
            Transaction::single(
                "Reject split code point",
                Command::UpdateInlineTextStyle {
                    object_id,
                    range: TextSourceRange { start: 2, end: 3 },
                    bold: Some(true),
                    italic: None,
                },
            ),
        )
        .expect_err("range starts inside an accented character");
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::TextSourceRangeNotUtf8Boundary)
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);

    let error = session
        .execute(
            revision,
            Transaction::single(
                "Reject out-of-range selection",
                Command::UpdateInlineTextStyle {
                    object_id: session.document().pages[0].objects[0].id.clone(),
                    range: TextSourceRange { start: 8, end: 9 },
                    bold: None,
                    italic: Some(true),
                },
            ),
        )
        .expect_err("range extends beyond story source");
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::TextSourceRangeOutOfRange)
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);

    let error = session
        .execute(
            revision,
            Transaction::single(
                "Reject edit inside code point",
                Command::ReplaceTextRange {
                    object_id: session.document().pages[0].objects[0].id.clone(),
                    range: TextSourceRange { start: 2, end: 2 },
                    text: "x".to_owned(),
                },
            ),
        )
        .expect_err("edit range starts inside an accented character");
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::TextSourceRangeNotUtf8Boundary)
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);
}

#[test]
fn paragraph_styles_overlay_and_include_the_empty_paragraph_after_trailing_lf() {
    let (mut session, object_id) = text_session("one\r\ntwo\n");
    let all = session
        .execute(
            session.revision(),
            Transaction::single(
                "Format all three paragraphs",
                Command::UpdateParagraphStyle {
                    object_id: object_id.clone(),
                    range: ParagraphRange { start: 0, end: 3 },
                    style: ParagraphStyle {
                        alignment: Some(TextAlignment::Center),
                        line_height_per_thousand: Some(1_250),
                        ..ParagraphStyle::default()
                    },
                },
            ),
        )
        .expect("trailing LF creates a third empty paragraph");
    session
        .execute(
            all.revision,
            Transaction::single(
                "Indent middle paragraph",
                Command::UpdateParagraphStyle {
                    object_id,
                    range: ParagraphRange { start: 1, end: 2 },
                    style: ParagraphStyle {
                        start_indent_um: Some(12_700),
                        first_line_indent_um: Some(-6_350),
                        space_before_millipoints: Some(6_000),
                        space_after_millipoints: Some(3_000),
                        ..ParagraphStyle::default()
                    },
                },
            ),
        )
        .expect("overlay paragraph properties");

    let runs = &story(&session).paragraph_runs;
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0].range, ParagraphRange { start: 0, end: 1 });
    assert_eq!(runs[1].range, ParagraphRange { start: 1, end: 2 });
    assert_eq!(runs[2].range, ParagraphRange { start: 2, end: 3 });
    assert_eq!(runs[1].style.alignment, Some(TextAlignment::Center));
    assert_eq!(runs[1].style.start_indent_um, Some(12_700));
    assert_eq!(runs[1].style.first_line_indent_um, Some(-6_350));
    assert_eq!(runs[2].style.line_height_per_thousand, Some(1_250));
}

#[test]
fn invalid_paragraph_formatting_and_malformed_runs_are_rejected() {
    let (mut session, object_id) = text_session("one\ntwo");
    let revision = session.revision();
    let before = session.document().clone();
    let error = session
        .execute(
            revision,
            Transaction::single(
                "Reject nonexistent paragraph",
                Command::UpdateParagraphStyle {
                    object_id,
                    range: ParagraphRange { start: 2, end: 3 },
                    style: ParagraphStyle {
                        alignment: Some(TextAlignment::Right),
                        ..ParagraphStyle::default()
                    },
                },
            ),
        )
        .expect_err("two paragraphs have indices zero and one");
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::ParagraphRangeOutOfRange)
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);

    let mut malformed = before;
    let ObjectKind::Text { story, .. } = &mut malformed.pages[0].objects[0].kind else {
        panic!("fixture remains text");
    };
    story.paragraph_runs = vec![
        ParagraphStyleRun {
            range: ParagraphRange { start: 0, end: 2 },
            style: ParagraphStyle {
                alignment: Some(TextAlignment::Left),
                ..ParagraphStyle::default()
            },
        },
        ParagraphStyleRun {
            range: ParagraphRange { start: 1, end: 2 },
            style: ParagraphStyle {
                alignment: Some(TextAlignment::Right),
                ..ParagraphStyle::default()
            },
        },
    ];
    assert_eq!(
        DocumentSession::new(malformed).expect_err("overlapping paragraph runs reject"),
        CommandError::InvalidModel(ModelError::OverlappingParagraphStyleRuns)
    );
}

#[test]
fn current_schema_round_trips_story_and_schema_eleven_defaults_it() {
    let (mut session, object_id) = text_session("Bold me\nThen me");
    let inline = session
        .execute(
            session.revision(),
            Transaction::single(
                "Bold selection",
                Command::UpdateInlineTextStyle {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 0, end: 4 },
                    bold: Some(true),
                    italic: None,
                },
            ),
        )
        .expect("format selection");
    session
        .execute(
            inline.revision,
            Transaction::single(
                "Align second paragraph",
                Command::UpdateParagraphStyle {
                    object_id,
                    range: ParagraphRange { start: 1, end: 2 },
                    style: ParagraphStyle {
                        alignment: Some(TextAlignment::Right),
                        ..ParagraphStyle::default()
                    },
                },
            ),
        )
        .expect("format paragraph");
    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("rich-text-story");
    let current_path = temp.path().join("schema-twelve.publisher-v2");
    save_new_package(&current_path, &snapshot, &AssetStore::default())
        .expect("save current schema story");
    let reopened = open_package(&current_path).expect("reopen current schema story");
    assert_eq!(reopened.snapshot, snapshot);
    assert_eq!(
        reopened
            .snapshot
            .semantic_digest()
            .expect("reopened digest"),
        snapshot.semantic_digest().expect("original digest")
    );

    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&current_path, "manifest.json"))
            .expect("parse current manifest");
    assert_eq!(
        manifest["schema_version"],
        document_core::package::SCHEMA_VERSION
    );
    assert!(
        manifest["snapshot"]["document"]["pages"][0]["objects"][0]["kind"]["story"].is_object()
    );
    manifest["schema_version"] = serde_json::json!(11);
    manifest["snapshot"]["document"]["pages"][0]["objects"][0]["kind"]
        .as_object_mut()
        .expect("text kind")
        .remove("story");
    let legacy_path = temp.path().join("schema-eleven.publisher-v2");
    std::fs::copy(&current_path, &legacy_path).expect("copy legacy fixture");
    replace_archive_entry(
        &legacy_path,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).expect("encode schema eleven manifest"),
    );
    let legacy = open_package(&legacy_path).expect("schema eleven remains readable");
    let ObjectKind::Text { story, .. } = &legacy.snapshot.document.pages[0].objects[0].kind else {
        panic!("legacy object remains text");
    };
    assert_eq!(story, &TextStoryFormatting::default());
}

#[test]
fn malformed_persisted_inline_spans_fail_open_instead_of_being_repaired() {
    let (session, _) = text_session("abcd");
    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("malformed-rich-text");
    let package = temp.path().join("malformed.publisher-v2");
    save_new_package(&package, &snapshot, &AssetStore::default()).expect("save valid source");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&read_archive_entry(&package, "manifest.json"))
            .expect("parse source manifest");
    manifest["snapshot"]["document"]["pages"][0]["objects"][0]["kind"]["story"] = serde_json::json!({
        "inline_spans": [
            { "range": { "start": 0, "end": 3 }, "style": { "bold": true } },
            { "range": { "start": 2, "end": 4 }, "style": { "italic": true } }
        ]
    });
    replace_archive_entry(
        &package,
        "manifest.json",
        serde_json::to_vec_pretty(&manifest).expect("encode malformed manifest"),
    );
    assert!(matches!(
        open_package(&package).expect_err("overlapping inline spans reject"),
        PackageError::InvalidModel(ModelError::OverlappingInlineTextSpans)
    ));
}

#[test]
#[allow(clippy::too_many_lines)] // The ordered edits share one style-affinity fixture.
fn iterative_text_edits_rebase_inline_styles_with_documented_affinity() {
    let (mut session, object_id) = text_session("ab");
    let bold = session
        .execute(
            session.revision(),
            Transaction::single(
                "Bold first character",
                Command::UpdateInlineTextStyle {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 0, end: 1 },
                    bold: Some(true),
                    italic: None,
                },
            ),
        )
        .expect("style first character");
    let italic = session
        .execute(
            bold.revision,
            Transaction::single(
                "Italicize second character",
                Command::UpdateInlineTextStyle {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 1, end: 2 },
                    bold: None,
                    italic: Some(true),
                },
            ),
        )
        .expect("style second character");
    let inserted = session
        .execute(
            italic.revision,
            Transaction::single(
                "Insert emoji between styles",
                Command::ReplaceTextRange {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 1, end: 1 },
                    text: "🙂".to_owned(),
                },
            ),
        )
        .expect("left-affine insertion");
    assert_eq!(
        story(&session).inline_spans,
        vec![
            InlineTextSpan {
                range: TextSourceRange { start: 0, end: 5 },
                style: InlineTextStyle {
                    bold: Some(true),
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: TextSourceRange { start: 5, end: 6 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: None,
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            },
        ]
    );

    let prepended = session
        .execute(
            inserted.revision,
            Transaction::single(
                "Insert at story start",
                Command::ReplaceTextRange {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 0, end: 0 },
                    text: "é".to_owned(),
                },
            ),
        )
        .expect("story-start insertion falls back right");
    assert_eq!(story(&session).inline_spans[0].range.end, 7);
    assert_eq!(story(&session).inline_spans[1].range.start, 7);

    session
        .execute(
            prepended.revision,
            Transaction::single(
                "Replace entire story",
                Command::UpdateText {
                    object_id,
                    text: "xyz".to_owned(),
                },
            ),
        )
        .expect("whole replacement inherits first replaced character");
    assert_eq!(
        story(&session).inline_spans,
        vec![InlineTextSpan {
            range: TextSourceRange { start: 0, end: 3 },
            style: InlineTextStyle {
                font_size_millipoints: None,
                bold: Some(true),
                italic: None,
                ..InlineTextStyle::default()
            },
        }]
    );
}

#[test]
fn paragraph_rebase_preserves_unaffected_paragraphs_and_inherits_insertions() {
    let (mut session, object_id) = text_session("first\nsecond\nthird\nfourth");
    let mut revision = session.revision();
    for (index, alignment) in [
        TextAlignment::Center,
        TextAlignment::Right,
        TextAlignment::Justify,
        TextAlignment::Left,
    ]
    .into_iter()
    .enumerate()
    {
        let index = u32::try_from(index).expect("four paragraph fixture fits u32");
        revision = session
            .execute(
                revision,
                Transaction::single(
                    "Style paragraph",
                    Command::UpdateParagraphStyle {
                        object_id: object_id.clone(),
                        range: ParagraphRange {
                            start: index,
                            end: index + 1,
                        },
                        style: ParagraphStyle {
                            alignment: Some(alignment),
                            ..ParagraphStyle::default()
                        },
                    },
                ),
            )
            .expect("style paragraph")
            .revision;
    }

    revision = session
        .execute(
            revision,
            Transaction::single(
                "Delete first paragraph break",
                Command::ReplaceTextRange {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 5, end: 6 },
                    text: String::new(),
                },
            ),
        )
        .expect("rebase deletion")
        .revision;
    assert_eq!(
        paragraph_style_at(story(&session), 0).alignment,
        Some(TextAlignment::Center),
        "merged paragraph keeps the paragraph containing the edit start"
    );
    assert_eq!(
        paragraph_style_at(story(&session), 1).alignment,
        Some(TextAlignment::Justify),
        "paragraph strictly after the consumed paragraph keeps its style"
    );
    assert_eq!(
        paragraph_style_at(story(&session), 2).alignment,
        Some(TextAlignment::Left)
    );

    session
        .execute(
            revision,
            Transaction::single(
                "Split third into two paragraphs",
                Command::ReplaceTextRange {
                    object_id,
                    range: TextSourceRange { start: 14, end: 14 },
                    text: "\n".to_owned(),
                },
            ),
        )
        .expect("rebase inserted paragraph break");
    assert_eq!(
        paragraph_style_at(story(&session), 1).alignment,
        Some(TextAlignment::Justify)
    );
    assert_eq!(
        paragraph_style_at(story(&session), 2).alignment,
        Some(TextAlignment::Justify),
        "new paragraph inherits the paragraph split by the insertion"
    );
    assert_eq!(
        paragraph_style_at(story(&session), 3).alignment,
        Some(TextAlignment::Left),
        "following unaffected paragraph shifts without losing its style"
    );
}

#[test]
fn ordered_disjoint_replacements_rebase_against_each_prior_edit() {
    let (mut session, object_id) = text_session("abcdef");
    let bold = session
        .execute(
            session.revision(),
            Transaction::single(
                "Bold b",
                Command::UpdateInlineTextStyle {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 1, end: 2 },
                    bold: Some(true),
                    italic: None,
                },
            ),
        )
        .expect("style first edit target");
    let italic = session
        .execute(
            bold.revision,
            Transaction::single(
                "Italicize e",
                Command::UpdateInlineTextStyle {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 4, end: 5 },
                    bold: None,
                    italic: Some(true),
                },
            ),
        )
        .expect("style second edit target");
    let before = session.document().clone();
    let edited = session
        .execute(
            italic.revision,
            Transaction::single(
                "Apply ordered text edits",
                Command::ApplyTextEdits {
                    object_id,
                    edits: vec![
                        TextEdit {
                            range: TextSourceRange { start: 1, end: 2 },
                            text: "🙂".to_owned(),
                            bold: None,
                            italic: None,
                            style: None,
                        },
                        TextEdit {
                            range: TextSourceRange { start: 5, end: 5 },
                            text: "!".to_owned(),
                            bold: None,
                            italic: None,
                            style: None,
                        },
                        TextEdit {
                            range: TextSourceRange { start: 8, end: 9 },
                            text: "É".to_owned(),
                            bold: None,
                            italic: None,
                            style: None,
                        },
                    ],
                },
            ),
        )
        .expect("each range resolves against the preceding command's text");
    let ObjectKind::Text { text, story, .. } = &session.document().pages[0].objects[0].kind else {
        panic!("fixture remains text");
    };
    assert_eq!(text, "a🙂!cdÉf");
    assert_eq!(
        story.inline_spans,
        vec![
            InlineTextSpan {
                range: TextSourceRange { start: 1, end: 6 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(true),
                    italic: None,
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: TextSourceRange { start: 8, end: 10 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: None,
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            },
        ]
    );

    session
        .undo(edited.revision)
        .expect("all ordered replacements undo atomically");
    assert_eq!(session.document(), &before);
}

#[test]
fn styled_text_edits_insert_exact_inline_styles_and_undo_once() {
    let (mut session, object_id) = text_session("Plain");
    let before = session.document().clone();
    let edited = session
        .execute(
            session.revision(),
            Transaction::single(
                "Type with caret styles",
                Command::ApplyTextEdits {
                    object_id,
                    edits: vec![
                        TextEdit {
                            range: TextSourceRange { start: 5, end: 5 },
                            text: " bold".to_owned(),
                            bold: Some(true),
                            italic: Some(false),
                            style: None,
                        },
                        TextEdit {
                            range: TextSourceRange { start: 10, end: 10 },
                            text: " italic".to_owned(),
                            bold: Some(false),
                            italic: Some(true),
                            style: None,
                        },
                    ],
                },
            ),
        )
        .expect("styled inserts are one canonical transaction");
    let ObjectKind::Text { text, story, .. } = &session.document().pages[0].objects[0].kind else {
        panic!("fixture remains text");
    };
    assert_eq!(text, "Plain bold italic");
    assert_eq!(
        story.inline_spans,
        vec![
            InlineTextSpan {
                range: TextSourceRange { start: 5, end: 10 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(true),
                    italic: Some(false),
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: TextSourceRange { start: 10, end: 17 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(false),
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            },
        ]
    );
    session
        .undo(edited.revision)
        .expect("text and typing styles undo together");
    assert_eq!(session.document(), &before);
}

#[test]
fn invalid_late_packed_edit_rolls_back_earlier_edits() {
    let (mut session, object_id) = text_session("éz");
    let revision = session.revision();
    let before = session.document().clone();
    let error = session
        .execute(
            revision,
            Transaction::single(
                "Reject invalid ordered edit",
                Command::ApplyTextEdits {
                    object_id,
                    edits: vec![
                        TextEdit {
                            range: TextSourceRange { start: 0, end: 0 },
                            text: "a".to_owned(),
                            bold: None,
                            italic: None,
                            style: None,
                        },
                        TextEdit {
                            range: TextSourceRange { start: 2, end: 2 },
                            text: "x".to_owned(),
                            bold: None,
                            italic: None,
                            style: None,
                        },
                    ],
                },
            ),
        )
        .expect_err("second range lands inside the shifted accented character");
    assert_eq!(
        error,
        CommandError::InvalidModel(ModelError::TextSourceRangeNotUtf8Boundary)
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);
}

#[test]
fn packed_text_edits_accept_more_than_one_hundred_changes_as_one_undo_step() {
    let (mut session, object_id) = text_session("");
    let before = session.document().clone();
    let edits = (0..101)
        .map(|index| TextEdit {
            range: TextSourceRange {
                start: index,
                end: index,
            },
            text: "x".to_owned(),
            bold: None,
            italic: None,
            style: None,
        })
        .collect();
    let outcome = session
        .execute(
            session.revision(),
            Transaction::single(
                "Type 101 characters",
                Command::ApplyTextEdits { object_id, edits },
            ),
        )
        .expect("packed edits exceed the bridge intent count without exceeding the batch bound");
    let ObjectKind::Text { text, .. } = &session.document().pages[0].objects[0].kind else {
        panic!("fixture remains text");
    };
    assert_eq!(text, &"x".repeat(101));
    session
        .undo(outcome.revision)
        .expect("packed batch is one undo step");
    assert_eq!(session.document(), &before);
}

#[test]
fn empty_and_excessive_text_edit_batches_fail_atomically() {
    let (mut session, object_id) = text_session("x");
    let revision = session.revision();
    let before = session.document().clone();
    assert_eq!(
        session
            .execute(
                revision,
                Transaction::single(
                    "Reject empty edit batch",
                    Command::ApplyTextEdits {
                        object_id: object_id.clone(),
                        edits: Vec::new(),
                    },
                ),
            )
            .expect_err("batch must contain at least one edit"),
        CommandError::EmptyTextEdits
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);

    let too_many = vec![
        TextEdit {
            range: TextSourceRange { start: 0, end: 0 },
            text: String::new(),
            bold: None,
            italic: None,
            style: None,
        };
        MAX_TEXT_EDIT_COUNT + 1
    ];
    assert_eq!(
        session
            .execute(
                revision,
                Transaction::single(
                    "Reject too many edits",
                    Command::ApplyTextEdits {
                        object_id,
                        edits: too_many,
                    },
                ),
            )
            .expect_err("bounded edit count"),
        CommandError::TextEditCountExceeded
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);
}

#[test]
fn direct_and_packed_text_byte_limits_fail_atomically() {
    let (mut session, object_id) = text_session("x");
    let revision = session.revision();
    let before = session.document().clone();
    let oversized_insertion = "x".repeat(MAX_TEXT_BYTES);
    assert_eq!(
        session
            .execute(
                revision,
                Transaction::single(
                    "Reject direct final story overflow",
                    Command::ReplaceTextRange {
                        object_id: object_id.clone(),
                        range: TextSourceRange { start: 1, end: 1 },
                        text: oversized_insertion.clone(),
                    },
                ),
            )
            .expect_err("direct range replacement obeys the final story bound"),
        CommandError::InvalidModel(ModelError::TextTooLarge)
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);

    assert_eq!(
        session
            .execute(
                revision,
                Transaction::single(
                    "Reject aggregate insertion overflow",
                    Command::ApplyTextEdits {
                        object_id: object_id.clone(),
                        edits: vec![TextEdit {
                            range: TextSourceRange { start: 0, end: 0 },
                            text: "x".repeat(MAX_TEXT_EDIT_INSERTED_BYTES + 1),
                            bold: None,
                            italic: None,
                            style: None,
                        }],
                    },
                ),
            )
            .expect_err("bounded inserted bytes"),
        CommandError::TextEditInsertedBytesExceeded
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);

    assert_eq!(
        session
            .execute(
                revision,
                Transaction::single(
                    "Reject final story overflow",
                    Command::ApplyTextEdits {
                        object_id,
                        edits: vec![TextEdit {
                            range: TextSourceRange { start: 1, end: 1 },
                            text: oversized_insertion,
                            bold: None,
                            italic: None,
                            style: None,
                        }],
                    },
                ),
            )
            .expect_err("final story is one byte beyond the model bound"),
        CommandError::InvalidModel(ModelError::TextTooLarge)
    );
    assert_eq!(session.revision(), revision);
    assert_eq!(session.document(), &before);
}

#[test]
fn selected_font_size_preserves_base_and_other_formatting_across_undo_and_reopen() {
    let (mut session, object_id) = text_session("Headé\nBody");
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Bold title",
                Command::UpdateInlineTextStyle {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 0, end: 6 },
                    bold: Some(true),
                    italic: None,
                },
            ),
        )
        .unwrap();
    let before = session.document().clone();
    session
        .execute(
            session.revision(),
            Transaction::single(
                "Title size",
                Command::SetInlineFontSize {
                    object_id: object_id.clone(),
                    range: TextSourceRange { start: 0, end: 6 },
                    font_size_millipoints: 20_000,
                },
            ),
        )
        .unwrap();
    let after = session.document().clone();
    let spans = &story(&session).inline_spans;
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].style.font_size_millipoints, Some(20_000));
    assert_eq!(spans[0].style.bold, Some(true));
    let ObjectKind::Text { style, .. } = &after.pages[0].objects[0].kind else {
        panic!("text");
    };
    assert_eq!(style.font_size_millipoints, 12_000);
    session.undo(session.revision()).unwrap();
    assert_eq!(session.document(), &before);
    session.redo(session.revision()).unwrap();
    assert_eq!(session.document(), &after);
    let snapshot = ProjectSnapshot::from_session(&session);
    let temp = TempDirectory::new("inline-font-size");
    let path = temp.path().join("sized.publisher-v2");
    save_new_package(&path, &snapshot, &AssetStore::default()).unwrap();
    assert_eq!(open_package(&path).unwrap().snapshot, snapshot);
    for (end, size) in [(6, 0), (6, 1_000_001), (5, 20_000)] {
        let revision = session.revision();
        assert!(
            session
                .execute(
                    revision,
                    Transaction::single(
                        "Invalid size or UTF-8 range",
                        Command::SetInlineFontSize {
                            object_id: object_id.clone(),
                            range: TextSourceRange { start: 0, end },
                            font_size_millipoints: size,
                        }
                    )
                )
                .is_err()
        );
        assert_eq!(session.revision(), revision);
        assert_eq!(session.document(), &after);
    }
}
