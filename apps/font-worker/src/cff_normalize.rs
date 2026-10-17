//! Correct the pinned subsetter's absent-Top CID translation normalization.
//! Replace the pinned writer's terminal `FDArray` and rebase only the following
//! `CharStrings` INDEX. Private/Subrs remain fixed; repack the sfnt wrapper.
use super::RemappedGlyph;
use super::cff_program::{Program, index, number};
use ttf_parser::{Face, Tag};
type Result<T> = std::result::Result<T, ()>;
type Entry = (u16, std::ops::Range<usize>, std::ops::Range<usize>);
fn entries(raw: &[u8]) -> Result<Vec<Entry>> {
    let mut at = 0;
    let mut start = 0;
    let mut result = Vec::new();
    while at < raw.len() {
        let b = raw[at];
        if matches!(b,28..=30|32..=254) {
            number(raw, &mut at, true).map_err(|_| ())?;
            continue;
        }
        let end = at;
        at += 1;
        let key = if b == 12 {
            let v = *raw.get(at).ok_or(())?;
            at += 1;
            1200 + u16::from(v)
        } else {
            u16::from(b)
        };
        result.push((key, start..end, start..at));
        start = at;
    }
    if at != start {
        return Err(());
    }
    Ok(result)
}
fn encode_bcd_times_1000(text: &str) -> Result<Vec<u8>> {
    let (mantissa, exponent) = text
        .split_once(['E', 'e'])
        .map_or((text, ""), |(a, b)| (a, b));
    let negative = mantissa.starts_with('-');
    let mantissa = mantissa.trim_start_matches(['-', '+']);
    let decimal = mantissa.find('.').unwrap_or(mantissa.len());
    let mut digits = mantissa.replace('.', "");
    let place = decimal.checked_add(3).ok_or(())?;
    if place >= digits.len() {
        digits.extend(std::iter::repeat_n('0', place - digits.len()));
    } else {
        digits.insert(place, '.');
    }
    let text = format!(
        "{}{}{}",
        if negative { "-" } else { "" },
        digits,
        if exponent.is_empty() {
            String::new()
        } else {
            format!("E{exponent}")
        }
    );
    let mut nibbles = Vec::new();
    for b in text.replace("E-", "Q").bytes() {
        match b {
            b'0'..=b'9' => nibbles.push(b - b'0'),
            b'.' => nibbles.push(10),
            b'E' => nibbles.push(11),
            b'Q' => nibbles.push(12),
            b'-' => nibbles.push(14),
            b'+' => {}
            _ => return Err(()),
        }
    }
    nibbles.push(15);
    if nibbles.len() % 2 != 0 {
        nibbles.push(15);
    }
    let mut output = vec![30];
    output.extend(nibbles.as_chunks::<2>().0.iter().map(|p| p[0] * 16 + p[1]));
    Ok(output)
}
pub fn normalize(
    source: &Face<'_>,
    prepared: &mut Vec<u8>,
    remapped: &[RemappedGlyph],
) -> Result<()> {
    let Some(original) = source.raw_face().table(Tag::from_bytes(b"CFF ")) else {
        return Ok(());
    };
    let original = Program::parse(original).map_err(|_| ())?;
    if !original.top.contains_key(&1230)
        || original.top.contains_key(&1207)
        || original.fds.iter().all(|fd| fd.matrix_texts.is_none())
    {
        return Ok(());
    }
    let face = Face::parse(prepared, 0).map_err(|_| ())?;
    let raw = face.raw_face().table(Tag::from_bytes(b"CFF ")).ok_or(())?;
    let parsed = Program::parse(raw).map_err(|_| ())?;
    let (_, at) = index(raw, usize::from(raw[2])).map_err(|_| ())?;
    let (tops, _) = index(raw, at).map_err(|_| ())?;
    let top = *tops.first().ok_or(())?;
    let top_at = top.as_ptr() as usize - raw.as_ptr() as usize;
    let offset = super::cff_program::offset(&parsed.top, 1236).map_err(|_| ())?;
    let (fds, fd_end) = index(raw, offset).map_err(|_| ())?;
    let chars_at = super::cff_program::offset(&parsed.top, 17).map_err(|_| ())?;
    let (_, chars_end) = index(raw, chars_at).map_err(|_| ())?;
    // This rewrite applies only to the pinned writer's established layout.
    if fd_end != chars_at || chars_end != raw.len() {
        return Err(());
    }
    let mut selected = vec![None; fds.len()];
    for m in remapped {
        let new = *parsed.select.get(usize::from(m.cid)).ok_or(())?;
        let old = *original
            .select
            .get(usize::from(m.source_glyph_id))
            .ok_or(())?;
        if selected[new].is_some_and(|previous| previous != old) {
            return Err(());
        }
        selected[new] = Some(old);
    }
    let mut replacements = Vec::new();
    for (i, fd) in fds.iter().enumerate() {
        let mut output = Vec::new();
        for (key, _, whole) in entries(fd)? {
            if key != 1207 {
                output.extend_from_slice(&fd[whole]);
            }
        }
        let source_fd = &original.fds[selected[i].ok_or(())?];
        if let Some(texts) = &source_fd.matrix_texts {
            for text in texts {
                output.extend(encode_bcd_times_1000(text)?);
            }
        } else {
            output.extend_from_slice(&[140, 139, 139, 140, 139, 139]);
        }
        output.extend_from_slice(&[12, 7]);
        replacements.push(output);
    }
    let mut fd_index = Vec::new();
    fd_index.extend_from_slice(
        &u16::try_from(replacements.len())
            .map_err(|_| ())?
            .to_be_bytes(),
    );
    fd_index.push(4);
    let mut end = 1_u32;
    fd_index.extend_from_slice(&end.to_be_bytes());
    for fd in &replacements {
        end = end
            .checked_add(u32::try_from(fd.len()).map_err(|_| ())?)
            .ok_or(())?;
        fd_index.extend_from_slice(&end.to_be_bytes());
    }
    for fd in replacements {
        fd_index.extend(fd);
    }
    let mut replacement = raw[..offset].to_vec();
    replacement.extend(fd_index);
    let new_chars_at = i32::try_from(replacement.len()).map_err(|_| ())?;
    replacement.extend_from_slice(&raw[chars_at..]);
    let (_, operand, _) = entries(top)?.into_iter().find(|e| e.0 == 17).ok_or(())?;
    if operand.len() != 5 || top[operand.start] != 29 {
        return Err(());
    }
    replacement[top_at + operand.start + 1..top_at + operand.end]
        .copy_from_slice(&new_chars_at.to_be_bytes());
    Program::parse(&replacement).map_err(|_| ())?;
    super::subset_metrics::replace_table(prepared, *b"CFF ", replacement)
}
