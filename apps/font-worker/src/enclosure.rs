#![allow(clippy::float_cmp, clippy::many_single_char_names)]
use std::cmp::Ordering;
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Interval {
    pub lo: f64,
    pub hi: f64,
}
pub type Point = [Interval; 2];
pub type Matrix = [Interval; 6];
impl Interval {
    pub const fn exact(v: f64) -> Self {
        Self { lo: v, hi: v }
    }
    pub fn add(self, b: Self) -> Self {
        fn directed(a: f64, b: f64, up: bool) -> f64 {
            let s = a + b;
            let z = s - a;
            let error = (a - (s - z)) + (b - z);
            if error > 0. && up {
                s.next_up()
            } else if error < 0. && !up {
                s.next_down()
            } else {
                s
            }
        }
        Self {
            lo: directed(self.lo, b.lo, false),
            hi: directed(self.hi, b.hi, true),
        }
    }
    pub fn neg(self) -> Self {
        Self {
            lo: -self.hi,
            hi: -self.lo,
        }
    }
    pub fn sub(self, b: Self) -> Self {
        self.add(b.neg())
    }
    pub fn mul(self, b: Self) -> Self {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for a in [self.lo, self.hi] {
            for b in [b.lo, b.hi] {
                let p = a * b;
                let error = a.mul_add(b, -p);
                // For a subnormal product, the residual itself can underflow.
                let underflow =
                    a != 0. && b != 0. && p.abs() < f64::MIN_POSITIVE * 9_007_199_254_740_992.;
                lo = lo.min(if error < 0. || underflow {
                    p.next_down()
                } else {
                    p
                });
                hi = hi.max(if error > 0. || underflow {
                    p.next_up()
                } else {
                    p
                });
            }
        }
        Self { lo, hi }
    }
    pub fn quotient(a: f64, b: f64) -> Self {
        let v = a / b;
        let residual = (-v).mul_add(b, a);
        Self {
            lo: if residual < 0. { v.next_down() } else { v },
            hi: if residual > 0. { v.next_up() } else { v },
        }
    }
    pub fn half(self) -> Self {
        self.mul(Self::exact(0.5))
    }
    pub fn midpoint(self, b: Self) -> Self {
        self.half().add(b.half())
    }
    pub fn decimal(s: &str, power: i32) -> Option<Self> {
        let d = Decimal::parse(s, power)?;
        let v = format!(
            "{}e{}",
            if d.negative {
                format!("-{}", d.digits)
            } else {
                d.digits.clone()
            },
            d.power
        )
        .parse::<f64>()
        .ok()?;
        if !v.is_finite() {
            return None;
        }
        let exact = Decimal::binary(v);
        Some(match exact.cmp(&d) {
            Ordering::Less => Self {
                lo: v,
                hi: v.next_up(),
            },
            Ordering::Greater => Self {
                lo: v.next_down(),
                hi: v,
            },
            Ordering::Equal => Self::exact(v),
        })
    }
    #[allow(clippy::cast_precision_loss)]
    pub fn fixed(v: i128) -> Self {
        let n = v as f64;
        if v.unsigned_abs() <= (1_u128 << 53) {
            return Self::exact(n / 65536.);
        }
        // Comparing canonical exact decimals also handles casts at i128's edge.
        let exact = Decimal::parse(&v.to_string(), 0).expect("integer decimal");
        let parsed = Decimal::binary(n);
        let r = match parsed.cmp(&exact) {
            Ordering::Less => Self {
                lo: n,
                hi: n.next_up(),
            },
            Ordering::Greater => Self {
                lo: n.next_down(),
                hi: n,
            },
            Ordering::Equal => Self::exact(n),
        };
        r.mul(Self::exact(1. / 65536.))
    }
}
pub fn transform(m: Matrix, p: Point) -> Point {
    [
        m[0].mul(p[0]).add(m[2].mul(p[1])).add(m[4]),
        m[1].mul(p[0]).add(m[3].mul(p[1])).add(m[5]),
    ]
}
pub fn concat(a: Matrix, b: Matrix) -> Matrix {
    [
        a[0].mul(b[0]).add(a[2].mul(b[1])),
        a[1].mul(b[0]).add(a[3].mul(b[1])),
        a[0].mul(b[2]).add(a[2].mul(b[3])),
        a[1].mul(b[2]).add(a[3].mul(b[3])),
        a[0].mul(b[4]).add(a[2].mul(b[5])).add(a[4]),
        a[1].mul(b[4]).add(a[3].mul(b[5])).add(a[5]),
    ]
}
#[derive(Debug)]
struct Decimal {
    negative: bool,
    digits: String,
    power: i32,
}
impl Decimal {
    fn parse(s: &str, power: i32) -> Option<Self> {
        let (mantissa, exponent) = s.split_once(['E', 'e']).map_or((s, 0), |(a, b)| {
            (
                a,
                b.parse::<i32>().ok().unwrap_or(if b.starts_with('-') {
                    i32::MIN
                } else {
                    i32::MAX
                }),
            )
        });
        let negative = mantissa.starts_with('-');
        let mantissa = mantissa.trim_start_matches(['-', '+']);
        let fractional = mantissa.split_once('.').map_or(0, |(_, b)| b.len());
        let mut digits = mantissa.replace('.', "").trim_start_matches('0').to_owned();
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut power = power
            .saturating_add(exponent)
            .saturating_sub(i32::try_from(fractional).ok()?);
        while digits.ends_with('0') {
            digits.pop();
            power = power.saturating_add(1);
        }
        if digits.is_empty() {
            return Some(Self {
                negative: false,
                digits: "0".into(),
                power: 0,
            });
        }
        Some(Self {
            negative,
            digits,
            power,
        })
    }
    fn binary(v: f64) -> Self {
        if v == 0. {
            return Self::parse("0", 0).unwrap();
        }
        let bits = v.to_bits();
        let exponent = ((bits >> 52) & 2047) as i32;
        let mantissa = (bits & ((1_u64 << 52) - 1)) | if exponent == 0 { 0 } else { 1_u64 << 52 };
        let exp = if exponent == 0 {
            -1074
        } else {
            exponent - 1023 - 52
        };
        let mut digits = mantissa
            .to_string()
            .bytes()
            .rev()
            .map(|b| b - b'0')
            .collect::<Vec<_>>();
        let factor = if exp < 0 { 5 } else { 2 };
        for _ in 0..exp.unsigned_abs() {
            let mut carry = 0_u16;
            for d in &mut digits {
                let n = u16::from(*d) * factor + carry;
                *d = u8::try_from(n % 10).unwrap();
                carry = n / 10;
            }
            while carry > 0 {
                digits.push(u8::try_from(carry % 10).unwrap());
                carry /= 10;
            }
        }
        let s = digits
            .into_iter()
            .rev()
            .map(|b| char::from(b + b'0'))
            .collect::<String>();
        Self::parse(&format!("{}{s}", if v < 0. { "-" } else { "" }), exp.min(0)).unwrap()
    }
    fn cmp(&self, b: &Self) -> Ordering {
        if self.negative != b.negative {
            return self.negative.cmp(&b.negative).reverse();
        }
        let order = if self.digits == "0" || b.digits == "0" {
            (self.digits != "0").cmp(&(b.digits != "0"))
        } else {
            (i64::try_from(self.digits.len()).unwrap() + i64::from(self.power))
                .cmp(&(i64::try_from(b.digits.len()).unwrap() + i64::from(b.power)))
                .then_with(|| {
                    self.digits
                        .bytes()
                        .chain(std::iter::repeat(b'0'))
                        .take(self.digits.len().max(b.digits.len()))
                        .cmp(
                            b.digits
                                .bytes()
                                .chain(std::iter::repeat(b'0'))
                                .take(self.digits.len().max(b.digits.len())),
                        )
                })
        };
        if self.negative {
            order.reverse()
        } else {
            order
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_neighbors_and_decimal_directions() {
        assert_eq!(Interval::decimal(".001", 3).unwrap(), Interval::exact(1.));
        assert_eq!(Interval::decimal("0", 3).unwrap(), Interval::exact(0.));
        for text in ["1E-18", "-1E-18"] {
            let d = Interval::decimal(text, 3).unwrap();
            let translated = Interval::exact(900.).add(d);
            assert!(translated.lo <= 900. && translated.hi >= 900.);
            if text.starts_with('-') {
                assert_eq!(translated.lo.floor(), 899.);
            } else {
                assert_eq!(translated.hi.ceil(), 901.);
            }
        }
        assert_eq!(
            Interval::exact(900.).add(Interval::exact(0.)),
            Interval::exact(900.)
        );
    }
}
