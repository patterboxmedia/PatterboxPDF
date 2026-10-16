use document_core::AssetHash;
use layout_core::text::{
    Millipoints, ResolvedGlyphRun, ResolvedTextLayout, SourceRange, StoryStyle, TextLayoutEngine,
    TextRect, TextRegion, TextStory,
};
use layout_core::{ExactFontRegistrationError, ExactFontResource, ResolvedGlyphFallback};
use sha2::Digest;

const FONT_BYTES: &[u8] = include_bytes!("fixtures/DejaVuSans.ttf");
const EMOJI_FALLBACK_BYTES: &[u8] = include_bytes!("fixtures/NotoEmoji-Regular.ttf");
const COLRV1_FONT_BYTES: &[u8] = include_bytes!("fixtures/Noto-COLRv1-noflags.ttf");
const DEVANAGARI_FONT_BYTES: &[u8] = include_bytes!("fixtures/NotoSansDevanagari-VF.ttf");
const THAI_FONT_BYTES: &[u8] = include_bytes!("fixtures/NotoSansThai-VF.ttf");
const CJK_FONT_BYTES: &[u8] = include_bytes!("fixtures/NotoSansCJKsc-Regular.otf");
const SVG_FONT_BYTES: &[u8] = include_bytes!("fixtures/NotoColorEmoji-SVG.otf");
const BITMAP_FONT_BYTES: &[u8] = include_bytes!("fixtures/NotoColorEmoji-CBDT.ttf");
// Keep the authored samples beside this suite so independent source packages build.
const CORPUS: &str = include_str!("fixtures/unicode-shaping-corpus.txt");

fn corpus_text(name: &str) -> &'static str {
    CORPUS
        .lines()
        .find_map(|line| {
            let (key, text) = line.split_once('|')?;
            (key == name).then_some(text)
        })
        .unwrap_or_else(|| panic!("missing {name:?} Unicode shaping fixture"))
}

fn exact_engine() -> (TextLayoutEngine, String) {
    let resource = ExactFontResource {
        source: AssetHash::for_bytes(FONT_BYTES),
        face_index: 0,
        bytes: FONT_BYTES.to_vec(),
    };
    let mut engine = TextLayoutEngine::new();
    let family = engine
        .register_exact_font(&resource)
        .expect("the checked-in DejaVu Sans fixture must register");
    (engine, family)
}

fn resolve(engine: &mut TextLayoutEngine, family: &str, text: &str) -> ResolvedTextLayout {
    let story = TextStory {
        text: text.to_owned(),
        base_style: StoryStyle::new(1, family),
        spans: Vec::new(),
    };
    let region = TextRegion {
        bounds: TextRect::new(0, 0, 500_000, 100_000).unwrap(),
        column_count: 1,
        column_gap: Millipoints::new(0),
        exclusions: Vec::new(),
    };
    engine.resolve(&story, &region).unwrap()
}

fn runs(layout: &ResolvedTextLayout) -> Vec<&ResolvedGlyphRun> {
    layout
        .lines
        .iter()
        .flat_map(|line| &line.fragments)
        .flat_map(|fragment| &fragment.glyph_runs)
        .collect()
}

fn slices<'a>(text: &'a str, layout: &'a ResolvedTextLayout) -> Vec<&'a str> {
    runs(layout)
        .iter()
        .map(|run| &text[run.source.start..run.source.end])
        .collect()
}

#[test]
fn latin_punctuation_and_fi_ligature_keep_exact_utf8_source_clusters() {
    let text = corpus_text("latin");
    let (mut engine, family) = exact_engine();
    let layout = resolve(&mut engine, &family, text);
    let resolved_runs = runs(&layout);

    assert_eq!(
        slices(text, &layout),
        vec![
            "f", "i", " ", "—", " ", "“", "c", "a", "f", "é", "”", " ", "…"
        ]
    );
    assert_eq!(resolved_runs[0].source, SourceRange { start: 0, end: 1 });
    assert_eq!(resolved_runs[0].glyphs.len(), 1);
    assert_eq!(resolved_runs[0].glyphs[0].id, 5_042);
    assert_eq!(
        resolved_runs[0].glyph_source,
        SourceRange { start: 0, end: 2 }
    );
    assert_eq!(resolved_runs[1].source, SourceRange { start: 1, end: 2 });
    assert!(
        resolved_runs[1].glyphs.is_empty(),
        "the i source component must point back to the glyph carried by the f component"
    );
    assert!(resolved_runs.iter().all(|run| !run.right_to_left));
    assert!(layout.overset.is_none());
}

