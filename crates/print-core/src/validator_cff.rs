//! Bounded structural facts from embedded CFF 1 font programs.

use std::collections::HashMap;

use read_fonts::{FontData, FontRead, FontRef, TableProvider, types::GlyphId};
use read_fonts::{
    ps::{
        cff::{self, CffFontRef},
        cs::CommandSink,
    },
    tables::cff::Cff,
    types::Fixed,
};

pub(crate) const MAX_CFF_GLYPHS: u32 = 65_536;
const MAX_CFF_GLYPH_NAME_BYTES: usize = 127;

#[derive(Debug)]
pub(crate) struct CffProgramProof {
    pub(crate) charset: CffCharsetProof,
    pub(crate) glyph_count: u32,
}

#[derive(Debug)]
pub(crate) enum CffCharsetProof {
    Cid(Vec<u16>),
    Named(HashMap<String, u32>),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CffInspectionError {
    reason: &'static str,
    glyph_count: u32,
    budget_exceeded: bool,
}

impl CffInspectionError {
    const fn malformed(reason: &'static str) -> Self {
        Self {
            reason,
            glyph_count: 0,
            budget_exceeded: false,
        }
    }

    const fn after_glyph_count(reason: &'static str, glyph_count: u32) -> Self {
        Self {
            reason,
            glyph_count,
            budget_exceeded: false,
        }
    }

    const fn budget_exceeded(glyph_count: u32) -> Self {
        Self {
            reason: "CFF aggregate glyph proof limit exceeded",
            glyph_count,
            budget_exceeded: true,
        }
    }

    pub(crate) const fn reason(self) -> &'static str {
        self.reason
    }

    pub(crate) const fn glyph_count(self) -> u32 {
        self.glyph_count
    }

    pub(crate) const fn exceeded_budget(self) -> bool {
        self.budget_exceeded
    }
}

pub(crate) fn inspect_cff_program(
    bytes: &[u8],
    glyph_budget: usize,
) -> Result<CffProgramProof, CffInspectionError> {
    let cff_bytes = if bytes.first() == Some(&1) {
        bytes
    } else {
        FontRef::new(bytes)
            .and_then(|font| font.cff())
            .map_err(|_| {
                CffInspectionError::malformed("OpenType CFF table is missing or malformed")
            })?
            .offset_data()
            .as_bytes()
    };
    let cff = Cff::read(FontData::new(cff_bytes))
        .map_err(|_| CffInspectionError::malformed("CFF header or indexes are malformed"))?;
    // The reader tolerates hdrSize below its four fixed fields. Reject that
    // impossible declaration before accepting either raw or SFNT-contained CFF.
    if cff.header().hdr_size() < 4 {
        return Err(CffInspectionError::malformed(
            "CFF header or indexes are malformed",
        ));
    }
    inspect_cff(&cff, cff_bytes, glyph_budget)
}

fn inspect_cff(
    cff: &Cff<'_>,
    cff_bytes: &[u8],
    glyph_budget: usize,
) -> Result<CffProgramProof, CffInspectionError> {
    if cff.names().count() != 1 || cff.top_dicts().count() != 1 {
        return Err(CffInspectionError::malformed(
            "embedded CFF must contain exactly one font",
        ));
    }
    let top_dict = cff
        .top_dicts()
        .get(0)
        .map_err(|_| CffInspectionError::malformed("CFF Top DICT is unreadable"))?;
    let mut charset_offset = None;
    let mut charstrings_offset = None;
    let mut is_cid = false;
    for entry in cff::dict::entries(top_dict, None) {
        match entry.map_err(|_| CffInspectionError::malformed("CFF Top DICT is malformed"))? {
            cff::dict::Entry::Charset(offset) => charset_offset = Some(offset),
            cff::dict::Entry::CharstringsOffset(offset) => charstrings_offset = Some(offset),
            cff::dict::Entry::Ros { .. } => is_cid = true,
            _ => {}
        }
    }

    let charstrings_offset = charstrings_offset
        .ok_or_else(|| CffInspectionError::malformed("CFF has no CharStrings INDEX"))?;
    let charstrings_data = cff
        .offset_data()
        .split_off(charstrings_offset)
        .ok_or_else(|| CffInspectionError::malformed("CFF CharStrings offset is out of bounds"))?;
    let charstrings = cff::v1::Index::read(charstrings_data)
        .map_err(|_| CffInspectionError::malformed("CFF CharStrings INDEX is malformed"))?;
    let glyph_count = u32::from(charstrings.count());
    if glyph_count == 0 || glyph_count > MAX_CFF_GLYPHS {
        return Err(CffInspectionError::malformed(
            "CFF glyph count is empty or exceeds the independent proof limit",
        ));
    }
    if glyph_count as usize > glyph_budget {
        return Err(CffInspectionError::budget_exceeded(glyph_count));
    }
    let malformed = |reason| CffInspectionError::after_glyph_count(reason, glyph_count);
    let font = CffFontRef::new_cff(cff_bytes, 0, None)
        .map_err(|_| malformed("CFF private dictionaries or subroutines are malformed"))?;
    if font.num_glyphs() != glyph_count {
        return Err(malformed(
            "CFF CharStrings indexes disagree on the glyph count",
        ));
    }
    for glyph_id in 0..glyph_count {
        let glyph_id = GlyphId::new(glyph_id);
        if charstrings
            .get(glyph_id.to_u32() as usize)
            .map_err(|_| malformed("CFF CharStrings entry is unreadable"))?
            .is_empty()
        {
            return Err(malformed("CFF contains an empty CharStrings entry"));
        }
        let subfont_index = font
            .subfont_index(glyph_id)
            .ok_or_else(|| malformed("CFF FDSelect does not cover every glyph"))?;
        let subfont = font
            .subfont(subfont_index, &[])
            .map_err(|_| malformed("CFF private dictionary or local subroutines are malformed"))?;
        font.evaluate_charstring(&subfont, glyph_id, &[], &mut NullCommandSink)
            .map_err(|_| malformed("CFF contains a malformed Type 2 CharString program"))?;
    }

    let charset = inspect_charset(cff, charset_offset, glyph_count, is_cid)?;

    Ok(CffProgramProof {
        charset,
        glyph_count,
    })
}

