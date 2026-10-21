// Copyright 2026 RealBrakedownPerf Authors
//
// Licensed under the Apache License, Version 2.0 (see
// LICENSE or https://www.apache.org/licenses/LICENSE-2.0).
// This file may not be copied, modified, or distributed
// except according to those terms.

//! Quadratic extension field [`Fp2M61`] ($\mathbb{F}_{p^2} = \mathbb{F}_p[i]/(i^2+1)$ where
//! $p = 2^{61} - 1$, Plan 02 §3).
//!
//! Elements $a = a_r + i a_i$ are stored in canonical residue form across two 64-bit
//! limbs `real` and `img`, each strictly in $[0, 2^{61} - 1)$.
//!
//! Accumulators [`Fp2M61Acc`] maintain two 128-bit unsigned integers (`re: u128`, `im: u128`).
//! By precomputing the negated imaginary weight $w_{i,\text{neg}} = p \oplus w_i$, both real
//! and imaginary accumulations are purely unsigned positive sums of products, permitting up
//! to 32 incident edges before partial folding or reduction is required (Plan 02 §3.3).

use ff::Field;
use lcpc_2d::{FieldHash, SizedField};
use rand_core::TryRng;
use serde::{Deserialize, Serialize};
use subtle::{Choice, ConditionallySelectable, ConstantTimeEq, CtOption};

use crate::{impl_field_ops_and_num, LazyField};

/// Quadratic extension field element $a = a_r + i a_i \in \mathbb{F}_{p^2} = \mathbb{F}_p[i]/(i^2+1)$
/// over the 9th Mersenne prime $p = 2^{61} - 1$.
#[repr(C, align(16))]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fp2M61 {
    /// Real component $a_r \in [0, 2^{61} - 1)$.
    pub real: u64,
    /// Imaginary component $a_i \in [0, 2^{61} - 1)$.
    pub img: u64,
}

/// 32-byte unreduced accumulator for [`Fp2M61`] lazy MACs (Plan 02 §3.3).
///
/// Accumulates up to [`Fp2M61::MAX_ACCUMULATIONS`] (32) incident graph edges directly into
/// two native 128-bit unsigned integers (`re` and `im`) without carry propagation or reduction.
#[repr(C, align(32))]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Fp2M61Acc {
    /// 128-bit unsigned accumulator for the real part.
    pub re: u128,
    /// 128-bit unsigned accumulator for the imaginary part.
    pub im: u128,
}

impl Fp2M61 {
    /// The 9th Mersenne prime modulus $p = 2^{61} - 1 = \text{0x1fffffffffffffff}$.
    pub const MODULUS_P: u64 = 0x1fffffffffffffff;

    /// The 9th Mersenne prime modulus $p = 2^{61} - 1$ as `u128`.
    pub const MODULUS_P_U128: u128 = 0x1fffffffffffffff;

    /// Additive identity $0 + 0i$.
    pub const ZERO: Self = Self { real: 0, img: 0 };

    /// Multiplicative identity $1 + 0i$.
    pub const ONE: Self = Self { real: 1, img: 0 };

    /// Imaginary unit $0 + 1i$, where $i^2 = -1 = p - 1$.
    pub const I: Self = Self { real: 0, img: 1 };

    /// Quadratic non-residue generator $1 + i \cdot 2^{31}$ (norm $N(g) = 1 + 2 = 3$,
    /// Legendre symbol $(3 / p) = -1$).
    pub const NON_SQUARE: Self = Self {
        real: 1,
        img: 1 << 31,
    };

    /// Maximum number of unreduced MAC operations before [`LazyField::fold`] or
    /// [`LazyField::reduce`] must be called (Plan 02 §3.3).
    pub const MAX_ACCUMULATIONS: usize = 32;

    /// Create a new [`Fp2M61`] from canonical coordinates `real` and `img`.
    ///
    /// # Panics
    /// Panics in debug builds if `real >= MODULUS_P` or `img >= MODULUS_P`.
    #[inline(always)]
    pub const fn from_parts(real: u64, img: u64) -> Self {
        assert!(
            real < Self::MODULUS_P && img < Self::MODULUS_P,
            "coordinates must be strictly less than 2^61 - 1"
        );
        Self { real, img }
    }

