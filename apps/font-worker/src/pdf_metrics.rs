use super::{
    cff_program,
    outline::{Matrix, Outline},
    truetype_program,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use ttf_parser::Face;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Serialize)]
pub struct PdfMetrics {
    pub bbox: [f32; 4],
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: Option<f32>,
    pub italic_angle: f32,
    pub stem_v: f32,
    pub widths: Vec<f32>,
    #[serde(skip)]
    pub requires_ascent_facts: bool,
    #[serde(skip)]
    pub requires_cap_height_fact: bool,
}
enum Program<'a> {
    Cff(Box<cff_program::Program<'a>>),
    TrueType(truetype_program::Program<'a>),
}
impl<'a> Program<'a> {
    fn parse(face: &Face<'a>, final_program: bool) -> Result<Self, cff_program::Error> {
        if face.tables().glyf.is_some() {
            Ok(Self::TrueType(truetype_program::Program::parse(
                face,
                final_program,
            )?))
        } else {
            let bytes = face
                .raw_face()
                .table(ttf_parser::Tag::from_bytes(b"CFF "))
                .ok_or(())?;
            Ok(Self::Cff(Box::new(cff_program::Program::parse(bytes)?)))
        }
    }
    fn matrix(&self, gid: u16, face: &Face<'_>) -> Result<Matrix, cff_program::Error> {
        match self {
            Self::Cff(p) => p.matrix(usize::from(gid)),
            Self::TrueType(_) => {
                let s = 1000. / f64::from(face.units_per_em());
                Ok([s, 0., 0., s, 0., 0.])
            }
        }
    }
    fn outline(&self, face: &Face<'_>, gid: u16) -> Result<Outline, cff_program::Error> {
        Ok(self.outline_with_width(face, gid)?.0)
    }
    fn outline_with_width(
        &self,
        face: &Face<'_>,
        gid: u16,
    ) -> Result<(Outline, f64), cff_program::Error> {
        let m = self.matrix(gid, face)?;
        let (o, width) = match self {
            Self::TrueType(p) => {
                let s = super::enclosure::Interval::quotient(1000., f64::from(face.units_per_em()));
                let z = super::enclosure::Interval::exact(0.);
                (
                    p.outline(usize::from(gid), m, [s, z, z, s, z, z])?,
                    f64::from(p.width(usize::from(gid))?),
                )
            }
            Self::Cff(p) => p.outline_with_width(usize::from(gid))?,
        };
        if o.invalid {
            return Err(().into());
        }
        Ok((o, width * m[0]))
    }
    fn width(&self, gid: u16, face: &Face<'_>) -> Result<f64, cff_program::Error> {
        let width = match self {
            Self::Cff(p) => p.width(usize::from(gid))?,
            Self::TrueType(p) => f64::from(p.width(usize::from(gid))?),
        };
        // An advance is a displacement, never a point: translation is absent.
        Ok(width * self.matrix(gid, face)?[0])
    }
}

fn subset_source_glyphs(
    program: &Program<'_>,
    source_program: &Program<'_>,
    remapped: &[super::RemappedGlyph],
    retained_glyphs: u16,
) -> Result<BTreeMap<u16, u16>, cff_program::Error> {
    if let (Program::TrueType(output), Program::TrueType(input)) = (program, source_program) {
        return output.subset_source_glyphs(input, remapped);
    }
    let originals = remapped
        .iter()
        .map(|m| (m.cid, m.source_glyph_id))
        .collect::<BTreeMap<_, _>>();
    if originals.len() != usize::from(retained_glyphs) {
        return Err(().into());
    }
    Ok(originals)
}

pub fn prepare(
    source: &Face<'_>,
    prepared: &Face<'_>,
    remapped: &[super::RemappedGlyph],
    full: bool,
    authored_accented: Option<&[u16]>,
) -> Result<PdfMetrics, cff_program::Error> {
    let program = Program::parse(prepared, true)?;
    let source_program = Program::parse(source, false)?;
    for mapping in remapped {
        source_program.width(mapping.source_glyph_id, source)?;
    }
    let accented = accented_glyphs(source);
    let roles = automatic_ascent_roles(source);
    let originals = if full {
        BTreeMap::new()
    } else {
        subset_source_glyphs(
            &program,
            &source_program,
            remapped,
            prepared.number_of_glyphs(),
        )?
    };
    let mut bbox = None;
    let mut certified_bbox = None;
    let mut ascent = 0f64;
    let mut ascent_proof = AscentProof::default();
    let mut fallback_stem = None;
    let mut widths = Vec::new();
    for gid in 0..prepared.number_of_glyphs() {
        let (outline, width) = program.outline_with_width(prepared, gid)?;
        if let Some(b) = outline.certified_bounds {
            certified_bbox = Some(union(certified_bbox, b));
        }
        if let Some(b) = outline.bounds {
            bbox = Some(union(bbox, b));
            let original = if full {
                gid
            } else {
                *originals.get(&gid).ok_or(())?
            };
            let excluded = authored_accented.map_or_else(
                || accented.contains(&original),
                |facts| facts.binary_search(&original).is_ok(),
            );
            if !excluded {
                ascent = ascent.max(b[3]);
            }
            if authored_accented.is_none() {
                ascent_proof.observe(
                    roles.get(&original).copied().unwrap_or(AscentRole::Unknown),
                    &outline,
                );
            }
        }
        if fallback_stem.is_none() {
            fallback_stem = outline.estimate_vertical_stem_width();
        }
        widths.push(scalar(width)?);
    }
    let complete_source_empty = bbox.is_none()
        && (full
            || originals.values().copied().collect::<BTreeSet<_>>().len()
                == usize::from(source.number_of_glyphs()));
    let bbox = bbox.unwrap_or([0.; 4]);
    let certified_bbox = certified_bbox.unwrap_or([0.; 4]);
    let (cap, stem, angle_gid) = design_metrics(&source_program, source, complete_source_empty)?;
    Ok(PdfMetrics {
        bbox: [
            scalar(certified_bbox[0].floor())?,
            scalar(certified_bbox[1].floor())?,
            scalar(certified_bbox[2].ceil())?,
            scalar(certified_bbox[3].ceil())?,
        ],
        ascent: scalar(ascent)?,
        descent: scalar(bbox[1].min(0.))?,
        cap_height: cap.map(scalar).transpose()?,
        italic_angle: scalar(effective_italic_angle(&source_program, source, angle_gid)?)?,
        stem_v: scalar(stem.or(fallback_stem).unwrap_or(0.))?,
        widths,
        requires_ascent_facts: authored_accented.is_none() && ascent_proof.requires_facts(),
        requires_cap_height_fact: cap.is_none(),
    })
}
fn design_metrics(
    program: &Program<'_>,
    source: &Face<'_>,
    complete_source_empty: bool,
) -> Result<(Option<f64>, Option<f64>, u16), cff_program::Error> {
    // OS/2 is a font-wide FUnits declaration, already outside local CFF FD
    // coordinates. Convert by UPEM; Top/FD matrices apply to outline operands.
    // The designer metric need not equal a measured H outline top.
    let flat_references = established_flat_references(source);
    let mut cap = source
        .capital_height()
        .filter(|h| *h > 0)
        .map(|h| f64::from(h) * 1000. / f64::from(source.units_per_em()));
    let mut stem = None;
    let mut angle_gid = None;
    let mut empty_references = BTreeSet::new();
    for letter in ['H', 'I', 'T', 'E', 'F', 'L'] {
        if let Some(gid) = source
            .glyph_index(letter)
            .filter(|gid| flat_references.contains(&gid.0))
        {
            let outline = match program.outline(source, gid.0) {
                Ok(outline) => outline,
                Err(
                    cff_program::Error::UnsupportedVerticalPhantomContext
                    | cff_program::Error::UnresolvedHorizontalMetrics
                    | cff_program::Error::UnsupportedVerticalMetricsVersion,
                ) => continue,
                Err(error) => return Err(error),
            };
            let m = program.matrix(gid.0, source)?;
            if let Some(b) = outline.bounds {
                if cap.is_none() {
                    cap = Some(b[3] - m[5]);
                }
                angle_gid.get_or_insert(gid.0);
                if stem.is_none() {
                    stem = outline.estimate_vertical_stem_width();
                }
            } else if gid.0 != 0 {
                empty_references.insert(gid.0);
            }
        }
    }
    if cap.is_none()
        && (complete_source_empty
            || (source.number_of_glyphs() > 1
                && (1..source.number_of_glyphs()).all(|gid| empty_references.contains(&gid))))
    {
        cap = Some(0.);
    }
    Ok((cap, stem, angle_gid.unwrap_or(0)))
}
fn established_flat_references(source: &Face<'_>) -> BTreeSet<u16> {
    let mut roles = BTreeMap::new();
    let Some(cmap) = source.tables().cmap else {
        return BTreeSet::new();
    };
    for i in 0..cmap.subtables.len() {
        let Some(table) = cmap.subtables.get(i) else {
            return BTreeSet::new();
        };
        if !table.is_unicode() {
            continue;
        }
        if matches!(
            table.format,
            ttf_parser::cmap::Format::MixedCoverage
                | ttf_parser::cmap::Format::UnicodeVariationSequences(_)
        ) {
            return BTreeSet::new();
        }
        table.codepoints(|cp| {
            if let Some(gid) = table.glyph_index(cp) {
                let flat = matches!(cp, 0x48 | 0x49 | 0x54 | 0x45 | 0x46 | 0x4c);
                roles
                    .entry(gid.0)
                    .and_modify(|old| *old &= flat)
                    .or_insert(flat);
            }
        });
    }
    roles
        .into_iter()
        .filter_map(|(gid, flat)| (flat && gid != 0).then_some(gid))
        .collect()
}
fn effective_italic_angle(
    program: &Program<'_>,
    source: &Face<'_>,
    gid: u16,
) -> Result<f64, cff_program::Error> {
    let angle = match program {
        Program::Cff(p) => p.italic_angle(usize::from(gid))?,
        Program::TrueType(_) => f64::from(source.italic_angle()),
    };
    let m = program.matrix(gid, source)?;
    let v = [(-angle).to_radians().tan(), 1.];
    Ok(-(m[0] * v[0] + m[2] * v[1])
        .atan2(m[1] * v[0] + m[3] * v[1])
        .to_degrees())
}
#[allow(clippy::cast_possible_truncation)]
fn scalar(v: f64) -> Result<f32, ()> {
    if !v.is_finite() || v.abs() > 10_000_000. {
        return Err(());
    }
    Ok(v as f32)
}
fn union(old: Option<[f64; 4]>, b: [f64; 4]) -> [f64; 4] {
    old.map_or(b, |a| {
        [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ]
    })
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AscentRole {
    Included,
    Excluded,
    Unknown,
}
#[derive(Default)]
struct AscentProof {
    included_attained_lower: f64,
    unknown_upper: f64,
}
impl AscentProof {
    fn observe(&mut self, role: AscentRole, outline: &Outline) {
        if let Some(bounds) = outline.certified_bounds {
            match role {
                AscentRole::Included => {
                    self.included_attained_lower = self
                        .included_attained_lower
                        .max(outline.attained_height_lower_bound);
                }
                AscentRole::Unknown => self.unknown_upper = self.unknown_upper.max(bounds[3]),
                AscentRole::Excluded => {}
            }
        }
    }
    fn requires_facts(&self) -> bool {
        self.unknown_upper > self.included_attained_lower
    }
}
fn unicode_ascent_role(cp: u32) -> AscentRole {
    if matches!(cp,0x20|0x30..=0x39|0x41..=0x5a|0x61..=0x7a) {
        return AscentRole::Included;
    }
    if (0xc0..=0x17f).contains(&cp)
        && let Some(ch) = char::from_u32(cp)
    {
        let mut decomposition = ch.nfd();
        if decomposition
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        {
            let marks = decomposition.collect::<Vec<_>>();
            if !marks.is_empty() && marks.iter().all(|c|matches!(u32::from(*c),0x300..=0x304|0x306..=0x308|0x30a..=0x30c|0x327|0x328)) {return AscentRole::Excluded;}
        }
    }
    AscentRole::Unknown
}
fn automatic_ascent_roles(source: &Face<'_>) -> BTreeMap<u16, AscentRole> {
    let mut roles = BTreeMap::new();
    let mut unresolved = false;
    if let Some(cmap) = source.tables().cmap {
        // Deliberately visit every record: the library iterator stops at an
        // undecodable record and could otherwise hide a conflicting context.
        for i in 0..cmap.subtables.len() {
            let Some(table) = cmap.subtables.get(i) else {
                unresolved = true;
                continue;
            };
            if !table.is_unicode() {
                continue;
            }
            if matches!(
                table.format,
                ttf_parser::cmap::Format::MixedCoverage
                    | ttf_parser::cmap::Format::UnicodeVariationSequences(_)
            ) {
                unresolved = true;
                continue;
            }
            table.codepoints(|cp| {
                if let Some(gid) = table.glyph_index(cp) {
                    let role = unicode_ascent_role(cp);
                    roles
                        .entry(gid.0)
                        .and_modify(|old| {
                            if *old != role {
                                *old = AscentRole::Unknown;
                            }
                        })
                        .or_insert(role);
                }
            });
        }
    }
    if unresolved {
        roles.clear();
    }
    roles.insert(0, AscentRole::Included);
    roles
}
fn accented_glyphs(source: &Face<'_>) -> BTreeSet<u16> {
    let Some(cmap) = source.tables().cmap else {
        return BTreeSet::new();
    };
    let mut accented = BTreeSet::new();
    let mut plain = BTreeSet::new();
    for table in cmap
        .subtables
        .into_iter()
        .filter(ttf_parser::cmap::Subtable::is_unicode)
    {
        table.codepoints(|cp| {
            let (Some(ch), Some(gid)) = (char::from_u32(cp), table.glyph_index(cp)) else {
                return;
            };
            if ch
                .nfd()
                .any(|c| unicode_normalization::char::canonical_combining_class(c) != 0)
            {
                accented.insert(gid.0);
            } else {
                plain.insert(gid.0);
            }
        });
    }
    accented.retain(|gid| !plain.contains(gid));
    accented
}
/// Compatibility integer accessor. Precise emitted cap height is measured by
/// `prepare`, in its own effective coordinate system.
pub fn capital_height(source: &Face<'_>) -> Option<i16> {
    source.capital_height().filter(|h| *h > 0).or_else(|| {
        ['H', 'I', 'T', 'E', 'F'].into_iter().find_map(|ch| {
            source
                .glyph_index(ch)
                .and_then(|g| source.glyph_bounding_box(g))
                .map(|b| b.y_max)
                .filter(|h| *h > 0)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::super::{enclosure::Interval, outline::IDENTITY};
    use super::*;
    fn line(height: Interval) -> Outline {
        let mut o = Outline::new(IDENTITY);
        o.mov([0., 0.]);
        o.line_enclosed([1., 880.], [Interval::exact(1.), height]);
        o
    }
    #[test]
    fn certified_attained_dominance_allows_equality_but_not_uncertain_overlap() {
        let included = line(Interval::exact(880.));
        let equal = line(Interval::exact(880.));
        let uncertain = line(Interval {
            lo: 880.,
            hi: 880_f64.next_up(),
        });
        let mut proof = AscentProof::default();
        proof.observe(AscentRole::Included, &included);
        proof.observe(AscentRole::Unknown, &equal);
        assert!(!proof.requires_facts());
        proof.observe(AscentRole::Unknown, &uncertain);
        assert!(proof.requires_facts());
    }
    #[test]
    fn a_control_point_is_not_an_attained_known_maximum() {
        let mut included = Outline::new(IDENTITY);
        included.mov([0., 0.]);
        included.quad([1., 1000.], [2., 0.]);
        assert_eq!(
            included.attained_height_lower_bound.to_bits(),
            0_f64.to_bits()
        );
        let mut unknown = Outline::new(IDENTITY);
        unknown.mov([0., 0.]);
        unknown.line([1., 400.]);
        let mut proof = AscentProof::default();
        proof.observe(AscentRole::Included, &included);
        proof.observe(AscentRole::Unknown, &unknown);
        assert!(
            proof.requires_facts(),
            "actual included maximum500 is not certified attained by endpoint evidence; request facts conservatively"
        );
    }
    #[test]
    fn bounded_unicode_roles_do_not_generalize_non_nfd_or_combining_labels() {
        for cp in [0x20, 0x39, 0x41, 0x7a] {
            assert_eq!(unicode_ascent_role(cp), AscentRole::Included);
        }
        for cp in [0xc0, 0x124, 0x17d] {
            assert_eq!(unicode_ascent_role(cp), AscentRole::Excluded);
        }
        for cp in [0x302, 0xb4, 0x3a9, 0xe000, 0x0010_ffff, 0x0011_0000] {
            assert_eq!(unicode_ascent_role(cp), AscentRole::Unknown);
        }
    }
}