fn inspect_charset(
    cff: &Cff<'_>,
    charset_offset: Option<usize>,
    glyph_count: u32,
    is_cid: bool,
) -> Result<CffCharsetProof, CffInspectionError> {
    let malformed = |reason| CffInspectionError::after_glyph_count(reason, glyph_count);
    if is_cid {
        let charset_offset = charset_offset
            .filter(|offset| *offset > 2)
            .ok_or_else(|| malformed("CID-keyed CFF has no explicit charset"))?;
        let charset = cff::charset::Charset::new(cff.offset_data(), charset_offset, glyph_count)
            .map_err(|_| malformed("CID-keyed CFF charset is malformed"))?;
        let mut cids = Vec::with_capacity(glyph_count as usize);
        for glyph_id in 0..glyph_count {
            let cid = charset
                .string_id(GlyphId::new(glyph_id))
                .map_err(|_| malformed("CID-keyed CFF charset does not cover every glyph"))?
                .to_u16();
            cids.push(cid);
        }
        cids.sort_unstable();
        if cids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(malformed(
                "CID-keyed CFF charset maps more than one glyph to the same CID",
            ));
        }
        Ok(CffCharsetProof::Cid(cids))
    } else {
        let charset =
            cff::charset::Charset::new(cff.offset_data(), charset_offset.unwrap_or(0), glyph_count)
                .map_err(|_| malformed("simple CFF charset is malformed"))?;
        let mut names = HashMap::with_capacity(glyph_count.saturating_sub(1) as usize);
        for glyph_id in 1..glyph_count {
            let sid = charset
                .string_id(GlyphId::new(glyph_id))
                .map_err(|_| malformed("simple CFF charset does not cover every glyph"))?;
            let name = cff
                .string(sid)
                .ok_or_else(|| malformed("simple CFF charset references an absent glyph name"))?;
            if name.is_empty() || name.len() > MAX_CFF_GLYPH_NAME_BYTES || !name.is_ascii() {
                return Err(malformed(
                    "simple CFF charset contains an invalid glyph name",
                ));
            }
            let name = std::str::from_utf8(name)
                .expect("ASCII glyph names are valid UTF-8")
                .to_owned();
            if names.insert(name, glyph_id).is_some() {
                return Err(malformed(
                    "simple CFF charset maps more than one glyph to the same name",
                ));
            }
        }
        Ok(CffCharsetProof::Named(names))
    }
}

struct NullCommandSink;

impl CommandSink for NullCommandSink {
    fn move_to(&mut self, _x: Fixed, _y: Fixed) {}

    fn line_to(&mut self, _x: Fixed, _y: Fixed) {}

    fn curve_to(
        &mut self,
        _cx0: Fixed,
        _cy0: Fixed,
        _cx1: Fixed,
        _cy1: Fixed,
        _x: Fixed,
        _y: Fixed,
    ) {
    }

    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::inspect_cff_program;

    const CFF_OPEN_TYPE_FIXTURE: &[u8] =
        include_bytes!("../../layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf");

    #[test]
    fn aggregate_budget_is_checked_before_any_cff_glyph_evaluation() {
        let error = inspect_cff_program(CFF_OPEN_TYPE_FIXTURE, 0)
            .expect_err("a zero aggregate budget must reject a nonempty CFF program");

        assert!(error.exceeded_budget());
        assert!(error.glyph_count() > 0);
    }

    #[test]
    fn impossible_cff_header_size_is_rejected_before_glyph_evaluation() {
        const CFF_OFFSET: usize = 239_496;
        assert_eq!(
            &CFF_OPEN_TYPE_FIXTURE[CFF_OFFSET..CFF_OFFSET + 3],
            b"\x01\0\x04"
        );
        let mut malformed = CFF_OPEN_TYPE_FIXTURE.to_vec();
        malformed[CFF_OFFSET + 2] = 0;
        // Exercise both reader entry paths with the same impossible declaration.
        for bytes in [&malformed[..], &malformed[CFF_OFFSET..]] {
            let error = inspect_cff_program(bytes, 0)
                .expect_err("a CFF header cannot be smaller than its four fixed fields");
            assert_eq!(error.reason(), "CFF header or indexes are malformed");
            assert!(!error.exceeded_budget());
            assert_eq!(error.glyph_count(), 0);
        }
    }
}
