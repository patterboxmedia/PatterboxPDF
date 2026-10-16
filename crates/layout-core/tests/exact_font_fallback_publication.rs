use document_core::model::{
    ExactFontFallbackChain, FontFaceBinding, FontFallbackScript, Insets, ObjectDraft, ObjectKind,
    PhysicalSize, TextFrame, TextStoryFormatting, TextStyle, Transform,
};
use document_core::package::{ProjectSnapshot, open_package, save_new_package};
use document_core::{
    AssetStore, Command, Document, DocumentId, DocumentSession, ObjectLocation, Revision,
    Transaction,
};
use layout_core::{
    ExactFontResource, FontSubstitutionStatus, ResolvedPublication, preview_font_face_bindings,
};

const BASE_FONT: &[u8] = include_bytes!("fixtures/DejaVuSans.ttf");
const FALLBACK_FONT: &[u8] = include_bytes!("fixtures/NotoEmoji-Regular.ttf");
const DEVANAGARI_FONT: &[u8] = include_bytes!("fixtures/NotoSansDevanagari-VF.ttf");
const THAI_FONT: &[u8] = include_bytes!("fixtures/NotoSansThai-VF.ttf");
const CJK_FONT: &[u8] = include_bytes!("fixtures/NotoSansCJKsc-Regular.otf");

fn exact_resource(source: document_core::AssetHash, bytes: &[u8]) -> ExactFontResource {
    ExactFontResource {
        source,
        face_index: 0,
        bytes: bytes.to_vec(),
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The single acceptance story intentionally keeps authored model, package reopen, layout identity, and fallback-glyph assertions together."
)]
fn exact_publication_fallback_survives_package_reopen_and_remains_print_identifiable() {
    let mut assets = AssetStore::default();
    let base_source = assets.insert("font/ttf", BASE_FONT.to_vec());
    let fallback_source = assets.insert("font/ttf", FALLBACK_FONT.to_vec());
    let devanagari_source = assets.insert("font/ttf", DEVANAGARI_FONT.to_vec());
    let thai_source = assets.insert("font/ttf", THAI_FONT.to_vec());
    let cjk_source = assets.insert("font/ttf", CJK_FONT.to_vec());
    let mut document = Document::new(
        DocumentId::parse("exact-fallback-publication").unwrap(),
        "Exact fallback publication",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap();
    document.exact_font_fallbacks = vec![
        fallback_chain(FontFallbackScript::Devanagari, &devanagari_source),
        fallback_chain(FontFallbackScript::Han, &cjk_source),
        fallback_chain(FontFallbackScript::Latin, &fallback_source),
        fallback_chain(FontFallbackScript::Thai, &thai_source),
    ];
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add mixed exact text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Fallback text".to_owned(),
                        size: PhysicalSize::new(120_000, 30_000).unwrap(),
                        transform: Transform::identity_at(10_000, 10_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: "Magic 🚀 हिन्दी ภาษาไทย 中文".to_owned(),
                            style: TextStyle {
                                font_face: Some(FontFaceBinding {
                                    source: base_source.clone(),
                                    face_index: 0,
                                }),
                                font_family: "DejaVu Sans".to_owned(),
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
    let snapshot = ProjectSnapshot::from_session(&session);
    assert_eq!(
        preview_font_face_bindings(&snapshot),
        [
            (base_source.clone(), 0),
            (cjk_source.clone(), 0),
            (devanagari_source.clone(), 0),
            (fallback_source.clone(), 0),
            (thai_source.clone(), 0),
        ]
        .into_iter()
        .collect()
    );
    let resources = vec![
        exact_resource(base_source.clone(), BASE_FONT),
        exact_resource(cjk_source.clone(), CJK_FONT),
        exact_resource(devanagari_source.clone(), DEVANAGARI_FONT),
        exact_resource(fallback_source.clone(), FALLBACK_FONT),
        exact_resource(thai_source.clone(), THAI_FONT),
    ];
    let before =
        ResolvedPublication::resolve_output_with_font_resources(&snapshot, &resources).unwrap();

    let path = std::env::temp_dir().join(format!(
        "publisher-v2-exact-fallback-{}.publisher-v2",
        std::process::id()
    ));
    let _ignored = std::fs::remove_file(&path);
    save_new_package(&path, &snapshot, &assets).unwrap();
    let reopened = open_package(&path).unwrap();
    let reopened_resources = vec![
        exact_resource(
            base_source.clone(),
            &reopened.assets.get(&base_source).unwrap().bytes,
        ),
        exact_resource(
            cjk_source.clone(),
            &reopened.assets.get(&cjk_source).unwrap().bytes,
        ),
        exact_resource(
            devanagari_source.clone(),
            &reopened.assets.get(&devanagari_source).unwrap().bytes,
        ),
        exact_resource(
            fallback_source.clone(),
            &reopened.assets.get(&fallback_source).unwrap().bytes,
        ),
        exact_resource(
            thai_source.clone(),
            &reopened.assets.get(&thai_source).unwrap().bytes,
        ),
    ];
    let after = ResolvedPublication::resolve_output_with_font_resources(
        &reopened.snapshot,
        &reopened_resources,
    )
    .unwrap();
    std::fs::remove_file(&path).unwrap();

    assert_eq!(before, after);
    let layout = after.text_boxes()[0];
    assert_eq!(
        layout.font_binding.substitution_status,
        FontSubstitutionStatus::Exact
    );
    assert_eq!(layout.permitted_fallback_fonts.len(), 4);
    assert_eq!(
        layout
            .permitted_fallback_fonts
            .iter()
            .map(|face| face.sha256_hex.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        [
            cjk_source.as_str(),
            devanagari_source.as_str(),
            fallback_source.as_str(),
            thai_source.as_str(),
        ]
        .into_iter()
        .collect()
    );
    let rocket = layout
        .lines
        .iter()
        .flat_map(|line| &line.glyph_runs)
        .find(|run| run.text.contains('🚀'))
        .unwrap();
    assert_eq!(
        layout.resolved_fonts[rocket.font_index].sha256_hex,
        fallback_source.as_str()
    );
    assert!(
        rocket
            .glyphs
            .iter()
            .all(|glyph| glyph.fallback.is_none() && glyph.outline.is_some())
    );
    for (segment, source) in [
        ("हिन्दी", &devanagari_source),
        ("ภาษาไทย", &thai_source),
        ("中文", &cjk_source),
    ] {
        let runs = layout
            .lines
            .iter()
            .flat_map(|line| &line.glyph_runs)
            .filter(|run| {
                run.text
                    .chars()
                    .any(|character| segment.contains(character))
            })
            .collect::<Vec<_>>();
        assert!(!runs.is_empty());
        assert!(
            runs.iter()
                .all(|run| { layout.resolved_fonts[run.font_index].sha256_hex == source.as_str() })
        );
        let glyphs = runs.iter().flat_map(|run| &run.glyphs).collect::<Vec<_>>();
        assert!(!glyphs.is_empty());
        assert!(
            glyphs
                .iter()
                .all(|glyph| glyph.fallback.is_none() && glyph.outline.is_some())
        );
    }
}

fn fallback_chain(
    script: FontFallbackScript,
    source: &document_core::AssetHash,
) -> ExactFontFallbackChain {
    ExactFontFallbackChain {
        script,
        faces: vec![FontFaceBinding {
            source: source.clone(),
            face_index: 0,
        }],
    }
}
