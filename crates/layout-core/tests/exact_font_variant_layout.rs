use document_core::model::{
    DocumentId, ExactFontVariantBinding, FontFaceBinding, InlineTextSpan, InlineTextStyle, Insets,
    ObjectDraft, ObjectKind, PhysicalSize, Revision, TextFrame, TextSourceRange,
    TextStoryFormatting, TextStyle, Transform,
};
use document_core::{
    AssetHash, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
};
use layout_core::{
    ExactFontResource, FontSubstitutionStatus, ResolvedPublication, TextBoxPreviewState,
    preview_font_face_bindings,
};

const REGULAR: &[u8] = include_bytes!("fixtures/DejaVuSans.ttf");
const BOLD: &[u8] = include_bytes!("fixtures/DejaVuSans-Bold.ttf");
const ITALIC: &[u8] = include_bytes!("fixtures/DejaVuSans-Oblique.ttf");
const BOLD_ITALIC: &[u8] = include_bytes!("fixtures/DejaVuSans-BoldOblique.ttf");

fn resource(bytes: &[u8]) -> ExactFontResource {
    ExactFontResource {
        source: AssetHash::for_bytes(bytes),
        face_index: 0,
        bytes: bytes.to_vec(),
    }
}

fn binding(resource: &ExactFontResource) -> FontFaceBinding {
    FontFaceBinding {
        source: resource.source.clone(),
        face_index: resource.face_index,
    }
}

fn range(text: &str, needle: &str) -> TextSourceRange {
    let start = text.find(needle).expect("fixture token");
    TextSourceRange {
        start,
        end: start + needle.len(),
    }
}

fn project_with_quartet() -> (ProjectSnapshot, Vec<ExactFontResource>) {
    let resources = [REGULAR, BOLD, ITALIC, BOLD_ITALIC]
        .into_iter()
        .map(resource)
        .collect::<Vec<_>>();
    let text = "Regular Bold Italic Both";
    let style = TextStyle {
        font_face: Some(binding(&resources[0])),
        exact_font_variants: vec![
            ExactFontVariantBinding {
                font_weight: 400,
                italic: true,
                face: binding(&resources[2]),
            },
            ExactFontVariantBinding {
                font_weight: 600,
                italic: false,
                face: FontFaceBinding {
                    source: AssetHash::for_bytes(b"unused exact variant"),
                    face_index: 0,
                },
            },
            ExactFontVariantBinding {
                font_weight: 700,
                italic: false,
                face: binding(&resources[1]),
            },
            ExactFontVariantBinding {
                font_weight: 700,
                italic: true,
                face: binding(&resources[3]),
            },
        ],
        font_family: "DejaVu Sans quartet".to_owned(),
        ..TextStyle::default()
    };
    let story = TextStoryFormatting {
        inline_spans: vec![
            InlineTextSpan {
                range: range(text, "Bold"),
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(true),
                    italic: None,
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: range(text, "Italic"),
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: None,
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            },
            InlineTextSpan {
                range: range(text, "Both"),
                style: InlineTextStyle {
                    font_size_millipoints: None,
                    bold: Some(true),
                    italic: Some(true),
                    ..InlineTextStyle::default()
                },
            },
        ],
        paragraph_runs: Vec::new(),
    };
    let document = Document::new(
        DocumentId::parse("exact-font-variant-layout").unwrap(),
        "Exact variant layout",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(0).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add quartet text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Quartet".to_owned(),
                        size: PhysicalSize::new(180_000, 50_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: text.to_owned(),
                            style,
                            frame: TextFrame::default(),
                            story,
                        },
                    },
                },
            ),
        )
        .unwrap();
    (ProjectSnapshot::from_session(&session), resources)
}

#[test]
fn captured_quartet_resolves_every_effective_range_without_synthesis() {
    let (project, resources) = project_with_quartet();
    let reachable = preview_font_face_bindings(&project);
    assert_eq!(
        reachable,
        resources
            .iter()
            .map(|resource| (resource.source.clone(), resource.face_index))
            .collect()
    );

    let publication = ResolvedPublication::resolve_with_font_resources(&project, &resources)
        .expect("resolve captured quartet");
    let layout = publication.text_boxes()[0];
    assert_eq!(layout.preview_state, TextBoxPreviewState::Complete);
    assert_eq!(
        layout.font_bindings.len(),
        6,
        "three styled islands split base gaps"
    );
    assert!(layout.font_bindings.iter().all(|binding| {
        binding.substitution_status == FontSubstitutionStatus::Exact && binding.exact_face.is_some()
    }));
    for binding in &layout.font_bindings {
        let expected = binding.exact_face.as_ref().expect("captured range face");
        assert!(
            layout
                .lines
                .iter()
                .flat_map(|line| &line.glyph_runs)
                .filter(|run| {
                    run.list_marker_paragraph.is_none()
                        && binding.source.start <= run.source.start
                        && run.source.end <= binding.source.end
                })
                .all(|run| &layout.resolved_fonts[run.font_index] == expected),
            "each source range must resolve through its own captured face"
        );
    }
    assert!(
        layout
            .lines
            .iter()
            .flat_map(|line| &line.glyph_runs)
            .all(|run| {
                !run.requires_font_synthesis
                    && resources.iter().any(|resource| {
                        let actual = &layout.resolved_fonts[run.font_index];
                        actual.sha256_hex == resource.source.as_str()
                            && actual.collection_index == resource.face_index
                    })
            })
    );
}

