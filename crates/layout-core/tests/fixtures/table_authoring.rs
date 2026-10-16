use document_core::model::{
    DocumentId, FontFaceBinding, Insets, ObjectDraft, ObjectKind, ParagraphListStyle,
    ParagraphRange, ParagraphStyle, ParagraphStyleRun, PhysicalSize, TableData, TextFrame,
    TextStoryFormatting, TextStyle, Transform,
};
use document_core::{
    AssetHash, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
};
use layout_core::ExactFontResource;

pub fn instruction_page() -> (ProjectSnapshot, ExactFontResource) {
    let bytes = include_bytes!("DejaVuSans.ttf").to_vec();
    let font = ExactFontResource {
        source: AssetHash::for_bytes(&bytes),
        face_index: 0,
        bytes,
    };
    let style = TextStyle {
        font_face: Some(FontFaceBinding {
            source: font.source.clone(),
            face_index: 0,
        }),
        ..TextStyle::default()
    };
    let document = Document::new(
        DocumentId::parse("full-word-search").unwrap(),
        "Word search",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page = document.pages[0].id.clone();
    let mut table = TableData::empty(30, 30).unwrap();
    table.cells = (0..900_u16)
        .map(|index| char::from(b'A' + u8::try_from(index % 26).unwrap()).to_string())
        .collect();
    table.style.font_face.clone_from(&style.font_face);
    table.style.font_size_millipoints = 9_000;
    let items = [
        (
            "Find the words",
            PhysicalSize::new(180_000, 15_000).unwrap(),
            10_000,
            ObjectKind::Text {
                text: "Find the words".into(),
                style: style.clone(),
                frame: TextFrame::default(),
                story: TextStoryFormatting::default(),
            },
        ),
        (
            "Instructions",
            PhysicalSize::new(180_000, 22_000).unwrap(),
            25_000,
            ObjectKind::Text {
                text: "Circle each word\nCheck your answer".into(),
                style,
                frame: TextFrame::default(),
                story: TextStoryFormatting {
                    paragraph_runs: vec![ParagraphStyleRun {
                        range: ParagraphRange { start: 0, end: 2 },
                        style: ParagraphStyle {
                            list_style: Some(ParagraphListStyle::Bullet),
                            ..ParagraphStyle::default()
                        },
                    }],
                    ..TextStoryFormatting::default()
                },
            },
        ),
        (
            "Word search",
            PhysicalSize::new(180_000, 210_000).unwrap(),
            50_000,
            ObjectKind::Table { table },
        ),
    ];
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Compose instruction page".into(),
                commands: items
                    .into_iter()
                    .map(|(name, size, y, kind)| Command::AddObject {
                        location: ObjectLocation::Page(page.clone()),
                        object: ObjectDraft {
                            name: name.into(),
                            size,
                            transform: Transform::identity_at(15_000, y),
                            opacity_millionths: 1_000_000,
                            kind,
                        },
                    })
                    .collect(),
            },
        )
        .unwrap();
    (
        ProjectSnapshot {
            document: session.document().clone(),
            revision: session.revision(),
        },
        font,
    )
}