#[test]
fn decomposed_combining_mark_keeps_exact_component_ranges_for_one_composed_glyph() {
    let text = corpus_text("combining");
    assert_eq!(
        text.as_bytes(),
        b"Cafe\xCC\x81",
        "fixture must remain decomposed"
    );
    let (mut engine, family) = exact_engine();
    let layout = resolve(&mut engine, &family, text);
    let resolved_runs = runs(&layout);

    assert_eq!(slices(text, &layout), vec!["C", "a", "f", "e", "́"]);
    assert_eq!(resolved_runs[3].source, SourceRange { start: 3, end: 4 });
    assert_eq!(resolved_runs[3].glyphs.len(), 1);
    assert_eq!(resolved_runs[3].glyphs[0].id, 171);
    assert_eq!(
        resolved_runs[3].glyph_source,
        SourceRange { start: 3, end: 6 }
    );
    assert_eq!(resolved_runs[4].source, SourceRange { start: 4, end: 6 });
    assert!(
        resolved_runs[4].glyphs.is_empty(),
        "the combining-mark source component must map to the composed glyph carried by e"
    );
}

#[test]
fn mixed_bidi_text_emits_rtl_clusters_in_visual_order_with_logical_ranges() {
    let text = corpus_text("rtl");
    let (mut engine, family) = exact_engine();
    let layout = resolve(&mut engine, &family, text);
    let resolved_runs = runs(&layout);
    let rtl_runs = resolved_runs
        .iter()
        .copied()
        .filter(|run| run.right_to_left)
        .collect::<Vec<_>>();

    assert_eq!(
        rtl_runs
            .iter()
            .map(|run| run.source.clone())
            .collect::<Vec<_>>(),
        vec![
            SourceRange { start: 10, end: 12 },
            SourceRange { start: 8, end: 10 },
            SourceRange { start: 6, end: 8 },
            SourceRange { start: 4, end: 6 },
        ]
    );
    assert_eq!(
        rtl_runs
            .iter()
            .map(|run| &text[run.source.start..run.source.end])
            .collect::<Vec<_>>(),
        vec!["ם", "ו", "ל", "ש"]
    );
    assert!(
        rtl_runs
            .windows(2)
            .all(|pair| pair[0].glyphs[0].x < pair[1].glyphs[0].x)
    );
    assert_eq!(layout.lines[0].source, SourceRange { start: 0, end: 16 });
}

#[test]
fn arabic_complex_script_shapes_without_losing_source_coverage() {
    let text = corpus_text("arabic");
    let (mut engine, family) = exact_engine();
    let layout = resolve(&mut engine, &family, text);
    let rtl_runs = runs(&layout)
        .into_iter()
        .filter(|run| run.right_to_left)
        .collect::<Vec<_>>();

    assert_eq!(
        rtl_runs
            .iter()
            .map(|run| run.source.clone())
            .collect::<Vec<_>>(),
        vec![
            SourceRange { start: 10, end: 12 },
            SourceRange { start: 8, end: 10 },
            SourceRange { start: 6, end: 8 },
            SourceRange { start: 4, end: 6 },
        ]
    );
    assert_eq!(
        rtl_runs
            .iter()
            .map(|run| &text[run.source.start..run.source.end])
            .collect::<Vec<_>>(),
        vec!["م", "ا", "ل", "س"]
    );
    assert_eq!(rtl_runs[1].glyphs.len(), 1);
    assert_eq!(rtl_runs[1].glyphs[0].id, 5_366);
    assert_eq!(
        &text[rtl_runs[1].glyph_source.start..rtl_runs[1].glyph_source.end],
        "لا"
    );
    assert!(
        rtl_runs[2].glyphs.is_empty(),
        "lam must point back to the lam-alef glyph carried by the alef component"
    );
    assert!(
        rtl_runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .all(|glyph| { glyph.outline.is_some() && glyph.fallback.is_none() })
    );
}