    /// Construct from raw coordinates, reducing each modulo $p = 2^{61} - 1$.
    #[inline(always)]
    pub const fn from_raw_reduced(real: u64, img: u64) -> Self {
        Self {
            real: reduce_u64_m61(real),
            img: reduce_u64_m61(img),
        }
    }

    /// Returns `true` iff both `real` and `img` are in the canonical range $[0, 2^{61} - 1)$.
    #[inline(always)]
    pub const fn is_valid(&self) -> bool {
        self.real < Self::MODULUS_P && self.img < Self::MODULUS_P
    }

    /// Convert canonical element to 16 little-endian bytes (`real` then `img`).
    #[inline(always)]
    pub const fn to_repr(&self) -> [u8; 16] {
        let r = self.real.to_le_bytes();
        let i = self.img.to_le_bytes();
        [
            r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], i[0], i[1], i[2], i[3], i[4], i[5],
            i[6], i[7],
        ]
    }

    /// Parse canonical element from 16 little-endian bytes (`real` then `img`).
    #[inline]
    pub fn from_repr(repr: [u8; 16]) -> Option<Self> {
        let real = u64::from_le_bytes([
            repr[0], repr[1], repr[2], repr[3], repr[4], repr[5], repr[6], repr[7],
        ]);
        let img = u64::from_le_bytes([
            repr[8], repr[9], repr[10], repr[11], repr[12], repr[13], repr[14], repr[15],
        ]);
        let cand = Self { real, img };
        if cand.is_valid() {
            Some(cand)
        } else {
            None
        }
    }

    /// Construct from a `u64` scalar: `real = x mod p`, `img = 0`.
    #[inline(always)]
    pub const fn from_u64_val(val: u64) -> Self {
        Self {
            real: reduce_u64_m61(val),
            img: 0,
        }
    }

    /// Field addition in $\mathbb{F}_{p^2}$.
    #[inline(always)]
    pub const fn add_val(a: Self, b: Self) -> Self {
        Self {
            real: add_p(a.real, b.real),
            img: add_p(a.img, b.img),
        }
    }

    /// Field subtraction in $\mathbb{F}_{p^2}$.
    #[inline(always)]
    pub const fn sub_val(a: Self, b: Self) -> Self {
        Self {
            real: sub_p(a.real, b.real),
            img: sub_p(a.img, b.img),
        }
    }

    /// Field negation in $\mathbb{F}_{p^2}$.
    #[inline(always)]
    pub const fn neg_val(a: Self) -> Self {
        Self {
            real: if a.real == 0 {
                0
            } else {
                Self::MODULUS_P ^ a.real
            },
            img: if a.img == 0 {
                0
            } else {
                Self::MODULUS_P ^ a.img
            },
        }
    }

    /// Field multiplication in $\mathbb{F}_{p^2}$ via 4-mul unsigned MAC form (Plan 02 §3.3).
    #[inline(always)]
    pub const fn mul_val(a: Self, b: Self) -> Self {
        let a_r = a.real as u128;
        let a_i = a.img as u128;
        let b_r = b.real as u128;
        let b_i = b.img as u128;
        let b_i_neg = (b.img ^ Self::MODULUS_P) as u128;

        let re_prod = a_r * b_r + a_i * b_i_neg;
        let im_prod = a_r * b_i + a_i * b_r;

        Self {
            real: reduce_128_m61(re_prod),
            img: reduce_128_m61(im_prod),
        }
    }

    /// Field squaring in $\mathbb{F}_{p^2}$ via 2 base-field multiplications:
    /// $(a_r + i a_i)^2 = (a_r + a_i)(a_r - a_i) + i(2 a_r a_i)$.
    #[inline(always)]
    pub const fn square_val(a: Self) -> Self {
        let a_r = a.real;
        let a_i = a.img;

        let s_add = add_p(a_r, a_i);
        let s_sub = sub_p(a_r, a_i);

        let re_prod = (s_add as u128) * (s_sub as u128);
        let im_prod = ((a_r as u128) * (a_i as u128)) << 1;

        Self {
            real: reduce_128_m61(re_prod),
            img: reduce_128_m61(im_prod),
        }
    }

    /// Multiplicative inverse in $\mathbb{F}_{p^2}$:
    /// $a^{-1} = (a_r - i a_i) \cdot (a_r^2 + a_i^2)^{-1}$.
    #[inline]
    pub fn invert(&self) -> CtOption<Self> {
        let norm = add_p(sq_p(self.real), sq_p(self.img));
        let norm_inv = invert_p(norm);
        let is_non_zero = !self.is_zero();

        let real = mul_p(self.real, norm_inv);
        let neg_img = if self.img == 0 {
            0
        } else {
            Self::MODULUS_P ^ self.img
        };
        let img = mul_p(neg_img, norm_inv);

        CtOption::new(Self { real, img }, is_non_zero)
    }

    /// Returns the square root of `self` in $\mathbb{F}_{p^2}$, if it is a quadratic residue.
    #[inline]
    pub fn sqrt(&self) -> CtOption<Self> {
        if self.real == 0 && self.img == 0 {
            return CtOption::new(Self::ZERO, 1.into());
        }

        if self.img == 0 {
            let r = self.real;
            let root = sqrt_p(r);
            if sq_p(root) == r {
                return CtOption::new(Self { real: root, img: 0 }, 1.into());
            } else {
                // r is a quadratic non-residue in F_p.
                // -r = p - r is a quadratic residue in F_p!
                let neg_r = Self::MODULUS_P - r;
                let root = sqrt_p(neg_r);
                return CtOption::new(Self { real: 0, img: root }, 1.into());
            }
        }

        let norm = add_p(sq_p(self.real), sq_p(self.img));
        let n = sqrt_p(norm);
        if sq_p(n) != norm {
            // norm is a quadratic non-residue in F_p => self is not a square in F_{p^2}
            return CtOption::new(Self::ZERO, 0.into());
        }

        let mut n_val = n;
        let mut u = div2_p(add_p(self.real, n_val));
        let mut x = sqrt_p(u);
        if sq_p(x) != u {
            n_val = Self::MODULUS_P - n_val;
            u = div2_p(add_p(self.real, n_val));
            x = sqrt_p(u);
        }

        let two_x = add_p(x, x);
        let two_x_inv = invert_p(two_x);
        let y = mul_p(self.img, two_x_inv);

        CtOption::new(Self { real: x, img: y }, 1.into())
    }

    /// Constant-time square-root ratio computation satisfying [`ff::Field::sqrt_ratio`].
    #[inline]
    pub fn sqrt_ratio(num: &Self, div: &Self) -> (Choice, Self) {
        if bool::from(num.is_zero()) {
            return (Choice::from(1), Self::ZERO);
        }
        if bool::from(div.is_zero()) {
            return (Choice::from(0), Self::ZERO);
        }

        let div_inv = div.invert().unwrap();
        let cand = *num * div_inv;
        let cand_sqrt = cand.sqrt();
        if bool::from(cand_sqrt.is_some()) {
            (Choice::from(1), cand_sqrt.unwrap())
        } else {
            let scaled = cand * Self::NON_SQUARE;
            (Choice::from(0), scaled.sqrt().unwrap())
        }
    }
}

