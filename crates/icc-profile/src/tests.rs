use super::*;

fn put(p: &mut [u8], at: usize, value: usize) {
    p[at..at + 4].copy_from_slice(&u32::try_from(value).unwrap().to_be_bytes());
}
#[allow(clippy::trivially_copy_pass_by_ref)]
fn typed(kind: &[u8; 4], size: usize) -> Vec<u8> {
    let mut p = vec![0; size];
    p[..4].copy_from_slice(kind);
    p
}
fn description() -> Vec<u8> {
    let mut p = typed(b"desc", 92);
    put(&mut p, 8, 2);
    p[12] = b'x';
    p
}
fn localized() -> Vec<u8> {
    let mut p = typed(b"mluc", 30);
    put(&mut p, 8, 1);
    put(&mut p, 12, 12);
    p[16..20].copy_from_slice(b"enUS");
    put(&mut p, 20, 2);
    put(&mut p, 24, 28);
    p[29] = b'x';
    p
}
fn lut(inputs: u8, outputs: u8, eight: bool) -> Vec<u8> {
    let samples = usize::from(inputs) * if eight { 256 } else { 2 }
        + (1_usize << inputs) * usize::from(outputs)
        + usize::from(outputs) * if eight { 256 } else { 2 };
    let mut p = typed(
        if eight { b"mft1" } else { b"mft2" },
        if eight {
            48 + samples
        } else {
            52 + 2 * samples
        },
    );
    p[8..11].copy_from_slice(&[inputs, outputs, 2]);
    for i in [12, 28, 44] {
        put(&mut p, i, 65536);
    }
    if !eight {
        p[48..52].copy_from_slice(&[0, 2, 0, 2]);
    }
    p
}
fn profile(version: [u8; 4], additional: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    let mut tags = vec![
        (
            *b"desc",
            if version[0] == 2 {
                description()
            } else {
                localized()
            },
        ),
        (
            *b"cprt",
            if version[0] == 2 {
                b"text\0\0\0\0x\0".to_vec()
            } else {
                localized()
            },
        ),
        (*b"wtpt", typed(b"XYZ ", 20)),
    ];
    for sig in [*b"A2B0", *b"A2B1", *b"A2B2"] {
        tags.push((sig, lut(4, 3, false)));
    }
    for sig in [*b"B2A0", *b"B2A1", *b"B2A2"] {
        tags.push((sig, lut(3, 4, false)));
    }
    tags.push((*b"gamt", lut(3, 1, false)));
    tags.extend(additional);
    let mut p = vec![0; 132 + tags.len() * 12];
    p[8..12].copy_from_slice(&version);
    p[12..24].copy_from_slice(b"prtrCMYKLab ");
    p[24..36].copy_from_slice(&[7, 232, 0, 2, 0, 29, 0, 23, 0, 59, 0, 59]);
    p[36..40].copy_from_slice(b"acsp");
    p[68..80].copy_from_slice(&[0, 0, 0xf6, 0xd6, 0, 1, 0, 0, 0, 0, 0xd3, 0x2d]);
    put(&mut p, 128, tags.len());
    for (i, (sig, payload)) in tags.into_iter().enumerate() {
        let start = p.len();
        p[132 + 12 * i..136 + 12 * i].copy_from_slice(&sig);
        put(&mut p, 136 + 12 * i, start);
        put(&mut p, 140 + 12 * i, payload.len());
        p.extend(payload);
        while !p.len().is_multiple_of(4) {
            p.push(0);
        }
    }
    let length = p.len();
    put(&mut p, 0, length);
    p
}

#[test]
fn parametric_power_is_defined_throughout_its_active_interval() {
    for version in [[4, 0, 0, 0], [4, 0x10, 0, 0], [4, 0x20, 0, 0]] {
        let curve = |parameters: [i32; 5]| {
            let mut p = typed(b"para", 32);
            p[9] = 3;
            for (at, value) in parameters.into_iter().enumerate() {
                p[12 + at * 4..16 + at * 4].copy_from_slice(&value.to_be_bytes());
            }
            profile(version, vec![(*b"priv", p)])
        };
        // Finite values at both endpoints do not rule out an interior pole.
        assert!(inspect_output(&curve([-65536, -131_072, 65536, 0, 0])).is_err());
        inspect_output(&curve([-65536, 65536, 65536, 0, 0])).unwrap();
        // A zero below the active power branch does not invalidate the curve.
        inspect_output(&curve([-65536, 65536, -32768, 0, 49152])).unwrap();
        inspect_output(&curve([65536, -131_072, 65536, 0, 0])).unwrap();
    }
}