#[test]
fn fixture_emoji_is_reported_as_a_monochrome_outline_not_a_color_fallback() {
    let text = corpus_text("emoji");
    let emoji_start = text.find('😀').unwrap();
    let emoji_end = emoji_start + '😀'.len_utf8();
    let (mut engine, family) = exact_engine();
    let layout = resolve(&mut engine, &family, text);
    let emoji = runs(&layout)
        .into_iter()
        .find(|run| {
            run.source
                == SourceRange {
                    start: emoji_start,
                    end: emoji_end,
                }
        })
        .expect("emoji must retain its exact four-byte source range");

    assert_eq!(&text[emoji.source.start..emoji.source.end], "😀");
    assert_eq!(emoji.glyphs.len(), 1);
    match emoji.glyphs[0].fallback {
        None => assert!(emoji.glyphs[0].outline.is_some()),
        Some(
            ResolvedGlyphFallback::Missing
            | ResolvedGlyphFallback::Colr
            | ResolvedGlyphFallback::Svg
            | ResolvedGlyphFallback::Bitmap,
        ) => {
            assert!(emoji.glyphs[0].outline.is_none());
        }
    }
    assert_eq!(
        emoji.glyphs[0].fallback, None,
        "DejaVu's monochrome emoji outline must not be mislabeled as color or bitmap"
    );
    assert_eq!(
        layout.fonts.len(),
        1,
        "exact fixture should not use system fallback"
    );
}

#[test]
fn source_bearing_notdef_glyph_is_reported_as_missing() {
    let text = "Magic \u{10FFFF} trick";
    let missing_start = text.find('\u{10FFFF}').unwrap();
    let missing_end = missing_start + '\u{10FFFF}'.len_utf8();
    let (mut engine, family) = exact_engine();
    let layout = resolve(&mut engine, &family, text);
    let missing = runs(&layout)
        .into_iter()
        .find(|run| {
            run.source
                == SourceRange {
                    start: missing_start,
                    end: missing_end,
                }
        })
        .expect("the unsupported scalar must retain its exact source range");

    assert_eq!(
        &text[missing.source.start..missing.source.end],
        "\u{10FFFF}"
    );
    assert_eq!(missing.glyphs.len(), 1);
    assert_eq!(missing.glyphs[0].id, 0);
    assert_eq!(
        missing.glyphs[0].fallback,
        Some(ResolvedGlyphFallback::Missing)
    );
    assert!(missing.glyphs[0].outline.is_none());

    let repeated = resolve(&mut engine, &family, text);
    let (mut fresh_engine, fresh_family) = exact_engine();
    let fresh = resolve(&mut fresh_engine, &fresh_family, text);
    assert_eq!(
        layout, repeated,
        "repeat resolution changed missing-glyph evidence"
    );
    assert_eq!(layout, fresh, "fresh engine changed missing-glyph evidence");
}

#[test]
fn repeated_resolution_is_byte_for_byte_deterministic_for_the_full_corpus() {
    let text = CORPUS
        .lines()
        .map(|line| line.split_once('|').unwrap().1)
        .collect::<Vec<_>>()
        .join("\n");
    let (mut engine, family) = exact_engine();
    let first = resolve(&mut engine, &family, &text);
    let second = resolve(&mut engine, &family, &text);
    let (mut fresh_engine, fresh_family) = exact_engine();
    let fresh = resolve(&mut fresh_engine, &fresh_family, &text);

    assert_eq!(first, second, "repeat resolution changed canonical output");
    assert_eq!(first, fresh, "fresh engine changed canonical output");
}

