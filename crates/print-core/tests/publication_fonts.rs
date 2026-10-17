use document_core::model::{
    Color, DocumentId, ExactFontFallbackChain, ExactFontVariantBinding, FontFaceBinding,
    FontFallbackScript, HardShadow, InlineTextSpan, InlineTextStyle, Insets, Micrometres,
    ObjectDraft, ObjectEffects, ObjectKind, PhysicalSize, Revision, ShapeStroke, TableData,
    TextFrame, TextSourceRange, TextStoryFormatting, TextStyle, Transform,
};
use document_core::{
    AssetHash, Command, Document, DocumentSession, ObjectLocation, ProjectSnapshot, Transaction,
};
use layout_core::{ExactFontResource, ResolvedGlyphFallback, ResolvedPublication};
use print_core::{
    FontPreparationKind, ProductionFontError, ScenePdfOptions, ScenePdfWriter,
    SceneSerializationError, TrustedFontBoundary, prepare_publication_fonts,
    prepare_publication_fonts_with_font_facts,
};
use sha2::{Digest, Sha256};

fn font_fixture() -> ExactFontResource {
    let bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    ExactFontResource {
        source: AssetHash::for_bytes(&bytes),
        face_index: 0,
        bytes,
    }
}

fn variant_font_fixture(bytes: &[u8]) -> ExactFontResource {
    ExactFontResource {
        source: AssetHash::for_bytes(bytes),
        face_index: 0,
        bytes: bytes.to_vec(),
    }
}

fn two_face_collection_fixture() -> [ExactFontResource; 2] {
    let faces = [
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").as_slice(),
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans-Bold.ttf").as_slice(),
    ];
    let mut bytes = vec![0; 20];
    bytes[..4].copy_from_slice(b"ttcf");
    bytes[4..8].copy_from_slice(&0x0001_0000_u32.to_be_bytes());
    bytes[8..12].copy_from_slice(&2_u32.to_be_bytes());
    for (index, face) in faces.into_iter().enumerate() {
        let start = bytes.len();
        bytes[12 + index * 4..16 + index * 4]
            .copy_from_slice(&u32::try_from(start).unwrap().to_be_bytes());
        bytes.extend_from_slice(face);
        let table_count = usize::from(u16::from_be_bytes([face[4], face[5]]));
        // Table offsets in a TTC are absolute offsets from the collection start.
        for table in 0..table_count {
            let record = start + 12 + table * 16;
            let old_offset = u32::from_be_bytes(bytes[record + 8..record + 12].try_into().unwrap());
            bytes[record + 8..record + 12]
                .copy_from_slice(&(old_offset + u32::try_from(start).unwrap()).to_be_bytes());
        }
        bytes.resize(bytes.len().next_multiple_of(4), 0);
    }
    let source = AssetHash::for_bytes(&bytes);
    [0, 1].map(|face_index| ExactFontResource {
        source: source.clone(),
        face_index,
        bytes: bytes.clone(),
    })
}

