#![allow(clippy::float_cmp, clippy::many_single_char_names)] // Exact source operands and matrix algebra.
//! Exact CFF1 coordinate/advance interpretation and retained-program framing.
//! Expands only the existing subsetter-supported path forms with exact 16.16
//! coordinate accumulation, then binary64 curve analysis.
//! This scanner checks grammar, termination and the width prefix; it does not
//! implement arithmetic, storage, random or a general font VM.
use super::enclosure::{self, Interval};
use super::outline::{IDENTITY, Matrix, Outline, Point, concat};
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};
#[derive(Clone, Default)]
pub(super) struct Dict {
    values: BTreeMap<u16, Vec<f64>>,
    matrix: Option<(enclosure::Matrix, enclosure::Matrix)>,
    pub(super) matrix_texts: Option<[String; 6]>,
}
impl Dict {
    fn new() -> Self {
        Self::default()
    }
}
impl Deref for Dict {
    type Target = BTreeMap<u16, Vec<f64>>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
impl DerefMut for Dict {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.values
    }
}

#[derive(Debug)]
pub enum Error {
    Malformed,
    ResourceLimit,
    UnsupportedVerticalPhantomContext,
    UnresolvedHorizontalMetrics,
    UnsupportedVerticalMetricsVersion,
}
impl From<()> for Error {
    fn from((): ()) -> Self {
        Self::Malformed
    }
}
type Result<T> = std::result::Result<T, Error>;
#[allow(clippy::cast_precision_loss)]
fn coordinate(v: i128) -> f64 {
    v as f64 / 65536.
}
#[allow(clippy::cast_possible_truncation)] // Type2 operands are signed integers or exact signed 16.16.
fn operand(v: f64) -> i128 {
    (v * 65536.) as i128
}

