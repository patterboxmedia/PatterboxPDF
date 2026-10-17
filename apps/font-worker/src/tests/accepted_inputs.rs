fn repair_checksums(bytes: &mut [u8]) {
    fn sum(bytes: &[u8]) -> u32 {
        bytes.chunks(4).fold(0u32, |v, b| {
            let mut w = [0; 4];
            w[..b.len()].copy_from_slice(b);
            v.wrapping_add(u32::from_be_bytes(w))
        })
    }
    let count = usize::from(u16::from_be_bytes(bytes[4..6].try_into().unwrap()));
    let mut head = 0;
    for i in 0..count {
        let r = 12 + i * 16;
        let at = u32::from_be_bytes(bytes[r + 8..r + 12].try_into().unwrap()) as usize;
        if &bytes[r..r + 4] == b"head" {
            head = at;
            bytes[at + 8..at + 12].fill(0);
        }
    }
    for i in 0..count {
        let r = 12 + i * 16;
        let at = u32::from_be_bytes(bytes[r + 8..r + 12].try_into().unwrap()) as usize;
        let n = u32::from_be_bytes(bytes[r + 12..r + 16].try_into().unwrap()) as usize;
        let check = sum(&bytes[at..at + n]);
        bytes[r + 4..r + 8].copy_from_slice(&check.to_be_bytes());
    }
    let adjust = 0xb1b0_afba_u32.wrapping_sub(sum(bytes));
    bytes[head + 8..head + 12].copy_from_slice(&adjust.to_be_bytes());
}
fn name_version_one_control(tag: Option<&str>) -> Vec<u8> {
    let mut font = include_bytes!("components/metrics-unmarked.ttf").to_vec();
    let face = Face::parse(&font, 0).unwrap();
    let mut name = face
        .raw_face()
        .table(Tag::from_bytes(b"name"))
        .unwrap()
        .to_vec();
    let count = usize::from(u16::from_be_bytes(name[2..4].try_into().unwrap()));
    let base = usize::from(u16::from_be_bytes(name[4..6].try_into().unwrap()));
    let records_end = 6 + count * 12;
    let storage = name[base..].to_vec();
    let mut extra = Vec::new();
    extra.extend(u16::from(tag.is_some()).to_be_bytes());
    let tag_bytes = tag
        .map(|v| {
            v.encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if tag.is_some() {
        extra.extend(u16::try_from(tag_bytes.len()).unwrap().to_be_bytes());
        extra.extend(u16::try_from(storage.len()).unwrap().to_be_bytes());
        for record in name[6..records_end].as_chunks_mut::<12>().0.iter_mut() {
            if record[..2] == [0, 3] {
                record[4..6].copy_from_slice(&0x8000_u16.to_be_bytes());
            }
        }
    }
    name[..2].copy_from_slice(&1_u16.to_be_bytes());
    name[4..6].copy_from_slice(&u16::try_from(base + extra.len()).unwrap().to_be_bytes());
    name.splice(records_end..records_end, extra);
    name.extend(tag_bytes);
    subset_metrics::replace_table(&mut font, *b"name", name).unwrap();
    font
}
fn allow_name_control_subset(font: &mut [u8]) {
    let face = Face::parse(font, 0).unwrap();
    let os2 = face.raw_face().table(Tag::from_bytes(b"OS/2")).unwrap();
    let at = os2.as_ptr() as usize - font.as_ptr() as usize;
    font[at + 8..at + 10].fill(0);
    repair_checksums(font);
}
#[test]
fn valid_version_one_naming_prepares_without_changing_full_source_bytes() {
    for tag in [None, Some("en-US")] {
        for subset in [false, true] {
            let mut font = name_version_one_control(tag);
            if subset {
                allow_name_control_subset(&mut font);
            }
            let (report, prepared) = prepared_control(&font);
            if subset {
                assert!(matches!(report.preparation_kind, PreparationKind::Subset));
            } else {
                assert_eq!(prepared, font);
            }
            assert_eq!(report.postscript_name, "PDFLibraryComponentMetricsunmarked");
        }
    }
}
#[test]
fn retained_postscript_records_cannot_hide_invalid_or_disagreeing_names() {
    for replacement in *b" Z" {
        let mut font = include_bytes!("components/metrics-unmarked.ttf").to_vec();
        let face = Face::parse(&font, 0).unwrap();
        let mut name = face
            .raw_face()
            .table(Tag::from_bytes(b"name"))
            .unwrap()
            .to_vec();
        let count = usize::from(u16::from_be_bytes(name[2..4].try_into().unwrap()));
        let base = usize::from(u16::from_be_bytes(name[4..6].try_into().unwrap()));
        let record = name[6..6 + count * 12]
            .as_chunks::<12>()
            .0
            .iter()
            .find(|r| r[..2] == [0, 1] && r[6..8] == [0, 6])
            .unwrap();
        let len = usize::from(u16::from_be_bytes(record[8..10].try_into().unwrap()));
        let start = base + usize::from(u16::from_be_bytes(record[10..12].try_into().unwrap()));
        name[start + len - 1] = replacement;
        subset_metrics::replace_table(&mut font, *b"name", name).unwrap();
        for subset in [false, true] {
            let mut font = font.clone();
            if subset {
                allow_name_control_subset(&mut font);
            }
            let directory = TestDirectory::new();
            let result = process_request(&request(
                &directory,
                &font,
                PreparationPolicy::Publication,
                vec![0, 3],
            ));
            assert!(
                matches!(result, Err(WorkerError::MalformedFont)),
                "{result:?}"
            );
            assert!(!directory.path("prepared.font").exists());
        }
    }
}
#[test]
fn unknown_postscript_encoding_preserves_bytes_without_guessing_identity() {
    let mut font = include_bytes!("components/metrics-unmarked.ttf").to_vec();
    let face = Face::parse(&font, 0).unwrap();
    let mut name = face
        .raw_face()
        .table(Tag::from_bytes(b"name"))
        .unwrap()
        .to_vec();
    let count = usize::from(u16::from_be_bytes(name[2..4].try_into().unwrap()));
    let base = usize::from(u16::from_be_bytes(name[4..6].try_into().unwrap()));
    let mut records = name[6..6 + count * 12].as_chunks::<12>().0.to_vec();
    let record = records
        .iter_mut()
        .find(|r| r[..2] == [0, 1] && r[6..8] == [0, 6])
        .unwrap();
    record[2..4].copy_from_slice(&99_u16.to_be_bytes());
    let start = base + usize::from(u16::from_be_bytes(record[10..12].try_into().unwrap()));
    name[start] = 0xff; // Unknown encoding: this byte has no guessed identity.
    records.sort_by(|a, b| a[..8].cmp(&b[..8]));
    for (target, record) in name[6..6 + count * 12]
        .as_chunks_mut::<12>()
        .0
        .iter_mut()
        .zip(records)
    {
        *target = record;
    }
    subset_metrics::replace_table(&mut font, *b"name", name).unwrap();
    for subset in [false, true] {
        let mut font = font.clone();
        if subset {
            allow_name_control_subset(&mut font);
        }
        let (report, prepared) = prepared_control(&font);
        assert_eq!(report.postscript_name, "PDFLibraryComponentMetricsunmarked");
        if !subset {
            assert_eq!(prepared, font);
        }
    }
}
fn prepared_control(font: &[u8]) -> (PreparationReport, Vec<u8>) {
    let directory = TestDirectory::new();
    let face = Face::parse(font, 0).unwrap();
    let gid = face.glyph_index('H').unwrap().0;
    let report = preparation_report(
        process_request(&request(
            &directory,
            font,
            PreparationPolicy::Publication,
            vec![0, gid],
        ))
        .unwrap(),
    );
    (
        report,
        std::fs::read(directory.path("prepared.font")).unwrap(),
    )
}
#[test]
fn accepted_cff_affine_flex_and_empty_controls_complete() {
    for font in [
        include_bytes!("fixtures/default.otf").as_slice(),
        include_bytes!("fixtures/top.otf"),
        include_bytes!("fixtures/fd.otf"),
        include_bytes!("fixtures/reflected.otf"),
        include_bytes!("fixtures/fractional.otf"),
        include_bytes!("fixtures/hflex.otf"),
        include_bytes!("fixtures/flex.otf"),
        include_bytes!("fixtures/hflex1.otf"),
        include_bytes!("fixtures/flex1.otf"),
        include_bytes!("fixtures/hflex_top.otf"),
        include_bytes!("fixtures/flex_top.otf"),
        include_bytes!("fixtures/hflex1_top.otf"),
        include_bytes!("fixtures/flex1_top.otf"),
        include_bytes!("fixtures/hflex_fd.otf"),
        include_bytes!("fixtures/flex_fd.otf"),
        include_bytes!("fixtures/hflex1_fd.otf"),
        include_bytes!("fixtures/flex1_fd.otf"),
        include_bytes!("fixtures/empty.otf"),
    ] {
        let (report, _) = prepared_control(font);
        assert!(report.pdf_metrics.bbox.iter().all(|v| v.is_finite()));
        assert_eq!(
            report.pdf_metrics.widths.len(),
            usize::from(report.embedded_glyph_count)
        );
    }
    let (top, _) = prepared_control(include_bytes!("fixtures/top.otf"));
    assert_eq!(
        top.pdf_metrics.bbox.map(f32::to_bits),
        [103_f32, -81., 1205., 1081.].map(f32::to_bits)
    );
    let (width, _) = prepared_control(include_bytes!("fixtures/width-diff.otf"));
    let (default, _) = prepared_control(include_bytes!("fixtures/default.otf"));
    assert_eq!(width.pdf_metrics.widths, default.pdf_metrics.widths);
    assert_eq!(top.pdf_metrics.widths, default.pdf_metrics.widths);
    assert!(top.pdf_metrics.italic_angle < -16. && top.pdf_metrics.italic_angle > -17.);
}
#[test]
fn cumulative_fixed_point_deltas_survive_native_outline_measurement() {
    let (report, _) = prepared_control(include_bytes!("fixtures/accumulated-fraction.otf"));
    assert_eq!(
        report.pdf_metrics.bbox.map(f32::to_bits),
        [100_f32, -120., 1001., 880.].map(f32::to_bits)
    );
}
#[test]
fn incomplete_and_partial_cff_controls_never_produce_a_prepared_resource() {
    for font in [
        include_bytes!("fixtures/no-endchar.otf").as_slice(),
        include_bytes!("fixtures/partial.otf"),
        include_bytes!("fixtures/subr-no-return.otf"),
    ] {
        let directory = TestDirectory::new();
        let gid = Face::parse(font, 0).unwrap().glyph_index('H').unwrap().0;
        assert!(
            process_request(&request(
                &directory,
                font,
                PreparationPolicy::Publication,
                vec![0, gid]
            ))
            .is_err()
        );
        assert!(!directory.path("prepared.font").exists());
    }
}
#[test]
fn partial_cff_callbacks_cannot_be_mistaken_for_a_completed_glyph() {
    let bytes = include_bytes!("fixtures/partial.otf");
    let face = Face::parse(bytes, 0).unwrap();
    let gid = face.glyph_index('H').unwrap();
    let mut outline = outline::Outline::new(outline::IDENTITY);
    let result = face.tables().cff.unwrap().outline(gid, &mut outline);
    assert!(
        outline.bounds.is_some(),
        "partial decoder emitted real path callbacks"
    );
    assert!(result.is_err(), "completion is separately required");
    assert!(
        pdf_metrics::prepare(
            &face,
            &face,
            &[
                RemappedGlyph {
                    source_glyph_id: 0,
                    cid: 0
                },
                RemappedGlyph {
                    source_glyph_id: gid.0,
                    cid: gid.0
                }
            ],
            false,
            None
        )
        .is_err()
    );
}
#[test]
fn full_truetype_keeps_exact_bytes_and_point_attached_composites() {
    let font = include_bytes!("fixtures/original.ttf");
    let (report, bytes) = prepared_control(font);
    assert_eq!(bytes, font);
    assert!(matches!(report.preparation_kind, PreparationKind::Full));
    // Attached curve starts at H's point 2=(90,700); its quadratic reaches
    // 750.5. Its top-level bearing shifts x by190. The off-curve control
    // point at y=801 is not a filled extremum.
    assert_eq!(
        report.pdf_metrics.bbox.map(f32::to_bits),
        [0_f32, 0., 380., 751.].map(f32::to_bits)
    );
    assert_eq!(report.pdf_metrics.ascent.to_bits(), 750.5_f32.to_bits());
}
#[test]
fn malformed_unused_full_glyph_is_rejected_inside_preparation() {
    let mut font = include_bytes!("fixtures/original.ttf").to_vec();
    let face = Face::parse(&font, 0).unwrap();
    let raw = face.raw_face();
    let loca = raw.table(Tag::from_bytes(b"loca")).unwrap();
    let head = raw.table(Tag::from_bytes(b"head")).unwrap();
    let offset = if &head[50..52] == b"\0\0" {
        usize::from(u16::from_be_bytes(loca[6..8].try_into().unwrap())) * 2
    } else {
        u32::from_be_bytes(loca[12..16].try_into().unwrap()) as usize
    };
    let glyph = raw.table(Tag::from_bytes(b"glyf")).unwrap();
    let glyf_at = glyph.as_ptr() as usize - font.as_ptr() as usize;
    // GID3 is not requested by prepared_control; its first component becomes
    // an out-of-range GID. Recalculate checksums to isolate glyph validity.
    font[glyf_at + offset + 12..glyf_at + offset + 14].copy_from_slice(&65535u16.to_be_bytes());
    repair_checksums(&mut font);
    let directory = TestDirectory::new();
    assert!(
        process_request(&request(
            &directory,
            &font,
            PreparationPolicy::Publication,
            vec![0, 1]
        ))
        .is_err()
    );
    assert!(!directory.path("prepared.font").exists());
}
#[test]
fn retained_full_font_metadata_and_capacity_fail_before_success() {
    for (tag, offset, bytes) in [
        (*b"post", 0, [0x12, 0x34]),
        (*b"hhea", 24, [0, 1]),
        (*b"maxp", 6, [0, 0]),
        (*b"name", 4, [0xff, 0xff]),
    ] {
        let mut font = include_bytes!("fixtures/original.ttf").to_vec();
        let face = Face::parse(&font, 0).unwrap();
        let table = face.raw_face().table(Tag::from_bytes(&tag)).unwrap();
        let at = table.as_ptr() as usize - font.as_ptr() as usize;
        font[at + offset..at + offset + 2].copy_from_slice(&bytes);
        repair_checksums(&mut font);
        let directory = TestDirectory::new();
        assert!(
            process_request(&request(
                &directory,
                &font,
                PreparationPolicy::Publication,
                vec![0, 1]
            ))
            .is_err(),
            "{tag:?} mutation"
        );
        assert!(!directory.path("prepared.font").exists());
    }
}
#[test]
fn full_program_with_inconsistent_cached_glyph_bounds_is_refused() {
    // The imported full DejaVu source has concrete cached glyph extrema
    // that disagree with its point data. A subset is normalized separately;
    // rights-required full bytes may not be repaired by the preparation path.
    let font = font_with_fs_type(FONT, 0x0108);
    let directory = TestDirectory::new();
    let error = process_request(&request(
        &directory,
        &font,
        PreparationPolicy::Publication,
        vec![0, 36, 37],
    ))
    .unwrap_err();
    assert_error(&error, "malformed-font", None);
    assert!(!directory.path("prepared.font").exists());
}

#[test]
fn unhinted_full_truetype_zone_count_neighbors() {
    // The original control has no glyph/fpgm/prep instructions. Zone1-only
    // programs may declare1; range checking does not prove twilight usage.
    for count in [0_u16, 1, 2, 3] {
        let mut font = include_bytes!("fixtures/original.ttf").to_vec();
        let face = Face::parse(&font, 0).unwrap();
        let table = face.raw_face().table(Tag::from_bytes(b"maxp")).unwrap();
        let at = table.as_ptr() as usize - font.as_ptr() as usize;
        font[at + 14..at + 16].copy_from_slice(&count.to_be_bytes());
        repair_checksums(&mut font);
        let directory = TestDirectory::new();
        let result = process_request(&request(
            &directory,
            &font,
            PreparationPolicy::Publication,
            vec![0, 1],
        ));
        assert_eq!(result.is_ok(), matches!(count, 1 | 2), "maxZones={count}");
        if result.is_err() {
            assert!(!directory.path("prepared.font").exists());
        }
    }
}

#[test]
fn exact_decimal_boundary_enclosure_preserves_zero_and_integral_neighbors() {
    for (font, expected) in [
        (
            include_bytes!("fixtures/default.otf").as_slice(),
            [100_f32, -120., 900., 880.],
        ),
        (
            include_bytes!("fixtures/tiny-positive.otf").as_slice(),
            [100., -120., 901., 880.],
        ),
        (
            include_bytes!("fixtures/tiny-negative.otf").as_slice(),
            [99., -120., 900., 880.],
        ),
        (
            include_bytes!("fixtures/integer-translation.otf").as_slice(),
            [101., -120., 901., 880.],
        ),
    ] {
        let (report, _) = prepared_control(font);
        assert_eq!(
            report.pdf_metrics.bbox.map(f32::to_bits),
            expected.map(f32::to_bits)
        );
    }
}

#[test]
fn absent_top_normalization_preserves_equivalent_source_geometry() {
    for (implicit, explicit, expected) in [
        (
            include_bytes!("fixtures/implicit-default.otf").as_slice(),
            include_bytes!("fixtures/explicit-default.otf").as_slice(),
            [100_f32, -120., 900., 880.],
        ),
        (
            include_bytes!("fixtures/implicit-scaled.otf").as_slice(),
            include_bytes!("fixtures/explicit-scaled.otf").as_slice(),
            [100., -150., 900., 1100.],
        ),
        (
            include_bytes!("fixtures/implicit-translated.otf").as_slice(),
            include_bytes!("fixtures/explicit-translated.otf").as_slice(),
            [120., -140., 920., 1110.],
        ),
    ] {
        let (a, _) = prepared_control(implicit);
        let (b, _) = prepared_control(explicit);
        assert_eq!(
            a.pdf_metrics.bbox.map(f32::to_bits),
            expected.map(f32::to_bits)
        );
        assert_eq!(
            a.pdf_metrics.bbox.map(f32::to_bits),
            b.pdf_metrics.bbox.map(f32::to_bits)
        );
        assert_eq!(a.pdf_metrics.widths, b.pdf_metrics.widths);
    }
}

#[test]
fn malformed_pinned_normalization_envelope_is_refused() {
    let source = include_bytes!("fixtures/implicit-translated.otf");
    let face = Face::parse(source, 0).unwrap();
    let gid = face.glyph_index('H').unwrap().0;
    let mut remapper = subsetter::GlyphRemapper::new();
    remapper.remap(0);
    remapper.remap(gid);
    let mut subset = subsetter::subset(source, 0, &remapper).unwrap();
    let parsed = Face::parse(&subset, 0).unwrap();
    let mut cff = parsed
        .raw_face()
        .table(Tag::from_bytes(b"CFF "))
        .unwrap()
        .to_vec();
    cff.push(0); // A trailing byte breaks the pinned complete-INDEX envelope.
    subset_metrics::replace_table(&mut subset, *b"CFF ", cff).unwrap();
    assert!(
        cff_normalize::normalize(
            &face,
            &mut subset,
            &[
                RemappedGlyph {
                    source_glyph_id: 0,
                    cid: 0
                },
                RemappedGlyph {
                    source_glyph_id: gid,
                    cid: 1
                }
            ]
        )
        .is_err()
    );
}

fn with_ascent_facts(
    mut request: WorkerRequest,
    font: &[u8],
    facts: Option<Vec<u16>>,
) -> WorkerRequest {
    let WorkerRequest::Prepare {
        accented_source_glyph_ids,
        expected_ascent_facts_sha256,
        face_index,
        expected_font_facts_sha256,
        cap_height_pdf_units,
        ..
    } = &mut request
    else {
        panic!("preparation request");
    };
    *expected_ascent_facts_sha256 = facts
        .as_ref()
        .map(|ids| hex(ascent_facts_hash(sha256(font), *face_index, ids)));
    *expected_font_facts_sha256 = font_facts_hash(
        sha256(font),
        *face_index,
        facts.as_deref(),
        *cap_height_pdf_units,
    )
    .map(hex);
    *accented_source_glyph_ids = facts;
    request
}
fn ascent_control(font: &[u8], gids: Vec<u16>, facts: Option<Vec<u16>>) -> PreparationReport {
    let directory = TestDirectory::new();
    let value = with_ascent_facts(
        request(&directory, font, PreparationPolicy::Publication, gids),
        font,
        facts,
    );
    preparation_report(process_request(&value).unwrap())
}
#[test]
fn authored_high_unencoded_ascent_needs_explicit_classification() {
    let font = include_bytes!("fixtures/ascent-unencoded-high.otf");
    let unresolved = ascent_control(font, vec![0, 1, 2], None);
    assert!(unresolved.requires_ascent_facts);
    assert!(unresolved.ascent_facts_sha256.is_none());
    assert_eq!(
        unresolved.pdf_metrics.ascent.to_bits(),
        1100_f32.to_bits(),
        "unresolved ordinary compatibility heuristic"
    );
    let authored = ascent_control(font, vec![0, 1, 2], Some(vec![2]));
    assert!(!authored.requires_ascent_facts);
    assert!(authored.ascent_facts_sha256.is_some());
    assert_eq!(authored.pdf_metrics.ascent.to_bits(), 880_f32.to_bits());
    assert_eq!(authored.prepared_sha256, unresolved.prepared_sha256);
    assert_eq!(
        authored.pdf_metrics.bbox.map(f32::to_bits),
        unresolved.pdf_metrics.bbox.map(f32::to_bits)
    );
    for (neighbor, expected) in [
        (
            include_bytes!("fixtures/ascent-coded-high.otf").as_slice(),
            880_f32,
        ),
        (
            include_bytes!("fixtures/ascent-unencoded-low.otf").as_slice(),
            880_f32,
        ),
    ] {
        let report = ascent_control(neighbor, vec![0, 1, 2], None);
        assert!(!report.requires_ascent_facts);
        assert_eq!(report.pdf_metrics.ascent.to_bits(), expected.to_bits());
    }
}
#[test]
fn facts_none_empty_presence_and_malformed_identity_are_distinct() {
    // This existing H control has no accented-only roles; [] is truthful.
    let font = include_bytes!("fixtures/default.otf");
    let automatic = ascent_control(font, vec![0, 1], None);
    let empty = ascent_control(font, vec![0, 1], Some(vec![]));
    assert!(!automatic.requires_ascent_facts && !empty.requires_ascent_facts);
    assert!(automatic.ascent_facts_sha256.is_none());
    assert_eq!(
        empty.ascent_facts_sha256,
        Some(hex(ascent_facts_hash(sha256(font), 0, &[])))
    );
    for facts in [vec![0], vec![1, 1], vec![2, 1], vec![2]] {
        let directory = TestDirectory::new();
        let value = with_ascent_facts(
            request(&directory, font, PreparationPolicy::Publication, vec![0, 1]),
            font,
            Some(facts),
        );
        assert_error(
            &process_request(&value).unwrap_err(),
            "invalid-ascent-facts",
            None,
        );
        assert!(!directory.path("prepared.font").exists());
    }
    for expected in [
        None,
        Some(hex([0; 32])),
        Some(hex(ascent_facts_hash(sha256(font), 1, &[]))),
        Some(hex(ascent_facts_hash([0; 32], 0, &[]))),
    ] {
        let directory = TestDirectory::new();
        let mut value = with_ascent_facts(
            request(&directory, font, PreparationPolicy::Publication, vec![0, 1]),
            font,
            Some(vec![]),
        );
        let WorkerRequest::Prepare {
            expected_ascent_facts_sha256,
            ..
        } = &mut value
        else {
            unreachable!()
        };
        *expected_ascent_facts_sha256 = expected;
        assert_error(
            &process_request(&value).unwrap_err(),
            "ascent-facts-hash-mismatch",
            None,
        );
        assert!(!directory.path("prepared.font").exists());
    }
}
fn font_with_unicode_mappings(font: &[u8], mappings: &[(u32, u16)]) -> Vec<u8> {
    let mut cmap = Vec::new();
    cmap.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 4, 0, 0, 0, 12]);
    cmap.extend_from_slice(&[0, 12, 0, 0]);
    cmap.extend_from_slice(&(16 + 12 * u32::try_from(mappings.len()).unwrap()).to_be_bytes());
    cmap.extend_from_slice(&0_u32.to_be_bytes());
    cmap.extend_from_slice(&u32::try_from(mappings.len()).unwrap().to_be_bytes());
    for &(cp, gid) in mappings {
        cmap.extend_from_slice(&cp.to_be_bytes());
        cmap.extend_from_slice(&cp.to_be_bytes());
        cmap.extend_from_slice(&u32::from(gid).to_be_bytes());
    }
    let mut out = font.to_vec();
    subset_metrics::replace_table(&mut out, *b"cmap", cmap).unwrap();
    out
}
#[test]
fn cap_height_empty_known_h_is_explicit_zero() {
    let (report, _) = prepared_control(include_bytes!("fixtures/empty.otf"));
    assert_eq!(
        report.pdf_metrics.cap_height.map(f32::to_bits),
        Some(0_f32.to_bits())
    );
}
#[test]
fn cap_height_old_os2_l_reference_is_measured() {
    let original = include_bytes!("fixtures/original.ttf");
    let mut font = font_with_unicode_mappings(original, &[(0x4c, 1)]);
    let face = Face::parse(&font, 0).unwrap();
    let mut os2 = face.raw_face().table(Tag::from_bytes(b"OS/2")).unwrap()[..78].to_vec();
    os2[..2].copy_from_slice(&0_u16.to_be_bytes());
    os2[8..10].copy_from_slice(&0_u16.to_be_bytes());
    subset_metrics::replace_table(&mut font, *b"OS/2", os2).unwrap();
    let report = ascent_control(&font, vec![1], None);
    assert_eq!(
        report.pdf_metrics.cap_height.map(f32::to_bits),
        Some(700_f32.to_bits())
    );
}
#[test]
fn cap_height_usable_declaration_does_not_require_a_reference() {
    let original = include_bytes!("fixtures/default.otf");
    let font = font_with_unicode_mappings(original, &[(0x51, 1)]);
    let expected = f32::from(Face::parse(&font, 0).unwrap().capital_height().unwrap());
    assert!(expected > 0.);
    let report = ascent_control(&font, vec![1], None);
    assert_eq!(
        report.pdf_metrics.cap_height.map(f32::to_bits),
        Some(expected.to_bits())
    );
}
#[test]
fn cap_height_declaration_uses_global_upem_units_across_cff_matrices() {
    for font in [
        include_bytes!("fixtures/default.otf").as_slice(),
        include_bytes!("fixtures/top.otf"),
        include_bytes!("fixtures/fd.otf"),
        include_bytes!("fixtures/reflected.otf"),
    ] {
        let face = Face::parse(font, 0).unwrap();
        let expected =
            f32::from(face.capital_height().unwrap()) * 1000. / f32::from(face.units_per_em());
        let (report, _) = prepared_control(font);
        assert_eq!(
            report.pdf_metrics.cap_height.map(f32::to_bits),
            Some(expected.to_bits())
        );
        assert!(!report.requires_cap_height_fact);
    }
}
fn font_with_cap_declaration(font: &[u8], cap: i16) -> Vec<u8> {
    let face = Face::parse(font, 0).unwrap();
    let mut os2 = face
        .raw_face()
        .table(Tag::from_bytes(b"OS/2"))
        .unwrap()
        .to_vec();
    assert!(os2.len() >= 90);
    os2[88..90].copy_from_slice(&cap.to_be_bytes());
    let mut out = font.to_vec();
    subset_metrics::replace_table(&mut out, *b"OS/2", os2).unwrap();
    out
}
fn with_cap_fact(mut request: WorkerRequest, font: &[u8], cap: Option<f32>) -> WorkerRequest {
    let WorkerRequest::Prepare {
        cap_height_pdf_units,
        accented_source_glyph_ids,
        expected_font_facts_sha256,
        face_index,
        ..
    } = &mut request
    else {
        panic!("prepare");
    };
    *cap_height_pdf_units = cap;
    *expected_font_facts_sha256 = font_facts_hash(
        sha256(font),
        *face_index,
        accented_source_glyph_ids.as_deref(),
        cap,
    )
    .map(hex);
    request
}
fn unknown_cap_source() -> Vec<u8> {
    font_with_cap_declaration(
        &font_with_unicode_mappings(include_bytes!("fixtures/default.otf"), &[(0x51, 1)]),
        0,
    )
}
#[test]
fn cap_height_unknown_authority_and_explicit_facts_preserve_program() {
    let font = unknown_cap_source();
    let automatic = ascent_control(&font, vec![1], None);
    assert!(automatic.requires_cap_height_fact);
    assert!(automatic.pdf_metrics.cap_height.is_none());
    assert!(automatic.font_facts_sha256.is_none());
    for cap in [0_f32, -0., 700.] {
        let directory = TestDirectory::new();
        let value = with_cap_fact(
            request(&directory, &font, PreparationPolicy::Publication, vec![1]),
            &font,
            Some(cap),
        );
        let report = preparation_report(process_request(&value).unwrap());
        assert!(!report.requires_cap_height_fact);
        assert_eq!(
            report.pdf_metrics.cap_height.map(f32::to_bits),
            Some(canonical_zero(cap).to_bits())
        );
        assert_eq!(report.prepared_sha256, automatic.prepared_sha256);
        assert_eq!(
            report.pdf_metrics.bbox.map(f32::to_bits),
            automatic.pdf_metrics.bbox.map(f32::to_bits)
        );
        assert_eq!(report.pdf_metrics.widths, automatic.pdf_metrics.widths);
        assert_eq!(
            report.font_facts_sha256,
            font_facts_hash(sha256(&font), 0, None, Some(cap)).map(hex)
        );
    }
    assert_eq!(
        font_facts_hash(sha256(&font), 0, None, Some(-0.)),
        font_facts_hash(sha256(&font), 0, None, Some(0.))
    );
    assert_ne!(
        font_facts_hash(sha256(&font), 0, None, None),
        font_facts_hash(sha256(&font), 0, Some(&[]), None)
    );
    assert_ne!(
        font_facts_hash(sha256(&font), 0, None, Some(0.)),
        font_facts_hash(sha256(&font), 0, Some(&[]), Some(0.))
    );
}
#[test]
fn cap_height_conflicts_and_malformed_facts_refuse_before_output() {
    let font = font_with_cap_declaration(include_bytes!("fixtures/default.otf"), 900);
    assert_eq!(
        ascent_control(&font, vec![1], None)
            .pdf_metrics
            .cap_height
            .map(f32::to_bits),
        Some(900_f32.to_bits()),
        "designer declaration may differ from H outline"
    );
    for (cap, code) in [
        (700., "cap-height-fact-conflict"),
        (f32::NAN, "invalid-cap-height-fact"),
        (f32::INFINITY, "invalid-cap-height-fact"),
        (10_000_001., "invalid-cap-height-fact"),
    ] {
        let directory = TestDirectory::new();
        let value = with_cap_fact(
            request(&directory, &font, PreparationPolicy::Publication, vec![1]),
            &font,
            Some(cap),
        );
        assert_error(&process_request(&value).unwrap_err(), code, None);
        assert!(!directory.path("prepared.font").exists());
    }
    let directory = TestDirectory::new();
    let value = with_cap_fact(
        request(&directory, &font, PreparationPolicy::Publication, vec![1]),
        &font,
        Some(900.),
    );
    assert!(process_request(&value).is_ok());
}
#[test]
fn cap_height_fact_hash_binds_source_face_presence_and_value() {
    let font = unknown_cap_source();
    let correct = font_facts_hash(sha256(&font), 0, None, Some(700.)).map(hex);
    for wrong in [
        None,
        font_facts_hash([0; 32], 0, None, Some(700.)).map(hex),
        font_facts_hash(sha256(&font), 1, None, Some(700.)).map(hex),
        font_facts_hash(sha256(&font), 0, Some(&[]), Some(700.)).map(hex),
        font_facts_hash(sha256(&font), 0, None, Some(701.)).map(hex),
    ] {
        assert_ne!(correct, wrong);
        let directory = TestDirectory::new();
        let mut value = with_cap_fact(
            request(&directory, &font, PreparationPolicy::Publication, vec![1]),
            &font,
            Some(700.),
        );
        let WorkerRequest::Prepare {
            expected_font_facts_sha256,
            ..
        } = &mut value
        else {
            unreachable!()
        };
        *expected_font_facts_sha256 = wrong;
        assert_error(
            &process_request(&value).unwrap_err(),
            "font-facts-hash-mismatch",
            None,
        );
        assert!(!directory.path("prepared.font").exists());
    }
}
#[test]
fn cap_height_empty_reference_does_not_classify_unknown_aliases() {
    let font = font_with_unicode_mappings(
        include_bytes!("fixtures/empty.otf"),
        &[(0x48, 1), (0x51, 1)],
    );
    let report = ascent_control(&font, vec![1], None);
    assert!(report.requires_cap_height_fact);
    assert!(report.pdf_metrics.cap_height.is_none());
}
#[test]
fn cap_height_facts_keep_rights_required_full_font_bytes_and_independent_ascent() {
    let font = font_with_unicode_mappings(include_bytes!("fixtures/original.ttf"), &[(0x51, 1)]);
    let automatic = ascent_control(&font, vec![1], None);
    assert!(automatic.requires_cap_height_fact && automatic.requires_ascent_facts);
    let directory = TestDirectory::new();
    let value = with_cap_fact(
        with_ascent_facts(
            request(&directory, &font, PreparationPolicy::Publication, vec![1]),
            &font,
            Some(vec![]),
        ),
        &font,
        Some(700.),
    );
    let report = preparation_report(process_request(&value).unwrap());
    assert!(matches!(report.preparation_kind, PreparationKind::Full));
    assert_eq!(
        std::fs::read(directory.path("prepared.font")).unwrap(),
        font
    );
    assert_eq!(report.prepared_sha256, automatic.prepared_sha256);
    assert_eq!(report.pdf_metrics.widths, automatic.pdf_metrics.widths);
    assert_eq!(
        report.pdf_metrics.ascent.to_bits(),
        automatic.pdf_metrics.ascent.to_bits()
    );
    assert!(!report.requires_cap_height_fact && !report.requires_ascent_facts);
    assert!(report.ascent_facts_sha256.is_some() && report.font_facts_sha256.is_some());
}
#[test]
fn conflicting_and_unclassified_unicode_aliases_remain_unknown() {
    let font = include_bytes!("fixtures/ascent-coded-high.otf");
    for mappings in [
        vec![(0x41, 2), (0x48, 1), (0x124, 2)],
        vec![(0x48, 1), (0x124, 2), (0xe000, 2)],
        vec![(0x48, 1), (0x302, 2)],
    ] {
        let altered = font_with_unicode_mappings(font, &mappings);
        let report = ascent_control(&altered, vec![0, 1, 2], None);
        assert!(
            report.requires_ascent_facts,
            "mixed A/E and unclassified aliases never select one convenient identity"
        );
        let authored = ascent_control(&altered, vec![0, 1, 2], Some(vec![2]));
        assert!(!authored.requires_ascent_facts);
        assert_eq!(authored.pdf_metrics.ascent.to_bits(), 880_f32.to_bits());
    }
}
#[test]
fn facts_scope_includes_full_unused_glyphs_and_component_tails() {
    let original = include_bytes!("fixtures/original.ttf");
    let full = font_with_unicode_mappings(original, &[(0x48, 1)]);
    let unresolved = ascent_control(&full, vec![0, 1], None);
    assert!(unresolved.requires_ascent_facts);
    assert_eq!(unresolved.embedded_glyph_count, 4);
    assert_eq!(unresolved.prepared_sha256, hex(sha256(&full)));
    let authored = ascent_control(&full, vec![0, 1], Some(vec![3]));
    assert!(!authored.requires_ascent_facts);
    assert_eq!(authored.pdf_metrics.ascent.to_bits(), 700_f32.to_bits());
    assert_eq!(authored.prepared_sha256, unresolved.prepared_sha256);
    // GID171's source component graph retains a spacing acute not in the public
    // requested prefix. Facts[171] affirmatively include that component role.
    let automatic = ascent_control(FONT, vec![171], None);
    assert!(automatic.embedded_glyph_count > 2);
    assert!(automatic.requires_ascent_facts);
    let authored = ascent_control(FONT, vec![171], Some(vec![171]));
    assert!(!authored.requires_ascent_facts);
    assert_eq!(authored.prepared_sha256, automatic.prepared_sha256);
    // A valid nonretained GID is permitted and participates in the fact hash.
    let narrow = ascent_control(FONT, vec![36], Some(vec![171]));
    assert!(!narrow.requires_ascent_facts);
}

