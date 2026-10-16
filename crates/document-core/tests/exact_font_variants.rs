mod common;

use common::letter_document;
use document_core::assets::AssetHash;
use document_core::commands::{
    Command, DocumentSession, FontFaceBindingUpdate, ObjectLocation, Transaction,
};
use document_core::model::{
    ExactFontVariantBinding, FontFaceBinding, InlineTextSpan, InlineTextStyle,
    MAX_EXACT_FONT_VARIANTS, ModelError, ObjectDraft, ObjectKind, PhysicalSize, Revision,
    TextFrame, TextSourceRange, TextStoryFormatting, TextStyle, Transform,
    effective_font_variant_ranges,
};

fn face(label: &[u8], face_index: u32) -> FontFaceBinding {
    FontFaceBinding {
        source: AssetHash::for_bytes(label),
        face_index,
    }
}

fn variant(weight: u16, italic: bool, label: &[u8]) -> ExactFontVariantBinding {
    ExactFontVariantBinding {
        font_weight: weight,
        italic,
        face: face(label, 0),
    }
}

fn text_session(
    style: TextStyle,
) -> Result<(DocumentSession, document_core::ObjectId), document_core::CommandError> {
    let mut session = DocumentSession::new(letter_document())?;
    let added = session.execute(
        Revision::initial(),
        Transaction::single(
            "Add text",
            Command::AddObject {
                location: ObjectLocation::Pasteboard,
                object: ObjectDraft {
                    name: "Story".to_owned(),
                    size: PhysicalSize::new(50_000, 50_000).expect("positive size"),
                    transform: Transform::identity_at(0, 0),
                    opacity_millionths: 1_000_000,
                    kind: ObjectKind::Text {
                        text: "Regular Bold Italic".to_owned(),
                        style,
                        frame: TextFrame::default(),
                        story: TextStoryFormatting::default(),
                    },
                },
            },
        ),
    )?;
    Ok((session, added.created_objects[0].clone()))
}

fn text_style(session: &DocumentSession) -> &TextStyle {
    let ObjectKind::Text { style, .. } = &session.document().pasteboard.objects[0].kind else {
        panic!("fixture remains text");
    };
    style
}

fn text_story(session: &DocumentSession) -> &TextStoryFormatting {
    let ObjectKind::Text { story, .. } = &session.document().pasteboard.objects[0].kind else {
        panic!("fixture remains text");
    };
    story
}