fn pdf_subset_tag(pdf: &[u8], key: &str, postscript_name: &str) -> String {
    let text = String::from_utf8_lossy(pdf);
    let names = text
        .split_ascii_whitespace()
        .zip(text.split_ascii_whitespace().skip(1))
        .filter_map(|(field, value)| {
            (field == key)
                .then(|| value.strip_prefix('/'))
                .flatten()
                .and_then(|name| name.strip_suffix(postscript_name))
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        names.len(),
        1,
        "one subset name for {key} {postscript_name}"
    );
    let prefix = names.into_iter().next().unwrap();
    assert_eq!(prefix.len(), 7);
    assert!(prefix[..6].bytes().all(|byte| byte.is_ascii_uppercase()));
    assert!(prefix.ends_with('+'));
    prefix[..6].to_owned()
}

fn pdf_has_stream_with_sha256(pdf: &[u8], expected: [u8; 32]) -> bool {
    let marker = b"stream\n";
    let end = b"\nendstream";
    pdf.windows(marker.len())
        .enumerate()
        .any(|(offset, bytes)| {
            if bytes != marker {
                return false;
            }
            let body = &pdf[offset + marker.len()..];
            let Some(length) = body.windows(end.len()).position(|bytes| bytes == end) else {
                return false;
            };
            let digest: [u8; 32] = Sha256::digest(&body[..length]).into();
            digest == expected
        })
}

fn retain_font_tag_pdf_if_requested(bytes: &[u8]) {
    if let Some(path) = std::env::var_os("PUBLISHER_FONT_TAG_OUTPUT") {
        use std::io::Write;
        let path = std::path::PathBuf::from(path);
        assert!(
            path.is_absolute(),
            "font-tag PDF output path must be absolute"
        );
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
}

fn full_font_fixture() -> ExactFontResource {
    fn checksum(bytes: &[u8]) -> u32 {
        bytes.chunks(4).fold(0u32, |sum, chunk| {
            let mut word = [0; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            sum.wrapping_add(u32::from_be_bytes(word))
        })
    }
    let mut bytes = include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").to_vec();
    let table_count = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
    let os2 = (0..table_count)
        .map(|index| 12 + index * 16)
        .find(|offset| bytes[*offset..*offset + 4] == *b"OS/2")
        .expect("fixture has OS/2 table");
    let table_offset = usize::try_from(u32::from_be_bytes([
        bytes[os2 + 8],
        bytes[os2 + 9],
        bytes[os2 + 10],
        bytes[os2 + 11],
    ]))
    .unwrap();
    let table_length = usize::try_from(u32::from_be_bytes([
        bytes[os2 + 12],
        bytes[os2 + 13],
        bytes[os2 + 14],
        bytes[os2 + 15],
    ]))
    .unwrap();
    let old_padded_end = table_offset + table_length.next_multiple_of(4);
    let new_length = 96_usize;
    let shift = new_length - table_length.next_multiple_of(4);
    bytes.splice(old_padded_end..old_padded_end, vec![0; shift]);
    bytes[os2 + 12..os2 + 16].copy_from_slice(&u32::try_from(new_length).unwrap().to_be_bytes());
    for index in 0..table_count {
        let record = 12 + index * 16;
        let offset = usize::try_from(u32::from_be_bytes([
            bytes[record + 8],
            bytes[record + 9],
            bytes[record + 10],
            bytes[record + 11],
        ]))
        .unwrap();
        if offset >= old_padded_end {
            bytes[record + 8..record + 12]
                .copy_from_slice(&u32::try_from(offset + shift).unwrap().to_be_bytes());
        }
    }
    // Editable embedding plus the OpenType no-subsetting bit.
    bytes[table_offset..table_offset + 2].copy_from_slice(&3_u16.to_be_bytes());
    bytes[table_offset + 8..table_offset + 10].copy_from_slice(&0x0108_u16.to_be_bytes());
    // This synthetic full-font source must have truthful caches. The retained
    // DejaVu source has several one-unit stale glyph boxes and bit1 bearings;
    // subset preparation normally rebuilds them, whereas full mode preserves
    // every byte. Repair the test source before asking full mode to attest it.
    refresh_full_font_metric_caches(&mut bytes);
    let records = (0..table_count)
        .map(|i| {
            let record = 12 + i * 16;
            let at = usize::try_from(u32::from_be_bytes(
                bytes[record + 8..record + 12].try_into().unwrap(),
            ))
            .unwrap();
            let length = usize::try_from(u32::from_be_bytes(
                bytes[record + 12..record + 16].try_into().unwrap(),
            ))
            .unwrap();
            (record, at, length)
        })
        .collect::<Vec<_>>();
    let head = records
        .iter()
        .find(|(record, _, _)| &bytes[*record..*record + 4] == b"head")
        .unwrap()
        .1;
    bytes[head + 8..head + 12].fill(0);
    for (record, at, length) in records {
        let sum = checksum(&bytes[at..at + length]);
        bytes[record + 4..record + 8].copy_from_slice(&sum.to_be_bytes());
    }
    let adjustment = 0xb1b0_afba_u32.wrapping_sub(checksum(&bytes));
    bytes[head + 8..head + 12].copy_from_slice(&adjustment.to_be_bytes());
    assert_eq!(checksum(&bytes), 0xb1b0_afba);
    ExactFontResource {
        source: AssetHash::for_bytes(&bytes),
        face_index: 0,
        bytes,
    }
}

fn refresh_full_font_metric_caches(bytes: &mut [u8]) {
    struct IgnoreOutline;
    impl ttf_parser::OutlineBuilder for IgnoreOutline {
        fn move_to(&mut self, _: f32, _: f32) {}
        fn line_to(&mut self, _: f32, _: f32) {}
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
        fn close(&mut self) {}
    }
    let table_offset = |tag: &[u8; 4]| {
        bytes[12..12 + 16 * usize::from(u16::from_be_bytes([bytes[4], bytes[5]]))]
            .as_chunks::<16>()
            .0
            .iter()
            .find_map(|record| {
                (&record[..4] == tag).then(|| {
                    usize::try_from(u32::from_be_bytes(record[8..12].try_into().unwrap())).unwrap()
                })
            })
            .unwrap()
    };
    let (head, hhea, hmtx, loca, glyf) = (
        table_offset(b"head"),
        table_offset(b"hhea"),
        table_offset(b"hmtx"),
        table_offset(b"loca"),
        table_offset(b"glyf"),
    );
    let metrics = usize::from(u16::from_be_bytes(
        bytes[hhea + 34..hhea + 36].try_into().unwrap(),
    ));
    let long = bytes[head + 51] == 1;
    let face = ttf_parser::Face::parse(bytes, 0).unwrap();
    let boxes = (0..face.number_of_glyphs())
        .filter_map(|gid| {
            let bounds = face.outline_glyph(ttf_parser::GlyphId(gid), &mut IgnoreOutline)?;
            let advance = face.glyph_hor_advance(ttf_parser::GlyphId(gid)).unwrap();
            Some((usize::from(gid), bounds, advance))
        })
        .collect::<Vec<_>>();
    let mut global = [i16::MAX, i16::MAX, i16::MIN, i16::MIN];
    let mut extrema = [i32::MAX, i32::MAX, i32::MIN];
    for (gid, bounds, advance) in boxes {
        let offset = if long {
            usize::try_from(u32::from_be_bytes(
                bytes[loca + 4 * gid..loca + 4 * gid + 4]
                    .try_into()
                    .unwrap(),
            ))
            .unwrap()
        } else {
            usize::from(u16::from_be_bytes(
                bytes[loca + 2 * gid..loca + 2 * gid + 2]
                    .try_into()
                    .unwrap(),
            )) * 2
        };
        let values = [bounds.x_min, bounds.y_min, bounds.x_max, bounds.y_max];
        for (i, value) in values.into_iter().enumerate() {
            bytes[glyf + offset + 2 + i * 2..glyf + offset + 4 + i * 2]
                .copy_from_slice(&value.to_be_bytes());
            global[i] = if i < 2 {
                global[i].min(value)
            } else {
                global[i].max(value)
            };
        }
        let bearing_at = hmtx
            + if gid < metrics {
                4 * gid + 2
            } else {
                4 * metrics + 2 * (gid - metrics)
            };
        bytes[bearing_at..bearing_at + 2].copy_from_slice(&bounds.x_min.to_be_bytes());
        extrema[0] = extrema[0].min(i32::from(bounds.x_min));
        extrema[1] = extrema[1].min(i32::from(advance) - i32::from(bounds.x_max));
        extrema[2] = extrema[2].max(i32::from(bounds.x_max));
    }
    for (i, value) in global.into_iter().enumerate() {
        bytes[head + 36 + i * 2..head + 38 + i * 2].copy_from_slice(&value.to_be_bytes());
    }
    for (i, value) in extrema.into_iter().enumerate() {
        bytes[hhea + 12 + i * 2..hhea + 14 + i * 2]
            .copy_from_slice(&i16::try_from(value).unwrap().to_be_bytes());
    }
}

fn document(id: &str) -> Document {
    Document::new(
        DocumentId::parse(id).unwrap(),
        "Publication font test",
        PhysicalSize::new(215_900, 279_400).unwrap(),
        Insets::uniform(3_175).unwrap(),
    )
    .unwrap()
}

fn hex_sha256(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

fn exact_text(resource: &ExactFontResource, text: &str, name: &str) -> ObjectDraft {
    ObjectDraft {
        name: name.to_owned(),
        size: PhysicalSize::new(120_000, 40_000).unwrap(),
        transform: Transform::identity_at(10_000, 15_000),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Text {
            text: text.to_owned(),
            style: TextStyle {
                font_face: Some(FontFaceBinding {
                    source: resource.source.clone(),
                    face_index: resource.face_index,
                }),
                font_family: "Exact packaged face".to_owned(),
                font_size_millipoints: 24_000,
                ..TextStyle::default()
            },
            frame: TextFrame::default(),
            story: TextStoryFormatting::default(),
        },
    }
}

fn exact_table(resource: &ExactFontResource) -> ObjectDraft {
    let mut table = TableData::empty(1, 2).unwrap();
    table.cells = vec!["M".to_owned(), "A".to_owned()];
    table.style = TextStyle {
        font_face: Some(FontFaceBinding {
            source: resource.source.clone(),
            face_index: resource.face_index,
        }),
        font_family: "Exact packaged table face".to_owned(),
        font_size_millipoints: 18_000,
        ..table.style
    };
    ObjectDraft {
        name: "Word search".to_owned(),
        size: PhysicalSize::new(120_000, 40_000).unwrap(),
        transform: Transform::identity_at(10_000, 15_000),
        opacity_millionths: 1_000_000,
        kind: ObjectKind::Table { table },
    }
}

#[test]
fn embedded_pdf_retains_ligature_and_combining_source_text() {
    let resource = font_fixture();
    let document = document("complete-shaped-source");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Shaped text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: exact_text(
                        &resource,
                        "Confirm final\nCafe\u{301}\nسلام",
                        "Source components",
                    ),
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_output_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();
    let fonts = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();
    let pdf = ScenePdfWriter::write_with_rasters_and_fonts(
        &publication,
        ScenePdfOptions::default(),
        &[],
        &fonts,
    )
    .unwrap();
    for expected in ["fi", "e\u{301}", "لا"] {
        let mut marked = pdf_writer::Content::new();
        marked
            .begin_marked_content_with_properties(pdf_writer::Name(b"Span"))
            .properties()
            .actual_text(pdf_writer::TextStr(expected));
        let expected_span = marked.finish();
        assert!(
            pdf.bytes()
                .windows(expected_span.len())
                .any(|part| part == expected_span.as_slice()),
            "ActualText must retain the full shaped sequence {expected:?}"
        );
        let mut unicode = String::new();
        for unit in expected.encode_utf16() {
            use std::fmt::Write;
            write!(&mut unicode, "{unit:04X}").unwrap();
        }
        assert!(
            String::from_utf8_lossy(pdf.bytes()).contains(&unicode),
            "ToUnicode must retain the full shaped sequence {expected:?}"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end test binds an exact subset to embedded-font fill and outline rendering"
)]
fn exact_page_glyphs_prepare_one_scene_bound_subset() {
    let resource = font_fixture();
    let source_sha256 = resource.source.as_str().to_owned();
    let document = document("prepare-publication-font");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add exact page text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id.clone()),
                    object: exact_text(&resource, "Magic", "Headline"),
                },
            ),
        )
        .unwrap();
    let baseline_publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();
    let baseline_fonts = prepare_publication_fonts(
        &baseline_publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();
    let baseline_pdf = ScenePdfWriter::write_with_rasters_and_fonts(
        &baseline_publication,
        ScenePdfOptions::default(),
        &[],
        &baseline_fonts,
    )
    .unwrap();
    let baseline_bytes = String::from_utf8_lossy(baseline_pdf.bytes()).into_owned();
    session
        .execute(
            added.revision,
            Transaction::single(
                "Outline exact page text",
                Command::UpdateObjectEffects {
                    object_id: added.created_objects[0].clone(),
                    effects: ObjectEffects {
                        shadow: Some(HardShadow {
                            color: Color {
                                red: 30,
                                green: 40,
                                blue: 50,
                                alpha: 255,
                            },
                            offset_x: Micrometres::new(2_540),
                            offset_y: Micrometres::new(1_270),
                        }),
                        text_outline: Some(ShapeStroke {
                            color: Color {
                                red: 200,
                                green: 30,
                                blue: 40,
                                alpha: 255,
                            },
                            width_millipoints: 1_500,
                        }),
                        picture_border: None,
                    },
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();

    let fonts = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();

    assert_eq!(fonts.len(), 1);
    let evidence = fonts[0].evidence();
    assert_eq!(evidence.scene_digest, publication.semantic_digest());
    assert_eq!(hex_sha256(evidence.source_sha256), source_sha256);
    assert_eq!(evidence.face_index, 0);
    assert!(evidence.used_glyph_count > 0);
    assert!(evidence.embedded_glyph_count >= evidence.used_glyph_count);
    assert_ne!(evidence.prepared_font_sha256, [0; 32]);

    let pdf = ScenePdfWriter::write_with_rasters_and_fonts(
        &publication,
        ScenePdfOptions::default(),
        &[],
        &fonts,
    )
    .unwrap();
    assert_eq!(pdf.evidence().embedded_fonts.len(), 1);
    assert!(!pdf.evidence().text_painted_as_native_outlines);
    let bytes = String::from_utf8_lossy(pdf.bytes());
    assert!(bytes.contains("/FontFile2"));
    assert!(bytes.contains("/ToUnicode"));
    assert!(bytes.contains("+DejaVuSans"));
    let (finished, _) = print_core::PublisherSceneAdapter::lower(
        &publication,
        ScenePdfOptions::default(),
        &[],
        Some(&fonts),
        None,
    )
    .unwrap();
    let decorative_paths = finished.pages[0]
        .items
        .iter()
        .filter_map(|item| match &item.content {
            print_core::finished::FinishedContent::Path(path) => Some(path),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decorative_paths.len(), 2, "shadow and outline paths");
    assert!(decorative_paths.iter().any(|path| path.fill.is_some()));
    assert!(decorative_paths.iter().any(|path| path.stroke.is_some()));
    assert!(bytes.contains("\nf\n"));
    assert!(bytes.contains("\nS\n"));
    assert_eq!(
        bytes.matches("/ActualText").count(),
        baseline_bytes.matches("/ActualText").count(),
        "effects must not duplicate extractable authored text"
    );
    assert_eq!(
        bytes.matches("BT\n").count(),
        baseline_bytes.matches("BT\n").count(),
        "effects must not add text objects"
    );
    assert_eq!(
        bytes.matches("Tj\n").count(),
        baseline_bytes.matches("Tj\n").count(),
        "effects must not add text-show operators"
    );
    assert!(
        !["1 Tr\n", "2 Tr\n", "3 Tr\n"]
            .iter()
            .any(|operator| bytes.contains(operator)),
        "the only embedded-font text pass remains semantic fill"
    );

    session
        .execute(
            session.revision(),
            Transaction::single(
                "Change the resolved scene",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: exact_text(&resource, "Changed", "Second headline"),
                },
            ),
        )
        .unwrap();
    let changed = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();
    assert_eq!(
        ScenePdfWriter::write_with_rasters_and_fonts(
            &changed,
            ScenePdfOptions::default(),
            &[],
            &fonts,
        )
        .unwrap_err(),
        SceneSerializationError::InvalidFontEvidence
    );
}

#[test]
fn exact_table_glyphs_prepare_and_embed_one_scene_bound_subset() {
    let resource = font_fixture();
    let document = document("prepare-table-font");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add exact word-search table",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: exact_table(&resource),
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();
    let fonts = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();

    assert_eq!(fonts.len(), 1);
    assert!(fonts[0].evidence().used_glyph_count >= 2);
    let pdf = ScenePdfWriter::write_with_rasters_and_fonts(
        &publication,
        ScenePdfOptions::default(),
        &[],
        &fonts,
    )
    .unwrap();
    assert_eq!(pdf.evidence().embedded_fonts.len(), 1);
    assert!(String::from_utf8_lossy(pdf.bytes()).contains("+DejaVuSans"));
}

#[test]
fn a_resolved_page_font_without_supplied_exact_bytes_fails_closed() {
    let resource = font_fixture();
    let expected_source = resource.source.as_str().to_owned();
    let document = document("missing-publication-font");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add exact page text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: exact_text(&resource, "Magic", "Headline"),
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();

    let Err(error) =
        prepare_publication_fonts(&publication, &[], &TrustedFontBoundary::resolve_packaged())
    else {
        panic!("missing exact font bytes must fail closed");
    };

    assert_eq!(
        error,
        ProductionFontError::MissingResource {
            source_sha256: expected_source,
            face_index: 0,
        }
    );
}

#[test]
fn unsupported_character_is_rejected_with_its_owning_text_box_identity() {
    let resource = font_fixture();
    let document = document("unsupported-publication-character");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add unsupported exact-font character",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: exact_text(&resource, "Magic \u{10FFFF} trick", "Magic instructions"),
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();

    let Err(first) = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    ) else {
        panic!("an unsupported character must not enter the print candidate");
    };
    let Err(repeated) = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    ) else {
        panic!("repeat preparation must preserve the same rejection");
    };
    let expected = ProductionFontError::UnsupportedGlyphFallback {
        object_id: added.created_objects[0].as_str().to_owned(),
        glyph_id: 0,
    };

    assert_eq!(first, expected);
    assert_eq!(repeated, expected);
}

#[test]
fn real_color_font_artwork_is_rejected_before_font_worker_or_candidate_creation() {
    for (bytes, expected) in [
        (
            include_bytes!("../../layout-core/tests/fixtures/Noto-COLRv1-noflags.ttf").as_slice(),
            ResolvedGlyphFallback::Colr,
        ),
        (
            include_bytes!("../../layout-core/tests/fixtures/NotoColorEmoji-SVG.otf").as_slice(),
            ResolvedGlyphFallback::Svg,
        ),
        (
            include_bytes!("../../layout-core/tests/fixtures/NotoColorEmoji-CBDT.ttf").as_slice(),
            ResolvedGlyphFallback::Bitmap,
        ),
    ] {
        let resource = variant_font_fixture(bytes);
        let document = document("color-font-publication-character");
        let page_id = document.pages[0].id.clone();
        let mut session = DocumentSession::new(document).unwrap();
        let added = session
            .execute(
                Revision::initial(),
                Transaction::single(
                    "Add exact color-font character",
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object: exact_text(&resource, "🚀", "Color emoji"),
                    },
                ),
            )
            .unwrap();
        let publication = ResolvedPublication::resolve_with_font_resources(
            &ProjectSnapshot::from_session(&session),
            core::slice::from_ref(&resource),
        )
        .unwrap();
        let glyph = publication
            .text_boxes()
            .into_iter()
            .flat_map(|layout| &layout.lines)
            .flat_map(|line| &line.glyph_runs)
            .flat_map(|run| &run.glyphs)
            .next()
            .expect("the exact color glyph must reach the resolved scene");

        assert_eq!(glyph.fallback, Some(expected));
        assert!(glyph.outline.is_none());
        let Err(error) = prepare_publication_fonts(
            &publication,
            core::slice::from_ref(&resource),
            &TrustedFontBoundary::resolve_packaged(),
        ) else {
            panic!("unsupported color artwork must stop before font preparation");
        };
        assert_eq!(
            error,
            ProductionFontError::UnsupportedGlyphFallback {
                object_id: added.created_objects[0].as_str().to_owned(),
                glyph_id: glyph.id,
            }
        );
    }
}

#[test]
fn variable_font_non_default_weight_is_refused_before_print_candidate() {
    let thai = variant_font_fixture(include_bytes!(
        "../../layout-core/tests/fixtures/NotoSansThai-VF.ttf"
    ));
    for (weight, base) in [
        (400, font_fixture()),
        (
            700,
            variant_font_fixture(include_bytes!(
                "../../layout-core/tests/fixtures/DejaVuSans-Bold.ttf"
            )),
        ),
    ] {
        for (case, text) in [("pure", "ภาษาไทย"), ("mixed", "Needs repair: ภาษาไทย")]
        {
            let mut document = document(&format!("thai-variable-fallback-{case}-{weight}"));
            document.exact_font_fallbacks =
                vec![exact_fallback_chain(FontFallbackScript::Thai, &thai)];
            let page_id = document.pages[0].id.clone();
            let mut session = DocumentSession::new(document).unwrap();
            let object = exact_text(&base, text, "Thai variable fallback");
            let added = session
                .execute(
                    Revision::initial(),
                    Transaction::single(
                        "Add Thai variable text",
                        Command::AddObject {
                            location: ObjectLocation::Page(page_id),
                            object,
                        },
                    ),
                )
                .unwrap();
            let publication = ResolvedPublication::resolve_output_with_font_resources(
                &ProjectSnapshot::from_session(&session),
                &[base.clone(), thai.clone()],
            )
            .unwrap();
            let text_box = publication.text_boxes()[0];
            let runs = text_box
                .lines
                .iter()
                .flat_map(|line| &line.glyph_runs)
                .collect::<Vec<_>>();
            assert_eq!(
                text_box.font_binding.substitution_status,
                layout_core::FontSubstitutionStatus::Exact
            );
            let thai_runs = runs
                .iter()
                .copied()
                .filter(|run| {
                    run.text
                        .chars()
                        .any(|ch| ('\u{0e00}'..='\u{0e7f}').contains(&ch))
                })
                .collect::<Vec<_>>();
            assert!(!thai_runs.is_empty());
            assert!(thai_runs.iter().all(|run| {
                text_box.resolved_fonts[run.font_index].sha256_hex == thai.source.as_str()
            }));
            if case == "mixed" {
                let latin = runs
                    .iter()
                    .find(|run| run.text.chars().any(|ch| ch.is_ascii_alphabetic()))
                    .expect("Latin prefix must resolve");
                assert!(latin.source.start < thai_runs[0].source.start);
                assert_eq!(
                    text_box.resolved_fonts[latin.font_index].sha256_hex,
                    base.source.as_str()
                );
                assert!(!latin.requires_font_synthesis);
            }
            assert!(
                runs.iter()
                    .flat_map(|run| &run.glyphs)
                    .any(|glyph| glyph.id != 0)
            );
            assert!(
                runs.iter()
                    .flat_map(|run| &run.glyphs)
                    .all(|glyph| glyph.fallback.is_none())
            );
            let prepared = prepare_publication_fonts(
                &publication,
                &[base.clone(), thai.clone()],
                &TrustedFontBoundary::resolve_packaged(),
            );
            if weight == 400 {
                assert!(runs.iter().all(|run| !run.requires_font_synthesis));
                assert_eq!(prepared.unwrap().len(), if case == "mixed" { 2 } else { 1 });
            } else {
                assert!(thai_runs.iter().any(|run| run.requires_font_synthesis));
                assert!(matches!(
                    prepared,
                    Err(ProductionFontError::UnsupportedFontSynthesis { object_id, .. })
                        if object_id == added.created_objects[0].as_str()
                ));
            }
        }
    }
}

#[test]
fn explicitly_permitted_multiscript_fallbacks_prepare_as_distinct_exact_fonts() {
    let base = font_fixture();
    let emoji = variant_font_fixture(include_bytes!(
        "../../layout-core/tests/fixtures/NotoEmoji-Regular.ttf"
    ));
    let devanagari = variant_font_fixture(include_bytes!(
        "../../layout-core/tests/fixtures/NotoSansDevanagari-VF.ttf"
    ));
    let thai = variant_font_fixture(include_bytes!(
        "../../layout-core/tests/fixtures/NotoSansThai-VF.ttf"
    ));
    let cjk = variant_font_fixture(include_bytes!(
        "../../layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf"
    ));
    let mut document = document("permitted-publication-fallback");
    document.exact_font_fallbacks = vec![
        exact_fallback_chain(FontFallbackScript::Devanagari, &devanagari),
        exact_fallback_chain(FontFallbackScript::Han, &cjk),
        exact_fallback_chain(FontFallbackScript::Latin, &emoji),
        exact_fallback_chain(FontFallbackScript::Thai, &thai),
    ];
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add exact text with fallback",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: exact_text(&base, "Magic 🚀 हिन्दी ภาษาไทย 中文", "Fallback text"),
                },
            ),
        )
        .unwrap();
    let resources = [
        base.clone(),
        cjk.clone(),
        devanagari.clone(),
        emoji.clone(),
        thai.clone(),
    ];
    // Production output must use the host-independent resolver. Interactive
    // preview may legitimately discover an installed emoji font on Windows.
    let publication = ResolvedPublication::resolve_output_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &resources,
    )
    .unwrap();
    let prepared = prepare_publication_fonts(
        &publication,
        &resources,
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();
    let prepared_sources = prepared
        .iter()
        .map(|font| hex_sha256(font.evidence().source_sha256))
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(prepared.len(), 5);
    assert_eq!(
        prepared_sources,
        [
            base.source.as_str().to_owned(),
            cjk.source.as_str().to_owned(),
            devanagari.source.as_str().to_owned(),
            emoji.source.as_str().to_owned(),
            thai.source.as_str().to_owned(),
        ]
        .into_iter()
        .collect()
    );
}