fn glyph_request_control(
    font: &[u8],
    gids: Vec<u16>,
    facts: Option<Vec<u16>>,
) -> (PreparationReport, Vec<u8>) {
    let directory = TestDirectory::new();
    let value = with_ascent_facts(
        request(&directory, font, PreparationPolicy::Publication, gids),
        font,
        facts,
    );
    let report = preparation_report(
        process_request(&value)
            .expect("valid positioned glyph request must not need a source cmap anchor"),
    );
    let bytes = std::fs::read(directory.path("prepared.font")).unwrap();
    assert_eq!(report.prepared_sha256, hex(sha256(&bytes)));
    assert_eq!(report.prepared_byte_length, bytes.len());
    (report, bytes)
}
#[test]
fn unencoded_only_requests_prepare_without_a_cmap_anchor() {
    for (font, automatic_ascent, requires_facts, bbox_top) in [
        (
            include_bytes!("fixtures/ascent-unencoded-high.otf").as_slice(),
            1100_f32,
            true,
            1100_f32,
        ),
        (
            include_bytes!("fixtures/ascent-unencoded-low.otf").as_slice(),
            880_f32,
            false,
            880_f32,
        ),
    ] {
        let (automatic, automatic_bytes) = glyph_request_control(font, vec![2], None);
        let (authored, authored_bytes) = glyph_request_control(font, vec![2], Some(vec![2]));
        assert_eq!(
            automatic_bytes, authored_bytes,
            "classification does not rewrite prepared programs"
        );
        assert_eq!(automatic.used_glyph_count, 1);
        assert_eq!(automatic.embedded_glyph_count, 2);
        let mapping = automatic
            .remapped_glyphs
            .iter()
            .map(|g| (g.source_glyph_id, g.cid))
            .collect::<Vec<_>>();
        assert_eq!(
            mapping,
            vec![(0, 0), (2, 1)],
            "no hidden encoded H is retained"
        );
        assert_eq!(
            (automatic.exercise_cid, automatic.exercise_unicode),
            (1, 0xfffd)
        );
        assert_eq!(
            (authored.exercise_cid, authored.exercise_unicode),
            (1, 0xfffd)
        );
        assert_eq!(
            automatic.pdf_metrics.ascent.to_bits(),
            automatic_ascent.to_bits()
        );
        assert_eq!(automatic.requires_ascent_facts, requires_facts);
        assert_eq!(authored.pdf_metrics.ascent.to_bits(), 880_f32.to_bits());
        assert!(!authored.requires_ascent_facts);
        assert_eq!(
            automatic.pdf_metrics.bbox.map(f32::to_bits),
            [100_f32, -120., 900., bbox_top].map(f32::to_bits)
        );
        assert_eq!(
            automatic.pdf_metrics.bbox.map(f32::to_bits),
            authored.pdf_metrics.bbox.map(f32::to_bits)
        );
        assert_eq!(automatic.pdf_metrics.widths, authored.pdf_metrics.widths);
        assert_eq!(automatic.pdf_metrics.widths, [1000_f32, 728.]);
        assert!(automatic.ascent_facts_sha256.is_none());
        assert_eq!(
            authored.ascent_facts_sha256,
            Some(hex(ascent_facts_hash(sha256(font), 0, &[2])))
        );
        assert_eq!(
            cff_program::Program::parse(&automatic_bytes)
                .unwrap()
                .width(1)
                .unwrap()
                .to_bits(),
            728_f64.to_bits()
        );
    }
    let coded = include_bytes!("fixtures/ascent-coded-high.otf");
    let (automatic, automatic_bytes) = glyph_request_control(coded, vec![2], None);
    let (authored, authored_bytes) = glyph_request_control(coded, vec![2], Some(vec![2]));
    assert_eq!(automatic_bytes, authored_bytes);
    assert!(!automatic.requires_ascent_facts);
    assert_eq!(automatic.pdf_metrics.ascent.to_bits(), 880_f32.to_bits());
    assert_eq!(
        (automatic.exercise_cid, automatic.exercise_unicode),
        (1, 0x124)
    );
    assert_eq!(
        (authored.exercise_cid, authored.exercise_unicode),
        (1, 0x124)
    );
}
#[test]
fn diagnostic_exercise_preserves_mapped_preference_and_notdef_only_policy() {
    let high = include_bytes!("fixtures/ascent-unencoded-high.otf");
    let (mapped, _) = glyph_request_control(high, vec![1, 2], None);
    assert_eq!(
        (mapped.exercise_cid, mapped.exercise_unicode),
        (1, u32::from('H'))
    );
    let (missing, bytes) = glyph_request_control(high, vec![0], None);
    assert_eq!(missing.used_glyph_count, 1);
    assert_eq!(missing.embedded_glyph_count, 1);
    assert_eq!(
        (missing.exercise_cid, missing.exercise_unicode),
        (0, 0xfffd)
    );
    assert!(!missing.requires_ascent_facts);
    assert_eq!(missing.pdf_metrics.ascent.to_bits(), 880_f32.to_bits());
    assert!(!bytes.is_empty());
}
#[test]
fn unencoded_exercise_fallback_preserves_full_bytes_and_retained_authority() {
    let font = font_with_unicode_mappings(include_bytes!("fixtures/original.ttf"), &[(0x48, 1)]);
    let (automatic, bytes) = glyph_request_control(&font, vec![2], None);
    let (authored, authored_bytes) = glyph_request_control(&font, vec![2], Some(vec![3]));
    assert_eq!(bytes, font);
    assert_eq!(authored_bytes, font);
    assert!(matches!(automatic.preparation_kind, PreparationKind::Full));
    assert_eq!(automatic.used_glyph_count, 1);
    assert_eq!(automatic.embedded_glyph_count, 4);
    assert_eq!(automatic.remapped_glyphs.len(), 1);
    assert_eq!(
        (
            automatic.remapped_glyphs[0].source_glyph_id,
            automatic.remapped_glyphs[0].cid
        ),
        (2, 2)
    );
    assert_eq!(
        (automatic.exercise_cid, automatic.exercise_unicode),
        (2, 0xfffd)
    );
    assert!(automatic.requires_ascent_facts);
    assert!(!authored.requires_ascent_facts);
    assert_eq!(authored.pdf_metrics.ascent.to_bits(), 700_f32.to_bits());
}

