use document_core::model::{
    Color, CornerRounding, DocumentId, FontFaceBinding, HardShadow, ImageContentKind, ImageCrop,
    ImageFlip, Insets, Micrometres, ObjectDraft, ObjectEffects, ObjectKind, PhysicalSize,
    ShapeStroke, TableData, TextBoxSizingMode, TextFrame, TextStoryFormatting, TextStyle,
    Transform, WrapMode,
};
use document_core::{
    AssetHash, AssetStore, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot,
    Transaction, inspect_raster,
};
use layout_core::ExactFontResource;

pub const PICTURE: &[u8] =
    include_bytes!("../../../print-core/tests/fixtures/embedded-rgb-alpha-3x2.tif");

#[expect(
    clippy::too_many_lines,
    reason = "One coherent publication fixture preserves fixed/Fit text, Table cells and an oriented transparent Picture through common physical corner paint"
)]
pub fn fixture() -> (ProjectSnapshot, Vec<ExactFontResource>, AssetStore) {
    let fonts = [
        include_bytes!("DejaVuSans.ttf").as_slice(),
        include_bytes!("DejaVuSans-Oblique.ttf").as_slice(),
    ]
    .into_iter()
    .map(|bytes| ExactFontResource {
        source: AssetHash::for_bytes(bytes),
        face_index: 0,
        bytes: bytes.to_vec(),
    })
    .collect::<Vec<_>>();
    let style = TextStyle {
        font_size_millipoints: 9_000,
        font_face: Some(FontFaceBinding {
            source: fonts[0].source.clone(),
            face_index: 0,
        }),
        ..TextStyle::default()
    };
    let mut table = TableData::empty(2, 2).unwrap();
    table.style = style.clone();
    table.cells = vec!["a".into(), "b".into(), "c".into(), "d".into()];
    let kinds = [
        ObjectKind::Text {
            text: "Full text".into(),
            style: style.clone(),
            frame: TextFrame::default(),
            story: TextStoryFormatting::default(),
        },
        ObjectKind::Text {
            text: "jf\nA".into(),
            style: TextStyle {
                italic: true,
                underline: true,
                font_face: Some(FontFaceBinding {
                    source: fonts[1].source.clone(),
                    face_index: 0,
                }),
                ..style
            },
            frame: TextFrame {
                sizing_mode: TextBoxSizingMode::Fit,
                ..TextFrame::default()
            },
            story: TextStoryFormatting::default(),
        },
        ObjectKind::Table { table },
        ObjectKind::Image {
            asset: AssetHash::for_bytes(PICTURE),
            content_kind: ImageContentKind::ContinuousTone,
            crop: ImageCrop::new(125_000, 250_000, 375_000, 250_000).unwrap(),
            flip: ImageFlip {
                horizontal: true,
                vertical: true,
            },
            lock_aspect_ratio: true,
            wrap_mode: WrapMode::None,
        },
    ];
    let document = Document::new(
        DocumentId::parse("common-rounding-fixture").unwrap(),
        "Common rounded objects",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    let page = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            session.revision(),
            Transaction {
                label: "Compose rounded objects".into(),
                commands: kinds
                    .into_iter()
                    .enumerate()
                    .map(|(index, kind)| Command::AddObject {
                        location: ObjectLocation::Page(page.clone()),
                        object: ObjectDraft {
                            name: format!("Object {index}"),
                            size: PhysicalSize::new(25_400, 12_700).unwrap(),
                            transform: Transform::identity_at(
                                10_000,
                                10_000 + i64::try_from(index).unwrap() * 30_000,
                            ),
                            opacity_millionths: 1_000_000,
                            kind,
                        },
                    })
                    .collect(),
            },
        )
        .unwrap();
    let rounding = CornerRounding {
        top_left_um: 6_350,
        top_right_um: 6_350,
        bottom_right_um: 6_350,
        bottom_left_um: 6_350,
        linked: true,
    };
    let effects = ObjectEffects {
        shadow: Some(HardShadow {
            color: Color {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 200,
            },
            offset_x: Micrometres::new(2_540),
            offset_y: Micrometres::new(5_080),
        }),
        ..ObjectEffects::default()
    };
    let mut commands = Vec::new();
    for (index, id) in added.created_objects.into_iter().enumerate() {
        commands.push(Command::UpdateCornerRounding {
            object_id: id.clone(),
            rounding,
        });
        commands.push(Command::UpdateObjectEffects {
            object_id: id,
            effects: if index == 3 {
                ObjectEffects {
                    picture_border: Some(ShapeStroke {
                        width_millipoints: 1_000,
                        ..ShapeStroke::default()
                    }),
                    ..effects
                }
            } else {
                effects
            },
        });
    }
    session
        .execute(
            session.revision(),
            Transaction {
                label: "Round appearances".into(),
                commands,
            },
        )
        .unwrap();
    let mut assets = AssetStore::default();
    for font in &fonts {
        assets.insert("font/ttf", font.bytes.clone());
    }
    assets.insert_raster(PICTURE.to_vec(), inspect_raster(PICTURE).unwrap());
    (ProjectSnapshot::from_session(&session), fonts, assets)
}