fn exact_fallback_chain(
    script: FontFallbackScript,
    resource: &ExactFontResource,
) -> ExactFontFallbackChain {
    ExactFontFallbackChain {
        script,
        faces: vec![FontFaceBinding {
            source: resource.source.clone(),
            face_index: resource.face_index,
        }],
    }
}

#[test]
fn unincluded_output_fonts_identify_the_text_or_table_before_worker_preparation() {
    for table_case in [false, true] {
        let resource = font_fixture();
        let document = document("unincluded-output-font");
        let page_id = document.pages[0].id.clone();
        let mut object = exact_text(&resource, "Needs inclusion", "Unincluded font");
        if table_case {
            let mut table = TableData::empty(1, 1).unwrap();
            table.cells[0] = "Needs inclusion".to_owned();
            object.kind = ObjectKind::Table { table };
        } else if let ObjectKind::Text { style, .. } = &mut object.kind {
            style.font_face = None;
        }
        let mut session = DocumentSession::new(document).unwrap();
        let added = session
            .execute(
                Revision::initial(),
                Transaction::single(
                    "Add unincluded font",
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id),
                        object,
                    },
                ),
            )
            .unwrap();
        let publication = ResolvedPublication::resolve_output_with_font_resources(
            &ProjectSnapshot::from_session(&session),
            &[],
        )
        .unwrap();
        let Err(ProductionFontError::FontBindingUnresolved { object_id, .. }) =
            prepare_publication_fonts(&publication, &[], &TrustedFontBoundary::resolve_packaged())
        else {
            panic!("must identify an unincluded font before launching a worker")
        };
        assert_eq!(object_id, added.created_objects[0].as_str());
    }
}