/// Reduce a `u64` integer modulo $p = 2^{61} - 1$.
#[inline(always)]
pub const fn reduce_u64_m61(x: u64) -> u64 {
    const P: u64 = Fp2M61::MODULUS_P;
    let lo = x & P;
    let hi = x >> 61;
    let mut s = lo + hi;
    if s >= P {
        s -= P;
    }
    s
}

/// Base-field addition $a + b \bmod (2^{61} - 1)$ for $a, b \in [0, 2^{61} - 1)$.
#[inline(always)]
pub const fn add_p(a: u64, b: u64) -> u64 {
    let s = a + b;
    if s >= Fp2M61::MODULUS_P {
        s - Fp2M61::MODULUS_P
    } else {
        s
    }
}

/// Base-field subtraction $a - b \bmod (2^{61} - 1)$ for $a, b \in [0, 2^{61} - 1)$.
#[inline(always)]
pub const fn sub_p(a: u64, b: u64) -> u64 {
    if a >= b {
        a - b
    } else {
        (a + Fp2M61::MODULUS_P) - b
    }
}

/// Base-field division by 2 modulo $2^{61} - 1$: computes $x / 2 \bmod p$.
#[inline(always)]
pub const fn div2_p(x: u64) -> u64 {
    let mask = (x & 1).wrapping_neg();
    (x + (mask & Fp2M61::MODULUS_P)) >> 1
}