fn selected_name_control(kind: u8) -> Vec<u8> {
    let mut font = include_bytes!("components/metrics-unmarked.ttf").to_vec();
    let face = Face::parse(&font, 0).unwrap();
    let raw = face.raw_face().table(Tag::from_bytes(b"name")).unwrap();
    let count = usize::from(u16::from_be_bytes(raw[2..4].try_into().unwrap()));
    let storage = usize::from(u16::from_be_bytes(raw[4..6].try_into().unwrap()));
    let mut records = Vec::new();
    for record in raw[6..6 + count * 12].as_chunks::<12>().0 {
        let platform = u16::from_be_bytes(record[..2].try_into().unwrap());
        let id = u16::from_be_bytes(record[6..8].try_into().unwrap());
        let identity = matches!(id, 1 | 6 | 16 | 21);
        if identity && ((kind < 2 && platform != 1) || (kind == 2 && platform != 3)) {
            continue;
        }
        let length = usize::from(u16::from_be_bytes(record[8..10].try_into().unwrap()));
        let at = storage + usize::from(u16::from_be_bytes(record[10..12].try_into().unwrap()));
        let mut key = *record;
        let mut value = raw[at..at + length].to_vec();
        if kind == 1 && id == 1 {
            value = b"Caf\x8e".to_vec();
        }
        if kind == 2 && platform == 3 {
            key[2..4].copy_from_slice(&10_u16.to_be_bytes());
        }
        records.push((key, value));
    }
    records.sort_by(|a, b| a.0[..8].cmp(&b.0[..8]));
    let mut name = Vec::new();
    name.extend(0_u16.to_be_bytes());
    name.extend(u16::try_from(records.len()).unwrap().to_be_bytes());
    name.extend(u16::try_from(6 + records.len() * 12).unwrap().to_be_bytes());
    let mut strings = Vec::new();
    for (mut key, value) in records {
        key[8..10].copy_from_slice(&u16::try_from(value.len()).unwrap().to_be_bytes());
        key[10..12].copy_from_slice(&u16::try_from(strings.len()).unwrap().to_be_bytes());
        name.extend(key);
        strings.extend(value);
    }
    name.extend(strings);
    subset_metrics::replace_table(&mut font, *b"name", name).unwrap();
    font
}

#[test]
fn known_raw_naming_encodings_prepare_and_inspect_without_a_unicode_alias() {
    for kind in 0..3 {
        for subset in [false, true] {
            let mut font = selected_name_control(kind);
            if subset {
                allow_name_control_subset(&mut font);
            }
            let directory = TestDirectory::new();
            let source = directory.path("source.ttf");
            std::fs::write(&source, &font).unwrap();
            let inspection = process_inspection(source.to_str().unwrap(), &hex(sha256(&font)), 0)
                .expect("declared MacRoman and Windows10 families are usable naming records");
            assert_eq!(
                inspection.family_names,
                [if kind == 1 {
                    "Café"
                } else {
                    "PDFLibraryComponentMetricsunmarked"
                }]
            );
            let (report, prepared) = prepared_control(&font);
            assert_eq!(report.postscript_name, "PDFLibraryComponentMetricsunmarked");
            if subset {
                assert!(matches!(report.preparation_kind, PreparationKind::Subset));
            } else {
                assert_eq!(prepared, font, "full naming source bytes remain exact");
            }
        }
    }
}
