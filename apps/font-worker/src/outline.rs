#![allow(clippy::many_single_char_names)]
use super::enclosure::{self, Interval};
use ttf_parser::OutlineBuilder;

pub type Point = [f64; 2];
pub type Matrix = [f64; 6];
pub const IDENTITY: Matrix = [1., 0., 0., 1., 0., 0.];

pub fn transform(m: Matrix, p: Point) -> Point {
    [
        m[0] * p[0] + m[2] * p[1] + m[4],
        m[1] * p[0] + m[3] * p[1] + m[5],
    ]
}
pub fn concat(a: Matrix, b: Matrix) -> Matrix {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

#[derive(Default)]
pub struct Outline {
    pub bounds: Option<[f64; 4]>,
    pub segments: Vec<(Point, Point)>,
    point: Point,
    start: Point,
    pub invalid: bool,
    pub matrix: Matrix,
    pub certified_bounds: Option<[f64; 4]>,
    /// Lower bound on an attained path height, including the zero baseline.
    /// Interior control points are not necessarily attained curve points.
    pub attained_height_lower_bound: f64,
    certified_matrix: enclosure::Matrix,
    certified_point: enclosure::Point,
    certified_start: enclosure::Point,
}
impl Outline {
    #[cfg(test)]
    pub fn new(matrix: Matrix) -> Self {
        Self::enclosed(matrix, matrix.map(Interval::exact))
    }
    pub fn enclosed(matrix: Matrix, certified_matrix: enclosure::Matrix) -> Self {
        Self {
            matrix,
            certified_matrix,
            ..Self::default()
        }
    }
    fn certify(&mut self, points: &[enclosure::Point]) {
        // The analytic extrema above remain the ideal geometry measurement.
        // A directed de Casteljau hull certifies its final representation.
        // Refine only hulls crossing that nominal whole-unit envelope; each
        // subdivision remains conservative even when floating resolution stops.
        if let (Some(first), Some(last)) = (points.first(), points.last()) {
            self.attained_height_lower_bound = self
                .attained_height_lower_bound
                .max(first[1].lo)
                .max(last[1].lo);
        }
        let target = self.bounds.unwrap_or([0.; 4]);
        let b = certified_curve(points, target, 0);
        self.invalid |= b.iter().any(|v| !v.is_finite());
        self.certified_bounds = Some(self.certified_bounds.map_or(b, |a| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        }));
    }
    fn include(&mut self, p: Point) {
        self.invalid |= p.iter().any(|v| !v.is_finite());
        self.bounds = Some(self.bounds.map_or([p[0], p[1], p[0], p[1]], |b| {
            [
                b[0].min(p[0]),
                b[1].min(p[1]),
                b[2].max(p[0]),
                b[3].max(p[1]),
            ]
        }));
    }
    pub fn mov(&mut self, p: Point) {
        self.mov_enclosed(p, p.map(Interval::exact));
    }
    pub fn mov_enclosed(&mut self, p: Point, enclosed: enclosure::Point) {
        self.certified_point = enclosure::transform(self.certified_matrix, enclosed);
        self.certified_start = self.certified_point;
        self.point = transform(self.matrix, p);
        self.start = self.point;
    }
    pub fn line(&mut self, p: Point) {
        self.line_enclosed(p, p.map(Interval::exact));
    }
    pub fn line_enclosed(&mut self, p: Point, enclosed: enclosure::Point) {
        let certified_end = enclosure::transform(self.certified_matrix, enclosed);
        let end = transform(self.matrix, p);
        self.include(self.point);
        self.include(end);
        self.certify(&[self.certified_point, certified_end]);
        self.certified_point = certified_end;
        self.segments.push((self.point, end));
        self.point = end;
    }
    pub fn quad(&mut self, c: Point, p: Point) {
        self.quad_enclosed(c, p, c.map(Interval::exact), p.map(Interval::exact));
    }
    pub fn quad_enclosed(
        &mut self,
        c: Point,
        p: Point,
        ci: enclosure::Point,
        pi: enclosure::Point,
    ) {
        let ci = enclosure::transform(self.certified_matrix, ci);
        let pi = enclosure::transform(self.certified_matrix, pi);
        let c = transform(self.matrix, c);
        let p = transform(self.matrix, p);
        let o = self.point;
        self.include(o);
        self.include(p);
        for axis in 0..2 {
            let den = o[axis] - 2. * c[axis] + p[axis];
            if den != 0. {
                let t = (o[axis] - c[axis]) / den;
                if t > 0. && t < 1. {
                    self.include(quad(o, c, p, t));
                }
            }
        }
        self.certify(&[self.certified_point, ci, pi]);
        self.certified_point = pi;
        for step in 1..=64 {
            let q = quad(o, c, p, f64::from(step) / 64.);
            self.segments.push((self.point, q));
            self.point = q;
        }
    }
    pub fn cubic(&mut self, a: Point, b: Point, p: Point) {
        self.cubic_enclosed(
            a,
            b,
            p,
            a.map(Interval::exact),
            b.map(Interval::exact),
            p.map(Interval::exact),
        );
    }
    pub fn cubic_enclosed(
        &mut self,
        a: Point,
        b: Point,
        p: Point,
        ai: enclosure::Point,
        bi: enclosure::Point,
        pi: enclosure::Point,
    ) {
        let ai = enclosure::transform(self.certified_matrix, ai);
        let bi = enclosure::transform(self.certified_matrix, bi);
        let pi = enclosure::transform(self.certified_matrix, pi);
        let a = transform(self.matrix, a);
        let b = transform(self.matrix, b);
        let p = transform(self.matrix, p);
        let o = self.point;
        self.include(o);
        self.include(p);
        for axis in 0..2 {
            let aa = -o[axis] + 3. * a[axis] - 3. * b[axis] + p[axis];
            let bb = 2. * (o[axis] - 2. * a[axis] + b[axis]);
            let cc = a[axis] - o[axis];
            let mut roots = [f64::NAN; 2];
            if aa == 0. {
                if bb != 0. {
                    roots[0] = -cc / bb;
                }
            } else {
                let d = bb * bb - 4. * aa * cc;
                if d >= 0. {
                    roots = [(-bb + d.sqrt()) / (2. * aa), (-bb - d.sqrt()) / (2. * aa)];
                }
            }
            for t in roots {
                if t > 0. && t < 1. {
                    self.include(cubic(o, a, b, p, t));
                }
            }
        }
        self.certify(&[self.certified_point, ai, bi, pi]);
        self.certified_point = pi;
        for step in 1..=64 {
            let q = cubic(o, a, b, p, f64::from(step) / 64.);
            self.segments.push((self.point, q));
            self.point = q;
        }
    }
    pub fn finish(&mut self) {
        if self.point.map(f64::to_bits) != self.start.map(f64::to_bits) {
            self.segments.push((self.point, self.start));
            self.point = self.start;
        }
        self.certified_point = self.certified_start;
    }
    #[expect(
        clippy::float_cmp,
        reason = "Only exactly coincident crossings share a winding event; nearby positions remain distinct"
    )]
    pub fn estimate_vertical_stem_width(&self) -> Option<f64> {
        let b = self.bounds?;
        if b[3] <= b[1] {
            return None;
        }
        let mut spans = Vec::new();
        for f in [0.25, 0.5, 0.75] {
            let y = b[1] + f * (b[3] - b[1]);
            let mut xs = self
                .segments
                .iter()
                .filter_map(|(a, b)| {
                    if (a[1] <= y && y < b[1]) || (b[1] <= y && y < a[1]) {
                        Some((
                            a[0] + (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]),
                            if b[1] > a[1] { 1 } else { -1 },
                        ))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            xs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut winding = 0;
            let mut start = 0.;
            // A shared edge is one event: contour ordering at that exact
            // position cannot split a continuous nonzero-filled interval.
            for crossings in xs.chunk_by(|a, b| a.0 == b.0) {
                let x = crossings[0].0;
                let was_filled = winding != 0;
                winding += crossings.iter().map(|crossing| crossing.1).sum::<i32>();
                if !was_filled && winding != 0 {
                    start = x;
                }
                if was_filled && winding == 0 && x > start {
                    spans.push(x - start);
                }
            }
        }
        let mid = spans.len() / 2;
        if spans.is_empty() {
            None
        } else {
            Some(*spans.select_nth_unstable_by(mid, f64::total_cmp).1)
        }
    }
}
fn hull(points: &[enclosure::Point]) -> [f64; 4] {
    points.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |a, p| {
            [
                a[0].min(p[0].lo),
                a[1].min(p[1].lo),
                a[2].max(p[0].hi),
                a[3].max(p[1].hi),
            ]
        },
    )
}
fn certified_curve(points: &[enclosure::Point], target: [f64; 4], depth: u8) -> [f64; 4] {
    let b = hull(points);
    if points.len() == 2
        || depth == 64
        || (b[0].floor() >= target[0].floor()
            && b[1].floor() >= target[1].floor()
            && b[2].ceil() <= target[2].ceil()
            && b[3].ceil() <= target[3].ceil())
    {
        return b;
    }
    let mut row = points.to_vec();
    let mut left = vec![row[0]];
    let mut right = vec![row[row.len() - 1]];
    while row.len() > 1 {
        row = row
            .windows(2)
            .map(|w| [w[0][0].midpoint(w[1][0]), w[0][1].midpoint(w[1][1])])
            .collect();
        left.push(row[0]);
        right.push(row[row.len() - 1]);
    }
    right.reverse();
    let lh = hull(&left);
    let rh = hull(&right);
    let combined = [
        lh[0].min(rh[0]),
        lh[1].min(rh[1]),
        lh[2].max(rh[2]),
        lh[3].max(rh[3]),
    ];
    if combined[0] <= b[0] && combined[1] <= b[1] && combined[2] >= b[2] && combined[3] >= b[3] {
        return b;
    }
    let a = certified_curve(&left, target, depth + 1);
    let b = certified_curve(&right, target, depth + 1);
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}
fn quad(o: Point, c: Point, p: Point, t: f64) -> Point {
    let u = 1. - t;
    [
        u * u * o[0] + 2. * u * t * c[0] + t * t * p[0],
        u * u * o[1] + 2. * u * t * c[1] + t * t * p[1],
    ]
}
fn cubic(o: Point, a: Point, b: Point, p: Point, t: f64) -> Point {
    let u = 1. - t;
    [
        u * u * u * o[0] + 3. * u * u * t * a[0] + 3. * u * t * t * b[0] + t * t * t * p[0],
        u * u * u * o[1] + 3. * u * u * t * a[1] + 3. * u * t * t * b[1] + t * t * t * p[1],
    ]
}
impl OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.mov([f64::from(x), f64::from(y)]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.line([f64::from(x), f64::from(y)]);
    }
    fn quad_to(&mut self, x: f32, y: f32, a: f32, b: f32) {
        self.quad([f64::from(x), f64::from(y)], [f64::from(a), f64::from(b)]);
    }
    fn curve_to(&mut self, x: f32, y: f32, a: f32, b: f32, c: f32, d: f32) {
        self.cubic(
            [f64::from(x), f64::from(y)],
            [f64::from(a), f64::from(b)],
            [f64::from(c), f64::from(d)],
        );
    }
    fn close(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transformed_curve_extrema_are_filled_bounds() {
        let mut o = Outline::new([1., 0.2, 0.3, 1., 40., 20.]);
        o.mov([0., 0.]);
        o.cubic([0., 100.], [100., 100.], [100., 0.]);
        let b = o.bounds.unwrap();
        assert_eq!(b[1].to_bits(), 20_f64.to_bits());
        assert!(b[3] > 105. && b[3] < 106.);
        assert!(b[2] > 140. && b[2] < 147.);
    }
    #[test]
    fn move_only_is_empty() {
        let mut o = Outline::new(IDENTITY);
        o.mov([10., 20.]);
        o.finish();
        assert!(o.bounds.is_none());
    }
    fn rectangle(outline: &mut Outline, left: f64, right: f64, opposite: bool) {
        let mut points = [[left, 0.], [right, 0.], [right, 100.], [left, 100.]];
        if opposite {
            points.reverse();
        }
        outline.mov(points[0]);
        for point in &points[1..] {
            outline.line(*point);
        }
        outline.line(points[0]);
        outline.finish();
    }
    #[test]
    fn touching_same_winding_contours_measure_one_filled_union_in_either_order() {
        // At every sampled height, nonzero fill covers the single interval
        // [0,200]. The shared edge at100 is not a boundary of that filled union.
        for reverse in [false, true] {
            let mut outline = Outline::new(IDENTITY);
            let ranges = if reverse {
                [(100., 200.), (0., 100.)]
            } else {
                [(0., 100.), (100., 200.)]
            };
            for (left, right) in ranges {
                rectangle(&mut outline, left, right, false);
            }
            assert_eq!(
                outline.estimate_vertical_stem_width().map(f64::to_bits),
                Some(200_f64.to_bits()),
                "contour order reverse={reverse}"
            );
        }
    }
    #[test]
    fn opposite_winding_cancellation_and_one_ulp_gap_keep_distinct_fill() {
        for reverse in [false, true] {
            let mut canceled = Outline::new(IDENTITY);
            rectangle(&mut canceled, 0., 100., reverse);
            rectangle(&mut canceled, 0., 100., !reverse);
            assert!(
                canceled.estimate_vertical_stem_width().is_none(),
                "identical opposite contours cancel"
            );
            let mut separated = Outline::new(IDENTITY);
            let ranges = if reverse {
                [(100_f64.next_up(), 200.), (0., 100.)]
            } else {
                [(0., 100.), (100_f64.next_up(), 200.)]
            };
            for (left, right) in ranges {
                rectangle(&mut separated, left, right, false);
            }
            assert_eq!(
                separated.estimate_vertical_stem_width().map(f64::to_bits),
                Some(100_f64.to_bits()),
                "a real one-ulp gap is not coalesced"
            );
        }
    }
}