/// Reduce a native `u128` integer modulo $p = 2^{61} - 1$ using zero multiplications (Plan 02 §3.3).
#[inline(always)]
pub const fn reduce_128_m61(x: u128) -> u64 {
    const P: u64 = Fp2M61::MODULUS_P;
    let lo = x as u64;
    let hi = (x >> 64) as u64;

    // x = lo + 2^64 * hi. Since 2^61 = 1 mod p and 2^122 = 1 mod p:
    let x0 = lo & P;
    let x1 = ((hi << 3) | (lo >> 61)) & P;
    let x2 = hi >> 58;

    let mut s = x0 + x1 + x2;
    s = (s & P) + (s >> 61);
    if s >= P {
        s -= P;
    }
    s
}

/// Base-field multiplication $a \cdot b \bmod (2^{61} - 1)$ for $a, b \in [0, 2^{61} - 1)$.
#[inline(always)]
pub const fn mul_p(a: u64, b: u64) -> u64 {
    reduce_128_m61((a as u128) * (b as u128))
}

/// Base-field squaring $a^2 \bmod (2^{61} - 1)$ for $a \in [0, 2^{61} - 1)$.
#[inline(always)]
pub const fn sq_p(a: u64) -> u64 {
    reduce_128_m61((a as u128) * (a as u128))
}

/// Compute $x^{-1} \equiv x^{2^{61}-3} \pmod{2^{61}-1}$ for $x \in (0, 2^{61}-1)$.
#[inline]
pub fn invert_p(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    // Addition chain for 2^59 - 1 (59 ones):
    // x1 = x (1 one)
    // x2 = x1^2 * x1 = x^3 (2 ones)
    let x2 = mul_p(sq_p(x), x);
    // x3 = x2^2 * x = x^7 (3 ones)
    let x3 = mul_p(sq_p(x2), x);

    // x6 = (x3)^(2^3) * x3 (6 ones)
    let mut t = x3;
    for _ in 0..3 {
        t = sq_p(t);
    }
    let x6 = mul_p(t, x3);

    // x12 = (x6)^(2^6) * x6 (12 ones)
    let mut t = x6;
    for _ in 0..6 {
        t = sq_p(t);
    }
    let x12 = mul_p(t, x6);

    // x24 = (x12)^(2^12) * x12 (24 ones)
    let mut t = x12;
    for _ in 0..12 {
        t = sq_p(t);
    }
    let x24 = mul_p(t, x12);

    // x48 = (x24)^(2^24) * x24 (48 ones)
    let mut t = x24;
    for _ in 0..24 {
        t = sq_p(t);
    }
    let x48 = mul_p(t, x24);

    // x8 = (x6)^(2^2) * x2 (8 ones)
    let mut t = x6;
    for _ in 0..2 {
        t = sq_p(t);
    }
    let x8 = mul_p(t, x2);

    // x56 = (x48)^(2^8) * x8 (56 ones)
    let mut t = x48;
    for _ in 0..8 {
        t = sq_p(t);
    }
    let x56 = mul_p(t, x8);

    // x59 = (x56)^(2^3) * x3 (59 ones)
    let mut t = x56;
    for _ in 0..3 {
        t = sq_p(t);
    }
    let x59 = mul_p(t, x3);

    // 2^61 - 3 = 4 * (2^59 - 1) + 1:
    // (x59)^4 * x
    mul_p(sq_p(sq_p(x59)), x)
}

