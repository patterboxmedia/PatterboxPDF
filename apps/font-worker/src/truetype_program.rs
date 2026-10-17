#![allow(clippy::many_single_char_names)]
use super::cff_program::Error;
use super::enclosure::{self, Interval};
use super::outline::{IDENTITY, Matrix, Outline, Point, transform};
use super::subset_metrics::checksum;
use std::collections::{BTreeMap, BTreeSet};
use ttf_parser::{Face, Tag};
type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Copy)]
struct Vertex {
    p: Point,
    enclosed: enclosure::Point,
    on: bool,
}
#[derive(Clone, Copy)]
struct Anchor {
    vertex: Vertex,
    known: [bool; 2],
    version_unknown: [bool; 2],
}
impl Anchor {
    fn actual(vertex: Vertex) -> Self {
        Self {
            vertex,
            known: [true; 2],
            version_unknown: [false; 2],
        }
    }
    fn transformed(self, m: Matrix) -> Self {
        Self {
            vertex: Vertex {
                p: transform(m, self.vertex.p),
                enclosed: enclosure::transform(m.map(Interval::exact), self.vertex.enclosed),
                on: false,
            },
            known: [
                (m[0] == 0. || self.known[0]) && (m[2] == 0. || self.known[1]),
                (m[1] == 0. || self.known[0]) && (m[3] == 0. || self.known[1]),
            ],
            version_unknown: [
                (m[0] != 0. && self.version_unknown[0]) || (m[2] != 0. && self.version_unknown[1]),
                (m[1] != 0. && self.version_unknown[0]) || (m[3] != 0. && self.version_unknown[1]),
            ],
        }
    }
}
struct DecodedGlyph {
    contours: Vec<Vec<Vertex>>,
    phantoms: [Anchor; 4],
    advance: u16,
}
impl DecodedGlyph {
    fn anchor(&self, index: usize) -> Result<Anchor> {
        let count = self.contours.iter().map(Vec::len).sum::<usize>();
        anchor_at(&self.contours, self.phantoms, count, index)
    }
}
pub struct Program<'a> {
    glyf: &'a [u8],
    locations: Vec<usize>,
    pub widths: Vec<u16>,
    raw_widths: Vec<u16>,
    bearings: Vec<i16>,
    vertical: VerticalMetrics,
    source_deferred: Vec<bool>,
}
struct VerticalMetrics {
    values: Option<Vec<(u16, i16)>>,
    unsupported_version: bool,
}
fn u16_at(b: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(
        b.get(at..at + 2).ok_or(())?.try_into().map_err(|_| ())?,
    ))
}
fn i16_at(b: &[u8], at: usize) -> Result<i16> {
    Ok(i16::from_be_bytes(
        b.get(at..at + 2).ok_or(())?.try_into().map_err(|_| ())?,
    ))
}
fn u32_at(b: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        b.get(at..at + 4).ok_or(())?.try_into().map_err(|_| ())?,
    ))
}
pub struct Component<'a> {
    pub glyph: usize,
    pub flags: u16,
    pub arguments: &'a [u8],
}
impl<'a> Program<'a> {
    #[allow(clippy::too_many_lines)]
    pub fn parse(face: &Face<'a>, checksums: bool) -> Result<Self> {
        let raw = face.raw_face();
        let table = |tag: &[u8; 4]| raw.table(Tag::from_bytes(tag)).ok_or(());
        let head = table(b"head")?;
        let hhea = table(b"hhea")?;
        let maxp = table(b"maxp")?;
        let hmtx = table(b"hmtx")?;
        let loca = table(b"loca")?;
        let glyf = table(b"glyf")?;
        if head.len() != 54
            || hhea.len() != 36
            || maxp.len() != 32
            || u32_at(head, 0)? != 0x10000
            || u32_at(head, 12)? != 0x5f0f_3cf5
            || u32_at(hhea, 0)? != 0x10000
            || u32_at(maxp, 0)? != 0x10000
            || i16_at(head, 52)? != 0
            || i16_at(hhea, 32)? != 0
        {
            return Err(().into());
        }
        if !(64..=16384).contains(&u16_at(head, 18)?)
            || u16_at(head, 16)? & 0x40 != 0
            || hhea[24..32].iter().any(|v| *v != 0)
            || (i16_at(hhea, 18)? == 0 && i16_at(hhea, 20)? == 0)
        {
            return Err(().into());
        }
        let n = usize::from(u16_at(maxp, 4)?);
        let metrics = usize::from(u16_at(hhea, 34)?);
        if n == 0 || metrics == 0 || metrics > n || hmtx.len() != metrics * 4 + (n - metrics) * 2 {
            return Err(().into());
        }
        let widths = (0..n)
            .map(|gid| u16_at(hmtx, gid.min(metrics - 1) * 4))
            .collect::<Result<Vec<_>>>()?;
        let bearings = (0..n)
            .map(|gid| {
                i16_at(
                    hmtx,
                    if gid < metrics {
                        gid * 4 + 2
                    } else {
                        metrics * 4 + (gid - metrics) * 2
                    },
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let long = match i16_at(head, 50)? {
            0 => false,
            1 => true,
            _ => return Err(().into()),
        };
        if loca.len() != (n + 1) * if long { 4 } else { 2 } {
            return Err(().into());
        }
        let locations = (0..=n)
            .map(|i| {
                if long {
                    usize::try_from(u32_at(loca, i * 4)?).map_err(|_| Error::Malformed)
                } else {
                    Ok(usize::from(u16_at(loca, i * 2)?) * 2)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        if locations[0] != 0
            || locations.windows(2).any(|w| w[0] > w[1])
            || locations[n] > glyf.len()
        {
            return Err(().into());
        }
        if checksums {
            if table(b"cvt ")?.len() % 2 != 0 {
                return Err(().into());
            }
            instructions(table(b"fpgm")?)?;
            instructions(table(b"prep")?)?;
            if !matches!(u16_at(maxp, 14)?, 1 | 2) {
                return Err(().into());
            }
            if let Some(name) = raw.table(Tag::from_bytes(b"name")) {
                name_table(name)?;
            }
            if let Some(post) = raw.table(Tag::from_bytes(b"post")) {
                post_table(post, n)?;
            }
        }
        if checksums {
            // A full font is emitted untouched: validate its exact directory and
            // checksum claims rather than silently repairing it.
            let bytes = raw.data;
            let count = usize::from(u16_at(bytes, 4)?);
            let mut regions = Vec::new();
            let mut tags = BTreeSet::new();
            let power = 1usize << count.checked_ilog2().ok_or(())?;
            if usize::from(u16_at(bytes, 6)?) != power * 16
                || usize::from(u16_at(bytes, 8)?) != power.ilog2() as usize
                || usize::from(u16_at(bytes, 10)?) != (count - power) * 16
            {
                return Err(().into());
            }
            for i in 0..count {
                let at = 12 + i * 16;
                let tag = bytes.get(at..at + 4).ok_or(())?;
                let off = usize::try_from(u32_at(bytes, at + 8)?).map_err(|_| ())?;
                let len = usize::try_from(u32_at(bytes, at + 12)?).map_err(|_| ())?;
                let end = off.checked_add(len).ok_or(())?;
                if off % 4 != 0 || off < 12 + count * 16 || end > bytes.len() || !tags.insert(tag) {
                    return Err(().into());
                }
                let value = if tag == b"head" {
                    let mut header = bytes[off..end].to_vec();
                    header.get_mut(8..12).ok_or(())?.fill(0);
                    checksum(&header)
                } else {
                    checksum(&bytes[off..end])
                };
                if value != u32_at(bytes, at + 4)? {
                    return Err(().into());
                }
                if len > 0 {
                    let padded = end.checked_add(3).ok_or(())? & !3;
                    if raw.data.get(end..padded).ok_or(())?.iter().any(|b| *b != 0) {
                        return Err(().into());
                    }
                    regions.push((off, padded));
                }
            }
            regions.sort_unstable();
            if regions.windows(2).any(|w| w[0].1 > w[1].0) || checksum(bytes) != 0xb1b0_afba {
                return Err(().into());
            }
        }
        let mut program = Self {
            glyf,
            locations,
            widths: widths.clone(),
            raw_widths: widths,
            bearings,
            vertical: vertical_metrics(face, n)?,
            source_deferred: vec![false; n],
        };
        let mut components = Vec::new();
        let mut boxes = Vec::new();
        let mut metric_extrema: Option<[i32; 3]> = None;
        let mut vertical_extrema: Option<[i32; 3]> = None;
        for gid in 0..n {
            let raw = &glyf[program.locations[gid]..program.locations[gid + 1]];
            let (refs, instructions) = requirements(raw)?;
            program.component_structure(gid)?;
            let decoded = match program.glyph(gid, &mut Vec::new()) {
                Ok(decoded) => decoded,
                Err(
                    Error::UnsupportedVerticalPhantomContext
                    | Error::UnresolvedHorizontalMetrics
                    | Error::UnsupportedVerticalMetricsVersion,
                ) if !checksums => {
                    program.source_deferred[gid] = true;
                    components.push(refs);
                    continue;
                }
                Err(error) => return Err(error),
            };
            program.widths[gid] = decoded.advance;
            let contours = decoded.contours;
            if checksums {
                let composite = !refs.is_empty();
                let points = contours.iter().map(Vec::len).sum::<usize>();
                let point_limit = usize::from(u16_at(maxp, if composite { 10 } else { 6 })?);
                let contour_limit = usize::from(u16_at(maxp, if composite { 12 } else { 8 })?);
                if points > point_limit
                    || contours.len() > contour_limit
                    || refs.len() > usize::from(u16_at(maxp, 28)?)
                    || instructions > usize::from(u16_at(maxp, 26)?)
                {
                    return Err(().into());
                }
                if points > 0 {
                    let stored = [
                        i16_at(raw, 2)?,
                        i16_at(raw, 4)?,
                        i16_at(raw, 6)?,
                        i16_at(raw, 8)?,
                    ];
                    let actual = [
                        contours
                            .iter()
                            .flatten()
                            .map(|v| v.p[0])
                            .fold(f64::INFINITY, f64::min),
                        contours
                            .iter()
                            .flatten()
                            .map(|v| v.p[1])
                            .fold(f64::INFINITY, f64::min),
                        contours
                            .iter()
                            .flatten()
                            .map(|v| v.p[0])
                            .fold(f64::NEG_INFINITY, f64::max),
                        contours
                            .iter()
                            .flatten()
                            .map(|v| v.p[1])
                            .fold(f64::NEG_INFINITY, f64::max),
                    ];
                    if actual
                        .into_iter()
                        .zip(stored)
                        .any(|(a, s)| f64::from(s) < a.floor() || f64::from(s) > a.ceil())
                    {
                        return Err(().into());
                    }
                    if u16_at(head, 16)? & 2 != 0 && stored[0] != program.bearings[gid] {
                        return Err(().into());
                    }
                    boxes.push(stored);
                    let left = i32::from(program.bearings[gid]);
                    let extent = left + i32::from(stored[2]) - i32::from(stored[0]);
                    let right = i32::from(program.raw_widths[gid]) - extent;
                    metric_extrema = Some(metric_extrema.map_or([left, right, extent], |old| {
                        [old[0].min(left), old[1].min(right), old[2].max(extent)]
                    }));
                    if let Some(vertical) = &program.vertical.values {
                        let (advance, top) = vertical[gid];
                        let top = i32::from(top);
                        let extent = top + i32::from(stored[3]) - i32::from(stored[1]);
                        let bottom = i32::from(advance) - extent;
                        vertical_extrema =
                            Some(vertical_extrema.map_or([top, bottom, extent], |old| {
                                [old[0].min(top), old[1].min(bottom), old[2].max(extent)]
                            }));
                    }
                }
            }
            components.push(refs);
        }
        if checksums {
            let expected = if boxes.is_empty() {
                [0; 4]
            } else {
                [
                    boxes.iter().map(|b| b[0]).min().ok_or(())?,
                    boxes.iter().map(|b| b[1]).min().ok_or(())?,
                    boxes.iter().map(|b| b[2]).max().ok_or(())?,
                    boxes.iter().map(|b| b[3]).max().ok_or(())?,
                ]
            };
            for (i, v) in expected.iter().enumerate() {
                if i16_at(head, 36 + i * 2)? != *v {
                    return Err(().into());
                }
            }
            if u16_at(hhea, 10)? != program.raw_widths.iter().copied().max().ok_or(())? {
                return Err(().into());
            }
            for (i, v) in metric_extrema.unwrap_or([0; 3]).iter().enumerate() {
                if i32::from(i16_at(hhea, 12 + i * 2)?) != *v {
                    return Err(().into());
                }
            }
            if let Some(vertical) = &program.vertical.values {
                let vhea = table(b"vhea")?;
                if u16_at(vhea, 10)? != vertical.iter().map(|m| m.0).max().ok_or(())? {
                    return Err(Error::Malformed);
                }
                for (i, value) in vertical_extrema.unwrap_or([0; 3]).iter().enumerate() {
                    if i32::from(i16_at(vhea, 12 + i * 2)?) != *value {
                        return Err(Error::Malformed);
                    }
                }
            }
            if let Some(post) = raw.table(Tag::from_bytes(b"post")) {
                if u32_at(post, 12)? != 0
                    && program
                        .raw_widths
                        .iter()
                        .copied()
                        .filter(|v| *v != 0)
                        .collect::<BTreeSet<_>>()
                        .len()
                        > 1
                {
                    return Err(().into());
                }
                if u32_at(post, 4)? == 0 && i16_at(hhea, 22)? != 0 {
                    return Err(().into());
                }
            }
            let declared = usize::from(u16_at(maxp, 30)?);
            if declared > 16 {
                return Err(().into());
            }
            let mut depths = vec![None; n];
            for gid in 0..n {
                if component_depth(gid, &components, &mut depths) > declared {
                    return Err(().into());
                }
            }
        }
        Ok(program)
    }
    pub fn width(&self, gid: usize) -> Result<u16> {
        if *self.source_deferred.get(gid).ok_or(())? {
            return self.glyph(gid, &mut Vec::new()).map(|glyph| glyph.advance);
        }
        self.widths.get(gid).copied().ok_or(Error::Malformed)
    }
    pub fn cached_bounds(&self, gid: usize) -> Result<Option<[i16; 4]>> {
        // Aggregate table caches use stored control-point bounds, not a second
        // outline interpreter. Empty composites contribute no rectangle.
        if self.point_count(gid, &mut Vec::new())? == 0 {
            return Ok(None);
        }
        let raw = self
            .glyf
            .get(self.locations[gid]..self.locations[gid + 1])
            .ok_or(())?;
        Ok(Some([
            i16_at(raw, 2)?,
            i16_at(raw, 4)?,
            i16_at(raw, 6)?,
            i16_at(raw, 8)?,
        ]))
    }
    pub fn vertical_metrics(&self) -> Option<&[(u16, i16)]> {
        self.vertical.values.as_deref()
    }
    pub fn subset_source_glyphs(
        &self,
        source: &Self,
        remapped: &[super::RemappedGlyph],
    ) -> Result<BTreeMap<u16, u16>> {
        let mut originals = remapped
            .iter()
            .map(|m| (m.cid, m.source_glyph_id))
            .collect::<BTreeMap<_, _>>();
        // The pinned subsetter preserves ordered records, changing only their
        // component GIDs. Follow those pairs through the entire retained tail.
        let mut pending = originals
            .iter()
            .map(|(&cid, &gid)| (cid, gid))
            .collect::<Vec<_>>();
        while let Some((cid, source_gid)) = pending.pop() {
            let retained = self.components(usize::from(cid))?;
            let original = source.components(usize::from(source_gid))?;
            if retained.len() != original.len() {
                return Err(Error::Malformed);
            }
            for (tail, source_tail) in retained.into_iter().zip(original) {
                if tail.flags != source_tail.flags || tail.arguments != source_tail.arguments {
                    return Err(Error::Malformed);
                }
                let tail = u16::try_from(tail.glyph).map_err(|_| ())?;
                let source_tail = u16::try_from(source_tail.glyph).map_err(|_| ())?;
                if let Some(previous) = originals.insert(tail, source_tail) {
                    if previous != source_tail {
                        return Err(Error::Malformed);
                    }
                } else {
                    pending.push((tail, source_tail));
                }
            }
        }
        if originals.len() != self.widths.len() {
            return Err(Error::Malformed);
        }
        Ok(originals)
    }
    pub fn components(&self, gid: usize) -> Result<Vec<Component<'_>>> {
        let start = *self.locations.get(gid).ok_or(())?;
        let end = *self.locations.get(gid + 1).ok_or(())?;
        let raw = self.glyf.get(start..end).ok_or(())?;
        if raw.is_empty() || i16_at(raw, 0)? >= 0 {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        let mut at = 10;
        loop {
            let flags = u16_at(raw, at)?;
            let glyph = usize::from(u16_at(raw, at + 2)?);
            at += 4;
            let start = at;
            at += if flags & 1 != 0 { 4 } else { 2 };
            at += if flags & 8 != 0 {
                2
            } else if flags & 64 != 0 {
                4
            } else if flags & 128 != 0 {
                8
            } else {
                0
            };
            result.push(Component {
                glyph,
                flags,
                arguments: raw.get(start..at).ok_or(())?,
            });
            if flags & 32 == 0 {
                break;
            }
        }
        Ok(result)
    }
    pub fn outline(
        &self,
        gid: usize,
        matrix: Matrix,
        certified_matrix: enclosure::Matrix,
    ) -> Result<Outline> {
        let decoded = self.glyph(gid, &mut Vec::new())?;
        // The resolved left phantom is the glyph's horizontal origin. A marked
        // component can supply it even when parent hmtx/cache values differ.
        let offset = -decoded.phantoms[0].vertex.p[0];
        let mut o = Outline::enclosed(
            super::outline::concat(matrix, [1., 0., 0., 1., offset, 0.]),
            enclosure::concat(
                certified_matrix,
                [1., 0., 0., 1., offset, 0.].map(Interval::exact),
            ),
        );
        for c in decoded.contours {
            if c.is_empty() {
                continue;
            }
            let first = c[0];
            let last = c[c.len() - 1];
            let start = if first.on {
                first
            } else if last.on {
                last
            } else {
                mid(first, last)
            };
            o.mov_enclosed(start.p, start.enclosed);
            let begin = usize::from(first.on);
            let end = c.len() - usize::from(!first.on && last.on);
            let mut off: Option<Vertex> = None;
            for v in &c[begin..end] {
                if v.on {
                    if let Some(a) = off.take() {
                        o.quad_enclosed(a.p, v.p, a.enclosed, v.enclosed);
                    } else {
                        o.line_enclosed(v.p, v.enclosed);
                    }
                } else {
                    if let Some(a) = off {
                        let end = mid(a, *v);
                        o.quad_enclosed(a.p, end.p, a.enclosed, end.enclosed);
                    }
                    off = Some(*v);
                }
            }
            if let Some(a) = off {
                o.quad_enclosed(a.p, start.p, a.enclosed, start.enclosed);
            } else {
                o.line_enclosed(start.p, start.enclosed);
            }
            o.finish();
        }
        Ok(o)
    }
    fn glyph(&self, gid: usize, active: &mut Vec<usize>) -> Result<DecodedGlyph> {
        if gid + 1 >= self.locations.len() || active.contains(&gid) {
            return Err(().into());
        }
        active.push(gid);
        let raw = self
            .glyf
            .get(self.locations[gid]..self.locations[gid + 1])
            .ok_or(())?;
        let xmin = if raw.is_empty() { 0 } else { i16_at(raw, 2)? };
        let left = f64::from(xmin) - f64::from(self.bearings[gid]);
        let make = |p: Point, known| Anchor {
            vertex: Vertex {
                p,
                enclosed: p.map(Interval::exact),
                on: false,
            },
            known,
            version_unknown: [false; 2],
        };
        let vertical = self
            .vertical
            .values
            .as_ref()
            .and_then(|metrics| metrics.get(gid))
            .map(|&(advance, bearing)| {
                let ymax = if raw.is_empty() {
                    0.
                } else {
                    f64::from(i16_at(raw, 8)?)
                };
                Ok::<_, Error>([
                    ymax + f64::from(bearing),
                    ymax + f64::from(bearing) - f64::from(advance),
                ])
            })
            .transpose()?;
        let ys = vertical.unwrap_or([0.; 2]);
        let mut phantoms = [
            make([left, 0.], [true; 2]),
            make([left + f64::from(self.raw_widths[gid]), 0.], [true; 2]),
            make([0., ys[0]], [false, vertical.is_some()]),
            make([0., ys[1]], [false, vertical.is_some()]),
        ];
        for anchor in &mut phantoms[2..] {
            anchor.version_unknown = [false, self.vertical.unsupported_version];
        }
        let result = self.decode(raw, active, phantoms, self.raw_widths[gid]);
        active.pop();
        result
    }
    #[allow(clippy::too_many_lines)]
    fn decode(
        &self,
        raw: &[u8],
        active: &mut Vec<usize>,
        mut phantoms: [Anchor; 4],
        mut advance: u16,
    ) -> Result<DecodedGlyph> {
        if raw.is_empty() {
            return Ok(DecodedGlyph {
                contours: Vec::new(),
                phantoms,
                advance,
            });
        }
        let n = i16_at(raw, 0)?;
        raw.get(..10).ok_or(())?;
        let mut at = 10;
        if n >= 0 {
            if n == 0 && raw.len() == 10 {
                return Ok(DecodedGlyph {
                    contours: Vec::new(),
                    phantoms,
                    advance,
                });
            }
            let mut ends = Vec::new();
            for _ in 0..n {
                ends.push(usize::from(u16_at(raw, at)?));
                at += 2;
            }
            if ends.windows(2).any(|w| w[0] >= w[1]) {
                return Err(().into());
            }
            let length = usize::from(u16_at(raw, at)?);
            at += 2;
            instructions(raw.get(at..at + length).ok_or(())?)?;
            at += length;
            let count = ends.last().map_or(0, |v| v + 1);
            let mut flags = Vec::new();
            while flags.len() < count {
                let flag = *raw.get(at).ok_or(())?;
                at += 1;
                if flag & 0x80 != 0 {
                    return Err(().into());
                }
                let repeat = if flag & 8 != 0 {
                    let v = usize::from(*raw.get(at).ok_or(())?);
                    at += 1;
                    v
                } else {
                    0
                };
                if flags.len() + repeat + 1 > count {
                    return Err(().into());
                }
                flags.extend(std::iter::repeat_n(flag, repeat + 1));
            }
            let mut points = vec![
                Vertex {
                    p: [0., 0.],
                    enclosed: [Interval::exact(0.); 2],
                    on: false
                };
                count
            ];
            for axis in 0..2 {
                let (short, same) = if axis == 0 { (2, 16) } else { (4, 32) };
                let mut pos = 0i32;
                for (i, flag) in flags.iter().enumerate() {
                    let delta = if flag & short != 0 {
                        let v = i32::from(*raw.get(at).ok_or(())?);
                        at += 1;
                        if flag & same != 0 { v } else { -v }
                    } else if flag & same != 0 {
                        0
                    } else {
                        let v = i32::from(i16_at(raw, at)?);
                        at += 2;
                        v
                    };
                    pos = pos.checked_add(delta).ok_or(())?;
                    if i16::try_from(pos).is_err() {
                        return Err(().into());
                    }
                    points[i].p[axis] = f64::from(pos);
                    points[i].enclosed[axis] = Interval::exact(f64::from(pos));
                    points[i].on = flag & 1 != 0;
                }
            }
            let mut out = Vec::new();
            let mut first = 0;
            for end in ends {
                out.push(points[first..=end].to_vec());
                first = end + 1;
            }
            return Ok(DecodedGlyph {
                contours: out,
                phantoms,
                advance,
            });
        }
        let mut out: Vec<Vec<Vertex>> = Vec::new();
        let mut has_instructions = false;
        let parent_points = self
            .components(*active.last().ok_or(())?)?
            .into_iter()
            .try_fold(0_usize, |sum, part| -> Result<usize> {
                sum.checked_add(self.point_count(part.glyph, active)?)
                    .ok_or_else(|| ().into())
            })?;
        let mut first_component = true;
        loop {
            let flags = u16_at(raw, at)?;
            let gid = usize::from(u16_at(raw, at + 2)?);
            at += 4;
            if flags & 0xe010 != 0
                || [8, 64, 128].iter().filter(|b| flags & **b != 0).count() > 1
                || flags & 0x1800 == 0x1800
            {
                return Err(().into());
            }
            let xy = flags & 2 != 0;
            if first_component && !xy {
                return Err(().into());
            }
            first_component = false;
            let words = flags & 1 != 0;
            let arg = |at: usize| -> Result<i32> {
                if words {
                    Ok(if xy {
                        i32::from(i16_at(raw, at)?)
                    } else {
                        i32::from(u16_at(raw, at)?)
                    })
                } else {
                    let v = *raw.get(at).ok_or(())?;
                    Ok(if xy {
                        i32::from(i8::from_be_bytes([v]))
                    } else {
                        i32::from(v)
                    })
                }
            };
            let x = arg(at)?;
            let y = arg(at + if words { 2 } else { 1 })?;
            at += if words { 4 } else { 2 };
            let mut m = IDENTITY;
            let mut read = || -> Result<f64> {
                let v = f64::from(i16_at(raw, at)?) / 16384.;
                at += 2;
                Ok(v)
            };
            if flags & 8 != 0 {
                m[0] = read()?;
                m[3] = m[0];
            } else if flags & 64 != 0 {
                m[0] = read()?;
                m[3] = read()?;
            } else if flags & 128 != 0 {
                m[0] = read()?;
                m[1] = read()?;
                m[2] = read()?;
                m[3] = read()?;
            }
            let mut child = self.glyph(gid, active)?;
            // USE_MY_METRICS selects the recursively resolved original component
            // metrics, independently of geometry attachment/placement.
            let component_metrics = child.phantoms;
            for v in child.contours.iter_mut().flatten() {
                v.p = transform(m, v.p);
                v.enclosed = enclosure::transform(m.map(Interval::exact), v.enclosed);
            }
            child.phantoms = child.phantoms.map(|anchor| anchor.transformed(m));
            let matched = if xy {
                None
            } else {
                let a = anchor_at(
                    &out,
                    phantoms,
                    parent_points,
                    usize::try_from(x).map_err(|_| ())?,
                )?;
                let b = child.anchor(usize::try_from(y).map_err(|_| ())?)?;
                let has_geometry = child.contours.iter().any(|contour| !contour.is_empty());
                if has_geometry
                    && (!a.known.into_iter().all(|v| v) || !b.known.into_iter().all(|v| v))
                {
                    return Err(
                        if a.version_unknown.into_iter().any(|v| v)
                            || b.version_unknown.into_iter().any(|v| v)
                        {
                            Error::UnsupportedVerticalMetricsVersion
                        } else {
                            Error::UnsupportedVerticalPhantomContext
                        },
                    );
                }
                // Empty component geometry needs no placement, but its indices
                // were still checked. Original inherited metrics are independent.
                Some(if has_geometry {
                    (a.vertex, b.vertex)
                } else {
                    let zero = Vertex {
                        p: [0.; 2],
                        enclosed: [Interval::exact(0.); 2],
                        on: false,
                    };
                    (zero, zero)
                })
            };
            let (offset, enclosed_offset) = if let Some((a, b)) = matched {
                (
                    [a.p[0] - b.p[0], a.p[1] - b.p[1]],
                    [
                        a.enclosed[0].sub(b.enclosed[0]),
                        a.enclosed[1].sub(b.enclosed[1]),
                    ],
                )
            } else {
                let p = [f64::from(x), f64::from(y)];
                if flags & 0x0800 != 0 {
                    (
                        transform(m, p),
                        enclosure::transform(m.map(Interval::exact), p.map(Interval::exact)),
                    )
                } else {
                    (p, p.map(Interval::exact))
                }
            };
            // ROUND_XY_TO_GRID is device-pixel hint behavior, and is ignored for
            // point matching. Unhinted design coordinates retain their fractions.
            for c in &mut child.contours {
                for v in c {
                    v.p[0] += offset[0];
                    v.p[1] += offset[1];
                    v.enclosed = [
                        v.enclosed[0].add(enclosed_offset[0]),
                        v.enclosed[1].add(enclosed_offset[1]),
                    ];
                }
            }
            out.extend(child.contours);
            if flags & 0x0200 != 0 {
                if m[1] != 0. || m[0] < 0. {
                    return Err(Error::UnresolvedHorizontalMetrics);
                }
                phantoms[..2].copy_from_slice(&component_metrics[..2]);
                advance = child.advance;
            }
            has_instructions |= flags & 256 != 0;
            if flags & 32 == 0 {
                break;
            }
        }
        if has_instructions {
            let n = usize::from(u16_at(raw, at)?);
            at += 2;
            instructions(raw.get(at..at + n).ok_or(())?)?;
        }
        Ok(DecodedGlyph {
            contours: out,
            phantoms,
            advance,
        })
    }
    fn component_structure(&self, gid: usize) -> Result<()> {
        let parts = self.components(gid)?;
        let mut active = vec![gid];
        let counts = parts
            .iter()
            .map(|part| self.point_count(part.glyph, &mut active))
            .collect::<Result<Vec<_>>>()?;
        let total = counts.iter().try_fold(0_usize, |sum, &n| {
            sum.checked_add(n).ok_or(Error::Malformed)
        })?;
        let mut incorporated = 0;
        for (i, (part, count)) in parts.iter().zip(counts).enumerate() {
            let flags = part.flags;
            if flags & 0xe010 != 0
                || [8, 64, 128].iter().filter(|b| flags & **b != 0).count() > 1
                || flags & 0x1800 == 0x1800
                || (i == 0 && flags & 2 == 0)
            {
                return Err(Error::Malformed);
            }
            if flags & 2 == 0 {
                let (parent, child) = if flags & 1 != 0 {
                    (
                        usize::from(u16_at(part.arguments, 0)?),
                        usize::from(u16_at(part.arguments, 2)?),
                    )
                } else {
                    (
                        usize::from(*part.arguments.first().ok_or(())?),
                        usize::from(*part.arguments.get(1).ok_or(())?),
                    )
                };
                if !(parent < incorporated || (parent >= total && parent - total < 4))
                    || child >= count.checked_add(4).ok_or(())?
                {
                    return Err(Error::Malformed);
                }
            }
            incorporated += count;
        }
        Ok(())
    }
    fn point_count(&self, gid: usize, active: &mut Vec<usize>) -> Result<usize> {
        if gid + 1 >= self.locations.len() || active.contains(&gid) {
            return Err(().into());
        }
        let raw = self
            .glyf
            .get(self.locations[gid]..self.locations[gid + 1])
            .ok_or(())?;
        if raw.is_empty() {
            return Ok(0);
        }
        let contours = i16_at(raw, 0)?;
        if contours >= 0 {
            return if contours == 0 {
                Ok(0)
            } else {
                Ok(usize::from(u16_at(
                    raw,
                    10 + (usize::try_from(contours).map_err(|_| ())? - 1) * 2,
                )?) + 1)
            };
        }
        active.push(gid);
        let result =
            self.components(gid)?
                .into_iter()
                .try_fold(0_usize, |sum, part| -> Result<usize> {
                    sum.checked_add(self.point_count(part.glyph, active)?)
                        .ok_or_else(|| ().into())
                });
        active.pop();
        result
    }
}
fn anchor_at(
    contours: &[Vec<Vertex>],
    phantoms: [Anchor; 4],
    final_points: usize,
    index: usize,
) -> Result<Anchor> {
    if index >= final_points {
        phantoms
            .get(index - final_points)
            .copied()
            .ok_or(Error::Malformed)
    } else {
        contours
            .iter()
            .flatten()
            .nth(index)
            .copied()
            .map(Anchor::actual)
            .ok_or(Error::Malformed)
    }
}
fn vertical_metrics(face: &Face<'_>, glyphs: usize) -> Result<VerticalMetrics> {
    let raw = face.raw_face();
    let (Some(vhea), Some(vmtx)) = (
        raw.table(Tag::from_bytes(b"vhea")),
        raw.table(Tag::from_bytes(b"vmtx")),
    ) else {
        return Ok(VerticalMetrics {
            values: None,
            unsupported_version: false,
        });
    };
    let version = u32_at(vhea, 0)?;
    if !matches!(version, 0x10000 | 0x11000) {
        return Ok(VerticalMetrics {
            values: None,
            unsupported_version: true,
        });
    }
    if vhea.len() != 36
        || i16_at(vhea, 32)? != 0
        || vhea[24..32].iter().any(|v| *v != 0)
        || (version == 0x10000 && i16_at(vhea, 8)? != 0)
    {
        return Err(Error::Malformed);
    }
    let count = usize::from(u16_at(vhea, 34)?);
    if count == 0 || count > glyphs || vmtx.len() != count * 4 + (glyphs - count) * 2 {
        return Err(Error::Malformed);
    }
    (0..glyphs)
        .map(|gid| {
            Ok((
                u16_at(vmtx, gid.min(count - 1) * 4)?,
                i16_at(
                    vmtx,
                    if gid < count {
                        gid * 4 + 2
                    } else {
                        count * 4 + (gid - count) * 2
                    },
                )?,
            ))
        })
        .collect::<Result<Vec<_>>>()
        .map(|values| VerticalMetrics {
            values: Some(values),
            unsupported_version: false,
        })
}

fn mid(a: Vertex, b: Vertex) -> Vertex {
    Vertex {
        p: [a.p[0].midpoint(b.p[0]), a.p[1].midpoint(b.p[1])],
        enclosed: [
            a.enclosed[0].midpoint(b.enclosed[0]),
            a.enclosed[1].midpoint(b.enclosed[1]),
        ],
        on: true,
    }
}
fn instructions(data: &[u8]) -> Result<()> {
    let mut at = 0;
    while at < data.len() {
        let b = data[at];
        at += 1;
        let n = match b {
            0x40 | 0x41 => {
                let n = usize::from(*data.get(at).ok_or(())?);
                at += 1;
                n * if b == 0x41 { 2 } else { 1 }
            }
            0xb0..=0xbf => usize::from((b & 7) + 1) * if b >= 0xb8 { 2 } else { 1 },
            _ => 0,
        };
        data.get(at..at + n).ok_or(())?;
        at += n;
    }
    Ok(())
}

pub(super) fn name_table(raw: &[u8]) -> Result<Option<String>> {
    let version = u16_at(raw, 0)?;
    let count = usize::from(u16_at(raw, 2)?);
    let base = usize::from(u16_at(raw, 4)?);
    if version > 1 || base < 6 + count * 12 || base > raw.len() {
        return Err(().into());
    }
    let langs = if version == 1 {
        let at = 6 + count * 12;
        let langs = usize::from(u16_at(raw, at)?);
        if at + 2 + langs * 4 > base {
            return Err(().into());
        }
        langs
    } else {
        0
    };
    let mut previous = None;
    let mut postscript = None;
    for i in 0..count {
        let at = 6 + i * 12;
        let key = [
            u16_at(raw, at)?,
            u16_at(raw, at + 2)?,
            u16_at(raw, at + 4)?,
            u16_at(raw, at + 6)?,
        ];
        if previous.is_some_and(|old| old > key) {
            return Err(().into());
        }
        // Macintosh nameID20 uses FFFF for its cmap context, independently
        // of the v1 LangTagRecord namespace.
        let cmap_language = key[0] == 1 && key[2] == 0xffff && key[3] == 20;
        if version == 1
            && !cmap_language
            && key[2] >= 0x8000
            && usize::from(key[2] - 0x8000) >= langs
        {
            return Err(().into());
        }
        previous = Some(key);
        let len = usize::from(u16_at(raw, at + 8)?);
        let start = base
            .checked_add(usize::from(u16_at(raw, at + 10)?))
            .ok_or(())?;
        let bytes = raw
            .get(start..start.checked_add(len).ok_or(())?)
            .ok_or(())?;
        let utf16_names = key[0] == 0 || key[0] == 3 && !matches!(key[1], 3..=5);
        if utf16_names {
            utf16(bytes)?;
        }
        let identity_unicode = key[0] == 0 || key[0] == 3 && matches!(key[1], 0 | 1 | 10);
        if key[3] == 6 && (identity_unicode || key[0] == 1 && key[1] == 0) {
            let name = decode_known_name(key[0], key[1], bytes)?.ok_or(())?;
            if !super::valid_postscript_name(&name)
                || postscript.as_ref().is_some_and(|old| *old != name)
            {
                return Err(().into());
            }
            postscript = Some(name);
        }
        // Other ID6 encodings are explicitly permitted to be ignored by the
        // OpenType recommendations. Preserve their framing and bytes; do not
        // guess their translated identity or impose an encoding refusal.
    }
    if version == 1 {
        let at = 6 + count * 12;
        for i in 0..langs {
            let r = at + 2 + i * 4;
            let len = usize::from(u16_at(raw, r)?);
            let start = base
                .checked_add(usize::from(u16_at(raw, r + 2)?))
                .ok_or(())?;
            let bytes = raw
                .get(start..start.checked_add(len).ok_or(())?)
                .ok_or(())?;
            utf16(bytes)?;
            let words = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_be_bytes(*b))
                .collect::<Vec<_>>();
            let tag = String::from_utf16(&words).map_err(|_| ())?;
            if !language_tag(&tag) {
                return Err(().into());
            }
        }
    }
    Ok(postscript)
}
pub(super) fn decode_known_name(
    platform: u16,
    encoding: u16,
    bytes: &[u8],
) -> Result<Option<String>> {
    // Default MacRoman character identities, published by Apple:
    // https://www.unicode.org/Public/MAPPINGS/VENDORS/APPLE/ROMAN.TXT
    // Family aliases use this default mapping. Historical regional or currency
    // variants need their actual context or a Unicode alias for exact spelling.
    const MAC_ROMAN: [char; 128] = [
        '\u{c4}', '\u{c5}', '\u{c7}', '\u{c9}', '\u{d1}', '\u{d6}', '\u{dc}', '\u{e1}', '\u{e0}',
        '\u{e2}', '\u{e4}', '\u{e3}', '\u{e5}', '\u{e7}', '\u{e9}', '\u{e8}', '\u{ea}', '\u{eb}',
        '\u{ed}', '\u{ec}', '\u{ee}', '\u{ef}', '\u{f1}', '\u{f3}', '\u{f2}', '\u{f4}', '\u{f6}',
        '\u{f5}', '\u{fa}', '\u{f9}', '\u{fb}', '\u{fc}', '\u{2020}', '\u{b0}', '\u{a2}', '\u{a3}',
        '\u{a7}', '\u{2022}', '\u{b6}', '\u{df}', '\u{ae}', '\u{a9}', '\u{2122}', '\u{b4}',
        '\u{a8}', '\u{2260}', '\u{c6}', '\u{d8}', '\u{221e}', '\u{b1}', '\u{2264}', '\u{2265}',
        '\u{a5}', '\u{b5}', '\u{2202}', '\u{2211}', '\u{220f}', '\u{3c0}', '\u{222b}', '\u{aa}',
        '\u{ba}', '\u{3a9}', '\u{e6}', '\u{f8}', '\u{bf}', '\u{a1}', '\u{ac}', '\u{221a}',
        '\u{192}', '\u{2248}', '\u{2206}', '\u{ab}', '\u{bb}', '\u{2026}', '\u{a0}', '\u{c0}',
        '\u{c3}', '\u{d5}', '\u{152}', '\u{153}', '\u{2013}', '\u{2014}', '\u{201c}', '\u{201d}',
        '\u{2018}', '\u{2019}', '\u{f7}', '\u{25ca}', '\u{ff}', '\u{178}', '\u{2044}', '\u{20ac}',
        '\u{2039}', '\u{203a}', '\u{fb01}', '\u{fb02}', '\u{2021}', '\u{b7}', '\u{201a}',
        '\u{201e}', '\u{2030}', '\u{c2}', '\u{ca}', '\u{c1}', '\u{cb}', '\u{c8}', '\u{cd}',
        '\u{ce}', '\u{cf}', '\u{cc}', '\u{d3}', '\u{d4}', '\u{f8ff}', '\u{d2}', '\u{da}', '\u{db}',
        '\u{d9}', '\u{131}', '\u{2c6}', '\u{2dc}', '\u{af}', '\u{2d8}', '\u{2d9}', '\u{2da}',
        '\u{b8}', '\u{2dd}', '\u{2db}', '\u{2c7}',
    ];
    if platform == 0 || platform == 3 && !matches!(encoding, 3..=5) {
        let words = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_be_bytes(*b))
            .collect::<Vec<_>>();
        return String::from_utf16(&words).map(Some).map_err(|_| ().into());
    }
    if platform != 1 || encoding != 0 {
        return Ok(None);
    }
    Ok(Some(
        bytes
            .iter()
            .map(|&byte| {
                if byte < 128 {
                    char::from(byte)
                } else {
                    MAC_ROMAN[usize::from(byte - 128)]
                }
            })
            .collect(),
    ))
}

fn language_tag(tag: &str) -> bool {
    // RFC 5646 well-formedness only: registry membership and the human
    // designation of a language remain distinct from this finite grammar.
    let lower = tag.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "en-gb-oed"
            | "i-ami"
            | "i-bnn"
            | "i-default"
            | "i-enochian"
            | "i-hak"
            | "i-klingon"
            | "i-lux"
            | "i-mingo"
            | "i-navajo"
            | "i-pwn"
            | "i-tao"
            | "i-tay"
            | "i-tsu"
            | "sgn-be-fr"
            | "sgn-be-nl"
            | "sgn-ch-de"
            | "art-lojban"
            | "cel-gaulish"
            | "no-bok"
            | "no-nyn"
            | "zh-guoyu"
            | "zh-hakka"
            | "zh-min"
            | "zh-min-nan"
            | "zh-xiang"
    ) {
        return true;
    }
    let parts = lower.split('-').collect::<Vec<_>>();
    if parts
        .iter()
        .any(|p| p.is_empty() || p.len() > 8 || !p.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return false;
    }
    if parts[0] == "x" {
        return parts.len() > 1;
    }
    let alpha = |p: &str| p.bytes().all(|b| b.is_ascii_alphabetic());
    if !(2..=8).contains(&parts[0].len()) || !alpha(parts[0]) {
        return false;
    }
    let mut at = 1;
    if parts[0].len() <= 3 {
        for _ in 0..3 {
            if parts.get(at).is_some_and(|p| p.len() == 3 && alpha(p)) {
                at += 1;
            } else {
                break;
            }
        }
    }
    if parts.get(at).is_some_and(|p| p.len() == 4 && alpha(p)) {
        at += 1;
    }
    if parts.get(at).is_some_and(|p| {
        p.len() == 2 && alpha(p) || p.len() == 3 && p.bytes().all(|b| b.is_ascii_digit())
    }) {
        at += 1;
    }
    let mut variants = BTreeSet::new();
    while let Some(p) = parts
        .get(at)
        .filter(|p| p.len() >= 5 || p.len() == 4 && p.as_bytes()[0].is_ascii_digit())
    {
        if !variants.insert(*p) {
            return false;
        }
        at += 1;
    }
    let mut singletons = BTreeSet::new();
    while let Some(p) = parts.get(at).filter(|p| p.len() == 1 && **p != "x") {
        if !singletons.insert(*p) {
            return false;
        }
        at += 1;
        let start = at;
        while parts.get(at).is_some_and(|p| p.len() >= 2) {
            at += 1;
        }
        if at == start {
            return false;
        }
    }
    if parts.get(at) == Some(&"x") {
        return at + 1 < parts.len();
    }
    at == parts.len()
}
fn utf16(bytes: &[u8]) -> Result<()> {
    if !bytes.len().is_multiple_of(2) {
        return Err(().into());
    }
    let words = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_be_bytes(*b));
    if char::decode_utf16(words).any(|v| v.is_err()) {
        return Err(().into());
    }
    Ok(())
}
fn post_table(raw: &[u8], glyphs: usize) -> Result<()> {
    raw.get(..32).ok_or(())?;
    match u32_at(raw, 0)? {
        0x0001_0000 => {
            if glyphs != 258 || raw.len() != 32 {
                return Err(().into());
            }
        }
        0x0003_0000 => {
            if raw.len() != 32 {
                return Err(().into());
            }
        }
        0x0002_0000 => {
            if usize::from(u16_at(raw, 32)?) != glyphs {
                return Err(().into());
            }
            let mut largest = 0;
            for i in 0..glyphs {
                let v = u16_at(raw, 34 + i * 2)?;
                if v > 32767 {
                    return Err(().into());
                }
                largest = largest.max(v);
            }
            let mut at = 34 + glyphs * 2;
            let mut strings = 0;
            while at < raw.len() {
                let n = usize::from(raw[at]);
                at += 1;
                raw.get(at..at + n).ok_or(())?;
                at += n;
                strings += 1;
            }
            if usize::from(largest) >= 258 + strings {
                return Err(().into());
            }
        }
        0x0002_5000 => {
            if usize::from(u16_at(raw, 32)?) != glyphs || raw.len() != 34 + glyphs {
                return Err(().into());
            }
            for i in 0..glyphs {
                let v =
                    i32::try_from(i).map_err(|_| ())? + i32::from(i8::from_be_bytes([raw[34 + i]]));
                if !(0..258).contains(&v) {
                    return Err(().into());
                }
            }
        }
        0x0004_0000 => {
            if raw.len() != 32 + glyphs * 2 {
                return Err(().into());
            }
        }
        _ => return Err(().into()),
    }
    Ok(())
}

fn component_depth(gid: usize, components: &[Vec<usize>], memo: &mut [Option<usize>]) -> usize {
    if let Some(depth) = memo[gid] {
        return depth;
    }
    // Cycles and missing components were already refused by complete decoding.
    let depth = components[gid]
        .iter()
        .map(|g| 1 + component_depth(*g, components, memo))
        .max()
        .unwrap_or(0);
    memo[gid] = Some(depth);
    depth
}
fn requirements(raw: &[u8]) -> Result<(Vec<usize>, usize)> {
    if raw.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let n = i16_at(raw, 0)?;
    if n >= 0 {
        let at = 10 + usize::try_from(n).map_err(|_| ())? * 2;
        return Ok((
            Vec::new(),
            if at == raw.len() {
                0
            } else {
                usize::from(u16_at(raw, at)?)
            },
        ));
    }
    let mut refs = Vec::new();
    let mut at = 10;
    let mut instructions = false;
    loop {
        let flags = u16_at(raw, at)?;
        refs.push(usize::from(u16_at(raw, at + 2)?));
        at += 4;
        at += if flags & 1 != 0 { 4 } else { 2 };
        at += if flags & 8 != 0 {
            2
        } else if flags & 64 != 0 {
            4
        } else if flags & 128 != 0 {
            8
        } else {
            0
        };
        instructions |= flags & 256 != 0;
        if flags & 32 == 0 {
            break;
        }
    }
    raw.get(..at).ok_or(())?;
    let length = if instructions {
        usize::from(u16_at(raw, at)?)
    } else {
        0
    };
    if instructions {
        super::truetype_program::instructions(raw.get(at + 2..at + 2 + length).ok_or(())?)?;
    }
    Ok((refs, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(language: u16, tags: &[&str]) -> Vec<u8> {
        let base = 20 + tags.len() * 4;
        let mut raw = Vec::new();
        for value in [
            1,
            1,
            u16::try_from(base).unwrap(),
            3,
            1,
            language,
            1,
            2,
            0,
            u16::try_from(tags.len()).unwrap(),
        ] {
            raw.extend(value.to_be_bytes());
        }
        let mut strings = vec![0, b'A'];
        for tag in tags {
            let bytes = tag
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<_>>();
            raw.extend(u16::try_from(bytes.len()).unwrap().to_be_bytes());
            raw.extend(u16::try_from(strings.len()).unwrap().to_be_bytes());
            strings.extend(bytes);
        }
        raw.extend(strings);
        raw
    }

    #[test]
    fn windows_name_strings_follow_naming_utf16_instead_of_cmap_codecs() {
        for encoding in [0_u16, 1, 2, 6, 10, 99] {
            let mut raw = named(0x409, &[]);
            raw[8..10].copy_from_slice(&encoding.to_be_bytes());
            assert!(name_table(&raw).is_ok(), "valid UTF16 encoding {encoding}");
            raw[20..22].copy_from_slice(&0xd800_u16.to_be_bytes());
            assert!(
                name_table(&raw).is_err(),
                "invalid UTF16 encoding {encoding}"
            );
        }
        for encoding in [3_u16, 4, 5] {
            let mut raw = named(0x409, &[]);
            raw[8..10].copy_from_slice(&encoding.to_be_bytes());
            raw[20..22].copy_from_slice(&0xd800_u16.to_be_bytes());
            assert!(name_table(&raw).is_ok(), "legacy context {encoding}");
        }
    }

    #[test]
    fn name_v1_language_tag_valid_neighbors() {
        assert!(name_table(&named(0x409, &[])).is_ok());
        for tag in [
            "en-US",
            "x-user-private",
            "i-klingon",
            "en-GB-oed",
            "zh-min-nan",
            "zh-cmn-Hans-CN",
            "sl-rozaj-biske-1994",
            "en-Latn-US-u-ca-gregory-x-a-a",
            "de-CH-1901",
            "es-419",
            "abcd",
            "abcde",
            "en-0-abc",
        ] {
            assert!(name_table(&named(0x8000, &[tag])).is_ok(), "{tag}");
        }
        assert!(name_table(&named(0x8001, &["en", "fr"])).is_ok());
        assert!(name_table(&named(0x8001, &["en", "en"])).is_ok());
        let mut shared = named(0x8001, &["en", "fr"]);
        shared[26..28].copy_from_slice(&2_u16.to_be_bytes());
        assert!(name_table(&shared).is_ok());
    }

    #[test]
    fn name_v1_language_indices_and_record_extents_are_checked() {
        assert!(name_table(&named(0x8000, &[])).is_err());
        assert!(name_table(&named(0x8001, &["en-US"])).is_err());
        assert!(name_table(&named(0xffff, &["en-US"])).is_err());
        let mut extent = named(0x8000, &["en-US"]);
        extent[22..24].copy_from_slice(&0xffff_u16.to_be_bytes());
        assert!(name_table(&extent).is_err());
        let mut utf = named(0x8000, &["en-US"]);
        let last = utf.len() - 2;
        utf[last..].copy_from_slice(&0xd800_u16.to_be_bytes());
        assert!(name_table(&utf).is_err());
        let mut cmap = named(0xffff, &[]);
        cmap[6..8].copy_from_slice(&1_u16.to_be_bytes());
        cmap[8..10].copy_from_slice(&1_u16.to_be_bytes());
        cmap[12..14].copy_from_slice(&20_u16.to_be_bytes());
        cmap[14..16].copy_from_slice(&1_u16.to_be_bytes());
        cmap[20] = b'A';
        cmap.truncate(21);
        assert!(
            name_table(&cmap).is_ok(),
            "Macintosh nameID20 uses cmap context, not a LangTag index"
        );
    }

    #[test]
    fn name_v1_language_tags_require_bcp47_lexical_syntax() {
        for tag in [
            "",
            "en_US",
            "en--US",
            "x",
            "en-x",
            "en-a",
            "en-a-foo-A-bar",
            "de-1901-1901",
            "sl-ROZAJ-rozaj",
            "en-abcdefghi",
            "abc-def-ghi-jkl-mno",
            "i-not-grandfathered",
            "en-ü",
            "en-US-Latn",
        ] {
            assert!(name_table(&named(0x8000, &[tag])).is_err(), "{tag}");
        }
    }

    #[test]
    fn empty_composite_cached_rectangle_does_not_enter_aggregate_bounds() {
        let raw = [255, 255, 0, 10, 0, 20, 0, 30, 0, 40, 0, 2, 0, 0, 0, 0];
        let program = Program {
            glyf: &raw,
            locations: vec![0, 0, raw.len()],
            widths: vec![0; 2],
            raw_widths: vec![0; 2],
            bearings: vec![0; 2],
            source_deferred: vec![false; 2],
            vertical: VerticalMetrics {
                values: None,
                unsupported_version: false,
            },
        };
        assert_eq!(program.cached_bounds(1).unwrap(), None);
    }
}
