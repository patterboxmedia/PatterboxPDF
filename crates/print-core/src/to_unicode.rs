//! Two-byte CID to Unicode extraction maps (PDF 1.6 section 5.9.2).
use std::collections::BTreeMap;
use std::fmt::Write;

const MAX_DESTINATION_BYTES: usize = 512;

pub(crate) fn destination_within_limit(codepoints: impl IntoIterator<Item = char>) -> bool {
    codepoints
        .into_iter()
        .try_fold(0, |bytes, c| {
            let next = bytes + c.len_utf16() * 2;
            (next <= MAX_DESTINATION_BYTES).then_some(next)
        })
        .is_some()
}

/// Emit a Type 2 `ToUnicode` resource directly. The pinned pdf-writer helper
/// hardcodes Type 0; Encoding `CMaps` such as Identity-H are a separate resource.
pub(crate) fn cmap(mappings: &BTreeMap<u16, Vec<char>>) -> Result<Vec<u8>, ()> {
    if mappings
        .values()
        .any(|s| s.is_empty() || !destination_within_limit(s.iter().copied()))
    {
        return Err(());
    }
    let mut result = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /PublisherV2ToUnicode def\n/CMapType 2 def\n\
         1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let pairs: Vec<_> = mappings.iter().collect();
    // CMap mapping blocks contain at most 100 entries. UTF-16BE destinations
    // have no byte-order mark; supplementary characters use two code units.
    for block in pairs.chunks(100) {
        writeln!(&mut result, "{} beginbfchar", block.len()).expect("String write");
        for (cid, text) in block {
            write!(&mut result, "<{cid:04X}> <").expect("String write");
            for c in *text {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    write!(&mut result, "{unit:04X}").expect("String write");
                }
            }
            result.push_str(">\n");
        }
        result.push_str("endbfchar\n");
    }
    result.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    Ok(result.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_map_blocks_are_bounded_and_invalid_destinations_fail() {
        let mut mappings: BTreeMap<_, _> = (1..=101).map(|cid| (cid, vec!['😀'])).collect();
        let bytes = cmap(&mappings).unwrap();
        let body = String::from_utf8(bytes).unwrap();
        assert_eq!(body.matches(" beginbfchar\n").count(), 2);
        assert!(body.contains("100 beginbfchar\n"));
        assert!(body.contains("1 beginbfchar\n<0065> <D83DDE00>\nendbfchar"));
        mappings.insert(1, vec!['a'; 257]);
        assert!(cmap(&mappings).is_err());
        mappings.insert(1, Vec::new());
        assert!(cmap(&mappings).is_err());
    }
}
