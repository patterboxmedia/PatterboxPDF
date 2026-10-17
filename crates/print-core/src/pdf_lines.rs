//! Line formatting for this writer's pinned `pdf-writer` output only.
//!
//! The upstream primitive/buffer interfaces are sealed. Format the finished
//! classic-xref file instead of restricting useful titles or font-width arrays.
//! Streams are copied by their direct Length, never by searching their payload
//! for delimiters. Only syntax whitespace, literal-string continuations and hex
//! string whitespace change; relocated objects get corrected xref offsets.
use crate::finished::FinishedPdfError;

const MAX_LINE: usize = 255;

pub(crate) fn finish(bytes: &[u8]) -> Result<Vec<u8>, FinishedPdfError> {
    format_generated(bytes).ok_or(FinishedPdfError::ResourceLimit)
}

fn number(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn format_generated(bytes: &[u8]) -> Option<Vec<u8>> {
    let marker = b"\nstartxref\n";
    let footer = bytes
        .windows(marker.len())
        .rposition(|part| part == marker)?;
    let offset_start = footer + marker.len();
    let offset_end = offset_start
        + bytes
            .get(offset_start..)?
            .iter()
            .position(|b| *b == b'\n')?;
    if bytes.get(offset_end..)? != b"\n%%EOF" {
        return None;
    }
    let xref_offset = number(bytes.get(offset_start..offset_end)?)?;
    let table = bytes.get(xref_offset..footer)?;
    let header_end = table.iter().skip(5).position(|b| *b == b'\n')? + 5;
    let count = number(table.get(7..header_end)?)?;
    if !table.starts_with(b"xref\n0 ") {
        return None;
    }
    let entries_start = xref_offset + header_end + 1;
    let entries_end = entries_start.checked_add(count.checked_mul(20)?)?;
    if !bytes.get(entries_end..)?.starts_with(b"trailer\n") {
        return None;
    }
    let mut objects = vec![];
    for (id, entry) in bytes
        .get(entries_start..entries_end)?
        .as_chunks::<20>()
        .0
        .iter()
        .enumerate()
    {
        if entry.get(10) != Some(&b' ')
            || entry.get(16) != Some(&b' ')
            || entry.get(18..) != Some(b"\r\n")
            || !matches!(entry[17], b'f' | b'n')
        {
            return None;
        }
        if entry[17] == b'n' {
            let offset = number(&entry[..10])?;
            let header = format!("{id} 0 obj\n");
            if !bytes.get(offset..)?.starts_with(header.as_bytes()) || offset >= xref_offset {
                return None;
            }
            objects.push((offset, id));
        }
    }
    objects.sort_unstable();
    let mut out = Formatter::default();
    out.raw(bytes.get(..objects.first()?.0)?);
    let mut relocated = vec![None; count];
    for (index, &(offset, id)) in objects.iter().enumerate() {
        let end = objects.get(index + 1).map_or(xref_offset, |next| next.0);
        if offset >= end {
            return None;
        }
        relocated[id] = Some(out.bytes.len());
        out.object(bytes.get(offset..end)?)?;
    }
    let new_xref = out.bytes.len();
    out.raw(bytes.get(xref_offset..entries_start)?);
    for (id, entry) in bytes
        .get(entries_start..entries_end)?
        .as_chunks::<20>()
        .0
        .iter()
        .enumerate()
    {
        if let Some(offset) = relocated[id] {
            let digits = format!("{offset:010}");
            if digits.len() != 10 {
                return None;
            }
            out.raw(digits.as_bytes());
            out.raw(&entry[10..]);
        } else {
            out.raw(entry);
        }
    }
    out.object(bytes.get(entries_end..footer)?)?;
    out.raw(marker);
    out.raw(new_xref.to_string().as_bytes());
    out.raw(b"\n%%EOF");
    Some(out.bytes)
}

fn white(byte: u8) -> bool {
    matches!(byte, 0 | b'\t' | b'\n' | 12 | b'\r' | b' ')
}

fn delimiter(byte: u8) -> bool {
    white(byte) || b"()<>[]{}/%".contains(&byte)
}

fn escape_length(bytes: &[u8]) -> usize {
    if bytes.starts_with(b"\\\r\n") {
        3
    } else if bytes.get(1).is_some_and(|b| matches!(b, b'0'..=b'7')) {
        1 + bytes
            .iter()
            .skip(1)
            .take(3)
            .take_while(|b| matches!(b, b'0'..=b'7'))
            .count()
    } else {
        2.min(bytes.len())
    }
}

fn literal_end(bytes: &[u8]) -> Option<usize> {
    let mut depth = 1_usize;
    let mut index = 1;
    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'\\' => {
                index += escape_length(&bytes[index..]);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

#[derive(Default)]
struct Formatter {
    bytes: Vec<u8>,
    column: usize,
}

impl Formatter {
    fn raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
        self.column = bytes
            .iter()
            .rposition(|b| matches!(b, b'\n' | b'\r'))
            .map_or(self.column + bytes.len(), |index| bytes.len() - index - 1);
    }

    fn token(&mut self, bytes: &[u8]) -> Option<()> {
        if bytes.len() > MAX_LINE {
            return None;
        }
        if self.column + bytes.len() > MAX_LINE {
            self.raw(b"\n");
        }
        self.raw(bytes);
        Some(())
    }

    fn literal(&mut self, bytes: &[u8]) -> Option<()> {
        if self.column + bytes.len() <= MAX_LINE {
            self.raw(bytes);
            return Some(());
        }
        self.token(b"(")?;
        let mut index = 1;
        while index < bytes.len() {
            let length = if bytes[index] == b'\\' {
                escape_length(&bytes[index..])
            } else {
                1
            };
            // Reserve one column for a continuation backslash. Escape atoms
            // (including octal escapes) must never be split internally.
            let atom = bytes.get(index..index + length)?;
            let newline = atom.iter().position(|b| matches!(b, b'\n' | b'\r'));
            let (width, limit) = newline.map_or((length, MAX_LINE - 1), |end| (end, MAX_LINE));
            if self.column + width > limit {
                self.raw(b"\\\n");
            }
            self.raw(atom);
            index += length;
        }
        Some(())
    }

    fn hex(&mut self, bytes: &[u8]) -> Option<()> {
        if self.column + bytes.len() <= MAX_LINE {
            self.raw(bytes);
            return Some(());
        }
        self.token(b"<")?;
        for byte in &bytes[1..] {
            if self.column == MAX_LINE && !matches!(byte, b'\n' | b'\r') {
                self.raw(b"\n");
            }
            self.raw(std::slice::from_ref(byte));
        }
        Some(())
    }

    fn object(&mut self, bytes: &[u8]) -> Option<()> {
        let mut index = 0;
        let mut depth = 0_usize;
        let mut expect_length = false;
        let mut stream_length = None;
        while let Some(&byte) = bytes.get(index) {
            if white(byte) {
                if self.column == MAX_LINE && !matches!(byte, b'\n' | b'\r') {
                    self.raw(b"\n");
                } else {
                    self.raw(&bytes[index..=index]);
                }
                index += 1;
                continue;
            }
            let length = match byte {
                b'(' => {
                    let end = literal_end(&bytes[index..])?;
                    self.literal(&bytes[index..index + end])?;
                    index += end;
                    continue;
                }
                b'<' if bytes.get(index + 1) != Some(&b'<') => {
                    let end = bytes[index..].iter().position(|b| *b == b'>')? + 1;
                    self.hex(&bytes[index..index + end])?;
                    index += end;
                    continue;
                }
                b'<' => {
                    depth += 1;
                    2
                }
                b'>' if bytes.get(index + 1) == Some(&b'>') => {
                    depth = depth.checked_sub(1)?;
                    2
                }
                b'[' | b']' => 1,
                b'%' => bytes[index..]
                    .iter()
                    .position(|b| matches!(b, b'\n' | b'\r'))
                    .unwrap_or(bytes.len() - index),
                b')' | b'>' | b'{' | b'}' => return None,
                _ => {
                    1 + bytes[index + 1..]
                        .iter()
                        .position(|b| delimiter(*b))
                        .unwrap_or(bytes.len() - index - 1)
                }
            };
            let token = bytes.get(index..index + length)?;
            self.token(token)?;
            if expect_length {
                stream_length = Some(number(token)?);
                expect_length = false;
            } else if depth == 1 && token == b"/Length" {
                expect_length = true;
            } else if depth == 0 && token == b"stream" {
                index += length;
                if bytes.get(index) != Some(&b'\n') {
                    return None;
                }
                let end = (index + 1).checked_add(stream_length.take()?)?;
                if !bytes.get(end..)?.starts_with(b"\nendstream\nendobj") {
                    return None;
                }
                self.raw(bytes.get(index..end)?);
                index = end;
                continue;
            }
            index += length;
        }
        if depth == 0 && !expect_length {
            Some(())
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_writer::{Finish, Name, Pdf, Ref, Str, TextStr};

    #[test]
    fn short_output_is_byte_identical() {
        let mut pdf = Pdf::new();
        pdf.indirect(Ref::new(1))
            .dict()
            .pair(Name(b"Title"), TextStr("Short title"));
        let bytes = pdf.finish();
        assert_eq!(finish(&bytes).unwrap(), bytes);
    }

    #[test]
    fn raw_stream_delimiters_are_never_treated_as_syntax() {
        let payload = [
            b"stream\n/Length 1\nendstream\nendobj\nxref\nstartxref\n%%EOF\r()\\".as_slice(),
            &[0, 255, 1],
            &vec![b'x'; 2_000],
        ]
        .concat();
        let mut pdf = Pdf::new();
        pdf.indirect(Ref::new(1))
            .dict()
            .pair(Name(b"Title"), TextStr(&"A".repeat(900)));
        pdf.stream(Ref::new(5), &payload)
            .pair(Name(b"Note"), Str(b"stream /Length 0 endobj"));
        pdf.indirect(Ref::new(3)).array().items(0..2_000).finish();
        pdf.indirect(Ref::new(4)).primitive(Str(&[255; 900]));
        pdf.catalog(Ref::new(6)).pages(Ref::new(7));
        pdf.pages(Ref::new(7)).count(1).kids([Ref::new(8)]);
        pdf.page(Ref::new(8))
            .parent(Ref::new(7))
            .media_box(pdf_writer::Rect::new(0.0, 0.0, 200.0, 300.0))
            .resources();
        let before = pdf.finish();
        let bytes = finish(&before).unwrap();
        assert!(bytes.windows(payload.len()).any(|part| part == payload));
        assert_eq!(finish(&bytes).unwrap(), bytes);
        if let Some(directory) = std::env::var_os("PDF_LIBRARY_LINE_ARTIFACTS") {
            let directory = std::path::Path::new(&directory);
            std::fs::write(directory.join("producer-stress-before.pdf"), before).unwrap();
            std::fs::write(directory.join("producer-stress.pdf"), bytes).unwrap();
        }
    }

    #[test]
    fn literal_continuations_do_not_split_escapes_or_change_octals() {
        let value = [
            "(".repeat(250).as_bytes(),
            br"\177\123\(\)\\",
            &b"x".repeat(550),
            ")".repeat(250).as_bytes(),
        ]
        .concat();
        let mut out = Formatter::default();
        out.literal(&value).unwrap();
        assert!(
            out.bytes
                .split(|b| *b == b'\n')
                .all(|line| line.len() <= MAX_LINE)
        );
        let restored = out.bytes.split(|b| *b == b'\n').enumerate().fold(
            Vec::new(),
            |mut result, (index, line)| {
                if index > 0 {
                    result.pop();
                }
                result.extend_from_slice(line);
                result
            },
        );
        assert_eq!(restored, value);
    }

    #[test]
    fn large_arrays_and_hex_strings_wrap_without_changing_tokens() {
        let array = format!("[{}]", "123.456 ".repeat(2_000));
        let hex = format!("<{}>", "FEFFD83DDE00".repeat(400));
        for original in [array, hex] {
            let mut out = Formatter::default();
            out.object(original.as_bytes()).unwrap();
            assert!(
                out.bytes
                    .split(|b| *b == b'\n')
                    .all(|line| line.len() <= MAX_LINE)
            );
            let strip = |bytes: &[u8]| {
                bytes
                    .iter()
                    .copied()
                    .filter(|b| !white(*b))
                    .collect::<Vec<_>>()
            };
            assert_eq!(strip(&out.bytes), strip(original.as_bytes()));
        }
    }

    #[test]
    fn changed_producer_envelopes_are_refused() {
        assert!(finish(b"%PDF-1.6\nstartxref\n0\n%%EOF").is_err());
        let mut pdf = Pdf::new();
        pdf.stream(Ref::new(1), b"payload");
        let mut bytes = pdf.finish();
        let length = bytes
            .windows(b"/Length 7".len())
            .position(|part| part == b"/Length 7")
            .unwrap();
        bytes[length + 8] = b'6';
        assert!(finish(&bytes).is_err());
    }
}
