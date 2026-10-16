use document_core::model::{
    Color, DocumentId, FontFaceBinding, Insets, ObjectDraft, ObjectKind, PageNumberFormat,
    PageNumberingSection, PhysicalSize, TextField, TextFrame, TextStoryFormatting, TextStyle,
    Transform,
};
use document_core::{
    AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
    open_package, save_new_package,
};
use layout_core::{ExactFontResource, ResolvedPublication, ResolvedSceneObjectKind};
use print_core::{ScenePdfOptions, ScenePdfWriter, TrustedFontBoundary, prepare_publication_fonts};
use sha2::{Digest, Sha256};

fn field(resource: &ExactFontResource) -> ObjectDraft {
    ObjectDraft {
        name: "Automatic page number".into(),
        size: PhysicalSize::new(50_000, 25_000).unwrap(),
        transform: Transform::identity_at(10_000, 240_000),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Text {
            text: String::new(),
            style: TextStyle {
                font_face: Some(FontFaceBinding {
                    source: resource.source.clone(),
                    face_index: 0,
                }),
                font_family: "DejaVu Sans".into(),
                font_size_millipoints: 24_000,
                color: Color {
                    red: 25,
                    green: 55,
                    blue: 125,
                    alpha: 255,
                },
                underline: true,
                ..TextStyle::default()
            },
            frame: TextFrame::default(),
            story: TextStoryFormatting::default(),
        },
    }
}

fn serialize(project: &ProjectSnapshot, resource: &ExactFontResource) -> Vec<u8> {
    let publication = ResolvedPublication::resolve_output_with_font_resources(
        project,
        core::slice::from_ref(resource),
    )
    .unwrap();
    assert!(publication.pages()[0].objects.is_empty());
    assert!(publication.pages()[2].objects.is_empty());
    for (index, expected) in [(1, "IX"), (3, "42")] {
        assert_eq!(publication.pages()[index].objects.len(), 1);
        let object = &publication.pages()[index].objects[0];
        assert!(object.master_id.is_some());
        let ResolvedSceneObjectKind::Text { layout, .. } = &object.kind else {
            panic!("number must resolve as text")
        };
        assert_eq!(layout.lines[0].text, expected);
    }
    let fonts = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(resource),
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();
    assert_eq!(fonts.len(), 1);
    let expected_source: [u8; 32] = Sha256::digest(&resource.bytes).into();
    assert_eq!(fonts[0].evidence().source_sha256, expected_source);
    assert!(fonts[0].evidence().used_glyph_count >= 4);
    ScenePdfWriter::write_with_rasters_and_fonts(
        &publication,
        ScenePdfOptions::default(),
        &[],
        &fonts,
    )
    .unwrap()
    .bytes()
    .to_vec()
}

#[test]
#[allow(clippy::too_many_lines)] // One output journey verifies semantic fields, section rules, exact fonts and reopened bytes.
fn shared_semantic_numbers_use_exact_worker_fonts_and_identical_reopened_output() {
    let mut assets = AssetStore::default();
    let bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let source = assets.insert("font/ttf", bytes.clone());
    let resource = ExactFontResource {
        source,
        face_index: 0,
        bytes,
    };
    let trim = PhysicalSize::new(215_900, 279_400).unwrap();
    let mut session = DocumentSession::new(
        Document::new_with_page_count(
            DocumentId::parse("master-number-print").unwrap(),
            "Numbered book",
            trim,
            Insets::default(),
            4,
        )
        .unwrap(),
    )
    .unwrap();
    let pages = session
        .document()
        .pages
        .iter()
        .map(|page| page.id.clone())
        .collect::<Vec<_>>();
    let master = session
        .execute(
            session.revision(),
            Transaction::single(
                "Master",
                Command::AddMaster {
                    name: "Numbered footer".into(),
                    trim,
                },
            ),
        )
        .unwrap()
        .created_masters[0]
        .clone();
    let number = session
        .execute(
            session.revision(),
            Transaction::single(
                "Number",
                Command::AddObject {
                    location: ObjectLocation::Master(master.clone()),
                    object: field(&resource),
                },
            ),
        )
        .unwrap()
        .created_objects[0]
        .clone();
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Sections".into(),
                commands: vec![
                    Command::SetTextField {
                        object_id: number,
                        field: Some(TextField::PageNumber),
                    },
                    Command::AssignMaster {
                        page_ids: pages[1..].to_vec(),
                        master_id: Some(master),
                    },
                    Command::UpdatePageNumbering {
                        page_id: pages[1].clone(),
                        section: Some(PageNumberingSection {
                            format: PageNumberFormat::RomanUpper,
                            start: Some(9),
                        }),
                    },
                    Command::SetPageNumberSuppression {
                        page_ids: vec![pages[2].clone()],
                        suppressed: true,
                    },
                    Command::UpdatePageNumbering {
                        page_id: pages[3].clone(),
                        section: Some(PageNumberingSection {
                            format: PageNumberFormat::Decimal,
                            start: Some(42),
                        }),
                    },
                ],
            },
        )
        .unwrap();
    let project = ProjectSnapshot::from_session(&session);
    let pdf = serialize(&project, &resource);
    for expected in ["I", "X", "4", "2"] {
        let mut marked = pdf_writer::Content::new();
        marked
            .begin_marked_content_with_properties(pdf_writer::Name(b"Span"))
            .properties()
            .actual_text(pdf_writer::TextStr(expected));
        let span = marked.finish();
        assert!(pdf.windows(span.len()).any(|part| part == span.as_slice()));
    }
    let path = std::env::temp_dir().join(format!(
        "publisher-master-print-{}.publisher",
        std::process::id()
    ));
    save_new_package(&path, &project, &assets).unwrap();
    let reopened = open_package(&path).unwrap();
    assert_eq!(reopened.snapshot, project);
    assert!(
        matches!(&reopened.snapshot.document.masters[0].objects[0].kind, ObjectKind::Text { text, .. } if text.is_empty())
    );
    assert_eq!(serialize(&reopened.snapshot, &resource), pdf);
    std::fs::remove_file(path).unwrap();
}
