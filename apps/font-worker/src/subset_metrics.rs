use super::{cff_program::Error, truetype_program::Program};
use ttf_parser::{Face, GlyphId};

pub fn normalize(
    source: &Face<'_>,
    bytes: &mut Vec<u8>,
    remapped: &[super::RemappedGlyph],
) -> Result<(), Error> {
    let face = Face::parse(bytes, 0).map_err(|_| ())?;
    if face.tables().glyf.is_none() {
        return Ok(());
    }
    let program = Program::parse(&face, false)?;
    let mut advance_max = 0_u16;
    let mut extrema: Option<[i32; 3]> = None;
    let mut bbox: Option<[i16; 4]> = None;
    for gid in 0..face.number_of_glyphs() {
        let glyph = GlyphId(gid);
        let advance = face.glyph_hor_advance(glyph).ok_or(())?;
        advance_max = advance_max.max(advance);
        // Aggregate caches use retained raw glyf control-point rectangles.
        // The final strict program parse checks those rectangles against actual
        // geometry; an alternate outline parser must not redefine them here.
        if let Some(bounds) = program.cached_bounds(usize::from(gid))? {
            let left = i32::from(face.glyph_hor_side_bearing(glyph).ok_or(())?);
            let extent = left + i32::from(bounds[2]) - i32::from(bounds[0]);
            let right = i32::from(advance) - extent;
            extrema = Some(extrema.map_or([left, right, extent], |old| {
                [old[0].min(left), old[1].min(right), old[2].max(extent)]
            }));
            bbox = Some(bbox.map_or(bounds, |old| {
                [
                    old[0].min(bounds[0]),
                    old[1].min(bounds[1]),
                    old[2].max(bounds[2]),
                    old[3].max(bounds[3]),
                ]
            }));
        }
    }
    let extrema = extrema.unwrap_or([0; 3]);
    let bbox = bbox.unwrap_or([0; 4]);
    let input = Program::parse(source, false)?;
    let originals = program.subset_source_glyphs(&input, remapped)?;
    // A subset must preserve the context of every retained source component,
    // including its tail. Optional unused source context remains deferred.
    for &gid in originals.values() {
        input.width(usize::from(gid))?;
    }
    let mut tables = collect_tables(bytes)?;
    let head = tables.get_mut(b"head").ok_or(())?;
    for (index, value) in bbox.into_iter().enumerate() {
        let start = 36 + index * 2;
        head[start..start + 2].copy_from_slice(&value.to_be_bytes());
    }
    let hhea = tables.get_mut(b"hhea").ok_or(())?;
    hhea[10..12].copy_from_slice(&advance_max.to_be_bytes());
    for (index, value) in extrema.into_iter().enumerate() {
        let start = 12 + index * 2;
        hhea[start..start + 2]
            .copy_from_slice(&i16::try_from(value).map_err(|_| ())?.to_be_bytes());
    }
    if let Some(vertical) = input.vertical_metrics() {
        let mut vhea = source
            .raw_face()
            .table(ttf_parser::Tag::from_bytes(b"vhea"))
            .ok_or(())?
            .to_vec();
        let mut vmtx = Vec::new();
        let mut maximum = 0;
        let mut extrema: Option<[i32; 3]> = None;
        for cid in 0..face.number_of_glyphs() {
            let &(advance, top) = vertical
                .get(usize::from(*originals.get(&cid).ok_or(())?))
                .ok_or(())?;
            maximum = maximum.max(advance);
            vmtx.extend(advance.to_be_bytes());
            vmtx.extend(top.to_be_bytes());
            if let Some(bounds) = program.cached_bounds(usize::from(cid))? {
                let top = i32::from(top);
                let extent = top + i32::from(bounds[3]) - i32::from(bounds[1]);
                let bottom = i32::from(advance) - extent;
                extrema = Some(extrema.map_or([top, bottom, extent], |old| {
                    [old[0].min(top), old[1].min(bottom), old[2].max(extent)]
                }));
            }
        }
        vhea[10..12].copy_from_slice(&maximum.to_be_bytes());
        vhea[34..36].copy_from_slice(&face.number_of_glyphs().to_be_bytes());
        for (i, value) in extrema.unwrap_or([0; 3]).into_iter().enumerate() {
            vhea[12 + i * 2..14 + i * 2]
                .copy_from_slice(&i16::try_from(value).map_err(|_| ())?.to_be_bytes());
        }
        tables.insert(*b"vhea", vhea);
        tables.insert(*b"vmtx", vmtx);
    }
    // Unhinted sources may omit these. Preserve existing programs verbatim.
    for tag in [*b"cvt ", *b"prep", *b"fpgm"] {
        tables.entry(tag).or_default();
    }
    rebuild(bytes, tables)?;
    let (_, head_offset, _) = table(bytes, *b"head")?;
    let adjustment = 0xb1b0_afba_u32.wrapping_sub(checksum(bytes));
    bytes[head_offset + 8..head_offset + 12].copy_from_slice(&adjustment.to_be_bytes());
    Ok(())
}

pub fn has_hint_tables(bytes: &[u8]) -> bool {
    [*b"cvt ", *b"prep", *b"fpgm"]
        .into_iter()
        .all(|tag| table(bytes, tag).is_ok())
}

pub fn replace_table(bytes: &mut Vec<u8>, tag: [u8; 4], replacement: Vec<u8>) -> Result<(), ()> {
    let mut tables = collect_tables(bytes)?;
    if tables.insert(tag, replacement).is_none() {
        return Err(());
    }
    rebuild(bytes, tables)?;
    let (_, head, _) = table(bytes, *b"head")?;
    let adjustment = 0xb1b0_afba_u32.wrapping_sub(checksum(bytes));
    bytes[head + 8..head + 12].copy_from_slice(&adjustment.to_be_bytes());
    Ok(())
}
fn collect_tables(bytes: &[u8]) -> Result<std::collections::BTreeMap<[u8; 4], Vec<u8>>, ()> {
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
    Ok(tables)
}
fn rebuild(
    bytes: &mut Vec<u8>,
    tables: std::collections::BTreeMap<[u8; 4], Vec<u8>>,
) -> Result<(), ()> {
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

pub(super) fn checksum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0_u32, |sum, chunk| {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}