/// Compute $r^{(p+1)/4} = r^{2^{59}} \pmod{2^{61}-1}$ via 59 squarings in $\mathbb{F}_p$.
#[inline]
pub fn sqrt_p(r: u64) -> u64 {
    let mut root = r;
    for _ in 0..59 {
        root = sq_p(root);
    }
    root
}

/// Fold upper bits $\ge 61$ of a `u128` back into bits $0..60$ modulo $2^{61} - 1$ (Plan 02 §3.3).
///
/// Reduces accumulator value from 128 bits down to $< 2^{62}$, restoring headroom for another
/// 31 unreduced MAC operations without overflow.
#[inline(always)]
pub const fn fold_u128(x: u128) -> u128 {
    const P: u128 = Fp2M61::MODULUS_P_U128;
    let lo = x & P;
    let hi = x >> 61;
    let s = lo + hi;
    (s & P) + (s >> 61)
}

/// Partial fold of an [`Fp2M61Acc`] accumulator (Plan 02 §3.3).
#[inline(always)]
pub fn fold_fp2_m61(acc: &mut Fp2M61Acc) {
    acc.re = fold_u128(acc.re);
    acc.im = fold_u128(acc.im);
}

/// Unreduced lazy multiply-accumulate for [`Fp2M61`]: `acc += w * x` (Plan 02 §3.3).
#[inline(always)]
pub fn mac_fp2_m61(acc: &mut Fp2M61Acc, w: &Fp2M61, x: &Fp2M61) {
    let w_r = w.real as u128;
    let w_i = w.img as u128;
    let w_i_neg = (w.img ^ Fp2M61::MODULUS_P) as u128;
    let x_r = x.real as u128;
    let x_i = x.img as u128;

    acc.re += x_r * w_r + x_i * w_i_neg;
    acc.im += x_r * w_i + x_i * w_r;
}

/// Unreduced 32-byte accumulator addition for [`Fp2M61Acc`]: `acc += other`.
#[inline(always)]
pub fn add_fp2_m61_acc(acc: &mut Fp2M61Acc, other: &Fp2M61Acc) {
    acc.re += other.re;
    acc.im += other.im;
}

/// Full reduction of a 32-byte [`Fp2M61Acc`] to a canonical [`Fp2M61`] element (Plan 02 §3.3).
#[inline(always)]
pub fn reduce_fp2_m61(acc: Fp2M61Acc) -> Fp2M61 {
    Fp2M61 {
        real: reduce_128_m61(acc.re),
        img: reduce_128_m61(acc.im),
    }
}

/// Non-inlined [`Fp2M61`] column MAC kernel for scalar disassembly verification (`check_codegen.py`).
#[inline(never)]
pub fn mac_column_fp2_m61(acc: &mut [Fp2M61Acc], w: &Fp2M61, src: &[Fp2M61]) {
    assert_eq!(acc.len(), src.len());
    let w_r = w.real as u128;
    let w_i = w.img as u128;
    let w_i_neg = (w.img ^ Fp2M61::MODULUS_P) as u128;
    for (a, x) in acc.iter_mut().zip(src.iter()) {
        let x_r = x.real as u128;
        let x_i = x.img as u128;
        a.re += x_r * w_r + x_i * w_i_neg;
        a.im += x_r * w_i + x_i * w_r;
    }
}

/// Non-inlined [`Fp2M61`] column reduction kernel for scalar disassembly verification (`check_codegen.py`).
#[inline(never)]
pub fn reduce_column_fp2_m61(dst: &mut [Fp2M61], acc: &mut [Fp2M61Acc]) {
    assert_eq!(dst.len(), acc.len());
    for (d, a) in dst.iter_mut().zip(acc.iter_mut()) {
        *d = reduce_fp2_m61(core::mem::take(a));
    }
}