#[test]
fn uncaptured_inline_face_is_rejected_before_font_preparation() {
    let resource = font_fixture();
    let document = document("unresolved-inline-publication-font");
    let page_id = document.pages[0].id.clone();
    let mut object = exact_text(&resource, "Bold", "Uncaptured bold");
    let ObjectKind::Text { story, .. } = &mut object.kind else {
        panic!("text fixture");
    };
    story.inline_spans.push(InlineTextSpan {
        range: TextSourceRange { start: 0, end: 4 },
        style: InlineTextStyle {
            font_size_millipoints: None,
            bold: Some(true),
            italic: None,
            ..InlineTextStyle::default()
        },
    });
    let mut session = DocumentSession::new(document).unwrap();
    let added = session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add uncaptured bold",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object,
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();

    let Err(error) = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    ) else {
        panic!("uncaptured inline face must fail before font preparation");
    };
    assert_eq!(
        error,
        ProductionFontError::FontBindingUnresolved {
            object_id: added.created_objects[0].as_str().to_owned(),
            source_start: 0,
            source_end: 4,
        }
    );
    let output = ResolvedPublication::resolve_output_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();
    let Err(output_error) = prepare_publication_fonts(
        &output,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    ) else {
        panic!("output must also retain the missing inline font repair");
    };
    assert_eq!(output_error, error);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end quartet fixture keeps all four authored bindings beside the four prepared font identities"
)]
fn exact_quartet_prepares_four_distinct_scene_bound_fonts() {
    let resources = [
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans.ttf").as_slice(),
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans-Bold.ttf").as_slice(),
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans-Oblique.ttf").as_slice(),
        include_bytes!("../../layout-core/tests/fixtures/DejaVuSans-BoldOblique.ttf").as_slice(),
    ]
    .into_iter()
    .map(variant_font_fixture)
    .collect::<Vec<_>>();
    let face = |index: usize| FontFaceBinding {
        source: resources[index].source.clone(),
        face_index: 0,
    };
    let text = "Regular Bold Italic Both";
    let document = document("exact-quartet-publication-font");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add exact quartet",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: ObjectDraft {
                        name: "Exact quartet".to_owned(),
                        size: PhysicalSize::new(180_000, 50_000).unwrap(),
                        transform: Transform::identity_at(10_000, 15_000),
                        opacity_millionths: 1_000_000,
                        kind: ObjectKind::Text {
                            text: text.to_owned(),
                            style: TextStyle {
                                font_face: Some(face(0)),
                                exact_font_variants: vec![
                                    ExactFontVariantBinding {
                                        font_weight: 400,
                                        italic: true,
                                        face: face(2),
                                    },
                                    ExactFontVariantBinding {
                                        font_weight: 700,
                                        italic: false,
                                        face: face(1),
                                    },
                                    ExactFontVariantBinding {
                                        font_weight: 700,
                                        italic: true,
                                        face: face(3),
                                    },
                                ],
                                exact_font_families: vec![
                                    document_core::model::ExactFontFamilyBinding {
                                        font_family: "Exact DejaVu additional family".to_owned(),
                                        font_weight: 700,
                                        italic: false,
                                        face: face(1),
                                    },
                                ],
                                font_family: "Exact DejaVu quartet".to_owned(),
                                ..TextStyle::default()
                            },
                            frame: TextFrame::default(),
                            story: TextStoryFormatting {
                                inline_spans: vec![
                                    InlineTextSpan {
                                        range: TextSourceRange { start: 8, end: 12 },
                                        style: InlineTextStyle {
                                            font_family: Some(
                                                "Exact DejaVu additional family".to_owned(),
                                            ),
                                            color: Some(Color {
                                                red: 255,
                                                green: 0,
                                                blue: 0,
                                                alpha: 128,
                                            }),
                                            underline: Some(true),
                                            font_size_millipoints: None,
                                            bold: Some(true),
                                            italic: None,
                                            ..InlineTextStyle::default()
                                        },
                                    },
                                    InlineTextSpan {
                                        range: TextSourceRange { start: 13, end: 19 },
                                        style: InlineTextStyle {
                                            font_size_millipoints: None,
                                            bold: None,
                                            italic: Some(true),
                                            ..InlineTextStyle::default()
                                        },
                                    },
                                    InlineTextSpan {
                                        range: TextSourceRange { start: 20, end: 24 },
                                        style: InlineTextStyle {
                                            font_size_millipoints: None,
                                            bold: Some(true),
                                            italic: Some(true),
                                            ..InlineTextStyle::default()
                                        },
                                    },
                                ],
                                paragraph_runs: Vec::new(),
                            },
                        },
                    },
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &resources,
    )
    .unwrap();
    let fonts = prepare_publication_fonts(
        &publication,
        &resources,
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();

    assert_eq!(fonts.len(), 4);
    assert_eq!(
        fonts
            .iter()
            .map(|font| hex_sha256(font.evidence().source_sha256))
            .collect::<std::collections::BTreeSet<_>>(),
        resources
            .iter()
            .map(|resource| resource.source.as_str().to_owned())
            .collect()
    );
    ScenePdfWriter::write_with_rasters_and_fonts(
        &publication,
        ScenePdfOptions::default(),
        &[],
        &fonts,
    )
    .expect("the exact quartet is printable without synthesis or substitution");
}