#[test]
fn explicit_rights_safe_font_fallback_is_exact_and_deterministic() {
    let text = "Magic 🚀";
    let emoji_start = text.find('🚀').unwrap();
    let emoji_end = emoji_start + '🚀'.len_utf8();
    let base = ExactFontResource {
        source: AssetHash::for_bytes(FONT_BYTES),
        face_index: 0,
        bytes: FONT_BYTES.to_vec(),
    };
    let fallback = ExactFontResource {
        source: AssetHash::for_bytes(EMOJI_FALLBACK_BYTES),
        face_index: 0,
        bytes: EMOJI_FALLBACK_BYTES.to_vec(),
    };
    assert!(
        ttf_parser::Face::parse(FONT_BYTES, 0)
            .unwrap()
            .glyph_index('🚀')
            .is_none(),
        "the base fixture must genuinely require a second face"
    );
    assert!(
        ttf_parser::Face::parse(EMOJI_FALLBACK_BYTES, 0)
            .unwrap()
            .glyph_index('🚀')
            .is_some(),
        "the pinned fallback fixture must contain the representative emoji"
    );

    let resolve_once = || {
        let mut engine = TextLayoutEngine::new_without_system_fonts();
        let base_family = engine.register_exact_font(&base).unwrap();
        assert_eq!(
            engine.register_exact_fallback_font(&fallback, *b"L1tn"),
            Err(ExactFontRegistrationError::InvalidFallbackScriptTag)
        );
        engine
            .register_exact_fallback_font(&fallback, *b"Latn")
            .unwrap();
        resolve(&mut engine, &base_family, text)
    };
    let first = resolve_once();
    let second = resolve_once();
    let emoji = runs(&first)
        .into_iter()
        .find(|run| {
            run.source
                == SourceRange {
                    start: emoji_start,
                    end: emoji_end,
                }
        })
        .expect("the fallback glyph must retain its exact source range");
    let expected_font = sha2::Sha256::digest(EMOJI_FALLBACK_BYTES);

    assert_eq!(
        first, second,
        "fresh fallback engines changed canonical output"
    );
    assert_eq!(emoji.glyphs.len(), 1);
    assert_eq!(emoji.glyphs[0].fallback, None);
    assert!(emoji.glyphs[0].outline.is_some());
    assert_eq!(
        first.fonts[emoji.font_index].sha256.as_slice(),
        expected_font.as_slice()
    );
    assert_eq!(first.fonts[emoji.font_index].collection_index, 0);
}

#[test]
fn representative_devanagari_thai_and_cjk_fallbacks_are_exact_and_deterministic() {
    const SEGMENTS: [(&str, [u8; 4], &[u8]); 3] = [
        ("हिन्दी", *b"Deva", DEVANAGARI_FONT_BYTES),
        ("ภาษาไทย", *b"Thai", THAI_FONT_BYTES),
        ("中文", *b"Hani", CJK_FONT_BYTES),
    ];
    let text = SEGMENTS
        .iter()
        .map(|(text, _, _)| *text)
        .collect::<Vec<_>>()
        .join(" ");
    let base = ExactFontResource {
        source: AssetHash::for_bytes(FONT_BYTES),
        face_index: 0,
        bytes: FONT_BYTES.to_vec(),
    };
    let fallbacks = SEGMENTS.map(|(_, _, bytes)| ExactFontResource {
        source: AssetHash::for_bytes(bytes),
        face_index: 0,
        bytes: bytes.to_vec(),
    });
    let base_face = ttf_parser::Face::parse(FONT_BYTES, 0).unwrap();
    for ((segment, _, bytes), resource) in SEGMENTS.iter().zip(&fallbacks) {
        let representative = segment.chars().next().unwrap();
        assert!(base_face.glyph_index(representative).is_none());
        assert!(
            ttf_parser::Face::parse(bytes, 0)
                .unwrap()
                .glyph_index(representative)
                .is_some(),
            "the pinned script fixture must contain {representative:?}"
        );
        assert_eq!(resource.source, AssetHash::for_bytes(bytes));
    }

    let resolve_once = || {
        let mut engine = TextLayoutEngine::new_without_system_fonts();
        let base_family = engine.register_exact_font(&base).unwrap();
        for ((_, tag, _), resource) in SEGMENTS.iter().zip(&fallbacks) {
            engine
                .set_exact_fallback_fonts(*tag, core::iter::once(resource))
                .unwrap();
        }
        resolve(&mut engine, &base_family, &text)
    };
    let first = resolve_once();
    let second = resolve_once();

    assert_eq!(first, second, "fresh engines changed multiscript layout");
    assert!(first.overset.is_none());
    for ((segment, _, bytes), resource) in SEGMENTS.iter().zip(&fallbacks) {
        let expected_digest = sha2::Sha256::digest(bytes);
        let start = text.find(segment).unwrap();
        let end = start + segment.len();
        let segment_runs = runs(&first)
            .into_iter()
            .filter(|run| run.source.start >= start && run.source.end <= end)
            .collect::<Vec<_>>();
        assert!(!segment_runs.is_empty());
        assert!(segment_runs.iter().all(|run| {
            first.fonts[run.font_index].sha256.as_slice() == expected_digest.as_slice()
                && first.fonts[run.font_index].collection_index == resource.face_index
        }));
        let glyphs = segment_runs
            .iter()
            .flat_map(|run| &run.glyphs)
            .collect::<Vec<_>>();
        assert!(!glyphs.is_empty());
        assert!(
            glyphs.iter().all(|glyph| {
                glyph.id != 0 && glyph.fallback.is_none() && glyph.outline.is_some()
            })
        );
    }
}

