use ttf_parser::{Face, GlyphId};

pub fn normalize(bytes: &mut Vec<u8>) -> Result<(), ()> {
    let face = Face::parse(bytes, 0).map_err(|_| ())?;
    if face.tables().glyf.is_none() {
        return Ok(());
    }
    ensure_hint_tables(bytes)?;
    let face = Face::parse(bytes, 0).map_err(|_| ())?;
    let mut advance_max = 0_u16;
    let mut extrema: Option<[i32; 3]> = None;
    let mut bbox: Option<[i16; 4]> = None;
    for gid in 0..face.number_of_glyphs() {
        let glyph = GlyphId(gid);
        let advance = face.glyph_hor_advance(glyph).ok_or(())?;
        advance_max = advance_max.max(advance);
        // The parser calculates outlines, including transformed composites;
        // it does not trust the glyf table's cached glyph bounding boxes.
        if let Some(bounds) = face.glyph_bounding_box(glyph) {
            let left = i32::from(face.glyph_hor_side_bearing(glyph).ok_or(())?);
            let extent = left + i32::from(bounds.x_max) - i32::from(bounds.x_min);
            let right = i32::from(advance) - extent;
            extrema = Some(extrema.map_or([left, right, extent], |old| {
                [old[0].min(left), old[1].min(right), old[2].max(extent)]
            }));
            bbox = Some(bbox.map_or(
                [bounds.x_min, bounds.y_min, bounds.x_max, bounds.y_max],
                |old| {
                    [
                        old[0].min(bounds.x_min),
                        old[1].min(bounds.y_min),
                        old[2].max(bounds.x_max),
                        old[3].max(bounds.y_max),
                    ]
                },
            ));
        }
    }
    let extrema = extrema.unwrap_or([0; 3]);
    let bbox = bbox.unwrap_or([0; 4]);
    let (head_record, head_offset, head_length) = table(bytes, *b"head")?;
    let (hhea_record, hhea_offset, hhea_length) = table(bytes, *b"hhea")?;
    if head_length < 54 || hhea_length < 36 {
        return Err(());
    }
    bytes[head_offset + 8..head_offset + 12].fill(0);
    for (index, value) in bbox.into_iter().enumerate() {
        let start = head_offset + 36 + index * 2;
        bytes[start..start + 2].copy_from_slice(&value.to_be_bytes());
    }
    bytes[hhea_offset + 10..hhea_offset + 12].copy_from_slice(&advance_max.to_be_bytes());
    for (index, value) in extrema.into_iter().enumerate() {
        let start = hhea_offset + 12 + index * 2;
        bytes[start..start + 2]
            .copy_from_slice(&i16::try_from(value).map_err(|_| ())?.to_be_bytes());
    }
    for (record, offset, length) in [
        (head_record, head_offset, head_length),
        (hhea_record, hhea_offset, hhea_length),
    ] {
        let value = checksum(&bytes[offset..offset + length]);
        bytes[record + 4..record + 8].copy_from_slice(&value.to_be_bytes());
    }
    let adjustment = 0xb1b0_afba_u32.wrapping_sub(checksum(bytes));
    bytes[head_offset + 8..head_offset + 12].copy_from_slice(&adjustment.to_be_bytes());
    Ok(())
}

pub fn has_hint_tables(bytes: &[u8]) -> bool {
    [*b"cvt ", *b"prep", *b"fpgm"]
        .into_iter()
        .all(|tag| table(bytes, tag).is_ok())
}

fn ensure_hint_tables(bytes: &mut Vec<u8>) -> Result<(), ()> {
    if has_hint_tables(bytes) {
        return Ok(());
    }
    let count = usize::from(u16::from_be_bytes(
        bytes.get(4..6).ok_or(())?.try_into().map_err(|_| ())?,
    ));
    let mut tables = std::collections::BTreeMap::new();
    for index in 0..count {
        let tag: [u8; 4] = bytes
            .get(12 + index * 16..16 + index * 16)
            .ok_or(())?
            .try_into()
            .map_err(|_| ())?;
        let (_, offset, length) = table(bytes, tag)?;
        let mut payload = bytes[offset..offset + length].to_vec();
        if tag == *b"head" {
            payload.get_mut(8..12).ok_or(())?.fill(0);
        }
        tables.insert(tag, payload);
    }
    // Unhinted source fonts can omit these tables. An embedded subset carries
    // explicit empty programs/CVT; existing programs are preserved byte-for-byte.
    for tag in [*b"cvt ", *b"prep", *b"fpgm"] {
        tables.entry(tag).or_default();
    }
    let count = u16::try_from(tables.len()).map_err(|_| ())?;
    let power = 1_u16
        .checked_shl(u16::BITS - count.leading_zeros() - 1)
        .ok_or(())?;
    let search = power.checked_mul(16).ok_or(())?;
    let range_shift = count.checked_mul(16).ok_or(())? - search;
    let mut output = vec![0; 12 + usize::from(count) * 16];
    output[..4].copy_from_slice(&bytes[..4]);
    for (index, value) in [
        count,
        search,
        u16::try_from(power.trailing_zeros()).map_err(|_| ())?,
        range_shift,
    ]
    .into_iter()
    .enumerate()
    {
        output[4 + index * 2..6 + index * 2].copy_from_slice(&value.to_be_bytes());
    }
    for (index, (tag, payload)) in tables.into_iter().enumerate() {
        let record = 12 + index * 16;
        output[record..record + 4].copy_from_slice(&tag);
        for (field, value) in [
            checksum(&payload),
            u32::try_from(output.len()).map_err(|_| ())?,
            u32::try_from(payload.len()).map_err(|_| ())?,
        ]
        .into_iter()
        .enumerate()
        {
            output[record + 4 + field * 4..record + 8 + field * 4]
                .copy_from_slice(&value.to_be_bytes());
        }
        output.extend_from_slice(&payload);
        output.resize((output.len() + 3) & !3, 0);
    }
    *bytes = output;
    Ok(())
}

fn table(bytes: &[u8], tag: [u8; 4]) -> Result<(usize, usize, usize), ()> {
    let count = usize::from(u16::from_be_bytes(
        bytes.get(4..6).ok_or(())?.try_into().map_err(|_| ())?,
    ));
    for index in 0..count {
        let record = 12 + index * 16;
        let entry = bytes.get(record..record + 16).ok_or(())?;
        if entry[..4] == tag {
            let offset =
                usize::try_from(u32::from_be_bytes(entry[8..12].try_into().map_err(|_| ())?))
                    .map_err(|_| ())?;
            let length = usize::try_from(u32::from_be_bytes(
                entry[12..16].try_into().map_err(|_| ())?,
            ))
            .map_err(|_| ())?;
            if offset
                .checked_add(length)
                .is_none_or(|end| end > bytes.len())
            {
                return Err(());
            }
            return Ok((record, offset, length));
        }
    }
    Err(())
}

fn checksum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0_u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}