#[test]
fn two_collection_faces_embed_distinct_subset_tags_and_programs() {
    let resources = two_face_collection_fixture();
    let document = document("two-collection-face-subsets");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    for (resource, name, y) in [
        (&resources[0], "Regular", 15_000),
        (&resources[1], "Bold", 65_000),
    ] {
        let revision = session.revision();
        let mut object = exact_text(resource, name, name);
        object.transform = Transform::identity_at(10_000, y);
        session
            .execute(
                revision,
                Transaction::single(
                    "Add collection face text",
                    Command::AddObject {
                        location: ObjectLocation::Page(page_id.clone()),
                        object,
                    },
                ),
            )
            .unwrap();
    }
    let publication = ResolvedPublication::resolve_output_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        &resources,
    )
    .unwrap();
    let fonts = prepare_publication_fonts(
        &publication,
        &resources,
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();
    assert_eq!(fonts.len(), 2);
    assert_eq!(
        fonts[0].evidence().source_sha256,
        fonts[1].evidence().source_sha256
    );
    assert_eq!(fonts[0].evidence().face_index, 0);
    assert_eq!(fonts[1].evidence().face_index, 1);
    for font in &fonts {
        assert_eq!(
            font.evidence().preparation_kind,
            FontPreparationKind::Subset
        );
        assert_eq!(font.evidence().scene_digest, publication.semantic_digest());
    }
    assert_ne!(
        fonts[0].evidence().prepared_font_sha256,
        fonts[1].evidence().prepared_font_sha256,
        "the two collection faces must retain distinct prepared programs"
    );

    let pdf = ScenePdfWriter::write_with_rasters_and_fonts(
        &publication,
        ScenePdfOptions::default(),
        &[],
        &fonts,
    )
    .unwrap();
    retain_font_tag_pdf_if_requested(pdf.bytes());
    assert_eq!(pdf.evidence().embedded_fonts.len(), 2);
    for font in &fonts {
        assert!(pdf.evidence().embedded_fonts.contains(font.evidence()));
        assert!(pdf_has_stream_with_sha256(
            pdf.bytes(),
            font.evidence().prepared_font_sha256
        ));
    }
    let regular_tag = pdf_subset_tag(pdf.bytes(), "/BaseFont", "DejaVuSans");
    let bold_tag = pdf_subset_tag(pdf.bytes(), "/BaseFont", "DejaVuSans-Bold");
    assert_ne!(
        regular_tag, bold_tag,
        "different subsets need different tags"
    );
    assert_eq!(
        regular_tag,
        pdf_subset_tag(pdf.bytes(), "/FontName", "DejaVuSans")
    );
    assert_eq!(
        bold_tag,
        pdf_subset_tag(pdf.bytes(), "/FontName", "DejaVuSans-Bold")
    );
    let repeat = ScenePdfWriter::write_with_rasters_and_fonts(
        &publication,
        ScenePdfOptions::default(),
        &[],
        &fonts,
    )
    .unwrap();
    assert!(
        pdf.bytes() == repeat.bytes(),
        "scene PDF must be repeatable"
    );
}