#[test]
fn real_colrv1_glyph_is_an_explicit_deterministic_artwork_fallback() {
    let text = "🚀";
    let face = ttf_parser::Face::parse(COLRV1_FONT_BYTES, 0).unwrap();
    let glyph_id = face
        .glyph_index('🚀')
        .expect("the pinned COLRv1 fixture must contain the representative emoji");
    assert!(face.is_color_glyph(glyph_id));
    let resource = ExactFontResource {
        source: AssetHash::for_bytes(COLRV1_FONT_BYTES),
        face_index: 0,
        bytes: COLRV1_FONT_BYTES.to_vec(),
    };
    let resolve_once = || {
        let mut engine = TextLayoutEngine::new_without_system_fonts();
        let family = engine.register_exact_font(&resource).unwrap();
        resolve(&mut engine, &family, text)
    };
    let first = resolve_once();
    let second = resolve_once();
    let run = runs(&first)
        .into_iter()
        .find(|run| {
            run.source
                == SourceRange {
                    start: 0,
                    end: text.len(),
                }
        })
        .expect("the color glyph must retain its exact source range");
    let expected_font = sha2::Sha256::digest(COLRV1_FONT_BYTES);

    assert_eq!(
        first, second,
        "fresh engines changed COLRv1 fallback evidence"
    );
    assert_eq!(run.glyphs.len(), 1);
    assert_eq!(run.glyphs[0].id, u32::from(glyph_id.0));
    assert_eq!(run.glyphs[0].fallback, Some(ResolvedGlyphFallback::Colr));
    assert!(run.glyphs[0].outline.is_none());
    assert_eq!(
        first.fonts[run.font_index].sha256.as_slice(),
        expected_font.as_slice()
    );
    assert_eq!(first.fonts[run.font_index].collection_index, 0);
}

#[test]
fn real_svg_and_bitmap_glyphs_are_explicit_deterministic_artwork_fallbacks() {
    for (bytes, expected) in [
        (SVG_FONT_BYTES, ResolvedGlyphFallback::Svg),
        (BITMAP_FONT_BYTES, ResolvedGlyphFallback::Bitmap),
    ] {
        let face = ttf_parser::Face::parse(bytes, 0).unwrap();
        let glyph_id = face
            .glyph_index('🚀')
            .expect("the pinned color fixture must contain rocket");
        match expected {
            ResolvedGlyphFallback::Svg => assert!(face.glyph_svg_image(glyph_id).is_some()),
            ResolvedGlyphFallback::Bitmap => {
                assert!(face.glyph_raster_image(glyph_id, u16::MAX).is_some());
            }
            ResolvedGlyphFallback::Missing | ResolvedGlyphFallback::Colr => unreachable!(),
        }
        let resource = ExactFontResource {
            source: AssetHash::for_bytes(bytes),
            face_index: 0,
            bytes: bytes.to_vec(),
        };
        let resolve_once = || {
            let mut engine = TextLayoutEngine::new_without_system_fonts();
            let family = engine.register_exact_font(&resource).unwrap();
            resolve(&mut engine, &family, "🚀")
        };
        let first = resolve_once();
        let second = resolve_once();
        let run = runs(&first)
            .into_iter()
            .find(|run| run.source == SourceRange { start: 0, end: 4 })
            .expect("color glyph must retain its exact source range");

        assert_eq!(first, second);
        assert_eq!(run.glyphs.len(), 1);
        assert_eq!(run.glyphs[0].id, u32::from(glyph_id.0));
        assert_eq!(run.glyphs[0].fallback, Some(expected));
        assert!(run.glyphs[0].outline.is_none());
        assert_eq!(
            first.fonts[run.font_index].sha256.as_slice(),
            sha2::Sha256::digest(bytes).as_slice()
        );
        assert_eq!(first.fonts[run.font_index].collection_index, 0);
    }
}