#[test]
fn effective_font_ranges_share_one_linear_coalesced_derivation() {
    let style = TextStyle {
        font_weight: 500,
        italic: false,
        ..TextStyle::default()
    };
    let story = TextStoryFormatting {
        inline_spans: vec![
            InlineTextSpan {
                range: TextSourceRange { start: 2, end: 4 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(true),
                    italic: None,
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: TextSourceRange { start: 4, end: 6 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(true),
                    italic: Some(false),
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: TextSourceRange { start: 6, end: 8 },
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: None,
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            },
        ],
        paragraph_runs: Vec::new(),
    };

    let ranges =
        effective_font_variant_ranges(&style, &story, TextSourceRange { start: 1, end: 9 });
    assert_eq!(
        ranges
            .iter()
            .map(|range| (
                range.range.start,
                range.range.end,
                range.font_weight,
                range.italic,
            ))
            .collect::<Vec<_>>(),
        vec![
            (1, 2, 500, false),
            (2, 6, 700, false),
            (6, 8, 500, true),
            (8, 9, 500, false),
        ]
    );
}

#[test]
fn exact_font_variant_registry_rejects_duplicates_order_and_bounds() {
    for variants in [
        vec![
            variant(700, false, b"bold-a"),
            variant(700, false, b"bold-b"),
        ],
        vec![
            variant(700, true, b"bold-italic"),
            variant(700, false, b"bold"),
        ],
    ] {
        let style = TextStyle {
            exact_font_variants: variants,
            ..TextStyle::default()
        };
        assert!(matches!(
            text_session(style),
            Err(document_core::CommandError::InvalidModel(
                ModelError::ExactFontVariantsNotNormalized
            ))
        ));
    }

    let style = TextStyle {
        exact_font_variants: (0..=MAX_EXACT_FONT_VARIANTS)
            .map(|index| {
                variant(
                    500 + u16::try_from(index).unwrap(),
                    false,
                    &index.to_le_bytes(),
                )
            })
            .collect(),
        ..TextStyle::default()
    };
    assert!(matches!(
        text_session(style),
        Err(document_core::CommandError::InvalidModel(
            ModelError::TooManyExactFontVariants
        ))
    ));
}

#[test]
fn exact_font_variant_registry_rejects_invalid_and_base_style_keys() {
    let invalid_weight = TextStyle {
        exact_font_variants: vec![variant(0, false, b"invalid")],
        ..TextStyle::default()
    };
    assert!(matches!(
        text_session(invalid_weight),
        Err(document_core::CommandError::InvalidModel(
            ModelError::FontWeightOutOfRange
        ))
    ));

    let duplicates_base = TextStyle {
        font_weight: 700,
        italic: true,
        exact_font_variants: vec![variant(700, true, b"duplicate-base")],
        ..TextStyle::default()
    };
    assert!(matches!(
        text_session(duplicates_base),
        Err(document_core::CommandError::InvalidModel(
            ModelError::ExactFontVariantMatchesBaseStyle
        ))
    ));
}

#[test]
fn exact_face_lookup_never_substitutes_another_style_key() {
    let regular = face(b"regular", 0);
    let italic = face(b"italic", 1);
    let bold = face(b"bold", 2);
    let style = TextStyle {
        font_face: Some(regular.clone()),
        exact_font_variants: vec![
            ExactFontVariantBinding {
                font_weight: 400,
                italic: true,
                face: italic.clone(),
            },
            ExactFontVariantBinding {
                font_weight: 700,
                italic: false,
                face: bold.clone(),
            },
        ],
        ..TextStyle::default()
    };

    assert_eq!(style.exact_font_face_for(400, false), Some(&regular));
    assert_eq!(style.exact_font_face_for(400, true), Some(&italic));
    assert_eq!(style.exact_font_face_for(700, false), Some(&bold));
    assert_eq!(style.exact_font_face_for(700, true), None);
}

#[test]
fn whole_object_style_edits_preserve_variants_only_for_the_same_family() {
    let original = TextStyle {
        font_face: Some(face(b"regular", 0)),
        exact_font_variants: vec![variant(400, true, b"italic"), variant(700, false, b"bold")],
        ..TextStyle::default()
    };
    let expected_variants = original.exact_font_variants.clone();
    let (mut session, object_id) = text_session(original).expect("valid exact variants");

    let same_family = session
        .execute(
            session.revision(),
            Transaction::single(
                "Change color",
                Command::UpdateTextStyle {
                    object_id: object_id.clone(),
                    style: TextStyle {
                        color: document_core::model::Color {
                            red: 10,
                            green: 20,
                            blue: 30,
                            alpha: 255,
                        },
                        ..TextStyle::default()
                    },
                    font_face_update: FontFaceBindingUpdate::PreserveIfFamilyUnchanged,
                },
            ),
        )
        .expect("same-family edit");
    assert_eq!(text_style(&session).exact_font_variants, expected_variants);

    let base_key_changed = session
        .execute(
            same_family.revision,
            Transaction::single(
                "Make base style bold",
                Command::UpdateTextStyle {
                    object_id: object_id.clone(),
                    style: TextStyle {
                        font_weight: 700,
                        ..TextStyle::default()
                    },
                    font_face_update: FontFaceBindingUpdate::PreserveIfFamilyUnchanged,
                },
            ),
        )
        .expect("base-key edit");
    assert_eq!(text_style(&session).font_face, Some(face(b"bold", 0)));
    assert_eq!(
        text_style(&session).exact_font_variants,
        vec![
            variant(400, false, b"regular"),
            variant(400, true, b"italic"),
        ],
        "the promoted variant and prior base exchange canonical positions without data loss"
    );

    session
        .execute(
            base_key_changed.revision,
            Transaction::single(
                "Change family",
                Command::UpdateTextStyle {
                    object_id,
                    style: TextStyle {
                        font_family: "Different Family".to_owned(),
                        ..TextStyle::default()
                    },
                    font_face_update: FontFaceBindingUpdate::PreserveIfFamilyUnchanged,
                },
            ),
        )
        .expect("family edit");
    assert!(text_style(&session).exact_font_variants.is_empty());
}

#[test]
fn exact_variant_upsert_and_inline_style_are_one_undoable_transaction() {
    let (mut session, object_id) = text_session(TextStyle::default()).expect("valid text");
    let first_bold_face = face(b"bold-first", 0);
    let applied = session
        .execute(
            session.revision(),
            Transaction {
                label: "Bind and apply bold".to_owned(),
                commands: vec![
                    Command::SetTextFontVariantBinding {
                        object_id: object_id.clone(),
                        font_weight: 700,
                        italic: false,
                        font_face: Some(first_bold_face.clone()),
                    },
                    Command::UpdateInlineTextStyle {
                        object_id: object_id.clone(),
                        range: TextSourceRange { start: 8, end: 12 },
                        bold: Some(true),
                        italic: None,
                    },
                ],
            },
        )
        .expect("bind exact bold face and style source range");
    assert_eq!(
        text_style(&session).exact_font_variants,
        vec![ExactFontVariantBinding {
            font_weight: 700,
            italic: false,
            face: first_bold_face.clone(),
        }]
    );
    assert_eq!(text_story(&session).inline_spans.len(), 1);

    let undone = session.undo(applied.revision).expect("undo transaction");
    assert!(text_style(&session).exact_font_variants.is_empty());
    assert!(text_story(&session).inline_spans.is_empty());
    let redone = session.redo(undone).expect("redo transaction");
    assert_eq!(
        text_style(&session).exact_font_variants[0].face,
        first_bold_face
    );
    assert_eq!(text_story(&session).inline_spans.len(), 1);

    let replacement = face(b"bold-replacement", 2);
    let replaced = session
        .execute(
            redone,
            Transaction::single(
                "Replace exact bold face",
                Command::SetTextFontVariantBinding {
                    object_id: object_id.clone(),
                    font_weight: 700,
                    italic: false,
                    font_face: Some(replacement.clone()),
                },
            ),
        )
        .expect("replace exact variant");
    assert_eq!(
        text_style(&session).exact_font_variants[0].face,
        replacement
    );

    let removed = session
        .execute(
            replaced.revision,
            Transaction::single(
                "Remove exact bold face",
                Command::SetTextFontVariantBinding {
                    object_id,
                    font_weight: 700,
                    italic: false,
                    font_face: None,
                },
            ),
        )
        .expect("remove exact variant");
    assert!(text_style(&session).exact_font_variants.is_empty());
    session.undo(removed.revision).expect("undo removal");
    assert_eq!(
        text_style(&session).exact_font_variants[0].face,
        replacement
    );
}

#[test]
fn exact_variant_commands_normalize_order_and_fail_atomically_at_the_bound() {
    let (mut session, object_id) = text_session(TextStyle::default()).expect("valid text");
    for (weight, italic) in [(700, true), (400, true), (700, false)] {
        session
            .execute(
                session.revision(),
                Transaction::single(
                    "Insert exact variant",
                    Command::SetTextFontVariantBinding {
                        object_id: object_id.clone(),
                        font_weight: weight,
                        italic,
                        font_face: Some(face(format!("{weight}-{italic}").as_bytes(), 0)),
                    },
                ),
            )
            .expect("variant insert");
    }
    assert_eq!(
        text_style(&session)
            .exact_font_variants
            .iter()
            .map(|variant| (variant.font_weight, variant.italic))
            .collect::<Vec<_>>(),
        vec![(400, true), (700, false), (700, true)]
    );

    let full_style = TextStyle {
        font_face: Some(face(b"regular", 0)),
        exact_font_variants: [
            (100, false),
            (100, true),
            (200, false),
            (200, true),
            (300, false),
            (300, true),
            (400, true),
            (500, false),
        ]
        .into_iter()
        .map(|(weight, italic)| variant(weight, italic, format!("{weight}-{italic}").as_bytes()))
        .collect(),
        ..TextStyle::default()
    };
    let (mut full_session, full_object_id) = text_session(full_style).expect("full registry");
    let before = full_session.document().clone();
    let error = full_session
        .execute(
            full_session.revision(),
            Transaction {
                label: "Reject ninth exact variant".to_owned(),
                commands: vec![
                    Command::UpdateInlineTextStyle {
                        object_id: full_object_id.clone(),
                        range: TextSourceRange { start: 0, end: 7 },
                        bold: Some(true),
                        italic: None,
                    },
                    Command::SetTextFontVariantBinding {
                        object_id: full_object_id,
                        font_weight: 500,
                        italic: true,
                        font_face: Some(face(b"ninth", 0)),
                    },
                ],
            },
        )
        .expect_err("bounded registry");
    assert!(matches!(
        error,
        document_core::CommandError::InvalidModel(ModelError::TooManyExactFontVariants)
    ));
    assert_eq!(full_session.document(), &before, "transaction rolls back");

    let overflow = full_session
        .execute(
            full_session.revision(),
            Transaction::single(
                "Reject lossy base-key change",
                Command::UpdateTextStyle {
                    object_id: full_session.document().pasteboard.objects[0].id.clone(),
                    style: TextStyle {
                        font_weight: 600,
                        ..TextStyle::default()
                    },
                    font_face_update: FontFaceBindingUpdate::PreserveIfFamilyUnchanged,
                },
            ),
        )
        .expect_err("changing the base key cannot silently discard the prior base face");
    assert!(matches!(
        overflow,
        document_core::CommandError::InvalidModel(ModelError::TooManyExactFontVariants)
    ));
    assert_eq!(full_session.document(), &before);
}