#[test]
fn v4_viewing_illuminance_matches_luminance_only_when_both_are_present() {
    let mut viewing = typed(b"view", 36);
    put(&mut viewing, 12, 65536);
    let mut luminance = typed(b"XYZ ", 20);
    put(&mut luminance, 12, 65536);
    for version in [
        [2, 0x20, 0, 0],
        [4, 0, 0, 0],
        [4, 0x10, 0, 0],
        [4, 0x20, 0, 0],
    ] {
        inspect_output(&profile(version, vec![(*b"view", viewing.clone())])).unwrap();
        inspect_output(&profile(version, vec![(*b"lumi", luminance.clone())])).unwrap();
        inspect_output(&profile(
            version,
            vec![(*b"view", viewing.clone()), (*b"lumi", luminance.clone())],
        ))
        .unwrap();
        let mut mismatch = luminance.clone();
        put(&mut mismatch, 12, 131_072);
        let result = inspect_output(&profile(
            version,
            vec![(*b"view", viewing.clone()), (*b"lumi", mismatch)],
        ));
        assert_eq!(result.is_ok(), version[0] == 2);
    }
}

#[test]
fn original_profile_and_generated_edition_headers_have_exact_byte_checks() {
    inspect_output(include_bytes!(
        "../../print-core/tests/fixtures/SWOP2006coated_300_mth.icc"
    ))
    .unwrap();
    for version in [
        [2, 0x20, 0, 0],
        [4, 0, 0, 0],
        [4, 0x10, 0, 0],
        [4, 0x20, 0, 0],
    ] {
        let original = profile(version, vec![]);
        inspect_output(&original).unwrap();
        for (offset, value) in [
            (27, 13),
            (29, 30),
            (31, 24),
            (33, 60),
            (35, 60),
            (47, 4),
            (63, 16),
            (67, 4),
            (79, 0),
            (127, 1),
        ] {
            let mut p = original.clone();
            p[offset] = value;
            assert!(inspect_output(&p).is_err(), "{version:?} offset {offset}");
        }
        let mut vendor = original;
        vendor[44] = 0xff; // vendor profile flags
        vendor[56] = 0xff; // vendor device attributes
        inspect_output(&vendor).unwrap();
    }
}

#[test]
fn profile_id_uses_the_edition_recipe_and_zero_retains_its_meaning() {
    for minor in [0, 0x10, 0x20] {
        let mut p = profile([4, minor, 0, 0], vec![]);
        p[44] = 1;
        p[56] = 2;
        p[67] = 3;
        let mut normalized = p.clone();
        if minor == 0 {
            normalized[56..64].fill(0);
        } else {
            normalized[44..48].fill(0);
        }
        normalized[64..68].fill(0);
        p[84..100].copy_from_slice(&Md5::digest(&normalized));
        inspect_output(&p).unwrap();
        p[84] ^= 1;
        assert_eq!(inspect_output(&p), Err(Error::ProfileId));
        p[84..100].fill(0);
        inspect_output(&p).unwrap();
    }
}

#[test]
fn dormant_lut8_and_lut16_require_complete_data_and_correct_direction() {
    for eight in [true, false] {
        let good = lut(4, 3, eight);
        for version in [
            [2, 0x20, 0, 0],
            [4, 0, 0, 0],
            [4, 0x10, 0, 0],
            [4, 0x20, 0, 0],
        ] {
            let p = profile(version, vec![(*b"test", good.clone())]);
            inspect_output(&p).unwrap();
            let mut bad = good.clone();
            bad.pop();
            assert!(inspect_output(&profile(version, vec![(*b"test", bad)])).is_err());
        }
    }
}

