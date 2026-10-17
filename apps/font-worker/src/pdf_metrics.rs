use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use ttf_parser::{Face, GlyphId, OutlineBuilder};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Serialize)]
pub struct PdfMetrics {
    pub bbox: [f32; 4],
    pub ascent: f32,
    pub descent: f32,
    pub cap_height: Option<f32>,
    pub stem_v: f32,
    pub widths: Vec<f32>,
}

pub fn prepare(
    source: &Face<'_>,
    prepared: &Face<'_>,
    remapped: &[super::RemappedGlyph],
    full: bool,
) -> PdfMetrics {
    let scale = 1000.0 / f32::from(prepared.units_per_em());
    let mut bbox: Option<[i16; 4]> = None;
    let accented = accented_glyphs(source);
    let originals = if full {
        BTreeMap::new()
    } else {
        remapped
            .iter()
            .map(|mapping| (mapping.cid, mapping.source_glyph_id))
            .collect()
    };
    let mut ascent = 0;
    for gid in 0..prepared.number_of_glyphs() {
        if let Some(rect) = prepared.glyph_bounding_box(GlyphId(gid)) {
            let original = if full {
                gid
            } else {
                originals.get(&gid).copied().unwrap_or(0)
            };
            if !accented.contains(&original) {
                ascent = ascent.max(rect.y_max);
            }
            bbox = Some(
                bbox.map_or([rect.x_min, rect.y_min, rect.x_max, rect.y_max], |old| {
                    [
                        old[0].min(rect.x_min),
                        old[1].min(rect.y_min),
                        old[2].max(rect.x_max),
                        old[3].max(rect.y_max),
                    ]
                }),
            );
        }
    }
    // OS/2 can be removed by subsetting. Capture its design information from
    // the original face. A flat Latin capital supplies the fallback; fonts
    // without one have no fabricated Latin cap-height value.
    let cap = capital_height(source);
    let source_scale = 1000.0 / f32::from(source.units_per_em());
    let stem = ['H', 'I', 'E', 'F', 'T']
        .into_iter()
        .find_map(|letter| {
            source
                .glyph_index(letter)
                .and_then(|glyph| stem_width(source, glyph))
        })
        .map(|width| width * source_scale)
        .or_else(|| {
            (1..prepared.number_of_glyphs())
                .find_map(|gid| stem_width(prepared, GlyphId(gid)))
                .map(|width| width * scale)
        });
    PdfMetrics {
        bbox: bbox.unwrap_or([0; 4]).map(|value| f32::from(value) * scale),
        ascent: f32::from(ascent) * scale,
        descent: f32::from(bbox.map_or(0, |bounds| bounds[1].min(0))) * scale,
        cap_height: cap.map(|value| f32::from(value) * source_scale),
        stem_v: stem.unwrap_or(0.0),
        widths: (0..prepared.number_of_glyphs())
            .map(|gid| f32::from(prepared.glyph_hor_advance(GlyphId(gid)).unwrap_or(0)) * scale)
            .collect(),
    }
}

fn accented_glyphs(source: &Face<'_>) -> BTreeSet<u16> {
    let Some(cmap) = source.tables().cmap else {
        return BTreeSet::new();
    };
    let mut accented = BTreeSet::new();
    let mut unaccented = BTreeSet::new();
    for table in cmap
        .subtables
        .into_iter()
        .filter(ttf_parser::cmap::Subtable::is_unicode)
    {
        table.codepoints(|codepoint| {
            let (Some(character), Some(glyph)) =
                (char::from_u32(codepoint), table.glyph_index(codepoint))
            else {
                return;
            };
            if character
                .nfd()
                .any(|part| unicode_normalization::char::canonical_combining_class(part) != 0)
            {
                accented.insert(glyph.0);
            } else {
                unaccented.insert(glyph.0);
            }
        });
    }
    accented.retain(|glyph| !unaccented.contains(glyph));
    accented
}

pub fn capital_height(source: &Face<'_>) -> Option<i16> {
    source
        .capital_height()
        .filter(|value| *value > 0)
        .or_else(|| {
            ['H', 'I', 'T', 'E', 'F'].into_iter().find_map(|letter| {
                source
                    .glyph_index(letter)
                    .and_then(|gid| source.glyph_bounding_box(gid))
                    .map(|rect| rect.y_max)
                    .filter(|value| *value > 0)
            })
        })
}

fn stem_width(face: &Face<'_>, glyph: GlyphId) -> Option<f32> {
    let mut outline = StemOutline::default();
    face.outline_glyph(glyph, &mut outline)?;
    outline.width()
}

#[derive(Default)]
struct StemOutline {
    point: (f32, f32),
    start: (f32, f32),
    segments: Vec<((f32, f32), (f32, f32))>,
}
impl StemOutline {
    fn width(&self) -> Option<f32> {
        let lo = self
            .segments
            .iter()
            .flat_map(|(a, b)| [a.1, b.1])
            .min_by(f32::total_cmp)?;
        let hi = self
            .segments
            .iter()
            .flat_map(|(a, b)| [a.1, b.1])
            .max_by(f32::total_cmp)?;
        if hi <= lo {
            return None;
        }
        let mut spans = Vec::new();
        for fraction in [0.25, 0.5, 0.75] {
            let y = lo + (hi - lo) * fraction;
            let mut crossings = self
                .segments
                .iter()
                .filter_map(|(a, b)| {
                    if (a.1 <= y && y < b.1) || (b.1 <= y && y < a.1) {
                        Some((
                            a.0 + (b.0 - a.0) * (y - a.1) / (b.1 - a.1),
                            if b.1 > a.1 { 1 } else { -1 },
                        ))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut winding = 0;
            let mut start = 0.0;
            for (x, direction) in crossings {
                if winding == 0 {
                    start = x;
                }
                winding += direction;
                if winding == 0 && x > start {
                    spans.push(x - start);
                }
            }
        }
        if spans.is_empty() {
            None
        } else {
            let middle = spans.len() / 2;
            Some(*spans.select_nth_unstable_by(middle, f32::total_cmp).1)
        }
    }
}
impl OutlineBuilder for StemOutline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.point = (x, y);
        self.start = (x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.segments.push((self.point, (x, y)));
        self.point = (x, y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let origin = self.point;
        for step in 1..=32_u8 {
            let t = f32::from(step) / 32.0;
            let u = 1.0 - t;
            self.line_to(
                u * u * origin.0 + 2.0 * u * t * cx + t * t * x,
                u * u * origin.1 + 2.0 * u * t * cy + t * t * y,
            );
        }
    }
    fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
        let origin = self.point;
        for step in 1..=32_u8 {
            let t = f32::from(step) / 32.0;
            let u = 1.0 - t;
            self.line_to(
                u * u * u * origin.0 + 3.0 * u * u * t * ax + 3.0 * u * t * t * bx + t * t * t * x,
                u * u * u * origin.1 + 3.0 * u * u * t * ay + 3.0 * u * t * t * by + t * t * t * y,
            );
        }
    }
    fn close(&mut self) {
        self.line_to(self.start.0, self.start.1);
    }
}