#[test]
fn explicitly_bound_missing_variant_never_falls_back_to_the_base_face() {
    let (project, resources) = project_with_quartet();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &project,
        &[
            resources[0].clone(),
            resources[2].clone(),
            resources[3].clone(),
        ],
    )
    .expect("missing variants remain an explicit preview state");
    let layout = publication.text_boxes()[0];
    assert_eq!(layout.preview_state, TextBoxPreviewState::MissingFont);
    assert!(layout.lines.is_empty());
    let bold = layout
        .font_bindings
        .iter()
        .find(|binding| binding.requested_weight == 700 && !binding.requested_italic)
        .expect("bold binding fact");
    assert_eq!(bold.substitution_status, FontSubstitutionStatus::Missing);
    assert_eq!(
        bold.exact_face
            .as_ref()
            .map(|face| face.sha256_hex.as_str()),
        Some(resources[1].source.as_str())
    );
}

#[test]
fn nearest_captured_weight_uses_the_real_face_without_synthesis() {
    let bold = resource(BOLD);
    let document = Document::new(
        DocumentId::parse("nearest-weight-layout").unwrap(),
        "Nearest weight layout",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(0).unwrap(),
    )
    .unwrap();
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add nearest-weight text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Semibold request".to_owned(),
                        size: PhysicalSize::new(120_000, 30_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Real nearest face".to_owned(),
                            style: TextStyle {
                                font_face: Some(binding(&bold)),
                                font_family: "DejaVu Sans".to_owned(),
                                font_weight: 650,
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story: TextStoryFormatting::default(),
                        },
                    },
                },
            ),
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);
    let publication = ResolvedPublication::resolve_with_font_resources(&project, &[bold])
        .expect("resolve nearest captured weight");
    let layout = publication.text_boxes()[0];

    assert_eq!(
        layout.font_binding.substitution_status,
        FontSubstitutionStatus::Exact
    );
    assert!(
        layout
            .lines
            .iter()
            .flat_map(|line| &line.glyph_runs)
            .all(|run| !run.requires_font_synthesis),
        "the captured 700 face must not be synthetically altered to satisfy authored weight 650"
    );
}

#[test]
fn mixed_family_color_and_underline_follow_exact_source_ranges() {
    let (mut project, resources) = project_with_quartet();
    let ObjectKind::Text {
        text, style, story, ..
    } = &mut project.document.pages[0].objects[0].kind
    else {
        panic!("text");
    };
    *text = "Body Bold Body".to_owned();
    style.exact_font_variants.clear();
    style.exact_font_families = vec![document_core::model::ExactFontFamilyBinding {
        font_family: "Captured alternate family".to_owned(),
        font_weight: 700,
        italic: false,
        face: binding(&resources[1]),
    }];
    let selected_color = document_core::model::Color {
        red: 210,
        green: 30,
        blue: 40,
        alpha: 120,
    };
    story.inline_spans = vec![InlineTextSpan {
        range: TextSourceRange { start: 5, end: 9 },
        style: InlineTextStyle {
            font_family: Some("Captured alternate family".to_owned()),
            font_weight: Some(700),
            color: Some(selected_color),
            underline: Some(true),
            ..InlineTextStyle::default()
        },
    }];
    story.paragraph_runs.clear();
    let reachable = preview_font_face_bindings(&project);
    assert_eq!(reachable.len(), 2);
    assert!(reachable.contains(&(resources[0].source.clone(), 0)));
    assert!(reachable.contains(&(resources[1].source.clone(), 0)));
    let publication =
        ResolvedPublication::resolve_output_with_font_resources(&project, &resources[..2]).unwrap();
    let layout = publication.text_boxes()[0];
    assert_eq!(layout.font_bindings.len(), 3);
    assert_eq!(
        layout.font_bindings[1].requested_family,
        "Captured alternate family"
    );
    assert_eq!(
        layout.font_bindings[1]
            .exact_face
            .as_ref()
            .unwrap()
            .sha256_hex,
        resources[1].source.as_str()
    );
    assert!(
        layout
            .font_bindings
            .iter()
            .all(|binding| binding.substitution_status == FontSubstitutionStatus::Exact)
    );
    for run in layout.lines.iter().flat_map(|line| &line.glyph_runs) {
        assert!(!run.requires_font_synthesis);
        let is_selected = run.source.start >= 5 && run.source.end <= 9;
        assert_eq!(
            run.color,
            if is_selected {
                selected_color
            } else {
                TextStyle::default().color
            }
        );
        assert_eq!(run.underline.is_some(), is_selected);
        let expected = if is_selected {
            &resources[1]
        } else {
            &resources[0]
        };
        assert_eq!(
            layout.resolved_fonts[run.font_index].sha256_hex,
            expected.source.as_str()
        );
        if let Some(underline) = &run.underline {
            assert!(underline.width.value() > 0 && underline.thickness.value() > 0);
            assert!(underline.y.value() > run.glyphs[0].y.value());
        }
    }
}