#[test]
fn localized_text_allows_unfamiliar_locale_and_identical_string_aliases() {
    let mut p = localized();
    p[16..20].copy_from_slice(b"zuZA");
    mluc_extent(&p, true).unwrap();
    p[29] = 0;
    assert_eq!(mluc_extent(&p, true), Ok(p.len()));
    let mut p = localized();
    p[28..30].copy_from_slice(&[0xd8, 0]);
    assert!(mluc_extent(&p, true).is_err());
    let mut p = typed(b"mluc", 42);
    put(&mut p, 8, 2);
    put(&mut p, 12, 12);
    p[16..20].copy_from_slice(b"enUS");
    p[28..32].copy_from_slice(b"zuZA");
    for at in [20, 32] {
        put(&mut p, at, 2);
        put(&mut p, at + 4, 40);
    }
    p[41] = b'x';
    assert_eq!(mluc_extent(&p, true), Ok(42));
}

#[test]
fn adaptation_is_conditional_and_present_matrices_are_invertible_even_when_aliased() {
    let no_adaptation = profile([4, 0x20, 0, 0], vec![]);
    inspect_output(&no_adaptation).unwrap(); // authoring provenance is external
    let mut m = typed(b"sf32", 44);
    for at in [8, 24, 40] {
        put(&mut m, at, 65536);
    }
    inspect_output(&profile([4, 0x20, 0, 0], vec![(*b"chad", m.clone())])).unwrap();
    m[8..].fill(0);
    assert!(inspect_output(&profile([4, 0x20, 0, 0], vec![(*b"chad", m)])).is_err());
}

#[test]
fn opaque_private_data_has_generic_framing_and_standard_types_keep_their_schema() {
    for version in [
        [2, 0x20, 0, 0],
        [4, 0, 0, 0],
        [4, 0x10, 0, 0],
        [4, 0x20, 0, 0],
    ] {
        let data = b"vndr\0\0\0\0\xff\x7f\x81".to_vec();
        inspect_output(&profile(version, vec![(*b"priv", data.clone())])).unwrap();
        let mut bad = data;
        bad[4] = 1;
        assert!(inspect_output(&profile(version, vec![(*b"priv", bad)])).is_err());
        let array = typed(b"ui64", 16);
        inspect_output(&profile(version, vec![(*b"priv", array.clone())])).unwrap();
        let mut bad = array;
        bad.pop();
        assert!(inspect_output(&profile(version, vec![(*b"priv", bad)])).is_err());
    }
}

#[test]
fn retained_standard_types_have_discriminating_structure_controls() {
    let mut measurement = typed(b"meas", 36);
    let mut viewing = typed(b"view", 36);
    let mut date = typed(b"dtim", 20);
    date[8..].copy_from_slice(&[7, 232, 0, 2, 0, 29, 0, 23, 0, 59, 0, 59]);
    let mut para = typed(b"para", 16);
    put(&mut para, 12, 65536);
    let mut order = typed(b"clro", 16);
    put(&mut order, 8, 4);
    order[12..].copy_from_slice(&[0, 1, 2, 3]);
    for (good, at, value) in [
        (measurement.clone(), 11, 3),
        (viewing.clone(), 35, 9),
        (date, 11, 13),
        (para, 9, 5),
        (order, 15, 1),
    ] {
        inspect_output(&profile([4, 0x20, 0, 0], vec![(*b"priv", good.clone())])).unwrap();
        let mut bad = good;
        bad[at] = value;
        assert!(inspect_output(&profile([4, 0x20, 0, 0], vec![(*b"priv", bad)])).is_err());
    }
    measurement[12] = 0x80;
    viewing[8] = 0x80;
    assert!(
        validate_tag_payload(b"priv", &measurement, &profile([4, 0x20, 0, 0], vec![])).is_err()
    );
    assert!(validate_tag_payload(b"priv", &viewing, &profile([4, 0x20, 0, 0], vec![])).is_err());
}

