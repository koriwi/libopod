use std::ops::{
    Add, AddAssign, BitAnd, BitOr, BitXor, BitXorAssign, Div, Mul, Neg, Not, Shl, Shr, Sub,
    SubAssign,
};

/// A 32-bit word whose arithmetic has C's unsigned wrapping semantics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Word(pub(super) u32);

impl Word {
    pub(super) const fn new(value: u32) -> Self {
        Self(value)
    }

    pub(super) const fn low_byte(self) -> u8 {
        self.0.to_le_bytes()[0]
    }

    pub(super) const fn index(self, mask: u32) -> usize {
        (self.0 & mask) as usize
    }

    pub(super) const fn rotate_left(self, count: u32) -> Self {
        Self(self.0.rotate_left(count))
    }

    pub(super) const fn rotate_right(self, count: u32) -> Self {
        Self(self.0.rotate_right(count))
    }
}

impl Add for Word {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0.wrapping_add(rhs.0))
    }
}
impl AddAssign for Word {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}
impl Sub for Word {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0.wrapping_sub(rhs.0))
    }
}
impl SubAssign for Word {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}
impl Mul for Word {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self {
        Self(self.0.wrapping_mul(rhs.0))
    }
}
impl Div for Word {
    type Output = Self;
    fn div(self, rhs: Self) -> Self {
        Self(self.0 / rhs.0)
    }
}
impl BitAnd for Word {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}
impl BitOr for Word {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}
impl BitXor for Word {
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self {
        Self(self.0 ^ rhs.0)
    }
}
impl BitXorAssign for Word {
    fn bitxor_assign(&mut self, rhs: Self) {
        *self = *self ^ rhs;
    }
}
impl Not for Word {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0)
    }
}
impl Neg for Word {
    type Output = Self;
    fn neg(self) -> Self {
        Self(self.0.wrapping_neg())
    }
}
impl Shl<u32> for Word {
    type Output = Self;
    fn shl(self, rhs: u32) -> Self {
        Self(self.0 << rhs)
    }
}
impl Shr<u32> for Word {
    type Output = Self;
    fn shr(self, rhs: u32) -> Self {
        Self(self.0 >> rhs)
    }
}