#[test]
fn no_subsetting_rights_embed_the_untouched_complete_font() {
    let resource = full_font_fixture();
    let document = document("full-publication-font");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add exact full-font text",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: exact_text(&resource, "Magic", "Headline"),
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();

    let fonts = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();

    let evidence = fonts[0].evidence();
    assert_eq!(evidence.preparation_kind, FontPreparationKind::Full);
    assert_eq!(evidence.prepared_font_sha256, evidence.source_sha256);
    assert!(evidence.embedded_glyph_count > evidence.used_glyph_count);

    let pdf = ScenePdfWriter::write_with_rasters_and_fonts(
        &publication,
        ScenePdfOptions::default(),
        &[],
        &fonts,
    )
    .unwrap();
    assert!(
        pdf.bytes()
            .windows(resource.bytes.len())
            .any(|window| window == resource.bytes),
        "the full-font route must embed the exact untouched source bytes"
    );
    assert!(String::from_utf8_lossy(pdf.bytes()).contains("/BaseFont /DejaVuSans"));
}

#[test]
fn pasteboard_only_text_does_not_prepare_or_embed_a_font() {
    let resource = font_fixture();
    let mut session = DocumentSession::new(document("pasteboard-font-exclusion")).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Store exact text on pasteboard",
                Command::AddObject {
                    location: ObjectLocation::Pasteboard,
                    object: exact_text(&resource, "Reference", "Pasteboard note"),
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();

    let fonts = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();

    assert!(fonts.is_empty());
}

fn high_emoji_publication() -> (ResolvedPublication, ExactFontResource) {
    let resource = variant_font_fixture(include_bytes!(
        "../../layout-core/tests/fixtures/NotoEmoji-Regular.ttf"
    ));
    let document = document("adapter-authoring-facts");
    let page_id = document.pages[0].id.clone();
    let mut session = DocumentSession::new(document).unwrap();
    session
        .execute(
            Revision::initial(),
            Transaction::single(
                "Add exact monochrome emoji",
                Command::AddObject {
                    location: ObjectLocation::Page(page_id),
                    object: exact_text(&resource, "\u{1f004}", "Mahjong tile"),
                },
            ),
        )
        .unwrap();
    let publication = ResolvedPublication::resolve_output_with_font_resources(
        &ProjectSnapshot::from_session(&session),
        core::slice::from_ref(&resource),
    )
    .unwrap();
    (publication, resource)
}

fn registered_adapter_font(
    font: print_core::ProductionFont,
) -> print_core::finished::FinishedDocument {
    use print_core::finished::{
        FinishedDocument, FinishedPage, FinishedPageBoxes, FinishedResources, PdfBox,
    };
    let area = PdfBox::new(0., 0., 612., 792.);
    FinishedDocument {
        pages: vec![FinishedPage {
            boxes: FinishedPageBoxes {
                media: area,
                bleed: area,
                trim: area,
            },
            items: vec![],
        }],
        resources: FinishedResources {
            fonts: vec![font.into_finished_resource("adapter-font".into()).unwrap()],
            images: vec![],
        },
    }
}

#[test]
fn automatic_adapter_font_preserves_missing_ascent_authority_before_print() {
    use print_core::finished::{FinishedOutputMode, FinishedPdfError, FinishedPdfOptions};
    use print_core::{FinishedPdfWriter, PdfCandidateMetadata};
    // The actual monochrome tile is above .notdef and has no automatic accent
    // role. Its author knows this is an unaccented symbol; the automatic wrapper
    // has no such assertion. The font-wide capital declaration is usable.
    let (publication, resource) = high_emoji_publication();
    let mut fonts = prepare_publication_fonts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
    )
    .unwrap();
    assert_eq!(fonts.len(), 1);
    let document = registered_adapter_font(fonts.pop().unwrap());
    let metadata = PdfCandidateMetadata::at_utc("Adapter facts", "2026-10-17T00:00:00Z").unwrap();
    FinishedPdfWriter::write(
        &document,
        &FinishedPdfOptions {
            metadata: metadata.clone(),
            mode: FinishedOutputMode::Ordinary,
        },
    )
    .unwrap();
    assert!(matches!(
        FinishedPdfWriter::write(
            &document,
            &FinishedPdfOptions {
                metadata,
                mode: FinishedOutputMode::PdfX4 {
                    output_profile: include_bytes!("fixtures/SWOP2006coated_300_mth.icc"),
                },
            },
        ),
        Err(FinishedPdfError::FontAscentFactsRequired(id)) if id == "adapter-font"
    ));
}