#[test]
fn v4_padding_and_known_measurement_conditions_are_checked_from_exact_bytes() {
    for minor in [0, 0x10, 0x20] {
        let original = profile([4, minor, 0, 0], vec![]);
        inspect_output(&original).unwrap();
        let mut bad = original.clone();
        let offset = profile_u32(&original, 136).unwrap();
        let size = profile_u32(&original, 140).unwrap();
        assert!(!(offset + size).is_multiple_of(4));
        bad[offset + size] = 1;
        assert!(inspect_output(&bad).is_err());
        let mut measurement = typed(b"meas", 36);
        put(&mut measurement, 32, 2);
        assert_eq!(
            inspect_output(&profile(
                [4, minor, 0, 0],
                vec![(*b"meas", measurement.clone())]
            )),
            Err(Error::MissingRequiredOutputTags)
        );
        let mut matrix = typed(b"sf32", 44);
        for at in [8, 24, 40] {
            put(&mut matrix, at, 65536);
        }
        inspect_output(&profile(
            [4, minor, 0, 0],
            vec![(*b"meas", measurement), (*b"chad", matrix)],
        ))
        .unwrap();
    }
}

#[test]
fn localized_shared_substrings_keep_valid_utf16_boundaries() {
    let count = 4096;
    let table = 16 + count * 12;
    let mut p = typed(b"mluc", table + 2 * (count + 1));
    put(&mut p, 8, count);
    put(&mut p, 12, 12);
    for index in 0..count {
        let at = 16 + 12 * index;
        p[at..at + 4].copy_from_slice(b"enUS");
        put(&mut p, at + 4, 2 * (count + 1 - index));
        put(&mut p, at + 8, table + 2 * index);
    }
    for index in 0..=count {
        p[table + 2 * index + 1] = b'x';
    }
    assert_eq!(mluc_extent(&p, true), Ok(p.len()));
    p[table + 2..table + 4].copy_from_slice(&[0xdc, 0]);
    assert!(mluc_extent(&p, true).is_err());
}

#[test]
fn adaptation_role_is_checked_when_a_private_tag_visits_the_shared_body_first() {
    let mut matrix = typed(b"sf32", 44);
    for at in [8, 24, 40] {
        put(&mut matrix, at, 65536);
    }
    let mut p = profile(
        [4, 0x20, 0, 0],
        vec![(*b"priv", matrix.clone()), (*b"chad", matrix)],
    );
    let source = 132 + 10 * 12;
    let target = source + 12;
    let record = p[source + 4..source + 12].to_vec();
    p[target + 4..target + 12].copy_from_slice(&record);
    p.truncate(p.len() - 44);
    let length = p.len();
    put(&mut p, 0, length);
    inspect_output(&p).unwrap();
    let offset = profile_u32(&p, source + 4).unwrap();
    p[offset + 8..offset + 44].fill(0);
    assert!(inspect_output(&p).is_err());
}