/// Non-inlined [`Fp2M61`] column partial-fold kernel for scalar disassembly verification (`check_codegen.py`).
#[inline(never)]
pub fn fold_column_fp2_m61(acc: &mut [Fp2M61Acc]) {
    for a in acc.iter_mut() {
        fold_fp2_m61(a);
    }
}

impl LazyField for Fp2M61 {
    type Acc = Fp2M61Acc;

    /// Documented maximum of 32 unreduced MAC operations before [`LazyField::fold`] is
    /// required (Plan 02 §3.3, §4.2).
    const MAX_ACCUMULATIONS: usize = 32;

    #[inline(always)]
    fn prepare_weight(w: &Self) -> Self {
        *w
    }

    #[inline(always)]
    fn mac(acc: &mut Self::Acc, w: &Self, x: &Self) {
        mac_fp2_m61(acc, w, x);
    }

    #[inline(always)]
    fn add(acc: &mut Self::Acc, other: &Self::Acc) {
        add_fp2_m61_acc(acc, other);
    }

    #[inline(always)]
    fn reduce(acc: Self::Acc) -> Self {
        reduce_fp2_m61(acc)
    }

    #[inline(always)]
    fn fold(acc: &mut Self::Acc) {
        fold_fp2_m61(acc);
    }
}

impl ConstantTimeEq for Fp2M61 {
    #[inline(always)]
    fn ct_eq(&self, other: &Self) -> Choice {
        self.real.ct_eq(&other.real) & self.img.ct_eq(&other.img)
    }
}

impl ConditionallySelectable for Fp2M61 {
    #[inline(always)]
    fn conditional_select(a: &Self, b: &Self, choice: Choice) -> Self {
        Self {
            real: u64::conditional_select(&a.real, &b.real, choice),
            img: u64::conditional_select(&a.img, &b.img, choice),
        }
    }
}

impl Ord for Fp2M61 {
    #[inline(always)]
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        match self.real.cmp(&other.real) {
            core::cmp::Ordering::Equal => self.img.cmp(&other.img),
            ord => ord,
        }
    }
}

impl PartialOrd for Fp2M61 {
    #[inline(always)]
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl From<u64> for Fp2M61 {
    #[inline(always)]
    fn from(val: u64) -> Self {
        Self::from_u64_val(val)
    }
}

impl_field_ops_and_num!(Fp2M61);

impl Field for Fp2M61 {
    const ZERO: Self = Self::ZERO;
    const ONE: Self = Self::ONE;

    #[inline]
    fn try_random<R: TryRng + ?Sized>(rng: &mut R) -> Result<Self, R::Error> {
        let real = loop {
            let v = rng.try_next_u64()? & Self::MODULUS_P;
            if v < Self::MODULUS_P {
                break v;
            }
        };
        let img = loop {
            let v = rng.try_next_u64()? & Self::MODULUS_P;
            if v < Self::MODULUS_P {
                break v;
            }
        };
        Ok(Self { real, img })
    }

    #[inline(always)]
    fn is_zero_vartime(&self) -> bool {
        self.real == 0 && self.img == 0
    }

    #[inline(always)]
    fn square(&self) -> Self {
        Self::square_val(*self)
    }

    #[inline(always)]
    fn double(&self) -> Self {
        Self::add_val(*self, *self)
    }

    #[inline]
    fn invert(&self) -> CtOption<Self> {
        Self::invert(self)
    }

    #[inline]
    fn sqrt_ratio(num: &Self, div: &Self) -> (Choice, Self) {
        Self::sqrt_ratio(num, div)
    }

    #[inline]
    fn sqrt(&self) -> CtOption<Self> {
        Self::sqrt(self)
    }
}

impl SizedField for Fp2M61 {
    const CLOG2: u32 = 122;
    const FLOG2: u32 = 121;
}

impl FieldHash for Fp2M61 {
    type HashRepr = [u8; 16];

    #[inline]
    fn to_hash_repr(&self) -> Self::HashRepr {
        self.to_repr()
    }
}