fn retain_adapter_facts_output(
    document: &print_core::finished::FinishedDocument,
    ordinary: &[u8],
    print: &[u8],
) {
    let Some(path) = std::env::var_os("PATTERBOX_ADAPTER_FACTS_OUTPUT") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    assert!(path.is_absolute());
    std::fs::create_dir(&path).unwrap();
    let program = document.resources.fonts[0].program();
    let optional_hash = |value: Option<[u8; 32]>| {
        value.map_or_else(
            || "null".to_owned(),
            |hash| format!("\"{}\"", hex_sha256(hash)),
        )
    };
    let anchors = program
        .remapped_glyphs()
        .iter()
        .map(|entry| {
            format!(
                "{{\"source_gid\":{},\"cid\":{}}}",
                entry.source_glyph_id(),
                entry.cid()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    // Export identity/anchor observations only. The independent checker
    // measures metrics from the retained program and original source.
    let witness = format!(
        concat!(
            "{{\"source_sha256\":\"{}\",\"prepared_sha256\":\"{}\",",
            "\"face_index\":{},\"outline_kind\":\"{:?}\",\"preparation_kind\":\"{:?}\",",
            "\"remapping\":[{}],\"ascent_facts_sha256\":{},\"font_facts_sha256\":{}}}"
        ),
        hex_sha256(program.source_sha256()),
        hex_sha256(program.prepared_sha256()),
        program.face_index(),
        program.outline_kind(),
        program.preparation_kind(),
        anchors,
        optional_hash(program.ascent_facts_sha256()),
        optional_hash(program.font_facts_sha256()),
    );
    for (name, bytes) in [
        ("ordinary.pdf", ordinary),
        ("print.pdf", print),
        ("prepared.font", program.prepared_bytes()),
        ("prepared.json", witness.as_bytes()),
    ] {
        std::fs::write(path.join(name), bytes).unwrap();
    }
}

#[test]
fn adapter_font_facts_preserve_authority_through_finished_print() {
    use print_core::finished::{FinishedOutputMode, FinishedPdfOptions};
    use print_core::{FinishedPdfWriter, PdfCandidateMetadata};
    let (publication, resource) = high_emoji_publication();
    let mut calls = 0;
    let mut fonts = prepare_publication_fonts_with_font_facts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
        |selected| {
            calls += 1;
            assert_eq!(selected.source, resource.source);
            assert_eq!(selected.face_index, resource.face_index);
            assert_eq!(selected.bytes, resource.bytes);
            (Some(vec![]), None)
        },
    )
    .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(fonts.len(), 1);
    let font = fonts.pop().unwrap();
    assert!(!font.requires_ascent_facts());
    assert!(!font.requires_cap_height_fact());
    assert!(font.ascent_facts_sha256().is_some());
    assert!(font.font_facts_sha256().is_some());
    assert_eq!(font.evidence().scene_digest, publication.semantic_digest());
    assert_eq!(
        hex_sha256(font.evidence().source_sha256),
        resource.source.as_str()
    );
    let document = registered_adapter_font(font);
    let metadata = PdfCandidateMetadata::at_utc("Adapter facts", "2026-10-17T00:00:00Z").unwrap();
    let ordinary = FinishedPdfWriter::write(
        &document,
        &FinishedPdfOptions {
            metadata: metadata.clone(),
            mode: FinishedOutputMode::Ordinary,
        },
    )
    .unwrap();
    let pdf = FinishedPdfWriter::write(
        &document,
        &FinishedPdfOptions {
            metadata,
            mode: FinishedOutputMode::PdfX4 {
                output_profile: include_bytes!("fixtures/SWOP2006coated_300_mth.icc"),
            },
        },
    )
    .unwrap();
    assert!(pdf.bytes().starts_with(b"%PDF-1.6"));
    retain_adapter_facts_output(&document, ordinary.bytes(), pdf.bytes());
}

#[test]
fn adapter_font_facts_validate_resources_before_callback() {
    let (publication, mut resource) = high_emoji_publication();
    resource.source = AssetHash::for_bytes(b"a different source");
    let mut called = false;
    let result = prepare_publication_fonts_with_font_facts(
        &publication,
        &[resource],
        &TrustedFontBoundary::resolve_packaged(),
        |_| {
            called = true;
            (Some(vec![]), None)
        },
    );
    assert!(!called);
    assert!(matches!(
        result,
        Err(ProductionFontError::InvalidResourceHash { .. })
    ));
}

#[test]
fn adapter_font_facts_propagate_native_conflicts() {
    let (publication, resource) = high_emoji_publication();
    // This pinned source declares 2163 FUnits at 2048 UPM. An author fact
    // cannot replace that computable declaration with a different value.
    let result = prepare_publication_fonts_with_font_facts(
        &publication,
        core::slice::from_ref(&resource),
        &TrustedFontBoundary::resolve_packaged(),
        |_| (Some(vec![]), Some(2163. * 1000. / 2048. + 1.)),
    );
    assert!(matches!(
        result,
        Err(ProductionFontError::FontWorker {
            error: print_core::FontWorkerError::WorkerRejected { code, .. },
            ..
        }) if code == "cap-height-fact-conflict"
    ));
}