#[test]
fn every_remaining_standard_payload_family_has_an_extent_control() {
    for version in [
        [2, 0x20, 0, 0],
        [4, 0, 0, 0],
        [4, 0x10, 0, 0],
        [4, 0x20, 0, 0],
    ] {
        let mut types = vec![
            typed(b"curv", 12),
            typed(b"sf32", 12),
            typed(b"uf32", 12),
            typed(b"ui16", 10),
            typed(b"ui32", 12),
            typed(b"ui64", 16),
            typed(b"sig ", 12),
            typed(b"XYZ ", 20),
            typed(b"meas", 36),
            typed(b"view", 36),
        ];
        let mut data = typed(b"data", 13);
        put(&mut data, 8, 1);
        types.push(data);
        let mut named = typed(b"ncl2", 122);
        put(&mut named, 12, 1);
        named[84] = b'x';
        types.push(named);
        let mut sequence = typed(b"pseq", 32);
        put(&mut sequence, 8, 1);
        sequence.extend(if version[0] == 2 {
            description()
        } else {
            localized()
        });
        sequence.extend(if version[0] == 2 {
            description()
        } else {
            localized()
        });
        types.push(sequence);
        let mut response = typed(b"rcs2", 84);
        response[9] = 4;
        response[11] = 1;
        put(&mut response, 12, 16);
        response[16..20].copy_from_slice(b"StaA");
        types.push(response);
        if version[0] == 4 {
            let mut chroma = typed(b"chrm", 44);
            chroma[9] = 4;
            types.push(chroma);
            let mut colorants = typed(b"clrt", 164);
            put(&mut colorants, 8, 4);
            for i in 0..4 {
                colorants[12 + 38 * i] = b'A' + u8::try_from(i).unwrap();
            }
            types.push(colorants);
        }
        if version[0] == 2 || version[1] == 0 {
            types.push(typed(b"crdi", 28));
            types.push(typed(b"scrn", 16));
            let mut ucr = typed(b"bfd ", 17);
            ucr[16] = 0;
            types.push(ucr);
            let mut settings = typed(b"devs", 48);
            put(&mut settings, 8, 1);
            settings[12..16].copy_from_slice(b"MSFT");
            put(&mut settings, 16, 36);
            put(&mut settings, 20, 1);
            put(&mut settings, 24, 24);
            put(&mut settings, 28, 1);
            settings[32..36].copy_from_slice(b"mtyp");
            put(&mut settings, 36, 4);
            put(&mut settings, 40, 1);
            put(&mut settings, 44, 1);
            types.push(settings);
        }
        if version[0] == 2 {
            let mut named = typed(b"ncol", 24);
            put(&mut named, 12, 1);
            named[18] = b'x';
            types.push(named);
        }
        for good in types {
            let carrier = profile(version, vec![(*b"priv", good.clone())]);
            inspect_output(&carrier)
                .unwrap_or_else(|e| panic!("{version:?} {:?}: {e:?}", &good[..4]));
            let mut bad = good;
            bad.pop();
            // A binary-data byte string has no length modulus. Its malformed
            // flag, rather than a shorter legitimate byte string, discriminates it.
            if &bad[..4] == b"data" {
                bad.resize(13, 0);
                put(&mut bad, 8, 2);
            }
            assert!(inspect_output(&profile(version, vec![(*b"priv", bad)])).is_err());
        }
    }
}

#[test]
fn edition_strength_and_extensible_localized_records_preserve_valid_forms() {
    let mut terminated = localized();
    terminated.extend_from_slice(&[0, 0]);
    put(&mut terminated, 20, 4);
    // Omitting the terminator is a recommendation in the admitted editions.
    for version in [[4, 0, 0, 0], [4, 0x10, 0, 0], [4, 0x20, 0, 0]] {
        assert!(inspect_output(&profile(version, vec![(*b"priv", terminated.clone())])).is_ok());
    }
    let mut alternate = localized();
    alternate.resize(31, 0);
    put(&mut alternate, 20, 3);
    alternate[28..].copy_from_slice(b"abc"); // Known valid UTF-8 context.
    for version in [[4, 0, 0, 0], [4, 0x10, 0, 0], [4, 0x20, 0, 0]] {
        assert_eq!(
            inspect_output(&profile(version, vec![(*b"priv", alternate.clone())])).is_ok(),
            version == [4, 0x20, 0, 0]
        );
    }
    for version in [
        [2, 0x20, 0, 0],
        [4, 0, 0, 0],
        [4, 0x10, 0, 0],
        [4, 0x20, 0, 0],
    ] {
        let mut p = profile(version, vec![]);
        p[24..30].copy_from_slice(&[255, 255, 0, 12, 0, 31]);
        inspect_output(&p).unwrap();
        p[40..44].copy_from_slice(b"TGNT");
        assert_eq!(inspect_output(&p).is_ok(), version != [4, 0x20, 0, 0]);
        p[40..44].fill(0);
        p[64] = 1;
        assert_eq!(inspect_output(&p).is_ok(), version != [4, 0x20, 0, 0]);
    }
    let mut extended = typed(b"mluc", 34);
    put(&mut extended, 8, 1);
    put(&mut extended, 12, 16);
    extended[16..20].copy_from_slice(b"zuZA");
    put(&mut extended, 20, 2);
    put(&mut extended, 24, 32);
    extended[28..32].copy_from_slice(b"extn");
    extended[33] = b'x';
    for minor in [0, 0x10, 0x20] {
        inspect_output(&profile(
            [4, minor, 0, 0],
            vec![(*b"priv", extended.clone())],
        ))
        .unwrap();
    }
    let empty = typed(b"mluc", 16);
    assert_eq!(mluc_extent(&empty, true), Ok(16));
}
