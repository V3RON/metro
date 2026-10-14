//! Typed 0-based and 1-based offsets.
//!
//! This is the Rust counterpart of the `ob1` package. In JavaScript `Number0`
//! and `Number1` are opaque types over `number`; here they are `i32`
//! newtypes, so mixing the two bases is a compile error rather than a Flow
//! error.
//!
//! The `ob1` package is deliberately not exposed through the napi binding:
//! each helper is a single integer operation, so a call across the boundary
//! would cost far more than the operation. Instead this crate is used by the
//! Rust source-map crate, where the same helpers appear in hot loops with no
//! boundary in the way.
//!
//! Arithmetic wraps on overflow, matching the 32-bit behaviour of the bit
//! operations used by source-map encoding; real positions are far below that.

use std::ops::{Add, Sub};

/// A 0-based offset (for example a column).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Number0(i32);

/// A 1-based offset (for example a line).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Number1(i32);

/// Cast a number to a 0-based offset (`add0` in ob1).
pub const fn add0(x: i32) -> Number0 {
    Number0(x)
}

/// Add 1 to a 0-based offset (or a plain number), converting it to 1-based
/// (`add1` in ob1).
pub const fn add1(x: i32) -> Number1 {
    Number1(x.wrapping_add(1))
}

/// Subtract 1 from a 1-based offset, converting it to 0-based (`sub1` in ob1).
pub const fn sub1(x: Number1) -> Number0 {
    Number0(x.0.wrapping_sub(1))
}

/// The underlying number of a 0-based offset (`get0` in ob1).
pub const fn get0(x: Number0) -> i32 {
    x.0
}

/// The underlying number of a 1-based offset (`get1` in ob1).
pub const fn get1(x: Number1) -> i32 {
    x.0
}

/// Negate a 0-based offset (`neg` in ob1).
pub const fn neg(x: Number0) -> Number0 {
    Number0(x.0.wrapping_neg())
}

impl Number0 {
    /// Increment (`inc` in ob1).
    pub const fn inc(self) -> Number0 {
        Number0(self.0.wrapping_add(1))
    }
}

impl Number1 {
    /// The first line.
    pub const FIRST: Number1 = Number1(1);

    /// Increment (`inc` in ob1).
    pub const fn inc(self) -> Number1 {
        Number1(self.0.wrapping_add(1))
    }
}

// add(Number1, number) -> Number1, add(Number0, number) -> Number0, ...
impl Add<i32> for Number0 {
    type Output = Number0;
    fn add(self, rhs: i32) -> Number0 {
        Number0(self.0.wrapping_add(rhs))
    }
}
impl Add<i32> for Number1 {
    type Output = Number1;
    fn add(self, rhs: i32) -> Number1 {
        Number1(self.0.wrapping_add(rhs))
    }
}
impl Add<Number0> for Number1 {
    type Output = Number1;
    fn add(self, rhs: Number0) -> Number1 {
        Number1(self.0.wrapping_add(rhs.0))
    }
}
impl Add<Number1> for Number0 {
    type Output = Number1;
    fn add(self, rhs: Number1) -> Number1 {
        Number1(self.0.wrapping_add(rhs.0))
    }
}
impl Add<Number0> for Number0 {
    type Output = Number0;
    fn add(self, rhs: Number0) -> Number0 {
        Number0(self.0.wrapping_add(rhs.0))
    }
}

// sub(Number1, number) -> Number1, sub(Number0, number) -> Number0, ...
impl Sub<i32> for Number0 {
    type Output = Number0;
    fn sub(self, rhs: i32) -> Number0 {
        Number0(self.0.wrapping_sub(rhs))
    }
}
impl Sub<i32> for Number1 {
    type Output = Number1;
    fn sub(self, rhs: i32) -> Number1 {
        Number1(self.0.wrapping_sub(rhs))
    }
}
impl Sub<Number0> for Number1 {
    type Output = Number1;
    fn sub(self, rhs: Number0) -> Number1 {
        Number1(self.0.wrapping_sub(rhs.0))
    }
}
impl Sub<Number0> for Number0 {
    type Output = Number0;
    fn sub(self, rhs: Number0) -> Number0 {
        Number0(self.0.wrapping_sub(rhs.0))
    }
}
impl Sub<Number1> for Number1 {
    type Output = Number0;
    fn sub(self, rhs: Number1) -> Number0 {
        Number0(self.0.wrapping_sub(rhs.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn casts_round_trip() {
        assert_eq!(get0(add0(7)), 7);
        assert_eq!(get1(add1(6)), 7);
        assert_eq!(get0(sub1(add1(6))), 6);
        assert_eq!(get0(neg(add0(5))), -5);
    }

    #[test]
    fn add_keeps_the_base_of_the_offset() {
        assert_eq!(get0(add0(2) + 3), 5);
        assert_eq!(get1(add1(1) + 3), 5);
        assert_eq!(get1(add1(1) + add0(3)), 5);
        assert_eq!(get1(add0(3) + add1(1)), 5);
        assert_eq!(get0(add0(3) + add0(4)), 7);
    }

    #[test]
    fn sub_follows_the_ob1_overloads() {
        assert_eq!(get1(add1(9) - 3), 7);
        assert_eq!(get0(add0(9) - 3), 6);
        assert_eq!(get1(add1(9) - add0(3)), 7);
        assert_eq!(get0(add0(9) - add0(3)), 6);
        // Two 1-based offsets differ by a plain 0-based distance.
        assert_eq!(get0(add1(9) - Number1::FIRST), 9);
    }

    #[test]
    fn inc_adds_one() {
        assert_eq!(get0(add0(0).inc()), 1);
        assert_eq!(get1(Number1::FIRST.inc()), 2);
    }

    #[test]
    fn arithmetic_wraps_instead_of_panicking() {
        assert_eq!(get0(add0(i32::MAX) + 1), i32::MIN);
        assert_eq!(get0(neg(add0(i32::MIN))), i32::MIN);
    }

    #[test]
    fn offsets_order_like_numbers() {
        assert!(add0(1) < add0(2));
        assert!(Number1::FIRST < add1(1));
    }
}
