//! f64 复数：基本运算 / abs / arg / conj。零外部依赖，行为与 RoomForge ComplexMath 的纯算术部分一致。

use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign};

/// A complex number with f64 parts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Complex {
    pub re: f64,
    pub im: f64,
}

impl Complex {
    pub const ZERO: Complex = Complex { re: 0.0, im: 0.0 };
    pub const ONE: Complex = Complex { re: 1.0, im: 0.0 };

    pub const fn new(re: f64, im: f64) -> Complex {
        Complex { re, im }
    }

    /// magnitude * (cos(phase) + j·sin(phase)).
    pub fn from_polar(magnitude: f64, phase_rad: f64) -> Complex {
        Complex {
            re: magnitude * phase_rad.cos(),
            im: magnitude * phase_rad.sin(),
        }
    }

    /// |z| = hypot(re, im)（不溢出，不丢精度）。
    pub fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }

    /// |z|² = re² + im²。
    pub fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }

    /// atan2(im, re)，返回 (-π, π]。
    pub fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }

    pub fn conj(self) -> Complex {
        Complex {
            re: self.re,
            im: -self.im,
        }
    }

    pub fn scale(self, factor: f64) -> Complex {
        Complex {
            re: self.re * factor,
            im: self.im * factor,
        }
    }

    pub fn is_finite(self) -> bool {
        self.re.is_finite() && self.im.is_finite()
    }
}

impl From<f64> for Complex {
    fn from(value: f64) -> Complex {
        Complex { re: value, im: 0.0 }
    }
}

impl Add for Complex {
    type Output = Complex;
    fn add(self, rhs: Complex) -> Complex {
        Complex::new(self.re + rhs.re, self.im + rhs.im)
    }
}

impl AddAssign for Complex {
    fn add_assign(&mut self, rhs: Complex) {
        self.re += rhs.re;
        self.im += rhs.im;
    }
}

impl Sub for Complex {
    type Output = Complex;
    fn sub(self, rhs: Complex) -> Complex {
        Complex::new(self.re - rhs.re, self.im - rhs.im)
    }
}

impl SubAssign for Complex {
    fn sub_assign(&mut self, rhs: Complex) {
        self.re -= rhs.re;
        self.im -= rhs.im;
    }
}

impl Neg for Complex {
    type Output = Complex;
    fn neg(self) -> Complex {
        Complex::new(-self.re, -self.im)
    }
}

impl Mul for Complex {
    type Output = Complex;
    fn mul(self, rhs: Complex) -> Complex {
        Complex::new(
            self.re * rhs.re - self.im * rhs.im,
            self.re * rhs.im + self.im * rhs.re,
        )
    }
}

impl MulAssign for Complex {
    fn mul_assign(&mut self, rhs: Complex) {
        *self = *self * rhs;
    }
}

impl Mul<f64> for Complex {
    type Output = Complex;
    fn mul(self, rhs: f64) -> Complex {
        self.scale(rhs)
    }
}

impl Mul<Complex> for f64 {
    type Output = Complex;
    fn mul(self, rhs: Complex) -> Complex {
        rhs.scale(self)
    }
}

impl Div for Complex {
    type Output = Complex;
    fn div(self, rhs: Complex) -> Complex {
        let den = rhs.norm_sqr();
        Complex::new(
            (self.re * rhs.re + self.im * rhs.im) / den,
            (self.im * rhs.re - self.re * rhs.im) / den,
        )
    }
}

impl DivAssign for Complex {
    fn div_assign(&mut self, rhs: Complex) {
        *self = *self / rhs;
    }
}

impl Div<f64> for Complex {
    type Output = Complex;
    fn div(self, rhs: f64) -> Complex {
        Complex::new(self.re / rhs, self.im / rhs)
    }
}

impl Sum for Complex {
    fn sum<I: Iterator<Item = Complex>>(iter: I) -> Complex {
        iter.fold(Complex::ZERO, |acc, value| acc + value)
    }
}

impl fmt::Display for Complex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{:+}j", self.re, self.im)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn arithmetic_matches_hand_calculation() {
        let a = Complex::new(3.0, 4.0);
        let b = Complex::new(1.0, -2.0);
        assert_eq!(a + b, Complex::new(4.0, 2.0));
        assert_eq!(a - b, Complex::new(2.0, 6.0));
        assert_eq!(a * b, Complex::new(11.0, -2.0));
        let q = a / b;
        assert!(close(q.re, -1.0, 1e-15));
        assert!(close(q.im, 2.0, 1e-15));
        assert_eq!(-a, Complex::new(-3.0, -4.0));
    }

    #[test]
    fn abs_arg_conj() {
        let z = Complex::new(3.0, 4.0);
        assert!(close(z.abs(), 5.0, 0.0));
        assert!(close(z.norm_sqr(), 25.0, 0.0));
        assert!(close(z.arg(), 0.9272952180016122, 1e-15));
        assert_eq!(z.conj(), Complex::new(3.0, -4.0));

        let one = Complex::ONE;
        assert!(close(one.arg(), 0.0, 0.0));
        let quarter = Complex::new(0.0, 1.0);
        assert!(close(quarter.arg(), std::f64::consts::FRAC_PI_2, 0.0));
    }

    #[test]
    fn polar_roundtrip() {
        let z = Complex::from_polar(2.5, std::f64::consts::FRAC_PI_3);
        assert!(close(z.abs(), 2.5, 1e-15));
        assert!(close(z.arg(), std::f64::consts::FRAC_PI_3, 1e-15));
        assert!(close(z.re, 1.25, 1e-15));
    }

    #[test]
    fn scalar_ops_and_sum() {
        assert_eq!(Complex::new(1.0, 2.0) * 2.0, Complex::new(2.0, 4.0));
        assert_eq!(2.0 * Complex::new(1.0, 2.0), Complex::new(2.0, 4.0));
        assert_eq!(Complex::new(4.0, 8.0) / 2.0, Complex::new(2.0, 4.0));
        let total: Complex = [Complex::new(1.0, 1.0), Complex::new(2.0, -3.0)]
            .into_iter()
            .sum();
        assert_eq!(total, Complex::new(3.0, -2.0));
        assert!(Complex::from(7.0).is_finite());
        assert!(!Complex::new(f64::NAN, 0.0).is_finite());
    }
}