pub struct Program<'a> {
    data: &'a [u8],
    pub(super) top: Dict,
    chars: Vec<&'a [u8]>,
    globals: Vec<&'a [u8]>,
    pub(super) fds: Vec<Dict>,
    pub(super) select: Vec<usize>,
}
impl<'a> Program<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        if data.len() < 4
            || data[0] != 1
            || data[1] != 0
            || data[2] < 4
            || !(1..=4).contains(&data[3])
        {
            return Err(Error::Malformed);
        }
        let (names, at) = index(data, usize::from(data[2]))?;
        let (tops, at) = index(data, at)?;
        if names.len() != 1 || tops.len() != 1 {
            return Err(Error::Malformed);
        }
        let (strings, at) = index(data, at)?;
        if names[0].is_empty()
            || names[0].len() > 127
            || names[0]
                .iter()
                .any(|b| !(33..=126).contains(b) || b"[](){}<>/%".contains(b))
            || strings.len() + 390 > 64999
        {
            return Err(Error::Malformed);
        }
        let (globals, _) = index(data, at)?;
        let top = dict(tops[0], false)?;
        sids(&top, strings.len())?;
        if value(&top, 1206, 2.) != 2. {
            return Err(Error::Malformed);
        }
        let (chars, _) = index(data, offset(&top, 17)?)?;
        if chars.is_empty() || chars.len() > 65535 {
            return Err(Error::Malformed);
        }
        let fds = if top.contains_key(&1230) {
            let (raw, _) = index(data, offset(&top, 1236)?)?;
            raw.into_iter()
                .map(|d| dict(d, false))
                .collect::<Result<Vec<_>>>()?
        } else {
            vec![top.clone()]
        };
        if fds.is_empty() || fds.len() > 256 {
            return Err(Error::Malformed);
        }
        let select = if top.contains_key(&1230) {
            fdselect(data, offset(&top, 1237)?, chars.len(), fds.len())?
        } else {
            vec![0; chars.len()]
        };
        for d in &fds {
            sids(d, strings.len())?;
            if top.contains_key(&1230) && !d.contains_key(&18) {
                return Err(Error::Malformed);
            }
            private(data, d)?;
            matrix(d, IDENTITY)?;
        }
        matrix(&top, [0.001, 0., 0., 0.001, 0., 0.])?;
        Ok(Self {
            data,
            top,
            chars,
            globals,
            fds,
            select,
        })
    }
    pub fn italic_angle(&self, gid: usize) -> Result<f64> {
        let fd = &self.fds[*self.select.get(gid).ok_or(())?];
        Ok(value(fd, 1202, value(&self.top, 1202, 0.)))
    }
    pub fn matrix(&self, gid: usize) -> Result<Matrix> {
        let absolute_fd = self.top.contains_key(&1230)
            && !self.top.contains_key(&1207)
            && self.fds[*self.select.get(gid).ok_or(())?].contains_key(&1207);
        let top = matrix(
            &self.top,
            if absolute_fd {
                IDENTITY
            } else {
                [0.001, 0., 0., 0.001, 0., 0.]
            },
        )?;
        // PLRM CID initialization: with absent Top, an explicit FD matrix is
        // absolute; an absent FD receives the default .001. With explicit Top,
        // FD is relative. Prepared subsets carry explicit Top after normalization.
        let fd = if self.top.contains_key(&1230) {
            matrix(&self.fds[*self.select.get(gid).ok_or(())?], IDENTITY)?
        } else {
            IDENTITY
        };
        Ok(concat(
            [
                top[0] * 1000.,
                top[1] * 1000.,
                top[2] * 1000.,
                top[3] * 1000.,
                top[4] * 1000.,
                top[5] * 1000.,
            ],
            fd,
        ))
    }
    fn matrix_enclosure(&self, gid: usize) -> Result<enclosure::Matrix> {
        let identity = IDENTITY.map(Interval::exact);
        let absolute_fd = self.top.contains_key(&1230)
            && !self.top.contains_key(&1207)
            && self.fds[*self.select.get(gid).ok_or(())?].contains_key(&1207);
        let top = self.top.matrix.map_or(
            if absolute_fd {
                [1000., 0., 0., 1000., 0., 0.].map(Interval::exact)
            } else {
                identity
            },
            |m| m.1,
        );
        let fd = if self.top.contains_key(&1230) {
            self.fds[*self.select.get(gid).ok_or(())?]
                .matrix
                .map_or(identity, |m| m.0)
        } else {
            identity
        };
        Ok(enclosure::concat(top, fd))
    }
    pub fn width(&self, gid: usize) -> Result<f64> {
        Ok(self.measure(gid, false)?.1)
    }
    pub fn outline_with_width(&self, gid: usize) -> Result<(Outline, f64)> {
        let (outline, width) = self.measure(gid, true)?;
        Ok((outline.ok_or(Error::Malformed)?, width))
    }
    fn measure(&self, gid: usize, draw: bool) -> Result<(Option<Outline>, f64)> {
        let (p, local) = private(self.data, &self.fds[*self.select.get(gid).ok_or(())?])?;
        let mut scan = Scanner::new(
            &local,
            &self.globals,
            if draw {
                Some(Outline::enclosed(
                    self.matrix(gid)?,
                    self.matrix_enclosure(gid)?,
                ))
            } else {
                None
            },
        );
        if !scan.run(self.chars[gid], 0, false)? {
            return Err(Error::Malformed);
        }
        let width = scan
            .width
            .map_or(value(&p, 20, 0.), |w| coordinate(w) + value(&p, 21, 0.));
        Ok((scan.outline, width))
    }
}
fn value(d: &Dict, key: u16, default: f64) -> f64 {
    d.get(&key).map_or(default, |v| v[0])
}
fn sids(d: &Dict, strings: usize) -> Result<()> {
    for key in [0, 1, 2, 3, 4, 1200, 1221, 1222, 1230, 1238] {
        if let Some(values) = d.get(&key) {
            for v in values.iter().take(if key == 1230 { 2 } else { 1 }) {
                let sid = integer(*v)?;
                if sid > 64999 || sid >= 391 + strings {
                    return Err(Error::Malformed);
                }
            }
        }
    }
    Ok(())
}
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn integer(v: f64) -> Result<usize> {
    if v.is_finite() && v >= 0. && v.fract() == 0. && v <= f64::from(u32::MAX) {
        Ok(v as usize)
    } else {
        Err(Error::Malformed)
    }
}
pub(super) fn offset(d: &Dict, key: u16) -> Result<usize> {
    integer(*d.get(&key).ok_or(())?.first().ok_or(())?)
}
fn matrix(d: &Dict, default: Matrix) -> Result<Matrix> {
    d.get(&1207).map_or(Ok(default), |v| {
        v.as_slice().try_into().map_err(|_| Error::Malformed)
    })
}
pub(super) fn index(data: &[u8], at: usize) -> Result<(Vec<&[u8]>, usize)> {
    let n = usize::from(u16::from_be_bytes(
        data.get(at..at + 2).ok_or(())?.try_into().map_err(|_| ())?,
    ));
    if n == 0 {
        return Ok((Vec::new(), at + 2));
    }
    let size = usize::from(*data.get(at + 2).ok_or(())?);
    if !(1..=4).contains(&size) {
        return Err(Error::Malformed);
    }
    let base = at.checked_add(3 + (n + 1) * size).ok_or(())?;
    let mut offsets = Vec::new();
    for i in 0..=n {
        let mut v = 0usize;
        for b in data
            .get(at + 3 + i * size..at + 3 + (i + 1) * size)
            .ok_or(())?
        {
            v = v
                .checked_mul(256)
                .and_then(|v| v.checked_add(usize::from(*b)))
                .ok_or(())?;
        }
        offsets.push(v);
    }
    if offsets[0] != 1 || offsets.windows(2).any(|p| p[0] > p[1]) {
        return Err(Error::Malformed);
    }
    let end = base.checked_add(offsets[n] - 1).ok_or(())?;
    if end > data.len() {
        return Err(Error::Malformed);
    }
    Ok((
        offsets
            .windows(2)
            .map(|p| &data[base + p[0] - 1..base + p[1] - 1])
            .collect(),
        end,
    ))
}
pub(super) fn number(data: &[u8], at: &mut usize, dict: bool) -> Result<f64> {
    let b = *data.get(*at).ok_or(())?;
    *at += 1;
    let signed = |at: &mut usize, n: usize| -> Result<i32> {
        let s = data.get(*at..*at + n).ok_or(())?;
        *at += n;
        Ok(if n == 2 {
            i32::from(i16::from_be_bytes(s.try_into().map_err(|_| ())?))
        } else {
            i32::from_be_bytes(s.try_into().map_err(|_| ())?)
        })
    };
    Ok(match b {
        32..=246 => f64::from(i32::from(b) - 139),
        247..=254 => {
            let tail = *data.get(*at).ok_or(())?;
            *at += 1;
            f64::from(if b <= 250 {
                (i32::from(b) - 247) * 256 + i32::from(tail) + 108
            } else {
                -(i32::from(b) - 251) * 256 - i32::from(tail) - 108
            })
        }
        28 => f64::from(signed(at, 2)?),
        29 if dict => f64::from(signed(at, 4)?),
        255 if !dict => f64::from(signed(at, 4)?) / 65536.,
        30 if dict => {
            let mut s = String::new();
            'real: loop {
                let b = *data.get(*at).ok_or(())?;
                *at += 1;
                for n in [b >> 4, b & 15] {
                    match n {
                        0..=9 => s.push(char::from(b'0' + n)),
                        10 => s.push('.'),
                        11 => s.push('E'),
                        12 => s.push_str("E-"),
                        14 => s.push('-'),
                        15 => break 'real,
                        _ => return Err(Error::Malformed),
                    }
                }
            }
            let v = s.parse::<f64>().map_err(|_| ())?;
            if !v.is_finite() {
                return Err(Error::Malformed);
            }
            v
        }
        _ => return Err(Error::Malformed),
    })
}
pub(super) fn dict(data: &[u8], private: bool) -> Result<Dict> {
    let mut result = Dict::new();
    let mut stack = Vec::new();
    let mut matrix_numbers = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let b = data[at];
        if b == 28 || b == 29 || b == 30 || (32..=254).contains(&b) {
            let start = at;
            let v = number(data, &mut at, true)?;
            stack.push(v);
            let text = if data[start] == 30 {
                let mut text = String::new();
                'digits: for b in &data[start + 1..at] {
                    for n in [b >> 4, b & 15] {
                        match n {
                            0..=9 => text.push(char::from(b'0' + n)),
                            10 => text.push('.'),
                            11 => text.push('E'),
                            12 => text.push_str("E-"),
                            14 => text.push('-'),
                            15 => break 'digits,
                            _ => return Err(Error::Malformed),
                        }
                    }
                }
                text
            } else {
                v.to_string()
            };
            matrix_numbers.push(text);
            if stack.len() > 48 {
                return Err(Error::Malformed);
            }
            continue;
        }
        at += 1;
        let key = if b == 12 {
            let k = *data.get(at).ok_or(())?;
            at += 1;
            1200 + u16::from(k)
        } else {
            u16::from(b)
        };
        let arity = if private {
            match key {
                6..=9 | 1212 | 1213 => None,
                10 | 11 | 19..=21 | 1209..=1211 | 1214 | 1217..=1219 => Some(1),
                _ => return Err(Error::Malformed),
            }
        } else {
            match key {
                5 => Some(4),
                18 => Some(2),
                1207 => Some(6),
                1230 => Some(3),
                14 | 1223 => None,
                0..=4 | 13 | 15..=17 | 1200..=1206 | 1208 | 1220..=1222 | 1231..=1238 => Some(1),
                _ => return Err(Error::Malformed),
            }
        };
        if arity.is_some_and(|n| n != stack.len()) || stack.iter().any(|v| !v.is_finite()) {
            return Err(Error::Malformed);
        }
        let integer_keys = if private {
            matches!(key, 19 | 1211 | 1214 | 1217 | 1219)
        } else {
            matches!(key, 0..=4 | 13..=17 | 18 | 1200 | 1201 | 1206 | 1220..=1222 | 1230 | 1233..=1238)
        };
        if integer_keys && stack.iter().any(|v| v.fract() != 0.) {
            return Err(Error::Malformed);
        }
        if matches!((private, key), (false, 1201) | (true, 1214 | 1217))
            && !matches!(stack[0], 0. | 1.)
        {
            return Err(Error::Malformed);
        }
        if key == 1207 {
            result.matrix_texts = Some(matrix_numbers.clone().try_into().map_err(|_| ())?);
            result.matrix = Some((
                matrix_numbers
                    .iter()
                    .map(|p| Interval::decimal(p, 0).ok_or(()))
                    .collect::<std::result::Result<Vec<_>, ()>>()?
                    .try_into()
                    .map_err(|_| ())?,
                matrix_numbers
                    .iter()
                    .map(|p| Interval::decimal(p, 3).ok_or(()))
                    .collect::<std::result::Result<Vec<_>, ()>>()?
                    .try_into()
                    .map_err(|_| ())?,
            ));
        }
        matrix_numbers.clear();
        result.insert(key, std::mem::take(&mut stack));
    }
    if !stack.is_empty() {
        return Err(Error::Malformed);
    }
    Ok(result)
}
fn private<'a>(data: &'a [u8], fd: &Dict) -> Result<(Dict, Vec<&'a [u8]>)> {
    let Some(v) = fd.get(&18) else {
        return Ok((Dict::new(), Vec::new()));
    };
    let size = integer(v[0])?;
    let at = integer(v[1])?;
    let p = dict(
        data.get(at..at.checked_add(size).ok_or(())?).ok_or(())?,
        true,
    )?;
    let mut largest_zone = 0f64;
    let fuzz = value(&p, 1211, 1.);
    if fuzz < 0. || fuzz.fract() != 0. {
        return Err(Error::Malformed);
    }
    for pair_keys in [[6, 7], [8, 9]] {
        let mut zones = Vec::new();
        for key in pair_keys {
            if let Some(v) = p.get(&key) {
                let limit = if key == 6 || key == 8 { 14 } else { 10 };
                if v.len() % 2 != 0 || v.len() > limit || v.iter().any(|v| v.fract() != 0.) {
                    return Err(Error::Malformed);
                }
                let mut pos = 0.;
                for pair in v.as_chunks::<2>().0 {
                    pos += pair[0];
                    if pair[1] < 0. {
                        return Err(Error::Malformed);
                    }
                    zones.push((pos, pos + pair[1]));
                    largest_zone = largest_zone.max(pair[1]);
                    pos += pair[1];
                }
                if !pos.is_finite() {
                    return Err(Error::Malformed);
                }
            }
        }
        zones.sort_by(|a, b| a.0.total_cmp(&b.0));
        if zones.windows(2).any(|w| w[1].0 - w[0].1 < 2. * fuzz + 1.) {
            return Err(Error::Malformed);
        }
    }
    if value(&p, 1209, 0.039_625) <= 0. || value(&p, 1209, 0.039_625) * largest_zone >= 1. {
        return Err(Error::Malformed);
    }
    for key in [10, 11] {
        if p.get(&key).is_some_and(|v| v[0] < 0.) {
            return Err(Error::Malformed);
        }
    }
    for key in [1212, 1213] {
        if let Some(v) = p.get(&key)
            && (v.len() > 12
                || v.iter()
                    .enumerate()
                    .any(|(i, n)| if i == 0 { *n < 0. } else { *n <= 0. }))
        {
            return Err(Error::Malformed);
        }
        if let Some(v) = p.get(&key) {
            let widths = v
                .iter()
                .scan(0., |sum, v| {
                    *sum += v;
                    Some(*sum)
                })
                .collect::<Vec<_>>();
            let standard = if key == 1212 { 10 } else { 11 };
            if p.get(&standard).is_some_and(|s| !widths.contains(&s[0])) {
                return Err(Error::Malformed);
            }
        }
    }
    let local = if p.contains_key(&19) {
        if offset(&p, 19)? < size {
            return Err(Error::Malformed);
        }
        index(data, at.checked_add(offset(&p, 19)?).ok_or(())?)?.0
    } else {
        Vec::new()
    };
    Ok((p, local))
}
fn fdselect(data: &[u8], at: usize, n: usize, fds: usize) -> Result<Vec<usize>> {
    let mut result = vec![usize::MAX; n];
    match *data.get(at).ok_or(())? {
        0 => {
            for (gid, v) in data.get(at + 1..at + 1 + n).ok_or(())?.iter().enumerate() {
                result[gid] = usize::from(*v);
            }
        }
        3 => {
            let count = usize::from(u16::from_be_bytes(
                data.get(at + 1..at + 3)
                    .ok_or(())?
                    .try_into()
                    .map_err(|_| ())?,
            ));
            if count == 0 {
                return Err(Error::Malformed);
            }
            let mut first = 0;
            for i in 0..count {
                let r = at + 3 + i * 3;
                let start = usize::from(u16::from_be_bytes(
                    data.get(r..r + 2).ok_or(())?.try_into().map_err(|_| ())?,
                ));
                let fd = usize::from(*data.get(r + 2).ok_or(())?);
                let end = usize::from(u16::from_be_bytes(
                    data.get(r + 3..r + 5)
                        .ok_or(())?
                        .try_into()
                        .map_err(|_| ())?,
                ));
                if start != first || start >= end || end > n {
                    return Err(Error::Malformed);
                }
                result[start..end].fill(fd);
                first = end;
            }
            if first != n {
                return Err(Error::Malformed);
            }
        }
        _ => return Err(Error::Malformed),
    }
    if result.iter().any(|v| *v >= fds) {
        return Err(Error::Malformed);
    }
    Ok(result)
}
#[allow(clippy::struct_excessive_bools)]
struct Scanner<'a> {
    stack: Vec<i128>,
    local: &'a [&'a [u8]],
    global: &'a [&'a [u8]],
    width: Option<i128>,
    seen: bool,
    path: bool,
    stems: Vec<(bool, f64, f64)>,
    mask_seen: bool,
    active_mask: bool,
    vertical_seen: bool,
    normal_stems: bool,
    point: [i128; 2],
    outline: Option<Outline>,
}
impl<'a> Scanner<'a> {
    fn new(local: &'a [&'a [u8]], global: &'a [&'a [u8]], outline: Option<Outline>) -> Self {
        Self {
            stack: Vec::new(),
            local,
            global,
            width: None,
            seen: false,
            path: false,
            stems: Vec::new(),
            mask_seen: false,
            active_mask: false,
            vertical_seen: false,
            normal_stems: false,
            point: [0, 0],
            outline,
        }
    }

    fn delta(&mut self, dx: i128, dy: i128) -> Result<Point> {
        self.point[0] = self.point[0].checked_add(dx).ok_or(Error::ResourceLimit)?;
        self.point[1] = self.point[1].checked_add(dy).ok_or(Error::ResourceLimit)?;
        Ok(self.point.map(coordinate))
    }
    fn line(&mut self, dx: i128, dy: i128) -> Result<()> {
        let end = self.delta(dx, dy)?;
        if let Some(o) = &mut self.outline {
            o.line_enclosed(end, self.point.map(Interval::fixed));
        }
        Ok(())
    }
    fn curve(&mut self, a: [i128; 2], b: [i128; 2], c: [i128; 2]) -> Result<()> {
        let a = self.delta(a[0], a[1])?;
        let ai = self.point.map(Interval::fixed);
        let b = self.delta(b[0], b[1])?;
        let bi = self.point.map(Interval::fixed);
        let c = self.delta(c[0], c[1])?;
        let ci = self.point.map(Interval::fixed);
        if let Some(o) = &mut self.outline {
            o.cubic_enclosed(a, b, c, ai, bi, ci);
        }
        Ok(())
    }
    /// Expand only the path operators the existing subsetter admits.
    /// Type2 16.16 operands stay exact through relative additions, including
    /// cancellation. Checked i128 overflow is a resource failure. Existing
    /// process time/memory budgets bound expansion operationally; source byte
    /// size and subroutine depth alone do not prove a finite expansion bound.
    /// Arithmetic/storage/random/conditional operators remain unsupported.
    fn draw(&mut self, operator: u8, args: &[i128]) -> Result<()> {
        match operator {
            5 => {
                for pair in args.as_chunks::<2>().0 {
                    self.line(pair[0], pair[1])?;
                }
            }
            6 | 7 => {
                let mut horizontal = operator == 6;
                for v in args {
                    self.line(
                        if horizontal { *v } else { 0 },
                        if horizontal { 0 } else { *v },
                    )?;
                    horizontal = !horizontal;
                }
            }
            8 => {
                for c in args.as_chunks::<6>().0 {
                    self.curve([c[0], c[1]], [c[2], c[3]], [c[4], c[5]])?;
                }
            }
            24 => {
                let (curves, line) = args.split_at(args.len() - 2);
                self.draw(8, curves)?;
                self.line(line[0], line[1])?;
            }
            25 => {
                let (lines, curve) = args.split_at(args.len() - 6);
                self.draw(5, lines)?;
                self.draw(8, curve)?;
            }
            26 | 27 => {
                let odd = args.len() % 4 == 1;
                let mut first = if odd { args[0] } else { 0 };
                for c in args[usize::from(odd)..].as_chunks::<4>().0 {
                    if operator == 26 {
                        self.curve([first, c[0]], [c[1], c[2]], [0, c[3]])?;
                    } else {
                        self.curve([c[0], first], [c[1], c[2]], [c[3], 0])?;
                    }
                    first = 0;
                }
            }
            30 | 31 => {
                let mut horizontal = operator == 31;
                let mut at = 0;
                while at < args.len() {
                    let c = &args[at..];
                    let last = if c.len() == 5 { c[4] } else { 0 };
                    if horizontal {
                        self.curve([c[0], 0], [c[1], c[2]], [last, c[3]])?;
                    } else {
                        self.curve([0, c[0]], [c[1], c[2]], [c[3], last])?;
                    }
                    at += if c.len() == 5 { 5 } else { 4 };
                    horizontal = !horizontal;
                }
            }
            _ => unreachable!("validated path operator"),
        }
        Ok(())
    }
    fn flex(&mut self, operator: u8, a: &[i128]) -> Result<()> {
        match operator {
            34 => {
                self.curve([a[0], 0], [a[1], a[2]], [a[3], 0])?;
                self.curve([a[4], 0], [a[5], -a[2]], [a[6], 0])?;
            }
            35 => {
                self.curve([a[0], a[1]], [a[2], a[3]], [a[4], a[5]])?;
                self.curve([a[6], a[7]], [a[8], a[9]], [a[10], a[11]])?;
            }
            36 => {
                self.curve([a[0], a[1]], [a[2], a[3]], [a[4], 0])?;
                self.curve([a[5], 0], [a[6], a[7]], [a[8], -(a[1] + a[3] + a[7])])?;
            }
            37 => {
                let dx = a[0] + a[2] + a[4] + a[6] + a[8];
                let dy = a[1] + a[3] + a[5] + a[7] + a[9];
                let end = if dx.abs() > dy.abs() {
                    [a[10], -dy]
                } else {
                    [-dx, a[10]]
                };
                self.curve([a[0], a[1]], [a[2], a[3]], [a[4], a[5]])?;
                self.curve([a[6], a[7]], [a[8], a[9]], end)?;
            }
            _ => unreachable!("validated flex operator"),
        }
        Ok(())
    }
    fn no_overlap(&self, mask: Option<&[u8]>) -> Result<()> {
        for vertical in [false, true] {
            let mut intervals = self
                .stems
                .iter()
                .enumerate()
                .filter(|(i, s)| {
                    s.0 == vertical && mask.is_none_or(|m| m[*i / 8] & (128 >> (*i % 8)) != 0)
                })
                .map(|(_, s)| (s.1, s.2))
                .collect::<Vec<_>>();
            intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
            if intervals.windows(2).any(|w| w[0].1 > w[1].0) {
                return Err(Error::Malformed);
            }
        }
        Ok(())
    }
    #[allow(clippy::too_many_lines)]
    fn run(&mut self, data: &[u8], depth: usize, sub: bool) -> Result<bool> {
        if depth > 10 || data.len() > 65535 {
            return Err(Error::Malformed);
        }
        let mut at = 0;
        while at < data.len() {
            let b = data[at];
            if b == 28 || b >= 32 {
                self.stack.push(operand(number(data, &mut at, false)?));
                if self.stack.len() > 48 {
                    return Err(Error::Malformed);
                }
                continue;
            }
            at += 1;
            if b == 10 || b == 29 {
                let v = self.stack.pop().ok_or(())?;
                let list = if b == 10 { self.local } else { self.global };
                let bias = if list.len() < 1240 {
                    107.
                } else if list.len() < 33900 {
                    1131.
                } else {
                    32768.
                };
                let ix = integer(coordinate(v) + bias)?;
                let ended = self.run(list.get(ix).ok_or(())?, depth + 1, true)?;
                if ended {
                    if at != data.len() {
                        return Err(Error::Malformed);
                    }
                    return Ok(true);
                }
                continue;
            }
            if b == 11 {
                if !sub || at != data.len() {
                    return Err(Error::Malformed);
                }
                return Ok(false);
            }
            if !self.seen && matches!(b, 1 | 3 | 18 | 23 | 19 | 20 | 4 | 21 | 22 | 14) {
                let n = self.stack.len();
                let extra = match b {
                    1 | 3 | 18 | 23 | 19 | 20 => n % 2 == 1,
                    21 => n == 3,
                    4 | 22 => n == 2,
                    14 => n == 1,
                    _ => false,
                };
                if extra {
                    self.width = Some(self.stack.remove(0));
                }
                self.seen = true;
            }
            let n = self.stack.len();
            match b {
                1 | 3 | 18 | 23 | 19 | 20 => {
                    if !n.is_multiple_of(2) || self.path && (b != 19 && b != 20) {
                        return Err(Error::Malformed);
                    }
                    if (n == 0 || self.mask_seen) && !matches!(b, 19 | 20) {
                        return Err(Error::Malformed);
                    }
                    let vertical = matches!(b, 3 | 23 | 19 | 20);
                    if !vertical && self.vertical_seen {
                        return Err(Error::Malformed);
                    }
                    if vertical && n > 0 {
                        self.vertical_seen = true;
                    }
                    self.normal_stems |= matches!(b, 1 | 3);
                    let mut pos = 0.;
                    for pair in self.stack.as_chunks::<2>().0 {
                        pos += coordinate(pair[0]);
                        let w = coordinate(pair[1]);
                        if w < 0. && w != -20. && w != -21. {
                            return Err(Error::Malformed);
                        }
                        let (a, z) = if w == -20. {
                            (pos + w, pos + w)
                        } else if w == -21. {
                            (pos, pos)
                        } else {
                            (pos, pos + w)
                        };
                        self.stems.push((vertical, a, z));
                        pos += w;
                    }
                    let hint_count = self.stems.len();
                    if hint_count > 96 {
                        return Err(Error::Malformed);
                    }
                    if b == 19 || b == 20 {
                        let bytes = hint_count.div_ceil(8);
                        let mask = data.get(at..at + bytes).ok_or(())?;
                        self.mask_seen = true;
                        if b == 19 && self.normal_stems {
                            return Err(Error::Malformed);
                        }
                        if let Some(last) = mask.last()
                            && !hint_count.is_multiple_of(8)
                            && last & ((1 << (8 - hint_count % 8)) - 1) != 0
                        {
                            return Err(Error::Malformed);
                        }
                        self.no_overlap(Some(mask))?;
                        if b == 19 {
                            self.active_mask = true;
                        }
                        at += bytes;
                    }
                }
                4 | 21 | 22 => {
                    if n != if b == 21 { 2 } else { 1 } {
                        return Err(Error::Malformed);
                    }
                    self.path = true;
                    let (dx, dy) = match b {
                        4 => (0, self.stack[0]),
                        22 => (self.stack[0], 0),
                        _ => (self.stack[0], self.stack[1]),
                    };
                    let point = self.delta(dx, dy)?;
                    if let Some(o) = &mut self.outline {
                        o.finish();
                        o.mov_enclosed(point, self.point.map(Interval::fixed));
                    }
                    if !self.active_mask {
                        self.no_overlap(None)?;
                    }
                }
                14 => {
                    if n != 0 || at != data.len() {
                        return Err(Error::Malformed);
                    }
                    if !self.active_mask {
                        self.no_overlap(None)?;
                    }
                    if let Some(o) = &mut self.outline {
                        o.finish();
                    }
                    return Ok(true);
                }
                5 | 6 | 7 | 8 | 24 | 25 | 26 | 27 | 30 | 31 => {
                    if !self.path {
                        return Err(Error::Malformed);
                    }
                    let valid = match b {
                        5 => n >= 2 && n.is_multiple_of(2),
                        6 | 7 => n >= 1,
                        8 => n >= 6 && n.is_multiple_of(6),
                        24 => n >= 8 && (n - 2).is_multiple_of(6),
                        25 => n >= 8 && (n - 6).is_multiple_of(2),
                        _ => n >= 4 && (n.is_multiple_of(4) || n % 4 == 1),
                    };
                    if !valid {
                        return Err(Error::Malformed);
                    }
                    self.draw(b, &self.stack.clone())?;
                }
                12 => {
                    let op = *data.get(at).ok_or(())?;
                    at += 1;
                    let need = match op {
                        34 => 7,
                        35 => 13,
                        36 => 9,
                        37 => 11,
                        _ => return Err(Error::Malformed),
                    };
                    if !self.path || n != need {
                        return Err(Error::Malformed);
                    }
                    self.flex(op, &self.stack.clone())?;
                }
                _ => return Err(Error::Malformed),
            }
            self.stack.clear();
        }
        Err(Error::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn relative_coordinates_remain_exact_through_large_cancellation() {
        let mut scan = Scanner::new(&[], &[], None);
        let large = 1_i128 << 60;
        scan.delta(large, -large).unwrap();
        scan.delta(1, -1).unwrap();
        let point = scan.delta(-large, large).unwrap();
        assert_eq!(scan.point, [1, -1]);
        assert_eq!(point, [1. / 65536., -1. / 65536.]);
    }
    #[test]
    fn exact_coordinate_overflow_is_an_explicit_resource_failure() {
        let mut scan = Scanner::new(&[], &[], None);
        scan.point = [i128::MAX, 0];
        assert!(matches!(scan.delta(1, 0), Err(Error::ResourceLimit)));
    }
}
