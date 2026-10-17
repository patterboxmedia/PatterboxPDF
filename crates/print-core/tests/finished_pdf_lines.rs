use print_core::finished::{
    FinishedDocument, FinishedOutputMode, FinishedPage, FinishedPageBoxes, FinishedPdfOptions,
    FinishedResources, PdfBox,
};
use print_core::{FinishedPdfWriter, PdfCandidateMetadata};

fn write_title(title: &str) -> Vec<u8> {
    let bounds = PdfBox::new(0.0, 0.0, 200.0, 300.0);
    let document = FinishedDocument {
        pages: vec![FinishedPage {
            boxes: FinishedPageBoxes {
                media: bounds,
                bleed: bounds,
                trim: bounds,
            },
            items: vec![],
        }],
        resources: FinishedResources::default(),
    };
    FinishedPdfWriter::write(
        &document,
        &FinishedPdfOptions {
            metadata: PdfCandidateMetadata::at_utc(title, "2026-10-04T00:00:00Z").unwrap(),
            mode: FinishedOutputMode::Ordinary,
        },
    )
    .unwrap()
    .into_bytes()
}

fn inspect_info_and_xref(bytes: &[u8]) {
    let tail_start = bytes
        .windows(b"\nstartxref\n".len())
        .rposition(|part| part == b"\nstartxref\n")
        .unwrap();
    let tail = std::str::from_utf8(&bytes[tail_start..]).unwrap();
    let xref_offset: usize = tail.lines().nth(2).unwrap().parse().unwrap();
    let xref = std::str::from_utf8(&bytes[xref_offset..]).unwrap();
    assert!(xref.starts_with("xref\n0 "));
    let count: usize = xref.lines().nth(1).unwrap()[2..].parse().unwrap();
    let mut offsets = vec![];
    for (id, entry) in xref.lines().skip(2).take(count).enumerate() {
        if entry.ends_with(" n") {
            let offset: usize = entry[..10].parse().unwrap();
            assert!(bytes[offset..].starts_with(format!("{id} 0 obj\n").as_bytes()));
            offsets.push(offset);
        }
    }
    offsets.sort_unstable();
    offsets.push(xref_offset);
    let info_id: usize = xref
        .split("/Info ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let info_header = format!("{info_id} 0 obj\n");
    let object = offsets
        .windows(2)
        .map(|pair| &bytes[pair[0]..pair[1]])
        .find(|object| object.starts_with(info_header.as_bytes()))
        .unwrap();
    assert!(
        object
            .split(|byte| matches!(byte, b'\n' | b'\r'))
            .all(|line| line.len() <= 255)
    );
}

#[test]
fn long_ascii_unicode_and_escaped_titles_keep_bounded_info_lines_and_valid_offsets() {
    for (name, title) in [
        ("ascii", "A useful long title ".repeat(180)),
        ("unicode", "Print 😀 café 文 ".repeat(180)),
        (
            "escaped",
            r"Title (balanced) ) unbalanced \\ stream endobj xref ".repeat(90),
        ),
        ("large-ascii", "a".repeat(32_768)),
        ("large-unicode", "試".repeat(16_383)),
    ] {
        let bytes = write_title(&title);
        inspect_info_and_xref(&bytes);
        assert_eq!(bytes, write_title(&title));
        if let Some(directory) = std::env::var_os("PDF_LIBRARY_LINE_ARTIFACTS") {
            std::fs::write(
                std::path::Path::new(&directory).join(format!("{name}.pdf")),
                bytes,
            )
            .unwrap();
        }
    }
}
